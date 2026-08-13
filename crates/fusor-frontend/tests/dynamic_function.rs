use fusor_frontend::{
    CompilationGoal, DiagnosticStage, DynamicFunctionFragmentRole, DynamicFunctionKind,
    DynamicFunctionMapError, DynamicFunctionMappedSource, DynamicFunctionSource,
    DynamicFunctionSpanBias, DynamicFunctionSyntheticKind, FrontendDiagnosticCode,
    FrontendLimitError, FrontendLimits, SourceFragment, Span, with_dynamic_function_source,
    with_dynamic_function_source_and_prepared,
};

const KINDS_AND_PREFIXES: [(DynamicFunctionKind, &str); 4] = [
    (DynamicFunctionKind::Function, "(function anonymous("),
    (
        DynamicFunctionKind::GeneratorFunction,
        "(function* anonymous(",
    ),
    (
        DynamicFunctionKind::AsyncFunction,
        "(async function anonymous(",
    ),
    (
        DynamicFunctionKind::AsyncGeneratorFunction,
        "(async function* anonymous(",
    ),
];

#[test]
fn ownership_preserving_entry_returns_the_exact_prepared_wrapper() {
    let parameters = [SourceFragment::new("value").with_origin("parameter")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new("return value;").with_origin("body"),
    );

    let (generated_len, prepared) = with_dynamic_function_source_and_prepared(
        source,
        FrontendLimits::default(),
        |unit, map| {
            assert_eq!(
                unit.goal(),
                CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            );
            map.fragment_map().generated_len()
        },
    )
    .expect("the complete generated Script parses");

    assert_eq!(prepared.fragment_map().generated_len(), generated_len);
    assert_eq!(
        prepared.generated_source(),
        "(function anonymous(value\n) {\nreturn value;\n})"
    );
}

#[test]
fn constructs_the_exact_quickjs_wrapper_for_all_four_families() {
    let parameters = [
        SourceFragment::new("left = 1"),
        SourceFragment::new("right = 2"),
    ];
    let no_parameters = [];
    let body = SourceFragment::new("return left + right;");

    for (kind, prefix) in KINDS_AND_PREFIXES {
        let expected = format!("{prefix}left = 1,right = 2\n) {{\nreturn left + right;\n}})");
        let source = DynamicFunctionSource::new(kind, &parameters, body);
        with_dynamic_function_source(source, FrontendLimits::default(), |unit, prepared| {
            assert_eq!(unit.goal(), CompilationGoal::DynamicFunction(kind));
            assert!(unit.program().source_type.is_script());
            assert_eq!(prepared.generated_source(), expected);
            assert_eq!(
                usize::try_from(prepared.fragment_map().generated_len()).unwrap(),
                expected.len()
            );
        })
        .expect("the exact wrapper must parse as a complete Script");

        let expected_empty = format!("{prefix}\n) {{\n\n}})");
        let source = DynamicFunctionSource::new(kind, &no_parameters, SourceFragment::new(""));
        with_dynamic_function_source(source, FrontendLimits::default(), |_, prepared| {
            assert_eq!(prepared.generated_source(), expected_empty);
        })
        .expect("the exact empty wrapper must parse");
    }
}

#[test]
fn parses_the_complete_script_and_preserves_quickjs_wrapper_escape() {
    let parameters = [];
    let body = SourceFragment::new("}), ({");

    for (kind, prefix) in KINDS_AND_PREFIXES {
        let expected = format!("{prefix}\n) {{\n}}), ({{\n}})");
        let source = DynamicFunctionSource::new(kind, &parameters, body);
        with_dynamic_function_source(source, FrontendLimits::default(), |unit, prepared| {
            assert_eq!(unit.goal(), CompilationGoal::DynamicFunction(kind));
            assert_eq!(prepared.generated_source(), expected);
            assert_eq!(unit.program().body.len(), 1);
        })
        .expect("QuickJS deliberately permits escaping the synthetic wrapper");
    }
}

#[test]
fn family_prefixes_select_their_contextual_parameter_and_body_grammars() {
    let yield_parameter = [SourceFragment::new("yield")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &yield_parameter,
        SourceFragment::new("return yield;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect("yield is an ordinary identifier in a normal dynamic function");

    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::GeneratorFunction,
        &yield_parameter,
        SourceFragment::new("return 0;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect_err("yield is contextual in a dynamic generator parameter list");

    let await_parameter = [SourceFragment::new("await")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &await_parameter,
        SourceFragment::new("return await;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect("await is an ordinary identifier in a normal dynamic function");

    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::AsyncFunction,
        &await_parameter,
        SourceFragment::new("return 0;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect_err("await is contextual in a dynamic async parameter list");

    let no_parameters = [];
    for (kind, body) in [
        (DynamicFunctionKind::GeneratorFunction, "yield 1;"),
        (DynamicFunctionKind::AsyncFunction, "return await 1;"),
        (
            DynamicFunctionKind::AsyncGeneratorFunction,
            "yield await 1;",
        ),
    ] {
        let source = DynamicFunctionSource::new(kind, &no_parameters, SourceFragment::new(body));
        with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
            .expect("the family prefix must enable its body grammar");
    }
}

#[test]
fn parameter_fragments_are_comma_joined_without_comment_sanitization() {
    let line_comment_parameters = [SourceFragment::new("a//"), SourceFragment::new("b")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &line_comment_parameters,
        SourceFragment::new("return a;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, prepared| {
        assert_eq!(
            prepared.generated_source(),
            "(function anonymous(a//,b\n) {\nreturn a;\n})"
        );
    })
    .expect("the wrapper newline terminates the line comment exactly as QuickJS does");

    let closed_block_comment_parameters = [SourceFragment::new("a/*"), SourceFragment::new("*/")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &closed_block_comment_parameters,
        SourceFragment::new("return a;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, prepared| {
        assert_eq!(
            prepared.generated_source(),
            "(function anonymous(a/*,*/\n) {\nreturn a;\n})"
        );
    })
    .expect("the inserted comma remains inside the closed block comment");

    let split_block_comment_parameters = [SourceFragment::new("a/*"), SourceFragment::new("*/b")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &split_block_comment_parameters,
        SourceFragment::new("return a;"),
    );
    let error = with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect_err("the comma is inside the split block comment, leaving invalid parameters");
    assert_eq!(error.stage(), DiagnosticStage::Parser);
    assert_eq!(
        error
            .prepared_source()
            .expect("generated parser errors retain their wrapper")
            .generated_source(),
        "(function anonymous(a/*,*/b\n) {\nreturn a;\n})"
    );
}

#[test]
fn leading_and_trailing_empty_parameters_remain_observably_distinct() {
    let leading_empty = [SourceFragment::new(""), SourceFragment::new("x")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &leading_empty,
        SourceFragment::new("return x;"),
    );
    let error = with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect_err("a leading elision is not a formal parameter");
    assert_eq!(error.stage(), DiagnosticStage::Parser);
    assert_eq!(
        error.prepared_source().unwrap().generated_source(),
        "(function anonymous(,x\n) {\nreturn x;\n})"
    );

    let trailing_empty = [SourceFragment::new("x"), SourceFragment::new("")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &trailing_empty,
        SourceFragment::new("return x;"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, prepared| {
        assert_eq!(
            prepared.generated_source(),
            "(function anonymous(x,\n) {\nreturn x;\n})"
        );
    })
    .expect("a trailing empty fragment becomes a permitted trailing comma");
}

#[test]
fn strict_duplicate_parameters_are_rejected_after_exact_wrapping() {
    let parameters = [SourceFragment::new("value"), SourceFragment::new("value")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new("\"use strict\"; return value;"),
    );
    let error = with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect_err("strict dynamic functions reject duplicate parameter names");
    assert!(matches!(
        error.stage(),
        DiagnosticStage::Parser | DiagnosticStage::Semantic
    ));
    assert_eq!(
        error.prepared_source().unwrap().generated_source(),
        "(function anonymous(value,value\n) {\n\"use strict\"; return value;\n})"
    );
}

#[test]
fn fragment_map_owns_origins_and_splits_synthetic_and_copied_ranges() {
    let parameters = [
        SourceFragment::new("π").with_origin("parameter-pi"),
        SourceFragment::new("").with_origin("empty-parameter"),
    ];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new("return π;").with_origin("body"),
    );

    with_dynamic_function_source(source, FrontendLimits::default(), |_, prepared| {
        assert_eq!(
            prepared.generated_source(),
            "(function anonymous(π,\n) {\nreturn π;\n})"
        );
        let map = prepared.fragment_map();
        let full = map
            .map_generated_span(
                Span::new(0, map.generated_len()),
                DynamicFunctionSpanBias::Later,
            )
            .expect("full generated wrapper");
        assert_eq!(full.len(), 6);
        assert_eq!(
            full[0].source(),
            DynamicFunctionMappedSource::Synthetic(DynamicFunctionSyntheticKind::Prefix)
        );
        assert!(matches!(
            full[1].source(),
            DynamicFunctionMappedSource::Copied {
                role: DynamicFunctionFragmentRole::Parameter { index: 0 },
                original_range,
                origin: Some("parameter-pi"),
                text: "π",
            } if original_range.start() == 0 && original_range.end() == 2
        ));
        assert_eq!(
            (
                full[1].generated_range().start(),
                full[1].generated_range().end()
            ),
            (20, 22)
        );
        assert_eq!(
            full[2].source(),
            DynamicFunctionMappedSource::Synthetic(
                DynamicFunctionSyntheticKind::ParameterSeparator
            )
        );
        assert_eq!(
            full[3].source(),
            DynamicFunctionMappedSource::Synthetic(
                DynamicFunctionSyntheticKind::ParametersBodySeparator
            )
        );
        assert!(matches!(
            full[4].source(),
            DynamicFunctionMappedSource::Copied {
                role: DynamicFunctionFragmentRole::Body,
                origin: Some("body"),
                text: "return π;",
                ..
            }
        ));
        assert_eq!(
            full[5].source(),
            DynamicFunctionMappedSource::Synthetic(DynamicFunctionSyntheticKind::Suffix)
        );
    })
    .expect("valid dynamic function");
}

#[test]
fn fragment_map_splits_crossing_spans_and_biases_zero_width_boundaries() {
    let parameters = [
        SourceFragment::new("π").with_origin("parameter-pi"),
        SourceFragment::new("").with_origin("empty-parameter"),
    ];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new("return π;").with_origin("body"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, prepared| {
        let map = prepared.fragment_map();
        let mapped = map
            .map_generated_span(Span::new(19, 24), DynamicFunctionSpanBias::Later)
            .expect("valid cross-segment span");
        assert_eq!(mapped.len(), 4);
        assert_eq!(
            (
                mapped[0].generated_range().start(),
                mapped[0].generated_range().end()
            ),
            (19, 20)
        );
        assert_eq!(
            mapped[0].source(),
            DynamicFunctionMappedSource::Synthetic(DynamicFunctionSyntheticKind::Prefix)
        );
        assert!(matches!(
            mapped[1].source(),
            DynamicFunctionMappedSource::Copied {
                role: DynamicFunctionFragmentRole::Parameter { index: 0 },
                original_range,
                origin: Some("parameter-pi"),
                text: "π",
            } if original_range.start() == 0 && original_range.end() == 2
        ));
        assert_eq!(
            mapped[2].source(),
            DynamicFunctionMappedSource::Synthetic(
                DynamicFunctionSyntheticKind::ParameterSeparator
            )
        );
        assert_eq!(
            mapped[3].source(),
            DynamicFunctionMappedSource::Synthetic(
                DynamicFunctionSyntheticKind::ParametersBodySeparator
            )
        );

        let empty = map
            .map_generated_span(Span::new(23, 23), DynamicFunctionSpanBias::Earlier)
            .expect("empty fragment anchor");
        assert!(matches!(
            empty.as_slice(),
            [mapped] if matches!(
                mapped.source(),
                DynamicFunctionMappedSource::Copied {
                    role: DynamicFunctionFragmentRole::Parameter { index: 1 },
                    original_range,
                    origin: Some("empty-parameter"),
                    text: "",
                } if original_range.is_empty()
            )
        ));

        let earlier = map
            .map_generated_span(Span::new(20, 20), DynamicFunctionSpanBias::Earlier)
            .unwrap();
        let later = map
            .map_generated_span(Span::new(20, 20), DynamicFunctionSpanBias::Later)
            .unwrap();
        assert_eq!(
            earlier[0].source(),
            DynamicFunctionMappedSource::Synthetic(DynamicFunctionSyntheticKind::Prefix)
        );
        assert!(matches!(
            later[0].source(),
            DynamicFunctionMappedSource::Copied {
                role: DynamicFunctionFragmentRole::Parameter { index: 0 },
                original_range,
                ..
            } if original_range.is_empty()
        ));
    })
    .expect("valid dynamic function");
}

#[test]
fn fragment_map_rejects_invalid_utf8_boundaries_and_out_of_range_spans() {
    let parameters = [SourceFragment::new("π")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new(""),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |_, prepared| {
        let map = prepared.fragment_map();
        assert_eq!(
            map.map_generated_span(Span::new(21, 22), DynamicFunctionSpanBias::Later),
            Err(DynamicFunctionMapError::InvalidUtf8Boundary { offset: 21 })
        );
        assert_eq!(
            map.map_generated_span(
                Span::new(map.generated_len(), map.generated_len() + 1),
                DynamicFunctionSpanBias::Later,
            ),
            Err(DynamicFunctionMapError::InvalidGeneratedSpan {
                start: map.generated_len(),
                end: map.generated_len() + 1,
                generated_len: map.generated_len(),
            })
        );
    })
    .expect("valid dynamic function");
}

#[test]
fn parse_failures_retain_the_prepared_source_and_fragment_map() {
    let parameters = [SourceFragment::new("value").with_origin("parameter")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::AsyncFunction,
        &parameters,
        SourceFragment::new("function {").with_origin("body"),
    );
    let error = with_dynamic_function_source(source, FrontendLimits::default(), |_, _| ())
        .expect_err("malformed body");
    assert_eq!(error.stage(), DiagnosticStage::Parser);
    let prepared = error
        .prepared_source()
        .expect("generated diagnostics must retain their exact source");
    assert_eq!(
        prepared.generated_source(),
        "(async function anonymous(value\n) {\nfunction {\n})"
    );
    let body_start = u32::try_from("(async function anonymous(value\n) {\n".len()).unwrap();
    let mapped = prepared
        .fragment_map()
        .map_generated_span(
            Span::new(body_start, body_start + 1),
            DynamicFunctionSpanBias::Later,
        )
        .expect("body byte maps after parse failure");
    assert!(matches!(
        mapped.as_slice(),
        [mapped] if matches!(
            mapped.source(),
            DynamicFunctionMappedSource::Copied {
                role: DynamicFunctionFragmentRole::Body,
                origin: Some("body"),
                ..
            }
        )
    ));
}

#[test]
fn dynamic_wrapper_preflight_has_structured_resource_failures() {
    let parameters = [SourceFragment::new("a"), SourceFragment::new("b")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new(""),
    );
    let error = with_dynamic_function_source(
        source,
        FrontendLimits::default().with_max_dynamic_function_fragments(2),
        |_, _| (),
    )
    .expect_err("two parameters plus the body exceed two records");
    assert_eq!(error.prepared_source(), None);
    assert_eq!(
        error.limit_error(),
        Some(FrontendLimitError::DynamicFunctionFragmentsExceeded {
            actual: 3,
            limit: 2,
        })
    );
    assert_eq!(
        error.diagnostics()[0].code,
        FrontendDiagnosticCode::DynamicFunctionFragmentsExceeded
    );

    let parameters = [SourceFragment::new("").with_origin("abc")];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new("").with_origin("de"),
    );
    let error = with_dynamic_function_source(
        source,
        FrontendLimits::default().with_max_dynamic_function_origin_bytes(4),
        |_, _| (),
    )
    .expect_err("origin labels exceed their independent budget");
    assert_eq!(
        error.limit_error(),
        Some(FrontendLimitError::DynamicFunctionOriginBytesExceeded {
            actual: 5,
            limit: 4,
        })
    );
    assert_eq!(
        error.diagnostics()[0].code,
        FrontendDiagnosticCode::DynamicFunctionOriginBytesExceeded
    );

    let parameters = [];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new(""),
    );
    let error = with_dynamic_function_source(source, FrontendLimits::new(27), |_, _| ())
        .expect_err("the exact empty Function wrapper is 28 bytes");
    assert_eq!(
        error.limit_error(),
        Some(FrontendLimitError::SourceBytesExceeded {
            actual: 28,
            limit: 27,
        })
    );
}
