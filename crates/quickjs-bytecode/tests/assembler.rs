use quickjs_bytecode::{
    AssemblerError, AssemblerLimits, AssemblerResource, BranchKind, BytecodeAssembler, BytecodePc,
    EncodeError, FinalOpcode, FunctionIndexDomains, InstructionDecoder, Operands,
    UnverifiedCompilerFunctionBody, UnverifiedFunctionBody, UnverifiedFunctionHeader,
    VerificationErrorKind, VerificationLimits, verify_compiler_control_flow, verify_control_flow,
};

fn decoded(bytecode: &[u8]) -> Vec<(BytecodePc, FinalOpcode, Operands)> {
    InstructionDecoder::new(bytecode)
        .map(|item| {
            let decoded = item.expect("assembled bytecode must decode");
            (
                decoded.pc(),
                decoded.instruction().opcode(),
                decoded.instruction().operands(),
            )
        })
        .collect()
}

fn forward_branch(kind: BranchKind, padding: usize) -> Vec<(BytecodePc, FinalOpcode, Operands)> {
    let mut builder = BytecodeAssembler::new();
    let target = builder.new_label().expect("target label");
    builder.branch(kind, &target).expect("forward branch");
    for _ in 0..padding {
        builder
            .push(FinalOpcode::Nop, Operands::None)
            .expect("padding");
    }
    builder.bind(&target).expect("target");
    builder
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("target instruction");
    decoded(builder.finish().expect("assembly").bytecode())
}

fn backward_branch(kind: BranchKind, padding: usize) -> Vec<(BytecodePc, FinalOpcode, Operands)> {
    let mut builder = BytecodeAssembler::new();
    let target = builder.new_label().expect("target label");
    builder.bind(&target).expect("target");
    for _ in 0..padding {
        builder
            .push(FinalOpcode::Nop, Operands::None)
            .expect("padding");
    }
    builder.branch(kind, &target).expect("backward branch");
    decoded(builder.finish().expect("assembly").bytecode())
}

fn append_widening_cascade(builder: &mut BytecodeAssembler) {
    let exit = builder.new_label().expect("exit label");
    let far = builder.new_label().expect("far label");
    builder
        .branch(BranchKind::IfFalse, &exit)
        .expect("outer branch");
    builder
        .branch(BranchKind::IfFalse, &far)
        .expect("inner branch");
    for _ in 0..124 {
        builder
            .push(FinalOpcode::Nop, Operands::None)
            .expect("inner padding");
    }
    builder.bind(&exit).expect("exit target");
    for _ in 0..3 {
        builder
            .push(FinalOpcode::Nop, Operands::None)
            .expect("far padding");
    }
    builder.bind(&far).expect("far target");
    builder
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("far instruction");
}

#[test]
fn symbolic_forward_branches_relax_to_exact_quickjs_short_forms() {
    let mut assembler = BytecodeAssembler::new();
    let alternate = assembler.new_label().expect("alternate label");
    let done = assembler.new_label().expect("done label");

    assembler
        .push(FinalOpcode::PushTrue, Operands::None)
        .expect("condition");
    assembler
        .branch(BranchKind::IfFalse, &alternate)
        .expect("conditional branch");
    assembler
        .push(FinalOpcode::Push1, Operands::NoneInt)
        .expect("consequent");
    assembler
        .branch(BranchKind::Goto, &done)
        .expect("join branch");
    assembler.bind(&alternate).expect("alternate target");
    assembler
        .push(FinalOpcode::Push2, Operands::NoneInt)
        .expect("alternate");
    assembler.bind(&done).expect("join target");
    assembler
        .push(FinalOpcode::Return, Operands::None)
        .expect("return");

    let output = assembler.finish().expect("assembly");
    assert_eq!(
        decoded(output.bytecode()),
        [
            (BytecodePc::new(0), FinalOpcode::PushTrue, Operands::None),
            (
                BytecodePc::new(1),
                FinalOpcode::IfFalse8,
                Operands::Label8(4),
            ),
            (BytecodePc::new(3), FinalOpcode::Push1, Operands::NoneInt),
            (BytecodePc::new(4), FinalOpcode::Goto8, Operands::Label8(2),),
            (BytecodePc::new(6), FinalOpcode::Push2, Operands::NoneInt),
            (BytecodePc::new(7), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        output.instruction_pcs(),
        [
            BytecodePc::new(0),
            BytecodePc::new(1),
            BytecodePc::new(3),
            BytecodePc::new(4),
            BytecodePc::new(6),
            BytecodePc::new(7),
        ]
    );

    let verified = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            output.into_bytes(),
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
        ),
        VerificationLimits::default(),
    )
    .expect("compiler output must be independently verified");
    assert_eq!(verified.computed_stack_size(), 1);
}

#[test]
fn compiler_verification_still_rejects_unequal_reachable_join_depths() {
    let mut builder = BytecodeAssembler::new();
    let join = builder.new_label().expect("join label");
    builder
        .push(FinalOpcode::PushTrue, Operands::None)
        .expect("condition");
    builder.branch(BranchKind::IfFalse, &join).expect("branch");
    builder
        .push(FinalOpcode::Push0, Operands::NoneInt)
        .expect("unbalanced value");
    builder.bind(&join).expect("join");
    builder
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("return");

    let error = verify_compiler_control_flow(
        UnverifiedCompilerFunctionBody::new(
            builder.finish().expect("assembly").into_bytes(),
            FunctionIndexDomains::default(),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
        ),
        VerificationLimits::default(),
    )
    .expect_err("unequal join depths must fail");
    assert!(matches!(
        error.kind(),
        VerificationErrorKind::InconsistentStackAtJoin {
            established_depth: 0,
            incoming_depth: 1,
            ..
        }
    ));
}

#[test]
fn compiler_verification_requires_reachable_terminals_to_empty_the_stack() {
    let cases: &[&[(FinalOpcode, Operands)]] = &[
        &[
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Push2, Operands::NoneInt),
            (FinalOpcode::Return, Operands::None),
        ],
        &[
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::ReturnUndef, Operands::None),
        ],
    ];

    for (instructions, serialized_stack_size) in cases.iter().zip([2, 1]) {
        let mut builder = BytecodeAssembler::new();
        for &(opcode, operands) in *instructions {
            builder.push(opcode, operands).expect("instruction");
        }
        let bytecode = builder.finish().expect("assembly").into_bytes();
        verify_control_flow(
            UnverifiedFunctionBody::new(
                bytecode.clone(),
                serialized_stack_size,
                FunctionIndexDomains::default(),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            ),
            VerificationLimits::default(),
        )
        .expect("serialized verification retains QuickJS stack semantics");
        let error = verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(
                bytecode,
                FunctionIndexDomains::default(),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            ),
            VerificationLimits::default(),
        )
        .expect_err("compiler exit must empty the ordinary stack");
        assert_eq!(
            error.kind(),
            &VerificationErrorKind::NonEmptyCompilerExitStack { remaining: 1 }
        );
        assert_eq!(
            error.pc(),
            Some(BytecodePc::new(
                u32::try_from(instructions.len() - 1).expect("small fixture")
            ))
        );
    }
}

#[test]
fn forward_relaxation_accounts_for_the_candidate_branch_width() {
    let mut assembler = BytecodeAssembler::new();
    let target = assembler.new_label().expect("target label");
    assembler
        .branch(BranchKind::Goto, &target)
        .expect("forward branch");
    for _ in 0..126 {
        assembler
            .push(FinalOpcode::Nop, Operands::None)
            .expect("padding");
    }
    assembler.bind(&target).expect("target");
    assembler
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("target instruction");

    let output = assembler.finish().expect("assembly");
    assert_eq!(
        decoded(output.bytecode())[0],
        (
            BytecodePc::new(0),
            FinalOpcode::Goto8,
            Operands::Label8(127),
        )
    );
}

#[test]
fn relaxation_repeats_until_an_earlier_branch_widens() {
    let mut builder = BytecodeAssembler::new();
    append_widening_cascade(&mut builder);

    let output = decoded(builder.finish().expect("fixed-point assembly").bytecode());
    assert_eq!(
        output[..2],
        [
            (
                BytecodePc::new(0),
                FinalOpcode::IfFalse,
                Operands::Label(133),
            ),
            (
                BytecodePc::new(5),
                FinalOpcode::IfFalse,
                Operands::Label(131),
            ),
        ]
    );
}

#[test]
fn mutually_dependent_branches_select_the_joint_shortest_layout() {
    let mut assembler = BytecodeAssembler::new();
    let head = assembler.new_label().expect("head label");
    let exit = assembler.new_label().expect("exit label");
    assembler.bind(&head).expect("head target");
    assembler
        .branch(BranchKind::IfFalse, &exit)
        .expect("forward branch");
    for _ in 0..124 {
        assembler
            .push(FinalOpcode::Nop, Operands::None)
            .expect("padding");
    }
    assembler
        .branch(BranchKind::IfTrue, &head)
        .expect("backward branch");
    assembler.bind(&exit).expect("exit target");
    assembler
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("exit instruction");

    let output = decoded(
        assembler
            .finish()
            .expect("joint shortest assembly")
            .bytecode(),
    );
    assert_eq!(
        [output[0], output[125], output[126]],
        [
            (
                BytecodePc::new(0),
                FinalOpcode::IfFalse8,
                Operands::Label8(127),
            ),
            (
                BytecodePc::new(126),
                FinalOpcode::IfTrue8,
                Operands::Label8(-127),
            ),
            (
                BytecodePc::new(128),
                FinalOpcode::ReturnUndef,
                Operands::None,
            ),
        ]
    );
}

#[test]
fn instruction_and_relaxation_limits_bound_assembler_work() {
    let mut instruction_limited =
        BytecodeAssembler::with_limits(AssemblerLimits::new(u32::MAX, 1, u64::MAX));
    instruction_limited
        .push(FinalOpcode::Nop, Operands::None)
        .expect("first instruction");
    assert_eq!(
        instruction_limited.push(FinalOpcode::ReturnUndef, Operands::None),
        Err(AssemblerError::LimitExceeded {
            resource: AssemblerResource::Instructions,
            instruction_index: 1,
            limit: 1,
            observed: 2,
        })
    );

    let mut branchless = BytecodeAssembler::with_limits(AssemblerLimits::new(u32::MAX, 1, 0));
    branchless
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("branchless instruction");
    branchless
        .finish()
        .expect("branchless layout performs no relaxation evaluations");

    let mut relaxation_limited =
        BytecodeAssembler::with_limits(AssemblerLimits::new(u32::MAX, 200, 261));
    append_widening_cascade(&mut relaxation_limited);
    assert_eq!(
        relaxation_limited.finish(),
        Err(AssemblerError::LimitExceeded {
            resource: AssemblerResource::RelaxationEvaluations,
            instruction_index: 1,
            limit: 261,
            observed: 262,
        })
    );

    let mut exact = BytecodeAssembler::with_limits(AssemblerLimits::new(u32::MAX, 200, 390));
    append_widening_cascade(&mut exact);
    exact
        .finish()
        .expect("three complete 130-instruction relaxation passes");
}

#[test]
fn branch_relaxation_uses_goto16_and_long_conditionals_when_needed() {
    let mut goto_assembler = BytecodeAssembler::new();
    let goto_target = goto_assembler.new_label().expect("target label");
    goto_assembler
        .branch(BranchKind::Goto, &goto_target)
        .expect("forward goto");
    for _ in 0..130 {
        goto_assembler
            .push(FinalOpcode::Nop, Operands::None)
            .expect("padding");
    }
    goto_assembler.bind(&goto_target).expect("target");
    goto_assembler
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("target instruction");
    assert_eq!(
        decoded(goto_assembler.finish().expect("goto assembly").bytecode())[0],
        (
            BytecodePc::new(0),
            FinalOpcode::Goto16,
            Operands::Label16(132),
        )
    );

    let mut conditional_assembler = BytecodeAssembler::new();
    let conditional_target = conditional_assembler
        .new_label()
        .expect("conditional target");
    conditional_assembler
        .push(FinalOpcode::PushTrue, Operands::None)
        .expect("condition");
    conditional_assembler
        .branch(BranchKind::IfFalse, &conditional_target)
        .expect("forward conditional");
    for _ in 0..130 {
        conditional_assembler
            .push(FinalOpcode::Nop, Operands::None)
            .expect("padding");
    }
    conditional_assembler
        .bind(&conditional_target)
        .expect("conditional target");
    conditional_assembler
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("target instruction");
    assert_eq!(
        decoded(
            conditional_assembler
                .finish()
                .expect("conditional assembly")
                .bytecode()
        )[1],
        (
            BytecodePc::new(1),
            FinalOpcode::IfFalse,
            Operands::Label(134),
        )
    );
}

#[test]
fn backward_branches_use_the_same_pc_plus_one_base() {
    let mut assembler = BytecodeAssembler::new();
    let loop_head = assembler.new_label().expect("loop label");
    assembler.bind(&loop_head).expect("loop head");
    assembler
        .push(FinalOpcode::Nop, Operands::None)
        .expect("loop body");
    assembler
        .branch(BranchKind::Goto, &loop_head)
        .expect("back edge");

    let output = assembler.finish().expect("assembly");
    assert_eq!(
        decoded(output.bytecode()),
        [
            (BytecodePc::new(0), FinalOpcode::Nop, Operands::None),
            (BytecodePc::new(1), FinalOpcode::Goto8, Operands::Label8(-2),),
        ]
    );
}

#[test]
fn branch_width_boundaries_are_checked_in_both_directions() {
    assert_eq!(
        forward_branch(BranchKind::Goto, 126)[0].2,
        Operands::Label8(127)
    );
    assert_eq!(
        forward_branch(BranchKind::Goto, 127)[0].2,
        Operands::Label16(129)
    );
    assert_eq!(
        forward_branch(BranchKind::Goto, 32_765)[0].2,
        Operands::Label16(i16::MAX)
    );
    assert_eq!(
        forward_branch(BranchKind::Goto, 32_766)[0].2,
        Operands::Label(32_770)
    );

    let backward_short = backward_branch(BranchKind::Goto, 127);
    assert_eq!(
        backward_short.last().expect("back edge").2,
        Operands::Label8(i8::MIN)
    );
    let backward_medium = backward_branch(BranchKind::Goto, 128);
    assert_eq!(
        backward_medium.last().expect("back edge").2,
        Operands::Label16(-129)
    );
    let backward_i16 = backward_branch(BranchKind::Goto, 32_767);
    assert_eq!(
        backward_i16.last().expect("back edge").2,
        Operands::Label16(i16::MIN)
    );
    let backward_long = backward_branch(BranchKind::Goto, 32_768);
    assert_eq!(
        backward_long.last().expect("back edge").2,
        Operands::Label(-32_769)
    );

    assert_eq!(
        forward_branch(BranchKind::IfFalse, 126)[0].2,
        Operands::Label8(127)
    );
    assert_eq!(
        forward_branch(BranchKind::IfFalse, 127)[0].2,
        Operands::Label(131)
    );
    let backward_conditional = backward_branch(BranchKind::IfTrue, 128);
    assert_eq!(
        backward_conditional.last().expect("back edge").2,
        Operands::Label(-129)
    );
}

#[test]
fn labels_are_assembler_owned_bound_once_and_target_instructions() {
    let mut first = BytecodeAssembler::new();
    let first_label = first.new_label().expect("first label");
    let mut second = BytecodeAssembler::new();
    assert!(matches!(
        second.bind(&first_label),
        Err(AssemblerError::ForeignLabel)
    ));

    first.bind(&first_label).expect("first bind");
    assert!(matches!(
        first.bind(&first_label),
        Err(AssemblerError::DuplicateLabel { .. })
    ));

    let mut unbound = BytecodeAssembler::new();
    let _ = unbound.new_label().expect("unbound label");
    assert!(matches!(
        unbound.finish(),
        Err(AssemblerError::UnboundLabel { .. })
    ));

    let mut end_target = BytecodeAssembler::new();
    let end = end_target.new_label().expect("end label");
    end_target
        .branch(BranchKind::Goto, &end)
        .expect("end branch");
    end_target.bind(&end).expect("end target");
    assert!(matches!(
        end_target.finish(),
        Err(AssemblerError::TargetAtEnd { .. })
    ));
}

#[test]
fn raw_label_operands_cannot_bypass_symbolic_fixups() {
    let mut assembler = BytecodeAssembler::new();
    assert!(matches!(
        assembler.push(FinalOpcode::Goto8, Operands::Label8(0)),
        Err(AssemblerError::SymbolicBranchRequired {
            opcode: FinalOpcode::Goto8
        })
    ));
}

#[test]
fn byte_limit_is_applied_to_the_relaxed_layout() {
    let mut exact = BytecodeAssembler::with_byte_limit(3);
    let exact_target = exact.new_label().expect("target label");
    exact
        .branch(BranchKind::Goto, &exact_target)
        .expect("branch");
    exact.bind(&exact_target).expect("target");
    exact
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("return");
    assert_eq!(
        exact.finish().expect("three final bytes").bytecode().len(),
        3
    );

    let mut too_small = BytecodeAssembler::with_byte_limit(2);
    let limited_target = too_small.new_label().expect("target label");
    too_small
        .branch(BranchKind::Goto, &limited_target)
        .expect("branch");
    too_small.bind(&limited_target).expect("target");
    too_small
        .push(FinalOpcode::ReturnUndef, Operands::None)
        .expect("return");
    assert!(matches!(
        too_small.finish(),
        Err(AssemblerError::Encoding {
            instruction_index: 1,
            source: EncodeError::ByteLimitExceeded {
                pc,
                encoded_bytes: 2,
                byte_limit: 2,
                ..
            },
        }) if pc == BytecodePc::new(2)
    ));
}

#[test]
fn final_encoding_limits_retain_the_exact_instruction_index_and_source() {
    let mut assembler = BytecodeAssembler::with_byte_limit(1);
    assembler
        .push(FinalOpcode::PushTrue, Operands::None)
        .expect("first byte");
    assembler
        .push(FinalOpcode::Return, Operands::None)
        .expect("planned return");

    let error = assembler.finish().expect_err("second byte exceeds limit");
    assert_eq!(
        error,
        AssemblerError::Encoding {
            instruction_index: 1,
            source: EncodeError::ByteLimitExceeded {
                pc: BytecodePc::new(1),
                instruction_size: 1,
                encoded_bytes: 1,
                byte_limit: 1,
            },
        }
    );
}
