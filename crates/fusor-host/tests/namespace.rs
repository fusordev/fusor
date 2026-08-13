//! Fusor namespace installation and op exposure (§5.4, §5.5).

use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::ops::{OpError, install_namespace, install_op};
use fusor_ops::op;
use fusor_runtime::{Context, ExecutionLimits, Runtime, RuntimeLimits};

#[op]
fn op_add(left: i32, right: i32) -> Result<i32, OpError> {
    Ok(left + right)
}

#[op]
fn op_greet(name: String) -> Result<String, OpError> {
    Ok(format!("hello {name}"))
}

#[op]
fn op_fail() -> Result<(), OpError> {
    Err(OpError::of_class("RangeError", "out of range"))
}

fn compile_global_script(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("fusor-namespace.js"))
                    .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn script_text(context: &mut Context<'_>, source: &str) -> String {
    let authority = compile_global_script(source);
    let result = context
        .execute_global_script(authority, ExecutionLimits::default())
        .expect("script");
    result
        .as_string()
        .expect("live string")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

fn with_context<T>(operation: impl FnOnce(&mut Context<'_>) -> T) -> T {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    operation(&mut context)
}

#[test]
fn the_fusor_namespace_installs_with_the_spec_shape() {
    with_context(|context| {
        install_namespace(context).expect("namespace");
        assert_eq!(
            script_text(
                context,
                "var f = Object.getOwnPropertyDescriptor(globalThis, 'Fusor');\
                 var o = Object.getOwnPropertyDescriptor(Fusor, 'ops');\
                 JSON.stringify({\
                     present: f !== undefined,\
                     writable: f.writable,\
                     enumerable: f.enumerable,\
                     configurable: f.configurable,\
                     opsPresent: o !== undefined,\
                     opsWritable: o.writable,\
                     opsConfigurable: o.configurable,\
                     opsObject: typeof Fusor.ops === 'object',\
                 });",
            ),
            "{\"present\":true,\"writable\":false,\"enumerable\":false,\"configurable\":false,\
             \"opsPresent\":true,\"opsWritable\":false,\"opsConfigurable\":false,\"opsObject\":true}"
        );
    });
}

#[test]
fn installed_ops_are_callable_from_javascript() {
    with_context(|context| {
        install_namespace(context).expect("namespace");
        install_op(context, __fusor_op_declaration_op_add(), __fusor_op_call_op_add)
            .expect("add");
        install_op(context, __fusor_op_declaration_op_greet(), __fusor_op_call_op_greet)
            .expect("greet");
        install_op(context, __fusor_op_declaration_op_fail(), __fusor_op_call_op_fail)
            .expect("fail");

        assert_eq!(
            script_text(context, "String(Fusor.ops.op_add(20, 22));"),
            "42"
        );
        assert_eq!(
            script_text(context, "String(Fusor.ops.op_greet('fusor'));"),
            "hello fusor"
        );
        // OpError classes reach JavaScript as the named intrinsic family.
        assert_eq!(
            script_text(
                context,
                "var kind; try { Fusor.ops.op_fail(); } catch (error) { kind = error.name + ':' + error.message; } String(kind);",
            ),
            "RangeError:out of range"
        );
    });
}

#[test]
fn argument_deserialization_failures_raise_parameter_indexed_type_errors() {
    with_context(|context| {
        install_namespace(context).expect("namespace");
        install_op(context, __fusor_op_declaration_op_add(), __fusor_op_call_op_add)
            .expect("add");

        assert_eq!(
            script_text(
                context,
                "var kind, message;\
                 try { Fusor.ops.op_add('not a number', 1); }\
                 catch (error) { kind = error.name; message = error.message; }\
                 String(kind + '|' + message);",
            ),
            "TypeError|parameter 0: expected a Number, received String"
        );
        assert_eq!(
            script_text(
                context,
                "var kind, message;\
                 try { Fusor.ops.op_add(1); }\
                 catch (error) { kind = error.name; message = error.message; }\
                 String(kind + '|' + message);",
            ),
            "TypeError|parameter 1: missing argument"
        );
    });
}

#[test]
fn installing_a_namespace_twice_is_idempotent_and_ops_accumulate() {
    with_context(|context| {
        install_namespace(context).expect("namespace");
        install_namespace(context).expect("namespace again");
        install_op(context, __fusor_op_declaration_op_add(), __fusor_op_call_op_add)
            .expect("add");
        install_op(context, __fusor_op_declaration_op_greet(), __fusor_op_call_op_greet)
            .expect("greet");
        assert_eq!(
            script_text(context, "String(Fusor.ops.op_add(1, 2) + '|' + Fusor.ops.op_greet('x'));"),
            "3|hello x"
        );
    });
}
