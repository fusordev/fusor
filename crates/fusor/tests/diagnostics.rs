use fusor::{
    RegisteredScriptFailure, ScriptLimits, SourceMap, SourceRegistry, evaluate_registered_script,
    render_pretty_report,
};
use fusor_runtime::{Runtime, RuntimeLimits};

fn identity_map(original_name: &str) -> SourceMap {
    SourceMap::from_slice(
        format!(r#"{{"version":3,"sources":["{original_name}"],"names":[],"mappings":"AAAA"}}"#)
            .as_bytes(),
    )
    .expect("identity source map")
}

fn evaluate_failure(
    source: &str,
    original: &str,
) -> (SourceRegistry, fusor::RegisteredScriptEvaluationError) {
    let mut sources = SourceRegistry::new();
    let generated = sources
        .register_with_source_map("bundle.js", source, Some(identity_map("original.ts")))
        .expect("generated source");
    sources
        .register("original.ts", original)
        .expect("original source");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let error =
        evaluate_registered_script(&mut context, &sources, &generated, ScriptLimits::default())
            .expect_err("Script must fail");
    (sources, error)
}

#[test]
fn registered_frontend_diagnostics_render_at_the_chained_original_source() {
    let (sources, error) = evaluate_failure("const value = ;", "let original: number;");
    assert!(matches!(
        error.failure(),
        RegisteredScriptFailure::Frontend(_)
    ));

    let report = error
        .diagnostic_report(&sources)
        .expect("diagnostic report");
    assert!(
        report
            .primary()
            .code()
            .as_str()
            .starts_with("fusor::frontend::")
    );
    assert_eq!(
        report.primary().labels()[0].span().source_id(),
        &sources
            .source_id_by_name("original.ts")
            .expect("original ID")
    );
    let rendered = render_pretty_report(&sources, &report).expect("Miette output");
    assert!(rendered.contains("original.ts"));
    assert!(!rendered.contains("bundle.js"));
}

#[test]
fn registered_semantic_rejections_have_stable_codes_and_mapped_labels() {
    // A function declaration as a loop body is an Oxc *semantic* early
    // error (the compiler now supports `with` statements, Annex B block
    // functions, and optional chains, so planning-stage rejections are no
    // longer reachable from Global Script source).
    let (sources, error) =
        evaluate_failure("while (x) function f() {}", "while (x) function f() {}");
    assert!(matches!(
        error.failure(),
        RegisteredScriptFailure::Frontend(_)
    ));

    let report = error
        .diagnostic_report(&sources)
        .expect("diagnostic report");
    assert_eq!(
        report.primary().code().as_str(),
        "fusor::frontend::oxc::semantic"
    );
    assert!(report.primary().message().contains("Invalid function declaration"));
    assert_eq!(
        report.primary().labels()[0].span().source_id(),
        &sources
            .source_id_by_name("original.ts")
            .expect("original ID")
    );
}

#[test]
fn registered_runtime_exceptions_render_verified_mapped_stacks() {
    let source = "function outer(){inner();} function inner(){null.value;} outer();";
    let (sources, error) = evaluate_failure(source, "throw new TypeError('original');");
    assert!(matches!(
        error.failure(),
        RegisteredScriptFailure::Runtime(_)
    ));

    let report = error
        .diagnostic_report(&sources)
        .expect("diagnostic report");
    assert_eq!(
        report.primary().code().as_str(),
        "fusor::runtime::exception::type_error"
    );
    assert!(!report.related().is_empty());
    assert!(
        report
            .related()
            .iter()
            .all(|diagnostic| diagnostic.code().as_str() == "fusor::runtime::stack_frame")
    );
    let rendered = render_pretty_report(&sources, &report).expect("Miette output");
    assert!(rendered.contains("TypeError"));
    assert!(rendered.contains("original.ts"));
    assert!(rendered.contains("called from bundle.js"));
}
