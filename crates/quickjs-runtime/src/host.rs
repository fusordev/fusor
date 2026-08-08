//! Dependency-neutral host services used by runtime execution.

use std::{error::Error, fmt, sync::Arc};

use quickjs_bytecode::VerifiedBytecode;

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
}

#[derive(Debug)]
struct IndirectEvalCompilerUnavailable;

impl fmt::Display for IndirectEvalCompilerUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the host compiler does not support indirect eval")
    }
}

impl Error for IndirectEvalCompilerUnavailable {}

/// Compatibility name for the pre-generator compiler-service contract.
pub use DynamicFunctionCompiler as OrdinaryDynamicFunctionCompiler;

/// Compatibility name for an ordinary dynamic-Function source request.
pub type OrdinaryDynamicFunctionSource = DynamicFunctionCompileRequest;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quickjs_bytecode::VerifiedBytecode;

    use super::{
        DynamicFunctionCompileRequest, DynamicFunctionCompiler, DynamicFunctionFamily,
        IndirectEvalCompileRequest,
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
    }
}
