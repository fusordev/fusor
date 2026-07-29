use std::error::Error as _;

use quickjs_bytecode::{
    BytecodePc, EncodeError, FinalOpcode, FunctionIndexDomains, MAX_OPERAND_STACK_DEPTH, Operands,
    VerificationErrorKind, VerificationLimits,
};
use quickjs_compiler::{
    CompilationContext, CompiledLeafFunction, LeafCompilationError, UnsupportedLeafFeature,
};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("straight-line compilation must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn compile_error(source: &str, name: &str) -> LeafCompilationError {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect_err("unsupported expression must fail closed")
        },
    )
    .expect("front-end acceptance")
}

fn decoded(compiled: &CompiledLeafFunction) -> Vec<(BytecodePc, FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let decoded = instruction.decoded();
            (
                decoded.pc(),
                decoded.instruction().opcode(),
                decoded.instruction().operands(),
            )
        })
        .collect()
}

fn opcodes(compiled: &CompiledLeafFunction) -> Vec<FinalOpcode> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect()
}

#[test]
fn multiple_lexicals_and_arithmetic_match_the_quickjs_final_opcode_oracle() {
    let source = "function f(a,b){ let x = a, y = b; const z = x * y + 1; return -z; }";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (
                BytecodePc::new(0),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(2),
            ),
            (
                BytecodePc::new(3),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(1),
            ),
            (
                BytecodePc::new(6),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(9), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(10), FinalOpcode::PutLoc0, Operands::NoneLoc,),
            (BytecodePc::new(11), FinalOpcode::GetArg1, Operands::NoneArg,),
            (BytecodePc::new(12), FinalOpcode::PutLoc1, Operands::NoneLoc,),
            (
                BytecodePc::new(13),
                FinalOpcode::GetLocCheck,
                Operands::Loc(0),
            ),
            (
                BytecodePc::new(16),
                FinalOpcode::GetLocCheck,
                Operands::Loc(1),
            ),
            (BytecodePc::new(19), FinalOpcode::Mul, Operands::None),
            (BytecodePc::new(20), FinalOpcode::Push1, Operands::NoneInt,),
            (BytecodePc::new(21), FinalOpcode::Add, Operands::None),
            (BytecodePc::new(22), FinalOpcode::PutLoc2, Operands::NoneLoc,),
            (
                BytecodePc::new(23),
                FinalOpcode::GetLocCheck,
                Operands::Loc(2),
            ),
            (BytecodePc::new(26), FinalOpcode::Neg, Operands::None),
            (BytecodePc::new(27), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        compiled.control_flow().domains(),
        FunctionIndexDomains::new(0, 0, 2, 3, 0)
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
    assert_eq!(
        compiled
            .locals()
            .iter()
            .map(|local| local.slot().index())
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(
        compiled
            .source_instructions()
            .iter()
            .take(3)
            .map(|entry| {
                let span = entry.span();
                &source[span.start as usize..span.end as usize]
            })
            .collect::<Vec<_>>(),
        ["z", "y", "x"],
        "QuickJS initializes lexical locals in reverse slot order"
    );
}

#[test]
fn cloned_compiled_artifacts_share_their_immutable_arc_backing() {
    let compiled = compile("function f(a){ let value = a + 1; return value; }", "f");
    let cloned = compiled.clone();

    assert!(std::ptr::eq(compiled.storage_plan(), cloned.storage_plan()));
    assert!(std::ptr::eq(compiled.source_text(), cloned.source_text()));
    assert!(std::ptr::eq(compiled.locals(), cloned.locals()));
    assert!(std::ptr::eq(
        compiled.source_instructions(),
        cloned.source_instructions()
    ));
    assert!(std::ptr::eq(compiled.control_flow(), cloned.control_flow()));
}

#[test]
fn uninitialized_let_var_and_integer_return_match_the_quickjs_oracle() {
    let compiled = compile("function g(){ let x; var y; return 42; }", "g");

    assert_eq!(
        decoded(&compiled),
        [
            (
                BytecodePc::new(0),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (BytecodePc::new(3), FinalOpcode::Undefined, Operands::None),
            (BytecodePc::new(4), FinalOpcode::PutLoc0, Operands::NoneLoc,),
            (BytecodePc::new(5), FinalOpcode::PushI8, Operands::I8(42)),
            (BytecodePc::new(7), FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        compiled.control_flow().domains(),
        FunctionIndexDomains::new(0, 0, 0, 2, 0)
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
    assert_eq!(compiled.locals().len(), 2);
}

#[test]
fn var_locals_and_parameter_redeclarations_use_their_actual_frame_slots() {
    let local = compile("function f(a){ var x = a; return x; }", "f");
    assert_eq!(
        opcodes(&local),
        [
            FinalOpcode::GetArg0,
            FinalOpcode::PutLoc0,
            FinalOpcode::GetLoc0,
            FinalOpcode::Return,
        ]
    );

    let parameter = compile("function f(a){ var a = 42; return a; }", "f");
    assert_eq!(
        opcodes(&parameter),
        [
            FinalOpcode::PushI8,
            FinalOpcode::PutArg0,
            FinalOpcode::GetArg0,
            FinalOpcode::Return,
        ]
    );
    assert!(parameter.locals().is_empty());
}

#[test]
fn lexical_reads_before_initialization_keep_runtime_tdz_checks() {
    let compiled = compile("function f(a){ let x = y; let y = a; return x; }", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (
                BytecodePc::new(0),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(1),
            ),
            (
                BytecodePc::new(3),
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(0),
            ),
            (
                BytecodePc::new(6),
                FinalOpcode::GetLocCheck,
                Operands::Loc(1),
            ),
            (BytecodePc::new(9), FinalOpcode::PutLoc0, Operands::NoneLoc,),
            (BytecodePc::new(10), FinalOpcode::GetArg0, Operands::NoneArg,),
            (BytecodePc::new(11), FinalOpcode::PutLoc1, Operands::NoneLoc,),
            (
                BytecodePc::new(12),
                FinalOpcode::GetLocCheck,
                Operands::Loc(0),
            ),
            (BytecodePc::new(15), FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn empty_and_bare_return_functions_use_return_undefined() {
    for source in ["function f(){}", "function f(){ return; }"] {
        let compiled = compile(source, "f");
        assert_eq!(opcodes(&compiled), [FinalOpcode::ReturnUndef], "{source}");
        assert_eq!(compiled.control_flow().computed_stack_size(), 0, "{source}");
    }
}

#[test]
fn binary_operator_family_lowers_left_to_right() {
    let cases = [
        ("*", FinalOpcode::Mul),
        ("/", FinalOpcode::Div),
        ("%", FinalOpcode::Mod),
        ("+", FinalOpcode::Add),
        ("-", FinalOpcode::Sub),
        ("**", FinalOpcode::Pow),
        ("<<", FinalOpcode::Shl),
        (">>", FinalOpcode::Sar),
        (">>>", FinalOpcode::Shr),
        ("<", FinalOpcode::Lt),
        ("<=", FinalOpcode::Lte),
        (">", FinalOpcode::Gt),
        (">=", FinalOpcode::Gte),
        ("instanceof", FinalOpcode::InstanceOf),
        ("in", FinalOpcode::In),
        ("==", FinalOpcode::Eq),
        ("!=", FinalOpcode::Neq),
        ("===", FinalOpcode::StrictEq),
        ("!==", FinalOpcode::StrictNeq),
        ("&", FinalOpcode::And),
        ("^", FinalOpcode::Xor),
        ("|", FinalOpcode::Or),
    ];

    for (operator, expected) in cases {
        let source = format!("function f(a,b){{ return a {operator} b; }}");
        let compiled = compile(&source, "f");
        assert_eq!(
            opcodes(&compiled),
            [
                FinalOpcode::GetArg0,
                FinalOpcode::GetArg1,
                expected,
                FinalOpcode::Return,
            ],
            "{operator}"
        );
    }
}

#[test]
fn unary_operator_family_lowers_after_its_operand() {
    let cases = [
        ("+", FinalOpcode::Plus),
        ("-", FinalOpcode::Neg),
        ("~", FinalOpcode::Not),
        ("!", FinalOpcode::Lnot),
        ("typeof ", FinalOpcode::Typeof),
    ];

    for (operator, expected) in cases {
        let source = format!("function f(a){{ return {operator}a; }}");
        let compiled = compile(&source, "f");
        assert_eq!(
            opcodes(&compiled),
            [FinalOpcode::GetArg0, expected, FinalOpcode::Return],
            "{operator:?}"
        );
    }
}

#[test]
fn void_negative_zero_and_int32_min_keep_their_distinct_value_semantics() {
    let void = compile("function f(a){ return void a; }", "f");
    assert_eq!(
        opcodes(&void),
        [
            FinalOpcode::GetArg0,
            FinalOpcode::Drop,
            FinalOpcode::Undefined,
            FinalOpcode::Return,
        ]
    );
    assert_eq!(void.control_flow().computed_stack_size(), 1);

    let negative_zero = compile("function f(){ return -0; }", "f");
    assert_eq!(
        opcodes(&negative_zero),
        [FinalOpcode::Push0, FinalOpcode::Neg, FinalOpcode::Return]
    );

    let minimum = compile("function f(){ return -2147483648; }", "f");
    assert_eq!(
        decoded(&minimum),
        [
            (
                BytecodePc::new(0),
                FinalOpcode::PushI32,
                Operands::I32(i32::MIN),
            ),
            (BytecodePc::new(5), FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn expression_statements_and_sequence_expressions_keep_evaluation_order() {
    let compiled = compile("function f(a,b){ a; return (a,b); }", "f");

    assert_eq!(
        opcodes(&compiled),
        [
            FinalOpcode::GetArg0,
            FinalOpcode::Drop,
            FinalOpcode::GetArg0,
            FinalOpcode::Drop,
            FinalOpcode::GetArg1,
            FinalOpcode::Return,
        ]
    );
}

#[test]
fn primitive_literal_forms_do_not_require_a_constant_pool() {
    let cases = [
        ("function f(){ return true; }", FinalOpcode::PushTrue),
        ("function f(){ return false; }", FinalOpcode::PushFalse),
        ("function f(){ return null; }", FinalOpcode::Null),
        ("function f(){ return \"\"; }", FinalOpcode::PushEmptyString),
        ("function f(){ return 42n; }", FinalOpcode::PushBigIntI32),
        ("function f(){ return -1; }", FinalOpcode::PushMinus1),
        ("function f(){ return 7; }", FinalOpcode::Push7),
        ("function f(){ return 127; }", FinalOpcode::PushI8),
        ("function f(){ return 32767; }", FinalOpcode::PushI16),
        ("function f(){ return 32768; }", FinalOpcode::PushI32),
    ];

    for (source, expected) in cases {
        let compiled = compile(source, "f");
        assert_eq!(
            opcodes(&compiled),
            [expected, FinalOpcode::Return],
            "{source}"
        );
    }
    assert_eq!(
        decoded(&compile("function f(){ return 42n; }", "f"))[0].2,
        Operands::I32(42)
    );
}

#[test]
fn unsupported_expression_families_fail_closed_at_the_exact_span() {
    let cases = [
        (
            "function f(a){ return a.value; }",
            UnsupportedLeafFeature::UnsupportedExpression,
            "a.value",
        ),
        (
            "function f(a){ return a(); }",
            UnsupportedLeafFeature::UnsupportedExpression,
            "a()",
        ),
        (
            "function f(){ return \"constant pool\"; }",
            UnsupportedLeafFeature::UnsupportedLiteral,
            "\"constant pool\"",
        ),
        (
            "function f(){ return 2147483648n; }",
            UnsupportedLeafFeature::UnsupportedLiteral,
            "2147483648n",
        ),
    ];

    for (source, expected_feature, expected_source) in cases {
        let error = compile_error(source, "f");
        let LeafCompilationError::Unsupported { feature, span } = error else {
            panic!("expected unsupported expression for {source}");
        };
        assert_eq!(feature, expected_feature, "{source}");
        assert_eq!(
            &source[span.start as usize..span.end as usize],
            expected_source,
            "{source}"
        );
    }
}

#[test]
fn deeply_left_nested_binary_lowering_is_iterative() {
    const OPERATOR_COUNT: usize = 20_000;
    let mut source =
        String::with_capacity("function f(operand){return operand;}".len() + 10 * OPERATOR_COUNT);
    source.push_str("function f(operand){return operand");
    for _ in 0..OPERATOR_COUNT {
        source.push_str("+operand");
    }
    source.push_str(";}");

    let compiled = compile(&source, "f");
    assert_eq!(
        compiled.control_flow().instructions().len(),
        2 * OPERATOR_COUNT + 2
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
    assert_eq!(
        compiled
            .control_flow()
            .instructions()
            .last()
            .expect("return instruction")
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Return
    );
}

#[test]
fn byte_limit_failures_keep_the_source_span_and_error_chain() {
    let source = "function f(a){ return a; }";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("f"))
                .expect("function executable");
            context
                .compile_leaf(&executable, VerificationLimits::new(1, 10, 0, 0, 10, 10))
                .expect_err("the return exceeds the one-byte body limit")
        },
    )
    .expect("front-end acceptance");

    let LeafCompilationError::BytecodeEncoding {
        span,
        source:
            EncodeError::ByteLimitExceeded {
                pc,
                instruction_size,
                encoded_bytes,
                byte_limit,
            },
    } = &error
    else {
        panic!("expected a byte-limit encoding error, got {error:?}");
    };
    assert_eq!(&source[span.start as usize..span.end as usize], "return a;");
    assert_eq!(*pc, BytecodePc::new(1));
    assert_eq!(*instruction_size, 1);
    assert_eq!(*encoded_bytes, 1);
    assert_eq!(*byte_limit, 1);
    assert!(error.source().is_some());
}

#[test]
fn verifier_failures_keep_primary_and_related_source_spans() {
    let source = "function f(a,b){ return a+b; }";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("f"))
                .expect("function executable");
            context
                .compile_leaf(&executable, VerificationLimits::new(100, 10, 0, 0, 100, 1))
                .expect_err("the second argument exceeds stack depth one")
        },
    )
    .expect("front-end acceptance");

    let LeafCompilationError::BytecodeVerification {
        span: Some(span),
        related_span,
        source: verification,
    } = &error
    else {
        panic!("expected a source-mapped verifier failure, got {error:?}");
    };
    assert_eq!(&source[span.start as usize..span.end as usize], "b");
    assert_eq!(*related_span, None);
    assert_eq!(
        verification.kind(),
        &VerificationErrorKind::StackLimitExceeded { depth: 2, limit: 1 }
    );
    assert!(error.source().is_some());
}

#[test]
fn verifier_source_mapping_uses_relocated_bytecode_pcs() {
    let source = "function f(a,b,c){return a ? b+c : b;}";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("f"))
                .expect("function executable");
            context
                .compile_leaf(&executable, VerificationLimits::new(100, 20, 0, 0, 100, 1))
                .expect_err("the true branch exceeds stack depth one")
        },
    )
    .expect("front-end acceptance");

    let LeafCompilationError::BytecodeVerification {
        span: Some(span),
        related_span: None,
        source: verification,
    } = &error
    else {
        panic!("expected a source-mapped verifier failure, got {error:?}");
    };
    assert_eq!(&source[span.start as usize..span.end as usize], "c");
    assert_eq!(verification.pc(), Some(BytecodePc::new(4)));
    assert_eq!(verification.opcode(), Some(FinalOpcode::GetArg2));
    assert_eq!(
        verification.kind(),
        &VerificationErrorKind::StackLimitExceeded { depth: 2, limit: 1 }
    );
}

#[test]
fn root_verifier_failures_do_not_fabricate_an_instruction_span() {
    let source = "function f(a){return a;}";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some("f"))
                .expect("function executable");
            context
                .compile_leaf(
                    &executable,
                    VerificationLimits::new(100, 10, 0, 0, 100, MAX_OPERAND_STACK_DEPTH + 1),
                )
                .expect_err("the stack limit exceeds the structural maximum")
        },
    )
    .expect("front-end acceptance");

    let LeafCompilationError::BytecodeVerification {
        span: None,
        related_span: None,
        source: verification,
    } = &error
    else {
        panic!("expected a root verifier failure, got {error:?}");
    };
    assert_eq!(verification.pc(), None);
    assert_eq!(
        verification.kind(),
        &VerificationErrorKind::InvalidStackLimit {
            value: MAX_OPERAND_STACK_DEPTH + 1,
            maximum: MAX_OPERAND_STACK_DEPTH,
        }
    );
}
