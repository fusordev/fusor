#[derive(Clone, Copy, Debug)]
struct FunctionInitializerSite {
    closure_index: usize,
    closure_pc: BytecodePc,
}

struct VerifiedFunctionInitializers {
    put_definitions: Vec<Option<usize>>,
    entry_prefix_end: usize,
}

#[allow(clippy::too_many_lines)]
fn verify_function_initializers(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
    entry_prefix: usize,
    internal_stack: &InternalStackCertificate,
) -> Result<VerifiedFunctionInitializers, BytecodeVerificationError> {
    let flow = function.control_flow();
    let instructions = flow.instructions();
    let mut predecessor_counts = try_filled_vec(
        id,
        instructions.len(),
        0_u32,
        BytecodeGraphResource::SourceMappings,
    )?;
    for index in 0..instructions.len() {
        for edge in internal_stack.effective_successors(instructions, index) {
            let successor = edge.target;
            let count = &mut predecessor_counts[successor.get() as usize];
            *count = count.saturating_add(1);
        }
    }

    let mut sites = try_filled_vec(
        id,
        variables.len(),
        None,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut matches = try_filled_vec(
        id,
        variables.len(),
        0_u32,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut closure_definitions = try_filled_vec(
        id,
        instructions.len(),
        None,
        BytecodeGraphResource::SourceMappings,
    )?;
    let mut put_definitions = try_filled_vec(
        id,
        instructions.len(),
        None,
        BytecodeGraphResource::SourceMappings,
    )?;
    let argument_count = flow.domains().argument_count() as usize;

    for index in 0..instructions.len().saturating_sub(1) {
        let closure = instructions[index].decoded().instruction();
        let Some(constant) = closure_constant(closure.opcode(), closure.operands()) else {
            continue;
        };
        let put = instructions[index + 1].decoded().instruction();
        let Some(definition_index) =
            initializer_put_definition(put.opcode(), put.operands(), argument_count)
        else {
            continue;
        };
        let Some(definition) = variables.get(definition_index) else {
            continue;
        };
        if definition.function_initializer != Some(constant)
            || !internal_stack.has_effective_successor(instructions, index, usize_to_u32(index + 1))
            || predecessor_counts[index + 1] != 1
        {
            continue;
        }
        let count = &mut matches[definition_index];
        *count = count.saturating_add(1);
        if *count == 1 {
            let site = FunctionInitializerSite {
                closure_index: index,
                closure_pc: instructions[index].decoded().pc(),
            };
            sites[definition_index] = Some(site);
            closure_definitions[index] = Some(definition_index);
            put_definitions[index + 1] = Some(definition_index);
        }
    }

    verify_scope_function_initializer_groups(
        id,
        variables,
        instructions,
        &predecessor_counts,
        &closure_definitions,
        &put_definitions,
        argument_count,
        internal_stack,
    )?;

    let first_instantiation_definition =
        variables
            .iter()
            .enumerate()
            .find_map(|(index, definition)| {
                (definition.function_initializer.is_some()
                    && definition.policy.initialization
                        != CompilerInitializationPolicy::FunctionAtScopeEntry)
                    .then_some(index)
            });
    let mut prefix_index = entry_prefix;
    if first_instantiation_definition.is_some() || entry_prefix != 0 {
        while let Some(verified) = instructions.get(prefix_index) {
            let instruction = verified.decoded().instruction();
            if instruction.opcode() != FinalOpcode::SetLocUninitialized {
                break;
            }
            let expected_predecessors = u32::from(prefix_index != 0);
            if predecessor_counts[prefix_index] != expected_predecessors
                || !internal_stack.has_effective_successor(
                    instructions,
                    prefix_index,
                    usize_to_u32(prefix_index + 1),
                )
            {
                if let Some(first_definition) = first_instantiation_definition {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
                            definition: usize_to_u32(first_definition),
                            pc: verified.decoded().pc(),
                        },
                    ));
                }
                let local =
                    local_operand(instruction.opcode(), instruction.operands()).unwrap_or(0);
                return Err(policy_error(
                    id,
                    BindingSlot::Local(local),
                    Some(verified.decoded().pc()),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            prefix_index += 1;
        }
    }
    for (definition_index, definition) in variables.iter().enumerate() {
        let Some(constant) = definition.function_initializer else {
            continue;
        };
        if matches[definition_index] != 1 {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionInitializerOpcodeMismatch {
                    definition: usize_to_u32(definition_index),
                    constant,
                    matches: matches[definition_index],
                },
            ));
        }
        let Some(site) = sites[definition_index] else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionInitializerOpcodeMismatch {
                    definition: usize_to_u32(definition_index),
                    constant,
                    matches: matches[definition_index],
                },
            ));
        };
        if definition.policy.initialization != CompilerInitializationPolicy::FunctionAtScopeEntry {
            let expected_predecessors = u32::from(prefix_index != 0);
            if site.closure_index != prefix_index
                || predecessor_counts[site.closure_index] != expected_predecessors
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
                        definition: usize_to_u32(definition_index),
                        pc: site.closure_pc,
                    },
                ));
            }
            prefix_index = prefix_index.checked_add(2).ok_or_else(|| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
                        definition: usize_to_u32(definition_index),
                        pc: site.closure_pc,
                    },
                )
            })?;
        }
    }

    Ok(VerifiedFunctionInitializers {
        put_definitions,
        entry_prefix_end: prefix_index,
    })
}

#[allow(
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
fn verify_scope_function_initializer_groups(
    id: FunctionTemplateId,
    variables: &[VariableDefinition],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    closure_definitions: &[Option<usize>],
    put_definitions: &[Option<usize>],
    argument_count: usize,
    internal_stack: &InternalStackCertificate,
) -> Result<(), BytecodeVerificationError> {
    let mut activation_epoch = try_filled_vec(
        id,
        variables.len(),
        0_u32,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut verified_sites = try_filled_vec(
        id,
        variables.len(),
        false,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut epoch = 0_u32;
    let mut index = 0_usize;

    while index < instructions.len() {
        if instructions[index].decoded().instruction().opcode() != FinalOpcode::SetLocUninitialized
        {
            index += 1;
            continue;
        }
        let activation_start = index;
        while index < instructions.len()
            && instructions[index].decoded().instruction().opcode()
                == FinalOpcode::SetLocUninitialized
        {
            index += 1;
        }
        let activation_end = index;
        let pair_start = index;
        while index + 1 < instructions.len() {
            let Some(definition) = closure_definitions[index] else {
                break;
            };
            if variables[definition].policy.initialization
                != CompilerInitializationPolicy::FunctionAtScopeEntry
                || put_definitions[index + 1] != Some(definition)
            {
                break;
            }
            index += 2;
        }
        let pair_end = index;
        if pair_start == pair_end {
            continue;
        }

        epoch = epoch.checked_add(1).ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::VariableDefinitions,
                    limit: u64::MAX,
                    observed: u64::MAX,
                },
            )
        })?;
        for activation in &instructions[activation_start..activation_end] {
            let instruction = activation.decoded().instruction();
            let Some(local) = local_operand(instruction.opcode(), instruction.operands()) else {
                continue;
            };
            let definition = argument_count + local as usize;
            if variables[definition].policy.initialization
                == CompilerInitializationPolicy::FunctionAtScopeEntry
            {
                activation_epoch[definition] = epoch;
            }
        }
        for pair in (pair_start..pair_end).step_by(2) {
            let Some(definition) = closure_definitions[pair] else {
                continue;
            };
            if activation_epoch[definition] != epoch {
                return Err(function_initializer_placement_error(
                    id,
                    definition,
                    instructions[pair].decoded().pc(),
                ));
            }
            verified_sites[definition] = true;
        }
        let Some(first_definition) = closure_definitions[pair_start] else {
            continue;
        };
        for edge in activation_start..pair_end {
            if edge != activation_start && predecessor_counts[edge] != 1 {
                return Err(function_initializer_placement_error(
                    id,
                    first_definition,
                    instructions[pair_start].decoded().pc(),
                ));
            }
            if edge + 1 < pair_end
                && !internal_stack.has_effective_successor(
                    instructions,
                    edge,
                    usize_to_u32(edge + 1),
                )
            {
                return Err(function_initializer_placement_error(
                    id,
                    first_definition,
                    instructions[pair_start].decoded().pc(),
                ));
            }
        }
    }

    for (definition, variable) in variables.iter().enumerate() {
        if variable.policy.initialization == CompilerInitializationPolicy::FunctionAtScopeEntry
            && variable.function_initializer.is_some()
            && !verified_sites[definition]
        {
            let pc = closure_definitions
                .iter()
                .position(|candidate| *candidate == Some(definition))
                .and_then(|index| instructions.get(index))
                .map_or(BytecodePc::new(0), |instruction| instruction.decoded().pc());
            return Err(function_initializer_placement_error(id, definition, pc));
        }
    }
    Ok(())
}
fn function_initializer_placement_error(
    id: FunctionTemplateId,
    definition: usize,
    pc: BytecodePc,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::FunctionInitializerPlacementMismatch {
            definition: usize_to_u32(definition),
            pc,
        },
    )
}

const fn closure_constant(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match (opcode, operands) {
        (FinalOpcode::FClosure, Operands::Const(index)) => Some(index),
        (FinalOpcode::FClosure8, Operands::Const8(index)) => Some(index as u32),
        _ => None,
    }
}

const fn initializer_put_definition(
    opcode: FinalOpcode,
    operands: Operands,
    argument_count: usize,
) -> Option<usize> {
    if matches!(
        opcode,
        FinalOpcode::PutArg
            | FinalOpcode::PutArg0
            | FinalOpcode::PutArg1
            | FinalOpcode::PutArg2
            | FinalOpcode::PutArg3
    ) {
        return match argument_operand(opcode, operands) {
            Some(index) => Some(index as usize),
            None => None,
        };
    }
    if matches!(
        opcode,
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    ) {
        return match local_operand(opcode, operands) {
            Some(index) => argument_count.checked_add(index as usize),
            None => None,
        };
    }
    None
}

fn verify_scope_links(
    id: FunctionTemplateId,
    locals: &[VariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    let mut states = try_filled_vec(
        id,
        locals.len(),
        0_u8,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut path = Vec::new();
    path.try_reserve_exact(locals.len()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource: BytecodeGraphResource::VariableDefinitions,
                requested: usize_to_u64(locals.len()),
            },
        )
    })?;
    for start in 0..locals.len() {
        if states[start] == 2 {
            continue;
        }
        path.clear();
        let mut current = start;
        loop {
            match states[current] {
                2 => break,
                1 => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ScopeLinkCycle {
                            local: usize_to_u32(current),
                        },
                    ));
                }
                _ => {
                    states[current] = 1;
                    path.push(current);
                }
            }
            match locals[current].scope_next {
                ScopeLink::End => break,
                ScopeLink::ArgumentScopeEnd => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ArgumentScopeMetadataUnsupported {
                            definition: usize_to_u32(current),
                        },
                    ));
                }
                ScopeLink::Local(next) => {
                    current = next as usize;
                }
            }
        }
        for local in path.drain(..) {
            states[local] = 2;
        }
    }
    Ok(())
}

/// Validates `QuickJS`'s adjusted direct-eval scope encoding.
///
/// `0` is `ARG_SCOPE_END`, `1` is the ordinary `-1` end sentinel, and every
/// larger value is a zero-based local index plus two. A concrete head must be
/// lexical; function-scoped arguments and locals are appended by direct-eval
/// environment construction after walking this lexical chain.
fn verify_eval_scope_operands(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    let arguments = flow.domains().argument_count() as usize;
    let locals = &variables[arguments..];
    for verified in flow.instructions() {
        let decoded = verified.decoded();
        let instruction = decoded.instruction();
        let ((FinalOpcode::Eval | FinalOpcode::TailEval, Operands::NPopU16 { scope_index, .. })
        | (FinalOpcode::ApplyEval | FinalOpcode::TailApplyEval, Operands::U16(scope_index))) =
            (instruction.opcode(), instruction.operands())
        else {
            continue;
        };
        let Some(local) = scope_index.checked_sub(2).map(u32::from) else {
            continue;
        };
        let Some(definition) = locals.get(local as usize) else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::EvalScopeIndexOutOfBounds {
                    pc: decoded.pc(),
                    scope_index,
                    locals: usize_to_u32(locals.len()),
                },
            ));
        };
        if !definition.has_scope {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::EvalScopeHeadNotLexical {
                    pc: decoded.pc(),
                    local,
                },
            ));
        }
    }
    Ok(())
}

fn verify_capture_layout(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
    arguments: usize,
) -> Result<(), BytecodeVerificationError> {
    let Some(layout) = function.control_flow().compiler_capture_layout() else {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableReferenceDomainMismatch {
                declared: function
                    .control_flow()
                    .function_header()
                    .variable_reference_count(),
                captured: 0,
            },
        ));
    };
    for (index, definition) in variables.iter().enumerate() {
        let Some(reference) = definition.variable_reference else {
            continue;
        };
        let expected = if index < arguments {
            CompilerCapturedBinding::Argument(usize_to_u32(index))
        } else if definition.has_scope {
            CompilerCapturedBinding::ScopedLocal(usize_to_u32(index - arguments))
        } else {
            CompilerCapturedBinding::FunctionLocal(usize_to_u32(index - arguments))
        };
        if layout.binding_for_variable_reference(reference) != Some(expected) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::CaptureLayoutMismatch { reference },
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RealmGlobalFunctionInitializerSite {
    closure_index: usize,
    closure_pc: BytecodePc,
}

#[allow(
    clippy::too_many_lines,
    reason = "global function initializer matching and entry placement form one authority check"
)]
fn verify_realm_global_function_initializers(
    id: FunctionTemplateId,
    root: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    closures: &[ClosureVariableDefinition],
    internal_stack: &InternalStackCertificate,
) -> Result<usize, BytecodeVerificationError> {
    if !closures.iter().any(|definition| {
        definition.function_initializer.is_some()
            && matches!(
                definition.binding(),
                CompilerClosureBinding::RealmGlobal(_)
            )
    }) {
        return Ok(0);
    }
    if id != root {
        let closure = closures
            .iter()
            .position(|definition| definition.function_initializer.is_some())
            .map_or(0, usize_to_u32);
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                closure,
                constant: closures
                    .get(closure as usize)
                    .and_then(ClosureVariableDefinition::function_initializer),
            },
        ));
    }

    let instructions = function.control_flow().instructions();
    let mut predecessor_counts = try_filled_vec(
        id,
        instructions.len(),
        0_u32,
        BytecodeGraphResource::SourceMappings,
    )?;
    for index in 0..instructions.len() {
        for edge in internal_stack.effective_successors(instructions, index) {
            let successor = edge.target;
            predecessor_counts[successor.get() as usize] =
                predecessor_counts[successor.get() as usize].saturating_add(1);
        }
    }

    let mut sites = try_filled_vec(
        id,
        closures.len(),
        None,
        BytecodeGraphResource::ClosureDefinitions,
    )?;
    let mut matches = try_filled_vec(
        id,
        closures.len(),
        0_u32,
        BytecodeGraphResource::ClosureDefinitions,
    )?;
    for index in 0..instructions.len().saturating_sub(1) {
        let closure_instruction = instructions[index].decoded().instruction();
        let Some(constant) =
            closure_constant(closure_instruction.opcode(), closure_instruction.operands())
        else {
            continue;
        };
        let put_instruction = instructions[index + 1].decoded().instruction();
        let (FinalOpcode::PutVar, Operands::VarRef(closure)) =
            (put_instruction.opcode(), put_instruction.operands())
        else {
            continue;
        };
        let Some(definition) = closures.get(closure as usize) else {
            continue;
        };
        if definition.function_initializer != Some(constant)
            || !internal_stack.has_effective_successor(instructions, index, usize_to_u32(index + 1))
            || predecessor_counts[index + 1] != 1
        {
            continue;
        }
        matches[closure as usize] = matches[closure as usize].saturating_add(1);
        if matches[closure as usize] == 1 {
            sites[closure as usize] = Some(RealmGlobalFunctionInitializerSite {
                closure_index: index,
                closure_pc: instructions[index].decoded().pc(),
            });
        }
    }

    let mut prefix_index = 0_usize;
    for (closure, definition) in closures.iter().enumerate() {
        let Some(constant) = definition.function_initializer else {
            continue;
        };
        // Only realm-global function declarations are verified here; module
        // function declarations (Captured + Module source) are verified by the
        // module function initializer check.
        if !matches!(
            definition.binding(),
            CompilerClosureBinding::RealmGlobal(_)
        ) {
            continue;
        }
        if matches[closure] != 1 {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerOpcodeMismatch {
                    closure: usize_to_u32(closure),
                    constant,
                    matches: matches[closure],
                },
            ));
        }
        let Some(site) = sites[closure] else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerOpcodeMismatch {
                    closure: usize_to_u32(closure),
                    constant,
                    matches: matches[closure],
                },
            ));
        };
        let expected_predecessors = u32::from(prefix_index != 0);
        if site.closure_index != prefix_index
            || predecessor_counts[site.closure_index] != expected_predecessors
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerPlacementMismatch {
                    closure: usize_to_u32(closure),
                    pc: site.closure_pc,
                },
            ));
        }
        prefix_index = prefix_index.checked_add(2).ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerPlacementMismatch {
                    closure: usize_to_u32(closure),
                    pc: site.closure_pc,
                },
            )
        })?;
    }
    Ok(prefix_index)
}

fn module_function_initializer_is_valid(
    function: &VerifiedCompilerFunction,
    closure: &ClosureVariableDefinition,
    module_function: bool,
) -> bool {
    match (module_function, closure.function_initializer) {
        (true, Some(constant)) => matches!(
            function.constants().get(constant as usize),
            Some(crate::CompilerConstant::Function(_))
        ),
        (false, None) => true,
        (true, None) | (false, Some(_)) => false,
    }
}

/// Verifies that every hoisted module-level function declaration is initialized
/// at instantiation by a `Closure`/`PutVarRef` pair placed at the root entry,
/// mirroring the realm-global function initializer prefix but for captured
/// module cells.
fn verify_module_function_initializers(
    id: FunctionTemplateId,
    closure_index: u32,
    function: &VerifiedCompilerFunction,
    closure: &ClosureVariableDefinition,
) -> Result<(), BytecodeVerificationError> {
    let Some(constant) = closure.function_initializer else {
        return Ok(());
    };
    let instructions = function.control_flow().instructions();
    let mut matched = 0_u32;
    let mut first_site: Option<RealmGlobalFunctionInitializerSite> = None;
    for index in 0..instructions.len().saturating_sub(1) {
        let closure_instruction = instructions[index].decoded().instruction();
        let Some(found) =
            closure_constant(closure_instruction.opcode(), closure_instruction.operands())
        else {
            continue;
        };
        if found != constant {
            continue;
        }
        let put_instruction = instructions[index + 1].decoded().instruction();
        if !matches!(
            (put_instruction.opcode(), put_instruction.operands()),
            (FinalOpcode::PutVarRef, Operands::VarRef(slot)) if u32::from(slot) == closure_index
        ) {
            continue;
        }
        matched = matched.saturating_add(1);
        if matched == 1 {
            first_site = Some(RealmGlobalFunctionInitializerSite {
                closure_index: index,
                closure_pc: instructions[index].decoded().pc(),
            });
        }
    }
    if matched != 1 {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerOpcodeMismatch {
                closure: closure_index,
                constant,
                matches: matched,
            },
        ));
    }
    let Some(_site) = first_site else {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerOpcodeMismatch {
                closure: closure_index,
                constant,
                matches: matched,
            },
        ));
    };
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "closure provenance, storage policy, and initializer checks form one audited boundary"
)]
fn verify_closures(
    id: FunctionTemplateId,
    root: FunctionTemplateId,
    authority_kind: CompilerExecutableKind,
    function: &VerifiedCompilerFunction,
    closures: &[ClosureVariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    for (index, (closure, staged_source)) in
        closures.iter().zip(function.closure_sources()).enumerate()
    {
        if matches!(
            staged_source,
            CompilerClosureSource::DirectEvalBinding { .. }
                | CompilerClosureSource::DirectEvalVariable { .. }
        ) && (id != root || authority_kind != CompilerExecutableKind::DirectEvalScript)
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::DirectEvalBindingSourceRequiresDirectEvalScript {
                    closure: usize_to_u32(index),
                },
            ));
        }
        let slot = BindingSlot::Closure(usize_to_u32(index));
        verify_required_atom(
            id,
            closure.name,
            MetadataAtomField::ClosureName(usize_to_u32(index)),
            function,
        )?;
        let policy = closure.policy();
        let arguments_object_valid = !closure.arguments_object
            || (atom_contents(closure.name, function.atoms())
                .is_some_and(|name| name.code_units().eq("arguments".encode_utf16()))
                && policy.kind() == CompilerBindingKind::Var
                && policy.initialization()
                    == CompilerInitializationPolicy::UndefinedAtInstantiation
                && policy.writes() == CompilerWritePolicy::Mutable
                && !policy.has_temporal_dead_zone());
        let deletable_eval_variable_valid = match staged_source {
            CompilerClosureSource::DirectEvalVariable { .. } => closure.deletable_eval_variable,
            CompilerClosureSource::ParentClosure(_)
            | CompilerClosureSource::Module { .. } => true,
            CompilerClosureSource::ParentVariableReference(_)
            | CompilerClosureSource::ConstructorRealmGlobal(_)
            | CompilerClosureSource::DirectEvalBinding { .. } => !closure.deletable_eval_variable,
        };
        let module_source = matches!(
            staged_source,
            CompilerClosureSource::Module { .. }
        );
        let module_source_valid = if module_source {
            id == root && authority_kind == CompilerExecutableKind::Module
        } else {
            true
        };
        let binding_valid = match closure.binding {
            CompilerClosureBinding::Captured(policy) => {
                let module_function = module_source
                    && policy.kind() == CompilerBindingKind::Function
                    && policy.initialization()
                        == CompilerInitializationPolicy::FunctionAtInstantiation;
                policy.is_valid()
                    && arguments_object_valid
                    && deletable_eval_variable_valid
                    && module_source_valid
                    && policy.kind() != CompilerBindingKind::GlobalReference
                    && (module_function || closure.function_initializer.is_none())
                    && (matches!(
                        staged_source,
                        CompilerClosureSource::ParentVariableReference(_)
                            | CompilerClosureSource::ParentClosure(_)
                            | CompilerClosureSource::DirectEvalBinding { .. }
                            | CompilerClosureSource::DirectEvalVariable { .. }
                            | CompilerClosureSource::Module { .. }
                    ))
                    && (!matches!(
                        staged_source,
                        CompilerClosureSource::DirectEvalVariable { .. }
                    ) || (matches!(
                        policy.kind(),
                        CompilerBindingKind::Var | CompilerBindingKind::Function
                    ) && closure.name.is_some()))
                    && (!module_source || (id == root && !closure.arguments_object))
            }
            CompilerClosureBinding::RealmGlobal(_) => {
                !closure.arguments_object
                    && !closure.deletable_eval_variable
                    && realm_global_policy_supported(policy)
                    && (!matches!(
                        policy.kind(),
                        CompilerBindingKind::Let | CompilerBindingKind::Const
                    ) || authority_kind == CompilerExecutableKind::GlobalScript)
                    && match *staged_source {
                        CompilerClosureSource::ConstructorRealmGlobal(atom) => {
                            if id != root || !is_script_authority_kind(authority_kind) {
                                return Err(BytecodeVerificationError::function(
                                    id,
                                    BytecodeVerificationErrorKind::ConstructorRealmGlobalSourceRequiresDynamicFunctionScript {
                                        closure: usize_to_u32(index),
                                    },
                                ));
                            }
                            closure.name == Some(atom)
                        }
                        CompilerClosureSource::ParentClosure(_) => id != root,
                        CompilerClosureSource::ParentVariableReference(_)
                        | CompilerClosureSource::DirectEvalBinding { .. }
                        | CompilerClosureSource::DirectEvalVariable { .. }
                        | CompilerClosureSource::Module { .. } => false,
                    }
            }
        };
        if !binding_valid {
            return Err(policy_error(
                id,
                slot,
                None,
                BindingPolicyViolationReason::InvalidDeclarationPolicy,
            ));
        }
        let realm_global_function = matches!(
            closure.binding,
            CompilerClosureBinding::RealmGlobal(policy)
                if policy.kind() == CompilerBindingKind::Function
        );
        let originates_in_constructor_realm = matches!(
            staged_source,
            CompilerClosureSource::ConstructorRealmGlobal(_)
        );
        let module_function = module_source
            && matches!(
                closure.binding,
                CompilerClosureBinding::Captured(policy)
                    if policy.kind() == CompilerBindingKind::Function
                        && policy.initialization()
                            == CompilerInitializationPolicy::FunctionAtInstantiation
            );
        let initializer_valid = if module_function {
            module_function_initializer_is_valid(function, closure, true)
        } else {
            realm_global_function_initializer_is_valid(
                function,
                closure,
                realm_global_function,
                originates_in_constructor_realm,
            )
        };
        if !initializer_valid {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                    closure: usize_to_u32(index),
                    constant: closure.function_initializer,
                },
            ));
        }
        if module_function && id == root {
            verify_module_function_initializers(id, usize_to_u32(index), function, closure)?;
        }
        verify_realm_global_lexical_initializer_sites(
            id,
            usize_to_u32(index),
            function,
            closure,
            originates_in_constructor_realm,
        )?;
        if closure.source != *staged_source {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                    child: id,
                    closure: usize_to_u32(index),
                },
            ));
        }
    }
    Ok(())
}

fn realm_global_function_initializer_is_valid(
    function: &VerifiedCompilerFunction,
    closure: &ClosureVariableDefinition,
    realm_global_function: bool,
    originates_in_constructor_realm: bool,
) -> bool {
    match (
        realm_global_function,
        originates_in_constructor_realm,
        closure.function_initializer,
    ) {
        (true, true, Some(constant)) => matches!(
            function.constants().get(constant as usize),
            Some(crate::CompilerConstant::Function(_))
        ),
        (true, false, None) | (false, _, None) => true,
        (true, true, None) | (true, false, Some(_)) | (false, _, Some(_)) => false,
    }
}

fn verify_realm_global_lexical_initializer_sites(
    id: FunctionTemplateId,
    closure_index: u32,
    function: &VerifiedCompilerFunction,
    closure: &ClosureVariableDefinition,
    originates_in_constructor_realm: bool,
) -> Result<(), BytecodeVerificationError> {
    let realm_global_lexical = matches!(
        closure.binding,
        CompilerClosureBinding::RealmGlobal(policy)
            if matches!(policy.kind(), CompilerBindingKind::Let | CompilerBindingKind::Const)
    );
    if !realm_global_lexical {
        return Ok(());
    }
    let lexical_initializers = function
        .control_flow()
        .instructions()
        .iter()
        .filter(|instruction| {
            matches!(
                (
                    instruction.decoded().instruction().opcode(),
                    instruction.decoded().instruction().operands(),
                ),
                (FinalOpcode::PutVarInit, Operands::VarRef(slot))
                    if u32::from(slot) == closure_index
            )
        })
        .count();
    let expected = usize::from(originates_in_constructor_realm);
    if lexical_initializers != expected {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::RealmGlobalLexicalInitializerCountMismatch {
                closure: closure_index,
                matches: usize_to_u32(lexical_initializers),
            },
        ));
    }
    Ok(())
}

const fn realm_global_policy_supported(policy: CompilerBindingPolicy) -> bool {
    match policy.kind() {
        CompilerBindingKind::GlobalReference => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::ConstructorRealmLookup
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && !policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Var => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::UndefinedAtInstantiation
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && !policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Function => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::FunctionAtInstantiation
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && !policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Let => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::AtDeclaration
            ) && matches!(policy.writes(), CompilerWritePolicy::Mutable)
                && policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Const => {
            matches!(
                policy.initialization(),
                CompilerInitializationPolicy::AtDeclaration
            ) && matches!(policy.writes(), CompilerWritePolicy::Immutable)
                && policy.has_temporal_dead_zone()
        }
        CompilerBindingKind::Parameter
        | CompilerBindingKind::FunctionName
        | CompilerBindingKind::ClassName
        | CompilerBindingKind::ClassFieldKey
        | CompilerBindingKind::ClassInstanceInitializer
        | CompilerBindingKind::ClassPrivateName
        | CompilerBindingKind::ClassStaticReceiver
        | CompilerBindingKind::WithObject
        | CompilerBindingKind::Catch => false,
    }
}

const fn is_script_authority_kind(kind: CompilerExecutableKind) -> bool {
    matches!(
        kind,
        CompilerExecutableKind::GlobalScript
            | CompilerExecutableKind::IndirectEvalScript
            | CompilerExecutableKind::DirectEvalScript
            | CompilerExecutableKind::DynamicFunctionScript
    )
}

fn verify_optional_atom(
    id: FunctionTemplateId,
    atom: Option<AtomPoolIndex>,
    field: MetadataAtomField,
    function: &VerifiedCompilerFunction,
) -> Result<(), BytecodeVerificationError> {
    if let Some(atom) = atom {
        verify_atom_bounds(id, atom, field, function)?;
    }
    Ok(())
}

fn verify_required_atom(
    id: FunctionTemplateId,
    atom: Option<AtomPoolIndex>,
    field: MetadataAtomField,
    function: &VerifiedCompilerFunction,
) -> Result<(), BytecodeVerificationError> {
    let atom = atom.ok_or_else(|| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::MissingMetadataAtom { field },
        )
    })?;
    verify_atom_bounds(id, atom, field, function)
}

fn verify_atom_bounds(
    id: FunctionTemplateId,
    atom: AtomPoolIndex,
    field: MetadataAtomField,
    function: &VerifiedCompilerFunction,
) -> Result<(), BytecodeVerificationError> {
    let len = usize_to_u32(function.atoms().len());
    if atom.get() >= len {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::MetadataAtomOutOfBounds {
                field,
                index: atom.get(),
                len,
            },
        ));
    }
    if function
        .atoms()
        .get(atom.get() as usize)
        .is_some_and(crate::CompilerAtom::is_static_property_only)
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::StaticPropertyOnlyMetadataAtom {
                field,
                index: atom.get(),
            },
        ));
    }
    Ok(())
}
