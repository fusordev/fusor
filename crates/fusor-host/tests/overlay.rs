//! Overlay assembly (subproject 6, §9): the `Overlay` trait, the
//! `HostRuntime::builder()` pipeline (topological ordering with cycle
//! detection, op registration through the assembly registry, init
//! scripts — Global Scripts, §8.4 — evaluated in dependency order), and
//! the host core overlay.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fusor_bytecode::VerifiedBytecode;
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::ops::{OpDeclaration, OpError, OpRegistry, set_print_sink};
use fusor_host::overlay::{
    CoreOverlay, HostBuildError, HostRuntime, InitScriptError, Overlay, OverlaySource,
};
use fusor_host::process::ExitCode;
use fusor_ops::{op, register_op};
use fusor_runtime::{Context, ExecutionError, ExecutionLimits, GlobalScriptError};

/// Compiles one Global Script into the authority
/// `execute_global_script` consumes.
fn compile(source: &str) -> Arc<VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from("overlay.js"))
                .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

/// Evaluates one Global Script, mapping both failure arms onto
/// [`ExecutionError`].
fn eval_script(
    context: &mut Context<'_>,
    authority: Arc<VerifiedBytecode>,
) -> Result<(), ExecutionError> {
    match context.execute_global_script(authority, ExecutionLimits::default()) {
        Ok(_) => Ok(()),
        Err(GlobalScriptError::Install(source)) => Err(source.into()),
        Err(GlobalScriptError::Execution(source)) => Err(source),
    }
}

/// Evaluates a Global Script in the host runtime's realm and returns its
/// completion value as a JavaScript String.
fn eval_string(host: &mut HostRuntime, source: &str) -> String {
    let authority = compile(source);
    let mut context = host.context().expect("context");
    let value = match context.execute_global_script(authority, ExecutionLimits::default()) {
        Ok(value) => value,
        Err(GlobalScriptError::Install(source)) => panic!("script install: {source}"),
        Err(GlobalScriptError::Execution(source)) => panic!("script execution: {source}"),
    };
    value
        .as_string()
        .expect("live string")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

/// A configurable overlay for assembly tests.
struct TestOverlay {
    name: &'static str,
    op_registrations: fn(&mut OpRegistry),
    init_sources: Vec<OverlaySource>,
    dependencies: &'static [&'static str],
}

impl TestOverlay {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            op_registrations: |_| {},
            init_sources: Vec::new(),
            dependencies: &[],
        }
    }

    fn with_ops(mut self, registrations: fn(&mut OpRegistry)) -> Self {
        self.op_registrations = registrations;
        self
    }

    fn with_init(mut self, specifier: &'static str, text: &'static str) -> Self {
        self.init_sources.push(OverlaySource {
            specifier: specifier.to_owned(),
            text,
        });
        self
    }

    fn depending_on(mut self, dependencies: &'static [&'static str]) -> Self {
        self.dependencies = dependencies;
        self
    }
}

impl Overlay for TestOverlay {
    fn name(&self) -> &'static str {
        self.name
    }

    fn ops(&self, registry: &mut OpRegistry) {
        (self.op_registrations)(registry);
    }

    fn init_sources(&self) -> Vec<OverlaySource> {
        self.init_sources.clone()
    }

    fn dependencies(&self) -> &'static [&'static str] {
        self.dependencies
    }
}

#[op]
fn op_answer() -> Result<f64, OpError> {
    Ok(42.0)
}

fn register_answer(registry: &mut OpRegistry) {
    register_op!(registry, op_answer);
}

fn register_clash(registry: &mut OpRegistry) {
    registry.register(
        OpDeclaration {
            name: "clash",
            parameter_types: &[],
            is_async: false,
        },
        op_answer::call,
    );
}

#[test]
fn the_builder_installs_the_host_core_without_any_overlay() {
    let mut host = HostRuntime::builder().build().expect("built");
    assert_eq!(
        eval_string(
            &mut host,
            "var f = Object.getOwnPropertyDescriptor(globalThis, 'Fusor');\
             var o = Object.getOwnPropertyDescriptor(Fusor, 'ops');\
             JSON.stringify({\
                 fusor: f !== undefined,\
                 writable: f.writable,\
                 enumerable: f.enumerable,\
                 configurable: f.configurable,\
                 opsObject: typeof Fusor.ops === 'object',\
                 noProcessObject: Fusor.process === undefined,\
             });",
        ),
        "{\"fusor\":true,\"writable\":false,\"enumerable\":false,\"configurable\":false,\
         \"opsObject\":true,\"noProcessObject\":true}"
    );
}

#[test]
fn init_scripts_evaluate_in_topological_order() {
    // `b` is registered first but depends on `a`: the build must evaluate
    // `a`'s init script first regardless of registration order.
    let a = TestOverlay::new("a").with_init(
        "fusor:a/init.js",
        "globalThis.order = ['a'];",
    );
    let b = TestOverlay::new("b")
        .with_init(
            "fusor:b/init.js",
            "globalThis.order.push('b'); globalThis.order_string = globalThis.order.join(',');",
        )
        .depending_on(&["a"]);
    let mut builder = HostRuntime::builder();
    builder.with_overlay(b).with_overlay(a);
    let mut host = builder.build().expect("built");
    assert_eq!(
        eval_string(&mut host, "String(globalThis.order_string);"),
        "a,b"
    );
}

#[test]
fn dependency_cycles_are_rejected_at_build_time() {
    let a = TestOverlay::new("a").depending_on(&["b"]);
    let b = TestOverlay::new("b").depending_on(&["a"]);
    let mut builder = HostRuntime::builder();
    builder.with_overlay(a).with_overlay(b);
    let error = builder.build().expect_err("cycle must fail the build");
    match error {
        HostBuildError::DependencyCycle { cycle } => {
            assert!(cycle.contains(&"a"), "cycle names the overlay: {cycle:?}");
            assert!(cycle.contains(&"b"), "cycle names the overlay: {cycle:?}");
        }
        other => panic!("expected a dependency cycle, got: {other}"),
    }
}

#[test]
fn unknown_dependencies_are_rejected_at_build_time() {
    let orphan = TestOverlay::new("orphan").depending_on(&["ghost"]);
    let mut builder = HostRuntime::builder();
    builder.with_overlay(orphan);
    let error = builder.build().expect_err("unknown dependency must fail the build");
    match error {
        HostBuildError::UnknownDependency {
            overlay: "orphan",
            dependency: "ghost",
        } => {}
        other => panic!("expected an unknown dependency, got: {other}"),
    }
}

#[test]
fn duplicate_overlay_names_are_rejected_at_build_time() {
    let mut builder = HostRuntime::builder();
    builder
        .with_overlay(TestOverlay::new("dup"))
        .with_overlay(TestOverlay::new("dup"));
    let error = builder.build().expect_err("duplicate names must fail the build");
    match error {
        HostBuildError::DuplicateOverlay { name: "dup" } => {}
        other => panic!("expected a duplicate overlay name, got: {other}"),
    }
}

#[test]
fn op_name_conflicts_between_overlays_are_rejected_at_build_time() {
    let mut builder = HostRuntime::builder();
    builder
        .with_overlay(TestOverlay::new("first").with_ops(register_clash))
        .with_overlay(TestOverlay::new("second").with_ops(register_clash));
    let error = builder.build().expect_err("op conflicts must fail the build");
    match error {
        HostBuildError::OpConflict(conflict) => assert_eq!(conflict.name, "clash"),
        other => panic!("expected an op conflict, got: {other}"),
    }
}

#[test]
fn overlay_ops_install_onto_fusor_ops() {
    let mut host = HostRuntime::builder()
        .with_overlay(TestOverlay::new("answer").with_ops(register_answer))
        .build()
        .expect("built");
    assert_eq!(
        eval_string(&mut host, "String(Fusor.ops.op_answer());"),
        "42"
    );
}

#[test]
fn init_scripts_share_the_global_across_overlays() {
    // Scripts have no imports (§8.4): later overlays read what earlier
    // ones published on `globalThis`, ordered by the dependency graph.
    let base = TestOverlay::new("base").with_init(
        "fusor:base/value.js",
        "globalThis.answer = 41; globalThis.order = ['base'];",
    );
    let user = TestOverlay::new("user")
        .with_init(
            "fusor:user/init.js",
            "globalThis.order.push('user');\
             globalThis.order_string = globalThis.order.join(',');\
             globalThis.derived = globalThis.answer + 1;",
        )
        .depending_on(&["base"]);
    let mut host = HostRuntime::builder()
        .with_overlay(user)
        .with_overlay(base)
        .build()
        .expect("built");
    assert_eq!(
        eval_string(&mut host, "String(globalThis.order_string + '|' + globalThis.derived);"),
        "base,user|42"
    );
}

#[test]
fn init_script_sources_evaluate_in_declaration_order() {
    // Within one overlay, sources run in declaration order.
    let overlay = TestOverlay::new("pair")
        .with_init("fusor:pair/first.js", "globalThis.steps = ['first'];")
        .with_init(
            "fusor:pair/second.js",
            "globalThis.steps.push('second'); globalThis.steps_string = globalThis.steps.join(',');",
        );
    let mut host = HostRuntime::builder()
        .with_overlay(overlay)
        .build()
        .expect("built");
    assert_eq!(
        eval_string(&mut host, "String(globalThis.steps_string);"),
        "first,second"
    );
}

#[test]
fn duplicate_init_sources_are_rejected_at_build_time() {
    let first = TestOverlay::new("first").with_init(
        "fusor:shared.js",
        "globalThis.first = true;",
    );
    let second = TestOverlay::new("second").with_init(
        "fusor:shared.js",
        "globalThis.second = true;",
    );
    let mut builder = HostRuntime::builder();
    builder.with_overlay(first).with_overlay(second);
    let error = builder.build().expect_err("duplicate init sources must fail the build");
    match error {
        HostBuildError::DuplicateInitSource { specifier } => {
            assert_eq!(specifier, "fusor:shared.js");
        }
        other => panic!("expected a duplicate init source, got: {other}"),
    }
}

#[test]
fn init_script_locations_use_the_specifier() {
    // A throwing init script reports its specifier as the source name
    // (§8.4: location only, no debugger hook).
    let boom = TestOverlay::new("boom").with_init(
        "fusor:boom/init.js",
        "throw new Error('boom');",
    );
    let mut builder = HostRuntime::builder();
    builder.with_overlay(boom);
    let error = builder.build().expect_err("a throwing init script must fail the build");
    match error {
        HostBuildError::InitScript {
            overlay: "boom",
            error: InitScriptError::Execution(GlobalScriptError::Execution(source)),
        } => {
            let ExecutionError::Exception(exception) = source else {
                panic!("expected a JavaScript exception, got: {source}");
            };
            assert_eq!(
                exception.source_name(),
                "fusor:boom/init.js",
                "the specifier is the diagnostic location"
            );
        }
        other => panic!("expected an init script failure, got: {other}"),
    }
}

#[test]
fn the_core_overlay_provides_the_core_ops() {
    let mut host = HostRuntime::builder()
        .with_overlay(CoreOverlay)
        .build()
        .expect("built");
    // The print op writes through the installable sink.
    let captured = Rc::new(RefCell::new(String::new()));
    let sink = Rc::clone(&captured);
    set_print_sink(Box::new(move |line: &str| sink.borrow_mut().push_str(line)));
    assert_eq!(
        eval_string(&mut host, "Fusor.ops.op_core_print('hello', 42); String('done');"),
        "done"
    );
    assert_eq!(*captured.borrow(), "hello 42");
}

#[test]
fn a_built_host_runtime_drives_the_event_loop() {
    let bumped = Rc::new(Cell::new(0u32));
    let read = Rc::clone(&bumped);
    let mut host = HostRuntime::builder()
        .with_overlay(CoreOverlay)
        .build()
        .expect("built")
        .into_loop()
        .expect("loop");
    {
        let authority = compile(
            "Fusor.ops.op_set_immediate(function () { globalThis.bumped = 7; });",
        );
        host.post_event(Box::new(move |context| eval_script(context, authority)));
    }
    host.run_one_turn().expect("turn");
    host.post_event(Box::new(move |context| {
        let global = context.global_object()?.into_object()?;
        let key = context.property_key("bumped")?;
        let value = global.get(context, key)?;
        bumped.set(value.as_u32().ok().flatten().unwrap_or(0));
        Ok(())
    }));
    host.run_one_turn().expect("turn");
    assert_eq!(read.get(), 7, "the setImmediate callback ran during the turn");
    assert_eq!(host.shutdown(), ExitCode::Clean);
}
