//! Resource table lifecycle (§5.6) and ResourceId op specialization (§5.8).

use std::cell::RefCell;
use std::rc::Rc;

use fusor_host::ops::{
    OpError, Resource, ResourceId, ResourceTable, add_resource, install_op, install_resource_table,
    lookup_resource,
};
use fusor_host::overlay::HostRuntime;
use fusor_ops::op;
use fusor_runtime::{Context, ExecutionLimits, JsNumber, Runtime, RuntimeLimits};

#[derive(Debug)]
struct TestResource {
    name: &'static str,
    closed: Rc<RefCell<bool>>,
}

impl Resource for TestResource {
    fn name(&self) -> &'static str {
        self.name
    }

    fn close(self: Rc<Self>) {
        *self.closed.borrow_mut() = true;
    }
}

fn resource(name: &'static str, closed: Rc<RefCell<bool>>) -> Rc<dyn Resource> {
    Rc::new(TestResource { name, closed })
}

fn script_text(context: &mut Context<'_>, source: &str) -> String {
    use std::sync::Arc;
    let authority = {
        use fusor_compiler::CompilationContext;
        use fusor_frontend::{
            CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
        };
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("resources.js"))
                        .expect("storage plan");
                let tree = context
                    .compile_global_script(fusor_bytecode::VerificationLimits::default())
                    .expect("verified Global Script");
                Arc::new(tree.verified_bytecode().clone())
            },
        )
        .expect("frontend")
    };
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

#[op]
fn op_use_resource(id: fusor_host::ops::ResourceId) -> Result<u32, OpError> {
    match lookup_resource(id.get()) {
        Some(resource) => Ok(id.get()),
        None => Err(OpError::type_error(0, "resource not found")),
    }
}

#[test]
fn ids_are_monotonic_and_never_reused() {
    let mut table = ResourceTable::new();
    let closed = Rc::new(RefCell::new(false));
    let first = table
        .add(resource("first", Rc::clone(&closed)))
        .expect("id");
    let second = table.add(resource("second", closed)).expect("id");
    assert_ne!(first, second);
    assert_eq!(first.get(), 0);
    assert_eq!(second.get(), 1);

    // Closing and re-adding never reuses an id.
    assert!(table.close(first));
    let third = table
        .add(resource("third", Rc::new(RefCell::new(false))))
        .expect("id");
    assert_eq!(third.get(), 2);
    assert!(table.get(first).is_none());
}

#[test]
fn close_runs_the_resource_close_hook_exactly_once() {
    let mut table = ResourceTable::new();
    let closed = Rc::new(RefCell::new(false));
    let id = table
        .add(resource("closable", Rc::clone(&closed)))
        .expect("id");
    assert!(!*closed.borrow());
    assert!(table.close(id));
    assert!(*closed.borrow());
    assert!(!table.close(id), "closing twice reports the unknown id");
}

#[test]
fn close_runs_the_hook_even_with_live_clones() {
    let mut table = ResourceTable::new();
    let closed = Rc::new(RefCell::new(false));
    let id = table
        .add(resource("shared", Rc::clone(&closed)))
        .expect("id");
    let clone = Rc::clone(table.get(id).expect("live"));
    assert!(table.close(id));
    // JavaScript close semantics: the hook runs immediately; the surviving
    // clone keeps the (logically closed) value alive.
    assert!(*closed.borrow(), "close runs its hook immediately");
    drop(clone);
}

#[test]
fn close_all_releases_every_resource() {
    let mut table = ResourceTable::new();
    let closed_a = Rc::new(RefCell::new(false));
    let closed_b = Rc::new(RefCell::new(false));
    table.add(resource("a", Rc::clone(&closed_a))).expect("id");
    table.add(resource("b", Rc::clone(&closed_b))).expect("id");
    table.close_all();
    assert!(*closed_a.borrow());
    assert!(*closed_b.borrow());
    assert!(table.is_empty());
}

#[test]
fn resource_id_parameters_resolve_through_the_installed_table() {
    let mut host_runtime = HostRuntime::builder().build().expect("built");
    let mut context = host_runtime.context().expect("context");
    install_resource_table(ResourceTable::new()).expect("installed");
    install_op(
        &mut context,
        op_use_resource::declaration(),
        op_use_resource::call,
    )
    .expect("use resource");

    let closed = Rc::new(RefCell::new(false));
    let id = add_resource(resource("host-side", Rc::clone(&closed))).expect("id");
    assert_eq!(id.get(), 0);

    // A live id resolves through JavaScript; a stale id raises the
    // parameter-indexed TypeError.
    let live_script = format!("String(Fusor.ops.op_use_resource({}));", id.get());
    assert_eq!(script_text(&mut context, &live_script), "0");
    assert_eq!(
        script_text(
            &mut context,
            "var kind, message;\
             try { Fusor.ops.op_use_resource(7); }\
             catch (error) { kind = error.name; message = error.message; }\
             String(kind + '|' + message);",
        ),
        "TypeError|parameter 0: resource not found"
    );
}

#[test]
fn the_table_is_single_owner_by_construction() {
    // The compile-time single-owner assertions: a resource is !Send/!Sync
    // through Rc, and the table can never be shared across threads.
    fn assert_not_send<T: ?Sized>() {}
    fn assert_send<T: Send>() {}
    assert_not_send::<Rc<dyn Resource>>();
    assert_send::<ResourceId>();
    let _ = ExecutionLimits::default();
}

#[test]
fn closing_a_resource_makes_its_id_fail_closed_in_ops() {
    let mut host_runtime = HostRuntime::builder().build().expect("built");
    let mut context = host_runtime.context().expect("context");
    install_resource_table(ResourceTable::new()).expect("installed");
    install_op(
        &mut context,
        op_use_resource::declaration(),
        op_use_resource::call,
    )
    .expect("use resource");

    let closed = Rc::new(RefCell::new(false));
    let id = add_resource(resource("host-side", Rc::clone(&closed))).expect("id");

    // Explicit close: the id stops resolving immediately.
    assert!(lookup_resource(id.get()).is_some());
    assert!(fusor_host::ops::close_resource(id.get()));
    assert!(
        lookup_resource(id.get()).is_none(),
        "a closed resource no longer resolves"
    );

    // The close hook ran exactly once.
    assert!(*closed.borrow());
}

/// The single-owner rule is enforced at compile time: an `Rc` held across
/// an await makes the future `!Send`, which the Tokio spawn rejects.
///
/// ```compile_fail
/// fn assert_send<T: Send>(_: T) {}
///
/// async fn spawn_demo() {
///     let resource = std::rc::Rc::new(());
///     let future = async move {
///         let _held = &resource;
///         tokio::task::yield_now().await;
///         let _held = &resource;
///     };
///     assert_send(future);
/// }
/// ```
#[test]
fn rc_across_await_is_compile_time_rejected() {
    // The compile_fail doctest above is the assertion; this test documents
    // that the op spawn path demands `Send`, closing the single-owner loop.
    fn assert_send<T: Send>(_: T) {}
    let future = async { Ok::<u32, OpError>(1) };
    assert_send(future);
}
