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

//! Failure-atomic verified bytecode staging, environments, and root publication.

use std::sync::Arc;

use super::{
    BindingCell, BindingCellId, BytecodeFunction, CompilerCaptureLayout, CompilerCapturedBinding,
    CompilerClosureBinding, CompilerClosureSource, CompilerConstant, CompilerConstantValue,
    EnvironmentBinding, FrameBindingAddress, FunctionId, FunctionImplementation, FunctionTemplateId,
    GlobalDeclarationRejectionKind, HeapFunction, HashSet, HeapReference, InstallError,
    InstalledCode, InstalledCodeId, InstalledConstant, InstalledRoot, InstalledTemplate,
    InstalledTemplateElement, InstalledTemplateObject, JsBigInt, JsNumber, JsValue, OwnProperty,
    PropertyKey, RealmGlobalBinding, RealmGlobalBindingState, RealmGlobalRequest, RealmId,
    RootEnvironment, RootTarget, Runtime, RuntimeError, RuntimeResource, SlotValue, StoredValue,
    VerifiedBytecode, check_execution_limit, check_install_limit, preflight_opcodes,
    global_declaration_property_layout, global_function_replacement_layout,
    rejected_global_declaration, runtime_string, stale_heap_reference, usize_to_u64,
};

fn stage_constant(constant: &CompilerConstant) -> Result<InstalledConstant, InstallError> {
    Ok(match constant {
        CompilerConstant::Value(CompilerConstantValue::Number(value)) => {
            InstalledConstant::Number(JsNumber::from_f64(value.to_f64()))
        }
        CompilerConstant::Value(CompilerConstantValue::String(value)) => {
            InstalledConstant::String(runtime_string(value)?)
        }
        CompilerConstant::Value(CompilerConstantValue::BigInt(value)) => {
            let bytes = value
                .decimal()
                .latin1_units()
                .ok_or(InstallError::AuthorityInvariant {
                    message: "verified BigInt decimal is not compact ASCII",
                })?;
            let decimal =
                std::str::from_utf8(bytes).map_err(|_| InstallError::AuthorityInvariant {
                    message: "verified BigInt decimal is not ASCII",
                })?;
            InstalledConstant::BigInt(Arc::new(JsBigInt::from_str_radix(decimal, 10)?))
        }
        CompilerConstant::Value(CompilerConstantValue::TemplateObject(value)) => {
            let mut elements = Vec::new();
            elements
                .try_reserve_exact(value.elements().len())
                .map_err(|_| InstallError::AllocationFailed {
                    resource: RuntimeResource::InstalledConstants,
                    additional: value.elements().len(),
                })?;
            for element in value.elements() {
                elements.push(InstalledTemplateElement {
                    cooked: element.cooked().map(runtime_string).transpose()?,
                    raw: runtime_string(element.raw())?,
                });
            }
            InstalledConstant::TemplateObject(InstalledTemplateObject {
                elements: elements.into(),
                object: None,
            })
        }
        CompilerConstant::Function(function) => InstalledConstant::Function(*function),
    })
}

impl Runtime {
    /// Installs module code and creates the module root function with a
    /// pre-built module environment. Returns (code_id, function_id).
    pub(crate) fn install_module_root(
        &mut self,
        realm: RealmId,
        authority: Arc<VerifiedBytecode>,
        module_environment: &[BindingCellId],
    ) -> Result<(InstalledCodeId, FunctionId), InstallError> {
        preflight_opcodes(&authority)?;
        let graph_usage = authority.compiler_graph().usage();
        let functions = graph_usage.functions();
        let atoms = graph_usage.atoms();
        let constants = graph_usage.constants();
        check_install_limit(
            RuntimeResource::InstalledCode,
            self.limits.max_installed_code,
            usize_to_u64(self.code.len()).saturating_add(1),
        )?;
        check_install_limit(
            RuntimeResource::InstalledTemplates,
            self.limits.max_installed_templates,
            self.installed_templates.saturating_add(functions),
        )?;
        check_install_limit(
            RuntimeResource::InstalledAtoms,
            self.limits.max_installed_atoms,
            self.installed_atoms.saturating_add(atoms),
        )?;
        check_install_limit(
            RuntimeResource::InstalledConstants,
            self.limits.max_installed_constants,
            self.installed_constants.saturating_add(constants),
        )?;
        check_install_limit(
            RuntimeResource::HeapFunctions,
            self.limits.max_heap_functions,
            usize_to_u64(self.functions.len()).saturating_add(1),
        )?;
        self.code
            .try_reserve(1)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledCode,
                additional: 1,
            })?;
        self.functions
            .try_reserve(1)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        let templates = self.stage_templates(&authority)?;
        let root_template = authority.root_id();
        let root_environment = self.materialize_root_environment(
            realm,
            &authority,
            &templates,
            None,
            Some(module_environment),
        )?;
        let environment = root_environment.bindings.clone();
        let eval_shadows = vec![None; environment.len()];
        let code = self
            .code
            .try_insert(InstalledCode {
                authority,
                realm,
                templates,
                live_functions: 1,
            })
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledCode,
                additional: 1,
            })?;
        let function_prototype = HeapReference::Function(
            self.realm_function_prototype(realm).map_err(|_| InstallError::AuthorityInvariant {
                message: "constructor realm has no Function.prototype intrinsic",
            })?,
        );
        let function_record = crate::object::ObjectRecord::empty(Some(function_prototype));
        let function = self
            .insert_heap_function(HeapFunction {
                implementation: FunctionImplementation::Bytecode(BytecodeFunction {
                    code,
                    template: root_template,
                    environment,
                    environment_eval_shadows: eval_shadows,
                    eval_environment: None,
                    lexical_receiver: None,
                    lexical_eval_in_function: false,
                    lexical_eval_in_class_field_initializer: false,
                    lexical_new_target: None,
                    lexical_derived_constructor: None,
                    lexical_derived_this: None,
                    has_instance_elements: false,
                    home_object: None,
                }),
                object: function_record,
                public_roots: 0,
            })
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        self.installed_templates += functions;
        self.installed_atoms += atoms;
        self.installed_constants += constants;
        Ok((code, function))
    }

    /// Creates a hoisted module-level function from its template, capturing the
    /// module environment cells. Used during InitializeEnvironment.
    ///
    /// `parent_environment` is the installed module root function's closure
    /// environment: a descendant forwards realm-global and module-binding slots
    /// through `ParentClosure`, exactly like the ordinary closure-creation path
    /// resolves them against the parent frame.
    pub(crate) fn create_module_closure(
        &mut self,
        realm: RealmId,
        code: InstalledCodeId,
        authority: &VerifiedBytecode,
        child: FunctionTemplateId,
        module_environment: &[BindingCellId],
        parent_environment: &[EnvironmentBinding],
    ) -> Result<FunctionId, InstallError> {
        let installed = self.code.get(code).ok_or(InstallError::AuthorityInvariant {
            message: "module installed code is stale",
        })?;
        let installed_index = usize::try_from(child.get()).map_err(|_| {
            InstallError::AuthorityInvariant {
                message: "function template index is not representable",
            }
        })?;
        let template = installed.templates.get(installed_index).ok_or(
            InstallError::AuthorityInvariant {
                message: "module function template is missing",
            },
        )?;
        let child_function = authority.function(child).ok_or(InstallError::AuthorityInvariant {
            message: "module function template not found in authority",
        })?;
        let sources = child_function.function().closure_sources();
        let mut environment = Vec::new();
        environment
            .try_reserve_exact(sources.len())
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: sources.len(),
            })?;
        for source in sources {
            match *source {
                CompilerClosureSource::Module { index } => {
                    let cell = module_environment.get(index as usize).copied().ok_or(
                        InstallError::AuthorityInvariant {
                            message: "module closure source index out of range",
                        },
                    )?;
                    environment.push(EnvironmentBinding::Captured(cell));
                }
                CompilerClosureSource::ConstructorRealmGlobal(atom) => {
                    let definition = child_function
                        .metadata()
                        .closures()
                        .get(environment.len())
                        .ok_or(InstallError::AuthorityInvariant {
                            message: "module realm-global source has no closure metadata",
                        })?;
                    let CompilerClosureBinding::RealmGlobal(policy) = definition.binding() else {
                        return Err(InstallError::AuthorityInvariant {
                            message: "module realm-global source has captured-cell metadata",
                        });
                    };
                    if !matches!(
                        RealmGlobalRequest::from_policy(policy)?,
                        RealmGlobalRequest::Lookup
                    ) {
                        return Err(InstallError::AuthorityInvariant {
                            message: "module-level function declares a constructor-realm global",
                        });
                    }
                    let name = template.atoms.get(atom.get() as usize).cloned().ok_or(
                        InstallError::AuthorityInvariant {
                            message: "constructor-realm global atom is missing",
                        },
                    )?;
                    let existing = self
                        .realms
                        .get(realm)
                        .and_then(|state| state.global_bindings.get(&name).copied());
                    let global = if let Some(global) = existing {
                        let valid = self.global_bindings.get(global).is_some_and(|binding| {
                            binding.realm == realm && binding.name.is_same_identity(&name)
                        });
                        if !valid {
                            return Err(InstallError::AuthorityInvariant {
                                message: "constructor-realm global binding has the wrong owner",
                            });
                        }
                        global
                    } else {
                        check_install_limit(
                            RuntimeResource::RealmGlobalBindings,
                            self.limits.max_realm_global_bindings,
                            usize_to_u64(self.global_bindings.len()).saturating_add(1),
                        )?;
                        let global = self
                            .global_bindings
                            .try_insert(RealmGlobalBinding {
                                realm,
                                name: name.clone(),
                                state: RealmGlobalBindingState::Unresolved,
                            })
                            .map_err(|_| InstallError::AllocationFailed {
                                resource: RuntimeResource::RealmGlobalBindings,
                                additional: 1,
                            })?;
                        let prior = self
                            .realms
                            .get_mut(realm)
                            .ok_or(InstallError::AuthorityInvariant {
                                message: "constructor realm disappeared during installation",
                            })?
                            .global_bindings
                            .insert(name, global);
                        if prior.is_some() {
                            return Err(InstallError::AuthorityInvariant {
                                message:
                                    "constructor-realm global insertion replaced an existing binding",
                            });
                        }
                        global
                    };
                    environment.push(EnvironmentBinding::RealmGlobal(global));
                }
                CompilerClosureSource::ParentClosure(slot) => {
                    let binding = parent_environment.get(slot as usize).copied().ok_or(
                        InstallError::AuthorityInvariant {
                            message: "module closure parent slot out of range",
                        },
                    )?;
                    match binding {
                        EnvironmentBinding::Captured(cell) => {
                            if !self.cells.contains(cell) {
                                return Err(InstallError::AuthorityInvariant {
                                    message: "module closure parent cell is stale",
                                });
                            }
                        }
                        EnvironmentBinding::RealmGlobal(global) => {
                            let valid = self
                                .global_bindings
                                .get(global)
                                .is_some_and(|binding| binding.realm == realm);
                            if !valid {
                                return Err(InstallError::AuthorityInvariant {
                                    message: "module closure parent realm-global binding is stale",
                                });
                            }
                        }
                    }
                    environment.push(binding);
                }
                _ => {
                    return Err(InstallError::AuthorityInvariant {
                        message: "module-level function has unsupported closure source",
                    });
                }
            }
        }
        let eval_shadows = vec![None; environment.len()];
        let function_prototype = HeapReference::Function(
            self.realm_function_prototype(realm).map_err(|_| {
                InstallError::AuthorityInvariant {
                    message: "constructor realm has no Function.prototype intrinsic",
                }
            })?,
        );
        let function_record = crate::object::ObjectRecord::empty(Some(function_prototype));
        check_install_limit(
            RuntimeResource::HeapFunctions,
            self.limits.max_heap_functions,
            usize_to_u64(self.functions.len()).saturating_add(1),
        )?;
        self.functions
            .try_reserve(1)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        let function = self
            .insert_heap_function(HeapFunction {
                implementation: FunctionImplementation::Bytecode(BytecodeFunction {
                    code,
                    template: child,
                    environment,
                    environment_eval_shadows: eval_shadows,
                    eval_environment: None,
                    lexical_receiver: None,
                    lexical_eval_in_function: false,
                    lexical_eval_in_class_field_initializer: false,
                    lexical_new_target: None,
                    lexical_derived_constructor: None,
                    lexical_derived_this: None,
                    has_instance_elements: false,
                    home_object: None,
                }),
                object: function_record,
                public_roots: 0,
            })
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        if let Some(installed) = self.code.get_mut(code) {
            installed.live_functions += 1;
        }
        Ok(function)
    }

    pub(crate) fn prepare_execution_safe_point(&mut self) -> Result<(), crate::ExecutionError> {
        self.collect_if_pending().map_err(|error| match error {
            RuntimeError::LimitExceeded {
                resource,
                limit,
                observed,
            } => crate::ExecutionError::LimitExceeded {
                resource,
                limit,
                observed,
            },
            RuntimeError::AllocationFailed {
                resource,
                additional,
            } => crate::ExecutionError::AllocationFailed {
                resource,
                additional,
            },
            RuntimeError::Atom(_) => crate::EngineFault::RuntimeInvariant {
                message: "cycle collection returned an atom-table construction error",
            }
            .into(),
        })
    }

    pub(super) fn prepare_installation_safe_point(&mut self) -> Result<(), InstallError> {
        self.collect_if_pending().map_err(|error| match error {
            RuntimeError::LimitExceeded {
                resource,
                limit,
                observed,
            } => InstallError::LimitExceeded {
                resource,
                limit,
                observed,
            },
            RuntimeError::AllocationFailed {
                resource,
                additional,
            } => InstallError::AllocationFailed {
                resource,
                additional,
            },
            RuntimeError::Atom(source) => InstallError::Atom(source),
        })
    }

    fn collect_if_pending(&mut self) -> Result<(), RuntimeError> {
        self.drain_releases();
        if self.collection_pending {
            self.collect_cycles()?;
        }
        Ok(())
    }

    pub(crate) fn public_value(
        &mut self,
        value: StoredValue,
    ) -> Result<JsValue, crate::ExecutionError> {
        let reference = match value.into_root_target() {
            RootTarget::Primitive(value) => return Ok(JsValue::primitive(&self.mailbox, value)),
            RootTarget::Heap(reference) => reference,
        };
        check_execution_limit(
            RuntimeResource::PublicRoots,
            self.limits.max_public_roots,
            self.public_roots.saturating_add(1),
        )?;
        self.mailbox
            .try_reserve_root()
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ReleaseMailbox,
                additional: 1,
            })?;
        let public_roots = match reference {
            HeapReference::Function(function) => self
                .functions
                .get_mut(function)
                .map(|node| &mut node.public_roots),
            HeapReference::Object(object) => self
                .objects
                .get_mut(object)
                .map(|node| &mut node.public_roots),
        };
        let Some(public_roots) = public_roots else {
            self.mailbox.cancel_reserved_root();
            return Err(stale_heap_reference(reference).into());
        };
        let Some(next_roots) = public_roots.checked_add(1) else {
            self.mailbox.cancel_reserved_root();
            return Err(crate::ExecutionError::LimitExceeded {
                resource: RuntimeResource::PublicRoots,
                limit: u64::from(u32::MAX),
                observed: u64::from(u32::MAX) + 1,
            });
        };
        *public_roots = next_roots;
        self.public_roots += 1;
        Ok(JsValue::rooted_heap(&self.mailbox, reference))
    }

    pub(crate) fn public_value_pair(
        &mut self,
        first: StoredValue,
        second: StoredValue,
    ) -> Result<(JsValue, JsValue), crate::ExecutionError> {
        let first = self.public_value(first)?;
        match self.public_value(second) {
            Ok(second) => Ok((first, second)),
            Err(error) => {
                drop(first);
                self.drain_releases();
                Err(error)
            }
        }
    }

    pub(crate) fn retire_internal_root(
        &mut self,
        root: FunctionId,
        expected_code: InstalledCodeId,
    ) -> Result<(), crate::EngineFault> {
        let function = self
            .functions
            .get(root)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "internal Script root",
                index: root.index(),
                generation: root.generation(),
            })?;
        let bytecode = function.bytecode()?;
        if bytecode.code != expected_code {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "internal Script root changed installed-code ownership",
            });
        }
        if function.public_roots != 0 {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "internal Script root became publicly rooted",
            });
        }
        let code = self
            .code
            .get(expected_code)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "installed code",
                index: expected_code.index(),
                generation: expected_code.generation(),
            })?;
        if code.live_functions == 0 {
            return Err(crate::EngineFault::RuntimeInvariant {
                message: "internal Script root has no installed-code live-function charge",
            });
        }

        let function = self
            .functions
            .remove(root)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "internal Script root",
                index: root.index(),
                generation: root.generation(),
            })?;
        self.object_properties = self
            .object_properties
            .saturating_sub(usize_to_u64(function.object.property_count()));
        let remove_code = {
            let code =
                self.code
                    .get_mut(expected_code)
                    .ok_or(crate::EngineFault::StaleHeapEdge {
                        edge: "installed code",
                        index: expected_code.index(),
                        generation: expected_code.generation(),
                    })?;
            code.live_functions -= 1;
            code.live_functions == 0
        };
        if remove_code {
            let removed = self.remove_installed_code(expected_code);
            debug_assert!(removed);
            self.atoms.collect_dead();
        }
        self.collection_pending = true;
        Ok(())
    }

    pub(crate) fn retire_dynamic_root(
        &mut self,
        mut root: InstalledRoot,
    ) -> Result<(), crate::EngineFault> {
        if let Some(pending) = root.pending_environment.take() {
            self.rollback_root_environment(pending.realm, &pending.environment);
        }
        self.retire_internal_root(root.function, root.code)
    }

    fn remove_installed_code(&mut self, id: InstalledCodeId) -> bool {
        let Some(code) = self.code.remove(id) else {
            return false;
        };
        self.installed_templates = self
            .installed_templates
            .saturating_sub(usize_to_u64(code.templates.len()));
        let atoms = code.templates.iter().fold(0_u64, |total, template| {
            total.saturating_add(usize_to_u64(template.atoms.len()))
        });
        let constants = code.templates.iter().fold(0_u64, |total, template| {
            total.saturating_add(usize_to_u64(template.constants.len()))
        });
        self.installed_atoms = self.installed_atoms.saturating_sub(atoms);
        self.installed_constants = self.installed_constants.saturating_sub(constants);
        true
    }

    pub(crate) fn stage_templates(
        &mut self,
        authority: &VerifiedBytecode,
    ) -> Result<Vec<InstalledTemplate>, InstallError> {
        let function_count = authority.functions().len();
        let mut templates = Vec::new();
        templates.try_reserve_exact(function_count).map_err(|_| {
            InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledTemplates,
                additional: function_count,
            }
        })?;

        for function in authority.functions() {
            let mut atoms = Vec::new();
            atoms
                .try_reserve_exact(function.function().atoms().len())
                .map_err(|_| InstallError::AllocationFailed {
                    resource: RuntimeResource::InstalledAtoms,
                    additional: function.function().atoms().len(),
                })?;
            for atom in function.function().atoms() {
                let string = runtime_string(atom.string())?;
                atoms.push(self.atoms.intern_string(&string)?);
            }

            let mut constants = Vec::new();
            constants
                .try_reserve_exact(function.function().constants().len())
                .map_err(|_| InstallError::AllocationFailed {
                    resource: RuntimeResource::InstalledConstants,
                    additional: function.function().constants().len(),
                })?;
            for constant in function.function().constants() {
                constants.push(stage_constant(constant)?);
            }

            let capture_layout = function.function().control_flow().compiler_capture_layout();
            let capture_count = function
                .function()
                .control_flow()
                .function_header()
                .variable_reference_count();
            let bindings =
                if capture_count == 0 {
                    Vec::new()
                } else {
                    let layout = capture_layout.ok_or(InstallError::AuthorityInvariant {
                        message: "captured bindings have no compiler capture layout",
                    })?;
                    let mut bindings = Vec::new();
                    bindings
                        .try_reserve_exact(layout.bindings().len())
                        .map_err(|_| InstallError::AllocationFailed {
                            resource: RuntimeResource::InstalledTemplates,
                            additional: layout.bindings().len(),
                        })?;
                    bindings.extend(layout.bindings().iter().copied().map(
                        |binding| match binding {
                            CompilerCapturedBinding::Argument(index) => {
                                FrameBindingAddress::Argument(index)
                            }
                            CompilerCapturedBinding::FunctionLocal(index)
                            | CompilerCapturedBinding::ScopedLocal(index) => {
                                FrameBindingAddress::Local(index)
                            }
                        },
                    ));
                    bindings
                };
            if bindings.len() != capture_count as usize {
                return Err(InstallError::AuthorityInvariant {
                    message: "capture layout length differs from the verified header",
                });
            }
            let mapped_arguments =
                capture_layout.and_then(CompilerCaptureLayout::mapped_arguments_arc);

            templates.push(InstalledTemplate {
                atoms,
                constants,
                own_cell_bindings: bindings,
                mapped_arguments,
            });
        }
        Ok(templates)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "realm-global preflight, reservation, commit, and rollback journaling remain one auditable transaction"
    )]
    pub(super) fn materialize_root_environment(
        &mut self,
        realm: RealmId,
        authority: &VerifiedBytecode,
        templates: &[InstalledTemplate],
        external_environment: Option<&[Option<EnvironmentBinding>]>,
        module_environment: Option<&[BindingCellId]>,
    ) -> Result<RootEnvironment, InstallError> {
        let root = authority.root();
        let executable_kind = root.metadata().executable_kind();
        let declaration_layout = global_declaration_property_layout(executable_kind);
        let sources = root.function().closure_sources();
        if sources.len() != root.metadata().closures().len() {
            return Err(InstallError::AuthorityInvariant {
                message: "root closure source and metadata lengths differ",
            });
        }
        let root_index = usize::try_from(authority.root_id().get()).map_err(|_| {
            InstallError::AuthorityInvariant {
                message: "root template index is not representable",
            }
        })?;
        let installed = templates
            .get(root_index)
            .ok_or(InstallError::AuthorityInvariant {
                message: "installed root template is missing",
            })?;
        let mut requests = Vec::new();
        requests
            .try_reserve_exact(sources.len())
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: sources.len(),
            })?;
        let mut binding_slots = Vec::new();
        binding_slots
            .try_reserve_exact(sources.len())
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: sources.len(),
            })?;
        binding_slots.resize(sources.len(), None);
        let mut requested_names = HashSet::new();
        requested_names
            .try_reserve(sources.len())
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: sources.len(),
            })?;
        for (closure, (source, definition)) in
            sources.iter().zip(root.metadata().closures()).enumerate()
        {
            match *source {
                quickjs_bytecode::CompilerClosureSource::ConstructorRealmGlobal(atom) => {
                    let CompilerClosureBinding::RealmGlobal(policy) = definition.binding() else {
                        return Err(InstallError::AuthorityInvariant {
                            message: "root constructor-realm source has captured-cell metadata",
                        });
                    };
                    let name = installed.atoms.get(atom.get() as usize).cloned().ok_or(
                        InstallError::AuthorityInvariant {
                            message: "constructor-realm global atom is missing",
                        },
                    )?;
                    if !requested_names.insert(name.clone()) {
                        return Err(InstallError::AuthorityInvariant {
                            message: "constructor-realm global names are not unique",
                        });
                    }
                    requests.push((
                        name,
                        RealmGlobalRequest::from_policy(policy)?,
                        u32::try_from(closure).map_err(|_| InstallError::AuthorityInvariant {
                            message: "constructor-realm global index is not representable",
                        })?,
                    ));
                }
                quickjs_bytecode::CompilerClosureSource::DirectEvalBinding {
                    index,
                    environment_size,
                }
                | quickjs_bytecode::CompilerClosureSource::DirectEvalVariable {
                    index,
                    environment_size,
                } => {
                    if !matches!(definition.binding(), CompilerClosureBinding::Captured(_)) {
                        return Err(InstallError::AuthorityInvariant {
                            message: "direct-eval source has Realm-global metadata",
                        });
                    }
                    let environment =
                        external_environment.ok_or(InstallError::AuthorityInvariant {
                            message: "direct-eval caller environment is missing",
                        })?;
                    if environment.len() != environment_size as usize {
                        return Err(InstallError::AuthorityInvariant {
                            message: "direct-eval caller environment has the wrong shape",
                        });
                    }
                    let binding = environment.get(index as usize).copied().flatten().ok_or(
                        InstallError::AuthorityInvariant {
                            message: "direct-eval caller binding is missing",
                        },
                    )?;
                    let EnvironmentBinding::Captured(cell) = binding else {
                        return Err(InstallError::AuthorityInvariant {
                            message: "direct-eval caller binding is not a captured cell",
                        });
                    };
                    if !self.cells.contains(cell) {
                        return Err(InstallError::AuthorityInvariant {
                            message: "direct-eval caller binding cell is stale",
                        });
                    }
                    binding_slots[closure] = Some(binding);
                }
                quickjs_bytecode::CompilerClosureSource::ParentVariableReference(_)
                | quickjs_bytecode::CompilerClosureSource::ParentClosure(_) => {
                    return Err(InstallError::AuthorityInvariant {
                        message: "root closure source requires an omitted parent",
                    });
                }
                quickjs_bytecode::CompilerClosureSource::Module { index } => {
                    if !matches!(definition.binding(), CompilerClosureBinding::Captured(_)) {
                        return Err(InstallError::AuthorityInvariant {
                            message: "module closure source has realm-global metadata",
                        });
                    }
                    let environment =
                        module_environment.ok_or(InstallError::AuthorityInvariant {
                            message: "module environment is missing",
                        })?;
                    let cell = environment.get(index as usize).copied().ok_or(
                        InstallError::AuthorityInvariant {
                            message: "module closure source index out of range",
                        },
                    )?;
                    if !self.cells.contains(cell) {
                        return Err(InstallError::AuthorityInvariant {
                            message: "module environment cell is stale",
                        });
                    }
                    binding_slots[closure] = Some(EnvironmentBinding::Captured(cell));
                }
            }
        }

        let realm_state = self
            .realms
            .get(realm)
            .ok_or(InstallError::AuthorityInvariant {
                message: "constructor realm disappeared during installation",
            })?;
        let global_object = realm_state.global_object;
        let missing = requests
            .iter()
            .filter(|(name, _, _)| !realm_state.global_bindings.contains_key(name))
            .count();
        let global_record =
            self.objects
                .get(global_object)
                .ok_or(InstallError::AuthorityInvariant {
                    message: "constructor-realm global object is stale",
                })?;
        let mut new_object_properties = 0_usize;
        for (name, request, closure) in &requests {
            let key = PropertyKey::from_validated_atom(name.clone());
            if let Some(global) = realm_state.global_bindings.get(name).copied() {
                let binding =
                    self.global_bindings
                        .get(global)
                        .ok_or(InstallError::AuthorityInvariant {
                            message: "constructor-realm global binding is stale",
                        })?;
                if binding.realm != realm || !binding.name.is_same_identity(name) {
                    return Err(InstallError::AuthorityInvariant {
                        message: "constructor-realm global binding has the wrong owner",
                    });
                }
            }
            match request {
                RealmGlobalRequest::Lookup => {}
                RealmGlobalRequest::Var | RealmGlobalRequest::Function => {
                    if let Some(global) = realm_state.global_bindings.get(name).copied()
                        && matches!(
                            self.global_bindings
                                .get(global)
                                .map(|binding| binding.state),
                            Some(RealmGlobalBindingState::Lexical { .. })
                        )
                    {
                        return Err(rejected_global_declaration(
                            authority,
                            *closure,
                            name,
                            GlobalDeclarationRejectionKind::BindingConflict,
                        )?);
                    }
                    if let Some(property) = global_record.record.own_property(&key) {
                        if matches!(request, RealmGlobalRequest::Function)
                            && global_function_replacement_layout(
                                property.layout(),
                                declaration_layout,
                            )
                            .is_none()
                        {
                            return Err(rejected_global_declaration(
                                authority,
                                *closure,
                                name,
                                GlobalDeclarationRejectionKind::ObjectDefinitionRejected,
                            )?);
                        }
                    } else {
                        if !global_record.record.is_extensible() {
                            return Err(rejected_global_declaration(
                                authority,
                                *closure,
                                name,
                                GlobalDeclarationRejectionKind::ObjectDefinitionRejected,
                            )?);
                        }
                        new_object_properties = new_object_properties.saturating_add(1);
                    }
                }
                RealmGlobalRequest::Let | RealmGlobalRequest::Const => {
                    if let Some(global) = realm_state.global_bindings.get(name).copied()
                        && !matches!(
                            self.global_bindings
                                .get(global)
                                .map(|binding| binding.state),
                            Some(RealmGlobalBindingState::Unresolved)
                        )
                    {
                        return Err(rejected_global_declaration(
                            authority,
                            *closure,
                            name,
                            GlobalDeclarationRejectionKind::BindingConflict,
                        )?);
                    }
                    if global_record
                        .record
                        .own_property(&key)
                        .is_some_and(|property| !property.layout().is_configurable())
                    {
                        return Err(rejected_global_declaration(
                            authority,
                            *closure,
                            name,
                            GlobalDeclarationRejectionKind::BindingConflict,
                        )?);
                    }
                }
            }
        }
        let lexical_cells = requests
            .iter()
            .filter(|(_, request, _)| request.lexical_mutability().is_some())
            .count();
        check_install_limit(
            RuntimeResource::RealmGlobalBindings,
            self.limits.max_realm_global_bindings,
            usize_to_u64(self.global_bindings.len()).saturating_add(usize_to_u64(missing)),
        )?;
        check_install_limit(
            RuntimeResource::ObjectProperties,
            self.limits.max_object_properties,
            self.object_properties
                .saturating_add(usize_to_u64(new_object_properties)),
        )?;
        check_install_limit(
            RuntimeResource::BindingCells,
            self.limits.max_binding_cells,
            usize_to_u64(self.cells.len()).saturating_add(usize_to_u64(lexical_cells)),
        )?;

        self.global_bindings
            .try_reserve(missing)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: missing,
            })?;
        self.cells
            .try_reserve(lexical_cells)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: lexical_cells,
            })?;
        self.realms
            .get_mut(realm)
            .ok_or(InstallError::AuthorityInvariant {
                message: "constructor realm disappeared during installation",
            })?
            .global_bindings
            .try_reserve(missing)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: missing,
            })?;
        self.objects
            .get_mut(global_object)
            .ok_or(InstallError::AuthorityInvariant {
                message: "constructor-realm global object is stale",
            })?
            .record
            .try_reserve_data(new_object_properties)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: new_object_properties,
            })?;

        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(sources.len())
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: sources.len(),
            })?;
        let mut inserted_globals = Vec::new();
        inserted_globals.try_reserve_exact(missing).map_err(|_| {
            InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: missing,
            }
        })?;
        let mut updated_globals = Vec::new();
        updated_globals
            .try_reserve_exact(requests.len())
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: requests.len(),
            })?;
        let mut inserted_cells = Vec::new();
        inserted_cells
            .try_reserve_exact(lexical_cells)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::BindingCells,
                additional: lexical_cells,
            })?;
        let mut inserted_global_properties = Vec::new();
        inserted_global_properties
            .try_reserve_exact(new_object_properties)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: new_object_properties,
            })?;
        let mut updated_global_properties = Vec::new();
        updated_global_properties
            .try_reserve_exact(requests.len())
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: requests.len(),
            })?;

        for (name, request, closure) in requests {
            let lexical_state = if let Some(mutable) = request.lexical_mutability() {
                let Ok(cell) = self.cells.try_insert(BindingCell {
                    value: SlotValue::Uninitialized,
                    forward: None,
                }) else {
                    let partial = RootEnvironment {
                        bindings,
                        inserted_globals,
                        updated_globals,
                        inserted_cells,
                        inserted_global_properties,
                        updated_global_properties,
                    };
                    self.rollback_root_environment(realm, &partial);
                    return Err(InstallError::AllocationFailed {
                        resource: RuntimeResource::BindingCells,
                        additional: 1,
                    });
                };
                inserted_cells.push(cell);
                Some(RealmGlobalBindingState::Lexical { cell, mutable })
            } else {
                None
            };
            let existing = self
                .realms
                .get(realm)
                .and_then(|state| state.global_bindings.get(&name).copied());
            let global = if let Some(global) = existing {
                let valid = self.global_bindings.get(global).is_some_and(|binding| {
                    binding.realm == realm && binding.name.is_same_identity(&name)
                });
                if !valid {
                    let partial = RootEnvironment {
                        bindings,
                        inserted_globals,
                        updated_globals,
                        inserted_cells,
                        inserted_global_properties,
                        updated_global_properties,
                    };
                    self.rollback_root_environment(realm, &partial);
                    return Err(InstallError::AuthorityInvariant {
                        message: "constructor-realm global binding is stale",
                    });
                }
                let current = self
                    .global_bindings
                    .get(global)
                    .ok_or(InstallError::AuthorityInvariant {
                        message: "constructor-realm global binding is stale",
                    })?
                    .state;
                let Some(upgraded) =
                    lexical_state.or_else(|| request.upgraded_object_state(current))
                else {
                    let partial = RootEnvironment {
                        bindings,
                        inserted_globals,
                        updated_globals,
                        inserted_cells,
                        inserted_global_properties,
                        updated_global_properties,
                    };
                    self.rollback_root_environment(realm, &partial);
                    return Err(InstallError::AuthorityInvariant {
                        message: "preflighted realm-global state upgrade became incompatible",
                    });
                };
                if upgraded != current {
                    updated_globals.push((global, current));
                    self.global_bindings
                        .get_mut(global)
                        .ok_or(InstallError::AuthorityInvariant {
                            message: "constructor-realm global binding is stale",
                        })?
                        .state = upgraded;
                }
                global
            } else {
                let Some(initial_state) =
                    lexical_state.or_else(|| request.initial_nonlexical_state())
                else {
                    let partial = RootEnvironment {
                        bindings,
                        inserted_globals,
                        updated_globals,
                        inserted_cells,
                        inserted_global_properties,
                        updated_global_properties,
                    };
                    self.rollback_root_environment(realm, &partial);
                    return Err(InstallError::AuthorityInvariant {
                        message: "realm-global request has no initial state",
                    });
                };
                let Ok(global) = self.global_bindings.try_insert(RealmGlobalBinding {
                    realm,
                    name: name.clone(),
                    state: initial_state,
                }) else {
                    let partial = RootEnvironment {
                        bindings,
                        inserted_globals,
                        updated_globals,
                        inserted_cells,
                        inserted_global_properties,
                        updated_global_properties,
                    };
                    self.rollback_root_environment(realm, &partial);
                    return Err(InstallError::AllocationFailed {
                        resource: RuntimeResource::RealmGlobalBindings,
                        additional: 1,
                    });
                };
                let prior = self
                    .realms
                    .get_mut(realm)
                    .ok_or(InstallError::AuthorityInvariant {
                        message: "constructor realm disappeared during installation",
                    })?
                    .global_bindings
                    .insert(name.clone(), global);
                if prior.is_some() {
                    let removed = self.global_bindings.remove(global);
                    debug_assert!(removed.is_some());
                    let partial = RootEnvironment {
                        bindings,
                        inserted_globals,
                        updated_globals,
                        inserted_cells,
                        inserted_global_properties,
                        updated_global_properties,
                    };
                    self.rollback_root_environment(realm, &partial);
                    return Err(InstallError::AuthorityInvariant {
                        message: "constructor-realm global insertion replaced an existing binding",
                    });
                }
                inserted_globals.push((name.clone(), global));
                global
            };
            if request.declares_object_property() {
                let key = PropertyKey::from_validated_atom(name.clone());
                let existing_property = self
                    .objects
                    .get(global_object)
                    .and_then(|object| object.record.own_property(&key));
                if let Some(existing_property) = existing_property {
                    if matches!(request, RealmGlobalRequest::Function) {
                        let existing_layout = existing_property.layout();
                        let replacement = global_function_replacement_layout(
                            existing_layout,
                            declaration_layout,
                        )
                        .ok_or(InstallError::AuthorityInvariant {
                            message: "preflighted global function property became incompatible",
                        })?;
                        if replacement != existing_layout
                            || matches!(&existing_property, OwnProperty::Accessor { .. })
                        {
                            let replacement_value = match &existing_property {
                                OwnProperty::Data { value, .. } => value.duplicate(),
                                OwnProperty::Accessor { .. } => StoredValue::Undefined,
                            };
                            let replaced = self.objects.get_mut(global_object).and_then(|object| {
                                object.record.replace_existing_with_data(
                                    &key,
                                    replacement,
                                    replacement_value,
                                )
                            });
                            let Some(previous) = replaced else {
                                let partial = RootEnvironment {
                                    bindings,
                                    inserted_globals,
                                    updated_globals,
                                    inserted_cells,
                                    inserted_global_properties,
                                    updated_global_properties,
                                };
                                self.rollback_root_environment(realm, &partial);
                                return Err(InstallError::AuthorityInvariant {
                                    message: "preflighted global function property disappeared",
                                });
                            };
                            if matches!(&previous, OwnProperty::Accessor { .. }) {
                                self.collection_pending = true;
                            }
                            updated_global_properties.push((key.clone(), previous));
                        }
                    }
                } else {
                    if let Err(error) = self.append_data_property(
                        HeapReference::Object(global_object),
                        key.clone(),
                        declaration_layout,
                        StoredValue::Undefined,
                    ) {
                        let partial = RootEnvironment {
                            bindings,
                            inserted_globals,
                            updated_globals,
                            inserted_cells,
                            inserted_global_properties,
                            updated_global_properties,
                        };
                        self.rollback_root_environment(realm, &partial);
                        return Err(match error {
                            crate::ExecutionError::LimitExceeded {
                                resource,
                                limit,
                                observed,
                            } => InstallError::LimitExceeded {
                                resource,
                                limit,
                                observed,
                            },
                            crate::ExecutionError::AllocationFailed {
                                resource,
                                additional,
                            } => InstallError::AllocationFailed {
                                resource,
                                additional,
                            },
                            crate::ExecutionError::Atom(source) => InstallError::Atom(source),
                            crate::ExecutionError::String(_)
                            | crate::ExecutionError::Handle(_)
                            | crate::ExecutionError::DynamicFunctionCompilation(_)
                            | crate::ExecutionError::DynamicFunctionInstallation(_)
                            | crate::ExecutionError::Exception(_)
                            | crate::ExecutionError::Interrupted { .. }
                            | crate::ExecutionError::InstructionLimitExceeded { .. }
                            | crate::ExecutionError::EngineFault(_) => {
                                InstallError::AuthorityInvariant {
                                    message: "preflighted global property insertion failed",
                                }
                            }
                        });
                    }
                    inserted_global_properties.push(key);
                }
            }
            binding_slots[closure as usize] = Some(EnvironmentBinding::RealmGlobal(global));
        }

        for binding in binding_slots {
            let Some(binding) = binding else {
                let partial = RootEnvironment {
                    bindings,
                    inserted_globals,
                    updated_globals,
                    inserted_cells,
                    inserted_global_properties,
                    updated_global_properties,
                };
                self.rollback_root_environment(realm, &partial);
                return Err(InstallError::AuthorityInvariant {
                    message: "root environment has an unresolved closure slot",
                });
            };
            bindings.push(binding);
        }

        Ok(RootEnvironment {
            bindings,
            inserted_globals,
            updated_globals,
            inserted_cells,
            inserted_global_properties,
            updated_global_properties,
        })
    }

    pub(super) fn rollback_root_environment(
        &mut self,
        realm: RealmId,
        environment: &RootEnvironment,
    ) {
        if let Some(global_object) = self.realms.get(realm).map(|state| state.global_object) {
            for (key, property) in environment.updated_global_properties.iter().rev() {
                if let Some(object) = self.objects.get_mut(global_object) {
                    let restored = object
                        .record
                        .restore_existing_property(key, property.duplicate());
                    debug_assert!(restored.is_some());
                }
            }
            for key in environment.inserted_global_properties.iter().rev() {
                if let Some(object) = self.objects.get_mut(global_object) {
                    let removed = object.record.pop_last_data(key);
                    debug_assert!(removed.is_some());
                    self.object_properties = self.object_properties.saturating_sub(1);
                }
            }
        }
        for (global, state) in environment.updated_globals.iter().rev() {
            if let Some(binding) = self.global_bindings.get_mut(*global) {
                binding.state = *state;
            }
        }
        for (name, global) in environment.inserted_globals.iter().rev() {
            if let Some(state) = self.realms.get_mut(realm) {
                let removed = state.global_bindings.remove(name);
                debug_assert_eq!(removed, Some(*global));
            }
            let removed = self.global_bindings.remove(*global);
            debug_assert!(removed.is_some());
        }
        for cell in environment.inserted_cells.iter().rev() {
            let removed = self.cells.remove(*cell);
            debug_assert!(removed.is_some());
        }
    }
}
