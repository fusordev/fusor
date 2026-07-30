/*
 * JavaScript bytecode execution tests derived from QuickJS.
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

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::{RuntimeLimits, value::StoredValue};
    use quickjs_compiler::CompilationContext;
    use quickjs_frontend::{
        CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits,
        FrontendOptions, GlobalScriptGoal, SourceFragment, with_dynamic_function_source,
        with_parsed_program,
    };

    struct NeverCompiler;

    impl OrdinaryDynamicFunctionCompiler for NeverCompiler {
        fn compile(
            &self,
            _source: OrdinaryDynamicFunctionSource,
        ) -> Result<Arc<quickjs_bytecode::VerifiedBytecode>, DynamicFunctionCompileFailure>
        {
            panic!("the coercion regression must fail or suspend before compilation")
        }
    }

    struct OxcDynamicCompiler;

    impl OrdinaryDynamicFunctionCompiler for OxcDynamicCompiler {
        fn compile(
            &self,
            source: OrdinaryDynamicFunctionSource,
        ) -> Result<Arc<quickjs_bytecode::VerifiedBytecode>, DynamicFunctionCompileFailure>
        {
            let parameter_text = source
                .parameters()
                .iter()
                .map(JsString::to_utf8_lossy)
                .collect::<Result<Vec<_>, _>>()
                .map_err(test_engine_failure)?;
            let body_text = source.body().to_utf8_lossy().map_err(test_engine_failure)?;
            let parameters = parameter_text
                .iter()
                .map(|parameter| SourceFragment::new(parameter.as_str()))
                .collect::<Vec<_>>();
            let dynamic_source = DynamicFunctionSource::new(
                DynamicFunctionKind::Function,
                &parameters,
                SourceFragment::new(&body_text),
            );
            with_dynamic_function_source(
                dynamic_source,
                FrontendLimits::default(),
                |unit, _prepared| {
                    let context = CompilationContext::new_with_source_name(
                        unit,
                        Arc::from("<vm accessor Function>"),
                    )
                    .map_err(test_engine_failure)?;
                    context
                        .compile_dynamic_function_script(
                            quickjs_bytecode::VerificationLimits::default(),
                        )
                        .map(|tree| Arc::new(tree.verified_bytecode().clone()))
                        .map_err(test_engine_failure)
                },
            )
            .map_err(test_engine_failure)?
        }
    }

    fn test_engine_failure(
        error: impl Error + Send + Sync + 'static,
    ) -> DynamicFunctionCompileFailure {
        DynamicFunctionCompileFailure::Engine {
            source: Arc::new(error),
        }
    }

    #[test]
    fn symbol_to_primitive_precedes_ordinary_methods_and_receives_string_hint() {
        let (mut runtime, realm, constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Function(constructor),
            )
            .expect("symbol method");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());

        let Ok(dispatch) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("conversion must suspend at Symbol.toPrimitive");
        };
        let NativeDispatch::Call(call) = dispatch else {
            panic!("Symbol.toPrimitive must be called first");
        };

        assert_eq!(call.function, constructor);
        assert!(matches!(call.receiver, StoredValue::Object(id) if id == object));
        assert_eq!(call.arguments.remaining().len(), 1);
        let StoredValue::String(hint) = &call.arguments.remaining()[0] else {
            panic!("hint must be a string");
        };
        assert_eq!(hint.to_utf8_lossy().expect("UTF-8"), "string");
        assert_eq!(call.continuations.len(), 1);
    }

    #[test]
    fn noncallable_symbol_to_primitive_throws_exact_type_error() {
        let (mut runtime, realm, _constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Number(JsNumber::from_i32(1)),
            )
            .expect("symbol value");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());

        let Err(error) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("noncallable exotic converter must fail");
        };

        assert_native_type_error(error, "not a function");
    }

    #[test]
    fn null_symbol_to_primitive_falls_back_to_the_ordinary_string_hint_order() {
        let (mut runtime, realm, constructor, _native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let exotic_key =
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                exotic_key,
                PropertyLayout::data(true, true, true),
                StoredValue::Null,
            )
            .expect("null exotic converter");
        let to_string_key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                to_string_key,
                PropertyLayout::data(true, true, true),
                StoredValue::Function(constructor),
            )
            .expect("ordinary converter");

        let Ok(NativeDispatch::Call(call)) = begin_property_key_conversion(
            &mut runtime,
            StoredValue::Object(object),
            PropertyKeyTarget::ToKey,
            None,
            native_function_host_origin(),
        ) else {
            panic!("null Symbol.toPrimitive must fall back to toString");
        };
        assert_eq!(call.function, constructor);
        assert!(matches!(call.receiver, StoredValue::Object(id) if id == object));
        assert!(call.arguments.remaining().is_empty());
        assert!(matches!(
            call.continuations.as_slice(),
            [NativeContinuation::PropertyKey(_)]
        ));
    }

    #[test]
    fn object_symbol_to_primitive_result_throws_before_ordinary_fallback() {
        let (mut runtime, realm, constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let result = source_object(&mut runtime, realm);
        let key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
        runtime
            .append_data_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::data(true, true, true),
                StoredValue::Function(constructor),
            )
            .expect("symbol method");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
        let Ok(dispatch) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("conversion must suspend at Symbol.toPrimitive");
        };
        let NativeDispatch::Call(call) = dispatch else {
            panic!("expected exotic conversion call");
        };

        let Err(error) = resume_native_continuations(
            &mut runtime,
            call.continuations,
            StoredValue::Object(result),
            call.return_to,
            0,
            0,
            Some(&compiler),
            &mut budget,
        ) else {
            panic!("object exotic result must fail");
        };

        assert_native_type_error(error, "toPrimitive");
    }

    #[test]
    fn constructor_source_continuation_charges_its_new_target_heap_edge() {
        let (_runtime, _realm, constructor, native) = runtime_with_function_constructor();
        let continuation = NativeContinuation::FunctionSource(FunctionSourceContinuation {
            native,
            arguments: vec![StoredValue::Undefined],
            index: 0,
            stage: PrimitiveConversionStage::Start,
            construction: Some(constructor),
            origin: native_function_host_origin(),
        });

        assert_eq!(continuation.retained_values(), 2);
    }

    #[test]
    fn property_key_continuations_charge_every_suspended_javascript_value() {
        let (mut runtime, realm, _constructor, _native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let origin = native_function_host_origin();
        let continuation = |target| {
            NativeContinuation::PropertyKey(PropertyKeyContinuation {
                receiver: StoredValue::Object(object),
                stage: PrimitiveConversionStage::Start,
                target,
                origin: origin.clone(),
            })
        };

        assert_eq!(continuation(PropertyKeyTarget::ToKey).retained_values(), 1);
        assert_eq!(
            continuation(PropertyKeyTarget::Read {
                base: StoredValue::Undefined,
            })
            .retained_values(),
            2
        );
        assert_eq!(
            continuation(PropertyKeyTarget::Write {
                base: StoredValue::Undefined,
                value: StoredValue::Undefined,
                strict: false,
            })
            .retained_values(),
            3
        );
        assert_eq!(
            continuation(PropertyKeyTarget::DefineMethod {
                base: StoredValue::Undefined,
                function: StoredValue::Undefined,
                kind: DefineMethodKind::Method,
                enumerable: true,
            })
            .retained_values(),
            3
        );
    }

    #[test]
    fn operator_primitive_continuations_charge_every_suspended_javascript_value() {
        let (mut runtime, realm, _constructor, _native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let origin = native_function_host_origin();
        let continuation = |target| {
            NativeContinuation::OperatorPrimitive(OperatorPrimitiveContinuation {
                receiver: StoredValue::Object(object),
                hint: OperatorPrimitiveHint::Number,
                stage: OperatorPrimitiveStage::Start,
                target,
                origin: origin.clone(),
            })
        };

        assert_eq!(
            continuation(OperatorPrimitiveTarget::Unary {
                opcode: FinalOpcode::Plus,
            })
            .retained_values(),
            1
        );
        assert_eq!(
            continuation(OperatorPrimitiveTarget::BinaryRight {
                opcode: FinalOpcode::Sub,
                right: StoredValue::Undefined,
                hint: OperatorPrimitiveHint::Number,
            })
            .retained_values(),
            2
        );
        assert_eq!(
            continuation(OperatorPrimitiveTarget::BinaryFinish {
                opcode: FinalOpcode::Add,
                left: StoredValue::Undefined,
            })
            .retained_values(),
            2
        );
        assert_eq!(
            continuation(OperatorPrimitiveTarget::EqualityFinish {
                opcode: FinalOpcode::Eq,
                other: StoredValue::Undefined,
            })
            .retained_values(),
            2
        );
    }

    #[test]
    fn synchronous_internal_read_rejects_an_accessor_instead_of_skipping_it() {
        let (mut runtime, realm, constructor, _native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key.clone(),
                PropertyLayout::accessor(false, true),
                Some(constructor),
                None,
            )
            .expect("accessor");

        let error = read_heap_property(&runtime, HeapReference::Object(object), &key)
            .expect_err("synchronous accessor read must fail closed");

        assert!(matches!(error, ExecutionError::EngineFault(_)));
    }

    #[test]
    fn get_field_executes_an_own_bytecode_getter() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let getter_authority = compile_test_function("function getter(){return 23;}", "getter");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (reader, getter) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(reader_authority).expect("reader"),
                context.instantiate(getter_authority).expect("getter"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object = source_object(&mut runtime, realm_id);
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[object], ExecutionLimits::default())
            .expect("getter read");
        let number = result
            .as_number()
            .expect("live result")
            .expect("number result");

        assert!(number.strict_equals(JsNumber::from_i32(23)));
    }

    #[test]
    fn define_method_installs_exact_descriptors_names_lengths_and_function_profile() {
        let maker_authority = compile_test_function(
            "function make(){\
                return {\
                    valueOf(first,second){return second;},\
                    get toString(){return 1;},\
                    set toString(next){}\
                };\
            }",
            "make",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let maker = runtime
            .context(&realm)
            .expect("context")
            .instantiate(maker_authority)
            .expect("maker");
        let object = runtime
            .context(&realm)
            .expect("context")
            .call(&maker, &[], ExecutionLimits::default())
            .expect("method object");
        let object_id = object.object_id().expect("object id");
        let value_of_key = runtime.predefined_property_key(PredefinedAtom::ValueOf);
        let to_string_key = runtime.predefined_property_key(PredefinedAtom::ToString);

        let record = runtime
            .object_record(HeapReference::Object(object_id))
            .expect("object record");
        assert_eq!(record.property_count(), 2);
        let Some(OwnProperty::Data {
            layout: method_layout,
            value: StoredValue::Function(method),
        }) = record.own_property(&value_of_key)
        else {
            panic!("valueOf must be an own method");
        };
        assert_eq!(method_layout, PropertyLayout::data(true, true, true));
        let Some(OwnProperty::Accessor {
            layout: accessor_layout,
            getter: Some(getter),
            setter: Some(setter),
        }) = record.own_property(&to_string_key)
        else {
            panic!("toString must merge one getter and setter slot");
        };
        assert_eq!(accessor_layout, PropertyLayout::accessor(true, true));

        assert_method_function_shape(&runtime, method, "valueOf", 2);
        assert_method_function_shape(&runtime, getter, "get toString", 0);
        assert_method_function_shape(&runtime, setter, "set toString", 1);
        assert_method_function_source(&runtime, method, "valueOf(first,second){return second;}");
        assert_method_function_source(&runtime, getter, "get toString(){return 1;}");
        assert_method_function_source(&runtime, setter, "set toString(next){}");
    }

    #[test]
    fn static_object_keys_use_exact_array_index_canonicalization() {
        let maker_authority = compile_test_function(
            r#"function make(){return {2147483648:1,"2147483648":7,"4294967294":2,4294967295:3,"01":4,0:5,"":6};}"#,
            "make",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let maker = runtime
            .context(&realm)
            .expect("context")
            .instantiate(maker_authority)
            .expect("maker");
        let maker_id = maker.id().expect("maker id");
        let FunctionImplementation::Bytecode(bytecode) = &runtime
            .functions
            .get(maker_id)
            .expect("installed maker")
            .implementation
        else {
            panic!("maker must be bytecode");
        };
        let installed_atoms = runtime
            .code
            .get(bytecode.code)
            .expect("installed code")
            .templates
            .get(bytecode.template.get() as usize)
            .expect("installed template")
            .atoms
            .clone();

        let object = runtime
            .context(&realm)
            .expect("context")
            .call(&maker, &[], ExecutionLimits::default())
            .expect("object");
        let object_id = object.object_id().expect("object id");
        let record = runtime
            .object_record(HeapReference::Object(object_id))
            .expect("object record");

        let assert_data = |key: &PropertyKey, expected: i32| {
            assert!(matches!(
                record.own_property(key),
                Some(OwnProperty::Data {
                    layout,
                    value: StoredValue::Number(number),
                }) if layout == PropertyLayout::data(true, true, true)
                    && number.strict_equals(JsNumber::from_i32(expected))
            ));
        };
        for (index, expected) in [(2_147_483_648, 7), (4_294_967_294, 2), (0, 5)] {
            assert_data(
                &PropertyKey::from_index(ArrayIndex::new(index).expect("array index")),
                expected,
            );
        }

        let atom_key = |expected: &str| {
            let atom = installed_atoms
                .iter()
                .find(|atom| {
                    atom.description().is_some_and(|description| {
                        description
                            .to_utf8_lossy()
                            .is_ok_and(|text| text == expected)
                    })
                })
                .cloned()
                .expect("installed property atom");
            PropertyKey::from_validated_atom(atom)
        };
        assert_data(&atom_key("4294967295"), 3);
        assert_data(&atom_key("01"), 4);
        assert_data(&atom_key(""), 6);
        assert_eq!(
            record.property_count(),
            6,
            "numeric and quoted canonical spellings share one property"
        );
    }

    #[test]
    fn canonical_number_bigint_and_quoted_keys_share_descriptor_transitions() {
        let maker_authority = compile_test_function(
            r#"function make(){return {16:1,get 0x10n(){return 2;},set "16"(next){next;},16n(){return 4;}};}"#,
            "make",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let maker = runtime
            .context(&realm)
            .expect("context")
            .instantiate(maker_authority)
            .expect("maker");
        let object = runtime
            .context(&realm)
            .expect("context")
            .call(&maker, &[], ExecutionLimits::default())
            .expect("object");
        let object_id = object.object_id().expect("object id");
        let record = runtime
            .object_record(HeapReference::Object(object_id))
            .expect("object record");
        let key = PropertyKey::from_index(ArrayIndex::new(16).expect("array index"));

        assert_eq!(
            record.property_count(),
            1,
            "Number, BigInt, and quoted spellings must transition one canonical slot"
        );
        let Some(OwnProperty::Data {
            layout,
            value: StoredValue::Function(method),
        }) = record.own_property(&key)
        else {
            panic!("the final BigInt method must replace the merged accessor");
        };
        assert_eq!(layout, PropertyLayout::data(true, true, true));
        assert_method_function_shape(&runtime, method, "16", 0);
        assert_method_function_source(&runtime, method, "16n(){return 4;}");
    }

    #[test]
    fn define_method_rejects_a_nonconfigurable_target_without_renaming_the_method() {
        let target_authority = compile_test_function(
            "function makeTarget(){function target(){}return target;}",
            "makeTarget",
        );
        let maker_authority =
            compile_test_function("function make(){return {valueOf(){}};}", "make");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (target_maker, maker) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(target_authority).expect("target maker"),
                context.instantiate(maker_authority).expect("maker"),
            )
        };
        let target = runtime
            .context(&realm)
            .expect("context")
            .call(&target_maker, &[], ExecutionLimits::default())
            .expect("constructable nested target")
            .into_function()
            .expect("target function");
        let target_id = target.id().expect("target id");
        let object = runtime
            .context(&realm)
            .expect("context")
            .call(&maker, &[], ExecutionLimits::default())
            .expect("method object");
        let object_id = object.object_id().expect("object id");
        let value_of_key = runtime.predefined_property_key(PredefinedAtom::ValueOf);
        let Some(OwnProperty::Data {
            value: StoredValue::Function(method),
            ..
        }) = runtime
            .object_record(HeapReference::Object(object_id))
            .expect("object record")
            .own_property(&value_of_key)
        else {
            panic!("valueOf must be an own method");
        };

        let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
        let prototype_name = JsString::from_utf8("prototype").expect("property name");
        let outcome = define_static_method(
            &mut runtime,
            &StoredValue::Function(target_id),
            prototype_key.clone(),
            &prototype_name,
            method,
            DefineMethodKind::Method,
            true,
        )
        .expect("descriptor compatibility check");
        assert!(matches!(
            outcome,
            PropertyDefinitionOutcome::Failed(PropertyFailure::NotConfigurable)
        ));
        assert_method_function_shape(&runtime, method, "valueOf", 0);
        assert!(matches!(
            runtime
                .object_record(HeapReference::Function(target_id))
                .expect("target record")
                .own_property(&prototype_key),
            Some(OwnProperty::Data { layout, .. }) if !layout.is_configurable()
        ));
    }

    #[test]
    fn inherited_setter_receives_the_original_receiver_and_does_not_create_the_key() {
        let writer_authority = compile_test_function(
            "function write(object,value){return object.toString=value;}",
            "write",
        );
        let setter_authority = compile_test_function(
            "function setter(value){\"use strict\";this.valueOf=value;return 99;}",
            "setter",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (writer, setter) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(writer_authority).expect("writer"),
                context.instantiate(setter_authority).expect("setter"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let prototype = source_object(&mut runtime, realm_id);
        let object = runtime
            .allocate_ordinary_object(prototype)
            .expect("child object");
        let to_string_key = runtime.predefined_property_key(PredefinedAtom::ToString);
        let value_of_key = runtime.predefined_property_key(PredefinedAtom::ValueOf);
        runtime
            .append_accessor_property(
                HeapReference::Object(prototype),
                to_string_key.clone(),
                PropertyLayout::accessor(false, true),
                None,
                Some(setter.id().expect("setter id")),
            )
            .expect("inherited setter");
        let object_value = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");
        let assigned = runtime
            .public_value(StoredValue::Number(JsNumber::from_i32(42)))
            .expect("number");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(
                &writer,
                &[object_value, assigned],
                ExecutionLimits::default(),
            )
            .expect("inherited setter");
        let number = result
            .as_number()
            .expect("live assignment")
            .expect("number assignment");
        assert!(number.strict_equals(JsNumber::from_i32(42)));

        let object_record = runtime
            .object_record(HeapReference::Object(object))
            .expect("object record");
        assert!(
            object_record.own_property(&to_string_key).is_none(),
            "an inherited setter must not create an own property for its key"
        );
        assert!(matches!(
            object_record.own_property(&value_of_key),
            Some(OwnProperty::Data {
                layout,
                value: StoredValue::Number(number),
            }) if layout == PropertyLayout::data(true, true, true)
                && number.strict_equals(JsNumber::from_i32(42))
        ));
        assert!(
            runtime
                .object_record(HeapReference::Object(prototype))
                .expect("prototype record")
                .own_property(&value_of_key)
                .is_none(),
            "the setter receiver must be the original child, not the holder"
        );
    }

    #[test]
    fn own_getter_without_a_setter_shadows_an_inherited_setter() {
        let writer_authority =
            compile_test_function("function write(object){return object.toString=7;}", "write");
        let getter_authority = compile_test_function("function getter(){return 1;}", "getter");
        let setter_authority = compile_test_function(
            "function setter(value){\"use strict\";this.valueOf=value;}",
            "setter",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (writer, getter, setter) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(writer_authority).expect("writer"),
                context.instantiate(getter_authority).expect("getter"),
                context.instantiate(setter_authority).expect("setter"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let prototype = source_object(&mut runtime, realm_id);
        let object = runtime
            .allocate_ordinary_object(prototype)
            .expect("child object");
        let to_string_key = runtime.predefined_property_key(PredefinedAtom::ToString);
        let value_of_key = runtime.predefined_property_key(PredefinedAtom::ValueOf);
        runtime
            .append_accessor_property(
                HeapReference::Object(prototype),
                to_string_key.clone(),
                PropertyLayout::accessor(false, true),
                None,
                Some(setter.id().expect("setter id")),
            )
            .expect("prototype setter");
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                to_string_key,
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("own getter");
        let object_value = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&writer, &[object_value], ExecutionLimits::default())
            .expect("sloppy own getter write");
        let number = result
            .as_number()
            .expect("live assignment")
            .expect("number assignment");
        assert!(number.strict_equals(JsNumber::from_i32(7)));
        assert!(
            runtime
                .object_record(HeapReference::Object(object))
                .expect("object record")
                .own_property(&value_of_key)
                .is_none(),
            "the shadowed inherited setter must not run"
        );
    }

    #[test]
    fn native_setter_completion_is_discarded_while_assignment_keeps_the_rhs() {
        let writer_authority = compile_test_function(
            "function write(object){return object.toString=29;}",
            "write",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let writer = runtime
            .context(&realm)
            .expect("context")
            .instantiate(writer_authority)
            .expect("writer");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object_prototype = runtime
            .realm_object_prototype(realm_id)
            .expect("Object.prototype");
        let StoredValue::Function(native_setter) = read_heap_property(
            &runtime,
            HeapReference::Object(object_prototype),
            &runtime.predefined_property_key(PredefinedAtom::ValueOf),
        )
        .expect("Object.prototype.valueOf") else {
            panic!("Object.prototype.valueOf must be callable");
        };
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                None,
                Some(native_setter),
            )
            .expect("native setter");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&writer, &[object], ExecutionLimits::default())
            .expect("native setter");
        let number = result
            .as_number()
            .expect("live assignment")
            .expect("number assignment");
        assert!(number.strict_equals(JsNumber::from_i32(29)));
    }

    #[test]
    fn dynamic_function_setter_completion_is_discarded_while_assignment_keeps_the_rhs() {
        let writer_authority = compile_test_function(
            "function write(object){return object.toString='return 47;';}",
            "write",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let writer = runtime
            .context(&realm)
            .expect("context")
            .instantiate(writer_authority)
            .expect("writer");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let global = runtime
            .realm_global_object(realm_id)
            .expect("global object");
        let StoredValue::Function(constructor) = read_heap_property(
            &runtime,
            HeapReference::Object(global),
            &runtime.predefined_property_key(PredefinedAtom::Function),
        )
        .expect("global Function") else {
            panic!("global Function must be callable");
        };
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                None,
                Some(constructor),
            )
            .expect("Function setter");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(OxcDynamicCompiler);

        let result = runtime
            .context(&realm)
            .expect("context")
            .call_with_dynamic_function_compiler(
                &writer,
                &[object],
                ExecutionLimits::default(),
                &compiler,
            )
            .expect("dynamic Function setter");
        assert_eq!(
            result
                .as_string()
                .expect("live assignment")
                .expect("assignment string")
                .to_utf8_lossy()
                .expect("UTF-8 assignment"),
            "return 47;"
        );
    }

    #[test]
    fn function_prototype_call_setter_forwards_then_discards_the_target_completion() {
        let writer_authority = compile_test_function(
            "function write(target,value){return target.toString=value;}",
            "write",
        );
        let target_authority = compile_test_function(
            "function target(){\"use strict\";this.valueOf=41;return 99;}",
            "target",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (writer, target) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(writer_authority).expect("writer"),
                context.instantiate(target_authority).expect("target"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let call = runtime
            .functions
            .iter()
            .find_map(|(id, function)| {
                (function.native().copied()
                    == Some(NativeFunction {
                        realm: realm_id,
                        kind: NativeFunctionKind::FunctionPrototypeCall,
                    }))
                .then_some(id)
            })
            .expect("Function.prototype.call");
        runtime
            .append_accessor_property(
                HeapReference::Function(target.id().expect("target id")),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                None,
                Some(call),
            )
            .expect("forwarding native setter");
        let receiver = source_object(&mut runtime, realm_id);
        let receiver_value = runtime
            .public_value(StoredValue::Object(receiver))
            .expect("receiver root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(
                &writer,
                &[target.as_value(), receiver_value],
                ExecutionLimits::default(),
            )
            .expect("forwarded setter");
        assert_eq!(result.object_id().expect("assignment object"), receiver);
        assert!(matches!(
            runtime
                .object_record(HeapReference::Object(receiver))
                .expect("receiver record")
                .own_property(&runtime.predefined_property_key(PredefinedAtom::ValueOf)),
            Some(OwnProperty::Data {
                layout,
                value: StoredValue::Number(number),
            }) if layout == PropertyLayout::data(true, true, true)
                && number.strict_equals(JsNumber::from_i32(41))
        ));
    }

    #[test]
    fn inherited_getter_receives_the_original_object() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let reader = runtime
            .context(&realm)
            .expect("context")
            .instantiate(reader_authority)
            .expect("reader");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object_prototype = runtime
            .realm_object_prototype(realm_id)
            .expect("Object.prototype");
        let StoredValue::Function(getter) = read_heap_property(
            &runtime,
            HeapReference::Object(object_prototype),
            &runtime.predefined_property_key(PredefinedAtom::ValueOf),
        )
        .expect("Object.prototype.valueOf") else {
            panic!("Object.prototype.valueOf must be callable");
        };
        let prototype = source_object(&mut runtime, realm_id);
        let object = runtime
            .allocate_ordinary_object(prototype)
            .expect("child object");
        runtime
            .append_accessor_property(
                HeapReference::Object(prototype),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter),
                None,
            )
            .expect("inherited accessor");
        let public_object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[public_object], ExecutionLimits::default())
            .expect("inherited getter");

        assert_eq!(result.object_id().expect("object result"), object);
    }

    #[test]
    fn missing_getter_returns_undefined_and_shadows_the_prototype() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let reader = runtime
            .context(&realm)
            .expect("context")
            .instantiate(reader_authority)
            .expect("reader");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let prototype = source_object(&mut runtime, realm_id);
        let object = runtime
            .allocate_ordinary_object(prototype)
            .expect("child object");
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_data_property(
                HeapReference::Object(prototype),
                key.clone(),
                PropertyLayout::data(true, true, true),
                StoredValue::Number(JsNumber::from_i32(44)),
            )
            .expect("prototype value");
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::accessor(false, true),
                None,
                None,
            )
            .expect("getterless accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[object], ExecutionLimits::default())
            .expect("getterless read");

        assert_eq!(
            result.kind().expect("live result"),
            crate::ValueKind::Undefined
        );
    }

    #[test]
    fn get_field2_keeps_the_original_base_for_the_returned_method() {
        let invoke_authority = compile_test_function(
            "function invoke(object){return object.toString();}",
            "invoke",
        );
        let maker_authority = compile_test_function(
            "function make(method){\
                 return function getter(){return method;};\
             }",
            "make",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object_prototype = runtime
            .realm_object_prototype(realm_id)
            .expect("Object.prototype");
        let StoredValue::Function(value_of) = read_heap_property(
            &runtime,
            HeapReference::Object(object_prototype),
            &runtime.predefined_property_key(PredefinedAtom::ValueOf),
        )
        .expect("Object.prototype.valueOf") else {
            panic!("Object.prototype.valueOf must be callable");
        };
        let value_of = runtime
            .public_value(StoredValue::Function(value_of))
            .expect("valueOf root")
            .into_function()
            .expect("valueOf function");
        let (invoke, getter) = {
            let mut context = runtime.context(&realm).expect("context");
            let invoke = context.instantiate(invoke_authority).expect("invoke");
            let maker = context.instantiate(maker_authority).expect("maker");
            let getter = context
                .call(&maker, &[value_of.as_value()], ExecutionLimits::default())
                .expect("getter closure")
                .into_function()
                .expect("getter");
            (invoke, getter)
        };
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let public_object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&invoke, &[public_object], ExecutionLimits::default())
            .expect("accessor method call");

        assert_eq!(result.object_id().expect("object result"), object);
    }

    #[test]
    fn throwing_getter_preserves_getter_origin_and_property_caller() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let getter_authority = compile_test_function("function getter(){throw 37;}", "getter");
        let constant_authority =
            compile_test_function("function constant(){return 1;}", "constant");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (reader, getter, constant) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(reader_authority).expect("reader"),
                context.instantiate(getter_authority).expect("getter"),
                context.instantiate(constant_authority).expect("constant"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");

        let error = runtime
            .context(&realm)
            .expect("context")
            .call(&reader, &[object], ExecutionLimits::default())
            .expect_err("getter throw");
        let ExecutionError::Exception(exception) = error else {
            panic!("getter throw must remain a JavaScript exception");
        };
        let thrown = exception
            .thrown_value()
            .expect("explicit throw")
            .as_number()
            .expect("live throw")
            .expect("number throw");

        assert!(thrown.strict_equals(JsNumber::from_i32(37)));
        assert_eq!(exception.caller_frames().len(), 1);
        assert!(
            exception.caller_frames()[0]
                .source_text()
                .contains("object.toString")
        );
        runtime
            .context(&realm)
            .expect("context")
            .call(&constant, &[], ExecutionLimits::default())
            .expect("runtime remains reusable");
    }

    #[test]
    fn bytecode_getter_obeys_the_active_frame_limit() {
        let reader_authority =
            compile_test_function("function read(object){return object.toString;}", "read");
        let getter_authority = compile_test_function("function getter(){return 1;}", "getter");
        let mut runtime =
            Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (reader, getter) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(reader_authority).expect("reader"),
                context.instantiate(getter_authority).expect("getter"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");
        let baseline = runtime.usage();

        for _ in 0..2 {
            let error = runtime
                .context(&realm)
                .expect("context")
                .call(
                    &reader,
                    std::slice::from_ref(&object),
                    ExecutionLimits::default(),
                )
                .expect_err("getter frame exceeds limit");
            assert!(matches!(
                error,
                ExecutionError::LimitExceeded {
                    resource: RuntimeResource::Frames,
                    limit: 1,
                    observed: 2,
                }
            ));
            assert_eq!(runtime.usage(), baseline);
        }
    }

    #[test]
    fn bytecode_setter_obeys_the_active_frame_limit_without_mutating_usage() {
        let writer_authority =
            compile_test_function("function write(object){return object.toString=1;}", "write");
        let setter_authority =
            compile_test_function("function setter(value){return value;}", "setter");
        let mut runtime =
            Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (writer, setter) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context.instantiate(writer_authority).expect("writer"),
                context.instantiate(setter_authority).expect("setter"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let object = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                None,
                Some(setter.id().expect("setter id")),
            )
            .expect("accessor");
        let object = runtime
            .public_value(StoredValue::Object(object))
            .expect("object root");
        let baseline = runtime.usage();

        for _ in 0..2 {
            let error = runtime
                .context(&realm)
                .expect("context")
                .call(
                    &writer,
                    std::slice::from_ref(&object),
                    ExecutionLimits::default(),
                )
                .expect_err("setter frame exceeds limit");
            assert!(matches!(
                error,
                ExecutionError::LimitExceeded {
                    resource: RuntimeResource::Frames,
                    limit: 1,
                    observed: 2,
                }
            ));
            assert_eq!(runtime.usage(), baseline);
        }
    }

    #[test]
    fn replaced_accessor_halves_are_collected_while_the_final_pair_stays_traced() {
        let maker_authority = compile_test_function(
            "function make(){\
                let stored=0;\
                return {\
                    toString:1,\
                    get toString(){return stored;},\
                    get toString(){return stored;},\
                    set toString(next){stored=next;},\
                    set toString(next){stored=next;},\
                    toString:2,\
                    get toString(){return stored;},\
                    set toString(next){stored=next;}\
                };\
            }",
            "make",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let maker = runtime
            .context(&realm)
            .expect("context")
            .instantiate(maker_authority)
            .expect("maker");
        let baseline = runtime.usage();
        let object = runtime
            .context(&realm)
            .expect("context")
            .call(&maker, &[], ExecutionLimits::default())
            .expect("object")
            .into_object()
            .expect("ordinary object");
        let object_id = object.as_value().object_id().expect("object id");
        let live = runtime.usage();

        assert_eq!(live.heap_functions(), baseline.heap_functions() + 6);
        assert_eq!(live.heap_objects(), baseline.heap_objects() + 1);
        assert_eq!(live.binding_cells(), baseline.binding_cells() + 1);
        assert_eq!(
            live.object_properties(),
            baseline.object_properties() + 13,
            "six nonconstructable method functions add name/length while every definition reuses one target slot"
        );
        let record = runtime
            .object_record(HeapReference::Object(object_id))
            .expect("object record");
        assert_eq!(record.property_count(), 1);
        assert!(matches!(
            record.own_property(
                &runtime.predefined_property_key(PredefinedAtom::ToString)
            ),
            Some(OwnProperty::Accessor {
                layout,
                getter: Some(_),
                setter: Some(_),
            }) if layout == PropertyLayout::accessor(true, true)
        ));

        let report = runtime
            .collect_cycles()
            .expect("collect replaced accessor functions");
        assert_eq!(report.functions(), 4);
        assert_eq!(report.objects(), 0);
        assert_eq!(report.binding_cells(), 0);
        let retained = runtime.usage();
        assert_eq!(
            retained.heap_functions(),
            baseline.heap_functions() + 2,
            "the final getter and setter stay live through the accessor slot"
        );
        assert_eq!(retained.heap_objects(), baseline.heap_objects() + 1);
        assert_eq!(retained.binding_cells(), baseline.binding_cells() + 1);
        assert_eq!(
            retained.object_properties(),
            baseline.object_properties() + 5
        );

        drop(object);
        let report = runtime
            .collect_cycles()
            .expect("collect final accessor graph");
        assert_eq!(report.functions(), 2);
        assert_eq!(report.objects(), 1);
        assert_eq!(report.binding_cells(), 1);
        assert_eq!(runtime.usage(), baseline);
    }

    #[test]
    fn define_method_property_limit_failure_does_not_publish_or_charge_the_target_slot() {
        let maker_authority = compile_test_function(
            "function make(define){\
                if(define){return {valueOf(){return 1;}};}\
                return 7;\
            }",
            "make",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_object_properties(21))
            .expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let maker = runtime
            .context(&realm)
            .expect("context")
            .instantiate(maker_authority)
            .expect("maker");
        let baseline = runtime.usage();
        let define = runtime
            .public_value(StoredValue::Boolean(true))
            .expect("boolean");

        let error = runtime
            .context(&realm)
            .expect("context")
            .call(&maker, &[define], ExecutionLimits::default())
            .expect_err("target property exceeds limit");
        assert!(matches!(
            error,
            ExecutionError::LimitExceeded {
                resource: RuntimeResource::ObjectProperties,
                limit: 21,
                observed: 22,
            }
        ));
        let failed = runtime.usage();
        assert_eq!(failed.heap_functions(), baseline.heap_functions() + 1);
        assert_eq!(failed.heap_objects(), baseline.heap_objects() + 1);
        assert_eq!(
            failed.object_properties(),
            baseline.object_properties() + 2,
            "only the unpublished method function's name and length were charged"
        );

        let report = runtime.collect_cycles().expect("collect failed literal");
        assert_eq!(report.functions(), 1);
        assert_eq!(report.objects(), 1);
        assert_eq!(runtime.usage(), baseline);

        let skip = runtime
            .public_value(StoredValue::Boolean(false))
            .expect("boolean");
        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&maker, &[skip], ExecutionLimits::default())
            .expect("runtime remains reusable");
        let number = result
            .as_number()
            .expect("live result")
            .expect("number result");
        assert!(number.strict_equals(JsNumber::from_i32(7)));
    }

    #[test]
    fn dynamic_function_calls_an_accessor_before_using_its_to_string_value() {
        let (mut runtime, realm, constructor, native) = runtime_with_function_constructor();
        let object = source_object(&mut runtime, realm);
        let key = runtime.predefined_property_key(PredefinedAtom::ToString);
        runtime
            .append_accessor_property(
                HeapReference::Object(object),
                key,
                PropertyLayout::accessor(false, true),
                Some(constructor),
                None,
            )
            .expect("accessor");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());

        let Ok(dispatch) = begin_function_source_conversion(
            &mut runtime,
            native,
            vec![StoredValue::Object(object)],
            None,
            None,
            native_function_host_origin(),
            0,
            0,
            &compiler,
            &mut budget,
        ) else {
            panic!("conversion must suspend at the accessor getter");
        };
        let NativeDispatch::Call(call) = dispatch else {
            panic!("accessor getter must be called");
        };

        assert_eq!(call.function, constructor);
        assert!(call.arguments.remaining().is_empty());
        assert!(matches!(call.receiver, StoredValue::Object(id) if id == object));
    }

    #[test]
    fn global_function_executes_an_accessor_getter_and_its_bytecode_conversion_method() {
        let method_authority = compile_test_function(
            "function sourceString(){return 'return 29;';}",
            "sourceString",
        );
        let maker_authority = compile_test_function(
            "function makeGetter(method){\
                 return function sourceGetter(){return method;};\
             }",
            "makeGetter",
        );
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let getter = {
            let mut context = runtime.context(&realm).expect("context");
            let method = context
                .instantiate(method_authority)
                .expect("conversion method");
            let maker = context.instantiate(maker_authority).expect("getter maker");
            context
                .call(&maker, &[method.as_value()], ExecutionLimits::default())
                .expect("getter closure")
                .into_function()
                .expect("getter function")
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let source = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(source),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("source accessor");
        let source = runtime
            .public_value(StoredValue::Object(source))
            .expect("source root");
        let global = runtime
            .realm_global_object(realm_id)
            .expect("global object");
        let StoredValue::Function(constructor) = read_heap_property(
            &runtime,
            HeapReference::Object(global),
            &runtime.predefined_property_key(PredefinedAtom::Function),
        )
        .expect("global Function") else {
            panic!("global Function must be callable");
        };
        let constructor = runtime
            .public_value(StoredValue::Function(constructor))
            .expect("Function root")
            .into_function()
            .expect("Function value");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(OxcDynamicCompiler);
        let generated = runtime
            .context(&realm)
            .expect("context")
            .call_with_dynamic_function_compiler(
                &constructor,
                &[source],
                ExecutionLimits::default(),
                &compiler,
            )
            .expect("accessor-backed Function source")
            .into_function()
            .expect("generated function");
        let result = runtime
            .context(&realm)
            .expect("context")
            .call(&generated, &[], ExecutionLimits::default())
            .expect("generated function result");
        let number = result
            .as_number()
            .expect("live result")
            .expect("number result");

        assert!(number.strict_equals(JsNumber::from_i32(29)));
    }

    #[test]
    fn global_function_accessor_throw_prevents_dynamic_compilation() {
        let getter_authority =
            compile_test_function("function sourceGetter(){throw 53;}", "sourceGetter");
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let getter = runtime
            .context(&realm)
            .expect("context")
            .instantiate(getter_authority)
            .expect("throwing getter");
        let realm_id = runtime.context(&realm).expect("context").realm;
        let source = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(source),
                runtime.predefined_property_key(PredefinedAtom::ToString),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("source accessor");
        let source = runtime
            .public_value(StoredValue::Object(source))
            .expect("source root");
        let global = runtime
            .realm_global_object(realm_id)
            .expect("global object");
        let StoredValue::Function(constructor) = read_heap_property(
            &runtime,
            HeapReference::Object(global),
            &runtime.predefined_property_key(PredefinedAtom::Function),
        )
        .expect("global Function") else {
            panic!("global Function must be callable");
        };
        let constructor = runtime
            .public_value(StoredValue::Function(constructor))
            .expect("Function root")
            .into_function()
            .expect("Function value");
        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(NeverCompiler);

        let error = runtime
            .context(&realm)
            .expect("context")
            .call_with_dynamic_function_compiler(
                &constructor,
                &[source],
                ExecutionLimits::default(),
                &compiler,
            )
            .expect_err("getter throw must escape before compilation");
        let ExecutionError::Exception(exception) = error else {
            panic!("getter throw must remain a JavaScript exception");
        };
        let thrown = exception
            .thrown_value()
            .expect("explicit throw")
            .as_number()
            .expect("live throw")
            .expect("number throw");

        assert!(thrown.strict_equals(JsNumber::from_i32(53)));
    }

    fn assert_method_function_shape(
        runtime: &Runtime,
        function: FunctionId,
        expected_name: &str,
        expected_length: i32,
    ) {
        let record = runtime
            .object_record(HeapReference::Function(function))
            .expect("method function record");
        let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
        let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
        let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);

        let Some(OwnProperty::Data {
            layout: name_layout,
            value: StoredValue::String(name),
        }) = record.own_property(&name_key)
        else {
            panic!("method function must have an own string name");
        };
        assert_eq!(
            name_layout,
            PropertyLayout::data(false, false, true),
            "name must be nonwritable, nonenumerable, and configurable"
        );
        assert_eq!(
            name.to_utf8_lossy().expect("UTF-8 function name"),
            expected_name
        );

        let Some(OwnProperty::Data {
            layout: length_layout,
            value: StoredValue::Number(length),
        }) = record.own_property(&length_key)
        else {
            panic!("method function must have an own numeric length");
        };
        assert_eq!(
            length_layout,
            PropertyLayout::data(false, false, true),
            "length must be nonwritable, nonenumerable, and configurable"
        );
        assert!(length.strict_equals(JsNumber::from_i32(expected_length)));
        assert!(
            record.own_property(&prototype_key).is_none(),
            "ordinary methods and accessors must not have an own prototype"
        );
        assert!(
            !bytecode_function_is_constructor(runtime, function).expect("constructor profile"),
            "ordinary methods and accessors must not be constructable"
        );
    }

    fn assert_method_function_source(
        runtime: &Runtime,
        function: FunctionId,
        expected_source: &str,
    ) {
        let Ok(source) = function_to_string(runtime, function, None) else {
            panic!("method source must remain readable");
        };
        assert_eq!(
            source.to_utf8_lossy().expect("UTF-8 method source"),
            expected_source
        );
    }

    fn compile_test_function(source: &str, name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context =
                    CompilationContext::new_with_source_name(unit, Arc::from("<vm accessor test>"))
                        .expect("storage plan");
                let root = context
                    .executables()
                    .find(|executable| executable.metadata().name() == Some(name))
                    .expect("named function");
                let tree = context
                    .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                    .expect("verified function tree");
                Arc::new(tree.verified_bytecode().clone())
            },
        )
        .expect("frontend")
    }

    fn runtime_with_function_constructor()
    -> (Runtime, crate::ids::RealmId, FunctionId, NativeFunction) {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let realm = runtime.context(&realm).expect("context").realm;
        let global = runtime.realm_global_object(realm).expect("global object");
        let key = runtime.predefined_property_key(PredefinedAtom::Function);
        let StoredValue::Function(constructor) =
            read_heap_property(&runtime, HeapReference::Object(global), &key)
                .expect("Function property")
        else {
            panic!("global Function is not callable");
        };
        let native = runtime
            .functions
            .get(constructor)
            .and_then(HeapFunction::native)
            .copied()
            .expect("native Function");
        (runtime, realm, constructor, native)
    }

    fn source_object(runtime: &mut Runtime, realm: crate::ids::RealmId) -> ObjectId {
        let prototype = runtime
            .realm_object_prototype(realm)
            .expect("Object.prototype");
        runtime
            .allocate_ordinary_object(prototype)
            .expect("source object")
    }

    fn assert_native_type_error(error: NativeFailure, expected: &str) {
        let NativeFailure::Abrupt(PendingException {
            payload: PendingExceptionPayload::EngineError { kind, message },
            ..
        }) = error
        else {
            panic!("expected native JavaScript exception");
        };
        assert_eq!(kind, ExceptionKind::TypeError);
        assert_eq!(message.to_utf8_lossy().expect("UTF-8"), expected);
    }
}
