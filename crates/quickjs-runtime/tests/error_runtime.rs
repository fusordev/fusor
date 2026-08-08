use std::sync::Arc;

use quickjs_bytecode::{VerificationLimits, VerifiedBytecode};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};
use quickjs_runtime::{
    AtomLimits, ExecutionLimits, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
    PREDEFINED_INTERNER_SLOTS, Runtime, RuntimeError, RuntimeLimits, RuntimeResource, RuntimeUsage,
    ValueKind,
};

const REALM_ERROR_GRAPH_OBJECTS: u64 = 69;
const REALM_ERROR_GRAPH_FUNCTIONS: u64 = 704;
const REALM_ERROR_GRAPH_PROPERTIES: u64 = 2_351;
const REALM_DYNAMIC_ATOMS: u32 = 338;
const REALM_DYNAMIC_ATOM_CODE_UNITS: u64 = 2_936;
const REALM_DYNAMIC_INTERNER_SLOTS: u32 = 338;

fn compile_dynamic(body: &str) -> Arc<VerifiedBytecode> {
    let parameters = [];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new(body),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        context
            .compile_dynamic_function_script(VerificationLimits::default())
            .map(|tree| Arc::new(tree.verified_bytecode().clone()))
    })
    .expect("dynamic frontend")
    .expect("dynamic compiler")
}

#[test]
fn complete_error_realm_graph_has_exact_public_resource_usage() {
    let mut runtime = Runtime::try_new(
        RuntimeLimits::default()
            .with_max_realms(2)
            .with_max_heap_objects(REALM_ERROR_GRAPH_OBJECTS * 2)
            .with_max_heap_functions(REALM_ERROR_GRAPH_FUNCTIONS * 2)
            .with_max_object_properties(REALM_ERROR_GRAPH_PROPERTIES * 2),
    )
    .expect("runtime");

    let first = runtime.create_realm().expect("first exact Error graph");
    let first_usage = runtime.usage();
    assert_eq!(first_usage.realms(), 1);
    assert_eq!(first_usage.heap_objects(), REALM_ERROR_GRAPH_OBJECTS);
    assert_eq!(first_usage.heap_functions(), REALM_ERROR_GRAPH_FUNCTIONS);
    assert_eq!(
        first_usage.object_properties(),
        REALM_ERROR_GRAPH_PROPERTIES
    );

    let atom_usage = runtime.atom_usage();
    assert_eq!(
        atom_usage.live_atoms,
        PREDEFINED_ATOM_COUNT + REALM_DYNAMIC_ATOMS
    );
    assert_eq!(
        atom_usage.live_description_code_units,
        PREDEFINED_DESCRIPTION_CODE_UNITS + REALM_DYNAMIC_ATOM_CODE_UNITS
    );
    assert_eq!(
        atom_usage.interner_slots,
        PREDEFINED_INTERNER_SLOTS + REALM_DYNAMIC_INTERNER_SLOTS
    );

    let second = runtime.create_realm().expect("second exact Error graph");
    let second_usage = runtime.usage();
    assert_eq!(second_usage.realms(), 2);
    assert_eq!(second_usage.heap_objects(), REALM_ERROR_GRAPH_OBJECTS * 2);
    assert_eq!(
        second_usage.heap_functions(),
        REALM_ERROR_GRAPH_FUNCTIONS * 2
    );
    assert_eq!(
        second_usage.object_properties(),
        REALM_ERROR_GRAPH_PROPERTIES * 2
    );
    assert_eq!(runtime.atom_usage(), atom_usage);

    runtime.collect_cycles().expect("rooted realm collection");
    assert_eq!(runtime.usage(), second_usage);
    assert_eq!(runtime.atom_usage(), atom_usage);

    let first_context = runtime.context(&first).expect("first realm remains live");
    assert_eq!(
        first_context.undefined().kind().expect("live value"),
        ValueKind::Undefined
    );
    let second_context = runtime.context(&second).expect("second realm remains live");
    assert_eq!(
        second_context.undefined().kind().expect("live value"),
        ValueKind::Undefined
    );
}

#[test]
fn error_realm_graph_limit_failures_are_atomic_and_runtime_is_reusable() {
    for (limits, expected_resource, limit, observed) in [
        (
            RuntimeLimits::default().with_max_heap_objects(REALM_ERROR_GRAPH_OBJECTS * 2 - 1),
            RuntimeResource::HeapObjects,
            REALM_ERROR_GRAPH_OBJECTS * 2 - 1,
            REALM_ERROR_GRAPH_OBJECTS * 2,
        ),
        (
            RuntimeLimits::default().with_max_heap_functions(REALM_ERROR_GRAPH_FUNCTIONS * 2 - 1),
            RuntimeResource::HeapFunctions,
            REALM_ERROR_GRAPH_FUNCTIONS * 2 - 1,
            REALM_ERROR_GRAPH_FUNCTIONS * 2,
        ),
        (
            RuntimeLimits::default()
                .with_max_object_properties(REALM_ERROR_GRAPH_PROPERTIES * 2 - 1),
            RuntimeResource::ObjectProperties,
            REALM_ERROR_GRAPH_PROPERTIES * 2 - 1,
            REALM_ERROR_GRAPH_PROPERTIES * 2,
        ),
    ] {
        let mut runtime = Runtime::try_new(limits).expect("runtime");
        let surviving_realm = runtime.create_realm().expect("surviving realm");
        let usage = runtime.usage();
        let atoms = runtime.atom_usage();

        for _ in 0..2 {
            assert!(matches!(
                runtime.create_realm(),
                Err(RuntimeError::LimitExceeded {
                    resource,
                    limit: actual_limit,
                    observed: actual_observed,
                }) if resource == expected_resource
                    && actual_limit == limit
                    && actual_observed == observed
            ));
            assert_eq!(runtime.usage(), usage);
            assert_eq!(runtime.atom_usage(), atoms);

            runtime
                .collect_cycles()
                .expect("runtime remains collectable after failed realm creation");
            assert_eq!(runtime.usage(), usage);
            assert_eq!(runtime.atom_usage(), atoms);

            let context = runtime
                .context(&surviving_realm)
                .expect("existing realm remains usable after failure");
            assert_eq!(
                context.undefined().kind().expect("live value"),
                ValueKind::Undefined
            );
        }
    }
}

#[test]
fn error_realm_dynamic_atom_failures_roll_back_all_partial_state() {
    for limits in [
        AtomLimits::new(
            PREDEFINED_ATOM_COUNT + REALM_DYNAMIC_ATOMS - 1,
            PREDEFINED_DESCRIPTION_CODE_UNITS + REALM_DYNAMIC_ATOM_CODE_UNITS,
            PREDEFINED_INTERNER_SLOTS + REALM_DYNAMIC_INTERNER_SLOTS,
        ),
        AtomLimits::new(
            PREDEFINED_ATOM_COUNT + REALM_DYNAMIC_ATOMS,
            PREDEFINED_DESCRIPTION_CODE_UNITS + REALM_DYNAMIC_ATOM_CODE_UNITS - 1,
            PREDEFINED_INTERNER_SLOTS + REALM_DYNAMIC_INTERNER_SLOTS,
        ),
        AtomLimits::new(
            PREDEFINED_ATOM_COUNT + REALM_DYNAMIC_ATOMS,
            PREDEFINED_DESCRIPTION_CODE_UNITS + REALM_DYNAMIC_ATOM_CODE_UNITS,
            PREDEFINED_INTERNER_SLOTS + REALM_DYNAMIC_INTERNER_SLOTS - 1,
        ),
    ] {
        let mut runtime = Runtime::try_new(
            RuntimeLimits::default()
                .with_max_realms(1)
                .with_atom_limits(limits),
        )
        .expect("runtime");
        let atoms = runtime.atom_usage();

        for _ in 0..2 {
            assert!(matches!(runtime.create_realm(), Err(RuntimeError::Atom(_))));
            assert_eq!(runtime.usage(), RuntimeUsage::default());
            assert_eq!(runtime.atom_usage(), atoms);

            runtime
                .collect_cycles()
                .expect("empty runtime remains reusable after atom failure");
            assert_eq!(runtime.usage(), RuntimeUsage::default());
            assert_eq!(runtime.atom_usage(), atoms);
        }
    }
}

#[test]
fn caught_engine_errors_are_branded_and_collect_without_damaging_the_realm_graph() {
    let authority = compile_dynamic(
        "try{null.missing;}catch(error){\
             return Error.isError(error)\
                 &&error.name===\"TypeError\"\
                 &&typeof error.message===\"string\";\
         }\
         return false;",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();

    let (function, result) = {
        let mut context = runtime.context(&realm).expect("context");
        let function = context
            .execute_dynamic_function_script(authority, ExecutionLimits::default())
            .expect("dynamic Function Script")
            .into_function()
            .expect("dynamic Function");
        let result = context
            .call(&function, &[], ExecutionLimits::default())
            .expect("caught engine TypeError");
        (function, result)
    };

    assert_eq!(result.as_boolean().expect("live result"), Some(true));
    drop(result);
    drop(function);
    runtime.collect_cycles().expect("collect dynamic execution");
    let collected = runtime.usage();
    assert_eq!(collected.realms(), baseline.realms());
    assert_eq!(collected.installed_code(), baseline.installed_code());
    assert_eq!(
        collected.installed_templates(),
        baseline.installed_templates()
    );
    assert_eq!(collected.installed_atoms(), baseline.installed_atoms());
    assert_eq!(
        collected.installed_constants(),
        baseline.installed_constants()
    );
    assert_eq!(collected.heap_functions(), baseline.heap_functions());
    assert_eq!(collected.heap_objects(), baseline.heap_objects());
    assert_eq!(collected.object_properties(), baseline.object_properties());
    assert_eq!(collected.for_in_entries(), baseline.for_in_entries());
    assert_eq!(collected.binding_cells(), baseline.binding_cells());
    assert_eq!(collected.public_roots(), baseline.public_roots());
    assert_eq!(collected.pending_releases(), baseline.pending_releases());
    assert_eq!(
        collected.realm_global_bindings(),
        baseline.realm_global_bindings() + 1,
        "the resolved Error lookup remains cached in the realm"
    );
}
