#[allow(
    clippy::too_many_lines,
    reason = "binding opcode authority and exact initializer counts are checked in one pass"
)]
fn verify_binding_opcodes(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
    closures: &[ClosureVariableDefinition],
    internal_stack: &InternalStackCertificate,
) -> Result<(), BytecodeVerificationError> {
    let argument_count = flow.domains().argument_count() as usize;
    // A for-in/of loop rotation re-arms the head's non-captured TDZ cells at
    // the loop back edge (the `rotate` label targets exactly that
    // instruction), so a second scope activation is admitted only at a
    // backward jump target; straight-line repeated initialization stays
    // rejected.
    let instructions = flow.instructions();
    let mut back_edge_targets = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    for (index, verified) in instructions.iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::VariableDefinitions,
                    limit: u64::from(u32::MAX),
                    observed: u64::from(u32::MAX),
                },
            )
        })?;
        for target in [
            verified.successors().branch_target(),
            verified.successors().jump_target(),
        ] {
            if let Some(target) = target
                && target.get() < index
            {
                back_edge_targets[target.get() as usize] = true;
            }
        }
    }
    // A rotation emits one activation per cell, and the loop label targets
    // only the first, so extend the back-edge set over the contiguous
    // activation run that starts at the target.
    for index in 0..instructions.len().saturating_sub(1) {
        if back_edge_targets[index]
            && instructions[index + 1].decoded().instruction().opcode()
                == FinalOpcode::SetLocUninitialized
        {
            back_edge_targets[index + 1] = true;
        }
    }
    let mut scope_activations = try_filled_vec(
        id,
        variables.len() - argument_count,
        0_u8,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut catch_initializations = try_filled_vec(
        id,
        variables.len() - argument_count,
        0_u8,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    for (index, verified) in flow.instructions().iter().enumerate() {
        let decoded = verified.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if opcode == FinalOpcode::DeleteVar {
            let Operands::Atom(atom) = instruction.operands() else {
                continue;
            };
            let has_binding = closures.iter().any(|definition| {
                definition.name == Some(atom)
                    && (matches!(
                        definition.binding,
                        CompilerClosureBinding::RealmGlobal(policy)
                            if matches!(
                                policy.kind(),
                                CompilerBindingKind::GlobalReference
                                    | CompilerBindingKind::Var
                                    | CompilerBindingKind::Function
                            )
                    ) || definition.deletable_eval_variable)
            });
            if !has_binding {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::RealmGlobalDeleteBindingMissing {
                        pc: decoded.pc(),
                        atom,
                    },
                ));
            }
        } else if let Some(local) = local_operand(opcode, instruction.operands()) {
            let definition = &variables[argument_count + local as usize];
            verify_local_opcode(id, decoded.pc(), local, opcode, definition)?;
            if internal_stack.certifies_catch_local_put(index, local)
                && !definition.policy.temporal_dead_zone
            {
                if definition.policy.initialization != CompilerInitializationPolicy::Catch {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(decoded.pc()),
                        BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ));
                }
                let count = &mut catch_initializations[local as usize];
                *count = count.saturating_add(1);
                if *count > 1 {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(decoded.pc()),
                        BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ));
                }
            }
            if matches!(opcode, FinalOpcode::SetLocUninitialized) {
                let count = &mut scope_activations[local as usize];
                *count = count.saturating_add(1);
                // The iteration rotation is the single legitimate second
                // activation, and it must sit at the loop back-edge target.
                if *count > 2 || (*count == 2 && !back_edge_targets[index]) {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(decoded.pc()),
                        BindingPolicyViolationReason::InvalidLexicalInitialization,
                    ));
                }
            }
        } else if let Some(argument) = argument_operand(opcode, instruction.operands()) {
            let definition = &variables[argument as usize];
            if is_argument_write(opcode) && definition.policy.writes != CompilerWritePolicy::Mutable
            {
                return Err(policy_error(
                    id,
                    BindingSlot::Argument(argument),
                    Some(decoded.pc()),
                    BindingPolicyViolationReason::ImmutableWrite,
                ));
            }
        } else if let Some(closure) = closure_operand(opcode, instruction.operands()) {
            let definition = &closures[closure as usize];
            verify_closure_opcode(id, decoded.pc(), closure, opcode, definition)?;
        }
    }
    for (local, ((definition, activations), catch_initializations)) in variables[argument_count..]
        .iter()
        .zip(scope_activations)
        .zip(catch_initializations)
        .enumerate()
    {
        let requires_scope_activation = definition.policy.temporal_dead_zone
            || definition.policy.initialization
                == CompilerInitializationPolicy::FunctionAtScopeEntry;
        // A for-in/of loop rotation adds exactly one back-edge re-arm to the
        // entry activation, so both one and two activations are admitted.
        if requires_scope_activation && !(activations == 1 || activations == 2) {
            return Err(policy_error(
                id,
                BindingSlot::Local(usize_to_u32(local)),
                None,
                BindingPolicyViolationReason::MissingLexicalScopeInitialization,
            ));
        }
        let expected_catch_initializations = u8::from(
            definition.policy.initialization == CompilerInitializationPolicy::Catch
                && !definition.policy.temporal_dead_zone,
        );
        if definition.policy.initialization == CompilerInitializationPolicy::Catch
            && catch_initializations != expected_catch_initializations
        {
            return Err(policy_error(
                id,
                BindingSlot::Local(usize_to_u32(local)),
                None,
                BindingPolicyViolationReason::MissingLexicalScopeInitialization,
            ));
        }
    }
    Ok(())
}

fn verify_local_opcode(
    id: FunctionTemplateId,
    pc: BytecodePc,
    local: u32,
    opcode: FinalOpcode,
    definition: &VariableDefinition,
) -> Result<(), BytecodeVerificationError> {
    let slot = BindingSlot::Local(local);
    let tdz = definition.policy.temporal_dead_zone;
    if matches!(opcode, FinalOpcode::SetLocUninitialized) {
        if !tdz
            && definition.policy.initialization
                != CompilerInitializationPolicy::FunctionAtScopeEntry
        {
            return Err(policy_error(
                id,
                slot,
                Some(pc),
                BindingPolicyViolationReason::UnexpectedCheckedAccess,
            ));
        }
        return Ok(());
    }
    if is_checked_local(opcode) && !tdz {
        return Err(policy_error(
            id,
            slot,
            Some(pc),
            BindingPolicyViolationReason::UnexpectedCheckedAccess,
        ));
    }
    let runtime_checked_immutable_write = definition.policy.writes != CompilerWritePolicy::Mutable
        && ((tdz
            && matches!(
                opcode,
                FinalOpcode::PutLocCheck | FinalOpcode::PutLocCheckInit | FinalOpcode::SetLocCheck
            ))
            || (!tdz && definition.policy.kind == CompilerBindingKind::FunctionName));
    if is_local_write(opcode)
        && !matches!(opcode, FinalOpcode::SetLocUninitialized)
        && definition.policy.writes != CompilerWritePolicy::Mutable
        && !(tdz && is_unchecked_local_put(opcode))
        && !runtime_checked_immutable_write
    {
        return Err(policy_error(
            id,
            slot,
            Some(pc),
            BindingPolicyViolationReason::ImmutableWrite,
        ));
    }
    Ok(())
}

fn verify_closure_opcode(
    id: FunctionTemplateId,
    pc: BytecodePc,
    closure: u32,
    opcode: FinalOpcode,
    definition: &ClosureVariableDefinition,
) -> Result<(), BytecodeVerificationError> {
    match definition.binding {
        CompilerClosureBinding::Captured(_) if is_realm_global_opcode(opcode) => {
            if opcode == FinalOpcode::GetVarUndef && definition.deletable_eval_variable {
                return Ok(());
            }
            return Err(closure_opcode_mismatch(id, pc, closure, opcode));
        }
        CompilerClosureBinding::RealmGlobal(policy) => {
            if !is_realm_global_opcode(opcode) {
                return Err(closure_opcode_mismatch(id, pc, closure, opcode));
            }
            let allowed = match policy.kind() {
                CompilerBindingKind::GlobalReference
                | CompilerBindingKind::Var
                | CompilerBindingKind::Function => matches!(
                    opcode,
                    FinalOpcode::GetVarUndef | FinalOpcode::GetVar | FinalOpcode::PutVar
                ),
                CompilerBindingKind::Let | CompilerBindingKind::Const => matches!(
                    opcode,
                    FinalOpcode::GetVarUndef
                        | FinalOpcode::GetVar
                        | FinalOpcode::PutVar
                        | FinalOpcode::PutVarInit
                ),
                _ => false,
            };
            if !allowed {
                if opcode == FinalOpcode::PutVar && policy.writes() != CompilerWritePolicy::Mutable
                {
                    return Err(policy_error(
                        id,
                        BindingSlot::Closure(closure),
                        Some(pc),
                        BindingPolicyViolationReason::ImmutableWrite,
                    ));
                }
                return Err(closure_opcode_mismatch(id, pc, closure, opcode));
            }
            return Ok(());
        }
        CompilerClosureBinding::Captured(_) => {}
    }

    let slot = BindingSlot::Closure(closure);
    let policy = definition.policy();
    if opcode == FinalOpcode::MakeVarRefRef {
        // Reference creation fixes the environment cell but does not read or
        // write it. `get_ref_value` performs the mandatory TDZ check, while
        // `put_ref_value` applies the retained declaration policy at runtime.
        return Ok(());
    }
    let checked = matches!(
        opcode,
        FinalOpcode::GetVarRefCheck | FinalOpcode::PutVarRefCheck
    );
    if checked != policy.temporal_dead_zone {
        return Err(policy_error(
            id,
            slot,
            Some(pc),
            if checked {
                BindingPolicyViolationReason::UnexpectedCheckedAccess
            } else {
                BindingPolicyViolationReason::UncheckedTemporalDeadZoneAccess
            },
        ));
    }
    // Captured writes retain their declaration policy in the authority. The
    // VM uses it to throw for immutable bindings or ignore a sloppy write to
    // an ImmutableInStrictCode binding; these opcodes never grant an
    // unchecked mutation capability.
    Ok(())
}

const fn is_realm_global_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::GetVarUndef
            | FinalOpcode::GetVar
            | FinalOpcode::PutVar
            | FinalOpcode::PutVarInit
    )
}
fn closure_opcode_mismatch(
    id: FunctionTemplateId,
    pc: BytecodePc,
    closure: u32,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::ClosureBindingOpcodeMismatch {
            closure,
            pc,
            opcode,
        },
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "binding-state analysis requires the complete verified function and entry authority"
)]
fn verify_binding_states(
    id: FunctionTemplateId,
    graph: &VerifiedCompilerFunctionGraph,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
    initializers: &VerifiedFunctionInitializers,
    internal_stack: &InternalStackCertificate,
    realm_global_initializer_prefix: usize,
    prior_transfers: u64,
    transfer_limit: u64,
) -> Result<u64, BytecodeVerificationError> {
    let flow = function.control_flow();
    let arguments = flow.domains().argument_count() as usize;
    let mut tracked = Vec::new();
    tracked
        .try_reserve_exact(variables.len() - arguments)
        .map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AllocationFailed {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    requested: usize_to_u64(variables.len() - arguments),
                },
            )
        })?;
    tracked.extend(
        variables[arguments..]
            .iter()
            .enumerate()
            .filter_map(|(local, definition)| {
                requires_binding_state(definition).then_some((local, definition))
            }),
    );
    if tracked.is_empty() {
        return Ok(0);
    }
    let mut tracked_by_local = try_filled_vec(
        id,
        variables.len() - arguments,
        None,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    for (position, (local, _)) in tracked.iter().enumerate() {
        tracked_by_local[*local] = Some(position);
    }
    let instructions = flow.instructions();
    let state_cells = instructions
        .len()
        .checked_mul(tracked.len())
        .ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    limit: u64::MAX,
                    observed: u64::MAX,
                },
            )
        })?;
    let mut entries = try_filled_vec(
        id,
        state_cells,
        0_u8,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    for (entry, (_, definition)) in entries[..tracked.len()].iter_mut().zip(&tracked) {
        *entry = initial_binding_state(definition);
    }
    let mut entry_present = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    entry_present[0] = true;
    let mut queued = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    queued[0] = true;
    let mut work = VecDeque::new();
    work.try_reserve_exact(instructions.len()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource: BytecodeGraphResource::PolicyTransfers,
                requested: usize_to_u64(instructions.len()),
            },
        )
    })?;
    work.push_back(0_usize);
    let mut evaluations = 0_u64;
    while let Some(index) = work.pop_front() {
        queued[index] = false;
        charge_policy_transfers(
            id,
            &mut evaluations,
            usize_to_u64(tracked.len()),
            prior_transfers,
            transfer_limit,
        )?;
        let start = index * tracked.len();
        let state = &entries[start..start + tracked.len()];
        let mut state = try_copy_slice(id, state, BytecodeGraphResource::FrameStateEntries)?;
        if realm_global_initializer_prefix != 0 && index == initializers.entry_prefix_end {
            for (position, (local, _)) in tracked.iter().enumerate() {
                if state[position] & BindingState::INACTIVE_ACTIVE != 0 {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(usize_to_u32(*local)),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                    ));
                }
            }
        }
        let instruction = instructions[index].decoded().instruction();
        let opcode = instruction.opcode();
        if let Some(constant) = closure_constant(opcode, instruction.operands())
            && let Some(crate::CompilerConstant::Function(child_id)) =
                function.constants().get(constant as usize)
            && let Some(child) = graph.function(*child_id)
        {
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(child.closure_sources().len()),
                prior_transfers,
                transfer_limit,
            )?;
            let capture_layout = flow.compiler_capture_layout();
            for source in child.closure_sources() {
                let CompilerClosureSource::ParentVariableReference(reference) = *source else {
                    continue;
                };
                let Some(CompilerCapturedBinding::ScopedLocal(local)) = capture_layout
                    .and_then(|layout| layout.binding_for_variable_reference(reference))
                else {
                    continue;
                };
                let Some(position) = tracked_by_local[local as usize] else {
                    continue;
                };
                let certified_realm_global_initializer =
                    index < realm_global_initializer_prefix && index % 2 == 0;
                if state[position] & BindingState::INACTIVE != 0
                    && !certified_realm_global_initializer
                {
                    return Err(policy_error(
                        id,
                        BindingSlot::Local(local),
                        Some(instructions[index].decoded().pc()),
                        BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                    ));
                }
                state[position] = BindingState::with_active_cell(state[position]);
            }
        }
        let mut normal_completion_possible = true;
        if let Some(local) = local_operand(opcode, instruction.operands())
            && let Some(position) = tracked_by_local[local as usize]
        {
            let definition_index = arguments + local as usize;
            normal_completion_possible = transfer_local_state(
                id,
                instructions[index].decoded().pc(),
                local,
                opcode,
                tracked[position].1,
                initializers.put_definitions[index] == Some(definition_index),
                internal_stack.certifies_iteration_local_put(index, local),
                internal_stack.certifies_catch_local_put(index, local)
                    && !tracked[position].1.policy.temporal_dead_zone,
                &mut state[position],
            )?;
        }
        if !normal_completion_possible {
            continue;
        }
        for edge in internal_stack.effective_successors(instructions, index) {
            let successor = edge.target;
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(tracked.len()),
                prior_transfers,
                transfer_limit,
            )?;
            propagate_binding_state(
                successor,
                &state,
                &mut entries,
                &mut entry_present,
                tracked.len(),
                &mut queued,
                &mut work,
            );
        }
    }
    Ok(evaluations)
}

fn charge_policy_transfers(
    id: FunctionTemplateId,
    evaluated: &mut u64,
    amount: u64,
    prior: u64,
    limit: u64,
) -> Result<(), BytecodeVerificationError> {
    let Some(local) = evaluated.checked_add(amount) else {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::PolicyTransfers,
                limit,
                observed: u64::MAX,
            },
        ));
    };
    let Some(observed) = prior.checked_add(local) else {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::PolicyTransfers,
                limit,
                observed: u64::MAX,
            },
        ));
    };
    if observed > limit {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::PolicyTransfers,
                limit,
                observed,
            },
        ));
    }
    *evaluated = local;
    Ok(())
}

struct BindingState;

impl BindingState {
    const INACTIVE_CLOSED: u8 = 1 << 0;
    const INACTIVE_ACTIVE: u8 = 1 << 1;
    const UNINITIALIZED_CLOSED: u8 = 1 << 2;
    const UNINITIALIZED_ACTIVE: u8 = 1 << 3;
    const INITIALIZED_CLOSED: u8 = 1 << 4;
    const INITIALIZED_ACTIVE: u8 = 1 << 5;

    const INACTIVE: u8 = Self::INACTIVE_CLOSED | Self::INACTIVE_ACTIVE;
    const UNINITIALIZED: u8 = Self::UNINITIALIZED_CLOSED | Self::UNINITIALIZED_ACTIVE;
    const INITIALIZED: u8 = Self::INITIALIZED_CLOSED | Self::INITIALIZED_ACTIVE;
    const CLOSED: u8 =
        Self::INACTIVE_CLOSED | Self::UNINITIALIZED_CLOSED | Self::INITIALIZED_CLOSED;
    const ACTIVE: u8 =
        Self::INACTIVE_ACTIVE | Self::UNINITIALIZED_ACTIVE | Self::INITIALIZED_ACTIVE;
    const ENTRY: u8 = Self::INACTIVE_CLOSED;

    const fn only(state: u8, allowed: u8) -> bool {
        state != 0 && state & !allowed == 0
    }

    const fn with_uninitialized_value(state: u8) -> u8 {
        let mut output = 0;
        if state & Self::CLOSED != 0 {
            output |= Self::UNINITIALIZED_CLOSED;
        }
        if state & Self::ACTIVE != 0 {
            output |= Self::UNINITIALIZED_ACTIVE;
        }
        output
    }

    const fn with_initialized_value(state: u8) -> u8 {
        let mut output = 0;
        if state & Self::CLOSED != 0 {
            output |= Self::INITIALIZED_CLOSED;
        }
        if state & Self::ACTIVE != 0 {
            output |= Self::INITIALIZED_ACTIVE;
        }
        output
    }

    const fn with_closed_cell(state: u8) -> u8 {
        (state & Self::CLOSED) | ((state & Self::ACTIVE) >> 1)
    }

    const fn with_active_cell(state: u8) -> u8 {
        ((state & Self::CLOSED) << 1) | (state & Self::ACTIVE)
    }
}

fn requires_binding_state(definition: &VariableDefinition) -> bool {
    definition.policy.temporal_dead_zone
        || (definition.has_scope && definition.variable_reference.is_some())
        || definition.function_initializer.is_some()
        || definition.policy.initialization == CompilerInitializationPolicy::FunctionName
        || definition.policy.initialization == CompilerInitializationPolicy::Catch
}

fn initial_binding_state(definition: &VariableDefinition) -> u8 {
    if definition.policy.initialization == CompilerInitializationPolicy::FunctionName {
        if definition.variable_reference.is_some() {
            BindingState::INITIALIZED_ACTIVE
        } else {
            BindingState::INITIALIZED_CLOSED
        }
    } else {
        BindingState::ENTRY
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "binding identity, declaration policy, initializer authority, and iteration-head authority are checked together"
)]
fn transfer_local_state(
    id: FunctionTemplateId,
    pc: BytecodePc,
    local: u32,
    opcode: FinalOpcode,
    definition: &VariableDefinition,
    is_function_initializer: bool,
    is_iteration_head_put: bool,
    is_catch_initialization: bool,
    state: &mut u8,
) -> Result<bool, BytecodeVerificationError> {
    let slot = BindingSlot::Local(local);
    match opcode {
        FinalOpcode::SetLocUninitialized => {
            if definition.has_scope
                && definition.variable_reference.is_some()
                && *state & (BindingState::UNINITIALIZED_ACTIVE | BindingState::INITIALIZED_ACTIVE)
                    != 0
            {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            *state = if definition.has_scope && definition.variable_reference.is_some() {
                BindingState::UNINITIALIZED_ACTIVE
            } else {
                BindingState::with_uninitialized_value(*state)
            };
        }
        opcode if is_unchecked_local_put(opcode) => {
            let valid = if is_catch_initialization {
                definition.policy.initialization == CompilerInitializationPolicy::Catch
                    && BindingState::only(
                        *state,
                        BindingState::INACTIVE | BindingState::INITIALIZED_CLOSED,
                    )
            } else if is_function_initializer {
                match definition.policy.initialization {
                    CompilerInitializationPolicy::FunctionAtScopeEntry => {
                        BindingState::only(*state, BindingState::UNINITIALIZED)
                            && (definition.variable_reference.is_none()
                                || BindingState::only(*state, BindingState::UNINITIALIZED_ACTIVE))
                    }
                    CompilerInitializationPolicy::FunctionAtInstantiation
                    | CompilerInitializationPolicy::Argument => {
                        BindingState::only(*state, BindingState::INACTIVE)
                    }
                    _ => false,
                }
            } else if definition.function_initializer.is_some()
                && !BindingState::only(*state, BindingState::INITIALIZED)
            {
                false
            } else if is_iteration_head_put {
                BindingState::only(
                    *state,
                    BindingState::UNINITIALIZED | BindingState::INITIALIZED_CLOSED,
                )
            } else if definition.policy.kind == CompilerBindingKind::FunctionName {
                BindingState::only(*state, BindingState::INITIALIZED)
            } else if definition.policy.writes == CompilerWritePolicy::Mutable {
                *state & BindingState::INACTIVE == 0
            } else {
                BindingState::only(*state, BindingState::UNINITIALIZED)
            };
            if !valid {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            *state = if is_catch_initialization
                && definition.has_scope
                && definition.variable_reference.is_some()
            {
                BindingState::INITIALIZED_ACTIVE
            } else {
                BindingState::with_initialized_value(*state)
            };
        }
        FinalOpcode::PutLocCheckInit => {
            if !BindingState::only(*state, BindingState::UNINITIALIZED) {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::InvalidLexicalInitialization,
                ));
            }
            *state = BindingState::with_initialized_value(*state);
        }
        FinalOpcode::GetLocCheck | FinalOpcode::PutLocCheck | FinalOpcode::SetLocCheck => {
            if *state & BindingState::INACTIVE != 0 {
                return Err(policy_error(
                    id,
                    slot,
                    Some(pc),
                    BindingPolicyViolationReason::MissingLexicalScopeInitialization,
                ));
            }
            let normal = *state & BindingState::INITIALIZED;
            if normal == 0 {
                return Ok(false);
            }
            *state = normal;
        }
        FinalOpcode::CloseLoc => {
            *state = BindingState::with_closed_cell(*state);
        }
        opcode
            if (is_local_read(opcode) || is_local_write(opcode))
                && !BindingState::only(*state, BindingState::INITIALIZED) =>
        {
            return Err(policy_error(
                id,
                slot,
                Some(pc),
                BindingPolicyViolationReason::UncheckedTemporalDeadZoneAccess,
            ));
        }
        _ => {}
    }
    Ok(true)
}

fn propagate_binding_state(
    successor: InstructionIndex,
    output: &[u8],
    entries: &mut [u8],
    entry_present: &mut [bool],
    state_width: usize,
    queued: &mut [bool],
    work: &mut VecDeque<usize>,
) {
    let index = successor.get() as usize;
    let start = index * state_width;
    let existing = &mut entries[start..start + state_width];
    let changed = if entry_present[index] {
        let mut changed = false;
        for (target, incoming) in existing.iter_mut().zip(output) {
            let merged = *target | *incoming;
            changed |= merged != *target;
            *target = merged;
        }
        changed
    } else {
        existing.copy_from_slice(output);
        entry_present[index] = true;
        true
    };
    if changed && !queued[index] {
        queued[index] = true;
        work.push_back(index);
    }
}

const fn local_operand(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match operands {
        Operands::Loc(index) => Some(index as u32),
        Operands::Loc8(index) => Some(index as u32),
        Operands::NoneLoc => implied_local_index(opcode),
        _ => None,
    }
}

const fn argument_operand(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match operands {
        Operands::Arg(index) => Some(index as u32),
        Operands::NoneArg => implied_argument_index(opcode),
        _ => None,
    }
}

const fn closure_operand(opcode: FinalOpcode, operands: Operands) -> Option<u32> {
    match operands {
        Operands::VarRef(index) => Some(index as u32),
        Operands::NoneVarRef => implied_closure_index(opcode),
        _ => None,
    }
}

const fn implied_local_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetLoc0 | FinalOpcode::PutLoc0 | FinalOpcode::SetLoc0 => Some(0),
        FinalOpcode::GetLoc1 | FinalOpcode::PutLoc1 | FinalOpcode::SetLoc1 => Some(1),
        FinalOpcode::GetLoc2 | FinalOpcode::PutLoc2 | FinalOpcode::SetLoc2 => Some(2),
        FinalOpcode::GetLoc3 | FinalOpcode::PutLoc3 | FinalOpcode::SetLoc3 => Some(3),
        _ => None,
    }
}

const fn implied_argument_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetArg0 | FinalOpcode::PutArg0 | FinalOpcode::SetArg0 => Some(0),
        FinalOpcode::GetArg1 | FinalOpcode::PutArg1 | FinalOpcode::SetArg1 => Some(1),
        FinalOpcode::GetArg2 | FinalOpcode::PutArg2 | FinalOpcode::SetArg2 => Some(2),
        FinalOpcode::GetArg3 | FinalOpcode::PutArg3 | FinalOpcode::SetArg3 => Some(3),
        _ => None,
    }
}

const fn implied_closure_index(opcode: FinalOpcode) -> Option<u32> {
    match opcode {
        FinalOpcode::GetVarRef0 | FinalOpcode::PutVarRef0 | FinalOpcode::SetVarRef0 => Some(0),
        FinalOpcode::GetVarRef1 | FinalOpcode::PutVarRef1 | FinalOpcode::SetVarRef1 => Some(1),
        FinalOpcode::GetVarRef2 | FinalOpcode::PutVarRef2 | FinalOpcode::SetVarRef2 => Some(2),
        FinalOpcode::GetVarRef3 | FinalOpcode::PutVarRef3 | FinalOpcode::SetVarRef3 => Some(3),
        _ => None,
    }
}

const fn is_checked_local(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::GetLocCheck
            | FinalOpcode::PutLocCheck
            | FinalOpcode::PutLocCheckInit
            | FinalOpcode::SetLocCheck
    )
}

const fn is_unchecked_local_put(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    )
}

const fn is_local_write(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutLoc
            | FinalOpcode::SetLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::SetLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
            | FinalOpcode::SetLoc0
            | FinalOpcode::SetLoc1
            | FinalOpcode::SetLoc2
            | FinalOpcode::SetLoc3
            | FinalOpcode::PutLocCheck
            | FinalOpcode::PutLocCheckInit
            | FinalOpcode::SetLocCheck
            | FinalOpcode::SetLocUninitialized
    )
}

const fn is_local_read(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::GetLoc
            | FinalOpcode::GetLoc8
            | FinalOpcode::GetLoc0
            | FinalOpcode::GetLoc1
            | FinalOpcode::GetLoc2
            | FinalOpcode::GetLoc3
            | FinalOpcode::GetLocCheck
    )
}

const fn is_argument_write(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutArg
            | FinalOpcode::SetArg
            | FinalOpcode::PutArg0
            | FinalOpcode::PutArg1
            | FinalOpcode::PutArg2
            | FinalOpcode::PutArg3
            | FinalOpcode::SetArg0
            | FinalOpcode::SetArg1
            | FinalOpcode::SetArg2
            | FinalOpcode::SetArg3
    )
}

fn policy_error(
    id: FunctionTemplateId,
    slot: BindingSlot,
    pc: Option<BytecodePc>,
    reason: BindingPolicyViolationReason,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::BindingPolicyViolation { slot, pc, reason },
    )
}
