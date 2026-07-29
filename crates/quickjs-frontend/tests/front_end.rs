use std::cell::Cell;

use quickjs_diagnostics::{SourceError, SourceRegistry, render_pretty};
use quickjs_frontend::{
    Allocator, CompilationGoal, DiagnosticStage, DirectEvalBinding, DirectEvalBindingKind,
    DirectEvalBindingLocation, DirectEvalCapabilities, DirectEvalContext, DirectEvalPrivateName,
    DirectEvalPrivateNameKind, DirectEvalScopeFrame, DirectEvalScopeKind, DirectEvalScopeSnapshot,
    DynamicFunctionKind, DynamicFunctionSource, FrontendDiagnosticCode, FrontendLimitError,
    FrontendLimits, FrontendOptions, FrontendSourceError, GlobalScriptGoal, IndirectEvalGoal,
    ParseMode, RegisteredFrontendError, SourceFragment, Span, UnsupportedCompilationGoal, parse,
    with_dynamic_function_source, with_parsed_program, with_registered_program,
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
fn compilation_goals_preserve_lossless_direct_eval_context() {
    let bindings = [DirectEvalBinding::new(
        "captured",
        DirectEvalBindingKind::FunctionDeclaration,
        true,
        false,
        DirectEvalBindingLocation::Closure { index: 3 },
    )];
    let private_names = [DirectEvalPrivateName::new(
        "value",
        DirectEvalPrivateNameKind::Field,
        false,
        true,
        true,
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
    );

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
    let private_name = direct_eval.scope_snapshot().frames()[0].private_names()[0];
    assert_eq!(private_name.kind(), DirectEvalPrivateNameKind::Field);
    assert!(private_name.is_lexical());
    assert!(private_name.is_const());
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
    assert!(direct_eval.capabilities().allows_arguments());
}

#[test]
fn global_and_eval_goals_reject_top_level_return() {
    let source = "return arguments[0] + 1;";
    let allocator = Allocator::new();

    for goal in [
        CompilationGoal::GlobalScript(GlobalScriptGoal::new()),
        CompilationGoal::IndirectEval(IndirectEvalGoal::new()),
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
fn contextual_goals_fail_before_oxc_until_their_adapters_are_faithful() {
    let direct_capabilities = DirectEvalCapabilities::new()
        .with_strict(true)
        .with_new_target(true)
        .with_super_call(true)
        .with_arguments_allowed(true);
    let direct_context =
        DirectEvalContext::new(direct_capabilities, DirectEvalScopeSnapshot::default());
    let cases = [
        (
            CompilationGoal::GlobalScript(GlobalScriptGoal::new().with_forced_strict(true)),
            UnsupportedCompilationGoal::GlobalScript(
                GlobalScriptGoal::new().with_forced_strict(true),
            ),
            "force_strict=true, allow_top_level_await=false",
        ),
        (
            CompilationGoal::GlobalScript(GlobalScriptGoal::new().with_top_level_await(true)),
            UnsupportedCompilationGoal::GlobalScript(
                GlobalScriptGoal::new().with_top_level_await(true),
            ),
            "force_strict=false, allow_top_level_await=true",
        ),
        (
            CompilationGoal::IndirectEval(IndirectEvalGoal::new().with_forced_strict(true)),
            UnsupportedCompilationGoal::IndirectEval(
                IndirectEvalGoal::new().with_forced_strict(true),
            ),
            "force_strict=true",
        ),
        (
            CompilationGoal::DirectEval(direct_context),
            UnsupportedCompilationGoal::DirectEval(direct_capabilities),
            "strict=true, new_target=true, super_property=false, super_call=true, arguments_allowed=true",
        ),
    ];

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

    let callback_called = Cell::new(false);
    let error = with_parsed_program(source, options, |_| callback_called.set(true))
        .expect_err("callback entry must enforce the same limit");
    assert_eq!(error.stage(), DiagnosticStage::ResourceLimit);
    assert!(!callback_called.get());

    let mut sources = SourceRegistry::new();
    let source_id = sources
        .register("oversized.js", source)
        .expect("registered source");
    let callback_called = Cell::new(false);
    let error = with_registered_program(&sources, &source_id, options, |_| {
        callback_called.set(true);
    })
    .expect_err("registered entry must enforce the same limit");
    assert!(!callback_called.get());
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
    let callback_called = Cell::new(false);
    let error = with_dynamic_function_source(dynamic_source, FrontendLimits::new(5), |_, _| {
        callback_called.set(true);
    })
    .expect_err("dynamic fragments must be limited before wrapper preparation");
    assert!(!callback_called.get());
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
            "import defer * as dependency from './dep.js';",
            ParseMode::Module,
            FrontendDiagnosticCode::UnsupportedImportDefer,
        ),
        (
            "const wasm = import.source('./module.wasm');",
            ParseMode::Script,
            FrontendDiagnosticCode::UnsupportedImportSource,
        ),
        (
            "const dependency = import.defer('./dep.js');",
            ParseMode::Script,
            FrontendDiagnosticCode::UnsupportedImportDefer,
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
fn leaves_regexp_pattern_validation_to_the_quickjs_runtime_layer() {
    let allocator = Allocator::new();
    let unit = parse(
        &allocator,
        "const pattern = /(/;",
        FrontendOptions::new(ParseMode::Script),
    )
    .expect("the front end only identifies the RegExp literal boundary");

    assert_eq!(unit.program().body.len(), 1);

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
