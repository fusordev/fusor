//! Async gather of the static module graph.
//!
//! The engine is synchronous and single-threaded (its GC'd types are not
//! `Send`), so all engine interaction stays on the calling thread. The gather
//! walks the static graph BFS level by level: parsing requests and resolving
//! specifiers stay synchronous, while the file reads of one level run
//! concurrently as spawned Tokio tasks — only pure data (paths, file bytes)
//! crosses the await boundary, exactly like the dynamic-import drain in
//! [`crate::imports`]. The resulting [`PreloadedModuleEdge`] set feeds
//! [`quickjs::evaluate_preloaded_module_graph`], which compiles, registers,
//! links, and evaluates the graph with the same semantics as the synchronous
//! loader path.

use std::collections::{HashMap, HashSet};

use quickjs::{
    LoadedModuleSource, ModuleEvaluationError, ModuleSourceError, ModuleSourceKind,
    PreloadedModuleEdge, ScriptLimits, module_import_requests,
};

use crate::resolver::{NodeLikeResolver, ResolvedModuleRequest};

/// Where a resolved request's source comes from at this BFS level.
enum Target {
    /// Already materialized (builtin or loaded at an earlier level); the
    /// value is the canonical key.
    Loaded(String),
    /// Read task index into the level's read list; the value is the index.
    Read(usize),
}

/// Loads the static dependency graph below `root_source` (registered under
/// `root_key`), returning one [`PreloadedModuleEdge`] per (referrer,
/// specifier) request occurrence.
///
/// Each BFS level parses its modules' requests with
/// [`module_import_requests`], resolves every specifier synchronously through
/// the [`NodeLikeResolver`], and reads that level's newly discovered files
/// concurrently. Canonical keys dedup across levels, so a diamond dependency
/// is read once and every referrer still gets its own edge.
///
/// # Errors
///
/// Returns the first parse, resolution, or read failure; compile errors
/// surface later from [`quickjs::evaluate_preloaded_module_graph`].
pub(crate) async fn gather_static_graph(
    resolver: &NodeLikeResolver,
    root_source: &str,
    root_key: &str,
    limits: ScriptLimits,
) -> Result<Vec<PreloadedModuleEdge>, ModuleEvaluationError> {
    let mut edges: Vec<PreloadedModuleEdge> = Vec::new();
    let mut loaded: HashMap<String, LoadedModuleSource> = HashMap::new();
    let mut frontier: Vec<(String, String)> = vec![(root_key.to_owned(), root_source.to_owned())];

    while !frontier.is_empty() {
        // Parse this level's requests and resolve each specifier. Both steps
        // are synchronous; only the file reads below cross an await.
        let mut requests: Vec<(String, String, ModuleSourceKind, ResolvedModuleRequest)> =
            Vec::new();
        for (referrer, source) in &frontier {
            for request in module_import_requests(source, limits)? {
                let resolution = resolver.resolve_request(&request.specifier, Some(referrer))?;
                requests.push((
                    referrer.clone(),
                    request.specifier,
                    request.kind,
                    resolution,
                ));
            }
        }

        // Builtins are already materialized by resolution. Spawn one read
        // task per newly discovered file key; repeat references within the
        // level share it.
        let mut metas: Vec<(String, String)> = Vec::with_capacity(requests.len());
        let mut targets: Vec<Target> = Vec::with_capacity(requests.len());
        let mut fresh: Vec<LoadedModuleSource> = Vec::new();
        let mut read_keys: Vec<String> = Vec::new();
        let mut read_tasks: Vec<
            tokio::task::JoinHandle<Result<LoadedModuleSource, ModuleSourceError>>,
        > = Vec::new();
        let mut read_index_by_key: HashMap<String, usize> = HashMap::new();
        let mut fresh_keys: HashSet<String> = HashSet::new();
        let mut kind_by_key: HashMap<String, ModuleSourceKind> = HashMap::new();
        for (referrer, specifier, kind, resolution) in requests {
            let target = match resolution {
                ResolvedModuleRequest::Builtin(source) => {
                    let key = source.key.as_str().to_owned();
                    if !loaded.contains_key(&key) && fresh_keys.insert(key.clone()) {
                        fresh.push(source);
                    }
                    kind_by_key.insert(key.clone(), kind);
                    Target::Loaded(key)
                }
                ResolvedModuleRequest::File { ref canonical, .. } => {
                    kind_by_key.insert(canonical.clone(), kind);
                    if loaded.contains_key(canonical) {
                        Target::Loaded(canonical.clone())
                    } else if let Some(&index) = read_index_by_key.get(canonical) {
                        Target::Read(index)
                    } else {
                        let index = read_tasks.len();
                        read_index_by_key.insert(canonical.clone(), index);
                        read_keys.push(canonical.clone());
                        read_tasks.push(tokio::spawn(async move { resolution.read().await }));
                        Target::Read(index)
                    }
                }
            };
            metas.push((referrer, specifier));
            targets.push(target);
        }

        // Reads run concurrently on the caller's runtime; only file bytes
        // cross the await boundary.
        for task in read_tasks {
            let source = task.await.unwrap_or_else(|error| {
                Err(ModuleSourceError::new(format!(
                    "module read task failed: {error}"
                )))
            })?;
            fresh.push(source);
        }

        // Newly loaded JavaScript modules become the next BFS frontier; a
        // JSON/text module is a leaf with no further requests to parse.
        let mut next: Vec<(String, String)> = Vec::new();
        for source in fresh {
            let kind = kind_by_key
                .get(source.key.as_str())
                .copied()
                .unwrap_or(ModuleSourceKind::JavaScript);
            if kind == ModuleSourceKind::JavaScript {
                next.push((source.key.as_str().to_owned(), source.source.clone()));
            }
            loaded.insert(source.key.as_str().to_owned(), source);
        }

        // Every request occurrence yields an edge, including repeat edges to
        // already-loaded targets (cycle/diamond dedup happens at registration).
        for ((referrer, specifier), target) in metas.into_iter().zip(targets) {
            let key = match target {
                Target::Loaded(key) => key,
                Target::Read(index) => read_keys[index].clone(),
            };
            let source = loaded
                .get(&key)
                .unwrap_or_else(|| unreachable!("target '{key}' loaded above"))
                .clone();
            edges.push(PreloadedModuleEdge {
                referrer,
                specifier,
                source,
            });
        }
        frontier = next;
    }

    Ok(edges)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use quickjs::{evaluate_preloaded_module_graph, evaluate_script};
    use quickjs_runtime::{Runtime, RuntimeLimits};

    use super::*;
    use crate::imports::drain_pending_imports;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "qjs-cli-loader-test-{}-{}-{tag}",
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

    /// Gathers the static graph below `root_name`, evaluates it, drains parked
    /// dynamic imports, then runs `probe` as a script (throwing on mismatch).
    async fn run_entry(
        directory: &Path,
        root_name: &str,
        source: &str,
        probe: &str,
    ) -> Result<(), String> {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).map_err(|e| e.to_string())?;
        let realm = runtime.create_realm().map_err(|e| e.to_string())?;
        let mut context = runtime.context(&realm).map_err(|e| e.to_string())?;
        let mut resolver = NodeLikeResolver::new(directory.to_path_buf(), Vec::new());
        let limits = ScriptLimits::default();
        let edges = gather_static_graph(&resolver, source, root_name, limits)
            .await
            .map_err(|error| format!("gather: {error}"))?;
        evaluate_preloaded_module_graph(&mut context, source, root_name, edges, limits)
            .map_err(|error| format!("evaluate: {error}"))?;
        drain_pending_imports(&mut context, &mut resolver, limits)
            .await
            .map_err(|error| format!("drain: {error}"))?;
        evaluate_script(&mut context, probe, "probe.js", limits)
            .map(|_| ())
            .map_err(|error| format!("probe: {error}"))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gathers_a_diamond_graph_with_single_registration() {
        let directory = temp_dir("diamond");
        let _cleanup = Cleanup(directory.clone());
        // d registers a side-effect counter; a diamond must evaluate it once.
        fs::write(
            directory.join("d.mjs"),
            "globalThis.dEvaluations = (globalThis.dEvaluations ?? 0) + 1;\nexport const d = 1;",
        )
        .expect("write d");
        fs::write(
            directory.join("b.mjs"),
            "import { d } from './d.mjs';\nexport const b = d + 1;",
        )
        .expect("write b");
        fs::write(
            directory.join("c.mjs"),
            "import { d } from './d.mjs';\nexport const c = d + 2;",
        )
        .expect("write c");
        let root = "import { b } from './b.mjs';\n\
                    import { c } from './c.mjs';\n\
                    import assert from 'node:assert';\n\
                    assert.strictEqual(b, 2);\n\
                    globalThis.sum = b + c;";

        let root_name = format!("file://{}/entry.mjs", directory.display());
        run_entry(
            &directory,
            &root_name,
            root,
            "if (globalThis.sum !== 5) { throw new Error('sum ' + globalThis.sum); }\n\
             if (globalThis.dEvaluations !== 1) {\n\
                 throw new Error('d evaluated ' + globalThis.dEvaluations + ' times');\n\
             }",
        )
        .await
        .expect("diamond graph gathers and evaluates once");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gather_reads_each_level_concurrently_and_dedups_reads() {
        let directory = temp_dir("levels");
        let _cleanup = Cleanup(directory.clone());
        fs::write(directory.join("shared.mjs"), "export const s = 1;").expect("write shared");
        // Two modules at the same level import the same new file: one read.
        fs::write(
            directory.join("x.mjs"),
            "import { s } from './shared.mjs';\nexport const x = s + 1;",
        )
        .expect("write x");
        fs::write(
            directory.join("y.mjs"),
            "import { s } from './shared.mjs';\nexport const y = s + 2;",
        )
        .expect("write y");

        let resolver = NodeLikeResolver::new(directory.clone(), Vec::new());
        let root = "import { x } from './x.mjs';\nimport { y } from './y.mjs';";
        let root_name = format!("file://{}/entry.mjs", directory.display());
        let edges = gather_static_graph(&resolver, root, &root_name, ScriptLimits::default())
            .await
            .expect("gather succeeds");
        // 2 root edges + 2 level edges to shared.mjs.
        assert_eq!(edges.len(), 4);
        let shared_key = format!("file://{}/shared.mjs", directory.display());
        let shared_edges = edges
            .iter()
            .filter(|edge| edge.source.key.as_str() == shared_key)
            .count();
        assert_eq!(shared_edges, 2, "both referrers get an edge to shared.mjs");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gather_errors_on_a_missing_dependency() {
        let directory = temp_dir("missing");
        let _cleanup = Cleanup(directory.clone());
        let resolver = NodeLikeResolver::new(directory.clone(), Vec::new());
        let root_name = format!("file://{}/entry.mjs", directory.display());
        let error = gather_static_graph(
            &resolver,
            "import './nope.mjs';",
            &root_name,
            ScriptLimits::default(),
        )
        .await
        .expect_err("missing dependency must error");
        assert!(error.to_string().contains("cannot find module"));
    }
}
