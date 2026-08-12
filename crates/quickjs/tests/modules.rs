//! Focused ES module linking and evaluation coverage.

use std::collections::HashMap;

use quickjs::{
    LoadedModuleSource, ModuleSourceError, ModuleSourceLoader, ScriptLimits, evaluate_module,
    evaluate_script, pump_dynamic_imports,
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

#[test]
fn top_level_arrows_compile_and_evaluate() {
    evaluate(
        "const identity = () => 42;\n\
         const add = (a, b) => a + b;\n\
         if (identity() !== 42) { throw new Error('identity'); }\n\
         if (add(identity(), 8) !== 50) { throw new Error('add'); }",
        &[],
    )
    .expect("top-level arrows compile and evaluate");
}

#[test]
fn arrows_capture_module_local_and_imported_bindings() {
    evaluate(
        "import { value } from './dep.mjs';\n\
         const local = 3;\n\
         const read = () => value + local;\n\
         if (read() !== 10) { throw new Error('read ' + read()); }",
        &[("./dep.mjs", "export const value = 7;")],
    )
    .expect("arrows capture module-local and imported bindings");
}

#[test]
fn top_level_arrow_this_is_undefined() {
    evaluate(
        "const read = () => this;\n\
         if (read() !== undefined) { throw new Error('this'); }",
        &[],
    )
    .expect("a module top-level arrow reads undefined as this");
}

#[test]
fn arrows_inside_module_functions_bind_the_function_receiver() {
    evaluate(
        "function make() { return () => this; }\n\
         const receiver = { marker: 7 };\n\
         const read = make.call(receiver);\n\
         if (read() !== receiver) { throw new Error('receiver'); }\n\
         function target() { return (() => new.target)(); }\n\
         if (target() !== undefined) { throw new Error('new.target'); }",
        &[],
    )
    .expect("arrows inside module functions resolve this and new.target lexically");
}

#[test]
fn async_arrows_in_modules_fulfill_promises() {
    evaluate_dynamic(
        "const read = async () => 5;\n\
         read().then((value) => { globalThis.asyncValue = value; });",
        &[],
        "if (globalThis.asyncValue !== 5) { throw new Error('value ' + globalThis.asyncValue); }",
    )
    .expect("async arrows in modules fulfill promises");
}

#[test]
fn classes_with_instance_fields_and_constructors_compile_in_modules() {
    evaluate(
        "class Point {\n\
             x = 1;\n\
             constructor() { this.y = 2; }\n\
         }\n\
         const point = new Point();\n\
         if (point.x !== 1 || point.y !== 2) { throw new Error('point'); }\n\
         class OnlyFields { value = 3; }\n\
         if (new OnlyFields().value !== 3) { throw new Error('default constructor'); }",
        &[],
    )
    .expect("classes with fields and constructors compile in modules");
}

#[test]
fn named_default_class_exports_compile_in_modules() {
    evaluate(
        "import Exported from './dep.mjs';\n\
         const instance = new Exported();\n\
         if (instance.marker !== 9) { throw new Error('marker'); }\n\
         if (Exported.name !== 'Named') { throw new Error('name ' + Exported.name); }",
        &[(
            "./dep.mjs",
            "export default class Named {\n\
                 marker = 9;\n\
             }",
        )],
    )
    .expect("named export default class compiles and links");
}

#[test]
fn anonymous_default_class_exports_compile_in_modules() {
    evaluate(
        "import Exported from './dep.mjs';\n\
         const instance = new Exported();\n\
         if (instance.marker !== 9) { throw new Error('marker'); }\n\
         if (Exported.name !== 'default') { throw new Error('name ' + Exported.name); }",
        &[(
            "./dep.mjs",
            "export default class {\n\
                 marker = 9;\n\
             }",
        )],
    )
    .expect("anonymous export default class compiles and links");
}

#[test]
fn anonymous_default_class_exports_with_heritage_compile_in_modules() {
    evaluate(
        "import Exported from './dep.mjs';\n\
         const instance = new Exported();\n\
         if (instance.tag !== 4) { throw new Error('tag'); }\n\
         if (Exported.name !== 'default') { throw new Error('name ' + Exported.name); }",
        &[(
            "./dep.mjs",
            "const Base = class {\n\
                 constructor() { this.tag = 4; }\n\
             };\n\
             export default class extends Base {}",
        )],
    )
    .expect("anonymous export default class with heritage compiles and links");
}

#[test]
fn parenthesized_default_class_expressions_compile_in_modules() {
    evaluate(
        "import Exported from './dep.mjs';\n\
         const instance = new Exported();\n\
         if (instance.valueOf() !== 45) { throw new Error('valueOf'); }\n\
         if (Exported.name !== 'default') { throw new Error('name ' + Exported.name); }",
        &[(
            "./dep.mjs",
            "export default (class { valueOf() { return 45; } });",
        )],
    )
    .expect("parenthesized default class expression compiles and links");
}

#[test]
fn anonymous_default_class_export_rejects_duplicate_constructors() {
    evaluate(
        "import Exported from './dep.mjs';\n\
         new Exported();",
        &[(
            "./dep.mjs",
            "export default class {\n\
                 constructor() {}\n\
                 constructor() {}\n\
             }",
        )],
    )
    .expect_err("duplicate constructors are an early error");
}

#[test]
fn import_meta_is_an_identity_stable_object() {
    evaluate(
        "const a = import.meta;\n\
         const b = import.meta;\n\
         if (typeof a !== 'object' || a === null) { throw new Error('not an object'); }\n\
         if (a !== b) { throw new Error('not identity-stable'); }\n\
         if (Object.getPrototypeOf(a) !== Object.prototype) { throw new Error('prototype'); }",
        &[],
    )
    .expect("import.meta is one identity-stable ordinary object per module");
}

#[test]
fn import_meta_url_is_the_module_key() {
    evaluate(
        "if (import.meta.url !== 'root.mjs') { throw new Error('url ' + import.meta.url); }",
        &[],
    )
    .expect("import.meta.url reflects the canonical module key");
}

#[test]
fn import_meta_resolve_resolves_relative_to_the_module() {
    evaluate(
        "if (import.meta.resolve('./x.js') !== 'x.js') { throw new Error(import.meta.resolve('./x.js')); }",
        &[],
    )
    .expect("import.meta.resolve resolves relative to the module key");
}

#[test]
fn import_meta_is_per_module_and_resolve_uses_the_referrer() {
    evaluate(
        "import './dep.mjs';\n\
         if (globalThis.depMeta === import.meta) { throw new Error('meta object shared'); }\n\
         if (globalThis.depMeta.url !== './dep.mjs') { throw new Error('dep url ' + globalThis.depMeta.url); }\n\
         if (globalThis.depResolved !== './x.js') { throw new Error('dep resolve ' + globalThis.depResolved); }",
        &[(
            "./dep.mjs",
            "globalThis.depMeta = import.meta;\n\
             globalThis.depResolved = import.meta.resolve('./x.js');",
        )],
    )
    .expect("each module gets its own import.meta with its own referrer");
}

#[test]
fn import_meta_is_visible_inside_module_functions() {
    evaluate(
        "function read() { return import.meta.url; }\n\
         if (read() !== 'root.mjs') { throw 1; }",
        &[],
    )
    .expect("import.meta resolves to the owning module inside nested functions");
}

#[test]
fn hoisted_module_functions_resolve_realm_globals() {
    evaluate(
        "function check() {\n\
             if (typeof Error !== 'function') { throw new Error('Error'); }\n\
             if (typeof JSON.parse !== 'function') { throw new Error('JSON'); }\n\
             if (globalThis.Math !== Math) { throw new Error('globalThis'); }\n\
             return Object.prototype.toString.call(Math).length > 0;\n\
         }\n\
         if (!check()) { throw new Error('check'); }",
        &[],
    )
    .expect("hoisted module functions resolve realm globals");
}

#[test]
fn hoisted_module_functions_capture_imported_bindings() {
    evaluate(
        "import { value } from './dep.mjs';\n\
         function read() { return value; }\n\
         if (read() !== 7) { throw new Error('value'); }",
        &[("./dep.mjs", "export const value = 7;")],
    )
    .expect("hoisted module functions capture imported bindings");
}

#[test]
fn nested_functions_capture_module_local_lexical_bindings() {
    evaluate(
        "let count = 1;\n\
         function outer() {\n\
             function inner() { return count + 1; }\n\
             return inner();\n\
         }\n\
         if (outer() !== 2) { throw new Error('count'); }",
        &[],
    )
    .expect("nested functions capture module-local let bindings");
}

#[test]
fn import_meta_inside_a_function_that_references_globals() {
    evaluate(
        "function read() {\n\
             try {\n\
                 throw new Error('boom');\n\
             } catch (error) {\n\
                 if (error.message !== 'boom') { throw new Error('message'); }\n\
             }\n\
             return import.meta.url;\n\
         }\n\
         if (read() !== 'root.mjs') { throw new Error('url'); }",
        &[],
    )
    .expect("import.meta resolves inside functions that also reference realm globals");
}

#[test]
fn import_meta_in_a_script_is_a_syntax_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let error = evaluate_script(
        &mut context,
        "const m = import.meta;",
        "probe.js",
        ScriptLimits::default(),
    );
    assert!(
        error.is_err(),
        "import.meta outside a module must be a syntax error"
    );
}

// ---- Dynamic import() through the host-load boundary ----

/// Evaluates `root` as a module, pumps every parked dynamic import to
/// quiescence, then evaluates `probe` as a Script. The probe throws on any
/// assertion failure.
fn evaluate_dynamic(root: &str, entries: &[(&str, &str)], probe: &str) -> Result<(), String> {
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
    .map_err(|error| format!("evaluate_module: {error}"))?;
    pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
        .map_err(|error| format!("pump: {error}"))?;
    evaluate_script(&mut context, probe, "probe.js", ScriptLimits::default())
        .map(|_| ())
        .map_err(|error| format!("probe: {error}"))
}

#[test]
fn dynamic_import_fulfills_with_namespace_and_live_bindings() {
    evaluate_dynamic(
        "import('./dep.mjs').then(function (ns) { globalThis.ns = ns; globalThis.named = ns.value; globalThis.def = ns.default; });",
        &[(
            "./dep.mjs",
            "export const value = 7;\n\
             export default 9;\n\
             export let counter = 0;\n\
             export function bump() { counter += 1; }",
        )],
        "if (globalThis.named !== 7) { throw new Error('named ' + globalThis.named); }\n\
         if (globalThis.def !== 9) { throw new Error('default ' + globalThis.def); }\n\
         if (globalThis.ns.counter !== 0) { throw new Error('initial'); }\n\
         globalThis.ns.bump();\n\
         if (globalThis.ns.counter !== 1) { throw new Error('not live'); }\n\
         if (Object.getPrototypeOf(globalThis.ns) !== null) { throw new Error('prototype'); }",
    )
    .expect("dynamic import fulfills with the namespace object");
}

#[test]
fn dynamic_import_from_a_script_has_no_referrer_module() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let mut loader = MapLoader::new(&[("./dep.mjs", "export const value = 11;")]);
    evaluate_script(
        &mut context,
        "import('./dep.mjs').then((ns) => { globalThis.value = ns.value; });",
        "entry.js",
        ScriptLimits::default(),
    )
    .expect("script evaluates");
    pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
        .expect("pump completes");
    evaluate_script(
        &mut context,
        "if (globalThis.value !== 11) { throw new Error('value ' + globalThis.value); }",
        "probe.js",
        ScriptLimits::default(),
    )
    .expect("script dynamic import settles");
}

#[test]
fn dynamic_import_rejects_for_a_missing_module() {
    evaluate_dynamic(
        "import('./missing.mjs').then(\n\
             function () { globalThis.settled = 'fulfilled'; },\n\
             function (error) { globalThis.settled = 'rejected'; globalThis.reason = String(error); });",
        &[],
        "if (globalThis.settled !== 'rejected') { throw new Error('settled ' + globalThis.settled); }\n\
         if (!globalThis.reason.includes('missing.mjs')) { throw new Error('reason ' + globalThis.reason); }",
    )
    .expect("a load failure rejects the import promise");
}

#[test]
fn dynamic_import_rejects_unsupported_attributes() {
    evaluate_dynamic(
        "import('./dep.mjs', { with: { type: 'json' } }).then(\n\
             function () { globalThis.settled = 'fulfilled'; },\n\
             function (error) { globalThis.settled = 'rejected'; globalThis.reason = String(error); });",
        &[("./dep.mjs", "export const value = 1;")],
        "if (globalThis.settled !== 'rejected') { throw new Error('settled ' + globalThis.settled); }\n\
         if (!globalThis.reason.includes('unsupported dynamic import attribute')) { throw new Error('reason ' + globalThis.reason); }",
    )
    .expect("non-empty attributes still reject before the host boundary");
}

#[test]
fn dynamic_import_rejects_with_the_evaluation_exception() {
    evaluate_dynamic(
        "import('./boom.mjs').then(\n\
             function () { globalThis.settled = 'fulfilled'; },\n\
             function (error) {\n\
                 globalThis.settled = 'rejected';\n\
                 globalThis.isTypeError = error instanceof TypeError;\n\
                 globalThis.reason = error.message; });",
        &[("./boom.mjs", "throw new TypeError('boom-eval');")],
        "if (globalThis.settled !== 'rejected') { throw new Error('settled ' + globalThis.settled); }\n\
         if (globalThis.isTypeError !== true) { throw new Error('error class'); }\n\
         if (globalThis.reason !== 'boom-eval') { throw new Error('reason ' + globalThis.reason); }",
    )
    .expect("an evaluation failure rejects with the original exception");
}

#[test]
fn dynamic_import_rejects_with_a_syntax_error_for_a_link_failure() {
    evaluate_dynamic(
        "import('./bad.mjs').then(\n\
             function () { globalThis.settled = 'fulfilled'; },\n\
             function (error) {\n\
                 globalThis.settled = 'rejected';\n\
                 globalThis.isSyntaxError = error instanceof SyntaxError;\n\
                 globalThis.reason = String(error); });",
        &[
            ("./bad.mjs", "import { missing } from './dep.mjs';"),
            ("./dep.mjs", "export const present = 1;"),
        ],
        "if (globalThis.settled !== 'rejected') { throw new Error('settled ' + globalThis.settled); }\n\
         if (globalThis.isSyntaxError !== true) { throw new Error('error class'); }",
    )
    .expect("a link failure rejects with a SyntaxError");
}

#[test]
fn concurrent_dynamic_imports_all_settle() {
    evaluate_dynamic(
        "Promise.all([import('./a.mjs'), import('./b.mjs')])\n\
             .then(function (pair) { globalThis.sum = pair[0].value + pair[1].value; });",
        &[
            ("./a.mjs", "export const value = 20;"),
            ("./b.mjs", "export const value = 22;"),
        ],
        "if (globalThis.sum !== 42) { throw new Error('sum ' + globalThis.sum); }",
    )
    .expect("concurrent dynamic imports all settle");
}

#[test]
fn repeated_dynamic_imports_share_one_module_instance() {
    evaluate_dynamic(
        "Promise.all([import('./dep.mjs'), import('./dep.mjs')])\n\
             .then(function (pair) { globalThis.same = pair[0] === pair[1]; globalThis.value = pair[0].value; });",
        &[(
            "./dep.mjs",
            "globalThis.evalCount = (globalThis.evalCount || 0) + 1;\nexport const value = 3;",
        )],
        "if (globalThis.same !== true) { throw new Error('namespace identity'); }\n\
         if (globalThis.value !== 3) { throw new Error('value ' + globalThis.value); }\n\
         if (globalThis.evalCount !== 1) { throw new Error('evaluated ' + globalThis.evalCount); }",
    )
    .expect("the registry deduplicates repeated imports of one module");
}

#[test]
fn dynamic_import_of_a_statically_imported_module_reuses_the_record() {
    evaluate_dynamic(
        "import { value } from './dep.mjs';\n\
         globalThis.staticValue = value;\n\
         import('./dep.mjs').then(function (ns) { globalThis.dynamicValue = ns.value; });",
        &[(
            "./dep.mjs",
            "globalThis.evalCount = (globalThis.evalCount || 0) + 1;\nexport const value = 5;",
        )],
        "if (globalThis.staticValue !== 5) { throw new Error('static ' + globalThis.staticValue); }\n\
         if (globalThis.dynamicValue !== 5) { throw new Error('dynamic ' + globalThis.dynamicValue); }\n\
         if (globalThis.evalCount !== 1) { throw new Error('evaluated ' + globalThis.evalCount); }",
    )
    .expect("a dynamic import of an evaluated module reuses its record");
}

#[test]
fn dynamic_import_cycle_back_into_the_referrer_settles() {
    evaluate_dynamic(
        "export function fromRoot() { return 'root'; }\n\
         import('./a.mjs').then(function (ns) { globalThis.cycleResult = ns.fromA(); });",
        &[
            (
                "./a.mjs",
                "import { fromRoot } from 'root.mjs';\n\
                 export function fromA() { return fromRoot() + '+a'; }",
            ),
            ("root.mjs", ""),
        ],
        "if (globalThis.cycleResult !== 'root+a') { throw new Error('cycle ' + globalThis.cycleResult); }",
    )
    .expect("a dynamic import cycle back into the referrer settles");
}

#[test]
fn dynamic_import_of_a_top_level_await_module_fulfills_with_the_namespace() {
    // The compiler admits top-level await and compiles the module root as an
    // async function, so loading succeeds and the pump drives the async root:
    // once the asynchronous evaluation completes, the import promise fulfills
    // with the evaluated namespace (ECMA-262 FinishDynamicImport waiting on
    // the module's [[TopLevelCapability]]).
    evaluate_dynamic(
        "import('./tla.mjs').then(\n\
             function (ns) { globalThis.settled = 'fulfilled'; globalThis.imported = ns.value; },\n\
             function (error) { globalThis.settled = 'rejected'; globalThis.reason = String(error); });",
        &[("./tla.mjs", "export const value = await Promise.resolve(1);")],
        "if (globalThis.settled !== 'fulfilled') { throw new Error('settled ' + globalThis.settled); }\n\
         if (globalThis.imported !== 1) { throw new Error('imported ' + globalThis.imported); }",
    )
    .expect("a TLA module fulfills the import promise with its namespace");
}

// ---- Top-level await: async module continuations ----

#[test]
fn top_level_await_completes_after_job_drain() {
    evaluate_dynamic(
        "const x = await Promise.resolve(41);\nglobalThis.x = x + 1;",
        &[],
        "if (globalThis.x !== 42) { throw new Error('x ' + globalThis.x); }",
    )
    .expect("a top-level await root completes once jobs drain");
}

#[test]
fn top_level_await_in_heritage_iteration_heads_and_destructuring_evaluates() {
    // Grammar positions that also host module top-level await: class heritage
    // (evaluated in the enclosing scope), iteration heads declaring
    // module-local `var` bindings, and destructuring declarations.
    evaluate_dynamic(
        "function fn(v) { return class { static tag = v; }; }\n\
         class C extends fn(await Promise.resolve(7)) {}\n\
         globalThis.tag = C.tag;\n\
         var iter;\n\
         for (iter of [await Promise.resolve(1)]) {}\n\
         for (var iter2 in { a: await Promise.resolve(2) }) {}\n\
         var seen = 0;\n\
         for await (var iter3 of [1, 2]) { seen += iter3; }\n\
         globalThis.seen = seen;\n\
         var { d = await Promise.resolve(5) } = {};\n\
         globalThis.d = d;",
        &[],
        "if (globalThis.tag !== 7) { throw new Error('tag ' + globalThis.tag); }\n\
         if (globalThis.seen !== 3) { throw new Error('seen ' + globalThis.seen); }\n\
         if (globalThis.d !== 5) { throw new Error('d ' + globalThis.d); }",
    )
    .expect("heritage, iteration-head, and destructuring top-level awaits evaluate");
}

#[test]
fn sync_importer_waits_for_async_dependency() {
    // The root has no top-level await of its own, but it depends on an async
    // module: its execution is deferred ([[PendingAsyncDependencies]]) until
    // the dependency fulfills, then runs synchronously in the same pass
    // (GatherAvailableAncestors + AsyncModuleExecutionFulfilled).
    evaluate_dynamic(
        "import { v } from './dep.mjs';\nglobalThis.vAtEval = v;",
        &[(
            "./dep.mjs",
            "export let v = 0;\nawait Promise.resolve();\nv = 7;",
        )],
        "if (globalThis.vAtEval !== 7) { throw new Error('v ' + globalThis.vAtEval); }",
    )
    .expect("a sync importer executes only after its async dependency completes");
}

#[test]
fn top_level_await_rejection_records_evaluation_error() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let mut loader = MapLoader::new(&[]);
        evaluate_module(
            &mut context,
            "await Promise.reject(new Error('boom'));",
            "root.mjs",
            &mut loader,
            ScriptLimits::default(),
        )
        .expect("a rejecting TLA module still evaluates asynchronously");
        pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
            .expect("pump drains the rejection continuation");
    }
    let error = runtime
        .module_evaluation_error(&realm, &ModuleKey::new("root.mjs".into()))
        .expect("the module records its evaluation error");
    assert!(
        error.message().contains("boom"),
        "expected the rejection message, got: {}",
        error.message()
    );
}

#[test]
fn tla_cycle_defers_sync_member_until_async_evaluation_completes() {
    // Cycle A (root, sync) <-> B (top-level await). B starts its asynchronous
    // execution in the initial pass; A counts the on-stack B as a pending
    // async dependency (ECMA-262 16.6.1.4 step 12.e) and executes only after
    // B's evaluation completes, observing B's post-await binding value.
    evaluate_dynamic(
        "import { bValue } from './b.mjs';\n\
         export let aValue = 1;\n\
         globalThis.order.push('A');\n\
         globalThis.aSawB = bValue;",
        &[
            (
                "./b.mjs",
                "import { aValue } from 'root.mjs';\n\
                 globalThis.order = ['B'];\n\
                 export let bValue = 0;\n\
                 await Promise.resolve();\n\
                 bValue = 7;",
            ),
            ("root.mjs", ""),
        ],
        "if (globalThis.order.join(',') !== 'B,A') { throw new Error('order ' + globalThis.order.join(',')); }\n\
         if (globalThis.aSawB !== 7) { throw new Error('aSawB ' + globalThis.aSawB); }",
    )
    .expect("the sync cycle member executes after the async member completes");
}

#[test]
fn tla_cycle_sync_prefix_sees_uninitialized_ancestor_bindings() {
    // B's synchronous prefix runs while A (the cycle root) is still deferred,
    // so reading A's uninitialized binding hits the TDZ and errors the cycle.
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    {
        let mut context = runtime.context(&realm).expect("context");
        let mut loader = MapLoader::new(&[
            (
                "./b.mjs",
                "import { aValue } from 'root.mjs';\n\
                 globalThis.seen = aValue;\n\
                 await Promise.resolve();",
            ),
            ("root.mjs", ""),
        ]);
        evaluate_module(
            &mut context,
            "import './b.mjs';\nexport let aValue = 1;",
            "root.mjs",
            &mut loader,
            ScriptLimits::default(),
        )
        .expect("the cycle evaluates asynchronously");
        pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
            .expect("pump drains the rejection continuation");
    }
    let error = runtime
        .module_evaluation_error(&realm, &ModuleKey::new("root.mjs".into()))
        .expect("the TDZ failure errors the whole cycle");
    assert!(
        error.message().contains("aValue") || error.message().contains("initialized"),
        "expected a TDZ ReferenceError, got: {}",
        error.message()
    );
}

#[test]
fn dynamic_import_of_tla_module_fulfills_after_evaluation() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let mut loader = MapLoader::new(&[(
        "./tla.mjs",
        "globalThis.stage = 'started';\n\
         await globalThis.gate;\n\
         globalThis.stage = 'finished';\n\
         export const value = 5;",
    )]);
    evaluate_module(
        &mut context,
        "globalThis.gate = new Promise((resolve) => { globalThis.open = resolve; });\n\
         import('./tla.mjs').then(\n\
             function (ns) { globalThis.settled = globalThis.stage; globalThis.imported = ns.value; },\n\
             function (error) { globalThis.settled = 'rejected'; globalThis.reason = String(error); });",
        "root.mjs",
        &mut loader,
        ScriptLimits::default(),
    )
    .expect("module evaluates");
    pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
        .expect("first pump drives the async root to its await");
    evaluate_script(
        &mut context,
        "if (globalThis.stage !== 'started') { throw new Error('stage ' + globalThis.stage); }\n\
         if (globalThis.settled !== undefined) { throw new Error('settled early ' + globalThis.settled); }",
        "probe.js",
        ScriptLimits::default(),
    )
    .expect("the import promise stays pending while the module awaits");
    evaluate_script(&mut context, "globalThis.open();", "release.js", ScriptLimits::default())
        .expect("the gate opens");
    pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
        .expect("second pump completes the async evaluation");
    evaluate_script(
        &mut context,
        "if (globalThis.stage !== 'finished') { throw new Error('stage ' + globalThis.stage); }\n\
         if (globalThis.settled !== 'finished') { throw new Error('settled ' + globalThis.settled); }\n\
         if (globalThis.imported !== 5) { throw new Error('imported ' + globalThis.imported); }",
        "probe2.js",
        ScriptLimits::default(),
    )
    .expect("the import fulfills only after the async evaluation completes");
}

#[test]
fn tla_diamond_executes_in_spec_async_evaluation_order() {
    // The spec's async-evaluation example graph (ECMA-262 16.6.1 example):
    // A -> B, C; B -> D; C -> D, E; D -> A (cycle). Every module awaits, so
    // the start order is D, E (kicked in the initial DFS pass), then B, C
    // (unblocked by D and E in [[AsyncEvaluationOrder]] order), then A.
    evaluate_dynamic(
        "import './b.mjs';\nimport './c.mjs';\nglobalThis.log.push('A');\nawait Promise.resolve();",
        &[
            (
                "./b.mjs",
                "import './d.mjs';\nglobalThis.log.push('B');\nawait Promise.resolve();",
            ),
            (
                "./c.mjs",
                "import './d.mjs';\nimport './e.mjs';\nglobalThis.log.push('C');\nawait Promise.resolve();",
            ),
            (
                "./d.mjs",
                "import 'root.mjs';\nglobalThis.log = [];\nglobalThis.log.push('D');\nawait Promise.resolve();",
            ),
            ("./e.mjs", "globalThis.log.push('E');\nawait Promise.resolve();"),
            ("root.mjs", ""),
        ],
        "if (globalThis.log.join(',') !== 'D,E,B,C,A') { throw new Error('order ' + globalThis.log.join(',')); }",
    )
    .expect("the async diamond starts modules in spec order");
}

#[test]
fn facade_module_evaluation_error_reports_the_async_outcome() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let mut loader = MapLoader::new(&[]);

    // A fulfilling top-level-await root records no evaluation error.
    evaluate_module(
        &mut context,
        "globalThis.fulfilled = await Promise.resolve(42);",
        "fulfill.mjs",
        &mut loader,
        ScriptLimits::default(),
    )
    .expect("a fulfilling TLA module starts its asynchronous evaluation");
    pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
        .expect("pump drains the fulfillment continuation");
    assert!(
        quickjs::module_evaluation_error(&context, "fulfill.mjs").is_none(),
        "a fulfilled async evaluation records no error"
    );

    // A rejecting top-level-await root records the rejection as its
    // [[EvaluationError]] once the rejection continuation has run.
    evaluate_module(
        &mut context,
        "await Promise.reject(new Error('boom'));",
        "reject.mjs",
        &mut loader,
        ScriptLimits::default(),
    )
    .expect("a rejecting TLA module still starts its asynchronous evaluation");
    pump_dynamic_imports(&mut context, &mut loader, ScriptLimits::default())
        .expect("pump drains the rejection continuation");
    let error = quickjs::module_evaluation_error(&context, "reject.mjs")
        .expect("the facade surfaces the recorded evaluation error");
    assert!(
        error.to_string().contains("boom"),
        "expected the rejection message, got: {error}"
    );
}

