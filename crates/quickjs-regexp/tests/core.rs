use quickjs_regexp::{CompileError, CompileLimits, CompiledRegExp, ExecError, ExecLimits};

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
