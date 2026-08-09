use oxc_span::GetSpan;
use quickjs_frontend::{
    Allocator, CompilationGoal, DiagnosticStage, FrontendDiagnosticCode, FrontendOptions,
    GlobalScriptGoal, ParseMode, parse,
};

fn parse_global(
    source: &str,
    goal: GlobalScriptGoal,
) -> Result<(), quickjs_frontend::FrontendError> {
    let allocator = Allocator::new();
    parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .map(|_| ())
}

#[test]
fn forced_strict_global_scripts_use_strict_semantics_without_rewriting_source() {
    let sloppy_source = "with (object) { value; }";
    parse_global(sloppy_source, GlobalScriptGoal::new()).expect("ordinary Script is sloppy");

    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    let error = parse(
        &allocator,
        sloppy_source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect_err("the host-forced strict flag must reject `with`");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);

    let unit = parse(
        &allocator,
        "function nested() { return 1; }",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect("ordinary syntax remains valid in a forced-strict Script");
    assert_eq!(unit.goal(), CompilationGoal::GlobalScript(goal));
    assert!(unit.program().source_type.is_script());
    assert!(
        unit.scoping()
            .scope_flags(unit.scoping().root_scope_id())
            .is_strict_mode()
    );
}

#[test]
fn forced_strictness_propagates_into_nested_functions() {
    let source = "function nested() { with (object) { value; } }";
    parse_global(source, GlobalScriptGoal::new()).expect("nested function inherits sloppy mode");

    let error = parse_global(source, GlobalScriptGoal::new().with_forced_strict(true))
        .expect_err("nested function must inherit host-forced strictness");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);
}

#[test]
fn forced_strict_block_functions_keep_lexical_binding_topology() {
    let source = "{ function local() { return local; } local; } local;";
    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    let unit = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect("a strict block function is valid");
    let scoping = unit.scoping();
    let root_scope = scoping.root_scope_id();
    let symbol = scoping
        .symbol_ids()
        .find(|symbol| scoping.symbol_name(*symbol) == "local")
        .expect("block function symbol");

    assert_ne!(scoping.symbol_scope_id(symbol), root_scope);
    assert!(scoping.get_root_binding("local".into()).is_none());
    assert_eq!(scoping.get_resolved_reference_ids(symbol).len(), 2);
    assert_eq!(
        scoping
            .root_unresolved_references()
            .get("local")
            .map(|references| references.len()),
        Some(1)
    );
}

#[test]
fn forced_strict_block_functions_do_not_merge_with_outer_or_sibling_bindings() {
    let source = concat!(
        "{ function same() {} same; } ",
        "{ function same() {} same; } ",
        "var same; same;"
    );
    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    let unit = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect("strict sibling and outer bindings are distinct");
    let scoping = unit.scoping();
    let symbols = scoping
        .symbol_ids()
        .filter(|symbol| scoping.symbol_name(*symbol) == "same")
        .collect::<Vec<_>>();

    assert_eq!(symbols.len(), 3);
    assert_eq!(
        symbols
            .iter()
            .filter(|symbol| scoping.symbol_scope_id(**symbol) == scoping.root_scope_id())
            .count(),
        1
    );
    assert!(
        symbols
            .iter()
            .all(|symbol| { scoping.get_resolved_reference_ids(*symbol).len() == 1 })
    );
}

#[test]
fn forced_strict_nested_block_function_does_not_escape_its_block() {
    let source = "function outer() { { function local() {} local; } local; }";
    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    let unit = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect("strict nested block function");
    let scoping = unit.scoping();
    let local = scoping
        .symbol_ids()
        .find(|symbol| scoping.symbol_name(*symbol) == "local")
        .expect("local symbol");

    assert_eq!(scoping.get_resolved_reference_ids(local).len(), 1);
    assert_eq!(
        scoping
            .root_unresolved_references()
            .get("local")
            .map(|references| references.len()),
        Some(1)
    );
}

#[test]
fn forced_strict_semantic_sentinel_preserves_the_caller_source_model() {
    let source = "#!/usr/bin/env qjs\n{ function local() {} }";
    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    let unit = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect("forced strict Script with a hashbang");

    assert!(unit.has_synthetic_strict_directive());
    assert!(unit.source_directives().is_empty());
    assert_eq!(unit.program().source_text, source);
    assert!(unit.program().source_type.is_script());
    assert_eq!(unit.program().body.len(), 1);
    assert_eq!(unit.program().body[0].span().start, 19);
    assert!(!unit.module_record().has_module_syntax);

    let explicit_source = "\"use strict\";\nvalue;";
    let explicit = parse(
        &allocator,
        explicit_source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect("source-provided strict directive");
    assert!(!explicit.has_synthetic_strict_directive());
    assert_eq!(explicit.source_directives().len(), 1);
}

#[test]
fn forced_strict_diagnostics_retain_original_source_spans() {
    let source = "with (object) { value; }";
    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    let error = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect_err("strict mode rejects with statements");
    let label = error
        .diagnostics()
        .iter()
        .flat_map(|diagnostic| &diagnostic.labels)
        .next()
        .expect("strict diagnostic label");

    assert_eq!(
        &source[label.span.start as usize..label.span.end as usize],
        "with"
    );
}

#[test]
fn forced_strict_hashbang_diagnostics_retain_original_source_spans() {
    let source = "#!/usr/bin/env qjs\nwith (object) { value; }";
    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    let error = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect_err("strict mode rejects with after a hashbang");
    let label = error
        .diagnostics()
        .iter()
        .flat_map(|diagnostic| &diagnostic.labels)
        .next()
        .expect("strict diagnostic label");

    assert_eq!(
        &source[label.span.start as usize..label.span.end as usize],
        "with"
    );
}

#[test]
fn async_global_script_goal_admits_top_level_await_without_becoming_a_module() {
    let source = "const value = await promise;";
    let ordinary = parse_global(source, GlobalScriptGoal::new())
        .expect_err("ordinary Script must reject top-level await");
    assert_eq!(ordinary.stage(), DiagnosticStage::Parser);

    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_top_level_await(true);
    let unit = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect("the async global Script goal admits top-level await");

    assert_eq!(unit.goal(), CompilationGoal::GlobalScript(goal));
    assert!(unit.program().source_type.is_script());
    assert!(!unit.module_record().has_module_syntax);
    assert!(
        !unit
            .scoping()
            .scope_flags(unit.scoping().root_scope_id())
            .is_strict_mode()
    );
}

#[test]
fn async_global_script_does_not_make_nested_functions_async() {
    let error = parse_global(
        "function nested() { return await promise; }",
        GlobalScriptGoal::new().with_top_level_await(true),
    )
    .expect_err("top-level await capability must not leak into nested functions");
    assert_eq!(error.stage(), DiagnosticStage::Parser);
}

#[test]
fn async_global_script_rejects_annex_b_html_comments() {
    let source = "await promise; <!-- html-open-comment\nvalue;";
    let allocator = Allocator::new();
    let goal = GlobalScriptGoal::new().with_top_level_await(true);
    let error = parse(
        &allocator,
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
    )
    .expect_err("Annex B HTML comments must be rejected");

    assert!(
        matches!(
            error.stage(),
            DiagnosticStage::Parser | DiagnosticStage::Profile
        ),
        "the async Script parse must reject Annex B HTML comments: {error}"
    );
}

#[test]
fn scripts_reject_annex_b_legacy_octal_numeric_literals() {
    for source in ["010;", "09;"] {
        let allocator = Allocator::new();
        let error = parse(
            &allocator,
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        )
        .expect_err("Annex B legacy octal numeric syntax must be rejected");
        assert_eq!(
            error.stage(),
            DiagnosticStage::Profile,
            "{source:?}: {error}"
        );
        assert!(error.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == FrontendDiagnosticCode::UnsupportedAnnexBLegacyOctal
        }));
    }
}

#[test]
fn sloppy_scripts_admit_annex_b_legacy_string_escapes() {
    for source in [r"'\1';", r"'\8';", r"'\08';"] {
        let allocator = Allocator::new();
        parse(
            &allocator,
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        )
        .unwrap_or_else(|error| panic!("sloppy Script should admit {source:?}: {error}"));
    }
}

#[test]
fn strict_scripts_reject_annex_b_legacy_string_escapes_as_early_errors() {
    for source in [r#""use strict"; '\1';"#, r#""use strict"; '\8';"#] {
        let allocator = Allocator::new();
        let error = parse(
            &allocator,
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        )
        .expect_err("strict Script must reject Annex B legacy string escapes");
        assert!(matches!(
            error.stage(),
            DiagnosticStage::Parser | DiagnosticStage::Semantic
        ));
    }

    let error = parse_global(r"'\1';", GlobalScriptGoal::new().with_forced_strict(true))
        .expect_err("host-forced strict Script must reject a legacy string escape");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);
}

#[test]
fn async_global_script_preserves_quickjs_script_context_rules() {
    let goal = GlobalScriptGoal::new().with_top_level_await(true);

    for source in [
        "await 1;",
        "await +1;",
        "await /pattern/;",
        "for await (const value of values) {}",
        "function nested(await) { return await; }",
        "const nested = async () => await promise;",
        "with (object) { value; }",
        "await promise; with (object) { value; }",
        "await import('./dependency.js');",
        "function nested() { await: while (false) { break await; } }",
    ] {
        parse_global(source, goal).unwrap_or_else(|error| {
            panic!("async Script should accept {source:?}: {error}");
        });
    }

    for source in [
        "var await = 1;",
        "const nested = () => await promise;",
        "return 1;",
        "new.target;",
        "import value from './value.js';",
        "import.meta;",
        "await import.meta;",
        "await: while (false) { break await; }",
        "010;",
        "await promise; 010;",
        "<!-- html-open-comment\nvalue;",
        "await promise; <!-- html-open-comment\nvalue;",
    ] {
        assert!(
            parse_global(source, goal).is_err(),
            "async Script should reject {source:?}"
        );
    }

    for (source, expected_code) in [
        (
            "import value from './value.js';",
            FrontendDiagnosticCode::AsyncScriptModuleDeclaration,
        ),
        (
            "import.meta;",
            FrontendDiagnosticCode::AsyncScriptImportMeta,
        ),
        (
            "await: while (false) { break await; }",
            FrontendDiagnosticCode::AsyncScriptAwaitIdentifier,
        ),
        (
            "var await = 1;",
            FrontendDiagnosticCode::AsyncScriptAwaitIdentifier,
        ),
    ] {
        let error = parse_global(source, goal).expect_err("async Script contextual rejection");
        assert!(
            error
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == expected_code),
            "{source:?}: {error}"
        );
    }
}

#[test]
fn forced_strict_global_script_enforces_quickjs_strict_early_errors() {
    let goal = GlobalScriptGoal::new().with_forced_strict(true);
    for source in [
        "010;",
        "with (object) {}",
        "delete identifier;",
        "var public;",
        "function duplicate(value, value) {}",
    ] {
        assert!(
            parse_global(source, goal).is_err(),
            "forced-strict Script should reject {source:?}"
        );
    }

    let error = parse_global("<!-- html-open-comment\nvalue;", goal)
        .expect_err("Annex B HTML comments are not supported");
    assert!(error.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == FrontendDiagnosticCode::UnsupportedAnnexBHtmlComment
    }));
}

#[test]
fn forced_strict_and_top_level_await_flags_compose() {
    let goal = GlobalScriptGoal::new()
        .with_forced_strict(true)
        .with_top_level_await(true);
    parse_global("await promise;", goal).expect("both global Script flags compose");

    let error = parse_global("with (object) {}", goal)
        .expect_err("forced strictness still applies to an async global Script");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);
}

#[test]
fn async_script_still_applies_the_quickjs_syntax_profile() {
    let allocator = Allocator::new();
    let error = parse(
        &allocator,
        "await using resource = acquire();",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(
            GlobalScriptGoal::new().with_top_level_await(true),
        )),
    )
    .expect_err("QuickJS 2026-06-04 does not support `await using`");
    assert_eq!(error.stage(), DiagnosticStage::Profile);
    assert!(error.diagnostics().iter().any(|diagnostic| {
        diagnostic.code == FrontendDiagnosticCode::UnsupportedAwaitUsingDeclaration
    }));
}

#[test]
fn compatibility_parse_mode_remains_an_ordinary_script() {
    parse_global("await = 1;", GlobalScriptGoal::new())
        .expect("await remains an identifier in an ordinary Script");

    let allocator = Allocator::new();
    let unit = parse(
        &allocator,
        "var value = 1;",
        FrontendOptions::new(ParseMode::Script),
    )
    .expect("compatibility Script mode remains supported");
    assert_eq!(
        unit.goal(),
        CompilationGoal::GlobalScript(GlobalScriptGoal::new())
    );
}
