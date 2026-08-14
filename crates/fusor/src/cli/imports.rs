//! Host-loop-driven drain of parked dynamic `import()` loads.
//!
//! The engine is synchronous and single-threaded (its GC'd types are not
//! `Send`), so all engine interaction happens inside host-loop turns. Each
//! round drains queued Promise reaction jobs and takes the parked batch
//! inside one turn, resolves specifiers synchronously through the
//! [`NodeLikeResolver`] and reads the resolved files concurrently as spawned
//! tasks on the caller's Tokio runtime — only pure data (paths, file bytes)
//! crosses the await boundary — then settles the batch inside the next turn
//! through [`settle_dynamic_import`]. The loop repeats until no parked loads
//! remain.

use std::cell::RefCell;
use std::rc::Rc;

use fusor::{
    LoadedModuleSource, ModuleEvaluationError, ModuleSourceError, ScriptLimits,
    drain_dynamic_import_jobs, settle_dynamic_import,
};
use fusor_host::r#loop::HostLoop;
use fusor_runtime::{ModuleKey, PendingDynamicImport};

use crate::cli::resolver::NodeLikeResolver;

/// Drains parked dynamic `import()` loads to quiescence across host-loop
/// turns, reading module sources concurrently on the caller's Tokio runtime.
///
/// Call this after the static graph has been evaluated inside a turn (see
/// [`crate::cli::loader::gather_static_graph`] and
/// [`fusor::evaluate_preloaded_module_graph`]). Mirrors
/// `fusor::pump_dynamic_imports` semantics: registry dedup, linking,
/// evaluation, and Promise settlement all happen inside the engine; only the
/// root file reads are async.
///
/// # Errors
///
/// Returns a [`ModuleEvaluationError`] only for internal runtime failures;
/// load, resolution, and compile failures reject their import Promises.
pub(crate) async fn drain_pending_imports(
    host_loop: &mut HostLoop,
    resolver: &Rc<RefCell<NodeLikeResolver>>,
    limits: ScriptLimits,
) -> Result<(), ModuleEvaluationError> {
    loop {
        // Phase 1 (turn): drain reaction jobs, then take every parked load.
        let taken: Rc<RefCell<Vec<PendingDynamicImport>>> = Rc::new(RefCell::new(Vec::new()));
        let target = Rc::clone(&taken);
        let drain_failure: Rc<RefCell<Option<ModuleEvaluationError>>> =
            Rc::new(RefCell::new(None));
        let failure = Rc::clone(&drain_failure);
        host_loop.post_event(Box::new(move |context| {
            if let Err(error) = drain_dynamic_import_jobs(context, limits) {
                *failure.borrow_mut() = Some(error);
                return Ok(());
            }
            while let Some(import) = context.take_pending_dynamic_import() {
                target.borrow_mut().push(import);
            }
            Ok(())
        }));
        host_loop
            .run_one_turn()
            .map_err(ModuleEvaluationError::from)?;
        if let Some(error) = drain_failure.replace(None) {
            return Err(error);
        }
        let batch = taken.replace(Vec::new());
        if batch.is_empty() {
            return Ok(());
        }

        // Resolution stays synchronous; only the file reads cross an await.
        let reads = batch
            .iter()
            .map(|import| {
                resolver.borrow().resolve_request(
                    &import.specifier(),
                    import.referrer().map(ModuleKey::as_str),
                )
            })
            .map(|resolved| {
                tokio::spawn(async move {
                    match resolved {
                        Ok(request) => request.read().await,
                        Err(error) => Err(error),
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut roots: Vec<Result<LoadedModuleSource, ModuleSourceError>> =
            Vec::with_capacity(reads.len());
        for read in reads {
            roots.push(read.await.unwrap_or_else(|error| {
                Err(ModuleSourceError::new(format!(
                    "module read task failed: {error}"
                )))
            }));
        }

        // Phase 2 (turn): settle the batch in FIFO order.
        let loader = Rc::clone(resolver);
        let settle_failure: Rc<RefCell<Option<ModuleEvaluationError>>> =
            Rc::new(RefCell::new(None));
        let failure = Rc::clone(&settle_failure);
        host_loop.post_event(Box::new(move |context| {
            for (import, root) in batch.into_iter().zip(roots) {
                if let Err(error) =
                    settle_dynamic_import(context, &mut *loader.borrow_mut(), import, root, limits)
                {
                    *failure.borrow_mut() = Some(error);
                    break;
                }
            }
            Ok(())
        }));
        host_loop
            .run_one_turn()
            .map_err(ModuleEvaluationError::from)?;
        if let Some(error) = settle_failure.replace(None) {
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        fs,
        path::PathBuf,
        rc::Rc,
        sync::atomic::{AtomicU64, Ordering},
    };

    use fusor::{evaluate_preloaded_module_graph, evaluate_script};
    use fusor_host::overlay::HostRuntime;

    use super::*;
    use crate::cli::loader::gather_static_graph;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "fusor-cli-imports-test-{}-{}-{tag}",
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

    /// Gathers and evaluates `source` as a module inside a host-loop turn,
    /// drains parked dynamic imports through the loop-driven driver, then
    /// runs `probe` as a script (throwing on mismatch) inside a final turn.
    async fn run_entry(
        directory: &std::path::Path,
        source: &str,
        probe: &str,
    ) -> Result<(), String> {
        let mut host = HostRuntime::builder().build().map_err(|e| e.to_string())?;
        let mut host_loop = host.into_loop().map_err(|e| e.to_string())?;
        let resolver = Rc::new(RefCell::new(NodeLikeResolver::new(
            directory.to_path_buf(),
            Vec::new(),
        )));
        let limits = ScriptLimits::default();
        let root_name = format!("file://{}/entry.mjs", directory.display());
        let edges = gather_static_graph(&resolver.borrow(), source, &root_name, limits)
            .await
            .map_err(|error| format!("gather: {error}"))?;
        let source = source.to_owned();
        let name = root_name.clone();
        let evaluate_outcome: Rc<RefCell<Option<Result<(), ModuleEvaluationError>>>> =
            Rc::new(RefCell::new(None));
        let outcome = Rc::clone(&evaluate_outcome);
        host_loop.post_event(Box::new(move |context| {
            let result = evaluate_preloaded_module_graph(context, &source, &name, edges, limits)
                .map(|_| ());
            *outcome.borrow_mut() = Some(result);
            Ok(())
        }));
        host_loop
            .run_one_turn()
            .map_err(|error| format!("evaluate turn: {error}"))?;
        evaluate_outcome
            .replace(None)
            .expect("evaluation completed")
            .map_err(|error| format!("evaluate: {error}"))?;
        drain_pending_imports(&mut host_loop, &resolver, limits)
            .await
            .map_err(|error| format!("drain: {error}"))?;

        let probe = probe.to_owned();
        let probe_outcome: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
        let outcome = Rc::clone(&probe_outcome);
        host_loop.post_event(Box::new(move |context| {
            let result = evaluate_script(context, &probe, "probe.js", limits)
                .map(|_| ())
                .map_err(|error| error.to_string());
            *outcome.borrow_mut() = Some(result);
            Ok(())
        }));
        host_loop
            .run_one_turn()
            .map_err(|error| format!("probe turn: {error}"))?;
        probe_outcome.replace(None).expect("probe completed")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drains_concurrent_dynamic_imports_to_quiescence() {
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
        .await
        .expect("concurrent imports settle");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_failures_reject_the_import_promise() {
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
        .await
        .expect("missing module rejects");
    }
}
