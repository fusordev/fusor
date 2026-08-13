use fusor_bytecode::{
    Binary64Constant, CompilerConstantKind, CompilerConstantValue, FinalOpcode, Operands,
    VerificationLimits,
};
use fusor_compiler::{
    CompilationContext, CompiledConstant, CompiledFunction, CompiledFunctionTree,
    CompiledLeafFunction,
};
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile_leaf(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("binary64 literal lowering must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn compile_tree(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("binary64 tree lowering must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn instructions(function: &CompiledFunction) -> Vec<(FinalOpcode, Operands)> {
    function
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

fn number_bits(constant: &CompiledConstant) -> u64 {
    let CompiledConstant::Value(CompilerConstantValue::Number(number)) = constant else {
        panic!("expected a binary64 Number constant");
    };
    number.to_bits()
}

#[test]
fn non_i32_numeric_literals_lower_to_exact_nondeduplicated_constants() {
    let compiled = compile_leaf("function f(){ return (1.5, 1e400, 1.5); }", "f");

    assert_eq!(compiled.constants().len(), 3);
    assert_eq!(number_bits(&compiled.constants()[0]), 1.5_f64.to_bits());
    assert_eq!(
        number_bits(&compiled.constants()[1]),
        f64::INFINITY.to_bits()
    );
    assert_eq!(number_bits(&compiled.constants()[2]), 1.5_f64.to_bits());
    assert_eq!(
        compiled
            .control_flow()
            .compiler_constant_layout()
            .expect("explicit constant layout")
            .kinds(),
        [
            CompilerConstantKind::Value,
            CompilerConstantKind::Value,
            CompilerConstantKind::Value,
        ]
    );
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::PushConst8, Operands::Const8(0)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::PushConst8, Operands::Const8(1)),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::PushConst8, Operands::Const8(2)),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn binary64_literals_preserve_rounding_subnormal_and_overflow_bits() {
    let compiled = compile_leaf(
        "function f(){ return (0.1, 5e-324, 9007199254740993, 1e400); }",
        "f",
    );

    assert_eq!(compiled.constants().len(), 4);
    assert_eq!(
        compiled
            .constants()
            .iter()
            .map(number_bits)
            .collect::<Vec<_>>(),
        [
            0x3fb9_9999_9999_999a,
            0x0000_0000_0000_0001,
            0x4340_0000_0000_0000,
            0x7ff0_0000_0000_0000,
        ]
    );
}

#[test]
fn exactly_representable_i32_literals_remain_pool_free_integer_opcodes() {
    let compiled = compile_leaf("function f(){ return (0, 0.0, 1.0); }", "f");

    assert!(compiled.constants().is_empty());
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (FinalOpcode::Return, Operands::None),
        ]
    );

    let minimum = compile_leaf("function f(){ return -2147483648; }", "f");
    assert!(minimum.constants().is_empty());
    assert_eq!(
        instructions(&minimum),
        [
            (FinalOpcode::PushI32, Operands::I32(i32::MIN)),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn negative_zero_remains_pool_free_push_zero_then_negate() {
    let compiled = compile_leaf("function f(){ return -0; }", "f");

    assert!(compiled.constants().is_empty());
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Neg, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
}

#[test]
fn value_constant_indices_cross_the_compact_boundary_exactly() {
    let mut source = String::from("function f(){ return (");
    for index in 0..257 {
        if index != 0 {
            source.push(',');
        }
        write!(&mut source, "{index}.5").expect("writing to a String cannot fail");
    }
    source.push_str("); }");

    let compiled = compile_leaf(&source, "f");
    let pushes = instructions(&compiled)
        .into_iter()
        .filter(|(opcode, _)| matches!(opcode, FinalOpcode::PushConst8 | FinalOpcode::PushConst))
        .collect::<Vec<_>>();

    assert_eq!(compiled.constants().len(), 257);
    assert_eq!(pushes.len(), 257);
    assert_eq!(pushes[0], (FinalOpcode::PushConst8, Operands::Const8(0)));
    assert_eq!(
        pushes[255],
        (FinalOpcode::PushConst8, Operands::Const8(u8::MAX))
    );
    assert_eq!(pushes[256], (FinalOpcode::PushConst, Operands::Const(256)));
}

#[test]
fn values_and_functions_share_the_compact_constant_index_boundary() {
    let mut compact_function_source = String::from("function outer(){ return (");
    for index in 0..255 {
        if index != 0 {
            compact_function_source.push(',');
        }
        compact_function_source.push_str("1.5");
    }
    compact_function_source.push_str(",function(){},1.5); }");

    let compact_function_tree = compile_tree(&compact_function_source, "outer");
    let compact_function = compact_function_tree.root();
    assert_eq!(compact_function.constants().len(), 257);
    assert!(
        compact_function.constants()[..255]
            .iter()
            .all(|constant| number_bits(constant) == 1.5_f64.to_bits())
    );
    assert!(matches!(
        compact_function.constants()[255],
        CompiledConstant::Function(_)
    ));
    assert_eq!(
        number_bits(&compact_function.constants()[256]),
        1.5_f64.to_bits()
    );

    let compact_boundary_instructions = instructions(compact_function)
        .into_iter()
        .filter(|(opcode, _)| {
            matches!(
                opcode,
                FinalOpcode::PushConst8
                    | FinalOpcode::PushConst
                    | FinalOpcode::FClosure8
                    | FinalOpcode::FClosure
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        compact_boundary_instructions[254],
        (FinalOpcode::PushConst8, Operands::Const8(254))
    );
    assert_eq!(
        compact_boundary_instructions[255],
        (FinalOpcode::FClosure8, Operands::Const8(u8::MAX))
    );
    assert_eq!(
        compact_boundary_instructions[256],
        (FinalOpcode::PushConst, Operands::Const(256))
    );

    let mut wide_function_source = String::from("function outer(){ return (");
    for index in 0..256 {
        if index != 0 {
            wide_function_source.push(',');
        }
        wide_function_source.push_str("1.5");
    }
    wide_function_source.push_str(",function(){}); }");

    let wide_function_tree = compile_tree(&wide_function_source, "outer");
    let wide_function = wide_function_tree.root();
    assert_eq!(wide_function.constants().len(), 257);
    assert!(
        wide_function.constants()[..256]
            .iter()
            .all(|constant| number_bits(constant) == 1.5_f64.to_bits())
    );
    assert!(matches!(
        wide_function.constants()[256],
        CompiledConstant::Function(_)
    ));
    assert!(instructions(wide_function).contains(&(FinalOpcode::FClosure, Operands::Const(256))));
}

#[test]
fn binary64_and_function_templates_share_one_typed_constant_pool() {
    let tree = compile_tree(
        "function outer(){ return (1.5, function(){ return 4.5; }, 2.5); }",
        "outer",
    );
    let outer = tree.root();

    assert_eq!(outer.constants().len(), 3);
    assert_eq!(number_bits(&outer.constants()[0]), 1.5_f64.to_bits());
    let CompiledConstant::Function(function) = outer.constants()[1] else {
        panic!("function template keeps its source-order pool position");
    };
    let child = tree
        .function(function.executable())
        .expect("function constant resolves to its child");
    assert_eq!(child.constants().len(), 1);
    assert_eq!(number_bits(&child.constants()[0]), 4.5_f64.to_bits());
    assert_eq!(number_bits(&outer.constants()[2]), 2.5_f64.to_bits());
    assert_eq!(
        outer
            .control_flow()
            .compiler_constant_layout()
            .expect("typed heterogeneous layout")
            .kinds(),
        [
            CompilerConstantKind::Value,
            CompilerConstantKind::Function,
            CompilerConstantKind::Value,
        ]
    );
    assert_eq!(
        tree.function_graph().root().constants(),
        [
            fusor_bytecode::CompilerConstant::Value(CompilerConstantValue::Number(
                Binary64Constant::from_f64(1.5),
            )),
            fusor_bytecode::CompilerConstant::Function(
                fusor_bytecode::FunctionTemplateId::new(1),
            ),
            fusor_bytecode::CompilerConstant::Value(CompilerConstantValue::Number(
                Binary64Constant::from_f64(2.5),
            )),
        ]
    );
}
use std::fmt::Write as _;
