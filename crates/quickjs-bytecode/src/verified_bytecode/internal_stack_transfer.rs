#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "marker isolation, edge-specific handler and iteration provenance, and ordinary stack transfer form one typed opcode boundary"
)]
fn transfer_internal_operand_stack(
    id: FunctionTemplateId,
    instruction_index: usize,
    decoded: crate::DecodedInstruction,
    effectively_reachable: bool,
    catch_handler: Option<InstructionIndex>,
    instructions: &[VerifiedInstruction],
    state: &mut Vec<InternalStackValue>,
    iteration_local_puts: &mut [Option<CertifiedIterationLocalPut>],
    catch_local_puts: &mut [Option<CertifiedCatchLocalPut>],
    nip_catch_transforms: &mut [Option<CertifiedNipCatchTransform>],
    ret_finalizers: &mut [Option<InstructionIndex>],
) -> Result<InternalStackTransfer, BytecodeVerificationError> {
    let instruction = decoded.instruction();
    let opcode = instruction.opcode();
    match opcode {
        FinalOpcode::SpecialObject => {
            let Operands::U8(selector) = instruction.operands() else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            match selector {
                // The only admitted producer of an active derived-constructor
                // capability. `get_super` must consume it immediately in the
                // typed stack transfer below.
                4 => {
                    state.try_reserve(1).map_err(|_| {
                        BytecodeVerificationError::function(
                            id,
                            BytecodeVerificationErrorKind::AllocationFailed {
                                resource: BytecodeGraphResource::FrameStateEntries,
                                requested: 1,
                            },
                        )
                    })?;
                    state.push(InternalStackValue::DerivedActiveConstructor(decoded.pc()));
                    return Ok(InternalStackTransfer {
                        normal_completion: true,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                // `new.target` becomes a derived-super capability only when
                // it immediately follows the typed superclass constructor.
                // Other source-level uses retain ordinary JavaScript-value
                // treatment through the generic stack transfer.
                3 if matches!(
                    state.last(),
                    Some(InternalStackValue::DerivedSuperConstructor(_))
                ) =>
                {
                    let Some(InternalStackValue::DerivedSuperConstructor(site)) =
                        state.last().copied()
                    else {
                        unreachable!("the derived superclass guard established the value")
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
                    state.push(InternalStackValue::DerivedSuperNewTarget(site));
                    return Ok(InternalStackTransfer {
                        normal_completion: true,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                _ => {}
            }
        }
        FinalOpcode::GetSuper
            if matches!(
                state.last(),
                Some(InternalStackValue::DerivedActiveConstructor(_))
            ) =>
        {
            // In a derived constructor `get_super` consumes the typed
            // superclass-constructor capability. In a method (or a static
            // field initializer) it instead consumes an ordinary home object
            // and follows the generic JavaScript-value transfer below.
            *state
                .last_mut()
                .expect("derived active constructor is present") =
                InternalStackValue::DerivedSuperConstructor(decoded.pc());
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::CallConstructor => {
            let Some(argument_count) = instruction.operands().dynamic_argument_count() else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            let required = usize::from(argument_count).saturating_add(2);
            let Some(base) = state.len().checked_sub(required) else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if let (
                InternalStackValue::DerivedSuperConstructor(super_site),
                InternalStackValue::DerivedSuperNewTarget(target_site),
            ) = (state[base], state[base + 1])
            {
                if super_site != target_site
                    || state[base + 2..]
                        .iter()
                        .any(|value| !value.is_javascript_value())
                {
                    return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                }
                state.truncate(base);
                state.push(InternalStackValue::DerivedSuperResult(decoded.pc()));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::Apply if matches!(instruction.operands(), Operands::U16(2)) => {
            let Some(base) = state.len().checked_sub(3) else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if let (
                InternalStackValue::DerivedSuperConstructor(super_site),
                InternalStackValue::DerivedSuperNewTarget(target_site),
            ) = (state[base], state[base + 1])
            {
                if super_site != target_site || !state[base + 2].is_javascript_value() {
                    return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                }
                state.truncate(base);
                state.push(InternalStackValue::DerivedSuperResult(decoded.pc()));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            // Operand mode two is reserved for the proven derived-super
            // transaction. Do not let hand-authored bytecode fall through to
            // ordinary `apply` transfer and reach the runtime construction
            // path without those capabilities.
            return Err(internal_stack_error(id, decoded.pc(), opcode, state));
        }
        FinalOpcode::CheckCtorReturn => {
            let Some(InternalStackValue::DerivedSuperResult(site)) = state.last().copied() else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
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
            state.push(InternalStackValue::DerivedSuperCompletion(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Drop
            if matches!(
                state.last(),
                Some(InternalStackValue::DerivedSuperCompletion(_))
            ) =>
        {
            let Some(base) = state.len().checked_sub(2) else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            let (
                InternalStackValue::DerivedSuperResult(result_site),
                InternalStackValue::DerivedSuperCompletion(completion_site),
            ) = (state[base], state[base + 1])
            else {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if result_site != completion_site {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            }
            state[base] = InternalStackValue::Ordinary;
            state.truncate(base + 1);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Catch => {
            invalidate_internal_value_provenance(state);
            let Some(handler) = catch_handler else {
                return Err(catch_stack_error(id, decoded.pc(), opcode));
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
            state.push(InternalStackValue::CatchMarker {
                site: decoded.pc(),
                handler,
            });
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForInStart => {
            invalidate_internal_value_provenance(state);
            let Some(input) = state.last_mut() else {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            };
            if *input != InternalStackValue::Ordinary {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            }
            *input = InternalStackValue::ForInIterator(decoded.pc());
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForInNext => {
            invalidate_internal_value_provenance(state);
            let Some(InternalStackValue::ForInIterator(site)) = state.last().copied() else {
                return Err(for_in_stack_error(id, decoded.pc(), opcode));
            };
            state.try_reserve(2).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 2,
                    },
                )
            })?;
            state.push(InternalStackValue::ForInKey(site));
            state.push(InternalStackValue::ForInDone(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForOfStart | FinalOpcode::ForAwaitOfStart => {
            invalidate_internal_value_provenance(state);
            let Some(input) = state.last_mut() else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if *input != InternalStackValue::Ordinary {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            let site = decoded.pc();
            *input = InternalStackValue::ForOfIterator(site);
            state.try_reserve(2).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 2,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfNextMethod(site));
            state.push(InternalStackValue::ForOfCatch(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForOfNext => {
            let Operands::U8(offset) = instruction.operands() else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            invalidate_internal_value_provenance(state);
            let Some(base) = state
                .len()
                .checked_sub(3_usize.saturating_add(usize::from(offset)))
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfCatch(catch),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next
                || next != catch
                || for_of_start_is_async(instructions, iterator) != Some(false)
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state.try_reserve(2).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 2,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfValue(iterator));
            state.push(InternalStackValue::ForOfDone(iterator));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::ForAwaitOfNext => {
            invalidate_internal_value_provenance(state);
            let Some(base) = state.len().checked_sub(3) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfCatch(catch),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next
                || next != catch
                || for_of_start_is_async(instructions, iterator) != Some(true)
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state[base + 2] = InternalStackValue::ForOfDisabledCatch(iterator);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfAwaitResult(iterator));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Await
            if matches!(state.last(), Some(InternalStackValue::ForOfAwaitResult(_))) =>
        {
            let Some(InternalStackValue::ForOfAwaitResult(site)) = state.pop() else {
                unreachable!("the for-await result guard established the top value")
            };
            state.push(InternalStackValue::ForOfAwaitedResult(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IteratorGetValueDone => {
            let Some(base) = state.len().checked_sub(4) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::ForOfIterator(iterator),
                InternalStackValue::ForOfNextMethod(next),
                InternalStackValue::ForOfDisabledCatch(catch),
                InternalStackValue::ForOfAwaitedResult(result),
            ) = (
                state[base],
                state[base + 1],
                state[base + 2],
                state[base + 3],
            )
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next
                || next != catch
                || catch != result
                || for_of_start_is_async(instructions, iterator) != Some(true)
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state[base + 2] = InternalStackValue::ForOfCatch(iterator);
            state[base + 3] = InternalStackValue::ForOfValue(iterator);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::ForOfDone(iterator));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IteratorNext => {
            let Some(base) = state.len().checked_sub(4) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::YieldStarIterator(iterator),
                InternalStackValue::YieldStarNextMethod(next),
                InternalStackValue::YieldStarDummy(dummy),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next || next != dummy || !state[base + 3].is_javascript_value() {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state[base + 3] = InternalStackValue::YieldStarIteratorResult(iterator);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IteratorCheckObject
            if matches!(
                state.last(),
                Some(
                    InternalStackValue::YieldStarIteratorResult(_)
                        | InternalStackValue::YieldStarYieldResult(_)
                        | InternalStackValue::YieldStarFinalResult(_)
                )
            ) =>
        {
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::GetField2
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarIteratorResult(_))
            ) =>
        {
            let Some(InternalStackValue::YieldStarIteratorResult(site)) = state.last().copied()
            else {
                unreachable!("delegated iterator result guard established the value")
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
            state.push(InternalStackValue::YieldStarDone(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::GetField
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarYieldResult(_))
            ) =>
        {
            let Some(InternalStackValue::YieldStarYieldResult(site)) = state.last().copied() else {
                unreachable!("delegated yield result guard established the value")
            };
            *state.last_mut().expect("matched delegated yield result") =
                InternalStackValue::YieldStarYieldValue(site);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::GetField
            if matches!(
                state.last(),
                Some(
                    InternalStackValue::YieldStarIteratorResult(_)
                        | InternalStackValue::YieldStarFinalResult(_)
                )
            ) =>
        {
            *state.last_mut().expect("matched delegated result") = InternalStackValue::Ordinary;
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::YieldStar | FinalOpcode::AsyncYieldStar => {
            let expected = match opcode {
                FinalOpcode::YieldStar => state.last().copied().and_then(|value| match value {
                    InternalStackValue::YieldStarYieldResult(site) => Some(site),
                    _ => None,
                }),
                FinalOpcode::AsyncYieldStar => {
                    state.last().copied().and_then(|value| match value {
                        InternalStackValue::YieldStarYieldValue(site) => Some(site),
                        _ => None,
                    })
                }
                _ => unreachable!("matched delegated yield opcode"),
            };
            let Some(site) = expected else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            *state.last_mut().expect("matched delegated result") =
                InternalStackValue::YieldStarResumeValue(site);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::YieldStarResumeMode(site));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Await
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarIteratorResult(_))
            ) =>
        {
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Dup => {
            if let Some(InternalStackValue::YieldStarResumeMode(site)) = state.last().copied() {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::YieldStarResumeModeTest(site));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::Push2 => {
            if matches!(
                state.last(),
                Some(InternalStackValue::YieldStarResumeMode(_))
            ) {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::Ordinary);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::StrictEq => {
            if let Some(base) = state.len().checked_sub(2)
                && let InternalStackValue::YieldStarResumeMode(site) = state[base]
                && state[base + 1] == InternalStackValue::Ordinary
            {
                state[base] = InternalStackValue::YieldStarIsThrow(site);
                state.truncate(base + 1);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::IteratorCall => {
            let Operands::U8(flags) = instruction.operands() else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let Some(base) = state.len().checked_sub(4) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::YieldStarIterator(iterator),
                InternalStackValue::YieldStarNextMethod(next),
                InternalStackValue::YieldStarDummy(dummy),
            ) = (state[base], state[base + 1], state[base + 2])
            else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let InternalStackValue::YieldStarResumeValue(value_site) = state[base + 3] else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            if iterator != next || next != dummy || dummy != value_site {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            let kind = match flags {
                0 => YieldStarCallKind::Return,
                1 => YieldStarCallKind::Throw,
                2 => YieldStarCallKind::Close,
                _ => return Err(for_of_stack_error(id, decoded.pc(), opcode)),
            };
            state[base + 3] = InternalStackValue::YieldStarCallValue(iterator, kind);
            state.try_reserve(1).map_err(|_| {
                BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::AllocationFailed {
                        resource: BytecodeGraphResource::FrameStateEntries,
                        requested: 1,
                    },
                )
            })?;
            state.push(InternalStackValue::YieldStarMethodMissing(iterator, kind));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 => {
            if let Some(InternalStackValue::YieldStarDone(site)) = state.last().copied()
                && matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::YieldStarIteratorResult(result_site))
                        if *result_site == site
                )
            {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::YieldStarDone {
                        site,
                        branch_when_true: false,
                    }),
                    ret_finalizer: None,
                });
            }
            if let Some((record_index, site)) = for_of_branch_record(state) {
                let value_index = state.len().saturating_sub(2);
                let extras = value_index
                    .saturating_sub(1)
                    .saturating_sub(record_index.saturating_add(2));
                state.pop();
                invalidate_internal_value_provenance(&mut state[..record_index]);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::ForOf { site, extras }),
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(3)
                && let (
                    InternalStackValue::ForInIterator(iterator),
                    InternalStackValue::ForInKey(key),
                    InternalStackValue::ForInDone(done),
                ) = (state[base], state[base + 1], state[base + 2])
                && iterator == key
                && key == done
            {
                state.pop();
                invalidate_internal_value_provenance(&mut state[..base]);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::ForIn(key)),
                    ret_finalizer: None,
                });
            }
            if matches!(state.last(), Some(InternalStackValue::ForOfDone(_))) {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
        }
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8 => {
            if let Some(InternalStackValue::YieldStarDone(site)) = state.last().copied()
                && matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::YieldStarIteratorResult(result_site))
                        if *result_site == site
                )
            {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::YieldStarDone {
                        site,
                        branch_when_true: true,
                    }),
                    ret_finalizer: None,
                });
            }
            if matches!(
                state.last(),
                Some(
                    InternalStackValue::YieldStarResumeModeTest(_)
                        | InternalStackValue::YieldStarIsThrow(_)
                )
            ) {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(InternalStackValue::YieldStarMethodMissing(site, kind)) =
                state.last().copied()
                && matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::YieldStarCallValue(value_site, value_kind))
                        if *value_site == site && *value_kind == kind
                )
            {
                state.pop();
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: Some(IterationBranchValue::YieldStarMethod {
                        site,
                        kind,
                    }),
                    ret_finalizer: None,
                });
            }
            if matches!(state.last(), Some(InternalStackValue::ForOfDone(_))) {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
        }
        opcode if is_unchecked_local_put(opcode) => {
            if let Some(InternalStackValue::CatchException(_catch_site)) = state.last().copied() {
                let Some(local) = local_operand(opcode, instruction.operands()) else {
                    return Err(catch_stack_error(id, decoded.pc(), opcode));
                };
                let Some(certificate) = catch_local_puts.get_mut(instruction_index) else {
                    return Err(catch_stack_error(id, decoded.pc(), opcode));
                };
                *certificate = Some(CertifiedCatchLocalPut { local });
                state.pop();
                invalidate_internal_value_provenance(state);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(marker) = state.len().checked_sub(2)
                && let (
                    InternalStackValue::ForInIterator(iterator),
                    InternalStackValue::ForInHeadKey(key),
                ) = (state[marker], state[marker + 1])
                && iterator == key
            {
                let Some(local) = local_operand(opcode, instruction.operands()) else {
                    return Err(for_in_stack_error(id, decoded.pc(), opcode));
                };
                let Some(certificate) = iteration_local_puts.get_mut(instruction_index) else {
                    return Err(for_in_stack_error(id, decoded.pc(), opcode));
                };
                *certificate = Some(CertifiedIterationLocalPut {
                    local,
                    cursor_site: key,
                });
                state.pop();
                invalidate_internal_value_provenance(state);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(marker) = state.len().checked_sub(4)
                && let (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch),
                    InternalStackValue::ForOfHeadValue(value),
                ) = (
                    state[marker],
                    state[marker + 1],
                    state[marker + 2],
                    state[marker + 3],
                )
                && iterator == next
                && next == catch
                && catch == value
            {
                let Some(local) = local_operand(opcode, instruction.operands()) else {
                    return Err(for_of_stack_error(id, decoded.pc(), opcode));
                };
                let Some(certificate) = iteration_local_puts.get_mut(instruction_index) else {
                    return Err(for_of_stack_error(id, decoded.pc(), opcode));
                };
                *certificate = Some(CertifiedIterationLocalPut {
                    local,
                    cursor_site: value,
                });
                state.pop();
                invalidate_internal_value_provenance(state);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::Drop => {
            if let Some(base) = state.len().checked_sub(3)
                && let (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch),
                ) = (state[base], state[base + 1], state[base + 2])
                && iterator == next
                && next == catch
            {
                state[base] = InternalStackValue::YieldStarIterator(iterator);
                state[base + 1] = InternalStackValue::YieldStarNextMethod(iterator);
                state.truncate(base + 2);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(2)
                && let (
                    InternalStackValue::YieldStarResumeValue(value),
                    InternalStackValue::YieldStarResumeMode(mode),
                ) = (state[base], state[base + 1])
                && value == mode
            {
                state[base] = InternalStackValue::Ordinary;
                state.truncate(base + 1);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            // A `for (const [value] of iterable)` head starts an inner
            // iterator for the pattern. Dropping that iterator's done flag
            // leaves the element value above its record. Preserve a distinct
            // head value only when an enclosing complete for-of record proves
            // that this nested iterator belongs to an iteration head. The
            // following lexical store can then be certified as the permitted
            // fresh initialization of a captured per-iteration binding.
            if let Some(base) = state.len().checked_sub(5)
                && let (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch),
                    InternalStackValue::ForOfValue(value),
                    InternalStackValue::ForOfDone(done),
                ) = (
                    state[base],
                    state[base + 1],
                    state[base + 2],
                    state[base + 3],
                    state[base + 4],
                )
                && iterator == next
                && next == catch
                && catch == value
                && value == done
                && has_enclosing_for_of_record(&state[..base])
            {
                state.truncate(base + 4);
                state[base + 3] = InternalStackValue::ForOfHeadValue(value);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if state.is_empty() {
                if effectively_reachable {
                    return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                }
                return Ok(InternalStackTransfer {
                    normal_completion: false,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if state
                .last()
                .is_some_and(|value| value.is_for_of_value() && !value.is_javascript_value())
            {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            if let Some(InternalStackValue::FinallyReturn { target }) = state.last().copied()
                && !matches!(
                    state.get(state.len().saturating_sub(2)),
                    Some(InternalStackValue::FinallyPending {
                        target: pending_target,
                        ..
                    }) if *pending_target == target
                )
            {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            }
            state.pop();
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Nip => {
            let Some(value_index) = state.len().checked_sub(1) else {
                if !effectively_reachable {
                    return Ok(InternalStackTransfer {
                        normal_completion: false,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            let Some(marker_index) = value_index.checked_sub(1) else {
                if !effectively_reachable {
                    return Ok(InternalStackTransfer {
                        normal_completion: false,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            };
            if !state[value_index].is_javascript_value() {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            }
            if matches!(
                state[marker_index],
                InternalStackValue::YieldStarIterator(_)
                    | InternalStackValue::YieldStarNextMethod(_)
                    | InternalStackValue::YieldStarDummy(_)
            ) {
                state[marker_index] = state[value_index];
                state.truncate(value_index);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            match state[marker_index] {
                InternalStackValue::ForInIterator(_)
                | InternalStackValue::FinallyPending { .. } => {}
                InternalStackValue::FinallyReturn { target } => {
                    if !matches!(
                        marker_index
                            .checked_sub(1)
                            .and_then(|pending| state.get(pending)),
                        Some(InternalStackValue::FinallyPending {
                            target: pending_target,
                            ..
                        }) if *pending_target == target
                    ) {
                        return Err(internal_stack_error(id, decoded.pc(), opcode, state));
                    }
                }
                _ => return Err(internal_stack_error(id, decoded.pc(), opcode, state)),
            }
            state[marker_index] = state[value_index];
            state.truncate(value_index);
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::NipCatch => {
            let Some(value_index) = state.len().checked_sub(1) else {
                if !effectively_reachable {
                    return Ok(InternalStackTransfer {
                        normal_completion: false,
                        iteration_branch_value: None,
                        ret_finalizer: None,
                    });
                }
                return Err(catch_stack_error(id, decoded.pc(), opcode));
            };
            if !state[value_index].is_javascript_value() {
                return Err(internal_stack_error(id, decoded.pc(), opcode, state));
            }
            let Some(marker_index) = state[..value_index].iter().rposition(|value| {
                matches!(
                    value,
                    InternalStackValue::CatchMarker { .. }
                        | InternalStackValue::ForOfCatch(_)
                        | InternalStackValue::ForOfExhaustedCatch(_)
                        | InternalStackValue::ForOfClosableCatch(_)
                )
            }) else {
                if state.iter().any(|value| value.is_for_of_value()) {
                    return Err(for_of_stack_error(id, decoded.pc(), opcode));
                }
                return Err(catch_stack_error(id, decoded.pc(), opcode));
            };
            let for_of_site = match state[marker_index] {
                InternalStackValue::CatchMarker { .. } => None,
                InternalStackValue::ForOfCatch(site) => {
                    let Some(record_start) = marker_index.checked_sub(2) else {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    };
                    if !matches!(
                        (state[record_start], state[record_start + 1]),
                        (
                            InternalStackValue::ForOfIterator(iterator),
                            InternalStackValue::ForOfNextMethod(next)
                        ) if iterator == site && next == site
                    ) {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    }
                    Some(site)
                }
                InternalStackValue::ForOfExhaustedCatch(site) => {
                    let Some(record_start) = marker_index.checked_sub(2) else {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    };
                    if !matches!(
                        (state[record_start], state[record_start + 1]),
                        (
                            InternalStackValue::ForOfExhaustedIterator(iterator),
                            InternalStackValue::ForOfExhaustedNextMethod(next)
                        ) if iterator == site && next == site
                    ) {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    }
                    Some(site)
                }
                InternalStackValue::ForOfClosableCatch(site) => {
                    let Some(record_start) = marker_index.checked_sub(2) else {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    };
                    if !matches!(
                        (state[record_start], state[record_start + 1]),
                        (
                            InternalStackValue::ForOfClosableIterator(iterator),
                            InternalStackValue::ForOfClosableNextMethod(next)
                        ) if iterator == site && next == site
                    ) {
                        return Err(for_of_stack_error(id, decoded.pc(), opcode));
                    }
                    Some(site)
                }
                _ => return Err(internal_stack_error(id, decoded.pc(), opcode, state)),
            };
            let marker_is_for_of = for_of_site.is_some();
            let mut cursor = marker_index + 1;
            while cursor < value_index {
                match state[cursor] {
                    value if value.is_javascript_value() => {
                        cursor += 1;
                    }
                    InternalStackValue::FinallyPending { target, .. } => {
                        if !matches!(
                            state.get(cursor + 1),
                            Some(InternalStackValue::FinallyReturn {
                                target: return_target
                            }) if *return_target == target && cursor + 1 < value_index
                        ) {
                            return Err(finally_stack_error(id, decoded.pc(), opcode));
                        }
                        cursor += 2;
                    }
                    InternalStackValue::FinallyReturn { .. } => {
                        return Err(finally_stack_error(id, decoded.pc(), opcode));
                    }
                    _ => {
                        return Err(if marker_is_for_of {
                            for_of_stack_error(id, decoded.pc(), opcode)
                        } else {
                            catch_stack_error(id, decoded.pc(), opcode)
                        });
                    }
                }
            }
            let transform = CertifiedNipCatchTransform {
                input_depth: usize_to_u32(state.len()),
                retained_prefix: usize_to_u32(marker_index),
            };
            let Some(certificate) = nip_catch_transforms.get_mut(instruction_index) else {
                return Err(if marker_is_for_of {
                    for_of_stack_error(id, decoded.pc(), opcode)
                } else {
                    catch_stack_error(id, decoded.pc(), opcode)
                });
            };
            match *certificate {
                Some(established) if established != transform => {
                    return Err(if marker_is_for_of {
                        for_of_stack_error(id, decoded.pc(), opcode)
                    } else {
                        catch_stack_error(id, decoded.pc(), opcode)
                    });
                }
                Some(_) => {}
                None => *certificate = Some(transform),
            }
            state.truncate(marker_index);
            invalidate_internal_value_provenance(state);
            state.push(for_of_site.map_or(
                InternalStackValue::Ordinary,
                InternalStackValue::ForOfReturnValue,
            ));
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Rot3r => {
            let Some(base) = state.len().checked_sub(3) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let iterator = match (state[base], state[base + 1], state[base + 2]) {
                (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfReturnValue(completion),
                )
                | (
                    InternalStackValue::ForOfExhaustedIterator(iterator),
                    InternalStackValue::ForOfExhaustedNextMethod(next),
                    InternalStackValue::ForOfReturnValue(completion),
                )
                | (
                    InternalStackValue::ForOfClosableIterator(iterator),
                    InternalStackValue::ForOfClosableNextMethod(next),
                    InternalStackValue::ForOfReturnValue(completion),
                ) if iterator == next && next == completion => iterator,
                _ => return Err(for_of_stack_error(id, decoded.pc(), opcode)),
            };
            state[base] = InternalStackValue::Ordinary;
            state[base + 1] = InternalStackValue::ForOfCloseIterator(iterator);
            state[base + 2] = InternalStackValue::ForOfCloseNextMethod(iterator);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Undefined => {
            if let Some(base) = state.len().checked_sub(2)
                && let (
                    InternalStackValue::YieldStarIterator(iterator),
                    InternalStackValue::YieldStarNextMethod(next),
                ) = (state[base], state[base + 1])
                && iterator == next
            {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::YieldStarDummy(iterator));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(3)
                && let (
                    InternalStackValue::YieldStarIterator(iterator),
                    InternalStackValue::YieldStarNextMethod(next),
                    InternalStackValue::YieldStarDummy(dummy),
                ) = (state[base], state[base + 1], state[base + 2])
                && iterator == next
                && next == dummy
            {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::Ordinary);
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            if let Some(base) = state.len().checked_sub(3)
                && state[base].is_javascript_value()
                && let (
                    InternalStackValue::ForOfCloseIterator(iterator),
                    InternalStackValue::ForOfCloseNextMethod(next),
                ) = (state[base + 1], state[base + 2])
                && iterator == next
            {
                state.try_reserve(1).map_err(|_| {
                    BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::AllocationFailed {
                            resource: BytecodeGraphResource::FrameStateEntries,
                            requested: 1,
                        },
                    )
                })?;
                state.push(InternalStackValue::ForOfCloseDummy(iterator));
                return Ok(InternalStackTransfer {
                    normal_completion: true,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
        }
        FinalOpcode::IteratorClose => {
            let Some(base) = state.len().checked_sub(3) else {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            };
            let valid = matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfIterator(iterator),
                    InternalStackValue::ForOfNextMethod(next),
                    InternalStackValue::ForOfCatch(catch)
                ) if iterator == next && next == catch
            ) || matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfExhaustedIterator(iterator),
                    InternalStackValue::ForOfExhaustedNextMethod(next),
                    InternalStackValue::ForOfExhaustedCatch(catch)
                ) if iterator == next && next == catch
            ) || matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfClosableIterator(iterator),
                    InternalStackValue::ForOfClosableNextMethod(next),
                    InternalStackValue::ForOfClosableCatch(catch)
                ) if iterator == next && next == catch
            ) || matches!(
                (state[base], state[base + 1], state[base + 2]),
                (
                    InternalStackValue::ForOfCloseIterator(iterator),
                    InternalStackValue::ForOfCloseNextMethod(next),
                    InternalStackValue::ForOfCloseDummy(dummy)
                ) if iterator == next && next == dummy
            );
            if !valid {
                return Err(for_of_stack_error(id, decoded.pc(), opcode));
            }
            state.truncate(base);
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        FinalOpcode::Ret => {
            if !effectively_reachable && state.is_empty() {
                return Ok(InternalStackTransfer {
                    normal_completion: false,
                    iteration_branch_value: None,
                    ret_finalizer: None,
                });
            }
            let Some(pair_start) = state.len().checked_sub(2) else {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            };
            let (
                InternalStackValue::FinallyPending {
                    target: pending_target,
                    original,
                },
                InternalStackValue::FinallyReturn {
                    target: return_target,
                },
            ) = (state[pair_start], state[pair_start + 1])
            else {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            };
            if pending_target != return_target {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            }
            let target = return_target;
            state.truncate(pair_start);
            state.push(original.into_internal());
            let Some(certificate) = ret_finalizers.get_mut(instruction_index) else {
                return Err(finally_stack_error(id, decoded.pc(), opcode));
            };
            match *certificate {
                Some(established) if established != target => {
                    return Err(finally_stack_error(id, decoded.pc(), opcode));
                }
                Some(_) => {}
                None => *certificate = Some(target),
            }
            invalidate_internal_value_provenance(state);
            return Ok(InternalStackTransfer {
                normal_completion: true,
                iteration_branch_value: None,
                ret_finalizer: Some(target),
            });
        }
        _ => {}
    }

    if state.iter().any(|value| {
        matches!(
            value,
            InternalStackValue::ForOfCloseIterator(_)
                | InternalStackValue::ForOfCloseNextMethod(_)
                | InternalStackValue::ForOfCloseDummy(_)
        )
    }) {
        return Err(for_of_stack_error(id, decoded.pc(), opcode));
    }

    invalidate_internal_value_provenance(state);
    let effect = instruction
        .stack_effect()
        .map_err(|_| internal_stack_error(id, decoded.pc(), opcode, state))?;
    let pops = effect.pops() as usize;
    let pushes = effect.pushes() as usize;
    let Some(input_start) = state.len().checked_sub(pops) else {
        if !effectively_reachable {
            return Ok(InternalStackTransfer {
                normal_completion: false,
                iteration_branch_value: None,
                ret_finalizer: None,
            });
        }
        return Err(internal_stack_error(id, decoded.pc(), opcode, state));
    };
    if state[input_start..]
        .iter()
        .any(|value| !value.is_javascript_value())
    {
        return Err(internal_stack_error(
            id,
            decoded.pc(),
            opcode,
            &state[input_start..],
        ));
    }
    let output_len = input_start
        .checked_add(pushes)
        .ok_or_else(|| internal_stack_error(id, decoded.pc(), opcode, state))?;
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
    state.truncate(input_start);
    state.resize(output_len, InternalStackValue::Ordinary);
    Ok(InternalStackTransfer {
        normal_completion: true,
        iteration_branch_value: None,
        ret_finalizer: None,
    })
}
