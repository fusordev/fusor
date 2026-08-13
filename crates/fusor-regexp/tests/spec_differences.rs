use fusor_regexp::{CompileLimits, CompiledRegExp, ExecLimits};

/// `FUS-REGEXP-001`: `QuickJS` 2026-06-04 fails this ES2025 `v`-mode
/// lookbehind, while ECMA-262's backwards matcher and Node 24.19 match it.
#[test]
fn unicode_set_strings_execute_backwards_inside_lookbehind() {
    let expression = CompiledRegExp::compile(r"(?<=[\q{ab}])c", "v", CompileLimits::default())
        .expect("the ES2025 pattern must compile");
    let input = "abc".encode_utf16().collect::<Vec<_>>();
    let result = expression
        .execute(&input, 0, ExecLimits::default())
        .expect("the bounded execution must succeed")
        .expect("the lookbehind must match");
    assert_eq!(result.range(), 2..3);
}
