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

#[allow(
    clippy::too_many_lines,
    reason = "caller-cell admission, reservation, promotion, and rollback journaling form one auditable transaction"
)]
pub(super) fn materialize_direct_eval_environment(
    runtime: &mut Runtime,
    frame: &mut Frame,
    authority: &quickjs_bytecode::VerifiedBytecode,
    caller_bindings: &[DirectEvalCallerBinding],
) -> Result<DirectEvalEnvironment, ExecutionError> {
    let mut environment_size = None;
    for source in authority.root().function().closure_sources() {
        let size = match *source {
            CompilerClosureSource::DirectEvalBinding {
                environment_size, ..
            }
            | CompilerClosureSource::DirectEvalVariable {
                environment_size, ..
            } => environment_size as usize,
            CompilerClosureSource::ParentVariableReference(_)
            | CompilerClosureSource::ParentClosure(_)
            | CompilerClosureSource::ConstructorRealmGlobal(_)
            | CompilerClosureSource::Module { .. } => continue,
        };
        if size < caller_bindings.len()
            || environment_size
                .replace(size)
                .is_some_and(|expected| expected != size)
        {
            return Err(EngineFault::RuntimeInvariant {
                message: "direct-eval authority expects an inconsistent environment shape",
            }
            .into());
        }
    }
    let environment_size = environment_size.unwrap_or(caller_bindings.len());
    let mut bindings = Vec::new();
    bindings
        .try_reserve_exact(environment_size)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: environment_size,
        })?;
    bindings.resize(environment_size, None);
    let mut selected = Vec::new();
    selected
        .try_reserve_exact(environment_size)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: environment_size,
        })?;
    selected.resize(environment_size, false);
    let mut pending = Vec::new();
    pending
        .try_reserve_exact(caller_bindings.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: caller_bindings.len(),
        })?;
    let mut pending_variables = Vec::new();
    pending_variables
        .try_reserve_exact(environment_size.saturating_sub(caller_bindings.len()))
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: environment_size.saturating_sub(caller_bindings.len()),
        })?;

    let root = authority.root();
    for (closure, source) in root.function().closure_sources().iter().enumerate() {
        let CompilerClosureSource::DirectEvalBinding { index, .. } = *source else {
            if let CompilerClosureSource::DirectEvalVariable { index, .. } = *source {
                let external_index = index as usize;
                if external_index < caller_bindings.len() {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "new direct-eval variable overlaps the caller snapshot",
                    }
                    .into());
                }
                let selected_entry =
                    selected
                        .get_mut(external_index)
                        .ok_or(EngineFault::RuntimeInvariant {
                            message: "new direct-eval variable indexes outside its environment",
                        })?;
                if std::mem::replace(selected_entry, true) {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "direct-eval authority imports one environment entry twice",
                    }
                    .into());
                }
                let definition = root.metadata().closures().get(closure).ok_or(
                    EngineFault::RuntimeInvariant {
                        message: "new direct-eval variable lost its closure metadata",
                    },
                )?;
                let atom = definition.name().ok_or(EngineFault::RuntimeInvariant {
                    message: "new direct-eval variable has no name",
                })?;
                let atom = root.function().atoms().get(atom.get() as usize).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "new direct-eval variable atom",
                        index: atom.get(),
                    },
                )?;
                let name = runtime_string(atom.string())?;
                if pending_variables
                    .iter()
                    .any(|pending: &PendingDirectEvalVariable| pending.name == name)
                {
                    return Err(EngineFault::RuntimeInvariant {
                        message: "direct-eval authority creates one variable more than once",
                    }
                    .into());
                }
                pending_variables.push(PendingDirectEvalVariable {
                    external_index,
                    name,
                });
            }
            continue;
        };
        let external_index = index as usize;
        if external_index >= caller_bindings.len() {
            return Err(EngineFault::RuntimeInvariant {
                message: "direct-eval caller binding indexes outside the caller snapshot",
            }
            .into());
        }
        let caller = caller_bindings
            .get(external_index)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "direct-eval authority indexes outside the caller environment",
            })?;
        let selected_entry =
            selected
                .get_mut(external_index)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "direct-eval caller selection is missing",
                })?;
        if std::mem::replace(selected_entry, true) {
            return Err(EngineFault::RuntimeInvariant {
                message: "direct-eval authority imports one caller binding more than once",
            }
            .into());
        }
        let location = caller.location();
        match location {
            DirectEvalCallerBindingLocation::Closure(index) => {
                let binding = *frame.environment.get(index as usize).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "direct-eval caller closure",
                        index,
                    },
                )?;
                let EnvironmentBinding::Captured(cell) = binding else {
                    return Err(EngineFault::InvalidClosureEnvironment {
                        function: frame.template,
                    }
                    .into());
                };
                if !runtime.cells.contains(cell) {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "direct-eval caller cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    }
                    .into());
                }
                bindings[external_index] = Some(binding);
            }
            DirectEvalCallerBindingLocation::Argument(index)
            | DirectEvalCallerBindingLocation::Local(index) => {
                let address = match location {
                    DirectEvalCallerBindingLocation::Argument(_) => {
                        FrameBindingAddress::Argument(index)
                    }
                    DirectEvalCallerBindingLocation::Local(_) => FrameBindingAddress::Local(index),
                    DirectEvalCallerBindingLocation::Closure(_)
                    | DirectEvalCallerBindingLocation::EvalVariable { .. } => unreachable!(),
                };
                let frame_binding = match address {
                    FrameBindingAddress::Argument(index) => frame_argument(frame, index)?,
                    FrameBindingAddress::Local(index) => frame_local(frame, index)?,
                };
                match frame_binding {
                    FrameBinding::Captured(cell) => {
                        if !runtime.cells.contains(*cell) {
                            return Err(EngineFault::StaleHeapEdge {
                                edge: "direct-eval caller cell",
                                index: cell.index(),
                                generation: cell.generation(),
                            }
                            .into());
                        }
                        bindings[external_index] = Some(EnvironmentBinding::Captured(*cell));
                    }
                    FrameBinding::Direct(value) => {
                        let own_index = frame
                            .own_cell_bindings
                            .iter()
                            .position(|candidate| *candidate == address);
                        if let Some(own) = own_index {
                            let own_cell = frame.own_cells.get(own).ok_or(
                                EngineFault::InvalidClosureEnvironment {
                                    function: frame.template,
                                },
                            )?;
                            if own_cell.is_some() {
                                return Err(EngineFault::InvalidClosureEnvironment {
                                    function: frame.template,
                                }
                                .into());
                            }
                        }
                        pending.push(PendingDirectEvalCell {
                            external_index,
                            location,
                            own_index,
                            value: value.duplicate(),
                        });
                    }
                }
            }
            DirectEvalCallerBindingLocation::EvalVariable { depth, index } => {
                let cell = direct_eval_variable_cell(frame, depth, index)?;
                if !runtime.cells.contains(cell) {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "direct-eval variable cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    }
                    .into());
                }
                bindings[external_index] = Some(EnvironmentBinding::Captured(cell));
            }
        }
    }

    let variable_environment = if pending_variables.is_empty() {
        None
    } else {
        let environment = frame
            .eval_declaration_environment
            .as_ref()
            .map(Rc::clone)
            .ok_or(EngineFault::RuntimeInvariant {
                message: "new direct-eval variables require a function environment",
            })?;
        let mut record = environment.borrow_mut();
        let original_len = record.bindings.len();
        for pending in &pending_variables {
            if record
                .bindings
                .iter()
                .any(|binding| !binding.deleted && binding.name == pending.name)
            {
                return Err(EngineFault::RuntimeInvariant {
                    message: "new direct-eval variable already exists in the function environment",
                }
                .into());
            }
        }
        record
            .bindings
            .try_reserve_exact(pending_variables.len())
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: pending_variables.len(),
            })?;
        drop(record);
        Some((environment, original_len))
    };

    let inserted_count = pending.len().checked_add(pending_variables.len()).ok_or(
        EngineFault::RuntimeInvariant {
            message: "direct-eval binding cell count overflowed",
        },
    )?;
    check_execution_limit(
        RuntimeResource::BindingCells,
        runtime.limits.max_binding_cells,
        usize_to_u64(runtime.cells.len()).saturating_add(usize_to_u64(inserted_count)),
    )?;
    runtime
        .cells
        .try_reserve(inserted_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: inserted_count,
        })?;
    let mut new_cells = Vec::new();
    new_cells
        .try_reserve_exact(inserted_count)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: inserted_count,
        })?;
    let mut created_cells = Vec::new();
    created_cells
        .try_reserve_exact(pending.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending.len(),
        })?;
    let mut created_variable_cells = Vec::new();
    created_variable_cells
        .try_reserve_exact(pending_variables.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::BindingCells,
            additional: pending_variables.len(),
        })?;
    for entry in &pending {
        let Ok(cell) = runtime.cells.try_insert(BindingCell {
            value: entry.value.duplicate(),
            forward: None,
        }) else {
            for cell in new_cells {
                let removed = runtime.cells.remove(cell);
                debug_assert!(removed.is_some());
            }
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: 1,
            });
        };
        new_cells.push(cell);
    }
    for _ in &pending_variables {
        let Ok(cell) = runtime.cells.try_insert(BindingCell {
            value: SlotValue::Value(StoredValue::Undefined),
            forward: None,
        }) else {
            for cell in new_cells {
                let removed = runtime.cells.remove(cell);
                debug_assert!(removed.is_some());
            }
            return Err(ExecutionError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: 1,
            });
        };
        new_cells.push(cell);
    }

    for (entry, cell) in pending
        .iter()
        .zip(new_cells.iter().copied().take(pending.len()))
    {
        let frame_binding = match entry.location {
            DirectEvalCallerBindingLocation::Argument(index) => {
                &mut frame.arguments[index as usize]
            }
            DirectEvalCallerBindingLocation::Local(index) => &mut frame.locals[index as usize],
            DirectEvalCallerBindingLocation::Closure(_) => {
                unreachable!("only direct frame bindings are scheduled for promotion")
            }
            DirectEvalCallerBindingLocation::EvalVariable { .. } => {
                unreachable!("existing eval-variable cells are never promoted")
            }
        };
        *frame_binding = FrameBinding::Captured(cell);
        if let Some(own_index) = entry.own_index {
            frame.own_cells[own_index] = Some(cell);
        }
        bindings[entry.external_index] = Some(EnvironmentBinding::Captured(cell));
        created_cells.push(CreatedDirectEvalCell {
            location: entry.location,
            own_index: entry.own_index,
            cell,
        });
    }
    created_variable_cells.extend(new_cells.iter().copied().skip(pending.len()));
    if let Some((environment, _)) = &variable_environment {
        let mut record = environment.borrow_mut();
        for (entry, cell) in pending_variables.iter().zip(&created_variable_cells) {
            record.bindings.push(EvalVariableBinding {
                name: entry.name.clone(),
                cell: *cell,
                deleted: false,
            });
            bindings[entry.external_index] = Some(EnvironmentBinding::Captured(*cell));
        }
    }
    runtime.collection_pending |= inserted_count != 0;
    Ok(DirectEvalEnvironment {
        bindings,
        created_cells,
        created_variable_environment: variable_environment,
        created_variable_cells,
    })
}

fn direct_eval_variable_cell(
    frame: &Frame,
    depth: u32,
    index: u32,
) -> Result<BindingCellId, EngineFault> {
    let mut current = frame.eval_environment.as_ref().map(Rc::clone);
    for _ in 0..depth {
        let environment = current.ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })?;
        current = environment.borrow().parent.as_ref().map(Rc::clone);
    }
    let environment = current.ok_or(EngineFault::InvalidClosureEnvironment {
        function: frame.template,
    })?;
    let cell = environment
        .borrow()
        .bindings
        .get(index as usize)
        .filter(|binding| !binding.deleted)
        .map(|binding| binding.cell)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })?;
    Ok(cell)
}

fn validate_direct_eval_rollback(
    runtime: &Runtime,
    frame: &Frame,
    environment: &DirectEvalEnvironment,
) -> Result<(), EngineFault> {
    let mut distinct_cells = HashSet::new();
    for &cell in &environment.created_variable_cells {
        if !distinct_cells.insert(cell) || !runtime.cells.contains(cell) {
            return Err(EngineFault::StaleHeapEdge {
                edge: "direct-eval variable cell",
                index: cell.index(),
                generation: cell.generation(),
            });
        }
    }
    match &environment.created_variable_environment {
        Some((variable_environment, original_len)) => {
            let record = variable_environment.borrow();
            let expected_len = original_len
                .checked_add(environment.created_variable_cells.len())
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "direct-eval variable rollback length overflowed",
                })?;
            if record.bindings.len() != expected_len
                || !record.bindings[*original_len..]
                    .iter()
                    .zip(&environment.created_variable_cells)
                    .all(|(binding, cell)| binding.cell == *cell)
            {
                return Err(EngineFault::RuntimeInvariant {
                    message: "direct-eval variable environment changed before rollback",
                });
            }
        }
        None if !environment.created_variable_cells.is_empty() => {
            return Err(EngineFault::RuntimeInvariant {
                message: "direct-eval variable rollback lost its environment",
            });
        }
        None => {}
    }
    for created in &environment.created_cells {
        if !distinct_cells.insert(created.cell) || !runtime.cells.contains(created.cell) {
            return Err(EngineFault::StaleHeapEdge {
                edge: "direct-eval caller cell",
                index: created.cell.index(),
                generation: created.cell.generation(),
            });
        }
        let binding = match created.location {
            DirectEvalCallerBindingLocation::Argument(index) => frame_argument(frame, index),
            DirectEvalCallerBindingLocation::Local(index) => frame_local(frame, index),
            DirectEvalCallerBindingLocation::Closure(_) => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "a direct-eval closure binding entered the promotion journal",
                });
            }
            DirectEvalCallerBindingLocation::EvalVariable { .. } => {
                return Err(EngineFault::RuntimeInvariant {
                    message: "an existing eval-variable binding entered the promotion journal",
                });
            }
        }?;
        if !matches!(binding, FrameBinding::Captured(cell) if *cell == created.cell) {
            return Err(EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            });
        }
        if let Some(own_index) = created.own_index {
            let own_cell =
                frame
                    .own_cells
                    .get(own_index)
                    .ok_or(EngineFault::InvalidClosureEnvironment {
                        function: frame.template,
                    })?;
            if *own_cell != Some(created.cell) {
                return Err(EngineFault::InvalidClosureEnvironment {
                    function: frame.template,
                });
            }
        }
    }

    Ok(())
}

pub(super) fn rollback_direct_eval_environment(
    runtime: &mut Runtime,
    frame: &mut Frame,
    environment: DirectEvalEnvironment,
) -> Result<(), EngineFault> {
    validate_direct_eval_rollback(runtime, frame, &environment)?;

    if let Some((variable_environment, original_len)) = &environment.created_variable_environment {
        variable_environment
            .borrow_mut()
            .bindings
            .truncate(*original_len);
    }
    for cell in environment.created_variable_cells.into_iter().rev() {
        runtime
            .cells
            .remove(cell)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "direct-eval variable cell",
                index: cell.index(),
                generation: cell.generation(),
            })?;
    }

    for created in environment.created_cells.into_iter().rev() {
        let removed = runtime
            .cells
            .remove(created.cell)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "direct-eval caller cell",
                index: created.cell.index(),
                generation: created.cell.generation(),
            })?;
        let binding = match created.location {
            DirectEvalCallerBindingLocation::Argument(index) => {
                &mut frame.arguments[index as usize]
            }
            DirectEvalCallerBindingLocation::Local(index) => &mut frame.locals[index as usize],
            DirectEvalCallerBindingLocation::Closure(_) => {
                unreachable!("validated promotion journals contain only frame bindings")
            }
            DirectEvalCallerBindingLocation::EvalVariable { .. } => {
                unreachable!("validated promotion journals exclude eval-variable bindings")
            }
        };
        *binding = FrameBinding::Direct(removed.value);
        if let Some(own_index) = created.own_index {
            frame.own_cells[own_index] = None;
        }
    }
    Ok(())
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

/// Finishes the runtime half of a verified class definition. The paired
/// closure already owns its code and intrinsic function slots; this installs
/// the public prototype object, the selected constructor/prototype inheritance
/// links, and the exact class-only constructor metadata.
pub(super) fn define_class(
    runtime: &mut Runtime,
    constructor: FunctionId,
    name: JsString,
    constructor_parent: HeapReference,
    prototype_parent: Option<HeapReference>,
    has_instance_elements: bool,
) -> Result<ObjectId, ExecutionError> {
    if !bytecode_function_is_class_constructor(runtime, constructor)? {
        return Err(EngineFault::RuntimeInvariant {
            message: "define_class did not receive a class-constructor closure",
        }
        .into());
    }
    let prototype = runtime.allocate_ordinary_object_with_optional_prototype(prototype_parent)?;
    let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
    let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);

    runtime.append_data_property(
        HeapReference::Object(prototype),
        constructor_key,
        PropertyLayout::data(true, false, true),
        StoredValue::Function(constructor),
    )?;
    if !runtime.replace_prototype_checked(
        HeapReference::Function(constructor),
        Some(constructor_parent),
    )? {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified class definition created a circular constructor prototype chain",
        }
        .into());
    }
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
    set_function_home_object(runtime, constructor, HeapReference::Object(prototype))?;
    runtime
        .bytecode_function_mut(constructor)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "class definition lost its bytecode constructor",
        })?
        .has_instance_elements = has_instance_elements;
    Ok(prototype)
}

/// Resolves the compiler-only method that implements
/// `InitializeInstanceElements` for a verified class constructor.
///
/// The whole-graph verifier binds exactly one
/// `ClassInstanceInitializer` closure to a constructor that advertises
/// instance elements. Direct eval cannot name that source-invisible binding,
/// so contextual `super()` resolves it through the inherited constructor's
/// certified closure environment.
pub(super) fn class_instance_initializer(
    runtime: &Runtime,
    constructor: FunctionId,
) -> Result<FunctionId, ExecutionError> {
    let bytecode = runtime
        .bytecode_function(constructor)
        .ok_or(EngineFault::RuntimeInvariant {
            message: "derived class instance initializer owner is not bytecode",
        })?;
    if !bytecode.has_instance_elements {
        return Err(EngineFault::RuntimeInvariant {
            message: "derived class constructor has no instance elements",
        }
        .into());
    }
    let installed = code(runtime, bytecode.code)?;
    let verified = installed.authority.function(bytecode.template).ok_or(
        EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        },
    )?;
    if verified.metadata().closures().len() != bytecode.environment.len() {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        }
        .into());
    }
    let mut initializer_slot = None;
    for (index, definition) in verified.metadata().closures().iter().enumerate() {
        if definition.policy().kind() != CompilerBindingKind::ClassInstanceInitializer {
            continue;
        }
        if initializer_slot.replace(index).is_some() {
            return Err(EngineFault::RuntimeInvariant {
                message: "class constructor retained multiple instance initializers",
            }
            .into());
        }
    }
    let slot = initializer_slot.ok_or(EngineFault::RuntimeInvariant {
        message: "class constructor lost its instance initializer capture",
    })?;
    let EnvironmentBinding::Captured(cell) = bytecode.environment[slot] else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        }
        .into());
    };
    let SlotValue::Value(StoredValue::Function(initializer)) = &runtime
        .cells
        .get(cell)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "class instance initializer binding",
            index: cell.index(),
            generation: cell.generation(),
        })?
        .value
    else {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        }
        .into());
    };
    let initializer_bytecode =
        runtime
            .bytecode_function(*initializer)
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: bytecode.template,
            })?;
    let initializer_metadata = code(runtime, initializer_bytecode.code)?
        .authority
        .function(initializer_bytecode.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: initializer_bytecode.template,
        })?;
    if initializer_bytecode.code != bytecode.code
        || initializer_metadata.metadata().executable_kind()
            != CompilerExecutableKind::ClassInstanceInitializer
    {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: bytecode.template,
        }
        .into());
    }
    Ok(*initializer)
}

/// Installs the non-observable `[[HomeObject]]` edge for one compiler-created
/// method-like closure.  A closure receives this exactly once, at its paired
/// class or method definition site.
pub(super) fn set_function_home_object(
    runtime: &mut Runtime,
    function: FunctionId,
    home_object: HeapReference,
) -> Result<(), ExecutionError> {
    let function = runtime
        .functions
        .get_mut(function)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "function home object",
            index: function.index(),
            generation: function.generation(),
        })?;
    let FunctionImplementation::Bytecode(bytecode) = &mut function.implementation else {
        return Err(EngineFault::RuntimeInvariant {
            message: "method definition received a non-bytecode closure",
        }
        .into());
    };
    if bytecode.home_object.replace(home_object).is_some() {
        return Err(EngineFault::RuntimeInvariant {
            message: "method closure received its home object twice",
        }
        .into());
    }
    runtime.collection_pending = true;
    Ok(())
}

fn captured_binding_eval_shadow(
    frame: &Frame,
    source: CompilerClosureSource,
    shadowable: bool,
) -> Result<Option<EvalBindingShadow>, EngineFault> {
    if !shadowable {
        return Ok(None);
    }
    match source {
        CompilerClosureSource::ParentVariableReference(_) => {
            let Some(boundary) = frame.parameter_eval_boundary.as_ref() else {
                return Ok(None);
            };
            if frame.body_eval_environment.is_some() {
                return Ok(None);
            }
            let head =
                frame
                    .eval_environment
                    .as_ref()
                    .ok_or(EngineFault::InvalidClosureEnvironment {
                        function: frame.template,
                    })?;
            Ok(Some(EvalBindingShadow {
                head: Rc::clone(head),
                boundary: Some(Rc::clone(boundary)),
            }))
        }
        CompilerClosureSource::ParentClosure(index) => {
            let inherited = frame
                .environment_eval_shadows
                .get(index as usize)
                .ok_or(EngineFault::MissingPoolEntry {
                    pool: "closure eval shadow",
                    index,
                })?
                .clone();
            let owns_environment = match (
                frame.eval_environment.as_ref(),
                frame.inherited_eval_environment.as_ref(),
            ) {
                (Some(head), Some(inherited)) => !Rc::ptr_eq(head, inherited),
                (Some(_), None) => frame.eval_declaration_environment.is_some(),
                (None, _) => false,
            };
            if !owns_environment {
                return Ok(inherited);
            }
            let head =
                frame
                    .eval_environment
                    .as_ref()
                    .ok_or(EngineFault::InvalidClosureEnvironment {
                        function: frame.template,
                    })?;
            Ok(Some(EvalBindingShadow {
                head: Rc::clone(head),
                boundary: inherited
                    .as_ref()
                    .and_then(|shadow| shadow.boundary.as_ref().map(Rc::clone))
                    .or_else(|| frame.inherited_eval_environment.as_ref().map(Rc::clone)),
            }))
        }
        CompilerClosureSource::ConstructorRealmGlobal(_)
        | CompilerClosureSource::DirectEvalBinding { .. }
        | CompilerClosureSource::DirectEvalVariable { .. }
        | CompilerClosureSource::Module { .. } => Ok(None),
    }
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
    let parent_home_object = parent.home_object;
    let (
        sources,
        expected,
        realm,
        function_name,
        defined_argument_count,
        has_prototype,
        function_kind,
        executable_kind,
        lexical_derived_this,
        closure_eval_shadowable,
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
        let mut closure_eval_shadowable = Vec::new();
        closure_eval_shadowable
            .try_reserve_exact(function.metadata().closures().len())
            .map_err(|_| ExecutionError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: function.metadata().closures().len(),
            })?;
        closure_eval_shadowable.extend(
            function
                .metadata()
                .closures()
                .iter()
                .map(|definition| eval_can_shadow_binding(definition.policy().kind())),
        );
        (
            copied,
            function.metadata().closures().len(),
            code.realm,
            function_name,
            header.defined_argument_count(),
            header.flags().has_prototype(),
            header.kind(),
            function.metadata().executable_kind(),
            function.lexical_derived_this(),
            closure_eval_shadowable,
        )
    };
    let lexical = matches!(
        executable_kind,
        CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow
    );
    let (lexical_derived_constructor, lexical_derived_this_cell) = if lexical_derived_this {
        let constructor =
            frame
                .derived_constructor
                .ok_or(EngineFault::InvalidClosureEnvironment {
                    function: frame.template,
                })?;
        let cell = frame
            .derived_this_cell
            .ok_or(EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            })?;
        if !runtime.cells.contains(cell) {
            return Err(EngineFault::StaleHeapEdge {
                edge: "lexical derived-this binding",
                index: cell.index(),
                generation: cell.generation(),
            }
            .into());
        }
        (Some(constructor), Some(cell))
    } else {
        (None, None)
    };
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
    let mut capture_eval_shadows = Vec::new();
    capture_eval_shadows
        .try_reserve_exact(sources.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
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

    for (source, eval_shadowable) in sources.into_iter().zip(closure_eval_shadowable) {
        capture_eval_shadows.push(captured_binding_eval_shadow(
            frame,
            source,
            eval_shadowable,
        )?);
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
            CompilerClosureSource::ConstructorRealmGlobal(_)
            | CompilerClosureSource::DirectEvalBinding { .. }
            | CompilerClosureSource::DirectEvalVariable { .. } => {
                return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
            }
            CompilerClosureSource::Module { index } => {
                let binding = *frame.environment.get(index as usize).ok_or(
                    EngineFault::MissingPoolEntry {
                        pool: "module cell",
                        index,
                    },
                )?;
                let EnvironmentBinding::Captured(cell) = binding else {
                    return Err(EngineFault::InvalidClosureEnvironment { function: child }.into());
                };
                if !runtime.cells.contains(cell) {
                    return Err(EngineFault::StaleHeapEdge {
                        edge: "module cell",
                        index: cell.index(),
                        generation: cell.generation(),
                    }
                    .into());
                }
                capture_plans.push(ClosureCapturePlan::Existing(binding));
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
            forward: None,
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
            environment_eval_shadows: capture_eval_shadows,
            eval_environment: frame.eval_environment.as_ref().map(Rc::clone),
            lexical_receiver: lexical.then(|| frame.receiver.duplicate()),
            lexical_eval_in_function: lexical && frame.eval_context.in_function,
            lexical_eval_in_class_field_initializer: lexical
                && frame.eval_context.in_class_field_initializer,
            lexical_new_target: if lexical { frame.new_target } else { None },
            lexical_derived_constructor,
            lexical_derived_this: lexical_derived_this_cell,
            has_instance_elements: false,
            home_object: if lexical { parent_home_object } else { None },
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
    Missing,
    Fault(EngineFault),
}

impl From<BindingAccessError> for ExecutionError {
    fn from(error: BindingAccessError) -> Self {
        match error {
            BindingAccessError::Uninitialized => EngineFault::UnexpectedUninitialized {
                function: FunctionTemplateId::new(u32::MAX),
            }
            .into(),
            BindingAccessError::Missing => EngineFault::RuntimeInvariant {
                message: "a missing dynamic binding escaped opcode-level exception handling",
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
            let resolved = BindingCell::resolve_forward(runtime, *cell)
                .map_err(BindingAccessError::Fault)?;
            &runtime
                .cells
                .get(resolved)
                .ok_or_else(|| {
                    BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                        edge: "binding cell",
                        index: resolved.index(),
                        generation: resolved.generation(),
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

pub(super) fn duplicate_local(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
    checked: bool,
) -> Result<StoredValue, BindingAccessError> {
    if let Some(cell) =
        local_eval_shadow_cell(runtime, frame, index).map_err(BindingAccessError::Fault)?
    {
        return duplicate_binding(runtime, &FrameBinding::Captured(cell), checked, frame);
    }
    duplicate_binding(
        runtime,
        frame_local(frame, index).map_err(BindingAccessError::Fault)?,
        checked,
        frame,
    )
}

pub(super) fn binding_is_uninitialized(
    runtime: &Runtime,
    binding: &FrameBinding,
) -> Result<bool, EngineFault> {
    Ok(match binding {
        FrameBinding::Direct(value) => matches!(value, SlotValue::Uninitialized),
        FrameBinding::Captured(cell) => {
            let resolved = BindingCell::resolve_forward(runtime, *cell)?;
            matches!(
                runtime
                    .cells
                    .get(resolved)
                    .ok_or(EngineFault::StaleHeapEdge {
                        edge: "binding cell",
                        index: resolved.index(),
                        generation: resolved.generation(),
                    })?
                    .value,
                SlotValue::Uninitialized
            )
        }
    })
}

pub(super) fn local_is_uninitialized(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<bool, EngineFault> {
    if let Some(cell) = local_eval_shadow_cell(runtime, frame, index)? {
        return binding_is_uninitialized(runtime, &FrameBinding::Captured(cell));
    }
    binding_is_uninitialized(runtime, frame_local(frame, index)?)
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
    if let Some(cell) = local_eval_shadow_cell(runtime, frame, index)? {
        let mut binding = FrameBinding::Captured(cell);
        return write_binding(runtime, &mut binding, value);
    }
    write_binding(runtime, frame_local_mut(frame, index)?, value)
}

fn local_eval_shadow_cell(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<Option<BindingCellId>, EngineFault> {
    let Some(boundary) = frame.parameter_eval_boundary.as_ref() else {
        return Ok(None);
    };
    if frame.body_eval_environment.is_some() {
        return Ok(None);
    }
    if boundary.borrow().kind != EvalVariableEnvironmentKind::ParameterBoundary {
        return Err(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        });
    }
    let function = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })?;
    let argument_count = function
        .function()
        .control_flow()
        .domains()
        .argument_count();
    let definition = function
        .metadata()
        .variables()
        .get(argument_count.saturating_add(index) as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "local metadata",
            index,
        })?;
    if !eval_can_shadow_binding(definition.policy().kind()) {
        return Ok(None);
    }
    let name = installed_binding_name(runtime, frame, definition.name())?;
    lookup_eval_variable_range(
        frame.eval_environment.as_ref().map(Rc::clone),
        Some(boundary),
        &name,
        frame.template,
    )
}

const fn eval_can_shadow_binding(kind: CompilerBindingKind) -> bool {
    matches!(
        kind,
        CompilerBindingKind::Parameter
            | CompilerBindingKind::Var
            | CompilerBindingKind::Let
            | CompilerBindingKind::Const
            | CompilerBindingKind::ClassName
            | CompilerBindingKind::Function
            | CompilerBindingKind::FunctionName
            | CompilerBindingKind::Catch
            | CompilerBindingKind::GlobalReference
    )
}

fn installed_binding_name(
    runtime: &Runtime,
    frame: &Frame,
    name: Option<quickjs_bytecode::AtomPoolIndex>,
) -> Result<JsString, EngineFault> {
    let name = name.ok_or(EngineFault::InvalidClosureEnvironment {
        function: frame.template,
    })?;
    installed_template(runtime, frame.code, frame.template)?
        .atoms
        .get(name.get() as usize)
        .ok_or(EngineFault::MissingPoolEntry {
            pool: "binding metadata atom",
            index: name.get(),
        })?
        .description()
        .cloned()
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })
}

fn lookup_eval_variable_range(
    mut current: Option<SharedEvalVariableEnvironment>,
    boundary: Option<&SharedEvalVariableEnvironment>,
    name: &JsString,
    function: FunctionTemplateId,
) -> Result<Option<BindingCellId>, EngineFault> {
    while let Some(environment) = current {
        if boundary.is_some_and(|boundary| Rc::ptr_eq(&environment, boundary)) {
            return Ok(None);
        }
        let record = environment.borrow();
        if let Some(cell) = record
            .bindings
            .iter()
            .find(|binding| !binding.deleted && binding.name.code_units().eq(name.code_units()))
            .map(|binding| binding.cell)
        {
            return Ok(Some(cell));
        }
        current = record.parent.as_ref().map(Rc::clone);
    }
    if boundary.is_some() {
        Err(EngineFault::InvalidClosureEnvironment { function })
    } else {
        Ok(None)
    }
}

fn closure_eval_shadow_cell(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<Option<BindingCellId>, EngineFault> {
    let function = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })?;
    let definition = function.metadata().closures().get(index as usize).ok_or(
        EngineFault::MissingPoolEntry {
            pool: "closure metadata",
            index,
        },
    )?;
    if !eval_can_shadow_binding(definition.policy().kind()) {
        return Ok(None);
    }
    let name = installed_binding_name(runtime, frame, definition.name())?;
    if let Some(cell) = lookup_eval_variable_range(
        frame.eval_environment.as_ref().map(Rc::clone),
        frame.inherited_eval_environment.as_ref(),
        &name,
        frame.template,
    )? {
        return Ok(Some(cell));
    }
    let shadow = frame.environment_eval_shadows.get(index as usize).ok_or(
        EngineFault::MissingPoolEntry {
            pool: "closure eval shadow",
            index,
        },
    )?;
    let Some(shadow) = shadow else {
        return Ok(None);
    };
    lookup_eval_variable_range(
        Some(Rc::clone(&shadow.head)),
        shadow.boundary.as_ref(),
        &name,
        frame.template,
    )
}

pub(super) fn resolve_environment_cell(
    runtime: &Runtime,
    frame: &Frame,
    index: u32,
) -> Result<Option<BindingCellId>, EngineFault> {
    if let Some(cell) = closure_eval_shadow_cell(runtime, frame, index)? {
        return Ok(Some(cell));
    }
    let function = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .ok_or(EngineFault::InvalidClosureEnvironment {
            function: frame.template,
        })?;
    let definition = function.metadata().closures().get(index as usize).ok_or(
        EngineFault::MissingPoolEntry {
            pool: "closure metadata",
            index,
        },
    )?;
    if definition.is_deletable_eval_variable() {
        return Ok(None);
    }
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
    Ok(Some(cell))
}

pub(super) fn write_binding_cell(
    runtime: &mut Runtime,
    cell: BindingCellId,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    let resolved = BindingCell::resolve_forward(runtime, cell)?;
    runtime
        .cells
        .get_mut(resolved)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: resolved.index(),
            generation: resolved.generation(),
        })?
        .value = value;
    runtime.collection_pending = true;
    Ok(())
}

fn write_binding(
    runtime: &mut Runtime,
    binding: &mut FrameBinding,
    value: SlotValue,
) -> Result<(), ExecutionError> {
    match binding {
        FrameBinding::Direct(current) => *current = value,
        FrameBinding::Captured(cell) => {
            write_binding_cell(runtime, *cell, value)?;
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
    if let Some(cell) =
        closure_eval_shadow_cell(runtime, frame, index).map_err(BindingAccessError::Fault)?
    {
        return duplicate_binding(runtime, &FrameBinding::Captured(cell), checked, frame);
    }
    let function = code(runtime, frame.code)
        .map_err(BindingAccessError::Fault)?
        .authority
        .function(frame.template)
        .ok_or(BindingAccessError::Fault(
            EngineFault::InvalidClosureEnvironment {
                function: frame.template,
            },
        ))?;
    let definition =
        function
            .metadata()
            .closures()
            .get(index as usize)
            .ok_or(BindingAccessError::Fault(EngineFault::MissingPoolEntry {
                pool: "closure metadata",
                index,
            }))?;
    if definition.is_deletable_eval_variable() {
        return Err(BindingAccessError::Missing);
    }
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
    let resolved = BindingCell::resolve_forward(runtime, cell).map_err(BindingAccessError::Fault)?;
    let value = &runtime
        .cells
        .get(resolved)
        .ok_or_else(|| {
            BindingAccessError::Fault(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: resolved.index(),
                generation: resolved.generation(),
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
    if let Some(cell) = closure_eval_shadow_cell(runtime, frame, index)? {
        return binding_is_uninitialized(runtime, &FrameBinding::Captured(cell));
    }
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
    let resolved = BindingCell::resolve_forward(runtime, cell)?;
    Ok(matches!(
        runtime
            .cells
            .get(resolved)
            .ok_or(EngineFault::StaleHeapEdge {
                edge: "binding cell",
                index: resolved.index(),
                generation: resolved.generation(),
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
    if let Some(cell) = closure_eval_shadow_cell(runtime, frame, index)? {
        let mut binding = FrameBinding::Captured(cell);
        return write_binding(runtime, &mut binding, value);
    }
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
    let resolved = BindingCell::resolve_forward(runtime, cell)?;
    runtime
        .cells
        .get_mut(resolved)
        .ok_or(EngineFault::StaleHeapEdge {
            edge: "binding cell",
            index: resolved.index(),
            generation: resolved.generation(),
        })?
        .value = value;
    runtime.collection_pending = true;
    Ok(())
}
