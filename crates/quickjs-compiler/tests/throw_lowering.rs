use quickjs_bytecode::{BytecodePc, FinalOpcode, Operands, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledLeafFunction};
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
                .expect("throw lowering must succeed")
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

fn source_slice_at<'source>(
    compiled: &CompiledLeafFunction,
    source: &'source str,
    pc: BytecodePc,
) -> &'source str {
    let span = compiled
        .source_instructions()
        .iter()
        .find(|entry| entry.pc() == pc)
        .expect("source entry at final instruction PC")
        .span();
    &source[span.start as usize..span.end as usize]
}

#[test]
fn throw_literal_is_a_real_terminal_without_a_synthetic_return() {
    let source = "function f(){throw 7;}";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::Push7, Operands::NoneInt,),
            (BytecodePc::new(1), FinalOpcode::Throw, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
    assert_eq!(source_slice_at(&compiled, source, BytecodePc::new(0)), "7");
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(1)),
        "throw 7;"
    );
    assert!(
        decoded(&compiled).iter().all(|(_, opcode, _)| !matches!(
            opcode,
            FinalOpcode::Return | FinalOpcode::ReturnUndef
        )),
        "throw is terminal and must not need a synthetic return"
    );
}

#[test]
fn throw_call_evaluates_the_expression_before_the_terminal() {
    let source = "function f(callee){throw callee();}";
    let compiled = compile(source, "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg,),
            (BytecodePc::new(1), FinalOpcode::Call0, Operands::NPopX),
            (BytecodePc::new(2), FinalOpcode::Throw, Operands::None),
        ]
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(0)),
        "callee"
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(1)),
        "callee()"
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(2)),
        "throw callee();"
    );
}

#[test]
fn unreachable_statements_after_throw_are_still_lowered_and_terminated() {
    let compiled = compile("function f(a){throw a;a;}", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg,),
            (BytecodePc::new(1), FinalOpcode::Throw, Operands::None),
            (BytecodePc::new(2), FinalOpcode::GetArg0, Operands::NoneArg,),
            (BytecodePc::new(3), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(4), FinalOpcode::ReturnUndef, Operands::None,),
        ]
    );
    assert_eq!(
        compiled.control_flow().instructions()[2].entry_stack_depth(),
        None,
        "the unreachable tail remains in the verified structure"
    );
}

#[test]
fn unreachable_object_spread_is_still_lowered_and_preserves_the_throw_prefix() {
    let compiled = compile("function f(a){throw a;({...a});}", "f");

    assert_eq!(
        decoded(&compiled),
        [
            (BytecodePc::new(0), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(1), FinalOpcode::Throw, Operands::None),
            (BytecodePc::new(2), FinalOpcode::Object, Operands::None),
            (BytecodePc::new(3), FinalOpcode::GetArg0, Operands::NoneArg),
            (BytecodePc::new(4), FinalOpcode::Undefined, Operands::None),
            (
                BytecodePc::new(5),
                FinalOpcode::CopyDataProperties,
                Operands::U8(0b0000_0110),
            ),
            (BytecodePc::new(7), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(8), FinalOpcode::Drop, Operands::None),
            (BytecodePc::new(9), FinalOpcode::Drop, Operands::None),
            (
                BytecodePc::new(10),
                FinalOpcode::ReturnUndef,
                Operands::None
            ),
        ]
    );
    assert_eq!(
        compiled.control_flow().instructions()[2].entry_stack_depth(),
        None,
        "the unreachable object-spread tail remains in the verified structure"
    );
}
