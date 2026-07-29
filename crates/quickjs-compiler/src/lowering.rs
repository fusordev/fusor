use std::{error::Error, fmt, sync::Arc};

use oxc_ast::{
    AstKind,
    ast::{
        AssignmentExpression, AssignmentTarget, BindingPattern, ConditionalExpression,
        DoWhileStatement, Expression, ForStatement, ForStatementInit, Function, FunctionBody,
        FunctionType, IfStatement, LogicalExpression, PropertyKind, SequenceExpression,
        SimpleAssignmentTarget, Statement, UnaryExpression, UpdateExpression, VariableDeclaration,
        VariableDeclarationKind, WhileStatement,
    },
};
use oxc_semantic::{NodeId, ReferenceId, ScopeId, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use quickjs_bytecode::{
    AssemblerError, AssemblerLabel, AssemblerLimits, BranchKind, BytecodeAssembler, BytecodePc,
    CompilerCaptureLayout, CompilerCapturedBinding, EncodeError, FinalOpcode, FunctionIndexDomains,
    MAX_FUNCTION_INDEX_ENTRIES, Operands, UnverifiedCompilerFunctionBody, UnverifiedFunctionHeader,
    VerificationError, VerificationErrorKind, VerificationLimits, VerifiedControlFlow,
    verify_compiler_control_flow,
};
use quickjs_frontend::{ParsedUnit, Span};

use crate::storage::{
    BindingId, CompilationUnitKind, CompilerError, DeclarationKind, Executable, ExecutableId,
    ExecutableKind, InitializationPolicy, NativeReferenceId, PlannedStorage, ResolvedReference,
    StoragePlacement, StoragePlan, WritePolicy, build_planned_storage,
};

/// A validated function-local slot number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LocalSlot(u16);

impl LocalSlot {
    /// Returns the encoded zero-based local index.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }
}

/// One compiler binding assigned to a function-local slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LoweredLocal {
    binding: BindingId,
    slot: LocalSlot,
}

impl LoweredLocal {
    /// Returns the compiler binding stored in this slot.
    #[must_use]
    pub const fn binding(self) -> BindingId {
        self.binding
    }

    /// Returns the function-local slot.
    #[must_use]
    pub const fn slot(self) -> LocalSlot {
        self.slot
    }
}

/// The source span associated with one emitted final instruction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceInstruction {
    pc: BytecodePc,
    span: Span,
}

impl SourceInstruction {
    /// Returns the final instruction's starting bytecode position.
    #[must_use]
    pub const fn pc(self) -> BytecodePc {
        self.pc
    }

    /// Returns the byte span in the retained source text.
    #[must_use]
    pub const fn span(self) -> Span {
        self.span
    }
}

/// Owned output from the validated ordinary leaf-function lowering family.
///
/// This artifact is deliberately not execution authority. Its control-flow
/// certificate still requires the future whole-function verifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledLeafFunction {
    executable: ExecutableId,
    storage_plan: Arc<StoragePlan>,
    source_text: Arc<str>,
    locals: Arc<[LoweredLocal]>,
    source_instructions: Arc<[SourceInstruction]>,
    control_flow: Arc<VerifiedControlFlow>,
}

impl CompiledLeafFunction {
    /// Returns the selected compiler-owned executable identity.
    #[must_use]
    pub const fn executable(&self) -> ExecutableId {
        self.executable
    }

    /// Returns the immutable storage plan that issued the executable identity.
    #[must_use]
    pub fn storage_plan(&self) -> &StoragePlan {
        &self.storage_plan
    }

    /// Returns the exact source text whose Oxc model was lowered.
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    /// Returns the selected function's dense local layout.
    #[must_use]
    pub fn locals(&self) -> &[LoweredLocal] {
        &self.locals
    }

    /// Returns source spans for final instructions in bytecode order.
    #[must_use]
    pub fn source_instructions(&self) -> &[SourceInstruction] {
        &self.source_instructions
    }

    /// Returns the non-executable staged verifier certificate.
    #[must_use]
    pub fn control_flow(&self) -> &VerifiedControlFlow {
        &self.control_flow
    }
}

#[derive(Debug)]
struct ContextIdentity;

/// An owned executable selection issued by one [`CompilationContext`].
///
/// Its private context identity prevents a numerically equal
/// [`ExecutableId`] from another storage plan from selecting the wrong body.
#[derive(Clone, Debug)]
pub struct CompilationExecutable {
    context_identity: Arc<ContextIdentity>,
    executable: Executable,
}

impl CompilationExecutable {
    /// Returns the selected plan-local executable identity.
    #[must_use]
    pub const fn id(&self) -> ExecutableId {
        self.executable.id()
    }

    /// Returns the selected executable metadata.
    #[must_use]
    pub const fn metadata(&self) -> &Executable {
        &self.executable
    }
}

/// Arena-borrowing compiler state that keeps Oxc identities isolated.
///
/// No Oxc node, symbol, scope, or reference identity escapes through compiled
/// output. The context is immutable and does not need a lock.
pub struct CompilationContext<'unit, 'arena, 'scope> {
    unit: &'unit ParsedUnit<'arena, 'scope>,
    planned: PlannedStorage,
    source_text: Arc<str>,
    identity: Arc<ContextIdentity>,
}

impl<'unit, 'arena, 'scope> CompilationContext<'unit, 'arena, 'scope> {
    /// Builds storage and private Oxc-to-compiler identity tables.
    ///
    /// # Errors
    ///
    /// Returns the storage planner's typed failure for unsupported dynamic
    /// binding behavior or inconsistent retained semantics.
    pub fn new(unit: &'unit ParsedUnit<'arena, 'scope>) -> Result<Self, CompilerError> {
        let planned = build_planned_storage(unit)?;
        let source_text = Arc::from(unit.program().source_text);
        Ok(Self {
            unit,
            planned,
            source_text,
            identity: Arc::new(ContextIdentity),
        })
    }

    /// Returns the arena-independent storage plan.
    #[must_use]
    pub fn storage_plan(&self) -> &StoragePlan {
        &self.planned.plan
    }

    /// Returns context-issued executable selections in storage-plan order.
    ///
    /// The selections are owned and contain no Oxc identity, but only this
    /// context accepts them for lowering.
    #[must_use]
    pub fn executables(
        &self,
    ) -> impl ExactSizeIterator<Item = CompilationExecutable> + DoubleEndedIterator + '_ {
        self.planned
            .plan
            .executables()
            .iter()
            .cloned()
            .map(|executable| CompilationExecutable {
                context_identity: Arc::clone(&self.identity),
                executable,
            })
    }

    /// Lowers one validated ordinary leaf-function family to final bytecode.
    ///
    /// The accepted Script-only family is pool-free. It supports simple local
    /// declarations, immediate primitive values, resolved argument/local
    /// reads and mutable writes, value operators including short-circuit and
    /// conditional expressions, lexical blocks, `if`/`else`, `while`,
    /// `do`/`while`, classic `for`, unlabeled `break`/`continue`, expression
    /// statements, and explicit or implicit returns. A leaf may read or write
    /// frame cells captured from an ancestor. The entire function is converted
    /// to typed symbolic instructions before branch relaxation emits any bytes.
    ///
    /// # Errors
    ///
    /// Rejects foreign executable selections, unsupported source structure,
    /// inconsistent semantic identities, assembler resource or encoding
    /// failures, and verifier failures.
    pub fn compile_leaf(
        &self,
        selection: &CompilationExecutable,
        limits: VerificationLimits,
    ) -> Result<CompiledLeafFunction, LeafCompilationError> {
        let executable = self.resolve_selection(selection)?;
        let validated = self.validate_leaf(executable, limits)?;
        let ValidatedLeaf {
            strict,
            argument_count,
            local_count,
            capture_count,
            capture_layout,
            locals,
            flow,
        } = validated;
        let domains = FunctionIndexDomains::new(0, 0, argument_count, local_count, capture_count);
        let variable_reference_count = checked_function_entry_count(
            capture_layout.bindings().len(),
            "function variable references",
        )?;
        let header =
            UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                strict,
                argument_count,
                variable_reference_count,
            );
        let finished = flow.finish()?;
        let (source_instructions, control_flow) =
            finished.verify_with_capture_layout(domains, header, capture_layout, limits)?;

        Ok(CompiledLeafFunction {
            executable,
            storage_plan: Arc::clone(&self.planned.plan),
            source_text: Arc::clone(&self.source_text),
            locals: locals.into(),
            source_instructions: source_instructions.into(),
            control_flow: Arc::new(control_flow),
        })
    }

    fn resolve_selection(
        &self,
        selection: &CompilationExecutable,
    ) -> Result<ExecutableId, LeafCompilationError> {
        let executable = selection.id();
        if !Arc::ptr_eq(&self.identity, &selection.context_identity) {
            return Err(LeafCompilationError::ForeignExecutable { executable });
        }
        let planned = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if planned != selection.metadata() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "context-issued executable metadata is immutable",
                span: Some(selection.metadata().span()),
            });
        }
        Ok(executable)
    }

    fn validate_leaf(
        &self,
        executable_id: ExecutableId,
        limits: VerificationLimits,
    ) -> Result<ValidatedLeaf, LeafCompilationError> {
        let (executable, function) = self.selected_ordinary_leaf(executable_id)?;
        let layout = FrameLayout::new(&self.planned.plan, executable_id)?;
        let body = function
            .body
            .as_ref()
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBody,
                span: function.span,
            })?;
        let mut flow = PlannedControlFlow::new(limits);
        self.validate_body(executable_id, function, body, &layout, &mut flow)?;
        let function_scope = self.created_scope(
            function.scope_id.get(),
            function.node_id.get(),
            function.span,
        )?;
        let capture_layout =
            self.compiler_capture_layout(executable_id, function_scope, &layout)?;

        Ok(ValidatedLeaf {
            strict: executable.is_strict(),
            argument_count: executable.parameter_count(),
            local_count: layout.local_count,
            capture_count: layout.capture_count,
            capture_layout,
            locals: layout
                .locals
                .iter()
                .map(|local| LoweredLocal {
                    binding: local.binding,
                    slot: local.slot,
                })
                .collect(),
            flow,
        })
    }

    fn validate_body<'statement>(
        &self,
        executable: ExecutableId,
        function: &'statement Function<'arena>,
        body: &'statement FunctionBody<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let function_scope = self.created_scope(
            function.scope_id.get(),
            function.node_id.get(),
            function.span,
        )?;
        let mut state = StatementPlanningState {
            work: vec![
                StatementWork::PopScope(function_scope),
                StatementWork::VisitList {
                    statements: &body.statements,
                    next: 0,
                },
                StatementWork::PushScope {
                    scope: function_scope,
                    creator: function.node_id.get(),
                    span: function.span,
                },
            ],
            active_scopes: Vec::new(),
            loop_controls: Vec::new(),
        };

        while let Some(task) = state.work.pop() {
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
                    self.plan_scope_entry(executable, scope, creator, span, layout, flow)?;
                    state.active_scopes.push(scope);
                }
                StatementWork::PopScope(expected) => {
                    if state.active_scopes.len() > 1 {
                        self.plan_scope_exit(executable, expected, layout, flow)?;
                    }
                    let actual = state.active_scopes.pop().ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "statement scope stack is nonempty on exit",
                            span: Some(body.span),
                        },
                    )?;
                    if actual != expected {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "statement scopes exit in last-in-first-out order",
                            span: Some(body.span),
                        });
                    }
                }
                StatementWork::CloseScope(scope) => {
                    self.plan_scope_exit(executable, scope, layout, flow)?;
                }
                StatementWork::PushLoop(control) => state.loop_controls.push(control),
                StatementWork::PopLoop => {
                    state
                        .loop_controls
                        .pop()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "statement loop stack is nonempty on exit",
                            span: Some(body.span),
                        })?;
                }
                StatementWork::Expression(expression) => {
                    self.plan_expression(expression, layout, flow)?;
                }
                StatementWork::Declaration(declaration) => {
                    self.validate_declaration(declaration, layout, flow)?;
                }
                StatementWork::Emit(instruction) => flow.emit(instruction)?,
                StatementWork::Branch { kind, target, span } => {
                    flow.branch(kind, &target, span)?;
                }
                StatementWork::Bind(label) => flow.bind(&label)?,
                StatementWork::Visit(statement) => {
                    self.plan_statement(statement, layout, flow, &mut state)?;
                }
            }
        }
        if !state.active_scopes.is_empty() || !state.loop_controls.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "statement planning closes every scope and loop region",
                span: Some(body.span),
            });
        }
        flow.ensure_terminal(body.span)?;
        Ok(())
    }

    fn plan_statement<'statement>(
        &self,
        statement: &'statement Statement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        match statement {
            Statement::BlockStatement(block) => {
                let scope =
                    self.created_scope(block.scope_id.get(), block.node_id.get(), block.span)?;
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
            }
            Statement::VariableDeclaration(declaration) => {
                self.validate_declaration(declaration, layout, flow)?;
            }
            Statement::ExpressionStatement(statement) => {
                state.work.push(StatementWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    statement.expression.span(),
                )));
                state
                    .work
                    .push(StatementWork::Expression(&statement.expression));
            }
            Statement::EmptyStatement(_) => {}
            Statement::ReturnStatement(statement) => {
                if let Some(argument) = &statement.argument {
                    state.work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Return,
                        Operands::None,
                        statement.span,
                    )));
                    state.work.push(StatementWork::Expression(argument));
                } else {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::ReturnUndef,
                        Operands::None,
                        statement.span,
                    ))?;
                }
            }
            Statement::IfStatement(statement) => {
                Self::schedule_if_statement(statement, flow, &mut state.work)?;
            }
            Statement::WhileStatement(statement) => {
                Self::schedule_while_statement(
                    statement,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                )?;
            }
            Statement::DoWhileStatement(statement) => {
                Self::schedule_do_while_statement(
                    statement,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                )?;
            }
            Statement::ForStatement(statement) => {
                self.plan_for_statement(statement, flow, state)?;
            }
            Statement::BreakStatement(statement) => {
                self.plan_loop_jump(
                    statement.label.as_ref().map(|label| label.span),
                    statement.span,
                    LoopJump::Break,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::ContinueStatement(statement) => {
                self.plan_loop_jump(
                    statement.label.as_ref().map(|label| label.span),
                    statement.span,
                    LoopJump::Continue,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::LabeledStatement(statement) => {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedBody,
                    statement.label.span,
                );
            }
            _ => {
                return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span());
            }
        }
        Ok(())
    }

    fn plan_for_statement<'statement>(
        &self,
        statement: &'statement ForStatement<'arena>,
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
        )
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
    ) -> Result<(), LeafCompilationError> {
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = LoopControl {
            break_target: done.clone(),
            continue_target: test.clone(),
            scope_depth,
        };
        work.push(StatementWork::Bind(done.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: test.clone(),
            span: statement.span,
        });
        work.push(StatementWork::PopLoop);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushLoop(control));
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
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        scope_depth: usize,
    ) -> Result<(), LeafCompilationError> {
        let iteration = flow.new_statement_label(statement.body.span())?;
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = LoopControl {
            break_target: done.clone(),
            continue_target: test.clone(),
            scope_depth,
        };
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfTrue,
            target: iteration.clone(),
            span: statement.test.span(),
        });
        work.push(StatementWork::Expression(&statement.test));
        work.push(StatementWork::Bind(test));
        work.push(StatementWork::PopLoop);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushLoop(control));
        work.push(StatementWork::Bind(iteration));
        Ok(())
    }

    fn schedule_for_statement<'statement>(
        statement: &'statement ForStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
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
        let control = LoopControl {
            break_target: done.clone(),
            continue_target: rotate.clone(),
            scope_depth: loop_scope_depth,
        };

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
        work.push(StatementWork::PopLoop);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushLoop(control));
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

    fn plan_loop_jump(
        &self,
        label_span: Option<Span>,
        statement_span: Span,
        jump: LoopJump,
        state: &StatementPlanningState<'_, '_>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let Some(label_span) = label_span {
            return unsupported(UnsupportedLeafFeature::UnsupportedBody, label_span);
        }
        let control =
            state
                .loop_controls
                .last()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: jump.missing_region_invariant(),
                    span: Some(statement_span),
                })?;
        if control.scope_depth > state.active_scopes.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: jump.scope_invariant(),
                span: Some(statement_span),
            });
        }
        for scope in state.active_scopes[control.scope_depth..].iter().rev() {
            self.plan_scope_exit(layout.executable, *scope, layout, flow)?;
        }
        flow.branch(BranchKind::Goto, jump.target(control), statement_span)
    }

    fn created_scope(
        &self,
        scope: Option<ScopeId>,
        creator: NodeId,
        span: Span,
    ) -> Result<ScopeId, LeafCompilationError> {
        let scope = scope.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "Oxc scope creator has a semantic scope identity",
            span: Some(span),
        })?;
        let scoping = self.unit.semantic().scoping();
        if scope.index() >= scoping.scopes_len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope identity indexes retained semantics",
                span: Some(span),
            });
        }
        if scoping.get_node_id(scope) != creator {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope identity names its creator node",
                span: Some(span),
            });
        }
        Ok(scope)
    }

    fn plan_scope_entry(
        &self,
        executable: ExecutableId,
        scope: ScopeId,
        creator: NodeId,
        span: Span,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        if scoping.get_node_id(scope) != creator {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope entry names its creator node",
                span: Some(span),
            });
        }
        let mut entries = Vec::new();
        for symbol in scoping.iter_bindings_in(scope) {
            if scoping.symbol_scope_id(symbol) != scope {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "Oxc exact-scope binding belongs to that scope",
                    span: Some(scoping.symbol_span(symbol)),
                });
            }
            let declaration_span = scoping.symbol_span(symbol);
            let binding = self.binding_for_identifier(Some(symbol), declaration_span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-entry compiler binding exists",
                    span: Some(declaration_span),
                },
            )?;
            if storage.executable() != executable {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scope-entry binding belongs to the selected executable",
                    span: Some(declaration_span),
                });
            }
            if !storage.policy().has_temporal_dead_zone() {
                continue;
            }
            if storage.policy().initialization() != InitializationPolicy::AtDeclaration {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedDeclaration,
                    declaration_span,
                );
            }
            let FrameSlot::Local(slot) =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "scope-entry binding has a frame slot",
                        span: Some(declaration_span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scope-entry lexical binding uses a local slot",
                    span: Some(declaration_span),
                });
            };
            entries.push((slot, declaration_span));
        }
        entries.sort_unstable_by_key(|(slot, _)| slot.index());
        for (slot, declaration_span) in entries.into_iter().rev() {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(slot.index()),
                declaration_span,
            ))?;
        }
        Ok(())
    }

    fn plan_scope_exit(
        &self,
        executable: ExecutableId,
        scope: ScopeId,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        let mut captured_locals = Vec::new();
        for symbol in scoping.iter_bindings_in(scope) {
            if scoping.symbol_scope_id(symbol) != scope {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scope-exit exact-scope binding belongs to that scope",
                    span: Some(scoping.symbol_span(symbol)),
                });
            }
            let declaration_span = scoping.symbol_span(symbol);
            let binding = self.binding_for_identifier(Some(symbol), declaration_span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-exit compiler binding exists",
                    span: Some(declaration_span),
                },
            )?;
            if storage.executable() != executable {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scope-exit binding belongs to the selected executable",
                    span: Some(declaration_span),
                });
            }
            if !storage.is_frame_captured() {
                continue;
            }
            let FrameSlot::Local(slot) =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "captured scope-exit binding has a frame slot",
                        span: Some(declaration_span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "captured scope-exit binding uses a local slot",
                    span: Some(declaration_span),
                });
            };
            captured_locals.push((slot, declaration_span));
        }
        captured_locals.sort_unstable_by_key(|(slot, _)| slot.index());
        for (slot, declaration_span) in captured_locals.into_iter().rev() {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::CloseLoc,
                Operands::Loc(slot.index()),
                declaration_span,
            ))?;
        }
        Ok(())
    }

    fn validate_declaration(
        &self,
        declaration: &VariableDeclaration<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if declaration.declare
            || !matches!(
                declaration.kind,
                VariableDeclarationKind::Var
                    | VariableDeclarationKind::Let
                    | VariableDeclarationKind::Const
            )
        {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                declaration.span,
            );
        }

        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedDeclaration,
                    declarator.span,
                );
            };
            let binding =
                self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
            let frame_slot = layout
                .slot(binding)
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                    span: identifier.span,
                })?;
            self.validate_declaration_storage(
                declaration.kind,
                binding,
                frame_slot,
                identifier.span,
            )?;

            match &declarator.init {
                Some(initializer) => {
                    self.plan_expression(initializer, layout, flow)?;
                    flow.emit(plan_put_slot(frame_slot, identifier.span))?;
                }
                None if declaration.kind == VariableDeclarationKind::Let => {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Undefined,
                        Operands::None,
                        identifier.span,
                    ))?;
                    flow.emit(plan_put_slot(frame_slot, identifier.span))?;
                }
                None if declaration.kind == VariableDeclarationKind::Var => {}
                None => {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedDeclaration,
                        declarator.span,
                    );
                }
            }
        }
        Ok(())
    }

    fn validate_declaration_storage(
        &self,
        declaration_kind: VariableDeclarationKind,
        binding: BindingId,
        frame_slot: FrameSlot,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "declared compiler binding exists",
                    span: Some(span),
                })?;
        let valid = match declaration_kind {
            VariableDeclarationKind::Let => {
                matches!(storage.policy().kind(), DeclarationKind::Let)
                    && storage.policy().has_temporal_dead_zone()
                    && matches!(frame_slot, FrameSlot::Local(_))
            }
            VariableDeclarationKind::Const => {
                matches!(storage.policy().kind(), DeclarationKind::Const)
                    && storage.policy().has_temporal_dead_zone()
                    && matches!(frame_slot, FrameSlot::Local(_))
            }
            VariableDeclarationKind::Var => {
                matches!(
                    storage.policy().kind(),
                    DeclarationKind::Var | DeclarationKind::Parameter
                ) && !storage.policy().has_temporal_dead_zone()
            }
            VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing => false,
        };
        if !valid {
            return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
        }
        Ok(())
    }

    fn plan_expression<'expression>(
        &self,
        expression: &'expression Expression<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let mut work = vec![ExpressionWork::Visit(expression)];
        while let Some(task) = work.pop() {
            match task {
                ExpressionWork::Emit(instruction) => flow.emit(instruction)?,
                ExpressionWork::Branch { kind, target, span } => {
                    flow.branch(kind, &target, span)?;
                }
                ExpressionWork::Bind(label) => flow.bind(&label)?,
                ExpressionWork::Visit(expression) => {
                    if let Some(literal) = plan_literal(expression) {
                        flow.emit(literal?)?;
                        continue;
                    }
                    match expression {
                        Expression::Identifier(identifier) => {
                            let binding = self
                                .resolved_binding(identifier.reference_id.get(), identifier.span)?;
                            let frame_slot =
                                layout
                                    .slot(binding)
                                    .ok_or(LeafCompilationError::Unsupported {
                                        feature: UnsupportedLeafFeature::UnsupportedBinding,
                                        span: identifier.span,
                                    })?;
                            flow.emit(self.plan_read_slot(
                                binding,
                                frame_slot,
                                identifier.span,
                            )?)?;
                        }
                        Expression::UnaryExpression(unary) => {
                            Self::plan_unary_expression(unary, &mut work, flow)?;
                        }
                        Expression::BinaryExpression(binary) => {
                            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                binary_opcode(binary.operator),
                                Operands::None,
                                binary.span,
                            )));
                            work.push(ExpressionWork::Visit(&binary.right));
                            work.push(ExpressionWork::Visit(&binary.left));
                        }
                        Expression::ParenthesizedExpression(parenthesized) => {
                            work.push(ExpressionWork::Visit(&parenthesized.expression));
                        }
                        Expression::SequenceExpression(sequence) => {
                            Self::plan_sequence_expression(sequence, &mut work)?;
                        }
                        Expression::ConditionalExpression(conditional) => {
                            Self::plan_conditional_expression(conditional, flow, &mut work)?;
                        }
                        Expression::LogicalExpression(logical) => {
                            Self::plan_logical_expression(logical, flow, &mut work)?;
                        }
                        Expression::AssignmentExpression(assignment) => {
                            self.plan_assignment_expression(assignment, layout, flow, &mut work)?;
                        }
                        Expression::UpdateExpression(update) => {
                            self.plan_update_expression(update, layout, &mut work)?;
                        }
                        _ => {
                            return unsupported(
                                UnsupportedLeafFeature::UnsupportedExpression,
                                expression.span(),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn plan_assignment_expression<'expression>(
        &self,
        assignment: &'expression AssignmentExpression<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.left else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        };
        let reference = self.resolved_reference(identifier.reference_id.get(), identifier.span)?;
        let needs_read = assignment.operator != AssignmentOperator::Assign;
        self.validate_mutation_reference(reference, needs_read, identifier.span)?;
        let binding = reference.binding();
        let frame_slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBinding,
                span: identifier.span,
            })?;

        match assignment.operator {
            AssignmentOperator::Assign => {
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
                work.push(ExpressionWork::Visit(&assignment.right));
            }
            AssignmentOperator::LogicalOr
            | AssignmentOperator::LogicalAnd
            | AssignmentOperator::LogicalNullish => {
                let done = flow.new_label(assignment.span)?;
                let branch_kind = match assignment.operator {
                    AssignmentOperator::LogicalOr => BranchKind::IfTrue,
                    AssignmentOperator::LogicalAnd | AssignmentOperator::LogicalNullish => {
                        BranchKind::IfFalse
                    }
                    _ => {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "logical assignment has a short-circuit branch",
                            span: Some(assignment.span),
                        });
                    }
                };
                work.push(ExpressionWork::Bind(done.clone()));
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    identifier.span,
                )));
                work.push(ExpressionWork::Branch {
                    kind: branch_kind,
                    target: done,
                    span: assignment.span,
                });
                if assignment.operator == AssignmentOperator::LogicalNullish {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::IsUndefinedOrNull,
                        Operands::None,
                        identifier.span,
                    )));
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    identifier.span,
                )));
                work.push(ExpressionWork::Emit(self.plan_read_slot(
                    binding,
                    frame_slot,
                    identifier.span,
                )?));
            }
            operator => {
                let binary = operator.to_binary_operator().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "nonlogical compound assignment has a binary operator",
                        span: Some(assignment.span),
                    },
                )?;
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    binary_opcode(binary),
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(self.plan_read_slot(
                    binding,
                    frame_slot,
                    identifier.span,
                )?));
            }
        }
        Ok(())
    }

    fn plan_update_expression<'expression>(
        &self,
        update: &'expression UpdateExpression<'arena>,
        layout: &FrameLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) = &update.argument
        else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        };
        let reference = self.resolved_reference(identifier.reference_id.get(), identifier.span)?;
        self.validate_mutation_reference(reference, true, identifier.span)?;
        let binding = reference.binding();
        let frame_slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBinding,
                span: identifier.span,
            })?;

        self.push_slot_write(binding, frame_slot, update.prefix, identifier.span, work)?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            match (update.operator, update.prefix) {
                (UpdateOperator::Increment, true) => FinalOpcode::Inc,
                (UpdateOperator::Decrement, true) => FinalOpcode::Dec,
                (UpdateOperator::Increment, false) => FinalOpcode::PostInc,
                (UpdateOperator::Decrement, false) => FinalOpcode::PostDec,
            },
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            frame_slot,
            identifier.span,
        )?));
        Ok(())
    }

    fn plan_unary_expression<'expression>(
        unary: &'expression UnaryExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if unary.operator == UnaryOperator::UnaryNegation
            && let Expression::NumericLiteral(literal) = &unary.argument
            && literal.value != 0.0
            && let Some(integer) = exact_negated_i32(literal.value)
        {
            flow.emit(plan_push_integer(integer, unary.span))?;
            return Ok(());
        }
        match unary.operator {
            UnaryOperator::UnaryPlus
            | UnaryOperator::UnaryNegation
            | UnaryOperator::LogicalNot
            | UnaryOperator::BitwiseNot
            | UnaryOperator::Typeof => {
                let opcode = unary_opcode(unary.operator).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "supported unary operator has final opcode",
                        span: Some(unary.span),
                    },
                )?;
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    opcode,
                    Operands::None,
                    unary.span,
                )));
                work.push(ExpressionWork::Visit(&unary.argument));
            }
            UnaryOperator::Void => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Undefined,
                    Operands::None,
                    unary.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    unary.argument.span(),
                )));
                work.push(ExpressionWork::Visit(&unary.argument));
            }
            UnaryOperator::Delete => {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, unary.span);
            }
        }
        Ok(())
    }

    fn plan_conditional_expression<'expression>(
        conditional: &'expression ConditionalExpression<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let alternate = flow.new_label(conditional.alternate.span())?;
        let done = flow.new_label(conditional.span)?;

        work.push(ExpressionWork::Bind(done.clone()));
        work.push(ExpressionWork::Visit(&conditional.alternate));
        work.push(ExpressionWork::Bind(alternate.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: conditional.span,
        });
        work.push(ExpressionWork::Visit(&conditional.consequent));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::IfFalse,
            target: alternate,
            span: conditional.test.span(),
        });
        work.push(ExpressionWork::Visit(&conditional.test));
        Ok(())
    }

    fn plan_logical_expression<'expression>(
        logical: &'expression LogicalExpression<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let done = flow.new_label(logical.span)?;
        let mut operands = same_operator_left_chain(logical);
        let final_operand = operands
            .pop()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc logical expression has two operands",
                span: Some(logical.span),
            })?;
        let branch_kind = match logical.operator {
            LogicalOperator::Or => BranchKind::IfTrue,
            LogicalOperator::And | LogicalOperator::Coalesce => BranchKind::IfFalse,
        };

        work.push(ExpressionWork::Bind(done.clone()));
        work.push(ExpressionWork::Visit(final_operand));
        for operand in operands.into_iter().rev() {
            let span = operand.span();
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                span,
            )));
            work.push(ExpressionWork::Branch {
                kind: branch_kind,
                target: done.clone(),
                span,
            });
            if logical.operator == LogicalOperator::Coalesce {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::IsUndefinedOrNull,
                    Operands::None,
                    span,
                )));
            }
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                span,
            )));
            work.push(ExpressionWork::Visit(operand));
        }
        Ok(())
    }

    fn plan_sequence_expression<'expression>(
        sequence: &'expression SequenceExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if sequence.expressions.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc sequence expression is nonempty",
                span: Some(sequence.span),
            });
        }
        for (index, expression) in sequence.expressions.iter().enumerate().rev() {
            if index + 1 != sequence.expressions.len() {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    expression.span(),
                )));
            }
            work.push(ExpressionWork::Visit(expression));
        }
        Ok(())
    }

    fn plan_read_slot(
        &self,
        binding: BindingId,
        frame_slot: FrameSlot,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        match frame_slot {
            FrameSlot::Argument(slot) => {
                let (opcode, operands) = compact_get_argument(slot);
                Ok(PlannedInstruction::new(opcode, operands, span))
            }
            FrameSlot::Local(slot) => {
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "read compiler binding exists",
                        span: Some(span),
                    },
                )?;
                if storage.policy().has_temporal_dead_zone() {
                    Ok(PlannedInstruction::new(
                        FinalOpcode::GetLocCheck,
                        Operands::Loc(slot.index()),
                        span,
                    ))
                } else {
                    let (opcode, operands) = compact_get_local(slot);
                    Ok(PlannedInstruction::new(opcode, operands, span))
                }
            }
            FrameSlot::Capture(slot) => {
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured read compiler binding exists",
                        span: Some(span),
                    },
                )?;
                if storage.policy().has_temporal_dead_zone() {
                    Ok(PlannedInstruction::new(
                        FinalOpcode::GetVarRefCheck,
                        Operands::VarRef(slot),
                        span,
                    ))
                } else {
                    let (opcode, operands) = compact_get_capture(slot);
                    Ok(PlannedInstruction::new(opcode, operands, span))
                }
            }
        }
    }

    fn plan_write_slot(
        &self,
        binding: BindingId,
        frame_slot: FrameSlot,
        preserve_value: bool,
        span: Span,
    ) -> Result<Vec<PlannedInstruction>, LeafCompilationError> {
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "written compiler binding exists",
                    span: Some(span),
                })?;
        if storage.policy().writes() != WritePolicy::Mutable {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }

        let mut instructions = Vec::with_capacity(2);
        let instruction = match frame_slot {
            FrameSlot::Argument(slot) => {
                let (opcode, operands) = if preserve_value {
                    compact_set_argument(slot)
                } else {
                    compact_put_argument(slot)
                };
                PlannedInstruction::new(opcode, operands, span)
            }
            FrameSlot::Local(slot) if storage.policy().has_temporal_dead_zone() => {
                PlannedInstruction::new(
                    if preserve_value {
                        FinalOpcode::SetLocCheck
                    } else {
                        FinalOpcode::PutLocCheck
                    },
                    Operands::Loc(slot.index()),
                    span,
                )
            }
            FrameSlot::Local(slot) => {
                let (opcode, operands) = if preserve_value {
                    compact_set_local(slot)
                } else {
                    compact_put_local(slot)
                };
                PlannedInstruction::new(opcode, operands, span)
            }
            FrameSlot::Capture(slot)
                if preserve_value && storage.policy().has_temporal_dead_zone() =>
            {
                instructions.push(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    span,
                ));
                PlannedInstruction::new(FinalOpcode::PutVarRefCheck, Operands::VarRef(slot), span)
            }
            FrameSlot::Capture(slot) if storage.policy().has_temporal_dead_zone() => {
                PlannedInstruction::new(FinalOpcode::PutVarRefCheck, Operands::VarRef(slot), span)
            }
            FrameSlot::Capture(slot) => {
                let (opcode, operands) = if preserve_value {
                    compact_set_capture(slot)
                } else {
                    compact_put_capture(slot)
                };
                PlannedInstruction::new(opcode, operands, span)
            }
        };
        instructions.push(instruction);
        Ok(instructions)
    }

    fn push_slot_write<'expression>(
        &self,
        binding: BindingId,
        frame_slot: FrameSlot,
        preserve_value: bool,
        span: Span,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        for instruction in self
            .plan_write_slot(binding, frame_slot, preserve_value, span)?
            .into_iter()
            .rev()
        {
            work.push(ExpressionWork::Emit(instruction));
        }
        Ok(())
    }

    fn compiler_capture_layout(
        &self,
        executable: ExecutableId,
        function_scope: ScopeId,
        layout: &FrameLayout,
    ) -> Result<CompilerCaptureLayout, LeafCompilationError> {
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let mut captured = Vec::new();
        for binding in bindings {
            if !binding.is_frame_captured() {
                continue;
            }
            let frame_slot =
                layout
                    .slot(binding.id())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "captured owner binding has a frame slot",
                        span: binding.declaration_spans().first().copied(),
                    })?;
            let captured_binding = match frame_slot {
                FrameSlot::Argument(slot) => CompilerCapturedBinding::Argument(u32::from(slot.0)),
                FrameSlot::Local(slot) => {
                    let scope = self.scope_for_binding(binding.id())?;
                    if scope == function_scope {
                        CompilerCapturedBinding::FunctionLocal(u32::from(slot.index()))
                    } else {
                        CompilerCapturedBinding::ScopedLocal(u32::from(slot.index()))
                    }
                }
                FrameSlot::Capture(_) => {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "captured owner binding is not an imported capture",
                        span: binding.declaration_spans().first().copied(),
                    });
                }
            };
            captured.push(captured_binding);
        }
        Ok(CompilerCaptureLayout::new(Arc::from(captured)))
    }

    fn scope_for_binding(&self, binding: BindingId) -> Result<ScopeId, LeafCompilationError> {
        self.planned
            .identities
            .scope_by_binding
            .get(binding.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "compiler binding has an Oxc scope identity",
                span: self
                    .planned
                    .plan
                    .binding(binding)
                    .and_then(|storage| storage.declaration_spans().first().copied()),
            })
    }

    fn selected_ordinary_leaf(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Function<'arena>), LeafCompilationError> {
        let executable = self.planned.plan.executable(executable_id).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: executable_id,
            },
        )?;
        let node_id = self
            .planned
            .identities
            .node_by_executable
            .get(executable_id.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "executable has an Oxc node identity",
                span: Some(executable.span()),
            })?;
        if self
            .planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            != Some(executable_id)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc node and executable identities are bijective",
                span: Some(executable.span()),
            });
        }

        let AstKind::Function(function) = self.unit.semantic().nodes().kind(node_id) else {
            return unsupported(
                UnsupportedLeafFeature::NonOrdinaryFunction,
                executable.span(),
            );
        };
        if function.r#async || function.generator {
            return unsupported(UnsupportedLeafFeature::NonOrdinaryFunction, function.span);
        }
        let is_declaration = function.r#type == FunctionType::FunctionDeclaration;
        let is_expression = function.r#type == FunctionType::FunctionExpression;
        if is_expression && function.id.is_some() {
            return unsupported(
                UnsupportedLeafFeature::NamedFunctionExpression,
                function.span,
            );
        }
        if !is_declaration && !is_expression {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedFunctionForm,
                function.span,
            );
        }
        if is_object_method_or_accessor(self.unit, node_id) {
            return unsupported(
                UnsupportedLeafFeature::ObjectMethodOrAccessor,
                function.span,
            );
        }
        if !matches!(
            executable.kind(),
            ExecutableKind::Function {
                asynchronous: false,
                generator: false,
            }
        ) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary Oxc function has ordinary executable metadata",
                span: Some(function.span),
            });
        }
        if self.planned.plan.kind() != CompilationUnitKind::Script {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                function.span,
            );
        }
        if let Some(child) = self
            .planned
            .plan
            .executables()
            .iter()
            .find(|candidate| candidate.parent() == Some(executable_id))
        {
            return unsupported(UnsupportedLeafFeature::NestedExecutable, child.span());
        }
        if let Some(reference) = self
            .planned
            .plan
            .unresolved_globals_for(executable_id)
            .and_then(|references| references.first())
        {
            return unsupported(
                UnsupportedLeafFeature::UnresolvedReference,
                reference.span(),
            );
        }
        Ok((executable, function))
    }

    fn binding_for_identifier(
        &self,
        symbol_id: Option<SymbolId>,
        span: Span,
    ) -> Result<BindingId, LeafCompilationError> {
        let symbol_id = symbol_id.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "binding identifier has Oxc symbol identity",
            span: Some(span),
        })?;
        self.planned
            .identities
            .binding_by_symbol
            .get(symbol_id.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc symbol has compiler binding identity",
                span: Some(span),
            })
    }

    fn resolved_binding(
        &self,
        reference_id: Option<ReferenceId>,
        span: Span,
    ) -> Result<BindingId, LeafCompilationError> {
        let reference = self.resolved_reference(reference_id, span)?;
        if !reference.access().reads() || reference.access().writes() {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }
        Ok(reference.binding())
    }

    fn resolved_reference(
        &self,
        reference_id: Option<ReferenceId>,
        span: Span,
    ) -> Result<&ResolvedReference, LeafCompilationError> {
        let reference_id = reference_id.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "identifier reference has Oxc reference identity",
            span: Some(span),
        })?;
        let native = self
            .planned
            .identities
            .reference_by_id
            .get(reference_id.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc reference has compiler identity",
                span: Some(span),
            })?;
        let resolved_id = match native {
            NativeReferenceId::Resolved(resolved_id) => resolved_id,
            NativeReferenceId::Unresolved(unresolved_id) => {
                let reference = self
                    .planned
                    .plan
                    .unresolved_globals()
                    .get(unresolved_id.index())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "unresolved compiler reference exists",
                        span: Some(span),
                    })?;
                if reference.span() != span {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "unresolved compiler reference retains its Oxc span",
                        span: Some(span),
                    });
                }
                return unsupported(UnsupportedLeafFeature::UnresolvedReference, span);
            }
        };
        let reference = self.planned.plan.resolved_reference(resolved_id).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "resolved compiler reference exists",
                span: Some(span),
            },
        )?;
        if reference.span() != span {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "resolved compiler reference retains its Oxc span",
                span: Some(span),
            });
        }
        Ok(reference)
    }

    fn validate_mutation_reference(
        &self,
        reference: &ResolvedReference,
        needs_read: bool,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if !reference.access().writes() || reference.access().reads() != needs_read {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }
        let storage = self.planned.plan.binding(reference.binding()).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "written compiler binding exists",
                span: Some(span),
            },
        )?;
        if storage.policy().writes() != WritePolicy::Mutable {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }
        Ok(())
    }
}

fn is_object_method_or_accessor(unit: &ParsedUnit<'_, '_>, node_id: NodeId) -> bool {
    let AstKind::ObjectProperty(property) = unit.semantic().nodes().parent_kind(node_id) else {
        return false;
    };
    let Expression::FunctionExpression(value) = &property.value else {
        return false;
    };
    value.node_id.get() == node_id
        && (property.method || !matches!(property.kind, PropertyKind::Init))
}

fn same_operator_left_chain<'expression, 'arena>(
    logical: &'expression LogicalExpression<'arena>,
) -> Vec<&'expression Expression<'arena>> {
    let mut reversed = vec![&logical.right];
    let mut left = &logical.left;
    while let Expression::LogicalExpression(inner) = left
        && inner.operator == logical.operator
    {
        reversed.push(&inner.right);
        left = &inner.left;
    }
    reversed.push(left);
    reversed.reverse();
    reversed
}

#[derive(Clone, Copy)]
struct PlannedInstruction {
    opcode: FinalOpcode,
    operands: Operands,
    span: Span,
}

impl PlannedInstruction {
    const fn new(opcode: FinalOpcode, operands: Operands, span: Span) -> Self {
        Self {
            opcode,
            operands,
            span,
        }
    }
}

#[derive(Clone)]
struct CompilerLabel {
    assembler: AssemblerLabel,
    owner_span: Span,
    expected_stack_depth: Option<u32>,
}

enum ExpressionWork<'expression, 'arena> {
    Visit(&'expression Expression<'arena>),
    Emit(PlannedInstruction),
    Branch {
        kind: BranchKind,
        target: CompilerLabel,
        span: Span,
    },
    Bind(CompilerLabel),
}

enum StatementWork<'statement, 'arena> {
    Visit(&'statement Statement<'arena>),
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
    PushLoop(LoopControl),
    PopLoop,
    Declaration(&'statement VariableDeclaration<'arena>),
    Expression(&'statement Expression<'arena>),
    Emit(PlannedInstruction),
    Branch {
        kind: BranchKind,
        target: CompilerLabel,
        span: Span,
    },
    Bind(CompilerLabel),
}

struct StatementPlanningState<'statement, 'arena> {
    work: Vec<StatementWork<'statement, 'arena>>,
    active_scopes: Vec<ScopeId>,
    loop_controls: Vec<LoopControl>,
}

#[derive(Clone)]
struct LoopControl {
    break_target: CompilerLabel,
    continue_target: CompilerLabel,
    scope_depth: usize,
}

#[derive(Clone, Copy)]
enum LoopJump {
    Break,
    Continue,
}

impl LoopJump {
    const fn missing_region_invariant(self) -> &'static str {
        match self {
            Self::Break => "Oxc accepts unlabeled break only in a breakable region",
            Self::Continue => "Oxc accepts unlabeled continue only in an iteration",
        }
    }

    const fn scope_invariant(self) -> &'static str {
        match self {
            Self::Break => "break target scope encloses the abrupt statement",
            Self::Continue => "continue target scope encloses the abrupt statement",
        }
    }

    const fn target(self, control: &LoopControl) -> &CompilerLabel {
        match self {
            Self::Break => &control.break_target,
            Self::Continue => &control.continue_target,
        }
    }
}

fn checked_function_entry_count<T>(
    count: T,
    domain: &'static str,
) -> Result<u32, LeafCompilationError>
where
    u32: TryFrom<T>,
{
    let count =
        u32::try_from(count).map_err(|_| LeafCompilationError::CapacityExceeded { domain })?;
    if count > MAX_FUNCTION_INDEX_ENTRIES {
        return Err(LeafCompilationError::CapacityExceeded { domain });
    }
    Ok(count)
}

fn checked_function_index<T>(index: T, domain: &'static str) -> Result<u16, LeafCompilationError>
where
    u32: TryFrom<T>,
{
    let index =
        u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded { domain })?;
    if index >= MAX_FUNCTION_INDEX_ENTRIES {
        return Err(LeafCompilationError::CapacityExceeded { domain });
    }
    u16::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded { domain })
}

#[derive(Clone, Copy)]
struct ArgumentSlot(u16);

#[derive(Clone, Copy)]
enum FrameSlot {
    Argument(ArgumentSlot),
    Local(LocalSlot),
    Capture(u16),
}

struct FrameLocal {
    binding: BindingId,
    slot: LocalSlot,
}

struct FrameLayout {
    executable: ExecutableId,
    slots: Vec<Option<FrameSlot>>,
    locals: Vec<FrameLocal>,
    local_count: u32,
    capture_count: u32,
}

impl FrameLayout {
    fn new(plan: &StoragePlan, executable: ExecutableId) -> Result<Self, LeafCompilationError> {
        let mut slots = vec![None; plan.bindings().len()];
        let mut locals = Vec::new();
        let mut local_count = 0_u32;
        let bindings = plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let executable_metadata = plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        checked_function_entry_count(
            executable_metadata.parameter_count(),
            "function argument slots",
        )?;
        for binding in bindings {
            let slot = match binding.placement() {
                StoragePlacement::Argument { parameter_index } => {
                    let parameter_index =
                        checked_function_index(parameter_index, "function argument slots")?;
                    FrameSlot::Argument(ArgumentSlot(parameter_index))
                }
                StoragePlacement::Local => {
                    let slot =
                        LocalSlot(checked_function_index(local_count, "function local slots")?);
                    local_count += 1;
                    locals.push(FrameLocal {
                        binding: binding.id(),
                        slot,
                    });
                    FrameSlot::Local(slot)
                }
                StoragePlacement::GlobalObject
                | StoragePlacement::GlobalLexical
                | StoragePlacement::ModuleLocal
                | StoragePlacement::ModuleImport => {
                    let span = binding
                        .declaration_spans()
                        .first()
                        .copied()
                        .unwrap_or_default();
                    return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
                }
            };
            let target = slots.get_mut(binding.id().index()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "binding identity indexes frame layout",
                    span: binding.declaration_spans().first().copied(),
                },
            )?;
            if target.replace(slot).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one frame slot per compiler binding",
                    span: binding.declaration_spans().first().copied(),
                });
            }
        }
        let captures = plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let capture_count = checked_function_entry_count(captures.len(), "function capture slots")?;
        for (expected_capture_index, capture) in captures.iter().enumerate() {
            let capture_index =
                checked_function_index(capture.slot().index(), "function capture slots")?;
            if capture.slot().index() != expected_capture_index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "function capture slots are dense and ordered",
                    span: plan
                        .binding(capture.binding())
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
            let target = slots.get_mut(capture.binding().index()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "captured binding identity indexes frame layout",
                    span: None,
                },
            )?;
            if target.replace(FrameSlot::Capture(capture_index)).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one frame or capture slot per compiler binding",
                    span: plan
                        .binding(capture.binding())
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
        }
        Ok(Self {
            executable,
            slots,
            locals,
            local_count,
            capture_count,
        })
    }

    fn slot(&self, binding: BindingId) -> Option<FrameSlot> {
        self.slots.get(binding.index()).copied().flatten()
    }
}

struct ValidatedLeaf {
    strict: bool,
    argument_count: u32,
    local_count: u32,
    capture_count: u32,
    capture_layout: CompilerCaptureLayout,
    locals: Vec<LoweredLocal>,
    flow: PlannedControlFlow,
}

#[derive(Clone, Copy)]
struct StackAnchor {
    instruction_index: usize,
    span: Span,
    expected_depth: u32,
}

#[derive(Clone, Copy, Debug)]
struct ResolvedStackAnchor {
    pc: BytecodePc,
    span: Span,
    expected_depth: u32,
}

struct PlannedControlFlow {
    assembler: BytecodeAssembler,
    instruction_spans: Vec<Span>,
    label_spans: Vec<Span>,
    stack_anchors: Vec<StackAnchor>,
    last_instruction_can_fall_through: Option<bool>,
    label_bound_after_last_instruction: bool,
}

#[derive(Debug)]
struct FinishedControlFlow {
    bytecode: Vec<u8>,
    source_instructions: Vec<SourceInstruction>,
    stack_anchors: Vec<ResolvedStackAnchor>,
}

impl PlannedControlFlow {
    fn new(limits: VerificationLimits) -> Self {
        let assembler_limits = AssemblerLimits::new(
            limits.max_bytecode_bytes_per_function(),
            limits.max_instructions_per_function(),
            limits.max_transfer_evaluations(),
        );
        Self {
            assembler: BytecodeAssembler::with_limits(assembler_limits),
            instruction_spans: Vec::new(),
            label_spans: Vec::new(),
            stack_anchors: Vec::new(),
            last_instruction_can_fall_through: None,
            label_bound_after_last_instruction: false,
        }
    }

    fn emit(&mut self, instruction: PlannedInstruction) -> Result<(), LeafCompilationError> {
        self.assembler
            .push(instruction.opcode, instruction.operands)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(instruction.span),
                source,
            })?;
        self.instruction_spans.push(instruction.span);
        self.last_instruction_can_fall_through = Some(!matches!(
            instruction.opcode,
            FinalOpcode::Return | FinalOpcode::ReturnUndef
        ));
        self.label_bound_after_last_instruction = false;
        Ok(())
    }

    fn new_label(&mut self, span: Span) -> Result<CompilerLabel, LeafCompilationError> {
        self.new_label_with_expected_depth(span, None)
    }

    fn new_statement_label(&mut self, span: Span) -> Result<CompilerLabel, LeafCompilationError> {
        self.new_label_with_expected_depth(span, Some(0))
    }

    fn new_label_with_expected_depth(
        &mut self,
        span: Span,
        expected_stack_depth: Option<u32>,
    ) -> Result<CompilerLabel, LeafCompilationError> {
        let assembler = self.assembler.new_label().map_err(|source| {
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source,
            }
        })?;
        self.label_spans.push(span);
        Ok(CompilerLabel {
            assembler,
            owner_span: span,
            expected_stack_depth,
        })
    }

    fn branch(
        &mut self,
        kind: BranchKind,
        target: &CompilerLabel,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        self.assembler
            .branch(kind, &target.assembler)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source,
            })?;
        self.instruction_spans.push(span);
        self.last_instruction_can_fall_through = Some(kind != BranchKind::Goto);
        self.label_bound_after_last_instruction = false;
        Ok(())
    }

    fn bind(&mut self, label: &CompilerLabel) -> Result<(), LeafCompilationError> {
        self.assembler.bind(&label.assembler).map_err(|source| {
            LeafCompilationError::BytecodeAssembly {
                span: Some(label.owner_span),
                source,
            }
        })?;
        if let Some(expected_depth) = label.expected_stack_depth {
            self.stack_anchors.push(StackAnchor {
                instruction_index: self.instruction_spans.len(),
                span: label.owner_span,
                expected_depth,
            });
        }
        self.label_bound_after_last_instruction = true;
        Ok(())
    }

    fn ensure_terminal(&mut self, span: Span) -> Result<(), LeafCompilationError> {
        if self.label_bound_after_last_instruction
            || self.last_instruction_can_fall_through.unwrap_or(true)
        {
            self.emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                span,
            ))?;
        }
        Ok(())
    }

    fn finish(self) -> Result<FinishedControlFlow, LeafCompilationError> {
        let Self {
            assembler,
            instruction_spans: spans,
            label_spans,
            stack_anchors,
            last_instruction_can_fall_through: _,
            label_bound_after_last_instruction: _,
        } = self;
        let assembled = match assembler.finish() {
            Ok(assembled) => assembled,
            Err(AssemblerError::Encoding {
                instruction_index,
                source,
            }) => {
                let span = spans.get(instruction_index as usize).copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "assembler encoding failure indexes a planned source span",
                        span: None,
                    },
                )?;
                return Err(LeafCompilationError::BytecodeEncoding { span, source });
            }
            Err(source) => {
                let span = source
                    .instruction_index()
                    .and_then(|index| spans.get(index as usize).copied())
                    .or_else(|| {
                        source
                            .label_index()
                            .and_then(|index| label_spans.get(index as usize).copied())
                    });
                return Err(LeafCompilationError::BytecodeAssembly { span, source });
            }
        };
        let (bytecode, instruction_pcs) = assembled.into_parts();
        if instruction_pcs.len() != spans.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "assembler returns one final PC per planned instruction",
                span: spans.last().copied(),
            });
        }
        let mut resolved_stack_anchors = Vec::with_capacity(stack_anchors.len());
        for anchor in stack_anchors {
            let Some(pc) = instruction_pcs.get(anchor.instruction_index).copied() else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "statement stack anchor resolves to a final instruction",
                    span: Some(anchor.span),
                });
            };
            resolved_stack_anchors.push(ResolvedStackAnchor {
                pc,
                span: anchor.span,
                expected_depth: anchor.expected_depth,
            });
        }
        let source_instructions = instruction_pcs
            .into_iter()
            .zip(spans)
            .map(|(pc, span)| SourceInstruction { pc, span })
            .collect();
        Ok(FinishedControlFlow {
            bytecode,
            source_instructions,
            stack_anchors: resolved_stack_anchors,
        })
    }
}

impl FinishedControlFlow {
    #[cfg(test)]
    fn verify(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        self.verify_with_capture_layout(domains, header, CompilerCaptureLayout::default(), limits)
    }

    fn verify_with_capture_layout(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        capture_layout: CompilerCaptureLayout,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        let Self {
            bytecode,
            source_instructions,
            stack_anchors,
        } = self;
        let control_flow = match verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(bytecode, domains, header)
                .with_capture_layout(capture_layout),
            limits,
        ) {
            Ok(control_flow) => control_flow,
            Err(source) => {
                let span = match source.pc() {
                    Some(pc) => Some(exact_source_span(&source_instructions, pc).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "verifier instruction PC resolves to an exact source instruction",
                            span: None,
                        },
                    )?),
                    None => None,
                };
                let related_span = match source.kind() {
                VerificationErrorKind::InconsistentStackAtJoin { target, .. } => {
                        Some(exact_source_span(&source_instructions, *target).ok_or(
                            LeafCompilationError::SemanticInvariant {
                                invariant:
                                    "verifier join target resolves to an exact source instruction",
                                span: None,
                            },
                        )?)
                }
                _ => None,
            };
                return Err(LeafCompilationError::BytecodeVerification {
                    span,
                    related_span,
                    source,
                });
            }
        };

        for anchor in stack_anchors {
            let Some(index) = control_flow.instruction_index_at(anchor.pc) else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "resolved statement stack anchor remains an instruction start",
                    span: Some(anchor.span),
                });
            };
            let Some(instruction) = control_flow.instruction(index) else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "resolved statement stack anchor has a verified instruction",
                    span: Some(anchor.span),
                });
            };
            let Some(actual) = instruction.entry_stack_depth() else {
                continue;
            };
            if actual != anchor.expected_depth {
                return Err(LeafCompilationError::BytecodeStackInvariant {
                    span: anchor.span,
                    pc: anchor.pc,
                    expected: anchor.expected_depth,
                    actual,
                });
            }
        }

        Ok((source_instructions, control_flow))
    }
}

fn exact_source_span(source_instructions: &[SourceInstruction], pc: BytecodePc) -> Option<Span> {
    source_instructions
        .binary_search_by_key(&pc, |instruction| instruction.pc())
        .ok()
        .map(|index| source_instructions[index].span())
}

fn compact_get_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::GetArg0, Operands::NoneArg),
        1 => (FinalOpcode::GetArg1, Operands::NoneArg),
        2 => (FinalOpcode::GetArg2, Operands::NoneArg),
        3 => (FinalOpcode::GetArg3, Operands::NoneArg),
        index => (FinalOpcode::GetArg, Operands::Arg(index)),
    }
}

fn compact_put_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::PutArg0, Operands::NoneArg),
        1 => (FinalOpcode::PutArg1, Operands::NoneArg),
        2 => (FinalOpcode::PutArg2, Operands::NoneArg),
        3 => (FinalOpcode::PutArg3, Operands::NoneArg),
        index => (FinalOpcode::PutArg, Operands::Arg(index)),
    }
}

fn compact_set_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::SetArg0, Operands::NoneArg),
        1 => (FinalOpcode::SetArg1, Operands::NoneArg),
        2 => (FinalOpcode::SetArg2, Operands::NoneArg),
        3 => (FinalOpcode::SetArg3, Operands::NoneArg),
        index => (FinalOpcode::SetArg, Operands::Arg(index)),
    }
}

fn compact_get_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::GetLoc0, Operands::NoneLoc),
        1 => (FinalOpcode::GetLoc1, Operands::NoneLoc),
        2 => (FinalOpcode::GetLoc2, Operands::NoneLoc),
        3 => (FinalOpcode::GetLoc3, Operands::NoneLoc),
        index => match u8::try_from(index) {
            Ok(short) => (FinalOpcode::GetLoc8, Operands::Loc8(short)),
            Err(_) => (FinalOpcode::GetLoc, Operands::Loc(index)),
        },
    }
}

fn compact_put_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::PutLoc0, Operands::NoneLoc),
        1 => (FinalOpcode::PutLoc1, Operands::NoneLoc),
        2 => (FinalOpcode::PutLoc2, Operands::NoneLoc),
        3 => (FinalOpcode::PutLoc3, Operands::NoneLoc),
        index => match u8::try_from(index) {
            Ok(short) => (FinalOpcode::PutLoc8, Operands::Loc8(short)),
            Err(_) => (FinalOpcode::PutLoc, Operands::Loc(index)),
        },
    }
}

fn compact_set_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::SetLoc0, Operands::NoneLoc),
        1 => (FinalOpcode::SetLoc1, Operands::NoneLoc),
        2 => (FinalOpcode::SetLoc2, Operands::NoneLoc),
        3 => (FinalOpcode::SetLoc3, Operands::NoneLoc),
        index => match u8::try_from(index) {
            Ok(short) => (FinalOpcode::SetLoc8, Operands::Loc8(short)),
            Err(_) => (FinalOpcode::SetLoc, Operands::Loc(index)),
        },
    }
}

fn compact_get_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::GetVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::GetVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::GetVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::GetVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::GetVarRef, Operands::VarRef(index)),
    }
}

fn compact_put_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::PutVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::PutVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::PutVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::PutVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::PutVarRef, Operands::VarRef(index)),
    }
}

fn compact_set_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::SetVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::SetVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::SetVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::SetVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::SetVarRef, Operands::VarRef(index)),
    }
}

fn plan_put_slot(slot: FrameSlot, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match slot {
        FrameSlot::Argument(slot) => compact_put_argument(slot),
        FrameSlot::Local(slot) => compact_put_local(slot),
        FrameSlot::Capture(slot) => compact_put_capture(slot),
    };
    PlannedInstruction::new(opcode, operands, span)
}

fn plan_literal(
    expression: &Expression<'_>,
) -> Option<Result<PlannedInstruction, LeafCompilationError>> {
    let planned = match expression {
        Expression::BooleanLiteral(literal) => Ok(PlannedInstruction::new(
            if literal.value {
                FinalOpcode::PushTrue
            } else {
                FinalOpcode::PushFalse
            },
            Operands::None,
            literal.span,
        )),
        Expression::NullLiteral(literal) => Ok(PlannedInstruction::new(
            FinalOpcode::Null,
            Operands::None,
            literal.span,
        )),
        Expression::NumericLiteral(literal) => exact_i32(literal.value)
            .map(|value| plan_push_integer(value, literal.span))
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedLiteral,
                span: literal.span,
            }),
        Expression::BigIntLiteral(literal) => literal
            .value
            .parse::<i32>()
            .map(|value| {
                PlannedInstruction::new(
                    FinalOpcode::PushBigIntI32,
                    Operands::I32(value),
                    literal.span,
                )
            })
            .map_err(|_| LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedLiteral,
                span: literal.span,
            }),
        Expression::StringLiteral(literal) if literal.value.is_empty() => Ok(
            PlannedInstruction::new(FinalOpcode::PushEmptyString, Operands::None, literal.span),
        ),
        Expression::StringLiteral(literal) => {
            unsupported(UnsupportedLeafFeature::UnsupportedLiteral, literal.span)
        }
        Expression::RegExpLiteral(literal) => {
            unsupported(UnsupportedLeafFeature::UnsupportedLiteral, literal.span)
        }
        Expression::TemplateLiteral(template) => {
            unsupported(UnsupportedLeafFeature::UnsupportedLiteral, template.span)
        }
        _ => return None,
    };
    Some(planned)
}

#[allow(clippy::cast_possible_truncation)]
fn exact_i32(value: f64) -> Option<i32> {
    if value.is_finite()
        && value.fract() == 0.0
        && value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX)
    {
        Some(value as i32)
    } else {
        None
    }
}

fn exact_negated_i32(value: f64) -> Option<i32> {
    exact_i32(-value)
}

fn plan_push_integer(value: i32, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match value {
        -1 => (FinalOpcode::PushMinus1, Operands::NoneInt),
        0 => (FinalOpcode::Push0, Operands::NoneInt),
        1 => (FinalOpcode::Push1, Operands::NoneInt),
        2 => (FinalOpcode::Push2, Operands::NoneInt),
        3 => (FinalOpcode::Push3, Operands::NoneInt),
        4 => (FinalOpcode::Push4, Operands::NoneInt),
        5 => (FinalOpcode::Push5, Operands::NoneInt),
        6 => (FinalOpcode::Push6, Operands::NoneInt),
        7 => (FinalOpcode::Push7, Operands::NoneInt),
        value => match i8::try_from(value) {
            Ok(value) => (FinalOpcode::PushI8, Operands::I8(value)),
            Err(_) => match i16::try_from(value) {
                Ok(value) => (FinalOpcode::PushI16, Operands::I16(value)),
                Err(_) => (FinalOpcode::PushI32, Operands::I32(value)),
            },
        },
    };
    PlannedInstruction::new(opcode, operands, span)
}

const fn unary_opcode(operator: UnaryOperator) -> Option<FinalOpcode> {
    match operator {
        UnaryOperator::UnaryPlus => Some(FinalOpcode::Plus),
        UnaryOperator::UnaryNegation => Some(FinalOpcode::Neg),
        UnaryOperator::LogicalNot => Some(FinalOpcode::Lnot),
        UnaryOperator::BitwiseNot => Some(FinalOpcode::Not),
        UnaryOperator::Typeof => Some(FinalOpcode::Typeof),
        UnaryOperator::Void | UnaryOperator::Delete => None,
    }
}

const fn binary_opcode(operator: BinaryOperator) -> FinalOpcode {
    match operator {
        BinaryOperator::Equality => FinalOpcode::Eq,
        BinaryOperator::Inequality => FinalOpcode::Neq,
        BinaryOperator::StrictEquality => FinalOpcode::StrictEq,
        BinaryOperator::StrictInequality => FinalOpcode::StrictNeq,
        BinaryOperator::LessThan => FinalOpcode::Lt,
        BinaryOperator::LessEqualThan => FinalOpcode::Lte,
        BinaryOperator::GreaterThan => FinalOpcode::Gt,
        BinaryOperator::GreaterEqualThan => FinalOpcode::Gte,
        BinaryOperator::Addition => FinalOpcode::Add,
        BinaryOperator::Subtraction => FinalOpcode::Sub,
        BinaryOperator::Multiplication => FinalOpcode::Mul,
        BinaryOperator::Division => FinalOpcode::Div,
        BinaryOperator::Remainder => FinalOpcode::Mod,
        BinaryOperator::Exponential => FinalOpcode::Pow,
        BinaryOperator::ShiftLeft => FinalOpcode::Shl,
        BinaryOperator::ShiftRight => FinalOpcode::Sar,
        BinaryOperator::ShiftRightZeroFill => FinalOpcode::Shr,
        BinaryOperator::BitwiseOR => FinalOpcode::Or,
        BinaryOperator::BitwiseXOR => FinalOpcode::Xor,
        BinaryOperator::BitwiseAnd => FinalOpcode::And,
        BinaryOperator::In => FinalOpcode::In,
        BinaryOperator::Instanceof => FinalOpcode::InstanceOf,
    }
}

fn unsupported<T>(feature: UnsupportedLeafFeature, span: Span) -> Result<T, LeafCompilationError> {
    Err(LeafCompilationError::Unsupported { feature, span })
}

/// Syntax or storage behavior outside the first ordinary leaf-function slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedLeafFeature {
    /// The selected executable is not a synchronous ordinary function.
    NonOrdinaryFunction,
    /// A named function expression requires its immutable self binding.
    NamedFunctionExpression,
    /// The Oxc function form is neither a declaration nor function expression.
    UnsupportedFunctionForm,
    /// Object methods and accessors need distinct prototype/home-object flags.
    ObjectMethodOrAccessor,
    /// The selected function contains another executable body.
    NestedExecutable,
    /// Module-owned storage is outside this Script-only lowering slice.
    UnsupportedCompilationUnit,
    /// A statement requires unsupported control flow or scope entry behavior.
    UnsupportedBody,
    /// A declaration is not a simple `var`, `let`, or `const` binding.
    UnsupportedDeclaration,
    /// An expression requires calls, properties, non-identifier mutation, or
    /// another unsupported semantic family.
    UnsupportedExpression,
    /// A literal requires a constant, atom, `BigInt`, or `RegExp` pool entry.
    UnsupportedLiteral,
    /// A binding cannot be represented by this frame layout.
    UnsupportedBinding,
    /// A reference access or binding write policy is not supported.
    UnsupportedReference,
    /// An identifier remained unresolved after Oxc semantics.
    UnresolvedReference,
}

/// Failure to lower or verify an ordinary leaf function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeafCompilationError {
    /// The executable selection was issued by another compilation context.
    ForeignExecutable {
        /// The foreign selection's plan-local identity.
        executable: ExecutableId,
    },
    /// A context-issued executable no longer resolves in its immutable plan.
    InvalidExecutable {
        /// The rejected plan-local identity.
        executable: ExecutableId,
    },
    /// The selected source requires behavior outside this lowering slice.
    Unsupported {
        /// The unsupported behavior.
        feature: UnsupportedLeafFeature,
        /// Exact source span requiring it.
        span: Span,
    },
    /// Retained Oxc semantics or compiler identities violated an invariant.
    SemanticInvariant {
        /// Stable invariant label.
        invariant: &'static str,
        /// Related source span, when available.
        span: Option<Span>,
    },
    /// A dense bytecode domain exceeded its encoded width.
    CapacityExceeded {
        /// Stable capacity-domain label.
        domain: &'static str,
    },
    /// A typed final instruction could not be encoded.
    BytecodeEncoding {
        /// Source span responsible for the instruction.
        span: Span,
        /// Exact encoder failure.
        source: EncodeError,
    },
    /// Symbolic labels or branch relaxation could not produce final bytecode.
    BytecodeAssembly {
        /// Related instruction or compiler-owned label span, when available.
        span: Option<Span>,
        /// Exact assembler failure.
        source: AssemblerError,
    },
    /// A reachable compiler-owned statement anchor had the wrong entry stack.
    BytecodeStackInvariant {
        /// Source span that owns the statement anchor.
        span: Span,
        /// Final relocated bytecode position of the anchor.
        pc: BytecodePc,
        /// Compiler-required operand-stack depth.
        expected: u32,
        /// Verified reachable operand-stack depth.
        actual: u32,
    },
    /// The emitted body failed staged control-flow verification.
    BytecodeVerification {
        /// Exact instruction span for the verifier PC, when the error has one.
        span: Option<Span>,
        /// Exact join-target span for a two-position verifier failure.
        related_span: Option<Span>,
        /// Exact verifier failure.
        source: VerificationError,
    },
}

impl fmt::Display for LeafCompilationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignExecutable { executable } => {
                write!(formatter, "foreign executable {}", executable.index())
            }
            Self::InvalidExecutable { executable } => {
                write!(formatter, "invalid executable {}", executable.index())
            }
            Self::Unsupported { feature, span } => {
                write!(
                    formatter,
                    "unsupported leaf feature {feature:?} at {span:?}"
                )
            }
            Self::SemanticInvariant { invariant, span } => {
                write!(formatter, "compiler invariant `{invariant}` failed")?;
                if let Some(span) = span {
                    write!(formatter, " at {span:?}")?;
                }
                Ok(())
            }
            Self::CapacityExceeded { domain } => {
                write!(formatter, "compiler capacity exceeded for {domain}")
            }
            Self::BytecodeEncoding { span, source } => {
                write!(formatter, "bytecode encoding failed at {span:?}: {source}")
            }
            Self::BytecodeAssembly { span, source } => {
                write!(formatter, "bytecode assembly failed")?;
                if let Some(span) = span {
                    write!(formatter, " at {span:?}")?;
                }
                write!(formatter, ": {source}")
            }
            Self::BytecodeStackInvariant {
                span,
                pc,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "compiler stack invariant failed at {span:?} (PC {pc}): \
                     expected depth {expected}, got {actual}"
                )
            }
            Self::BytecodeVerification {
                span,
                related_span,
                source,
            } => {
                write!(formatter, "{source}")?;
                if let Some(span) = span {
                    write!(formatter, " at source {span:?}")?;
                }
                if let Some(related_span) = related_span {
                    write!(formatter, " (related source {related_span:?})")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for LeafCompilationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BytecodeEncoding { source, .. } => Some(source),
            Self::BytecodeAssembly { source, .. } => Some(source),
            Self::BytecodeVerification { source, .. } => Some(source),
            Self::ForeignExecutable { .. }
            | Self::InvalidExecutable { .. }
            | Self::Unsupported { .. }
            | Self::SemanticInvariant { .. }
            | Self::CapacityExceeded { .. }
            | Self::BytecodeStackInvariant { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickjs_frontend::{
        CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
    };

    #[test]
    fn function_index_capacity_checks_count_and_index_boundaries() {
        assert_eq!(
            checked_function_entry_count(MAX_FUNCTION_INDEX_ENTRIES, "test count"),
            Ok(MAX_FUNCTION_INDEX_ENTRIES)
        );
        assert_eq!(
            checked_function_index(MAX_FUNCTION_INDEX_ENTRIES - 1, "test index"),
            Ok(u16::try_from(MAX_FUNCTION_INDEX_ENTRIES - 1).expect("u16 index"))
        );
        assert!(matches!(
            checked_function_entry_count(u64::from(MAX_FUNCTION_INDEX_ENTRIES) + 1, "test count"),
            Err(LeafCompilationError::CapacityExceeded {
                domain: "test count"
            })
        ));
        assert!(matches!(
            checked_function_index(MAX_FUNCTION_INDEX_ENTRIES, "test index"),
            Err(LeafCompilationError::CapacityExceeded {
                domain: "test index"
            })
        ));
    }

    #[test]
    fn own_capture_layout_distinguishes_argument_function_and_scoped_cells() {
        let source = "function outer(arg){ var functionLocal=1; { let scoped=2; \
                      const capture=function(){ return arg+functionLocal+scoped; }; } }";
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let layout = FrameLayout::new(context.storage_plan(), executable.id())
                    .expect("frame layout");
                let capture_layout = context
                    .compiler_capture_layout(
                        executable.id(),
                        function.scope_id.get().expect("function scope"),
                        &layout,
                    )
                    .expect("capture layout");

                assert_eq!(
                    capture_layout.bindings(),
                    [
                        CompilerCapturedBinding::Argument(0),
                        CompilerCapturedBinding::FunctionLocal(0),
                        CompilerCapturedBinding::ScopedLocal(1),
                    ]
                );
            },
        )
        .expect("front-end acceptance");
    }

    #[test]
    fn captured_block_exit_closes_exact_scope_locals_in_reverse_slot_order() {
        let source = "function outer(){ { let first=1; let second=2; \
                      const capture=function(){ return first+second; }; } }";
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::BlockStatement(block) = &body.statements[0] else {
                    panic!("captured block");
                };
                let block_scope = context
                    .created_scope(block.scope_id.get(), block.node_id.get(), block.span)
                    .expect("block scope");
                let function_scope = context
                    .created_scope(function.scope_id.get(), function.node_id.get(), function.span)
                    .expect("function scope");
                let layout =
                    FrameLayout::new(context.storage_plan(), executable.id()).expect("frame layout");
                let capture_layout = context
                    .compiler_capture_layout(executable.id(), function_scope, &layout)
                    .expect("capture layout");
                assert_eq!(
                    capture_layout.bindings(),
                    [
                        CompilerCapturedBinding::ScopedLocal(0),
                        CompilerCapturedBinding::ScopedLocal(1),
                    ]
                );

                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                context
                    .plan_scope_exit(executable.id(), block_scope, &layout, &mut flow)
                    .expect("scope exit");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::ReturnUndef,
                    Operands::None,
                    block.span,
                ))
                .expect("terminal");
                let header =
                    UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                        false,
                        0,
                        2,
                    );
                let (_, verified) = flow
                    .finish()
                    .expect("assembly")
                    .verify_with_capture_layout(
                        FunctionIndexDomains::new(0, 0, 0, layout.local_count, 0),
                        header,
                        capture_layout,
                        VerificationLimits::default(),
                    )
                    .expect("verified close_loc");
                assert_eq!(
                    verified
                        .instructions()
                        .iter()
                        .map(|instruction| {
                            let instruction = instruction.decoded().instruction();
                            (instruction.opcode(), instruction.operands())
                        })
                        .collect::<Vec<_>>(),
                    [
                        (FinalOpcode::CloseLoc, Operands::Loc(1)),
                        (FinalOpcode::CloseLoc, Operands::Loc(0)),
                        (FinalOpcode::ReturnUndef, Operands::None),
                    ]
                );
            },
        )
        .expect("front-end acceptance");
    }

    fn abrupt_cleanup_fixture(
        source: &str,
    ) -> (
        Vec<SourceInstruction>,
        VerifiedControlFlow,
        Vec<CompilerCapturedBinding>,
        Span,
        Span,
    ) {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::WhileStatement(loop_statement) = &body.statements[0] else {
                    panic!("while statement");
                };
                let Statement::BlockStatement(loop_body) = &loop_statement.body else {
                    panic!("loop body");
                };
                let Statement::BlockStatement(inner) = &loop_body.body[2] else {
                    panic!("inner block");
                };
                let Statement::BreakStatement(break_statement) = &inner.body[2] else {
                    panic!("break statement");
                };
                let function_scope = function.scope_id.get().expect("function scope");
                let loop_scope = loop_body.scope_id.get().expect("loop body scope");
                let inner_scope = inner.scope_id.get().expect("inner scope");
                let layout =
                    FrameLayout::new(context.storage_plan(), executable.id()).expect("frame layout");
                let capture_layout = context
                    .compiler_capture_layout(executable.id(), function_scope, &layout)
                    .expect("capture layout");
                let captured_bindings = capture_layout.bindings().to_vec();

                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                let done = flow
                    .new_statement_label(loop_statement.span)
                    .expect("done label");
                let state = StatementPlanningState {
                    work: Vec::new(),
                    active_scopes: vec![function_scope, loop_scope, inner_scope],
                    loop_controls: vec![LoopControl {
                        break_target: done.clone(),
                        continue_target: done.clone(),
                        scope_depth: 1,
                    }],
                };
                context
                    .plan_loop_jump(
                        None,
                        break_statement.span,
                        LoopJump::Break,
                        &state,
                        &layout,
                        &mut flow,
                    )
                    .expect("abrupt cleanup");
                flow.bind(&done).expect("done binding");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::ReturnUndef,
                    Operands::None,
                    loop_statement.span,
                ))
                .expect("terminal");
                let header =
                    UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                        false,
                        0,
                        2,
                    );
                let (source_instructions, verified) = flow
                    .finish()
                    .expect("assembly")
                    .verify_with_capture_layout(
                        FunctionIndexDomains::new(0, 0, 0, layout.local_count, 0),
                        header,
                        capture_layout,
                        VerificationLimits::default(),
                    )
                    .expect("verified abrupt cleanup");
                let inner_binding_span = unit.semantic().scoping().symbol_span(
                    unit.semantic()
                        .scoping()
                        .iter_bindings_in(inner_scope)
                        .next()
                        .expect("inner binding"),
                );
                (
                    source_instructions,
                    verified,
                    captured_bindings,
                    inner_binding_span,
                    break_statement.span,
                )
            },
        )
        .expect("front-end acceptance")
    }

    #[test]
    fn abrupt_loop_exit_closes_captured_scope_suffix_from_inner_to_outer() {
        let source = "function outer(){ while(true){ let outerValue=1; \
                      const outerCapture=function(){return outerValue;}; \
                      { let innerValue=2; \
                      const innerCapture=function(){return innerValue;}; break; } } }";
        let (source_instructions, verified, captured, inner_span, break_span) =
            abrupt_cleanup_fixture(source);

        assert_eq!(
            captured,
            [
                CompilerCapturedBinding::ScopedLocal(0),
                CompilerCapturedBinding::ScopedLocal(2),
            ]
        );
        assert_eq!(
            verified
                .instructions()
                .iter()
                .map(|instruction| {
                    let instruction = instruction.decoded().instruction();
                    (instruction.opcode(), instruction.operands())
                })
                .collect::<Vec<_>>(),
            [
                (FinalOpcode::CloseLoc, Operands::Loc(2)),
                (FinalOpcode::CloseLoc, Operands::Loc(0)),
                (FinalOpcode::Goto8, Operands::Label8(1)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]
        );
        assert_eq!(
            exact_source_span(&source_instructions, BytecodePc::new(0)),
            Some(inner_span)
        );
        assert_eq!(
            exact_source_span(&source_instructions, BytecodePc::new(6)),
            Some(break_span)
        );
    }

    #[test]
    fn classic_for_schedule_places_rotation_before_test_update_and_final_exit() {
        let source = "function f(){ for(let i=0;i<2;i++){} }";
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::ForStatement(statement) = &body.statements[0] else {
                    panic!("classic for");
                };
                let scope = statement.scope_id.get().expect("for scope");
                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                let mut work = Vec::new();
                CompilationContext::schedule_for_statement(
                    statement, scope, &mut flow, &mut work, 1,
                )
                .expect("iterative for schedule");
                let execution = work.iter().rev().collect::<Vec<_>>();
                let close_positions = execution
                    .iter()
                    .enumerate()
                    .filter_map(|(position, task)| {
                        matches!(task, StatementWork::CloseScope(found) if *found == scope)
                            .then_some(position)
                    })
                    .collect::<Vec<_>>();
                let test_position = execution
                    .iter()
                    .position(|task| {
                        matches!(
                            task,
                            StatementWork::Bind(label)
                                if label.owner_span == statement.test.as_ref().expect("test").span()
                        )
                    })
                    .expect("test label");
                let rotate_position = execution
                    .iter()
                    .position(|task| {
                        matches!(
                            task,
                            StatementWork::Bind(label)
                                if label.owner_span
                                    == statement.update.as_ref().expect("update").span()
                        )
                    })
                    .expect("rotation label");
                let update_position = execution
                    .iter()
                    .position(|task| {
                        matches!(
                            task,
                            StatementWork::Expression(expression)
                                if expression.span()
                                    == statement.update.as_ref().expect("update").span()
                        )
                    })
                    .expect("update expression");
                let control = execution
                    .iter()
                    .find_map(|task| match task {
                        StatementWork::PushLoop(control) => Some(control),
                        _ => None,
                    })
                    .expect("loop control");

                assert_eq!(close_positions.len(), 2);
                assert!(close_positions[0] < test_position);
                assert!(rotate_position < close_positions[1]);
                assert!(close_positions[1] < update_position);
                assert_eq!(
                    control.continue_target.owner_span,
                    statement.update.as_ref().expect("update").span()
                );
                assert_eq!(control.scope_depth, 2);
                assert_eq!(
                    execution
                        .iter()
                        .filter(|task| {
                            matches!(task, StatementWork::PopScope(found) if *found == scope)
                        })
                        .count(),
                    1
                );
            },
        )
        .expect("front-end acceptance");
    }

    fn scheduled_statement_label(work: &[StatementWork<'_, '_>], span: Span) -> CompilerLabel {
        work.iter()
            .find_map(|task| match task {
                StatementWork::Bind(label) if label.owner_span == span => Some(label.clone()),
                _ => None,
            })
            .expect("scheduled label")
    }

    fn verify_capture_fixture(
        flow: PlannedControlFlow,
        local_count: u32,
        capture_layout: CompilerCaptureLayout,
        variable_reference_count: u32,
    ) -> (Vec<SourceInstruction>, VerifiedControlFlow) {
        let header =
            UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                false,
                0,
                variable_reference_count,
            );
        flow.finish()
            .expect("assembly")
            .verify_with_capture_layout(
                FunctionIndexDomains::new(0, 0, 0, local_count, 0),
                header,
                capture_layout,
                VerificationLimits::default(),
            )
            .expect("verified capture fixture")
    }

    const CAPTURED_FOR_CONTINUE_SOURCE: &str = "function outer(){ for(let i=0;i<2;i++){ \
        const capture=function(){return i;}; continue; } }";

    fn captured_for_continue_fixture() -> (Vec<SourceInstruction>, VerifiedControlFlow) {
        with_parsed_program(
            CAPTURED_FOR_CONTINUE_SOURCE,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::ForStatement(statement) = &body.statements[0] else {
                    panic!("classic for");
                };
                let Statement::BlockStatement(loop_body) = &statement.body else {
                    panic!("loop body");
                };
                let Statement::ContinueStatement(continue_statement) = &loop_body.body[1] else {
                    panic!("continue statement");
                };
                let scope = statement.scope_id.get().expect("for scope");
                let layout = FrameLayout::new(context.storage_plan(), executable.id())
                    .expect("frame layout");
                let capture_layout = context
                    .compiler_capture_layout(
                        executable.id(),
                        function.scope_id.get().expect("function scope"),
                        &layout,
                    )
                    .expect("capture layout");

                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                let mut work = Vec::new();
                CompilationContext::schedule_for_statement(
                    statement, scope, &mut flow, &mut work, 1,
                )
                .expect("for schedule");
                let control = work
                    .iter()
                    .find_map(|task| match task {
                        StatementWork::PushLoop(control) => Some(control.clone()),
                        _ => None,
                    })
                    .expect("loop control");
                let test =
                    scheduled_statement_label(&work, statement.test.as_ref().expect("test").span());
                let done = scheduled_statement_label(&work, statement.span);

                flow.branch(
                    BranchKind::Goto,
                    &control.continue_target,
                    continue_statement.span,
                )
                .expect("continue branch");
                flow.bind(&test).expect("test label");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Nop,
                    Operands::None,
                    statement.test.as_ref().expect("test").span(),
                ))
                .expect("unreachable test");
                flow.branch(
                    BranchKind::Goto,
                    &control.continue_target,
                    statement.test.as_ref().expect("test").span(),
                )
                .expect("test-to-rotation branch");
                flow.bind(&control.continue_target).expect("rotation label");
                context
                    .plan_scope_exit(executable.id(), scope, &layout, &mut flow)
                    .expect("loop-head rotation");
                let update = statement.update.as_ref().expect("update");
                context
                    .plan_expression(update, &layout, &mut flow)
                    .expect("update");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    update.span(),
                ))
                .expect("drop update");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::ReturnUndef,
                    Operands::None,
                    statement.span,
                ))
                .expect("rotation terminal");
                flow.bind(&done).expect("done label");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::ReturnUndef,
                    Operands::None,
                    statement.span,
                ))
                .expect("done terminal");
                verify_capture_fixture(flow, layout.local_count, capture_layout, 1)
            },
        )
        .expect("front-end acceptance")
    }

    #[test]
    fn captured_classic_for_continue_targets_close_loc_before_update() {
        let source = CAPTURED_FOR_CONTINUE_SOURCE;
        let (source_instructions, verified) = captured_for_continue_fixture();
        let continue_instruction = verified
            .instructions()
            .iter()
            .find(|instruction| {
                let pc = instruction.decoded().pc();
                exact_source_span(&source_instructions, pc).is_some_and(|span| {
                    &source[span.start as usize..span.end as usize] == "continue;"
                })
            })
            .expect("continue instruction");
        let target = continue_instruction
            .successors()
            .jump_target()
            .and_then(|target| verified.instruction(target))
            .expect("continue target");
        assert_eq!(
            target.decoded().instruction().opcode(),
            FinalOpcode::CloseLoc
        );
        let target_position = verified
            .instructions()
            .iter()
            .position(|instruction| instruction.decoded().pc() == target.decoded().pc())
            .expect("target position");
        let update = &verified.instructions()[target_position + 1];
        assert_eq!(
            update.decoded().instruction().opcode(),
            FinalOpcode::GetLocCheck
        );
        let update_span =
            exact_source_span(&source_instructions, update.decoded().pc()).expect("update span");
        assert_eq!(
            &source[update_span.start as usize..update_span.end as usize],
            "i"
        );
    }

    #[test]
    fn compiler_labels_retain_owner_spans_for_bind_and_finish_failures() {
        let owner = Span::new(10, 20);

        let mut duplicate = PlannedControlFlow::new(VerificationLimits::default());
        let label = duplicate.new_label(owner).expect("label");
        duplicate.bind(&label).expect("first bind");
        let error = duplicate.bind(&label).expect_err("duplicate bind");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source: AssemblerError::DuplicateLabel { .. },
            } if span == owner
        ));

        let mut unbound = PlannedControlFlow::new(VerificationLimits::default());
        let _unbound_label = unbound.new_label(owner).expect("unbound label");
        let distractor_owner = Span::new(1, 2);
        let distractor = unbound
            .new_label(distractor_owner)
            .expect("distractor label");
        unbound.bind(&distractor).expect("distractor binding");
        unbound
            .emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                Span::new(30, 31),
            ))
            .expect("terminal");
        let error = unbound.finish().expect_err("unbound label");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source: AssemblerError::UnboundLabel { .. },
            } if span == owner
        ));

        let mut end_target = PlannedControlFlow::new(VerificationLimits::default());
        let label = end_target.new_label(owner).expect("label");
        let distractor_owner = Span::new(2, 3);
        let distractor = end_target
            .new_label(distractor_owner)
            .expect("distractor label");
        end_target.bind(&distractor).expect("distractor binding");
        end_target
            .branch(BranchKind::Goto, &label, Span::new(40, 41))
            .expect("branch");
        end_target.bind(&label).expect("bind at end");
        let error = end_target.finish().expect_err("end target");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source: AssemblerError::TargetAtEnd { .. },
            } if span == owner
        ));
    }

    #[test]
    fn reachable_statement_anchors_require_an_empty_stack() {
        let anchor_span = Span::new(20, 30);
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(10, 11),
        ))
        .expect("push");
        let anchor = flow
            .new_statement_label(anchor_span)
            .expect("statement label");
        flow.branch(BranchKind::Goto, &anchor, anchor_span)
            .expect("widened branch");
        for _ in 0..130 {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                Span::new(12, 13),
            ))
            .expect("padding");
        }
        flow.bind(&anchor).expect("bind");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Return,
            Operands::None,
            Span::new(30, 31),
        ))
        .expect("return");

        let finished = flow.finish().expect("assembly");
        let error = finished
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("depth-one statement anchor");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeStackInvariant {
                span,
                pc,
                expected: 0,
                actual: 1,
            } if span == anchor_span && pc == BytecodePc::new(134)
        ));
    }

    #[test]
    fn unreachable_statement_anchors_have_no_required_entry_depth() {
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let live_exit = flow.new_label(Span::new(40, 41)).expect("live exit");
        flow.branch(BranchKind::Goto, &live_exit, Span::new(0, 1))
            .expect("skip unreachable region");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(10, 11),
        ))
        .expect("unreachable push");
        let anchor = flow
            .new_statement_label(Span::new(20, 21))
            .expect("unreachable statement anchor");
        flow.bind(&anchor).expect("unreachable anchor binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(21, 22),
        ))
        .expect("unreachable terminal");
        flow.bind(&live_exit).expect("live exit binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(40, 41),
        ))
        .expect("live terminal");

        let (source_instructions, control_flow) = flow
            .finish()
            .expect("assembly")
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect("unreachable anchor is accepted");
        let anchor_source = source_instructions
            .iter()
            .find(|instruction| instruction.span() == Span::new(21, 22))
            .expect("unreachable target source");
        let anchor_index = control_flow
            .instruction_index_at(anchor_source.pc())
            .expect("verified unreachable target");
        assert_eq!(
            control_flow
                .instruction(anchor_index)
                .expect("unreachable target instruction")
                .entry_stack_depth(),
            None
        );
    }

    #[test]
    fn inconsistent_join_maps_incoming_and_target_source_spans() {
        let incoming_span = Span::new(20, 21);
        let target_span = Span::new(30, 31);
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let join = flow.new_label(Span::new(10, 11)).expect("join");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(0, 1),
        ))
        .expect("condition");
        flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
            .expect("conditional branch");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(2, 3),
        ))
        .expect("unbalanced value");
        flow.branch(BranchKind::Goto, &join, incoming_span)
            .expect("incoming edge");
        flow.bind(&join).expect("join binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            target_span,
        ))
        .expect("join target");

        let error = flow
            .finish()
            .expect("assembly")
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("inconsistent stack join");

        assert!(matches!(
            error,
            LeafCompilationError::BytecodeVerification {
                span: Some(span),
                related_span: Some(related_span),
                source,
            } if span == incoming_span
                && related_span == target_span
                && matches!(
                    source.kind(),
                    VerificationErrorKind::InconsistentStackAtJoin { .. }
                )
        ));
    }

    #[test]
    fn missing_primary_verifier_source_mapping_fails_as_a_compiler_invariant() {
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(0, 1),
        ))
        .expect("first push");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(1, 2),
        ))
        .expect("second push");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Return,
            Operands::None,
            Span::new(2, 3),
        ))
        .expect("return");
        let mut finished = flow.finish().expect("assembly");
        finished
            .source_instructions
            .retain(|instruction| instruction.pc() != BytecodePc::new(1));

        let error = finished
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::new(100, 10, 0, 0, 100, 1),
            )
            .expect_err("missing exact primary provenance");
        assert!(matches!(
            error,
            LeafCompilationError::SemanticInvariant {
                invariant: "verifier instruction PC resolves to an exact source instruction",
                span: None,
            }
        ));
    }

    #[test]
    fn missing_join_target_source_mapping_fails_as_a_compiler_invariant() {
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let join = flow.new_label(Span::new(10, 11)).expect("join");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(0, 1),
        ))
        .expect("condition");
        flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
            .expect("conditional branch");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(2, 3),
        ))
        .expect("unbalanced value");
        flow.branch(BranchKind::Goto, &join, Span::new(3, 4))
            .expect("incoming edge");
        flow.bind(&join).expect("join binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(4, 5),
        ))
        .expect("join target");
        let mut finished = flow.finish().expect("assembly");
        finished
            .source_instructions
            .retain(|instruction| instruction.span() != Span::new(4, 5));

        let error = finished
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("missing exact related provenance");
        assert!(matches!(
            error,
            LeafCompilationError::SemanticInvariant {
                invariant: "verifier join target resolves to an exact source instruction",
                span: None,
            }
        ));
    }

    #[test]
    fn widened_branch_verifier_failures_use_the_relocated_target_span() {
        let target_span = Span::new(30, 31);
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let target = flow.new_label(Span::new(20, 21)).expect("target");
        flow.branch(BranchKind::Goto, &target, Span::new(0, 1))
            .expect("widened branch");
        for _ in 0..130 {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                Span::new(10, 11),
            ))
            .expect("padding");
        }
        flow.bind(&target).expect("target binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            target_span,
        ))
        .expect("underflowing target");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(31, 32),
        ))
        .expect("terminal");

        let error = flow
            .finish()
            .expect("assembly")
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("reachable drop underflows");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeVerification {
                span: Some(span),
                related_span: None,
                source,
            } if span == target_span
                && source.pc() == Some(BytecodePc::new(133))
                && matches!(
                    source.kind(),
                    VerificationErrorKind::StackUnderflow {
                        required: 1,
                        available: 0,
                    }
                )
        ));
    }
}
