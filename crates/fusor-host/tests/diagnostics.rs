//! Unified diagnostic rendering (§7.5): the color policy matrix and the
//! evaluation-layer frame-to-label adaptation.

use std::sync::Arc;

use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use fusor_host::ops::OpError;
use fusor_host::process::diagnostics::{
    ColorPolicy, HostDiagnostic, MessageDiagnostic, OpDiagnostic, render_diagnostic,
};
use fusor_runtime::{
    EngineFault, ExecutionError, ExecutionLimits, GlobalScriptError, Runtime, RuntimeLimits,
};

#[test]
fn color_policy_resolution_matrix() {
    assert_eq!(ColorPolicy::Auto.resolve(true, false), ColorPolicy::Always);
    assert_eq!(
        ColorPolicy::Auto.resolve(false, false),
        ColorPolicy::Never,
        "non-TTY degrades to no color (§7.5)"
    );
    assert_eq!(
        ColorPolicy::Auto.resolve(true, true),
        ColorPolicy::Never,
        "NO_COLOR wins over a TTY (§7.5)"
    );
    assert_eq!(
        ColorPolicy::Always.resolve(false, true),
        ColorPolicy::Always,
        "explicit always beats the environment"
    );
    assert_eq!(
        ColorPolicy::Never.resolve(true, false),
        ColorPolicy::Never,
        "explicit never beats the TTY"
    );
}

#[test]
fn the_nocolor_policy_emits_no_ansi_sequences() {
    let diagnostic = HostDiagnostic::new(ExecutionError::EngineFault(
        EngineFault::RuntimeInvariant {
            message: "invariant boom",
        },
    ));
    let rendered = render_diagnostic(diagnostic, ColorPolicy::Never);
    assert!(
        rendered.contains("invariant boom"),
        "the message renders: {rendered}"
    );
    assert!(
        !rendered.contains("\x1b["),
        "no ANSI escapes with Never: {rendered:?}"
    );
}

#[test]
fn the_color_policy_emits_ansi_sequences() {
    let diagnostic = HostDiagnostic::new(ExecutionError::EngineFault(
        EngineFault::RuntimeInvariant {
            message: "invariant boom",
        },
    ));
    let rendered = render_diagnostic(diagnostic, ColorPolicy::Always);
    assert!(
        rendered.contains("\x1b["),
        "ANSI escapes with Always: {rendered:?}"
    );
}

#[test]
fn uncaught_frames_become_source_labels() {
    // Drive a real thrown exception so the frames carry verified spans.
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let authority = {
        with_parsed_program(
            "globalThis.n = 1;\nthrow new Error('boom');",
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new_with_source_name(
                    unit,
                    Arc::from("diagnostics.js"),
                )
                .expect("storage plan");
                let tree = context
                    .compile_global_script(fusor_bytecode::VerificationLimits::default())
                    .expect("verified Global Script");
                Arc::new(tree.verified_bytecode().clone())
            },
        )
        .expect("frontend")
    };
    let error = match context.execute_global_script(authority, ExecutionLimits::default()) {
        Err(GlobalScriptError::Execution(error)) => error,
        other => panic!("expected a thrown script: {other:?}"),
    };
    let rendered = render_diagnostic(HostDiagnostic::new(error), ColorPolicy::Never);
    assert!(rendered.contains("boom"), "the message renders: {rendered}");
    assert!(
        rendered.contains("throw new Error"),
        "the source line renders under the frame label: {rendered}"
    );
    assert!(
        rendered.contains("diagnostics.js"),
        "the source name renders: {rendered}"
    );
}

#[test]
fn op_errors_render_their_class_and_message() {
    let rendered = render_diagnostic(
        OpDiagnostic::new(OpError::of_class("RangeError", "unsupported event")),
        ColorPolicy::Never,
    );
    assert!(
        rendered.contains("RangeError") && rendered.contains("unsupported event"),
        "{rendered}"
    );
}

#[test]
fn message_diagnostics_render_through_the_same_pipeline() {
    let rendered = render_diagnostic(
        MessageDiagnostic::new("Uncaught exception: Error: boom"),
        ColorPolicy::Never,
    );
    assert!(
        rendered.contains("Uncaught exception: Error: boom"),
        "{rendered}"
    );
    assert!(!rendered.contains("\x1b["), "no color with Never");
}
