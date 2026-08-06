use std::sync::Arc;

use super::super::{
    BindingPattern, CatchClause, CompilationContext, CompiledConstantPool, DeclarationKind,
    DoWhileStatement, ExecutableKind, ExpressionPlanner, ExpressionStatement, FinalOpcode,
    ForInStatement, ForOfStatement, ForStatement, ForStatementInit, FrameLayout,
    FunctionPlanningContext, FunctionTreeLayout, GetSpan, IfStatement, InitializationPolicy,
    LabelIdentifier, LabeledStatement, LeafCompilationError, Operands, PlannedControlFlow,
    ReturnStatement, StoragePlacement, ThrowStatement, TryStatement, UnsupportedLeafFeature,
    WhileStatement, WritePolicy, compact_put_local, plan_put_slot, unsupported,
};

use oxc_ast::ast::{
    BlockStatement, Expression, ForStatementLeft, Statement, SwitchStatement, VariableDeclaration,
};
use oxc_semantic::{NodeId, ScopeId};
use quickjs_bytecode::BranchKind;
use quickjs_frontend::Span;

use crate::lowering::{CompilerLabel, LocalSlot, PlannedInstruction};

use super::abrupt::{
    AbruptMarker, AbruptMarkerKind, AbruptMarkerTag, TryFinallyCatchPlan, TryFinallyLabels,
};
use super::control::{
    ControlRegion, LoopJump, StatementControlStack, SwitchControlLabels,
    switch_scaffold_instruction_count,
};

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
    InitializeInstanceFields,
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

impl<'arena> CompilationContext<'_, 'arena, '_> {
    #[allow(
        clippy::too_many_lines,
        reason = "the iterative statement dispatcher keeps one explicit work-stack loop"
    )]
    pub(in crate::lowering) fn process_statement_work<'statement>(
        &self,
        task: StatementWork<'statement, 'arena>,
        body_span: Span,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        match task {
            StatementWork::VisitList { statements, next } => {
                if let Some(statement) = statements.get(next) {
                    state.work.push(StatementWork::VisitList {
                        statements,
                        next: next + 1,
                    });
                    state.work.push(StatementWork::Visit(statement));
                }
            }
            StatementWork::PushScope {
                scope,
                creator,
                span,
            } => {
                self.plan_scope_entry(scope, creator, span, planning, flow)?;
                state.active_scopes.push(scope);
            }
            StatementWork::PopScope(expected) => {
                if state.active_scopes.len() > 1 {
                    self.plan_scope_exit(planning.executable, expected, planning.layout, flow)?;
                }
                let actual =
                    state
                        .active_scopes
                        .pop()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "statement scope stack is nonempty on exit",
                            span: Some(body_span),
                        })?;
                if actual != expected {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "statement scopes exit in last-in-first-out order",
                        span: Some(body_span),
                    });
                }
            }
            StatementWork::CloseScope(scope) => {
                self.plan_scope_exit(planning.executable, scope, planning.layout, flow)?;
            }
            StatementWork::PushStatementStackBase { span } => {
                flow.push_statement_stack_base(span)?;
            }
            StatementWork::PopStatementStackBase { span } => {
                flow.pop_statement_stack_base(span)?;
            }
            StatementWork::PushControl(mut control) => {
                let owned_iteration_marker = control.owned_iteration_marker;
                if owned_iteration_marker.is_some() {
                    state.abrupt_markers.try_reserve(1).map_err(|_| {
                        LeafCompilationError::CapacityExceeded {
                            domain: "statement abrupt-marker stack",
                        }
                    })?;
                }
                let abrupt_marker_depth = state
                    .abrupt_markers
                    .len()
                    .checked_add(usize::from(owned_iteration_marker.is_some()))
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "statement abrupt-marker depth",
                    })?;
                control.abrupt_marker_depth = Some(abrupt_marker_depth);
                let owned_marker_scope_depth = control.owned_marker_scope_depth;
                state.controls.push(control, body_span)?;
                if let Some(marker) = owned_iteration_marker {
                    let scope_depth = owned_marker_scope_depth.ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "owned iteration marker has a scope depth",
                            span: Some(body_span),
                        },
                    )?;
                    if scope_depth > state.active_scopes.len() {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "owned iteration marker scope remains active",
                            span: Some(body_span),
                        });
                    }
                    state
                        .abrupt_markers
                        .push(AbruptMarker::new(marker.abrupt_kind(), scope_depth));
                }
            }
            StatementWork::PopControl => {
                let control = state.controls.pop(body_span)?;
                let expected_depth =
                    control
                        .abrupt_marker_depth
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "active statement control has an abrupt-marker depth",
                            span: Some(body_span),
                        })?;
                if state.abrupt_markers.len() != expected_depth {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "statement control exits at its abrupt-marker depth",
                        span: Some(body_span),
                    });
                }
                if let Some(marker) = control.owned_iteration_marker
                    && state.abrupt_markers.pop().map(|marker| marker.tag()) != Some(marker.tag())
                {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "iteration control owns the innermost abrupt marker",
                        span: Some(body_span),
                    });
                }
            }
            StatementWork::PushAbruptMarker(kind) => {
                state.abrupt_markers.try_reserve(1).map_err(|_| {
                    LeafCompilationError::CapacityExceeded {
                        domain: "statement abrupt-marker stack",
                    }
                })?;
                state
                    .abrupt_markers
                    .push(AbruptMarker::new(kind, state.active_scopes.len()));
            }
            StatementWork::PopAbruptMarker(expected) => {
                if state.abrupt_markers.pop().map(|marker| marker.tag()) != Some(expected) {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "statement abrupt markers exit in last-in-first-out order",
                        span: Some(body_span),
                    });
                }
            }
            StatementWork::SetCompletion(completion) => {
                state.completion = completion;
            }
            StatementWork::ForInHead(left) => self.plan_for_in_head(
                left,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                &state.abrupt_markers,
                flow,
            )?,
            StatementWork::ForOfHead(left) => {
                self.plan_for_of_head(left, planning.layout)?;
            }
            StatementWork::ForInAssignment(left) => self.plan_for_in_assignment(
                left,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                &state.abrupt_markers,
                flow,
            )?,
            StatementWork::ForOfAssignment(left) => self.plan_for_of_assignment(
                left,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                &state.abrupt_markers,
                flow,
            )?,
            StatementWork::ForOfRotate(scope) => {
                self.plan_for_of_rotation(planning.executable, scope, planning.layout, flow)?;
            }
            StatementWork::Expression(expression) => self.plan_expression_with_abrupt_markers(
                expression,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                &state.abrupt_markers,
                flow,
            )?,
            StatementWork::InitializeInstanceFields => {
                ExpressionPlanner::new(self).plan_instance_field_initializations(
                    planning.executable,
                    planning.layout,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
            StatementWork::Declaration(declaration) => self.validate_declaration(
                declaration,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                &state.abrupt_markers,
                flow,
            )?,
            StatementWork::Emit(instruction) => flow.emit(instruction)?,
            StatementWork::Branch { kind, target, span } => {
                flow.branch(kind, &target, span)?;
            }
            StatementWork::Bind(label) => flow.bind(&label)?,
            StatementWork::SwitchDispatch {
                statement,
                labels,
                next,
            } => Self::schedule_next_switch_dispatch(statement, labels, next, &mut state.work)?,
            StatementWork::SwitchTrampoline {
                statement,
                labels,
                next,
            } => Self::schedule_next_switch_trampoline(statement, labels, next, &mut state.work)?,
            StatementWork::SwitchNoMatch { labels, done, span } => {
                Self::schedule_switch_no_match(&labels, done, span, &mut state.work);
            }
            StatementWork::SwitchBody {
                statement,
                labels,
                next,
            } => Self::schedule_next_switch_body(statement, labels, next, &mut state.work)?,
            StatementWork::Visit(statement) => self.plan_statement(
                statement,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
                state,
            )?,
            StatementWork::VisitBlock(block) => {
                self.schedule_block_statement(block, state)?;
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the statement-kind dispatcher keeps lowering decisions in one exhaustive match"
    )]
    fn plan_statement<'statement>(
        &self,
        statement: &'statement Statement<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        match statement {
            Statement::BlockStatement(block) => {
                self.schedule_block_statement(block, state)?;
            }
            Statement::FunctionDeclaration(function) => {
                self.validate_function_declaration(
                    function,
                    layout.executable,
                    tree_layout,
                    state.active_scopes.last().copied(),
                )?;
            }
            Statement::ClassDeclaration(class) => {
                ExpressionPlanner::new(self).plan_base_class_declaration(
                    class,
                    layout,
                    tree_layout,
                    constants,
                    flow,
                )?;
            }
            Statement::VariableDeclaration(declaration) => {
                self.validate_declaration(
                    declaration,
                    layout,
                    tree_layout,
                    constants,
                    &state.abrupt_markers,
                    flow,
                )?;
            }
            Statement::ExpressionStatement(statement) => {
                Self::schedule_expression_statement(statement, state.completion, &mut state.work);
            }
            Statement::DebuggerStatement(statement) => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Nop,
                    Operands::None,
                    statement.span,
                ))?;
            }
            Statement::EmptyStatement(_) => {}
            Statement::ReturnStatement(statement) => {
                let executable = self.planned.plan.executable(layout.executable).ok_or(
                    LeafCompilationError::InvalidExecutable {
                        executable: layout.executable,
                    },
                )?;
                let async_generator = matches!(
                    executable.kind(),
                    ExecutableKind::Function {
                        asynchronous: true,
                        generator: true,
                    }
                );
                let return_opcode = if async_generator
                    || matches!(
                        executable.kind(),
                        ExecutableKind::Function {
                            asynchronous: true,
                            generator: false,
                        } | ExecutableKind::Function {
                            asynchronous: false,
                            generator: true,
                        }
                    ) {
                    FinalOpcode::ReturnAsync
                } else {
                    FinalOpcode::Return
                };
                Self::schedule_return_statement(
                    statement,
                    &state.abrupt_markers,
                    return_opcode,
                    async_generator,
                    flow,
                    &mut state.work,
                )?;
            }
            Statement::ThrowStatement(statement) => {
                Self::schedule_throw_statement(statement, &state.abrupt_markers, &mut state.work)?;
            }
            Statement::IfStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_if_statement(statement, flow, &mut state.work)?;
            }
            Statement::WhileStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_while_statement(
                    statement,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                    Vec::new(),
                )?;
            }
            Statement::DoWhileStatement(statement) => {
                Self::schedule_do_while_statement(
                    statement,
                    state.completion,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                    Vec::new(),
                )?;
            }
            Statement::ForStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::ForInStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_in_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::ForOfStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_of_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::BreakStatement(statement) => {
                self.plan_control_jump(
                    statement.label.as_ref(),
                    statement.span,
                    LoopJump::Break,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::ContinueStatement(statement) => {
                self.plan_control_jump(
                    statement.label.as_ref(),
                    statement.span,
                    LoopJump::Continue,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::LabeledStatement(statement) => {
                self.plan_labeled_statement(statement, flow, state)?;
            }
            Statement::SwitchStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_switch_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::TryStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_try_statement(statement, layout, flow, state)?;
            }
            _ => {
                return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span());
            }
        }
        Ok(())
    }

    fn schedule_block_statement<'statement>(
        &self,
        block: &'statement BlockStatement<'arena>,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(block.scope_id.get(), block.node_id.get(), block.span)?;
        state.work.push(StatementWork::PopScope(scope));
        state.work.push(StatementWork::VisitList {
            statements: &block.body,
            next: 0,
        });
        state.work.push(StatementWork::PushScope {
            scope,
            creator: block.node_id.get(),
            span: block.span,
        });
        Ok(())
    }

    fn schedule_expression_statement<'statement>(
        statement: &'statement ExpressionStatement<'arena>,
        completion: StatementCompletion,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        let (opcode, operands) = match completion {
            StatementCompletion::Discard => (FinalOpcode::Drop, Operands::None),
            StatementCompletion::Script(slot) => compact_put_local(slot),
        };
        work.push(StatementWork::Emit(PlannedInstruction::new(
            opcode,
            operands,
            statement.expression.span(),
        )));
        work.push(StatementWork::Expression(&statement.expression));
    }

    fn reset_script_completion(
        completion: StatementCompletion,
        span: Span,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let StatementCompletion::Script(slot) = completion else {
            return Ok(());
        };
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            span,
        ))?;
        let (opcode, operands) = compact_put_local(slot);
        flow.emit(PlannedInstruction::new(opcode, operands, span))
    }

    fn schedule_return_statement<'statement>(
        statement: &'statement ReturnStatement<'arena>,
        abrupt_markers: &[AbruptMarker],
        return_opcode: FinalOpcode,
        await_value: bool,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let crosses_finalizer = abrupt_markers
            .iter()
            .any(|marker| matches!(&marker.kind, AbruptMarkerKind::Catch { finalizer: Some(_) }));
        let has_pending_finally_subroutine = abrupt_markers
            .iter()
            .any(|marker| matches!(&marker.kind, AbruptMarkerKind::FinallySubroutine));
        let closes_iterator = abrupt_markers
            .iter()
            .any(|marker| matches!(&marker.kind, AbruptMarkerKind::ForOf));
        let has_physical_marker = abrupt_markers.iter().any(|marker| {
            matches!(
                &marker.kind,
                AbruptMarkerKind::Catch { .. } | AbruptMarkerKind::ForIn | AbruptMarkerKind::ForOf
            )
        });
        if let Some(argument) = &statement.argument {
            Self::reserve_return_work(abrupt_markers, work)?;
            work.push(StatementWork::Emit(PlannedInstruction::new(
                return_opcode,
                Operands::None,
                statement.span,
            )));
            Self::schedule_value_return_cleanup(abrupt_markers, statement.span, work);
            if await_value {
                work.push(StatementWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Await,
                    Operands::None,
                    statement.span,
                )));
            }
            work.push(StatementWork::Expression(argument));
        } else if crosses_finalizer
            || closes_iterator
            || (has_pending_finally_subroutine && has_physical_marker)
        {
            Self::reserve_return_work(abrupt_markers, work)?;
            work.push(StatementWork::Emit(PlannedInstruction::new(
                return_opcode,
                Operands::None,
                statement.span,
            )));
            Self::schedule_value_return_cleanup(abrupt_markers, statement.span, work);
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                statement.span,
            )));
        } else {
            for marker in abrupt_markers.iter().rev() {
                match &marker.kind {
                    AbruptMarkerKind::Catch { .. } | AbruptMarkerKind::ForIn => {
                        flow.emit(PlannedInstruction::new(
                            FinalOpcode::Drop,
                            Operands::None,
                            statement.span,
                        ))?;
                    }
                    AbruptMarkerKind::ForOf => {
                        flow.emit(PlannedInstruction::new(
                            FinalOpcode::IteratorClose,
                            Operands::None,
                            statement.span,
                        ))?;
                    }
                    AbruptMarkerKind::FinallySubroutine => {}
                }
            }
            if return_opcode == FinalOpcode::ReturnAsync {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Undefined,
                    Operands::None,
                    statement.span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::ReturnAsync,
                    Operands::None,
                    statement.span,
                ))?;
            } else {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::ReturnUndef,
                    Operands::None,
                    statement.span,
                ))?;
            }
        }
        Ok(())
    }

    fn reserve_return_work<'statement>(
        abrupt_markers: &[AbruptMarker],
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let capacity = abrupt_markers
            .len()
            .checked_mul(4)
            .and_then(|capacity| capacity.checked_add(2))
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "statement return cleanup",
            })?;
        work.try_reserve(capacity)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })
    }

    fn schedule_value_return_cleanup<'statement>(
        abrupt_markers: &[AbruptMarker],
        span: Span,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        for marker in abrupt_markers {
            match &marker.kind {
                AbruptMarkerKind::Catch { finalizer } => {
                    if let Some(finalizer) = finalizer {
                        work.push(StatementWork::Branch {
                            kind: BranchKind::Gosub,
                            target: finalizer.clone(),
                            span,
                        });
                    }
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::NipCatch,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::ForIn => {
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Nip,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::ForOf => {
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::IteratorClose,
                        Operands::None,
                        span,
                    )));
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Undefined,
                        Operands::None,
                        span,
                    )));
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Rot3r,
                        Operands::None,
                        span,
                    )));
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::NipCatch,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::FinallySubroutine => {
                    for _ in 0..2 {
                        work.push(StatementWork::Emit(PlannedInstruction::new(
                            FinalOpcode::Nip,
                            Operands::None,
                            span,
                        )));
                    }
                }
            }
        }
    }

    fn schedule_throw_statement<'statement>(
        statement: &'statement ThrowStatement<'arena>,
        abrupt_markers: &[AbruptMarker],
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let removable_markers = abrupt_markers
            .iter()
            .rposition(|marker| marker.tag() == AbruptMarkerTag::Catch)
            .map_or(abrupt_markers, |catch| &abrupt_markers[catch + 1..]);
        let preserves_for_of_marker = removable_markers
            .iter()
            .any(|marker| marker.tag() == AbruptMarkerTag::ForOf);
        let cleanup_instructions = if preserves_for_of_marker {
            0
        } else {
            removable_markers
                .iter()
                .try_fold(0_usize, |count, marker| {
                    count.checked_add(match marker.tag() {
                        AbruptMarkerTag::ForIn => 1,
                        AbruptMarkerTag::FinallySubroutine => 2,
                        AbruptMarkerTag::Catch | AbruptMarkerTag::ForOf => 0,
                    })
                })
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "statement throw cleanup",
                })?
        };
        work.try_reserve(cleanup_instructions.saturating_add(2))
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Throw,
            Operands::None,
            statement.span,
        )));
        if !preserves_for_of_marker {
            for marker in removable_markers {
                let nips = match marker.tag() {
                    AbruptMarkerTag::ForIn => 1,
                    AbruptMarkerTag::FinallySubroutine => 2,
                    AbruptMarkerTag::Catch | AbruptMarkerTag::ForOf => 0,
                };
                for _ in 0..nips {
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Nip,
                        Operands::None,
                        statement.span,
                    )));
                }
            }
        }
        work.push(StatementWork::Expression(&statement.argument));
        Ok(())
    }

    fn plan_try_statement<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(finalizer) = &statement.finalizer {
            return self.plan_try_finally_statement(statement, finalizer, layout, flow, state);
        }
        self.plan_catch_only_statement(statement, layout, flow, state)
    }

    fn plan_catch_only_statement<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let handler = statement
            .handler
            .as_ref()
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBody,
                span: statement.span,
            })?;
        let catch_scope =
            self.created_scope(handler.scope_id.get(), handler.node_id.get(), handler.span)?;
        let catch_body_scope = self.created_scope(
            handler.body.scope_id.get(),
            handler.body.node_id.get(),
            handler.body.span,
        )?;
        let binding = self.plan_catch_binding(handler, catch_body_scope, layout)?;

        let handler_target = flow.new_statement_label_with_offset(handler.span, 1)?;
        let done = flow.new_statement_label(statement.span)?;
        state
            .work
            .try_reserve(15)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        state.work.push(StatementWork::Bind(done.clone()));
        state.work.push(StatementWork::PopScope(catch_scope));
        state.work.push(StatementWork::VisitBlock(&handler.body));
        state.work.push(StatementWork::Emit(binding));
        state.work.push(StatementWork::PushScope {
            scope: catch_scope,
            creator: handler.node_id.get(),
            span: handler.span,
        });
        state.work.push(StatementWork::Bind(handler_target.clone()));
        state.work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: statement.span,
        });
        state.work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        state
            .work
            .push(StatementWork::PopAbruptMarker(AbruptMarkerTag::Catch));
        state.work.push(StatementWork::PopStatementStackBase {
            span: statement.span,
        });
        state.work.push(StatementWork::VisitBlock(&statement.block));
        state
            .work
            .push(StatementWork::PushAbruptMarker(AbruptMarkerKind::Catch {
                finalizer: None,
            }));
        state.work.push(StatementWork::PushStatementStackBase {
            span: statement.span,
        });
        state.work.push(StatementWork::Branch {
            kind: BranchKind::Catch,
            target: handler_target,
            span: statement.span,
        });
        Ok(())
    }

    fn plan_try_finally_statement<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        finalizer: &'statement BlockStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let labels = TryFinallyLabels {
            handler: flow.new_statement_label_with_offset(statement.span, 1)?,
            finalizer: flow.new_statement_label_with_offset(finalizer.span, 2)?,
            done: flow.new_statement_label(statement.span)?,
        };
        let catch_plan = self.create_try_finally_catch_plan(statement, layout, flow)?;

        state
            .work
            .try_reserve(if catch_plan.is_some() { 48 } else { 32 })
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;

        Self::push_finalizer_subroutine(&mut state.work, finalizer, &labels, state.completion);
        Self::push_try_finally_handler_path(&mut state.work, statement, catch_plan, &labels);
        Self::push_try_finally_body(&mut state.work, statement, &labels);
        Ok(())
    }

    fn create_try_finally_catch_plan<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<Option<TryFinallyCatchPlan<'statement, 'arena>>, LeafCompilationError> {
        let Some(handler) = &statement.handler else {
            return Ok(None);
        };
        let scope =
            self.created_scope(handler.scope_id.get(), handler.node_id.get(), handler.span)?;
        let body_scope = self.created_scope(
            handler.body.scope_id.get(),
            handler.body.node_id.get(),
            handler.body.span,
        )?;
        let binding = self.plan_catch_binding(handler, body_scope, layout)?;
        let rethrow = flow.new_statement_label_with_offset(handler.body.span, 1)?;
        Ok(Some(TryFinallyCatchPlan {
            handler,
            scope,
            binding,
            rethrow,
        }))
    }

    fn push_finalizer_subroutine<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        finalizer: &'statement BlockStatement<'arena>,
        labels: &TryFinallyLabels,
        completion: StatementCompletion,
    ) {
        work.push(StatementWork::Bind(labels.done.clone()));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Ret,
            Operands::None,
            finalizer.span,
        )));
        for _ in 0..2 {
            work.push(StatementWork::PopStatementStackBase {
                span: finalizer.span,
            });
        }
        work.push(StatementWork::PopAbruptMarker(
            AbruptMarkerTag::FinallySubroutine,
        ));
        work.push(StatementWork::SetCompletion(completion));
        work.push(StatementWork::VisitBlock(finalizer));
        work.push(StatementWork::SetCompletion(StatementCompletion::Discard));
        work.push(StatementWork::PushAbruptMarker(
            AbruptMarkerKind::FinallySubroutine,
        ));
        for _ in 0..2 {
            work.push(StatementWork::PushStatementStackBase {
                span: finalizer.span,
            });
        }
        work.push(StatementWork::Bind(labels.finalizer.clone()));
    }

    fn push_try_finally_handler_path<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        statement: &'statement TryStatement<'arena>,
        catch_plan: Option<TryFinallyCatchPlan<'statement, 'arena>>,
        labels: &TryFinallyLabels,
    ) {
        if let Some(catch) = catch_plan {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Throw,
                Operands::None,
                catch.handler.body.span,
            )));
            work.push(StatementWork::Branch {
                kind: BranchKind::Gosub,
                target: labels.finalizer.clone(),
                span: catch.handler.body.span,
            });
            work.push(StatementWork::Bind(catch.rethrow.clone()));

            Self::push_normal_finalizer_path(
                work,
                &labels.finalizer,
                &labels.done,
                catch.handler.body.span,
            );
            work.push(StatementWork::PopScope(catch.scope));
            work.push(StatementWork::PopAbruptMarker(AbruptMarkerTag::Catch));
            work.push(StatementWork::PopStatementStackBase {
                span: catch.handler.body.span,
            });
            work.push(StatementWork::VisitBlock(&catch.handler.body));
            work.push(StatementWork::PushAbruptMarker(AbruptMarkerKind::Catch {
                finalizer: Some(labels.finalizer.clone()),
            }));
            work.push(StatementWork::PushStatementStackBase {
                span: catch.handler.body.span,
            });
            work.push(StatementWork::Branch {
                kind: BranchKind::Catch,
                target: catch.rethrow,
                span: catch.handler.body.span,
            });
            work.push(StatementWork::Emit(catch.binding));
            work.push(StatementWork::PushScope {
                scope: catch.scope,
                creator: catch.handler.node_id.get(),
                span: catch.handler.span,
            });
            work.push(StatementWork::Bind(labels.handler.clone()));
        } else {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Throw,
                Operands::None,
                statement.span,
            )));
            work.push(StatementWork::Branch {
                kind: BranchKind::Gosub,
                target: labels.finalizer.clone(),
                span: statement.span,
            });
            work.push(StatementWork::Bind(labels.handler.clone()));
        }
    }

    fn push_try_finally_body<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        statement: &'statement TryStatement<'arena>,
        labels: &TryFinallyLabels,
    ) {
        Self::push_normal_finalizer_path(
            work,
            &labels.finalizer,
            &labels.done,
            statement.block.span,
        );
        work.push(StatementWork::PopAbruptMarker(AbruptMarkerTag::Catch));
        work.push(StatementWork::PopStatementStackBase {
            span: statement.block.span,
        });
        work.push(StatementWork::VisitBlock(&statement.block));
        work.push(StatementWork::PushAbruptMarker(AbruptMarkerKind::Catch {
            finalizer: Some(labels.finalizer.clone()),
        }));
        work.push(StatementWork::PushStatementStackBase {
            span: statement.block.span,
        });
        work.push(StatementWork::Branch {
            kind: BranchKind::Catch,
            target: labels.handler.clone(),
            span: statement.span,
        });
    }

    fn push_normal_finalizer_path<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        finalizer: &CompilerLabel,
        done: &CompilerLabel,
        span: Span,
    ) {
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: done.clone(),
            span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        )));
        work.push(StatementWork::Branch {
            kind: BranchKind::Gosub,
            target: finalizer.clone(),
            span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            span,
        )));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        )));
    }

    fn plan_catch_binding(
        &self,
        handler: &CatchClause<'arena>,
        catch_body_scope: ScopeId,
        layout: &FrameLayout,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        Ok(match &handler.param {
            None => PlannedInstruction::new(FinalOpcode::Drop, Operands::None, handler.span),
            Some(parameter) => {
                let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedBinding,
                        parameter.pattern.span(),
                    );
                };
                let binding =
                    self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "catch binding has compiler storage",
                        span: Some(identifier.span),
                    },
                )?;
                if storage.executable() != layout.executable
                    || storage.placement() != StoragePlacement::Local
                    || storage.policy().kind() != DeclarationKind::Catch
                    || storage.policy().initialization() != InitializationPolicy::Catch
                    || storage.policy().writes() != WritePolicy::Mutable
                    || storage.policy().has_temporal_dead_zone()
                {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedBinding,
                        identifier.span,
                    );
                }
                if self.scope_for_binding(binding)? != catch_body_scope {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "catch binding belongs to the catch-body scope",
                        span: Some(identifier.span),
                    });
                }
                let slot = layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedBinding,
                        span: identifier.span,
                    })?;
                plan_put_slot(slot, identifier.span)
            }
        })
    }

    fn plan_for_statement<'statement>(
        &self,
        statement: &'statement ForStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        Self::schedule_for_statement(
            statement,
            scope,
            flow,
            &mut state.work,
            state.active_scopes.len(),
            labels,
        )
    }

    fn plan_for_in_statement<'statement>(
        &self,
        statement: &'statement ForInStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        Self::schedule_for_in_statement(
            statement,
            scope,
            flow,
            &mut state.work,
            state.active_scopes.len(),
            labels,
        )
    }

    fn plan_for_of_statement<'statement>(
        &self,
        statement: &'statement ForOfStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        if statement.r#await {
            return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span);
        }
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        Self::schedule_for_of_statement(
            statement,
            scope,
            flow,
            &mut state.work,
            state.active_scopes.len(),
            labels,
        )
    }

    fn plan_labeled_statement<'statement>(
        &self,
        statement: &'statement LabeledStatement<'arena>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let mut labels = Vec::new();
        labels
            .try_reserve(1)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "labeled statement chain",
            })?;
        labels.push(statement.label.name.as_str());
        let mut body = &statement.body;
        while let Statement::LabeledStatement(nested) = body {
            labels
                .try_reserve(1)
                .map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "labeled statement chain",
                })?;
            labels.push(nested.label.name.as_str());
            body = &nested.body;
        }

        match body {
            Statement::WhileStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_while_statement(
                    statement,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                    labels,
                )
            }
            Statement::DoWhileStatement(statement) => Self::schedule_do_while_statement(
                statement,
                state.completion,
                flow,
                &mut state.work,
                state.active_scopes.len(),
                labels,
            ),
            Statement::ForStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_statement(statement, labels, flow, state)
            }
            Statement::ForInStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_in_statement(statement, labels, flow, state)
            }
            Statement::ForOfStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_of_statement(statement, labels, flow, state)
            }
            Statement::SwitchStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_switch_statement(statement, labels, flow, state)
            }
            body => {
                let done = flow.new_statement_label(statement.span)?;
                let control = ControlRegion::breakable(
                    labels,
                    done.clone(),
                    false,
                    state.active_scopes.len(),
                );
                state.work.push(StatementWork::Bind(done));
                state.work.push(StatementWork::PopControl);
                state.work.push(StatementWork::Visit(body));
                state.work.push(StatementWork::PushControl(control));
                Ok(())
            }
        }
    }

    fn schedule_if_statement<'statement>(
        statement: &'statement IfStatement<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(alternate_statement) = &statement.alternate {
            let alternate = flow.new_statement_label(alternate_statement.span())?;
            let done = flow.new_statement_label(statement.span)?;
            work.push(StatementWork::Bind(done.clone()));
            work.push(StatementWork::Visit(alternate_statement));
            work.push(StatementWork::Bind(alternate.clone()));
            work.push(StatementWork::Branch {
                kind: BranchKind::Goto,
                target: done,
                span: statement.span,
            });
            work.push(StatementWork::Visit(&statement.consequent));
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: alternate,
                span: statement.test.span(),
            });
        } else {
            let done = flow.new_statement_label(statement.span)?;
            work.push(StatementWork::Bind(done.clone()));
            work.push(StatementWork::Visit(&statement.consequent));
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: done,
                span: statement.test.span(),
            });
        }
        work.push(StatementWork::Expression(&statement.test));
        Ok(())
    }

    fn schedule_while_statement<'statement>(
        statement: &'statement WhileStatement<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::iteration(labels, done.clone(), test.clone(), scope_depth);
        work.push(StatementWork::Bind(done.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: test.clone(),
            span: statement.span,
        });
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfFalse,
            target: done,
            span: statement.test.span(),
        });
        work.push(StatementWork::Expression(&statement.test));
        work.push(StatementWork::Bind(test));
        Ok(())
    }

    fn schedule_do_while_statement<'statement>(
        statement: &'statement DoWhileStatement<'arena>,
        completion: StatementCompletion,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let iteration = flow.new_statement_label(statement.body.span())?;
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::iteration(labels, done.clone(), test.clone(), scope_depth);
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfTrue,
            target: iteration.clone(),
            span: statement.test.span(),
        });
        work.push(StatementWork::Expression(&statement.test));
        work.push(StatementWork::Bind(test));
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        if let StatementCompletion::Script(slot) = completion {
            let (opcode, operands) = compact_put_local(slot);
            work.push(StatementWork::Emit(PlannedInstruction::new(
                opcode,
                operands,
                statement.span,
            )));
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                statement.span,
            )));
        }
        work.push(StatementWork::Bind(iteration));
        Ok(())
    }

    pub(in crate::lowering) fn schedule_for_statement<'statement>(
        statement: &'statement ForStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let test = flow.new_statement_label(
            statement
                .test
                .as_ref()
                .map_or(statement.span, GetSpan::span),
        )?;
        let rotate = flow.new_statement_label(
            statement
                .update
                .as_ref()
                .map_or(statement.span, GetSpan::span),
        )?;
        let done = flow.new_statement_label(statement.span)?;
        let loop_scope_depth =
            enclosing_scope_depth
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "statement scope depth",
                })?;
        let control =
            ControlRegion::iteration(labels, done.clone(), rotate.clone(), loop_scope_depth);

        work.push(StatementWork::PopScope(scope));
        work.push(StatementWork::Bind(done.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: test.clone(),
            span: statement.span,
        });
        if let Some(update) = &statement.update {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                update.span(),
            )));
            work.push(StatementWork::Expression(update));
        }
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Bind(rotate));
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        if let Some(test_expression) = &statement.test {
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: done,
                span: test_expression.span(),
            });
            work.push(StatementWork::Expression(test_expression));
        }
        work.push(StatementWork::Bind(test));
        work.push(StatementWork::CloseScope(scope));
        if let Some(initializer) = &statement.init {
            match initializer {
                ForStatementInit::VariableDeclaration(declaration) => {
                    work.push(StatementWork::Declaration(declaration));
                }
                initializer => {
                    let expression = initializer.to_expression();
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        expression.span(),
                    )));
                    work.push(StatementWork::Expression(expression));
                }
            }
        }
        work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        Ok(())
    }

    fn schedule_for_in_statement<'statement>(
        statement: &'statement ForInStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let next = flow.new_statement_label_with_offset(statement.right.span(), 1)?;
        let assign = flow.new_statement_label_with_offset(statement.left.span(), 2)?;
        let rotate = flow.new_statement_label_with_offset(statement.span, 1)?;
        let cleanup = flow.new_statement_label_with_offset(statement.span, 1)?;
        let done = flow.new_statement_label(statement.span)?;
        let loop_scope_depth =
            enclosing_scope_depth
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "statement scope depth",
                })?;
        let control = ControlRegion::for_in_iteration(
            labels,
            cleanup.clone(),
            rotate.clone(),
            loop_scope_depth,
        );

        work.try_reserve(25)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::PopScope(scope));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Bind(cleanup.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: next.clone(),
            span: statement.span,
        });
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Bind(rotate.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: rotate,
            span: statement.body.span(),
        });
        work.push(StatementWork::PopStatementStackBase {
            span: statement.span,
        });
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        work.push(StatementWork::PushStatementStackBase {
            span: statement.span,
        });
        work.push(StatementWork::ForInAssignment(&statement.left));
        work.push(StatementWork::Bind(assign.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: cleanup,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfFalse,
            target: assign,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForInNext,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Bind(next));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForInStart,
            Operands::None,
            statement.right.span(),
        )));
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Expression(&statement.right));
        work.push(StatementWork::ForInHead(&statement.left));
        work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        Ok(())
    }

    fn schedule_for_of_statement<'statement>(
        statement: &'statement ForOfStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let next = flow.new_statement_label_with_offset(statement.right.span(), 3)?;
        let assign = flow.new_statement_label_with_offset(statement.left.span(), 4)?;
        let rotate = flow.new_statement_label_with_offset(statement.span, 3)?;
        let cleanup = flow.new_statement_label_with_offset(statement.span, 3)?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::for_of_iteration(
            labels,
            cleanup.clone(),
            next.clone(),
            enclosing_scope_depth,
        );

        work.try_reserve(32)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::PopScope(scope));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::IteratorClose,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Bind(cleanup.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: next.clone(),
            span: statement.span,
        });
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Bind(rotate.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: rotate,
            span: statement.body.span(),
        });
        for _ in 0..3 {
            work.push(StatementWork::PopStatementStackBase {
                span: statement.span,
            });
        }
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        for _ in 0..3 {
            work.push(StatementWork::PushStatementStackBase {
                span: statement.span,
            });
        }
        work.push(StatementWork::ForOfAssignment(&statement.left));
        work.push(StatementWork::Bind(assign.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: cleanup,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfFalse,
            target: assign,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForOfNext,
            Operands::U8(0),
            statement.span,
        )));
        work.push(StatementWork::ForOfRotate(scope));
        work.push(StatementWork::Bind(next));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForOfStart,
            Operands::None,
            statement.right.span(),
        )));
        // The RHS closes the initial lexical environment so closures created
        // there keep the TDZ cell rather than the first iteration's cell.
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Expression(&statement.right));
        work.push(StatementWork::ForOfHead(&statement.left));
        work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        Ok(())
    }

    fn plan_switch_statement<'statement>(
        &self,
        statement: &'statement SwitchStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        let scope_depth = state.active_scopes.len().checked_add(1).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "statement scope depth",
            },
        )?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::breakable(labels, done.clone(), true, scope_depth);
        let switch_labels = Arc::new(Self::prepare_switch_control_labels(statement, flow)?);

        state
            .work
            .try_reserve(10)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        state.work.push(StatementWork::PopScope(scope));
        state.work.push(StatementWork::Bind(done.clone()));
        state.work.push(StatementWork::PopControl);
        state.work.push(StatementWork::SwitchBody {
            statement,
            labels: Arc::clone(&switch_labels),
            next: 0,
        });
        state.work.push(StatementWork::SwitchNoMatch {
            labels: Arc::clone(&switch_labels),
            done,
            span: statement.span,
        });
        state.work.push(StatementWork::SwitchTrampoline {
            statement,
            labels: Arc::clone(&switch_labels),
            next: 0,
        });
        state.work.push(StatementWork::SwitchDispatch {
            statement,
            labels: switch_labels,
            next: 0,
        });
        state.work.push(StatementWork::PushControl(control));
        state.work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        state
            .work
            .push(StatementWork::Expression(&statement.discriminant));
        Ok(())
    }

    fn prepare_switch_control_labels(
        statement: &SwitchStatement<'arena>,
        flow: &mut PlannedControlFlow,
    ) -> Result<SwitchControlLabels, LeafCompilationError> {
        let mut default_index = None;
        for (index, case) in statement.cases.iter().enumerate() {
            if case.test.is_none() && default_index.replace(index).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "Oxc accepts at most one switch default clause",
                    span: Some(case.span),
                });
            }
        }
        let tested_count = statement
            .cases
            .len()
            .checked_sub(usize::from(default_index.is_some()))
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "switch tested case count",
            })?;
        let scaffold_instructions = switch_scaffold_instruction_count(
            statement.cases.len(),
            tested_count,
            default_index.is_some(),
        )?;
        flow.ensure_additional_instruction_capacity(scaffold_instructions, statement.span)?;

        let mut body = Vec::new();
        body.try_reserve_exact(statement.cases.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "switch body labels",
            }
        })?;
        let mut matched = Vec::new();
        matched
            .try_reserve_exact(statement.cases.len())
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "switch match labels",
            })?;
        for case in &statement.cases {
            body.push(flow.new_statement_label(case.span)?);
            matched.push(flow.new_statement_label_with_offset(case.span, 1)?);
        }
        let no_match = if default_index.is_none() {
            Some(flow.new_statement_label_with_offset(statement.span, 1)?)
        } else {
            None
        };
        let fallback = match default_index {
            Some(index) => {
                matched
                    .get(index)
                    .cloned()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "switch default index names a case",
                        span: Some(statement.span),
                    })?
            }
            None => no_match
                .clone()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "switch without a default has a no-match target",
                    span: Some(statement.span),
                })?,
        };
        Ok(SwitchControlLabels {
            body,
            matched,
            fallback,
            no_match,
        })
    }

    fn schedule_next_switch_dispatch<'statement>(
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        mut next: usize,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        while let Some(case) = statement.cases.get(next) {
            let index = next;
            next = next
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "switch dispatch case index",
                })?;
            let Some(test) = &case.test else {
                continue;
            };
            let target = labels.matched.get(index).cloned().ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a match target",
                    span: Some(case.span),
                },
            )?;
            work.try_reserve(5)
                .map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "statement work stack",
                })?;
            work.push(StatementWork::SwitchDispatch {
                statement,
                labels,
                next,
            });
            work.push(StatementWork::Branch {
                kind: BranchKind::IfTrue,
                target,
                span: test.span(),
            });
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::StrictEq,
                Operands::None,
                test.span(),
            )));
            work.push(StatementWork::Expression(test));
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                test.span(),
            )));
            return Ok(());
        }
        work.try_reserve(1)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: labels.fallback.clone(),
            span: statement.span,
        });
        Ok(())
    }

    fn schedule_next_switch_trampoline<'statement>(
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        next: usize,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let Some(case) = statement.cases.get(next) else {
            return Ok(());
        };
        let match_target =
            labels
                .matched
                .get(next)
                .cloned()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a match trampoline",
                    span: Some(case.span),
                })?;
        let body_target =
            labels
                .body
                .get(next)
                .cloned()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a body target",
                    span: Some(case.span),
                })?;
        let next = next
            .checked_add(1)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "switch trampoline case index",
            })?;
        work.try_reserve(4)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::SwitchTrampoline {
            statement,
            labels,
            next,
        });
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: body_target,
            span: case.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            case.span,
        )));
        work.push(StatementWork::Bind(match_target));
        Ok(())
    }

    fn schedule_switch_no_match<'statement>(
        labels: &SwitchControlLabels,
        done: CompilerLabel,
        span: Span,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        let Some(no_match) = &labels.no_match else {
            return;
        };
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        )));
        work.push(StatementWork::Bind(no_match.clone()));
    }

    fn schedule_next_switch_body<'statement>(
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        next: usize,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let Some(case) = statement.cases.get(next) else {
            return Ok(());
        };
        let body_target =
            labels
                .body
                .get(next)
                .cloned()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a body target",
                    span: Some(case.span),
                })?;
        let next = next
            .checked_add(1)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "switch body case index",
            })?;
        work.try_reserve(3)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::SwitchBody {
            statement,
            labels,
            next,
        });
        work.push(StatementWork::VisitList {
            statements: &case.consequent,
            next: 0,
        });
        work.push(StatementWork::Bind(body_target));
        Ok(())
    }

    pub(in crate::lowering) fn plan_control_jump<'statement>(
        &self,
        label: Option<&'statement LabelIdentifier<'arena>>,
        statement_span: Span,
        jump: LoopJump,
        state: &StatementPlanningState<'statement, 'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let (_, control) = state
            .controls
            .resolve(label.map(|label| label.name.as_str()), jump)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: if label.is_some() {
                    jump.missing_label_invariant()
                } else {
                    jump.missing_region_invariant()
                },
                span: Some(statement_span),
            })?;
        let target = jump
            .target(control)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: jump.invalid_labeled_target_invariant(),
                span: Some(statement_span),
            })?;
        if control.scope_depth > state.active_scopes.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: jump.scope_invariant(),
                span: Some(statement_span),
            });
        }
        let abrupt_marker_depth =
            control
                .abrupt_marker_depth
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "active statement control has an abrupt-marker depth",
                    span: Some(statement_span),
                })?;
        let crossed_markers = state.abrupt_markers.get(abrupt_marker_depth..).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "statement control abrupt-marker depth is active",
                span: Some(statement_span),
            },
        )?;
        let open_scope_depth = self.plan_crossed_abrupt_marker_exits(
            crossed_markers,
            &state.active_scopes,
            statement_span,
            layout,
            flow,
        )?;
        if control.scope_depth > open_scope_depth {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: jump.scope_invariant(),
                span: Some(statement_span),
            });
        }
        for scope in state.active_scopes[control.scope_depth..open_scope_depth]
            .iter()
            .rev()
        {
            self.plan_scope_exit(layout.executable, *scope, layout, flow)?;
        }
        flow.branch(BranchKind::Goto, target, statement_span)
    }

    fn plan_crossed_abrupt_marker_exits(
        &self,
        crossed_markers: &[AbruptMarker],
        active_scopes: &[ScopeId],
        statement_span: Span,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<usize, LeafCompilationError> {
        let mut open_scope_depth = active_scopes.len();
        for marker in crossed_markers.iter().rev() {
            if marker.scope_depth > open_scope_depth {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "abrupt marker scope depth remains active",
                    span: Some(statement_span),
                });
            }
            for scope in active_scopes[marker.scope_depth..open_scope_depth]
                .iter()
                .rev()
            {
                self.plan_scope_exit(layout.executable, *scope, layout, flow)?;
            }
            open_scope_depth = marker.scope_depth;
            Self::emit_abrupt_marker_cleanup(&marker.kind, statement_span, flow)?;
        }
        Ok(open_scope_depth)
    }

    fn emit_abrupt_marker_cleanup(
        marker: &AbruptMarkerKind,
        span: Span,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        match marker {
            AbruptMarkerKind::Catch { finalizer: None } | AbruptMarkerKind::ForIn => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    span,
                ))?;
            }
            AbruptMarkerKind::Catch {
                finalizer: Some(finalizer),
            } => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Undefined,
                    Operands::None,
                    span,
                ))?;
                flow.branch(BranchKind::Gosub, finalizer, span)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    span,
                ))?;
            }
            AbruptMarkerKind::ForOf => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::IteratorClose,
                    Operands::None,
                    span,
                ))?;
            }
            AbruptMarkerKind::FinallySubroutine => {
                for _ in 0..2 {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        span,
                    ))?;
                }
            }
        }
        Ok(())
    }
}
