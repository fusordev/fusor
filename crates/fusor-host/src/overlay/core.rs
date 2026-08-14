//! The host core overlay (§9): the standard op set every host assembly
//! starts from. The CLI assembly is "core overlay + CLI overlay"
//! (§9 step 4); hosts with no extra features build with just this overlay.

use fusor_ops::register_op;

use crate::ops::{
    OpRegistry, op_clear_interval, op_clear_timeout, op_core_gc, op_core_print,
    op_queue_microtask, op_set_immediate, op_set_interval, op_set_timeout,
};

use super::{Overlay, OverlaySource};

/// The core overlay: the five timer ops (§6.4) and the core print op
/// (§5.4).
///
/// The `Fusor.process` object with its `on`/`exit` ops (§7) is host core,
/// installed unconditionally by the builder, not by this overlay. Ops are
/// only ever installed under `Fusor.ops` — never onto the realm global
/// (§5.4).
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreOverlay;

impl CoreOverlay {
    /// The core overlay's stable name.
    pub const NAME: &'static str = "fusor:core";
}

impl Overlay for CoreOverlay {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn ops(&self, registry: &mut OpRegistry) {
        register_op!(registry, op_set_timeout);
        register_op!(registry, op_set_interval);
        register_op!(registry, op_clear_timeout);
        register_op!(registry, op_clear_interval);
        register_op!(registry, op_set_immediate);
        register_op!(registry, op_queue_microtask);
        register_op!(registry, op_core_print);
        register_op!(registry, op_core_gc);
    }

    fn init_sources(&self) -> Vec<OverlaySource> {
        Vec::new()
    }

    fn entry(&self) -> &'static str {
        ""
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &[]
    }
}
