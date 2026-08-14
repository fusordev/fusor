//! Core print op (§5.4 host global conventions): `Fusor.ops.op_core_print`
//! renders variadic arguments to the installable print sink (stdout by
//! default). Strings render raw; every other value converts through the
//! engine `ToString`, falling back to a kind-shaped rendering for values
//! the host conversion rejects (objects, functions).

use std::cell::RefCell;

use fusor_runtime::{Context, ExecutionError, HostCall, JsValue, ValueKind};

use super::{OpDeclaration, install_op};

thread_local! {
    /// The current print sink; stdout by default. Installable so the
    /// console overlay (subproject 6) can redirect output.
    static PRINT_SINK: RefCell<Box<dyn FnMut(&str)>> =
        RefCell::new(Box::new(|line: &str| println!("{line}")));
}

/// Replaces the print sink, returning the previous one.
#[must_use]
pub fn set_print_sink(sink: Box<dyn FnMut(&str)>) -> Box<dyn FnMut(&str)> {
    PRINT_SINK.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), sink))
}

/// Renders one print argument: strings raw, everything else through the
/// engine `ToString` with a kind-shaped fallback.
fn format_print_argument(context: &mut Context<'_>, value: &JsValue) -> String {
    if value.kind() == Ok(ValueKind::String)
        && let Ok(Some(string)) = value.as_string()
    {
        let units: Vec<u16> = string.code_units().collect();
        return String::from_utf16_lossy(&units);
    }
    match value.to_string(context) {
        Ok(string) => string.to_utf8_lossy().unwrap_or_default(),
        Err(_) => match value.kind() {
            Ok(ValueKind::Function) => "[function]".to_owned(),
            Ok(ValueKind::Object) => "[object Object]".to_owned(),
            _ => "<unprintable>".to_owned(),
        },
    }
}

/// The variadic core print glue (§5.7 shape; variadic parameters are not
/// expressible in the `#[op]` macro's flat signature).
fn op_core_print_glue(
    context: &mut Context<'_>,
    call: HostCall,
) -> Result<JsValue, JsValue> {
    let rendered: Vec<String> = call
        .arguments()
        .iter()
        .map(|value| format_print_argument(context, value))
        .collect();
    let line = rendered.join(" ");
    PRINT_SINK.with(|slot| (slot.borrow_mut())(&line));
    Ok(context.undefined())
}

/// Installs the core ops as `Fusor.ops.op_core_print` (§5.4).
///
/// # Errors
///
/// Returns an [`ExecutionError`] when the op cannot be installed.
pub fn install_core_ops(context: &mut Context<'_>) -> Result<(), ExecutionError> {
    install_op(
        context,
        OpDeclaration {
            name: "op_core_print",
            parameter_types: &["...values"],
            is_async: false,
        },
        op_core_print_glue,
    )
}
