use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use fusor_diagnostics::{SourceError, SourceRegistry, render_pretty};
use fusor_frontend::{
    Allocator, CompilationGoal, DiagnosticStage, DirectEvalBinding, DirectEvalBindingKind,
    DirectEvalBindingLocation, DirectEvalBindingScope, DirectEvalCapabilities, DirectEvalContext,
    DirectEvalPrivateName, DirectEvalScopeFrame, DirectEvalScopeKind, DirectEvalScopeSnapshot,
    DirectEvalVariableEnvironment, DynamicFunctionKind, DynamicFunctionSource,
    FrontendDiagnosticCode, FrontendLimitError, FrontendLimits, FrontendOptions,
    FrontendSourceError, GlobalScriptGoal, IndirectEvalGoal, ParseMode, RegisteredFrontendError,
    SourceFragment, Span, UnsupportedCompilationGoal, parse, with_dynamic_function_source,
    with_parsed_program, with_registered_program,
};

#[test]
fn parses_javascript_scripts_and_preserves_utf8_byte_spans() {
    let source = "const π = 1;\nπ += 2;";
    let allocator = Allocator::new();

    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Script)).expect("valid script");
    let program = unit.program();

    assert!(program.source_type.is_script());
    assert!(program.source_type.is_javascript());
    assert!(!program.source_type.is_jsx());
    assert!(!program.source_type.is_typescript());
    assert_eq!(
        program.span,
        Span::new(0, u32::try_from(source.len()).unwrap())
    );
    assert_eq!(program.body.len(), 2);
    assert_eq!(
        unit.goal(),
        CompilationGoal::GlobalScript(GlobalScriptGoal::new())
    );
    assert!(!unit.module_record().has_module_syntax);
    assert!(unit.module_record().requested_modules.is_empty());
    assert_eq!(unit.scoping().symbol_names().collect::<Vec<_>>(), vec!["π"]);
}

#[test]
fn parses_modules_only_when_module_mode_is_explicit() {
    let source = "import value from './dep.js'; export { value };";
    let allocator = Allocator::new();

    let module =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Module)).expect("valid module");
    assert!(module.program().source_type.is_module());
    assert_eq!(module.goal(), CompilationGoal::Module);
    assert!(module.module_record().has_module_syntax);
    assert_eq!(module.module_record().requested_modules.len(), 1);
    assert_eq!(module.module_record().import_entries.len(), 1);
    assert_eq!(module.module_record().exported_bindings.len(), 1);
    assert_eq!(
        module.scoping().symbol_names().collect::<Vec<_>>(),
        vec!["value"]
    );

    let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
        .expect_err("module syntax must not be accepted as a script");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);
}

#[test]
fn retains_oxc_semantic_scopes_symbols_and_unresolved_references() {
    let source =
        "let local = external; function add(argument) { return local + argument + missing; }";
    let allocator = Allocator::new();
    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Script)).expect("valid script");
    let scoping = unit.scoping();

    assert_eq!(scoping.scopes_len(), 2);
    assert_eq!(
        scoping.symbol_names().collect::<Vec<_>>(),
        vec!["local", "add", "argument"]
    );
    assert_eq!(scoping.root_unresolved_references().len(), 2);
    assert_eq!(scoping.references_len(), 4);
    assert!(unit.semantic().nodes().len() > scoping.references_len());
}

#[test]
fn retains_oxc_class_and_private_name_semantics() {
    let source = "class Box { #value = 1; read() { return this.#value; } }";
    let allocator = Allocator::new();
    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Script)).expect("valid class");

    let classes = unit.semantic().classes();
    assert_eq!(classes.len(), 1);
    let (class_id, _) = classes
        .iter_enumerated()
        .next()
        .expect("class semantic entry");
    assert!(
        classes.elements[class_id]
            .iter()
            .any(|element| element.is_private && element.name == "value")
    );
    let references = classes
        .iter_private_identifiers(class_id)
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].name, "value");
    assert_eq!(references[0].element_ids.len(), 1);
    assert!(!unit.semantic().nodes().is_empty());
}

#[test]
fn script_module_record_retains_dynamic_imports_without_changing_parse_goal() {
    let source = "const pending = import('./dynamic.js');";
    let allocator = Allocator::new();
    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Script)).expect("valid script");

    assert_eq!(
        unit.goal(),
        CompilationGoal::GlobalScript(GlobalScriptGoal::new())
    );
    assert!(!unit.module_record().has_module_syntax);
    assert!(unit.module_record().requested_modules.is_empty());
    assert_eq!(unit.module_record().dynamic_imports.len(), 1);
}

#[test]
fn rejects_both_fatal_and_recoverable_parser_diagnostics() {
    for source in ["function {", "const missing = ; let recovered = 1;"] {
        let allocator = Allocator::new();
        let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
            .expect_err("all parser diagnostics must reject the program");

        assert_eq!(error.stage(), DiagnosticStage::Parser, "{source}");
        assert!(!error.diagnostics().is_empty(), "{source}");
        assert!(
            error
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.code == FrontendDiagnosticCode::OxcParser),
            "{source}"
        );
    }
}

#[test]
fn rejects_semantic_early_errors_and_retains_diagnostic_byte_spans() {
    let source = "let duplicate; let duplicate;";
    let allocator = Allocator::new();
    let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
        .expect_err("redeclaration is an ECMAScript early error");

    assert_eq!(error.stage(), DiagnosticStage::Semantic);
    assert!(
        error
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code == FrontendDiagnosticCode::OxcSemantic)
    );
    let source_len = u32::try_from(source.len()).expect("test source fits in an Oxc span");
    assert!(error.diagnostics().iter().any(|diagnostic| {
        diagnostic
            .labels
            .iter()
            .any(|label| label.span.start < label.span.end && label.span.end <= source_len)
    }));
}

#[test]
fn rejects_chained_continue_targets_that_do_not_end_in_an_iteration() {
    for source in [
        "outer: inner: { continue outer; }",
        "outer: inner: switch (0) { case 0: continue outer; }",
    ] {
        let allocator = Allocator::new();
        let error = parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
            .expect_err("a chained continue target must end in an iteration");

        assert_eq!(error.stage(), DiagnosticStage::Semantic, "{source}");
        assert_eq!(error.diagnostics().len(), 1, "{source}");
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(
            diagnostic.code,
            FrontendDiagnosticCode::InvalidChainedContinueTarget,
            "{source}"
        );
        assert_eq!(diagnostic.message, "break/continue label not found");
        assert_eq!(diagnostic.labels.len(), 1, "{source}");
        let span = diagnostic.labels[0].span;
        assert_eq!(&source[span.start as usize..span.end as usize], "outer");
    }
}

#[test]
fn preserves_valid_chained_continue_targets_ending_in_an_iteration() {
    for source in [
        "outer: inner: while (false) { continue outer; }",
        "outer: inner: do { continue outer; } while (false);",
        "outer: inner: for (;;) { continue outer; }",
        "outer: inner: for (const value in object) { continue outer; }",
        "outer: inner: for (const value of values) { continue outer; }",
    ] {
        let allocator = Allocator::new();
        parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
            .expect("a chained label ending in an iteration is a valid continue target");
    }
}

/// `FUS-OXC-003`: pinned `QuickJS` caps parser recursion around 695 nested
/// parentheses and reports `stack overflow` (`quickjs.c:22720`). That bound is
/// a `QuickJS` resource limit rather than ECMAScript grammar, so this front end
/// keeps parsing well-formed deeply nested sources on its isolated stack. The
/// difference is recorded in `tests/parser/manifest.json` and reproduced by
/// `candidate-accept/script/parser-stack-overflow.js`.
#[test]
fn accepts_nesting_deeper_than_the_pinned_quickjs_parser_recursion_limit() {
    // The pinned oracle already reports `stack overflow` at 696 nested
    // parentheses, so this depth is unambiguously past its limit.
    const DEPTH: usize = 4_000;

    let mut source = String::with_capacity(2 * DEPTH + 2);
    source.extend(std::iter::repeat_n('(', DEPTH));
    source.push('1');
    source.extend(std::iter::repeat_n(')', DEPTH));
    source.push(';');

    with_parsed_program(&source, FrontendOptions::new(ParseMode::Script), |unit| {
        assert!(!unit.semantic().nodes().is_empty());
    })
    .expect("deeply nested parentheses remain valid ECMAScript");
}

/// `FUS-OXC-004`: pinned `QuickJS` rejects an instance field named `prototype`
/// with `invalid field name` (`quickjs.c:25396`), a check its own source marks
/// `XXX: spec: not consistent with method name checks`. ECMAScript reserves
/// `constructor` for instance fields and `prototype` for static ones only, so
/// this front end accepts the instance field and still rejects the forms the
/// specification forbids.
#[test]
fn accepts_an_instance_field_named_prototype_and_rejects_the_reserved_forms() {
    let allocator = Allocator::new();
    parse(
        &allocator,
        "class C { prototype = 1; }",
        FrontendOptions::new(ParseMode::Script),
    )
    .expect("ECMAScript reserves `prototype` only for static class fields");

    for source in [
        "class C { static prototype = 1; }",
        "class C { static prototype() {} }",
        "class C { constructor = 1; }",
        "class C { static constructor = 1; }",
    ] {
        let allocator = Allocator::new();
        parse(&allocator, source, FrontendOptions::new(ParseMode::Script))
            .expect_err("the specification forbids this class element name");
    }
}

#[test]
fn handles_many_chained_labels_and_continues_without_rewalking_each_chain() {
    const LABEL_COUNT: usize = 1_024;
    const CONTINUE_COUNT: usize = 2_048;

    let mut source = String::with_capacity(LABEL_COUNT * 8 + CONTINUE_COUNT * 12 + 32);
    for index in 0..LABEL_COUNT {
        source.push('l');
        source.push_str(&index.to_string());
        source.push(':');
    }
    source.push_str("while (false) {");
    for _ in 0..CONTINUE_COUNT {
        source.push_str("continue l0;");
    }
    source.push('}');

    with_parsed_program(&source, FrontendOptions::new(ParseMode::Script), |_| ())
        .expect("large valid label chains must remain bounded frontend work");
}

#[test]
fn compilation_goals_preserve_lossless_direct_eval_context() {
    let bindings = [DirectEvalBinding::new(
        "captured",
        DirectEvalBindingKind::FunctionDeclaration,
        true,
        false,
        DirectEvalBindingLocation::Closure { index: 3 },
    )
    .with_scope(DirectEvalBindingScope::Lexical)];
    let private_names = [DirectEvalPrivateName::new(
        "value",
        DirectEvalBindingLocation::Local { index: 7 },
    )];
    let frames = [
        DirectEvalScopeFrame::new(DirectEvalScopeKind::Class, &bindings, &private_names),
        DirectEvalScopeFrame::new(DirectEvalScopeKind::Module, &[], &[]),
        DirectEvalScopeFrame::new(DirectEvalScopeKind::Dynamic, &[], &[]),
        DirectEvalScopeFrame::new(DirectEvalScopeKind::Pseudo, &[], &[]),
    ];
    let direct_eval = DirectEvalContext::new(
        DirectEvalCapabilities::new()
            .with_strict(true)
            .with_new_target(true)
            .with_super_property(true)
            .with_arguments_allowed(true),
        DirectEvalScopeSnapshot::new(&frames),
    )
    .with_variable_environment(DirectEvalVariableEnvironment::Global);

    let goals = [
        CompilationGoal::GlobalScript(GlobalScriptGoal::new()),
        CompilationGoal::Module,
        CompilationGoal::IndirectEval(IndirectEvalGoal::new()),
        CompilationGoal::DirectEval(direct_eval),
        CompilationGoal::DynamicFunction(DynamicFunctionKind::AsyncGeneratorFunction),
    ];

    assert_eq!(goals.len(), 5);
    assert_eq!(
        direct_eval.scope_snapshot().frames()[0].bindings()[0].name(),
        "captured"
    );
    assert_eq!(
        direct_eval.scope_snapshot().frames()[0].private_names()[0].name(),
        "value"
    );
    let binding = direct_eval.scope_snapshot().frames()[0].bindings()[0];
    assert_eq!(binding.kind(), DirectEvalBindingKind::FunctionDeclaration);
    assert!(binding.is_lexical());
    assert!(!binding.is_const());
    assert_eq!(
        binding.location(),
        DirectEvalBindingLocation::Closure { index: 3 }
    );
    assert_eq!(binding.scope(), DirectEvalBindingScope::Lexical);
    let private_name = direct_eval.scope_snapshot().frames()[0].private_names()[0];
    assert_eq!(
        private_name.location(),
        DirectEvalBindingLocation::Local { index: 7 }
    );
    assert_eq!(
        direct_eval.scope_snapshot().frames()[1].kind(),
        DirectEvalScopeKind::Module
    );
    assert_eq!(
        direct_eval.scope_snapshot().frames()[2].kind(),
        DirectEvalScopeKind::Dynamic
    );
    assert_eq!(
        direct_eval.scope_snapshot().frames()[3].kind(),
        DirectEvalScopeKind::Pseudo
    );
    assert!(direct_eval.capabilities().is_strict());
    assert!(direct_eval.capabilities().allows_new_target());
    assert!(!direct_eval.capabilities().allows_super_call());
    assert!(!direct_eval.capabilities().has_instance_elements());
    assert!(direct_eval.capabilities().allows_arguments());
    assert_eq!(
        direct_eval.variable_environment(),
        DirectEvalVariableEnvironment::Global
    );
}

#[test]
fn global_and_eval_goals_reject_top_level_return() {
    let source = "return arguments[0] + 1;";
    let allocator = Allocator::new();

    for goal in [
        CompilationGoal::GlobalScript(GlobalScriptGoal::new()),
        CompilationGoal::IndirectEval(IndirectEvalGoal::new()),
        CompilationGoal::DirectEval(DirectEvalContext::new(
            DirectEvalCapabilities::new(),
            DirectEvalScopeSnapshot::default(),
        )),
    ] {
        let error = parse(&allocator, source, FrontendOptions::for_goal(goal))
            .expect_err("top-level return is invalid for global and eval code");
        assert_eq!(error.stage(), DiagnosticStage::Parser);
        assert_eq!(error.unsupported_goal(), None);
        assert!(
            error
                .diagnostics()
                .iter()
                .all(|diagnostic| { diagnostic.code == FrontendDiagnosticCode::OxcParser })
        );
    }
}

#[test]
fn plain_indirect_eval_succeeds_without_losing_its_compilation_goal() {
    let source = "var answer = 40 + 2;";
    let allocator = Allocator::new();
    let goal = CompilationGoal::IndirectEval(IndirectEvalGoal::new());
    let unit = parse(&allocator, source, FrontendOptions::for_goal(goal))
        .expect("plain indirect eval is supported");

    assert_eq!(unit.goal(), goal);
    assert!(unit.program().source_type.is_script());
    assert_eq!(unit.program().body.len(), 1);
}

#[test]
fn forced_strict_indirect_eval_fails_before_oxc_until_its_adapter_is_faithful() {
    let cases = [(
        CompilationGoal::IndirectEval(IndirectEvalGoal::new().with_forced_strict(true)),
        UnsupportedCompilationGoal::IndirectEval(IndirectEvalGoal::new().with_forced_strict(true)),
        "force_strict=true",
    )];

    for (goal, expected, requested_flags) in cases {
        let allocator = Allocator::new();
        let error = parse(&allocator, "function {", FrontendOptions::for_goal(goal))
            .expect_err("contextual adapter is not implemented");

        assert_eq!(error.stage(), DiagnosticStage::CompilationGoal);
        assert_eq!(error.unsupported_goal(), Some(expected));
        assert!(!error.parser_panicked());
        assert_eq!(error.diagnostics().len(), 1);
        assert_eq!(
            error.diagnostics()[0].code,
            FrontendDiagnosticCode::UnsupportedCompilationGoal
        );
        assert!(error.diagnostics()[0].labels.is_empty());
        assert!(
            error.diagnostics()[0].message.contains(requested_flags),
            "{}",
            error.diagnostics()[0].message
        );
        assert!(error.diagnostics()[0].message.contains("not implemented"));
    }
}

#[test]
fn direct_eval_parses_as_script_and_inherits_caller_strictness() {
    let capabilities = DirectEvalCapabilities::new()
        .with_strict(true)
        .with_new_target(true)
        .with_arguments_allowed(true);
    let context = DirectEvalContext::new(capabilities, DirectEvalScopeSnapshot::default());
    let allocator = Allocator::new();
    let unit = parse(
        &allocator,
        "let answer = 40 + 2; answer;",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(context)),
    )
    .expect("closed direct eval Script");

    assert_eq!(unit.goal(), CompilationGoal::DirectEval(context));
    assert!(unit.program().source_type.is_script());
    assert!(unit.has_synthetic_strict_directive());
    assert!(unit.source_directives().is_empty());
}

#[test]
fn direct_eval_admits_only_inherited_new_target_context() {
    let allocator = Allocator::new();
    let allowed = DirectEvalContext::new(
        DirectEvalCapabilities::new().with_new_target(true),
        DirectEvalScopeSnapshot::default(),
    );
    let unit = parse(
        &allocator,
        "new.target;",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(allowed)),
    )
    .expect("direct eval inherits new.target grammar context");
    assert!(unit.program().source_type.is_script());

    let denied = DirectEvalContext::new(
        DirectEvalCapabilities::new(),
        DirectEvalScopeSnapshot::default(),
    );
    parse(
        &allocator,
        "new.target;",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(denied)),
    )
    .expect_err("direct eval outside function code rejects new.target");

    parse(
        &allocator,
        "new.target; if (true) { return; }",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(allowed)),
    )
    .expect_err("contextual new.target does not admit a top-level return");
}

#[test]
fn direct_eval_admits_only_lexically_inherited_super_property_context() {
    let allocator = Allocator::new();
    let allowed = DirectEvalContext::new(
        DirectEvalCapabilities::new().with_super_property(true),
        DirectEvalScopeSnapshot::default(),
    );
    parse(
        &allocator,
        "super.answer; (() => super.answer)();",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(allowed)),
    )
    .expect("direct eval and nested arrows inherit method super property syntax");

    parse(
        &allocator,
        "function nested() { return super.answer; }",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(allowed)),
    )
    .expect_err("ordinary nested functions do not inherit the eval caller's super binding");

    let denied = DirectEvalContext::new(
        DirectEvalCapabilities::new(),
        DirectEvalScopeSnapshot::default(),
    );
    parse(
        &allocator,
        "super.answer;",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(denied)),
    )
    .expect_err("direct eval outside a method rejects super property syntax");
}

#[test]
fn direct_eval_enforces_class_field_contains_arguments_boundaries() {
    let denied = DirectEvalContext::new(
        DirectEvalCapabilities::new().with_arguments_allowed(false),
        DirectEvalScopeSnapshot::default(),
    );
    for source in [
        "arguments;",
        "(() => arguments)();",
        "({ [arguments]() {} });",
        "class C { [arguments]() {} }",
    ] {
        let allocator = Allocator::new();
        let error = parse(
            &allocator,
            source,
            FrontendOptions::for_goal(CompilationGoal::DirectEval(denied)),
        )
        .expect_err("class field direct eval ContainsArguments early error");

        assert_eq!(error.stage(), DiagnosticStage::Semantic, "{source}");
        assert_eq!(error.diagnostics().len(), 1, "{source}");
        assert_eq!(
            error.diagnostics()[0].code,
            FrontendDiagnosticCode::DirectEvalContainsArguments,
            "{source}"
        );
        let label = &error.diagnostics()[0].labels[0];
        assert_eq!(
            &source[label.span.start as usize..label.span.end as usize],
            "arguments",
            "{source}"
        );
    }

    for source in [
        "function nested() { return arguments; }",
        "(function () { return arguments; });",
        "({ method() { return arguments; } });",
        "class C { method() { return arguments; } }",
    ] {
        let allocator = Allocator::new();
        parse(
            &allocator,
            source,
            FrontendOptions::for_goal(CompilationGoal::DirectEval(denied)),
        )
        .expect("ordinary functions stop ContainsArguments");
    }

    let allowed = DirectEvalContext::new(
        DirectEvalCapabilities::new().with_arguments_allowed(true),
        DirectEvalScopeSnapshot::default(),
    );
    let allocator = Allocator::new();
    parse(
        &allocator,
        "arguments; (() => arguments)();",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(allowed)),
    )
    .expect("non-initializer direct eval admits arguments references");
}

#[test]
fn direct_eval_admits_only_private_names_from_the_caller_environment() {
    let private_names = [DirectEvalPrivateName::new(
        "visible",
        DirectEvalBindingLocation::Closure { index: 2 },
    )];
    let frames = [DirectEvalScopeFrame::new(
        DirectEvalScopeKind::Class,
        &[],
        &private_names,
    )];
    let context = DirectEvalContext::new(
        DirectEvalCapabilities::new(),
        DirectEvalScopeSnapshot::new(&frames),
    );

    for source in ["this.#visible;", "#visible in this;"] {
        let allocator = Allocator::new();
        parse(
            &allocator,
            source,
            FrontendOptions::for_goal(CompilationGoal::DirectEval(context)),
        )
        .expect("caller private name is in the direct-eval PrivateEnvironment");
    }

    let allocator = Allocator::new();
    let error = parse(
        &allocator,
        "this.#missing;",
        FrontendOptions::for_goal(CompilationGoal::DirectEval(context)),
    )
    .expect_err("unknown private name remains an early error");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);
    assert_eq!(
        error.diagnostics()[0].code,
        FrontendDiagnosticCode::OxcSemantic
    );
    assert!(error.diagnostics()[0].message.contains("#missing"));
}

#[test]
fn source_byte_limits_reject_malformed_input_before_oxc_for_every_entry() {
    let source = "function {";
    let limits = FrontendLimits::new(source.len() - 1);
    let options = FrontendOptions::new(ParseMode::Script).with_limits(limits);
    let allocator = Allocator::new();

    let error = parse(&allocator, source, options).expect_err("source limit");
    assert_eq!(error.stage(), DiagnosticStage::ResourceLimit);
    assert_eq!(
        error.limit_error(),
        Some(FrontendLimitError::SourceBytesExceeded {
            actual: source.len(),
            limit: source.len() - 1,
        })
    );
    assert_eq!(
        error.diagnostics()[0].code,
        FrontendDiagnosticCode::SourceBytesExceeded
    );
    assert!(!error.parser_panicked());
    assert_eq!(error.unsupported_goal(), None);

    let callback_called = AtomicBool::new(false);
    let error = with_parsed_program(source, options, |_| {
        callback_called.store(true, Ordering::Relaxed);
    })
    .expect_err("callback entry must enforce the same limit");
    assert_eq!(error.stage(), DiagnosticStage::ResourceLimit);
    assert!(!callback_called.load(Ordering::Relaxed));

    let mut sources = SourceRegistry::new();
    let source_id = sources
        .register("oversized.js", source)
        .expect("registered source");
    let callback_called = AtomicBool::new(false);
    let error = with_registered_program(&sources, &source_id, options, |_| {
        callback_called.store(true, Ordering::Relaxed);
    })
    .expect_err("registered entry must enforce the same limit");
    assert!(!callback_called.load(Ordering::Relaxed));
    let RegisteredFrontendError::Diagnostics(diagnostics) = error else {
        panic!("expected a registered limit diagnostic");
    };
    assert_eq!(diagnostics.stage(), DiagnosticStage::ResourceLimit);
    assert_eq!(
        diagnostics.limit_error(),
        Some(FrontendLimitError::SourceBytesExceeded {
            actual: source.len(),
            limit: source.len() - 1,
        })
    );
    assert_eq!(
        diagnostics.diagnostics()[0].code().as_str(),
        FrontendDiagnosticCode::SourceBytesExceeded.as_str()
    );

    let parameters = [SourceFragment::new("abc")];
    let dynamic_source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new("def"),
    );
    let callback_called = AtomicBool::new(false);
    let error = with_dynamic_function_source(dynamic_source, FrontendLimits::new(5), |_, _| {
        callback_called.store(true, Ordering::Relaxed);
    })
    .expect_err("dynamic fragments must be limited before wrapper preparation");
    assert!(!callback_called.load(Ordering::Relaxed));
    assert_eq!(
        error.limit_error(),
        Some(FrontendLimitError::SourceBytesExceeded {
            actual: 6,
            limit: 5,
        })
    );
}

#[test]
fn ordinary_parse_never_treats_naked_source_as_a_dynamic_function() {
    for kind in [
        DynamicFunctionKind::Function,
        DynamicFunctionKind::GeneratorFunction,
        DynamicFunctionKind::AsyncFunction,
        DynamicFunctionKind::AsyncGeneratorFunction,
    ] {
        let allocator = Allocator::new();
        let error = parse(
            &allocator,
            "return value;",
            FrontendOptions::for_goal(CompilationGoal::DynamicFunction(kind)),
        )
        .expect_err("dynamic function fragments require the dedicated wrapper entry");

        assert_eq!(error.stage(), DiagnosticStage::CompilationGoal);
        assert_eq!(
            error.unsupported_goal(),
            Some(UnsupportedCompilationGoal::DynamicFunction(kind))
        );
        assert_eq!(
            error.diagnostics()[0].code,
            FrontendDiagnosticCode::UnsupportedCompilationGoal
        );
        assert!(
            error.diagnostics()[0]
                .message
                .contains(&format!("kind={kind}"))
        );
        assert!(
            error.diagnostics()[0]
                .message
                .contains("with_dynamic_function_source")
        );
    }
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

fn assert_profile_rejection(source: &str, mode: ParseMode, expected_code: FrontendDiagnosticCode) {
    let allocator = Allocator::new();
    let error = parse(&allocator, source, FrontendOptions::new(mode))
        .expect_err("syntax outside the QuickJS profile must be rejected");

    assert_eq!(error.stage(), DiagnosticStage::Profile, "{source}");
    assert!(!error.parser_panicked(), "{source}");
    assert!(
        error
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == expected_code),
        "{source}: {error:?}"
    );
    assert!(
        error
            .diagnostics()
            .iter()
            .all(|diagnostic| !diagnostic.message.is_empty()),
        "{source}: canonical messages must be retained"
    );

    let source_len = u32::try_from(source.len()).expect("test source fits in an Oxc span");
    assert!(error.diagnostics().iter().all(|diagnostic| {
        diagnostic
            .labels
            .iter()
            .any(|label| label.span.start < label.span.end && label.span.end <= source_len)
    }));
}

const QUICKJS_MAX_FIXED_CALL_ARGUMENTS: usize = u16::MAX as usize;

fn invocation_source(opening: &str, fixed_arguments: usize, tail: Option<&str>) -> String {
    let estimated_arguments = fixed_arguments
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(tail.map_or(0, str::len)))
        .expect("test source size");
    let mut source = String::with_capacity(opening.len() + estimated_arguments + 3);
    source.push_str(opening);
    for index in 0..fixed_arguments {
        if index != 0 {
            source.push(',');
        }
        source.push('0');
    }
    if let Some(tail) = tail {
        if fixed_arguments != 0 {
            source.push(',');
        }
        source.push_str(tail);
    }
    source.push_str(");");
    source
}

fn spread_first_invocation_source(opening: &str, trailing_arguments: usize) -> String {
    let mut source = String::with_capacity(opening.len() + trailing_arguments * 2 + 12);
    source.push_str(opening);
    source.push_str("...values");
    for _ in 0..trailing_arguments {
        source.push_str(",0");
    }
    source.push_str(");");
    source
}

fn assert_script_accepts(source: &str) {
    with_parsed_program(source, FrontendOptions::new(ParseMode::Script), |_| ())
        .expect("QuickJS-compatible invocation must be accepted");
}

fn assert_too_many_call_arguments(source: &str, offending_text: &str) {
    let allocator = Allocator::new();
    let Err(error) = parse(&allocator, source, FrontendOptions::new(ParseMode::Script)) else {
        panic!("QuickJS fixed call-argument prefix must be bounded");
    };

    assert_eq!(error.stage(), DiagnosticStage::Profile);
    let diagnostic = error
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code == FrontendDiagnosticCode::TooManyCallArguments)
        .expect("QuickJS-compatible call argument diagnostic");
    assert_eq!(diagnostic.message, "Too many call arguments");
    assert_eq!(diagnostic.labels.len(), 1);
    let span = diagnostic.labels[0].span;
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        offending_text
    );
}

#[test]
fn quickjs_fixed_call_argument_prefix_limit_matches_spread_behavior() {
    assert_script_accepts(&invocation_source(
        "callee(",
        QUICKJS_MAX_FIXED_CALL_ARGUMENTS,
        None,
    ));
    assert_too_many_call_arguments(
        &invocation_source(
            "callee(",
            QUICKJS_MAX_FIXED_CALL_ARGUMENTS,
            Some("overflow"),
        ),
        "overflow",
    );
    assert_too_many_call_arguments(
        &invocation_source(
            "callee(",
            QUICKJS_MAX_FIXED_CALL_ARGUMENTS,
            Some("...values"),
        ),
        "...values",
    );
    assert_script_accepts(&spread_first_invocation_source(
        "callee(",
        QUICKJS_MAX_FIXED_CALL_ARGUMENTS + 1,
    ));
}

#[test]
fn quickjs_fixed_new_argument_prefix_uses_the_same_limit() {
    assert_script_accepts(&invocation_source(
        "new Constructor(",
        QUICKJS_MAX_FIXED_CALL_ARGUMENTS,
        None,
    ));
    assert_too_many_call_arguments(
        &invocation_source(
            "new Constructor(",
            QUICKJS_MAX_FIXED_CALL_ARGUMENTS,
            Some("overflow"),
        ),
        "overflow",
    );
}

#[test]
fn rejects_explicit_resource_management_outside_the_quickjs_profile() {
    for (source, mode, expected_code) in [
        (
            "using resource = acquire();",
            ParseMode::Script,
            FrontendDiagnosticCode::UnsupportedUsingDeclaration,
        ),
        (
            "async function collect() { await using resource = acquire(); }",
            ParseMode::Script,
            FrontendDiagnosticCode::UnsupportedAwaitUsingDeclaration,
        ),
    ] {
        assert_profile_rejection(source, mode, expected_code);
    }
}

#[test]
fn rejects_import_phases_outside_the_quickjs_profile() {
    for (source, mode, expected_code) in [
        (
            "import source wasm from './module.wasm';",
            ParseMode::Module,
            FrontendDiagnosticCode::UnsupportedImportSource,
        ),
        (
            "const wasm = import.source('./module.wasm');",
            ParseMode::Script,
            FrontendDiagnosticCode::UnsupportedImportSource,
        ),
    ] {
        assert_profile_rejection(source, mode, expected_code);
    }
}

#[test]
fn rejects_decorators_and_class_accessor_declarations() {
    for (source, expected_code) in [
        (
            "@sealed class Example {}",
            FrontendDiagnosticCode::UnsupportedDecorator,
        ),
        (
            "class Example { accessor value = 1; }",
            FrontendDiagnosticCode::UnsupportedClassAccessor,
        ),
    ] {
        assert_profile_rejection(source, ParseMode::Script, expected_code);
    }
}

#[test]
fn rejects_legacy_import_assertions_but_accepts_import_attributes() {
    assert_profile_rejection(
        "import data from './data.json' assert { type: 'json' };",
        ParseMode::Module,
        FrontendDiagnosticCode::UnsupportedLegacyImportAssertion,
    );

    let allocator = Allocator::new();
    let unit = parse(
        &allocator,
        "import data from './data.json' with { type: 'json' }; export default data;",
        FrontendOptions::new(ParseMode::Module),
    )
    .expect("QuickJS supports import attributes using `with`");
    assert_eq!(unit.program().body.len(), 2);
}

#[test]
fn accepts_promise_try_from_the_quickjs_es2025_profile() {
    let allocator = Allocator::new();
    let unit = parse(
        &allocator,
        "const result = Promise.try(() => 42);",
        FrontendOptions::new(ParseMode::Script),
    )
    .expect("QuickJS 2026-06-04 supports Promise.try");

    assert_eq!(unit.program().body.len(), 1);
}

#[test]
fn regexp_literal_grammar_failures_are_semantic_early_errors() {
    let allocator = Allocator::new();
    let error = parse(
        &allocator,
        "const pattern = /(/;",
        FrontendOptions::new(ParseMode::Script),
    )
    .expect_err("RegExp pattern grammar is an ECMAScript early error");
    assert_eq!(error.stage(), DiagnosticStage::Semantic);
    assert_eq!(
        error.diagnostics()[0].code,
        FrontendDiagnosticCode::InvalidRegExpLiteral
    );
    assert_eq!(error.diagnostics()[0].labels[0].span, Span::new(16, 19));

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
        |unit| {
            assert_eq!(
                unit.goal(),
                CompilationGoal::GlobalScript(GlobalScriptGoal::new())
            );
            unit.program().body.len()
        },
    )
    .expect("callback parse");

    assert_eq!(statement_count, 2);
}

#[test]
fn callback_api_runs_parser_and_semantic_work_in_an_isolated_thread() {
    let caller = thread::current().id();
    let worker = with_parsed_program(
        "let value = 1;",
        FrontendOptions::new(ParseMode::Script),
        |_| thread::current().id(),
    )
    .expect("isolated callback parse");

    assert_ne!(worker, caller);
}

#[test]
fn registered_source_callback_keeps_the_ast_in_a_short_lived_arena() {
    let mut sources = SourceRegistry::new();
    let source_id = sources
        .register("registered.js", "let first = 1; let second = 2;")
        .expect("source");

    let statement_count = with_registered_program(
        &sources,
        &source_id,
        FrontendOptions::new(ParseMode::Script),
        |unit| {
            assert_eq!(
                unit.goal(),
                CompilationGoal::GlobalScript(GlobalScriptGoal::new())
            );
            unit.program().body.len()
        },
    )
    .expect("registered parse");

    assert_eq!(statement_count, 2);
}

#[test]
fn registered_source_errors_distinguish_foreign_ids_from_javascript_diagnostics() {
    let mut owner = SourceRegistry::new();
    let foreign_id = owner.register("foreign.js", "let x;").expect("foreign");
    let mut sources = SourceRegistry::new();
    sources.register("local.js", "let y;").expect("local");

    let error = with_registered_program(
        &sources,
        &foreign_id,
        FrontendOptions::new(ParseMode::Script),
        |_| (),
    )
    .expect_err("foreign source ID");
    assert!(matches!(
        error,
        RegisteredFrontendError::Source(FrontendSourceError::Registry(
            SourceError::ForeignSourceId
        ))
    ));

    let invalid_id = sources
        .register("invalid.js", "const missing = ;")
        .expect("invalid source");
    let error = with_registered_program(
        &sources,
        &invalid_id,
        FrontendOptions::new(ParseMode::Script),
        |_| (),
    )
    .expect_err("invalid JavaScript");
    let RegisteredFrontendError::Diagnostics(diagnostics) = error else {
        panic!("expected JavaScript diagnostics");
    };
    assert_eq!(diagnostics.stage(), DiagnosticStage::Parser);
    assert_eq!(diagnostics.source_id(), &invalid_id);
    assert!(diagnostics.diagnostics().iter().all(|diagnostic| {
        diagnostic.code().as_str() == FrontendDiagnosticCode::OxcParser.as_str()
    }));
}

#[test]
fn registered_diagnostics_preserve_the_structured_unsupported_goal() {
    let mut sources = SourceRegistry::new();
    let source_id = sources
        .register("dynamic-function-body.js", "return 1;")
        .expect("source");

    let error = with_registered_program(
        &sources,
        &source_id,
        FrontendOptions::for_goal(CompilationGoal::DynamicFunction(
            DynamicFunctionKind::Function,
        )),
        |_| (),
    )
    .expect_err("naked dynamic Function parsing requires the fragment adapter");
    let RegisteredFrontendError::Diagnostics(diagnostics) = error else {
        panic!("expected goal diagnostics");
    };

    assert_eq!(diagnostics.stage(), DiagnosticStage::CompilationGoal);
    assert_eq!(
        diagnostics.unsupported_goal(),
        Some(UnsupportedCompilationGoal::DynamicFunction(
            DynamicFunctionKind::Function
        ))
    );
    assert_eq!(
        diagnostics.diagnostics()[0].code().as_str(),
        FrontendDiagnosticCode::UnsupportedCompilationGoal.as_str()
    );
    assert!(diagnostics.diagnostics()[0].help().is_none());
    assert!(
        diagnostics.diagnostics()[0]
            .message()
            .contains("kind=function")
    );
    assert!(
        diagnostics.diagnostics()[0]
            .message()
            .contains("with_dynamic_function_source")
    );
}

#[test]
fn registered_multibyte_diagnostics_render_with_validated_miette_spans() {
    let source_text = "const π = ;\n";
    let mut sources = SourceRegistry::new();
    let source_id = sources
        .register("multibyte.js", source_text)
        .expect("source");
    let error = with_registered_program(
        &sources,
        &source_id,
        FrontendOptions::new(ParseMode::Script),
        |_| (),
    )
    .expect_err("missing initializer");
    let RegisteredFrontendError::Diagnostics(diagnostics) = error else {
        panic!("expected parser diagnostics");
    };

    assert!(!diagnostics.diagnostics().is_empty());
    for diagnostic in diagnostics.diagnostics() {
        for label in diagnostic.labels() {
            assert!(label.span().bytes().end() as usize <= source_text.len());
        }
    }
    let rendered =
        render_pretty(&sources, &diagnostics.diagnostics()[0]).expect("Miette rendering");
    assert!(rendered.contains("multibyte.js"));
    assert!(rendered.contains("const π = ;"));
    assert!(rendered.contains(FrontendDiagnosticCode::OxcParser.as_str()));
}

#[test]
fn profile_diagnostic_codes_convert_without_message_matching() {
    let mut sources = SourceRegistry::new();
    let source_id = sources
        .register("profile.js", "using resource = acquire();")
        .expect("source");
    let error = with_registered_program(
        &sources,
        &source_id,
        FrontendOptions::new(ParseMode::Script),
        |_| (),
    )
    .expect_err("unsupported profile syntax");
    let RegisteredFrontendError::Diagnostics(diagnostics) = error else {
        panic!("expected profile diagnostics");
    };

    assert_eq!(diagnostics.stage(), DiagnosticStage::Profile);
    assert!(diagnostics.diagnostics().iter().any(|diagnostic| {
        diagnostic.code().as_str() == FrontendDiagnosticCode::UnsupportedUsingDeclaration.as_str()
    }));
    assert!(
        diagnostics
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.help().is_some())
    );
}
