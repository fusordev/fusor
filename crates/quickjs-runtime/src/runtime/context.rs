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

//! Public execution-context values plus verified root installation and execution.

use super::{
    Arc, AtomError, BytecodeFunction, CompilerExecutableKind, Context, DynamicFunctionScriptError,
    EnvironmentBinding, ErrorObjectKind, ExceptionKind, ExecutionLimits, Function,
    FunctionImplementation, GlobalScriptError, HandleKind, HeapFunction, HeapObject, HeapReference,
    InstallError, InstalledCode, InstalledRoot, InstalledTemplate, JsNumber, JsString, JsValue,
    ObjectId, ObjectRecord, OrdinaryDynamicFunctionCompiler, PendingRootEnvironment,
    PredefinedAtom, PrimitiveValue, PropertyKey, PropertyLayout, RootPublication, Runtime,
    RuntimeResource, RuntimeUsage, StoredValue, VerifiedBytecode, check_install_limit,
    global_declaration_error, preflight_opcodes, require_root_kind, usize_to_u64,
};

struct RootFunctionRecords {
    function: ObjectRecord,
    prototype: Option<ObjectRecord>,
    property_count: u64,
}

fn append_root_data(
    record: &mut ObjectRecord,
    key: PropertyKey,
    layout: PropertyLayout,
    value: StoredValue,
) -> Result<(), InstallError> {
    record
        .append_data(key, layout, value)
        .map_err(|_| InstallError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: 1,
        })
}

fn build_root_function_records(
    runtime: &Runtime,
    authority: &VerifiedBytecode,
    templates: &[InstalledTemplate],
    publication: RootPublication,
    function_prototype: HeapReference,
    object_prototype: Option<ObjectId>,
) -> Result<RootFunctionRecords, InstallError> {
    let mut function = ObjectRecord::empty(Some(function_prototype));
    if !publication.is_public() {
        return Ok(RootFunctionRecords {
            function,
            prototype: None,
            property_count: 0,
        });
    }

    let root = authority.root();
    let header = root.function().control_flow().function_header();
    let has_prototype = header.flags().has_prototype();
    let creates_prototype =
        has_prototype || header.kind() == quickjs_bytecode::FunctionKind::Generator;
    let installed_index = usize::try_from(authority.root_id().get()).map_err(|_| {
        InstallError::AuthorityInvariant {
            message: "root template index is not representable",
        }
    })?;
    let installed = templates
        .get(installed_index)
        .ok_or(InstallError::AuthorityInvariant {
            message: "root template is absent from staged installation",
        })?;
    let name = root.metadata().function_name().map_or_else(
        || Ok(JsString::empty()),
        |index| {
            installed
                .atoms
                .get(index.get() as usize)
                .and_then(super::Atom::description)
                .cloned()
                .ok_or(InstallError::AuthorityInvariant {
                    message: "root function name atom is absent",
                })
        },
    )?;
    let function_property_count = 2_usize + usize::from(creates_prototype);
    function
        .try_reserve_data(function_property_count)
        .map_err(|_| InstallError::AllocationFailed {
            resource: RuntimeResource::ObjectProperties,
            additional: function_property_count,
        })?;
    append_root_data(
        &mut function,
        runtime.predefined_property_key(PredefinedAtom::Length),
        PropertyLayout::data(false, false, true),
        StoredValue::Number(JsNumber::from_f64(f64::from(
            header.defined_argument_count(),
        ))),
    )?;
    append_root_data(
        &mut function,
        runtime.predefined_property_key(PredefinedAtom::Name),
        PropertyLayout::data(false, false, true),
        StoredValue::String(name),
    )?;

    let prototype = if creates_prototype {
        let object_prototype = object_prototype.ok_or(InstallError::AuthorityInvariant {
            message: "constructable root has no Object.prototype intrinsic",
        })?;
        let prototype_key = runtime.predefined_property_key(PredefinedAtom::Prototype);
        append_root_data(
            &mut function,
            prototype_key,
            PropertyLayout::data(true, false, false),
            StoredValue::Undefined,
        )?;
        let mut prototype = ObjectRecord::empty(Some(HeapReference::Object(object_prototype)));
        prototype
            .try_reserve_data(usize::from(has_prototype))
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: usize::from(has_prototype),
            })?;
        if has_prototype {
            append_root_data(
                &mut prototype,
                runtime.predefined_property_key(PredefinedAtom::Constructor),
                PropertyLayout::data(true, false, true),
                StoredValue::Undefined,
            )?;
        }
        Some(prototype)
    } else {
        None
    };

    Ok(RootFunctionRecords {
        function,
        prototype,
        property_count: usize_to_u64(function_property_count + usize::from(has_prototype)),
    })
}

impl Context<'_> {
    /// Returns current logical usage without ending the exclusive context.
    #[must_use]
    pub fn runtime_usage(&self) -> RuntimeUsage {
        self.runtime.usage()
    }

    /// Creates a runtime-local `undefined` value.
    #[must_use]
    pub fn undefined(&self) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Undefined)
    }

    /// Creates a runtime-local `null` value.
    #[must_use]
    pub fn null(&self) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Null)
    }

    /// Creates a runtime-local Boolean value.
    #[must_use]
    pub fn boolean(&self, value: bool) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Boolean(value))
    }

    /// Creates a runtime-local Number value.
    #[must_use]
    pub fn number(&self, value: JsNumber) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Number(value))
    }

    /// Roots an already-owned immutable JavaScript string in this runtime.
    #[must_use]
    pub fn string(&self, value: JsString) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::String(value))
    }

    /// Creates a fresh runtime-local Symbol with an optional description.
    ///
    /// `None` and `Some(empty_string)` remain observably distinct. Each call
    /// creates a new identity even when descriptions are equal.
    ///
    /// # Errors
    ///
    /// Returns a structured atom limit, allocation, or string-copy error.
    pub fn symbol(&mut self, description: Option<&JsString>) -> Result<JsValue, AtomError> {
        let symbol = self.runtime.atoms.new_unique_symbol(description)?;
        Ok(JsValue::primitive(
            &self.runtime.mailbox,
            PrimitiveValue::Symbol(symbol),
        ))
    }

    /// Roots one predefined well-known Symbol in this runtime.
    ///
    /// String atoms and the private brand atom return `None`; only the pinned
    /// well-known Symbol identities are exposed through this entry.
    #[must_use]
    pub fn well_known_symbol(&self, atom: PredefinedAtom) -> Option<JsValue> {
        let symbol = self.runtime.atoms.predefined(atom);
        (symbol.kind() == crate::AtomKind::Symbol)
            .then(|| JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Symbol(symbol)))
    }

    /// Classifies a same-runtime JavaScript Error object by its intrinsic
    /// prototype lineage.
    ///
    /// Arbitrary non-Error values return `None`. This lets a host classify an
    /// explicit JavaScript `throw` without evaluating additional source or
    /// invoking observable property accessors.
    ///
    /// # Errors
    ///
    /// Returns a handle error for an orphaned, foreign, or stale value, or an
    /// engine fault if the runtime's intrinsic prototype graph is inconsistent.
    pub fn error_object_kind(
        &self,
        value: &JsValue,
    ) -> Result<Option<ErrorObjectKind>, crate::ExecutionError> {
        let owner = value.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Value)?;
        let StoredValue::Object(object) = value.stored()? else {
            return Ok(None);
        };
        self.runtime.error_object_kind(*object).map_err(Into::into)
    }

    /// Transactionally installs complete verified bytecode and materializes
    /// its root function in this context's realm.
    ///
    /// Every instruction in every template is feature-checked, including
    /// unreachable instructions and child functions. Unsupported graphs are
    /// rejected before the runtime safe point. Later failures commit no state
    /// attributable to this installation; the safe point may still reclaim
    /// previously unreachable runtime nodes.
    ///
    /// # Errors
    ///
    /// Returns an exact unsupported opcode, limit, allocation, string, atom,
    /// or authority-invariant failure.
    pub fn instantiate(
        &mut self,
        authority: Arc<VerifiedBytecode>,
    ) -> Result<Function, InstallError> {
        require_root_kind(&authority, CompilerExecutableKind::OrdinaryFunction)?;
        let installed = self.install_root(authority, RootPublication::Public, true, None)?;
        Ok(Function::from_root(JsValue::rooted_heap(
            &self.runtime.mailbox,
            HeapReference::Function(installed.function),
        )))
    }

    /// Installs and executes one complete verified dynamic-Function Script.
    ///
    /// Only an authority whose root is tagged as
    /// [`CompilerExecutableKind::DynamicFunctionScript`] is accepted. The
    /// internal root has no external lexical environment and is never exposed
    /// as a public function. Its receiver is this context's realm-owned global
    /// object; the exact Script completion is rooted before the internal root
    /// is retired.
    ///
    /// # Errors
    ///
    /// Returns a typed installation failure before execution, or an execution,
    /// exception, resource, allocation, publication, or engine failure after
    /// installation.
    pub fn execute_dynamic_function_script(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        limits: ExecutionLimits,
    ) -> Result<JsValue, DynamicFunctionScriptError> {
        self.execute_dynamic_function_script_with_optional_compiler(authority, limits, None)
    }

    /// Installs and executes one complete verified host-loaded Global Script.
    ///
    /// Declaration instantiation is committed immediately before execution,
    /// so Global Script bindings persist in the realm after either normal or
    /// abrupt completion. The internal root function itself is always retired.
    ///
    /// # Errors
    ///
    /// Returns a typed installation failure or an execution, JavaScript
    /// exception, resource, publication, or engine failure.
    pub fn execute_global_script(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        limits: ExecutionLimits,
    ) -> Result<JsValue, GlobalScriptError> {
        self.execute_global_script_with_optional_compiler(authority, limits, None)
    }

    /// Executes a Global Script while allowing nested calls to the realm's
    /// dynamic Function constructors.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::execute_global_script`], plus a
    /// typed nested dynamic-compilation failure or JavaScript `SyntaxError`.
    pub fn execute_global_script_with_dynamic_function_compiler(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        limits: ExecutionLimits,
        compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    ) -> Result<JsValue, GlobalScriptError> {
        self.execute_global_script_with_optional_compiler(authority, limits, Some(compiler))
    }

    fn execute_global_script_with_optional_compiler(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        limits: ExecutionLimits,
        compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    ) -> Result<JsValue, GlobalScriptError> {
        require_root_kind(&authority, CompilerExecutableKind::GlobalScript)?;
        let global_object = self
            .runtime
            .realm_global_object(self.realm)
            .map_err(crate::ExecutionError::from)?;
        let exception_authority = Arc::clone(&authority);
        let mut installed =
            match self.install_root(authority, RootPublication::Internal, true, None) {
                Ok(installed) => installed,
                Err(InstallError::GlobalDeclarationRejected {
                    name,
                    kind: _,
                    function,
                    pc,
                    source_span,
                }) => {
                    let (message, origin) = global_declaration_error(
                        &exception_authority,
                        &name,
                        function,
                        pc,
                        source_span,
                    )
                    .map_err(GlobalScriptError::Execution)?;
                    let exception = crate::JsException::engine_error(
                        ExceptionKind::SyntaxError,
                        message,
                        origin,
                        Vec::new(),
                    );
                    return Err(GlobalScriptError::Execution(
                        crate::ExecutionError::Exception(exception),
                    ));
                }
                Err(error) => return Err(error.into()),
            };
        let result = match compiler {
            Some(compiler) => self.execute_internal_root_with_dynamic_function_compiler(
                &mut installed,
                StoredValue::Object(global_object),
                limits,
                compiler,
            ),
            None => self.execute_internal_root(
                &mut installed,
                StoredValue::Object(global_object),
                limits,
            ),
        }
        .and_then(|completion| self.runtime.public_value(completion));
        let retirement = self.runtime.retire_dynamic_root(installed);
        match retirement {
            Ok(()) => result.map_err(GlobalScriptError::Execution),
            Err(fault) => Err(GlobalScriptError::Execution(fault.into())),
        }
    }

    /// Installs and executes one complete verified dynamic-Function Script
    /// while allowing nested calls to the realm's `%Function%` intrinsic.
    ///
    /// The immutable compiler service receives only owned source strings and
    /// returns only a complete verified authority. It cannot observe this
    /// context or the Script's lexical environment.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::execute_dynamic_function_script`],
    /// plus a typed nested dynamic-compilation failure or JavaScript
    /// `SyntaxError`.
    pub fn execute_dynamic_function_script_with_dynamic_function_compiler(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        limits: ExecutionLimits,
        compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    ) -> Result<JsValue, DynamicFunctionScriptError> {
        self.execute_dynamic_function_script_with_optional_compiler(
            authority,
            limits,
            Some(compiler),
        )
    }

    fn execute_dynamic_function_script_with_optional_compiler(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        limits: ExecutionLimits,
        compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    ) -> Result<JsValue, DynamicFunctionScriptError> {
        require_root_kind(&authority, CompilerExecutableKind::DynamicFunctionScript)?;
        let global_object = self
            .runtime
            .realm_global_object(self.realm)
            .map_err(crate::ExecutionError::from)?;
        let exception_authority = Arc::clone(&authority);
        let mut installed =
            match self.install_root(authority, RootPublication::Internal, true, None) {
                Ok(installed) => installed,
                Err(InstallError::GlobalDeclarationRejected {
                    name,
                    kind: _,
                    function,
                    pc,
                    source_span,
                }) => {
                    let (message, origin) = global_declaration_error(
                        &exception_authority,
                        &name,
                        function,
                        pc,
                        source_span,
                    )
                    .map_err(DynamicFunctionScriptError::Execution)?;
                    let exception = crate::JsException::engine_error(
                        ExceptionKind::TypeError,
                        message,
                        origin,
                        Vec::new(),
                    );
                    return Err(DynamicFunctionScriptError::Execution(
                        crate::ExecutionError::Exception(exception),
                    ));
                }
                Err(error) => return Err(error.into()),
            };
        let result = match compiler {
            Some(compiler) => self.execute_internal_root_with_dynamic_function_compiler(
                &mut installed,
                StoredValue::Object(global_object),
                limits,
                compiler,
            ),
            None => self.execute_internal_root(
                &mut installed,
                StoredValue::Object(global_object),
                limits,
            ),
        }
        .and_then(|completion| self.runtime.public_value(completion));
        let retirement = self.runtime.retire_dynamic_root(installed);
        match retirement {
            Ok(()) => result.map_err(DynamicFunctionScriptError::Execution),
            Err(fault) => Err(DynamicFunctionScriptError::Execution(fault.into())),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "installation preflight and the failure-atomic commit are one audited transaction"
    )]
    fn install_root(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        publication: RootPublication,
        prepare_safe_point: bool,
        external_environment: Option<&[Option<EnvironmentBinding>]>,
    ) -> Result<InstalledRoot, InstallError> {
        preflight_opcodes(&authority)?;
        if prepare_safe_point {
            self.runtime.prepare_installation_safe_point()?;
        }
        let graph_usage = authority.compiler_graph().usage();
        let functions = graph_usage.functions();
        let atoms = graph_usage.atoms();
        let constants = graph_usage.constants();
        check_install_limit(
            RuntimeResource::InstalledCode,
            self.runtime.limits.max_installed_code,
            usize_to_u64(self.runtime.code.len()).saturating_add(1),
        )?;
        check_install_limit(
            RuntimeResource::InstalledTemplates,
            self.runtime.limits.max_installed_templates,
            self.runtime.installed_templates.saturating_add(functions),
        )?;
        check_install_limit(
            RuntimeResource::InstalledAtoms,
            self.runtime.limits.max_installed_atoms,
            self.runtime.installed_atoms.saturating_add(atoms),
        )?;
        check_install_limit(
            RuntimeResource::InstalledConstants,
            self.runtime.limits.max_installed_constants,
            self.runtime.installed_constants.saturating_add(constants),
        )?;
        check_install_limit(
            RuntimeResource::HeapFunctions,
            self.runtime.limits.max_heap_functions,
            usize_to_u64(self.runtime.functions.len()).saturating_add(1),
        )?;
        let root_header = authority.root().function().control_flow().function_header();
        let root_is_generator = root_header.kind() == quickjs_bytecode::FunctionKind::Generator;
        let root_is_async = root_header.kind() == quickjs_bytecode::FunctionKind::Async;
        let root_is_async_generator =
            root_header.kind() == quickjs_bytecode::FunctionKind::AsyncGenerator;
        let root_has_prototype = publication.is_public() && root_header.flags().has_prototype();
        let root_creates_prototype = publication.is_public()
            && (root_has_prototype || root_is_generator || root_is_async_generator);
        let function_prototype = if root_is_async_generator {
            HeapReference::Object(
                self.runtime
                    .realm_async_generator_function_prototype(self.realm)
                    .map_err(|_| InstallError::AuthorityInvariant {
                        message: "constructor realm has no AsyncGeneratorFunction.prototype intrinsic",
                    })?,
            )
        } else if root_is_generator {
            HeapReference::Object(
                self.runtime
                    .realm_generator_function_prototype(self.realm)
                    .map_err(|_| InstallError::AuthorityInvariant {
                        message: "constructor realm has no GeneratorFunction.prototype intrinsic",
                    })?,
            )
        } else if root_is_async {
            HeapReference::Object(
                self.runtime
                    .realm_async_function_prototype(self.realm)
                    .map_err(|_| InstallError::AuthorityInvariant {
                        message: "constructor realm has no AsyncFunction.prototype intrinsic",
                    })?,
            )
        } else {
            HeapReference::Function(self.runtime.realm_function_prototype(self.realm).map_err(
                |_| InstallError::AuthorityInvariant {
                    message: "constructor realm has no Function.prototype intrinsic",
                },
            )?)
        };
        let object_prototype = if root_is_async_generator {
            root_creates_prototype
                .then(|| self.runtime.realm_async_generator_prototype(self.realm))
                .transpose()
        } else if root_is_generator {
            root_creates_prototype
                .then(|| self.runtime.realm_generator_prototype(self.realm))
                .transpose()
        } else {
            root_has_prototype
                .then(|| self.runtime.realm_object_prototype(self.realm))
                .transpose()
        }
        .map_err(|_| InstallError::AuthorityInvariant {
            message: "constructor realm has no function instance prototype intrinsic",
        })?;
        let root_property_count = if publication.is_public() {
            2_u64
                .saturating_add(u64::from(root_creates_prototype))
                .saturating_add(u64::from(root_has_prototype))
        } else {
            0
        };
        check_install_limit(
            RuntimeResource::HeapObjects,
            self.runtime.limits.max_heap_objects,
            usize_to_u64(self.runtime.objects.len())
                .saturating_add(u64::from(root_creates_prototype)),
        )?;
        check_install_limit(
            RuntimeResource::ObjectProperties,
            self.runtime.limits.max_object_properties,
            self.runtime
                .object_properties
                .saturating_add(root_property_count),
        )?;
        if publication.is_public() {
            check_install_limit(
                RuntimeResource::PublicRoots,
                self.runtime.limits.max_public_roots,
                self.runtime.public_roots.saturating_add(1),
            )?;
        }

        let root_sources = authority.root().function().closure_sources();
        for source in root_sources {
            match *source {
                quickjs_bytecode::CompilerClosureSource::ConstructorRealmGlobal(_) => {}
                quickjs_bytecode::CompilerClosureSource::DirectEvalBinding {
                    index,
                    environment_size,
                }
                | quickjs_bytecode::CompilerClosureSource::DirectEvalVariable {
                    index,
                    environment_size,
                } => {
                    let Some(environment) = external_environment else {
                        return Err(InstallError::AuthorityInvariant {
                            message: "direct-eval root has no caller environment",
                        });
                    };
                    if environment.len() != environment_size as usize
                        || environment.get(index as usize).copied().flatten().is_none()
                    {
                        return Err(InstallError::AuthorityInvariant {
                            message: "direct-eval caller environment does not match its authority",
                        });
                    }
                }
                quickjs_bytecode::CompilerClosureSource::ParentVariableReference(_)
                | quickjs_bytecode::CompilerClosureSource::ParentClosure(_) => {
                    return Err(InstallError::AuthorityInvariant {
                        message: "root function requires an external parent environment",
                    });
                }
            }
        }

        self.runtime
            .code
            .try_reserve(1)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledCode,
                additional: 1,
            })?;
        self.runtime
            .functions
            .try_reserve(1)
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        self.runtime
            .objects
            .try_reserve(usize::from(root_creates_prototype))
            .map_err(|_| InstallError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: usize::from(root_creates_prototype),
            })?;
        if publication.is_public() {
            self.runtime.mailbox.try_reserve_root().map_err(|_| {
                InstallError::AllocationFailed {
                    resource: RuntimeResource::ReleaseMailbox,
                    additional: 1,
                }
            })?;
        }

        let templates = match self.runtime.stage_templates(&authority) {
            Ok(templates) => templates,
            Err(error) => {
                if publication.is_public() {
                    self.runtime.mailbox.cancel_reserved_root();
                }
                self.runtime.atoms.collect_dead();
                return Err(error);
            }
        };
        let mut root_records = match build_root_function_records(
            self.runtime,
            &authority,
            &templates,
            publication,
            function_prototype,
            object_prototype,
        ) {
            Ok(records) => records,
            Err(error) => {
                if publication.is_public() {
                    self.runtime.mailbox.cancel_reserved_root();
                }
                self.runtime.atoms.collect_dead();
                return Err(error);
            }
        };
        let mut root_environment = match self.runtime.materialize_root_environment(
            self.realm,
            &authority,
            &templates,
            external_environment,
        ) {
            Ok(environment) => environment,
            Err(error) => {
                if publication.is_public() {
                    self.runtime.mailbox.cancel_reserved_root();
                }
                self.runtime.atoms.collect_dead();
                return Err(error);
            }
        };

        let prototype_object = if let Some(record) = root_records.prototype.take() {
            let Ok(object) = self
                .runtime
                .insert_heap_object(HeapObject::ordinary(record))
            else {
                self.runtime
                    .rollback_root_environment(self.realm, &root_environment);
                if publication.is_public() {
                    self.runtime.mailbox.cancel_reserved_root();
                }
                self.runtime.atoms.collect_dead();
                return Err(InstallError::AllocationFailed {
                    resource: RuntimeResource::HeapObjects,
                    additional: 1,
                });
            };
            let updated = root_records.function.replace_existing_data(
                &self
                    .runtime
                    .predefined_property_key(PredefinedAtom::Prototype),
                StoredValue::Object(object),
            );
            if !updated {
                let removed = self.runtime.objects.remove(object);
                debug_assert!(removed.is_some());
                self.runtime
                    .rollback_root_environment(self.realm, &root_environment);
                if publication.is_public() {
                    self.runtime.mailbox.cancel_reserved_root();
                }
                self.runtime.atoms.collect_dead();
                return Err(InstallError::AuthorityInvariant {
                    message: "constructable root lost its prototype property",
                });
            }
            Some(object)
        } else {
            None
        };

        let mut root_eval_shadows = Vec::new();
        if root_eval_shadows
            .try_reserve_exact(root_environment.bindings.len())
            .is_err()
        {
            if let Some(object) = prototype_object {
                let removed = self.runtime.objects.remove(object);
                debug_assert!(removed.is_some());
            }
            self.runtime
                .rollback_root_environment(self.realm, &root_environment);
            if publication.is_public() {
                self.runtime.mailbox.cancel_reserved_root();
            }
            self.runtime.atoms.collect_dead();
            return Err(InstallError::AllocationFailed {
                resource: RuntimeResource::FrameValues,
                additional: root_environment.bindings.len(),
            });
        }
        root_eval_shadows.resize_with(root_environment.bindings.len(), || None);

        let root_template = authority.root_id();
        let Ok(code) = self.runtime.code.try_insert(InstalledCode {
            authority,
            realm: self.realm,
            templates,
            live_functions: 1,
        }) else {
            if let Some(object) = prototype_object {
                let removed = self.runtime.objects.remove(object);
                debug_assert!(removed.is_some());
            }
            self.runtime
                .rollback_root_environment(self.realm, &root_environment);
            if publication.is_public() {
                self.runtime.mailbox.cancel_reserved_root();
            }
            self.runtime.atoms.collect_dead();
            return Err(InstallError::AllocationFailed {
                resource: RuntimeResource::InstalledCode,
                additional: 1,
            });
        };
        let root_bindings = std::mem::take(&mut root_environment.bindings);
        let Ok(root) = self.runtime.insert_heap_function(HeapFunction {
            implementation: FunctionImplementation::Bytecode(BytecodeFunction {
                code,
                template: root_template,
                environment: root_bindings,
                environment_eval_shadows: root_eval_shadows,
                eval_environment: None,
                lexical_receiver: None,
                lexical_eval_in_function: false,
                lexical_new_target: None,
                lexical_derived_constructor: None,
                lexical_derived_this: None,
                has_instance_elements: false,
                home_object: None,
            }),
            object: root_records.function,
            public_roots: u32::from(publication.is_public()),
        }) else {
            let removed = self.runtime.code.remove(code);
            debug_assert!(removed.is_some());
            if let Some(object) = prototype_object {
                let removed = self.runtime.objects.remove(object);
                debug_assert!(removed.is_some());
            }
            self.runtime
                .rollback_root_environment(self.realm, &root_environment);
            if publication.is_public() {
                self.runtime.mailbox.cancel_reserved_root();
            }
            self.runtime.atoms.collect_dead();
            return Err(InstallError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            });
        };
        if root_has_prototype && let Some(object) = prototype_object {
            let constructor_key = self
                .runtime
                .predefined_property_key(PredefinedAtom::Constructor);
            let updated = self
                .runtime
                .objects
                .get_mut(object)
                .is_some_and(|prototype| {
                    prototype
                        .record
                        .replace_existing_data(&constructor_key, StoredValue::Function(root))
                });
            if !updated {
                let removed = self.runtime.functions.remove(root);
                debug_assert!(removed.is_some());
                let removed = self.runtime.objects.remove(object);
                debug_assert!(removed.is_some());
                let removed = self.runtime.code.remove(code);
                debug_assert!(removed.is_some());
                self.runtime
                    .rollback_root_environment(self.realm, &root_environment);
                if publication.is_public() {
                    self.runtime.mailbox.cancel_reserved_root();
                }
                self.runtime.atoms.collect_dead();
                return Err(InstallError::AuthorityInvariant {
                    message: "new root prototype lost its constructor property",
                });
            }
        }

        self.runtime.installed_templates += functions;
        self.runtime.installed_atoms += atoms;
        self.runtime.installed_constants += constants;
        self.runtime.object_properties = self
            .runtime
            .object_properties
            .saturating_add(root_records.property_count);
        if publication.is_public() {
            self.runtime.public_roots += 1;
        }
        let pending_environment = (!publication.is_public()).then_some(PendingRootEnvironment {
            realm: self.realm,
            environment: root_environment,
        });
        Ok(InstalledRoot {
            function: root,
            code,
            pending_environment,
        })
    }

    /// Installs a verified dynamic-Function Script while bytecode frames are
    /// active.
    ///
    /// The ordinary installation safe point is deliberately skipped because
    /// active VM frames are not public GC roots. Every capability, resource,
    /// reservation, and rollback check performed by normal installation still
    /// applies.
    pub(crate) fn install_dynamic_function_script_during_execution(
        &mut self,
        authority: Arc<VerifiedBytecode>,
    ) -> Result<InstalledRoot, InstallError> {
        require_root_kind(&authority, CompilerExecutableKind::DynamicFunctionScript)?;
        self.install_root(authority, RootPublication::Internal, false, None)
    }

    /// Installs a verified indirect-eval Script while bytecode frames are
    /// active.
    pub(crate) fn install_indirect_eval_script_during_execution(
        &mut self,
        authority: Arc<VerifiedBytecode>,
    ) -> Result<InstalledRoot, InstallError> {
        require_root_kind(&authority, CompilerExecutableKind::IndirectEvalScript)?;
        self.install_root(authority, RootPublication::Internal, false, None)
    }

    /// Installs a verified direct-eval Script and its caller environment while
    /// bytecode frames are active. The authority remains internal and is
    /// retired with its frame.
    pub(crate) fn install_direct_eval_script_during_execution(
        &mut self,
        authority: Arc<VerifiedBytecode>,
        external_environment: &[Option<EnvironmentBinding>],
    ) -> Result<InstalledRoot, InstallError> {
        require_root_kind(&authority, CompilerExecutableKind::DirectEvalScript)?;
        self.install_root(
            authority,
            RootPublication::Internal,
            false,
            Some(external_environment),
        )
    }
}
