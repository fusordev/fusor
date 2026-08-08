//! Dependency-neutral host services used by runtime execution.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::{CompilerBindingPolicy, VerifiedBytecode};

use crate::{JsString, error::DynamicFunctionCompileFailure};

/// Dynamic-function families executable by the runtime core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicFunctionFamily {
    /// An ordinary dynamic `Function`.
    Function,
    /// A synchronous dynamic `GeneratorFunction`.
    GeneratorFunction,
    /// A dynamic `AsyncFunction`.
    AsyncFunction,
    /// A dynamic `AsyncGeneratorFunction`.
    AsyncGeneratorFunction,
}

/// Owned source fragments for one supported dynamic-function compilation.
///
/// JavaScript argument coercion happens before this value is created. The
/// compiler therefore receives immutable JavaScript strings without a runtime
/// handle, caller frame, lexical environment, or Oxc-backed lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicFunctionCompileRequest {
    family: DynamicFunctionFamily,
    parameters: Arc<[JsString]>,
    body: JsString,
}

/// Owned source for one indirect `%eval%` compilation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndirectEvalCompileRequest {
    source: JsString,
}

/// Storage address of one caller binding exposed to direct `eval`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectEvalCallerBindingLocation {
    /// Argument slot in the calling activation.
    Argument(u32),
    /// Local slot in the calling activation.
    Local(u32),
    /// Imported closure slot in the calling function object.
    Closure(u32),
    /// Binding in a retained per-activation eval variable-environment chain.
    EvalVariable {
        /// Zero-based environment depth from the current activation.
        depth: u32,
        /// Zero-based binding entry within that environment.
        index: u32,
    },
}

/// The caller-environment region that owns one direct-eval binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectEvalCallerBindingScope {
    /// A lexical environment between the callsite and the variable environment.
    Lexical,
    /// The caller variable environment inherited by sloppy direct eval.
    Variable,
    /// An environment outside the caller variable environment.
    Outer,
}

/// The variable environment inherited by sloppy direct eval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectEvalVariableEnvironment {
    /// The Realm global environment record.
    Global,
    /// A function or eval-owned declarative environment.
    Function,
    /// A function environment outside an active parameter-initializer scope.
    FunctionParameterInitializer,
}

/// One named live caller binding visible to direct `eval`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectEvalCallerBinding {
    name: JsString,
    policy: CompilerBindingPolicy,
    location: DirectEvalCallerBindingLocation,
    scope: DirectEvalCallerBindingScope,
}

impl DirectEvalCallerBinding {
    /// Creates one outer caller-environment snapshot entry.
    ///
    /// Call [`Self::with_scope`] for intervening lexical or inherited
    /// variable-environment bindings.
    #[must_use]
    pub const fn new(
        name: JsString,
        policy: CompilerBindingPolicy,
        location: DirectEvalCallerBindingLocation,
    ) -> Self {
        Self {
            name,
            policy,
            location,
            scope: DirectEvalCallerBindingScope::Outer,
        }
    }

    /// Attaches the caller-environment region that owns this binding.
    #[must_use]
    pub const fn with_scope(mut self, scope: DirectEvalCallerBindingScope) -> Self {
        self.scope = scope;
        self
    }

    /// Returns the exact identifier name.
    #[must_use]
    pub const fn name(&self) -> &JsString {
        &self.name
    }

    /// Returns the verified declaration policy of the caller binding.
    #[must_use]
    pub const fn policy(&self) -> CompilerBindingPolicy {
        self.policy
    }

    /// Returns the caller-frame storage address.
    #[must_use]
    pub const fn location(&self) -> DirectEvalCallerBindingLocation {
        self.location
    }

    /// Returns the caller-environment region that owns this binding.
    #[must_use]
    pub const fn scope(&self) -> DirectEvalCallerBindingScope {
        self.scope
    }
}

/// Owned source and caller grammar capabilities for one direct `eval`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectEvalCompileRequest {
    source: JsString,
    scope_index: u16,
    capabilities: u8,
    bindings: Arc<[DirectEvalCallerBinding]>,
    variable_environment: DirectEvalVariableEnvironment,
}

impl DirectEvalCompileRequest {
    const STRICT: u8 = 1 << 0;
    const NEW_TARGET: u8 = 1 << 1;
    const SUPER_PROPERTY: u8 = 1 << 2;
    const SUPER_CALL: u8 = 1 << 3;
    const ARGUMENTS_ALLOWED: u8 = 1 << 4;

    /// Creates a direct-eval request with the caller's strictness and a
    /// function-owned variable environment.
    ///
    /// Global and parameter-initializer callsites must select their exact
    /// environment through [`Self::with_variable_environment`].
    #[must_use]
    pub fn new(source: JsString, strict: bool) -> Self {
        Self {
            source,
            scope_index: 1,
            capabilities: if strict { Self::STRICT } else { 0 },
            bindings: Arc::from([]),
            variable_environment: DirectEvalVariableEnvironment::Function,
        }
    }

    /// Attaches the ordered caller-environment snapshot.
    #[must_use]
    pub fn with_bindings(mut self, bindings: Arc<[DirectEvalCallerBinding]>) -> Self {
        self.bindings = bindings;
        self
    }

    /// Retains the verified adjusted lexical-scope operand from the callsite.
    #[must_use]
    pub const fn with_scope_index(mut self, scope_index: u16) -> Self {
        self.scope_index = scope_index;
        self
    }

    /// Attaches the variable environment inherited by sloppy eval code.
    #[must_use]
    pub const fn with_variable_environment(
        mut self,
        variable_environment: DirectEvalVariableEnvironment,
    ) -> Self {
        self.variable_environment = variable_environment;
        self
    }

    /// Selects whether the caller admits `new.target`.
    #[must_use]
    pub const fn with_new_target(mut self, yes: bool) -> Self {
        self.set_capability(Self::NEW_TARGET, yes);
        self
    }

    /// Selects whether the caller admits `super` property access.
    #[must_use]
    pub const fn with_super_property(mut self, yes: bool) -> Self {
        self.set_capability(Self::SUPER_PROPERTY, yes);
        self
    }

    /// Selects whether the caller admits a direct `super()` call.
    #[must_use]
    pub const fn with_super_call(mut self, yes: bool) -> Self {
        self.set_capability(Self::SUPER_CALL, yes);
        self
    }

    /// Selects whether the caller admits the `arguments` identifier.
    #[must_use]
    pub const fn with_arguments_allowed(mut self, yes: bool) -> Self {
        self.set_capability(Self::ARGUMENTS_ALLOWED, yes);
        self
    }

    /// Returns the exact JavaScript source string.
    #[must_use]
    pub const fn source(&self) -> &JsString {
        &self.source
    }

    /// Returns whether caller strictness forces strict eval code.
    #[must_use]
    pub const fn is_strict(&self) -> bool {
        self.has_capability(Self::STRICT)
    }

    /// Returns the verified adjusted lexical-scope operand from the callsite.
    #[must_use]
    pub const fn scope_index(&self) -> u16 {
        self.scope_index
    }

    /// Returns the variable environment inherited by sloppy eval code.
    #[must_use]
    pub const fn variable_environment(&self) -> DirectEvalVariableEnvironment {
        self.variable_environment
    }

    /// Returns caller bindings from innermost to outermost resolution order.
    #[must_use]
    pub fn bindings(&self) -> &[DirectEvalCallerBinding] {
        &self.bindings
    }

    /// Clones the shared caller-environment snapshot.
    #[must_use]
    pub fn shared_bindings(&self) -> Arc<[DirectEvalCallerBinding]> {
        Arc::clone(&self.bindings)
    }

    /// Returns whether `new.target` is meaningful in the caller.
    #[must_use]
    pub const fn allows_new_target(&self) -> bool {
        self.has_capability(Self::NEW_TARGET)
    }

    /// Returns whether the caller admits `super` property access.
    #[must_use]
    pub const fn allows_super_property(&self) -> bool {
        self.has_capability(Self::SUPER_PROPERTY)
    }

    /// Returns whether the caller admits a direct `super()` call.
    #[must_use]
    pub const fn allows_super_call(&self) -> bool {
        self.has_capability(Self::SUPER_CALL)
    }

    /// Returns whether the caller admits the `arguments` identifier.
    #[must_use]
    pub const fn allows_arguments(&self) -> bool {
        self.has_capability(Self::ARGUMENTS_ALLOWED)
    }

    const fn set_capability(&mut self, flag: u8, yes: bool) {
        if yes {
            self.capabilities |= flag;
        } else {
            self.capabilities &= !flag;
        }
    }

    const fn has_capability(&self, flag: u8) -> bool {
        self.capabilities & flag != 0
    }
}

impl IndirectEvalCompileRequest {
    /// Creates an owned indirect-eval request.
    #[must_use]
    pub const fn new(source: JsString) -> Self {
        Self { source }
    }

    /// Returns the exact JavaScript source string.
    #[must_use]
    pub const fn source(&self) -> &JsString {
        &self.source
    }
}

impl DynamicFunctionCompileRequest {
    /// Creates one owned ordinary dynamic-`Function` source request.
    ///
    /// This compatibility constructor defaults to [`DynamicFunctionFamily::Function`].
    #[must_use]
    pub const fn new(parameters: Arc<[JsString]>, body: JsString) -> Self {
        Self::for_family(DynamicFunctionFamily::Function, parameters, body)
    }

    /// Creates one owned request for an explicitly supported family.
    #[must_use]
    pub const fn for_family(
        family: DynamicFunctionFamily,
        parameters: Arc<[JsString]>,
        body: JsString,
    ) -> Self {
        Self {
            family,
            parameters,
            body,
        }
    }

    /// Returns the requested dynamic-function family.
    #[must_use]
    pub const fn family(&self) -> DynamicFunctionFamily {
        self.family
    }

    /// Returns the separately supplied parameter fragments in source order.
    #[must_use]
    pub fn parameters(&self) -> &[JsString] {
        &self.parameters
    }

    /// Returns the separately supplied function-body fragment.
    #[must_use]
    pub const fn body(&self) -> &JsString {
        &self.body
    }
}

/// Host compiler for runtime-created ECMAScript source.
///
/// Implementations must return only a fully verified, immutable bytecode
/// authority. Parsing, semantic analysis, and compilation remain outside the
/// runtime crate, and the request contains no caller lexical environment.
pub trait DynamicFunctionCompiler: Send + Sync + 'static {
    /// Compiles one owned dynamic-function request.
    ///
    /// # Errors
    ///
    /// Returns either an exact JavaScript syntax failure or a shared engine
    /// failure.
    fn compile(
        &self,
        source: DynamicFunctionCompileRequest,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure>;

    /// Compiles one indirect-eval Script.
    ///
    /// The default keeps embedders that provide only dynamic-Function support
    /// fail closed. Engines that expose `%eval%` override this method.
    ///
    /// # Errors
    ///
    /// Returns either an exact JavaScript syntax failure or a shared engine
    /// failure.
    fn compile_indirect_eval(
        &self,
        _source: IndirectEvalCompileRequest,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        Err(DynamicFunctionCompileFailure::Engine {
            source: Arc::new(IndirectEvalCompilerUnavailable),
        })
    }

    /// Compiles one direct-eval Script against caller grammar context.
    ///
    /// The default keeps embedders without direct-eval environment support
    /// fail closed.
    ///
    /// # Errors
    ///
    /// Returns either an exact JavaScript syntax failure or a shared engine
    /// failure.
    fn compile_direct_eval(
        &self,
        _source: DirectEvalCompileRequest,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
        Err(DynamicFunctionCompileFailure::Engine {
            source: Arc::new(DirectEvalCompilerUnavailable),
        })
    }
}

#[derive(Debug)]
struct IndirectEvalCompilerUnavailable;

#[derive(Debug)]
struct DirectEvalCompilerUnavailable;

impl fmt::Display for IndirectEvalCompilerUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the host compiler does not support indirect eval")
    }
}

impl Error for IndirectEvalCompilerUnavailable {}

impl fmt::Display for DirectEvalCompilerUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the host compiler does not support direct eval")
    }
}

impl Error for DirectEvalCompilerUnavailable {}

/// Compatibility name for the pre-generator compiler-service contract.
pub use DynamicFunctionCompiler as OrdinaryDynamicFunctionCompiler;

/// Compatibility name for an ordinary dynamic-Function source request.
pub type OrdinaryDynamicFunctionSource = DynamicFunctionCompileRequest;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quickjs_bytecode::{
        CompilerBindingKind, CompilerBindingPolicy, CompilerInitializationPolicy,
        CompilerWritePolicy, VerifiedBytecode,
    };

    use super::{
        DirectEvalCallerBinding, DirectEvalCallerBindingLocation, DirectEvalCallerBindingScope,
        DirectEvalCompileRequest, DirectEvalVariableEnvironment, DynamicFunctionCompileRequest,
        DynamicFunctionCompiler, DynamicFunctionFamily, IndirectEvalCompileRequest,
    };
    use crate::{JsString, error::DynamicFunctionCompileFailure};

    struct RejectingCompiler;

    impl DynamicFunctionCompiler for RejectingCompiler {
        fn compile(
            &self,
            _source: DynamicFunctionCompileRequest,
        ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure> {
            Err(DynamicFunctionCompileFailure::Syntax {
                message: string("rejected"),
            })
        }
    }

    fn string(value: &str) -> JsString {
        JsString::from_utf8(value).expect("test string")
    }

    #[test]
    fn source_owns_shared_parameters_and_body() {
        let parameters: Arc<[JsString]> = Arc::from([string("left"), string("right")]);
        let body = string("return left + right");
        let source = DynamicFunctionCompileRequest::new(Arc::clone(&parameters), body.clone());
        let clone = source.clone();

        assert_eq!(source.family(), DynamicFunctionFamily::Function);
        assert_eq!(source.parameters(), parameters.as_ref());
        assert_eq!(source.body(), &body);
        assert!(Arc::ptr_eq(&source.parameters, &clone.parameters));
        assert_eq!(clone.body(), &body);
    }

    #[test]
    fn direct_eval_request_retains_verified_caller_context() {
        let source = string("answer");
        let binding = DirectEvalCallerBinding::new(
            string("answer"),
            CompilerBindingPolicy::new(
                CompilerBindingKind::Var,
                CompilerInitializationPolicy::UndefinedAtInstantiation,
                CompilerWritePolicy::Mutable,
                false,
            ),
            DirectEvalCallerBindingLocation::Local(3),
        )
        .with_scope(DirectEvalCallerBindingScope::Variable);
        let bindings: Arc<[DirectEvalCallerBinding]> = Arc::from([binding.clone()]);
        let request = DirectEvalCompileRequest::new(source.clone(), true)
            .with_bindings(Arc::clone(&bindings))
            .with_scope_index(7)
            .with_variable_environment(DirectEvalVariableEnvironment::Global)
            .with_new_target(true)
            .with_super_property(true)
            .with_super_call(true)
            .with_arguments_allowed(true);

        assert_eq!(request.source(), &source);
        assert_eq!(request.bindings(), std::slice::from_ref(&binding));
        assert!(Arc::ptr_eq(&request.shared_bindings(), &bindings));
        assert!(request.is_strict());
        assert_eq!(request.scope_index(), 7);
        assert_eq!(binding.scope(), DirectEvalCallerBindingScope::Variable);
        assert_eq!(
            request.variable_environment(),
            DirectEvalVariableEnvironment::Global
        );
        assert!(request.allows_new_target());
        assert!(request.allows_super_property());
        assert!(request.allows_super_call());
        assert!(request.allows_arguments());
    }

    #[test]
    fn compiler_contract_is_shared_and_object_safe() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}
        assert_send_sync_static::<Arc<dyn DynamicFunctionCompiler>>();

        let compiler: Arc<dyn DynamicFunctionCompiler> = Arc::new(RejectingCompiler);
        let source = DynamicFunctionCompileRequest::for_family(
            DynamicFunctionFamily::GeneratorFunction,
            Arc::from([]),
            string(""),
        );
        let error = compiler.compile(source).expect_err("rejecting compiler");

        assert_eq!(
            error.syntax_message().expect("syntax message"),
            &string("rejected")
        );

        let error = compiler
            .compile_indirect_eval(IndirectEvalCompileRequest::new(string("1")))
            .expect_err("default eval compiler is unavailable");
        assert!(error.engine_source().is_some());

        let error = compiler
            .compile_direct_eval(DirectEvalCompileRequest::new(string("1"), false))
            .expect_err("default direct eval compiler is unavailable");
        assert!(error.engine_source().is_some());
    }
}
