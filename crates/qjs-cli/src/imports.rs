//! Tokio-driven drain of parked dynamic `import()` loads.
//!
//! The engine is synchronous and single-threaded (its GC'd types are not
//! `Send`), so all engine interaction stays on the calling thread. Each
//! round drains queued Promise reaction jobs, takes every currently parked
//! load from the runtime queue, resolves its specifier synchronously through
//! the [`NodeLikeResolver`], and reads the resolved files concurrently on a
//! `current_thread` Tokio runtime — only pure data (paths, file bytes)
//! crosses the await boundary. Loaded roots are then fed back to the engine
//! in FIFO order through [`settle_dynamic_import`], and the loop repeats
//! until no reactions or parked loads remain.

use quickjs::{
    LoadedModuleSource, ModuleEvaluationError, ModuleSourceError, ScriptLimits,
    drain_dynamic_import_jobs, settle_dynamic_import,
};
use quickjs_runtime::{Context, ModuleKey};

use crate::resolver::NodeLikeResolver;

/// Drains parked dynamic `import()` loads to quiescence, reading module
/// sources concurrently on a fresh `current_thread` Tokio runtime.
///
/// Call this after `evaluate_module` (the static graph load itself stays
/// synchronous). Mirrors `quickjs::pump_dynamic_imports` semantics: registry
/// dedup, linking, evaluation, and Promise settlement all happen inside the
/// engine; only the root file reads are async.
///
/// # Errors
///
/// Returns a [`ModuleEvaluationError`] only for internal runtime failures;
/// load, resolution, and compile failures reject their import Promises.
pub(crate) fn drain_pending_imports(
    context: &mut Context<'_>,
    resolver: &mut NodeLikeResolver,
    limits: ScriptLimits,
) -> Result<(), ModuleEvaluationError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|error| {
            ModuleSourceError::new(format!("cannot start the module load runtime: {error}"))
        })?;
    runtime.block_on(drain(context, resolver, limits))
}

async fn drain(
    context: &mut Context<'_>,
    resolver: &mut NodeLikeResolver,
    limits: ScriptLimits,
) -> Result<(), ModuleEvaluationError> {
    loop {
        drain_dynamic_import_jobs(context, limits)?;
        let mut batch = Vec::new();
        while let Some(import) = context.take_pending_dynamic_import() {
            batch.push(import);
        }
        if batch.is_empty() {
            return Ok(());
        }
        // Resolution stays synchronous; only the file reads cross an await.
        let reads = batch
            .iter()
            .map(|import| {
                resolver.resolve_request(&import.specifier(), import.referrer().map(ModuleKey::as_str))
            })
            .map(|resolved| tokio::spawn(async move {
                match resolved {
                    Ok(request) => request.read().await,
                    Err(error) => Err(error),
                }
            }))
            .collect::<Vec<_>>();
        let mut roots: Vec<Result<LoadedModuleSource, ModuleSourceError>> = Vec::with_capacity(reads.len());
        for read in reads {
            roots.push(read.await.unwrap_or_else(|error| {
                Err(ModuleSourceError::new(format!("module read task failed: {error}")))
            }));
        }
        for (import, root) in batch.into_iter().zip(roots) {
            settle_dynamic_import(context, resolver, import, root, limits)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use quickjs::{evaluate_module, evaluate_script};
    use quickjs_runtime::{Runtime, RuntimeLimits};

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "qjs-cli-imports-test-{}-{}-{tag}",
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

    /// Evaluates `source` as a module, drains parked dynamic imports through
    /// the async driver, then runs `probe` as a script (throwing on mismatch).
    fn run_entry(directory: &std::path::Path, source: &str, probe: &str) -> Result<(), String> {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).map_err(|e| e.to_string())?;
        let realm = runtime.create_realm().map_err(|e| e.to_string())?;
        let mut context = runtime.context(&realm).map_err(|e| e.to_string())?;
        let mut resolver = NodeLikeResolver::new(directory.to_path_buf(), Vec::new());
        let root_name = format!("file://{}/entry.mjs", directory.display());
        evaluate_module(&mut context, source, &root_name, &mut resolver, ScriptLimits::default())
            .map_err(|error| format!("evaluate_module: {error}"))?;
        drain_pending_imports(&mut context, &mut resolver, ScriptLimits::default())
            .map_err(|error| format!("drain: {error}"))?;
        evaluate_script(&mut context, probe, "probe.js", ScriptLimits::default())
            .map(|_| ())
            .map_err(|error| format!("probe: {error}"))
    }

    #[test]
    fn drains_concurrent_dynamic_imports_to_quiescence() {
        let directory = temp_dir("batch");
        let _cleanup = Cleanup(directory.clone());
        fs::write(directory.join("a.mjs"), "export const value = 1;").expect("write a");
        fs::write(
            directory.join("b.mjs"),
            "export { value } from './a.mjs';\nexport const extra = 2;",
        )
        .expect("write b");

        run_entry(
            &directory,
            "Promise.all([import('./a.mjs'), import('./b.mjs'), import('node:assert')])\n\
                 .then(([a, b, assert]) => {\n\
                     assert.strictEqual(b.value, 1);\n\
                     globalThis.total = a.value + b.extra;\n\
                 });",
            "if (globalThis.total !== 3) { throw new Error('total ' + globalThis.total); }",
        )
        .expect("concurrent imports settle");
    }

    #[test]
    fn load_failures_reject_the_import_promise() {
        let directory = temp_dir("reject");
        let _cleanup = Cleanup(directory.clone());

        run_entry(
            &directory,
            "import('./missing.mjs').then(\n\
                 () => { globalThis.outcome = 'fulfilled'; },\n\
                 (error) => { globalThis.outcome = 'rejected: ' + error.message; });",
            "if (!globalThis.outcome?.startsWith('rejected: ')) {\n\
                 throw new Error('outcome ' + globalThis.outcome);\n\
             }",
        )
        .expect("missing module rejects");
    }
}
