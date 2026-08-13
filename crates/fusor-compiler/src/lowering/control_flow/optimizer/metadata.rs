use fusor_bytecode::{BytecodePc, VerifiedControlFlow};

use super::{
    super::{LeafCompilationError, ResolvedStackAnchor, SourceInstruction},
    analysis::ControlFlowFacts,
};

pub(super) fn remap_sources(
    instruction_pcs: &[BytecodePc],
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

pub(super) fn remap_instruction_metadata(
    old_indices: &[u32],
    prefix_instruction_counts: &[u32],
    facts: &ControlFlowFacts,
) -> Result<Vec<u32>, LeafCompilationError> {
    let mut remapped = reserved_vec(old_indices.len(), "optimized instruction metadata")?;
    for &old_index in old_indices {
        let position = usize_from_u32(old_index, "compiler instruction metadata index")?;
        if position >= facts.instruction_count() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiler instruction metadata resolves to a verified instruction",
                span: None,
            });
        }
        if !facts.is_retained(position) {
            continue;
        }
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

pub(super) fn remap_boundary(
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

pub(super) fn remap_stack_anchors(
    control_flow: &VerifiedControlFlow,
    anchors: &[ResolvedStackAnchor],
    prefix_instruction_counts: &[u32],
    instruction_pcs: &[BytecodePc],
    facts: &ControlFlowFacts,
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
        if !facts.is_retained(old_index) {
            continue;
        }
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
