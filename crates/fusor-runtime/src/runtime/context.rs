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
    FunctionImplementation, GlobalScriptError, HandleError, HandleKind, HeapFunction, HeapObject,
    HeapReference,
    InstallError, InstalledCode, InstalledRoot, InstalledTemplate, JsNumber, JsString, JsValue,
    ObjectId, ObjectRecord, OrdinaryDynamicFunctionCompiler, PendingRootEnvironment,
    PredefinedAtom, PrimitiveValue, PropertyKey, PropertyLayout, RootPublication, Runtime,
    RuntimeResource, RuntimeUsage, StoredValue, VerifiedBytecode, check_install_limit,
    global_declaration_error, preflight_opcodes, require_root_kind, usize_to_u64,
};
use crate::SharedArrayBufferHandle;

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
        has_prototype || header.kind() == fusor_bytecode::FunctionKind::Generator;
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

    /// Adds Annex B.3.6's `[[IsHTMLDDA]]` internal slot to a host-designated
    /// callable or non-callable object.
    ///
    /// ECMAScript code has no path to this operation. The designation is
    /// irreversible and is intended only for a host compatibility object such
    /// as `document.all`. Primitive values are left unchanged and return
    /// `false`; a function or object returns `true`.
    ///
    /// # Errors
    ///
    /// Returns a handle error for an orphaned or foreign value, or an engine
    /// fault if a rooted heap identity is internally stale.
    pub fn mark_host_defined_is_html_dda(
        &mut self,
        value: &JsValue,
    ) -> Result<bool, crate::ExecutionError> {
        let owner = value.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Value)?;
        let reference = match value.stored()? {
            StoredValue::Function(function) => HeapReference::Function(*function),
            StoredValue::Object(object) => HeapReference::Object(*object),
            StoredValue::Undefined
            | StoredValue::Null
            | StoredValue::Boolean(_)
            | StoredValue::Number(_)
            | StoredValue::BigInt(_)
            | StoredValue::String(_)
            | StoredValue::Symbol(_) => return Ok(false),
        };
        self.runtime
            .object_record_mut(reference)?
            .mark_host_defined_is_html_dda();
        Ok(true)
    }

    /// Exports a thread-safe host capability for a live
    /// `SharedArrayBuffer`. Other values return `None` without invoking
    /// JavaScript or observing user properties.
    ///
    /// # Errors
    ///
    /// Returns a handle or engine error for an orphaned, foreign, stale, or
    /// internally inconsistent value.
    pub fn shared_array_buffer_handle(
        &self,
        value: &JsValue,
    ) -> Result<Option<SharedArrayBufferHandle>, crate::ExecutionError> {
        let owner = value.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Value)?;
        let StoredValue::Object(object) = value.stored()? else {
            return Ok(None);
        };
        if !self.runtime.objects.contains(*object) {
            return Err(crate::HandleError::Stale {
                kind: HandleKind::Value,
                index: object.index(),
                generation: object.generation(),
            }
            .into());
        }
        let handle = self
            .runtime
            .array_buffer_state(*object)?
            .and_then(|state| state.shared_data_block())
            .map(|block| SharedArrayBufferHandle {
                block: Arc::clone(block),
            });
        Ok(handle)
    }

    /// Imports one shared host capability as a new `SharedArrayBuffer` object
    /// using this realm's intrinsic prototype.
    ///
    /// # Errors
    ///
    /// Returns a structured heap, byte-budget, or public-root failure.
    pub fn import_shared_array_buffer(
        &mut self,
        handle: &SharedArrayBufferHandle,
    ) -> Result<JsValue, crate::ExecutionError> {
        let prototype = HeapReference::Object(
            self.runtime
                .realm_shared_array_buffer_prototype(self.realm)?,
        );
        let object = self
            .runtime
            .allocate_shared_array_buffer_block(prototype, Arc::clone(&handle.block))?;
        self.runtime.public_value(StoredValue::Object(object))
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

    /// Creates a string property key for the host property API.
    ///
    /// Integer-like names become canonical array-index keys (ECMA-262 6.1.7);
    /// every other name interns a string atom. The result feeds
    /// [`Object::get`], [`Object::set`], [`Object::define_own_property`],
    /// [`Object::has`], and [`Object::delete`].
    ///
    /// This function never panics.
    ///
    /// # Errors
    ///
    /// Returns a string error when `name` is not valid UTF-16, or an atom
    /// error when the atom table refuses the interning.
    pub fn property_key(
        &mut self,
        name: &str,
    ) -> Result<PropertyKey, crate::ExecutionError> {
        let string = JsString::from_utf8(name)?;
        Ok(self.runtime.property_key_from_string(&string)?)
    }

    /// Converts a same-runtime String or Symbol value into a property key.
    ///
    /// Unlike ECMA-262 `ToPropertyKey`, no implicit coercion is performed:
    /// numbers and every other non-key value are rejected instead of being
    /// stringified, so host code cannot accidentally index an object with a
    /// surprise key. Symbol values come from [`Self::symbol`] or from any
    /// JavaScript Symbol observation.
    ///
    /// This function never panics.
    ///
    /// # Errors
    ///
    /// Returns a handle error for an orphaned, foreign, or stale value, a
    /// wrong-value-kind error (reporting `String` as the expected kind) for
    /// anything but a String or Symbol, or an atom error when the interning
    /// fails.
    pub fn property_key_from_value(
        &mut self,
        value: &JsValue,
    ) -> Result<PropertyKey, crate::ExecutionError> {
        let owner = value.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Value)?;
        match value.stored()? {
            StoredValue::String(string) => {
                Ok(self.runtime.property_key_from_string(string)?)
            }
            StoredValue::Symbol(symbol) => {
                Ok(self.runtime.property_key_from_symbol(symbol)?)
            }
            other => Err(HandleError::WrongValueKind {
                expected: crate::ValueKind::String,
                actual: other.kind(),
            }
            .into()),
        }
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

    /// Reports whether a live object is a Proxy exotic object.
    ///
    /// # Errors
    ///
    /// Returns a handle error for an orphaned, foreign, or stale value, or an
    /// engine fault if the runtime heap does not contain the object.
    pub fn object_is_proxy(&self, value: &JsValue) -> Result<bool, crate::ExecutionError> {
        let owner = value.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Value)?;
        let StoredValue::Object(object) = value.stored()? else {
            return Ok(false);
        };
        Ok(self
            .runtime
            .objects
            .get(*object)
            .ok_or(crate::EngineFault::StaleHeapEdge {
                edge: "object",
                index: object.index(),
                generation: object.generation(),
            })?
            .proxy_state()
            .is_some())
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
        let root_is_generator = root_header.kind() == fusor_bytecode::FunctionKind::Generator;
        let root_is_async = root_header.kind() == fusor_bytecode::FunctionKind::Async;
        let root_is_async_generator =
            root_header.kind() == fusor_bytecode::FunctionKind::AsyncGenerator;
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
                fusor_bytecode::CompilerClosureSource::ConstructorRealmGlobal(_) => {}
                fusor_bytecode::CompilerClosureSource::DirectEvalBinding {
                    index,
                    environment_size,
                }
                | fusor_bytecode::CompilerClosureSource::DirectEvalVariable {
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
                fusor_bytecode::CompilerClosureSource::ParentVariableReference(_)
                | fusor_bytecode::CompilerClosureSource::ParentClosure(_) => {
                    return Err(InstallError::AuthorityInvariant {
                        message: "root function requires an external parent environment",
                    });
                }
                fusor_bytecode::CompilerClosureSource::Module { .. } => {
                    // Module cells are materialized by the runtime linker; root
                    // admission of a Module authority is not yet supported.
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
            None,
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
                lexical_eval_in_class_field_initializer: false,
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

    // ---- Module API ----

    /// Returns an undefined `JsValue` rooted in this context's release mailbox.
    #[must_use]
    pub fn undefined_value(&self) -> JsValue {
        JsValue::primitive(&self.runtime.mailbox, PrimitiveValue::Undefined)
    }

    /// Invokes an installed JavaScript function with a receiver and argument
    /// list, returning the rooted completion value (ECMA-262 `[[Call]]`).
    ///
    /// The receiver and arguments must be rooted handles belonging to this
    /// context's runtime. The returned value is a fresh public root; the
    /// caller drops it to release the root.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Thrown`] with the rooted exception when the call
    /// throws, and [`CallError::Execution`] for engine faults.
    pub fn call_function(
        &mut self,
        function: &Function,
        receiver: JsValue,
        arguments: Vec<JsValue>,
        limits: ExecutionLimits,
    ) -> Result<JsValue, crate::CallError> {
        let function_id = function
            .id()
            .map_err(|error| crate::CallError::Execution(error.into()))?;
        let receiver = receiver
            .stored()
            .map_err(|error| crate::CallError::Execution(error.into()))?
            .duplicate();
        let mut stored_arguments = Vec::with_capacity(arguments.len());
        for argument in &arguments {
            let value = argument
                .stored()
                .map_err(|error| crate::CallError::Execution(error.into()))?
                .duplicate();
            stored_arguments.push(value);
        }
        let result = crate::vm::call_function_internal(
            self.runtime,
            function_id,
            receiver,
            stored_arguments,
            limits,
            None,
        );
        match result {
            Ok(value) => self
                .runtime
                .public_value(value)
                .map_err(crate::CallError::Execution),
            Err(error) => Err(call_error_from_execution(self.runtime, self.realm, error)),
        }
    }

    /// Installs a Rust closure as a JavaScript function.
    ///
    /// The callback is `Fn(&mut Context, HostCall) -> Result<JsValue, JsValue>`:
    /// return `Ok` with the result, or `Err` with the value to throw.
    ///
    /// The installed function carries the spec construct shape (ECMA-262
    /// 9.2.12 / 20.2.4.3): a non-enumerable, non-configurable, writable own
    /// `prototype` property whose value is a fresh ordinary object with a
    /// non-enumerable `constructor` property pointing back at the function,
    /// so `instanceof` works and subclasses inherit the shape.
    ///
    /// A construct call (`new f()`, `[[Construct]]` in ECMA-262 9.2.2) first
    /// performs `OrdinaryCreateFromConstructor(new_target,
    /// "%Object.prototype%")` — including the observable `Get(newTarget,
    /// "prototype")` — and hands that fresh object to the callback as
    /// [`HostCall::this`] with [`HostCall::new_target`] set. An object result
    /// replaces `this`; a primitive result falls back to `this`. A plain call
    /// keeps the ordinary receiver and reports no `new.target`.
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] for a resource limit or engine fault.
    pub fn create_host_function<F>(
        &mut self,
        name: &str,
        callback: F,
    ) -> Result<Function, crate::ExecutionError>
    where
        F: crate::HostCallback + 'static,
    {
        let index = self.runtime.host_functions.len();
        self.runtime.host_functions.push(Some(Box::new(callback)));
        let id = super::HostFunctionId::new(index);

        let prototype = self.runtime.realm_function_prototype(self.realm)?;
        let name_key = self.runtime.predefined_property_key(PredefinedAtom::Name);
        let length_key = self.runtime.predefined_property_key(PredefinedAtom::Length);
        let prototype_key = self.runtime.predefined_property_key(PredefinedAtom::Prototype);
        let constructor_key = self.runtime.predefined_property_key(PredefinedAtom::Constructor);
        let function_name = JsString::from_utf8(name).map_err(crate::ExecutionError::from)?;
        let mut record = ObjectRecord::empty(Some(HeapReference::Function(prototype)));
        record
            .try_reserve_data(3)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 3,
            })?;
        record
            .append_data(
                length_key,
                PropertyLayout::data(false, false, true),
                StoredValue::Number(JsNumber::from_i32(0)),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                name_key,
                PropertyLayout::data(false, false, true),
                StoredValue::String(function_name),
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        record
            .append_data(
                prototype_key,
                PropertyLayout::data(true, false, false),
                StoredValue::Undefined,
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;

        // The spec `prototype` own property is a fresh ordinary object whose
        // prototype is `%Object.prototype%`, with a non-enumerable
        // `constructor` back reference (ECMA-262 9.2.12 / 20.2.4.3).
        let object_prototype = self.runtime.realm_object_prototype(self.realm)?;
        super::check_execution_limit(
            RuntimeResource::HeapObjects,
            self.runtime.limits.max_heap_objects,
            usize_to_u64(self.runtime.objects.len()).saturating_add(1),
        )?;
        let mut prototype_record =
            ObjectRecord::empty(Some(HeapReference::Object(object_prototype)));
        prototype_record
            .try_reserve_data(1)
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        prototype_record
            .append_data(
                constructor_key.clone(),
                PropertyLayout::data(true, false, true),
                StoredValue::Undefined,
            )
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::ObjectProperties,
                additional: 1,
            })?;
        let prototype_object = self
            .runtime
            .insert_heap_object(HeapObject::ordinary(prototype_record))
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapObjects,
                additional: 1,
            })?;
        if !record.replace_existing_data(
            &self.runtime.predefined_property_key(PredefinedAtom::Prototype),
            StoredValue::Object(prototype_object),
        ) {
            let removed = self.runtime.objects.remove(prototype_object);
            debug_assert!(removed.is_some());
            return Err(crate::ExecutionError::from(crate::EngineFault::RuntimeInvariant {
                message: "host function lost its prototype property before installation",
            }));
        }

        super::check_execution_limit(
            RuntimeResource::HeapFunctions,
            self.runtime.limits.max_heap_functions,
            usize_to_u64(self.runtime.functions.len()).saturating_add(1),
        )?;
        self.runtime.functions.try_reserve(1).map_err(|_| {
            crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            }
        })?;
        let function = self
            .runtime
            .insert_heap_function(HeapFunction {
                implementation: FunctionImplementation::Native(super::NativeFunction {
                    realm: self.realm,
                    kind: super::NativeFunctionKind::Host(id),
                }),
                object: record,
                public_roots: 0,
            })
            .map_err(|_| crate::ExecutionError::AllocationFailed {
                resource: RuntimeResource::HeapFunctions,
                additional: 1,
            })?;
        let updated = self
            .runtime
            .objects
            .get_mut(prototype_object)
            .is_some_and(|node| {
                node.record
                    .replace_existing_data(&constructor_key, StoredValue::Function(function))
            });
        if !updated {
            let removed = self.runtime.functions.remove(function);
            debug_assert!(removed.is_some());
            let removed = self.runtime.objects.remove(prototype_object);
            debug_assert!(removed.is_some());
            return Err(crate::ExecutionError::from(crate::EngineFault::RuntimeInvariant {
                message: "host function prototype lost its constructor property",
            }));
        }
        self.runtime.object_properties = self.runtime.object_properties.saturating_add(4);
        self.runtime.collection_pending = true;
        let value = self.runtime.public_value(StoredValue::Function(function))?;
        Ok(Function::from_root(value))
    }

    /// Defines a data property named `name` on the realm's global object with
    /// the fixed descriptor `{ value, writable: true, enumerable: false,
    /// configurable: true }`, so JavaScript code reaches it as
    /// `globalThis.name`.
    ///
    /// The definition goes through the ordinary `[[DefineOwnProperty]]`
    /// authority (`ValidateAndApplyPropertyDescriptor`): redefining an
    /// existing compatible property updates its slot in place (no shadow
    /// property is appended), and an incompatible existing property or a
    /// non-extensible (frozen) global raises the same `TypeError` JavaScript
    /// observes. A global `var` binding is non-configurable and enumerable,
    /// so overwriting it with this enumerable:false descriptor raises
    /// "property is not configurable" — fail closed by design.
    ///
    /// This function never panics.
    ///
    /// # Errors
    ///
    /// Returns a handle error for a foreign or orphaned value, a `TypeError`
    /// exception when the definition is rejected by the descriptor
    /// authority, and a limit, allocation, or engine error for runtime
    /// failures.
    pub fn set_global(&mut self, name: &str, value: JsValue) -> Result<(), crate::ExecutionError> {
        let owner = value.owner()?;
        self.runtime.validate_owner(&owner, HandleKind::Value)?;
        let stored = value.stored()?.duplicate();
        let object = self.runtime.realm_global_object(self.realm)?;
        let name_string = JsString::from_utf8(name)?;
        let key = self.runtime.property_key_from_string(&name_string)?;
        crate::vm::host_set_global(
            self.runtime,
            HeapReference::Object(object),
            key,
            stored,
            self.realm,
        )
    }

    /// Constructs a fresh realm-owned JavaScript Error object of the given
    /// intrinsic family (ECMA-262 20.5.1.1 `Error` constructor semantics:
    /// `%Error%`, `%TypeError%`, `%RangeError%`, `%SyntaxError%`, and the
    /// remaining families all construct here).
    ///
    /// The returned value is a rooted ordinary object with the family's
    /// intrinsic prototype and a `message` own property. It is not thrown by
    /// this call; hand it to [`Object::get`]-observable code, to a host
    /// callback's `Err` arm, or to a future Promise rejection to make the
    /// engine surface it.
    ///
    /// This function never panics.
    ///
    /// # Errors
    ///
    /// Returns a string error when `message` is not valid UTF-16, or a
    /// limit, allocation, or engine error when the object cannot be
    /// materialized.
    pub fn error(
        &mut self,
        kind: crate::ErrorObjectKind,
        message: &str,
    ) -> Result<JsValue, crate::ExecutionError> {
        let object = self.runtime.materialize_error_object_of_family(
            self.realm,
            super::ErrorIntrinsicKind::from_error_object_kind(kind),
            JsString::from_utf8(message)?,
            None,
        )?;
        self.runtime.public_value(StoredValue::Object(object))
    }

    /// Roots this context's realm global object as an ordinary object value
    /// (ECMA-262 9.1.1 `GetGlobalObject`).
    ///
    /// The returned value has [`crate::ValueKind::Object`]; convert it with
    /// [`JsValue::into_object`] and use the [`Object`] property API to read
    /// and write globals from the host, the inverse of [`Self::set_global`].
    ///
    /// This function never panics.
    ///
    /// # Errors
    ///
    /// Returns an engine error if the realm's global object is internally
    /// absent or the public root cannot be allocated.
    pub fn global_object(&mut self) -> Result<JsValue, crate::ExecutionError> {
        let object = self.runtime.realm_global_object(self.realm)?;
        self.runtime.public_value(StoredValue::Object(object))
    }

    /// Registers a module record in this context's realm.
    pub fn register_module(
        &mut self,
        key: super::ModuleKey,
        syntax_record: fusor_frontend::ModuleSyntaxRecord,
        authority: Arc<VerifiedBytecode>,
    ) -> Result<(), super::ModuleError> {
        let record = super::modules::SourceTextModuleRecord::new(
            self.realm,
            key.clone(),
            syntax_record,
            authority,
        );
        let id = self
            .runtime
            .modules
            .try_insert(record)
            .map_err(|_| super::ModuleError::link("module allocation failed"))?;
        let realm_state = self
            .runtime
            .realms
            .get_mut(self.realm)
            .ok_or_else(|| super::ModuleError::link("realm disappeared"))?;
        realm_state.module_registry.insert(key, id);
        Ok(())
    }

    /// Records a host resolution edge from `referrer` to `dependency` for the
    /// raw import `specifier` text.
    ///
    /// This is the host-driven half of ECMA-262 `HostResolveImportedModule`:
    /// resolution is per (referrer, specifier), not per specifier text, so two
    /// modules importing the same specifier text may resolve to different
    /// records, and one record may be the target of many specifier texts.
    /// Both modules must already be registered through
    /// [`Self::register_module`]; `specifier` is stored exactly as passed (the
    /// facade passes the raw specifier text from the syntax record).
    pub fn register_module_dependency(
        &mut self,
        referrer: &super::ModuleKey,
        specifier: &str,
        dependency: &super::ModuleKey,
    ) -> Result<(), super::ModuleError> {
        let realm_state = self
            .runtime
            .realms
            .get(self.realm)
            .ok_or_else(|| super::ModuleError::link("realm disappeared"))?;
        let referrer_id = realm_state
            .module_registry
            .get(referrer)
            .copied()
            .ok_or_else(|| {
                super::ModuleError::link(format!("referrer module '{referrer}' is not registered"))
            })?;
        let dependency_id = realm_state
            .module_registry
            .get(dependency)
            .copied()
            .ok_or_else(|| {
                super::ModuleError::link(format!(
                    "dependency module '{dependency}' is not registered"
                ))
            })?;
        let record = self
            .runtime
            .modules
            .get_mut(referrer_id)
            .ok_or_else(|| super::ModuleError::link("referrer record disappeared"))?;
        record
            .resolved_dependencies
            .insert(specifier.to_owned(), dependency_id);
        Ok(())
    }

    /// Links a module graph starting from the given root key.
    pub fn link_module(&mut self, root: &super::ModuleKey) -> Result<(), super::ModuleError> {
        let id = self
            .runtime
            .realms
            .get(self.realm)
            .and_then(|state| state.module_registry.get(root).copied())
            .ok_or_else(|| super::ModuleError::link("module is not registered"))?;
        super::modules::link_module(self.runtime, self.realm, id)
    }

    /// Evaluates a linked module graph starting from the given root key.
    pub fn evaluate_module(
        &mut self,
        root: &super::ModuleKey,
        limits: ExecutionLimits,
    ) -> Result<(), super::ModuleError> {
        let id = self
            .runtime
            .realms
            .get(self.realm)
            .and_then(|state| state.module_registry.get(root).copied())
            .ok_or_else(|| super::ModuleError::link("module is not registered"))?;
        super::modules::evaluate_module(self.runtime, self.realm, id, limits, None)
    }

    /// Evaluates a linked module graph with a dynamic-function compiler
    /// available for `eval`/`Function` created during module execution.
    pub fn evaluate_module_with_dynamic_function_compiler(
        &mut self,
        root: &super::ModuleKey,
        limits: ExecutionLimits,
        compiler: &Arc<dyn OrdinaryDynamicFunctionCompiler>,
    ) -> Result<(), super::ModuleError> {
        let id = self
            .runtime
            .realms
            .get(self.realm)
            .and_then(|state| state.module_registry.get(root).copied())
            .ok_or_else(|| super::ModuleError::link("module is not registered"))?;
        super::modules::evaluate_module(self.runtime, self.realm, id, limits, Some(compiler))
    }

    // ---- Dynamic import host-load boundary ----

    /// Removes the oldest parked dynamic `import()` load request, if any.
    ///
    /// The host resolves [`PendingDynamicImport::specifier`] (relative to
    /// [`PendingDynamicImport::referrer`] when present), loads and compiles
    /// the module graph, registers every record through
    /// [`Self::register_module`], and then settles the import through
    /// [`Self::complete_dynamic_import`] or [`Self::reject_dynamic_import`].
    pub fn take_pending_dynamic_import(&mut self) -> Option<super::PendingDynamicImport> {
        self.runtime.take_pending_dynamic_import()
    }

    /// Returns the number of parked dynamic `import()` load requests.
    #[must_use]
    pub fn pending_dynamic_import_count(&self) -> usize {
        self.runtime.pending_dynamic_import_count()
    }

    /// Returns whether a module record is registered under `key` in this
    /// context's realm. A registered module must not be registered again;
    /// completing a dynamic import against it reuses the existing record
    /// (link and evaluation are idempotent per ECMA-262).
    #[must_use]
    pub fn has_module(&self, key: &super::ModuleKey) -> bool {
        self.runtime.registered_module(self.realm, key).is_some()
    }

    /// Returns the recorded evaluation error of the module registered under
    /// `key` in this context's realm, if its evaluation failed (ECMA-262
    /// [[EvaluationError]]).
    ///
    /// For a graph with top-level await the failure settles asynchronously:
    /// [`Self::evaluate_module`] returns once evaluation *starts*, and the
    /// rejection continuation records the error while host jobs drain (see
    /// [`Self::drain_host_jobs`]).
    #[must_use]
    pub fn module_evaluation_error(&self, key: &super::ModuleKey) -> Option<super::ModuleError> {
        let module = self.runtime.registered_module(self.realm, key)?;
        self.runtime.modules.get(module)?.evaluation_error.clone()
    }

    /// Completes a parked dynamic `import()` (`FinishDynamicImport`).
    ///
    /// The host must have registered the loaded graph under `root`. The graph
    /// is linked and evaluated synchronously at completion time; the import
    /// Promise then fulfills with the module namespace exotic object, or
    /// rejects with the link error (`SyntaxError`) or the evaluation
    /// exception. Promise reactions queue as ordinary jobs and run at the
    /// next host-job checkpoint (see [`Self::drain_host_jobs`]).
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] only for internal runtime failures;
    /// spec-level load, link, and evaluation failures settle the Promise.
    pub fn complete_dynamic_import(
        &mut self,
        import: super::PendingDynamicImport,
        root: &super::ModuleKey,
        limits: ExecutionLimits,
        compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    ) -> Result<(), crate::ExecutionError> {
        crate::vm::complete_dynamic_import_load(self.runtime, import, root, limits, compiler)
    }

    /// Rejects a parked dynamic `import()` with a `TypeError` carrying the
    /// host's load or resolution failure message. Use this when the module
    /// graph could not be produced at all (resolution miss, IO error, parse
    /// or compile failure); the failure never throws synchronously.
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] only for internal runtime failures.
    pub fn reject_dynamic_import(
        &mut self,
        import: super::PendingDynamicImport,
        message: &str,
    ) -> Result<(), crate::ExecutionError> {
        crate::vm::reject_dynamic_import_load(self.runtime, import, message)
    }

    /// Rejects a parked dynamic `import()` whose requested module failed to
    /// parse or compile with a `SyntaxError` carrying the message (ECMA-262
    /// `FinishDynamicImport` onRejected for a resolution-phase failure).
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] only for internal runtime failures.
    pub fn reject_dynamic_import_syntax(
        &mut self,
        import: super::PendingDynamicImport,
        message: &str,
    ) -> Result<(), crate::ExecutionError> {
        crate::vm::reject_dynamic_import_load_kind(
            self.runtime,
            import,
            crate::ExceptionKind::SyntaxError,
            message,
        )
    }

    /// Drains queued host jobs (Promise reactions, finalization cleanup,
    /// ready `Atomics.waitAsync` completions) to quiescence.
    ///
    /// Host-turn checkpoint for drivers that settle parked dynamic `import()`
    /// loads outside an interpreter call: run this after
    /// [`Self::complete_dynamic_import`] / [`Self::reject_dynamic_import`] so
    /// the queued reactions (which may park further dynamic imports) execute.
    ///
    /// # Errors
    ///
    /// Returns an [`ExecutionError`] when a job fails with an uncatchable
    /// host/runtime failure; ordinary JavaScript exceptions inside jobs are
    /// delivered to their Promise reactions and do not surface here.
    pub fn drain_host_jobs(
        &mut self,
        limits: ExecutionLimits,
        compiler: Option<&Arc<dyn OrdinaryDynamicFunctionCompiler>>,
    ) -> Result<(), crate::ExecutionError> {
        crate::vm::drain_host_jobs_with_limits(self.runtime, compiler, limits)
    }
}

/// Converts a function-call failure into a [`crate::CallError`], re-rooting a
/// thrown JavaScript exception (or materializing an engine-generated error
/// object) so the caller can observe it.
fn call_error_from_execution(
    runtime: &mut Runtime,
    realm: super::RealmId,
    error: crate::ExecutionError,
) -> crate::CallError {
    match error {
        crate::ExecutionError::Exception(exception) => {
            if let Some(value) = exception.thrown_value() {
                return crate::CallError::Thrown(value.clone());
            }
            if let (Some(kind), Some(message)) = (exception.kind(), exception.message()) {
                match runtime.materialize_error_object(realm, kind, message.clone(), None) {
                    Ok(object) => {
                        return match runtime.public_value(StoredValue::Object(object)) {
                            Ok(value) => crate::CallError::Thrown(value),
                            Err(error) => crate::CallError::Execution(error),
                        };
                    }
                    Err(error) => return crate::CallError::Execution(error),
                }
            }
            crate::CallError::Execution(crate::ExecutionError::Exception(exception))
        }
        other => crate::CallError::Execution(other),
    }
}
