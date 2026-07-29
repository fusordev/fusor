use std::{error::Error, fmt, sync::Arc};

use oxc_ast::{
    AstKind,
    ast::{
        BindingPattern, Expression, Function, FunctionBody, FunctionType, PropertyKind,
        SequenceExpression, Statement, UnaryExpression, VariableDeclaration,
        VariableDeclarationKind,
    },
};
use oxc_semantic::{ReferenceId, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::operator::{BinaryOperator, UnaryOperator};
use quickjs_bytecode::{
    BytecodeBuilder, BytecodePc, EncodeError, FinalOpcode, FunctionIndexDomains, Instruction,
    Operands, StackEffectError, UnverifiedFunctionBody, UnverifiedFunctionHeader,
    VerificationError, VerificationLimits, VerifiedControlFlow, verify_control_flow,
};
use quickjs_frontend::{ParsedUnit, Span};

use crate::storage::{
    BindingId, CompilationUnitKind, CompilerError, DeclarationKind, Executable, ExecutableId,
    ExecutableKind, NativeReferenceId, PlannedStorage, StoragePlacement, StoragePlan,
    build_planned_storage,
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

/// Owned output from the validated ordinary leaf-function lowering slice.
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
    /// The accepted Script-only family is straight-line and pool-free. It
    /// supports simple local declarations, immediate primitive values,
    /// resolved argument/local reads, unary and binary operators, expression
    /// statements, and a terminal or implicit return. The entire function is
    /// converted to typed pseudo-instructions before any byte is emitted.
    ///
    /// # Errors
    ///
    /// Rejects foreign executable selections, unsupported source structure,
    /// inconsistent semantic identities, bytecode encoding failures, and
    /// verifier failures.
    pub fn compile_leaf(
        &self,
        selection: &CompilationExecutable,
        limits: VerificationLimits,
    ) -> Result<CompiledLeafFunction, LeafCompilationError> {
        let executable = self.resolve_selection(selection)?;
        let validated = self.validate_leaf(executable)?;
        let mut emitter = StraightLineEmitter::new(limits.max_bytecode_bytes_per_function());

        for instruction in &validated.instructions {
            emitter.emit(instruction.opcode, instruction.operands, instruction.span)?;
        }
        if emitter.depth != 0 {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary leaf returns with an empty operand stack",
                span: validated
                    .instructions
                    .last()
                    .map(|instruction| instruction.span),
            });
        }

        let (bytecode, source_instructions, expected_stack_size) = emitter.finish();
        let domains =
            FunctionIndexDomains::new(0, 0, validated.argument_count, validated.local_count, 0);
        let header = UnverifiedFunctionHeader::stripped_ordinary_source_function(
            validated.strict,
            validated.argument_count,
        );
        let control_flow = verify_control_flow(
            UnverifiedFunctionBody::new(bytecode, expected_stack_size, domains, header),
            limits,
        )
        .map_err(|source| LeafCompilationError::BytecodeVerification { source })?;

        Ok(CompiledLeafFunction {
            executable,
            storage_plan: Arc::clone(&self.planned.plan),
            source_text: Arc::clone(&self.source_text),
            locals: validated.locals.into(),
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
        let mut instructions = Vec::new();
        for local in layout.locals.iter().rev() {
            if local.has_temporal_dead_zone {
                instructions.push(PlannedInstruction::new(
                    FinalOpcode::SetLocUninitialized,
                    Operands::Loc(local.slot.index()),
                    local.declaration_span,
                ));
            }
        }
        self.validate_body(body, &layout, &mut instructions)?;

        Ok(ValidatedLeaf {
            strict: executable.is_strict(),
            argument_count: executable.parameter_count(),
            local_count: u32::from(layout.local_count),
            locals: layout
                .locals
                .iter()
                .map(|local| LoweredLocal {
                    binding: local.binding,
                    slot: local.slot,
                })
                .collect(),
            instructions,
        })
    }

    fn validate_body(
        &self,
        body: &FunctionBody<'arena>,
        layout: &FrameLayout,
        instructions: &mut Vec<PlannedInstruction>,
    ) -> Result<(), LeafCompilationError> {
        let mut terminated = false;
        for statement in &body.statements {
            if terminated {
                return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span());
            }
            match statement {
                Statement::VariableDeclaration(declaration) => {
                    self.validate_declaration(declaration, layout, instructions)?;
                }
                Statement::ExpressionStatement(statement) => {
                    self.plan_expression(&statement.expression, layout, instructions)?;
                    instructions.push(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        statement.expression.span(),
                    ));
                }
                Statement::EmptyStatement(_) => {}
                Statement::ReturnStatement(statement) => {
                    if let Some(argument) = &statement.argument {
                        self.plan_expression(argument, layout, instructions)?;
                        instructions.push(PlannedInstruction::new(
                            FinalOpcode::Return,
                            Operands::None,
                            statement.span,
                        ));
                    } else {
                        instructions.push(PlannedInstruction::new(
                            FinalOpcode::ReturnUndef,
                            Operands::None,
                            statement.span,
                        ));
                    }
                    terminated = true;
                }
                _ => {
                    return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span());
                }
            }
        }
        if !terminated {
            instructions.push(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                body.span,
            ));
        }
        Ok(())
    }

    fn validate_declaration(
        &self,
        declaration: &VariableDeclaration<'arena>,
        layout: &FrameLayout,
        instructions: &mut Vec<PlannedInstruction>,
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
                    self.plan_expression(initializer, layout, instructions)?;
                    instructions.push(plan_put_slot(frame_slot, identifier.span));
                }
                None if declaration.kind == VariableDeclarationKind::Let => {
                    instructions.push(PlannedInstruction::new(
                        FinalOpcode::Undefined,
                        Operands::None,
                        identifier.span,
                    ));
                    instructions.push(plan_put_slot(frame_slot, identifier.span));
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
        instructions: &mut Vec<PlannedInstruction>,
    ) -> Result<(), LeafCompilationError> {
        let mut work = vec![ExpressionWork::Visit(expression)];
        while let Some(task) = work.pop() {
            match task {
                ExpressionWork::Emit(instruction) => instructions.push(instruction),
                ExpressionWork::Visit(expression) => {
                    if let Some(literal) = plan_literal(expression) {
                        instructions.push(literal?);
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
                            instructions.push(self.plan_read_slot(
                                binding,
                                frame_slot,
                                identifier.span,
                            )?);
                        }
                        Expression::UnaryExpression(unary) => {
                            Self::plan_unary_expression(unary, &mut work, instructions)?;
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

    fn plan_unary_expression<'expression>(
        unary: &'expression UnaryExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
        instructions: &mut Vec<PlannedInstruction>,
    ) -> Result<(), LeafCompilationError> {
        if unary.operator == UnaryOperator::UnaryNegation
            && let Expression::NumericLiteral(literal) = &unary.argument
            && literal.value != 0.0
            && let Some(integer) = exact_negated_i32(literal.value)
        {
            instructions.push(plan_push_integer(integer, unary.span));
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
        }
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
        if !reference.access().reads() || reference.access().writes() {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }
        Ok(reference.binding())
    }
}

fn is_object_method_or_accessor(unit: &ParsedUnit<'_, '_>, node_id: oxc_semantic::NodeId) -> bool {
    let AstKind::ObjectProperty(property) = unit.semantic().nodes().parent_kind(node_id) else {
        return false;
    };
    let Expression::FunctionExpression(value) = &property.value else {
        return false;
    };
    value.node_id.get() == node_id
        && (property.method || !matches!(property.kind, PropertyKind::Init))
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

enum ExpressionWork<'expression, 'arena> {
    Visit(&'expression Expression<'arena>),
    Emit(PlannedInstruction),
}

#[derive(Clone, Copy)]
struct ArgumentSlot(u16);

#[derive(Clone, Copy)]
enum FrameSlot {
    Argument(ArgumentSlot),
    Local(LocalSlot),
}

struct FrameLocal {
    binding: BindingId,
    slot: LocalSlot,
    has_temporal_dead_zone: bool,
    declaration_span: Span,
}

struct FrameLayout {
    slots: Vec<Option<FrameSlot>>,
    locals: Vec<FrameLocal>,
    local_count: u16,
}

impl FrameLayout {
    fn new(plan: &StoragePlan, executable: ExecutableId) -> Result<Self, LeafCompilationError> {
        let mut slots = vec![None; plan.bindings().len()];
        let mut locals = Vec::new();
        let mut local_count = 0_u16;
        let bindings = plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        for binding in bindings {
            let slot = match binding.placement() {
                StoragePlacement::Argument { parameter_index } => {
                    let parameter_index = u16::try_from(parameter_index).map_err(|_| {
                        LeafCompilationError::CapacityExceeded {
                            domain: "function argument slots",
                        }
                    })?;
                    FrameSlot::Argument(ArgumentSlot(parameter_index))
                }
                StoragePlacement::Local => {
                    let slot = LocalSlot(local_count);
                    local_count = local_count.checked_add(1).ok_or(
                        LeafCompilationError::CapacityExceeded {
                            domain: "function local slots",
                        },
                    )?;
                    let declaration_span = binding.declaration_spans().first().copied().ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "function local has a declaration span",
                            span: None,
                        },
                    )?;
                    locals.push(FrameLocal {
                        binding: binding.id(),
                        slot,
                        has_temporal_dead_zone: binding.policy().has_temporal_dead_zone(),
                        declaration_span,
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
        Ok(Self {
            slots,
            locals,
            local_count,
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
    locals: Vec<LoweredLocal>,
    instructions: Vec<PlannedInstruction>,
}

struct StraightLineEmitter {
    builder: BytecodeBuilder,
    source_instructions: Vec<SourceInstruction>,
    depth: u32,
    max_depth: u32,
}

impl StraightLineEmitter {
    fn new(byte_limit: u32) -> Self {
        Self {
            builder: BytecodeBuilder::with_byte_limit(byte_limit),
            source_instructions: Vec::new(),
            depth: 0,
            max_depth: 0,
        }
    }

    fn emit(
        &mut self,
        opcode: FinalOpcode,
        operands: Operands,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let pc = self.builder.next_pc();
        let instruction = Instruction::new(opcode, operands).map_err(|source| {
            LeafCompilationError::BytecodeEncoding {
                span,
                source: EncodeError::InvalidInstruction { pc, source },
            }
        })?;
        let effect = instruction
            .stack_effect()
            .map_err(|source| LeafCompilationError::BytecodeStackEffect { span, source })?;
        if self.depth < effect.pops() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiler-emitted straight-line stack does not underflow",
                span: Some(span),
            });
        }
        let output_depth = self
            .depth
            .checked_sub(effect.pops())
            .and_then(|depth| depth.checked_add(effect.pushes()))
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "operand stack depth",
            })?;
        let emitted_pc = self
            .builder
            .push_instruction(instruction)
            .map_err(|source| LeafCompilationError::BytecodeEncoding { span, source })?;
        if emitted_pc != pc {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "bytecode builder returns its prior next position",
                span: Some(span),
            });
        }
        self.depth = output_depth;
        self.max_depth = self.max_depth.max(output_depth);
        self.source_instructions
            .push(SourceInstruction { pc, span });
        Ok(())
    }

    fn finish(self) -> (Vec<u8>, Vec<SourceInstruction>, u32) {
        (
            self.builder.into_bytes(),
            self.source_instructions,
            self.max_depth,
        )
    }
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

fn plan_put_slot(slot: FrameSlot, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match slot {
        FrameSlot::Argument(slot) => compact_put_argument(slot),
        FrameSlot::Local(slot) => compact_put_local(slot),
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
    /// A statement sequence is outside the validated straight-line family.
    UnsupportedBody,
    /// A declaration is not a simple `var`, `let`, or `const` binding.
    UnsupportedDeclaration,
    /// An expression requires control flow, calls, properties, or another
    /// unsupported semantic family.
    UnsupportedExpression,
    /// A literal requires a constant, atom, `BigInt`, or `RegExp` pool entry.
    UnsupportedLiteral,
    /// A binding cannot be represented by this frame layout.
    UnsupportedBinding,
    /// A reference is not a pure read.
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
    /// Static opcode metadata could not produce a complete stack effect.
    BytecodeStackEffect {
        /// Source span responsible for the instruction.
        span: Span,
        /// Exact stack-effect failure.
        source: StackEffectError,
    },
    /// The emitted body failed staged control-flow verification.
    BytecodeVerification {
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
            Self::BytecodeStackEffect { span, source } => {
                write!(
                    formatter,
                    "bytecode stack effect failed at {span:?}: {source}"
                )
            }
            Self::BytecodeVerification { source } => {
                write!(formatter, "bytecode verification failed: {source}")
            }
        }
    }
}

impl Error for LeafCompilationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BytecodeEncoding { source, .. } => Some(source),
            Self::BytecodeStackEffect { source, .. } => Some(source),
            Self::BytecodeVerification { source } => Some(source),
            Self::ForeignExecutable { .. }
            | Self::InvalidExecutable { .. }
            | Self::Unsupported { .. }
            | Self::SemanticInvariant { .. }
            | Self::CapacityExceeded { .. } => None,
        }
    }
}
