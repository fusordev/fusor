use oxc_ast::ast::CatchClause;
use oxc_semantic::ScopeId;

use crate::lowering::{CompilerLabel, LocalSlot};

#[derive(Clone, Copy)]
pub(in crate::lowering) struct ScriptCompletionPreservation {
    pub(in crate::lowering) current: LocalSlot,
    pub(in crate::lowering) saved: LocalSlot,
}

#[derive(Clone)]
pub(in crate::lowering) struct FinallyTarget {
    pub(in crate::lowering) label: CompilerLabel,
    pub(in crate::lowering) script_completion: Option<ScriptCompletionPreservation>,
}

pub(in crate::lowering) struct TryFinallyLabels {
    pub(in crate::lowering) handler: CompilerLabel,
    pub(in crate::lowering) finalizer: FinallyTarget,
    pub(in crate::lowering) done: CompilerLabel,
}

pub(in crate::lowering) struct TryFinallyCatchPlan<'statement, 'arena> {
    pub(in crate::lowering) handler: &'statement CatchClause<'arena>,
    pub(in crate::lowering) scope: ScopeId,
    pub(in crate::lowering) body_scope: ScopeId,
    pub(in crate::lowering) rethrow: CompilerLabel,
}

#[derive(Clone)]
pub(in crate::lowering) struct AbruptMarker {
    pub(in crate::lowering) kind: AbruptMarkerKind,
    pub(in crate::lowering) scope_depth: usize,
}

impl AbruptMarker {
    pub(in crate::lowering) const fn new(kind: AbruptMarkerKind, scope_depth: usize) -> Self {
        Self { kind, scope_depth }
    }

    pub(in crate::lowering) const fn tag(&self) -> AbruptMarkerTag {
        match self.kind {
            AbruptMarkerKind::Catch { .. } => AbruptMarkerTag::Catch,
            AbruptMarkerKind::ForIn => AbruptMarkerTag::ForIn,
            AbruptMarkerKind::ForOf => AbruptMarkerTag::ForOf,
            AbruptMarkerKind::FinallySubroutine => AbruptMarkerTag::FinallySubroutine,
        }
    }
}

#[derive(Clone)]
pub(in crate::lowering) enum AbruptMarkerKind {
    Catch { finalizer: Option<FinallyTarget> },
    ForIn,
    ForOf,
    FinallySubroutine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum AbruptMarkerTag {
    Catch,
    ForIn,
    ForOf,
    FinallySubroutine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum IterationMarkerKind {
    ForIn,
    ForOf,
}

impl IterationMarkerKind {
    pub(in crate::lowering) const fn abrupt_kind(self) -> AbruptMarkerKind {
        match self {
            Self::ForIn => AbruptMarkerKind::ForIn,
            Self::ForOf => AbruptMarkerKind::ForOf,
        }
    }

    pub(in crate::lowering) const fn tag(self) -> AbruptMarkerTag {
        match self {
            Self::ForIn => AbruptMarkerTag::ForIn,
            Self::ForOf => AbruptMarkerTag::ForOf,
        }
    }
}
