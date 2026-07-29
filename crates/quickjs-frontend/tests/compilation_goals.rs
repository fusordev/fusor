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
fn async_global_script_preserves_quickjs_script_context_rules() {
    let goal = GlobalScriptGoal::new().with_top_level_await(true);

    for source in [
        "await 1;",
        "await +1;",
        "await /pattern/;",
        "for await (const value of values) {}",
        "function nested(await) { return await; }",
        "const nested = async () => await promise;",
        "010;",
        "with (object) { value; }",
        "<!-- html-open-comment\nvalue;",
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
    ] {
        assert!(
            parse_global(source, goal).is_err(),
            "async Script should reject {source:?}"
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

    parse_global("<!-- html-open-comment\nvalue;", goal)
        .expect("forced strictness does not turn Script into Module");
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
