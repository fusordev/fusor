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

use std::error::Error;

use super::*;
use crate::{RuntimeLimits, value::StoredValue};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};

struct NeverCompiler;

impl OrdinaryDynamicFunctionCompiler for NeverCompiler {
    fn compile(
        &self,
        _source: OrdinaryDynamicFunctionSource,
    ) -> Result<Arc<quickjs_bytecode::VerifiedBytecode>, DynamicFunctionCompileFailure> {
        panic!("the coercion regression must fail or suspend before compilation")
    }
}

struct OxcDynamicCompiler;

impl OrdinaryDynamicFunctionCompiler for OxcDynamicCompiler {
    fn compile(
        &self,
        source: OrdinaryDynamicFunctionSource,
    ) -> Result<Arc<quickjs_bytecode::VerifiedBytecode>, DynamicFunctionCompileFailure> {
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

fn test_engine_failure(error: impl Error + Send + Sync + 'static) -> DynamicFunctionCompileFailure {
    DynamicFunctionCompileFailure::Engine {
        source: Arc::new(error),
    }
}

#[test]
fn for_in_next_rejects_a_non_iterator_cursor_after_verified_admission() {
    let authority = compile_test_function(
        "function iterate(value){for(var key in value){}}",
        "iterate",
    );
    let control_flow = authority.root().function().control_flow();
    let for_in_next_pc = control_flow
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::ForInNext)
        .expect("for_in_next")
        .decoded()
        .pc();
    let for_in_next = control_flow
        .instruction_index_at(for_in_next_pc)
        .expect("verified instruction index");

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let function = runtime
        .context(&realm)
        .expect("context")
        .instantiate(authority)
        .expect("function");
    let function = function.id().expect("function id");
    let plan = plan_frame(&runtime, function, 0, 0).expect("frame plan");
    let mut frame = create_frame(
        &mut runtime,
        plan,
        StoredValue::Undefined,
        FrameArguments::Owned(CallArguments::empty()),
        None,
        None,
    )
    .expect("frame");
    frame.instruction = for_in_next;
    frame.stack.push(StoredValue::Undefined);

    let mut executed = 1;
    let Err(error) = execute_one(&mut runtime, &mut frame, &mut executed, u64::MAX) else {
        panic!("a forged non-iterator cursor must fail closed");
    };
    assert!(matches!(
        error,
        ExecutionError::EngineFault(EngineFault::RuntimeInvariant {
            message: "verified for_in_next cursor is not a for-in iterator",
        })
    ));
}

#[test]
fn for_in_next_fuel_exhaustion_preserves_the_unvisited_candidate_for_retry() {
    let authority = compile_test_function(
        "function iterate(value){for(var key in value){return key;}}",
        "iterate",
    );
    let control_flow = authority.root().function().control_flow();
    let for_in_next_pc = control_flow
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::ForInNext)
        .expect("for_in_next")
        .decoded()
        .pc();
    let for_in_next = control_flow
        .instruction_index_at(for_in_next_pc)
        .expect("verified instruction index");

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let function = runtime
        .context(&realm)
        .expect("context")
        .instantiate(authority)
        .expect("function");
    let function = function.id().expect("function id");
    let plan = plan_frame(&runtime, function, 0, 0).expect("frame plan");
    let mut frame = create_frame(
        &mut runtime,
        plan,
        StoredValue::Undefined,
        FrameArguments::Owned(CallArguments::empty()),
        None,
        None,
    )
    .expect("frame");
    frame.instruction = for_in_next;

    let source = source_object(&mut runtime, realm_id);
    let key = runtime.predefined_property_key(PredefinedAtom::Name);
    runtime
        .append_data_property(
            HeapReference::Object(source),
            key,
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("enumerable source property");
    let (iterator, _) = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Object(source))
        .expect("iterator");
    assert_eq!(runtime.usage().for_in_entries(), 1);
    frame.stack.push(StoredValue::Object(iterator));

    let mut exhausted = 1;
    let Err(error) = execute_one(&mut runtime, &mut frame, &mut exhausted, 1) else {
        panic!("candidate scan must be precharged");
    };
    assert!(
        matches!(
            error,
            ExecutionError::InstructionLimitExceeded {
                limit: 1,
                executed: 1,
            }
        ),
        "{error:?}"
    );
    assert_eq!(runtime.usage().for_in_entries(), 1);
    assert!(matches!(
        frame.stack.as_slice(),
        [StoredValue::Object(actual)] if *actual == iterator
    ));

    let mut executed = 1;
    execute_one(&mut runtime, &mut frame, &mut executed, u64::MAX)
        .expect("the untouched candidate remains available");
    assert_eq!(runtime.usage().for_in_entries(), 2);
    assert!(matches!(
        frame.stack.as_slice(),
        [
            StoredValue::Object(actual),
            StoredValue::String(name),
            StoredValue::Boolean(false),
        ] if *actual == iterator && name.to_utf8_lossy().expect("UTF-8") == "name"
    ));
}

#[test]
fn for_in_next_precharges_snapshot_release_before_prototype_transition() {
    let (mut runtime, realm_id, function, for_in_next) = for_in_transition_test_runtime();

    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let prototype = runtime
        .allocate_ordinary_object(object_prototype)
        .expect("prototype");
    let source = runtime
        .allocate_ordinary_object_with_prototype(HeapReference::Object(prototype))
        .expect("source");
    let own_key = runtime.predefined_property_key(PredefinedAtom::Name);
    runtime
        .append_data_property(
            HeapReference::Object(source),
            own_key,
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("own property");
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Length);
    runtime
        .append_data_property(
            HeapReference::Object(prototype),
            prototype_key,
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("prototype property");

    let (prototype_iterator, _) = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Object(source))
        .expect("prototype iterator");
    assert!(matches!(
        runtime
            .advance_for_in_iterator(prototype_iterator)
            .expect("consume own candidate"),
        ForInAdvance::Yield { .. }
    ));
    let state = runtime
        .objects
        .get(prototype_iterator)
        .and_then(crate::object::HeapObject::for_in_state)
        .expect("prototype iterator state");
    assert_eq!(state.current(), Some(HeapReference::Object(source)));
    assert_eq!(state.snapshot_len(), 1);
    assert!(state.candidate().is_none());
    let usage_before_prototype = runtime.usage().for_in_entries();

    let plan = plan_frame(&runtime, function, 0, 0).expect("frame plan");
    let mut prototype_frame = create_frame(
        &mut runtime,
        plan,
        StoredValue::Undefined,
        FrameArguments::Owned(CallArguments::empty()),
        None,
        None,
    )
    .expect("frame");
    prototype_frame.instruction = for_in_next;
    prototype_frame
        .stack
        .push(StoredValue::Object(prototype_iterator));

    let mut exhausted = 1;
    let Err(error) = execute_one(&mut runtime, &mut prototype_frame, &mut exhausted, 7) else {
        panic!("prototype snapshot replacement must include old-snapshot release work");
    };
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded {
            limit: 7,
            executed: 7,
        }
    ));
    let state = runtime
        .objects
        .get(prototype_iterator)
        .and_then(crate::object::HeapObject::for_in_state)
        .expect("prototype iterator state");
    assert_eq!(state.current(), Some(HeapReference::Object(source)));
    assert_eq!(state.snapshot_len(), 1);
    assert!(state.candidate().is_none());
    assert_eq!(runtime.usage().for_in_entries(), usage_before_prototype);

    let mut executed = 1;
    execute_one(&mut runtime, &mut prototype_frame, &mut executed, u64::MAX)
        .expect("prototype transition retry");
    let state = runtime
        .objects
        .get(prototype_iterator)
        .and_then(crate::object::HeapObject::for_in_state)
        .expect("prototype iterator state");
    assert_eq!(state.current(), Some(HeapReference::Object(prototype)));
    assert!(matches!(
        prototype_frame.stack.as_slice(),
        [
            StoredValue::Object(actual),
            StoredValue::String(name),
            StoredValue::Boolean(false),
        ] if *actual == prototype_iterator
            && name.to_utf8_lossy().expect("UTF-8") == "length"
    ));
}

#[test]
fn for_in_next_precharges_snapshot_release_before_terminal_transition() {
    let (mut runtime, realm_id, function, for_in_next) = for_in_transition_test_runtime();
    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let terminal_key = runtime
        .property_key_from_string(
            &JsString::from_utf8("terminal-release-key").expect("terminal key"),
        )
        .expect("terminal property key");
    runtime
        .append_data_property(
            HeapReference::Object(object_prototype),
            terminal_key,
            PropertyLayout::data(true, true, true),
            StoredValue::Undefined,
        )
        .expect("terminal property");
    let (terminal_iterator, _) = runtime
        .allocate_for_in_iterator(realm_id, StoredValue::Object(object_prototype))
        .expect("terminal iterator");
    loop {
        let state = runtime
            .objects
            .get(terminal_iterator)
            .and_then(crate::object::HeapObject::for_in_state)
            .expect("terminal iterator state");
        if state.candidate().is_none() {
            break;
        }
        runtime
            .advance_for_in_iterator(terminal_iterator)
            .expect("consume terminal candidate");
    }
    let terminal_snapshot_len = runtime
        .objects
        .get(terminal_iterator)
        .and_then(crate::object::HeapObject::for_in_state)
        .expect("terminal iterator state")
        .snapshot_len();
    assert!(terminal_snapshot_len > 0);
    let usage_before_terminal = runtime.usage().for_in_entries();

    let plan = plan_frame(&runtime, function, 0, 0).expect("frame plan");
    let mut terminal_frame = create_frame(
        &mut runtime,
        plan,
        StoredValue::Undefined,
        FrameArguments::Owned(CallArguments::empty()),
        None,
        None,
    )
    .expect("frame");
    terminal_frame.instruction = for_in_next;
    terminal_frame
        .stack
        .push(StoredValue::Object(terminal_iterator));

    let mut exhausted = 1;
    let Err(error) = execute_one(&mut runtime, &mut terminal_frame, &mut exhausted, 2) else {
        panic!("terminal snapshot release must be precharged");
    };
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded {
            limit: 2,
            executed: 2,
        }
    ));
    let state = runtime
        .objects
        .get(terminal_iterator)
        .and_then(crate::object::HeapObject::for_in_state)
        .expect("terminal iterator state");
    assert_eq!(
        state.current(),
        Some(HeapReference::Object(object_prototype))
    );
    assert_eq!(state.snapshot_len(), terminal_snapshot_len);
    assert_eq!(runtime.usage().for_in_entries(), usage_before_terminal);

    let mut executed = 1;
    execute_one(&mut runtime, &mut terminal_frame, &mut executed, u64::MAX)
        .expect("terminal transition retry");
    let state = runtime
        .objects
        .get(terminal_iterator)
        .and_then(crate::object::HeapObject::for_in_state)
        .expect("terminal iterator state");
    assert_eq!(state.current(), None);
    assert_eq!(state.snapshot_len(), 0);
    assert_eq!(
        runtime.usage().for_in_entries(),
        usage_before_terminal.saturating_sub(usize_to_u64(terminal_snapshot_len))
    );
    assert!(matches!(
        terminal_frame.stack.as_slice(),
        [
            StoredValue::Object(actual),
            StoredValue::Undefined,
            StoredValue::Boolean(true),
        ] if *actual == terminal_iterator
    ));
}

fn for_in_transition_test_runtime() -> (Runtime, RealmId, FunctionId, InstructionIndex) {
    let authority = compile_test_function(
        "function iterate(value){for(var key in value){return key;}}",
        "iterate",
    );
    let control_flow = authority.root().function().control_flow();
    let for_in_next_pc = control_flow
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::ForInNext)
        .expect("for_in_next")
        .decoded()
        .pc();
    let for_in_next = control_flow
        .instruction_index_at(for_in_next_pc)
        .expect("verified instruction index");

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let function = runtime
        .context(&realm)
        .expect("context")
        .instantiate(authority)
        .expect("function")
        .id()
        .expect("function id");
    (runtime, realm_id, function, for_in_next)
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
    let exotic_key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToPrimitive);
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
        &[],
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
    assert_eq!(
        NativeContinuation::IntrinsicGet(IntrinsicGetContinuation::NumberConstructor {
            new_target: constructor,
            value: JsNumber::from_i32(1),
        })
        .retained_values(),
        1
    );
}

#[test]
fn boolean_constructor_wrapper_uses_new_target_prototype_and_realm_fallback() {
    let (mut runtime, realm, new_target, _native) = runtime_with_function_constructor();
    let function_prototype = runtime
        .realm_function_prototype(realm)
        .expect("Function.prototype");
    let wrapper = immediate_boolean_wrapper(&mut runtime, new_target, true);
    assert_eq!(
        runtime.boxed_boolean(wrapper).expect("live wrapper"),
        Some(true)
    );
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Function(function_prototype))
    );

    let custom_prototype = source_object(&mut runtime, realm);
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let constructor = runtime
        .functions
        .get_mut(new_target)
        .expect("new target function");
    let replaced = constructor.object.replace_existing_with_data(
        &prototype_key,
        PropertyLayout::data(false, false, false),
        StoredValue::Object(custom_prototype),
    );
    assert!(replaced.is_some());
    let wrapper = immediate_boolean_wrapper(&mut runtime, new_target, false);
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Object(custom_prototype))
    );

    let constructor = runtime
        .functions
        .get_mut(new_target)
        .expect("new target function");
    let replaced = constructor.object.replace_existing_with_data(
        &prototype_key,
        PropertyLayout::data(false, false, false),
        StoredValue::Number(JsNumber::from_i32(1)),
    );
    assert!(replaced.is_some());
    let wrapper = immediate_boolean_wrapper(&mut runtime, new_target, true);
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Object(
            runtime
                .realm_boolean_prototype(realm)
                .expect("Boolean.prototype")
        ))
    );
}

#[test]
fn number_constructor_wrapper_uses_new_target_prototype_and_realm_fallback() {
    let (mut runtime, realm, new_target, _native) = runtime_with_function_constructor();
    let function_prototype = runtime
        .realm_function_prototype(realm)
        .expect("Function.prototype");
    let negative_zero = JsNumber::from_f64(-0.0);
    let wrapper = immediate_number_wrapper(&mut runtime, new_target, negative_zero);
    assert!(
        runtime
            .boxed_number(wrapper)
            .expect("live wrapper")
            .expect("Number payload")
            .same_value(negative_zero)
    );
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Function(function_prototype))
    );

    let custom_prototype = source_object(&mut runtime, realm);
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let constructor = runtime
        .functions
        .get_mut(new_target)
        .expect("new target function");
    let replaced = constructor.object.replace_existing_with_data(
        &prototype_key,
        PropertyLayout::data(false, false, false),
        StoredValue::Object(custom_prototype),
    );
    assert!(replaced.is_some());
    let wrapper = immediate_number_wrapper(&mut runtime, new_target, JsNumber::from_i32(7));
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Object(custom_prototype))
    );

    let constructor = runtime
        .functions
        .get_mut(new_target)
        .expect("new target function");
    let replaced = constructor.object.replace_existing_with_data(
        &prototype_key,
        PropertyLayout::data(false, false, false),
        StoredValue::Null,
    );
    assert!(replaced.is_some());
    let wrapper = immediate_number_wrapper(&mut runtime, new_target, JsNumber::from_i32(9));
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Object(
            runtime
                .realm_number_prototype(realm)
                .expect("Number.prototype")
        ))
    );
}

#[test]
fn boolean_constructor_suspends_for_accessor_backed_new_target_prototype() {
    let (mut runtime, realm, boolean_constructor, native, new_target) =
        runtime_with_boolean_constructor_prototype_getter(
            "function getter(){\"use strict\";return this.valueOf;}",
        );
    let custom_prototype = source_object(&mut runtime, realm);
    let value_of_key = runtime.predefined_property_key(PredefinedAtom::ValueOf);
    runtime
        .append_data_property(
            HeapReference::Function(new_target),
            value_of_key,
            PropertyLayout::data(true, true, true),
            StoredValue::Object(custom_prototype),
        )
        .expect("newTarget receiver marker");

    let heap_objects_before_get = runtime.usage().heap_objects();
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        boolean_constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![StoredValue::Boolean(true)]),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("accessor-backed Boolean construction must start");
    };
    let NativeDispatch::Call(call) = dispatch else {
        panic!("newTarget.prototype getter must suspend Boolean construction");
    };
    assert!(matches!(
        call.receiver,
        StoredValue::Function(function) if function == new_target
    ));
    assert!(matches!(
        call.continuations.as_slice(),
        [NativeContinuation::IntrinsicGet(
            IntrinsicGetContinuation::BooleanConstructor {
                new_target: retained_target,
                value: true,
            }
        )] if *retained_target == new_target
    ));
    assert_eq!(native_continuation_values(&call.continuations), 1);
    assert_eq!(runtime.usage().heap_objects(), heap_objects_before_get);
    let Ok(dispatch) = resolve_native_dispatch(
        &mut runtime,
        NativeDispatch::Call(call),
        &[],
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("getter dispatch must resolve");
    };
    let NativeDispatch::Frame(frame) = dispatch else {
        panic!("bytecode getter must produce an execution frame");
    };
    let result = execute_prepared_frames_with_dynamic_budget(
        &mut runtime,
        vec![frame],
        ExecutionLimits::default(),
        None,
        None,
        &mut budget,
    )
    .expect("resumed Boolean construction");
    let StoredValue::Object(wrapper) = result else {
        panic!("Boolean construction must return a wrapper");
    };

    assert_eq!(
        runtime.boxed_boolean(wrapper).expect("live wrapper"),
        Some(true)
    );
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Object(custom_prototype))
    );
}

#[test]
fn number_constructor_suspends_for_accessor_backed_new_target_prototype() {
    let (mut runtime, realm, number_constructor, native, new_target) =
        runtime_with_number_constructor_prototype_getter(
            "function getter(){\"use strict\";return this.valueOf;}",
        );
    let custom_prototype = source_object(&mut runtime, realm);
    let value_of_key = runtime.predefined_property_key(PredefinedAtom::ValueOf);
    runtime
        .append_data_property(
            HeapReference::Function(new_target),
            value_of_key,
            PropertyLayout::data(true, true, true),
            StoredValue::Object(custom_prototype),
        )
        .expect("newTarget receiver marker");

    let heap_objects_before_get = runtime.usage().heap_objects();
    let negative_zero = JsNumber::from_f64(-0.0);
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        number_constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![StoredValue::Number(negative_zero)]),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("accessor-backed Number construction must start");
    };
    let NativeDispatch::Call(call) = dispatch else {
        panic!("newTarget.prototype getter must suspend Number construction");
    };
    assert!(matches!(
        call.continuations.as_slice(),
        [NativeContinuation::IntrinsicGet(
            IntrinsicGetContinuation::NumberConstructor {
                new_target: retained_target,
                value,
            }
        )] if *retained_target == new_target && value.same_value(negative_zero)
    ));
    assert_eq!(native_continuation_values(&call.continuations), 1);
    assert_eq!(runtime.usage().heap_objects(), heap_objects_before_get);

    let Ok(dispatch) = resolve_native_dispatch(
        &mut runtime,
        NativeDispatch::Call(call),
        &[],
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("getter dispatch must resolve");
    };
    let NativeDispatch::Frame(frame) = dispatch else {
        panic!("bytecode getter must produce an execution frame");
    };
    let result = execute_prepared_frames_with_dynamic_budget(
        &mut runtime,
        vec![frame],
        ExecutionLimits::default(),
        None,
        None,
        &mut budget,
    )
    .expect("resumed Number construction");
    let StoredValue::Object(wrapper) = result else {
        panic!("Number construction must return a wrapper");
    };

    assert!(
        runtime
            .boxed_number(wrapper)
            .expect("live wrapper")
            .expect("Number payload")
            .same_value(negative_zero)
    );
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(wrapper))
            .expect("wrapper")
            .prototype(),
        Some(HeapReference::Object(custom_prototype))
    );
}

#[test]
fn boolean_constructor_getter_throw_precedes_wrapper_allocation() {
    let (mut runtime, _realm, boolean_constructor, native, new_target) =
        runtime_with_boolean_constructor_prototype_getter("function getter(){throw 41;}");

    let heap_objects_before_get = runtime.usage().heap_objects();
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        boolean_constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![StoredValue::Boolean(true)]),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("throwing prototype getter must start");
    };
    assert_eq!(runtime.usage().heap_objects(), heap_objects_before_get);
    let Ok(dispatch) =
        resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("throwing getter dispatch must resolve");
    };
    let NativeDispatch::Frame(frame) = dispatch else {
        panic!("bytecode getter must produce an execution frame");
    };
    let error = execute_prepared_frames_with_dynamic_budget(
        &mut runtime,
        vec![frame],
        ExecutionLimits::default(),
        None,
        None,
        &mut budget,
    )
    .expect_err("prototype getter throw must escape");
    assert_eq!(runtime.usage().heap_objects(), heap_objects_before_get);
    let ExecutionError::Exception(exception) = error else {
        panic!("getter throw must remain a JavaScript exception");
    };
    assert_eq!(exception.kind(), None);
    let thrown = exception.thrown_value().expect("explicit getter throw");
    let number = thrown
        .as_number()
        .expect("live thrown value")
        .expect("number throw");
    assert!(number.strict_equals(JsNumber::from_i32(41)));
}

#[test]
fn boolean_constructor_accessor_continuation_obeys_frame_and_value_limits() {
    let (mut runtime, _realm, constructor, native, new_target) =
        runtime_with_boolean_constructor_prototype_getter(
            "function getter(){\"use strict\";return this;}",
        );
    runtime.limits.max_active_frames = 1;
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let dispatch =
        begin_test_boolean_construction(&mut runtime, constructor, native, new_target, &mut budget);
    let Err(error) = resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("getter plus intrinsic continuation must exceed one active frame");
    };
    assert!(matches!(
        error,
        NativeFailure::Execution(ExecutionError::LimitExceeded {
            resource: RuntimeResource::Frames,
            limit: 1,
            observed: 2,
        })
    ));

    let (mut runtime, _realm, constructor, native, new_target) =
        runtime_with_boolean_constructor_prototype_getter(
            "function getter(){\"use strict\";return this;}",
        );
    runtime.limits.max_active_frame_values = 2;
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let dispatch =
        begin_test_boolean_construction(&mut runtime, constructor, native, new_target, &mut budget);
    let Err(error) = resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("getter receiver plus retained newTarget must exceed two frame values");
    };
    assert!(matches!(
        error,
        NativeFailure::Execution(ExecutionError::LimitExceeded {
            resource: RuntimeResource::FrameValues,
            limit: 2,
            observed: 3,
        })
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn object_prototype_to_string_boxes_boolean_before_symbol_tag_getter() {
    let getter_authority = compile_test_function(
        "function getter(){\"use strict\";return typeof this;}",
        "getter",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let getter = runtime
        .context(&realm)
        .expect("context")
        .instantiate(getter_authority)
        .expect("getter");
    let realm = runtime.context(&realm).expect("context").realm;
    let (to_string, native) = object_prototype_to_string_native(&runtime, realm);
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm)
        .expect("Boolean.prototype");
    let tag_key = runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag);
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            tag_key,
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("Boolean @@toStringTag getter");

    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        to_string,
        native,
        CallInputs {
            receiver: StoredValue::Boolean(true),
            arguments: CallArguments::empty(),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("Boolean @@toStringTag access must start");
    };
    let NativeDispatch::Call(call) = dispatch else {
        panic!("Boolean @@toStringTag getter must suspend toString");
    };
    let StoredValue::Object(boxed_receiver) = call.receiver else {
        panic!("@@toStringTag getter must receive a boxed Boolean");
    };
    assert_eq!(
        runtime
            .boxed_boolean(boxed_receiver)
            .expect("boxed receiver"),
        Some(true)
    );
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(boxed_receiver))
            .expect("boxed receiver")
            .prototype(),
        Some(HeapReference::Object(boolean_prototype))
    );
    assert!(matches!(
        call.continuations.as_slice(),
        [NativeContinuation::IntrinsicGet(
            IntrinsicGetContinuation::ObjectPrototypeToString {
                default_tag: ObjectPrototypeTag::Boolean,
                temporary_receiver: Some(temporary_receiver),
            }
        )] if *temporary_receiver == boxed_receiver
    ));
    assert_eq!(native_continuation_values(&call.continuations), 1);

    let Ok(dispatch) = resolve_native_dispatch(
        &mut runtime,
        NativeDispatch::Call(call),
        &[],
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("Boolean tag getter dispatch must resolve");
    };
    let NativeDispatch::Frame(frame) = dispatch else {
        panic!("bytecode tag getter must produce an execution frame");
    };
    let result = execute_prepared_frames_with_dynamic_budget(
        &mut runtime,
        vec![frame],
        ExecutionLimits::default(),
        None,
        None,
        &mut budget,
    )
    .expect("resumed Object.prototype.toString");
    let StoredValue::String(result) = result else {
        panic!("Object.prototype.toString must return a string");
    };
    assert_eq!(result.to_utf8_lossy().expect("UTF-8"), "[object object]");
}

#[test]
fn object_prototype_to_string_boolean_boxing_is_limit_checked_and_transient() {
    let mut limited = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = limited.create_realm().expect("realm");
    let realm = limited.context(&realm).expect("context").realm;
    let (to_string, native) = object_prototype_to_string_native(&limited, realm);
    let usage_before = limited.usage();
    limited.limits.max_heap_objects = usage_before.heap_objects();
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Err(error) = dispatch_native_call(
        &mut limited,
        to_string,
        native,
        CallInputs {
            receiver: StoredValue::Boolean(true),
            arguments: CallArguments::empty(),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("Boolean ToObject must honor the heap-object limit");
    };
    let NativeFailure::Execution(ExecutionError::LimitExceeded {
        resource,
        limit,
        observed,
    }) = error
    else {
        panic!("Boolean ToObject must report the exact heap-object limit");
    };
    assert_eq!(resource, RuntimeResource::HeapObjects);
    assert_eq!(limit, usage_before.heap_objects());
    assert_eq!(observed, usage_before.heap_objects() + 1);
    assert_eq!(limited.usage(), usage_before);

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm = runtime.context(&realm).expect("context").realm;
    let (to_string, native) = object_prototype_to_string_native(&runtime, realm);
    let usage_before = runtime.usage();
    runtime.limits.max_heap_objects = usage_before.heap_objects() + 1;
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Ok(NativeDispatch::Immediate(StoredValue::String(result))) = dispatch_native_call(
        &mut runtime,
        to_string,
        native,
        CallInputs {
            receiver: StoredValue::Boolean(true),
            arguments: CallArguments::empty(),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("Boolean ToObject must finish immediately without a tag getter");
    };
    assert_eq!(result.to_utf8_lossy().expect("UTF-8"), "[object Boolean]");
    assert_eq!(runtime.usage(), usage_before);
}

#[test]
fn object_prototype_to_string_string_boxing_charges_and_releases_the_length_property() {
    let mut limited = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = limited.create_realm().expect("realm");
    let realm = limited.context(&realm).expect("context").realm;
    let (to_string, native) = object_prototype_to_string_native(&limited, realm);
    let usage_before = limited.usage();
    limited.limits.max_object_properties = usage_before.object_properties();
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Err(error) = dispatch_native_call(
        &mut limited,
        to_string,
        native,
        CallInputs {
            receiver: StoredValue::String(JsString::from_utf8("xy").expect("String")),
            arguments: CallArguments::empty(),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("String ToObject must honor the wrapper length-property limit");
    };
    let NativeFailure::Execution(ExecutionError::LimitExceeded {
        resource,
        limit,
        observed,
    }) = error
    else {
        panic!("String ToObject must report the exact object-property limit");
    };
    assert_eq!(resource, RuntimeResource::ObjectProperties);
    assert_eq!(limit, usage_before.object_properties());
    assert_eq!(observed, usage_before.object_properties() + 1);
    assert_eq!(limited.usage(), usage_before);

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm = runtime.context(&realm).expect("context").realm;
    let (to_string, native) = object_prototype_to_string_native(&runtime, realm);
    let usage_before = runtime.usage();
    runtime.limits.max_heap_objects = usage_before.heap_objects() + 1;
    runtime.limits.max_object_properties = usage_before.object_properties() + 1;
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Ok(NativeDispatch::Immediate(StoredValue::String(result))) = dispatch_native_call(
        &mut runtime,
        to_string,
        native,
        CallInputs {
            receiver: StoredValue::String(JsString::from_utf8("xy").expect("String")),
            arguments: CallArguments::empty(),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("String ToObject must finish immediately without a tag getter");
    };
    assert_eq!(result.to_utf8_lossy().expect("UTF-8"), "[object String]");
    assert_eq!(runtime.usage(), usage_before);
}

#[test]
fn object_prototype_to_string_reclaims_unescaped_boolean_receivers_within_one_execution() {
    let (mut runtime, realm, repeat, _getter, to_string) =
        runtime_with_boolean_tag_getter_and_invoker(
            RuntimeLimits::default(),
            "function getter(){\
                 \"use strict\";\
                 let unreachable={valueOf:this};\
                 return 7;\
             }",
            "function repeat(target){\
                 let survivor={valueOf:\"alive\"};\
                 target.call(true);\
                 target.call(false);\
                 let prefix=survivor.valueOf;\
                 survivor=null;\
                 return prefix+target.call(true);\
             }",
            "repeat",
        );
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 3;

    let result = runtime
        .context(&realm)
        .expect("context")
        .call(
            &repeat,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect("repeated Boolean tagging");

    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string result")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "alive[object Boolean]"
    );
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn object_prototype_to_string_resumes_native_symbol_tag_getters_without_leaking() {
    let repeat_authority = compile_test_function(
        "function repeat(target){\
             target.call(true);\
             return target.call(false);\
         }",
        "repeat",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let repeat = runtime
        .context(&realm)
        .expect("context")
        .instantiate(repeat_authority)
        .expect("repeat");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let StoredValue::Function(native_getter) = read_heap_property(
        &runtime,
        HeapReference::Object(object_prototype),
        &runtime.predefined_property_key(PredefinedAtom::ValueOf),
    )
    .expect("Object.prototype.valueOf") else {
        panic!("Object.prototype.valueOf must be callable");
    };
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm_id)
        .expect("Boolean.prototype");
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag),
            PropertyLayout::accessor(true, true),
            Some(native_getter),
            None,
        )
        .expect("native Object.prototype.valueOf @@toStringTag getter");
    let (to_string, _) = object_prototype_to_string_native(&runtime, realm_id);
    let to_string = runtime
        .public_value(StoredValue::Function(to_string))
        .expect("Object.prototype.toString root");
    runtime.collect_cycles().expect("settle setup roots");
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;

    let result = runtime
        .context(&realm)
        .expect("context")
        .call(
            &repeat,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect("native tag getter");

    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string result")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "[object Boolean]"
    );
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn object_prototype_to_string_reclaims_boolean_receiver_after_native_getter_throw() {
    let invoke_authority = compile_test_function(
        "function invoke(target){return target.call(true);}",
        "invoke",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let invoke = runtime
        .context(&realm)
        .expect("context")
        .instantiate(invoke_authority)
        .expect("invoke");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let function_prototype = runtime
        .realm_function_prototype(realm_id)
        .expect("Function.prototype");
    let StoredValue::Function(native_getter) = read_heap_property(
        &runtime,
        HeapReference::Function(function_prototype),
        &runtime.predefined_property_key(PredefinedAtom::ToString),
    )
    .expect("Function.prototype.toString") else {
        panic!("Function.prototype.toString must be callable");
    };
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm_id)
        .expect("Boolean.prototype");
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag),
            PropertyLayout::accessor(true, true),
            Some(native_getter),
            None,
        )
        .expect("throwing native Boolean @@toStringTag getter");
    let (to_string, _) = object_prototype_to_string_native(&runtime, realm_id);
    let to_string = runtime
        .public_value(StoredValue::Function(to_string))
        .expect("Object.prototype.toString root");
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;

    let error = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect_err("native tag getter receiver error");
    let ExecutionError::Exception(exception) = error else {
        panic!("native getter failure must remain a JavaScript exception");
    };

    assert_eq!(exception.kind(), Some(ExceptionKind::TypeError));
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn object_prototype_to_string_preserves_a_boolean_receiver_escaped_to_the_heap() {
    let maker_authority = compile_test_function(
        "function make(holder){\
             return function getter(){\
                 \"use strict\";\
                 holder.valueOf=this;\
                 return \"Escaped\";\
             };\
         }",
        "make",
    );
    let invoke_authority = compile_test_function(
        "function invoke(target){return target.call(false);}",
        "invoke",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm_id)
        .expect("Boolean.prototype");
    let public_boolean_prototype = runtime
        .public_value(StoredValue::Object(boolean_prototype))
        .expect("Boolean.prototype root");
    let (maker, invoke) = {
        let mut context = runtime.context(&realm).expect("context");
        (
            context.instantiate(maker_authority).expect("maker"),
            context.instantiate(invoke_authority).expect("invoker"),
        )
    };
    let getter = runtime
        .context(&realm)
        .expect("context")
        .call(
            &maker,
            std::slice::from_ref(&public_boolean_prototype),
            ExecutionLimits::default(),
        )
        .expect("getter closure")
        .into_function()
        .expect("getter");
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag),
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("Boolean @@toStringTag getter");
    let (to_string, _) = object_prototype_to_string_native(&runtime, realm_id);
    let to_string = runtime
        .public_value(StoredValue::Function(to_string))
        .expect("Object.prototype.toString root");
    runtime.collect_cycles().expect("settle setup roots");
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;

    let result = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect("escaped Boolean tag receiver");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string result")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "[object Escaped]"
    );
    let StoredValue::Object(wrapper) = read_heap_property(
        &runtime,
        HeapReference::Object(boolean_prototype),
        &runtime.predefined_property_key(PredefinedAtom::ValueOf),
    )
    .expect("escaped wrapper") else {
        panic!("Boolean.prototype.valueOf must retain the boxed receiver");
    };

    assert_eq!(
        runtime.boxed_boolean(wrapper).expect("live wrapper"),
        Some(false)
    );
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
    assert_eq!(runtime.usage().public_roots(), baseline.public_roots());
}

#[test]
fn object_prototype_to_string_preserves_a_boolean_receiver_captured_by_the_getter() {
    let maker_authority = compile_test_function(
        "function make(){\
             let saved;\
             return function getter(read){\
                 \"use strict\";\
                 if(read)return saved;\
                 saved=this;\
                 return \"Captured\";\
             };\
         }",
        "make",
    );
    let invoke_authority = compile_test_function(
        "function invoke(target){return target.call(false);}",
        "invoke",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (maker, invoke) = {
        let mut context = runtime.context(&realm).expect("context");
        (
            context.instantiate(maker_authority).expect("maker"),
            context.instantiate(invoke_authority).expect("invoker"),
        )
    };
    let getter = runtime
        .context(&realm)
        .expect("context")
        .call(&maker, &[], ExecutionLimits::default())
        .expect("getter closure")
        .into_function()
        .expect("getter");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm_id)
        .expect("Boolean.prototype");
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag),
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("Boolean @@toStringTag getter");
    let (to_string, _) = object_prototype_to_string_native(&runtime, realm_id);
    let to_string = runtime
        .public_value(StoredValue::Function(to_string))
        .expect("Object.prototype.toString root");
    runtime.collect_cycles().expect("settle setup roots");
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;

    let result = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect("captured Boolean tag receiver");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string result")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "[object Captured]"
    );
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
    assert_eq!(runtime.usage().public_roots(), baseline.public_roots());

    let wrapper = {
        let mut context = runtime.context(&realm).expect("context");
        let read = context.boolean(true);
        context
            .call(&getter, &[read], ExecutionLimits::default())
            .expect("captured wrapper")
    };
    let wrapper = wrapper.object_id().expect("boxed Boolean result");
    assert_eq!(
        runtime.boxed_boolean(wrapper).expect("live wrapper"),
        Some(false)
    );
    assert!(matches!(
        read_heap_property(
            &runtime,
            HeapReference::Object(boolean_prototype),
            &runtime.predefined_property_key(PredefinedAtom::ValueOf),
        )
        .expect("Boolean.prototype.valueOf"),
        StoredValue::Function(_)
    ));
}

#[test]
fn object_prototype_to_string_reclaims_boolean_receiver_after_getter_throw() {
    let (mut runtime, realm, invoke, _getter, to_string) =
        runtime_with_boolean_tag_getter_and_invoker(
            RuntimeLimits::default(),
            "function getter(){\"use strict\";throw 41;}",
            "function invoke(target){return target.call(true);}",
            "invoke",
        );
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;

    let error = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect_err("tag getter throw");
    let ExecutionError::Exception(exception) = error else {
        panic!("getter throw must remain a JavaScript exception");
    };
    let thrown = exception
        .thrown_value()
        .expect("explicit getter throw")
        .as_number()
        .expect("live throw")
        .expect("number throw");

    assert!(thrown.strict_equals(JsNumber::from_i32(41)));
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn object_prototype_to_string_reclaims_boolean_receiver_after_getter_frame_limit() {
    let getter_authority =
        compile_test_function("function getter(){return \"Boolean\";}", "getter");
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_active_frames(1)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let getter = runtime
        .context(&realm)
        .expect("context")
        .instantiate(getter_authority)
        .expect("getter");
    let realm = runtime.context(&realm).expect("context").realm;
    let (to_string, native) = object_prototype_to_string_native(&runtime, realm);
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm)
        .expect("Boolean.prototype");
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag),
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("Boolean @@toStringTag getter");
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;

    for _ in 0..2 {
        let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
        let Ok(dispatch) = dispatch_native_call(
            &mut runtime,
            to_string,
            native,
            CallInputs {
                receiver: StoredValue::Boolean(true),
                arguments: CallArguments::empty(),
                new_target: None,
            },
            None,
            Some(native_function_host_origin()),
            0,
            0,
            None,
            &mut budget,
        ) else {
            panic!("Boolean tag getter dispatch must start");
        };
        assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
        let Err(error) =
            resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
        else {
            panic!("getter frame must exceed the limit");
        };
        assert!(matches!(
            error,
            NativeFailure::Execution(ExecutionError::LimitExceeded {
                resource: RuntimeResource::Frames,
                limit: 1,
                observed: 2,
            })
        ));
        assert_eq!(runtime.usage(), baseline);
    }
}

#[test]
fn object_prototype_to_string_reclaims_boolean_receiver_after_getter_value_limit() {
    let getter_authority =
        compile_test_function("function getter(){return \"Boolean\";}", "getter");
    let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_active_frame_values(1))
        .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let getter = runtime
        .context(&realm)
        .expect("context")
        .instantiate(getter_authority)
        .expect("getter");
    let realm = runtime.context(&realm).expect("context").realm;
    let (to_string, native) = object_prototype_to_string_native(&runtime, realm);
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm)
        .expect("Boolean.prototype");
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag),
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("Boolean @@toStringTag getter");
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        to_string,
        native,
        CallInputs {
            receiver: StoredValue::Boolean(true),
            arguments: CallArguments::empty(),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("Boolean tag getter dispatch must start");
    };
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
    let Err(error) = resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("getter receiver and continuation must exceed one frame value");
    };
    let NativeFailure::Execution(ExecutionError::LimitExceeded {
        resource,
        limit,
        observed,
    }) = error
    else {
        panic!("getter frame-value failure must remain a limit error");
    };
    assert_eq!(resource, RuntimeResource::FrameValues);
    assert_eq!(limit, 1);
    assert_eq!(observed, 3);
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn object_prototype_to_string_preserves_a_thrown_boolean_receiver() {
    let (mut runtime, realm, invoke, _getter, to_string) =
        runtime_with_boolean_tag_getter_and_invoker(
            RuntimeLimits::default(),
            "function getter(){\"use strict\";throw this;}",
            "function invoke(target){return target.call(true);}",
            "invoke",
        );
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects() + 1;

    let error = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect_err("tag getter throw");
    let ExecutionError::Exception(exception) = error else {
        panic!("getter throw must remain a JavaScript exception");
    };
    let wrapper = exception
        .thrown_value()
        .expect("explicit getter throw")
        .object_id()
        .expect("boxed Boolean throw");

    assert_eq!(
        runtime.boxed_boolean(wrapper).expect("live wrapper"),
        Some(true)
    );
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
    assert_eq!(runtime.usage().public_roots(), baseline.public_roots() + 1);

    drop(exception);
    runtime.collect_cycles().expect("collection");
    assert_eq!(runtime.usage(), baseline);
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
            realm,
        })
        .retained_values(),
        2
    );
    assert_eq!(
        continuation(PropertyKeyTarget::Write {
            base: StoredValue::Undefined,
            value: StoredValue::Undefined,
            strict: false,
            realm,
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
    let (mut runtime, realm, constructor, _native) = runtime_with_function_constructor();
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
    assert_eq!(
        continuation(OperatorPrimitiveTarget::NumberIntrinsic { new_target: None })
            .retained_values(),
        1
    );
    assert_eq!(
        continuation(OperatorPrimitiveTarget::NumberIntrinsic {
            new_target: Some(constructor),
        })
        .retained_values(),
        2
    );
    assert_eq!(
        continuation(OperatorPrimitiveTarget::NumberToString {
            number: JsNumber::from_i32(1),
        })
        .retained_values(),
        2
    );
}

#[test]
fn number_to_string_radix_conversion_obeys_frame_and_value_limits() {
    let (mut runtime, to_string, native, radix) =
        runtime_with_number_to_string_radix_method(RuntimeLimits::default());
    runtime.limits.max_active_frames = 1;
    let baseline = runtime.usage();
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let dispatch = begin_test_number_to_string(&mut runtime, to_string, native, radix, &mut budget);
    let NativeDispatch::Call(call) = &dispatch else {
        panic!("radix valueOf must suspend Number.prototype.toString");
    };
    assert!(matches!(
        call.continuations.as_slice(),
        [NativeContinuation::OperatorPrimitive(
            OperatorPrimitiveContinuation {
                target: OperatorPrimitiveTarget::NumberToString { number },
                ..
            }
        )] if number.same_value(JsNumber::from_i32(31))
    ));
    assert_eq!(native_continuation_values(&call.continuations), 2);
    let Err(error) = resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("radix method plus continuation must exceed one active frame");
    };
    assert!(matches!(
        error,
        NativeFailure::Execution(ExecutionError::LimitExceeded {
            resource: RuntimeResource::Frames,
            limit: 1,
            observed: 2,
        })
    ));
    assert_eq!(runtime.usage(), baseline);

    let (mut runtime, to_string, native, radix) =
        runtime_with_number_to_string_radix_method(RuntimeLimits::default());
    runtime.limits.max_active_frame_values = 2;
    let baseline = runtime.usage();
    let mut budget = DynamicCompilationBudget::new(ExecutionLimits::default());
    let dispatch = begin_test_number_to_string(&mut runtime, to_string, native, radix, &mut budget);
    let Err(error) = resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("radix receiver plus retained Number must exceed two frame values");
    };
    let NativeFailure::Execution(ExecutionError::LimitExceeded {
        resource,
        limit,
        observed,
    }) = error
    else {
        panic!("radix frame-value failure must remain a limit error");
    };
    assert_eq!(resource, RuntimeResource::FrameValues);
    assert_eq!(limit, 2);
    assert_eq!(observed, 4);
    assert_eq!(runtime.usage(), baseline);
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
    let maker_authority = compile_test_function("function make(){return {valueOf(){}};}", "make");
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
    let constant_authority = compile_test_function("function constant(){return 1;}", "constant");
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
    let setter_authority = compile_test_function("function setter(value){return value;}", "setter");
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
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_object_properties(55)).expect("runtime");
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
            limit: 55,
            observed: 56,
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

fn assert_method_function_source(runtime: &Runtime, function: FunctionId, expected_source: &str) {
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

fn runtime_with_function_constructor() -> (Runtime, RealmId, FunctionId, NativeFunction) {
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

fn runtime_with_boolean_constructor_prototype_getter(
    getter_source: &str,
) -> (Runtime, RealmId, FunctionId, NativeFunction, FunctionId) {
    runtime_with_primitive_constructor_prototype_getter(getter_source, PredefinedAtom::Boolean)
}

fn runtime_with_number_constructor_prototype_getter(
    getter_source: &str,
) -> (Runtime, RealmId, FunctionId, NativeFunction, FunctionId) {
    runtime_with_primitive_constructor_prototype_getter(getter_source, PredefinedAtom::Number)
}

fn runtime_with_primitive_constructor_prototype_getter(
    getter_source: &str,
    constructor_atom: PredefinedAtom,
) -> (Runtime, RealmId, FunctionId, NativeFunction, FunctionId) {
    let getter_authority = compile_test_function(getter_source, "getter");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let getter = runtime
        .context(&realm)
        .expect("context")
        .instantiate(getter_authority)
        .expect("getter");
    let realm = runtime.context(&realm).expect("context").realm;
    let global = runtime.realm_global_object(realm).expect("global object");
    let constructor_key = runtime.predefined_property_key(constructor_atom);
    let function_key = runtime.predefined_property_key(PredefinedAtom::Function);
    let StoredValue::Function(constructor) =
        read_heap_property(&runtime, HeapReference::Object(global), &constructor_key)
            .expect("primitive constructor")
    else {
        panic!("global primitive constructor is not callable");
    };
    let StoredValue::Function(new_target) =
        read_heap_property(&runtime, HeapReference::Object(global), &function_key)
            .expect("Function constructor")
    else {
        panic!("global Function is not callable");
    };
    let native = runtime
        .functions
        .get(constructor)
        .and_then(HeapFunction::native)
        .copied()
        .expect("native primitive constructor");
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let replaced = runtime
        .functions
        .get_mut(new_target)
        .expect("new target function")
        .object
        .replace_existing_with_accessor(
            &prototype_key,
            PropertyLayout::accessor(false, false),
            Some(getter.id().expect("getter id")),
            None,
        );
    assert!(matches!(replaced, Some(OwnProperty::Data { .. })));
    (runtime, realm, constructor, native, new_target)
}

fn source_object(runtime: &mut Runtime, realm: RealmId) -> ObjectId {
    let prototype = runtime
        .realm_object_prototype(realm)
        .expect("Object.prototype");
    runtime
        .allocate_ordinary_object(prototype)
        .expect("source object")
}

fn immediate_boolean_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: bool,
) -> ObjectId {
    let Ok(NativeDispatch::Immediate(StoredValue::Object(wrapper))) =
        begin_boolean_constructor_wrapper(
            runtime,
            new_target,
            value,
            None,
            Some(native_function_host_origin()),
        )
    else {
        panic!("data-valued newTarget.prototype must construct immediately");
    };
    wrapper
}

fn immediate_number_wrapper(
    runtime: &mut Runtime,
    new_target: FunctionId,
    value: JsNumber,
) -> ObjectId {
    let Ok(NativeDispatch::Immediate(StoredValue::Object(wrapper))) =
        begin_number_constructor_wrapper(
            runtime,
            new_target,
            value,
            None,
            Some(native_function_host_origin()),
        )
    else {
        panic!("data-valued newTarget.prototype must construct immediately");
    };
    wrapper
}

fn begin_test_boolean_construction(
    runtime: &mut Runtime,
    constructor: FunctionId,
    native: NativeFunction,
    new_target: FunctionId,
    budget: &mut DynamicCompilationBudget,
) -> NativeDispatch {
    let Ok(dispatch) = dispatch_native_call(
        runtime,
        constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![StoredValue::Boolean(true)]),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        budget,
    ) else {
        panic!("accessor-backed Boolean construction must start");
    };
    dispatch
}

fn object_prototype_to_string_native(
    runtime: &Runtime,
    realm: RealmId,
) -> (FunctionId, NativeFunction) {
    let object_prototype = runtime
        .realm_object_prototype(realm)
        .expect("Object.prototype");
    let to_string_key = runtime.predefined_property_key(PredefinedAtom::ToString);
    let StoredValue::Function(to_string) = read_heap_property(
        runtime,
        HeapReference::Object(object_prototype),
        &to_string_key,
    )
    .expect("Object.prototype.toString") else {
        panic!("Object.prototype.toString is not callable");
    };
    let native = runtime
        .functions
        .get(to_string)
        .and_then(HeapFunction::native)
        .copied()
        .expect("native Object.prototype.toString");
    (to_string, native)
}

fn runtime_with_number_to_string_radix_method(
    limits: RuntimeLimits,
) -> (Runtime, FunctionId, NativeFunction, ObjectId) {
    let value_of_authority = compile_test_function("function valueOf(){return 16;}", "valueOf");
    let mut runtime = Runtime::try_new(limits).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let value_of = runtime
        .context(&realm)
        .expect("context")
        .instantiate(value_of_authority)
        .expect("valueOf");
    let realm = runtime.context(&realm).expect("context").realm;
    let number_prototype = runtime
        .realm_number_prototype(realm)
        .expect("Number.prototype");
    let to_string_key = runtime.predefined_property_key(PredefinedAtom::ToString);
    let StoredValue::Function(to_string) = read_heap_property(
        &runtime,
        HeapReference::Object(number_prototype),
        &to_string_key,
    )
    .expect("Number.prototype.toString") else {
        panic!("Number.prototype.toString is not callable");
    };
    let native = runtime
        .functions
        .get(to_string)
        .and_then(HeapFunction::native)
        .copied()
        .expect("native Number.prototype.toString");
    let radix = source_object(&mut runtime, realm);
    runtime
        .append_data_property(
            HeapReference::Object(radix),
            runtime.predefined_property_key(PredefinedAtom::ValueOf),
            PropertyLayout::data(true, true, true),
            StoredValue::Function(value_of.id().expect("valueOf id")),
        )
        .expect("radix valueOf");
    drop(value_of);
    runtime.drain_releases();
    (runtime, to_string, native, radix)
}

fn begin_test_number_to_string(
    runtime: &mut Runtime,
    to_string: FunctionId,
    native: NativeFunction,
    radix: ObjectId,
    budget: &mut DynamicCompilationBudget,
) -> NativeDispatch {
    let Ok(dispatch) = dispatch_native_call(
        runtime,
        to_string,
        native,
        CallInputs {
            receiver: StoredValue::Number(JsNumber::from_i32(31)),
            arguments: CallArguments::from_values(vec![StoredValue::Object(radix)]),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        budget,
    ) else {
        panic!("resumable radix conversion must start");
    };
    dispatch
}

fn runtime_with_boolean_tag_getter_and_invoker(
    limits: RuntimeLimits,
    getter_source: &str,
    invoker_source: &str,
    invoker_name: &str,
) -> (Runtime, crate::Realm, Function, Function, JsValue) {
    let getter_authority = compile_test_function(getter_source, "getter");
    let invoker_authority = compile_test_function(invoker_source, invoker_name);
    let mut runtime = Runtime::try_new(limits).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (getter, invoker) = {
        let mut context = runtime.context(&realm).expect("context");
        (
            context.instantiate(getter_authority).expect("getter"),
            context.instantiate(invoker_authority).expect("invoker"),
        )
    };
    let realm_id = runtime.context(&realm).expect("context").realm;
    let (to_string, _) = object_prototype_to_string_native(&runtime, realm_id);
    let boolean_prototype = runtime
        .realm_boolean_prototype(realm_id)
        .expect("Boolean.prototype");
    runtime
        .append_accessor_property(
            HeapReference::Object(boolean_prototype),
            runtime.predefined_symbol_property_key(PredefinedAtom::SymbolToStringTag),
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("Boolean @@toStringTag getter");
    let to_string = runtime
        .public_value(StoredValue::Function(to_string))
        .expect("Object.prototype.toString root");
    (runtime, realm, invoker, getter, to_string)
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
