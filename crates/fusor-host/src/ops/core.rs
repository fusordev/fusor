//! Core print op (§5.4 host global conventions): `Fusor.ops.op_core_print`
//! renders variadic arguments to the installable print sink (stdout by
//! default). Strings render raw; every other value converts through the
//! engine `ToString`, falling back to a kind-shaped rendering for values
//! the host conversion rejects (objects, functions).

use std::cell::RefCell;

use fusor_runtime::{Context, ExecutionError, HostCall, JsValue, ValueKind};

use super::{OpDeclaration, OpError, OpStateRegistry, install_op};

/// The current print sink: stdout by default. Installed into the
/// op-state registry by the host builder; the console overlay
/// (subproject 6) replaces it to redirect output.
pub struct PrintSink(Box<dyn FnMut(&str)>);

impl Default for PrintSink {
    fn default() -> Self {
        Self(Box::new(|line: &str| println!("{line}")))
    }
}

impl std::fmt::Debug for PrintSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrintSink(..)")
    }
}

/// Installs the print sink, returning the previous one (or `None` when
/// no sink was installed yet).
#[must_use]
pub fn set_print_sink(sink: Box<dyn FnMut(&str)>) -> Option<Box<dyn FnMut(&str)>> {
    if !OpStateRegistry::has::<PrintSink>() {
        let _ = OpStateRegistry::install(PrintSink::default());
    }
    OpStateRegistry::with_mut::<PrintSink, _>(|slot| std::mem::replace(&mut slot.0, sink)).ok()
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
    if !OpStateRegistry::has::<PrintSink>() {
        let _ = OpStateRegistry::install(PrintSink::default());
    }
    OpStateRegistry::with_mut::<PrintSink, _>(|sink| (sink.0)(&line))
        .map_err(|error| super::op_error_value(context, OpError::new(error.to_string())))?;
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
