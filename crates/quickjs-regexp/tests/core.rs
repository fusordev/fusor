use quickjs_regexp::{
    CompileError, CompileLimits, CompiledRegExp, ExecError, ExecLimits, validate_literal,
};

fn compile(pattern: &str, flags: &str) -> CompiledRegExp {
    CompiledRegExp::compile(pattern, flags, CompileLimits::default())
        .expect("test pattern must compile")
}

#[test]
fn capture_names_follow_match_capture_indices() {
    let expression = compile("(?<left>a)(b)(?<right>c)", "d");
    assert_eq!(
        expression.capture_names(),
        [
            None,
            Some("left".to_owned()),
            None,
            Some("right".to_owned()),
        ]
    );
}

#[test]
fn capture_names_and_references_share_the_cooked_identifier() {
    let source = r"(?:(?<𝑓>a)|(?<\u{1D453}>b))\k<\uD835\uDC53>";
    let expression = CompiledRegExp::compile_utf16(
        &source.encode_utf16().collect::<Vec<_>>(),
        &[],
        CompileLimits::default(),
    )
    .expect("disjoint duplicate names with mixed spellings are valid");
    assert_eq!(
        expression.capture_names(),
        [None, Some("𝑓".to_owned()), Some("𝑓".to_owned())]
    );
    assert_eq!(
        expression
            .execute(
                &"aa".encode_utf16().collect::<Vec<_>>(),
                0,
                ExecLimits::default(),
            )
            .expect("bounded execution")
            .expect("cooked backreference matches")
            .range(),
        0..2
    );
    assert!(matches!(
        CompiledRegExp::compile(r"(?<A>a)(?<\u0041>b)", "", CompileLimits::default()),
        Err(CompileError::Syntax(_))
    ));
}

fn ranges(pattern: &str, flags: &str, input: &str) -> Vec<Option<(usize, usize)>> {
    compile(pattern, flags)
        .execute(
            &input.encode_utf16().collect::<Vec<_>>(),
            0,
            ExecLimits::default(),
        )
        .expect("test execution must stay within its limits")
        .expect("test pattern must match")
        .captures
        .into_iter()
        .map(|range| range.map(|range| (range.start, range.end)))
        .collect()
}

fn matched_text(pattern: &str, flags: &str, input: &str) -> Option<String> {
    let input_utf16 = input.encode_utf16().collect::<Vec<_>>();
    compile(pattern, flags)
        .execute(&input_utf16, 0, ExecLimits::default())
        .expect("test execution must stay within its limits")
        .map(|matched| String::from_utf16_lossy(&input_utf16[matched.range()]))
}

#[test]
fn annex_b_class_control_letters_use_modulo_32_inside_classes_only() {
    assert_eq!(matched_text(r"\c0", "", "\u{f}\u{10}\u{11}"), None);
    assert_eq!(
        matched_text(r"[\c0]", "", "\u{f}\u{10}\u{11}"),
        Some("\u{10}".to_owned())
    );
    assert_eq!(
        matched_text(r"[\c00]+", "", "\u{f}0\u{10}\u{11}"),
        Some("0\u{10}".to_owned())
    );
    assert_eq!(
        matched_text(r"[\c_]", "", "\u{1e}\u{1f} "),
        Some("\u{1f}".to_owned())
    );
}

#[test]
fn annex_b_legacy_octal_escapes_stop_at_the_specified_digit_count() {
    for (pattern, input, expected) in [
        (r"\1", "\u{1}", "\u{1}"),
        (r"\00", "\0", "\0"),
        (r"\30", "\u{18}", "\u{18}"),
        (r"\77", "?", "?"),
        (r"\400", " 0", " 0"),
        (r"\770", "?0", "?0"),
        (r"\377", "\u{ff}", "\u{ff}"),
        (r"\0111", "\t1", "\t1"),
        (r"\0022", "\u{2}2", "\u{2}2"),
    ] {
        assert_eq!(
            matched_text(pattern, "", input),
            Some(expected.to_owned()),
            "pattern {pattern}"
        );
    }
    assert_eq!(
        matched_text(r"(.)\1", "", "a\u{1} aa"),
        Some("aa".to_owned())
    );
}

#[test]
fn validates_and_canonicalizes_es2025_flags() {
    assert_eq!(compile("a", "ymigdsu").flags(), "dgimsuy");
    assert_eq!(compile("a", "v").flags(), "v");
    assert!(matches!(
        CompiledRegExp::compile("a", "gg", CompileLimits::default()),
        Err(CompileError::InvalidFlags)
    ));
    assert!(matches!(
        CompiledRegExp::compile("a", "uv", CompileLimits::default()),
        Err(CompileError::InvalidFlags)
    ));
    assert!(matches!(
        CompiledRegExp::compile("a", "z", CompileLimits::default()),
        Err(CompileError::InvalidFlags)
    ));
}

#[test]
fn literal_validation_and_execution_accept_rgi_emoji_properties() {
    assert!(validate_literal("[z-a]", "u", usize::MAX).is_err());
    assert!(validate_literal("a", "gg", usize::MAX).is_err());
    assert!(validate_literal(r"\p{RGI_Emoji_ZWJ_Sequence}", "v", usize::MAX).is_ok());
    assert!(
        CompiledRegExp::compile(r"\p{RGI_Emoji_ZWJ_Sequence}", "v", CompileLimits::default())
            .is_ok()
    );
}

#[test]
fn ordered_alternation_greedy_quantifiers_and_captures_match_utf16_ranges() {
    assert_eq!(
        ranges("(a|ab)+?(b)", "", "zzababq"),
        [Some((2, 4)), Some((2, 3)), Some((3, 4)),]
    );
    assert_eq!(
        ranges("(ab|a)+b", "", "aaab"),
        [Some((0, 4)), Some((2, 3)),]
    );
}

#[test]
fn unicode_mode_consumes_a_surrogate_pair_as_one_character() {
    assert_eq!(ranges("^(.)$", "u", "😀"), [Some((0, 2)), Some((0, 2))]);
    assert!(
        compile("^(.)$", "")
            .execute(
                &"😀".encode_utf16().collect::<Vec<_>>(),
                0,
                ExecLimits::default(),
            )
            .expect("execution must stay within its limits")
            .is_none()
    );
}

#[test]
fn optional_zero_length_repeats_rollback_the_attempted_capture() {
    assert_eq!(
        ranges("(?:(?=(abc)))a", "", "abc"),
        [Some((0, 1)), Some((0, 3))]
    );
    assert_eq!(ranges("(?:(?=(abc)))?a", "", "abc"), [Some((0, 1)), None]);
    assert_eq!(
        ranges("(?:(?=(abc))){1,1}a", "", "abc"),
        [Some((0, 1)), Some((0, 3))]
    );
    assert_eq!(
        ranges("(?:(?=(abc))){0,1}a", "", "abc"),
        [Some((0, 1)), None]
    );
}

#[test]
fn class_string_disjunction_is_one_set_operand() {
    assert_eq!(
        matched_text(r"^[\d&&\q{0|2|4|9\uFE0F\u20E3}]+$", "v", "024"),
        Some("024".to_owned())
    );
    assert_eq!(
        matched_text(
            r"^[\q{0|2|4|9\uFE0F\u20E3}--\d]+$",
            "v",
            "9\u{fe0f}\u{20e3}",
        ),
        Some("9\u{fe0f}\u{20e3}".to_owned())
    );
    assert_eq!(
        matched_text(
            r"^[\q{0|2|4|9\uFE0F\u20E3}&&\q{2|4|9\uFE0F\u20E3}]+$",
            "v",
            "24",
        ),
        Some("24".to_owned())
    );
    assert_eq!(
        matched_text(
            r"^[\d&&\q{0|2|4|9\uFE0F\u20E3}]+$",
            "v",
            "9\u{fe0f}\u{20e3}"
        ),
        None
    );
}

#[test]
fn classes_anchors_backreferences_and_inline_modifiers_execute() {
    assert_eq!(
        ranges(r"^(?<word>[a-z]+)\s+\k<word>$", "i", "Rust rUsT"),
        [Some((0, 9)), Some((0, 4)),]
    );
    assert_eq!(
        ranges(r"(?s:^a.$)|(?-s:^b.$)", "m", "x\na\n\ny"),
        [Some((2, 4)),]
    );
}

#[test]
fn malformed_patterns_and_structural_limits_fail_before_execution() {
    assert!(matches!(
        CompiledRegExp::compile("[z-a]", "u", CompileLimits::default()),
        Err(CompileError::Syntax(_))
    ));
    assert!(matches!(
        CompiledRegExp::compile(
            "abcd",
            "",
            CompileLimits {
                max_pattern_bytes: 3,
                ..CompileLimits::default()
            },
        ),
        Err(CompileError::ResourceLimit("source length"))
    ));
}

#[test]
fn unicode_mode_rejects_restricted_identity_and_control_escapes() {
    let compile_utf16 = |pattern: &[u16]| {
        CompiledRegExp::compile_utf16(pattern, &[u16::from(b'u')], CompileLimits::default())
    };
    let atom_escapes = b"bBfnrtvdDsSwW";
    let class_escapes = b"bfnrtvdDsSwW";
    for letter in b'A'..=b'Z' {
        if !atom_escapes.contains(&letter) {
            assert!(
                compile_utf16(&[u16::from(b'\\'), u16::from(letter)]).is_err(),
                "atom identity escape {letter:?}"
            );
        }
        if !class_escapes.contains(&letter) {
            assert!(
                compile_utf16(&[
                    u16::from(b'['),
                    u16::from(b'\\'),
                    u16::from(letter),
                    u16::from(b']'),
                ])
                .is_err(),
                "class identity escape {letter:?}"
            );
        }
    }
    for letter in b'a'..=b'z' {
        if !atom_escapes.contains(&letter) {
            assert!(
                compile_utf16(&[u16::from(b'\\'), u16::from(letter)]).is_err(),
                "atom identity escape {letter:?}"
            );
        }
        if !class_escapes.contains(&letter) {
            assert!(
                compile_utf16(&[
                    u16::from(b'['),
                    u16::from(b'\\'),
                    u16::from(letter),
                    u16::from(b']'),
                ])
                .is_err(),
                "class identity escape {letter:?}"
            );
        }
    }
    assert!(compile_utf16(&[u16::from(b'\\'), u16::from(b'c')]).is_err());
    assert!(
        compile_utf16(&[
            u16::from(b'['),
            u16::from(b'\\'),
            u16::from(b'c'),
            u16::from(b']'),
        ])
        .is_err()
    );
    for value in 0_u16..=0x7f {
        if !u8::try_from(value).is_ok_and(|value| value.is_ascii_alphabetic()) {
            assert!(
                compile_utf16(&[u16::from(b'\\'), u16::from(b'c'), value]).is_err(),
                "atom control escape {value:#x}"
            );
            assert!(
                compile_utf16(&[
                    u16::from(b'['),
                    u16::from(b'\\'),
                    u16::from(b'c'),
                    value,
                    u16::from(b']'),
                ])
                .is_err(),
                "class control escape {value:#x}"
            );
        }
    }
}

#[test]
fn execution_budget_fails_closed_on_exponential_backtracking() {
    let expression = compile("^(a|aa)*b$", "");
    let input = "a".repeat(30).encode_utf16().collect::<Vec<_>>();
    assert_eq!(
        expression.execute(
            &input,
            0,
            ExecLimits {
                max_steps: 100,
                max_backtrack_states: 1_000,
            },
        ),
        Err(ExecError::StepLimit)
    );
}

/// A one-pass, anchored quantified class must not retain one backtrack state
/// for every matching code point. Test262's generated Unicode-property tests
/// exercise this shape over nearly all Unicode scalar values.
#[test]
fn anchored_property_repeat_scales_without_linear_backtracking_storage() {
    let expression = compile(r"^\P{ASCII}+$", "u");
    let input = vec![0x0100; 1_100_000];
    assert_eq!(
        expression
            .execute(&input, 0, ExecLimits::default())
            .expect("anchored property repeat must stay within its execution limits")
            .expect("all input code points satisfy \\P{ASCII}")
            .range(),
        0..input.len()
    );
}

/// Reproduces Test262's generated `ASCII` non-match string, including its
/// deliberate lone-surrogate ranges and supplementary scalar values.
#[test]
fn anchored_property_repeat_accepts_test262_ascii_non_match_symbols() {
    let mut input = Vec::new();
    for code_unit in 0xDC00_u16..=0xDFFF {
        input.push(code_unit);
    }
    for code_unit in 0x0080_u16..=0xDBFF {
        input.push(code_unit);
    }
    for code_point in 0xE000_u32..=0x10_FFFF {
        if let Ok(code_unit) = u16::try_from(code_point) {
            input.push(code_unit);
        } else {
            let scalar = code_point - 0x1_0000;
            input.push(
                u16::try_from(0xD800_u32 + (scalar >> 10)).expect("high surrogate fits in UTF-16"),
            );
            input.push(
                u16::try_from(0xDC00_u32 + (scalar & 0x03FF))
                    .expect("low surrogate fits in UTF-16"),
            );
        }
    }
    let expression = compile(r"^\P{ASCII}+$", "u");
    assert_eq!(
        expression
            .execute(&input, 0, ExecLimits::default())
            .expect("generated property repeat must stay within its execution limits")
            .expect("every generated code point satisfies \\P{ASCII}")
            .range(),
        0..input.len()
    );
}

/// The terminal-repeat fast path must retain ordinary candidate search and
/// defer to normal backtracking for multiline end anchors.
#[test]
fn terminal_repeat_preserves_candidate_and_multiline_semantics() {
    assert_eq!(ranges("a+$", "", "zzzaaa"), [Some((3, 6))]);
    assert!(
        compile("^a+$", "")
            .execute(
                &"aaab".encode_utf16().collect::<Vec<_>>(),
                0,
                ExecLimits::default(),
            )
            .expect("ordinary non-match must stay within its execution limits")
            .is_none()
    );
    assert_eq!(ranges("^a+$", "m", "a\nb"), [Some((0, 1))]);
}

#[test]
fn constructor_sources_preserve_lone_surrogates_and_utf16_escape_rules() {
    let lone = [0xd800];
    for flags in ["", "u"] {
        let expression = CompiledRegExp::compile_utf16(
            &lone,
            &flags.encode_utf16().collect::<Vec<_>>(),
            CompileLimits::default(),
        )
        .expect("a literal lone surrogate is valid in both modes");
        assert_eq!(
            expression
                .execute(&lone, 0, ExecLimits::default())
                .expect("bounded execution")
                .expect("lone surrogate match")
                .range(),
            0..1
        );
    }

    let identity_escape = [u16::from(b'\\'), 0xd800];
    let legacy_identity =
        CompiledRegExp::compile_utf16(&identity_escape, &[], CompileLimits::default())
            .expect("legacy identity escape accepts a surrogate");
    assert_eq!(
        legacy_identity
            .execute(&lone, 0, ExecLimits::default())
            .expect("bounded execution")
            .expect("escaped lone surrogate match")
            .range(),
        0..1
    );
    assert!(matches!(
        CompiledRegExp::compile_utf16(
            &identity_escape,
            &[u16::from(b'u')],
            CompileLimits::default(),
        ),
        Err(CompileError::Syntax(_))
    ));
    assert!(matches!(
        CompiledRegExp::compile_utf16(&lone, &[0xd800], CompileLimits::default()),
        Err(CompileError::InvalidFlags)
    ));

    let smile = [0xd83d, 0xde00];
    let legacy = CompiledRegExp::compile_utf16(&smile, &[], CompileLimits::default())
        .expect("legacy mode interprets each UTF-16 element as one BMP code point");
    let unicode =
        CompiledRegExp::compile_utf16(&smile, &[u16::from(b'u')], CompileLimits::default())
            .expect("Unicode mode decodes a valid surrogate pair");
    for expression in [&legacy, &unicode] {
        assert_eq!(
            expression
                .execute(&smile, 0, ExecLimits::default())
                .expect("bounded execution")
                .expect("paired surrogate match")
                .range(),
            0..2
        );
    }
}
