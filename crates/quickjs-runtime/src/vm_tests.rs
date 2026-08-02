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
fn ordinary_stack_helpers_reject_finally_return_addresses() {
    let (_, _, mut frame) = ordinary_test_frame();
    let continuation = frame.instruction;

    frame
        .stack
        .push(OperandStackEntry::FinallyReturn { continuation });
    assert!(matches!(
        pop(&mut frame),
        Err(EngineFault::RuntimeInvariant {
            message: "verified JavaScript value operation consumed an internal finally return address",
        })
    ));

    frame
        .stack
        .push(OperandStackEntry::FinallyReturn { continuation });
    assert!(matches!(
        peek(&frame),
        Err(EngineFault::RuntimeInvariant {
            message: "verified JavaScript value operation inspected an internal finally return address",
        })
    ));
    assert!(matches!(
        stack_value_at(&frame, 0),
        Err(EngineFault::RuntimeInvariant {
            message: "verified JavaScript value operation indexed an internal finally return address",
        })
    ));
}

#[test]
fn finally_return_pop_rejects_forged_javascript_and_catch_entries() {
    let (_, _, mut frame) = ordinary_test_frame();

    push(&mut frame, StoredValue::Number(JsNumber::from_i32(1)));
    assert!(matches!(
        pop_finally_continuation(&mut frame),
        Err(EngineFault::RuntimeInvariant {
            message: "verified ret operand is not an internal finally return address",
        })
    ));

    let handler = frame.instruction;
    frame.stack.push(OperandStackEntry::Catch { handler });
    assert!(matches!(
        pop_finally_continuation(&mut frame),
        Err(EngineFault::RuntimeInvariant {
            message: "verified ret operand is not an internal finally return address",
        })
    ));

    let continuation = frame.instruction;
    frame
        .stack
        .push(OperandStackEntry::FinallyReturn { continuation });
    assert_eq!(
        pop_finally_continuation(&mut frame).expect("typed finally return address"),
        continuation
    );
}

#[test]
fn gosub_uses_the_verified_target_and_ret_uses_the_verified_continuation() {
    // This structural certificate only drives the private dispatch helper; it
    // is never installed or executed in place of whole-graph authority.
    let mut builder = quickjs_bytecode::BytecodeBuilder::new();
    for (opcode, operands) in [
        (FinalOpcode::Undefined, Operands::None),
        (FinalOpcode::Gosub, Operands::Label(6)),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::ReturnUndef, Operands::None),
        (FinalOpcode::Ret, Operands::None),
    ] {
        builder
            .push(opcode, operands)
            .expect("structural finally fixture");
    }
    let control_flow = quickjs_bytecode::verify_control_flow(
        quickjs_bytecode::UnverifiedFunctionBody::new(
            builder.into_bytes(),
            2,
            quickjs_bytecode::FunctionIndexDomains::default(),
            quickjs_bytecode::UnverifiedFunctionHeader::default(),
        ),
        quickjs_bytecode::VerificationLimits::default(),
    )
    .expect("structurally verified finally fixture");
    let gosub = control_flow.instructions()[1];
    let target = gosub
        .successors()
        .branch_target()
        .expect("verified finally target");
    let continuation = gosub
        .successors()
        .fallthrough()
        .expect("verified finally continuation");

    let (_, _, mut frame) = ordinary_test_frame();
    push(&mut frame, StoredValue::Undefined);
    enter_finally_subroutine(gosub, &mut frame).expect("verified gosub");
    assert_eq!(frame.instruction, target);
    assert!(matches!(
        frame.stack.as_slice(),
        [
            OperandStackEntry::JavaScript(StoredValue::Undefined),
            OperandStackEntry::FinallyReturn {
                continuation: actual,
            },
        ] if *actual == continuation
    ));

    frame.instruction =
        pop_finally_continuation(&mut frame).expect("verified ret continuation marker");
    assert_eq!(frame.instruction, continuation);
    assert!(matches!(
        frame.stack.as_slice(),
        [OperandStackEntry::JavaScript(StoredValue::Undefined)]
    ));
}

#[test]
fn verified_generic_drop_can_discard_a_finally_return_address() {
    let (_, _, mut frame) = ordinary_test_frame();
    frame.stack.push(OperandStackEntry::FinallyReturn {
        continuation: frame.instruction,
    });

    drop_stack_entry(&mut frame).expect("verified abrupt override discards the return address");
    assert!(frame.stack.is_empty());
}

#[test]
fn array_from_consumes_the_verified_suffix_in_left_to_right_order() {
    let (mut runtime, _realm, mut frame) = array_from_test_frame();
    let baseline = runtime.usage();
    let mut budget = execution_budget_with_consumed(u64::MAX, 1);

    assert!(matches!(
        execute_one(&mut runtime, &mut frame, &mut budget),
        Ok(Step::Continue)
    ));
    let [OperandStackEntry::JavaScript(StoredValue::Object(array))] = frame.stack.as_slice() else {
        panic!("array_from must replace its complete input suffix with one array");
    };
    assert_eq!(runtime.array_length(*array).expect("array length"), Some(2));
    for (index, expected) in [(0, 11), (1, 22)] {
        assert!(matches!(
            runtime
                .array_own_property(
                    *array,
                    &PropertyKey::from_index(ArrayIndex::new(index).expect("index")),
                )
                .expect("array property"),
            Some(OwnProperty::Data {
                layout,
                value: StoredValue::Number(value),
            }) if layout == PropertyLayout::data(true, true, true)
                && value.strict_equals(JsNumber::from_i32(expected))
        ));
    }
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
    assert_eq!(
        runtime.usage().object_properties(),
        baseline.object_properties() + 3
    );
}

#[test]
fn array_from_preflights_fuel_and_runtime_limits_before_stack_mutation() {
    let (mut fuel_runtime, _realm, mut fuel_frame) = array_from_test_frame();
    let fuel_usage = fuel_runtime.usage();
    let mut fuel = execution_budget_with_consumed(2, 1);
    assert!(matches!(
        execute_one(&mut fuel_runtime, &mut fuel_frame, &mut fuel),
        Err(ExecutionError::InstructionLimitExceeded {
            limit: 2,
            executed: 2,
        })
    ));
    assert_array_from_input_stack(&fuel_frame);
    assert_eq!(fuel_runtime.usage(), fuel_usage);

    let (mut heap_runtime, _realm, mut heap_frame) = array_from_test_frame();
    let heap_usage = heap_runtime.usage();
    heap_runtime.limits.max_heap_objects = heap_usage.heap_objects();
    let mut budget = execution_budget_with_consumed(u64::MAX, 1);
    assert!(matches!(
        execute_one(&mut heap_runtime, &mut heap_frame, &mut budget),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::HeapObjects,
            limit,
            observed,
        }) if limit == heap_usage.heap_objects() && observed == limit + 1
    ));
    assert_array_from_input_stack(&heap_frame);
    assert_eq!(heap_runtime.usage(), heap_usage);

    let (mut property_runtime, _realm, mut property_frame) = array_from_test_frame();
    let property_usage = property_runtime.usage();
    property_runtime.limits.max_object_properties =
        property_usage.object_properties().saturating_add(2);
    let mut budget = execution_budget_with_consumed(u64::MAX, 1);
    assert!(matches!(
        execute_one(&mut property_runtime, &mut property_frame, &mut budget),
        Err(ExecutionError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit,
            observed,
        }) if limit == property_usage.object_properties() + 2 && observed == limit + 1
    ));
    assert_array_from_input_stack(&property_frame);
    assert_eq!(property_runtime.usage(), property_usage);
}

#[test]
fn array_length_and_index_mutations_precharge_shape_work_before_mutation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm = runtime.context(&realm).expect("context").realm;
    let array = runtime
        .allocate_array(
            realm,
            vec![
                StoredValue::Number(JsNumber::from_i32(1)),
                StoredValue::Number(JsNumber::from_i32(2)),
            ],
        )
        .expect("array");
    let baseline = runtime.usage();

    let length_work = runtime
        .preview_array_length_write_work(array, 1)
        .expect("length work");
    let mut budget = execution_budget_with_consumed(length_work, 1);
    let Err(NativeFailure::Execution(error)) = finish_array_length_write(
        &mut runtime,
        ArrayLengthWriteState {
            base: StoredValue::Object(array),
            name: JsString::from_utf8("length").expect("length"),
            strict: true,
            original: None,
            first_length: None,
        },
        StoredValue::Number(JsNumber::from_i32(1)),
        realm,
        None,
        &native_function_host_origin(),
        &mut budget,
    ) else {
        panic!("length mutation must precharge its complete shape work");
    };
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded { limit, executed }
            if limit == length_work && executed == length_work
    ));
    assert_eq!(runtime.array_length(array).expect("length"), Some(2));
    assert_eq!(runtime.usage(), baseline);

    let define_work = runtime
        .preview_array_define_data_property_work(array)
        .expect("definition work");
    let mut budget = execution_budget_with_consumed(define_work, 1);
    let Err(error) = write_static_property(
        &mut runtime,
        realm,
        &StoredValue::Object(array),
        PropertyKey::from_index(ArrayIndex::new(2).expect("index")),
        StoredValue::Number(JsNumber::from_i32(3)),
        true,
        &mut budget,
    ) else {
        panic!("index mutation must precharge its complete shape work");
    };
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded { limit, executed }
            if limit == define_work && executed == define_work
    ));
    assert_eq!(runtime.array_length(array).expect("length"), Some(2));
    assert_eq!(runtime.usage(), baseline);
    assert!(
        runtime
            .array_own_property(
                array,
                &PropertyKey::from_index(ArrayIndex::new(2).expect("index")),
            )
            .expect("array property")
            .is_none()
    );
}

#[test]
fn nip_catch_scans_to_the_nearest_marker_and_preserves_the_top_value() {
    let (_, _, mut frame) = ordinary_test_frame();
    let handler = frame.instruction;
    let continuation = frame.instruction;
    frame.stack.push(OperandStackEntry::Catch { handler });
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(7)));
    frame.stack.push(OperandStackEntry::Catch { handler });
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(1)));
    frame
        .stack
        .push(OperandStackEntry::FinallyReturn { continuation });
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(2)));

    let mut budget = execution_budget_with_consumed(u64::MAX, 1);
    nip_catch(&mut frame, &mut budget).expect("nearest catch marker");

    assert!(matches!(
        frame.stack.as_slice(),
        [
            OperandStackEntry::Catch {
                handler: outer_handler,
            },
            OperandStackEntry::JavaScript(StoredValue::Number(prefix)),
            OperandStackEntry::JavaScript(StoredValue::Number(top)),
        ] if *outer_handler == handler
            && prefix.strict_equals(JsNumber::from_i32(7))
            && top.strict_equals(JsNumber::from_i32(2))
    ));
}

#[test]
fn nip_catch_precharges_the_bounded_scan_before_mutating_the_stack() {
    let (_, _, mut frame) = ordinary_test_frame();
    let handler = frame.instruction;
    let continuation = frame.instruction;
    frame.stack.push(OperandStackEntry::Catch { handler });
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(1)));
    frame
        .stack
        .push(OperandStackEntry::FinallyReturn { continuation });
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(2)));

    let mut budget = execution_budget_with_consumed(4, 1);
    let Err(error) = nip_catch(&mut frame, &mut budget) else {
        panic!("nip_catch must precharge its complete bounded scan");
    };
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded {
            limit: 4,
            executed: 4,
        }
    ));
    assert!(matches!(
        frame.stack.as_slice(),
        [
            OperandStackEntry::Catch {
                handler: actual_handler,
            },
            OperandStackEntry::JavaScript(StoredValue::Number(pending)),
            OperandStackEntry::FinallyReturn {
                continuation: actual_continuation,
            },
            OperandStackEntry::JavaScript(StoredValue::Number(top)),
        ] if *actual_handler == handler
            && pending.strict_equals(JsNumber::from_i32(1))
            && *actual_continuation == continuation
            && top.strict_equals(JsNumber::from_i32(2))
    ));
}

#[test]
fn inactive_for_of_record_cannot_be_stepped_again_but_can_be_closed() {
    let (_, _, mut frame) = ordinary_test_frame();
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(1)));
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(2)));
    frame
        .stack
        .push(OperandStackEntry::ForOfCatch { active: false });

    assert!(matches!(
        deactivate_for_of_record(&mut frame, false, 0),
        Err(EngineFault::RuntimeInvariant {
            message: "verified for-of operation has the wrong record marker",
        })
    ));
    assert!(matches!(
        frame.stack.last(),
        Some(OperandStackEntry::ForOfCatch { active: false })
    ));

    let (iterator, next) =
        deactivate_for_of_record(&mut frame, true, 0).expect("inactive record remains closable");
    assert!(
        matches!(iterator, StoredValue::Number(value) if value.strict_equals(JsNumber::from_i32(1)))
    );
    assert!(
        matches!(next, StoredValue::Number(value) if value.strict_equals(JsNumber::from_i32(2)))
    );
}

#[test]
fn exception_unwind_discards_intervening_finally_return_addresses() {
    let (mut runtime, realm, mut frame) = ordinary_test_frame();
    let handler = frame.instruction;
    let continuation = frame.instruction;
    frame.stack.push(OperandStackEntry::Catch { handler });
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(1)));
    frame
        .stack
        .push(OperandStackEntry::FinallyReturn { continuation });
    let mut active_frame_values = frame.reserved_values;
    let mut frames = vec![frame];
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());

    dispatch_pending_exception(
        &mut runtime,
        &mut frames,
        &mut active_frame_values,
        PendingException {
            realm,
            payload: PendingExceptionPayload::ThrownValue(StoredValue::Number(JsNumber::from_i32(
                9,
            ))),
            origin: native_function_host_origin(),
        },
        None,
        &mut execution_budget,
    )
    .expect("verified catch dispatch");

    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!(frame.instruction, handler);
    assert!(matches!(
        frame.stack.as_slice(),
        [OperandStackEntry::JavaScript(StoredValue::Number(caught))]
            if caught.strict_equals(JsNumber::from_i32(9))
    ));
}

#[test]
fn exceptional_for_of_close_keeps_the_iterator_rooted_through_pending_collection() {
    let (mut runtime, realm, mut frame) = ordinary_test_frame();
    let iterator = source_object(&mut runtime, realm);
    let thrown = source_object(&mut runtime, realm);
    push(&mut frame, StoredValue::Object(iterator));
    push(&mut frame, StoredValue::Undefined);
    frame
        .stack
        .push(OperandStackEntry::ForOfCatch { active: true });
    frame.transient_cleanup_pending = true;
    runtime.collection_pending = true;

    let mut active_frame_values = frame.reserved_values;
    let mut frames = vec![frame];
    let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
    let error = dispatch_pending_exception(
        &mut runtime,
        &mut frames,
        &mut active_frame_values,
        PendingException {
            realm,
            payload: PendingExceptionPayload::ThrownValue(StoredValue::Object(thrown)),
            origin: native_function_host_origin(),
        },
        None,
        &mut execution_budget,
    )
    .expect_err("the original body error must escape after exceptional close");

    let ExecutionError::Exception(exception) = error else {
        panic!("the explicit body throw must remain a JavaScript exception");
    };
    assert_eq!(exception.kind(), None);
    assert!(
        exception
            .thrown_value()
            .expect("explicit body throw")
            .clone()
            .into_object()
            .is_ok(),
        "the original thrown object must survive collection through close"
    );
    assert!(
        runtime.heap_reference_is_live(HeapReference::Object(iterator)),
        "the iterator must survive collection until its return property is read"
    );
    assert!(
        runtime.heap_reference_is_live(HeapReference::Object(thrown)),
        "the pending thrown object must remain rooted until exception publication"
    );
    drop(exception);
    frames.clear();
    runtime
        .collect_cycles()
        .expect("release completed for-of iterator");
    assert!(
        !runtime.heap_reference_is_live(HeapReference::Object(iterator)),
        "the iterator record must stop rooting the iterator after unwind"
    );
    assert!(
        !runtime.heap_reference_is_live(HeapReference::Object(thrown)),
        "the completed exception must release the original thrown object"
    );
}

#[test]
fn array_iterator_creation_boxes_a_primitive_receiver_once() {
    let (mut runtime, realm, _) = ordinary_test_frame();
    let Ok(dispatch) = begin_array_iterator_method(
        &mut runtime,
        StoredValue::Number(JsNumber::from_i32(7)),
        crate::object::ArrayIteratorKind::Value,
        realm,
        native_function_host_origin(),
    ) else {
        panic!("Array iterator creation failed");
    };
    let NativeDispatch::Immediate(StoredValue::Object(iterator)) = dispatch else {
        panic!("Array iterator creation must complete immediately");
    };
    let snapshot = runtime
        .array_iterator_snapshot(iterator)
        .expect("Array iterator state");
    let Some(StoredValue::Object(wrapper)) = snapshot.iterated else {
        panic!("primitive receiver must be retained as one wrapper");
    };
    assert!(
        runtime
            .boxed_number(wrapper)
            .expect("live Number wrapper")
            .is_some_and(|number| number.strict_equals(JsNumber::from_i32(7)))
    );
}

#[test]
fn array_iterator_primitive_boxing_preflights_the_complete_transaction() {
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_max_heap_objects(21)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let usage = runtime.usage();
    let collection_pending = runtime.collection_pending;

    let result = begin_array_iterator_method(
        &mut runtime,
        StoredValue::Number(JsNumber::from_i32(7)),
        crate::object::ArrayIteratorKind::Value,
        realm_id,
        native_function_host_origin(),
    );
    assert!(matches!(
        result,
        Err(NativeFailure::Execution(ExecutionError::LimitExceeded {
            resource: RuntimeResource::HeapObjects,
            limit: 21,
            observed: 22,
        }))
    ));
    assert_eq!(runtime.usage(), usage);
    assert_eq!(runtime.collection_pending, collection_pending);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one matrix regression proves heap and property atomicity for all Array iterator result shapes"
)]
fn array_iterator_result_preflight_preserves_cursor_and_allows_retry() {
    for (kind, result_objects, result_properties) in [
        (crate::object::ArrayIteratorKind::Key, 1_u64, 2_u64),
        (crate::object::ArrayIteratorKind::Value, 1, 2),
        (crate::object::ArrayIteratorKind::KeyAndValue, 2, 5),
    ] {
        for constrained in [
            RuntimeResource::HeapObjects,
            RuntimeResource::ObjectProperties,
        ] {
            let (mut runtime, realm, _) = ordinary_test_frame();
            let iterated = runtime
                .allocate_array(realm, vec![StoredValue::Number(JsNumber::from_i32(11))])
                .expect("iterated Array");
            let iterator = runtime
                .allocate_array_iterator(realm, StoredValue::Object(iterated), kind)
                .expect("Array iterator");
            let baseline = runtime.usage();
            let collection_pending = runtime.collection_pending;
            let original_limits = runtime.limits;
            let (limit, observed) = match constrained {
                RuntimeResource::HeapObjects => {
                    let observed = baseline.heap_objects() + result_objects;
                    runtime.limits.max_heap_objects = observed - 1;
                    (runtime.limits.max_heap_objects, observed)
                }
                RuntimeResource::ObjectProperties => {
                    let observed = baseline.object_properties() + result_properties;
                    runtime.limits.max_object_properties = observed - 1;
                    (runtime.limits.max_object_properties, observed)
                }
                _ => unreachable!("the matrix only constrains iterator-result resources"),
            };
            let state = ArrayIteratorNextContinuation {
                iterator,
                iterated: StoredValue::Object(iterated),
                kind,
                index: 0,
                realm,
                stage: ArrayIteratorNextStage::AwaitLength,
                prepared_result: None,
                origin: native_function_host_origin(),
            };
            let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());

            let failure = finish_array_iterator_length(
                &mut runtime,
                state,
                StoredValue::Number(JsNumber::from_i32(1)),
                None,
                &mut execution_budget,
            );

            assert!(matches!(
                failure,
                Err(NativeFailure::Execution(ExecutionError::LimitExceeded {
                    resource,
                    limit: actual_limit,
                    observed: actual_observed,
                })) if resource == constrained
                    && actual_limit == limit
                    && actual_observed == observed
            ));
            assert_eq!(runtime.usage(), baseline);
            assert_eq!(runtime.collection_pending, collection_pending);
            assert_eq!(
                runtime
                    .array_iterator_snapshot(iterator)
                    .expect("Array iterator after failed preflight")
                    .next,
                0
            );

            runtime.limits = original_limits;
            let retry_state = ArrayIteratorNextContinuation {
                iterator,
                iterated: StoredValue::Object(iterated),
                kind,
                index: 0,
                realm,
                stage: ArrayIteratorNextStage::AwaitLength,
                prepared_result: None,
                origin: native_function_host_origin(),
            };
            let Ok(retry) = finish_array_iterator_length(
                &mut runtime,
                retry_state,
                StoredValue::Number(JsNumber::from_i32(1)),
                None,
                &mut execution_budget,
            ) else {
                panic!("retry after restoring resource capacity failed");
            };
            assert!(matches!(
                retry,
                NativeDispatch::Immediate(StoredValue::Object(_))
            ));
            assert_eq!(
                runtime
                    .array_iterator_snapshot(iterator)
                    .expect("Array iterator after retry")
                    .next,
                1
            );
        }
    }
}

#[test]
fn string_iterator_result_preflight_preserves_cursor_and_allows_retry() {
    for constrained in [
        RuntimeResource::HeapObjects,
        RuntimeResource::ObjectProperties,
    ] {
        let (mut runtime, realm, _) = ordinary_test_frame();
        let iterator = runtime
            .allocate_string_iterator(realm, JsString::from_utf8("A").expect("String"))
            .expect("String iterator");
        let baseline = runtime.usage();
        let collection_pending = runtime.collection_pending;
        let original_limits = runtime.limits;
        let (limit, observed) = match constrained {
            RuntimeResource::HeapObjects => {
                runtime.limits.max_heap_objects = baseline.heap_objects();
                (baseline.heap_objects(), baseline.heap_objects() + 1)
            }
            RuntimeResource::ObjectProperties => {
                runtime.limits.max_object_properties = baseline.object_properties() + 1;
                (
                    baseline.object_properties() + 1,
                    baseline.object_properties() + 2,
                )
            }
            _ => unreachable!("the matrix only constrains iterator-result resources"),
        };

        let failure = begin_string_iterator_next(
            &mut runtime,
            StoredValue::Object(iterator),
            realm,
            native_function_host_origin(),
        );

        assert!(matches!(
            failure,
            Err(NativeFailure::Execution(ExecutionError::LimitExceeded {
                resource,
                limit: actual_limit,
                observed: actual_observed,
            })) if resource == constrained
                && actual_limit == limit
                && actual_observed == observed
        ));
        assert_eq!(runtime.usage(), baseline);
        assert_eq!(runtime.collection_pending, collection_pending);
        assert_eq!(
            runtime
                .objects
                .get(iterator)
                .expect("String iterator after failed preflight")
                .string_iterator_state()
                .expect("String Iterator class")
                .next(),
            0
        );

        runtime.limits = original_limits;
        let Ok(retry) = begin_string_iterator_next(
            &mut runtime,
            StoredValue::Object(iterator),
            realm,
            native_function_host_origin(),
        ) else {
            panic!("retry after restoring resource capacity failed");
        };
        assert!(matches!(
            retry,
            NativeDispatch::Immediate(StoredValue::Object(_))
        ));
        assert_eq!(
            runtime
                .objects
                .get(iterator)
                .expect("String iterator after retry")
                .string_iterator_state()
                .expect("String Iterator class")
                .next(),
            1
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one matrix regression covers getter admission for both prepared Array iterator result shapes"
)]
fn array_iterator_getter_admission_preserves_cursor_and_prepared_values() {
    for (kind, prepared_values) in [
        (crate::object::ArrayIteratorKind::Value, 2_u64),
        (crate::object::ArrayIteratorKind::KeyAndValue, 5_u64),
    ] {
        for constrained in [RuntimeResource::Frames, RuntimeResource::FrameValues] {
            let (mut runtime, realm, getter_frame) = ordinary_test_frame();
            let getter = getter_frame.function;
            let prototype = runtime
                .realm_array_prototype(realm)
                .expect("Array.prototype");
            let iterated = runtime
                .allocate_sparse_array_with_prototype(HeapReference::Object(prototype), 1)
                .expect("sparse Array");
            runtime
                .append_accessor_property(
                    HeapReference::Object(iterated),
                    PropertyKey::from_index(ArrayIndex::new(0).expect("index")),
                    PropertyLayout::accessor(true, true),
                    Some(getter),
                    None,
                )
                .expect("element getter");
            let iterator = runtime
                .allocate_array_iterator(realm, StoredValue::Object(iterated), kind)
                .expect("Array iterator");
            let baseline = runtime.usage();
            let collection_pending = runtime.collection_pending;
            let original_limits = runtime.limits;
            let expected_continuation_values = 2_u64.saturating_add(prepared_values);
            let (expected_limit, expected_observed) = match constrained {
                RuntimeResource::Frames => {
                    runtime.limits.max_active_frames = 1;
                    (1, 2)
                }
                RuntimeResource::FrameValues => {
                    runtime.limits.max_active_frame_values =
                        expected_continuation_values.saturating_sub(1);
                    (
                        expected_continuation_values.saturating_sub(1),
                        expected_continuation_values,
                    )
                }
                _ => unreachable!("the matrix only constrains call-admission resources"),
            };
            let state = ArrayIteratorNextContinuation {
                iterator,
                iterated: StoredValue::Object(iterated),
                kind,
                index: 0,
                realm,
                stage: ArrayIteratorNextStage::AwaitLength,
                prepared_result: None,
                origin: native_function_host_origin(),
            };
            let mut execution_budget = ExecutionBudget::new(ExecutionLimits::default());
            let Ok(dispatch) = finish_array_iterator_length(
                &mut runtime,
                state,
                StoredValue::Number(JsNumber::from_i32(1)),
                None,
                &mut execution_budget,
            ) else {
                panic!("element getter dispatch must be prepared");
            };
            let NativeDispatch::Call(call) = &dispatch else {
                panic!("an accessor element must dispatch its getter");
            };
            assert_eq!(
                native_continuation_values(&call.continuations),
                expected_continuation_values
            );
            assert_eq!(
                runtime
                    .array_iterator_snapshot(iterator)
                    .expect("Array iterator before getter admission")
                    .next,
                0
            );

            let failure = resolve_native_dispatch(
                &mut runtime,
                dispatch,
                &[],
                0,
                0,
                None,
                &mut execution_budget,
            );

            assert!(matches!(
                failure,
                Err(NativeFailure::Execution(ExecutionError::LimitExceeded {
                    resource,
                    limit,
                    observed,
                })) if resource == constrained
                    && limit == expected_limit
                    && observed == expected_observed
            ));
            assert_eq!(runtime.usage(), baseline);
            assert_eq!(runtime.collection_pending, collection_pending);
            assert_eq!(
                runtime
                    .array_iterator_snapshot(iterator)
                    .expect("Array iterator after rejected getter")
                    .next,
                0
            );

            runtime.limits = original_limits;
            let retry_state = ArrayIteratorNextContinuation {
                iterator,
                iterated: StoredValue::Object(iterated),
                kind,
                index: 0,
                realm,
                stage: ArrayIteratorNextStage::AwaitLength,
                prepared_result: None,
                origin: native_function_host_origin(),
            };
            let mut retry_budget = ExecutionBudget::new(ExecutionLimits::default());
            let Ok(retry_dispatch) = finish_array_iterator_length(
                &mut runtime,
                retry_state,
                StoredValue::Number(JsNumber::from_i32(1)),
                None,
                &mut retry_budget,
            ) else {
                panic!("getter retry must be prepared");
            };
            let Ok(resolved) = resolve_native_dispatch(
                &mut runtime,
                retry_dispatch,
                &[],
                0,
                0,
                None,
                &mut retry_budget,
            ) else {
                panic!("getter retry must pass call admission");
            };
            assert!(matches!(resolved, NativeDispatch::Frame(_)));
            assert_eq!(
                runtime
                    .array_iterator_snapshot(iterator)
                    .expect("Array iterator after admitted getter")
                    .next,
                1
            );
        }
    }
}

#[test]
fn array_iterator_lookup_fuel_failure_preserves_cursor_and_allows_retry() {
    let (mut runtime, realm, _) = ordinary_test_frame();
    let iterated = runtime
        .allocate_array(realm, vec![StoredValue::Number(JsNumber::from_i32(11))])
        .expect("iterated Array");
    let iterator = runtime
        .allocate_array_iterator(
            realm,
            StoredValue::Object(iterated),
            crate::object::ArrayIteratorKind::Value,
        )
        .expect("Array iterator");
    let baseline = runtime.usage();
    let collection_pending = runtime.collection_pending;
    let mut preview = ExecutionBudget::new(ExecutionLimits::default());
    charge_iterator_property_lookup(&runtime, &StoredValue::Object(iterated), &mut preview)
        .unwrap_or_else(|_| panic!("property-lookup work preview failed"));
    let required_fuel = preview.executed_instructions;
    assert!(required_fuel > 0);
    let mut tight_budget =
        ExecutionBudget::new(ExecutionLimits::default().with_instruction_fuel(required_fuel - 1));
    let state = ArrayIteratorNextContinuation {
        iterator,
        iterated: StoredValue::Object(iterated),
        kind: crate::object::ArrayIteratorKind::Value,
        index: 0,
        realm,
        stage: ArrayIteratorNextStage::AwaitLength,
        prepared_result: None,
        origin: native_function_host_origin(),
    };

    let failure = finish_array_iterator_length(
        &mut runtime,
        state,
        StoredValue::Number(JsNumber::from_i32(1)),
        None,
        &mut tight_budget,
    );

    let Err(NativeFailure::Execution(ExecutionError::InstructionLimitExceeded { limit, executed })) =
        failure
    else {
        panic!("tight iterator lookup fuel must fail before cursor mutation");
    };
    assert_eq!(limit, required_fuel - 1);
    assert_eq!(executed, required_fuel - 1);
    assert_eq!(runtime.usage(), baseline);
    assert_eq!(runtime.collection_pending, collection_pending);
    assert_eq!(
        runtime
            .array_iterator_snapshot(iterator)
            .expect("Array iterator after fuel rejection")
            .next,
        0
    );

    let retry_state = ArrayIteratorNextContinuation {
        iterator,
        iterated: StoredValue::Object(iterated),
        kind: crate::object::ArrayIteratorKind::Value,
        index: 0,
        realm,
        stage: ArrayIteratorNextStage::AwaitLength,
        prepared_result: None,
        origin: native_function_host_origin(),
    };
    let mut retry_budget = ExecutionBudget::new(ExecutionLimits::default());
    let Ok(retry) = finish_array_iterator_length(
        &mut runtime,
        retry_state,
        StoredValue::Number(JsNumber::from_i32(1)),
        None,
        &mut retry_budget,
    ) else {
        panic!("retry with sufficient lookup fuel failed");
    };
    assert!(matches!(
        retry,
        NativeDispatch::Immediate(StoredValue::Object(_))
    ));
    assert_eq!(
        runtime
            .array_iterator_snapshot(iterator)
            .expect("Array iterator after fuel retry")
            .next,
        1
    );
}

#[test]
fn finally_return_markers_do_not_hide_or_create_execution_roots() {
    let (mut runtime, realm, mut frame) = ordinary_test_frame();
    let prototype = runtime
        .realm_object_prototype(realm)
        .expect("Object.prototype");
    let object = runtime
        .allocate_ordinary_object(prototype)
        .expect("frame-only object");
    push(&mut frame, StoredValue::Object(object));
    frame.stack.push(OperandStackEntry::FinallyReturn {
        continuation: frame.instruction,
    });

    runtime.collection_pending = true;
    collect_cycles_with_execution_roots(&mut runtime, std::slice::from_ref(&frame), &[], &[])
        .expect("collection with typed operand-stack roots");
    assert!(
        runtime.heap_reference_is_live(HeapReference::Object(object)),
        "the JavaScript value below the finally return address remains traced"
    );

    let removed = frame.stack.remove(0);
    assert!(matches!(
        removed,
        OperandStackEntry::JavaScript(StoredValue::Object(actual)) if actual == object
    ));
    runtime.collection_pending = true;
    collect_cycles_with_execution_roots(&mut runtime, std::slice::from_ref(&frame), &[], &[])
        .expect("collection with only a finally return address");
    assert!(
        !runtime.heap_reference_is_live(HeapReference::Object(object)),
        "a finally return address must not retain unrelated heap state"
    );
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
    push(&mut frame, StoredValue::Undefined);

    let mut budget = execution_budget_with_consumed(u64::MAX, 1);
    let Err(error) = execute_one(&mut runtime, &mut frame, &mut budget) else {
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
    push(&mut frame, StoredValue::Object(iterator));

    let mut budget = execution_budget_with_consumed(1, 1);
    let Err(error) = execute_one(&mut runtime, &mut frame, &mut budget) else {
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
        [OperandStackEntry::JavaScript(StoredValue::Object(actual))] if *actual == iterator
    ));

    let mut budget = execution_budget_with_consumed(u64::MAX, 1);
    execute_one(&mut runtime, &mut frame, &mut budget)
        .expect("the untouched candidate remains available");
    assert_eq!(runtime.usage().for_in_entries(), 2);
    assert!(matches!(
        frame.stack.as_slice(),
        [
            OperandStackEntry::JavaScript(StoredValue::Object(actual)),
            OperandStackEntry::JavaScript(StoredValue::String(name)),
            OperandStackEntry::JavaScript(StoredValue::Boolean(false)),
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
    push(
        &mut prototype_frame,
        StoredValue::Object(prototype_iterator),
    );

    let mut budget = execution_budget_with_consumed(7, 1);
    let Err(error) = execute_one(&mut runtime, &mut prototype_frame, &mut budget) else {
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

    let mut budget = execution_budget_with_consumed(u64::MAX, 1);
    execute_one(&mut runtime, &mut prototype_frame, &mut budget)
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
            OperandStackEntry::JavaScript(StoredValue::Object(actual)),
            OperandStackEntry::JavaScript(StoredValue::String(name)),
            OperandStackEntry::JavaScript(StoredValue::Boolean(false)),
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
    push(&mut terminal_frame, StoredValue::Object(terminal_iterator));

    let mut budget = execution_budget_with_consumed(2, 1);
    let Err(error) = execute_one(&mut runtime, &mut terminal_frame, &mut budget) else {
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

    let mut budget = execution_budget_with_consumed(u64::MAX, 1);
    execute_one(&mut runtime, &mut terminal_frame, &mut budget).expect("terminal transition retry");
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
            OperandStackEntry::JavaScript(StoredValue::Object(actual)),
            OperandStackEntry::JavaScript(StoredValue::Undefined),
            OperandStackEntry::JavaScript(StoredValue::Boolean(true)),
        ] if *actual == terminal_iterator
    ));
}

fn execution_budget_with_consumed(limit: u64, consumed: u64) -> ExecutionBudget {
    let mut budget = ExecutionBudget::new(ExecutionLimits::default().with_instruction_fuel(limit));
    budget
        .charge_instructions(consumed)
        .expect("test setup remains within its instruction budget");
    budget
}

fn ordinary_test_frame() -> (Runtime, RealmId, Frame) {
    let authority = compile_test_function("function run(){return 0;}", "run");
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
    let plan = plan_frame(&runtime, function, 0, 0).expect("frame plan");
    let frame = create_frame(
        &mut runtime,
        plan,
        StoredValue::Undefined,
        FrameArguments::Owned(CallArguments::empty()),
        None,
        None,
    )
    .expect("frame");
    (runtime, realm_id, frame)
}

fn array_from_test_frame() -> (Runtime, RealmId, Frame) {
    let authority = compile_test_function("function run(){return [1,2];}", "run");
    let control_flow = authority.root().function().control_flow();
    let array_from_pc = control_flow
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::ArrayFrom)
        .expect("array_from")
        .decoded()
        .pc();
    let array_from = control_flow
        .instruction_index_at(array_from_pc)
        .expect("array_from instruction index");
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
    frame.instruction = array_from;
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(11)));
    push(&mut frame, StoredValue::Number(JsNumber::from_i32(22)));
    (runtime, realm_id, frame)
}

fn assert_array_from_input_stack(frame: &Frame) {
    assert!(matches!(
        frame.stack.as_slice(),
        [
            OperandStackEntry::JavaScript(StoredValue::Number(first)),
            OperandStackEntry::JavaScript(StoredValue::Number(second)),
        ] if first.strict_equals(JsNumber::from_i32(11))
            && second.strict_equals(JsNumber::from_i32(22))
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());

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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());

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

    let mut budget = execution_budget_with_consumed(u64::MAX, 0);
    let Ok(NativeDispatch::Call(call)) = begin_property_key_conversion(
        &mut runtime,
        StoredValue::Object(object),
        PropertyKeyTarget::ToKey,
        realm,
        None,
        native_function_host_origin(),
        &mut budget,
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let result =
        execute_prepared_frames_with_budget(&mut runtime, vec![frame], None, None, &mut budget)
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let result =
        execute_prepared_frames_with_budget(&mut runtime, vec![frame], None, None, &mut budget)
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
fn array_constructor_suspends_before_allocation_and_retains_every_argument() {
    let (mut runtime, realm, array_constructor, native, new_target) =
        runtime_with_array_constructor_prototype_getter(
            "function getter(){\"use strict\";return this.valueOf;}",
        );
    let custom_prototype = source_object(&mut runtime, realm);
    let retained_argument = source_object(&mut runtime, realm);
    let value_of_key = runtime.predefined_property_key(PredefinedAtom::ValueOf);
    runtime
        .append_data_property(
            HeapReference::Function(new_target),
            value_of_key,
            PropertyLayout::data(true, true, true),
            StoredValue::Object(custom_prototype),
        )
        .expect("newTarget receiver marker");

    let usage_before_get = runtime.usage();
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        array_constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![
                StoredValue::Object(retained_argument),
                StoredValue::Boolean(true),
            ]),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("accessor-backed Array construction must start");
    };
    let NativeDispatch::Call(call) = dispatch else {
        panic!("newTarget.prototype getter must suspend Array construction");
    };
    assert!(matches!(
        call.receiver,
        StoredValue::Function(function) if function == new_target
    ));
    assert!(matches!(
        call.continuations.as_slice(),
        [NativeContinuation::IntrinsicGet(
            IntrinsicGetContinuation::ArrayConstructor {
                new_target: retained_target,
                arguments,
                ..
            }
        )] if *retained_target == new_target
            && matches!(arguments.as_slice(), [
                StoredValue::Object(object),
                StoredValue::Boolean(true),
            ] if *object == retained_argument)
    ));
    assert_eq!(native_continuation_values(&call.continuations), 3);
    assert_eq!(runtime.usage(), usage_before_get);

    collect_cycles_with_execution_roots(&mut runtime, &[], &call.continuations, &[])
        .expect("continuation-rooted collection");
    assert!(runtime.objects.contains(retained_argument));

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
    let result =
        execute_prepared_frames_with_budget(&mut runtime, vec![frame], None, None, &mut budget)
            .expect("resumed Array construction");
    let StoredValue::Object(array) = result else {
        panic!("Array construction must return an object");
    };

    assert_eq!(runtime.array_length(array).expect("array length"), Some(2));
    assert_eq!(
        runtime
            .object_record(HeapReference::Object(array))
            .expect("array")
            .prototype(),
        Some(HeapReference::Object(custom_prototype))
    );
    assert_array_object_index(&runtime, array, 0, retained_argument);
}

#[test]
fn array_constructor_primitive_prototype_falls_back_to_the_new_target_realm() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let constructor_realm = runtime.create_realm().expect("constructor realm");
    let target_realm = runtime.create_realm().expect("target realm");
    let constructor_realm = runtime
        .context(&constructor_realm)
        .expect("constructor context")
        .realm;
    let target_realm = runtime
        .context(&target_realm)
        .expect("target context")
        .realm;
    let constructor = global_native_function(&runtime, constructor_realm, PredefinedAtom::Array);
    let native = runtime
        .functions
        .get(constructor)
        .and_then(HeapFunction::native)
        .copied()
        .expect("native Array");
    let new_target = global_native_function(&runtime, target_realm, PredefinedAtom::Function);
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    runtime
        .functions
        .get_mut(new_target)
        .expect("new target")
        .object
        .replace_existing_with_data(
            &prototype_key,
            PropertyLayout::data(false, false, false),
            StoredValue::Null,
        )
        .expect("replace newTarget.prototype");

    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let Ok(NativeDispatch::Immediate(StoredValue::Object(array))) = dispatch_native_call(
        &mut runtime,
        constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::empty(),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("primitive newTarget.prototype must construct immediately");
    };

    assert_eq!(
        runtime
            .object_record(HeapReference::Object(array))
            .expect("array")
            .prototype(),
        Some(HeapReference::Object(
            runtime
                .realm_array_prototype(target_realm)
                .expect("target-realm Array.prototype"),
        ))
    );
}

#[test]
fn array_constructor_prototype_get_precedes_invalid_length_validation() {
    let (mut runtime, _realm, array_constructor, native, new_target) =
        runtime_with_array_constructor_prototype_getter("function getter(){throw 41;}");
    let usage_before_get = runtime.usage();
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        array_constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![StoredValue::Number(JsNumber::from_f64(
                1.5,
            ))]),
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
    assert_eq!(runtime.usage(), usage_before_get);
    let Ok(dispatch) =
        resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("throwing getter dispatch must resolve");
    };
    let NativeDispatch::Frame(frame) = dispatch else {
        panic!("bytecode getter must produce an execution frame");
    };
    let error =
        execute_prepared_frames_with_budget(&mut runtime, vec![frame], None, None, &mut budget)
            .expect_err("prototype getter throw must beat the invalid-length RangeError");
    assert_eq!(runtime.usage(), usage_before_get);
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
fn array_constructor_prototype_get_precedes_dense_work_fuel_charge() {
    let (mut runtime, _realm, array_constructor, native, new_target) =
        runtime_with_array_constructor_prototype_getter("function getter(){throw 41;}");
    let usage_before_get = runtime.usage();
    let arguments = (0..64)
        .map(|_| StoredValue::Boolean(true))
        .collect::<Vec<_>>();
    // The budget must be large enough to reach the observable prototype Get but
    // smaller than the 64-element dense charge, so exhaustion after the Get
    // proves the ordering. The lower bound tracks `Object.prototype`'s property
    // count, because the lookup charges its full shape scan.
    let mut budget = ExecutionBudget::new(ExecutionLimits::default().with_instruction_fuel(24));
    let Ok(dispatch) = dispatch_native_call(
        &mut runtime,
        array_constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(arguments),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("prototype Get must start before charging 64 dense elements");
    };
    let Ok(dispatch) =
        resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("throwing getter dispatch must resolve within the tight budget");
    };
    let NativeDispatch::Frame(frame) = dispatch else {
        panic!("bytecode getter must produce an execution frame");
    };
    let error =
        execute_prepared_frames_with_budget(&mut runtime, vec![frame], None, None, &mut budget)
            .expect_err("getter throw must beat the dense-work fuel charge");
    assert_eq!(runtime.usage(), usage_before_get);
    let ExecutionError::Exception(exception) = error else {
        panic!("getter throw must remain a JavaScript exception");
    };
    let thrown = exception.thrown_value().expect("explicit getter throw");
    let number = thrown
        .as_number()
        .expect("live thrown value")
        .expect("number throw");
    assert!(number.strict_equals(JsNumber::from_i32(41)));
}

#[test]
fn array_constructor_accessor_continuation_obeys_frame_and_value_limits() {
    let (mut runtime, _realm, constructor, native, new_target) =
        runtime_with_array_constructor_prototype_getter(
            "function getter(){\"use strict\";return this;}",
        );
    runtime.limits.max_active_frames = 1;
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let dispatch = begin_test_array_construction(
        &mut runtime,
        constructor,
        native,
        new_target,
        vec![StoredValue::Boolean(true)],
        &mut budget,
    );
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
        runtime_with_array_constructor_prototype_getter(
            "function getter(){\"use strict\";return this;}",
        );
    runtime.limits.max_active_frame_values = 2;
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let dispatch = begin_test_array_construction(
        &mut runtime,
        constructor,
        native,
        new_target,
        vec![StoredValue::Boolean(true), StoredValue::Boolean(false)],
        &mut budget,
    );
    let Err(error) = resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("newTarget and both arguments must exceed two retained values");
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
fn array_constructor_precharges_dense_work_and_rolls_back_allocation_limits() {
    let (mut runtime, _realm, constructor, native) = runtime_with_array_constructor();
    let baseline = runtime.usage();
    runtime.limits.max_heap_objects = baseline.heap_objects();
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let Err(error) = dispatch_native_call(
        &mut runtime,
        constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![
                StoredValue::Boolean(true),
                StoredValue::Boolean(false),
            ]),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("dense Array allocation must exceed the heap-object limit");
    };
    assert!(matches!(
        error,
        NativeFailure::Execution(ExecutionError::LimitExceeded {
            resource: RuntimeResource::HeapObjects,
            limit,
            observed,
        }) if limit == baseline.heap_objects() && observed == baseline.heap_objects() + 1
    ));
    assert_eq!(runtime.usage(), baseline);

    let (mut runtime, _realm, constructor, native) = runtime_with_array_constructor();
    let baseline = runtime.usage();
    let mut budget = ExecutionBudget::new(ExecutionLimits::default().with_instruction_fuel(1));
    let Err(error) = dispatch_native_call(
        &mut runtime,
        constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![
                StoredValue::Boolean(true),
                StoredValue::Boolean(false),
            ]),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("dense Array work must exceed one instruction of fuel");
    };
    assert!(matches!(
        error,
        NativeFailure::Execution(ExecutionError::InstructionLimitExceeded {
            limit: 1,
            executed: 1,
        })
    ));
    assert_eq!(runtime.usage(), baseline);

    let (mut runtime, _realm, constructor, native) = runtime_with_array_constructor();
    let baseline = runtime.usage();
    runtime.limits.max_object_properties = baseline.object_properties();
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let Err(error) = dispatch_native_call(
        &mut runtime,
        constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(vec![StoredValue::Number(JsNumber::from_u32(
                u32::MAX,
            ))]),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) else {
        panic!("sparse Array allocation must exceed the length-property limit");
    };
    assert!(matches!(
        error,
        NativeFailure::Execution(ExecutionError::LimitExceeded {
            resource: RuntimeResource::ObjectProperties,
            limit,
            observed,
        }) if limit == baseline.object_properties()
            && observed == baseline.object_properties() + 1
    ));
    assert_eq!(runtime.usage(), baseline);
}

#[test]
fn boolean_constructor_getter_throw_precedes_wrapper_allocation() {
    let (mut runtime, _realm, boolean_constructor, native, new_target) =
        runtime_with_boolean_constructor_prototype_getter("function getter(){throw 41;}");

    let heap_objects_before_get = runtime.usage().heap_objects();
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let error =
        execute_prepared_frames_with_budget(&mut runtime, vec![frame], None, None, &mut budget)
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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

    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let result =
        execute_prepared_frames_with_budget(&mut runtime, vec![frame], None, None, &mut budget)
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
fn execution_root_collection_stays_dirty_through_catch_and_frame_return() {
    let (mut runtime, realm, run, _getter, to_string) = runtime_with_boolean_tag_getter_and_invoker(
        RuntimeLimits::default(),
        "function getter(){throw 1;}",
        "function run(target){\
                 if(typeof target===\"undefined\")return 7;\
                 let survivor={};\
                 survivor.self=survivor;\
                 try{\
                     target.call(true);\
                 }catch(error){\
                     return error;\
                 }\
             }",
        "run",
    );
    let baseline = runtime.usage();

    let result = runtime
        .context(&realm)
        .expect("context")
        .call(
            &run,
            std::slice::from_ref(&to_string),
            ExecutionLimits::default(),
        )
        .expect("caught throw after in-execution collection");
    let result = result
        .as_number()
        .expect("live result")
        .expect("Number result");
    assert!(result.strict_equals(JsNumber::from_i32(1)));
    assert_eq!(runtime.usage().heap_objects(), baseline.heap_objects() + 1);
    assert!(
        runtime.collection_pending,
        "dropping frame-only roots after an in-execution collection must leave the collector dirty"
    );

    let result = runtime
        .context(&realm)
        .expect("context")
        .call(&run, &[], ExecutionLimits::default())
        .expect("next execution safe point");
    let result = result
        .as_number()
        .expect("live result")
        .expect("Number result");
    assert!(result.strict_equals(JsNumber::from_i32(7)));
    assert_eq!(
        runtime.usage(),
        baseline,
        "the next root-free safe point must reclaim the frame-only cycle"
    );
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
        let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
            realm,
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
            realm,
        })
        .retained_values(),
        3
    );
}

#[test]
fn operator_primitive_continuations_charge_every_suspended_javascript_value() {
    let (mut runtime, realm, constructor, _native) = runtime_with_function_constructor();
    let object = source_object(&mut runtime, realm);
    let array = runtime.allocate_array(realm, Vec::new()).expect("array");
    let origin = native_function_host_origin();
    let continuation = |target| {
        NativeContinuation::OperatorPrimitive(OperatorPrimitiveContinuation {
            receiver: StoredValue::Object(object),
            realm,
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
    assert_eq!(
        continuation(OperatorPrimitiveTarget::ArrayLengthWrite(
            ArrayLengthWriteState {
                base: StoredValue::Object(array),
                name: JsString::from_utf8("length").expect("length"),
                strict: true,
                original: Some(StoredValue::Object(object)),
                first_length: None,
            },
        ))
        .retained_values(),
        3,
        "the receiver, array base, and original first-pass RHS are retained"
    );
    assert_eq!(
        continuation(OperatorPrimitiveTarget::ArrayLengthWrite(
            ArrayLengthWriteState {
                base: StoredValue::Object(array),
                name: JsString::from_utf8("length").expect("length"),
                strict: true,
                original: None,
                first_length: Some(1),
            },
        ))
        .retained_values(),
        2,
        "the second pass retains its receiver and array base"
    );
}

#[test]
fn number_to_string_radix_conversion_obeys_frame_and_value_limits() {
    let (mut runtime, to_string, native, radix) =
        runtime_with_number_to_string_radix_method(RuntimeLimits::default());
    runtime.limits.max_active_frames = 1;
    let baseline = runtime.usage();
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
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
    let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_object_properties(406))
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
            limit: 406,
            observed: 407,
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
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());

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

#[test]
fn function_prototype_apply_checks_callable_before_touching_the_list_and_preserves_abrupt_getters()
{
    let getter_authority =
        compile_test_function("function lengthGetter(){throw 73;}", "lengthGetter");
    let target_authority = compile_test_function("function target(){return 1;}", "target");
    let (mut runtime, realm, invoke, apply) = runtime_with_apply_invoker();
    let (getter, target) = {
        let mut context = runtime.context(&realm).expect("context");
        (
            context
                .instantiate(getter_authority)
                .expect("throwing length getter"),
            context.instantiate(target_authority).expect("target"),
        )
    };
    let realm_id = runtime.context(&realm).expect("context").realm;
    let list = source_object(&mut runtime, realm_id);
    runtime
        .append_accessor_property(
            HeapReference::Object(list),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::accessor(false, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("length accessor");
    let non_callable = source_object(&mut runtime, realm_id);
    let list = runtime
        .public_value(StoredValue::Object(list))
        .expect("list root");
    let non_callable = runtime
        .public_value(StoredValue::Object(non_callable))
        .expect("non-callable root");
    let receiver = runtime
        .public_value(StoredValue::Null)
        .expect("receiver root");

    let error = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            &[apply.as_value(), non_callable, receiver, list.clone()],
            ExecutionLimits::default(),
        )
        .expect_err("non-callable target");
    assert_execution_engine_error(error, ExceptionKind::TypeError, "not a function");

    let receiver = runtime
        .public_value(StoredValue::Null)
        .expect("receiver root");
    let error = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            &[apply.as_value(), target.as_value(), receiver, list],
            ExecutionLimits::default(),
        )
        .expect_err("length getter throw");
    let ExecutionError::Exception(exception) = error else {
        panic!("length getter throw must remain a JavaScript exception");
    };
    let thrown = exception
        .thrown_value()
        .expect("explicit getter throw")
        .as_number()
        .expect("live throw")
        .expect("number throw");
    assert!(thrown.strict_equals(JsNumber::from_i32(73)));
}

#[test]
fn function_prototype_apply_treats_null_and_undefined_lists_as_zero_arguments() {
    let (mut runtime, realm, invoke, apply) = runtime_with_apply_invoker();
    let realm_id = runtime.context(&realm).expect("context").realm;
    let global = runtime
        .realm_global_object(realm_id)
        .expect("global object");
    let StoredValue::Function(target) = read_heap_property(
        &runtime,
        HeapReference::Object(global),
        &runtime.predefined_property_key(PredefinedAtom::String),
    )
    .expect("global String") else {
        panic!("global String must be callable");
    };
    let target = runtime
        .public_value(StoredValue::Function(target))
        .expect("String root")
        .into_function()
        .expect("String function");

    for list in [StoredValue::Null, StoredValue::Undefined] {
        let receiver = runtime
            .public_value(StoredValue::Undefined)
            .expect("receiver root");
        let list = runtime.public_value(list).expect("list root");
        let result = runtime
            .context(&realm)
            .expect("context")
            .call(
                &invoke,
                &[apply.as_value(), target.as_value(), receiver, list],
                ExecutionLimits::default(),
            )
            .expect("zero-argument apply");

        assert_eq!(
            result
                .as_string()
                .expect("live result")
                .expect("string result")
                .to_utf8_lossy()
                .expect("UTF-8 result"),
            "",
            "zero arguments must remain distinguishable from one undefined argument"
        );
    }
}

#[test]
fn function_prototype_apply_observes_length_conversion_and_index_mutation_in_order() {
    let authority = compile_test_function(
        "function run(){\
             let trace={name:''};\
             let list={\
                 get length(){\
                     trace.name=trace.name+'L';\
                     return {valueOf(){trace.name=trace.name+'V';return 2;}};\
                 },\
                 get 0(){trace.name=trace.name+'0';list[1]=9;return 5;},\
                 1:7\
             };\
             function target(first,second){\
                 'use strict';\
                 this.name=this.name+'T';\
                 return first*100+second;\
             }\
             let result=target.apply(trace,list);\
             return trace.name+':'+result;\
         }",
        "run",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let run = runtime
        .context(&realm)
        .expect("context")
        .instantiate(authority)
        .expect("run");

    let result = runtime
        .context(&realm)
        .expect("context")
        .call(&run, &[], ExecutionLimits::default())
        .expect("getter-backed apply");
    assert_eq!(
        result
            .as_string()
            .expect("live result")
            .expect("string result")
            .to_utf8_lossy()
            .expect("UTF-8 result"),
        "LV0T:509"
    );
}

#[test]
fn function_prototype_apply_rejects_every_non_nullish_primitive_list_exactly() {
    let target_authority = compile_test_function("function target(){return 1;}", "target");
    let (mut runtime, realm, invoke, apply) = runtime_with_apply_invoker();
    let target = runtime
        .context(&realm)
        .expect("context")
        .instantiate(target_authority)
        .expect("target");
    let primitives = {
        let mut context = runtime.context(&realm).expect("context");
        vec![
            context.boolean(false),
            context.number(JsNumber::from_i32(7)),
            context.string(JsString::from_utf8("xy").expect("string")),
            context.symbol(None).expect("symbol"),
        ]
    };

    for primitive in primitives {
        let receiver = runtime
            .public_value(StoredValue::Undefined)
            .expect("receiver root");
        let error = runtime
            .context(&realm)
            .expect("context")
            .call(
                &invoke,
                &[apply.as_value(), target.as_value(), receiver, primitive],
                ExecutionLimits::default(),
            )
            .expect_err("primitive argument list");
        assert_execution_engine_error(error, ExceptionKind::TypeError, "not a object");
    }
}

#[test]
fn function_prototype_apply_uses_to_length_and_enforces_quickjs_argument_limit() {
    let target_authority = compile_test_function(
        "function target(first,second,third){\
             if(first===void 0)return 0;\
             if(third===void 0)return first*10+second;\
             return 999;\
         }",
        "target",
    );
    let (mut runtime, realm, invoke, apply) = runtime_with_apply_invoker();
    let target = runtime
        .context(&realm)
        .expect("context")
        .instantiate(target_authority)
        .expect("target");
    let realm_id = runtime.context(&realm).expect("context").realm;

    for (length, expected) in [
        (JsNumber::from_i32(-1), 0),
        (JsNumber::from_f64(f64::NAN), 0),
        (JsNumber::from_f64(2.9), 46),
    ] {
        let list = source_object(&mut runtime, realm_id);
        append_apply_list_data(&mut runtime, list, length);
        let list = runtime
            .public_value(StoredValue::Object(list))
            .expect("list root");
        let receiver = runtime
            .public_value(StoredValue::Undefined)
            .expect("receiver root");
        let result = runtime
            .context(&realm)
            .expect("context")
            .call(
                &invoke,
                &[apply.as_value(), target.as_value(), receiver, list],
                ExecutionLimits::default(),
            )
            .expect("ToLength apply");
        let result = result
            .as_number()
            .expect("live result")
            .expect("number result");
        assert!(result.strict_equals(JsNumber::from_i32(expected)));
    }

    let oversized = source_object(&mut runtime, realm_id);
    runtime
        .append_data_property(
            HeapReference::Object(oversized),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_i32(65_535)),
        )
        .expect("oversized length");
    let oversized = runtime
        .public_value(StoredValue::Object(oversized))
        .expect("oversized list root");
    let receiver = runtime
        .public_value(StoredValue::Undefined)
        .expect("receiver root");
    let error = runtime
        .context(&realm)
        .expect("context")
        .call(
            &invoke,
            &[apply.as_value(), target.as_value(), receiver, oversized],
            ExecutionLimits::default(),
        )
        .expect_err("argument limit");
    assert_execution_engine_error(
        error,
        ExceptionKind::RangeError,
        "too many arguments in function call (only 65534 allowed)",
    );
}

#[test]
fn function_prototype_apply_precharges_the_index_scan_before_the_first_getter() {
    let getter_authority = compile_test_function("function first(){throw 91;}", "first");
    let target_authority = compile_test_function("function target(){return 1;}", "target");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (getter, target) = {
        let mut context = runtime.context(&realm).expect("context");
        (
            context.instantiate(getter_authority).expect("getter"),
            context.instantiate(target_authority).expect("target"),
        )
    };
    let realm_id = runtime.context(&realm).expect("context").realm;
    let list = source_object(&mut runtime, realm_id);
    runtime
        .append_data_property(
            HeapReference::Object(list),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_i32(1)),
        )
        .expect("list length");
    runtime
        .append_accessor_property(
            HeapReference::Object(list),
            PropertyKey::from_index(ArrayIndex::new(0).expect("array index")),
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("first index getter");
    let (apply, native) = function_prototype_apply_native(&runtime, realm_id);
    let length_lookup_work = {
        let mut preview = ExecutionBudget::new(ExecutionLimits::default());
        charge_heap_property_lookup(&runtime, &StoredValue::Object(list), &mut preview)
            .unwrap_or_else(|_| panic!("preview length lookup"));
        preview.executed_instructions
    };
    let fuel = length_lookup_work.saturating_add(1);
    let mut budget = ExecutionBudget::new(ExecutionLimits::default().with_instruction_fuel(fuel));

    let Err(error) = begin_test_function_apply(
        &mut runtime,
        apply,
        native,
        target.id().expect("target id"),
        StoredValue::Undefined,
        list,
        0,
        0,
        &mut budget,
    ) else {
        panic!("the fixed index scan must exhaust fuel before dispatching its first getter");
    };
    assert!(matches!(
        error,
        NativeFailure::Execution(ExecutionError::InstructionLimitExceeded {
            limit,
            executed,
        }) if limit == fuel && executed == fuel
    ));
    assert_eq!(
        budget.executed_instructions, fuel,
        "the failed all-or-nothing scan charge must not exceed its fixed limit"
    );
}

#[test]
fn function_prototype_apply_native_preprocessing_and_target_share_one_fuel_budget() {
    let target_authority = compile_test_function("function target(){return 19;}", "target");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let target = runtime
        .context(&realm)
        .expect("context")
        .instantiate(target_authority)
        .expect("target");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let list = source_object(&mut runtime, realm_id);
    runtime
        .append_data_property(
            HeapReference::Object(list),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_i32(0)),
        )
        .expect("empty list length");
    let (apply, native) = function_prototype_apply_native(&runtime, realm_id);
    let length_lookup_work = {
        let mut preview = ExecutionBudget::new(ExecutionLimits::default());
        charge_heap_property_lookup(&runtime, &StoredValue::Object(list), &mut preview)
            .unwrap_or_else(|_| panic!("preview length lookup"));
        preview.executed_instructions
    };
    let mut budget =
        ExecutionBudget::new(ExecutionLimits::default().with_instruction_fuel(length_lookup_work));
    let Ok(dispatch) = begin_test_function_apply(
        &mut runtime,
        apply,
        native,
        target.id().expect("target id"),
        StoredValue::Undefined,
        list,
        0,
        0,
        &mut budget,
    ) else {
        panic!("native preprocessing must succeed");
    };
    let Ok(dispatch) =
        resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("target frame admission must succeed");
    };
    let NativeDispatch::Frame(target_frame) = dispatch else {
        panic!("a bytecode target must produce a frame");
    };
    assert_eq!(
        budget.executed_instructions, length_lookup_work,
        "apply preprocessing must debit the execution's shared budget"
    );

    let error = execute_prepared_frames_with_budget(
        &mut runtime,
        vec![target_frame],
        None,
        None,
        &mut budget,
    )
    .expect_err("the target must receive only the fuel left after native preprocessing");
    assert!(matches!(
        error,
        ExecutionError::InstructionLimitExceeded {
            limit,
            executed,
        } if limit == length_lookup_work && executed == length_lookup_work
    ));
}

#[test]
fn function_prototype_apply_forwarded_through_call_does_not_charge_transient_call_values() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default().with_max_active_frame_values(4))
        .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let function_prototype = runtime
        .realm_function_prototype(realm_id)
        .expect("Function.prototype");
    let (apply, _) = function_prototype_apply_native(&runtime, realm_id);
    let expected_call = NativeFunction {
        realm: realm_id,
        kind: NativeFunctionKind::FunctionPrototypeCall,
    };
    let call = runtime
        .functions
        .iter()
        .find_map(|(id, function)| {
            (function.native().copied() == Some(expected_call)).then_some(id)
        })
        .expect("Function.prototype.call");
    let list = source_object(&mut runtime, realm_id);
    runtime
        .append_data_property(
            HeapReference::Object(list),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_i32(0)),
        )
        .expect("empty list length");
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());

    let dispatch = dispatch_native_call(
        &mut runtime,
        call,
        expected_call,
        CallInputs {
            receiver: StoredValue::Function(apply),
            arguments: CallArguments::from_values(vec![
                StoredValue::Function(function_prototype),
                StoredValue::Undefined,
                StoredValue::Object(list),
            ]),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    )
    .unwrap_or_else(|_| panic!("begin call forwarding"));
    let dispatch = resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
        .unwrap_or_else(|_| panic!("resolve call forwarding"));
    assert!(
        matches!(dispatch, NativeDispatch::Immediate(StoredValue::Undefined)),
        "the forwarding buffer must not be charged as values reserved by active frames"
    );
}

#[test]
fn function_prototype_apply_preflights_frame_and_value_limits_before_the_length_getter() {
    for (limits, active_frames, active_values, expected_resource, expected_limit, expected) in [
        (
            RuntimeLimits::default().with_max_active_frames(1),
            1,
            0,
            RuntimeResource::Frames,
            1,
            2,
        ),
        (
            RuntimeLimits::default().with_max_active_frame_values(3),
            0,
            0,
            RuntimeResource::FrameValues,
            3,
            4,
        ),
    ] {
        let getter_authority =
            compile_test_function("function lengthGetter(){throw 67;}", "lengthGetter");
        let target_authority = compile_test_function("function target(){return 1;}", "target");
        let mut runtime = Runtime::try_new(limits).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let (getter, target) = {
            let mut context = runtime.context(&realm).expect("context");
            (
                context
                    .instantiate(getter_authority)
                    .expect("length getter"),
                context.instantiate(target_authority).expect("target"),
            )
        };
        let realm_id = runtime.context(&realm).expect("context").realm;
        let list = source_object(&mut runtime, realm_id);
        runtime
            .append_accessor_property(
                HeapReference::Object(list),
                runtime.predefined_property_key(PredefinedAtom::Length),
                PropertyLayout::accessor(false, true),
                Some(getter.id().expect("getter id")),
                None,
            )
            .expect("length accessor");
        let (apply, native) = function_prototype_apply_native(&runtime, realm_id);
        let mut budget = ExecutionBudget::new(ExecutionLimits::default());

        let Err(error) = begin_test_function_apply(
            &mut runtime,
            apply,
            native,
            target.id().expect("target id"),
            StoredValue::Undefined,
            list,
            active_frames,
            active_values,
            &mut budget,
        ) else {
            panic!("apply admission must reject the suspended state before reading length");
        };
        assert!(matches!(
            error,
            NativeFailure::Execution(ExecutionError::LimitExceeded {
                resource,
                limit,
                observed,
            }) if resource == expected_resource
                && limit == expected_limit
                && observed == expected
        ));
        assert_eq!(
            budget.executed_instructions, 0,
            "limit admission must precede both the length getter and native work"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the GC regression keeps construction, collection, resumption, and final reclamation in one lifecycle"
)]
fn function_prototype_apply_traces_gathered_heap_arguments_across_a_later_getter() {
    let getter_authority = compile_test_function("function second(){return 9;}", "second");
    let target_authority =
        compile_test_function("function target(first){return first.value;}", "target");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (getter, target) = {
        let mut context = runtime.context(&realm).expect("context");
        (
            context.instantiate(getter_authority).expect("getter"),
            context.instantiate(target_authority).expect("target"),
        )
    };
    let realm_id = runtime.context(&realm).expect("context").realm;
    let gathered = source_object(&mut runtime, realm_id);
    runtime
        .append_data_property(
            HeapReference::Object(gathered),
            runtime.predefined_property_key(PredefinedAtom::Value),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_i32(91)),
        )
        .expect("gathered value");
    let list = source_object(&mut runtime, realm_id);
    runtime
        .append_data_property(
            HeapReference::Object(list),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_i32(2)),
        )
        .expect("list length");
    let first_key = PropertyKey::from_index(ArrayIndex::new(0).expect("first index"));
    runtime
        .append_data_property(
            HeapReference::Object(list),
            first_key.clone(),
            PropertyLayout::data(true, true, true),
            StoredValue::Object(gathered),
        )
        .expect("first argument");
    runtime
        .append_accessor_property(
            HeapReference::Object(list),
            PropertyKey::from_index(ArrayIndex::new(1).expect("second index")),
            PropertyLayout::accessor(true, true),
            Some(getter.id().expect("getter id")),
            None,
        )
        .expect("second argument getter");
    let _list_root = runtime
        .public_value(StoredValue::Object(list))
        .expect("list root");
    let (apply, native) = function_prototype_apply_native(&runtime, realm_id);
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = begin_test_function_apply(
        &mut runtime,
        apply,
        native,
        target.id().expect("target id"),
        StoredValue::Undefined,
        list,
        0,
        0,
        &mut budget,
    ) else {
        panic!("apply scan must reach the later getter");
    };
    let Ok(dispatch) =
        resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("later getter frame admission must succeed");
    };
    let NativeDispatch::Frame(getter_frame) = dispatch else {
        panic!("the later indexed getter must suspend apply");
    };

    assert!(
        runtime
            .object_record_mut(HeapReference::Object(list))
            .expect("list")
            .replace_existing_data(&first_key, StoredValue::Undefined),
        "test setup must remove the list's original edge"
    );
    runtime.collection_pending = true;
    collect_cycles_with_execution_roots(
        &mut runtime,
        std::slice::from_ref(&getter_frame),
        &[],
        &[],
    )
    .expect("collection with suspended apply roots");
    assert!(
        runtime.heap_reference_is_live(HeapReference::Object(gathered)),
        "the gathered argument must remain rooted only by the apply continuation"
    );

    let result = execute_prepared_frames_with_budget(
        &mut runtime,
        vec![getter_frame],
        None,
        None,
        &mut budget,
    )
    .expect("resume apply after collection");
    let StoredValue::Number(result) = result else {
        panic!("target must return the gathered object's value");
    };
    assert!(result.strict_equals(JsNumber::from_i32(91)));

    runtime.collect_cycles().expect("release gathered argument");
    assert!(
        !runtime.heap_reference_is_live(HeapReference::Object(gathered)),
        "the gathered argument must stop being a root after apply completes"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the abrupt-completion regression keeps getter ordering and continuation cleanup in one lifecycle"
)]
fn function_prototype_apply_getter_throw_stops_later_gets_and_target_then_releases_arguments() {
    let throwing_authority = compile_test_function("function throwing(){throw 77;}", "throwing");
    let later_authority = compile_test_function("function later(){throw 88;}", "later");
    let target_authority = compile_test_function("function target(){throw 99;}", "target");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let (throwing, later, target) = {
        let mut context = runtime.context(&realm).expect("context");
        (
            context
                .instantiate(throwing_authority)
                .expect("throwing getter"),
            context.instantiate(later_authority).expect("later getter"),
            context.instantiate(target_authority).expect("target"),
        )
    };
    let realm_id = runtime.context(&realm).expect("context").realm;
    let gathered = source_object(&mut runtime, realm_id);
    let list = source_object(&mut runtime, realm_id);
    runtime
        .append_data_property(
            HeapReference::Object(list),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(JsNumber::from_i32(3)),
        )
        .expect("list length");
    let first_key = PropertyKey::from_index(ArrayIndex::new(0).expect("first index"));
    runtime
        .append_data_property(
            HeapReference::Object(list),
            first_key.clone(),
            PropertyLayout::data(true, true, true),
            StoredValue::Object(gathered),
        )
        .expect("gathered first argument");
    runtime
        .append_accessor_property(
            HeapReference::Object(list),
            PropertyKey::from_index(ArrayIndex::new(1).expect("throwing index")),
            PropertyLayout::accessor(true, true),
            Some(throwing.id().expect("throwing getter id")),
            None,
        )
        .expect("throwing indexed getter");
    runtime
        .append_accessor_property(
            HeapReference::Object(list),
            PropertyKey::from_index(ArrayIndex::new(2).expect("later index")),
            PropertyLayout::accessor(true, true),
            Some(later.id().expect("later getter id")),
            None,
        )
        .expect("later indexed getter");
    let _list_root = runtime
        .public_value(StoredValue::Object(list))
        .expect("list root");
    let usage_before_call = runtime.usage();
    let (apply, native) = function_prototype_apply_native(&runtime, realm_id);
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let Ok(dispatch) = begin_test_function_apply(
        &mut runtime,
        apply,
        native,
        target.id().expect("target id"),
        StoredValue::Object(list),
        list,
        0,
        0,
        &mut budget,
    ) else {
        panic!("apply scan must reach the throwing getter");
    };
    let Ok(dispatch) =
        resolve_native_dispatch(&mut runtime, dispatch, &[], 0, 0, None, &mut budget)
    else {
        panic!("throwing getter frame admission must succeed");
    };
    let NativeDispatch::Frame(getter_frame) = dispatch else {
        panic!("the throwing indexed getter must suspend apply");
    };
    let error = execute_prepared_frames_with_budget(
        &mut runtime,
        vec![getter_frame],
        None,
        None,
        &mut budget,
    )
    .expect_err("the indexed getter throw must escape apply");
    let ExecutionError::Exception(exception) = error else {
        panic!("the getter throw must remain a JavaScript exception");
    };
    let thrown = exception
        .thrown_value()
        .expect("explicit getter throw")
        .as_number()
        .expect("live throw")
        .expect("numeric throw");
    assert!(thrown.strict_equals(JsNumber::from_i32(77)));
    assert!(matches!(
        read_heap_property(&runtime, HeapReference::Object(list), &first_key)
            .expect("first argument"),
        StoredValue::Object(object) if object == gathered
    ));

    assert!(
        runtime
            .object_record_mut(HeapReference::Object(list))
            .expect("list")
            .replace_existing_data(&first_key, StoredValue::Undefined),
        "test cleanup must remove the list's source edge"
    );
    runtime.collection_pending = true;
    let report = runtime
        .collect_cycles()
        .expect("release the abandoned gathered argument");
    assert_eq!(
        report.objects(),
        1,
        "only the gathered argument abandoned by the abrupt continuation is unreachable"
    );
    assert!(
        !runtime.heap_reference_is_live(HeapReference::Object(gathered)),
        "the abrupt path must release gathered arguments"
    );
    assert_eq!(
        runtime.usage().heap_objects() + 1,
        usage_before_call.heap_objects()
    );
    assert_eq!(
        runtime.usage().object_properties(),
        usage_before_call.object_properties(),
        "clearing an indexed slot must not leak property charges"
    );
}

#[test]
fn function_prototype_apply_calls_a_dynamic_function_across_realms() {
    let invoke_authority = compile_test_function(
        "function invoke(apply,target,receiver,list){\
             return apply.call(target,receiver,list);\
         }",
        "invoke",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let target_realm = runtime.create_realm().expect("target realm");
    let caller_realm = runtime.create_realm().expect("caller realm");
    let target_realm_id = runtime
        .context(&target_realm)
        .expect("target context")
        .realm;
    let caller_realm_id = runtime
        .context(&caller_realm)
        .expect("caller context")
        .realm;
    let constructor = {
        let global = runtime
            .realm_global_object(target_realm_id)
            .expect("target global");
        let StoredValue::Function(constructor) = read_heap_property(
            &runtime,
            HeapReference::Object(global),
            &runtime.predefined_property_key(PredefinedAtom::Function),
        )
        .expect("global Function") else {
            panic!("global Function must be callable");
        };
        runtime
            .public_value(StoredValue::Function(constructor))
            .expect("Function root")
            .into_function()
            .expect("Function value")
    };
    let dynamic_arguments = {
        let context = runtime.context(&target_realm).expect("target context");
        [
            context.string(JsString::from_utf8("a").expect("parameter a")),
            context.string(JsString::from_utf8("b").expect("parameter b")),
            context.string(JsString::from_utf8("return a*10+b;").expect("body")),
        ]
    };
    let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(OxcDynamicCompiler);
    let target = runtime
        .context(&target_realm)
        .expect("target context")
        .call_with_dynamic_function_compiler(
            &constructor,
            &dynamic_arguments,
            ExecutionLimits::default(),
            &compiler,
        )
        .expect("dynamic target")
        .into_function()
        .expect("dynamic function");
    let invoke = runtime
        .context(&caller_realm)
        .expect("caller context")
        .instantiate(invoke_authority)
        .expect("apply invoker");
    let apply = public_function_prototype_apply(&mut runtime, caller_realm_id);
    let list = source_object(&mut runtime, caller_realm_id);
    append_apply_list_data(&mut runtime, list, JsNumber::from_i32(2));
    let list = runtime
        .public_value(StoredValue::Object(list))
        .expect("cross-realm list root");
    let receiver = runtime
        .public_value(StoredValue::Undefined)
        .expect("receiver root");

    let result = runtime
        .context(&caller_realm)
        .expect("caller context")
        .call(
            &invoke,
            &[apply.as_value(), target.as_value(), receiver, list],
            ExecutionLimits::default(),
        )
        .expect("cross-realm dynamic apply");
    let result = result
        .as_number()
        .expect("live result")
        .expect("number result");
    assert!(result.strict_equals(JsNumber::from_i32(46)));
}

#[test]
fn foreign_nonconstructor_type_errors_use_the_constructing_frame_realm() {
    let invoke_authority = compile_test_function(
        "function invoke(candidate){\
             try{new candidate();}catch(error){return error;}\
         }",
        "invoke",
    );
    let maker_authority =
        compile_test_function("function make(){return ({method(){}}).method;}", "make");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let target_realm = runtime.create_realm().expect("target realm");
    let caller_realm = runtime.create_realm().expect("caller realm");
    let target_realm_id = runtime
        .context(&target_realm)
        .expect("target context")
        .realm;
    let caller_realm_id = runtime
        .context(&caller_realm)
        .expect("caller context")
        .realm;
    let maker = runtime
        .context(&target_realm)
        .expect("target context")
        .instantiate(maker_authority)
        .expect("method maker");
    let bytecode_candidate = runtime
        .context(&target_realm)
        .expect("target context")
        .call(&maker, &[], ExecutionLimits::default())
        .expect("foreign method")
        .into_function()
        .expect("method function");
    let function_prototype = runtime
        .realm_function_prototype(target_realm_id)
        .expect("foreign Function.prototype");
    let native_candidate = runtime
        .public_value(StoredValue::Function(function_prototype))
        .expect("Function.prototype root")
        .into_function()
        .expect("Function.prototype");
    let invoke = runtime
        .context(&caller_realm)
        .expect("caller context")
        .instantiate(invoke_authority)
        .expect("constructor invoker");
    let caller_type_error = runtime
        .realm_error_prototype(caller_realm_id, ExceptionKind::TypeError)
        .expect("caller TypeError.prototype");
    let target_type_error = runtime
        .realm_error_prototype(target_realm_id, ExceptionKind::TypeError)
        .expect("target TypeError.prototype");

    for (kind, candidate) in [
        ("bytecode", bytecode_candidate),
        ("native", native_candidate),
    ] {
        let error = runtime
            .context(&caller_realm)
            .expect("caller context")
            .call(&invoke, &[candidate.as_value()], ExecutionLimits::default())
            .expect("caught nonconstructor TypeError");
        let error = error.object_id().expect("materialized TypeError object");
        let prototype = runtime
            .object_record(HeapReference::Object(error))
            .expect("TypeError object")
            .prototype();

        assert_eq!(
            prototype,
            Some(HeapReference::Object(caller_type_error)),
            "{kind} nonconstructor error must belong to the constructing frame realm"
        );
        assert_ne!(
            prototype,
            Some(HeapReference::Object(target_type_error)),
            "{kind} target realm must not own the operation error"
        );
    }
}

fn runtime_with_apply_invoker() -> (Runtime, crate::Realm, Function, Function) {
    let invoke_authority = compile_test_function(
        "function invoke(apply,target,receiver,list){\
             return apply.call(target,receiver,list);\
         }",
        "invoke",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let invoke = runtime
        .context(&realm)
        .expect("context")
        .instantiate(invoke_authority)
        .expect("apply invoker");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let apply = public_function_prototype_apply(&mut runtime, realm_id);
    (runtime, realm, invoke, apply)
}

fn public_function_prototype_apply(runtime: &mut Runtime, realm: RealmId) -> Function {
    let function_prototype = runtime
        .realm_function_prototype(realm)
        .expect("Function.prototype");
    let StoredValue::Function(apply) = read_heap_property(
        runtime,
        HeapReference::Function(function_prototype),
        &runtime.predefined_property_key(PredefinedAtom::Apply),
    )
    .expect("Function.prototype.apply") else {
        panic!("Function.prototype.apply must be callable");
    };
    runtime
        .public_value(StoredValue::Function(apply))
        .expect("apply root")
        .into_function()
        .expect("apply function")
}

fn function_prototype_apply_native(
    runtime: &Runtime,
    realm: RealmId,
) -> (FunctionId, NativeFunction) {
    let function_prototype = runtime
        .realm_function_prototype(realm)
        .expect("Function.prototype");
    let StoredValue::Function(apply) = read_heap_property(
        runtime,
        HeapReference::Function(function_prototype),
        &runtime.predefined_property_key(PredefinedAtom::Apply),
    )
    .expect("Function.prototype.apply") else {
        panic!("Function.prototype.apply must be callable");
    };
    let native = runtime
        .functions
        .get(apply)
        .and_then(HeapFunction::native)
        .copied()
        .expect("native Function.prototype.apply");
    (apply, native)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the apply admission tests provide every explicit execution-limit input"
)]
fn begin_test_function_apply(
    runtime: &mut Runtime,
    apply: FunctionId,
    native: NativeFunction,
    target: FunctionId,
    receiver: StoredValue,
    list: ObjectId,
    active_frames: usize,
    active_frame_values: u64,
    budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    dispatch_native_call(
        runtime,
        apply,
        native,
        CallInputs {
            receiver: StoredValue::Function(target),
            arguments: CallArguments::from_values(vec![receiver, StoredValue::Object(list)]),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        active_frames,
        active_frame_values,
        None,
        budget,
    )
}

fn append_apply_list_data(runtime: &mut Runtime, list: ObjectId, length: JsNumber) {
    runtime
        .append_data_property(
            HeapReference::Object(list),
            runtime.predefined_property_key(PredefinedAtom::Length),
            PropertyLayout::data(true, true, true),
            StoredValue::Number(length),
        )
        .expect("list length");
    for (index, value) in [(0, 4), (1, 6), (2, 8)] {
        runtime
            .append_data_property(
                HeapReference::Object(list),
                PropertyKey::from_index(ArrayIndex::new(index).expect("array index")),
                PropertyLayout::data(true, true, true),
                StoredValue::Number(JsNumber::from_i32(value)),
            )
            .expect("list element");
    }
}

fn function_prototype_bind_native(
    runtime: &mut Runtime,
    realm: RealmId,
) -> (FunctionId, NativeFunction) {
    let function_prototype = runtime
        .realm_function_prototype(realm)
        .expect("Function.prototype");
    let name_key = runtime
        .property_key_from_string(&JsString::from_utf8("bind").expect("bind name"))
        .expect("bind key");
    let StoredValue::Function(bind) = read_heap_property(
        runtime,
        HeapReference::Function(function_prototype),
        &name_key,
    )
    .expect("Function.prototype.bind") else {
        panic!("Function.prototype.bind must be callable");
    };
    let native = runtime
        .functions
        .get(bind)
        .and_then(HeapFunction::native)
        .copied()
        .expect("native Function.prototype.bind");
    (bind, native)
}

fn bind_target(
    runtime: &mut Runtime,
    bind: FunctionId,
    native: NativeFunction,
    target: FunctionId,
    arguments: Vec<StoredValue>,
) -> Result<FunctionId, ExecutionError> {
    let mut budget = ExecutionBudget::new(ExecutionLimits::default());
    let dispatch = match dispatch_native_call(
        runtime,
        bind,
        native,
        CallInputs {
            receiver: StoredValue::Function(target),
            arguments: CallArguments::from_values(arguments),
            new_target: None,
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        &mut budget,
    ) {
        Ok(dispatch) => dispatch,
        Err(NativeFailure::Abrupt(_) | NativeFailure::AbruptAfterTransient(_)) => {
            panic!("bind failed with an abrupt completion")
        }
        Err(NativeFailure::Execution(error)) => return Err(error),
    };
    let NativeDispatch::Immediate(StoredValue::Function(bound)) = dispatch else {
        panic!("bind must return its bound function immediately");
    };
    Ok(bound)
}

fn bound_own_length(runtime: &Runtime, bound: FunctionId) -> JsNumber {
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    let Some(OwnProperty::Data {
        value: StoredValue::Number(length),
        ..
    }) = runtime
        .functions
        .get(bound)
        .expect("bound function")
        .object
        .own_property(&length_key)
    else {
        panic!("bound function must have an own numeric length");
    };
    length
}

fn bound_own_name(runtime: &Runtime, bound: FunctionId) -> String {
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let Some(OwnProperty::Data {
        value: StoredValue::String(name),
        ..
    }) = runtime
        .functions
        .get(bound)
        .expect("bound function")
        .object
        .own_property(&name_key)
    else {
        panic!("bound function must have an own string name");
    };
    name.to_utf8_lossy().expect("UTF-8 name")
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "bind length expectations are exact binary64 values from the pinned QuickJS rules"
)]
#[expect(
    clippy::too_many_lines,
    reason = "every QuickJS bind-length rule stays one explicit table in the regression test"
)]
fn bind_length_uses_the_exact_quickjs_number_rules() {
    let target_authority = compile_test_function("function target(a, b, c) {}", "target");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let target = runtime
        .context(&realm)
        .expect("context")
        .instantiate(target_authority)
        .expect("target");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let target_id = target.id().expect("target id");
    let (bind, native) = function_prototype_bind_native(&mut runtime, realm_id);
    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);

    // The instantiated root function carries no own name/length metadata, so
    // the missing-property rule (QuickJS: no own `length` -> 0) applies.
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![StoredValue::Undefined],
    )
    .expect("plain bind");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 0.0);
    assert_eq!(
        bound_own_name(&runtime, bound),
        "bound ",
        "missing name becomes the empty string"
    );

    let set_target_length = |runtime: &mut Runtime, value: StoredValue| {
        let record = &mut runtime.functions.get_mut(target_id).expect("target").object;
        if !record.replace_existing_data(&length_key, value.duplicate()) {
            record
                .append_data(
                    length_key.clone(),
                    PropertyLayout::data(false, false, true),
                    value,
                )
                .expect("target length");
        }
    };
    set_target_length(&mut runtime, StoredValue::Number(JsNumber::from_i32(3)));
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![StoredValue::Undefined],
    )
    .expect("three-argument bind");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 3.0);
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![
            StoredValue::Undefined,
            StoredValue::Number(JsNumber::from_i32(1)),
        ],
    )
    .expect("one bound argument");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 2.0);
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![
            StoredValue::Undefined,
            StoredValue::Number(JsNumber::from_i32(1)),
            StoredValue::Number(JsNumber::from_i32(2)),
            StoredValue::Number(JsNumber::from_i32(3)),
        ],
    )
    .expect("excess bound arguments");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 0.0);

    set_target_length(&mut runtime, StoredValue::Number(JsNumber::from_f64(2.9)));
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![
            StoredValue::Undefined,
            StoredValue::Number(JsNumber::from_i32(1)),
        ],
    )
    .expect("fractional length bind");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 1.0);

    set_target_length(
        &mut runtime,
        StoredValue::Number(JsNumber::from_f64(f64::NAN)),
    );
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![StoredValue::Undefined],
    )
    .expect("NaN length bind");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 0.0);

    set_target_length(
        &mut runtime,
        StoredValue::String(JsString::from_utf8("abc").expect("string length")),
    );
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![StoredValue::Undefined],
    )
    .expect("string length bind");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 0.0);

    set_target_length(&mut runtime, StoredValue::Undefined);
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![StoredValue::Undefined],
    )
    .expect("undefined length bind");
    assert_eq!(bound_own_length(&runtime, bound).as_f64(), 0.0);
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "the host-call result is an exact binary64 constant"
)]
fn bind_public_prototype_call_and_host_dispatch_share_one_bound_function() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let (bind, native) = function_prototype_bind_native(&mut runtime, realm_id);
    let target_authority = compile_test_function("function target() { return 19; }", "target");
    let target = runtime
        .context(&realm)
        .expect("context")
        .instantiate(target_authority)
        .expect("target");
    let target_id = target.id().expect("target id");
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![StoredValue::Undefined],
    )
    .expect("bound target");
    let bound_value = runtime
        .public_value(StoredValue::Function(bound))
        .expect("bound root");
    let mut context = runtime.context(&realm).expect("context");
    let result = context
        .call(
            &bound_value.into_function().expect("bound function"),
            &[],
            ExecutionLimits::default(),
        )
        .expect("bound call");
    assert_eq!(
        result
            .as_number()
            .expect("live result")
            .expect("number")
            .as_f64(),
        19.0
    );
}

#[test]
fn bound_function_edges_keep_target_this_and_arguments_live_until_collection() {
    let target_authority = compile_test_function("function target(value) { return 0; }", "target");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm_id = runtime.context(&realm).expect("context").realm;
    let target = runtime
        .context(&realm)
        .expect("context")
        .instantiate(target_authority)
        .expect("target");
    let target_id = target.id().expect("target id");
    let (bind, native) = function_prototype_bind_native(&mut runtime, realm_id);
    let object_prototype = runtime
        .realm_object_prototype(realm_id)
        .expect("Object.prototype");
    let receiver = runtime
        .allocate_ordinary_object(object_prototype)
        .expect("bound this");
    let argument = runtime
        .allocate_ordinary_object(object_prototype)
        .expect("bound argument");
    let bound = bind_target(
        &mut runtime,
        bind,
        native,
        target_id,
        vec![StoredValue::Object(receiver), StoredValue::Object(argument)],
    )
    .expect("bound target");
    drop(target);
    let bound_root = runtime
        .public_value(StoredValue::Function(bound))
        .expect("bound root");

    let report = runtime
        .collect_cycles()
        .expect("collection with only bound edges");
    assert_eq!(
        report.functions(),
        0,
        "the bound function must keep its target alive"
    );
    assert_eq!(report.objects(), 0);
    assert!(runtime.functions.get(target_id).is_some());
    assert!(runtime.objects.get(receiver).is_some());
    assert!(runtime.objects.get(argument).is_some());

    drop(bound_root);
    runtime.collect_cycles().expect("post-root collection");
    assert!(
        runtime.functions.get(bound).is_none(),
        "the unrooted bound function must be reclaimed"
    );
    assert!(
        runtime.functions.get(target_id).is_none(),
        "the bound target must be reclaimed with its bound function"
    );
    assert!(runtime.objects.get(receiver).is_none());
    assert!(runtime.objects.get(argument).is_none());
}

fn assert_execution_engine_error(
    error: ExecutionError,
    expected_kind: ExceptionKind,
    expected_message: &str,
) {
    let ExecutionError::Exception(exception) = error else {
        panic!("expected JavaScript exception");
    };
    assert_eq!(exception.kind(), Some(expected_kind));
    assert_eq!(
        exception
            .message()
            .expect("engine error message")
            .to_utf8_lossy()
            .expect("UTF-8 message"),
        expected_message
    );
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
    let realm = runtime.function_realm(function).expect("function realm");
    let Ok(source) = function_to_string(runtime, function, realm, None) else {
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

fn runtime_with_array_constructor_prototype_getter(
    getter_source: &str,
) -> (Runtime, RealmId, FunctionId, NativeFunction, FunctionId) {
    runtime_with_primitive_constructor_prototype_getter(getter_source, PredefinedAtom::Array)
}

fn runtime_with_array_constructor() -> (Runtime, RealmId, FunctionId, NativeFunction) {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let realm = runtime.context(&realm).expect("context").realm;
    let global = runtime.realm_global_object(realm).expect("global object");
    let key = runtime.predefined_property_key(PredefinedAtom::Array);
    let StoredValue::Function(constructor) =
        read_heap_property(&runtime, HeapReference::Object(global), &key).expect("Array property")
    else {
        panic!("global Array is not callable");
    };
    let native = runtime
        .functions
        .get(constructor)
        .and_then(HeapFunction::native)
        .copied()
        .expect("native Array");
    (runtime, realm, constructor, native)
}

fn global_native_function(runtime: &Runtime, realm: RealmId, atom: PredefinedAtom) -> FunctionId {
    let global = runtime.realm_global_object(realm).expect("global object");
    let key = runtime.predefined_property_key(atom);
    let StoredValue::Function(function) =
        read_heap_property(runtime, HeapReference::Object(global), &key).expect("global function")
    else {
        panic!("global intrinsic is not callable");
    };
    function
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
    budget: &mut ExecutionBudget,
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

fn begin_test_array_construction(
    runtime: &mut Runtime,
    constructor: FunctionId,
    native: NativeFunction,
    new_target: FunctionId,
    arguments: Vec<StoredValue>,
    budget: &mut ExecutionBudget,
) -> NativeDispatch {
    let Ok(dispatch) = dispatch_native_call(
        runtime,
        constructor,
        native,
        CallInputs {
            receiver: StoredValue::Undefined,
            arguments: CallArguments::from_values(arguments),
            new_target: Some(new_target),
        },
        None,
        Some(native_function_host_origin()),
        0,
        0,
        None,
        budget,
    ) else {
        panic!("accessor-backed Array construction must start");
    };
    dispatch
}

fn assert_array_object_index(runtime: &Runtime, array: ObjectId, index: u32, expected: ObjectId) {
    assert!(matches!(
        runtime
            .array_own_property(
                array,
                &PropertyKey::from_index(ArrayIndex::new(index).expect("index")),
            )
            .expect("array index"),
        Some(OwnProperty::Data {
            value: StoredValue::Object(object),
            ..
        }) if object == expected
    ));
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
    budget: &mut ExecutionBudget,
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
