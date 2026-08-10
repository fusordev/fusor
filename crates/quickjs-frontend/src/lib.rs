//! Oxc-backed JavaScript front end for the pure-Rust `QuickJS` port.
//!
//! This crate is under active construction. Public APIs are added only when
//! their invariants and observable behavior have executable tests.

#![forbid(unsafe_code)]

mod frontend;
mod module_syntax;
mod string_code_units;

pub use frontend::{
    Allocator, CompilationGoal, DEFAULT_ISOLATED_FRONTEND_STACK_BYTES,
    DEFAULT_MAX_DYNAMIC_FUNCTION_FRAGMENTS, DEFAULT_MAX_DYNAMIC_FUNCTION_ORIGIN_BYTES,
    DEFAULT_MAX_SOURCE_BYTES, DiagnosticLabel, DiagnosticStage, DirectEvalBinding,
    DirectEvalBindingKind, DirectEvalBindingLocation, DirectEvalBindingScope,
    DirectEvalCapabilities, DirectEvalContext, DirectEvalPrivateName, DirectEvalScopeFrame,
    DirectEvalScopeKind, DirectEvalScopeSnapshot, DirectEvalVariableEnvironment,
    DynamicFunctionByteRange, DynamicFunctionError, DynamicFunctionFragmentMap,
    DynamicFunctionFragmentRole, DynamicFunctionKind, DynamicFunctionMapError,
    DynamicFunctionMappedSegment, DynamicFunctionMappedSource, DynamicFunctionPreparationResource,
    DynamicFunctionSource, DynamicFunctionSpanBias, DynamicFunctionSyntheticKind,
    FrontendDiagnostic, FrontendDiagnosticCode, FrontendError, FrontendLimitError, FrontendLimits,
    FrontendOptions, FrontendSourceError, GlobalScriptGoal, IndirectEvalGoal,
    IsolatedFrontendContext, ModuleRecord, ParseMode, ParsedUnit, PreparedDynamicFunctionSource,
    Program, RegisteredFrontendDiagnostics, RegisteredFrontendError, Scoping, Semantic,
    SourceFragment, Span, UnsupportedCompilationGoal, parse, with_dynamic_function_source,
    with_dynamic_function_source_and_prepared, with_parsed_program, with_registered_program,
};
pub use module_syntax::{
    ImportAttribute, ImportAttributeKeyword, ImportAttributes, ModuleExportEntry,
    ModuleExportEntryRole, ModuleExportImportName, ModuleExportLocalName, ModuleExportName,
    ModuleImportEntry, ModuleImportName, ModuleNameSpan, ModuleRequestIndex, ModuleRequestKind,
    ModuleSyntaxRecord, StaticModuleRequest,
};
pub use string_code_units::{OxcStringDecodeError, decode_oxc_cooked_string};

/// The official `QuickJS` release whose behavior this port targets.
pub const QUICKJS_COMPATIBILITY_RELEASE: &str = "2026-06-04";

/// The ECMAScript language edition documented by the compatibility release.
pub const ECMASCRIPT_COMPATIBILITY_EDITION: &str = "ES2025";

#[cfg(test)]
mod tests {
    use super::{
        CompilationGoal, ECMASCRIPT_COMPATIBILITY_EDITION, FrontendOptions, GlobalScriptGoal,
        ParseMode, QUICKJS_COMPATIBILITY_RELEASE,
    };

    #[test]
    fn compatibility_target_is_explicit() {
        assert_eq!(QUICKJS_COMPATIBILITY_RELEASE, "2026-06-04");
        assert_eq!(ECMASCRIPT_COMPATIBILITY_EDITION, "ES2025");
    }

    #[test]
    fn script_is_the_safe_default_parse_goal() {
        assert_eq!(FrontendOptions::default().mode(), ParseMode::Script);
        assert_eq!(
            FrontendOptions::default().goal(),
            CompilationGoal::GlobalScript(GlobalScriptGoal::new())
        );
    }
}
