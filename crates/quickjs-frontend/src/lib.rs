//! Oxc-backed JavaScript front end for the pure-Rust `QuickJS` port.
//!
//! This crate is under active construction. Public APIs are added only when
//! their invariants and observable behavior have executable tests.

#![forbid(unsafe_code)]

mod frontend;

pub use frontend::{
    Allocator, DiagnosticLabel, DiagnosticStage, FrontendDiagnostic, FrontendDiagnosticCode,
    FrontendError, FrontendOptions, FrontendSourceError, ParseMode, Program,
    RegisteredFrontendDiagnostics, RegisteredFrontendError, Span, parse, with_parsed_program,
    with_registered_program,
};

/// The official `QuickJS` release whose behavior this port targets.
pub const QUICKJS_COMPATIBILITY_RELEASE: &str = "2026-06-04";

/// The ECMAScript language edition documented by the compatibility release.
pub const ECMASCRIPT_COMPATIBILITY_EDITION: &str = "ES2025";

#[cfg(test)]
mod tests {
    use super::{
        ECMASCRIPT_COMPATIBILITY_EDITION, FrontendOptions, ParseMode, QUICKJS_COMPATIBILITY_RELEASE,
    };

    #[test]
    fn compatibility_target_is_explicit() {
        assert_eq!(QUICKJS_COMPATIBILITY_RELEASE, "2026-06-04");
        assert_eq!(ECMASCRIPT_COMPATIBILITY_EDITION, "ES2025");
    }

    #[test]
    fn script_is_the_safe_default_parse_goal() {
        assert_eq!(FrontendOptions::default().mode(), ParseMode::Script);
        assert!(!FrontendOptions::default().allows_top_level_return());
    }
}
