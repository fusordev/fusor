#[allow(
    clippy::too_many_lines,
    reason = "the bounded CFG worklist and exact operand-stack transfer form one internal marker certificate"
)]
fn verify_internal_operand_stack(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<InternalStackCertificate, BytecodeVerificationError> {
    let instructions = function.control_flow().instructions();
    let gosub_sites = usize_to_u64(
        instructions
            .iter()
            .filter(|verified| verified.decoded().instruction().opcode() == FinalOpcode::Gosub)
            .count(),
    );
    if gosub_sites > u64::from(MAX_GOSUB_SITES_PER_FUNCTION) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::GosubSiteCountOutOfRange {
                sites: gosub_sites,
                maximum: MAX_GOSUB_SITES_PER_FUNCTION,
            },
        ));
    }
    if !instructions.iter().any(|verified| {
        matches!(
            verified.decoded().instruction().opcode(),
            FinalOpcode::ForInStart
                | FinalOpcode::ForInNext
                | FinalOpcode::ForOfStart
                | FinalOpcode::ForAwaitOfStart
                | FinalOpcode::ForOfNext
                | FinalOpcode::ForAwaitOfNext
                | FinalOpcode::IteratorGetValueDone
                | FinalOpcode::IteratorClose
                | FinalOpcode::Rot3r
                | FinalOpcode::Nip
                | FinalOpcode::Catch
                | FinalOpcode::NipCatch
                | FinalOpcode::Gosub
                | FinalOpcode::Ret
                | FinalOpcode::WithGetVar
                | FinalOpcode::WithDeleteVar
                | FinalOpcode::WithMakeRef
                | FinalOpcode::WithGetRef
        )
    }) {
        return Ok(InternalStackCertificate::default());
    }

    let mut entries = try_filled_vec(
        id,
        instructions.len(),
        None::<Vec<InternalStackValue>>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut queued = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    let mut components = try_filled_vec(
        id,
        instructions.len(),
        None::<u32>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut iteration_local_puts = try_filled_vec(
        id,
        instructions.len(),
        None::<CertifiedIterationLocalPut>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut catch_local_puts = try_filled_vec(
        id,
        instructions.len(),
        None::<CertifiedCatchLocalPut>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut nip_catch_transforms = try_filled_vec(
        id,
        instructions.len(),
        None::<CertifiedNipCatchTransform>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut catch_handler_targets = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut finally_targets = try_filled_vec(
        id,
        instructions.len(),
        false,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut finally_continuations = try_filled_vec(
        id,
        instructions.len(),
        Vec::<InstructionIndex>::new(),
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut ret_finalizers = try_filled_vec(
        id,
        instructions.len(),
        None::<InstructionIndex>,
        BytecodeGraphResource::FrameStateEntries,
    )?;
    for verified in instructions {
        match verified.decoded().instruction().opcode() {
            FinalOpcode::Catch => {
                let handler = verified.successors().branch_target().ok_or_else(|| {
                    catch_stack_error(
                        id,
                        verified.decoded().pc(),
                        verified.decoded().instruction().opcode(),
                    )
                })?;
                let target = catch_handler_targets
                    .get_mut(handler.get() as usize)
                    .ok_or_else(|| {
                        catch_stack_error(
                            id,
                            verified.decoded().pc(),
                            verified.decoded().instruction().opcode(),
                        )
                    })?;
                *target = true;
            }
            FinalOpcode::Gosub => {
                let target = verified.successors().branch_target().ok_or_else(|| {
                    finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                })?;
                let continuation = verified.successors().fallthrough().ok_or_else(|| {
                    finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                })?;
                *finally_targets
                    .get_mut(target.get() as usize)
                    .ok_or_else(|| {
                        finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                    })? = true;
                let continuations = finally_continuations
                    .get_mut(target.get() as usize)
                    .ok_or_else(|| {
                        finally_stack_error(id, verified.decoded().pc(), FinalOpcode::Gosub)
                    })?;
                continuations.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                continuations.push(continuation);
                charge_frame_state_entries(id, usage, 1, limits.max_frame_state_entries)?;
            }
            _ => {}
        }
    }
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
                || catch_handler_targets.get(next_seed) == Some(&true)
                || finally_targets.get(next_seed) == Some(&true)
            {
                next_seed = next_seed.saturating_add(1);
            }
            if next_seed == entries.len() {
                if let Some(protected) = entries.iter().enumerate().find_map(|(index, entry)| {
                    (entry.is_none() && (catch_handler_targets[index] || finally_targets[index]))
                        .then_some(index)
                }) {
                    let pc = instructions[protected].decoded().pc();
                    let error = if finally_targets[protected] {
                        BytecodeVerificationErrorKind::FinallyReturnJoinMismatch {
                            target: pc,
                            incoming_from: pc,
                        }
                    } else {
                        BytecodeVerificationErrorKind::CatchMarkerJoinMismatch {
                            target: pc,
                            incoming_from: pc,
                        }
                    };
                    return Err(BytecodeVerificationError::function(id, error));
                }
                break;
            }
            entries[next_seed] = Some(Vec::new());
            components[next_seed] = Some(usize_to_u32(next_seed));
            queued[next_seed] = true;
            work.push_back(next_seed);
        }

        let Some(index) = work.pop_front() else {
            continue;
        };
        queued[index] = false;
        let decoded = instructions[index].decoded();
        let component = components[index].ok_or_else(|| {
            internal_stack_error(id, decoded.pc(), decoded.instruction().opcode(), &[])
        })?;
        let entry = entries[index].as_deref().ok_or_else(|| {
            internal_stack_error(id, decoded.pc(), decoded.instruction().opcode(), &[])
        })?;
        charge_policy_transfers(
            id,
            &mut evaluations,
            usize_to_u64(entry.len()).saturating_add(1),
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let mut state = try_copy_slice(id, entry, BytecodeGraphResource::FrameStateEntries)?;
        // Component zero starts at function entry. Later components only audit
        // structurally retained instructions absent from the effective graph,
        // such as a gosub continuation whose finalizer never executes `ret`.
        let effectively_reachable = component == 0;
        let transfer = transfer_internal_operand_stack(
            id,
            index,
            decoded,
            effectively_reachable,
            instructions[index].successors().branch_target(),
            instructions,
            &mut state,
            &mut iteration_local_puts,
            &mut catch_local_puts,
            &mut nip_catch_transforms,
            &mut ret_finalizers,
        )?;
        if !transfer.normal_completion {
            continue;
        }

        let mut has_successor = false;
        for edge in effective_successors(
            instructions,
            index,
            &finally_continuations,
            transfer.ret_finalizer,
        ) {
            has_successor = true;
            let successor = edge.target;
            let (target_pc, target_is_iterator_close) = instructions
                .get(successor.get() as usize)
                .map(|instruction| {
                    (
                        instruction.decoded().pc(),
                        instruction.decoded().instruction().opcode() == FinalOpcode::IteratorClose,
                    )
                })
                .ok_or_else(|| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ForInIteratorJoinMismatch {
                            target: BytecodePc::new(successor.get()),
                            incoming_from: decoded.pc(),
                        },
                    )
                })?;
            let finally_marker = if edge.enters_finally {
                let pending_index = state.len().checked_sub(1).ok_or_else(|| {
                    finally_stack_error(id, decoded.pc(), decoded.instruction().opcode())
                })?;
                let original = JavaScriptStackValue::from_internal(state[pending_index])
                    .ok_or_else(|| {
                        finally_stack_error(id, decoded.pc(), decoded.instruction().opcode())
                    })?;
                state[pending_index] = InternalStackValue::FinallyPending {
                    target: successor,
                    original,
                };
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::FinallyReturn { target: successor });
                Some((pending_index, original))
            } else {
                None
            };
            let branch_value = transfer
                .iteration_branch_value
                .map(|branch_value| {
                    let value_index = state.len().checked_sub(1).ok_or_else(|| {
                        internal_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                            &state,
                        )
                    })?;
                    let replacement = match branch_value {
                        IterationBranchValue::ForIn(site)
                            if state[value_index] == InternalStackValue::ForInKey(site) =>
                        {
                            if edge.is_branch_target {
                                InternalStackValue::ForInHeadKey(site)
                            } else {
                                InternalStackValue::Ordinary
                            }
                        }
                        IterationBranchValue::ForOf { site, extras }
                            if state[value_index] == InternalStackValue::ForOfValue(site) =>
                        {
                            let Some(record_index) =
                                value_index.checked_sub(3_usize.saturating_add(extras))
                            else {
                                return Err(for_of_stack_error(
                                    id,
                                    decoded.pc(),
                                    decoded.instruction().opcode(),
                                ));
                            };
                            if !matches!(
                                (
                                    state[record_index],
                                    state[record_index + 1],
                                    state[record_index + 2],
                                ),
                                (
                                    InternalStackValue::ForOfIterator(iterator),
                                    InternalStackValue::ForOfNextMethod(next),
                                    InternalStackValue::ForOfCatch(catch),
                                ) if iterator == site && next == site && catch == site
                            ) {
                                return Err(for_of_stack_error(
                                    id,
                                    decoded.pc(),
                                    decoded.instruction().opcode(),
                                ));
                            }
                            if edge.is_branch_target {
                                InternalStackValue::ForOfHeadValue(site)
                            } else {
                                state[record_index] =
                                    InternalStackValue::ForOfExhaustedIterator(site);
                                state[record_index + 1] =
                                    InternalStackValue::ForOfExhaustedNextMethod(site);
                                state[record_index + 2] =
                                    InternalStackValue::ForOfExhaustedCatch(site);
                                InternalStackValue::Ordinary
                            }
                        }
                        IterationBranchValue::YieldStarDone {
                            site,
                            branch_when_true,
                        } if state[value_index]
                            == InternalStackValue::YieldStarIteratorResult(site) =>
                        {
                            if edge.is_branch_target == branch_when_true {
                                InternalStackValue::YieldStarFinalResult(site)
                            } else {
                                InternalStackValue::YieldStarYieldResult(site)
                            }
                        }
                        IterationBranchValue::YieldStarMethod { site, kind }
                            if state[value_index]
                                == InternalStackValue::YieldStarCallValue(site, kind) =>
                        {
                            if kind == YieldStarCallKind::Close {
                                InternalStackValue::Ordinary
                            } else if edge.is_branch_target {
                                if kind == YieldStarCallKind::Throw {
                                    InternalStackValue::YieldStarResumeValue(site)
                                } else {
                                    InternalStackValue::Ordinary
                                }
                            } else {
                                InternalStackValue::YieldStarIteratorResult(site)
                            }
                        }
                        _ => {
                            return Err(internal_stack_error(
                                id,
                                decoded.pc(),
                                decoded.instruction().opcode(),
                                &state,
                            ));
                        }
                    };
                    state[value_index] = replacement;
                    Ok((value_index, branch_value))
                })
                .transpose()?;
            let catch_exception = if decoded.instruction().opcode() == FinalOpcode::Catch
                && edge.is_branch_target
            {
                let marker_index = state.len().checked_sub(1).ok_or_else(|| {
                    catch_stack_error(id, decoded.pc(), decoded.instruction().opcode())
                })?;
                let InternalStackValue::CatchMarker { site, handler } = state[marker_index] else {
                    return Err(catch_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                };
                if handler != successor {
                    return Err(catch_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
                state[marker_index] = InternalStackValue::CatchException(site);
                Some((marker_index, site, handler))
            } else {
                None
            };
            let with_binding_results = if edge.is_branch_target {
                match decoded.instruction().opcode() {
                    FinalOpcode::WithGetVar | FinalOpcode::WithDeleteVar => 1,
                    FinalOpcode::WithMakeRef | FinalOpcode::WithGetRef => 2,
                    _ => 0,
                }
            } else {
                0
            };
            if with_binding_results != 0 {
                state.try_reserve(with_binding_results).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: usize_to_u64(with_binding_results),
                        },
                    )
                })?;
                state.extend(std::iter::repeat_n(
                    InternalStackValue::Ordinary,
                    with_binding_results,
                ));
            }
            charge_policy_transfers(
                id,
                &mut evaluations,
                usize_to_u64(state.len()).saturating_add(1),
                usage.policy_transfers,
                limits.max_policy_transfers,
            )?;
            propagate_internal_operand_stack(
                id,
                decoded.pc(),
                successor,
                target_pc,
                component,
                InternalStackTarget {
                    catch_handler: catch_handler_targets[successor.get() as usize],
                    finally_entry: finally_targets[successor.get() as usize],
                    iterator_close: target_is_iterator_close,
                },
                edge.enters_finally,
                &state,
                &mut entries,
                &mut components,
                &mut queued,
                &mut work,
                limits.max_frame_state_entries,
                usage,
            )?;
            if let Some((value_index, branch_value)) = branch_value {
                state[value_index] = match branch_value {
                    IterationBranchValue::ForIn(site) => InternalStackValue::ForInKey(site),
                    IterationBranchValue::ForOf { site, extras } => {
                        let Some(record_index) =
                            value_index.checked_sub(3_usize.saturating_add(extras))
                        else {
                            return Err(for_of_stack_error(
                                id,
                                decoded.pc(),
                                decoded.instruction().opcode(),
                            ));
                        };
                        state[record_index] = InternalStackValue::ForOfIterator(site);
                        state[record_index + 1] = InternalStackValue::ForOfNextMethod(site);
                        state[record_index + 2] = InternalStackValue::ForOfCatch(site);
                        InternalStackValue::ForOfValue(site)
                    }
                    IterationBranchValue::YieldStarDone { site, .. } => {
                        InternalStackValue::YieldStarIteratorResult(site)
                    }
                    IterationBranchValue::YieldStarMethod { site, kind } => {
                        InternalStackValue::YieldStarCallValue(site, kind)
                    }
                };
            }
            if let Some((marker_index, site, handler)) = catch_exception {
                state[marker_index] = InternalStackValue::CatchMarker { site, handler };
            }
            if with_binding_results != 0 {
                state.truncate(state.len() - with_binding_results);
            }
            if let Some((pending_index, original)) = finally_marker {
                match state.pop() {
                    Some(InternalStackValue::FinallyReturn { target }) if target == successor => {}
                    _ => {
                        return Err(finally_stack_error(
                            id,
                            decoded.pc(),
                            decoded.instruction().opcode(),
                        ));
                    }
                }
                if !matches!(
                    state.get(pending_index),
                    Some(InternalStackValue::FinallyPending { target, .. })
                        if *target == successor
                ) {
                    return Err(finally_stack_error(
                        id,
                        decoded.pc(),
                        decoded.instruction().opcode(),
                    ));
                }
                state[pending_index] = original.into_internal();
            }
        }
        if !has_successor {
            verify_internal_stack_exit(
                id,
                decoded,
                &state,
                finally_continuations
                    .iter()
                    .any(|continuations| !continuations.is_empty()),
            )?;
        }
    }

    charge(
        &mut usage.policy_transfers,
        evaluations,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    Ok(InternalStackCertificate {
        iteration_local_puts,
        catch_local_puts,
        nip_catch_transforms,
        finally_continuations,
        ret_finalizers,
    })
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the classifier shares the graph resource limits and usage ledger with the typed stack pass"
)]
fn classify_iteration_declarative_local_puts(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    variables: &[VariableDefinition],
    certificate: &mut InternalStackCertificate,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<(), BytecodeVerificationError> {
    if certificate.iteration_local_puts.iter().all(Option::is_none) {
        return Ok(());
    }

    let argument_count = flow.domains().argument_count() as usize;
    let local_count = variables.len() - argument_count;
    charge_frame_state_entries(id, usage, local_count, limits.max_frame_state_entries)?;
    let mut summaries = try_filled_vec(
        id,
        local_count,
        IterationLocalPutSummary::default(),
        BytecodeGraphResource::FrameStateEntries,
    )?;
    let mut evaluations = 0_u64;

    for (index, verified) in flow.instructions().iter().enumerate() {
        let decoded = verified.decoded();
        let instruction = decoded.instruction();
        let opcode = instruction.opcode();
        if !is_unchecked_local_put(opcode) {
            continue;
        }
        charge_policy_transfers(
            id,
            &mut evaluations,
            1,
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let Some(local) = local_operand(opcode, instruction.operands()) else {
            return Err(for_in_stack_error(id, decoded.pc(), opcode));
        };
        let Some(summary) = summaries.get_mut(local as usize) else {
            return Err(for_in_stack_error(id, decoded.pc(), opcode));
        };
        summary.unchecked_puts = summary.unchecked_puts.saturating_add(1);

        let certified = certificate
            .iteration_local_puts
            .get(index)
            .copied()
            .flatten();
        let Some(certified) = certified else {
            summary.has_uncertified_put = true;
            continue;
        };
        if certified.local != local {
            return Err(for_in_stack_error(id, decoded.pc(), opcode));
        }
        summary.certified_puts = summary.certified_puts.saturating_add(1);
        summary.first_certified_pc.get_or_insert(decoded.pc());
        match summary.cursor_site {
            Some(site) if site != certified.cursor_site => {
                summary.multiple_cursor_sites = true;
            }
            Some(_) => {}
            None => summary.cursor_site = Some(certified.cursor_site),
        }
    }

    for (local, summary) in summaries.iter_mut().enumerate() {
        if summary.certified_puts == 0 {
            continue;
        }
        charge_policy_transfers(
            id,
            &mut evaluations,
            1,
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        let definition = &variables[argument_count + local];
        // Mutable lexical writes are normally proven safe by the binding-state
        // pass once their scope is active. A captured per-iteration `let`
        // declaration is different: its unchecked iterator-head put must be
        // certified so the binding-state pass can require the previous cell
        // to be closed before the backedge reinitializes it. Ordinary
        // iterator-backed assignments use checked puts; mixed or uncertified
        // unchecked puts therefore cannot claim declaration authority.
        let captured_mutable_iteration_declaration = definition.policy.writes
            == CompilerWritePolicy::Mutable
            && definition.policy.temporal_dead_zone
            && definition.has_scope
            && definition.variable_reference.is_some()
            && !summary.has_uncertified_put
            && summary.unchecked_puts == summary.certified_puts;
        if definition.policy.writes == CompilerWritePolicy::Mutable
            && !captured_mutable_iteration_declaration
        {
            continue;
        }
        if definition.policy.temporal_dead_zone && summary.multiple_cursor_sites {
            return Err(policy_error(
                id,
                BindingSlot::Local(usize_to_u32(local)),
                summary.first_certified_pc,
                BindingPolicyViolationReason::InvalidLexicalInitialization,
            ));
        }
        summary.declarative_authority = definition.policy.temporal_dead_zone
            && !summary.has_uncertified_put
            && summary.unchecked_puts == summary.certified_puts;
    }

    for certified in &mut certificate.iteration_local_puts {
        let Some(local_put) = *certified else {
            continue;
        };
        charge_policy_transfers(
            id,
            &mut evaluations,
            1,
            usage.policy_transfers,
            limits.max_policy_transfers,
        )?;
        if !summaries[local_put.local as usize].declarative_authority {
            *certified = None;
        }
    }

    charge(
        &mut usage.policy_transfers,
        evaluations,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )
}
