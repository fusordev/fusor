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

use super::{
    CompilerCaptureLayout, CompilerCapturedBinding, CompilerClosureBinding, CompilerConstant,
    CompilerConstantValue, EnvironmentBinding, FrameBindingAddress, FunctionId, HashSet,
    HeapReference, InstallError, InstalledCodeId, InstalledConstant, InstalledRoot,
    InstalledTemplate, JsNumber, JsValue, OwnProperty, PropertyKey, RealmGlobalBinding,
    RealmGlobalRequest, RealmId, RootEnvironment, RootTarget, Runtime, RuntimeError,
    RuntimeResource, StoredValue, VerifiedBytecode, check_execution_limit, check_install_limit,
    dynamic_function_declaration_property_layout, global_function_replacement_layout,
    rejected_global_declaration, runtime_string, stale_heap_reference, usize_to_u64,
};

impl Runtime {
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

    pub(super) fn stage_templates(
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
                constants.push(match constant {
                    CompilerConstant::Value(CompilerConstantValue::Number(value)) => {
                        InstalledConstant::Number(JsNumber::from_f64(value.to_f64()))
                    }
                    CompilerConstant::Value(CompilerConstantValue::String(value)) => {
                        InstalledConstant::String(runtime_string(value)?)
                    }
                    CompilerConstant::Function(function) => InstalledConstant::Function(*function),
                });
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
    ) -> Result<RootEnvironment, InstallError> {
        let root = authority.root();
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
            let quickjs_bytecode::CompilerClosureSource::ConstructorRealmGlobal(atom) = *source
            else {
                return Err(InstallError::AuthorityInvariant {
                    message: "root closure source is not constructor-realm global",
                });
            };
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
                    if let Some(property) = global_record.record.own_property(&key) {
                        if matches!(request, RealmGlobalRequest::Function)
                            && global_function_replacement_layout(property.layout()).is_none()
                        {
                            return Err(rejected_global_declaration(authority, *closure, name)?);
                        }
                    } else {
                        if !global_record.record.is_extensible() {
                            return Err(rejected_global_declaration(authority, *closure, name)?);
                        }
                        new_object_properties = new_object_properties.saturating_add(1);
                    }
                }
            }
        }
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

        self.global_bindings
            .try_reserve(missing)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::RealmGlobalBindings,
                additional: missing,
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

        for (name, request, _) in requests {
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
                        inserted_global_properties,
                        updated_global_properties,
                    };
                    self.rollback_root_environment(realm, &partial);
                    return Err(InstallError::AuthorityInvariant {
                        message: "constructor-realm global binding is stale",
                    });
                }
                {
                    let binding = self.global_bindings.get_mut(global).ok_or(
                        InstallError::AuthorityInvariant {
                            message: "constructor-realm global binding is stale",
                        },
                    )?;
                    let upgraded = request.upgraded_state(binding.state);
                    if upgraded != binding.state {
                        updated_globals.push((global, binding.state));
                        binding.state = upgraded;
                    }
                }
                global
            } else {
                let Ok(global) = self.global_bindings.try_insert(RealmGlobalBinding {
                    realm,
                    name: name.clone(),
                    state: request.initial_state(),
                }) else {
                    let partial = RootEnvironment {
                        bindings,
                        inserted_globals,
                        updated_globals,
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
                        let replacement = global_function_replacement_layout(existing_layout)
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
                        dynamic_function_declaration_property_layout(),
                        StoredValue::Undefined,
                    ) {
                        let partial = RootEnvironment {
                            bindings,
                            inserted_globals,
                            updated_globals,
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
            bindings.push(EnvironmentBinding::RealmGlobal(global));
        }

        Ok(RootEnvironment {
            bindings,
            inserted_globals,
            updated_globals,
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
    }
}
