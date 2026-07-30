use std::{fmt::Write as _, sync::Arc};

use quickjs_bytecode::FinalOpcode;
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{ExecutionLimits, JsNumber, JsString, Runtime, RuntimeLimits};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("boundary-test.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn opcodes(authority: &quickjs_bytecode::VerifiedBytecode, function: u32) -> Vec<FinalOpcode> {
    authority
        .function(quickjs_bytecode::FunctionTemplateId::new(function))
        .expect("function")
        .function()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect()
}

#[test]
fn compact_and_wide_constant_indices_execute_identically() {
    let values = (0..=256)
        .map(|index| format!("{}.5", 10_000 + index))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("function constants(){{return ({values});}}");
    let authority = compile(&source, "constants");
    let opcodes = opcodes(&authority, 0);
    assert!(opcodes.contains(&FinalOpcode::PushConst8));
    assert!(opcodes.contains(&FinalOpcode::PushConst));

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let result = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("result")
        .as_number()
        .expect("live value")
        .expect("Number");
    assert_eq!(result.as_f64().to_bits(), 10_256.5_f64.to_bits());
}

#[test]
fn compact_and_wide_local_indices_execute_identically() {
    let declarations = (0..=256)
        .map(|index| {
            if index == 256 {
                "let local256=input;".to_owned()
            } else {
                format!("let local{index};")
            }
        })
        .collect::<String>();
    let source = format!("function locals(input){{{declarations}return local256;}}");
    let authority = compile(&source, "locals");
    let opcodes = opcodes(&authority, 0);
    assert!(
        opcodes
            .iter()
            .any(|opcode| matches!(opcode, FinalOpcode::PutLoc8 | FinalOpcode::SetLoc8))
    );
    assert!(
        opcodes
            .iter()
            .any(|opcode| matches!(opcode, FinalOpcode::PutLoc | FinalOpcode::SetLoc))
    );
    assert!(opcodes.contains(&FinalOpcode::GetLocCheck));

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let input = context.string(JsString::from_utf8("wide-local").expect("string"));
    let result = context
        .call(&function, &[input], ExecutionLimits::default())
        .expect("result");
    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "wide-local"
    );
}

#[test]
fn compact_and_wide_function_constants_create_callable_closures() {
    let mut declarations = String::new();
    for index in 0..=256 {
        write!(declarations, "function child{index}(){{return {index};}}")
            .expect("writing to String cannot fail");
    }
    let source = format!("function functions(){{{declarations}return child256;}}");
    let authority = compile(&source, "functions");
    let opcodes = opcodes(&authority, 0);
    assert!(opcodes.contains(&FinalOpcode::FClosure8));
    assert!(opcodes.contains(&FinalOpcode::FClosure));

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let child = context
        .call(&function, &[], ExecutionLimits::default())
        .expect("child")
        .into_function()
        .expect("child function");
    let result = context
        .call(&child, &[], ExecutionLimits::default())
        .expect("result")
        .as_number()
        .expect("live value")
        .expect("Number");
    assert_eq!(result.as_f64().to_bits(), 256.0_f64.to_bits());
}

#[test]
fn compact_and_wide_branches_follow_only_verified_successors() {
    let compact = compile(
        "function compact(value){if(value){return 1;}return 0;}",
        "compact",
    );
    assert!(opcodes(&compact, 0).contains(&FinalOpcode::IfFalse8));

    let body = "0;".repeat(300);
    let source = format!("function wide(value){{if(value){{{body}return 1;}}return 0;}}");
    let wide = compile(&source, "wide");
    assert!(opcodes(&wide, 0).contains(&FinalOpcode::IfFalse));

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let compact = context.instantiate(compact).expect("compact");
    let wide = context.instantiate(wide).expect("wide");

    for function in [&compact, &wide] {
        for (condition, expected) in [(false, 0.0_f64), (true, 1.0_f64)] {
            let condition = context.boolean(condition);
            let result = context
                .call(function, &[condition], ExecutionLimits::default())
                .expect("branch result")
                .as_number()
                .expect("live value")
                .expect("Number");
            assert_eq!(result.as_f64().to_bits(), expected.to_bits());
        }
    }
}

#[test]
fn full_width_argument_indices_are_runtime_checked_and_executed() {
    let parameters = (0..=4)
        .map(|index| format!("argument{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("function argument({parameters}){{return argument4;}}");
    let authority = compile(&source, "argument");
    assert!(opcodes(&authority, 0).contains(&FinalOpcode::GetArg));

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let function = context.instantiate(authority).expect("function");
    let arguments = (0..=4)
        .map(|value| context.number(JsNumber::from_i32(value)))
        .collect::<Vec<_>>();
    let result = context
        .call(&function, &arguments, ExecutionLimits::default())
        .expect("result")
        .as_number()
        .expect("live value")
        .expect("Number");
    assert_eq!(result.as_f64().to_bits(), 4.0_f64.to_bits());
}
