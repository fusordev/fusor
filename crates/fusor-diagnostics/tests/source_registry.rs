use fusor_diagnostics::{ColumnEncoding, SourceError, SourceRegistry};

#[test]
fn byte_offsets_map_to_explicit_columns_and_ecmascript_line_breaks() {
    let text = "α😀\r\nx\u{2028}猫\u{2029}";
    let mut sources = SourceRegistry::new();
    let source = sources.register("unicode.js", text).expect("valid source");

    let utf8 = sources
        .position_with_encoding(&source, 6, ColumnEncoding::Utf8Byte)
        .expect("boundary after emoji");
    let scalar = sources
        .position_with_encoding(&source, 6, ColumnEncoding::UnicodeScalar)
        .expect("boundary after emoji");
    let utf16 = sources
        .position_with_encoding(&source, 6, ColumnEncoding::Utf16CodeUnit)
        .expect("boundary after emoji");
    assert_eq!((utf8.line(), utf8.column()), (1, 7));
    assert_eq!((scalar.line(), scalar.column()), (1, 3));
    assert_eq!((utf16.line(), utf16.column()), (1, 4));

    let after_crlf = sources.position(&source, 8).expect("after CRLF");
    assert_eq!((after_crlf.line(), after_crlf.column()), (2, 1));
    let after_line_separator = sources.position(&source, 12).expect("after U+2028");
    assert_eq!(
        (after_line_separator.line(), after_line_separator.column()),
        (3, 1)
    );
    let after_paragraph_separator = sources
        .position(&source, text.len())
        .expect("end of source");
    assert_eq!(
        (
            after_paragraph_separator.line(),
            after_paragraph_separator.column()
        ),
        (4, 1)
    );
}

#[test]
fn spans_reject_invalid_ranges_and_utf8_boundaries() {
    let mut sources = SourceRegistry::new();
    let source = sources.register("span.js", "éx").expect("valid source");

    assert!(matches!(
        sources.span(&source, 2, 1),
        Err(SourceError::InvalidRange { .. })
    ));
    assert!(matches!(
        sources.span(&source, 0, 4),
        Err(SourceError::InvalidRange { .. })
    ));
    assert_eq!(
        sources.span(&source, 1, 2),
        Err(SourceError::InvalidUtf8Boundary { offset: 1 })
    );
    assert_eq!(
        sources.position(&source, 1),
        Err(SourceError::InvalidUtf8Boundary { offset: 1 })
    );
}

#[test]
fn source_ids_cannot_cross_registry_boundaries() {
    let mut first = SourceRegistry::new();
    let first_id = first.register("same.js", "first").expect("first source");
    let mut second = SourceRegistry::new();
    second.register("same.js", "second").expect("second source");

    assert_eq!(
        second.source(&first_id).expect_err("foreign ID"),
        SourceError::ForeignSourceId
    );
}

#[test]
fn snippets_are_line_aligned_and_keep_a_relative_highlight() {
    let text = "first\r\nsecond\nthird";
    let mut sources = SourceRegistry::new();
    let source = sources.register("snippet.js", text).expect("source");
    let start = text.find("second").expect("needle");
    let span = sources
        .span(&source, start, start + "second".len())
        .expect("span");
    let snippet = sources.snippet(&span, 1, 1).expect("snippet");

    assert_eq!(snippet.source_name(), "snippet.js");
    assert_eq!(snippet.text(), text);
    assert_eq!(
        &snippet.text()[snippet.highlight().start() as usize..snippet.highlight().end() as usize],
        "second"
    );
    assert_eq!(
        (snippet.starts_at().line(), snippet.starts_at().column()),
        (1, 1)
    );
}

#[test]
fn duplicate_and_empty_source_names_are_rejected() {
    let mut sources = SourceRegistry::new();
    assert_eq!(sources.register("", ""), Err(SourceError::EmptySourceName));
    sources.register("one.js", "").expect("first registration");
    assert_eq!(
        sources.register("one.js", ""),
        Err(SourceError::DuplicateSourceName("one.js".to_owned()))
    );
}
