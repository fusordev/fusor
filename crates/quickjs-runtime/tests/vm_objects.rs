use std::sync::Arc;

use quickjs_bytecode::FunctionTemplateId;
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    Context, ExceptionKind, ExecutionError, ExecutionLimits, JsException, JsNumber, JsValue,
    Object, Realm, Runtime, RuntimeLimits, RuntimeResource, ValueKind,
};

macro_rules! assert_not_impl {
    ($type:ty: $trait:path) => {
        const _: fn() = || {
            trait AmbiguousIfImpl<A> {
                fn marker() {}
            }

            impl<T: ?Sized> AmbiguousIfImpl<()> for T {}

            struct Invalid;
            impl<T: ?Sized + $trait> AmbiguousIfImpl<Invalid> for T {}

            let _ = <$type as AmbiguousIfImpl<_>>::marker;
        };
    };
}

assert_not_impl!(Object: Send);
assert_not_impl!(Object: Sync);

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("runtime-objects.js"))
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

fn runtime(limits: RuntimeLimits) -> Runtime {
    Runtime::try_new(limits).expect("runtime")
}

fn with_context<T>(
    runtime: &mut Runtime,
    realm: &Realm,
    operation: impl FnOnce(&mut Context<'_>) -> T,
) -> T {
    let mut context = runtime.context(realm).expect("context");
    operation(&mut context)
}

fn assert_number(value: &JsValue, expected: i32) {
    let actual = value.as_number().expect("live value").expect("number");
    assert!(actual.strict_equals(JsNumber::from_i32(expected)));
}

fn reserved_frame_values(
    authority: &quickjs_bytecode::VerifiedBytecode,
    function: FunctionTemplateId,
) -> u64 {
    let control_flow = authority
        .function(function)
        .expect("function")
        .function()
        .control_flow();
    let domains = control_flow.domains();
    u64::from(domains.argument_count())
        + u64::from(domains.local_count())
        + u64::from(control_flow.computed_stack_size())
        + 1
}

fn escaping_exception(result: Result<JsValue, ExecutionError>) -> JsException {
    match result {
        Err(ExecutionError::Exception(exception)) => exception,
        Err(error) => panic!("expected escaping JavaScript throw, found {error:?}"),
        Ok(value) => panic!("expected escaping JavaScript throw, returned {value:?}"),
    }
}

fn assert_engine_exception(
    exception: &JsException,
    source: &str,
    expected_message: &str,
    expected_origin: &str,
) {
    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(
        exception
            .message()
            .expect("engine message")
            .to_utf8_lossy()
            .expect("message"),
        expected_message
    );
    assert_eq!(exception.source_name(), "runtime-objects.js");
    assert_eq!(exception.source_text(), source);
    let span = exception.source_span();
    assert_eq!(
        &source[span.start() as usize..span.end() as usize],
        expected_origin
    );
}

#[test]
fn object_literal_own_data_property_reads_and_writes() {
    let authority = compile(
        "function run(write){\
            let object={value:1};\
            if(write){object.value=9;}\
            return object.value;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let read_only = context.boolean(false);
    let initial = context
        .call(&run, &[read_only], ExecutionLimits::default())
        .expect("own property read");
    assert_number(&initial, 1);

    let write = context.boolean(true);
    let updated = context
        .call(&run, &[write], ExecutionLimits::default())
        .expect("own property write");
    assert_number(&updated, 9);
}

#[test]
fn duplicate_literal_key_replaces_one_slot_without_double_charging() {
    let authority = compile("function run(){return {value:1,value:2}.value;}", "run");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let (_run, baseline) = with_context(&mut runtime, &realm, |context| {
        let run = context.instantiate(authority).expect("run");
        let baseline = context.runtime_usage();

        let result = context
            .call(&run, &[], ExecutionLimits::default())
            .expect("duplicate property replacement");
        assert_number(&result, 2);
        assert_eq!(
            context.runtime_usage().object_properties(),
            baseline.object_properties() + 1,
            "a duplicate key replaces its data slot instead of appending one"
        );
        (run, baseline)
    });

    let report = runtime.collect_cycles().expect("collection");
    assert_eq!(report.objects(), 1);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn static_getter_and_setter_halves_merge_in_both_source_orders() {
    let getter_first = compile(
        "function run(){\
            let stored=0;\
            let object={\
                get value(){return stored;},\
                set value(next){stored=next;}\
            };\
            let assigned=object.value=17;\
            if(assigned!==17){return 1;}\
            return object.value;\
        }",
        "run",
    );
    let setter_first = compile(
        "function run(){\
            let stored=0;\
            let object={\
                set value(next){stored=next;},\
                get value(){return stored;}\
            };\
            let assigned=object.value=23;\
            if(assigned!==23){return 1;}\
            return object.value;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let getter_first = context.instantiate(getter_first).expect("getter first");
    let setter_first = context.instantiate(setter_first).expect("setter first");

    let result = context
        .call(&getter_first, &[], ExecutionLimits::default())
        .expect("getter then setter");
    assert_number(&result, 17);
    let result = context
        .call(&setter_first, &[], ExecutionLimits::default())
        .expect("setter then getter");
    assert_number(&result, 23);
}

#[test]
fn repeated_accessor_halves_replace_only_the_matching_half() {
    let authority = compile(
        "function run(){\
            let stored=0;\
            let object={\
                get value(){return 1;},\
                get value(){return stored;},\
                set value(next){stored=2;},\
                set value(next){stored=3;}\
            };\
            object.value=9;\
            return object.value;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("repeated accessor halves");
    assert_number(&result, 3);
}

#[test]
fn data_and_accessor_sandwiches_follow_source_order() {
    let authority = compile(
        "function run(){\
            let dataLast={value:1,get value(){return 2;},value:3};\
            let getterLast={get value(){return 4;},value:5,get value(){return 6;}};\
            if(dataLast.value!==3){return 1;}\
            return getterLast.value;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("data/accessor replacement");
    assert_number(&result, 6);
}

#[test]
fn methods_and_getters_replace_each_other_in_source_order() {
    let authority = compile(
        "function run(){\
            let methodLast={get value(){return 1;},value(){return 2;}};\
            let getterLast={value(){return 3;},get value(){return 4;}};\
            if(methodLast.value()!==2){return 1;}\
            return getterLast.value;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("method/accessor replacement");
    assert_number(&result, 4);
}

#[test]
fn extracted_concise_methods_preserve_sloppy_and_strict_this_modes() {
    let authority = compile(
        "function run(){\
            let object={\
                sloppy(){return this;},\
                strict(){\"use strict\";return this;}\
            };\
            let sloppy=object.sloppy;\
            let strict=object.strict;\
            let sloppyThis=sloppy();\
            let strictThis=strict();\
            if(sloppyThis===void 0){return false;}\
            if(sloppyThis===object){return false;}\
            return strictThis===void 0;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("extracted method calls");
    assert_eq!(result.as_boolean().expect("live value"), Some(true));
}

#[test]
fn proto_spelled_concise_method_is_an_ordinary_method_property() {
    let authority = compile(
        "function run(){\
            let object={__proto__(){return 31;}};\
            if(object.__proto__.name!==\"__proto__\"){return 1;}\
            return object.__proto__();\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("__proto__ method");
    assert_number(&result, 31);
}

#[test]
fn escaped_identifier_method_uses_its_cooked_property_name() {
    let authority = compile(
        "function run(){\
            let object={\\u006dethod(){return 43;}};\
            if(object.method.name!==\"method\"){return 1;}\
            return object.method();\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("escaped identifier method");
    assert_number(&result, 43);
}

#[test]
fn setter_receives_original_receiver_and_rhs_and_its_return_is_discarded() {
    let authority = compile(
        "function run(){\
            let observed=0;\
            let receiver;\
            let object={\
                marker:31,\
                get value(){return observed;},\
                set value(next){\"use strict\";receiver=this;observed=next;return 99;}\
            };\
            let assigned=object.value=47;\
            if(assigned!==47){return 1;}\
            if(receiver!==object){return 2;}\
            return object.value;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("setter call");
    assert_number(&result, 47);
}

#[test]
fn getter_only_assignment_is_a_sloppy_noop_and_a_strict_exact_error() {
    let sloppy_source = "function write(){\
        let object={get value(){return 3;}};\
        let assigned=object.value=9;\
        if(assigned!==9){return 1;}\
        return object.value;\
    }";
    let strict_source = "function write(){\"use strict\";\
        let object={get value(){return 3;}};\
        return object.value=9;\
    }";
    let sloppy = compile(sloppy_source, "write");
    let strict = compile(strict_source, "write");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let sloppy = context.instantiate(sloppy).expect("sloppy writer");
    let strict = context.instantiate(strict).expect("strict writer");

    let result = context
        .call(&sloppy, &[], ExecutionLimits::default())
        .expect("sloppy missing setter");
    assert_number(&result, 3);

    let exception = escaping_exception(context.call(&strict, &[], ExecutionLimits::default()));
    assert_engine_exception(
        &exception,
        strict_source,
        "no setter for property",
        "object.value",
    );
    assert!(exception.caller_frames().is_empty());
}

#[test]
fn object_literal_methods_are_named_nonconstructors_without_a_prototype_value() {
    let inspect = compile(
        "function inspect(){\
            let object={method(first,second){return second;}};\
            if(object.method.name!==\"method\"){return 1;}\
            if(object.method.length!==2){return 2;}\
            return object.method.prototype===void 0;\
        }",
        "inspect",
    );
    let construct_source =
        "function construct(){let object={method(){return 1;}};return new object.method();}";
    let construct = compile(construct_source, "construct");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let inspect = context.instantiate(inspect).expect("inspect");
    let construct = context.instantiate(construct).expect("construct");

    let result = context
        .call(&inspect, &[], ExecutionLimits::default())
        .expect("method observables");
    assert_eq!(result.as_boolean().expect("live value"), Some(true));

    let exception = escaping_exception(context.call(&construct, &[], ExecutionLimits::default()));
    assert_engine_exception(
        &exception,
        construct_source,
        "method is not a constructor",
        "new object.method()",
    );
    assert!(exception.caller_frames().is_empty());
}

#[test]
fn throwing_setter_keeps_throw_origin_and_assignment_caller_then_runtime_reuses() {
    let source = "function run(shouldThrow){\
        let object={\
            set value(next){if(shouldThrow){throw next;}return 99;}\
        };\
        object.value=37;\
        return 7;\
    }";
    let authority = compile(source, "run");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let should_throw = context.boolean(true);
    let exception =
        escaping_exception(context.call(&run, &[should_throw], ExecutionLimits::default()));
    assert_eq!(exception.kind(), None);
    assert_number(exception.thrown_value().expect("thrown RHS"), 37);
    assert_eq!(exception.source_text(), source);
    let origin = exception.source_span();
    assert_eq!(
        &source[origin.start() as usize..origin.end() as usize],
        "throw next;"
    );
    let callers = exception.caller_frames();
    assert_eq!(callers.len(), 1);
    let caller = callers[0].source_span();
    assert_eq!(
        &source[caller.start() as usize..caller.end() as usize],
        "object.value"
    );

    let should_return = context.boolean(false);
    let result = context
        .call(&run, &[should_return], ExecutionLimits::default())
        .expect("runtime remains reusable");
    assert_number(&result, 7);
}

#[test]
fn setter_calls_charge_the_exact_cumulative_frame_value_limit() {
    let authority = compile(
        "function run(input){\
            let object={set value(next){return next;}};\
            object.value=input;\
            return input;\
        }",
        "run",
    );
    let outer_values = reserved_frame_values(&authority, FunctionTemplateId::new(0));
    let setter_values = reserved_frame_values(&authority, FunctionTemplateId::new(1));
    let limit = outer_values
        .checked_add(setter_values)
        .expect("small frame usage")
        - 1;
    let mut runtime = runtime(
        RuntimeLimits::default()
            .with_max_active_frames(2)
            .with_max_active_frame_values(limit),
    );
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let input = context.number(JsNumber::from_i32(5));

    assert!(matches!(
        context
            .call(&run, &[input], ExecutionLimits::default())
            .expect_err("setter frame values exceed limit"),
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::FrameValues,
            limit: actual_limit,
            observed,
        } if actual_limit == limit && observed == outer_values + setter_values
    ));
}

#[test]
fn setter_recursion_uses_shared_instruction_fuel_not_the_rust_stack() {
    let authority = compile(
        "function run(){\
            let object={set value(next){object.value=next;}};\
            object.value=1;\
        }",
        "run",
    );
    let constant = compile("function constant(){return 3;}", "constant");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");
    let constant = context.instantiate(constant).expect("constant");

    assert!(matches!(
        context
            .call(
                &run,
                &[],
                ExecutionLimits::default().with_instruction_fuel(64),
            )
            .expect_err("shared fuel interrupts setter recursion"),
        ExecutionError::InstructionLimitExceeded {
            limit: 64,
            executed: 64,
        }
    ));
    let result = context
        .call(&constant, &[], ExecutionLimits::default())
        .expect("runtime remains reusable after fuel exhaustion");
    assert_number(&result, 3);
}

#[test]
fn strict_method_call_receives_the_base_object_as_this() {
    let authority = compile(
        "function run(){\
            function method(){\"use strict\";return this.value;}\
            let object={\
                value:37,\
                method:method\
            };\
            return object.method();\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("receiver-aware method call");
    assert_number(&result, 37);
}

#[test]
fn parenthesized_member_call_keeps_this_while_sequence_call_drops_it() {
    let authority = compile(
        "function run(bound){\
            function method(){\"use strict\";return this;}\
            let object={method:method};\
            if(bound){return (object.method)()===object;}\
            return (0,object.method)()===void 0;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let bound = context.boolean(true);
    let bound_result = context
        .call(&run, &[bound], ExecutionLimits::default())
        .expect("parenthesized member call");
    assert_eq!(bound_result.as_boolean().expect("live value"), Some(true));

    let unbound = context.boolean(false);
    let unbound_result = context
        .call(&run, &[unbound], ExecutionLimits::default())
        .expect("sequence direct call");
    assert_eq!(unbound_result.as_boolean().expect("live value"), Some(true));
}

#[test]
fn nullish_property_reads_and_writes_throw_exact_type_errors() {
    let read_source = "function read(value){return value.property;}";
    let write_source = "function write(value){return value.property=1;}";
    let read = compile(read_source, "read");
    let write = compile(write_source, "write");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let read = context.instantiate(read).expect("read");
    let write = context.instantiate(write).expect("write");

    for (argument, suffix) in [(context.null(), "null"), (context.undefined(), "undefined")] {
        let read_exception = escaping_exception(context.call(
            &read,
            std::slice::from_ref(&argument),
            ExecutionLimits::default(),
        ));
        assert_engine_exception(
            &read_exception,
            read_source,
            &format!("cannot read property 'property' of {suffix}"),
            "value.property",
        );
        assert!(read_exception.caller_frames().is_empty());

        let write_exception =
            escaping_exception(context.call(&write, &[argument], ExecutionLimits::default()));
        assert_engine_exception(
            &write_exception,
            write_source,
            &format!("cannot set property 'property' of {suffix}"),
            "value.property",
        );
        assert!(write_exception.caller_frames().is_empty());
    }
}

#[test]
fn primitive_property_write_is_ignored_sloppily_and_throws_strictly() {
    let sloppy_source = "function write(){return (1).property=9;}";
    let strict_source = "function write(){\"use strict\";return (1).property=9;}";
    let sloppy = compile(sloppy_source, "write");
    let strict = compile(strict_source, "write");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let sloppy = context.instantiate(sloppy).expect("sloppy write");
    let strict = context.instantiate(strict).expect("strict write");

    let result = context
        .call(&sloppy, &[], ExecutionLimits::default())
        .expect("sloppy primitive write");
    assert_number(&result, 9);

    let exception = escaping_exception(context.call(&strict, &[], ExecutionLimits::default()));
    assert_engine_exception(&exception, strict_source, "not an object", "(1).property");
    assert!(exception.caller_frames().is_empty());
}

#[test]
fn function_object_properties_are_traced_and_reclaimed() {
    let authority = compile(
        "function run(){\
            function target(){}\
            target.value={answer:31};\
            return target.value.answer;\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let (_run, baseline) = with_context(&mut runtime, &realm, |context| {
        let run = context.instantiate(authority).expect("run");
        let baseline = context.runtime_usage();

        let result = context
            .call(&run, &[], ExecutionLimits::default())
            .expect("function object property");
        assert_number(&result, 31);
        let live = context.runtime_usage();
        assert_eq!(live.heap_functions(), baseline.heap_functions() + 1);
        assert_eq!(live.heap_objects(), baseline.heap_objects() + 2);
        assert_eq!(live.object_properties(), baseline.object_properties() + 6);
        (run, baseline)
    });

    let report = runtime.collect_cycles().expect("collection");
    assert_eq!(report.functions(), 1);
    assert_eq!(report.objects(), 2);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn method_failures_keep_call_provenance_and_leave_the_runtime_reusable() {
    let non_callable_source = "function fail(){let object={method:1};return object.method();}";
    let throwing_source = "function run(shouldThrow){\
        function method(){\"use strict\";\
            if(shouldThrow){throw this.value;}\
            return this.value;\
        }\
        let object={value:7,method:method};\
        return object.method();\
    }";
    let non_callable = compile(non_callable_source, "fail");
    let throwing = compile(throwing_source, "run");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let non_callable = context.instantiate(non_callable).expect("non-callable");
    let throwing = context.instantiate(throwing).expect("throwing");

    let exception =
        escaping_exception(context.call(&non_callable, &[], ExecutionLimits::default()));
    assert_engine_exception(
        &exception,
        non_callable_source,
        "not a function",
        "object.method()",
    );
    assert!(exception.caller_frames().is_empty());

    let should_throw = context.boolean(true);
    let exception =
        escaping_exception(context.call(&throwing, &[should_throw], ExecutionLimits::default()));
    assert_eq!(exception.kind(), None);
    assert_number(exception.thrown_value().expect("thrown receiver value"), 7);
    assert_eq!(exception.source_text(), throwing_source);
    let origin = exception.source_span();
    assert_eq!(
        &throwing_source[origin.start() as usize..origin.end() as usize],
        "throw this.value;"
    );
    let callers = exception.caller_frames();
    assert_eq!(callers.len(), 1);
    let caller = callers[0].source_span();
    assert_eq!(
        &throwing_source[caller.start() as usize..caller.end() as usize],
        "object.method()"
    );

    let should_return = context.boolean(false);
    let result = context
        .call(&throwing, &[should_return], ExecutionLimits::default())
        .expect("runtime remains reusable");
    assert_number(&result, 7);
}

#[test]
fn extracted_strict_function_call_receives_undefined_as_this() {
    let authority = compile(
        "function run(){\"use strict\";\
            function method(){\"use strict\";return this;}\
            let object={method:method};\
            let direct=object.method;\
            return direct();\
        }",
        "run",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let run = context.instantiate(authority).expect("run");

    let result = context
        .call(&run, &[], ExecutionLimits::default())
        .expect("strict direct call");
    assert_eq!(result.kind().expect("live value"), ValueKind::Undefined);
}

#[test]
fn returned_object_clones_share_one_logical_public_root() {
    let authority = compile("function make(){return {value:1};}", "make");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let (_make, baseline) = with_context(&mut runtime, &realm, |context| {
        let make = context.instantiate(authority).expect("make");
        let baseline = context.runtime_usage();

        let value = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("object value");
        assert_eq!(value.kind().expect("live value"), ValueKind::Object);
        let object = value.into_object().expect("object");
        let clone = object.clone();
        let generic = object.as_value();
        let round_trip = generic.clone().into_object().expect("round-trip object");

        assert!(object.same_identity(&clone).expect("same runtime"));
        assert!(
            object
                .same_identity(&round_trip)
                .expect("same runtime and object")
        );
        assert_eq!(
            context.runtime_usage().public_roots(),
            baseline.public_roots() + 1
        );
        assert_eq!(
            context.runtime_usage().heap_objects(),
            baseline.heap_objects() + 1
        );
        assert_eq!(
            context.runtime_usage().object_properties(),
            baseline.object_properties() + 1
        );

        drop(object);
        drop(generic);
        drop(round_trip);
        assert_eq!(
            context.runtime_usage().public_roots(),
            baseline.public_roots() + 1,
            "remaining Object clone keeps the one logical root alive"
        );
        drop(clone);
        assert_eq!(
            context.runtime_usage().pending_releases(),
            baseline.pending_releases() + 1
        );
        (make, baseline)
    });

    let report = runtime.collect_cycles().expect("collection");
    assert_eq!(report.objects(), 1);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn self_referential_object_is_reclaimed_after_its_public_root_drops() {
    let authority = compile(
        "function make(){let object={};object.self=object;return object;}",
        "make",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let (_make, baseline) = with_context(&mut runtime, &realm, |context| {
        let make = context.instantiate(authority).expect("make");
        let baseline = context.runtime_usage();

        let object = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("self-referential object")
            .into_object()
            .expect("object");
        assert_eq!(
            context.runtime_usage().object_properties(),
            baseline.object_properties() + 1
        );
        drop(object);
        (make, baseline)
    });

    let report = runtime.collect_cycles().expect("cycle collection");
    assert_eq!(report.objects(), 1);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn object_function_and_captured_cell_cycle_is_reclaimed() {
    let authority = compile(
        "function make(){\
            let object={};\
            function closure(){return object;}\
            object.closure=closure;\
            return object;\
        }",
        "make",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let (_make, baseline) = with_context(&mut runtime, &realm, |context| {
        let make = context.instantiate(authority).expect("make");
        let baseline = context.runtime_usage();

        let object = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("cyclic object")
            .into_object()
            .expect("object");
        let live = context.runtime_usage();
        assert_eq!(live.heap_objects(), baseline.heap_objects() + 2);
        assert_eq!(live.object_properties(), baseline.object_properties() + 5);
        assert_eq!(live.heap_functions(), baseline.heap_functions() + 1);
        assert_eq!(live.binding_cells(), baseline.binding_cells() + 1);
        drop(object);
        (make, baseline)
    });

    let report = runtime.collect_cycles().expect("cycle collection");
    assert_eq!(report.objects(), 2);
    assert_eq!(report.functions(), 1);
    assert_eq!(report.binding_cells(), 1);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn rooted_closure_keeps_an_object_alive_through_its_binding_cell() {
    let make = compile("function make(){return {answer:41};}", "make");
    let hold = compile(
        "function hold(value){\
            function read(){return value.answer;}\
            return read;\
        }",
        "hold",
    );
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let (_make, _hold, baseline, read) = with_context(&mut runtime, &realm, |context| {
        let make = context.instantiate(make).expect("make");
        let hold = context.instantiate(hold).expect("hold");
        let baseline = context.runtime_usage();

        let object = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("object")
            .into_object()
            .expect("object");
        let object_value = object.as_value();
        let read = context
            .call(&hold, &[object_value], ExecutionLimits::default())
            .expect("capturing reader")
            .into_function()
            .expect("reader");
        drop(object);
        (make, hold, baseline, read)
    });

    let report = runtime
        .collect_cycles()
        .expect("rooted reader keeps its captured object");
    assert_eq!(report.objects(), 0);
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 2);

    with_context(&mut runtime, &realm, |context| {
        let answer = context
            .call(&read, &[], ExecutionLimits::default())
            .expect("captured object remains live");
        assert_number(&answer, 41);
    });
    drop(read);

    let report = runtime.collect_cycles().expect("final collection");
    assert_eq!(report.objects(), 2);
    assert_eq!(report.functions(), 1);
    assert_eq!(report.binding_cells(), 1);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn thrown_object_remains_rooted_until_the_last_exception_value_clone_drops() {
    let fail = compile("function fail(){throw {answer:17};}", "fail");
    let read = compile("function read(value){return value.answer;}", "read");
    let mut runtime = runtime(RuntimeLimits::default());
    let realm = runtime.create_realm().expect("realm");
    let (_fail, _read, baseline) = with_context(&mut runtime, &realm, |context| {
        let fail = context.instantiate(fail).expect("fail");
        let read = context.instantiate(read).expect("read");
        let baseline = context.runtime_usage();

        let exception = escaping_exception(context.call(&fail, &[], ExecutionLimits::default()));
        let thrown = exception.thrown_value().expect("object payload");
        assert_eq!(thrown.kind().expect("live payload"), ValueKind::Object);
        let object = thrown.clone().into_object().expect("object payload");
        let clone = object.clone();
        assert!(object.same_identity(&clone).expect("same runtime"));
        assert_eq!(
            context.runtime_usage().public_roots(),
            baseline.public_roots() + 1
        );

        drop(exception);
        let object_value = object.as_value();
        let answer = context
            .call(&read, &[object_value], ExecutionLimits::default())
            .expect("exception clone keeps object live");
        assert_number(&answer, 17);
        drop(answer);
        drop(object);
        assert_eq!(
            context.runtime_usage().public_roots(),
            baseline.public_roots() + 1
        );
        drop(clone);
        assert_eq!(
            context.runtime_usage().pending_releases(),
            baseline.pending_releases() + 1
        );
        (fail, read, baseline)
    });

    let report = runtime.collect_cycles().expect("collection");
    assert_eq!(report.objects(), 1);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn aggregate_object_limit_failure_is_atomic_and_runtime_is_reusable() {
    let authority = compile("function make(){return {};}", "make");
    let mut runtime = runtime(RuntimeLimits::default().with_max_heap_objects(41));
    let realm = runtime.create_realm().expect("realm");
    let (_make, baseline) = with_context(&mut runtime, &realm, |context| {
        let make = context.instantiate(authority).expect("make");
        let baseline = context.runtime_usage();

        let first = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("first object")
            .into_object()
            .expect("object");
        let before_failure = context.runtime_usage();
        assert!(matches!(
            context.call(&make, &[], ExecutionLimits::default()),
            Err(ExecutionError::LimitExceeded {
                resource: RuntimeResource::HeapObjects,
                limit: 41,
                observed: 42,
            })
        ));
        assert_eq!(
            context.runtime_usage(),
            before_failure,
            "failed object allocation must not mutate usage"
        );

        drop(first);
        let replacement = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("dropped root is reclaimed before retry")
            .into_object()
            .expect("replacement object");
        assert_eq!(
            context.runtime_usage().heap_objects(),
            baseline.heap_objects() + 1
        );
        drop(replacement);
        (make, baseline)
    });

    runtime.collect_cycles().expect("final collection");
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn aggregate_property_limit_failure_is_atomic_and_runtime_is_reusable() {
    let authority = compile("function make(){return {value:1};}", "make");
    let mut runtime = runtime(RuntimeLimits::default().with_max_object_properties(1_092));
    let realm = runtime.create_realm().expect("realm");
    let (make, first, baseline, before_failure) = with_context(&mut runtime, &realm, |context| {
        let make = context.instantiate(authority).expect("make");
        let baseline = context.runtime_usage();

        let first = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("first property")
            .into_object()
            .expect("object");
        let before_failure = context.runtime_usage();
        assert!(matches!(
            context.call(&make, &[], ExecutionLimits::default()),
            Err(ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: 1_092,
                observed: 1_093,
            })
        ));
        assert_eq!(
            context.runtime_usage().object_properties(),
            before_failure.object_properties(),
            "failed property insertion must not charge a slot"
        );
        assert_eq!(
            context.runtime_usage().public_roots(),
            before_failure.public_roots(),
            "failed object publication must not create a public root"
        );
        (make, first, baseline, before_failure)
    });

    runtime
        .collect_cycles()
        .expect("failed literal remains collectable");
    assert_eq!(runtime.usage(), before_failure);

    drop(first);
    with_context(&mut runtime, &realm, |context| {
        let replacement = context
            .call(&make, &[], ExecutionLimits::default())
            .expect("property budget is reusable after collection")
            .into_object()
            .expect("replacement object");
        assert_eq!(
            context.runtime_usage().object_properties(),
            baseline.object_properties() + 1
        );
        drop(replacement);
    });

    runtime.collect_cycles().expect("final collection");
    assert_eq!(runtime.usage(), baseline);
}
