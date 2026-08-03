use quickjs_frontend::Span;

use crate::lowering::{FrameSlot, RealmGlobalId};
use crate::storage::{BindingId, ExecutableId, ReferenceAccess};

#[derive(Clone, Copy)]
pub(in crate::lowering) enum LoweredReference {
    Frame {
        binding: BindingId,
        slot: FrameSlot,
        access: ReferenceAccess,
    },
    RealmGlobal {
        global: RealmGlobalId,
        slot: u16,
        access: ReferenceAccess,
    },
}

impl LoweredReference {
    pub(in crate::lowering) const fn access(self) -> ReferenceAccess {
        match self {
            Self::Frame { access, .. } | Self::RealmGlobal { access, .. } => access,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::lowering) enum ScopeEntryInitialization {
    Uninitialized {
        slot: crate::lowering::LocalSlot,
        span: Span,
    },
    Function {
        slot: FrameSlot,
        child: ExecutableId,
        span: Span,
        scoped: bool,
    },
}

impl ScopeEntryInitialization {
    pub(in crate::lowering) const fn order_key(&self) -> (u8, u16) {
        match self {
            Self::Function {
                slot: FrameSlot::Argument(slot),
                ..
            } => (0, slot.0),
            Self::Uninitialized { slot, .. }
            | Self::Function {
                slot: FrameSlot::Local(slot),
                ..
            } => (1, slot.index()),
            Self::Function {
                slot: FrameSlot::Capture(slot),
                ..
            } => (2, *slot),
        }
    }
}
