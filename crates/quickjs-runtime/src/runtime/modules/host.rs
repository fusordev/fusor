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

/// Host hook populating per-module `import.meta` properties.
///
/// The runtime creates the `import.meta` object lazily on first access and
/// consults this hook for its observable properties. Both methods have
/// defaults so a host can override only what it needs; with no hook installed
/// the defaults apply: `url` is the canonical module key and `resolve`
/// performs relative resolution against it ([`default_import_meta_resolve`]).
///
/// The hook runs while the interpreter is suspended at a step boundary, so it
/// must not re-enter the runtime; it exists to consult host state such as a
/// resolution cache.
pub trait ImportMetaHook: Send + Sync {
    /// Returns the value exposed as `import.meta.url` for the module.
    fn url(&self, key: &ModuleKey) -> String {
        key.as_str().to_owned()
    }

    /// Resolves an `import.meta.resolve(specifier)` call against the module's
    /// `import.meta.url`.
    ///
    /// # Errors
    ///
    /// Returns a [`ModuleResolveError`] when the specifier cannot be resolved.
    /// The runtime surfaces this as a `TypeError` thrown by `resolve`.
    fn resolve(&self, specifier: &str, referrer_url: &str) -> Result<String, ModuleResolveError> {
        Ok(default_import_meta_resolve(specifier, referrer_url))
    }
}

/// The default `import.meta.resolve` policy: relative-URL resolution against
/// the referrer, treating keys as plain paths.
///
/// Specifiers with a scheme (`scheme://...`) or a leading `/`, and bare
/// specifiers, pass through unchanged; `./` and `../` specifiers join against
/// the referrer's directory with `.` and `..` segments normalized away.
#[must_use]
pub fn default_import_meta_resolve(specifier: &str, referrer_url: &str) -> String {
    let is_relative = specifier == "."
        || specifier == ".."
        || specifier.starts_with("./")
        || specifier.starts_with("../");
    if !is_relative || specifier.contains("://") {
        return specifier.to_owned();
    }
    let base = match referrer_url.rfind('/') {
        Some(index) => &referrer_url[..index],
        None => "",
    };
    let mut segments: Vec<&str> = base
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    for segment in specifier.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    segments.join("/")
}

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
