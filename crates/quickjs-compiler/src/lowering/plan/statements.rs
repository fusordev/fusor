use std::sync::Arc;

use oxc_ast::ast::{
    BlockStatement, Expression, ForStatementLeft, Statement, SwitchStatement, VariableDeclaration,
};
use oxc_semantic::{NodeId, ScopeId};
use quickjs_bytecode::BranchKind;
use quickjs_frontend::Span;

use crate::lowering::{CompilerLabel, LocalSlot, PlannedInstruction};

use super::abrupt::{AbruptMarker, AbruptMarkerKind, AbruptMarkerTag};
use super::control::{ControlRegion, StatementControlStack, SwitchControlLabels};

pub(in crate::lowering) enum StatementWork<'statement, 'arena> {
    Visit(&'statement Statement<'arena>),
    VisitBlock(&'statement BlockStatement<'arena>),
    VisitList {
        statements: &'statement [Statement<'arena>],
        next: usize,
    },
    PushScope {
        scope: ScopeId,
        creator: NodeId,
        span: Span,
    },
    PopScope(ScopeId),
    CloseScope(ScopeId),
    PushStatementStackBase {
        span: Span,
    },
    PopStatementStackBase {
        span: Span,
    },
    PushControl(ControlRegion<'statement>),
    PopControl,
    PushAbruptMarker(AbruptMarkerKind),
    PopAbruptMarker(AbruptMarkerTag),
    SetCompletion(StatementCompletion),
    ForInHead(&'statement ForStatementLeft<'arena>),
    ForInAssignment(&'statement ForStatementLeft<'arena>),
    ForOfHead(&'statement ForStatementLeft<'arena>),
    ForOfAssignment(&'statement ForStatementLeft<'arena>),
    ForOfRotate(ScopeId),
    Declaration(&'statement VariableDeclaration<'arena>),
    Expression(&'statement Expression<'arena>),
    Emit(PlannedInstruction),
    Branch {
        kind: BranchKind,
        target: CompilerLabel,
        span: Span,
    },
    Bind(CompilerLabel),
    SwitchDispatch {
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        next: usize,
    },
    SwitchTrampoline {
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        next: usize,
    },
    SwitchNoMatch {
        labels: Arc<SwitchControlLabels>,
        done: CompilerLabel,
        span: Span,
    },
    SwitchBody {
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        next: usize,
    },
}

pub(in crate::lowering) struct StatementPlanningState<'statement, 'arena> {
    pub(in crate::lowering) work: Vec<StatementWork<'statement, 'arena>>,
    pub(in crate::lowering) active_scopes: Vec<ScopeId>,
    pub(in crate::lowering) controls: StatementControlStack<'statement>,
    pub(in crate::lowering) abrupt_markers: Vec<AbruptMarker>,
    pub(in crate::lowering) completion: StatementCompletion,
}

#[derive(Clone, Copy)]
pub(in crate::lowering) enum StatementCompletion {
    Discard,
    Script(LocalSlot),
}
