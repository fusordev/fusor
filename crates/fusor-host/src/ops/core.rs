//! Core print op (§5.4 host global conventions): `Fusor.ops.op_core_print`
//! renders variadic arguments to the installable print sink (stdout by
//! default). Strings render raw; every other value converts through the
//! engine `ToString`, falling back to a kind-shaped rendering for values
//! the host conversion rejects (objects, functions).

use std::io::Write as _;

use fusor_runtime::{Context, JsValue, ValueKind};

use super::OpStateRegistry;

/// The current print sink: stdout by default. Installed into the
/// op-state registry by the host builder; the console overlay
/// (subproject 6) replaces it to redirect output.
pub struct PrintSink(Box<dyn FnMut(&str)>);

impl Default for PrintSink {
    fn default() -> Self {
        Self(Box::new(|line: &str| {
            println!("{line}");
            // Piped stdout is block-buffered: flush each line so print
            // output appears promptly instead of at process exit.
            let _ = std::io::stdout().flush();
        }))
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

/// The core print op (§5.4): `Fusor.ops.op_core_print` with variadic
/// parameters (not expressible in the `#[op]` macro's flat signature), so
/// the op is hand-rolled here in the same shape the `#[op]` macro
/// generates — a module named after the op carrying `declaration()` and
/// `call` — which `register_op!` consumes (§9).
#[doc(hidden)]
pub(crate) mod op_core_print {
    use fusor_runtime::{Context, HostCall, JsValue};

    use crate::ops::{OpDeclaration, OpError};

    use super::{OpStateRegistry, PrintSink, format_print_argument};

    /// The core print op's declaration.
    #[must_use]
    pub(crate) fn declaration() -> OpDeclaration {
        OpDeclaration {
            name: "op_core_print",
            parameter_types: &["...values"],
            is_async: false,
        }
    }

    /// The variadic core print glue (§5.7 shape).
    pub(crate) fn call(context: &mut Context<'_>, call: HostCall) -> Result<JsValue, JsValue> {
        let rendered: Vec<String> = call
            .arguments()
            .iter()
            .map(|value| format_print_argument(context, value))
            .collect();
        let line = rendered.join(" ");
        if !OpStateRegistry::has::<PrintSink>() {
            let _ = OpStateRegistry::install(PrintSink::default());
        }
        OpStateRegistry::with_mut::<PrintSink, _>(|sink| (sink.0)(&line)).map_err(|error| {
            ::fusor_host::ops::op_error_value(context, OpError::new(error.to_string()))
        })?;
        Ok(context.undefined())
    }
}

/// The core garbage-collection op: `Fusor.ops.op_core_gc` runs a full
/// mark-and-sweep collection and returns `undefined` (§8.2 snapshot
/// hygiene: snapshot creation collects first, and this op lets scripts
/// request a collection directly).
#[doc(hidden)]
pub(crate) mod op_core_gc {
    use fusor_runtime::{Context, HostCall, JsValue};

    use crate::ops::{OpDeclaration, OpError};

    /// The core gc op's declaration.
    #[must_use]
    pub(crate) fn declaration() -> OpDeclaration {
        OpDeclaration {
            name: "op_core_gc",
            parameter_types: &[],
            is_async: false,
        }
    }

    /// Runs the forced collection (§5.7 shape).
    pub(crate) fn call(context: &mut Context<'_>, _call: HostCall) -> Result<JsValue, JsValue> {
        context.collect_cycles().map_err(|error| {
            ::fusor_host::ops::op_error_value(context, OpError::new(error.to_string()))
        })?;
        Ok(context.undefined())
    }
}

/// The monotonic clock anchor for [`op_core_now`] (§5.4): captured when the
/// host core installs, so `performance.now()`-style readings are relative to
/// the process time origin (V8 semantics: a high-resolution timestamp in
/// milliseconds, monotonically increasing, unaffected by system clock
/// changes).
#[derive(Debug)]
pub struct ClockState {
    start: std::time::Instant,
}

impl Default for ClockState {
    fn default() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

/// Installs the clock anchor when no clock state exists yet (idempotent, like
/// the print sink).
pub(crate) fn install_clock_state() {
    if !OpStateRegistry::has::<ClockState>() {
        let _ = OpStateRegistry::install(ClockState::default());
    }
}

/// The core clock op: `Fusor.ops.op_core_now` returns the elapsed
/// milliseconds since the process time origin (`performance.now()`
/// semantics, V8-aligned): a finite non-negative `Number`, monotonically
/// non-decreasing across calls.
#[doc(hidden)]
pub(crate) mod op_core_now {
    use fusor_runtime::{Context, HostCall, JsNumber, JsValue};

    use crate::ops::OpDeclaration;

    use super::{ClockState, OpStateRegistry};

    /// The core now op's declaration.
    #[must_use]
    pub(crate) fn declaration() -> OpDeclaration {
        OpDeclaration {
            name: "op_core_now",
            parameter_types: &[],
            is_async: false,
        }
    }

    /// Returns the elapsed milliseconds since the time origin (§5.7 shape).
    pub(crate) fn call(context: &mut Context<'_>, _call: HostCall) -> Result<JsValue, JsValue> {
        let elapsed = OpStateRegistry::with::<ClockState, _>(|state| state.start.elapsed())
            .map(|elapsed| elapsed.as_secs_f64() * 1_000.0)
            .unwrap_or(0.0);
        Ok(context.number(JsNumber::from_f64(elapsed)))
    }
}
