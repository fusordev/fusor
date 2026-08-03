/*
 * JavaScript runtime and closure ownership derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Runtime unit tests kept in the runtime module's private visibility boundary.

use super::{
    ArrayDefineOutcome, ArrayLengthWriteOutcome, CollectionRoot, ErrorIntrinsicKind, ForInAdvance,
    FunctionImplementation, HeapFunction, KeyPhases, NativeFunction, NativeFunctionKind,
    RealmIntrinsics, RootEnvironment, Runtime, RuntimeLimits, RuntimeUsage,
    array_length_from_number, dynamic_function_declaration_property_layout,
    global_function_replacement_layout, is_supported_opcode, usize_to_u64,
};

#[test]
fn array_from_is_admitted_by_whole_graph_runtime_preflight() {
    assert!(is_supported_opcode(
        quickjs_bytecode::FinalOpcode::ArrayFrom
    ));
}

#[test]
fn array_spread_opcodes_are_admitted_without_public_iterator_markers() {
    use quickjs_bytecode::FinalOpcode;

    assert!(is_supported_opcode(FinalOpcode::Append));
    assert!(is_supported_opcode(FinalOpcode::Dup1));
    assert!(is_supported_opcode(FinalOpcode::Rot3r));
    assert!(is_supported_opcode(FinalOpcode::ForOfStart));
    assert!(is_supported_opcode(FinalOpcode::ForOfNext));
    assert!(is_supported_opcode(FinalOpcode::IteratorClose));
    assert!(!is_supported_opcode(FinalOpcode::IteratorNext));
    assert!(!is_supported_opcode(FinalOpcode::ForAwaitOfStart));
}

#[test]
fn realm_installs_a_rooted_branded_array_prototype_with_exact_length() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_array_prototype(realm_id)
        .expect("Array.prototype");
    let state = runtime.realms.get(realm_id).expect("realm state");
    let object = runtime.objects.get(prototype).expect("Array.prototype");

    assert_eq!(object.array_state().map(|array| array.length()), Some(0));
    assert_eq!(
        object.record.prototype(),
        Some(HeapReference::Object(state.object_prototype))
    );
    assert!(matches!(
        object.record.own_property(
            &runtime.predefined_property_key(PredefinedAtom::Length)
        ),
        Some(OwnProperty::Data {
            layout,
            value: StoredValue::Number(value),
        }) if layout == PropertyLayout::data(true, false, false)
            && value.strict_equals(JsNumber::from_i32(0))
    ));
    assert_eq!(runtime.usage().heap_objects(), 20);
    assert_eq!(runtime.usage().object_properties(), 437);

    assert_eq!(runtime.collect_cycles().expect("collection").objects(), 0);
    assert!(runtime.objects.contains(prototype));
}

#[test]
fn realm_installs_a_realm_owned_array_constructor_with_exact_descriptors() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let state = runtime.realms.get(realm_id).expect("realm state");
    let RealmIntrinsics::Ready {
        function_prototype,
        array,
        ..
    } = state.intrinsics
    else {
        panic!("realm intrinsics remained uninitialized");
    };

    let prototype = runtime
        .objects
        .get(array.prototype)
        .expect("Array.prototype");
    assert_data_property(
        &prototype.record,
        &runtime,
        PredefinedAtom::Constructor,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == array.constructor),
    );

    let constructor = runtime
        .functions
        .get(array.constructor)
        .expect("Array constructor");
    assert_eq!(
        constructor.object.prototype(),
        Some(HeapReference::Function(function_prototype))
    );
    let native = constructor.native().expect("native Array constructor");
    assert_eq!(native.realm, realm_id);
    assert_eq!(native.kind, NativeFunctionKind::ArrayConstructor);
    assert!(native.kind.is_constructor());
    assert_data_property(
        &constructor.object,
        &runtime,
        PredefinedAtom::Prototype,
        PropertyLayout::data(false, false, false),
        |value| matches!(value, StoredValue::Object(id) if id == array.prototype),
    );
    assert_data_property(
        &constructor.object,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(1))),
    );
    assert_data_property(
        &constructor.object,
        &runtime,
        PredefinedAtom::Name,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::String(name) if name == JsString::from_utf8("Array").expect("name")),
    );

    let global = runtime
        .objects
        .get(state.global_object)
        .expect("global object");
    assert_data_property(
        &global.record,
        &runtime,
        PredefinedAtom::Array,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == array.constructor),
    );
    assert_eq!(runtime.usage().heap_objects(), 20);
    assert_eq!(runtime.usage().heap_functions(), 131);
    assert_eq!(runtime.usage().object_properties(), 437);
}

#[test]
fn array_intrinsics_are_realm_unique_and_direct_collection_roots() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let first = runtime.create_realm().expect("first realm");
    let second = runtime.create_realm().expect("second realm");
    let first_state = runtime.realms.get(first.0.id).expect("first realm");
    let first_global = first_state.global_object;
    let first_array = match first_state.intrinsics {
        RealmIntrinsics::Ready { array, .. } => array,
        RealmIntrinsics::Initializing => {
            panic!("first realm intrinsics remained uninitialized")
        }
    };
    let second_array = match runtime
        .realms
        .get(second.0.id)
        .expect("second realm")
        .intrinsics
    {
        RealmIntrinsics::Ready { array, .. } => array,
        RealmIntrinsics::Initializing => {
            panic!("second realm intrinsics remained uninitialized")
        }
    };
    assert_ne!(first_array.prototype, second_array.prototype);
    assert_ne!(first_array.constructor, second_array.constructor);

    let array_key = runtime.predefined_property_key(PredefinedAtom::Array);
    let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    assert!(
        runtime
            .objects
            .get_mut(first_global)
            .expect("first global")
            .record
            .replace_existing_data(&array_key, StoredValue::Undefined)
    );
    assert!(
        runtime
            .objects
            .get_mut(first_array.prototype)
            .expect("first Array.prototype")
            .record
            .replace_existing_data(&constructor_key, StoredValue::Undefined)
    );

    let report = runtime.collect_cycles().expect("collection");

    assert_eq!(report.functions(), 0);
    assert!(runtime.functions.contains(first_array.constructor));
    assert!(runtime.objects.contains(first_array.prototype));
    assert!(runtime.functions.contains(second_array.constructor));
    assert!(runtime.objects.contains(second_array.prototype));
}

#[test]
fn sparse_array_allocation_is_constant_size_and_exactly_charged() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let prototype = runtime
        .realm_array_prototype(realm.0.id)
        .expect("Array.prototype");
    let baseline = runtime.usage();

    for (offset, length) in [0, 3, u32::MAX].into_iter().enumerate() {
        let array = runtime
            .allocate_sparse_array_with_prototype(HeapReference::Object(prototype), length)
            .expect("sparse array");
        let object = runtime.objects.get(array).expect("sparse array object");

        assert_eq!(
            runtime.array_length(array).expect("array length"),
            Some(length)
        );
        assert_eq!(object.record.property_count(), 1);
        assert_eq!(
            object.record.prototype(),
            Some(HeapReference::Object(prototype))
        );
        assert!(
            runtime
                .array_own_property(
                    array,
                    &PropertyKey::from_index(ArrayIndex::new(0).expect("index")),
                )
                .expect("array property")
                .is_none()
        );
        assert_data_property(
            &object.record,
            &runtime,
            PredefinedAtom::Length,
            PropertyLayout::data(true, false, false),
            |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_f64(f64::from(length)))),
        );
        assert_eq!(
            runtime.usage().heap_objects(),
            baseline.heap_objects() + usize_to_u64(offset + 1)
        );
        assert_eq!(
            runtime.usage().object_properties(),
            baseline.object_properties() + usize_to_u64(offset + 1)
        );
    }
}

#[test]
fn sparse_array_allocation_preflights_each_resource_before_mutation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let prototype = runtime
        .realm_array_prototype(realm.0.id)
        .expect("Array.prototype");
    let baseline = runtime.usage();
    let collection_pending = runtime.collection_pending;

    runtime.limits.max_heap_objects = baseline.heap_objects();
    assert!(matches!(
        runtime.allocate_sparse_array_with_prototype(
            HeapReference::Object(prototype),
            u32::MAX,
        ),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::HeapObjects,
            limit,
            observed,
        }) if limit == baseline.heap_objects()
            && observed == baseline.heap_objects() + 1
    ));
    assert_eq!(runtime.usage(), baseline);
    assert_eq!(runtime.collection_pending, collection_pending);

    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;
    runtime.limits.max_object_properties = baseline.object_properties();
    assert!(matches!(
        runtime.allocate_sparse_array_with_prototype(
            HeapReference::Object(prototype),
            u32::MAX,
        ),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit,
            observed,
        }) if limit == baseline.object_properties()
            && observed == baseline.object_properties() + 1
    ));
    assert_eq!(runtime.usage(), baseline);
    assert_eq!(runtime.collection_pending, collection_pending);
}

#[test]
fn dense_array_allocation_is_exactly_charged_and_traces_elements() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let child = runtime
        .allocate_ordinary_object(runtime.realm_object_prototype(realm_id).expect("prototype"))
        .expect("child");
    let baseline = runtime.usage();
    let array = runtime
        .allocate_array(
            realm_id,
            vec![StoredValue::Object(child), StoredValue::Boolean(true)],
        )
        .expect("array");

    assert_eq!(runtime.array_length(array).expect("array length"), Some(2));
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
    assert_eq!(
        runtime.usage().object_properties(),
        baseline.object_properties() + 3,
        "length plus two dense indices are charged"
    );
    for index in 0..2 {
        assert!(matches!(
            runtime
                .array_own_property(
                    array,
                    &PropertyKey::from_index(ArrayIndex::new(index).expect("index")),
                )
                .expect("array property"),
            Some(OwnProperty::Data { layout, .. })
                if layout == PropertyLayout::data(true, true, true)
        ));
    }
    let snapshot = runtime
        .objects
        .get(array)
        .expect("array")
        .record
        .try_own_key_snapshot(None, KeyPhases::FOR_IN)
        .expect("for-in snapshot");
    let enumerable_indices = (0..snapshot.len())
        .filter_map(|position| {
            let candidate = snapshot.get(position).expect("candidate");
            if candidate.enumerable() {
                candidate.key().as_index().map(ArrayIndex::get)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        enumerable_indices,
        vec![0, 1],
        "the non-enumerable synthetic length slot is never yielded"
    );

    let report = runtime
        .collect_cycles_with_roots(|trace| {
            trace(CollectionRoot::Heap(HeapReference::Object(array)));
        })
        .expect("rooted array collection");
    assert_eq!(report.objects(), 0, "the array edge keeps its child live");
    let report = runtime.collect_cycles().expect("unrooted collection");
    assert_eq!(
        report.objects(),
        2,
        "the array and child are reclaimed together"
    );
}

#[test]
fn array_allocation_and_index_extension_preflight_before_mutation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let baseline = runtime.usage();
    runtime.limits.max_object_properties = baseline.object_properties() + 2;

    assert!(matches!(
        runtime.allocate_array(
            realm_id,
            vec![StoredValue::Undefined, StoredValue::Undefined],
        ),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit,
            observed,
        }) if limit == baseline.object_properties() + 2
            && observed == baseline.object_properties() + 3
    ));
    assert_eq!(runtime.usage(), baseline);

    runtime.limits.max_object_properties = baseline.object_properties() + 1;
    let array = runtime
        .allocate_array(realm_id, Vec::new())
        .expect("empty array");
    let before_definition = runtime.usage();
    assert!(matches!(
        runtime.define_array_data_property(
            array,
            PropertyKey::from_index(ArrayIndex::new(4).expect("index")),
            PropertyLayout::data(true, true, true),
            StoredValue::Boolean(true),
        ),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit,
            observed,
        }) if limit == before_definition.object_properties()
            && observed == before_definition.object_properties() + 1
    ));
    assert_eq!(runtime.usage(), before_definition);
    assert_eq!(runtime.array_length(array).expect("length"), Some(0));
}

#[test]
fn canonical_array_indices_extend_length_but_u32_max_does_not() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let array = runtime.allocate_array(realm_id, Vec::new()).expect("array");

    assert_eq!(
        runtime
            .define_array_data_property(
                array,
                PropertyKey::from_index(ArrayIndex::new(4).expect("index")),
                PropertyLayout::data(true, true, true),
                StoredValue::Boolean(true),
            )
            .expect("index definition"),
        ArrayDefineOutcome::Complete
    );
    assert_eq!(runtime.array_length(array).expect("length"), Some(5));

    let max_u32 = runtime
        .property_key_from_string(&JsString::from_utf8("4294967295").expect("key"))
        .expect("property key");
    assert!(max_u32.as_index().is_none());
    assert_eq!(
        runtime
            .define_array_data_property(
                array,
                max_u32,
                PropertyLayout::data(true, true, true),
                StoredValue::Boolean(false),
            )
            .expect("ordinary property definition"),
        ArrayDefineOutcome::Complete
    );
    assert_eq!(runtime.array_length(array).expect("length"), Some(5));
}

#[test]
fn array_length_number_validation_matches_the_uint32_domain_exactly() {
    for (value, expected) in [
        (-0.0, Some(0)),
        (0.0, Some(0)),
        (1.0, Some(1)),
        (f64::from(u32::MAX), Some(u32::MAX)),
        (-1.0, None),
        (1.5, None),
        (f64::from(u32::MAX) + 1.0, None),
        (f64::INFINITY, None),
        (f64::NEG_INFINITY, None),
        (f64::NAN, None),
    ] {
        assert_eq!(
            array_length_from_number(JsNumber::from_f64(value)),
            expected
        );
    }
}

#[test]
fn shrinking_array_length_deletes_indices_and_reports_a_nonconfigurable_blocker() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let array = runtime.allocate_array(realm_id, Vec::new()).expect("array");
    for (index, configurable) in [(1, true), (3, false), (5, true)] {
        assert_eq!(
            runtime
                .define_array_data_property(
                    array,
                    PropertyKey::from_index(ArrayIndex::new(index).expect("index")),
                    PropertyLayout::data(true, true, configurable),
                    StoredValue::Number(JsNumber::from_i32(
                        i32::try_from(index).expect("small fixture index"),
                    )),
                )
                .expect("definition"),
            ArrayDefineOutcome::Complete
        );
    }
    let before = runtime.usage();
    assert_eq!(
        runtime
            .preview_array_length_write_work(array, 1)
            .expect("work preview"),
        20,
        "four exact shape slots produce a conservative four-pass bound"
    );

    assert_eq!(
        runtime.set_array_length(array, 1).expect("length write"),
        ArrayLengthWriteOutcome::BlockedByNonConfigurable {
            index: ArrayIndex::new(3).expect("index"),
            final_length: 4,
        }
    );
    assert_eq!(runtime.array_length(array).expect("length"), Some(4));
    assert_eq!(
        runtime.usage().object_properties(),
        before.object_properties() - 1
    );
    assert!(
        runtime
            .array_own_property(
                array,
                &PropertyKey::from_index(ArrayIndex::new(5).expect("index"))
            )
            .expect("property")
            .is_none()
    );
}
use crate::{
    ArrayIndex, AtomError, AtomLimits, AtomUsage, EngineFault, ExceptionKind, ExecutionError,
    JsNumber, JsString, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
    PREDEFINED_INTERNER_SLOTS, PredefinedAtom, PropertyKey, PropertyLayout, RuntimeError,
    RuntimeResource,
    object::{ObjectRecord, OwnProperty},
    value::{HeapReference, StoredValue},
};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one test audits the complete intrinsic graph and all exact descriptors"
)]
fn realm_installs_the_exact_function_intrinsic_graph() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let call_name = JsString::from_utf8("call").expect("call");
    let call_key = runtime
        .atoms
        .property_key_from_string(&call_name)
        .expect("call key");
    let state = runtime.realms.get(realm_id).expect("realm state");
    let RealmIntrinsics::Ready {
        function_prototype,
        function_constructor,
        errors: _,
        boolean,
        number,
        bigint: _,
        string,
        array: _,
        symbol: _,
        iterators: _,
    } = state.intrinsics
    else {
        panic!("realm intrinsics remained uninitialized");
    };

    assert_eq!(runtime.usage().realms(), 1);
    assert_eq!(runtime.usage().heap_objects(), 20);
    assert_eq!(runtime.usage().heap_functions(), 131);
    assert_eq!(runtime.usage().object_properties(), 437);
    assert_eq!(runtime.usage().installed_code(), 0);
    assert_eq!(
        runtime.atom_usage(),
        AtomUsage {
            live_atoms: PREDEFINED_ATOM_COUNT + 91,
            live_description_code_units: PREDEFINED_DESCRIPTION_CODE_UNITS + 776,
            interner_slots: PREDEFINED_INTERNER_SLOTS + 91,
        }
    );

    let prototype = runtime
        .functions
        .get(function_prototype)
        .expect("Function.prototype");
    assert_eq!(
        prototype.object.prototype(),
        Some(HeapReference::Object(state.object_prototype))
    );
    assert!(matches!(
        prototype.implementation,
        FunctionImplementation::Native(ref native)
            if native.realm == realm_id
                && native.kind == NativeFunctionKind::FunctionPrototype
    ));

    let constructor = runtime
        .functions
        .get(function_constructor)
        .expect("Function");
    assert_eq!(
        constructor.object.prototype(),
        Some(HeapReference::Function(function_prototype))
    );
    assert!(matches!(
        constructor.implementation,
        FunctionImplementation::Native(ref native)
            if native.realm == realm_id
                && native.kind == NativeFunctionKind::OrdinaryFunctionConstructor
    ));

    assert_data_property(
        &prototype.object,
        &runtime,
        PredefinedAtom::Constructor,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == function_constructor),
    );
    assert_data_property(
        &prototype.object,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(0))),
    );
    assert_data_property(
        &prototype.object,
        &runtime,
        PredefinedAtom::Name,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::String(name) if name == JsString::empty()),
    );
    let function_to_string = function_property(
        &prototype.object,
        &runtime,
        PredefinedAtom::ToString,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        function_to_string,
        function_prototype,
        realm_id,
        NativeFunctionKind::FunctionPrototypeToString,
        PredefinedAtom::ToString,
        0,
    );
    let (call_layout, call_value) = prototype
        .object
        .own_data_property(&call_key)
        .expect("Function.prototype.call");
    assert_eq!(
        call_layout,
        PropertyLayout::data(true, false, true),
        "Function.prototype.call descriptor"
    );
    let StoredValue::Function(function_call) = call_value else {
        panic!("Function.prototype.call is not a function");
    };
    assert_native_method_named(
        &runtime,
        function_call,
        function_prototype,
        realm_id,
        NativeFunctionKind::FunctionPrototypeCall,
        &call_name,
        1,
    );
    let call_native = runtime
        .functions
        .get(function_call)
        .and_then(HeapFunction::native)
        .expect("native Function.prototype.call");
    assert!(!call_native.kind.is_constructor());
    assert!(
        !has_own_property(
            &runtime
                .functions
                .get(function_call)
                .expect("Function.prototype.call")
                .object,
            &runtime,
            PredefinedAtom::Prototype,
        ),
        "Function.prototype.call must not have an own prototype"
    );
    let function_apply = function_property(
        &prototype.object,
        &runtime,
        PredefinedAtom::Apply,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        function_apply,
        function_prototype,
        realm_id,
        NativeFunctionKind::FunctionPrototypeApply,
        PredefinedAtom::Apply,
        2,
    );
    let apply_native = runtime
        .functions
        .get(function_apply)
        .and_then(HeapFunction::native)
        .expect("native Function.prototype.apply");
    assert!(!apply_native.kind.is_constructor());
    assert!(
        !has_own_property(
            &runtime
                .functions
                .get(function_apply)
                .expect("Function.prototype.apply")
                .object,
            &runtime,
            PredefinedAtom::Prototype,
        ),
        "Function.prototype.apply must not have an own prototype"
    );

    let object_prototype = &runtime
        .objects
        .get(state.object_prototype)
        .expect("Object.prototype")
        .record;
    let object_to_string = function_property(
        object_prototype,
        &runtime,
        PredefinedAtom::ToString,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        object_to_string,
        function_prototype,
        realm_id,
        NativeFunctionKind::ObjectPrototypeToString,
        PredefinedAtom::ToString,
        0,
    );
    let object_value_of = function_property(
        object_prototype,
        &runtime,
        PredefinedAtom::ValueOf,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        object_value_of,
        function_prototype,
        realm_id,
        NativeFunctionKind::ObjectPrototypeValueOf,
        PredefinedAtom::ValueOf,
        0,
    );

    assert_data_property(
        &constructor.object,
        &runtime,
        PredefinedAtom::Prototype,
        PropertyLayout::data(false, false, false),
        |value| matches!(value, StoredValue::Function(id) if id == function_prototype),
    );
    assert_data_property(
        &constructor.object,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(1))),
    );
    assert_data_property(
        &constructor.object,
        &runtime,
        PredefinedAtom::Name,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::String(name) if name == JsString::from_utf8("Function").expect("name")),
    );

    let boolean_prototype = runtime
        .objects
        .get(boolean.prototype)
        .expect("Boolean.prototype");
    assert_eq!(
        boolean_prototype.record.prototype(),
        Some(HeapReference::Object(state.object_prototype))
    );
    assert_eq!(
        boolean_prototype
            .boxed_primitive()
            .and_then(crate::object::BoxedPrimitive::as_boolean),
        Some(false),
        "Boolean.prototype carries the false Boolean internal slot"
    );
    assert_eq!(
        runtime
            .realm_boolean_prototype(realm_id)
            .expect("Boolean.prototype intrinsic"),
        boolean.prototype
    );
    assert_data_property(
        &boolean_prototype.record,
        &runtime,
        PredefinedAtom::Constructor,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == boolean.constructor),
    );
    let boolean_to_string = function_property(
        &boolean_prototype.record,
        &runtime,
        PredefinedAtom::ToString,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        boolean_to_string,
        function_prototype,
        realm_id,
        NativeFunctionKind::BooleanPrototypeToString,
        PredefinedAtom::ToString,
        0,
    );
    let boolean_value_of = function_property(
        &boolean_prototype.record,
        &runtime,
        PredefinedAtom::ValueOf,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        boolean_value_of,
        function_prototype,
        realm_id,
        NativeFunctionKind::BooleanPrototypeValueOf,
        PredefinedAtom::ValueOf,
        0,
    );
    for method in [boolean_to_string, boolean_value_of] {
        let node = runtime
            .functions
            .get(method)
            .expect("Boolean prototype method");
        assert!(
            !node
                .native()
                .expect("native Boolean method")
                .kind
                .is_constructor(),
            "Boolean prototype methods must not be constructors"
        );
        assert!(
            !has_own_property(&node.object, &runtime, PredefinedAtom::Prototype),
            "Boolean prototype methods must not have an own prototype"
        );
    }

    let boolean_constructor = runtime.functions.get(boolean.constructor).expect("Boolean");
    assert_eq!(
        boolean_constructor.object.prototype(),
        Some(HeapReference::Function(function_prototype))
    );
    let boolean_native = boolean_constructor.native().expect("native Boolean");
    assert_eq!(boolean_native.realm, realm_id);
    assert_eq!(boolean_native.kind, NativeFunctionKind::BooleanConstructor);
    assert!(boolean_native.kind.is_constructor());
    assert_data_property(
        &boolean_constructor.object,
        &runtime,
        PredefinedAtom::Prototype,
        PropertyLayout::data(false, false, false),
        |value| matches!(value, StoredValue::Object(id) if id == boolean.prototype),
    );
    assert_data_property(
        &boolean_constructor.object,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(1))),
    );
    assert_data_property(
        &boolean_constructor.object,
        &runtime,
        PredefinedAtom::Name,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::String(name) if name == JsString::from_utf8("Boolean").expect("name")),
    );

    let number_prototype = runtime
        .objects
        .get(number.prototype)
        .expect("Number.prototype");
    assert_eq!(
        number_prototype.record.prototype(),
        Some(HeapReference::Object(state.object_prototype))
    );
    assert!(
        number_prototype
            .boxed_primitive()
            .and_then(crate::object::BoxedPrimitive::as_number)
            .is_some_and(|value| value.same_value(JsNumber::from_i32(0))),
        "Number.prototype carries the positive-zero Number internal slot"
    );
    assert_eq!(
        runtime
            .realm_number_prototype(realm_id)
            .expect("Number.prototype intrinsic"),
        number.prototype
    );
    assert_data_property(
        &number_prototype.record,
        &runtime,
        PredefinedAtom::Constructor,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == number.constructor),
    );
    let number_to_string = function_property(
        &number_prototype.record,
        &runtime,
        PredefinedAtom::ToString,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        number_to_string,
        function_prototype,
        realm_id,
        NativeFunctionKind::NumberPrototypeToString,
        PredefinedAtom::ToString,
        1,
    );
    let number_value_of = function_property(
        &number_prototype.record,
        &runtime,
        PredefinedAtom::ValueOf,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        number_value_of,
        function_prototype,
        realm_id,
        NativeFunctionKind::NumberPrototypeValueOf,
        PredefinedAtom::ValueOf,
        0,
    );
    for method in [number_to_string, number_value_of] {
        let node = runtime
            .functions
            .get(method)
            .expect("Number prototype method");
        assert!(
            !node
                .native()
                .expect("native Number method")
                .kind
                .is_constructor(),
            "Number prototype methods must not be constructors"
        );
        assert!(
            !has_own_property(&node.object, &runtime, PredefinedAtom::Prototype),
            "Number prototype methods must not have an own prototype"
        );
    }

    let number_constructor = runtime.functions.get(number.constructor).expect("Number");
    assert_eq!(
        number_constructor.object.prototype(),
        Some(HeapReference::Function(function_prototype))
    );
    let number_native = number_constructor.native().expect("native Number");
    assert_eq!(number_native.realm, realm_id);
    assert_eq!(number_native.kind, NativeFunctionKind::NumberConstructor);
    assert!(number_native.kind.is_constructor());
    assert_data_property(
        &number_constructor.object,
        &runtime,
        PredefinedAtom::Prototype,
        PropertyLayout::data(false, false, false),
        |value| matches!(value, StoredValue::Object(id) if id == number.prototype),
    );
    assert_data_property(
        &number_constructor.object,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(1))),
    );
    assert_data_property(
        &number_constructor.object,
        &runtime,
        PredefinedAtom::Name,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::String(name) if name == JsString::from_utf8("Number").expect("name")),
    );

    let string_prototype = runtime
        .objects
        .get(string.prototype)
        .expect("String.prototype");
    assert_eq!(
        string_prototype.record.prototype(),
        Some(HeapReference::Object(state.object_prototype))
    );
    assert!(
        string_prototype
            .boxed_primitive()
            .and_then(crate::object::BoxedPrimitive::as_string)
            .is_some_and(JsString::is_empty),
        "String.prototype carries the empty String internal slot"
    );
    assert_eq!(
        runtime
            .realm_string_prototype(realm_id)
            .expect("String.prototype intrinsic"),
        string.prototype
    );
    assert_data_property(
        &string_prototype.record,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(0))),
    );
    assert_data_property(
        &string_prototype.record,
        &runtime,
        PredefinedAtom::Constructor,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == string.constructor),
    );
    let string_to_string = function_property(
        &string_prototype.record,
        &runtime,
        PredefinedAtom::ToString,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        string_to_string,
        function_prototype,
        realm_id,
        NativeFunctionKind::StringPrototypeToString,
        PredefinedAtom::ToString,
        0,
    );
    let string_value_of = function_property(
        &string_prototype.record,
        &runtime,
        PredefinedAtom::ValueOf,
        PropertyLayout::data(true, false, true),
    );
    assert_native_method(
        &runtime,
        string_value_of,
        function_prototype,
        realm_id,
        NativeFunctionKind::StringPrototypeValueOf,
        PredefinedAtom::ValueOf,
        0,
    );
    for method in [string_to_string, string_value_of] {
        let node = runtime
            .functions
            .get(method)
            .expect("String prototype method");
        assert!(
            !node
                .native()
                .expect("native String method")
                .kind
                .is_constructor(),
            "String prototype methods must not be constructors"
        );
        assert!(
            !has_own_property(&node.object, &runtime, PredefinedAtom::Prototype),
            "String prototype methods must not have an own prototype"
        );
    }

    let string_constructor = runtime.functions.get(string.constructor).expect("String");
    assert_eq!(
        string_constructor.object.prototype(),
        Some(HeapReference::Function(function_prototype))
    );
    let string_native = string_constructor.native().expect("native String");
    assert_eq!(string_native.realm, realm_id);
    assert_eq!(string_native.kind, NativeFunctionKind::StringConstructor);
    assert!(string_native.kind.is_constructor());
    assert_data_property(
        &string_constructor.object,
        &runtime,
        PredefinedAtom::Prototype,
        PropertyLayout::data(false, false, false),
        |value| matches!(value, StoredValue::Object(id) if id == string.prototype),
    );
    assert_data_property(
        &string_constructor.object,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(1))),
    );
    assert_data_property(
        &string_constructor.object,
        &runtime,
        PredefinedAtom::Name,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::String(name) if name == JsString::from_utf8("String").expect("name")),
    );

    let global = runtime
        .objects
        .get(state.global_object)
        .expect("global object");
    assert_eq!(
        global.record.prototype(),
        Some(HeapReference::Object(state.object_prototype))
    );
    assert_data_property(
        &global.record,
        &runtime,
        PredefinedAtom::Function,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == function_constructor),
    );
    assert_data_property(
        &global.record,
        &runtime,
        PredefinedAtom::Boolean,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == boolean.constructor),
    );
    assert_data_property(
        &global.record,
        &runtime,
        PredefinedAtom::Number,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == number.constructor),
    );
    assert_data_property(
        &global.record,
        &runtime,
        PredefinedAtom::String,
        PropertyLayout::data(true, false, true),
        |value| matches!(value, StoredValue::Function(id) if id == string.constructor),
    );
}

#[test]
fn function_call_is_realm_owned_while_its_dynamic_atom_is_reused() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let first = runtime.create_realm().expect("first realm");
    let second = runtime.create_realm().expect("second realm");
    let call_name = JsString::from_utf8("call").expect("call");
    let call_key = runtime
        .atoms
        .property_key_from_string(&call_name)
        .expect("call key");
    let mut calls = Vec::new();
    for realm in [first.0.id, second.0.id] {
        let RealmIntrinsics::Ready {
            function_prototype, ..
        } = runtime.realms.get(realm).expect("realm").intrinsics
        else {
            panic!("realm intrinsics remained uninitialized");
        };
        let call = function_property_by_key(
            &runtime
                .functions
                .get(function_prototype)
                .expect("Function.prototype")
                .object,
            &call_key,
            PropertyLayout::data(true, false, true),
        );
        let node = runtime.functions.get(call).expect("call");
        assert_eq!(
            node.object.prototype(),
            Some(HeapReference::Function(function_prototype))
        );
        assert!(matches!(
            node.implementation,
            FunctionImplementation::Native(ref native)
                if native.realm == realm
                    && native.kind == NativeFunctionKind::FunctionPrototypeCall
        ));
        calls.push(call);
    }

    assert_ne!(calls[0], calls[1]);
    assert_eq!(
        runtime.atom_usage(),
        AtomUsage {
            live_atoms: PREDEFINED_ATOM_COUNT + 91,
            live_description_code_units: PREDEFINED_DESCRIPTION_CODE_UNITS + 776,
            interner_slots: PREDEFINED_INTERNER_SLOTS + 91,
        }
    );
}

#[test]
fn function_apply_is_realm_owned_while_its_predefined_atom_is_reused() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let first = runtime.create_realm().expect("first realm");
    let second = runtime.create_realm().expect("second realm");
    let apply_key =
        PropertyKey::from_validated_atom(runtime.atoms.predefined(PredefinedAtom::Apply));
    let mut applies = Vec::new();
    for realm in [first.0.id, second.0.id] {
        let RealmIntrinsics::Ready {
            function_prototype, ..
        } = runtime.realms.get(realm).expect("realm").intrinsics
        else {
            panic!("realm intrinsics remained uninitialized");
        };
        let apply = function_property_by_key(
            &runtime
                .functions
                .get(function_prototype)
                .expect("Function.prototype")
                .object,
            &apply_key,
            PropertyLayout::data(true, false, true),
        );
        let node = runtime.functions.get(apply).expect("apply");
        assert_eq!(
            node.object.prototype(),
            Some(HeapReference::Function(function_prototype))
        );
        assert!(matches!(
            node.implementation,
            FunctionImplementation::Native(ref native)
                if native.realm == realm
                    && native.kind == NativeFunctionKind::FunctionPrototypeApply
        ));
        applies.push(apply);
    }

    assert_ne!(applies[0], applies[1]);
    assert_eq!(
        runtime.atom_usage(),
        AtomUsage {
            live_atoms: PREDEFINED_ATOM_COUNT + 91,
            live_description_code_units: PREDEFINED_DESCRIPTION_CODE_UNITS + 776,
            interner_slots: PREDEFINED_INTERNER_SLOTS + 91,
        }
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table-driven invariant test audits the complete realm-owned Error intrinsic graph"
)]
fn realm_installs_complete_realm_owned_error_intrinsic_graph() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let (object_prototype, global_object, function_prototype, errors) = {
        let state = runtime.realms.get(realm_id).expect("realm state");
        let RealmIntrinsics::Ready {
            function_prototype,
            errors,
            ..
        } = state.intrinsics
        else {
            panic!("realm intrinsics remained uninitialized");
        };
        (
            state.object_prototype,
            state.global_object,
            function_prototype,
            errors,
        )
    };
    let is_error_key = runtime
        .property_key_from_string(&JsString::from_utf8("isError").expect("name"))
        .expect("isError key");

    assert_eq!(runtime.usage().heap_objects(), 20);
    assert_eq!(runtime.usage().heap_functions(), 131);
    assert_eq!(runtime.usage().object_properties(), 437);

    assert_native_method_named(
        &runtime,
        errors.to_string,
        function_prototype,
        realm_id,
        NativeFunctionKind::ErrorPrototypeToString,
        &JsString::from_utf8("toString").expect("name"),
        0,
    );
    assert_native_method_named(
        &runtime,
        errors.is_error,
        function_prototype,
        realm_id,
        NativeFunctionKind::ErrorIsError,
        &JsString::from_utf8("isError").expect("name"),
        1,
    );

    let error_constructor = errors.intrinsic(ErrorIntrinsicKind::Error).constructor;
    for kind in ErrorIntrinsicKind::ALL {
        let intrinsic = errors.intrinsic(kind);
        assert_eq!(
            runtime
                .realm_error_intrinsic_prototype(realm_id, kind)
                .expect("error prototype"),
            intrinsic.prototype
        );
        let prototype = runtime
            .objects
            .get(intrinsic.prototype)
            .expect("native error prototype");
        let expected_prototype = if kind == ErrorIntrinsicKind::Error {
            object_prototype
        } else {
            errors.intrinsic(ErrorIntrinsicKind::Error).prototype
        };
        assert_eq!(
            prototype.record.prototype(),
            Some(HeapReference::Object(expected_prototype))
        );
        assert!(!prototype.is_error());
        assert_eq!(
            prototype.record.property_count(),
            if kind == ErrorIntrinsicKind::Error {
                4
            } else {
                3
            }
        );
        assert_data_property(
            &prototype.record,
            &runtime,
            PredefinedAtom::Name,
            PropertyLayout::data(true, false, true),
            |value| matches!(value, StoredValue::String(name) if name == JsString::from_utf8(kind.name()).expect("name")),
        );
        assert_data_property(
            &prototype.record,
            &runtime,
            PredefinedAtom::Message,
            PropertyLayout::data(true, false, true),
            |value| matches!(value, StoredValue::String(message) if message.is_empty()),
        );
        assert_data_property(
            &prototype.record,
            &runtime,
            PredefinedAtom::Constructor,
            PropertyLayout::data(true, false, true),
            |value| matches!(value, StoredValue::Function(function) if function == intrinsic.constructor),
        );

        let to_string_key = runtime.predefined_property_key(PredefinedAtom::ToString);
        let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
        let message_key = runtime.predefined_property_key(PredefinedAtom::Message);
        let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
        if kind == ErrorIntrinsicKind::Error {
            assert_eq!(
                function_property(
                    &prototype.record,
                    &runtime,
                    PredefinedAtom::ToString,
                    PropertyLayout::data(true, false, true),
                ),
                errors.to_string
            );
            assert_eq!(
                prototype.record.has_own_property_with_scan(&to_string_key),
                (true, 1)
            );
            assert_eq!(
                prototype.record.has_own_property_with_scan(&name_key),
                (true, 2)
            );
            assert_eq!(
                prototype.record.has_own_property_with_scan(&message_key),
                (true, 3)
            );
            assert_eq!(
                prototype
                    .record
                    .has_own_property_with_scan(&constructor_key),
                (true, 4)
            );
        } else {
            assert!(!has_own_property(
                &prototype.record,
                &runtime,
                PredefinedAtom::ToString,
            ));
            assert_eq!(
                prototype.record.has_own_property_with_scan(&name_key),
                (true, 1)
            );
            assert_eq!(
                prototype.record.has_own_property_with_scan(&message_key),
                (true, 2)
            );
            assert_eq!(
                prototype
                    .record
                    .has_own_property_with_scan(&constructor_key),
                (true, 3)
            );
        }

        let constructor = runtime
            .functions
            .get(intrinsic.constructor)
            .expect("Error constructor");
        assert_eq!(
            constructor.object.prototype(),
            Some(HeapReference::Function(
                if kind == ErrorIntrinsicKind::Error {
                    function_prototype
                } else {
                    error_constructor
                }
            ))
        );
        assert!(matches!(
            constructor.implementation,
            FunctionImplementation::Native(ref native)
                if native.realm == realm_id
                    && native.kind == NativeFunctionKind::ErrorConstructor(kind)
                    && native.kind.is_constructor()
        ));
        assert_eq!(
            constructor.object.property_count(),
            if kind == ErrorIntrinsicKind::Error {
                4
            } else {
                3
            }
        );
        assert_data_property(
            &constructor.object,
            &runtime,
            PredefinedAtom::Length,
            PropertyLayout::data(false, false, true),
            |value| matches!(value, StoredValue::Number(length) if length.strict_equals(JsNumber::from_i32(if kind == ErrorIntrinsicKind::AggregateError { 2 } else { 1 }))),
        );
        assert_data_property(
            &constructor.object,
            &runtime,
            PredefinedAtom::Name,
            PropertyLayout::data(false, false, true),
            |value| matches!(value, StoredValue::String(name) if name == JsString::from_utf8(kind.name()).expect("name")),
        );
        assert_data_property(
            &constructor.object,
            &runtime,
            PredefinedAtom::Prototype,
            PropertyLayout::data(false, false, false),
            |value| matches!(value, StoredValue::Object(object) if object == intrinsic.prototype),
        );
        let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
        let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
        assert_eq!(
            constructor.object.has_own_property_with_scan(&length_key),
            (true, 1)
        );
        assert_eq!(
            constructor.object.has_own_property_with_scan(&name_key),
            (true, 2)
        );
        if kind == ErrorIntrinsicKind::Error {
            assert_eq!(
                function_property_by_key(
                    &constructor.object,
                    &is_error_key,
                    PropertyLayout::data(true, false, true),
                ),
                errors.is_error
            );
            assert_eq!(
                constructor.object.has_own_property_with_scan(&is_error_key),
                (true, 3)
            );
            assert_eq!(
                constructor
                    .object
                    .has_own_property_with_scan(&prototype_key),
                (true, 4)
            );
        } else {
            assert!(constructor.object.own_property(&is_error_key).is_none());
            assert_eq!(
                constructor
                    .object
                    .has_own_property_with_scan(&prototype_key),
                (true, 3)
            );
        }

        let global = &runtime
            .objects
            .get(global_object)
            .expect("global object")
            .record;
        assert_data_property(
            global,
            &runtime,
            kind.predefined_atom(),
            PropertyLayout::data(true, false, true),
            |value| matches!(value, StoredValue::Function(function) if function == intrinsic.constructor),
        );
    }

    for (exception, intrinsic) in [
        (
            ExceptionKind::InternalError,
            ErrorIntrinsicKind::InternalError,
        ),
        (ExceptionKind::RangeError, ErrorIntrinsicKind::RangeError),
        (
            ExceptionKind::ReferenceError,
            ErrorIntrinsicKind::ReferenceError,
        ),
        (ExceptionKind::SyntaxError, ErrorIntrinsicKind::SyntaxError),
        (ExceptionKind::TypeError, ErrorIntrinsicKind::TypeError),
    ] {
        assert_eq!(
            runtime
                .realm_error_prototype(realm_id, exception)
                .expect("engine Error prototype"),
            errors.intrinsic(intrinsic).prototype
        );
    }
}

#[test]
fn error_runtime_helpers_preserve_brand_descriptors_and_exact_accounting() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let prototype = runtime
        .realm_error_intrinsic_prototype(realm.0.id, ErrorIntrinsicKind::AggregateError)
        .expect("AggregateError.prototype");
    let prior_usage = runtime.usage();
    let error = runtime
        .allocate_error_with_prototype(HeapReference::Object(prototype))
        .expect("Error object");

    assert!(runtime.is_error_object(error).expect("live Error object"));
    assert_eq!(
        runtime.usage().heap_objects(),
        prior_usage.heap_objects() + 1
    );
    assert_eq!(
        runtime.usage().object_properties(),
        prior_usage.object_properties()
    );
    assert_eq!(
        runtime
            .objects
            .get(error)
            .expect("Error object")
            .record
            .prototype(),
        Some(HeapReference::Object(prototype))
    );

    for (index, atom) in [
        PredefinedAtom::Message,
        PredefinedAtom::Cause,
        PredefinedAtom::Errors,
        PredefinedAtom::Stack,
    ]
    .into_iter()
    .enumerate()
    {
        let value = JsString::from_utf8(&format!("value-{index}")).expect("value");
        runtime
            .define_error_data_property(error, atom, StoredValue::String(value.clone()))
            .expect("Error data property");
        assert_data_property(
            &runtime.objects.get(error).expect("Error object").record,
            &runtime,
            atom,
            PropertyLayout::data(true, false, true),
            |actual| matches!(actual, StoredValue::String(actual) if actual == value),
        );
        let key = runtime.predefined_property_key(atom);
        assert_eq!(
            runtime
                .objects
                .get(error)
                .expect("Error object")
                .record
                .has_own_property_with_scan(&key),
            (true, index + 1)
        );
    }
    assert_eq!(
        runtime.usage().object_properties(),
        prior_usage.object_properties() + 4
    );

    let usage = runtime.usage();
    assert!(
        runtime
            .define_error_data_property(error, PredefinedAtom::Name, StoredValue::Undefined)
            .is_err()
    );
    assert!(
        runtime
            .define_error_data_property(error, PredefinedAtom::Message, StoredValue::Undefined)
            .is_err()
    );
    assert_eq!(runtime.usage(), usage);
}

#[test]
fn engine_error_materialization_is_realm_owned_branded_and_exact() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let first = runtime.create_realm().expect("first realm");
    let second = runtime.create_realm().expect("second realm");
    let first_id = first.0.id;
    let second_id = second.0.id;
    let mut prior_usage = runtime.usage();

    for (index, kind) in [
        ExceptionKind::InternalError,
        ExceptionKind::RangeError,
        ExceptionKind::ReferenceError,
        ExceptionKind::SyntaxError,
        ExceptionKind::TypeError,
    ]
    .into_iter()
    .enumerate()
    {
        let realm = if index % 2 == 0 { first_id } else { second_id };
        let message = JsString::from_utf8(&format!("message-{index}")).expect("error message");
        let stack = JsString::from_utf8(&format!("    at test-{index} (unit.js:1:1)\n"))
            .expect("error stack");
        let object = runtime
            .materialize_error_object(realm, kind, message.clone(), Some(stack.clone()))
            .expect("engine Error object");
        let node = runtime.objects.get(object).expect("materialized Error");

        assert!(node.is_error());
        assert_eq!(
            node.record.prototype(),
            Some(HeapReference::Object(
                runtime
                    .realm_error_prototype(realm, kind)
                    .expect("realm error prototype")
            ))
        );
        assert!(
            !has_own_property(&node.record, &runtime, PredefinedAtom::Name),
            "Error instance inherits its exact native-error name"
        );
        assert_data_property(
            &node.record,
            &runtime,
            PredefinedAtom::Message,
            PropertyLayout::data(true, false, true),
            |value| matches!(value, StoredValue::String(actual) if actual == message),
        );
        assert_data_property(
            &node.record,
            &runtime,
            PredefinedAtom::Stack,
            PropertyLayout::data(true, false, true),
            |value| matches!(value, StoredValue::String(actual) if actual == stack),
        );
        assert_eq!(
            runtime.usage().heap_objects(),
            prior_usage.heap_objects() + 1
        );
        assert_eq!(
            runtime.usage().object_properties(),
            prior_usage.object_properties() + 2
        );
        prior_usage = runtime.usage();
    }
    assert!(runtime.collection_pending);
}

#[test]
fn engine_error_materialization_limit_failures_are_atomic() {
    for (limits, expected_resource, expected_limit, expected_observed, stack) in [
        (
            RuntimeLimits::default().with_max_heap_objects(20),
            RuntimeResource::HeapObjects,
            20,
            21,
            None,
        ),
        (
            RuntimeLimits::default().with_max_object_properties(437),
            RuntimeResource::ObjectProperties,
            437,
            438,
            None,
        ),
        (
            RuntimeLimits::default().with_max_object_properties(438),
            RuntimeResource::ObjectProperties,
            438,
            439,
            Some(JsString::from_utf8("    at test (unit.js:1:1)\n").expect("stack")),
        ),
    ] {
        let mut runtime = Runtime::try_new(limits).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let usage = runtime.usage();
        let collection_pending = runtime.collection_pending;

        let error = runtime
            .materialize_error_object(
                realm.0.id,
                ExceptionKind::TypeError,
                JsString::from_utf8("boom").expect("message"),
                stack,
            )
            .expect_err("materialization must exceed its exact limit");

        assert!(matches!(
            error,
            ExecutionError::LimitExceeded {
                resource,
                limit,
                observed,
            } if resource == expected_resource
                && limit == expected_limit
                && observed == expected_observed
        ));
        assert_eq!(runtime.usage(), usage);
        assert_eq!(runtime.collection_pending, collection_pending);
    }
}

#[test]
fn unrooted_engine_error_is_collected_without_reclaiming_error_prototypes() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_error_prototype(realm_id, ExceptionKind::ReferenceError)
        .expect("ReferenceError.prototype");
    let error = runtime
        .materialize_error_object(
            realm_id,
            ExceptionKind::ReferenceError,
            JsString::from_utf8("missing").expect("message"),
            None,
        )
        .expect("engine Error object");

    let report = runtime.collect_cycles().expect("collection");

    assert_eq!(report.objects(), 1);
    assert!(runtime.objects.get(error).is_none());
    assert!(runtime.objects.get(prototype).is_some());
    assert_eq!(runtime.usage().heap_objects(), 20);
    assert_eq!(runtime.usage().object_properties(), 437);
}

#[test]
fn realm_intrinsic_creation_is_failure_atomic_at_each_limit() {
    for (limits, expected_resource, limit, observed) in [
        (
            RuntimeLimits::default().with_max_heap_objects(19),
            RuntimeResource::HeapObjects,
            19,
            20,
        ),
        (
            RuntimeLimits::default().with_max_heap_functions(122),
            RuntimeResource::HeapFunctions,
            122,
            131,
        ),
        (
            RuntimeLimits::default().with_max_object_properties(412),
            RuntimeResource::ObjectProperties,
            412,
            437,
        ),
    ] {
        let mut runtime = Runtime::try_new(limits).expect("runtime");
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
        assert_eq!(runtime.usage(), RuntimeUsage::default());
    }
}

#[test]
fn boxed_boolean_allocation_limit_failure_is_atomic() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_objects(20)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let usage = runtime.usage();
    let collection_pending = runtime.collection_pending;

    for value in [false, true] {
        let error = runtime
            .allocate_boxed_boolean(realm_id, value)
            .expect_err("boxed Boolean must exceed the exact intrinsic object limit");

        assert!(matches!(
            error,
            ExecutionError::LimitExceeded {
                resource: RuntimeResource::HeapObjects,
                limit: 20,
                observed: 21,
            }
        ));
        assert_eq!(runtime.usage(), usage);
        assert_eq!(runtime.collection_pending, collection_pending);
    }
}

#[test]
fn boxed_boolean_allocation_at_exact_limit_preserves_brand_and_prototype() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_objects(21)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_boolean_prototype(realm_id)
        .expect("Boolean.prototype");

    let object = runtime
        .allocate_boxed_boolean(realm_id, true)
        .expect("one boxed Boolean fits the exact limit");

    assert_eq!(runtime.usage().heap_objects(), 21);
    assert_eq!(runtime.usage().object_properties(), 437);
    assert_eq!(
        runtime.boxed_boolean(object).expect("live wrapper"),
        Some(true)
    );
    assert_eq!(
        runtime
            .objects
            .get(object)
            .expect("boxed Boolean")
            .record
            .prototype(),
        Some(HeapReference::Object(prototype))
    );
    assert!(runtime.collection_pending);
}

#[test]
fn boolean_brand_is_not_inferred_from_the_prototype_chain() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_boolean_prototype(realm_id)
        .expect("Boolean.prototype");
    let fake = runtime
        .allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
        .expect("ordinary object with Boolean.prototype");

    assert_eq!(
        runtime
            .objects
            .get(fake)
            .expect("ordinary object")
            .record
            .prototype(),
        Some(HeapReference::Object(prototype))
    );
    assert_eq!(runtime.boxed_boolean(fake).expect("live object"), None);
}

#[test]
fn boxed_number_allocation_limit_failure_is_atomic() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_objects(20)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let usage = runtime.usage();
    let collection_pending = runtime.collection_pending;

    for value in [
        JsNumber::from_i32(0),
        JsNumber::from_f64(-0.0),
        JsNumber::from_f64(f64::NAN),
    ] {
        let error = runtime
            .allocate_boxed_number(realm_id, value)
            .expect_err("boxed Number must exceed the exact intrinsic object limit");

        assert!(matches!(
            error,
            ExecutionError::LimitExceeded {
                resource: RuntimeResource::HeapObjects,
                limit: 20,
                observed: 21,
            }
        ));
        assert_eq!(runtime.usage(), usage);
        assert_eq!(runtime.collection_pending, collection_pending);
    }
}

#[test]
fn boxed_number_allocation_at_exact_limit_preserves_payload_and_prototype() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_objects(21)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_number_prototype(realm_id)
        .expect("Number.prototype");
    let negative_zero = JsNumber::from_f64(-0.0);

    let object = runtime
        .allocate_boxed_number(realm_id, negative_zero)
        .expect("one boxed Number fits the exact limit");

    assert_eq!(runtime.usage().heap_objects(), 21);
    assert_eq!(runtime.usage().object_properties(), 437);
    assert!(
        runtime
            .boxed_number(object)
            .expect("live wrapper")
            .is_some_and(|value| value.same_value(negative_zero))
    );
    assert_eq!(
        runtime
            .objects
            .get(object)
            .expect("boxed Number")
            .record
            .prototype(),
        Some(HeapReference::Object(prototype))
    );
    assert!(runtime.collection_pending);
}

#[test]
fn number_brand_is_not_inferred_from_the_prototype_chain() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_number_prototype(realm_id)
        .expect("Number.prototype");
    let fake = runtime
        .allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
        .expect("ordinary object with Number.prototype");

    assert_eq!(
        runtime
            .objects
            .get(fake)
            .expect("ordinary object")
            .record
            .prototype(),
        Some(HeapReference::Object(prototype))
    );
    assert!(runtime.boxed_number(fake).expect("live object").is_none());
}

#[test]
fn boxed_string_allocation_limits_fail_atomically() {
    for (limits, resource, limit, observed) in [
        (
            RuntimeLimits::default().with_max_heap_objects(20),
            RuntimeResource::HeapObjects,
            20,
            21,
        ),
        (
            RuntimeLimits::default().with_max_object_properties(437),
            RuntimeResource::ObjectProperties,
            437,
            438,
        ),
    ] {
        let mut runtime = Runtime::try_new(limits).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let realm_id = realm.0.id;
        let usage = runtime.usage();
        let collection_pending = runtime.collection_pending;

        let error = runtime
            .allocate_boxed_string(realm_id, JsString::from_utf8("xy").expect("String payload"))
            .expect_err("boxed String must exceed the exact resource limit");

        assert!(matches!(
            error,
            ExecutionError::LimitExceeded {
                resource: actual_resource,
                limit: actual_limit,
                observed: actual_observed,
            } if actual_resource == resource
                && actual_limit == limit
                && actual_observed == observed
        ));
        assert_eq!(runtime.usage(), usage);
        assert_eq!(runtime.collection_pending, collection_pending);
    }
}

#[test]
fn boxed_string_allocation_preserves_payload_prototype_and_exact_length_property() {
    let limits = RuntimeLimits::default()
        .with_max_heap_objects(21)
        .with_max_object_properties(438);
    let mut runtime = Runtime::try_new(limits).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_string_prototype(realm_id)
        .expect("String.prototype");
    let text = JsString::from_code_units([u16::from(b'A'), 0xd83d]).expect("String payload");

    let object = runtime
        .allocate_boxed_string(realm_id, text.clone())
        .expect("one boxed String fits the exact limits");

    assert_eq!(runtime.usage().heap_objects(), 21);
    assert_eq!(runtime.usage().object_properties(), 438);
    assert_eq!(
        runtime.boxed_string(object).expect("live wrapper"),
        Some(&text)
    );
    let wrapper = runtime.objects.get(object).expect("boxed String");
    assert_eq!(
        wrapper.record.prototype(),
        Some(HeapReference::Object(prototype))
    );
    assert_data_property(
        &wrapper.record,
        &runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, false),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(2))),
    );
    assert_eq!(
        runtime
            .boxed_string_code_unit_at(object, 1)
            .expect("live wrapper"),
        Some(0xd83d)
    );
    assert_eq!(
        runtime
            .boxed_string_code_unit_at(object, 2)
            .expect("live wrapper"),
        None
    );
    assert!(runtime.collection_pending);
}

#[test]
fn string_brand_is_not_inferred_and_unrooted_wrapper_collection_releases_length_charge() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let prototype = runtime
        .realm_string_prototype(realm_id)
        .expect("String.prototype");
    let fake = runtime
        .allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
        .expect("ordinary object with String.prototype");
    assert!(runtime.boxed_string(fake).expect("live object").is_none());
    let wrapper = runtime
        .allocate_boxed_string(realm_id, JsString::from_utf8("temporary").expect("String"))
        .expect("boxed String");
    assert!(
        runtime
            .boxed_string(wrapper)
            .expect("live wrapper")
            .is_some()
    );
    assert_eq!(runtime.usage().object_properties(), 438);

    let report = runtime.collect_cycles().expect("collection");

    assert_eq!(report.objects(), 2);
    assert!(runtime.objects.get(fake).is_none());
    assert!(runtime.objects.get(wrapper).is_none());
    assert_eq!(runtime.usage().object_properties(), 437);
}

#[test]
fn function_call_atom_limit_failure_is_failure_atomic() {
    let atom_limits = AtomLimits::new(
        PREDEFINED_ATOM_COUNT,
        PREDEFINED_DESCRIPTION_CODE_UNITS,
        PREDEFINED_INTERNER_SLOTS,
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_atom_limits(atom_limits)).expect("runtime");
    let atoms_before = runtime.atom_usage();

    let error = runtime
        .create_realm()
        .expect_err("call atom must exceed limit");

    assert!(matches!(
        error,
        RuntimeError::Atom(AtomError::LiveAtomLimit {
            current: PREDEFINED_ATOM_COUNT,
            additional: 1,
            maximum: PREDEFINED_ATOM_COUNT,
        })
    ));
    assert_eq!(runtime.usage(), RuntimeUsage::default());
    assert_eq!(runtime.atom_usage(), atoms_before);
}

#[test]
fn partial_dynamic_realm_atom_failure_rolls_back_every_interned_name() {
    const PERMITTED_DYNAMIC_ATOMS: u32 = 8;
    let atom_limits = AtomLimits::new(
        PREDEFINED_ATOM_COUNT + PERMITTED_DYNAMIC_ATOMS,
        PREDEFINED_DESCRIPTION_CODE_UNITS + 168,
        PREDEFINED_INTERNER_SLOTS + 20,
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_atom_limits(atom_limits)).expect("runtime");
    let usage_before = runtime.usage();
    let atoms_before = runtime.atom_usage();

    let error = runtime
        .create_realm()
        .expect_err("the ninth dynamic intrinsic name must exceed the atom limit");

    assert!(matches!(
        error,
        RuntimeError::Atom(AtomError::LiveAtomLimit {
            current,
            additional: 1,
            maximum,
        }) if current == PREDEFINED_ATOM_COUNT + PERMITTED_DYNAMIC_ATOMS
            && maximum == PREDEFINED_ATOM_COUNT + PERMITTED_DYNAMIC_ATOMS
    ));
    assert_eq!(runtime.usage(), usage_before);
    assert_eq!(runtime.atom_usage(), atoms_before);
}

#[test]
fn realm_function_intrinsics_remain_roots_during_collection() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let expected_intrinsics = runtime
        .realms
        .get(realm_id)
        .expect("realm state")
        .intrinsics;

    let report = runtime.collect_cycles().expect("collection");

    assert_eq!(report.functions(), 0);
    assert_eq!(runtime.usage().heap_functions(), 131);
    assert_eq!(runtime.usage().installed_code(), 0);
    assert_eq!(
        runtime
            .realms
            .get(realm_id)
            .expect("realm state")
            .intrinsics,
        expected_intrinsics
    );
}

#[test]
fn function_methods_are_collected_after_their_realm_prototype_edges_are_replaced() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let call_name = JsString::from_utf8("call").expect("call");
    let call_key = runtime
        .atoms
        .property_key_from_string(&call_name)
        .expect("call key");
    let apply_key =
        PropertyKey::from_validated_atom(runtime.atoms.predefined(PredefinedAtom::Apply));
    let RealmIntrinsics::Ready {
        function_prototype, ..
    } = runtime.realms.get(realm_id).expect("realm").intrinsics
    else {
        panic!("realm intrinsics remained uninitialized");
    };
    let function_call = function_property_by_key(
        &runtime
            .functions
            .get(function_prototype)
            .expect("Function.prototype")
            .object,
        &call_key,
        PropertyLayout::data(true, false, true),
    );
    let function_apply = function_property_by_key(
        &runtime
            .functions
            .get(function_prototype)
            .expect("Function.prototype")
            .object,
        &apply_key,
        PropertyLayout::data(true, false, true),
    );
    assert!(
        runtime
            .functions
            .get_mut(function_prototype)
            .expect("Function.prototype")
            .object
            .replace_existing_data(&call_key, StoredValue::Undefined)
    );
    assert!(
        runtime
            .functions
            .get_mut(function_prototype)
            .expect("Function.prototype")
            .object
            .replace_existing_data(&apply_key, StoredValue::Undefined)
    );

    let report = runtime.collect_cycles().expect("collection");

    assert_eq!(report.functions(), 2);
    assert!(runtime.functions.get(function_call).is_none());
    assert!(runtime.functions.get(function_apply).is_none());
    assert_eq!(runtime.usage().heap_functions(), 129);
    assert_eq!(runtime.usage().object_properties(), 433);
}

#[test]
fn dynamic_function_declaration_properties_are_deletable_eval_properties() {
    let layout = dynamic_function_declaration_property_layout();

    assert_eq!(layout.writable(), Some(true));
    assert!(layout.is_enumerable());
    assert!(layout.is_configurable());
}

#[test]
fn global_function_descriptor_compatibility_matches_quickjs() {
    let replacement = dynamic_function_declaration_property_layout();
    assert_eq!(
        global_function_replacement_layout(PropertyLayout::data(false, false, true)),
        Some(replacement)
    );
    assert_eq!(
        global_function_replacement_layout(PropertyLayout::data(true, true, false)),
        Some(PropertyLayout::data(true, true, false))
    );
    assert_eq!(
        global_function_replacement_layout(PropertyLayout::data(false, true, false)),
        None
    );
    assert_eq!(
        global_function_replacement_layout(PropertyLayout::data(true, false, false)),
        None
    );
    assert_eq!(
        global_function_replacement_layout(PropertyLayout::accessor(false, true)),
        Some(replacement)
    );
    assert_eq!(
        global_function_replacement_layout(PropertyLayout::accessor(true, false)),
        None
    );
}

#[test]
fn accessor_getter_and_setter_are_traced_as_function_edges() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let global_object = runtime.realms.get(realm_id).expect("realm").global_object;
    let getter = runtime
        .functions
        .try_insert(HeapFunction {
            implementation: FunctionImplementation::Native(NativeFunction {
                realm: realm_id,
                kind: NativeFunctionKind::FunctionPrototype,
            }),
            object: ObjectRecord::empty(None),
            public_roots: 0,
        })
        .expect("getter");
    let setter = runtime
        .functions
        .try_insert(HeapFunction {
            implementation: FunctionImplementation::Native(NativeFunction {
                realm: realm_id,
                kind: NativeFunctionKind::FunctionPrototype,
            }),
            object: ObjectRecord::empty(None),
            public_roots: 0,
        })
        .expect("setter");
    let orphan = runtime
        .functions
        .try_insert(HeapFunction {
            implementation: FunctionImplementation::Native(NativeFunction {
                realm: realm_id,
                kind: NativeFunctionKind::FunctionPrototype,
            }),
            object: ObjectRecord::empty(None),
            public_roots: 0,
        })
        .expect("orphan");
    let key = PropertyKey::from_validated_atom(runtime.atoms.predefined(PredefinedAtom::Name));

    runtime
        .append_accessor_property(
            HeapReference::Object(global_object),
            key,
            PropertyLayout::accessor(false, true),
            Some(getter),
            Some(setter),
        )
        .expect("accessor");
    let report = runtime.collect_cycles().expect("collection");

    assert_eq!(report.functions(), 1);
    assert!(runtime.functions.get(getter).is_some());
    assert!(runtime.functions.get(setter).is_some());
    assert!(runtime.functions.get(orphan).is_none());
    assert_eq!(runtime.usage().object_properties(), 438);
}

#[test]
fn duplicate_accessor_insertion_is_rejected_without_mutation_or_recharging() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let state = runtime.realms.get(realm_id).expect("realm");
    let global_object = state.global_object;
    let RealmIntrinsics::Ready {
        function_constructor,
        ..
    } = state.intrinsics
    else {
        panic!("realm intrinsics remained uninitialized");
    };
    let key = PropertyKey::from_validated_atom(runtime.atoms.predefined(PredefinedAtom::Name));
    let layout = PropertyLayout::accessor(false, true);
    runtime
        .append_accessor_property(
            HeapReference::Object(global_object),
            key.clone(),
            layout,
            Some(function_constructor),
            None,
        )
        .expect("initial accessor");
    let usage = runtime.usage();

    let error = runtime
        .append_accessor_property(
            HeapReference::Object(global_object),
            key.clone(),
            PropertyLayout::accessor(true, false),
            None,
            Some(function_constructor),
        )
        .expect_err("duplicate accessor");

    assert!(matches!(
        error,
        ExecutionError::EngineFault(EngineFault::RuntimeInvariant {
            message: "accessor insertion targeted an existing own property",
        })
    ));
    assert_eq!(runtime.usage(), usage);
    assert!(matches!(
        runtime
            .objects
            .get(global_object)
            .expect("global")
            .record
            .own_property(&key),
        Some(OwnProperty::Accessor {
            layout: actual_layout,
            getter: Some(actual_getter),
            setter: None,
        }) if actual_layout == layout && actual_getter == function_constructor
    ));
}

#[test]
fn accessor_to_data_global_replacement_rolls_back_the_complete_slot() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let global_object = runtime.realms.get(realm_id).expect("realm").global_object;
    let RealmIntrinsics::Ready {
        function_constructor,
        ..
    } = runtime.realms.get(realm_id).expect("realm").intrinsics
    else {
        panic!("realm intrinsics remained uninitialized");
    };
    let key = PropertyKey::from_validated_atom(runtime.atoms.predefined(PredefinedAtom::Name));
    let accessor_layout = PropertyLayout::accessor(false, true);
    runtime
        .append_accessor_property(
            HeapReference::Object(global_object),
            key.clone(),
            accessor_layout,
            Some(function_constructor),
            None,
        )
        .expect("accessor");
    let previous = runtime
        .objects
        .get_mut(global_object)
        .expect("global")
        .record
        .replace_existing_with_data(
            &key,
            dynamic_function_declaration_property_layout(),
            StoredValue::Undefined,
        )
        .expect("accessor replacement");
    let environment = RootEnvironment {
        bindings: Vec::new(),
        inserted_globals: Vec::new(),
        updated_globals: Vec::new(),
        inserted_global_properties: Vec::new(),
        updated_global_properties: vec![(key.clone(), previous)],
    };

    runtime.rollback_root_environment(realm_id, &environment);

    assert_eq!(runtime.usage().object_properties(), 438);
    assert!(matches!(
        runtime
            .objects
            .get(global_object)
            .expect("global")
            .record
            .own_property(&key),
        Some(OwnProperty::Accessor {
            layout,
            getter: Some(actual_getter),
            setter: None,
        }) if layout == accessor_layout && actual_getter == function_constructor
    ));
}

#[test]
fn for_in_orders_keys_suppresses_shadowed_prototype_names_and_never_reads_getters() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let prototype = runtime
        .allocate_ordinary_object(object_prototype)
        .expect("prototype");
    let object = runtime
        .allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
        .expect("object");
    let getter = match runtime.realms.get(realm_id).expect("realm").intrinsics {
        RealmIntrinsics::Ready {
            function_constructor,
            ..
        } => function_constructor,
        RealmIntrinsics::Initializing => panic!("realm intrinsics"),
    };

    for (reference, name, enumerable) in [
        (HeapReference::Object(object), "b", true),
        (HeapReference::Object(object), "a", true),
        (HeapReference::Object(object), "dup", true),
        (HeapReference::Object(object), "hidden", false),
        (HeapReference::Object(prototype), "p", true),
        (HeapReference::Object(prototype), "dup", true),
        (HeapReference::Object(prototype), "hidden", true),
    ] {
        let key = string_property_key(&mut runtime, name);
        runtime
            .append_data_property(
                reference,
                key,
                PropertyLayout::data(true, enumerable, true),
                StoredValue::Undefined,
            )
            .expect("property");
    }
    runtime
        .append_data_property(
            HeapReference::Object(object),
            PropertyKey::from_index(ArrayIndex::new(2).expect("index")),
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("index 2");
    runtime
        .append_data_property(
            HeapReference::Object(object),
            PropertyKey::from_index(ArrayIndex::new(1).expect("index")),
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("index 1");
    let getter_key = string_property_key(&mut runtime, "get");
    runtime
        .append_accessor_property(
            HeapReference::Object(prototype),
            getter_key,
            PropertyLayout::accessor(true, true),
            Some(getter),
            None,
        )
        .expect("getter");

    let (iterator, _) = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Object(object))
        .expect("iterator");
    assert_eq!(
        collect_for_in_keys(&mut runtime, iterator),
        ["1", "2", "b", "a", "dup", "p", "get"]
    );
}

#[test]
fn for_in_observes_deletion_and_late_prototype_snapshots_without_late_own_additions() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let prototype = runtime
        .allocate_ordinary_object(object_prototype)
        .expect("prototype");
    let object = runtime
        .allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
        .expect("object");
    let a = string_property_key(&mut runtime, "a");
    let b = string_property_key(&mut runtime, "b");
    runtime
        .append_data_property(
            HeapReference::Object(object),
            a,
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("a");
    runtime
        .append_data_property(
            HeapReference::Object(object),
            b.clone(),
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("own b");
    runtime
        .append_data_property(
            HeapReference::Object(prototype),
            b.clone(),
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("prototype b");

    let (iterator, _) = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Object(object))
        .expect("iterator");
    assert_eq!(
        next_for_in_key(&mut runtime, iterator).as_deref(),
        Some("a")
    );
    let removed = runtime
        .object_record_mut(HeapReference::Object(object))
        .expect("object")
        .pop_last_data(&b);
    assert!(removed.is_some());
    runtime.object_properties = runtime.object_properties.saturating_sub(1);
    let late_own = string_property_key(&mut runtime, "late-own");
    runtime
        .append_data_property(
            HeapReference::Object(object),
            late_own,
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("late own");
    let late_prototype = string_property_key(&mut runtime, "late-prototype");
    runtime
        .append_data_property(
            HeapReference::Object(prototype),
            late_prototype,
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("late prototype");

    assert_eq!(
        collect_for_in_keys(&mut runtime, iterator),
        ["late-prototype"]
    );
}

#[test]
fn for_in_boxes_primitives_and_enumerates_utf16_string_indices() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    for (prototype, name) in [
        (
            runtime
                .realm_boolean_prototype(realm_id)
                .expect("Boolean.prototype"),
            "b",
        ),
        (
            runtime
                .realm_number_prototype(realm_id)
                .expect("Number.prototype"),
            "n",
        ),
        (
            runtime
                .realm_string_prototype(realm_id)
                .expect("String.prototype"),
            "s",
        ),
    ] {
        let key = string_property_key(&mut runtime, name);
        runtime
            .append_data_property(
                HeapReference::Object(prototype),
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Undefined,
            )
            .expect("prototype property");
    }

    assert_eq!(
        for_in_keys_for_value(&mut runtime, realm_id, StoredValue::Boolean(true)),
        ["b"]
    );
    assert_eq!(
        for_in_keys_for_value(
            &mut runtime,
            realm_id,
            StoredValue::Number(JsNumber::from_i32(42)),
        ),
        ["n"]
    );
    assert_eq!(
        for_in_keys_for_value(
            &mut runtime,
            realm_id,
            StoredValue::String(JsString::from_utf8("A😀").expect("string")),
        ),
        ["0", "1", "2", "s"]
    );
    assert!(for_in_keys_for_value(&mut runtime, realm_id, StoredValue::Null).is_empty());
    assert!(for_in_keys_for_value(&mut runtime, realm_id, StoredValue::Undefined).is_empty());

    let description = JsString::from_utf8("symbol").expect("description");
    let symbol = runtime
        .atoms
        .new_unique_symbol(Some(&description))
        .expect("symbol");
    let usage = runtime.usage();
    let error = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Symbol(symbol))
        .expect_err("Symbol boxing remains fail closed");
    assert!(matches!(
        error,
        ExecutionError::EngineFault(EngineFault::RuntimeInvariant {
            message: "for-in Symbol boxing is not implemented",
        })
    ));
    assert_eq!(runtime.usage(), usage);
}

#[test]
fn for_in_limits_roll_back_primitive_wrappers_and_gc_traces_iterator_current() {
    let mut limited = Runtime::try_new(
        RuntimeLimits::default()
            .with_max_heap_objects(22)
            .with_max_for_in_entries(0),
    )
    .expect("runtime");
    let realm = limited.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let usage = limited.usage();
    let collection_pending = limited.collection_pending;
    let error = limited
        .allocate_for_in_iterator(
            realm_id,
            StoredValue::String(JsString::from_utf8("x").expect("string")),
        )
        .expect_err("entry limit");
    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::ForInEntries,
            limit: 0,
            observed: 2,
        }
    ));
    assert_eq!(limited.usage(), usage);
    assert_eq!(limited.collection_pending, collection_pending);

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let baseline = runtime.usage().heap_objects();
    let (iterator, _) = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Boolean(true))
        .expect("iterator");
    runtime
        .collect_cycles_with_roots(|mark| {
            mark(CollectionRoot::Heap(HeapReference::Object(iterator)));
        })
        .expect("rooted collection");
    assert_eq!(runtime.usage().heap_objects(), baseline + 2);
    runtime.collect_cycles().expect("unrooted collection");
    assert_eq!(runtime.usage().heap_objects(), baseline);
    assert_eq!(runtime.usage().for_in_entries(), 0);
}

#[test]
fn for_in_work_previews_cover_primitive_function_and_prototype_transitions() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let prototype = runtime
        .allocate_ordinary_object(object_prototype)
        .expect("prototype");
    let object = runtime
        .allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
        .expect("object");
    for (reference, name) in [
        (HeapReference::Object(object), "own"),
        (HeapReference::Object(prototype), "inherited"),
    ] {
        let key = string_property_key(&mut runtime, name);
        runtime
            .append_data_property(
                reference,
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Undefined,
            )
            .expect("enumerable property");
    }
    let function = match runtime.realms.get(realm_id).expect("realm").intrinsics {
        RealmIntrinsics::Ready {
            function_constructor,
            ..
        } => function_constructor,
        RealmIntrinsics::Initializing => panic!("realm intrinsics"),
    };

    for value in [
        StoredValue::Undefined,
        StoredValue::Null,
        StoredValue::Boolean(true),
        StoredValue::Number(JsNumber::from_i32(42)),
        StoredValue::String(JsString::from_utf8("A😀").expect("string")),
        StoredValue::Object(object),
        StoredValue::Function(function),
    ] {
        let preview = runtime
            .preview_for_in_iterator_work(&value)
            .expect("initial work preview");
        let (iterator, actual) = runtime
            .allocate_for_in_iterator(realm_id, value)
            .expect("iterator");
        assert!(actual <= preview);

        let mut completed = false;
        for _ in 0..10_000 {
            let preview = runtime
                .preview_for_in_advance_work(iterator)
                .expect("advance work preview");
            let advance = runtime.advance_for_in_iterator(iterator).expect("advance");
            assert!(advance.work() <= preview);
            if matches!(advance, ForInAdvance::Done { .. }) {
                completed = true;
                break;
            }
        }
        assert!(completed, "for-in preview test iterator did not complete");
    }
}

#[test]
fn for_in_visited_growth_is_precharged_for_non_enumerable_candidates() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let object = runtime
        .allocate_ordinary_object(object_prototype)
        .expect("object");
    for index in 0..64 {
        let key = string_property_key(&mut runtime, &format!("hidden-{index}"));
        runtime
            .append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, false, true),
                StoredValue::Undefined,
            )
            .expect("non-enumerable property");
    }
    let (iterator, _) = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Object(object))
        .expect("iterator");

    let mut crossed_capacity_boundary = false;
    for _ in 0..64 {
        let preview = runtime
            .preview_for_in_advance_work(iterator)
            .expect("visited growth preview");
        let advance = runtime
            .advance_for_in_iterator(iterator)
            .expect("visited growth");
        assert!(matches!(
            advance,
            ForInAdvance::Continue { work } if work == preview
        ));
        crossed_capacity_boundary |= preview > 1;
    }
    assert!(
        crossed_capacity_boundary,
        "the regression must force a visited HashSet capacity boundary"
    );

    let mut limited = Runtime::try_new(RuntimeLimits::default().with_max_for_in_entries(1))
        .expect("limited runtime");
    let realm = limited.create_realm().expect("realm");
    let realm_id = realm.0.id;
    let object_prototype = limited
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let object = limited
        .allocate_ordinary_object(object_prototype)
        .expect("object");
    let key = string_property_key(&mut limited, "hidden");
    limited
        .append_data_property(
            HeapReference::Object(object),
            key,
            PropertyLayout::data(true, false, true),
            StoredValue::Undefined,
        )
        .expect("non-enumerable property");
    let (iterator, _) = limited
        .allocate_for_in_iterator(realm_id, StoredValue::Object(object))
        .expect("iterator");
    let usage = limited.usage();
    let error = limited
        .preview_for_in_advance_work(iterator)
        .expect_err("visited entry limit must be checked before non-enumerable insertion");
    assert!(matches!(
        error,
        ExecutionError::LimitExceeded {
            resource: RuntimeResource::ForInEntries,
            limit: 1,
            observed: 2,
        }
    ));
    assert_eq!(limited.usage(), usage);
    assert!(
        limited
            .objects
            .get(iterator)
            .and_then(crate::object::HeapObject::for_in_state)
            .is_some_and(|state| state.candidate().is_some())
    );
}

fn string_property_key(runtime: &mut Runtime, name: &str) -> PropertyKey {
    runtime
        .property_key_from_string(&JsString::from_utf8(name).expect("string"))
        .expect("property key")
}

fn for_in_keys_for_value(
    runtime: &mut Runtime,
    realm: crate::ids::RealmId,
    value: StoredValue,
) -> Vec<String> {
    let (iterator, _) = runtime
        .allocate_for_in_iterator(realm, value)
        .expect("iterator");
    collect_for_in_keys(runtime, iterator)
}

fn collect_for_in_keys(runtime: &mut Runtime, iterator: crate::ids::ObjectId) -> Vec<String> {
    let mut keys = Vec::new();
    while let Some(key) = next_for_in_key(runtime, iterator) {
        keys.push(key);
    }
    keys
}

fn next_for_in_key(runtime: &mut Runtime, iterator: crate::ids::ObjectId) -> Option<String> {
    for _ in 0..10_000 {
        match runtime
            .advance_for_in_iterator(iterator)
            .expect("for-in advance")
        {
            ForInAdvance::Continue { .. } => {}
            ForInAdvance::Yield { key, .. } => {
                return Some(key.as_index().map_or_else(
                    || {
                        key.as_atom()
                            .and_then(crate::Atom::description)
                            .expect("string atom")
                            .to_utf8_lossy()
                            .expect("UTF-8")
                    },
                    |index| {
                        index
                            .to_js_string()
                            .expect("index string")
                            .to_utf8_lossy()
                            .expect("UTF-8")
                    },
                ));
            }
            ForInAdvance::Done { .. } => return None,
        }
    }
    panic!("for-in iterator did not complete within its bounded test work");
}

fn assert_data_property(
    record: &ObjectRecord,
    runtime: &Runtime,
    atom: PredefinedAtom,
    expected_layout: PropertyLayout,
    expected_value: impl FnOnce(StoredValue) -> bool,
) {
    let key = PropertyKey::from_validated_atom(runtime.atoms.predefined(atom));
    let (layout, value) = record.own_data_property(&key).expect("data property");
    assert_eq!(layout, expected_layout);
    assert!(expected_value(value));
}

fn function_property(
    record: &ObjectRecord,
    runtime: &Runtime,
    atom: PredefinedAtom,
    expected_layout: PropertyLayout,
) -> crate::ids::FunctionId {
    let key = PropertyKey::from_validated_atom(runtime.atoms.predefined(atom));
    let (layout, value) = record.own_data_property(&key).expect("function property");
    assert_eq!(layout, expected_layout);
    let StoredValue::Function(function) = value else {
        panic!("property is not a function");
    };
    function
}

fn has_own_property(record: &ObjectRecord, runtime: &Runtime, atom: PredefinedAtom) -> bool {
    let key = PropertyKey::from_validated_atom(runtime.atoms.predefined(atom));
    record.own_property(&key).is_some()
}

fn function_property_by_key(
    record: &ObjectRecord,
    key: &PropertyKey,
    expected_layout: PropertyLayout,
) -> crate::ids::FunctionId {
    let (layout, value) = record.own_data_property(key).expect("function property");
    assert_eq!(layout, expected_layout);
    let StoredValue::Function(function) = value else {
        panic!("property is not a function");
    };
    function
}

#[allow(clippy::too_many_arguments)]
fn assert_native_method(
    runtime: &Runtime,
    function: crate::ids::FunctionId,
    function_prototype: crate::ids::FunctionId,
    realm: crate::ids::RealmId,
    kind: NativeFunctionKind,
    name: PredefinedAtom,
    length: i32,
) {
    let expected_name = runtime
        .atoms
        .predefined(name)
        .description()
        .expect("predefined method name")
        .clone();
    assert_native_method_named(
        runtime,
        function,
        function_prototype,
        realm,
        kind,
        &expected_name,
        length,
    );
}

#[allow(clippy::too_many_arguments)]
fn assert_native_method_named(
    runtime: &Runtime,
    function: crate::ids::FunctionId,
    function_prototype: crate::ids::FunctionId,
    realm: crate::ids::RealmId,
    kind: NativeFunctionKind,
    expected_name: &JsString,
    length: i32,
) {
    let method = runtime.functions.get(function).expect("native method");
    assert_eq!(
        method.object.prototype(),
        Some(HeapReference::Function(function_prototype))
    );
    assert!(matches!(
        method.implementation,
        FunctionImplementation::Native(ref native)
            if native.realm == realm && native.kind == kind
    ));
    assert_data_property(
        &method.object,
        runtime,
        PredefinedAtom::Length,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::Number(number) if number.strict_equals(JsNumber::from_i32(length))),
    );
    assert_data_property(
        &method.object,
        runtime,
        PredefinedAtom::Name,
        PropertyLayout::data(false, false, true),
        |value| matches!(value, StoredValue::String(actual) if actual == *expected_name),
    );
}
