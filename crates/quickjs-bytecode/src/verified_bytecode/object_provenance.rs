//! Fresh-object, class-definition, and linear array-append provenance certification.

use super::{
    BytecodeGraphResource, BytecodeGraphUsage, BytecodeGraphVerificationLimits, BytecodePc,
    BytecodeVerificationError, BytecodeVerificationErrorKind, CertifiedNipCatchTransform,
    CompilerBindingKind, CompilerExecutableKind, FinalOpcode, FunctionTemplateId, InstructionIndex,
    InternalStackCertificate, Operands, VecDeque, VerifiedCompilerFunction,
    VerifiedFunctionMetadata, charge, charge_policy_transfers, closure_operand, local_operand,
    try_copy_slice, try_filled_vec, usize_to_u32, usize_to_u64,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectDefinitionProvenance {
    Unknown,
    LiteralUndefined,
    FreshObject(u32),
    ClassConstructor(u32),
    ClassPrototype(u32),
    /// `this` in a class constructor. It is accepted as a dynamic
    /// `define_array_el` target only with a verified class-field key cell.
    ClassFieldReceiver(u32),
    FreshArray {
        site: u32,
        minimum_cursor: u32,
    },
    ArrayCursorCandidate {
        site: u32,
        value: u32,
    },
    AppendDestination(u32),
    CheckedAppendCursor(u32),
    AppendCursorAfterElision(u32),
    AppendCursorNeedsIncrement(u32),
    AppendLengthTarget(u32),
    AppendLengthCursor(u32),
    ConvertedPropertyKey(u32),
    /// A property key that class definition evaluation converted exactly once
    /// and retained in a compiler-only class-field-key cell.
    ClassFieldKey(u32),
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded CFG worklist and exact operand-stack transfer form one fresh-object certificate"
)]
pub(super) fn verify_object_definition_provenance(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    metadata: &VerifiedFunctionMetadata,
    internal_stack: &InternalStackCertificate,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let instructions = function.control_flow().instructions();
    let mut entries = try_filled_vec(
        id,
        instructions.len(),
        None::<Vec<ObjectDefinitionProvenance>>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut queued = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::PolicyTransfers,
    )?;
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

    let mut next_seed = 0_usize;
    let mut evaluations = 0_u64;
    loop {
        if work.is_empty() {
            while entries.get(next_seed).is_some_and(Option::is_some)
                || internal_stack.is_finally_target(next_seed)
            {
                next_seed = next_seed.saturating_add(1);
            }
            if next_seed == entries.len() {
                break;
            }
            entries[next_seed] = Some(Vec::new());
            queued[next_seed] = true;
            work.push_back(next_seed);
        }

        let Some(index) = work.pop_front() else {
            continue;
        };
        queued[index] = false;
        let entry = entries[index]
            .as_deref()
            .ok_or_else(|| object_definition_error(id, instructions[index].decoded().pc()))?;
        charge_policy_transfers(
            id,
            &mut evaluations,
            usize_to_u64(entry.len()).saturating_add(1),
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let mut state = try_copy_slice(id, entry, BytecodeGraphResource::FrameStateEntries)?;
        let decoded = instructions[index].decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if state.iter().copied().any(is_append_length_marker)
            && (opcode != FinalOpcode::PutField
                || !is_append_length_finalizer(function, instruction.operands(), &state))
        {
            return Err(append_stack_error(id, decoded.pc(), opcode));
        }
        if state.iter().any(|value| {
            matches!(
                value,
                ObjectDefinitionProvenance::AppendCursorNeedsIncrement(_)
            )
        }) && (opcode != FinalOpcode::Inc
            || append_pair_needing_increment_at_top(&state).is_none())
        {
            return Err(append_stack_error(id, decoded.pc(), opcode));
        }
        verify_linear_append_inputs(id, decoded, function, &state)?;
        match opcode {
            FinalOpcode::DefineClass => match instruction.operands() {
                Operands::AtomU8 { value, .. }
                    if value & 1 == 0
                        && !matches!(
                            state.get(state.len().saturating_sub(2)),
                            Some(ObjectDefinitionProvenance::LiteralUndefined)
                        ) =>
                {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::DefineClassTemplateMismatch {
                            pc: decoded.pc(),
                        },
                    ));
                }
                Operands::AtomU8 { value, .. } if value & 1 != 0 && state.len() < 3 => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::DefineClassTemplateMismatch {
                            pc: decoded.pc(),
                        },
                    ));
                }
                Operands::AtomU8 { value: 0..=3, .. } => {}
                _ => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::DefineClassTemplateMismatch {
                            pc: decoded.pc(),
                        },
                    ));
                }
            },
            FinalOpcode::DefineMethod => {
                let Operands::AtomU8 { value: flags, .. } = instruction.operands() else {
                    return Err(method_target_error(id, decoded.pc()));
                };
                if !method_target_matches_enumerability(
                    state.get(state.len().saturating_sub(2)),
                    flags,
                ) {
                    return Err(method_target_error(id, decoded.pc()));
                }
            }
            FinalOpcode::DefineMethodComputed => {
                let Operands::U8(flags) = instruction.operands() else {
                    return Err(method_target_error(id, decoded.pc()));
                };
                if !method_target_matches_enumerability(
                    state.get(state.len().saturating_sub(3)),
                    flags,
                ) {
                    return Err(method_target_error(id, decoded.pc()));
                }
            }
            FinalOpcode::CopyDataProperties => {
                let Some(target) =
                    copy_data_properties_target_index(&state, instruction.operands())
                else {
                    return Err(object_definition_error(id, decoded.pc()));
                };
                if !matches!(
                    state.get(target),
                    Some(ObjectDefinitionProvenance::FreshObject(_))
                ) {
                    return Err(object_definition_error(id, decoded.pc()));
                }
            }
            FinalOpcode::DefineArrayEl => {
                let object = state.get(state.len().saturating_sub(3));
                let key = state.get(state.len().saturating_sub(2));
                let object_literal = matches!(
                    (object, key),
                    (
                        Some(ObjectDefinitionProvenance::FreshObject(object_site)),
                        Some(ObjectDefinitionProvenance::ConvertedPropertyKey(key_site))
                    ) if object_site == key_site
                );
                let static_class_field = matches!(
                    (object, key),
                    (
                        Some(ObjectDefinitionProvenance::ClassConstructor(_)),
                        Some(ObjectDefinitionProvenance::ConvertedPropertyKey(_))
                    )
                );
                let computed_instance_field = matches!(
                    (object, key),
                    (
                        Some(ObjectDefinitionProvenance::ClassFieldReceiver(_)),
                        Some(ObjectDefinitionProvenance::ClassFieldKey(_))
                    )
                );
                let computed_static_field = matches!(
                    (object, key),
                    (
                        Some(ObjectDefinitionProvenance::ClassConstructor(_)),
                        Some(ObjectDefinitionProvenance::ClassFieldKey(_))
                    )
                );
                if !object_literal
                    && !static_class_field
                    && !computed_instance_field
                    && !computed_static_field
                    && append_pair_for_element(&state).is_none()
                {
                    return Err(define_array_element_key_error(id, decoded.pc()));
                }
            }
            FinalOpcode::DefineField
                if matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(ObjectDefinitionProvenance::FreshArray { .. })
                ) =>
            {
                let Some(index) = static_array_index(function, instruction.operands()) else {
                    return Err(append_stack_error(id, decoded.pc(), opcode));
                };
                let Some(ObjectDefinitionProvenance::FreshArray { minimum_cursor, .. }) =
                    state.get(state.len() - 2)
                else {
                    return Err(append_stack_error(id, decoded.pc(), opcode));
                };
                if index < *minimum_cursor {
                    return Err(append_stack_error(id, decoded.pc(), opcode));
                }
            }
            FinalOpcode::DefinePrivateField
                if !matches!(
                    state.get(state.len().saturating_sub(3)),
                    Some(
                        ObjectDefinitionProvenance::ClassConstructor(_)
                            | ObjectDefinitionProvenance::ClassFieldReceiver(_)
                    )
                ) =>
            {
                return Err(object_definition_error(id, decoded.pc()));
            }
            FinalOpcode::Append if append_pair_for_append(&state).is_none() => {
                return Err(append_stack_error(id, decoded.pc(), opcode));
            }
            FinalOpcode::Dup1 if trailing_elision_pair_at_top(&state).is_none() => {
                return Err(append_stack_error(id, decoded.pc(), opcode));
            }
            _ => {}
        }
        if !transfer_object_definition_provenance(
            id,
            index,
            decoded,
            internal_stack.nip_catch_transform(index),
            function,
            metadata,
            &mut state,
        )? {
            continue;
        }

        let mut has_successor = false;
        for edge in internal_stack.effective_successors(instructions, index) {
            has_successor = true;
            let successor = edge.target;
            if edge.enters_finally {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(ObjectDefinitionProvenance::Unknown);
            }
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(state.len()).saturating_add(1),
                usage.policy_transfers,
                limits.max_policy_transfers,
            )?;
            propagate_object_definition_provenance(
                id,
                decoded.pc(),
                successor,
                instructions[successor.get() as usize].decoded().pc(),
                &state,
                &mut entries,
                &mut queued,
                &mut work,
                limits.max_frame_state_entries,
                usage,
            )?;
            if edge.enters_finally {
                state.pop();
            }
        }
        let abandons_generator_expression = decoded.instruction().opcode()
            == FinalOpcode::ReturnAsync
            && matches!(
                metadata.executable_kind,
                CompilerExecutableKind::GeneratorFunction
                    | CompilerExecutableKind::GeneratorMethod
                    | CompilerExecutableKind::AsyncGeneratorFunction
                    | CompilerExecutableKind::AsyncGeneratorMethod
            );
        if !has_successor
            && state.iter().copied().any(is_append_provenance)
            && !abandons_generator_expression
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AppendMarkerAtExit { pc: decoded.pc() },
            ));
        }
    }
    charge(
        &mut usage.policy_transfers,
        evaluations,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "fresh object definitions and the exact array-append state machine share one stack transfer boundary"
)]
fn transfer_object_definition_provenance(
    id: FunctionTemplateId,
    instruction_index: usize,
    decoded: crate::DecodedInstruction,
    nip_catch_transform: Option<CertifiedNipCatchTransform>,
    function: &VerifiedCompilerFunction,
    metadata: &VerifiedFunctionMetadata,
    state: &mut Vec<ObjectDefinitionProvenance>,
) -> Result<bool, BytecodeVerificationError> {
    let instruction = decoded.instruction();
    if instruction.opcode() == FinalOpcode::NipCatch {
        apply_nip_catch_provenance(id, decoded.pc(), nip_catch_transform, state)?;
        return Ok(true);
    }
    let effect = instruction
        .stack_effect()
        .map_err(|_| object_definition_error(id, decoded.pc()))?;
    let pops = effect.pops() as usize;
    let pushes = effect.pushes() as usize;
    if state.len() < pops {
        return Ok(false);
    }
    let output_len = state
        .len()
        .checked_sub(pops)
        .and_then(|length| length.checked_add(pushes))
        .ok_or_else(|| object_definition_error(id, decoded.pc()))?;
    if output_len > state.len() {
        let additional = output_len - state.len();
        state.try_reserve(additional).map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AllocationFailed {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    requested: usize_to_u64(additional),
                },
            )
        })?;
    }

    match instruction.opcode() {
        FinalOpcode::Undefined => state.push(ObjectDefinitionProvenance::LiteralUndefined),
        FinalOpcode::PushThis
            if metadata.executable_kind == CompilerExecutableKind::ClassConstructor =>
        {
            state.push(ObjectDefinitionProvenance::ClassFieldReceiver(
                usize_to_u32(instruction_index),
            ));
        }
        FinalOpcode::GetVarRefCheck
            if closure_operand(instruction.opcode(), instruction.operands()).is_some_and(
                |slot| {
                    metadata
                        .closures
                        .get(slot as usize)
                        .is_some_and(|definition| {
                            definition.policy().kind() == CompilerBindingKind::ClassFieldKey
                        })
                },
            ) =>
        {
            state.push(ObjectDefinitionProvenance::ClassFieldKey(usize_to_u32(
                instruction_index,
            )));
        }
        FinalOpcode::GetLocCheck
            if local_operand(instruction.opcode(), instruction.operands()).is_some_and(
                |slot| {
                    let arguments = function.control_flow().domains().argument_count() as usize;
                    metadata
                        .variables
                        .get(arguments.saturating_add(slot as usize))
                        .is_some_and(|definition| {
                            definition.policy().kind() == CompilerBindingKind::ClassFieldKey
                        })
                },
            ) =>
        {
            state.push(ObjectDefinitionProvenance::ClassFieldKey(usize_to_u32(
                instruction_index,
            )));
        }
        FinalOpcode::Object => state.push(ObjectDefinitionProvenance::FreshObject(usize_to_u32(
            instruction_index,
        ))),
        FinalOpcode::DefineClass => {
            let site = usize_to_u32(instruction_index);
            let heritage = match instruction.operands() {
                Operands::AtomU8 {
                    value: value @ 0..=3,
                    ..
                } => usize::from(value & 1),
                _ => return Err(object_definition_error(id, decoded.pc())),
            };
            state.truncate(state.len() - 2 - heritage);
            state.push(ObjectDefinitionProvenance::ClassConstructor(site));
            state.push(ObjectDefinitionProvenance::ClassPrototype(site));
        }
        FinalOpcode::ArrayFrom => {
            let Some(argument_count) = instruction.operands().dynamic_argument_count() else {
                return Err(append_stack_error(id, decoded.pc(), instruction.opcode()));
            };
            state.truncate(state.len() - pops);
            state.push(ObjectDefinitionProvenance::FreshArray {
                site: usize_to_u32(instruction_index),
                minimum_cursor: u32::from(argument_count),
            });
        }
        FinalOpcode::Dup => {
            let value = *state
                .last()
                .ok_or_else(|| object_definition_error(id, decoded.pc()))?;
            if is_append_provenance(value) {
                *state
                    .last_mut()
                    .ok_or_else(|| object_definition_error(id, decoded.pc()))? =
                    ObjectDefinitionProvenance::Unknown;
                state.push(ObjectDefinitionProvenance::Unknown);
            } else {
                state.push(shuffled_object_definition_provenance(value));
            }
        }
        FinalOpcode::Dup1 => {
            let Some(site) = trailing_elision_pair_at_top(state) else {
                return Err(append_stack_error(id, decoded.pc(), instruction.opcode()));
            };
            let pair = state.len() - 2;
            state[pair] = ObjectDefinitionProvenance::AppendDestination(site);
            state[pair + 1] = ObjectDefinitionProvenance::AppendLengthTarget(site);
            state.push(ObjectDefinitionProvenance::AppendLengthCursor(site));
        }
        FinalOpcode::Dup2 => {
            let left_index = state.len() - 2;
            let left = shuffled_object_definition_provenance(state[left_index]);
            let right = shuffled_object_definition_provenance(state[left_index + 1]);
            state.push(left);
            state.push(right);
        }
        FinalOpcode::Insert2 => {
            let left_index = state.len() - 2;
            let left = shuffled_object_definition_provenance(state[left_index]);
            let right = shuffled_object_definition_provenance(state[left_index + 1]);
            state[left_index] = right;
            state[left_index + 1] = left;
            state.push(right);
        }
        FinalOpcode::Insert3 => {
            let first_index = state.len() - 3;
            let first = shuffled_object_definition_provenance(state[first_index]);
            let second = shuffled_object_definition_provenance(state[first_index + 1]);
            let third = shuffled_object_definition_provenance(state[first_index + 2]);
            state[first_index] = third;
            state[first_index + 1] = first;
            state[first_index + 2] = second;
            state.push(third);
        }
        FinalOpcode::Swap => {
            // Pure stack rotation: the object-rest exclude list and the
            // converted computed key keep their provenance through the
            // pinned `swap` reordering.
            let left_index = state
                .len()
                .checked_sub(2)
                .ok_or_else(|| object_definition_error(id, decoded.pc()))?;
            state.swap(left_index, left_index + 1);
        }
        FinalOpcode::Perm3 => {
            // Pure stack rotation: `[a, b, c] -> [b, a, c]` keeps the
            // exclude list and the converted key below the value.
            let left_index = state
                .len()
                .checked_sub(3)
                .ok_or_else(|| object_definition_error(id, decoded.pc()))?;
            state.swap(left_index, left_index + 1);
        }
        FinalOpcode::GetField2 => {
            let base = state.len() - 1;
            if is_append_provenance(state[base]) {
                state[base] = ObjectDefinitionProvenance::Unknown;
            }
            state.push(ObjectDefinitionProvenance::Unknown);
        }
        FinalOpcode::GetArrayEl2 => {
            let base = retained_object_definition_provenance(state[state.len() - 2]);
            state.truncate(state.len() - 2);
            state.push(base);
            state.push(ObjectDefinitionProvenance::Unknown);
        }
        FinalOpcode::ToPropKey => convert_property_key_provenance(state),
        // The closure-name/home-object primitives retain their surrounding
        // class provenance. `copy_data_properties` likewise retains all
        // referenced operands after its resumable work; its fresh target and
        // packed depths were checked by the entry validation above.
        FinalOpcode::SetNameComputed
        | FinalOpcode::SetHomeObject
        | FinalOpcode::CopyDataProperties => {}
        FinalOpcode::DefineField => {
            let base = state[state.len() - 2];
            let base = match base {
                ObjectDefinitionProvenance::FreshArray { site, .. } => {
                    let Some(index) = static_array_index(function, instruction.operands()) else {
                        return Err(append_stack_error(id, decoded.pc(), instruction.opcode()));
                    };
                    ObjectDefinitionProvenance::FreshArray {
                        site,
                        minimum_cursor: index.saturating_add(1),
                    }
                }
                value => retained_object_definition_provenance(value),
            };
            state.truncate(state.len() - 2);
            state.push(base);
        }
        // Object-literal `__proto__: value` mutates the fresh literal in
        // place; just like `define_method`, it retains the one valid method
        // target for a later compiler-shaped definition.
        FinalOpcode::SetProto | FinalOpcode::DefineMethod => {
            let base = retained_object_definition_provenance(state[state.len() - 2]);
            state.truncate(state.len() - 2);
            state.push(base);
        }
        FinalOpcode::DefineArrayEl => {
            if let Some(site) = append_pair_for_element(state) {
                state.truncate(state.len() - 3);
                state.push(ObjectDefinitionProvenance::AppendDestination(site));
                state.push(ObjectDefinitionProvenance::AppendCursorNeedsIncrement(site));
            } else {
                let base = state[state.len() - 3];
                let key = state[state.len() - 2];
                state.truncate(state.len() - 3);
                state.push(base);
                state.push(key);
            }
        }
        // A computed method definition and private element definition each
        // preserve their base below key/name and value. Private elements have
        // already been restricted to a certified class target above.
        FinalOpcode::DefinePrivateField | FinalOpcode::DefineMethodComputed => {
            let base = retained_object_definition_provenance(state[state.len() - 3]);
            state.truncate(state.len() - 3);
            state.push(base);
        }
        FinalOpcode::Append => {
            let Some(pair) = append_pair_for_append(state) else {
                return Err(append_stack_error(id, decoded.pc(), instruction.opcode()));
            };
            state.truncate(state.len() - 3);
            state.push(ObjectDefinitionProvenance::AppendDestination(pair.site));
            state.push(if pair.pending_elision {
                ObjectDefinitionProvenance::AppendCursorAfterElision(pair.site)
            } else {
                ObjectDefinitionProvenance::CheckedAppendCursor(pair.site)
            });
        }
        FinalOpcode::Inc => {
            let cursor = state.len() - 1;
            let destination = cursor.saturating_sub(1);
            let next = match (state.get(destination), state.get(cursor)) {
                (
                    Some(ObjectDefinitionProvenance::AppendDestination(destination)),
                    Some(ObjectDefinitionProvenance::AppendCursorNeedsIncrement(cursor)),
                ) if destination == cursor => Some(
                    ObjectDefinitionProvenance::CheckedAppendCursor(*destination),
                ),
                (
                    Some(ObjectDefinitionProvenance::AppendDestination(destination)),
                    Some(
                        ObjectDefinitionProvenance::CheckedAppendCursor(cursor)
                        | ObjectDefinitionProvenance::AppendCursorAfterElision(cursor),
                    ),
                ) if destination == cursor => Some(
                    ObjectDefinitionProvenance::AppendCursorAfterElision(*destination),
                ),
                _ => None,
            };
            if let Some(next) = next {
                state[cursor] = next;
            } else {
                state.truncate(state.len() - pops);
                state.resize(output_len, ObjectDefinitionProvenance::Unknown);
            }
        }
        FinalOpcode::ForOfNext => {
            // The verified for-of step pushes the certified value and done
            // flag above the record without popping anything. The record
            // slots and any rest-collector fresh-array/cursor pair remain in
            // place, so the loop can keep appending to the same array.
            state.try_reserve(2).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 2,
                    },
                )
            })?;
            state.push(ObjectDefinitionProvenance::Unknown);
            state.push(ObjectDefinitionProvenance::Unknown);
        }
        FinalOpcode::Drop if checked_append_pair_at_top(state).is_some() => {
            state.truncate(state.len() - 2);
            state.push(ObjectDefinitionProvenance::Unknown);
        }
        FinalOpcode::PutField
            if is_append_length_finalizer(function, instruction.operands(), state) =>
        {
            state.truncate(state.len() - 3);
            state.push(ObjectDefinitionProvenance::Unknown);
        }
        opcode if integer_opcode_value(opcode, instruction.operands()).is_some() => {
            let value = integer_opcode_value(opcode, instruction.operands())
                .and_then(|value| u32::try_from(value).ok());
            let candidate = state.last().copied().and_then(|provenance| {
                let ObjectDefinitionProvenance::FreshArray {
                    site,
                    minimum_cursor,
                } = provenance
                else {
                    return None;
                };
                let value = value.filter(|value| *value >= minimum_cursor)?;
                Some(ObjectDefinitionProvenance::ArrayCursorCandidate { site, value })
            });
            state.push(candidate.unwrap_or(ObjectDefinitionProvenance::Unknown));
        }
        _ => {
            state.truncate(state.len() - pops);
            state.resize(output_len, ObjectDefinitionProvenance::Unknown);
        }
    }
    if state.len() != output_len {
        return Err(object_definition_error(id, decoded.pc()));
    }
    Ok(true)
}

/// Returns the target slot of a well-formed packed `copy_data_properties`
/// operand. Its fixed stack effect requires three values, while the packed
/// source/excluded depths may refer farther down the stack; prove all three
/// references are in bounds before granting the mutation authority.
fn copy_data_properties_target_index(
    state: &[ObjectDefinitionProvenance],
    operands: Operands,
) -> Option<usize> {
    let Operands::U8(mask) = operands else {
        return None;
    };
    let target_depth = usize::from(mask & 0b11);
    let source_depth = usize::from((mask >> 2) & 0b111);
    let excluded_depth = usize::from((mask >> 5) & 0b111);
    let index_at_depth = |depth: usize| state.len().checked_sub(depth.saturating_add(1));
    let target = index_at_depth(target_depth)?;
    index_at_depth(source_depth)?;
    index_at_depth(excluded_depth)?;
    Some(target)
}

fn apply_nip_catch_provenance(
    id: FunctionTemplateId,
    pc: BytecodePc,
    transform: Option<CertifiedNipCatchTransform>,
    state: &mut Vec<ObjectDefinitionProvenance>,
) -> Result<(), BytecodeVerificationError> {
    let Some(transform) = transform else {
        return Err(object_definition_error(id, pc));
    };
    let input_depth = transform.input_depth as usize;
    let retained_prefix = transform.retained_prefix as usize;
    if state.len() != input_depth || retained_prefix >= input_depth {
        return Err(object_definition_error(id, pc));
    }
    let value = *state
        .last()
        .ok_or_else(|| object_definition_error(id, pc))?;
    state.truncate(retained_prefix);
    state.push(value);
    Ok(())
}

// A converted key is also a temporal anchor: the certified target must remain
// immediately below that exact stack slot while the value is evaluated.
// Copying or moving the marker would let a value evaluated earlier be rotated
// across the pair and masquerade as the compiler's post-conversion RHS.
const fn shuffled_object_definition_provenance(
    value: ObjectDefinitionProvenance,
) -> ObjectDefinitionProvenance {
    match value {
        ObjectDefinitionProvenance::ConvertedPropertyKey(_)
        | ObjectDefinitionProvenance::FreshArray { .. }
        | ObjectDefinitionProvenance::ArrayCursorCandidate { .. }
        | ObjectDefinitionProvenance::AppendDestination(_)
        | ObjectDefinitionProvenance::CheckedAppendCursor(_)
        | ObjectDefinitionProvenance::AppendCursorAfterElision(_)
        | ObjectDefinitionProvenance::AppendCursorNeedsIncrement(_)
        | ObjectDefinitionProvenance::AppendLengthTarget(_)
        | ObjectDefinitionProvenance::AppendLengthCursor(_) => ObjectDefinitionProvenance::Unknown,
        value => value,
    }
}

const fn retained_object_definition_provenance(
    value: ObjectDefinitionProvenance,
) -> ObjectDefinitionProvenance {
    if is_append_provenance(value) {
        ObjectDefinitionProvenance::Unknown
    } else {
        value
    }
}

const fn is_append_provenance(value: ObjectDefinitionProvenance) -> bool {
    matches!(
        value,
        ObjectDefinitionProvenance::FreshArray { .. }
            | ObjectDefinitionProvenance::ArrayCursorCandidate { .. }
            | ObjectDefinitionProvenance::AppendDestination(_)
            | ObjectDefinitionProvenance::CheckedAppendCursor(_)
            | ObjectDefinitionProvenance::AppendCursorAfterElision(_)
            | ObjectDefinitionProvenance::AppendCursorNeedsIncrement(_)
            | ObjectDefinitionProvenance::AppendLengthTarget(_)
            | ObjectDefinitionProvenance::AppendLengthCursor(_)
    )
}

const fn is_append_length_marker(value: ObjectDefinitionProvenance) -> bool {
    matches!(
        value,
        ObjectDefinitionProvenance::AppendLengthTarget(_)
            | ObjectDefinitionProvenance::AppendLengthCursor(_)
    )
}

const fn is_linear_append_provenance(value: ObjectDefinitionProvenance) -> bool {
    matches!(
        value,
        ObjectDefinitionProvenance::AppendDestination(_)
            | ObjectDefinitionProvenance::CheckedAppendCursor(_)
            | ObjectDefinitionProvenance::AppendCursorAfterElision(_)
            | ObjectDefinitionProvenance::AppendCursorNeedsIncrement(_)
            | ObjectDefinitionProvenance::AppendLengthTarget(_)
            | ObjectDefinitionProvenance::AppendLengthCursor(_)
    )
}

fn verify_linear_append_inputs(
    id: FunctionTemplateId,
    decoded: crate::DecodedInstruction,
    function: &VerifiedCompilerFunction,
    state: &[ObjectDefinitionProvenance],
) -> Result<(), BytecodeVerificationError> {
    if !state.iter().copied().any(is_linear_append_provenance) {
        return Ok(());
    }
    let instruction = decoded.instruction();
    let opcode = instruction.opcode();
    let exact_transition = match opcode {
        FinalOpcode::Append => {
            append_pair_for_append(state).is_some()
                && state
                    .last()
                    .copied()
                    .is_some_and(|value| !is_linear_append_provenance(value))
        }
        FinalOpcode::DefineArrayEl => {
            append_pair_for_element(state).is_some()
                && state
                    .last()
                    .copied()
                    .is_some_and(|value| !is_linear_append_provenance(value))
        }
        FinalOpcode::Inc => append_cursor_pair_at_top(state).is_some(),
        FinalOpcode::Dup1 => trailing_elision_pair_at_top(state).is_some(),
        FinalOpcode::PutField => {
            is_append_length_finalizer(function, instruction.operands(), state)
        }
        FinalOpcode::Drop => checked_append_pair_at_top(state).is_some(),
        // The verified for-of opcodes never consume a tracked fresh-array or
        // append pair. `for_of_start` pops the iterable into the
        // internal-stack-certified three-slot record, and `for_of_next`
        // performs no runtime pops at all (its record-slot stack metadata
        // models the three-slot record, which this pass never tracks); the
        // destructuring rest collector therefore keeps its fresh array and
        // cursor alive across the loop.
        FinalOpcode::ForOfStart | FinalOpcode::ForAwaitOfStart | FinalOpcode::ForOfNext => true,
        _ => false,
    };
    if exact_transition {
        return Ok(());
    }
    if opcode == FinalOpcode::NipCatch {
        return Err(append_stack_error(id, decoded.pc(), opcode));
    }
    let effect = instruction
        .stack_effect()
        .map_err(|_| append_stack_error(id, decoded.pc(), opcode))?;
    let pops = effect.pops() as usize;
    let input_start = state
        .len()
        .checked_sub(pops)
        .ok_or_else(|| append_stack_error(id, decoded.pc(), opcode))?;
    if state[input_start..]
        .iter()
        .copied()
        .any(is_linear_append_provenance)
    {
        return Err(append_stack_error(id, decoded.pc(), opcode));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedAppendPair {
    site: u32,
    pending_elision: bool,
}

fn append_pair_for_append(state: &[ObjectDefinitionProvenance]) -> Option<CertifiedAppendPair> {
    let base = state.len().checked_sub(3)?;
    match (state[base], state[base + 1]) {
        (
            ObjectDefinitionProvenance::FreshArray {
                site,
                minimum_cursor,
            },
            ObjectDefinitionProvenance::ArrayCursorCandidate {
                site: cursor_site,
                value,
            },
        ) if site == cursor_site && value >= minimum_cursor => Some(CertifiedAppendPair {
            site,
            pending_elision: value > minimum_cursor,
        }),
        (
            ObjectDefinitionProvenance::AppendDestination(destination),
            ObjectDefinitionProvenance::CheckedAppendCursor(cursor),
        ) if destination == cursor => Some(CertifiedAppendPair {
            site: destination,
            pending_elision: false,
        }),
        (
            ObjectDefinitionProvenance::AppendDestination(destination),
            ObjectDefinitionProvenance::AppendCursorAfterElision(cursor),
        ) if destination == cursor => Some(CertifiedAppendPair {
            site: destination,
            pending_elision: true,
        }),
        _ => None,
    }
}

fn append_pair_for_element(state: &[ObjectDefinitionProvenance]) -> Option<u32> {
    let base = state.len().checked_sub(3)?;
    match (state[base], state[base + 1]) {
        (
            ObjectDefinitionProvenance::AppendDestination(destination),
            ObjectDefinitionProvenance::CheckedAppendCursor(cursor)
            | ObjectDefinitionProvenance::AppendCursorAfterElision(cursor),
        ) if destination == cursor => Some(destination),
        // First use inside an array-destructuring rest-collection loop: the
        // fresh array and its verified initial cursor write the first
        // collected value, then `inc` advances into the certified
        // destination/cursor pair shape shared with the loop backedge. The
        // straight-line dynamic-array-literal program always converts
        // through `append` first, so this arm admits exactly the loop form.
        (
            ObjectDefinitionProvenance::FreshArray {
                site,
                minimum_cursor,
            },
            ObjectDefinitionProvenance::ArrayCursorCandidate {
                site: cursor_site,
                value,
            },
        ) if site == cursor_site && value >= minimum_cursor => Some(site),
        _ => None,
    }
}

fn append_pair_needing_increment_at_top(state: &[ObjectDefinitionProvenance]) -> Option<u32> {
    let base = state.len().checked_sub(2)?;
    match (state[base], state[base + 1]) {
        (
            ObjectDefinitionProvenance::AppendDestination(destination),
            ObjectDefinitionProvenance::AppendCursorNeedsIncrement(cursor),
        ) if destination == cursor => Some(destination),
        _ => None,
    }
}

fn append_cursor_pair_at_top(state: &[ObjectDefinitionProvenance]) -> Option<u32> {
    let base = state.len().checked_sub(2)?;
    match (state[base], state[base + 1]) {
        (
            ObjectDefinitionProvenance::AppendDestination(destination),
            ObjectDefinitionProvenance::CheckedAppendCursor(cursor)
            | ObjectDefinitionProvenance::AppendCursorAfterElision(cursor)
            | ObjectDefinitionProvenance::AppendCursorNeedsIncrement(cursor),
        ) if destination == cursor => Some(destination),
        _ => None,
    }
}

fn checked_append_pair_at_top(state: &[ObjectDefinitionProvenance]) -> Option<u32> {
    let base = state.len().checked_sub(2)?;
    match (state[base], state[base + 1]) {
        (
            ObjectDefinitionProvenance::AppendDestination(destination),
            ObjectDefinitionProvenance::CheckedAppendCursor(cursor),
        ) if destination == cursor => Some(destination),
        _ => None,
    }
}

fn trailing_elision_pair_at_top(state: &[ObjectDefinitionProvenance]) -> Option<u32> {
    let base = state.len().checked_sub(2)?;
    match (state[base], state[base + 1]) {
        (
            ObjectDefinitionProvenance::AppendDestination(destination),
            ObjectDefinitionProvenance::AppendCursorAfterElision(cursor),
        ) if destination == cursor => Some(destination),
        _ => None,
    }
}

fn is_append_length_finalizer(
    function: &VerifiedCompilerFunction,
    operands: Operands,
    state: &[ObjectDefinitionProvenance],
) -> bool {
    let Some(base) = state.len().checked_sub(3) else {
        return false;
    };
    let (
        ObjectDefinitionProvenance::AppendDestination(destination),
        ObjectDefinitionProvenance::AppendLengthTarget(target),
        ObjectDefinitionProvenance::AppendLengthCursor(cursor),
    ) = (state[base], state[base + 1], state[base + 2])
    else {
        return false;
    };
    destination == target
        && target == cursor
        && operands
            .atom_pool_index()
            .and_then(|index| usize::try_from(index.get()).ok())
            .and_then(|index| function.atoms().get(index))
            .is_some_and(|atom| compiler_string_is_ascii(atom.string(), b"length"))
}

fn static_array_index(function: &VerifiedCompilerFunction, operands: Operands) -> Option<u32> {
    let atom = operands
        .atom_pool_index()
        .and_then(|index| usize::try_from(index.get()).ok())
        .and_then(|index| function.atoms().get(index))?;
    if !atom.is_static_property_only() || !atom.string().is_tagged_integer_atom() {
        return None;
    }
    let mut value = 0_u32;
    for unit in atom.string().code_units() {
        let digit = u32::from(unit.checked_sub(u16::from(b'0'))?);
        value = value.checked_mul(10)?.checked_add(digit)?;
    }
    (value < i32::MAX as u32).then_some(value)
}

fn compiler_string_is_ascii(value: &crate::CompilerString, expected: &[u8]) -> bool {
    value
        .code_units()
        .eq(expected.iter().copied().map(u16::from))
}

const fn integer_opcode_value(opcode: FinalOpcode, operands: Operands) -> Option<i32> {
    match (opcode, operands) {
        (FinalOpcode::PushMinus1, Operands::NoneInt) => Some(-1),
        (FinalOpcode::Push0, Operands::NoneInt) => Some(0),
        (FinalOpcode::Push1, Operands::NoneInt) => Some(1),
        (FinalOpcode::Push2, Operands::NoneInt) => Some(2),
        (FinalOpcode::Push3, Operands::NoneInt) => Some(3),
        (FinalOpcode::Push4, Operands::NoneInt) => Some(4),
        (FinalOpcode::Push5, Operands::NoneInt) => Some(5),
        (FinalOpcode::Push6, Operands::NoneInt) => Some(6),
        (FinalOpcode::Push7, Operands::NoneInt) => Some(7),
        (FinalOpcode::PushI8, Operands::I8(value)) => Some(value as i32),
        (FinalOpcode::PushI16, Operands::I16(value)) => Some(value as i32),
        (FinalOpcode::PushI32, Operands::I32(value)) => Some(value),
        _ => None,
    }
}

fn convert_property_key_provenance(state: &mut [ObjectDefinitionProvenance]) {
    let key_index = state.len() - 1;
    let converted = key_index
        .checked_sub(1)
        .and_then(|object_index| state.get(object_index))
        .and_then(|provenance| match provenance {
            ObjectDefinitionProvenance::FreshObject(site)
            | ObjectDefinitionProvenance::ClassConstructor(site) => Some(*site),
            _ => None,
        })
        .map_or(
            ObjectDefinitionProvenance::Unknown,
            ObjectDefinitionProvenance::ConvertedPropertyKey,
        );
    state[key_index] = converted;
}

#[allow(clippy::too_many_arguments)]
fn propagate_object_definition_provenance(
    id: FunctionTemplateId,
    source_pc: BytecodePc,
    successor: InstructionIndex,
    target_pc: BytecodePc,
    output: &[ObjectDefinitionProvenance],
    entries: &mut [Option<Vec<ObjectDefinitionProvenance>>],
    queued: &mut [bool],
    work: &mut VecDeque<usize>,
    state_limit: u64,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let index = successor.get() as usize;
    let entry = entries
        .get_mut(index)
        .ok_or_else(|| method_target_error(id, source_pc))?;
    let changed = match entry {
        None => {
            charge_frame_state_entries(id, usage, output.len(), state_limit)?;
            *entry = Some(try_copy_slice(
                id,
                output,
                BytecodeGraphResource::FrameStateEntries,
            )?);
            true
        }
        Some(existing) if existing.len() == output.len() => {
            let mut changed = false;
            for (target, incoming) in existing.iter_mut().zip(output) {
                let merged = match (*target, *incoming) {
                    (established, incoming) if established == incoming => established,
                    // The array-destructuring rest-collection loop joins the
                    // pre-loop fresh-array/cursor pair with the backedge's
                    // certified destination/cursor pair at the same `array_from`
                    // site; the post-loop shape strictly extends the pre-loop
                    // shape, so the backedge state wins.
                    (
                        ObjectDefinitionProvenance::FreshArray { site, .. },
                        ObjectDefinitionProvenance::AppendDestination(destination),
                    ) if site == destination => {
                        ObjectDefinitionProvenance::AppendDestination(destination)
                    }
                    (
                        ObjectDefinitionProvenance::AppendDestination(destination),
                        ObjectDefinitionProvenance::FreshArray { site, .. },
                    ) if site == destination => {
                        ObjectDefinitionProvenance::AppendDestination(destination)
                    }
                    (
                        ObjectDefinitionProvenance::ArrayCursorCandidate { site, .. },
                        ObjectDefinitionProvenance::CheckedAppendCursor(cursor),
                    ) if site == cursor => ObjectDefinitionProvenance::CheckedAppendCursor(cursor),
                    (
                        ObjectDefinitionProvenance::CheckedAppendCursor(cursor),
                        ObjectDefinitionProvenance::ArrayCursorCandidate { site, .. },
                    ) if site == cursor => ObjectDefinitionProvenance::CheckedAppendCursor(cursor),
                    _ => {
                        if *target != *incoming
                            && (is_linear_append_provenance(*target)
                                || is_linear_append_provenance(*incoming))
                        {
                            return Err(BytecodeVerificationError::function(
                                id,
                                BytecodeVerificationErrorKind::AppendProvenanceJoinMismatch {
                                    target: target_pc,
                                    incoming_from: source_pc,
                                },
                            ));
                        }
                        ObjectDefinitionProvenance::Unknown
                    }
                };
                changed |= merged != *target;
                *target = merged;
            }
            changed
        }
        Some(existing) => {
            if existing.iter().copied().any(is_linear_append_provenance)
                || output.iter().copied().any(is_linear_append_provenance)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AppendProvenanceJoinMismatch {
                        target: target_pc,
                        incoming_from: source_pc,
                    },
                ));
            }
            let changed = existing
                .iter()
                .any(|value| *value != ObjectDefinitionProvenance::Unknown);
            existing.fill(ObjectDefinitionProvenance::Unknown);
            changed
        }
    };
    if changed && !queued[index] {
        queued[index] = true;
        work.push_back(index);
    }
    Ok(())
}

pub(super) fn charge_frame_state_entries(
    id: FunctionTemplateId,
    usage: &mut BytecodeGraphUsage,
    amount: usize,
    limit: u64,
) -> Result<(), BytecodeVerificationError> {
    let amount = usize_to_u64(amount);
    let observed = usage
        .frame_state_entries
        .checked_add(amount)
        .ok_or_else(|| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    limit,
                    observed: u64::MAX,
                },
            )
        })?;
    if observed > limit {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::FrameStateEntries,
                limit,
                observed,
            },
        ));
    }
    usage.frame_state_entries = observed;
    Ok(())
}

fn method_target_error(id: FunctionTemplateId, pc: BytecodePc) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::DefineMethodTargetMismatch { pc },
    )
}

const fn method_target_matches_enumerability(
    target: Option<&ObjectDefinitionProvenance>,
    flags: u8,
) -> bool {
    matches!(
        (target, flags),
        (Some(ObjectDefinitionProvenance::FreshObject(_)), 4..=6)
            | (
                Some(
                    ObjectDefinitionProvenance::ClassConstructor(_)
                        | ObjectDefinitionProvenance::ClassPrototype(_)
                ),
                0..=2
            )
    )
}

fn define_array_element_key_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::DefineArrayElementKeyMismatch { pc },
    )
}

fn append_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::AppendOperandStackMismatch { pc, opcode },
    )
}

fn object_definition_error(id: FunctionTemplateId, pc: BytecodePc) -> BytecodeVerificationError {
    method_target_error(id, pc)
}
