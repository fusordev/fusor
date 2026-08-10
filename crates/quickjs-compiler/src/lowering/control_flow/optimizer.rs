mod abstract_value;
mod analysis;
mod metadata;
mod retention;
mod rewrite;

use quickjs_bytecode::{CompilerAtom, VerificationLimits, VerifiedControlFlow};

use super::{LeafCompilationError, ResolvedStackAnchor, SourceInstruction};
use crate::lowering::CompiledConstant;

struct ConstantInputs<'a> {
    atoms: &'a [CompilerAtom],
    constants: &'a [CompiledConstant],
}

impl<'a> ConstantInputs<'a> {
    const fn new(atoms: &'a [CompilerAtom], constants: &'a [CompiledConstant]) -> Self {
        Self { atoms, constants }
    }

    fn atom(&self, index: u32) -> Option<&CompilerAtom> {
        self.atoms.get(usize::try_from(index).ok()?)
    }

    fn constant(&self, index: u32) -> Option<&CompiledConstant> {
        self.constants.get(usize::try_from(index).ok()?)
    }
}

pub(super) struct OptimizedControlFlow {
    pub(super) bytecode: Vec<u8>,
    pub(super) source_instructions: Vec<SourceInstruction>,
    pub(super) eval_reference_call_instructions: Vec<u32>,
    pub(super) parameter_initialization_end: Option<u32>,
    pub(super) function_initializer_prefix_start: u32,
    pub(super) stack_anchors: Vec<ResolvedStackAnchor>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn optimize_control_flow(
    control_flow: &VerifiedControlFlow,
    source_instructions: &[SourceInstruction],
    eval_reference_call_instructions: &[u32],
    parameter_initialization_end: Option<u32>,
    function_initializer_prefix_start: u32,
    stack_anchors: &[ResolvedStackAnchor],
    atoms: &[CompilerAtom],
    constants: &[CompiledConstant],
    limits: VerificationLimits,
) -> Result<Option<OptimizedControlFlow>, LeafCompilationError> {
    metadata::validate_source_instructions(control_flow, source_instructions)?;
    let inputs = ConstantInputs::new(atoms, constants);
    let facts = analysis::analyze_control_flow(control_flow, &inputs, limits)?;
    if !facts.changes_control_flow() {
        return Ok(None);
    }

    rewrite::rebuild_optimized_control_flow(
        control_flow,
        source_instructions,
        eval_reference_call_instructions,
        parameter_initialization_end,
        function_initializer_prefix_start,
        stack_anchors,
        &facts,
        limits,
    )
    .map(Some)
}
