//! Safe, pure-Rust implementation of the `QuickJS` engine.
//!
//! This crate is under active construction. Public APIs are added only when
//! their invariants and observable behavior have executable tests.

#![forbid(unsafe_code)]

/// The official `QuickJS` release whose behavior this port targets.
pub const QUICKJS_COMPATIBILITY_RELEASE: &str = "2026-06-04";

/// The ECMAScript language edition documented by the compatibility release.
pub const ECMASCRIPT_COMPATIBILITY_EDITION: &str = "ES2025";

#[cfg(test)]
mod tests {
    use super::{ECMASCRIPT_COMPATIBILITY_EDITION, QUICKJS_COMPATIBILITY_RELEASE};

    #[test]
    fn compatibility_target_is_explicit() {
        assert_eq!(QUICKJS_COMPATIBILITY_RELEASE, "2026-06-04");
        assert_eq!(ECMASCRIPT_COMPATIBILITY_EDITION, "ES2025");
    }
}
