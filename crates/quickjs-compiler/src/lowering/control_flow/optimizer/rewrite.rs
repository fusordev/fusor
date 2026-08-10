use quickjs_bytecode::{
    AssemblerError, AssemblerLabel, AssemblerLimits, BranchKind, BytecodeAssembler, FinalOpcode,
    FunctionKind, InstructionIndex, Operands, VerificationLimits, VerifiedControlFlow,
    VerifiedInstruction, VerifiedSuccessorKind,
};

use super::{
    super::{LeafCompilationError, ResolvedStackAnchor, SourceInstruction},
    OptimizedControlFlow,
    analysis::ControlFlowFacts,
    metadata,
};

struct RebuiltInstructions {
    bytecode: Vec<u8>,
    instruction_pcs: Vec<quickjs_bytecode::BytecodePc>,
    origins: Vec<usize>,
    prefix_counts: Vec<u32>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rebuild_optimized_control_flow(
    control_flow: &VerifiedControlFlow,
    source_instructions: &[SourceInstruction],
    eval_reference_call_instructions: &[u32],
    parameter_initialization_end: Option<u32>,
    function_initializer_prefix_start: u32,
    stack_anchors: &[ResolvedStackAnchor],
    facts: &ControlFlowFacts,
    limits: VerificationLimits,
) -> Result<OptimizedControlFlow, LeafCompilationError> {
    let instruction_count = control_flow.instructions().len();
    if facts.instruction_count() != instruction_count {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "CFG optimization facts cover every verified instruction",
            span: None,
        });
    }

    let rebuilt =
        assemble_optimized_control_flow(control_flow, source_instructions, facts, limits)?;
    let RebuiltInstructions {
        bytecode,
        instruction_pcs,
        origins,
        prefix_counts,
    } = rebuilt;

    let source_instructions =
        metadata::remap_sources(&instruction_pcs, &origins, source_instructions)?;
    let eval_reference_call_instructions = metadata::remap_instruction_metadata(
        eval_reference_call_instructions,
        &prefix_counts,
        facts,
    )?;
    let parameter_initialization_end = parameter_initialization_end
        .map(|boundary| metadata::remap_boundary(boundary, &prefix_counts))
        .transpose()?;
    let function_initializer_prefix_start =
        metadata::remap_boundary(function_initializer_prefix_start, &prefix_counts)?;
    let stack_anchors = metadata::remap_stack_anchors(
        control_flow,
        stack_anchors,
        &prefix_counts,
        &instruction_pcs,
        facts,
    )?;

    Ok(OptimizedControlFlow {
        bytecode,
        source_instructions,
        eval_reference_call_instructions,
        parameter_initialization_end,
        function_initializer_prefix_start,
        stack_anchors,
    })
}

fn assemble_optimized_control_flow(
    control_flow: &VerifiedControlFlow,
    sources: &[SourceInstruction],
    facts: &ControlFlowFacts,
    limits: VerificationLimits,
) -> Result<RebuiltInstructions, LeafCompilationError> {
    let instruction_count = control_flow.instructions().len();
    let assembler_limits = AssemblerLimits::new(
        limits.max_bytecode_bytes_per_function(),
        limits.max_instructions_per_function(),
        limits.max_transfer_evaluations(),
    );
    let mut plan = BytecodeAssembler::with_limits(assembler_limits);
    let (labels, label_spans) = allocate_labels(&mut plan, sources, facts)?;
    let mut origins = reserved_vec(
        instruction_count.saturating_add(1),
        "optimized instruction origins",
    )?;
    let mut prefix_counts = reserved_vec(
        instruction_count.saturating_add(1),
        "optimized instruction boundaries",
    )?;
    prefix_counts.push(0_u32);
    let mut last_retained = None;

    for (position, verified) in control_flow.instructions().iter().copied().enumerate() {
        if facts.is_retained(position) {
            last_retained = Some(position);
            let span = sources[position].span();
            let label = labels.get(position).and_then(Option::as_ref).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "retained optimized instruction has a symbolic label",
                    span: Some(span),
                },
            )?;
            plan.bind(label)
                .map_err(|source| LeafCompilationError::BytecodeAssembly {
                    span: Some(span),
                    source,
                })?;
            emit_rewritten_instruction(
                &mut plan,
                &mut origins,
                &labels,
                position,
                verified,
                facts.branch_outcome(position),
                span,
            )?;
        }
        prefix_counts.push(u32_from_usize(
            origins.len(),
            "optimized instruction boundary",
        )?);
    }

    if let Some(position) = last_retained
        && control_flow.instructions()[position].successors().kind()
            == VerifiedSuccessorKind::Fallthrough
    {
        emit_disconnected_terminal(
            &mut plan,
            &mut origins,
            position,
            sources[position].span(),
            control_flow.function_header().kind(),
        )?;
        let final_boundary =
            prefix_counts
                .last_mut()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "optimized instruction boundaries retain their final boundary",
                    span: Some(sources[position].span()),
                })?;
        *final_boundary = u32_from_usize(origins.len(), "optimized final instruction boundary")?;
    }

    let assembly = finish_assembler(plan, &origins, sources, &label_spans)?;
    let (bytecode, instruction_pcs) = assembly.into_parts();
    if instruction_pcs.len() != origins.len() {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "optimized assembler returns one PC per emitted instruction",
            span: sources.last().map(|source| source.span()),
        });
    }
    Ok(RebuiltInstructions {
        bytecode,
        instruction_pcs,
        origins,
        prefix_counts,
    })
}

fn emit_disconnected_terminal(
    plan: &mut BytecodeAssembler,
    origins: &mut Vec<usize>,
    origin: usize,
    span: quickjs_frontend::Span,
    function_kind: FunctionKind,
) -> Result<(), LeafCompilationError> {
    if function_kind == FunctionKind::Normal {
        emit_instruction(
            plan,
            origins,
            origin,
            span,
            FinalOpcode::ReturnUndef,
            Operands::None,
        )
    } else {
        emit_instruction(
            plan,
            origins,
            origin,
            span,
            FinalOpcode::Undefined,
            Operands::None,
        )?;
        emit_instruction(
            plan,
            origins,
            origin,
            span,
            FinalOpcode::ReturnAsync,
            Operands::None,
        )
    }
}

fn allocate_labels(
    plan: &mut BytecodeAssembler,
    sources: &[SourceInstruction],
    facts: &ControlFlowFacts,
) -> Result<(Vec<Option<AssemblerLabel>>, Vec<quickjs_frontend::Span>), LeafCompilationError> {
    let mut labels = reserved_vec(sources.len(), "optimized CFG labels")?;
    let mut spans = reserved_vec(sources.len(), "optimized CFG label spans")?;
    for (position, source) in sources.iter().enumerate() {
        if !facts.is_retained(position) {
            labels.push(None);
            continue;
        }
        let label =
            plan.new_label()
                .map_err(|assembler_error| LeafCompilationError::BytecodeAssembly {
                    span: Some(source.span()),
                    source: assembler_error,
                })?;
        labels.push(Some(label));
        spans.push(source.span());
    }
    Ok((labels, spans))
}

#[allow(clippy::too_many_arguments)]
fn emit_rewritten_instruction(
    plan: &mut BytecodeAssembler,
    origins: &mut Vec<usize>,
    labels: &[Option<AssemblerLabel>],
    position: usize,
    verified: VerifiedInstruction,
    outcome: Option<bool>,
    span: quickjs_frontend::Span,
) -> Result<(), LeafCompilationError> {
    let instruction = verified.decoded().instruction();
    if let Some(taken) = outcome {
        emit_instruction(
            plan,
            origins,
            position,
            span,
            FinalOpcode::Drop,
            Operands::None,
        )?;
        if taken {
            let target = required_successor(verified.successors().branch_target())?;
            let fallthrough = required_successor(verified.successors().fallthrough())?;
            if target != fallthrough {
                emit_branch(
                    plan,
                    origins,
                    position,
                    span,
                    BranchKind::Goto,
                    label_for(labels, target)?,
                )?;
            }
        }
        return Ok(());
    }
    if let Some(kind) = symbolic_branch_kind(instruction.opcode()) {
        let target = match verified.successors().kind() {
            VerifiedSuccessorKind::Branch => verified.successors().branch_target(),
            VerifiedSuccessorKind::Jump => verified.successors().jump_target(),
            VerifiedSuccessorKind::Fallthrough | VerifiedSuccessorKind::Terminate => None,
        };
        return emit_branch(
            plan,
            origins,
            position,
            span,
            kind,
            label_for(labels, required_successor(target)?)?,
        );
    }
    if is_with_branch(instruction.opcode()) {
        let Operands::AtomLabelU8 { atom, value, .. } = instruction.operands() else {
            return invalid_verified_operand();
        };
        let target = required_successor(verified.successors().branch_target())?;
        plan.with_branch(
            instruction.opcode(),
            atom,
            value,
            label_for(labels, target)?,
        )
        .map_err(|source| LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source,
        })?;
        return push_origin(origins, position);
    }
    emit_instruction(
        plan,
        origins,
        position,
        span,
        instruction.opcode(),
        instruction.operands(),
    )
}

fn finish_assembler(
    plan: BytecodeAssembler,
    origins: &[usize],
    sources: &[SourceInstruction],
    label_spans: &[quickjs_frontend::Span],
) -> Result<quickjs_bytecode::AssembledBytecode, LeafCompilationError> {
    match plan.finish() {
        Ok(assembly) => Ok(assembly),
        Err(AssemblerError::Encoding {
            instruction_index,
            source,
        }) => Err(LeafCompilationError::BytecodeEncoding {
            span: emitted_span(instruction_index, origins, sources)?,
            source,
        }),
        Err(source) => {
            let span = source
                .instruction_index()
                .and_then(|index| emitted_span(index, origins, sources).ok())
                .or_else(|| {
                    source
                        .label_index()
                        .and_then(|index| label_spans.get(index as usize).copied())
                });
            Err(LeafCompilationError::BytecodeAssembly { span, source })
        }
    }
}

fn emit_instruction(
    assembler: &mut BytecodeAssembler,
    origins: &mut Vec<usize>,
    origin: usize,
    span: quickjs_frontend::Span,
    opcode: FinalOpcode,
    operands: Operands,
) -> Result<(), LeafCompilationError> {
    assembler
        .push(opcode, operands)
        .map_err(|source| LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source,
        })?;
    push_origin(origins, origin)
}

fn emit_branch(
    assembler: &mut BytecodeAssembler,
    origins: &mut Vec<usize>,
    origin: usize,
    span: quickjs_frontend::Span,
    kind: BranchKind,
    target: &AssemblerLabel,
) -> Result<(), LeafCompilationError> {
    assembler
        .branch(kind, target)
        .map_err(|source| LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source,
        })?;
    push_origin(origins, origin)
}

fn push_origin(origins: &mut Vec<usize>, origin: usize) -> Result<(), LeafCompilationError> {
    origins
        .try_reserve(1)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "optimized instruction origins",
        })?;
    origins.push(origin);
    Ok(())
}

fn symbolic_branch_kind(opcode: FinalOpcode) -> Option<BranchKind> {
    match opcode {
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 => Some(BranchKind::IfFalse),
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8 => Some(BranchKind::IfTrue),
        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16 => Some(BranchKind::Goto),
        FinalOpcode::Catch => Some(BranchKind::Catch),
        FinalOpcode::Gosub => Some(BranchKind::Gosub),
        _ => None,
    }
}

fn is_with_branch(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::WithGetVar
            | FinalOpcode::WithPutVar
            | FinalOpcode::WithDeleteVar
            | FinalOpcode::WithMakeRef
            | FinalOpcode::WithGetRef
    )
}

fn label_for(
    labels: &[Option<AssemblerLabel>],
    target: InstructionIndex,
) -> Result<&AssemblerLabel, LeafCompilationError> {
    labels
        .get(usize_from_u32(target.get(), "optimized branch target")?)
        .and_then(Option::as_ref)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "retained optimized branch target has a symbolic label",
            span: None,
        })
}

fn required_successor(
    successor: Option<InstructionIndex>,
) -> Result<InstructionIndex, LeafCompilationError> {
    successor.ok_or(LeafCompilationError::SemanticInvariant {
        invariant: "verified successor shape retains its target",
        span: None,
    })
}

fn emitted_span(
    instruction_index: u32,
    origins: &[usize],
    sources: &[SourceInstruction],
) -> Result<quickjs_frontend::Span, LeafCompilationError> {
    let index = usize_from_u32(instruction_index, "optimized assembler instruction index")?;
    let origin = *origins
        .get(index)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "optimized assembler failure resolves to an instruction origin",
            span: None,
        })?;
    sources
        .get(origin)
        .copied()
        .map(SourceInstruction::span)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "optimized assembler failure origin resolves to a source span",
            span: None,
        })
}

fn invalid_verified_operand<T>() -> Result<T, LeafCompilationError> {
    Err(LeafCompilationError::SemanticInvariant {
        invariant: "verified optimizer opcode retains its operand format",
        span: None,
    })
}

fn reserved_vec<T>(capacity: usize, domain: &'static str) -> Result<Vec<T>, LeafCompilationError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| LeafCompilationError::CapacityExceeded { domain })?;
    Ok(values)
}

fn usize_from_u32(value: u32, domain: &'static str) -> Result<usize, LeafCompilationError> {
    usize::try_from(value).map_err(|_| LeafCompilationError::CapacityExceeded { domain })
}

fn u32_from_usize(value: usize, domain: &'static str) -> Result<u32, LeafCompilationError> {
    u32::try_from(value).map_err(|_| LeafCompilationError::CapacityExceeded { domain })
}
