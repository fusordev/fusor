use fusor_diagnostics::{
    Diagnostic, DiagnosticCode, DiagnosticCodeError, DiagnosticLabel, DiagnosticReport,
    DiagnosticSeverity, PrettyDiagnosticError, SourceMap, SourceRegistry, render_pretty,
    render_pretty_report,
};

#[test]
fn diagnostic_codes_have_a_stable_validated_alphabet() {
    let code = DiagnosticCode::new("fusor::parser::unexpected_token").expect("valid code");
    assert_eq!(code.as_str(), "fusor::parser::unexpected_token");
    assert_eq!(DiagnosticCode::new(""), Err(DiagnosticCodeError::Empty));
    assert!(matches!(
        DiagnosticCode::new("fusor parser"),
        Err(DiagnosticCodeError::InvalidCharacter {
            index: 7,
            character: ' '
        })
    ));
}

#[test]
fn pretty_output_contains_stable_metadata_and_a_unicode_source_snippet() {
    let source_text = "const π = ;\n";
    let mut sources = SourceRegistry::new();
    let source = sources.register("pretty.js", source_text).expect("source");
    let start = source_text.find('π').expect("pi");
    let span = sources
        .span(&source, start, start + 'π'.len_utf8())
        .expect("span");
    let diagnostic = Diagnostic::new(
        DiagnosticCode::new("fusor::parser::missing_initializer").expect("code"),
        DiagnosticSeverity::Error,
        "expected an initializer expression",
    )
    .with_help("write an expression after `=`")
    .with_label(DiagnosticLabel::primary(
        span,
        Some("declaration starts here".to_owned()),
    ));

    assert_eq!(
        diagnostic.code().as_str(),
        "fusor::parser::missing_initializer"
    );
    assert_eq!(diagnostic.severity(), DiagnosticSeverity::Error);
    assert_eq!(diagnostic.message(), "expected an initializer expression");
    let rendered = render_pretty(&sources, &diagnostic).expect("pretty output");
    assert!(rendered.contains("fusor::parser::missing_initializer"));
    assert!(rendered.contains("expected an initializer expression"));
    assert!(rendered.contains("pretty.js"));
    assert!(rendered.contains("const π = ;"));
    assert!(rendered.contains("declaration starts here"));
    assert!(rendered.contains("write an expression after `=`"));
}

#[test]
fn miette_adapter_rejects_labels_from_multiple_sources() {
    let mut sources = SourceRegistry::new();
    let first = sources.register("first.js", "a").expect("first");
    let second = sources.register("second.js", "b").expect("second");
    let first_span = sources.span(&first, 0, 1).expect("first span");
    let second_span = sources.span(&second, 0, 1).expect("second span");
    let diagnostic = Diagnostic::new(
        DiagnosticCode::new("fusor::compiler::cross_source").expect("code"),
        DiagnosticSeverity::Warning,
        "labels cross a rendering boundary",
    )
    .with_label(DiagnosticLabel::primary(first_span, None))
    .with_label(DiagnosticLabel::secondary(second_span, None));

    assert_eq!(
        diagnostic.to_pretty(&sources).expect_err("mixed sources"),
        PrettyDiagnosticError::MultipleSources
    );
}

#[test]
fn related_miette_diagnostics_render_independent_sources() {
    let mut sources = SourceRegistry::new();
    let origin = sources
        .register("origin.js", "throw value;")
        .expect("origin");
    let caller = sources.register("caller.js", "run();").expect("caller");
    let origin_span = sources.span(&origin, 0, 5).expect("origin span");
    let caller_span = sources.span(&caller, 0, 5).expect("caller span");
    let primary = Diagnostic::new(
        DiagnosticCode::new("fusor::runtime::uncaught").expect("code"),
        DiagnosticSeverity::Error,
        "uncaught JavaScript value",
    )
    .with_label(DiagnosticLabel::primary(
        origin_span,
        Some("thrown here".to_owned()),
    ));
    let related = Diagnostic::new(
        DiagnosticCode::new("fusor::runtime::stack_frame").expect("code"),
        DiagnosticSeverity::Advice,
        "called from caller.js",
    )
    .with_label(DiagnosticLabel::primary(
        caller_span,
        Some("call site".to_owned()),
    ));
    let report = DiagnosticReport::new(primary).with_related(related);

    let rendered = render_pretty_report(&sources, &report).expect("pretty report");
    assert!(rendered.contains("uncaught JavaScript value"));
    assert!(rendered.contains("origin.js"));
    assert!(rendered.contains("called from caller.js"));
    assert!(rendered.contains("caller.js"));
}

#[test]
fn diagnostics_resolve_registered_source_map_chains_before_rendering() {
    let outer = SourceMap::from_slice(
        br#"{"version":3,"sources":["intermediate.js"],"names":[],"mappings":"AAAA"}"#,
    )
    .expect("outer map");
    let inner = SourceMap::from_slice(
        br#"{"version":3,"sources":["original.ts"],"names":[],"mappings":"AAAA"}"#,
    )
    .expect("inner map");
    let mut sources = SourceRegistry::new();
    let bundle = sources
        .register_with_source_map("bundle.js", "x", Some(outer))
        .expect("bundle");
    sources
        .register_with_source_map("intermediate.js", "x", Some(inner))
        .expect("intermediate");
    let original = sources.register("original.ts", "x").expect("original");
    let generated = sources.span(&bundle, 0, 1).expect("generated span");
    let diagnostic = Diagnostic::new(
        DiagnosticCode::new("fusor::compiler::lowering").expect("code"),
        DiagnosticSeverity::Error,
        "lowering failed",
    )
    .with_label(DiagnosticLabel::primary(generated, None));

    let resolved = diagnostic
        .resolve_source_maps(&sources)
        .expect("resolved diagnostic");
    assert_eq!(resolved.labels()[0].span().source_id(), &original);
    let rendered = render_pretty(&sources, &resolved).expect("pretty output");
    assert!(rendered.contains("original.ts"));
    assert!(!rendered.contains("bundle.js"));
}
