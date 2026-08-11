//! Host module-loader boundary.
//!
//! The runtime exposes a typed boundary: the facade implements [`ModuleLoader`]
//! to resolve specifiers to canonical [`ModuleKey`]s. The runtime never
//! performs IO. The trait is designed so dynamic `import()` can later emit a
//! host load request event and be completed by an explicit host call.

use crate::ModuleKey;

/// A module resolution failure from the host loader.
#[derive(Clone, Debug)]
pub struct ModuleResolveError {
    message: String,
}

impl ModuleResolveError {
    /// Creates a resolution error with a human-readable message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ModuleResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "module resolution failed: {}", self.message)
    }
}

impl std::error::Error for ModuleResolveError {}

/// Host-supplied module resolution policy.
///
/// The loader receives the specifier text (as UTF-8 bytes) and any import
/// attributes from the module syntax record, plus the referrer's key for
/// relative resolution. It returns a canonical [`ModuleKey`] that the runtime
/// uses to deduplicate module records within a realm.
///
/// Loading (reading source text, parsing, compiling) stays in the facade; the
/// runtime has no parser. The trait is `&mut self` so the host can maintain
/// mutable resolution state (caches, canonicalization maps).
pub trait ModuleLoader {
    /// Resolves a module specifier to a canonical key.
    ///
    /// # Errors
    ///
    /// Returns a [`ModuleResolveError`] when the specifier cannot be resolved.
    /// The runtime surfaces this as a link-time resolution error.
    fn resolve(
        &mut self,
        specifier: &str,
        has_attributes: bool,
        referrer: Option<&ModuleKey>,
    ) -> Result<ModuleKey, ModuleResolveError>;
}
