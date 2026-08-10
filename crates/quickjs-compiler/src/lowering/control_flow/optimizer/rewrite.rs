use quickjs_bytecode::{
    AssemblerError, AssemblerLabel, AssemblerLimits, BranchKind, BytecodeAssembler, FinalOpcode,
    InstructionIndex, Operands, VerificationLimits, VerifiedControlFlow, VerifiedInstruction,
    VerifiedSuccessorKind,
};

use super::{
    super::{LeafCompilationError, ResolvedStackAnchor, SourceInstruction},
    ConstantPropagationOutput,
};

struct RebuiltInstructions {
    bytecode: Vec<u8>,
    instruction_pcs: Vec<quickjs_bytecode::BytecodePc>,
    origins: Vec<usize>,
    prefix_counts: Vec<u32>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rebuild_with_constant_branches(
    control_flow: &VerifiedControlFlow,
    source_instructions: &[SourceInstruction],
    eval_reference_call_instructions: &[u32],
    parameter_initialization_end: Option<u32>,
    function_initializer_prefix_start: u32,
    stack_anchors: &[ResolvedStackAnchor],
    branch_outcomes: &[Option<bool>],
    limits: VerificationLimits,
) -> Result<ConstantPropagationOutput, LeafCompilationError> {
    let instruction_count = control_flow.instructions().len();
    if branch_outcomes.len() != instruction_count {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "CFG constant outcomes cover every verified instruction",
            span: None,
        });
    }

    let rebuilt =
        assemble_constant_branches(control_flow, source_instructions, branch_outcomes, limits)?;
    let RebuiltInstructions {
        bytecode,
        instruction_pcs,
        origins,
        prefix_counts,
    } = rebuilt;

    let source_instructions = remap_sources(&instruction_pcs, &origins, source_instructions)?;
    let eval_reference_call_instructions =
        remap_instruction_metadata(eval_reference_call_instructions, &prefix_counts)?;
    let parameter_initialization_end = parameter_initialization_end
        .map(|boundary| remap_boundary(boundary, &prefix_counts))
        .transpose()?;
    let function_initializer_prefix_start =
        remap_boundary(function_initializer_prefix_start, &prefix_counts)?;
    let stack_anchors = remap_stack_anchors(
        control_flow,
        stack_anchors,
        &prefix_counts,
        &instruction_pcs,
    )?;

    Ok(ConstantPropagationOutput {
        bytecode,
        source_instructions,
        eval_reference_call_instructions,
        parameter_initialization_end,
        function_initializer_prefix_start,
        stack_anchors,
    })
}

fn assemble_constant_branches(
    control_flow: &VerifiedControlFlow,
    sources: &[SourceInstruction],
    outcomes: &[Option<bool>],
    limits: VerificationLimits,
) -> Result<RebuiltInstructions, LeafCompilationError> {
    let instruction_count = control_flow.instructions().len();
    let assembler_limits = AssemblerLimits::new(
        limits.max_bytecode_bytes_per_function(),
        limits.max_instructions_per_function(),
        limits.max_transfer_evaluations(),
    );
    let mut plan = BytecodeAssembler::with_limits(assembler_limits);
    let (labels, label_spans) = allocate_labels(&mut plan, sources)?;
    let mut origins = reserved_vec(
        instruction_count.saturating_add(1),
        "optimized instruction origins",
    )?;
    let mut prefix_counts = reserved_vec(
        instruction_count.saturating_add(1),
        "optimized instruction boundaries",
    )?;
    prefix_counts.push(0_u32);

    for (position, verified) in control_flow.instructions().iter().copied().enumerate() {
        let span = sources[position].span();
        plan.bind(&labels[position])
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
            outcomes[position],
            span,
        )?;
        prefix_counts.push(u32_from_usize(
            origins.len(),
            "optimized instruction boundary",
        )?);
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

fn allocate_labels(
    plan: &mut BytecodeAssembler,
    sources: &[SourceInstruction],
) -> Result<(Vec<AssemblerLabel>, Vec<quickjs_frontend::Span>), LeafCompilationError> {
    let mut labels = reserved_vec(sources.len(), "optimized CFG labels")?;
    let mut spans = reserved_vec(sources.len(), "optimized CFG label spans")?;
    for source in sources {
        let label =
            plan.new_label()
                .map_err(|assembler_error| LeafCompilationError::BytecodeAssembly {
                    span: Some(source.span()),
                    source: assembler_error,
                })?;
        labels.push(label);
        spans.push(source.span());
    }
    Ok((labels, spans))
}

#[allow(clippy::too_many_arguments)]
fn emit_rewritten_instruction(
    plan: &mut BytecodeAssembler,
    origins: &mut Vec<usize>,
    labels: &[AssemblerLabel],
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

fn remap_sources(
    instruction_pcs: &[quickjs_bytecode::BytecodePc],
    origins: &[usize],
    sources: &[SourceInstruction],
) -> Result<Vec<SourceInstruction>, LeafCompilationError> {
    let mut optimized = reserved_vec(instruction_pcs.len(), "optimized source instructions")?;
    for (pc, origin) in instruction_pcs.iter().copied().zip(origins.iter().copied()) {
        let span = sources
            .get(origin)
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "optimized instruction origin resolves to a source span",
                span: None,
            })?
            .span();
        optimized.push(SourceInstruction { pc, span });
    }
    Ok(optimized)
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
    labels: &[AssemblerLabel],
    target: InstructionIndex,
) -> Result<&AssemblerLabel, LeafCompilationError> {
    labels
        .get(usize_from_u32(target.get(), "optimized branch target")?)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "verified optimized branch target has a symbolic label",
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

fn remap_instruction_metadata(
    old_indices: &[u32],
    prefix_instruction_counts: &[u32],
) -> Result<Vec<u32>, LeafCompilationError> {
    let mut remapped = reserved_vec(old_indices.len(), "optimized instruction metadata")?;
    for &old_index in old_indices {
        let position = usize_from_u32(old_index, "compiler instruction metadata index")?;
        let new_index = *prefix_instruction_counts.get(position).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "compiler instruction metadata resolves before a verified instruction",
                span: None,
            },
        )?;
        remapped.push(new_index);
    }
    Ok(remapped)
}

fn remap_boundary(
    boundary: u32,
    prefix_instruction_counts: &[u32],
) -> Result<u32, LeafCompilationError> {
    prefix_instruction_counts
        .get(usize_from_u32(boundary, "compiler instruction boundary")?)
        .copied()
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "compiler instruction boundary resolves in optimized control flow",
            span: None,
        })
}

fn remap_stack_anchors(
    control_flow: &VerifiedControlFlow,
    anchors: &[ResolvedStackAnchor],
    prefix_instruction_counts: &[u32],
    instruction_pcs: &[quickjs_bytecode::BytecodePc],
) -> Result<Vec<ResolvedStackAnchor>, LeafCompilationError> {
    let mut remapped = reserved_vec(anchors.len(), "optimized statement stack anchors")?;
    for anchor in anchors {
        let old_index = control_flow.instruction_index_at(anchor.pc).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "pre-optimization statement anchor resolves to a verified instruction",
                span: Some(anchor.span),
            },
        )?;
        let old_index = usize_from_u32(old_index.get(), "statement anchor instruction index")?;
        let new_index = *prefix_instruction_counts.get(old_index).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "statement anchor resolves to an optimized instruction boundary",
                span: Some(anchor.span),
            },
        )?;
        let pc = instruction_pcs
            .get(usize_from_u32(
                new_index,
                "optimized statement anchor index",
            )?)
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "optimized statement anchor resolves to a final instruction",
                span: Some(anchor.span),
            })?;
        remapped.push(ResolvedStackAnchor { pc, ..*anchor });
    }
    Ok(remapped)
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

pub(super) fn validate_source_instructions(
    control_flow: &VerifiedControlFlow,
    source_instructions: &[SourceInstruction],
) -> Result<(), LeafCompilationError> {
    if source_instructions.len() != control_flow.instructions().len() {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "pre-optimization source table covers every verified instruction",
            span: source_instructions.last().map(|source| source.span()),
        });
    }
    for (source, verified) in source_instructions
        .iter()
        .copied()
        .zip(control_flow.instructions().iter().copied())
    {
        if source.pc() != verified.decoded().pc() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "pre-optimization source table follows verified instruction PCs",
                span: Some(source.span()),
            });
        }
    }
    Ok(())
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
