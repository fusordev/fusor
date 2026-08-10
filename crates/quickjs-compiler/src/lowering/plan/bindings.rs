use super::super::{
    ArgumentSlot, AssignmentExpression, AssignmentOperator, AssignmentTarget, BindingId,
    BindingIdentifier, BindingPattern, BranchKind, CompilationContext, CompiledConstantPool,
    CompilerClosureBinding, ComputedMemberExpression, DestructuringBindingInitialization,
    ExecutableId, Expression, FinalOpcode, ForStatementLeft, FrameLayout, FrameSlot,
    FunctionTreeLayout, GetSpan, IdentifierReference, LeafCompilationError, LocalSlot,
    NativeReferenceId, NodeId, Operands, PlannedControlFlow, PlannedInstruction,
    PrivateFieldExpression, RealmGlobalId, ReferenceAccess, ReferenceId, ScopeId, Span,
    StaticMemberExpression, StoragePlacement, SymbolId, UnresolvedGlobalId, UnsupportedLeafFeature,
    VariableDeclaration, VariableDeclarationKind, VariableDeclarator, WritePolicy,
    anonymous_named_evaluation_span, binary_opcode, unsupported,
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
        binding: CompilerClosureBinding,
        access: ReferenceAccess,
    },
}

/// Storage source for one active Object Environment Record binding object.
/// Source `with` statements use ordinary frame bindings; direct eval imports
/// caller objects through verified external closure slots.
#[derive(Clone, Copy)]
pub(in crate::lowering) enum WithObjectSource {
    Frame(BindingId),
    DirectEval(RealmGlobalId),
}

pub(in crate::lowering) fn compact_get_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::GetArg0, Operands::NoneArg),
        1 => (FinalOpcode::GetArg1, Operands::NoneArg),
        2 => (FinalOpcode::GetArg2, Operands::NoneArg),
        3 => (FinalOpcode::GetArg3, Operands::NoneArg),
        index => (FinalOpcode::GetArg, Operands::Arg(index)),
    }
}

pub(in crate::lowering) fn compact_put_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::PutArg0, Operands::NoneArg),
        1 => (FinalOpcode::PutArg1, Operands::NoneArg),
        2 => (FinalOpcode::PutArg2, Operands::NoneArg),
        3 => (FinalOpcode::PutArg3, Operands::NoneArg),
        index => (FinalOpcode::PutArg, Operands::Arg(index)),
    }
}

pub(in crate::lowering) fn compact_set_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::SetArg0, Operands::NoneArg),
        1 => (FinalOpcode::SetArg1, Operands::NoneArg),
        2 => (FinalOpcode::SetArg2, Operands::NoneArg),
        3 => (FinalOpcode::SetArg3, Operands::NoneArg),
        index => (FinalOpcode::SetArg, Operands::Arg(index)),
    }
}

pub(in crate::lowering) fn compact_get_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
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

pub(in crate::lowering) fn compact_put_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
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

pub(in crate::lowering) fn compact_set_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
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

pub(in crate::lowering) fn compact_get_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::GetVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::GetVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::GetVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::GetVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::GetVarRef, Operands::VarRef(index)),
    }
}

pub(in crate::lowering) fn compact_put_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::PutVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::PutVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::PutVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::PutVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::PutVarRef, Operands::VarRef(index)),
    }
}

pub(in crate::lowering) fn compact_set_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::SetVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::SetVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::SetVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::SetVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::SetVarRef, Operands::VarRef(index)),
    }
}

pub(in crate::lowering) fn plan_put_slot(slot: FrameSlot, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match slot {
        FrameSlot::Argument(slot) => compact_put_argument(slot),
        FrameSlot::Local(slot) => compact_put_local(slot),
        FrameSlot::Capture(slot) => compact_put_capture(slot),
    };
    PlannedInstruction::new(opcode, operands, span)
}

pub(in crate::lowering) fn plan_external_read(
    binding: CompilerClosureBinding,
    slot: u16,
    unresolved_is_undefined: bool,
    span: Span,
) -> PlannedInstruction {
    let (opcode, operands) = match binding {
        CompilerClosureBinding::Captured(policy) if policy.has_temporal_dead_zone() => {
            (FinalOpcode::GetVarRefCheck, Operands::VarRef(slot))
        }
        CompilerClosureBinding::Captured(_) => compact_get_capture(slot),
        CompilerClosureBinding::RealmGlobal(_) if unresolved_is_undefined => {
            (FinalOpcode::GetVarUndef, Operands::VarRef(slot))
        }
        CompilerClosureBinding::RealmGlobal(_) => (FinalOpcode::GetVar, Operands::VarRef(slot)),
    };
    PlannedInstruction::new(opcode, operands, span)
}

pub(in crate::lowering) fn plan_external_put(
    binding: CompilerClosureBinding,
    slot: u16,
    span: Span,
) -> PlannedInstruction {
    let (opcode, operands) = match binding {
        CompilerClosureBinding::Captured(policy) if policy.has_temporal_dead_zone() => {
            (FinalOpcode::PutVarRefCheck, Operands::VarRef(slot))
        }
        CompilerClosureBinding::Captured(_) => compact_put_capture(slot),
        CompilerClosureBinding::RealmGlobal(_) => (FinalOpcode::PutVar, Operands::VarRef(slot)),
    };
    PlannedInstruction::new(opcode, operands, span)
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
    pub(in crate::lowering) fn plan_with_object_read(
        &self,
        source: WithObjectSource,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        match source {
            WithObjectSource::Frame(binding) => {
                let slot = layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "visible with-object binding has a frame slot",
                        span: Some(span),
                    })?;
                self.plan_read_slot(binding, slot, span)
            }
            WithObjectSource::DirectEval(global) => {
                let slot = tree_layout.realm_globals.closure_slot(
                    &self.planned.plan,
                    layout.executable,
                    global,
                )?;
                let descriptor = tree_layout.realm_globals.binding(global).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "direct-eval with object has an external-binding descriptor",
                        span: Some(span),
                    },
                )?;
                if descriptor.policy.kind() != quickjs_bytecode::CompilerBindingKind::WithObject {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "direct-eval with object retains its binding kind",
                        span: Some(span),
                    });
                }
                Ok(plan_external_read(descriptor.binding, slot, false, span))
            }
        }
    }

    pub(in crate::lowering) fn plan_identifier_read(
        &self,
        identifier: &IdentifierReference<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        unresolved_is_undefined: bool,
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
            LoweredReference::RealmGlobal { slot, binding, .. } => {
                plan_external_read(binding, slot, unresolved_is_undefined, identifier.span)
            }
        };
        let with_objects = self.with_object_sources_for_reference(
            identifier.reference_id.get(),
            identifier.span,
            tree_layout,
        )?;
        if with_objects.is_empty() {
            return flow.emit(instruction);
        }
        let done = flow.new_label(identifier.span)?;
        let atom = constants.property_atom_index(identifier.span)?;
        for source in with_objects {
            flow.emit(self.plan_with_object_read(source, layout, tree_layout, identifier.span)?)?;
            flow.with_branch(FinalOpcode::WithGetVar, atom, 1, &done, identifier.span)?;
        }
        flow.emit(instruction)?;
        flow.bind(&done)
    }

    pub(in crate::lowering) fn plan_identifier_call_reference(
        &self,
        identifier: &IdentifierReference<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
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
            LoweredReference::RealmGlobal { slot, binding, .. } => {
                plan_external_read(binding, slot, false, identifier.span)
            }
        };
        let with_objects = self.with_object_sources_for_reference(
            identifier.reference_id.get(),
            identifier.span,
            tree_layout,
        )?;
        if with_objects.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "with call-reference lowering has a visible object environment",
                span: Some(identifier.span),
            });
        }
        let done = flow.new_label(identifier.span)?;
        let atom = constants.property_atom_index(identifier.span)?;
        for source in with_objects {
            flow.emit(self.plan_with_object_read(source, layout, tree_layout, identifier.span)?)?;
            flow.with_branch(FinalOpcode::WithGetRef, atom, 1, &done, identifier.span)?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            identifier.span,
        ))?;
        flow.emit(instruction)?;
        flow.bind(&done)
    }

    pub(in crate::lowering) fn plan_identifier_value_store(
        &self,
        identifier: &IdentifierReference<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        self.validate_lowered_mutation_reference(reference, false, identifier.span)?;
        let with_objects = self.with_object_sources_for_reference(
            identifier.reference_id.get(),
            identifier.span,
            tree_layout,
        )?;
        let emit_fallback = |flow: &mut PlannedControlFlow| match reference {
            LoweredReference::Frame { binding, slot, .. } => {
                for instruction in self.plan_write_slot(binding, slot, false, identifier.span)? {
                    flow.emit(instruction)?;
                }
                Ok(())
            }
            LoweredReference::RealmGlobal { slot, binding, .. } => {
                flow.emit(plan_external_put(binding, slot, identifier.span))
            }
        };
        if with_objects.is_empty() {
            return emit_fallback(flow);
        }

        let with_reference = flow.new_label(identifier.span)?;
        let done = flow.new_label(identifier.span)?;
        let atom = constants.property_atom_index(identifier.span)?;
        for source in with_objects {
            flow.emit(self.plan_with_object_read(source, layout, tree_layout, identifier.span)?)?;
            flow.with_branch(
                FinalOpcode::WithMakeRef,
                atom,
                1,
                &with_reference,
                identifier.span,
            )?;
        }
        emit_fallback(flow)?;
        flow.branch(BranchKind::Goto, &done, identifier.span)?;
        flow.bind(&with_reference)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Rot3l,
            Operands::None,
            identifier.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PutRefValue,
            Operands::None,
            identifier.span,
        ))?;
        flow.bind(&done)
    }

    pub(in crate::lowering) fn plan_realm_global_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        slot: u16,
        binding: CompilerClosureBinding,
        inferred_name: Option<PlannedInstruction>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let read = plan_external_read(binding, slot, false, assignment.left.span());
        let write = plan_external_put(binding, slot, assignment.left.span());
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
        if storage.policy().writes() == WritePolicy::Internal {
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

    pub(in crate::lowering) fn push_lowered_reference_write<'expression>(
        &self,
        reference: LoweredReference,
        preserve_value: bool,
        span: Span,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        match reference {
            LoweredReference::Frame { binding, slot, .. } => {
                self.push_slot_write(binding, slot, preserve_value, span, work)
            }
            LoweredReference::RealmGlobal { slot, binding, .. } => {
                work.push(ExpressionWork::Emit(plan_external_put(binding, slot, span)));
                if preserve_value {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Dup,
                        Operands::None,
                        span,
                    )));
                }
                Ok(())
            }
        }
    }

    /// Resolves a `with`-visible object reference while keeping one certified
    /// object placeholder on the not-found path. Both paths merge as
    /// `[object, propertyKey, found]`, so the caller can evaluate its RHS once
    /// before selecting the object or static binding store.
    pub(in crate::lowering) fn plan_with_make_reference_selection(
        &self,
        with_objects: &[WithObjectSource],
        atom: quickjs_bytecode::AtomPoolIndex,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        span: Span,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if with_objects.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "with reference selection has a visible object environment",
                span: Some(span),
            });
        }
        let resolved = flow.new_label(span)?;
        let merged = flow.new_label(span)?;
        for (index, &source) in with_objects.iter().enumerate() {
            if index != 0 {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    span,
                ))?;
            }
            flow.emit(self.plan_with_object_read(source, layout, tree_layout, span)?)?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                span,
            ))?;
            flow.with_branch(FinalOpcode::WithMakeRef, atom, 1, &resolved, span)?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PushAtomValue,
            Operands::Atom(atom),
            span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PushFalse,
            Operands::None,
            span,
        ))?;
        flow.branch(BranchKind::Goto, &merged, span)?;
        flow.bind(&resolved)?;
        // `WithMakeRef` leaves `[placeholder, object, key]`; the duplicate
        // placeholder is the same object and can represent the resolved base.
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PushTrue,
            Operands::None,
            span,
        ))?;
        flow.bind(&merged)
    }
}

impl CompilationContext<'_, '_, '_> {
    pub(in crate::lowering) fn with_object_sources_for_reference(
        &self,
        reference_id: Option<ReferenceId>,
        span: Span,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Vec<WithObjectSource>, LeafCompilationError> {
        let reference_id = reference_id.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "identifier reference has Oxc reference identity",
            span: Some(span),
        })?;
        let mut sources = self
            .with_object_bindings_for_reference(Some(reference_id), span)?
            .into_iter()
            .map(WithObjectSource::Frame)
            .collect::<Vec<_>>();
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
        let ambient = match native {
            NativeReferenceId::Resolved(reference) => {
                let binding = self
                    .planned
                    .plan
                    .resolved_reference(reference)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "resolved compiler reference exists",
                        span: Some(span),
                    })?
                    .binding();
                tree_layout.realm_globals.with_objects_for_binding(binding)
            }
            NativeReferenceId::Unresolved(reference) => tree_layout
                .realm_globals
                .with_objects_for_unresolved(reference),
        };
        sources.extend(ambient.iter().copied().map(WithObjectSource::DirectEval));
        Ok(sources)
    }

    pub(in crate::lowering) fn with_object_sources_for_node_before_binding(
        &self,
        node: NodeId,
        binding: BindingId,
        span: Span,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Vec<WithObjectSource>, LeafCompilationError> {
        let mut sources = self
            .with_object_bindings_for_node_before_binding(node, binding, span)?
            .into_iter()
            .map(WithObjectSource::Frame)
            .collect::<Vec<_>>();
        sources.extend(
            tree_layout
                .realm_globals
                .with_objects_for_binding(binding)
                .iter()
                .copied()
                .map(WithObjectSource::DirectEval),
        );
        Ok(sources)
    }

    pub(in crate::lowering) fn with_object_binding(
        &self,
        statement: NodeId,
        span: Span,
    ) -> Result<BindingId, LeafCompilationError> {
        self.planned
            .identities
            .with_object_bindings
            .get(&statement)
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "with statement has a compiler object-environment binding",
                span: Some(span),
            })
    }

    pub(in crate::lowering) fn with_object_bindings_for_reference(
        &self,
        reference_id: Option<ReferenceId>,
        span: Span,
    ) -> Result<Vec<BindingId>, LeafCompilationError> {
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
        let stop_scope = match native {
            NativeReferenceId::Resolved(resolved) => {
                let binding = self
                    .planned
                    .plan
                    .resolved_reference(resolved)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "resolved compiler reference exists",
                        span: Some(span),
                    })?
                    .binding();
                self.planned
                    .identities
                    .scope_by_binding
                    .get(binding.index())
                    .copied()
                    .flatten()
            }
            NativeReferenceId::Unresolved(_) => None,
        };
        let scoping = self.unit.semantic().scoping();
        let reference = scoping.get_reference(reference_id);
        Ok(self.with_object_bindings_for_scope(reference.scope_id(), stop_scope))
    }

    pub(in crate::lowering) fn with_object_bindings_for_node_before_binding(
        &self,
        node: NodeId,
        binding: BindingId,
        span: Span,
    ) -> Result<Vec<BindingId>, LeafCompilationError> {
        let scope = self.unit.semantic().nodes().get_node(node).scope_id();
        let stop_scope = self
            .planned
            .identities
            .scope_by_binding
            .get(binding.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "declared binding has a retained semantic scope",
                span: Some(span),
            })?;
        Ok(self.with_object_bindings_for_scope(scope, Some(stop_scope)))
    }

    fn with_object_bindings_for_scope(
        &self,
        scope: ScopeId,
        stop_scope: Option<ScopeId>,
    ) -> Vec<BindingId> {
        let scoping = self.unit.semantic().scoping();
        let mut visible = Vec::new();
        for ancestor in scoping.scope_ancestors(scope) {
            if Some(ancestor) == stop_scope {
                break;
            }
            if let Some(&binding) = self
                .planned
                .identities
                .with_object_binding_by_scope
                .get(&ancestor)
            {
                visible.push(binding);
            }
        }
        visible
    }
}

impl<'arena> CompilationContext<'_, 'arena, '_> {
    pub(in crate::lowering) fn plan_for_in_head(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let ForStatementLeft::VariableDeclaration(declaration) = left else {
            if left.as_assignment_target().is_none() {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, left.span());
            }
            return Ok(());
        };
        let (pattern, initializer) = Self::validate_for_in_declaration(declaration)?;
        let Some(initializer) = initializer else {
            return Ok(());
        };
        let BindingPattern::BindingIdentifier(identifier) = pattern else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc permits a for-in declaration initializer only on an identifier",
                span: Some(declaration.span),
            });
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
        self.plan_expression_with_abrupt_markers(
            initializer,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
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
            self.validate_realm_global_declaration(declaration.kind, storage, identifier.span)?;
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
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let ForStatementLeft::VariableDeclaration(declaration) = left {
            let (pattern, _) = Self::validate_for_in_declaration(declaration)?;
            return match pattern {
                BindingPattern::BindingIdentifier(identifier) => self
                    .emit_for_in_declaration_write(
                        declaration.kind,
                        identifier,
                        layout,
                        tree_layout,
                        flow,
                    ),
                BindingPattern::ArrayPattern(_)
                | BindingPattern::ObjectPattern(_)
                | BindingPattern::AssignmentPattern(_) => self.plan_destructuring_pattern_value(
                    pattern,
                    DestructuringBindingInitialization::IterationDeclaration(declaration.kind),
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                ),
            };
        }

        let target =
            left.as_assignment_target()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-in non-declaration head is an assignment target",
                    span: Some(left.span()),
                })?;
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                ExpressionPlanner::new(self).plan_identifier_value_store(
                    identifier,
                    layout,
                    tree_layout,
                    constants,
                    flow,
                )?;
            }
            AssignmentTarget::StaticMemberExpression(member) if !member.optional => self
                .plan_for_in_static_member_assignment(
                    member,
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                )?,
            AssignmentTarget::ComputedMemberExpression(member) if !member.optional => self
                .plan_for_in_computed_member_assignment(
                    member,
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                )?,
            AssignmentTarget::PrivateFieldExpression(member) if !member.optional => self
                .plan_for_in_private_member_assignment(
                    member,
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                )?,
            AssignmentTarget::ArrayAssignmentTarget(_)
            | AssignmentTarget::ObjectAssignmentTarget(_) => self.plan_for_of_assignment_pattern(
                target,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            )?,
            _ => {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, target.span());
            }
        }
        Ok(())
    }

    fn plan_for_in_static_member_assignment(
        &self,
        member: &StaticMemberExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            ExpressionPlanner::new(self).plan_super_property_base(
                member.object.span(),
                false,
                layout,
                flow,
            )?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::PushAtomValue,
                Operands::Atom(constants.property_atom_index(member.property.span)?),
                member.property.span,
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Rot4l,
                Operands::None,
                member.span,
            ))?;
            return flow.emit(PlannedInstruction::new(
                FinalOpcode::PutSuperValue,
                Operands::None,
                member.span,
            ));
        }

        self.plan_expression_with_abrupt_markers(
            &member.object,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            member.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PutField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        ))
    }

    fn plan_for_in_computed_member_assignment(
        &self,
        member: &ComputedMemberExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            let planner = ExpressionPlanner::new(self);
            planner.plan_super_property_receiver(member.object.span(), false, layout, flow)?;
            self.plan_expression_with_abrupt_markers(
                &member.expression,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            )?;
            planner.plan_super_property_base_after_key(member.object.span(), layout, flow)?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::ToPropKey,
                Operands::None,
                member.expression.span(),
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Rot4l,
                Operands::None,
                member.span,
            ))?;
            return flow.emit(PlannedInstruction::new(
                FinalOpcode::PutSuperValue,
                Operands::None,
                member.span,
            ));
        }

        self.plan_expression_with_abrupt_markers(
            &member.object,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
        self.plan_expression_with_abrupt_markers(
            &member.expression,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Rot3l,
            Operands::None,
            member.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PutArrayEl,
            Operands::None,
            member.span,
        ))
    }

    fn plan_for_in_private_member_assignment(
        &self,
        member: &PrivateFieldExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let planner = ExpressionPlanner::new(self);
        let reference = planner.private_name_reference_for_access(
            member.node_id.get(),
            member.field.name.as_str(),
            member.span,
            layout,
            tree_layout,
        )?;
        self.plan_expression_with_abrupt_markers(
            &member.object,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
        flow.emit(planner.plan_private_name_read(reference, member.field.span)?)?;
        // The iteration value was produced before reference evaluation.
        // `[value, receiver, privateName] -> [receiver, privateName, value]`.
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Rot3l,
            Operands::None,
            member.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PutPrivateField,
            Operands::None,
            member.span,
        ))
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
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let ForStatementLeft::VariableDeclaration(declaration) = left {
            let pattern = Self::validate_for_of_declaration(declaration)?;
            if matches!(pattern, BindingPattern::BindingIdentifier(_)) {
                return self.plan_for_in_assignment(
                    left,
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                );
            }
            return self.plan_destructuring_pattern_value(
                pattern,
                DestructuringBindingInitialization::Declaration(declaration.kind),
                layout,
                tree_layout,
                constants,
                abrupt_markers,
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
            | AssignmentTarget::ObjectAssignmentTarget(_) => self.plan_for_of_assignment_pattern(
                target,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            ),
            AssignmentTarget::AssignmentTargetIdentifier(_)
            | AssignmentTarget::StaticMemberExpression(_)
            | AssignmentTarget::ComputedMemberExpression(_)
            | AssignmentTarget::PrivateFieldExpression(_) => self.plan_for_in_assignment(
                left,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            ),
            AssignmentTarget::TSAsExpression(_)
            | AssignmentTarget::TSSatisfiesExpression(_)
            | AssignmentTarget::TSNonNullExpression(_)
            | AssignmentTarget::TSTypeAssertion(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedExpression, target.span())
            }
        }
    }

    fn plan_for_of_assignment_pattern(
        &self,
        target: &AssignmentTarget<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let initial_abrupt_marker_count = abrupt_markers.len();
        let mut active_abrupt_markers = Vec::new();
        active_abrupt_markers
            .try_reserve_exact(initial_abrupt_marker_count)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "assignment-pattern abrupt-marker stack",
            })?;
        active_abrupt_markers.extend_from_slice(abrupt_markers);
        let mut work = Vec::new();
        self.plan_assignment_target_value(target, &mut work, flow, layout, tree_layout, constants)?;
        while let Some(task) = work.pop() {
            match task {
                ExpressionWork::Emit(instruction) => flow.emit(instruction)?,
                ExpressionWork::EnterAbruptMarker(marker) => {
                    active_abrupt_markers.try_reserve(1).map_err(|_| {
                        LeafCompilationError::CapacityExceeded {
                            domain: "assignment-pattern abrupt-marker stack",
                        }
                    })?;
                    active_abrupt_markers.push(marker);
                }
                ExpressionWork::ExitAbruptMarker { expected, span } => {
                    let Some(marker) = active_abrupt_markers.pop() else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "assignment-pattern abrupt-marker exit has an active marker",
                            span: Some(span),
                        });
                    };
                    if marker.tag() != expected {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "assignment-pattern abrupt-marker exits in LIFO order",
                            span: Some(span),
                        });
                    }
                }
                ExpressionWork::Branch { kind, target, span } => {
                    flow.branch(kind, &target, span)?;
                }
                ExpressionWork::Bind(label) => flow.bind(&label)?,
                ExpressionWork::Visit(expression) => {
                    self.plan_expression_with_abrupt_markers(
                        expression,
                        layout,
                        tree_layout,
                        constants,
                        &active_abrupt_markers,
                        flow,
                    )?;
                }
                ExpressionWork::IdentifierValueStore(identifier) => {
                    ExpressionPlanner::new(self).plan_identifier_value_store(
                        identifier,
                        layout,
                        tree_layout,
                        constants,
                        flow,
                    )?;
                }
                ExpressionWork::VisitTail(_)
                | ExpressionWork::VisitCallExpression { .. }
                | ExpressionWork::VisitOptionalChain { .. }
                | ExpressionWork::IdentifierCallReference(_)
                | ExpressionWork::IdentifierDelete { .. }
                | ExpressionWork::CallAfterCallee { .. }
                | ExpressionWork::SuperPropertyBase { .. }
                | ExpressionWork::SuperPropertyReceiver { .. }
                | ExpressionWork::SuperPropertyBaseAfterKey { .. }
                | ExpressionWork::InitializeInstanceFields { .. }
                | ExpressionWork::InitializeContextualInstanceFields { .. } => {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "assignment-target scheduling delegates complete expressions",
                        span: Some(target.span()),
                    });
                }
            }
        }
        if active_abrupt_markers.len() != initial_abrupt_marker_count {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "assignment-pattern abrupt-marker scheduling is balanced",
                span: Some(target.span()),
            });
        }
        Ok(())
    }

    fn validate_for_in_declaration<'declaration>(
        declaration: &'declaration VariableDeclaration<'arena>,
    ) -> Result<
        (
            &'declaration BindingPattern<'arena>,
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
        Ok((&declarator.id, declarator.init.as_ref()))
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
            self.validate_realm_global_declaration(declaration_kind, storage, identifier.span)?;
            let global = tree_layout
                .realm_globals
                .for_binding_reference(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-in Program var has a realm-global reference identity",
                    span: Some(identifier.span),
                })?;
            let slot = tree_layout.realm_globals.closure_slot(
                &self.planned.plan,
                layout.executable,
                global,
            )?;
            let descriptor = tree_layout.realm_globals.binding(global).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "for-in external declaration descriptor exists",
                    span: Some(identifier.span),
                },
            )?;
            return flow.emit(plan_external_put(descriptor.binding, slot, identifier.span));
        }

        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBinding,
                span: identifier.span,
            })?;
        self.validate_declaration_storage(declaration_kind, binding, slot, identifier.span)?;
        flow.emit(plan_put_slot(slot, identifier.span))
    }

    pub(in crate::lowering) fn validate_declaration(
        &self,
        declaration: &VariableDeclaration<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[super::abrupt::AbruptMarker],
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
                        abrupt_markers,
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
                        abrupt_markers,
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
                        abrupt_markers,
                        flow,
                    )?;
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
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
        abrupt_markers: &[super::abrupt::AbruptMarker],
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
            if declaration_kind == VariableDeclarationKind::Var
                && let Some(initializer) = declarator.init.as_ref()
            {
                let with_objects = self.with_object_sources_for_node_before_binding(
                    initializer.node_id(),
                    binding,
                    identifier.span,
                    tree_layout,
                )?;
                if !with_objects.is_empty() {
                    return self.plan_with_identifier_var_initializer(
                        identifier,
                        initializer,
                        binding,
                        storage,
                        &with_objects,
                        layout,
                        tree_layout,
                        constants,
                        abrupt_markers,
                        flow,
                    );
                }
            }
            if matches!(
                storage.placement(),
                StoragePlacement::GlobalObject | StoragePlacement::GlobalLexical
            ) {
                self.validate_realm_global_declaration(declaration_kind, storage, identifier.span)?;
                let initializes = match &declarator.init {
                    Some(initializer) => {
                        let set_name = self.plan_inferred_function_name_for_initializer(
                            identifier,
                            initializer,
                            constants,
                        )?;
                        self.plan_expression_with_abrupt_markers(
                            initializer,
                            layout,
                            tree_layout,
                            constants,
                            abrupt_markers,
                            flow,
                        )?;
                        if let Some(set_name) = set_name {
                            flow.emit(set_name)?;
                        }
                        true
                    }
                    None if storage.placement() == StoragePlacement::GlobalLexical
                        && declaration_kind == VariableDeclarationKind::Let =>
                    {
                        flow.emit(PlannedInstruction::new(
                            FinalOpcode::Undefined,
                            Operands::None,
                            identifier.span,
                        ))?;
                        true
                    }
                    None if storage.placement() == StoragePlacement::GlobalLexical => {
                        return unsupported(
                            UnsupportedLeafFeature::UnsupportedDeclaration,
                            declarator.span,
                        );
                    }
                    None => false,
                };
                if initializes {
                    let global = tree_layout
                        .realm_globals
                        .for_binding_reference(binding)
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "declared Program binding has a realm-global write identity",
                            span: Some(identifier.span),
                        })?;
                    let slot = tree_layout.realm_globals.closure_slot(
                        &self.planned.plan,
                        layout.executable,
                        global,
                    )?;
                    let instruction = if storage.placement() == StoragePlacement::GlobalLexical {
                        PlannedInstruction::new(
                            FinalOpcode::PutVarInit,
                            Operands::VarRef(slot),
                            identifier.span,
                        )
                    } else {
                        let descriptor = tree_layout.realm_globals.binding(global).ok_or(
                            LeafCompilationError::SemanticInvariant {
                                invariant: "declared external binding descriptor exists",
                                span: Some(identifier.span),
                            },
                        )?;
                        plan_external_put(descriptor.binding, slot, identifier.span)
                    };
                    flow.emit(instruction)?;
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
                    self.plan_expression_with_abrupt_markers(
                        initializer,
                        layout,
                        tree_layout,
                        constants,
                        abrupt_markers,
                        flow,
                    )?;
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

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "with-visible var initialization carries the declaration binding, static fallback, initializer, and whole-graph planning authorities explicitly"
    )]
    fn plan_with_identifier_var_initializer(
        &self,
        identifier: &BindingIdentifier<'arena>,
        initializer: &Expression<'arena>,
        binding: BindingId,
        storage: &crate::storage::BindingStorage,
        with_objects: &[WithObjectSource],
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[super::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let fallback = match storage.placement() {
            StoragePlacement::Argument { .. } | StoragePlacement::Local => {
                let slot = layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedBinding,
                        span: identifier.span,
                    })?;
                self.validate_declaration_storage(
                    VariableDeclarationKind::Var,
                    binding,
                    slot,
                    identifier.span,
                )?;
                ExpressionPlanner::new(self).plan_write_slot(
                    binding,
                    slot,
                    false,
                    identifier.span,
                )?
            }
            StoragePlacement::GlobalObject | StoragePlacement::GlobalLexical => {
                self.validate_realm_global_declaration(
                    VariableDeclarationKind::Var,
                    storage,
                    identifier.span,
                )?;
                let global = tree_layout
                    .realm_globals
                    .for_binding_reference(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "declared Program binding has a realm-global write identity",
                        span: Some(identifier.span),
                    })?;
                let slot = tree_layout.realm_globals.closure_slot(
                    &self.planned.plan,
                    layout.executable,
                    global,
                )?;
                let descriptor = tree_layout.realm_globals.binding(global).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "declared external binding descriptor exists",
                        span: Some(identifier.span),
                    },
                )?;
                vec![plan_external_put(descriptor.binding, slot, identifier.span)]
            }
            StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport => {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedDeclaration,
                    identifier.span,
                );
            }
        };

        let atom = constants.property_atom_index(identifier.span)?;
        ExpressionPlanner::new(self).plan_with_make_reference_selection(
            with_objects,
            atom,
            layout,
            tree_layout,
            identifier.span,
            flow,
        )?;
        self.plan_expression_with_abrupt_markers(
            initializer,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
        if let Some(set_name) =
            self.plan_inferred_function_name_for_initializer(identifier, initializer, constants)?
        {
            flow.emit(set_name)?;
        }

        let fallback_store = flow.new_label(identifier.span)?;
        let done = flow.new_label(identifier.span)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            identifier.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            identifier.span,
        ))?;
        flow.branch(BranchKind::IfFalse, &fallback_store, identifier.span)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            identifier.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PutRefValue,
            Operands::None,
            identifier.span,
        ))?;
        flow.branch(BranchKind::Goto, &done, identifier.span)?;

        flow.bind(&fallback_store)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            identifier.span,
        ))?;
        for opcode in [
            FinalOpcode::Swap,
            FinalOpcode::Drop,
            FinalOpcode::Swap,
            FinalOpcode::Drop,
        ] {
            flow.emit(PlannedInstruction::new(
                opcode,
                Operands::None,
                identifier.span,
            ))?;
        }
        for instruction in fallback {
            flow.emit(instruction)?;
        }
        flow.bind(&done)
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
                    StoragePlacement::GlobalObject | StoragePlacement::GlobalLexical => self
                        .lowered_realm_global_binding_reference(
                            binding.id(),
                            reference.access(),
                            span,
                            layout,
                            tree_layout,
                        ),
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
        if !crate::is_supported_script_compilation_goal(self.unit.goal()) {
            return unsupported(UnsupportedLeafFeature::UnresolvedReference, span);
        }
        let global = tree_layout.realm_globals.for_unresolved(unresolved).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "Script unresolved reference has an external-binding identity",
                span: Some(span),
            },
        )?;
        let slot = tree_layout.realm_globals.closure_slot(
            &self.planned.plan,
            layout.executable,
            global,
        )?;
        let binding = tree_layout.realm_globals.binding(global).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "Script external-binding descriptor exists",
                span: Some(span),
            },
        )?;
        Ok(LoweredReference::RealmGlobal {
            global,
            slot,
            binding: binding.binding,
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
        if !crate::is_supported_realm_global_binding_goal(self.unit.goal()) {
            return unsupported(UnsupportedLeafFeature::GlobalEnvironment, span);
        }
        let global = tree_layout
            .realm_globals
            .for_binding_reference(binding)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Program global reference has a realm-global identity",
                span: Some(span),
            })?;
        let slot = tree_layout.realm_globals.closure_slot(
            &self.planned.plan,
            layout.executable,
            global,
        )?;
        let descriptor = tree_layout.realm_globals.binding(global).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "Script external-binding descriptor exists",
                span: Some(span),
            },
        )?;
        Ok(LoweredReference::RealmGlobal {
            global,
            slot,
            binding: descriptor.binding,
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
        match reference {
            LoweredReference::Frame { binding, .. } => {
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "written compiler binding exists",
                        span: Some(span),
                    },
                )?;
                if storage.policy().writes() == WritePolicy::Internal {
                    return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
                }
            }
            LoweredReference::RealmGlobal { .. } => {}
        }
        Ok(())
    }
}
