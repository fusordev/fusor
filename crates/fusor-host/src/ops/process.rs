//! Process ops (§7.1, §7.2) installed on the `Fusor.process` object of
//! the realm-global `Fusor` namespace.

use fusor_ops::op;
use fusor_runtime::{Context, ExecutionError, JsValue};

use super::{OpError, define_op_on};
use crate::process::{with_process_state, with_signal_state};

#[op(name = "on")]
fn op_process_on(event: String, handler: JsValue) -> Result<(), OpError> {
    let _function = handler
        .clone()
        .into_function()
        .map_err(|_| OpError::type_error(1, "expected a function"))?;
    match event.as_str() {
        "SIGINT" => {
            with_process_state(|state| {
                state.sigint_handler = Some(handler);
            })
            .map_err(|error| OpError::new(error.to_string()))?;
            with_signal_state(|state| {
                state.set_js_sigint_handler(true);
                // A handler replaces the default policy (§7.1): a pending
                // interrupt request from before the registration must not
                // abort the next script.
                state.consume_interrupt();
            })
            .map_err(|error| OpError::new(error.to_string()))?;
        }
        "uncaughtException" => {
            with_process_state(|state| {
                state.uncaught_handler = Some(handler);
            })
            .map_err(|error| OpError::new(error.to_string()))?;
        }
        "unhandledRejection" => {
            with_process_state(|state| {
                state.unhandled_rejection_handler = Some(handler);
            })
            .map_err(|error| OpError::new(error.to_string()))?;
        }
        _ => {
            return Err(OpError::of_class(
                "RangeError",
                format!(
                    "unsupported process event '{event}' (the alpha host supports \
                     'SIGINT', 'uncaughtException', and 'unhandledRejection')"
                ),
            ));
        }
    }
    Ok(())
}

/// Truncates the JavaScript-supplied exit code to 8 bits (Node
/// semantics: `process.exit(256)` exits 0, `process.exit(-1)` exits 255);
/// non-finite values resolve to 0.
fn resolve_exit_code(code: f64) -> i32 {
    (code as i32) & 0xFF
}

#[op(name = "exit")]
fn op_process_exit(code: f64) -> Result<(), OpError> {
    with_signal_state(|state| {
        state.request_exit(resolve_exit_code(code));
    })
    .map_err(|error| OpError::new(error.to_string()))?;
    Ok(())
}

/// Installs one op on the `Fusor.process` object (§7.1).
fn install_process_op<F>(
    context: &mut Context<'_>,
    declaration: super::OpDeclaration,
    glue: F,
) -> Result<(), ExecutionError>
where
    F: fusor_runtime::HostCallback + 'static,
{
    let function = context.create_host_function(declaration.name, glue)?;
    let global = context.global_object()?.into_object()?;
    let fusor_key = context.property_key("Fusor")?;
    let process_key = context.property_key("process")?;
    let fusor = global.get(context, fusor_key)?.into_object()?;
    let process = fusor.get(context, process_key)?.into_object()?;
    define_op_on(context, process, declaration, function.as_value())
}

/// Installs the `Fusor.process` object and its ops (§7): a non-writable,
/// non-enumerable, non-configurable data property on the `Fusor`
/// namespace object, with `process.on` (§7.1) and `process.exit` (§7.2)
/// on it. Repeated installation is idempotent, like
/// [`super::install_namespace`], and it must run after
/// [`super::install_namespace`] so the namespace exists.
///
/// `Fusor.process.exit(code)` truncates the code to 8 bits and requests
/// the exit; it does not wait for pending async ops, and the exit takes
/// effect at the next turn boundary (§7.2, documented).
///
/// # Errors
///
/// Returns an [`ExecutionError`] when the namespace is missing, the
/// objects refuse the definitions (for example a frozen global), or
/// allocation fails.
pub fn install_process(context: &mut Context<'_>) -> Result<(), ExecutionError> {
    let global = context.global_object()?.into_object()?;
    let fusor_key = context.property_key("Fusor")?;
    let process_key = context.property_key("process")?;
    let fusor = global.get(context, fusor_key)?.into_object()?;
    if fusor.has(context, process_key.clone())? {
        return Ok(());
    }
    let process = context.new_object()?;
    let descriptor = fusor_runtime::DescriptorFields::<JsValue> {
        value: Some(process.clone()),
        writable: Some(false),
        enumerable: Some(false),
        configurable: Some(false),
        ..fusor_runtime::DescriptorFields::new()
    }
    .into_descriptor()
    .map_err(|_| fusor_runtime::EngineFault::RuntimeInvariant {
        message: "process descriptor is data-only by construction",
    })?;
    if !fusor.define_own_property(context, process_key, descriptor)? {
        return Err(fusor_runtime::EngineFault::RuntimeInvariant {
            message: "the Fusor namespace refused the process object definition",
        }
        .into());
    }
    install_process_op(
        context,
        __fusor_op_declaration_op_process_on(),
        __fusor_op_call_op_process_on,
    )?;
    install_process_op(
        context,
        __fusor_op_declaration_op_process_exit(),
        __fusor_op_call_op_process_exit,
    )
}
