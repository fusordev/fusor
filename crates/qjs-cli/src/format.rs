//! Completion-value formatting for the REPL and smoke output.

use quickjs_runtime::{JsValue, ValueKind};

/// Largest string length (in UTF-16 code units) rendered before truncation.
const MAX_STRING_UNITS: usize = 4096;

/// Formats a completion value with a simple, deterministic rendering:
/// numbers, quoted strings, `undefined`/`null`/booleans, and shallow
/// placeholders for objects, functions, symbols, and bigints.
pub(crate) fn format_value(value: &JsValue) -> String {
    let Ok(kind) = value.kind() else {
        return "<released value>".to_owned();
    };
    match kind {
        ValueKind::Undefined => "undefined".to_owned(),
        ValueKind::Null => "null".to_owned(),
        ValueKind::Boolean => match value.as_boolean() {
            Ok(Some(boolean)) => boolean.to_string(),
            _ => "[boolean]".to_owned(),
        },
        ValueKind::Number => match value.as_number() {
            Ok(Some(number)) => format_number(number.as_f64()),
            _ => "[number]".to_owned(),
        },
        ValueKind::String => match value.as_string() {
            Ok(Some(string)) => {
                let units: Vec<u16> = string.code_units().take(MAX_STRING_UNITS + 1).collect();
                let mut text = String::from_utf16_lossy(&units[..units.len().min(MAX_STRING_UNITS)]);
                if units.len() > MAX_STRING_UNITS {
                    text.push('…');
                }
                format!("{text:?}")
            }
            _ => "[string]".to_owned(),
        },
        ValueKind::Symbol => "[symbol]".to_owned(),
        ValueKind::BigInt => "[bigint]".to_owned(),
        ValueKind::Function => "[function]".to_owned(),
        ValueKind::Object => "[object Object]".to_owned(),
    }
}

fn format_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the value is range-checked against 2**53 and integral"
        )]
        return (value as i64).to_string();
    }
    format!("{value}")
}

#[cfg(test)]
mod tests {
    use quickjs::{ScriptLimits, evaluate_script};
    use quickjs_runtime::{Runtime, RuntimeLimits};

    use super::*;

    fn evaluate(source: &str) -> String {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        let value = evaluate_script(&mut context, source, "format-test.js", ScriptLimits::default())
            .expect("script evaluates");
        format_value(&value)
    }

    #[test]
    fn formats_primitive_completions() {
        assert_eq!(evaluate("1 + 1"), "2");
        assert_eq!(evaluate("0.5"), "0.5");
        assert_eq!(evaluate("'hi'"), "\"hi\"");
        assert_eq!(evaluate("true"), "true");
        assert_eq!(evaluate("undefined"), "undefined");
        assert_eq!(evaluate("null"), "null");
    }

    #[test]
    fn formats_object_completions_as_placeholders() {
        assert_eq!(evaluate("({})"), "[object Object]");
        assert_eq!(evaluate("(function () {})"), "[function]");
    }
}
