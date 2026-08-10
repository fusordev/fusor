fn for_of_start_is_async(instructions: &[VerifiedInstruction], site: BytecodePc) -> Option<bool> {
    let index = instructions
        .binary_search_by_key(&site, |verified| verified.decoded().pc())
        .ok()?;
    match instructions[index].decoded().instruction().opcode() {
        FinalOpcode::ForOfStart => Some(false),
        FinalOpcode::ForAwaitOfStart => Some(true),
        _ => None,
    }
}

fn has_enclosing_for_of_record(state: &[InternalStackValue]) -> bool {
    state.windows(3).any(|record| {
        matches!(
            record,
            [
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfCatch(catch),
            ] if iterator == next && next == catch
        )
    })
}

fn invalidate_internal_value_provenance(state: &mut [InternalStackValue]) {
    for value in state {
        if matches!(
            value,
            InternalStackValue::ForInKey(_)
                | InternalStackValue::ForInDone(_)
                | InternalStackValue::ForInHeadKey(_)
                | InternalStackValue::ForOfValue(_)
                | InternalStackValue::ForOfDone(_)
                | InternalStackValue::ForOfHeadValue(_)
                | InternalStackValue::ForOfReturnValue(_)
                | InternalStackValue::CatchException(_)
        ) {
            *value = InternalStackValue::Ordinary;
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ForOfRecordState {
    Active,
    Exhausted,
    Closable,
}

#[derive(Clone, Copy)]
struct InternalStackTarget {
    catch_handler: bool,
    finally_entry: bool,
    iterator_close: bool,
}

fn trailing_for_of_record(
    state: &[InternalStackValue],
) -> Option<(usize, BytecodePc, ForOfRecordState)> {
    let base = state.len().checked_sub(3)?;
    let (site, record_state) = match (state[base], state[base + 1], state[base + 2]) {
        (
            InternalStackValue::ForOfIterator(iterator),
            InternalStackValue::ForOfNextMethod(next),
            InternalStackValue::ForOfCatch(catch),
        ) if iterator == next && next == catch => (iterator, ForOfRecordState::Active),
        (
            InternalStackValue::ForOfExhaustedIterator(iterator),
            InternalStackValue::ForOfExhaustedNextMethod(next),
            InternalStackValue::ForOfExhaustedCatch(catch),
        ) if iterator == next && next == catch => (iterator, ForOfRecordState::Exhausted),
        (
            InternalStackValue::ForOfClosableIterator(iterator),
            InternalStackValue::ForOfClosableNextMethod(next),
            InternalStackValue::ForOfClosableCatch(catch),
        ) if iterator == next && next == catch => (iterator, ForOfRecordState::Closable),
        _ => return None,
    };
    Some((base, site, record_state))
}

fn merge_trailing_for_of_close_record(
    established: &mut [InternalStackValue],
    incoming: &[InternalStackValue],
) -> Option<bool> {
    if established.len() != incoming.len() {
        return None;
    }
    let (established_base, established_site, established_state) =
        trailing_for_of_record(established)?;
    let (incoming_base, incoming_site, incoming_state) = trailing_for_of_record(incoming)?;
    if established_base != incoming_base
        || established_site != incoming_site
        || established[..established_base] != incoming[..incoming_base]
    {
        return None;
    }
    let mergeable = established_state == ForOfRecordState::Closable
        || incoming_state == ForOfRecordState::Closable
        || established_state != incoming_state;
    if !mergeable {
        return None;
    }
    let changed = established_state != ForOfRecordState::Closable;
    established[established_base] = InternalStackValue::ForOfClosableIterator(established_site);
    established[established_base + 1] =
        InternalStackValue::ForOfClosableNextMethod(established_site);
    established[established_base + 2] = InternalStackValue::ForOfClosableCatch(established_site);
    Some(changed)
}

fn is_inert_disconnected_finalizer_edge(
    index: usize,
    component: u32,
    target: InternalStackTarget,
    enters_finally: bool,
    output: &[InternalStackValue],
    components: &[Option<u32>],
) -> bool {
    // A retained but unreachable loop backedge can target the first
    // instruction of an already verified enclosing finalizer. Dead components
    // start marker-free, so they cannot reproduce its live pending/return pair.
    target.finally_entry
        && !enters_finally
        && !target.catch_handler
        && components
            .get(index)
            .copied()
            .flatten()
            .is_some_and(|established| established != component)
        && output
            .iter()
            .all(|value| *value == InternalStackValue::Ordinary)
}

fn has_targeted_finalizer_pair(output: &[InternalStackValue], successor: InstructionIndex) -> bool {
    matches!(
        output.get(output.len().saturating_sub(2)..),
        Some([
            InternalStackValue::FinallyPending {
                target: pending_target,
                ..
            },
            InternalStackValue::FinallyReturn {
                target: return_target
            }
        ]) if *pending_target == successor && *return_target == successor
    )
}

#[allow(clippy::too_many_arguments)]
fn propagate_internal_operand_stack(
    id: FunctionTemplateId,
    source_pc: BytecodePc,
    successor: InstructionIndex,
    target_pc: BytecodePc,
    component: u32,
    target: InternalStackTarget,
    enters_finally: bool,
    output: &[InternalStackValue],
    entries: &mut [Option<Vec<InternalStackValue>>],
    components: &mut [Option<u32>],
    queued: &mut [bool],
    work: &mut VecDeque<usize>,
    state_limit: u64,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    let index = successor.get() as usize;
    if is_inert_disconnected_finalizer_edge(
        index,
        component,
        target,
        enters_finally,
        output,
        components,
    ) {
        return Ok(());
    }
    if target.finally_entry {
        if !has_targeted_finalizer_pair(output, successor) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
                    target: target_pc,
                    incoming_from: source_pc,
                },
            ));
        }
    } else if enters_finally {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
                target: target_pc,
                incoming_from: source_pc,
            },
        ));
    }
    let component_slot = components
        .get_mut(index)
        .ok_or_else(|| internal_join_error(id, target_pc, source_pc, output, &[]))?;
    match *component_slot {
        Some(established) if established != component => {
            let existing = entries
                .get(index)
                .and_then(Option::as_deref)
                .ok_or_else(|| internal_join_error(id, target_pc, source_pc, output, &[]))?;
            // A Gosub necessarily contributes its own certified pending/return
            // suffix. Ignore only that suffix when deciding whether a later,
            // disconnected compiler component is trying to smuggle an
            // independently forged marker into an already verified finalizer.
            let component_prefix = if target.finally_entry && enters_finally {
                &output[..output.len() - 2]
            } else {
                output
            };
            let carries_internal_value = component_prefix
                .iter()
                .any(|value| *value != InternalStackValue::Ordinary);
            if existing != output && (target.catch_handler || carries_internal_value) {
                return Err(internal_join_error(
                    id, target_pc, source_pc, existing, output,
                ));
            }
            return Ok(());
        }
        Some(_) => {}
        None => *component_slot = Some(component),
    }
    let entry = entries
        .get_mut(index)
        .ok_or_else(|| internal_join_error(id, target_pc, source_pc, output, &[]))?;
    match entry {
        None => {
            charge_frame_state_entries(id, usage, output.len(), state_limit)?;
            *entry = Some(try_copy_slice(
                id,
                output,
                BytecodeGraphResource::FrameStateEntries,
            )?);
            if !queued[index] {
                queued[index] = true;
                work.push_back(index);
            }
        }
        Some(existing) if existing == output => {}
        Some(existing) => {
            let merged = target.iterator_close
                && merge_trailing_for_of_close_record(existing, output).is_some_and(|changed| {
                    if changed && !queued[index] {
                        queued[index] = true;
                        work.push_back(index);
                    }
                    true
                });
            if !merged {
                return Err(internal_join_error(
                    id, target_pc, source_pc, existing, output,
                ));
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "terminal cleanup validates nested finally, catch, for-in, and synchronous for-of marker grammars together"
)]
fn verify_internal_stack_exit(
    id: FunctionTemplateId,
    decoded: crate::DecodedInstruction,
    state: &[InternalStackValue],
    has_finally: bool,
) -> Result<(), BytecodeVerificationError> {
    let is_throw = matches!(
        decoded.instruction().opcode(),
        FinalOpcode::Throw | FinalOpcode::ThrowError
    );
    let mut prefix_len = state.len();
    while matches!(
        state.get(prefix_len.saturating_sub(1)),
        Some(InternalStackValue::FinallyReturn { .. })
    ) {
        let Some(pair_start) = prefix_len.checked_sub(2) else {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit { pc: decoded.pc() },
            ));
        };
        if !matches!(
            (state[pair_start], state[pair_start + 1]),
            (
                InternalStackValue::FinallyPending {
                    target: pending_target,
                    ..
                },
                InternalStackValue::FinallyReturn {
                    target: return_target
                }
            ) if pending_target == return_target
        ) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit { pc: decoded.pc() },
            ));
        }
        prefix_len = pair_start;
    }
    let state = &state[..prefix_len];
    let tail_transfer = matches!(
        decoded.instruction().opcode(),
        FinalOpcode::TailCall
            | FinalOpcode::TailCallMethod
            | FinalOpcode::TailApply
            | FinalOpcode::TailEval
            | FinalOpcode::TailApplyEval
    );
    if !is_throw && state.iter().any(|value| value.is_finally_value()) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if tail_transfer {
        if state.iter().any(|value| value.is_for_of_value()) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::ForOfIteratorMarkerAtExit { pc: decoded.pc() },
            ));
        }
        if state
            .iter()
            .any(|value| matches!(value, InternalStackValue::CatchMarker { .. }))
        {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::CatchMarkerAtExit { pc: decoded.pc() },
            ));
        }
        // A proper tail transfer abandons the current execution context.
        // Ordinary values and a certified for-in iterator marker therefore
        // need no cleanup; catch and for-of regions above remain forbidden by
        // HasCallInTailPosition and are rejected explicitly.
        return Ok(());
    }
    if is_throw {
        let mut cursor = 0;
        while cursor < state.len() {
            match state[cursor] {
                value if value.is_javascript_value() => cursor += 1,
                InternalStackValue::CatchMarker { .. } => cursor += 1,
                InternalStackValue::FinallyPending { target, .. } => {
                    if !matches!(
                        state.get(cursor.saturating_add(1)),
                        Some(InternalStackValue::FinallyReturn {
                            target: return_target
                        }) if *return_target == target
                    ) {
                        return Err(BytecodeVerificationError::function(
                            id,
                            BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit {
                                pc: decoded.pc(),
                            },
                        ));
                    }
                    cursor += 2;
                }
                InternalStackValue::FinallyReturn { .. } => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::FinallyReturnMarkerAtExit {
                            pc: decoded.pc(),
                        },
                    ));
                }
                InternalStackValue::ForOfIterator(site) => {
                    if !matches!(
                        state.get(cursor..cursor.saturating_add(3)),
                        Some([
                            InternalStackValue::ForOfIterator(iterator),
                            InternalStackValue::ForOfNextMethod(next),
                            InternalStackValue::ForOfCatch(catch),
                        ]) if *iterator == site && *next == site && *catch == site
                    ) {
                        return Err(for_of_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                        ));
                    }
                    cursor += 3;
                }
                value if value.is_for_of_value() => {
                    return Err(for_of_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
                InternalStackValue::ForInIterator(_) => {
                    // A catch or active for-of handler nested inside the
                    // for-in region owns the next unwind step and may retain
                    // the enumeration marker beneath it. An uncaught throw,
                    // or a throw to an outer handler, must instead have removed
                    // every crossed for-in marker before this terminal.
                    let retained_by_inner_handler =
                        state[cursor.saturating_add(1)..].iter().any(|value| {
                            matches!(
                                value,
                                InternalStackValue::CatchMarker { .. }
                                    | InternalStackValue::ForOfCatch(_)
                            )
                        });
                    if !retained_by_inner_handler {
                        return Err(for_in_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                        ));
                    }
                    cursor += 1;
                }
                _ => {
                    return Err(catch_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
            }
        }
        return Ok(());
    }
    if state.iter().any(|value| value.is_for_of_value()) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ForOfIteratorMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if state
        .iter()
        .any(|value| matches!(value, InternalStackValue::CatchMarker { .. }))
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::CatchMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if state
        .iter()
        .any(|value| matches!(value, InternalStackValue::ForInIterator(_)))
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ForInIteratorMarkerAtExit { pc: decoded.pc() },
        ));
    }
    if has_finally && !state.is_empty() {
        return Err(finally_stack_error(
            id,
            decoded.pc(),
            decoded.instruction().opcode(),
        ));
    }
    Ok(())
}

fn internal_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
    state: &[InternalStackValue],
) -> BytecodeVerificationError {
    if opcode == FinalOpcode::Gosub
        || opcode == FinalOpcode::Ret
        || state.iter().any(|value| value.is_finally_value())
    {
        finally_stack_error(id, pc, opcode)
    } else if matches!(
        opcode,
        FinalOpcode::ForOfStart
            | FinalOpcode::ForAwaitOfStart
            | FinalOpcode::ForOfNext
            | FinalOpcode::ForAwaitOfNext
            | FinalOpcode::IteratorGetValueDone
            | FinalOpcode::IteratorClose
            | FinalOpcode::Rot3r
    ) || state.iter().any(|value| value.is_for_of_value())
    {
        for_of_stack_error(id, pc, opcode)
    } else if opcode == FinalOpcode::Catch
        || opcode == FinalOpcode::NipCatch
        || state.iter().any(|value| value.is_catch_value())
    {
        catch_stack_error(id, pc, opcode)
    } else {
        for_in_stack_error(id, pc, opcode)
    }
}

fn finally_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::FinallyReturnStackMismatch { pc, opcode },
    )
}

fn catch_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::CatchMarkerStackMismatch { pc, opcode },
    )
}

fn for_in_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::ForInIteratorStackMismatch { pc, opcode },
    )
}

fn for_of_stack_error(
    id: FunctionTemplateId,
    pc: BytecodePc,
    opcode: FinalOpcode,
) -> BytecodeVerificationError {
    BytecodeVerificationError::function(
        id,
        BytecodeVerificationErrorKind::ForOfIteratorStackMismatch { pc, opcode },
    )
}

/// Locates the certified for-of record beneath a `value`/`done` pair about
/// to be branched on.
///
/// The `done` flag must sit at the top with the `value` directly below it.
/// Any number of ordinary JavaScript values may sit between the value and
/// the record: the array-destructuring rest collector keeps its fresh array
/// and cursor there. The three-slot record must share the value's exact
/// `for_of_start` site, and no other certified internal value may intervene.
/// Returns the record start and the shared site.
fn for_of_branch_record(state: &[InternalStackValue]) -> Option<(usize, BytecodePc)> {
    let done_index = state.len().checked_sub(1)?;
    let InternalStackValue::ForOfDone(site) = state[done_index] else {
        return None;
    };
    let value_index = done_index.checked_sub(1)?;
    let InternalStackValue::ForOfValue(value_site) = state[value_index] else {
        return None;
    };
    if value_site != site {
        return None;
    }
    let mut cursor = value_index;
    while cursor > 0 {
        cursor -= 1;
        match state[cursor] {
            InternalStackValue::Ordinary => {}
            InternalStackValue::ForOfCatch(catch) => {
                let iterator_index = cursor.checked_sub(2)?;
                if matches!(
                    (state[iterator_index], state[iterator_index + 1]),
                    (
                        InternalStackValue::ForOfIterator(iterator),
                        InternalStackValue::ForOfNextMethod(next)
                    ) if iterator == next && next == catch && catch == site
                ) {
                    return Some((iterator_index, site));
                }
                return None;
            }
            _ => return None,
        }
    }
    None
}

fn internal_join_error(
    id: FunctionTemplateId,
    target: BytecodePc,
    incoming_from: BytecodePc,
    established: &[InternalStackValue],
    incoming: &[InternalStackValue],
) -> BytecodeVerificationError {
    let kind = if established
        .iter()
        .chain(incoming)
        .any(|value| value.is_finally_value())
    {
        BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
            target,
            incoming_from,
        }
    } else if established
        .iter()
        .chain(incoming)
        .any(|value| value.is_for_of_value())
    {
        BytecodeVerificationErrorKind::ForOfIteratorJoinMismatch {
            target,
            incoming_from,
        }
    } else if established
        .iter()
        .chain(incoming)
        .any(|value| value.is_catch_value())
    {
        BytecodeVerificationErrorKind::CatchMarkerJoinMismatch {
            target,
            incoming_from,
        }
    } else {
        BytecodeVerificationErrorKind::ForInIteratorJoinMismatch {
            target,
            incoming_from,
        }
    };
    BytecodeVerificationError::function(id, kind)
}
