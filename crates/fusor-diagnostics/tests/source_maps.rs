use std::panic::{AssertUnwindSafe, catch_unwind};

use fusor_diagnostics::{SourceMap, SourceMapErrorKind, SourceMapPosition, SourceRegistry};

fn regular_map(sources: &str, names: &str, mappings: &str, sources_content: &str) -> SourceMap {
    let json = format!(
        r#"{{"version":3,"sources":{sources},"names":{names},"mappings":"{mappings}","sourcesContent":{sources_content}}}"#
    );
    SourceMap::from_slice(json.as_bytes()).expect("valid source map")
}

#[test]
fn regular_lookup_uses_greatest_lower_bound_tokens() {
    let map = regular_map(
        r#"["original.js"]"#,
        r#"["first","second"]"#,
        "AAAAA,KAAGC",
        r#"["abcdef"]"#,
    );

    let before_second = map
        .lookup(SourceMapPosition::new(0, 4))
        .expect("lookup")
        .expect("first token");
    assert_eq!(before_second.original(), SourceMapPosition::new(0, 0));
    assert_eq!(before_second.name(), Some("first"));

    let after_second = map
        .lookup(SourceMapPosition::new(0, 99))
        .expect("lookup")
        .expect("second token");
    assert_eq!(after_second.original(), SourceMapPosition::new(0, 3));
    assert_eq!(after_second.name(), Some("second"));
    assert_eq!(
        after_second.source_content().map(AsRef::as_ref),
        Some("abcdef")
    );
}

#[test]
fn indexed_embedded_maps_apply_section_offsets_and_glb_lookup() {
    let json = br#"{
        "version": 3,
        "sections": [{
            "offset": {"line": 2, "column": 5},
            "map": {
                "version": 3,
                "sources": ["original.js"],
                "names": [],
                "mappings": "AAAA",
                "sourcesContent": ["source"]
            }
        }]
    }"#;
    let map = SourceMap::from_slice(json).expect("indexed map");

    assert_eq!(
        map.lookup(SourceMapPosition::new(2, 4))
            .expect("lookup before section"),
        None
    );
    let exact = map
        .lookup(SourceMapPosition::new(2, 5))
        .expect("exact lookup")
        .expect("mapped");
    assert_eq!(exact.source_name(), "original.js");
    assert_eq!(exact.original(), SourceMapPosition::new(0, 0));
    let glb = map
        .lookup(SourceMapPosition::new(2, 12))
        .expect("GLB lookup")
        .expect("mapped");
    assert_eq!(glb.original(), SourceMapPosition::new(0, 0));
}

#[test]
fn url_only_index_sections_are_a_non_panicking_unresolved_boundary() {
    let json = br#"{
        "version": 3,
        "sections": [{
            "offset": {"line": 0, "column": 0},
            "url": "external.map"
        }]
    }"#;
    let map = SourceMap::from_slice(json).expect("standard URL section");
    let error = map
        .lookup(SourceMapPosition::new(0, 0))
        .expect_err("external section needs a host loader");
    assert_eq!(error.kind(), SourceMapErrorKind::UnresolvedSection);
    assert_eq!(error.code(), "fusor::sourcemap::unresolved_section");
}

#[test]
fn indexed_sections_must_be_strictly_ordered() {
    let json = br#"{
        "version": 3,
        "sections": [
            {
                "offset": {"line": 2, "column": 0},
                "map": {"version":3,"sources":[],"names":[],"mappings":""}
            },
            {
                "offset": {"line": 1, "column": 0},
                "map": {"version":3,"sources":[],"names":[],"mappings":""}
            }
        ]
    }"#;
    let error = SourceMap::from_slice(json).expect_err("out-of-order sections");
    assert_eq!(error.kind(), SourceMapErrorKind::InvalidDocument);
}

#[test]
fn malformed_maps_return_structured_errors_without_unwinding() {
    let cases: &[&[u8]] = &[
        b"",
        b"null",
        br#"{"version":2,"sources":[],"names":[],"mappings":""}"#,
        br#"{"version":3,"sources":[],"names":[],"mappings":"!"}"#,
        br#"{"version":3,"sources":[0],"names":[],"mappings":""}"#,
    ];

    for input in cases {
        let outcome = catch_unwind(AssertUnwindSafe(|| SourceMap::from_slice(input)));
        assert!(outcome.is_ok(), "input unwound: {input:?}");
        assert!(
            outcome.expect("caught").is_err(),
            "input accepted: {input:?}"
        );
    }

    let version_error =
        SourceMap::from_slice(br#"{"version":2,"sources":[],"names":[],"mappings":""}"#)
            .expect_err("unsupported version");
    assert_eq!(version_error.kind(), SourceMapErrorKind::UnsupportedVersion);
    assert_eq!(
        version_error.code(),
        "fusor::sourcemap::unsupported_version"
    );

    let mapping_error =
        SourceMap::from_slice(br#"{"version":3,"sources":[],"names":[],"mappings":"!"}"#)
            .expect_err("bad VLQ");
    assert_eq!(mapping_error.kind(), SourceMapErrorKind::MalformedMappings);
}

#[test]
fn a_missing_incoming_map_resolves_to_the_generated_location() {
    let mut sources = SourceRegistry::new();
    let generated = sources.register("plain.js", "let x;").expect("source");
    let resolved = sources
        .resolve_original(&generated, SourceMapPosition::new(0, 4))
        .expect("identity resolution");

    assert!(!resolved.is_mapped());
    assert_eq!(resolved.hops(), 0);
    assert_eq!(resolved.original().source_name(), "plain.js");
    assert_eq!(resolved.original().source_id(), Some(&generated));
    assert_eq!(resolved.original().position(), SourceMapPosition::new(0, 4));
}

#[test]
fn multi_hop_chaining_keeps_the_deepest_source_name_position_and_name() {
    let outer = regular_map(
        r#"["intermediate.js"]"#,
        r#"["loweredName"]"#,
        "AACEA",
        r#"["zero\n12x"]"#,
    );
    let inner = regular_map(
        r#"["original.ts"]"#,
        r#"["originalName"]"#,
        ";EAGIA",
        r#"["a\nb\nc\n1234x"]"#,
    );

    let mut sources = SourceRegistry::new();
    let bundle = sources
        .register_with_source_map("bundle.js", "x", Some(outer))
        .expect("bundle");
    sources
        .register_with_source_map("intermediate.js", "zero\n12x", Some(inner))
        .expect("intermediate");
    let original = sources
        .register("original.ts", "a\nb\nc\n1234x")
        .expect("original");

    let resolved = sources
        .resolve_original(&bundle, SourceMapPosition::new(0, 0))
        .expect("chained");
    assert_eq!(resolved.hops(), 2);
    assert_eq!(resolved.name(), Some("originalName"));
    assert_eq!(resolved.original().source_name(), "original.ts");
    assert_eq!(resolved.original().source_id(), Some(&original));
    assert_eq!(resolved.original().position(), SourceMapPosition::new(3, 4));
    assert_eq!(
        resolved.original().embedded_source().map(AsRef::as_ref),
        Some("a\nb\nc\n1234x")
    );
}

#[test]
fn source_map_chains_detect_cycles_and_depth_limits() {
    let to_b = regular_map(r#"["b.js"]"#, "[]", "AAAA", "[null]");
    let to_a = regular_map(r#"["a.js"]"#, "[]", "AAAA", "[null]");
    let mut sources = SourceRegistry::new();
    let a = sources
        .register_with_source_map("a.js", "a", Some(to_b))
        .expect("a");
    sources
        .register_with_source_map("b.js", "b", Some(to_a))
        .expect("b");

    let cycle = sources
        .resolve_original(&a, SourceMapPosition::new(0, 0))
        .expect_err("cycle");
    assert_eq!(cycle.kind(), SourceMapErrorKind::ChainCycle);

    let depth = sources
        .resolve_original_with_limit(&a, SourceMapPosition::new(0, 0), 1)
        .expect_err("depth limit");
    assert_eq!(depth.kind(), SourceMapErrorKind::ChainDepthExceeded);
}

#[test]
fn zero_depth_allows_an_unmapped_terminal_but_rejects_a_successful_hop() {
    let unmapped = regular_map(r#"["original.js"]"#, "[]", "EAAA", "[null]");
    let mapped = regular_map(r#"["original.js"]"#, "[]", "AAAA", "[null]");
    let mut sources = SourceRegistry::new();
    let no_token = sources
        .register_with_source_map("no-token.js", "x", Some(unmapped))
        .expect("unmapped source");
    let has_token = sources
        .register_with_source_map("has-token.js", "x", Some(mapped))
        .expect("mapped source");

    let terminal = sources
        .resolve_original_with_limit(&no_token, SourceMapPosition::new(0, 0), 0)
        .expect("an absent mapping consumes no depth");
    assert_eq!(terminal.hops(), 0);

    let error = sources
        .resolve_original_with_limit(&has_token, SourceMapPosition::new(0, 0), 0)
        .expect_err("a successful hop exceeds a zero limit");
    assert_eq!(error.kind(), SourceMapErrorKind::ChainDepthExceeded);
}

#[test]
fn registered_positions_reject_utf16_columns_inside_surrogate_pairs() {
    let mut sources = SourceRegistry::new();
    let source = sources.register("emoji.js", "😀x").expect("source");
    let error = sources
        .resolve_original(&source, SourceMapPosition::new(0, 1))
        .expect_err("UTF-16 position splits emoji");
    assert_eq!(error.kind(), SourceMapErrorKind::InvalidPosition);

    sources
        .resolve_original(&source, SourceMapPosition::new(0, 2))
        .expect("column after emoji");
}

#[test]
fn byte_offsets_and_source_map_positions_round_trip_utf16_columns() {
    let mut sources = SourceRegistry::new();
    let source = sources
        .register("round-trip.js", "😀x\n猫")
        .expect("source");

    let position = sources
        .source_map_position(&source, "😀".len())
        .expect("position after emoji");
    assert_eq!(position, SourceMapPosition::new(0, 2));
    assert_eq!(
        sources
            .byte_offset_for_source_map_position(&source, position)
            .expect("byte offset"),
        "😀".len()
    );
}

#[test]
fn source_spans_follow_registered_multi_hop_maps() {
    let to_intermediate = regular_map(r#"["intermediate.js"]"#, "[]", "AAAA", "[null]");
    let to_original = regular_map(r#"["original.ts"]"#, "[]", "AAAA", "[null]");
    let mut sources = SourceRegistry::new();
    let bundle = sources
        .register_with_source_map("bundle.js", "x", Some(to_intermediate))
        .expect("bundle");
    sources
        .register_with_source_map("intermediate.js", "x", Some(to_original))
        .expect("intermediate");
    let original = sources.register("original.ts", "x").expect("original");
    let generated_span = sources.span(&bundle, 0, 1).expect("generated span");

    let resolved = sources
        .resolve_span(&generated_span)
        .expect("resolved span");
    assert_eq!(resolved.location().hops(), 2);
    assert_eq!(resolved.location().original().source_id(), Some(&original));
    assert_eq!(
        resolved.mapped_span().expect("mapped").source_id(),
        &original
    );
    assert_eq!(resolved.mapped_span().expect("mapped").bytes().start(), 0);
    assert_eq!(resolved.mapped_span().expect("mapped").bytes().end(), 0);
    assert_eq!(
        resolved.display_span(),
        resolved.mapped_span().expect("mapped")
    );
}

#[test]
fn unresolved_embedded_sources_keep_the_generated_span_as_fallback() {
    let map = regular_map(
        r#"["missing.ts"]"#,
        "[]",
        "AAAA",
        r#"["let original = true;"]"#,
    );
    let mut sources = SourceRegistry::new();
    let bundle = sources
        .register_with_source_map("bundle.js", "x", Some(map))
        .expect("bundle");
    let generated_span = sources.span(&bundle, 0, 1).expect("generated span");

    let resolved = sources
        .resolve_span(&generated_span)
        .expect("resolved span");
    assert!(resolved.location().is_mapped());
    assert_eq!(resolved.location().original().source_name(), "missing.ts");
    assert_eq!(
        resolved
            .location()
            .original()
            .embedded_source()
            .map(AsRef::as_ref),
        Some("let original = true;")
    );
    assert_eq!(resolved.mapped_span(), None);
    assert_eq!(resolved.display_span(), &generated_span);
}
