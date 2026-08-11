//! Node-like filesystem + builtin module resolution.
//!
//! This is non-normative host sugar on top of the facade's
//! [`ModuleSourceLoader`] boundary:
//!
//! - Referrers and roots are filesystem paths. The CLI passes roots as
//!   `file://<path>` names; dependency referrers arrive as whatever key the
//!   referrer was loaded under.
//! - `./` and `../` specifiers resolve against the referrer's directory;
//!   absolute specifiers are used as-is. Resolution is exact: no extension
//!   guessing beyond one documented fallback — when the resolved path has no
//!   extension and no exact file exists, `<path>.mjs` then `<path>.js` are
//!   probed in that order. Directory specifiers are rejected (no
//!   `index.js`/`package.json` lookup).
//! - `node:<name>` and the bare names in [`builtins::NAMES`] resolve against
//!   the builtin table. Any other bare specifier is rejected (no
//!   `node_modules` lookup).
//!
//! # Canonical keys
//!
//! The issued [`ModuleKey`] is the absolute, lexically normalized path
//! (`.`/`..` resolved without touching symlinks) prefixed with `file://`, or
//! the canonical `node:<name>` specifier for builtins (a bare builtin name
//! like `assert` canonicalizes to `node:assert`). The facade records a
//! (referrer, specifier) resolution edge for every successful load, so the
//! same file imported through two different specifier texts is registered and
//! evaluated exactly once, and one specifier text resolving to different files
//! from different referrers links each referrer to its own file.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use quickjs::{LoadedModuleSource, ModuleSourceError, ModuleSourceLoader};
use quickjs_runtime::ModuleKey;

use crate::builtins;

const FILE_SCHEME: &str = "file://";
const NODE_SCHEME: &str = "node:";

/// Filesystem + builtin [`ModuleSourceLoader`] with Node-like resolution.
pub(crate) struct NodeLikeResolver {
    cwd: PathBuf,
    argv: Vec<String>,
}

impl NodeLikeResolver {
    /// Creates a resolver observing `cwd` (for `node:path`/`node:process`) and
    /// exposing `argv` through `node:process`.
    pub(crate) fn new(cwd: PathBuf, argv: Vec<String>) -> Self {
        Self { cwd, argv }
    }

    fn load_builtin(&self, specifier: &str) -> Option<Result<LoadedModuleSource, ModuleSourceError>> {
        if let Some(name) = specifier.strip_prefix(NODE_SCHEME) {
            return Some(builtin_source(name, &self.cwd, &self.argv));
        }
        if builtins::is_builtin(specifier) {
            return Some(builtin_source(specifier, &self.cwd, &self.argv));
        }
        None
    }

    /// Resolves a non-builtin specifier to a candidate filesystem path without
    /// performing any extension fallback.
    fn resolve_path(&self, specifier: &str, referrer: Option<&str>) -> Result<PathBuf, ModuleSourceError> {
        if specifier.starts_with('/') {
            return Ok(normalize_path(Path::new(specifier)));
        }
        if specifier == "." || specifier == ".." || specifier.starts_with("./") || specifier.starts_with("../") {
            let referrer = referrer.ok_or_else(|| {
                ModuleSourceError::new(format!("relative specifier '{specifier}' has no referrer"))
            })?;
            let Some(referrer_file) = referrer.strip_prefix(FILE_SCHEME) else {
                return Err(ModuleSourceError::new(format!(
                    "relative specifier '{specifier}' cannot resolve against unknown referrer '{referrer}'"
                )));
            };
            let directory = Path::new(referrer_file).parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
            return Ok(normalize_path(&directory.join(specifier)));
        }
        Err(ModuleSourceError::new(format!(
            "unsupported bare specifier '{specifier}' (no node_modules lookup)"
        )))
    }
}

fn builtin_source(
    name: &str,
    cwd: &Path,
    argv: &[String],
) -> Result<LoadedModuleSource, ModuleSourceError> {
    let source = builtins::source(name, cwd, argv)
        .ok_or_else(|| ModuleSourceError::new(format!("no such builtin module '{NODE_SCHEME}{name}'")))?;
    let canonical = format!("{NODE_SCHEME}{name}");
    Ok(LoadedModuleSource {
        key: ModuleKey::new(Arc::from(canonical.as_str())),
        source,
        display_name: canonical,
    })
}

/// Applies the extension fallback and existence checks to a candidate path.
fn resolve_file(candidate: &Path) -> Result<PathBuf, ModuleSourceError> {
    if candidate.is_dir() {
        return Err(directory_error(candidate));
    }
    if candidate.is_file() {
        return Ok(candidate.to_path_buf());
    }
    if candidate.extension().is_none() {
        for extension in ["mjs", "js"] {
            let probed = candidate.with_extension(extension);
            if probed.is_dir() {
                return Err(directory_error(&probed));
            }
            if probed.is_file() {
                return Ok(probed);
            }
        }
    }
    Err(ModuleSourceError::new(format!(
        "cannot find module '{}'",
        candidate.display()
    )))
}

fn directory_error(path: &Path) -> ModuleSourceError {
    ModuleSourceError::new(format!(
        "directory specifier '{}' is unsupported (no index.js lookup)",
        path.display()
    ))
}

/// The canonical `file://` name for a resolved path.
fn canonical_name(path: &Path) -> String {
    format!("{FILE_SCHEME}{}", path.display())
}

/// Lexically resolves `.`/`..` components without touching the filesystem.
///
/// The resolver only feeds absolute paths here (referrers are absolute
/// `file://` keys and joined specifiers stay absolute), so a `..` that would
/// escape the root simply stays at the root.
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

impl ModuleSourceLoader for NodeLikeResolver {
    fn load_module(
        &mut self,
        specifier: &str,
        referrer: Option<&str>,
    ) -> Result<LoadedModuleSource, ModuleSourceError> {
        if let Some(result) = self.load_builtin(specifier) {
            return result;
        }
        let candidate = self.resolve_path(specifier, referrer)?;
        let path = resolve_file(&candidate)?;
        let source = fs::read_to_string(&path).map_err(|error| {
            ModuleSourceError::new(format!("cannot read '{}': {error}", path.display()))
        })?;
        let canonical = canonical_name(&path);
        Ok(LoadedModuleSource {
            key: ModuleKey::new(Arc::from(canonical.as_str())),
            source,
            display_name: canonical,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn resolver() -> NodeLikeResolver {
        NodeLikeResolver::new(PathBuf::from("/unused-cwd"), Vec::new())
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "qjs-cli-test-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).expect("create temp dir");
        directory
    }

    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn normalizes_dot_segments_lexically() {
        assert_eq!(normalize_path(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize_path(Path::new("/a/./b/")), PathBuf::from("/a/b"));
        assert_eq!(normalize_path(Path::new("/../../a")), PathBuf::from("/a"));
        assert_eq!(normalize_path(Path::new("/a/b/c.mjs")), PathBuf::from("/a/b/c.mjs"));
    }

    #[test]
    fn resolves_relative_against_the_referrer_directory() {
        let resolver = resolver();
        let path = resolver
            .resolve_path("./dep.mjs", Some("file:///x/dir/main.mjs"))
            .expect("relative resolution");
        assert_eq!(path, PathBuf::from("/x/dir/dep.mjs"));
        let path = resolver
            .resolve_path("../lib.mjs", Some("file:///x/dir/main.mjs"))
            .expect("parent resolution");
        assert_eq!(path, PathBuf::from("/x/lib.mjs"));
        let path = resolver
            .resolve_path("./a/./b/../c.mjs", Some("file:///x/main.mjs"))
            .expect("dot segment normalization");
        assert_eq!(path, PathBuf::from("/x/a/c.mjs"));
    }

    #[test]
    fn resolves_absolute_specifiers_as_is() {
        let resolver = resolver();
        let path = resolver
            .resolve_path("/a/b/../c.mjs", None)
            .expect("absolute resolution");
        assert_eq!(path, PathBuf::from("/a/c.mjs"));
    }

    #[test]
    fn relative_specifiers_require_a_file_referrer() {
        let resolver = resolver();
        resolver
            .resolve_path("./dep.mjs", None)
            .expect_err("missing referrer must error");
        resolver
            .resolve_path("./dep.mjs", Some("node:assert"))
            .expect_err("non-file referrer must error");
    }

    #[test]
    fn bare_non_builtin_specifiers_error() {
        let resolver = resolver();
        let error = resolver
            .resolve_path("lodash", Some("file:///x/main.mjs"))
            .expect_err("bare specifier must error");
        assert!(error.to_string().contains("unsupported bare specifier"));
    }

    #[test]
    fn node_builtins_use_canonical_node_keys() {
        let mut resolver = resolver();
        let loaded = resolver
            .load_module("node:assert", None)
            .expect("node:assert loads");
        assert_eq!(loaded.key.as_str(), "node:assert");
        assert_eq!(loaded.display_name, "node:assert");
        assert!(loaded.source.contains("strictEqual"));

        // A bare builtin name canonicalizes to its `node:` key.
        let bare = resolver.load_module("assert", None).expect("bare builtin loads");
        assert_eq!(bare.key.as_str(), "node:assert");
        assert_eq!(bare.display_name, "node:assert");

        let path = resolver.load_module("node:path", None).expect("node:path loads");
        assert!(path.source.contains("join"));

        resolver
            .load_module("node:nonexistent", None)
            .expect_err("unknown builtin must error");
    }

    #[test]
    fn process_builtin_embeds_argv_and_cwd() {
        let mut resolver = NodeLikeResolver::new(
            PathBuf::from("/some/cwd"),
            vec!["qjs".to_owned(), "entry.mjs".to_owned()],
        );
        let loaded = resolver
            .load_module("node:process", None)
            .expect("node:process loads");
        assert!(loaded.source.contains(r#"["qjs", "entry.mjs"]"#));
        assert!(loaded.source.contains(r#""/some/cwd""#));
    }

    #[test]
    fn exact_files_load_without_extension_guessing() {
        let directory = temp_dir("exact");
        let _cleanup = Cleanup(directory.clone());
        fs::write(directory.join("dep.mjs"), "export const v = 1;").expect("write dep");

        let mut resolver = resolver();
        let referrer = format!("file://{}/main.mjs", directory.display());
        let loaded = resolver
            .load_module("./dep.mjs", Some(&referrer))
            .expect("exact file loads");
        assert_eq!(loaded.source, "export const v = 1;");
        let expected_name = format!("file://{}/dep.mjs", directory.display());
        assert_eq!(loaded.key.as_str(), expected_name);
        assert_eq!(loaded.display_name, expected_name);
    }

    #[test]
    fn nested_referrers_resolve_through_canonical_keys() {
        let directory = temp_dir("nested");
        let _cleanup = Cleanup(directory.clone());
        fs::create_dir_all(directory.join("lib")).expect("create lib");
        fs::write(directory.join("lib/util.mjs"), "// util").expect("write util");
        fs::write(directory.join("lib/helper.mjs"), "// helper").expect("write helper");

        let mut resolver = resolver();
        let referrer = format!("file://{}/main.mjs", directory.display());
        let util = resolver
            .load_module("./lib/util.mjs", Some(&referrer))
            .expect("util loads");
        // util.mjs was registered under its canonical `file://` key, so its
        // own imports arrive with that key as the referrer.
        let loaded = resolver
            .load_module("./helper.mjs", Some(util.key.as_str()))
            .expect("nested relative load resolves against the canonical key");
        let expected_name = format!("file://{}/lib/helper.mjs", directory.display());
        assert_eq!(loaded.key.as_str(), expected_name);
        assert_eq!(loaded.display_name, expected_name);
    }

    #[test]
    fn same_specifier_text_from_two_referrers_yields_distinct_keys() {
        let directory = temp_dir("conflict");
        let _cleanup = Cleanup(directory.clone());
        fs::create_dir_all(directory.join("a")).expect("create a");
        fs::create_dir_all(directory.join("b")).expect("create b");
        fs::write(directory.join("a/dep.mjs"), "// a").expect("write a dep");
        fs::write(directory.join("b/dep.mjs"), "// b").expect("write b dep");

        let mut resolver = resolver();
        let referrer_a = format!("file://{}/a/main.mjs", directory.display());
        let referrer_b = format!("file://{}/b/main.mjs", directory.display());
        let dep_a = resolver
            .load_module("./dep.mjs", Some(&referrer_a))
            .expect("first load");
        let dep_b = resolver
            .load_module("./dep.mjs", Some(&referrer_b))
            .expect("second load");
        assert_ne!(dep_a.key, dep_b.key);
        assert!(dep_a.key.as_str().ends_with("/a/dep.mjs"));
        assert!(dep_b.key.as_str().ends_with("/b/dep.mjs"));
    }

    #[test]
    fn extension_fallback_prefers_mjs_then_js() {
        let directory = temp_dir("ext");
        let _cleanup = Cleanup(directory.clone());
        fs::write(directory.join("both.mjs"), "// mjs").expect("write mjs");
        fs::write(directory.join("both.js"), "// js").expect("write js");
        fs::write(directory.join("only.js"), "// only js").expect("write only js");

        let mut resolver = resolver();
        let referrer = format!("file://{}/main.mjs", directory.display());
        let loaded = resolver
            .load_module("./both", Some(&referrer))
            .expect("extension fallback loads");
        assert!(loaded.display_name.ends_with("both.mjs"));
        let loaded = resolver
            .load_module("./only", Some(&referrer))
            .expect("js fallback loads");
        assert!(loaded.display_name.ends_with("only.js"));
    }

    #[test]
    fn directory_specifiers_are_rejected() {
        let directory = temp_dir("dir");
        let _cleanup = Cleanup(directory.clone());
        fs::create_dir_all(directory.join("sub")).expect("create subdir");

        let mut resolver = resolver();
        let referrer = format!("file://{}/main.mjs", directory.display());
        let error = resolver
            .load_module("./sub", Some(&referrer))
            .expect_err("directory must error");
        assert!(error.to_string().contains("directory specifier"));
    }

    #[test]
    fn display_names_are_lexically_normalized() {
        let directory = temp_dir("keys");
        let _cleanup = Cleanup(directory.clone());
        fs::create_dir_all(directory.join("sub")).expect("create subdir");
        fs::write(directory.join("dep.mjs"), "// dep").expect("write dep");

        let mut resolver = resolver();
        let referrer = format!("file://{}/sub/main.mjs", directory.display());
        let loaded = resolver
            .load_module("./../dep.mjs", Some(&referrer))
            .expect("normalized load");
        let expected_name = format!("file://{}/dep.mjs", directory.display());
        assert_eq!(loaded.display_name, expected_name);
    }

    #[test]
    fn missing_files_error() {
        let directory = temp_dir("missing");
        let _cleanup = Cleanup(directory.clone());
        let mut resolver = resolver();
        let referrer = format!("file://{}/main.mjs", directory.display());
        let error = resolver
            .load_module("./nope", Some(&referrer))
            .expect_err("missing file must error");
        assert!(error.to_string().contains("cannot find module"));
    }
}
