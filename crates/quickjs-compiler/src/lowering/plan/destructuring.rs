use oxc_ast::ast::VariableDeclarationKind;

use super::super::{
    ArrayAssignmentTarget, ArrayPattern, AssignmentTarget, AssignmentTargetMaybeDefault,
    AssignmentTargetProperty, AssignmentTargetRest, AtomPoolIndex, BindingIdentifier,
    BindingPattern, BindingRestElement, BranchKind, CompilationContext, CompiledConstantPool,
    CompiledMetadataAtomKey, DeclarationKind, Expression, ExpressionPlanner, ExpressionWork,
    FinalOpcode, FrameLayout, FunctionTreeLayout, GetSpan, IdentifierReference,
    InitializationPolicy, LeafCompilationError, LoweredReference, ObjectAssignmentTarget,
    ObjectPattern, Operands, PlannedControlFlow, PlannedInstruction, Span, StoragePlacement,
    UnsupportedLeafFeature, WritePolicy, anonymous_class_expression_span,
    anonymous_named_evaluation_span, anonymous_ordinary_function_span, plan_put_slot, unsupported,
};
use super::abrupt::{AbruptMarker, AbruptMarkerKind};

#[derive(Clone, Copy)]
pub(in crate::lowering) enum DestructuringBindingInitialization {
    Declaration(VariableDeclarationKind),
    Parameter,
}

impl<'arena> CompilationContext<'_, 'arena, '_> {
    #[allow(
        clippy::too_many_arguments,
        reason = "array-pattern declaration planning carries the same explicit frame, tree, constant, and flow authority as every other declaration form"
    )]
    pub(in crate::lowering) fn plan_array_destructuring_declaration<'pattern, 'expression>(
        &self,
        pattern: &'pattern ArrayPattern<'arena>,
        initializer: &'expression Expression<'arena>,
        declaration_kind: VariableDeclarationKind,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        self.plan_expression_with_abrupt_markers(
            initializer,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
        self.plan_array_destructuring_value(
            pattern,
            DestructuringBindingInitialization::Declaration(declaration_kind),
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )
    }

    /// Destructures an iterable value already on the stack through an array
    /// pattern: `for_of_start`, then every element, elision, default, and
    /// rest, then `iterator_close`. The value is consumed and the verified
    /// record (possibly under an outer pattern's record) remains.
    #[allow(
        clippy::too_many_arguments,
        reason = "array-pattern value destructuring carries the same explicit frame, tree, constant, and flow authority as every other pattern form"
    )]
    fn plan_array_destructuring_value<'pattern>(
        &self,
        pattern: &'pattern ArrayPattern<'arena>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        self.plan_array_destructuring_elements(
            pattern.elements.iter(),
            pattern.rest.as_deref(),
            binding_initialization,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
            pattern.span,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "object-pattern declaration planning carries the same explicit frame, tree, constant, and flow authority as every other declaration form"
    )]
    pub(in crate::lowering) fn plan_object_destructuring_declaration<'pattern, 'expression>(
        &self,
        pattern: &'pattern ObjectPattern<'arena>,
        initializer: &'expression Expression<'arena>,
        declaration_kind: VariableDeclarationKind,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        self.plan_expression_with_abrupt_markers(
            initializer,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )?;
        self.plan_object_destructuring_value(
            pattern,
            DestructuringBindingInitialization::Declaration(declaration_kind),
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )
    }

    /// Packs the three `copy_data_properties` operand depths into the single
    /// U8: bits 0-1 are the target depth, bits 2-4 the source depth, and bits
    /// 5-7 the excluded-object depth, each measured from the stack top with
    /// the freshly allocated target at depth zero.
    const fn copy_data_properties_offsets(target: u8, source: u8, excluded: u8) -> u8 {
        target | (source << 2) | (excluded << 5)
    }

    /// Destructures an on-stack value through an object pattern: `to_object`,
    /// then one `get_field2`/`get_array_el2` per property (the source object
    /// stays below the read value), then `drop` the source. With object rest
    /// (`{...rest}`) the pinned exclude list is created below the source
    /// (`object; swap`), every destructured key is recorded in it, and the
    /// remaining own enumerable string properties are copied into a fresh
    /// target with `copy_data_properties` before both source and list drop.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "object-pattern value destructuring carries the same explicit frame, tree, constant, and flow authority as every other pattern form; the exclude-list program and copy-data-properties tail extend the same single transaction"
    )]
    fn plan_object_destructuring_value<'pattern>(
        &self,
        pattern: &'pattern ObjectPattern<'arena>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let has_rest = pattern.rest.is_some();
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ToObject,
            Operands::None,
            pattern.span,
        ))?;
        if has_rest {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Object,
                Operands::None,
                pattern.span,
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                pattern.span,
            ))?;
        }
        for property in &pattern.properties {
            let span = property.span;
            if property.computed {
                let key = property.key.as_expression().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "computed object-pattern key is an expression",
                        span: Some(span),
                    },
                )?;
                self.plan_expression_with_abrupt_markers(
                    key,
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                )?;
                if has_rest {
                    // Record the key in the exclude list below the source.
                    // The verifier's object-definition pass converts the key
                    // while the exclude list is directly below it, so the
                    // pinned `perm3` rotation runs before the single
                    // `to_propkey` (observably identical: the same value is
                    // converted exactly once).
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Perm3,
                        Operands::None,
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::ToPropKey,
                        Operands::None,
                        key.span(),
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Null,
                        Operands::None,
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::DefineArrayEl,
                        Operands::None,
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Perm3,
                        Operands::None,
                        span,
                    ))?;
                } else {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::ToPropKey,
                        Operands::None,
                        key.span(),
                    ))?;
                }
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::GetArrayEl2,
                    Operands::None,
                    span,
                ))?;
            } else {
                if has_rest {
                    // Record the key in the exclude list: [excludeList, source]
                    // -> swap -> [source, excludeList] -> define_field ->
                    // [source, excludeList] -> swap -> [excludeList, source].
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Swap,
                        Operands::None,
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Null,
                        Operands::None,
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::DefineField,
                        Operands::Atom(constants.property_atom_index(property.key.span())?),
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Swap,
                        Operands::None,
                        span,
                    ))?;
                }
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::GetField2,
                    Operands::Atom(constants.property_atom_index(property.key.span())?),
                    span,
                ))?;
            }
            self.plan_destructuring_pattern_value(
                &property.value,
                binding_initialization,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            )?;
        }
        if let Some(rest) = pattern.rest.as_deref() {
            // [excludeList, source] -> target -> copy -> bind -> drops.
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Object,
                Operands::None,
                rest.span,
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::CopyDataProperties,
                Operands::U8(Self::copy_data_properties_offsets(0, 1, 2)),
                rest.span,
            ))?;
            self.plan_destructuring_pattern_value(
                &rest.argument,
                binding_initialization,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            )?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                pattern.span,
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                pattern.span,
            ))?;
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                pattern.span,
            ))?;
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "array-pattern element planning carries the same explicit frame, tree, constant, and flow authority as every other pattern form"
    )]
    fn plan_array_destructuring_elements<'pattern>(
        &self,
        elements: impl ExactSizeIterator<Item = &'pattern Option<BindingPattern<'arena>>>,
        rest: Option<&'pattern BindingRestElement<'arena>>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ForOfStart,
            Operands::None,
            span,
        ))?;
        let mut element_abrupt_markers = Vec::new();
        element_abrupt_markers
            .try_reserve_exact(abrupt_markers.len().saturating_add(1))
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "destructuring abrupt-marker stack",
            })?;
        element_abrupt_markers.extend_from_slice(abrupt_markers);
        element_abrupt_markers.push(AbruptMarker::new(AbruptMarkerKind::ForOf, 0));
        for element in elements {
            match element {
                None => {
                    // An elision consumes one iterator value and discards
                    // both the value and the done flag.
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::ForOfNext,
                        Operands::U8(0),
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        span,
                    ))?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        span,
                    ))?;
                }
                Some(pattern) => {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::ForOfNext,
                        Operands::U8(0),
                        pattern.span(),
                    ))?;
                    // Discard the done flag, leaving the value on top.
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        pattern.span(),
                    ))?;
                    self.plan_destructuring_pattern_value(
                        pattern,
                        binding_initialization,
                        layout,
                        tree_layout,
                        constants,
                        &element_abrupt_markers,
                        flow,
                    )?;
                }
            }
        }
        if let Some(rest) = rest {
            self.plan_destructuring_rest(
                rest,
                binding_initialization,
                layout,
                tree_layout,
                constants,
                &element_abrupt_markers,
                flow,
            )?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::IteratorClose,
            Operands::None,
            span,
        ))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "rest binding keeps iterator cleanup markers beside the existing explicit lowering authorities"
    )]
    fn plan_destructuring_rest<'pattern>(
        &self,
        rest: &'pattern BindingRestElement<'arena>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        // `[first, ...rest]`: collect the remaining values into a fresh
        // array using the pinned `array_from; push index; for_of_next;
        // define_array_el; inc` loop. The record created by `for_of_start`
        // stays three slots below the fresh array and its cursor, so every
        // loop `for_of_next` addresses it with temporary offset 2 (the
        // verifier certifies that the two slots above the record are
        // ordinary JavaScript values). The `if_false` branch uses the
        // certified for-of iteration shape: the body label receives the
        // head value, and the fallthrough is the exhausted exit.
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ArrayFrom,
            Operands::NPop { argument_count: 0 },
            rest.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push0,
            Operands::NoneInt,
            rest.span,
        ))?;
        let next = flow.new_label(rest.span)?;
        let body = flow.new_label(rest.span)?;
        let done = flow.new_label(rest.span)?;
        flow.bind(&next)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ForOfNext,
            Operands::U8(2),
            rest.span,
        ))?;
        flow.branch(BranchKind::IfFalse, &body, rest.span)?;
        flow.branch(BranchKind::Goto, &done, rest.span)?;
        flow.bind(&body)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefineArrayEl,
            Operands::None,
            rest.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Inc,
            Operands::None,
            rest.span,
        ))?;
        flow.branch(BranchKind::Goto, &next, rest.span)?;
        flow.bind(&done)?;
        // The exhausted exit carries the final `undefined` value and the
        // cursor above the fresh array; both are dropped before the array
        // is stored into the rest binding.
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            rest.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            rest.span,
        ))?;
        self.plan_destructuring_pattern_value(
            &rest.argument,
            binding_initialization,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "destructuring pattern value storage carries the same explicit frame, tree, constant, and flow authority as every other declaration form"
    )]
    pub(in crate::lowering) fn plan_destructuring_pattern_value<'pattern>(
        &self,
        pattern: &'pattern BindingPattern<'arena>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        match pattern {
            BindingPattern::BindingIdentifier(identifier) => self
                .plan_destructuring_binding_identifier(
                    identifier,
                    binding_initialization,
                    layout,
                    tree_layout,
                    flow,
                ),
            BindingPattern::AssignmentPattern(assignment) => {
                // `[a = default]`: when the destructured value is `undefined`,
                // evaluate the default expression and store it instead.
                let skip = flow.new_label(assignment.span)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Undefined,
                    Operands::None,
                    assignment.span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::StrictEq,
                    Operands::None,
                    assignment.span,
                ))?;
                flow.branch(BranchKind::IfFalse, &skip, assignment.span)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    assignment.span,
                ))?;
                let inferred_name = match &assignment.left {
                    BindingPattern::BindingIdentifier(identifier) => self
                        .plan_inferred_function_name_for_initializer(
                            identifier,
                            &assignment.right,
                            constants,
                        )?,
                    BindingPattern::AssignmentPattern(_)
                    | BindingPattern::ArrayPattern(_)
                    | BindingPattern::ObjectPattern(_) => None,
                };
                self.plan_expression_with_abrupt_markers(
                    &assignment.right,
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                )?;
                if let Some(set_name) = inferred_name {
                    flow.emit(set_name)?;
                }
                flow.bind(&skip)?;
                self.plan_destructuring_pattern_value(
                    &assignment.left,
                    binding_initialization,
                    layout,
                    tree_layout,
                    constants,
                    abrupt_markers,
                    flow,
                )
            }
            BindingPattern::ArrayPattern(pattern) => self.plan_array_destructuring_value(
                pattern,
                binding_initialization,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            ),
            BindingPattern::ObjectPattern(pattern) => self.plan_object_destructuring_value(
                pattern,
                binding_initialization,
                layout,
                tree_layout,
                constants,
                abrupt_markers,
                flow,
            ),
        }
    }

    fn plan_destructuring_binding_identifier(
        &self,
        identifier: &BindingIdentifier<'arena>,
        binding_initialization: DestructuringBindingInitialization,
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
                    invariant: "destructured compiler binding exists",
                    span: Some(identifier.span),
                })?;
        if matches!(
            storage.placement(),
            StoragePlacement::GlobalObject | StoragePlacement::GlobalLexical
        ) {
            let DestructuringBindingInitialization::Declaration(declaration_kind) =
                binding_initialization
            else {
                return unsupported(UnsupportedLeafFeature::UnsupportedBinding, identifier.span);
            };
            self.validate_realm_global_declaration(declaration_kind, storage, identifier.span)?;
            let global = tree_layout.realm_globals.for_binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "destructured Program binding has a realm-global identity",
                    span: Some(identifier.span),
                },
            )?;
            let slot = tree_layout.realm_globals.closure_slot(
                &self.planned.plan,
                layout.executable,
                global,
            )?;
            let opcode = if storage.placement() == StoragePlacement::GlobalLexical {
                FinalOpcode::PutVarInit
            } else {
                FinalOpcode::PutVar
            };
            return flow.emit(PlannedInstruction::new(
                opcode,
                Operands::VarRef(slot),
                identifier.span,
            ));
        }
        let frame_slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBinding,
                span: identifier.span,
            })?;
        match binding_initialization {
            DestructuringBindingInitialization::Declaration(declaration_kind) => {
                self.validate_declaration_storage(
                    declaration_kind,
                    binding,
                    frame_slot,
                    identifier.span,
                )?;
                flow.emit(plan_put_slot(frame_slot, identifier.span))
            }
            DestructuringBindingInitialization::Parameter => {
                if storage.placement() != StoragePlacement::Local
                    || storage.policy().writes() != WritePolicy::Mutable
                {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedBinding,
                        identifier.span,
                    );
                }
                match (storage.policy().kind(), storage.policy().initialization()) {
                    (DeclarationKind::Parameter, InitializationPolicy::Argument) => {
                        flow.emit(plan_put_slot(frame_slot, identifier.span))
                    }
                    (DeclarationKind::Function, InitializationPolicy::FunctionAtScopeEntry) => flow
                        .emit(PlannedInstruction::new(
                            FinalOpcode::Drop,
                            Operands::None,
                            identifier.span,
                        )),
                    _ => unsupported(UnsupportedLeafFeature::UnsupportedBinding, identifier.span),
                }
            }
        }
    }

    fn plan_inferred_function_name(
        &self,
        identifier: &BindingIdentifier<'arena>,
        constants: &CompiledConstantPool,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let binding = self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
        Ok(PlannedInstruction::new(
            FinalOpcode::SetName,
            Operands::Atom(
                constants.metadata_atom_index(CompiledMetadataAtomKey::Binding(binding))?,
            ),
            span,
        ))
    }

    pub(in crate::lowering) fn plan_inferred_function_name_for_initializer(
        &self,
        identifier: &BindingIdentifier<'arena>,
        initializer: &Expression<'arena>,
        constants: &CompiledConstantPool,
    ) -> Result<Option<PlannedInstruction>, LeafCompilationError> {
        let Some(span) = anonymous_named_evaluation_span(initializer) else {
            return Ok(None);
        };
        if anonymous_class_expression_span(initializer).is_some() {
            return Ok(None);
        }
        if anonymous_ordinary_function_span(initializer).is_none() {
            return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
        }
        self.plan_inferred_function_name(identifier, constants, span)
            .map(Some)
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "array-pattern assignment planning carries the same explicit frame, tree, and flow authority as every other pattern form"
    )]
    pub(in crate::lowering) fn plan_array_assignment_elements<'pattern>(
        &self,
        pattern: &'pattern ArrayAssignmentTarget<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'pattern, 'arena>>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<(), LeafCompilationError> {
        // Work is a LIFO stack; the destructuring sequence runs after the
        // caller's `dup` of the RHS, so `iterator_close` is pushed first and
        // `for_of_start` last. The rest collector (when present) runs after
        // every element and before the close.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::IteratorClose,
            Operands::None,
            pattern.span,
        )));
        if let Some(rest) = pattern.rest.as_deref() {
            self.plan_assignment_rest_collection(rest, work, flow, layout, tree_layout, constants)?;
        }
        for element in pattern.elements.iter().rev() {
            match element {
                None => {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        pattern.span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        pattern.span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::ForOfNext,
                        Operands::U8(0),
                        pattern.span,
                    )));
                }
                Some(element) => {
                    // Member-expression targets evaluate their base (and
                    // computed key) before the step so the reference sits
                    // below the destructured value; the `for_of_next`
                    // temporary offset is exactly that reference depth. The
                    // reference prelude is therefore pushed last (executes
                    // first), the store machinery first (executes last).
                    let target = Self::assignment_element_target(element).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "assignment target element is an assignment target",
                            span: Some(element.span()),
                        },
                    )?;
                    let depth = Self::assignment_target_depth(target)?;
                    if let AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(default) =
                        element
                    {
                        // `[a = default]` assignment: when the
                        // destructured value is `undefined`, evaluate the
                        // default expression and store it instead.
                        let skip = flow.new_label(default.span)?;
                        let inferred_name = ExpressionPlanner::new(self)
                            .plan_inferred_assignment_target_name_for_initializer(
                                &default.binding,
                                &default.init,
                                layout,
                                tree_layout,
                                constants,
                            )?;
                        self.plan_assignment_target_value(
                            &default.binding,
                            work,
                            flow,
                            layout,
                            tree_layout,
                            constants,
                        )?;
                        work.push(ExpressionWork::Bind(skip.clone()));
                        if let Some(set_name) = inferred_name {
                            work.push(ExpressionWork::Emit(set_name));
                        }
                        work.push(ExpressionWork::Visit(&default.init));
                        work.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::Drop,
                            Operands::None,
                            default.span,
                        )));
                        work.push(ExpressionWork::Branch {
                            kind: BranchKind::IfFalse,
                            target: skip,
                            span: default.span,
                        });
                        work.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::StrictEq,
                            Operands::None,
                            default.span,
                        )));
                        work.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::Undefined,
                            Operands::None,
                            default.span,
                        )));
                        work.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::Dup,
                            Operands::None,
                            default.span,
                        )));
                    } else {
                        self.plan_assignment_target_value(
                            target,
                            work,
                            flow,
                            layout,
                            tree_layout,
                            constants,
                        )?;
                    }
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        element.span(),
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::ForOfNext,
                        Operands::U8(depth),
                        element.span(),
                    )));
                    Self::plan_assignment_target_prelude(target, constants, work)?;
                }
            }
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForOfStart,
            Operands::None,
            pattern.span,
        )));
        Ok(())
    }

    pub(in crate::lowering) fn plan_assignment_target_value<'pattern>(
        &self,
        target: &'pattern AssignmentTarget<'arena>,
        work: &mut Vec<ExpressionWork<'pattern, 'arena>>,
        flow: &mut PlannedControlFlow,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<(), LeafCompilationError> {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                self.plan_assignment_identifier_store(identifier, work, layout, tree_layout)
            }
            AssignmentTarget::StaticMemberExpression(member) if !member.optional => {
                // The base is evaluated by the reference prelude before the
                // `for_of_next` step; the store consumes it with the value.
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::PutField,
                    Operands::Atom(constants.property_atom_index(member.property.span)?),
                    member.span,
                )));
                Ok(())
            }
            AssignmentTarget::ComputedMemberExpression(member) if !member.optional => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::PutArrayEl,
                    Operands::None,
                    member.span,
                )));
                Ok(())
            }
            AssignmentTarget::ArrayAssignmentTarget(pattern) => {
                // `[a, [b]] = expr`: destructure the on-stack value with a
                // nested iterator; the sequence emits `for_of_start`, the
                // nested elements, and `iterator_close` around the target
                // stores.
                self.plan_array_assignment_elements(
                    pattern,
                    flow,
                    work,
                    layout,
                    tree_layout,
                    constants,
                )
            }
            AssignmentTarget::ObjectAssignmentTarget(pattern) => {
                // `{a, ...rest} = expr`: destructure the on-stack value with
                // the object read shape and the exclude-list rest collector.
                self.plan_object_assignment_value(
                    pattern,
                    work,
                    flow,
                    layout,
                    tree_layout,
                    constants,
                )
            }
            AssignmentTarget::StaticMemberExpression(_)
            | AssignmentTarget::ComputedMemberExpression(_)
            | AssignmentTarget::TSAsExpression(_)
            | AssignmentTarget::TSSatisfiesExpression(_)
            | AssignmentTarget::TSNonNullExpression(_)
            | AssignmentTarget::TSTypeAssertion(_)
            | AssignmentTarget::PrivateFieldExpression(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedPattern, target.span())
            }
        }
    }

    /// Stores an already-evaluated value into an identifier reference target.
    fn plan_assignment_identifier_store<'pattern>(
        &self,
        identifier: &'pattern IdentifierReference<'arena>,
        work: &mut Vec<ExpressionWork<'pattern, 'arena>>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<(), LeafCompilationError> {
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        if !reference.access().writes() {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedReference,
                identifier.span,
            );
        }
        match reference {
            LoweredReference::Frame { slot, .. } => {
                work.push(ExpressionWork::Emit(plan_put_slot(slot, identifier.span)));
            }
            LoweredReference::RealmGlobal { slot, .. } => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::PutVar,
                    Operands::VarRef(slot),
                    identifier.span,
                )));
            }
        }
        Ok(())
    }

    /// Pushes the `value === undefined` default machinery: when the value is
    /// `undefined`, drop it and evaluate the initializer. Executes after the
    /// property read and before the store.
    fn push_object_property_default<'expression>(
        init: &'expression Expression<'arena>,
        inferred_name: Option<PlannedInstruction>,
        span: Span,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let skip = flow.new_label(span)?;
        work.push(ExpressionWork::Bind(skip.clone()));
        if let Some(set_name) = inferred_name {
            work.push(ExpressionWork::Emit(set_name));
        }
        work.push(ExpressionWork::Visit(init));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::IfFalse,
            target: skip,
            span,
        });
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::StrictEq,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            span,
        )));
        Ok(())
    }

    /// Destructures an on-stack value through an object assignment pattern:
    /// `to_object`, one `get_field2`/`get_array_el2` per property, then
    /// `drop` the source. Property targets are identifier stores, member
    /// stores, or nested patterns. A member target evaluates its base (and
    /// computed key) after the property read and rotates the reference below
    /// the fetched value with the pinned `perm3`/`swap` shape before the
    /// `put_field`/`put_array_el` store. Object rest (`{...rest}`) creates
    /// the exclude list below the source right after conversion, records
    /// every destructured key in it, and copies the remaining own enumerable
    /// string properties into a fresh target before both list and source
    /// drop. The original RHS copy stays below the whole sequence and
    /// remains the assignment expression's value.
    #[allow(
        clippy::too_many_lines,
        reason = "object-pattern assignment stays one LIFO work-list transaction; member references and the exclude-list recording extend the same push sequence"
    )]
    pub(in crate::lowering) fn plan_object_assignment_value<'pattern>(
        &self,
        pattern: &'pattern ObjectAssignmentTarget<'arena>,
        work: &mut Vec<ExpressionWork<'pattern, 'arena>>,
        flow: &mut PlannedControlFlow,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<(), LeafCompilationError> {
        let has_rest = pattern.rest.is_some();
        // Work is a LIFO stack: the source drop runs last and `to_object`
        // first; each property pushes its store, its default machinery, the
        // property read, and any computed-key visits in reverse order. With
        // object rest the exclude list sits below the converted source, so
        // the recording rotations never touch the retained RHS copy.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            pattern.span,
        )));
        if let Some(rest) = pattern.rest.as_deref() {
            // [rhsCopy, excludeList, source] -> target -> copy -> assign ->
            // drop source -> drop exclude list.
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                rest.span,
            )));
            self.plan_assignment_target_value(
                &rest.target,
                work,
                flow,
                layout,
                tree_layout,
                constants,
            )?;
            Self::push_object_assignment_member_prelude(&rest.target, constants, work)?;
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::CopyDataProperties,
                Operands::U8(Self::copy_data_properties_offsets(0, 1, 2)),
                rest.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Object,
                Operands::None,
                rest.span,
            )));
        }
        for property in pattern.properties.iter().rev() {
            match property {
                AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(identifier) => {
                    // `{a}` or `{a = default}`: the key is the binding name.
                    let inferred_name = identifier
                        .init
                        .as_ref()
                        .map(|init| {
                            ExpressionPlanner::new(self)
                                .plan_inferred_identifier_reference_name_for_initializer(
                                    &identifier.binding,
                                    init,
                                    layout,
                                    tree_layout,
                                    constants,
                                )
                        })
                        .transpose()?
                        .flatten();
                    self.plan_assignment_identifier_store(
                        &identifier.binding,
                        work,
                        layout,
                        tree_layout,
                    )?;
                    if let Some(init) = &identifier.init {
                        Self::push_object_property_default(
                            init,
                            inferred_name,
                            identifier.span,
                            flow,
                            work,
                        )?;
                    }
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetField2,
                        Operands::Atom(constants.property_atom_index(identifier.binding.span)?),
                        identifier.span,
                    )));
                    if has_rest {
                        Self::push_object_rest_static_key_record(
                            constants.property_atom_index(identifier.binding.span)?,
                            identifier.span,
                            work,
                        );
                    }
                }
                AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
                    let target = Self::assignment_element_target(&property.binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "object assignment property binding is an assignment target",
                            span: Some(property.span),
                        },
                    )?;
                    match target {
                        AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                            self.plan_assignment_identifier_store(
                                identifier,
                                work,
                                layout,
                                tree_layout,
                            )?;
                        }
                        AssignmentTarget::ArrayAssignmentTarget(_)
                        | AssignmentTarget::ObjectAssignmentTarget(_) => {
                            self.plan_assignment_target_value(
                                target,
                                work,
                                flow,
                                layout,
                                tree_layout,
                                constants,
                            )?;
                        }
                        AssignmentTarget::StaticMemberExpression(_)
                        | AssignmentTarget::ComputedMemberExpression(_) => {
                            self.plan_assignment_target_value(
                                target,
                                work,
                                flow,
                                layout,
                                tree_layout,
                                constants,
                            )?;
                            Self::push_object_assignment_member_prelude(target, constants, work)?;
                        }
                        AssignmentTarget::TSAsExpression(_)
                        | AssignmentTarget::TSSatisfiesExpression(_)
                        | AssignmentTarget::TSNonNullExpression(_)
                        | AssignmentTarget::TSTypeAssertion(_)
                        | AssignmentTarget::PrivateFieldExpression(_) => {
                            return unsupported(
                                UnsupportedLeafFeature::UnsupportedPattern,
                                property.span,
                            );
                        }
                    }
                    if let AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(default) =
                        &property.binding
                    {
                        let inferred_name = ExpressionPlanner::new(self)
                            .plan_inferred_assignment_target_name_for_initializer(
                                &default.binding,
                                &default.init,
                                layout,
                                tree_layout,
                                constants,
                            )?;
                        Self::push_object_property_default(
                            &default.init,
                            inferred_name,
                            default.span,
                            flow,
                            work,
                        )?;
                    }
                    if property.computed {
                        let key = property.name.as_expression().ok_or(
                            LeafCompilationError::SemanticInvariant {
                                invariant: "computed object assignment key is an expression",
                                span: Some(property.span),
                            },
                        )?;
                        work.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::GetArrayEl2,
                            Operands::None,
                            property.span,
                        )));
                        if has_rest {
                            Self::push_object_rest_computed_key_record(property.span, work);
                        } else {
                            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                FinalOpcode::ToPropKey,
                                Operands::None,
                                key.span(),
                            )));
                        }
                        work.push(ExpressionWork::Visit(key));
                    } else {
                        work.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::GetField2,
                            Operands::Atom(constants.property_atom_index(property.name.span())?),
                            property.span,
                        )));
                        if has_rest {
                            Self::push_object_rest_static_key_record(
                                constants.property_atom_index(property.name.span())?,
                                property.span,
                                work,
                            );
                        }
                    }
                }
            }
        }
        if has_rest {
            // [rhsCopy, source] -> object -> swap -> [rhsCopy, excludeList,
            // source]: the exclude list is created below the converted source
            // immediately after conversion. Pushed before `to_object` so the
            // LIFO work list runs the conversion first.
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                pattern.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Object,
                Operands::None,
                pattern.span,
            )));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ToObject,
            Operands::None,
            pattern.span,
        )));
        Ok(())
    }

    /// Records a static property key in the object-rest exclude list. The
    /// list sits directly below the source, so the pinned rotation is
    /// `[excludeList, source] -> swap -> [source, excludeList] -> null ->
    /// define_field -> [source, excludeList] -> swap -> [excludeList,
    /// source]`, exactly as the declaration path emits. Pushed in reverse
    /// so it runs before the property read.
    fn push_object_rest_static_key_record(
        atom: AtomPoolIndex,
        span: Span,
        work: &mut Vec<ExpressionWork<'_, '_>>,
    ) {
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::DefineField,
            Operands::Atom(atom),
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Null,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            span,
        )));
    }

    /// Records a computed property key in the object-rest exclude list:
    /// `[excludeList, source, key] -> perm3 -> [source, excludeList, key] ->
    /// to_propkey -> null -> define_array_el -> [source, excludeList, key]
    /// -> perm3 -> [excludeList, key, source]`, converting the key exactly
    /// once before `get_array_el2` reads from the source. Pushed in reverse
    /// so it runs between the key evaluation and the read.
    fn push_object_rest_computed_key_record(span: Span, work: &mut Vec<ExpressionWork<'_, '_>>) {
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Perm3,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::DefineArrayEl,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Null,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ToPropKey,
            Operands::None,
            span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Perm3,
            Operands::None,
            span,
        )));
    }

    /// Evaluates the member base and computed key of an object-assignment
    /// target after the fetched value and rotates the reference below it:
    /// static targets run `visit(base); swap`, computed targets run
    /// `visit(base); visit(key); to_propkey; perm3; swap`, leaving
    /// `[source, base, value]` / `[source, base, key, value]` for the
    /// pinned `put_field`/`put_array_el` store.
    fn push_object_assignment_member_prelude<'pattern>(
        target: &'pattern AssignmentTarget<'arena>,
        _constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'pattern, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        match target {
            AssignmentTarget::StaticMemberExpression(member) if !member.optional => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    member.span,
                )));
                work.push(ExpressionWork::Visit(&member.object));
                Ok(())
            }
            AssignmentTarget::ComputedMemberExpression(member) if !member.optional => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    member.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Perm3,
                    Operands::None,
                    member.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::ToPropKey,
                    Operands::None,
                    member.expression.span(),
                )));
                work.push(ExpressionWork::Visit(&member.expression));
                work.push(ExpressionWork::Visit(&member.object));
                Ok(())
            }
            AssignmentTarget::AssignmentTargetIdentifier(_)
            | AssignmentTarget::ArrayAssignmentTarget(_)
            | AssignmentTarget::ObjectAssignmentTarget(_) => Ok(()),
            AssignmentTarget::StaticMemberExpression(_)
            | AssignmentTarget::ComputedMemberExpression(_)
            | AssignmentTarget::TSAsExpression(_)
            | AssignmentTarget::TSSatisfiesExpression(_)
            | AssignmentTarget::TSNonNullExpression(_)
            | AssignmentTarget::TSTypeAssertion(_)
            | AssignmentTarget::PrivateFieldExpression(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedPattern, target.span())
            }
        }
    }

    /// Returns the assignment target underlying an element (skipping any
    /// default wrapper).
    fn assignment_element_target<'pattern>(
        element: &'pattern AssignmentTargetMaybeDefault<'arena>,
    ) -> Option<&'pattern AssignmentTarget<'arena>> {
        match element {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(default) => {
                Some(&default.binding)
            }
            _ => element.as_assignment_target(),
        }
    }

    /// Returns the number of reference slots a destructuring assignment
    /// target keeps below the incoming value: identifiers and nested array
    /// patterns keep none, static members keep their base, and computed
    /// members keep their base and key.
    fn assignment_target_depth(
        target: &AssignmentTarget<'arena>,
    ) -> Result<u8, LeafCompilationError> {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(_)
            | AssignmentTarget::ArrayAssignmentTarget(_)
            | AssignmentTarget::ObjectAssignmentTarget(_) => Ok(0),
            AssignmentTarget::StaticMemberExpression(member) if !member.optional => Ok(1),
            AssignmentTarget::ComputedMemberExpression(member) if !member.optional => Ok(2),
            AssignmentTarget::StaticMemberExpression(_)
            | AssignmentTarget::ComputedMemberExpression(_)
            | AssignmentTarget::TSAsExpression(_)
            | AssignmentTarget::TSSatisfiesExpression(_)
            | AssignmentTarget::TSNonNullExpression(_)
            | AssignmentTarget::TSTypeAssertion(_)
            | AssignmentTarget::PrivateFieldExpression(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedPattern, target.span())
            }
        }
    }

    /// Evaluates the member base and computed key of a destructuring
    /// assignment target below the incoming value. Pushed last so it runs
    /// before the element's `for_of_next` step.
    fn plan_assignment_target_prelude<'pattern>(
        target: &'pattern AssignmentTarget<'arena>,
        _constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'pattern, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(_)
            | AssignmentTarget::ArrayAssignmentTarget(_)
            | AssignmentTarget::ObjectAssignmentTarget(_) => Ok(()),
            AssignmentTarget::StaticMemberExpression(member) if !member.optional => {
                work.push(ExpressionWork::Visit(&member.object));
                Ok(())
            }
            AssignmentTarget::ComputedMemberExpression(member) if !member.optional => {
                work.push(ExpressionWork::Visit(&member.expression));
                work.push(ExpressionWork::Visit(&member.object));
                Ok(())
            }
            AssignmentTarget::StaticMemberExpression(_)
            | AssignmentTarget::ComputedMemberExpression(_)
            | AssignmentTarget::TSAsExpression(_)
            | AssignmentTarget::TSSatisfiesExpression(_)
            | AssignmentTarget::TSNonNullExpression(_)
            | AssignmentTarget::TSTypeAssertion(_)
            | AssignmentTarget::PrivateFieldExpression(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedPattern, target.span())
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "array-pattern rest assignment carries the same explicit frame, tree, and flow authority as every other pattern form"
    )]
    fn plan_assignment_rest_collection<'pattern>(
        &self,
        rest: &'pattern AssignmentTargetRest<'arena>,
        work: &mut Vec<ExpressionWork<'pattern, 'arena>>,
        flow: &mut PlannedControlFlow,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<(), LeafCompilationError> {
        // `[first, ...rest] = expr`: collect the remaining values into a
        // fresh array with the pinned `array_from; push index; for_of_next;
        // define_array_el; inc` loop. The retained RHS copy and the record
        // sit below the fresh array and cursor, so the loop `for_of_next`
        // addresses the record with temporary offset 2; the `if_false`
        // branch uses the certified for-of iteration shape.
        //
        // Work is a LIFO stack, so each item is pushed in reverse execution
        // order: the target store runs last, after the two exit drops. A
        // member-expression rest target evaluates its base before the fresh
        // array is materialized (pushed last, so it executes first), leaving
        // `[base, array]` for the store.
        self.plan_assignment_target_value(
            &rest.target,
            work,
            flow,
            layout,
            tree_layout,
            constants,
        )?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            rest.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            rest.span,
        )));
        let next = flow.new_label(rest.span)?;
        let body = flow.new_label(rest.span)?;
        let done = flow.new_label(rest.span)?;
        work.push(ExpressionWork::Bind(done.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: next.clone(),
            span: rest.span,
        });
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Inc,
            Operands::None,
            rest.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::DefineArrayEl,
            Operands::None,
            rest.span,
        )));
        work.push(ExpressionWork::Bind(body.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: rest.span,
        });
        work.push(ExpressionWork::Branch {
            kind: BranchKind::IfFalse,
            target: body,
            span: rest.span,
        });
        // A member-expression rest target keeps its reference slots below
        // the fresh array and cursor, so the loop `for_of_next` temporary
        // offset is 2 (array and cursor) plus the reference depth.
        let step_offset = u8::try_from(
            2_usize.saturating_add(usize::from(Self::assignment_target_depth(&rest.target)?)),
        )
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "rest target reference depth",
        })?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForOfNext,
            Operands::U8(step_offset),
            rest.span,
        )));
        work.push(ExpressionWork::Bind(next));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Push0,
            Operands::NoneInt,
            rest.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ArrayFrom,
            Operands::NPop { argument_count: 0 },
            rest.span,
        )));
        // The member base (and computed key) execute before `array_from`
        // and stay below the fresh array for the store.
        Self::plan_assignment_target_prelude(&rest.target, constants, work)?;
        Ok(())
    }
}
