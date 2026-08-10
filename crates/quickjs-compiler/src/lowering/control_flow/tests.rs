use quickjs_bytecode::{
    AssemblerError, BranchKind, BytecodePc, CompilerCaptureLayout, CompilerConstantLayout,
    FinalOpcode, FunctionIndexDomains, Operands, UnverifiedFunctionHeader, VerificationErrorKind,
    VerificationLimits,
};
use quickjs_frontend::Span;

use super::{ControlFlowVerificationInputs, PlannedControlFlow, PlannedInstruction};
use crate::lowering::LeafCompilationError;

#[test]
fn compiler_labels_retain_owner_spans_for_bind_and_finish_failures() {
    let owner = Span::new(10, 20);

    let mut duplicate = PlannedControlFlow::new(VerificationLimits::default());
    let label = duplicate.new_label(owner).expect("label");
    duplicate.bind(&label).expect("first bind");
    let error = duplicate.bind(&label).expect_err("duplicate bind");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source: AssemblerError::DuplicateLabel { .. },
        } if span == owner
    ));

    let mut unbound = PlannedControlFlow::new(VerificationLimits::default());
    let _unbound_label = unbound.new_label(owner).expect("unbound label");
    let distractor_owner = Span::new(1, 2);
    let distractor = unbound
        .new_label(distractor_owner)
        .expect("distractor label");
    unbound.bind(&distractor).expect("distractor binding");
    unbound
        .emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(30, 31),
        ))
        .expect("terminal");
    let error = unbound.finish().expect_err("unbound label");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source: AssemblerError::UnboundLabel { .. },
        } if span == owner
    ));

    let mut end_target = PlannedControlFlow::new(VerificationLimits::default());
    let label = end_target.new_label(owner).expect("label");
    let distractor_owner = Span::new(2, 3);
    let distractor = end_target
        .new_label(distractor_owner)
        .expect("distractor label");
    end_target.bind(&distractor).expect("distractor binding");
    end_target
        .branch(BranchKind::Goto, &label, Span::new(40, 41))
        .expect("branch");
    end_target.bind(&label).expect("bind at end");
    let error = end_target.finish().expect_err("end target");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source: AssemblerError::TargetAtEnd { .. },
        } if span == owner
    ));
}

#[test]
fn reachable_statement_anchors_require_an_empty_stack() {
    let anchor_span = Span::new(20, 30);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(10, 11),
    ))
    .expect("push");
    let anchor = flow
        .new_statement_label(anchor_span)
        .expect("statement label");
    flow.branch(BranchKind::Goto, &anchor, anchor_span)
        .expect("widened branch");
    for _ in 0..130 {
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Nop,
            Operands::None,
            Span::new(12, 13),
        ))
        .expect("padding");
    }
    flow.bind(&anchor).expect("bind");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Return,
        Operands::None,
        Span::new(30, 31),
    ))
    .expect("return");

    let finished = flow.finish().expect("assembly");
    let error = finished
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("depth-one statement anchor");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeStackInvariant {
            span,
            pc,
            expected: 0,
            actual: 1,
        } if span == anchor_span && pc == BytecodePc::new(134)
    ));
}

#[test]
fn unreachable_statement_anchors_have_no_required_entry_depth() {
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let live_exit = flow.new_label(Span::new(40, 41)).expect("live exit");
    flow.branch(BranchKind::Goto, &live_exit, Span::new(0, 1))
        .expect("skip unreachable region");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(10, 11),
    ))
    .expect("unreachable push");
    let anchor = flow
        .new_statement_label(Span::new(20, 21))
        .expect("unreachable statement anchor");
    flow.bind(&anchor).expect("unreachable anchor binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(21, 22),
    ))
    .expect("unreachable terminal");
    flow.bind(&live_exit).expect("live exit binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(40, 41),
    ))
    .expect("live terminal");

    let (source_instructions, control_flow) = flow
        .finish()
        .expect("assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect("unreachable anchor is accepted");
    let anchor_source = source_instructions
        .iter()
        .find(|instruction| instruction.span() == Span::new(21, 22))
        .expect("unreachable target source");
    let anchor_index = control_flow
        .instruction_index_at(anchor_source.pc())
        .expect("verified unreachable target");
    assert_eq!(
        control_flow
            .instruction(anchor_index)
            .expect("unreachable target instruction")
            .entry_stack_depth(),
        None
    );
}

#[test]
fn inconsistent_join_maps_incoming_and_target_source_spans() {
    let incoming_span = Span::new(20, 21);
    let target_span = Span::new(30, 31);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let join = flow.new_label(Span::new(10, 11)).expect("join");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(0, 1),
    ))
    .expect("condition");
    flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
        .expect("conditional branch");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(2, 3),
    ))
    .expect("unbalanced value");
    flow.branch(BranchKind::Goto, &join, incoming_span)
        .expect("incoming edge");
    flow.bind(&join).expect("join binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        target_span,
    ))
    .expect("join target");

    let error = flow
        .finish()
        .expect("assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("inconsistent stack join");

    assert!(matches!(
        error,
        LeafCompilationError::BytecodeVerification {
            span: Some(span),
            related_span: Some(related_span),
            source,
        } if span == incoming_span
            && related_span == target_span
            && matches!(
                source.kind(),
                VerificationErrorKind::InconsistentStackAtJoin { .. }
            )
    ));
}

#[test]
fn missing_primary_verifier_source_mapping_fails_as_a_compiler_invariant() {
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(0, 1),
    ))
    .expect("first push");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(1, 2),
    ))
    .expect("second push");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Return,
        Operands::None,
        Span::new(2, 3),
    ))
    .expect("return");
    let mut finished = flow.finish().expect("assembly");
    finished
        .source_instructions
        .retain(|instruction| instruction.pc() != BytecodePc::new(1));

    let error = finished
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::new(100, 10, 0, 0, 100, 1),
        )
        .expect_err("missing exact primary provenance");
    assert!(matches!(
        error,
        LeafCompilationError::SemanticInvariant {
            invariant: "verifier instruction PC resolves to an exact source instruction",
            span: None,
        }
    ));
}

#[test]
fn missing_join_target_source_mapping_fails_as_a_compiler_invariant() {
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let join = flow.new_label(Span::new(10, 11)).expect("join");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(0, 1),
    ))
    .expect("condition");
    flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
        .expect("conditional branch");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(2, 3),
    ))
    .expect("unbalanced value");
    flow.branch(BranchKind::Goto, &join, Span::new(3, 4))
        .expect("incoming edge");
    flow.bind(&join).expect("join binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(4, 5),
    ))
    .expect("join target");
    let mut finished = flow.finish().expect("assembly");
    finished
        .source_instructions
        .retain(|instruction| instruction.span() != Span::new(4, 5));

    let error = finished
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("missing exact related provenance");
    assert!(matches!(
        error,
        LeafCompilationError::SemanticInvariant {
            invariant: "verifier join target resolves to an exact source instruction",
            span: None,
        }
    ));
}

#[test]
fn widened_branch_verifier_failures_use_the_relocated_target_span() {
    let target_span = Span::new(30, 31);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let target = flow.new_label(Span::new(20, 21)).expect("target");
    flow.branch(BranchKind::Goto, &target, Span::new(0, 1))
        .expect("widened branch");
    for _ in 0..130 {
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Nop,
            Operands::None,
            Span::new(10, 11),
        ))
        .expect("padding");
    }
    flow.bind(&target).expect("target binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Drop,
        Operands::None,
        target_span,
    ))
    .expect("underflowing target");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(31, 32),
    ))
    .expect("terminal");

    let error = flow
        .finish()
        .expect("assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("reachable drop underflows");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeVerification {
            span: Some(span),
            related_span: None,
            source,
        } if span == target_span
            && source.pc() == Some(BytecodePc::new(133))
            && matches!(
                source.kind(),
                VerificationErrorKind::StackUnderflow {
                    required: 1,
                    available: 0,
                }
            )
    ));
}

#[test]
fn known_branch_rewrite_preserves_sources_anchors_and_instruction_boundaries() {
    let condition_span = Span::new(0, 1);
    let branch_span = Span::new(1, 2);
    let dead_span = Span::new(2, 3);
    let target_span = Span::new(3, 4);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let target = flow.new_statement_label(target_span).expect("target label");

    flow.emit(PlannedInstruction::new(
        FinalOpcode::PushTrue,
        Operands::None,
        condition_span,
    ))
    .expect("known condition");
    flow.mark_function_initializer_prefix_start(condition_span)
        .expect("initializer boundary");
    flow.branch(BranchKind::IfTrue, &target, branch_span)
        .expect("known branch");
    flow.mark_parameter_initialization_end(branch_span)
        .expect("parameter boundary");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        dead_span,
    ))
    .expect("dead value");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Drop,
        Operands::None,
        dead_span,
    ))
    .expect("dead drop");
    flow.bind(&target).expect("target binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        target_span,
    ))
    .expect("target return");

    let verified = flow
        .finish()
        .expect("assembly")
        .verify_with_inputs(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            ControlFlowVerificationInputs::new(
                CompilerCaptureLayout::default(),
                CompilerConstantLayout::default(),
                &[],
                &[],
            ),
            VerificationLimits::default(),
        )
        .expect("optimized verification");
    assert_eq!(verified.function_initializer_prefix_start(), 1);
    assert_eq!(verified.parameter_initialization_end(), Some(3));

    let (sources, control_flow) = verified.into_control_flow();
    let opcodes = control_flow
        .instructions()
        .iter()
        .map(|verified| verified.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    assert_eq!(opcodes[0], FinalOpcode::PushTrue);
    assert_eq!(opcodes[1], FinalOpcode::Drop);
    assert!(matches!(
        opcodes[2],
        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
    ));
    assert!(!opcodes.iter().any(|opcode| matches!(
        opcode,
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 | FinalOpcode::IfTrue | FinalOpcode::IfTrue8
    )));
    assert_eq!(
        sources
            .iter()
            .filter(|source| source.span() == branch_span)
            .count(),
        2
    );
    assert_eq!(sources.len(), control_flow.instructions().len());

    let dead = sources
        .iter()
        .position(|source| source.span() == dead_span)
        .expect("retained dead block source");
    assert_eq!(control_flow.instructions()[dead].entry_stack_depth(), None);
    let target = sources
        .iter()
        .position(|source| source.span() == target_span)
        .expect("remapped target source");
    assert_eq!(
        control_flow.instructions()[target].entry_stack_depth(),
        Some(0)
    );
}

#[test]
fn constant_propagation_folds_es_truthiness_and_known_truthy_joins() {
    let folded_span = Span::new(20, 21);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let alternate = flow.new_label(Span::new(10, 11)).expect("alternate");
    let join = flow.new_label(Span::new(20, 21)).expect("join");
    let taken = flow.new_label(Span::new(30, 31)).expect("taken");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::GetArg0,
        Operands::NoneArg,
        Span::new(0, 1),
    ))
    .expect("unknown selector");
    flow.branch(BranchKind::IfFalse, &alternate, Span::new(1, 2))
        .expect("unknown branch");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(2, 3),
    ))
    .expect("first truthy value");
    flow.branch(BranchKind::Goto, &join, Span::new(3, 4))
        .expect("join branch");
    flow.bind(&alternate).expect("alternate binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push2,
        Operands::NoneInt,
        Span::new(10, 11),
    ))
    .expect("second truthy value");
    flow.bind(&join).expect("join binding");
    flow.branch(BranchKind::IfTrue, &taken, folded_span)
        .expect("folded branch");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(21, 22),
    ))
    .expect("dead fallthrough");
    flow.bind(&taken).expect("taken binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(30, 31),
    ))
    .expect("taken return");

    let (sources, control_flow) = flow
        .finish()
        .expect("assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 1, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 1),
            VerificationLimits::default(),
        )
        .expect("optimized verification");
    let conditional_spans = control_flow
        .instructions()
        .iter()
        .zip(&sources)
        .filter_map(|(verified, source)| {
            matches!(
                verified.decoded().instruction().opcode(),
                FinalOpcode::IfFalse
                    | FinalOpcode::IfFalse8
                    | FinalOpcode::IfTrue
                    | FinalOpcode::IfTrue8
            )
            .then_some(source.span())
        })
        .collect::<Vec<_>>();
    assert_eq!(conditional_spans, [Span::new(1, 2)]);
    assert_eq!(
        sources
            .iter()
            .filter(|source| source.span() == folded_span)
            .count(),
        2
    );
}

#[test]
fn empty_string_is_folded_as_falsy_but_unknown_values_keep_both_edges() {
    let mut falsy = PlannedControlFlow::new(VerificationLimits::default());
    let falsy_target = falsy.new_label(Span::new(10, 11)).expect("falsy target");
    falsy
        .emit(PlannedInstruction::new(
            FinalOpcode::PushEmptyString,
            Operands::None,
            Span::new(0, 1),
        ))
        .expect("empty string");
    falsy
        .branch(BranchKind::IfTrue, &falsy_target, Span::new(1, 2))
        .expect("falsy branch");
    falsy
        .emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(2, 3),
        ))
        .expect("fallthrough");
    falsy.bind(&falsy_target).expect("falsy target binding");
    falsy
        .emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(10, 11),
        ))
        .expect("target return");
    let (_, falsy) = falsy
        .finish()
        .expect("falsy assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect("falsy verification");
    assert!(!falsy.instructions().iter().any(|verified| matches!(
        verified.decoded().instruction().opcode(),
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8
    )));

    let mut unknown = PlannedControlFlow::new(VerificationLimits::default());
    let unknown_target = unknown
        .new_label(Span::new(20, 21))
        .expect("unknown target");
    unknown
        .emit(PlannedInstruction::new(
            FinalOpcode::GetArg0,
            Operands::NoneArg,
            Span::new(0, 1),
        ))
        .expect("unknown value");
    unknown
        .branch(BranchKind::IfTrue, &unknown_target, Span::new(1, 2))
        .expect("unknown branch");
    unknown
        .emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(2, 3),
        ))
        .expect("unknown fallthrough");
    unknown
        .bind(&unknown_target)
        .expect("unknown target binding");
    unknown
        .emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(20, 21),
        ))
        .expect("unknown target return");
    let (_, unknown) = unknown
        .finish()
        .expect("unknown assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 1, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 1),
            VerificationLimits::default(),
        )
        .expect("unknown verification");
    assert!(unknown.instructions().iter().any(|verified| matches!(
        verified.decoded().instruction().opcode(),
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8
    )));
}
