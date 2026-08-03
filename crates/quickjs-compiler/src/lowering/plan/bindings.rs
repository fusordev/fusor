use super::super::{
    AssignmentExpression, AssignmentOperator, AssignmentTarget, BindingId, BindingIdentifier,
    BindingPattern, BranchKind, CompilationContext, CompilationGoal, CompiledConstantPool,
    DestructuringBindingInitialization, DynamicFunctionKind, ExecutableId, Expression, FinalOpcode,
    ForStatementLeft, FrameLayout, FrameSlot, FunctionTreeLayout, GetSpan, IdentifierReference,
    LeafCompilationError, LocalSlot, NativeReferenceId, Operands, PlannedControlFlow,
    PlannedInstruction, RealmGlobalId, ReferenceAccess, ReferenceId, Span, StoragePlacement,
    SymbolId, UnresolvedGlobalId, UnsupportedLeafFeature, VariableDeclaration,
    VariableDeclarationKind, VariableDeclarator, WritePolicy, anonymous_named_evaluation_span,
    binary_opcode, compact_get_argument, compact_get_capture, compact_get_local,
    compact_put_argument, compact_put_capture, compact_put_local, compact_set_argument,
    compact_set_capture, compact_set_local, plan_put_slot, unsupported,
};
use super::expressions::{ExpressionPlanner, ExpressionWork};

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
        slot: LocalSlot,
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

impl<'arena> ExpressionPlanner<'_, '_, 'arena, '_> {
    pub(in crate::lowering) fn plan_identifier_read(
        &self,
        identifier: &IdentifierReference<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        if !reference.access().reads() || reference.access().writes() {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedReference,
                identifier.span,
            );
        }
        let instruction = match reference {
            LoweredReference::Frame { binding, slot, .. } => {
                self.plan_read_slot(binding, slot, identifier.span)?
            }
            LoweredReference::RealmGlobal { slot, .. } => PlannedInstruction::new(
                FinalOpcode::GetVar,
                Operands::VarRef(slot),
                identifier.span,
            ),
        };
        flow.emit(instruction)
    }

    pub(in crate::lowering) fn plan_realm_global_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        slot: u16,
        inferred_name: Option<PlannedInstruction>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let read = PlannedInstruction::new(
            FinalOpcode::GetVar,
            Operands::VarRef(slot),
            assignment.left.span(),
        );
        let write = PlannedInstruction::new(
            FinalOpcode::PutVar,
            Operands::VarRef(slot),
            assignment.left.span(),
        );
        match assignment.operator {
            AssignmentOperator::Assign => {
                work.push(ExpressionWork::Emit(write));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.span,
                )));
                if let Some(set_name) = inferred_name {
                    work.push(ExpressionWork::Emit(set_name));
                }
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
                work.push(ExpressionWork::Emit(write));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.span,
                )));
                if let Some(set_name) = inferred_name {
                    work.push(ExpressionWork::Emit(set_name));
                }
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    assignment.left.span(),
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
                        assignment.left.span(),
                    )));
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.left.span(),
                )));
                work.push(ExpressionWork::Emit(read));
            }
            operator => {
                let binary = operator.to_binary_operator().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "nonlogical compound assignment has a binary operator",
                        span: Some(assignment.span),
                    },
                )?;
                work.push(ExpressionWork::Emit(write));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    binary_opcode(binary),
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(read));
            }
        }
        Ok(())
    }

    pub(in crate::lowering) fn plan_read_slot(
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

    pub(in crate::lowering) fn plan_write_slot(
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

    pub(in crate::lowering) fn push_slot_write<'expression>(
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
}

impl<'arena> CompilationContext<'_, 'arena, '_> {
    pub(in crate::lowering) fn plan_for_in_head(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let ForStatementLeft::VariableDeclaration(declaration) = left else {
            if left.as_assignment_target().is_none() {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, left.span());
            }
            return Ok(());
        };
        let (identifier, initializer) = self.validate_for_in_declaration(declaration, layout)?;
        let Some(initializer) = initializer else {
            return Ok(());
        };
        let executable = self.planned.plan.executable(layout.executable).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: layout.executable,
            },
        )?;
        if declaration.kind != VariableDeclarationKind::Var || executable.is_strict() {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                declaration.span,
            );
        }
        if let Some(span) = anonymous_named_evaluation_span(initializer) {
            return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
        }
        self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
        self.emit_for_in_declaration_write(declaration.kind, identifier, layout, tree_layout, flow)
    }

    pub(in crate::lowering) fn plan_for_of_head(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
    ) -> Result<(), LeafCompilationError> {
        let ForStatementLeft::VariableDeclaration(declaration) = left else {
            if left.as_assignment_target().is_none() {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, left.span());
            }
            return Ok(());
        };
        let pattern = Self::validate_for_of_declaration(declaration)?;
        let BindingPattern::BindingIdentifier(identifier) = pattern else {
            // Destructuring heads validate every binding when the
            // per-iteration destructure binds it, exactly like the ordinary
            // destructuring-declaration path.
            return Ok(());
        };
        let binding = self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-of declared compiler binding exists",
                    span: Some(identifier.span),
                })?;
        if storage.placement() == StoragePlacement::GlobalObject {
            self.validate_realm_global_var_declaration(declaration.kind, storage, identifier.span)?;
        } else {
            let slot = layout
                .slot(binding)
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                    span: identifier.span,
                })?;
            self.validate_declaration_storage(declaration.kind, binding, slot, identifier.span)?;
        }
        Ok(())
    }

    /// Validates the shared shape of a for-of declaration head: exactly one
    /// `var`/`let`/`const` declarator with no initializer. Returns the
    /// declarator's binding pattern.
    fn validate_for_of_declaration<'declaration>(
        declaration: &'declaration VariableDeclaration<'arena>,
    ) -> Result<&'declaration BindingPattern<'arena>, LeafCompilationError> {
        if declaration.declare
            || !matches!(
                declaration.kind,
                VariableDeclarationKind::Var
                    | VariableDeclarationKind::Let
                    | VariableDeclarationKind::Const
            )
            || declaration.declarations.len() != 1
        {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                declaration.span,
            );
        }
        let declarator = &declaration.declarations[0];
        if let Some(initializer) = declarator.init.as_ref() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc rejects initializers in for-of declarations",
                span: Some(initializer.span()),
            });
        }
        Ok(&declarator.id)
    }

    pub(in crate::lowering) fn plan_for_in_assignment(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let ForStatementLeft::VariableDeclaration(declaration) = left {
            let (identifier, _) = self.validate_for_in_declaration(declaration, layout)?;
            return self.emit_for_in_declaration_write(
                declaration.kind,
                identifier,
                layout,
                tree_layout,
                flow,
            );
        }

        let target =
            left.as_assignment_target()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-in non-declaration head is an assignment target",
                    span: Some(left.span()),
                })?;
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                let reference = self.lowered_reference(
                    identifier.reference_id.get(),
                    identifier.span,
                    layout,
                    tree_layout,
                )?;
                self.validate_lowered_mutation_reference(reference, false, identifier.span)?;
                match reference {
                    LoweredReference::Frame { binding, slot, .. } => {
                        for instruction in ExpressionPlanner::new(self).plan_write_slot(
                            binding,
                            slot,
                            false,
                            identifier.span,
                        )? {
                            flow.emit(instruction)?;
                        }
                    }
                    LoweredReference::RealmGlobal { slot, .. } => {
                        flow.emit(PlannedInstruction::new(
                            FinalOpcode::PutVar,
                            Operands::VarRef(slot),
                            identifier.span,
                        ))?;
                    }
                }
            }
            AssignmentTarget::StaticMemberExpression(member) if !member.optional => {
                self.plan_expression(&member.object, layout, tree_layout, constants, flow)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    member.span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::PutField,
                    Operands::Atom(constants.property_atom_index(member.property.span)?),
                    member.span,
                ))?;
            }
            AssignmentTarget::ComputedMemberExpression(member) if !member.optional => {
                self.plan_expression(&member.object, layout, tree_layout, constants, flow)?;
                self.plan_expression(&member.expression, layout, tree_layout, constants, flow)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Rot3l,
                    Operands::None,
                    member.span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::PutArrayEl,
                    Operands::None,
                    member.span,
                ))?;
            }
            _ => {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, target.span());
            }
        }
        Ok(())
    }

    /// Stores the per-iteration for-of value into the loop head. Identifier
    /// and member heads share the for-in path; destructuring heads run the
    /// declaration or assignment pattern machinery directly on the value
    /// already on the stack (the loop's `for_of_next` step pushed it above
    /// the verified record, whose offset stays zero).
    pub(in crate::lowering) fn plan_for_of_assignment(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let ForStatementLeft::VariableDeclaration(declaration) = left {
            let pattern = Self::validate_for_of_declaration(declaration)?;
            if matches!(pattern, BindingPattern::BindingIdentifier(_)) {
                return self.plan_for_in_assignment(left, layout, tree_layout, constants, flow);
            }
            return self.plan_destructuring_pattern_value(
                pattern,
                DestructuringBindingInitialization::Declaration(declaration.kind),
                layout,
                tree_layout,
                constants,
                flow,
            );
        }
        let target =
            left.as_assignment_target()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-of non-declaration head is an assignment target",
                    span: Some(left.span()),
                })?;
        match target {
            AssignmentTarget::ArrayAssignmentTarget(_)
            | AssignmentTarget::ObjectAssignmentTarget(_) => {
                let mut work = Vec::new();
                self.plan_assignment_target_value(
                    target,
                    &mut work,
                    flow,
                    layout,
                    tree_layout,
                    constants,
                )?;
                while let Some(task) = work.pop() {
                    match task {
                        ExpressionWork::Emit(instruction) => flow.emit(instruction)?,
                        ExpressionWork::Branch { kind, target, span } => {
                            flow.branch(kind, &target, span)?;
                        }
                        ExpressionWork::Bind(label) => flow.bind(&label)?,
                        ExpressionWork::Visit(expression) => {
                            self.plan_expression(expression, layout, tree_layout, constants, flow)?;
                        }
                    }
                }
                Ok(())
            }
            AssignmentTarget::AssignmentTargetIdentifier(_)
            | AssignmentTarget::StaticMemberExpression(_)
            | AssignmentTarget::ComputedMemberExpression(_) => {
                self.plan_for_in_assignment(left, layout, tree_layout, constants, flow)
            }
            AssignmentTarget::TSAsExpression(_)
            | AssignmentTarget::TSSatisfiesExpression(_)
            | AssignmentTarget::TSNonNullExpression(_)
            | AssignmentTarget::TSTypeAssertion(_)
            | AssignmentTarget::PrivateFieldExpression(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedExpression, target.span())
            }
        }
    }

    fn validate_for_in_declaration<'declaration>(
        &self,
        declaration: &'declaration VariableDeclaration<'arena>,
        layout: &FrameLayout,
    ) -> Result<
        (
            &'declaration BindingIdentifier<'arena>,
            Option<&'declaration Expression<'arena>>,
        ),
        LeafCompilationError,
    > {
        if declaration.declare
            || !matches!(
                declaration.kind,
                VariableDeclarationKind::Var
                    | VariableDeclarationKind::Let
                    | VariableDeclarationKind::Const
            )
            || declaration.declarations.len() != 1
        {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                declaration.span,
            );
        }
        let declarator = &declaration.declarations[0];
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                declarator.id.span(),
            );
        };
        let binding = self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-in declared compiler binding exists",
                    span: Some(identifier.span),
                })?;
        if storage.placement() == StoragePlacement::GlobalObject {
            self.validate_realm_global_var_declaration(declaration.kind, storage, identifier.span)?;
        } else {
            let slot = layout
                .slot(binding)
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                    span: identifier.span,
                })?;
            self.validate_declaration_storage(declaration.kind, binding, slot, identifier.span)?;
        }
        Ok((identifier, declarator.init.as_ref()))
    }

    fn emit_for_in_declaration_write(
        &self,
        declaration_kind: VariableDeclarationKind,
        identifier: &BindingIdentifier<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let binding = self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-in declared compiler binding exists",
                    span: Some(identifier.span),
                })?;
        if storage.placement() == StoragePlacement::GlobalObject {
            self.validate_realm_global_var_declaration(declaration_kind, storage, identifier.span)?;
            let global = tree_layout.realm_globals.for_binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "for-in Program var has a constructor-realm global identity",
                    span: Some(identifier.span),
                },
            )?;
            let slot = tree_layout.realm_globals.closure_slot(
                &self.planned.plan,
                layout.executable,
                global,
            )?;
            return flow.emit(PlannedInstruction::new(
                FinalOpcode::PutVar,
                Operands::VarRef(slot),
                identifier.span,
            ));
        }

        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBinding,
                span: identifier.span,
            })?;
        flow.emit(plan_put_slot(slot, identifier.span))
    }

    pub(in crate::lowering) fn validate_declaration(
        &self,
        declaration: &VariableDeclaration<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
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
            match &declarator.id {
                BindingPattern::ArrayPattern(pattern) => {
                    let Some(initializer) = &declarator.init else {
                        return unsupported(
                            UnsupportedLeafFeature::UnsupportedDeclaration,
                            declarator.span,
                        );
                    };
                    return self.plan_array_destructuring_declaration(
                        pattern,
                        initializer,
                        declaration.kind,
                        layout,
                        tree_layout,
                        constants,
                        flow,
                    );
                }
                BindingPattern::ObjectPattern(pattern) => {
                    let Some(initializer) = &declarator.init else {
                        return unsupported(
                            UnsupportedLeafFeature::UnsupportedDeclaration,
                            declarator.span,
                        );
                    };
                    return self.plan_object_destructuring_declaration(
                        pattern,
                        initializer,
                        declaration.kind,
                        layout,
                        tree_layout,
                        constants,
                        flow,
                    );
                }
                BindingPattern::AssignmentPattern(_) => {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedDeclaration,
                        declarator.span,
                    );
                }
                BindingPattern::BindingIdentifier(identifier) => {
                    self.plan_identifier_declaration(
                        identifier,
                        declaration.kind,
                        declarator,
                        layout,
                        tree_layout,
                        constants,
                        flow,
                    )?;
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "identifier declaration planning carries the same explicit frame, tree, constant, and flow authority as every other declaration form"
    )]
    fn plan_identifier_declaration(
        &self,
        identifier: &BindingIdentifier<'arena>,
        declaration_kind: VariableDeclarationKind,
        declarator: &VariableDeclarator<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        {
            let binding =
                self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "declared compiler binding exists",
                    span: Some(identifier.span),
                },
            )?;
            if storage.placement() == StoragePlacement::GlobalObject {
                self.validate_realm_global_var_declaration(
                    declaration_kind,
                    storage,
                    identifier.span,
                )?;
                if let Some(initializer) = &declarator.init {
                    let set_name = self.plan_inferred_function_name_for_initializer(
                        identifier,
                        initializer,
                        constants,
                    )?;
                    self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
                    if let Some(set_name) = set_name {
                        flow.emit(set_name)?;
                    }
                    let global = tree_layout.realm_globals.for_binding(binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "declared Program var has a constructor-realm global identity",
                            span: Some(identifier.span),
                        },
                    )?;
                    let slot = tree_layout.realm_globals.closure_slot(
                        &self.planned.plan,
                        layout.executable,
                        global,
                    )?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::PutVar,
                        Operands::VarRef(slot),
                        identifier.span,
                    ))?;
                }
                return Ok(());
            }

            let frame_slot = layout
                .slot(binding)
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                    span: identifier.span,
                })?;
            self.validate_declaration_storage(
                declaration_kind,
                binding,
                frame_slot,
                identifier.span,
            )?;

            match &declarator.init {
                Some(initializer) => {
                    let set_name = self.plan_inferred_function_name_for_initializer(
                        identifier,
                        initializer,
                        constants,
                    )?;
                    self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
                    if let Some(set_name) = set_name {
                        flow.emit(set_name)?;
                    }
                    flow.emit(plan_put_slot(frame_slot, identifier.span))?;
                }
                None if declaration_kind == VariableDeclarationKind::Let => {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Undefined,
                        Operands::None,
                        identifier.span,
                    ))?;
                    flow.emit(plan_put_slot(frame_slot, identifier.span))?;
                }
                None if declaration_kind == VariableDeclarationKind::Var => {}
                None => {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedDeclaration,
                        declarator.span,
                    );
                }
            }
            Ok(())
        }
    }
}

impl CompilationContext<'_, '_, '_> {
    pub(in crate::lowering) fn binding_for_identifier(
        &self,
        symbol_id: Option<SymbolId>,
        span: Span,
    ) -> Result<BindingId, LeafCompilationError> {
        let symbol_id = symbol_id.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "binding identifier has Oxc symbol identity",
            span: Some(span),
        })?;
        if let Some(binding) = self
            .planned
            .identities
            .binding_by_declaration
            .get(&(symbol_id, span.start, span.end))
            .copied()
        {
            return Ok(binding);
        }
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

    pub(in crate::lowering) fn lowered_reference(
        &self,
        reference_id: Option<ReferenceId>,
        span: Span,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<LoweredReference, LeafCompilationError> {
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
        match native {
            NativeReferenceId::Resolved(resolved_id) => {
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
                let binding = self.planned.plan.binding(reference.binding()).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "resolved compiler binding exists",
                        span: Some(span),
                    },
                )?;
                match binding.placement() {
                    StoragePlacement::Argument { .. } | StoragePlacement::Local => {
                        let slot =
                            layout
                                .slot(binding.id())
                                .ok_or(LeafCompilationError::Unsupported {
                                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                                    span,
                                })?;
                        Ok(LoweredReference::Frame {
                            binding: binding.id(),
                            slot,
                            access: reference.access(),
                        })
                    }
                    StoragePlacement::GlobalObject => self.lowered_realm_global_binding_reference(
                        binding.id(),
                        reference.access(),
                        span,
                        layout,
                        tree_layout,
                    ),
                    StoragePlacement::GlobalLexical => {
                        unsupported(UnsupportedLeafFeature::GlobalEnvironment, span)
                    }
                    StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport => {
                        unsupported(UnsupportedLeafFeature::UnsupportedBinding, span)
                    }
                }
            }
            NativeReferenceId::Unresolved(unresolved_id) => {
                self.lowered_unresolved_reference(unresolved_id, span, layout, tree_layout)
            }
        }
    }

    fn lowered_unresolved_reference(
        &self,
        unresolved: UnresolvedGlobalId,
        span: Span,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<LoweredReference, LeafCompilationError> {
        let reference = self
            .planned
            .plan
            .unresolved_globals()
            .get(unresolved.index())
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
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) {
            return unsupported(UnsupportedLeafFeature::UnresolvedReference, span);
        }
        let global = tree_layout.realm_globals.for_unresolved(unresolved).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "dynamic unresolved reference has a constructor-realm global identity",
                span: Some(span),
            },
        )?;
        let slot = tree_layout.realm_globals.closure_slot(
            &self.planned.plan,
            layout.executable,
            global,
        )?;
        Ok(LoweredReference::RealmGlobal {
            global,
            slot,
            access: reference.access(),
        })
    }

    fn lowered_realm_global_binding_reference(
        &self,
        binding: BindingId,
        access: ReferenceAccess,
        span: Span,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<LoweredReference, LeafCompilationError> {
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) {
            return unsupported(UnsupportedLeafFeature::GlobalEnvironment, span);
        }
        let global = tree_layout.realm_globals.for_binding(binding).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Program global binding has a realm-global identity",
                span: Some(span),
            },
        )?;
        let slot = tree_layout.realm_globals.closure_slot(
            &self.planned.plan,
            layout.executable,
            global,
        )?;
        Ok(LoweredReference::RealmGlobal {
            global,
            slot,
            access,
        })
    }

    pub(in crate::lowering) fn validate_lowered_mutation_reference(
        &self,
        reference: LoweredReference,
        needs_read: bool,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if !reference.access().writes() || reference.access().reads() != needs_read {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }
        if let LoweredReference::Frame { binding, .. } = reference {
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "written compiler binding exists",
                    span: Some(span),
                },
            )?;
            if storage.policy().writes() != WritePolicy::Mutable {
                return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
            }
        }
        Ok(())
    }
}
