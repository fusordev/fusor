//! The error-code system (§7.5, §12.1): plain five-digit numbers
//! organized by classification range, rendered through the unified
//! diagnostic pipeline.

use fusor_host::ops::OpError;
use fusor_host::process::diagnostics::{ColorPolicy, HostDiagnostic, OpDiagnostic, render_diagnostic};
use fusor_host::process::error_codes::ErrorCode;
use fusor_runtime::{EngineFault, ExecutionError};

#[test]
fn the_documented_table_matches_the_classification() {
    assert_eq!(ErrorCode::HandleOrphaned.number(), 10001);
    assert_eq!(ErrorCode::HandleForeign.number(), 10002);
    assert_eq!(ErrorCode::UncaughtException.number(), 11001);
    assert_eq!(ErrorCode::Interrupted.number(), 11002);
    assert_eq!(ErrorCode::InstructionLimit.number(), 11003);
    assert_eq!(ErrorCode::EngineFault.number(), 11005);
    assert_eq!(ErrorCode::CallThrown.number(), 12001);
    assert_eq!(ErrorCode::OpFailure.number(), 14001);
    assert_eq!(ErrorCode::OpFailure.to_string(), "14001");
}

#[test]
fn execution_errors_map_to_their_codes() {
    let fault = ExecutionError::EngineFault(EngineFault::RuntimeInvariant { message: "x" });
    assert_eq!(ErrorCode::from_execution_error(&fault), ErrorCode::EngineFault);
    let interrupted = ExecutionError::Interrupted { executed: 0 };
    assert_eq!(
        ErrorCode::from_execution_error(&interrupted),
        ErrorCode::Interrupted
    );
    let limit = ExecutionError::InstructionLimitExceeded {
        limit: 1,
        executed: 1,
    };
    assert_eq!(
        ErrorCode::from_execution_error(&limit),
        ErrorCode::InstructionLimit
    );
}

#[test]
fn host_diagnostics_render_their_codes() {
    let diagnostic = HostDiagnostic::new(ExecutionError::EngineFault(
        EngineFault::RuntimeInvariant {
            message: "invariant boom",
        },
    ));
    let rendered = render_diagnostic(diagnostic, ColorPolicy::Never);
    assert!(
        rendered.contains("11005"),
        "the engine-fault code renders in the report: {rendered}"
    );
}

#[test]
fn op_errors_render_their_numeric_codes() {
    let coded = render_diagnostic(
        OpDiagnostic::new(OpError::new("failed").with_code(14007)),
        ColorPolicy::Never,
    );
    assert!(coded.contains("14007"), "{coded}");
    let defaulted = render_diagnostic(
        OpDiagnostic::new(OpError::new("failed")),
        ColorPolicy::Never,
    );
    assert!(
        defaulted.contains("14001"),
        "the op-layer class code renders when the op has no own code: {defaulted}"
    );
}
