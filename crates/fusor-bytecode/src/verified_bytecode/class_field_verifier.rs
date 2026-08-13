/// Certifies the synthetic cell that retains one computed public field key.
/// The cell is not source-addressable: it has one lexical activation and one
/// immediately-post-`to_prop_key` initialization. An instance key is captured
/// once by its constructor; a static key is instead read once by the class
/// definition after all element keys have been evaluated.
#[allow(
    clippy::too_many_lines,
    reason = "local initialization and every parent-child capture edge are one certificate"
)]
fn verify_class_field_key_bindings(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
) -> Result<(), BytecodeVerificationError> {
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let parent_metadata = &metadata[parent_index];
        let arguments = parent.control_flow().domains().argument_count() as usize;
        let mut captures = try_filled_vec(
            parent_id,
            parent_metadata.variables.len(),
            0_u32,
            BytecodeGraphResource::VariableDefinitions,
        )?;
        let mut direct_reads = try_filled_vec(
            parent_id,
            parent_metadata.variables.len(),
            0_u32,
            BytecodeGraphResource::VariableDefinitions,
        )?;

        for (definition_index, definition) in parent_metadata.variables.iter().enumerate() {
            if definition.policy.kind() != CompilerBindingKind::ClassFieldKey {
                continue;
            }
            let Some(local) = definition_index
                .checked_sub(arguments)
                .and_then(|index| u32::try_from(index).ok())
            else {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Argument(usize_to_u32(definition_index)),
                    None,
                    BindingPolicyViolationReason::InvalidDeclarationPolicy,
                ));
            };
            if !definition.has_scope || definition.function_initializer.is_some() {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Local(local),
                    None,
                    BindingPolicyViolationReason::InvalidDeclarationPolicy,
                ));
            }

            let instructions = parent.control_flow().instructions();
            let mut initialization = None;
            let mut initialization_count = 0_u32;
            let mut direct_read_count = 0_u32;
            for index in 0..instructions.len() {
                let instruction = instructions[index].decoded().instruction();
                if local_operand(instruction.opcode(), instruction.operands()) != Some(local) {
                    continue;
                }
                if instruction.opcode() == FinalOpcode::GetLocCheck {
                    direct_read_count = direct_read_count.saturating_add(1);
                    continue;
                }
                if instruction.opcode() == FinalOpcode::SetLocUninitialized {
                    continue;
                }
                if instruction.opcode() == FinalOpcode::CloseLoc {
                    continue;
                }
                if !is_unchecked_local_put(instruction.opcode()) || index == 0 {
                    return Err(policy_error(
                        parent_id,
                        BindingSlot::Local(local),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::InvalidDeclarationPolicy,
                    ));
                }
                initialization_count = initialization_count.saturating_add(1);
                let prior = instructions[index - 1].decoded().instruction();
                if prior.opcode() != FinalOpcode::ToPropKey
                    || !parent_metadata.internal_stack.has_effective_successor(
                        instructions,
                        index - 1,
                        usize_to_u32(index),
                    )
                {
                    return Err(policy_error(
                        parent_id,
                        BindingSlot::Local(local),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ));
                }
                initialization = Some(index);
            }
            if initialization_count != 1 {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Local(local),
                    initialization.and_then(|index| {
                        instructions
                            .get(index)
                            .map(|instruction| instruction.decoded().pc())
                    }),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            direct_reads[definition_index] = direct_read_count;
        }

        for constant in parent.constants() {
            let crate::CompilerConstant::Function(child_id) = constant else {
                continue;
            };
            let child_index = usize::try_from(child_id.get()).map_err(|_| {
                BytecodeVerificationError::function(
                    *child_id,
                    BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                        child: *child_id,
                        closure: 0,
                    },
                )
            })?;
            let child = graph.function(*child_id).ok_or_else(|| {
                BytecodeVerificationError::function(
                    *child_id,
                    BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                        child: *child_id,
                        closure: 0,
                    },
                )
            })?;
            let child_metadata = &metadata[child_index];
            for (closure_index, (closure, source)) in child_metadata
                .closures
                .iter()
                .zip(child.closure_sources())
                .enumerate()
            {
                if closure.policy().kind() != CompilerBindingKind::ClassFieldKey {
                    continue;
                }
                let CompilerClosureSource::ParentVariableReference(reference) = *source else {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                };
                let Some(CompilerCapturedBinding::ScopedLocal(local)) = parent
                    .control_flow()
                    .compiler_capture_layout()
                    .and_then(|layout| layout.binding_for_variable_reference(reference))
                else {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                };
                let Some(definition_index) =
                    arguments.checked_add(local as usize).filter(|&index| {
                        parent_metadata
                            .variables
                            .get(index)
                            .is_some_and(|definition| {
                                definition.policy.kind() == CompilerBindingKind::ClassFieldKey
                            })
                    })
                else {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                };
                if child_metadata.executable_kind
                    != CompilerExecutableKind::ClassInstanceInitializer
                {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                }
                captures[definition_index] = captures[definition_index].saturating_add(1);
            }
        }

        for (definition_index, definition) in parent_metadata.variables.iter().enumerate() {
            if definition.policy.kind() != CompilerBindingKind::ClassFieldKey {
                continue;
            }
            let local = definition_index
                .checked_sub(arguments)
                .and_then(|index| u32::try_from(index).ok())
                .ok_or_else(|| {
                    policy_error(
                        parent_id,
                        BindingSlot::Argument(usize_to_u32(definition_index)),
                        None,
                        BindingPolicyViolationReason::InvalidDeclarationPolicy,
                    )
                })?;
            let valid_use = if definition.variable_reference.is_some() {
                captures[definition_index] == 1 && direct_reads[definition_index] == 0
            } else {
                captures[definition_index] == 0 && direct_reads[definition_index] == 1
            };
            if !valid_use {
                return Err(policy_error(
                    parent_id,
                    BindingSlot::Local(local),
                    None,
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
        }
    }
    Ok(())
}
#[allow(
    clippy::too_many_lines,
    reason = "typed class/method closure pairing, unique CFG entry, arity, and ownership form one definition certificate"
)]
fn verify_method_definitions(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let mut method_definition_counts = try_filled_vec(
        graph.root_id(),
        graph.functions().len(),
        0_u32,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    let mut class_definition_counts = try_filled_vec(
        graph.root_id(),
        graph.functions().len(),
        0_u32,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    let mut instance_initializer_counts = try_filled_vec(
        graph.root_id(),
        graph.functions().len(),
        0_u32,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let instructions = parent.control_flow().instructions();
        let internal_stack = &metadata[parent_index].internal_stack;
        let mut predecessor_counts = try_filled_vec(
            parent_id,
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

        for (index, verified) in instructions.iter().enumerate() {
            let decoded = verified.decoded();
            let instruction = decoded.instruction();
            if is_method_definition_opcode(instruction.opcode())
                && method_definition_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::DefineMethodTemplateMismatch {
                        pc: decoded.pc(),
                    },
                ));
            }
            if is_class_definition_opcode(instruction.opcode())
                && class_definition_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::DefineClassTemplateMismatch { pc: decoded.pc() },
                ));
            }
            if instruction.opcode() == FinalOpcode::CheckCtor {
                let default_constructor_check = metadata[parent_index].executable_kind
                    == CompilerExecutableKind::ClassConstructor
                    && parent
                        .control_flow()
                        .function_header()
                        .flags()
                        .is_derived_class_constructor()
                    && index == 0
                    && derived_default_constructor_pair(
                        parent,
                        &metadata[parent_index],
                        &predecessor_counts,
                        internal_stack,
                    );
                let heritage_check = index.checked_add(6).is_some_and(|definition_index| {
                    matches!(
                        instructions.get(definition_index).map(|instruction| {
                            let instruction = instruction.decoded().instruction();
                            (instruction.opcode(), instruction.operands())
                        }),
                        Some((
                            FinalOpcode::DefineClass,
                            Operands::AtomU8 { value, .. },
                        )) if value & 1 != 0
                    ) && class_definition_pair(
                        graph,
                        parent,
                        metadata,
                        instructions,
                        &predecessor_counts,
                        internal_stack,
                        definition_index,
                    )
                    .is_some()
                });
                if !default_constructor_check && !heritage_check {
                    return Err(BytecodeVerificationError::function(
                        parent_id,
                        BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                            pc: decoded.pc(),
                            opcode: instruction.opcode(),
                        },
                    ));
                }
            }
            if instruction.opcode() == FinalOpcode::InitCtor
                && !(metadata[parent_index].executable_kind
                    == CompilerExecutableKind::ClassConstructor
                    && parent
                        .control_flow()
                        .function_header()
                        .flags()
                        .is_derived_class_constructor()
                    && derived_default_constructor_pair(
                        parent,
                        &metadata[parent_index],
                        &predecessor_counts,
                        internal_stack,
                    ))
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                        pc: decoded.pc(),
                        opcode: instruction.opcode(),
                    },
                ));
            }

            let Some(constant) = closure_constant(instruction.opcode(), instruction.operands())
            else {
                continue;
            };
            let Some(crate::CompilerConstant::Function(child)) =
                parent.constants().get(constant as usize)
            else {
                continue;
            };
            let Some(child_metadata) = usize::try_from(child.get())
                .ok()
                .and_then(|index| metadata.get(index))
            else {
                continue;
            };
            if child_metadata.executable_kind == CompilerExecutableKind::ClassInstanceInitializer {
                if class_instance_initializer_pair(
                    parent,
                    &metadata[parent_index],
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                ) != Some(*child)
                {
                    return Err(BytecodeVerificationError::function(
                        parent_id,
                        BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch {
                            pc: decoded.pc(),
                            child: *child,
                        },
                    ));
                }
                let child_index = usize::try_from(child.get()).map_err(|_| {
                    BytecodeVerificationError::function(
                        *child,
                        BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch {
                            pc: decoded.pc(),
                            child: *child,
                        },
                    )
                })?;
                instance_initializer_counts[child_index] =
                    instance_initializer_counts[child_index].saturating_add(1);
                continue;
            }
            if child_metadata.executable_kind == CompilerExecutableKind::ClassConstructor {
                let pair = index.checked_add(1).and_then(|definition_index| {
                    class_definition_pair(
                        graph,
                        parent,
                        metadata,
                        instructions,
                        &predecessor_counts,
                        internal_stack,
                        definition_index,
                    )
                });
                if pair != Some(*child) {
                    return Err(BytecodeVerificationError::function(
                        parent_id,
                        BytecodeVerificationErrorKind::DefineClassTemplateMismatch {
                            pc: decoded.pc(),
                        },
                    ));
                }
                let child_index = usize::try_from(child.get()).map_err(|_| {
                    BytecodeVerificationError::function(
                        *child,
                        BytecodeVerificationErrorKind::DefineClassTemplateMismatch {
                            pc: decoded.pc(),
                        },
                    )
                })?;
                let count = &mut class_definition_counts[child_index];
                *count = count.saturating_add(1);
                continue;
            }
            if !matches!(
                child_metadata.executable_kind,
                CompilerExecutableKind::OrdinaryMethod
                    | CompilerExecutableKind::GeneratorMethod
                    | CompilerExecutableKind::AsyncMethod
                    | CompilerExecutableKind::AsyncGeneratorMethod
            ) {
                continue;
            }
            let pair = index.checked_add(1).and_then(|definition_index| {
                method_definition_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    definition_index,
                )
            });
            let private_method = [4_usize, 10].into_iter().find_map(|offset| {
                index.checked_add(offset).and_then(|set_name_index| {
                    private_method_name_pair(
                        parent,
                        metadata,
                        instructions,
                        &predecessor_counts,
                        internal_stack,
                        set_name_index,
                    )
                })
            });
            if pair.map(|(defined, _)| defined) != Some(*child) && private_method != Some(*child) {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch {
                        pc: decoded.pc(),
                        child: *child,
                    },
                ));
            }
            let child_index = usize::try_from(child.get()).map_err(|_| {
                BytecodeVerificationError::function(
                    *child,
                    BytecodeVerificationErrorKind::OrdinaryMethodTemplatePlacementMismatch {
                        pc: decoded.pc(),
                        child: *child,
                    },
                )
            })?;
            let count = &mut method_definition_counts[child_index];
            *count = count.saturating_add(1);
        }
        if instructions.iter().any(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::ArrayFrom
                    | FinalOpcode::DefineMethod
                    | FinalOpcode::DefineMethodComputed
                    | FinalOpcode::DefineClass
                    | FinalOpcode::CopyDataProperties
                    | FinalOpcode::DefineArrayEl
                    | FinalOpcode::Append
                    | FinalOpcode::Dup1
            )
        }) {
            verify_object_definition_provenance(
                parent_id,
                parent,
                &metadata[parent_index],
                internal_stack,
                limits,
                usage,
            )?;
        }
    }

    for (index, (metadata, &definitions)) in
        metadata.iter().zip(&method_definition_counts).enumerate()
    {
        if !matches!(
            metadata.executable_kind,
            CompilerExecutableKind::OrdinaryMethod
                | CompilerExecutableKind::GeneratorMethod
                | CompilerExecutableKind::AsyncMethod
                | CompilerExecutableKind::AsyncGeneratorMethod
        ) {
            continue;
        }
        let child = function_id(index)?;
        if definitions != 1 {
            return Err(BytecodeVerificationError::function(
                child,
                BytecodeVerificationErrorKind::OrdinaryMethodTemplateOwnershipMismatch {
                    child,
                    definitions,
                },
            ));
        }
    }
    for (index, (metadata, &definitions)) in
        metadata.iter().zip(&class_definition_counts).enumerate()
    {
        if metadata.executable_kind != CompilerExecutableKind::ClassConstructor {
            continue;
        }
        let child = function_id(index)?;
        if definitions != 1 {
            return Err(BytecodeVerificationError::function(
                child,
                BytecodeVerificationErrorKind::ClassConstructorTemplateOwnershipMismatch {
                    child,
                    definitions,
                },
            ));
        }
    }
    for (index, (metadata, &definitions)) in metadata
        .iter()
        .zip(&instance_initializer_counts)
        .enumerate()
    {
        if metadata.executable_kind != CompilerExecutableKind::ClassInstanceInitializer {
            continue;
        }
        let child = function_id(index)?;
        if definitions != 1 {
            return Err(BytecodeVerificationError::function(
                child,
                BytecodeVerificationErrorKind::OrdinaryMethodTemplateOwnershipMismatch {
                    child,
                    definitions,
                },
            ));
        }
    }
    Ok(())
}
