use quickjs_frontend::{
    Allocator, DiagnosticStage, FrontendOptions, ParseMode, Span, parse, with_parsed_program,
};

#[test]
fn parses_javascript_scripts_and_preserves_utf8_byte_spans() {
    let source = "const π = 1;\nπ += 2;";
    let allocator = Allocator::new();

    let program =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Script)).expect("valid script");

    assert!(program.source_type.is_script());
    assert!(program.source_type.is_javascript());
    assert!(!program.source_type.is_jsx());
    assert!(!program.source_type.is_typescript());
    assert_eq!(
        program.span,
        Span::new(0, u32::try_from(source.len()).unwrap())
    );
    assert_eq!(program.body.len(), 2);
}

#[test]
fn parses_modules_only_when_module_mode_is_explicit() {
    let source = "import value from './dep.js'; export { value };";
    let allocator = Allocator::new();

    let module =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Module)).expect("valid module");
    assert!(module.source_type.is_module());

    let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
        .expect_err("module syntax must not be accepted as a script");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);
}

#[test]
fn rejects_both_fatal_and_recoverable_parser_diagnostics() {
    for source in ["function {", "const missing = ; let recovered = 1;"] {
        let allocator = Allocator::new();
        let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
            .expect_err("all parser diagnostics must reject the program");

        assert_eq!(error.stage(), DiagnosticStage::Parser, "{source}");
        assert!(!error.diagnostics().is_empty(), "{source}");
    }
}

#[test]
fn rejects_semantic_early_errors_and_retains_diagnostic_byte_spans() {
    let source = "let duplicate; let duplicate;";
    let allocator = Allocator::new();
    let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
        .expect_err("redeclaration is an ECMAScript early error");

    assert_eq!(error.stage(), DiagnosticStage::Semantic);
    let source_len = u32::try_from(source.len()).expect("test source fits in an Oxc span");
    assert!(error.diagnostics().iter().any(|diagnostic| {
        diagnostic
            .labels
            .iter()
            .any(|label| label.span.start < label.span.end && label.span.end <= source_len)
    }));
}

#[test]
fn top_level_return_is_an_explicit_function_constructor_option() {
    let source = "return arguments[0] + 1;";
    let allocator = Allocator::new();

    assert!(parse(&allocator, source, FrontendOptions::new(ParseMode::Script)).is_err());

    let program = parse(
        &allocator,
        source,
        FrontendOptions::new(ParseMode::Script).with_top_level_return(true),
    )
    .expect("Function constructor body mode");
    assert_eq!(program.body.len(), 1);

    assert!(
        parse(
            &allocator,
            source,
            FrontendOptions::new(ParseMode::Module).with_top_level_return(true),
        )
        .is_err(),
        "Function constructor relaxation must not weaken Module grammar"
    );
}

#[test]
fn engine_mode_rejects_typescript_and_jsx() {
    for source in ["const value: number = 1;", "const view = <main />;"] {
        let allocator = Allocator::new();
        assert!(
            parse(&allocator, source, FrontendOptions::new(ParseMode::Script)).is_err(),
            "{source}"
        );
    }
}

#[test]
fn accepts_an_es2025_explicit_resource_management_sample() {
    let source = r"
        export async function collect() {
            await using resource = acquire();
            return Promise.try(() => resource);
        }
    ";
    let allocator = Allocator::new();

    let program =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Module)).expect("ES2025 module");

    assert_eq!(program.body.len(), 1);
}

#[test]
fn leaves_regexp_pattern_validation_to_the_quickjs_runtime_layer() {
    let allocator = Allocator::new();
    let program = parse(
        &allocator,
        "const pattern = /(/;",
        FrontendOptions::new(ParseMode::Script),
    )
    .expect("the front end only identifies the RegExp literal boundary");

    assert_eq!(program.body.len(), 1);

    for source in ["const pattern = /[/;", "const pattern = /unterminated;"] {
        let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
            .expect_err("Oxc must still reject malformed literal boundaries");
        assert_eq!(error.stage(), DiagnosticStage::Parser);
    }
}

#[test]
fn callback_api_keeps_arena_owned_ast_inside_the_allocator_lifetime() {
    let statement_count = with_parsed_program(
        "let first = 1; let second = 2;",
        FrontendOptions::new(ParseMode::Script),
        |program| program.body.len(),
    )
    .expect("callback parse");

    assert_eq!(statement_count, 2);
}
