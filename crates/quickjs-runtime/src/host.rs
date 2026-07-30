//! Dependency-neutral host services used by runtime execution.

use std::sync::Arc;

use quickjs_bytecode::VerifiedBytecode;

use crate::{JsString, error::DynamicFunctionCompileFailure};

/// Owned source fragments for one ordinary dynamic `Function` compilation.
///
/// JavaScript argument coercion happens before this value is created. The
/// compiler therefore receives immutable JavaScript strings without a runtime
/// handle, caller frame, lexical environment, or Oxc-backed lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrdinaryDynamicFunctionSource {
    parameters: Arc<[JsString]>,
    body: JsString,
}

impl OrdinaryDynamicFunctionSource {
    /// Creates one owned ordinary dynamic-Function source request.
    #[must_use]
    pub const fn new(parameters: Arc<[JsString]>, body: JsString) -> Self {
        Self { parameters, body }
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

/// Host compiler for an ordinary dynamic `Function`.
///
/// Implementations must return only a fully verified, immutable bytecode
/// authority. Parsing, semantic analysis, and compilation remain outside the
/// runtime crate, and the request contains no caller lexical environment.
pub trait OrdinaryDynamicFunctionCompiler: Send + Sync + 'static {
    /// Compiles one owned ordinary dynamic-Function request.
    ///
    /// # Errors
    ///
    /// Returns either an exact JavaScript syntax failure or a shared engine
    /// failure. Unsupported generator and asynchronous constructor families
    /// are intentionally outside this ordinary-only contract.
    fn compile(
        &self,
        source: OrdinaryDynamicFunctionSource,
    ) -> Result<Arc<VerifiedBytecode>, DynamicFunctionCompileFailure>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quickjs_bytecode::VerifiedBytecode;

    use super::{OrdinaryDynamicFunctionCompiler, OrdinaryDynamicFunctionSource};
    use crate::{JsString, error::DynamicFunctionCompileFailure};

    struct RejectingCompiler;

    impl OrdinaryDynamicFunctionCompiler for RejectingCompiler {
        fn compile(
            &self,
            _source: OrdinaryDynamicFunctionSource,
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
        let source = OrdinaryDynamicFunctionSource::new(Arc::clone(&parameters), body.clone());
        let clone = source.clone();

        assert_eq!(source.parameters(), parameters.as_ref());
        assert_eq!(source.body(), &body);
        assert!(Arc::ptr_eq(&source.parameters, &clone.parameters));
        assert_eq!(clone.body(), &body);
    }

    #[test]
    fn compiler_contract_is_shared_and_object_safe() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}
        assert_send_sync_static::<Arc<dyn OrdinaryDynamicFunctionCompiler>>();

        let compiler: Arc<dyn OrdinaryDynamicFunctionCompiler> = Arc::new(RejectingCompiler);
        let source = OrdinaryDynamicFunctionSource::new(Arc::from([]), string(""));
        let error = compiler.compile(source).expect_err("rejecting compiler");

        assert_eq!(
            error.syntax_message().expect("syntax message"),
            &string("rejected")
        );
    }
}
