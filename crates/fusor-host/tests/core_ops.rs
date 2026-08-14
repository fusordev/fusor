//! The core print op (§5.4 host global conventions):
//! `Fusor.ops.op_core_print` renders variadic arguments console.log-style
//! through the installable print sink (stdout by default).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::r#loop::HostLoop;
use fusor_runtime::{
    Context, ExecutionError, ExecutionLimits, GlobalScriptError, Runtime, RuntimeLimits,
};

/// Evaluates one Global Script, mapping both failure arms onto
/// [`ExecutionError`].
fn eval_script(
    context: &mut Context<'_>,
    authority: Arc<fusor_bytecode::VerifiedBytecode>,
) -> Result<(), ExecutionError> {
    match context.execute_global_script(authority, ExecutionLimits::default()) {
        Ok(_) => Ok(()),
        Err(GlobalScriptError::Install(source)) => Err(source.into()),
        Err(GlobalScriptError::Execution(source)) => Err(source),
    }
}

fn compile(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("core-ops.js"))
                    .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

/// A loop with the namespace and core ops installed.
fn fixture() -> HostLoop {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        fusor_host::ops::install_namespace(&mut context).expect("namespace");
        fusor_host::ops::install_core_ops(&mut context).expect("core ops");
    }
    HostLoop::new(runtime, realm).expect("host loop")
}

#[test]
fn op_core_print_renders_variadic_arguments_to_the_sink() {
    let lines = Rc::new(RefCell::new(Vec::new()));
    let lines_in = Rc::clone(&lines);
    fusor_host::ops::set_print_sink(Box::new(move |line: &str| {
        lines_in.borrow_mut().push(line.to_owned());
    }));
    let mut host = fixture();
    let authority = compile("Fusor.ops.op_core_print('hello', 42, null, true);");
    host.post_event(Box::new(move |context| eval_script(context, authority)));
    host.run_one_turn().expect("turn");
    let authority = compile("Fusor.ops.op_core_print();");
    host.post_event(Box::new(move |context| eval_script(context, authority)));
    host.run_one_turn().expect("turn");
    assert_eq!(
        *lines.borrow(),
        vec!["hello 42 null true".to_owned(), String::new()],
        "strings render raw, numbers via ToString, zero arguments print an empty line"
    );
}

#[test]
fn op_core_print_returns_undefined_without_throwing() {
    let mut host = fixture();
    let result = Rc::new(RefCell::new(String::new()));
    let result_in = Rc::clone(&result);
    let authority = compile(
        "Fusor.ops.op_core_print({ a: 1 }, function () {}); 'ok';",
    );
    host.post_event(Box::new(move |context| {
        let value = context
            .execute_global_script(authority, ExecutionLimits::default())
            .map_err(|error| match error {
                GlobalScriptError::Install(source) => source.into(),
                GlobalScriptError::Execution(source) => source,
            })?;
        *result_in.borrow_mut() = value
            .as_string()
            .expect("live")
            .expect("string")
            .to_utf8_lossy()
            .expect("utf8");
        Ok(())
    }));
    host.run_one_turn().expect("turn");
    assert_eq!(
        *result.borrow(),
        "ok",
        "objects and functions print without throwing (console.log-style)"
    );
}
