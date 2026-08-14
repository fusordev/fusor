//! The CLI overlay (§9 step 4): the `fusor` binary assembles "core overlay +
//! CLI overlay" instead of hand-installing host functions. The CLI overlay
//! contributes the `print` shim as a Global Script init (§8.4): the host
//! installs no `print` global itself (§5.4 — ops live only under
//! `Fusor.ops`), the shim delegates to `Fusor.ops.op_core_print`, so
//! differential fixtures and REPL sessions keep their bare `print` spelling
//! while all output flows through the installable print sink.

use fusor_host::overlay::{CoreOverlay, Overlay, OverlaySource};

/// The CLI overlay (§9 step 4): the `fusor` binary's own overlay, layered
/// over [`CoreOverlay`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CliOverlay;

impl CliOverlay {
    /// The CLI overlay's stable name.
    pub const NAME: &'static str = "fusor:cli";
}

impl Overlay for CliOverlay {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn ops(&self, _registry: &mut fusor_host::ops::OpRegistry) {}

    fn init_sources(&self) -> Vec<OverlaySource> {
        vec![OverlaySource {
            specifier: "fusor:core/testing".to_owned(),
            text: r#"// Core
Fusor.print = Fusor.ops.op_core_print;
Fusor.now = () => Fusor.ops.op_core_now();
Fusor.exit = (code) => Fusor.ops.op_process_exit(code);

// Timers
globalThis.setImmediate = Fusor.ops.op_set_immediate;
globalThis.setTimeout = Fusor.ops.op_set_timeout;
globalThis.setInterval = Fusor.ops.op_set_interval;
globalThis.clearImmediate = Fusor.ops.op_clear_immediate;
globalThis.clearTimeout = Fusor.ops.op_clear_timeout;
globalThis.clearInterval = Fusor.ops.op_clear_interval;
"#,
        }]
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &[CoreOverlay::NAME]
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use fusor_bytecode::VerifiedBytecode;
    use fusor_compiler::CompilationContext;
    use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
    use fusor_host::ops::set_print_sink;
    use fusor_host::overlay::{CoreOverlay, HostRuntime};
    use fusor_runtime::ExecutionLimits;

    use super::CliOverlay;

    /// Compiles one Global Script for the print-shim probe.
    fn compile(source: &str) -> Arc<VerifiedBytecode> {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new_with_source_name(unit, Arc::from("print.js"))
                    .expect("storage plan");
                let tree = context
                    .compile_global_script(fusor_bytecode::VerificationLimits::default())
                    .expect("verified Global Script");
                Arc::new(tree.verified_bytecode().clone())
            },
        )
        .expect("frontend")
    }

    #[test]
    fn the_cli_overlay_installs_the_print_shim_through_the_sink() {
        let mut host = HostRuntime::builder()
            .with_overlay(CoreOverlay)
            .with_overlay(CliOverlay)
            .build()
            .expect("assembled host runtime");

        // The shim delegates to the core print op, so output reaches the
        // installable sink — capture it, then restore the previous sink.
        let captured = Rc::new(RefCell::new(String::new()));
        let sink = Rc::clone(&captured);
        let previous = set_print_sink(Box::new(move |line: &str| sink.borrow_mut().push_str(line)));

        let authority = compile("print(1, 'two', 3); 'done';");
        let mut context = host.context().expect("context");
        let value = context
            .execute_global_script(authority, ExecutionLimits::default())
            .expect("script execution");
        assert_eq!(
            value
                .as_string()
                .expect("live value")
                .expect("String")
                .to_utf8_lossy()
                .expect("UTF-8"),
            "done"
        );
        assert_eq!(*captured.borrow(), "1 two 3");
        if let Some(previous) = previous {
            let _ = set_print_sink(previous);
        }
    }
}
