//! Focused ES module linking and evaluation coverage.

use std::collections::HashMap;

use quickjs::{
    LoadedModuleSource, ModuleSourceError, ModuleSourceLoader, ScriptLimits, evaluate_module,
    evaluate_script,
};
use quickjs_runtime::{JsNumber, ModuleKey, Runtime, RuntimeLimits};

/// In-memory loader keyed by exact specifier text.
struct MapLoader {
    sources: HashMap<String, String>,
}

impl MapLoader {
    fn new(entries: &[(&str, &str)]) -> Self {
        Self {
            sources: entries
                .iter()
                .map(|(name, source)| ((*name).to_owned(), (*source).to_owned()))
                .collect(),
        }
    }
}

impl ModuleSourceLoader for MapLoader {
    fn load_module(
        &mut self,
        specifier: &str,
        _referrer: Option<&str>,
    ) -> Result<LoadedModuleSource, ModuleSourceError> {
        let source = self
            .sources
            .get(specifier)
            .ok_or_else(|| ModuleSourceError::new(format!("no module '{specifier}'")))?;
        Ok(LoadedModuleSource {
            key: ModuleKey::new(specifier.into()),
            source: source.clone(),
            display_name: specifier.to_owned(),
        })
    }
}

fn evaluate(root: &str, entries: &[(&str, &str)]) -> Result<(), String> {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let mut loader = MapLoader::new(entries);
    evaluate_module(
        &mut context,
        root,
        "root.mjs",
        &mut loader,
        ScriptLimits::default(),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

#[test]
fn single_module_evaluates_top_level_statements() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let mut loader = MapLoader::new(&[]);
    evaluate_module(
        &mut context,
        "globalThis.witness = 42;",
        "root.mjs",
        &mut loader,
        ScriptLimits::default(),
    )
    .expect("module evaluates");
    let observed = evaluate_script(
        &mut context,
        "globalThis.witness",
        "probe.js",
        ScriptLimits::default(),
    )
    .expect("probe evaluates");
    let number = observed.as_number().expect("live value").expect("Number");
    assert!(number.strict_equals(JsNumber::from_i32(42)));
}

#[test]
fn module_evaluation_propagates_thrown_exceptions() {
    let error = evaluate("throw new TypeError('boom');", &[]).expect_err("module throws");
    assert!(
        error.contains("boom"),
        "expected the thrown message, got: {error}"
    );
}

#[test]
fn named_imports_observe_exporter_bindings() {
    evaluate(
        "import { value } from './dep.mjs';\nif (value !== 7) { throw new Error('value'); }",
        &[("./dep.mjs", "export const value = 7;")],
    )
    .expect("named import links and evaluates");
}

#[test]
fn default_imports_observe_exporter_bindings() {
    evaluate(
        "import value from './dep.mjs';\nif (value !== 9) { throw new Error('default'); }",
        &[("./dep.mjs", "export default 9;")],
    )
    .expect("default import links and evaluates");
}

#[test]
fn namespace_imports_expose_sorted_live_exports() {
    evaluate(
        "import * as ns from './dep.mjs';\n\
         if (ns.b !== 2) { throw new Error('b'); }\n\
         if (ns.a !== 1) { throw new Error('a'); }\n\
         const keys = Object.keys(ns).join(',');\n\
         if (keys !== 'a,b') { throw new Error('keys ' + keys); }",
        &[("./dep.mjs", "export const a = 1; export const b = 2;")],
    )
    .expect("namespace import links and evaluates");
}

#[test]
fn imported_bindings_are_live() {
    evaluate(
        "import { counter, bump } from './dep.mjs';\n\
         if (counter !== 0) { throw new Error('initial'); }\n\
         bump();\n\
         if (counter !== 1) { throw new Error('not live'); }",
        &[(
            "./dep.mjs",
            "export let counter = 0;\nexport function bump() { counter += 1; }",
        )],
    )
    .expect("live bindings observe exporter mutation");
}

#[test]
fn named_re_exports_resolve_through_the_intermediate_module() {
    evaluate(
        "import { value } from './mid.mjs';\nif (value !== 5) { throw new Error('value'); }",
        &[
            ("./mid.mjs", "export { value } from './dep.mjs';"),
            ("./dep.mjs", "export const value = 5;"),
        ],
    )
    .expect("named re-export resolves");
}

#[test]
fn star_re_exports_resolve_through_the_intermediate_module() {
    evaluate(
        "import { value } from './mid.mjs';\nif (value !== 6) { throw new Error('value'); }",
        &[
            ("./mid.mjs", "export * from './dep.mjs';"),
            ("./dep.mjs", "export const value = 6;"),
        ],
    )
    .expect("star re-export resolves");
}

#[test]
fn ambiguous_star_re_exports_fail_to_link() {
    let error = evaluate(
        "import { value } from './mid.mjs';",
        &[
            (
                "./mid.mjs",
                "export * from './a.mjs';\nexport * from './b.mjs';",
            ),
            ("./a.mjs", "export const value = 1;"),
            ("./b.mjs", "export const value = 2;"),
        ],
    )
    .expect_err("ambiguous star export rejects");
    assert!(
        error.contains("ambiguous"),
        "expected an ambiguity link error, got: {error}"
    );
}

#[test]
fn unresolved_imports_fail_to_link() {
    let error = evaluate(
        "import { missing } from './dep.mjs';",
        &[("./dep.mjs", "export const present = 1;")],
    )
    .expect_err("unresolved import rejects");
    assert!(
        error.contains("link") || error.contains("unresolved"),
        "expected a link-phase error, got: {error}"
    );
}

#[test]
fn cycles_link_and_evaluate_with_hoisted_functions() {
    evaluate(
        "import { fromB } from './b.mjs';\nif (fromB() !== 'a+b') { throw new Error('cycle'); }",
        &[
            (
                "./b.mjs",
                "import { fromA } from './root.entry.mjs';\n\
                 export function fromB() { return fromA() + '+b'; }",
            ),
            ("./root.entry.mjs", "export function fromA() { return 'a'; }"),
        ],
    )
    .expect("cyclic graph links and evaluates");
}

#[test]
fn reading_an_uninitialized_imported_binding_throws_reference_error() {
    let error = evaluate(
        "import { late } from './dep.mjs';\n",
        &[(
            "./dep.mjs",
            "import { readLate } from './probe.mjs';\nreadLate();\nexport let late = 1;",
        ), (
            "./probe.mjs",
            "import { late } from './dep.mjs';\nexport function readLate() { return late; }",
        )],
    )
    .expect_err("TDZ read throws");
    assert!(
        error.contains("late") || error.contains("initialized"),
        "expected a TDZ ReferenceError, got: {error}"
    );
}

#[test]
fn namespace_objects_reject_writes_and_have_a_null_prototype() {
    evaluate(
        "import * as ns from './dep.mjs';\n\
         if (Object.getPrototypeOf(ns) !== null) { throw new Error('prototype'); }\n\
         if (ns[Symbol.toStringTag] !== 'Module') { throw new Error('tag'); }\n\
         let threw = false;\n\
         try { ns.a = 5; } catch (e) { threw = e instanceof TypeError; }\n\
         if (!threw) { throw new Error('write did not throw'); }\n\
         if (ns.a !== 1) { throw new Error('mutated'); }",
        &[("./dep.mjs", "export const a = 1;")],
    )
    .expect("namespace object invariants hold");
}

#[test]
fn debug_exporter_runs() {
    evaluate(
        "import { value } from './dep.mjs';\nif (globalThis.depRan !== 1) { throw new Error('dep did not run'); }",
        &[("./dep.mjs", "globalThis.depRan = 1; export const value = 7;")],
    )
    .expect("dep runs first");
}

#[test]
fn debug_exporter_sees_own_binding() {
    evaluate(
        "import { value } from './dep.mjs';\nif (globalThis.seen !== 7) { throw new Error('own read ' + globalThis.seen); }",
        &[(
            "./dep.mjs",
            "export const value = 7; globalThis.seen = value;",
        )],
    )
    .expect("exporter reads its own binding");
}

#[test]
fn debug_module_local_const_read_in_same_module() {
    evaluate("const x = 3; globalThis.out = x;", &[]).expect("module-local const round-trips");
}

#[test]
fn debug_module_local_let_read_in_same_module() {
    evaluate("let x = 3; globalThis.out = x;", &[]).expect("module-local let round-trips");
}

#[test]
fn debug_exported_const_read_in_same_module() {
    evaluate("export const x = 3; globalThis.out = x;", &[])
        .expect("exported const round-trips");
}
