/*
 * JavaScript bytecode execution and closure semantics derived from QuickJS.
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

//! Installed templates, closure materialization, and captured binding access.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn code(runtime: &Runtime, id: InstalledCodeId) -> Result<&InstalledCode, EngineFault> {
    runtime.code.get(id).ok_or(EngineFault::StaleHeapEdge {
        edge: "installed code",
        index: id.index(),
        generation: id.generation(),
    })
}

pub(super) fn installed_template(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
) -> Result<&InstalledTemplate, EngineFault> {
    let code = code(runtime, code_id)?;
    let index = usize::try_from(template.get())
        .map_err(|_| EngineFault::InvalidClosureEnvironment { function: template })?;
    code.templates
        .get(index)
        .ok_or(EngineFault::InvalidClosureEnvironment { function: template })
}

pub(super) fn materialize_constant(
    runtime: &mut Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
    index: u32,
) -> Result<StoredValue, ExecutionError> {
    let elements = match installed_template(runtime, code_id, template)?
        .constants
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "constant",
            index,
        })? {
        InstalledConstant::Number(value) => return Ok(StoredValue::Number(*value)),
        InstalledConstant::String(value) => return Ok(StoredValue::String(value.clone())),
        InstalledConstant::BigInt(value) => {
            return Ok(StoredValue::BigInt(Arc::clone(value)));
        }
        InstalledConstant::TemplateObject(value) => {
            if let Some(object) = value.object {
                return Ok(StoredValue::Object(object));
            }
            Arc::clone(&value.elements)
        }
        InstalledConstant::Function(_) => {
            return Err(EngineFault::MissingPoolEntry {
                pool: "ordinary value constant",
                index,
            }
            .into());
        }
    };
    let realm = code(runtime, code_id)?.realm;
    let object = runtime.allocate_template_object(realm, &elements)?;
    let installed = runtime
        .code
        .get_mut(code_id)
        .and_then(|code| {
            usize::try_from(template.get())
                .ok()
                .and_then(|index| code.templates.get_mut(index))
        })
        .and_then(|template| template.constants.get_mut(index as usize))
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "template object constant",
            index,
        })?;
    let InstalledConstant::TemplateObject(template_object) = installed else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "template object constant",
            index,
        }
        .into());
    };
    if template_object.object.replace(object).is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "template object cache was populated during non-observable allocation",
        }
        .into());
    }
    Ok(StoredValue::Object(object))
}

pub(super) fn function_constant(
    runtime: &Runtime,
    code_id: InstalledCodeId,
    template: FunctionTemplateId,
    index: u32,
) -> Result<FunctionTemplateId, ExecutionError> {
    match installed_template(runtime, code_id, template)?
        .constants
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "function constant",
            index,
        })? {
        InstalledConstant::Function(function) => Ok(*function),
        InstalledConstant::Number(_)
        | InstalledConstant::String(_)
        | InstalledConstant::BigInt(_)
        | InstalledConstant::TemplateObject(_) => Err(EngineFault::MissingPoolEntry {
            pool: "function constant",
            index,
        }
        .into()),
    }
}

/// Finishes the runtime half of a verified base-class definition. The paired
/// closure already owns the code and intrinsic function slots; this installs
/// the public prototype object and the exact class-only constructor metadata.
pub(super) fn define_base_class(
    runtime: &mut Runtime,
    frame: &Frame,
    constructor: FunctionId,
    name: JsString,
) -> Result<ObjectId, ExecutionError> {
    if !bytecode_function_is_class_constructor(runtime, constructor)? {
        return Err(EngineFault::RuntimeInvariant {
            message: "define_class did not receive a class-constructor closure",
        }
        .into());
    }
    let realm = code(runtime, frame.code)?.realm;
    let prototype = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);

    runtime.append_data_property(
        HeapReference::Object(prototype),
        constructor_key,
        PropertyLayout::data(true, false, true),
        StoredValue::Function(constructor),
    )?;
    runtime.append_data_property(
        HeapReference::Function(constructor),
        prototype_key,
        PropertyLayout::data(false, false, false),
        StoredValue::Object(prototype),
    )?;
    let renamed = runtime
        .object_record_mut(HeapReference::Function(constructor))?
        .replace_existing_with_data(
            &name_key,
            PropertyLayout::data(false, false, true),
            StoredValue::String(name),
        );
    if renamed.is_none() {
        return Err(EngineFault::RuntimeInvariant {
            message: "class-constructor closure lost its name property",
        }
        .into());
    }
    Ok(prototype)
}

#[allow(
    clippy::too_many_lines,
    reason = "closure validation, capture materialization, and publication are one transaction"
)]
pub(super) fn create_closure(
    runtime: &mut Runtime,
    frame: &mut Frame,
    child: FunctionTemplateId,
) -> Result<FunctionId, ExecutionError> {
    let parent = runtime
        .functions
        .get(frame.function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "active function",
            index: frame.function.index(),
            generation: frame.function.generation(),
        })?;
    let parent = parent.bytecode()?;
    if parent.code != frame.code
        || parent.template != frame.template
        || parent.environment.as_slice() != frame.environment.as_slice()
    {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        }
        .into());
    }
    let (
        sources,
        expected,
        realm,
        function_name,
        defined_argument_count,
        has_prototype,
        function_kind,
        executable_kind,
    ) = {
        let code = code(runtime, frame.code)?;
        let function = code
            .authority
            .function(child)
            .ok_or(EngineFault::InvalidClosureEnvironment { function: child })?;
        let source = function.function().closure_sources();
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(source.len())
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: source.len(),
            })?;
        copied.extend_from_slice(source);
        let installed_index = usize::try_from(child.get())
            .map_err(|_| EngineFault::InvalidClosureEnvironment { function: child })?;
        let installed = code
            .templates
            .get(installed_index)
            .ok_or(EngineFault::InvalidClosureEnvironment { function: child })?;
        let function_name = function.metadata().function_name().map_or_else(
            || Ok(JsString::empty()),
            |index| {
                installed
                    .atoms
                    .get(index.get() as usize)
                    .and_then(AtomDescription::description)
                    .cloned()
                    .ok_or(EngineFault::MissingPoolEntry {
                        pool: "function name atom",
                        index: index.get(),
                    })
            },
        )?;
        let header = function.function().control_flow().function_header();
        (
            copied,
            function.metadata().closures().len(),
            code.realm,
            function_name,
            header.defined_argument_count(),
            header.flags().has_prototype(),
            header.kind(),
            function.metadata().executable_kind(),
        )
    };
    let lexical = executable_kind == CompilerExecutableKind::OrdinaryArrow;
    let generator = function_kind == FunctionKind::Generator;
    let asynchronous = function_kind == FunctionKind::Async;
    let async_generator = function_kind == FunctionKind::AsyncGenerator;
    let creates_prototype = has_prototype || generator || async_generator;
    let function_prototype = if async_generator {
        HeapReference::Object(runtime.realm_async_generator_function_prototype(realm)?)
    } else if generator {
        HeapReference::Object(runtime.realm_generator_function_prototype(realm)?)
    } else if asynchronous {
        HeapReference::Object(runtime.realm_async_function_prototype(realm)?)
    } else {
        HeapReference::Function(runtime.realm_function_prototype(realm)?)
    };
    let object_prototype = if async_generator {
        Some(runtime.realm_async_generator_prototype(realm)?)
    } else if generator {
        Some(runtime.realm_generator_prototype(realm)?)
    } else {
        has_prototype
            .then(|| runtime.realm_object_prototype(realm))
            .transpose()?
    };
    if sources.len() != expected {
        return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
    }

    let mut capture_plans = Vec::new();
    capture_plans
        .try_reserve_exact(sources.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: sources.len(),
        })?;
    let mut pending_by_own = Vec::new();
    pending_by_own
        .try_reserve_exact(frame.own_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: frame.own_cells.len(),
        })?;
    pending_by_own.resize(frame.own_cells.len(), None);
    let mut pending_cells = Vec::new();
    pending_cells
        .try_reserve_exact(sources.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: sources.len(),
        })?;

    for source in sources {
        match source {
            CompilerClosureSource::ParentVariableReference(index) => {
                let own_index = index as usize;
                let own_cell = frame.own_cells.get(own_index).copied().ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "own variable-reference",
                        index,
                    },
                )?;
                let address = *frame.own_cell_bindings.get(own_index).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "own captured binding",
                        index,
                    },
                )?;
                if let Some(cell) = own_cell {
                    if !runtime.cells.contains(cell) {
                        return Err(EngineFault::StaleHeapEdge {
                            edge: "binding cell",
                            index: cell.index(),
                            generation: cell.generation(),
                        }
                        .into());
                    }
                    capture_plans.push(ClosureCapturePlan::Existing(EnvironmentBinding::Captured(
                        cell,
                    )));
                    continue;
                }

                if let Some(pending) = pending_by_own[own_index] {
                    capture_plans.push(ClosureCapturePlan::New(pending));
                    continue;
                }

                let binding = match address {
                    FrameBindingAddress::Argument(binding) => frame_argument(frame, binding)?,
                    FrameBindingAddress::Local(binding) => frame_local(frame, binding)?,
                };
                let FrameBinding::Direct(value) = binding else {
                    return Err(EngineFault::InvalidClosureEnvironment {
                        function: frame.template,
                    }
                    .into());
                };
                let pending = pending_cells.len();
                pending_cells.push(PendingOwnCell {
                    own_index,
                    address,
                    value: value.duplicate(),
                });
                pending_by_own[own_index] = Some(pending);
                capture_plans.push(ClosureCapturePlan::New(pending));
            }
            CompilerClosureSource::ParentClosure(index) => {
                let binding = *frame.environment.get(index as usize).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "parent closure",
                        index,
                    },
                )?;
                match binding {
                    EnvironmentBinding::Captured(cell) => {
                        if !runtime.cells.contains(cell) {
                            return Err(EngineFault::StaleHeapEdge {
                                edge: "closure cell",
                                index: cell.index(),
                                generation: cell.generation(),
                            }
                            .into());
                        }
                    }
                    EnvironmentBinding::RealmGlobal(global) => {
                        let valid = runtime
                            .global_bindings
                            .get(global)
                            .is_some_and(|binding| binding.realm == realm);
                        if !valid {
                            return Err(EngineFault::StaleHeapEdge {
                                edge: "realm global binding",
                                index: global.index(),
                                generation: global.generation(),
                            }
                            .into());
                        }
                    }
                }
                capture_plans.push(ClosureCapturePlan::Existing(binding));
            }
            CompilerClosureSource::ConstructorRealmGlobal(_) => {
                return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
            }
        }
    }

    check_execution_limit(
        RuntimeResource::HeapFunctions,
        runtime.limits.max_heap_functions,
        usize_to_u64(runtime.functions.len()).saturating_add(1),
    )?;
    let function_property_count = 2_usize + usize::from(creates_prototype);
    let prototype_property_count = usize::from(has_prototype);
    let new_property_count = function_property_count.saturating_add(prototype_property_count);
    check_execution_limit(
        RuntimeResource::HeapObjects,
        runtime.limits.max_heap_objects,
        usize_to_u64(runtime.objects.len()).saturating_add(usize::from(creates_prototype) as u64),
    )?;
    check_execution_limit(
        RuntimeResource::ObjectProperties,
        runtime.limits.max_object_properties,
        runtime
            .object_properties
            .saturating_add(usize_to_u64(new_property_count)),
    )?;
    check_execution_limit(
        RuntimeResource::BindingCells,
        runtime.limits.max_binding_cells,
        usize_to_u64(runtime.cells.len()).saturating_add(usize_to_u64(pending_cells.len())),
    )?;
    runtime
        .functions
        .try_reserve(1)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        })?;
    runtime
        .objects
        .try_reserve(usize::from(creates_prototype))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapObjects,
            additional: usize::from(creates_prototype),
        })?;
    runtime
        .cells
        .try_reserve(pending_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending_cells.len(),
        })?;
    let mut environment = Vec::new();
    environment
        .try_reserve_exact(capture_plans.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: capture_plans.len(),
        })?;

    let mut new_cells = Vec::new();
    new_cells
        .try_reserve_exact(pending_cells.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending_cells.len(),
        })?;

    let length_key = runtime.predefined_property_key(PredefinedAtom::Length);
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    let mut function_record = crate::object::ObjectRecord::empty(Some(function_prototype));
    function_record
        .try_reserve_data(function_property_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: function_property_count,
        })?;
    function_record
        .append_data(
            length_key,
            PropertyLayout::data(false, false, true),
            StoredValue::Number(JsNumber::from_f64(f64::from(defined_argument_count))),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    function_record
        .append_data(
            name_key,
            PropertyLayout::data(false, false, true),
            StoredValue::String(function_name),
        )
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })?;
    let mut prototype_record = object_prototype.map(|object_prototype| {
        crate::object::ObjectRecord::empty(Some(HeapReference::Object(object_prototype)))
    });
    if has_prototype && let Some(record) = prototype_record.as_mut() {
        record
            .try_reserve_data(1)
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                constructor_key.clone(),
                PropertyLayout::data(true, false, true),
                StoredValue::Undefined,
            )
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
    }

    for pending in &pending_cells {
        if let Ok(cell) = runtime.cells.try_insert(BindingCell {
            value: pending.value.duplicate(),
        }) {
            new_cells.push(cell);
        } else {
            for cell in new_cells {
                let removed = runtime.cells.remove(cell);
                debug_assert!(removed.is_some());
            }
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: 1,
            });
        }
    }

    for capture in capture_plans {
        let binding = match capture {
            ClosureCapturePlan::Existing(binding) => binding,
            ClosureCapturePlan::New(index) => {
                let Some(cell) = new_cells.get(index).copied() else {
                    rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
                    return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
                };
                EnvironmentBinding::Captured(cell)
            }
        };
        environment.push(binding);
    }

    for (pending, cell) in pending_cells.iter().zip(new_cells.iter().copied()) {
        let binding = match pending.address {
            FrameBindingAddress::Argument(index) => frame_argument_mut(frame, index),
            FrameBindingAddress::Local(index) => frame_local_mut(frame, index),
        };
        let binding = match binding {
            Ok(binding) => binding,
            Err(fault) => {
                rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
                return Err(fault.into());
            }
        };
        *binding = FrameBinding::Captured(cell);
        frame.own_cells[pending.own_index] = Some(cell);
    }

    let prototype_object = if let Some(record) = prototype_record {
        let Ok(object) = runtime.insert_heap_object(crate::object::HeapObject::ordinary(record))
        else {
            rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            });
        };
        if function_record
            .append_data(
                prototype_key,
                PropertyLayout::data(true, false, false),
                StoredValue::Object(object),
            )
            .is_err()
        {
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
            rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            });
        }
        Some(object)
    } else {
        None
    };

    let Ok(function) = runtime.insert_heap_function(HeapFunction {
        implementation: FunctionImplementation::Bytecode(BytecodeFunction {
            code: frame.code,
            template: child,
            environment,
            lexical_receiver: lexical.then(|| frame.receiver.duplicate()),
            lexical_new_target: if lexical { frame.new_target } else { None },
        }),
        object: function_record,
        public_roots: 0,
    }) else {
        if let Some(object) = prototype_object {
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
        }
        rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
        return Err(ExecutionError::AllocationFailed {
            resource: RuntimeResource::HeapFunctions,
            additional: 1,
        });
    };
    if has_prototype && let Some(object) = prototype_object {
        let updated = runtime.objects.get_mut(object).is_some_and(|prototype| {
            prototype
                .record
                .replace_existing_data(&constructor_key, StoredValue::Function(function))
        });
        if !updated {
            let removed = runtime.functions.remove(function);
            debug_assert!(removed.is_some());
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
            rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
            return Err(EngineFault::RuntimeInvariant {
                message: "new ordinary function prototype lost its constructor property",
            }
            .into());
        }
    }
    let Some(code) = runtime.code.get_mut(frame.code) else {
        let removed = runtime.functions.remove(function);
        debug_assert!(removed.is_some());
        if let Some(object) = prototype_object {
            let removed = runtime.objects.remove(object);
            debug_assert!(removed.is_some());
        }
        rollback_new_cells(runtime, frame, &pending_cells, &new_cells);
        return Err(EngineFault::StaleHeapEdge {
            edge: "installed code",
            index: frame.code.index(),
            generation: frame.code.generation(),
        }
        .into());
    };
    code.live_functions = code.live_functions.saturating_add(1);
    runtime.object_properties = runtime
        .object_properties
        .saturating_add(usize_to_u64(new_property_count));
    runtime.collection_pending = true;
    Ok(function)
}

fn rollback_new_cells(
    runtime: &mut Runtime,
    frame: &mut Frame,
    pending_cells: &[PendingOwnCell],
    new_cells: &[BindingCellId],
) {
    for (pending, cell) in pending_cells.iter().zip(new_cells.iter().copied()) {
        let binding = match pending.address {
            FrameBindingAddress::Argument(index) => frame.arguments.get_mut(index as usize),
            FrameBindingAddress::Local(index) => frame.locals.get_mut(index as usize),
        };
        if let Some(binding) = binding {
            *binding = FrameBinding::Direct(pending.value.duplicate());
        }
        if let Some(own_cell) = frame.own_cells.get_mut(pending.own_index) {
            *own_cell = None;
        }
        let removed = runtime.cells.remove(cell);
        debug_assert!(removed.is_some());
    }
}

pub(super) fn close_local(
    runtime: &Runtime,
    frame: &mut Frame,
    local: u32,
) -> Result<(), ExecutionError> {
    let Some(index) = frame.own_cell_bindings.iter().position(
        |address| matches!(address, FrameBindingAddress::Local(index) if *index == local),
    ) else {
        return Err(EngineFault::MissingPoolEntry {
            pool: "captured local",
            index: local,
        }
        .into());
    };
    let Some(cell) = frame.own_cells[index] else {
        return Ok(());
    };
    let value = runtime
        .cells
        .get(cell)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: cell.index(),
            generation: cell.generation(),
        })?
        .value
        .duplicate();
    *frame_local_mut(frame, local)? = FrameBinding::Direct(value);
    frame.own_cells[index] = None;
    Ok(())
}

pub(super) enum BindingAccessError {
    Uninitialized,
    Fault(EngineFault),
}

impl From<BindingAccessError> for ExecutionError {
    fn from(error: BindingAccessError) -> Self {
        match error {
            BindingAccessError::Uninitialized => EngineFault::UnexpectedUninitialized {
                function: FunctionTemplateId::new(u32::MAX),
            }
            .into(),
            BindingAccessError::Fault(fault) => fault.into(),
        }
    }
}

pub(super) fn duplicate_binding(
    runtime: &Runtime,
    binding: &FrameBinding,
    checked: bool,
    frame: &Frame,
) -> Result<StoredValue, BindingAccessError> {
    let value = match binding {
        FrameBinding::Direct(value) => value,
        FrameBinding::Captured(cell) => {
            &runtime
                .cells
                .get(*cell)
                .ok_or_else(|| {
                    BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                        edge: "binding cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    })
                })?
                .value
        }
    };
    match value {
        SlotValue::Uninitialized if checked => Err(BindingAccessError::Uninitialized),
        SlotValue::Uninitialized => Err(BindingAccessError::Fault(
            EngineFault::UnexpectedUninitialized {
                function: frame.template,
            },
        )),
        SlotValue::Value(value) => Ok(value.duplicate()),
    }
}

pub(super) fn binding_is_uninitialized(
    runtime: &Runtime,
    binding: &FrameBinding,
) -> Result<bool, EngineFault> {
    Ok(match binding {
        FrameBinding::Direct(value) => matches!(value, SlotValue::Uninitialized),
        FrameBinding::Captured(cell) => matches!(
            runtime
                .cells
                .get(*cell)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "binding cell",
                    index: cell.index(),
                    generation: cell.generation(),
                })?
                .value,
            SlotValue::Uninitialized
        ),
    })
}

pub(super) fn write_argument(
    runtime: &mut Runtime,
    frame: &mut Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    write_binding(runtime, frame_argument_mut(frame, index)?, value)
}

pub(super) fn write_local(
    runtime: &mut Runtime,
    frame: &mut Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    write_binding(runtime, frame_local_mut(frame, index)?, value)
}

fn write_binding(
    runtime: &mut Runtime,
    binding: &mut FrameBinding,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    match binding {
        FrameBinding::Direct(current) => *current = value,
        FrameBinding::Captured(cell) => {
            runtime
                .cells
                .get_mut(*cell)
                .ok_or(EngineFault::StaleHeapEdge {
                    edge: "binding cell",
                    index: cell.index(),
                    generation: cell.generation(),
                })?
                .value = value;
            runtime.collection_pending = true;
        }
    }
    Ok(())
}

pub(super) fn duplicate_environment(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
    checked: bool,
) -> Result<StoredValue, BindingAccessError> {
    let binding = *frame.environment.get(index as usize).ok_or({
        BindingAccessError::Fault(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })
    })?;
    let EnvironmentBinding::Captured(cell) = binding else {
        return Err(BindingAccessError::Fault(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        ));
    };
    let value = &runtime
        .cells
        .get(cell)
        .ok_or_else(|| {
            BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })
        })?
        .value;
    match value {
        SlotValue::Uninitialized if checked => Err(BindingAccessError::Uninitialized),
        SlotValue::Uninitialized => Err(BindingAccessError::Fault(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        )),
        SlotValue::Value(value) => Ok(value.duplicate()),
    }
}

pub(super) fn environment_is_uninitialized(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<bool, EngineFault> {
    let binding = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })?;
    let EnvironmentBinding::Captured(cell) = binding else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        });
    };
    Ok(matches!(
        runtime
            .cells
            .get(cell)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: cell.index(),
                generation: cell.generation(),
            })?
            .value,
        SlotValue::Uninitialized
    ))
}

pub(super) fn write_environment(
    runtime: &mut Runtime,
    frame: &Frame,
    index: u32,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    let binding = *frame
        .environment
        .get(index as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "closure environment",
            index,
        })?;
    let EnvironmentBinding::Captured(cell) = binding else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        }
        .into());
    };
    runtime
        .cells
        .get_mut(cell)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: cell.index(),
            generation: cell.generation(),
        })?
        .value = value;
    runtime.collection_pending = true;
    Ok(())
}
