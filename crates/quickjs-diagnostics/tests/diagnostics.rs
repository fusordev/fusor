use quickjs_diagnostics::{
    Diagnostic, DiagnosticCode, DiagnosticCodeError, DiagnosticLabel, DiagnosticSeverity,
    PrettyDiagnosticError, SourceRegistry, render_pretty,
};

#[test]
fn diagnostic_codes_have_a_stable_validated_alphabet() {
    let code = DiagnosticCode::new("quickjs::parser::unexpected_token").expect("valid code");
    assert_eq!(code.as_str(), "quickjs::parser::unexpected_token");
    assert_eq!(DiagnosticCode::new(""), Err(DiagnosticCodeError::Empty));
    assert!(matches!(
        DiagnosticCode::new("quickjs parser"),
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
        DiagnosticCode::new("quickjs::parser::missing_initializer").expect("code"),
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
        "quickjs::parser::missing_initializer"
    );
    assert_eq!(diagnostic.severity(), DiagnosticSeverity::Error);
    assert_eq!(diagnostic.message(), "expected an initializer expression");
    let rendered = render_pretty(&sources, &diagnostic).expect("pretty output");
    assert!(rendered.contains("quickjs::parser::missing_initializer"));
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
        DiagnosticCode::new("quickjs::compiler::cross_source").expect("code"),
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
