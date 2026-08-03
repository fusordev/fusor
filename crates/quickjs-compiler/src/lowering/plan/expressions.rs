use super::super::{
    ArrayExpression, ArrayExpressionElement, AssignmentExpression, AssignmentOperator,
    AssignmentTarget, AtomPoolIndex, BranchKind, CompilationContext, CompilationGoal,
    CompiledConstantPool, CompiledMetadataAtomKey, CompilerLabel, ConditionalExpression,
    DynamicFunctionKind, ExecutableId, Expression, FinalOpcode, FrameLayout, Function,
    FunctionTreeLayout, GetSpan, IdentifierReference, LeafCompilationError, LogicalExpression,
    LogicalOperator, LoweredReference, ObjectExpression, ObjectProperty, ObjectPropertyKind,
    Operands, PlannedControlFlow, PlannedInstruction, PropertyKind, SequenceExpression,
    SimpleAssignmentTarget, Span, UnaryExpression, UnaryOperator, UnsupportedLeafFeature,
    UpdateExpression, UpdateOperator, anonymous_named_evaluation_span,
    anonymous_ordinary_function_span, binary_opcode, compiled_static_property_key,
    exact_negated_i32, object_method_or_accessor_span, plan_literal, plan_push_integer,
    unary_opcode, unsupported,
};

pub(in crate::lowering) enum ExpressionWork<'expression, 'arena> {
    Visit(&'expression Expression<'arena>),
    Emit(PlannedInstruction),
    Branch {
        kind: BranchKind,
        target: CompilerLabel,
        span: Span,
    },
    Bind(CompilerLabel),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum ObjectMethodKind {
    Method,
    Getter,
    Setter,
}

impl ObjectMethodKind {
    const ENUMERABLE: u8 = 1 << 2;

    pub(in crate::lowering) const fn define_method_flags(self) -> u8 {
        Self::ENUMERABLE
            | match self {
                Self::Method => 0,
                Self::Getter => 1,
                Self::Setter => 2,
            }
    }
}

pub(in crate::lowering) fn same_operator_left_chain<'expression, 'arena>(
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

pub(in crate::lowering) struct ExpressionPlanner<'compiler, 'unit, 'arena, 'scope> {
    compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
}

impl<'compiler, 'unit, 'arena, 'scope> ExpressionPlanner<'compiler, 'unit, 'arena, 'scope> {
    pub(in crate::lowering) const fn new(
        compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
    ) -> Self {
        Self { compiler }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "the iterative dispatcher is the exhaustive expression-shape boundary"
    )]
    pub(in crate::lowering) fn plan_expression<'expression>(
        &self,
        expression: &'expression Expression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
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
                    if let Some(literal) = plan_literal(expression, constants) {
                        flow.emit(literal?)?;
                        continue;
                    }
                    match expression {
                        Expression::Identifier(identifier) => {
                            self.plan_identifier_read(identifier, layout, tree_layout, flow)?;
                        }
                        Expression::UnaryExpression(unary) => {
                            self.plan_unary_expression(
                                unary,
                                layout,
                                tree_layout,
                                constants,
                                &mut work,
                                flow,
                            )?;
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
                        Expression::ObjectExpression(object) => {
                            self.plan_object_expression(
                                object,
                                layout,
                                tree_layout,
                                constants,
                                &mut work,
                            )?;
                        }
                        Expression::ArrayExpression(array) => {
                            Self::plan_array_expression(array, constants, &mut work)?;
                        }
                        Expression::StaticMemberExpression(member) => {
                            Self::plan_static_member_read(member, constants, &mut work)?;
                        }
                        Expression::ComputedMemberExpression(member) => {
                            Self::plan_computed_member_read(member, &mut work)?;
                        }
                        Expression::AssignmentExpression(assignment) => {
                            self.plan_assignment_expression(
                                assignment,
                                layout,
                                tree_layout,
                                constants,
                                flow,
                                &mut work,
                            )?;
                        }
                        Expression::UpdateExpression(update) => {
                            self.plan_update_expression(update, layout, tree_layout, &mut work)?;
                        }
                        Expression::CallExpression(call) => {
                            Self::plan_call_expression(call, constants, &mut work)?;
                        }
                        Expression::NewExpression(constructor) => {
                            Self::plan_new_expression(constructor, &mut work)?;
                        }
                        Expression::FunctionExpression(function) => {
                            flow.emit(self.plan_function_closure(
                                function,
                                layout.executable,
                                tree_layout,
                                constants,
                            )?)?;
                        }
                        Expression::ThisExpression(this) => {
                            flow.emit(self.plan_this_expression(this.span, layout)?)?;
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

    fn plan_this_expression(
        &self,
        span: Span,
        layout: &FrameLayout,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let executable = self.planned.plan.executable(layout.executable).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: layout.executable,
            },
        )?;
        let is_dynamic_function_authority =
            self.unit.goal() == CompilationGoal::DynamicFunction(DynamicFunctionKind::Function);
        let is_object_method = self
            .planned
            .identities
            .node_by_executable
            .get(layout.executable.index())
            .copied()
            .and_then(|node_id| object_method_or_accessor_span(self.unit, node_id))
            .is_some();
        if !executable.is_strict() && !is_dynamic_function_authority && !is_object_method {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, span);
        }
        Ok(PlannedInstruction::new(
            FinalOpcode::PushThis,
            Operands::None,
            span,
        ))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the exact QuickJS spread-call argument packing is planned as one reviewable transaction"
    )]
    fn plan_array_expression<'expression>(
        array: &'expression ArrayExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(first_spread) = array
            .elements
            .iter()
            .position(ArrayExpressionElement::is_spread)
        {
            return Self::plan_spread_array_expression(array, first_spread, constants, work);
        }
        let Some(first_elision) = array
            .elements
            .iter()
            .position(ArrayExpressionElement::is_elision)
        else {
            return Self::plan_dense_array_expression(array, work);
        };

        Self::plan_sparse_array_expression(array, first_elision, constants, work)
    }

    pub(in crate::lowering) fn spread_array_dense_prefix_len(
        array: &ArrayExpression<'arena>,
    ) -> usize {
        // The pinned parser only stack-builds a small direct prefix before it
        // switches to explicit fields and the dynamic spread cursor.
        const QUICKJS_STACK_ARRAY_ELEMENT_LIMIT: usize = 32;

        array
            .elements
            .iter()
            .take(QUICKJS_STACK_ARRAY_ELEMENT_LIMIT)
            .take_while(|element| !element.is_elision() && !element.is_spread())
            .count()
    }

    pub(in crate::lowering) fn spread_array_final_length_span(
        array: &ArrayExpression<'arena>,
    ) -> Option<Span> {
        // A spread does not clear a pending hole: an empty trailing iterable
        // still requires the cursor to become the array's observable length.
        let mut final_length_span = None;
        for element in array
            .elements
            .iter()
            .skip(Self::spread_array_dense_prefix_len(array))
        {
            match element {
                ArrayExpressionElement::SpreadElement(_) => {}
                ArrayExpressionElement::Elision(elision) => {
                    final_length_span = Some(elision.span);
                }
                _ => final_length_span = None,
            }
        }
        final_length_span
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the exact QuickJS array-spread stack program is planned as one reviewable transaction"
    )]
    fn plan_spread_array_expression<'expression>(
        array: &'expression ArrayExpression<'arena>,
        first_spread: usize,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let dense_prefix = Self::spread_array_dense_prefix_len(array);
        let argument_count =
            u16::try_from(dense_prefix).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "array literal stack prefix",
            })?;
        let dynamic_index =
            i32::try_from(first_spread).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "array literal dynamic index",
            })?;
        let first_spread_span = array.elements[first_spread].span();
        let final_length_span = Self::spread_array_final_length_span(array);
        let mut planned = Vec::new();

        for element in array.elements.iter().take(dense_prefix) {
            let expression =
                element
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "prevalidated spread-array stack prefix contains expressions",
                        span: Some(element.span()),
                    })?;
            planned.push(ExpressionWork::Visit(expression));
        }
        planned.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ArrayFrom,
            Operands::NPop { argument_count },
            array.span,
        )));

        for (index, element) in array
            .elements
            .iter()
            .enumerate()
            .take(first_spread)
            .skip(dense_prefix)
        {
            if element.is_elision() {
                continue;
            }
            let expression =
                element
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "pre-spread array element is an expression or elision",
                        span: Some(element.span()),
                    })?;
            let index =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "array literal element indices",
                })?;
            planned.push(ExpressionWork::Visit(expression));
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::DefineField,
                Operands::Atom(constants.array_index_atom_index(
                    array.span,
                    index,
                    expression.span(),
                )?),
                expression.span(),
            )));
        }

        planned.push(ExpressionWork::Emit(plan_push_integer(
            dynamic_index,
            first_spread_span,
        )));
        for element in array.elements.iter().skip(first_spread) {
            match element {
                ArrayExpressionElement::SpreadElement(spread) => {
                    planned.push(ExpressionWork::Visit(&spread.argument));
                    planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Append,
                        Operands::None,
                        spread.span,
                    )));
                }
                ArrayExpressionElement::Elision(elision) => {
                    planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Inc,
                        Operands::None,
                        elision.span,
                    )));
                }
                _ => {
                    let expression = element.as_expression().ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "dynamic array element is an expression, spread, or elision",
                            span: Some(element.span()),
                        },
                    )?;
                    planned.push(ExpressionWork::Visit(expression));
                    planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::DefineArrayEl,
                        Operands::None,
                        expression.span(),
                    )));
                    planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Inc,
                        Operands::None,
                        expression.span(),
                    )));
                }
            }
        }

        if let Some(final_length_span) = final_length_span {
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup1,
                Operands::None,
                final_length_span,
            )));
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::PutField,
                Operands::Atom(constants.array_length_atom_index(array.span, final_length_span)?),
                final_length_span,
            )));
        } else {
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                array.span,
            )));
        }

        work.extend(planned.into_iter().rev());
        Ok(())
    }

    fn plan_dense_array_expression<'expression>(
        array: &'expression ArrayExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let argument_count = u16::try_from(array.elements.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "array literal elements",
            }
        })?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ArrayFrom,
            Operands::NPop { argument_count },
            array.span,
        )));
        for element in array.elements.iter().rev() {
            let expression =
                element
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "prevalidated dense array element is an expression",
                        span: Some(element.span()),
                    })?;
            work.push(ExpressionWork::Visit(expression));
        }
        Ok(())
    }

    fn plan_sparse_array_expression<'expression>(
        array: &'expression ArrayExpression<'arena>,
        first_elision: usize,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let sparse_length = i32::try_from(array.elements.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "sparse array literal length",
            }
        })?;

        if let Some(trailing_elision) = array.elements.last().filter(|element| element.is_elision())
        {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::PutField,
                Operands::Atom(
                    constants.array_length_atom_index(array.span, trailing_elision.span())?,
                ),
                trailing_elision.span(),
            )));
            work.push(ExpressionWork::Emit(plan_push_integer(
                sparse_length,
                trailing_elision.span(),
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                trailing_elision.span(),
            )));
        }

        for (index, element) in array
            .elements
            .iter()
            .enumerate()
            .skip(first_elision + 1)
            .rev()
        {
            if element.is_elision() {
                continue;
            }
            let expression =
                element
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "non-spread array element is an expression or elision",
                        span: Some(element.span()),
                    })?;
            let index =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "array literal element indices",
                })?;
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::DefineField,
                Operands::Atom(constants.array_index_atom_index(
                    array.span,
                    index,
                    expression.span(),
                )?),
                expression.span(),
            )));
            work.push(ExpressionWork::Visit(expression));
        }

        let dense_prefix =
            u16::try_from(first_elision).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "array literal dense prefix",
            })?;

        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ArrayFrom,
            Operands::NPop {
                argument_count: dense_prefix,
            },
            array.span,
        )));
        for element in array.elements.iter().take(first_elision).rev() {
            let expression =
                element
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "pre-elision array element is an expression",
                        span: Some(element.span()),
                    })?;
            work.push(ExpressionWork::Visit(expression));
        }
        Ok(())
    }

    fn plan_object_expression<'expression>(
        &self,
        object: &'expression ObjectExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        for property in object.properties.iter().rev() {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedExpression,
                    property.span(),
                );
            };
            if property.shorthand {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, property.span);
            }
            let method_kind = match (property.method, property.kind) {
                (true, PropertyKind::Init) => Some(ObjectMethodKind::Method),
                (false, PropertyKind::Get) => Some(ObjectMethodKind::Getter),
                (false, PropertyKind::Set) => Some(ObjectMethodKind::Setter),
                (false, PropertyKind::Init) => None,
                _ => {
                    return unsupported(
                        UnsupportedLeafFeature::ObjectMethodOrAccessor,
                        property.span,
                    );
                }
            };
            if property.computed {
                self.plan_computed_object_property(
                    property,
                    method_kind,
                    layout,
                    tree_layout,
                    constants,
                    work,
                )?;
                continue;
            }
            let Some(key) = compiled_static_property_key(&property.key)? else {
                return unsupported(
                    if property.method || property.kind != PropertyKind::Init {
                        UnsupportedLeafFeature::ObjectMethodOrAccessor
                    } else {
                        UnsupportedLeafFeature::UnsupportedExpression
                    },
                    property.key.span(),
                );
            };
            if let Some(kind) = method_kind {
                let Expression::FunctionExpression(function) = &property.value else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "object method or accessor value is a function expression",
                        span: Some(property.value.span()),
                    });
                };
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::DefineMethod,
                    Operands::AtomU8 {
                        atom: constants.property_atom_index(key.span)?,
                        value: kind.define_method_flags(),
                    },
                    property.span,
                )));
                work.push(ExpressionWork::Emit(self.plan_function_closure(
                    function,
                    layout.executable,
                    tree_layout,
                    constants,
                )?));
                continue;
            }
            // `__proto__: value` in an object literal is a prototype
            // mutation, not an own property. Only an object or `null` takes
            // effect; every other value is silently ignored, which the pinned
            // `OP_set_proto` handler enforces (`quickjs.c:19330-19341`).
            // Shorthand and computed forms are ordinary own properties and are
            // handled by their own planners.
            if key.value.code_units().eq("__proto__".encode_utf16())
                && !property.shorthand
                && property.kind == PropertyKind::Init
            {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::SetProto,
                    Operands::None,
                    property.span,
                )));
                work.push(ExpressionWork::Visit(&property.value));
                continue;
            }
            let inferred_name = Self::plan_inferred_static_property_name_for_initializer(
                &property.value,
                constants.property_atom_index(key.span)?,
            )?;
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::DefineField,
                Operands::Atom(constants.property_atom_index(key.span)?),
                property.span,
            )));
            if let Some(set_name) = inferred_name {
                work.push(ExpressionWork::Emit(set_name));
            }
            work.push(ExpressionWork::Visit(&property.value));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Object,
            Operands::None,
            object.span,
        )));
        Ok(())
    }

    fn plan_inferred_static_property_name_for_initializer(
        initializer: &Expression<'arena>,
        atom: AtomPoolIndex,
    ) -> Result<Option<PlannedInstruction>, LeafCompilationError> {
        let Some(span) = anonymous_named_evaluation_span(initializer) else {
            return Ok(None);
        };
        if anonymous_ordinary_function_span(initializer).is_none() {
            return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
        }
        Ok(Some(PlannedInstruction::new(
            FinalOpcode::SetName,
            Operands::Atom(atom),
            span,
        )))
    }

    fn plan_computed_object_property<'expression>(
        &self,
        property: &'expression ObjectProperty<'arena>,
        method_kind: Option<ObjectMethodKind>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let key = property
            .key
            .as_expression()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "computed object property key is an expression",
                span: Some(property.key.span()),
            })?;
        if let Some(kind) = method_kind {
            let Expression::FunctionExpression(function) = &property.value else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "object method or accessor value is a function expression",
                    span: Some(property.value.span()),
                });
            };
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::DefineMethodComputed,
                Operands::U8(kind.define_method_flags()),
                property.span,
            )));
            work.push(ExpressionWork::Emit(self.plan_function_closure(
                function,
                layout.executable,
                tree_layout,
                constants,
            )?));
            work.push(ExpressionWork::Visit(key));
            return Ok(());
        }
        let inferred_name = if let Some(span) = anonymous_named_evaluation_span(&property.value) {
            if anonymous_ordinary_function_span(&property.value).is_none() {
                return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
            }
            Some(PlannedInstruction::new(
                FinalOpcode::SetNameComputed,
                Operands::None,
                span,
            ))
        } else {
            None
        };
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            property.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::DefineArrayEl,
            Operands::None,
            property.span,
        )));
        if let Some(set_name) = inferred_name {
            work.push(ExpressionWork::Emit(set_name));
        }
        work.push(ExpressionWork::Visit(&property.value));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ToPropKey,
            Operands::None,
            key.span(),
        )));
        work.push(ExpressionWork::Visit(key));
        Ok(())
    }

    fn plan_function_closure(
        &self,
        function: &Function<'arena>,
        parent: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let child = self.executable_for_function(function)?;
        self.plan_child_function_closure(child, parent, function.span, tree_layout, constants)
    }

    pub(in crate::lowering) fn executable_for_function(
        &self,
        function: &Function<'arena>,
    ) -> Result<ExecutableId, LeafCompilationError> {
        let node_id = function.node_id.get();
        self.planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "nested function node has a compiler executable identity",
                span: Some(function.span),
            })
    }

    pub(in crate::lowering) fn plan_child_function_closure(
        &self,
        child: ExecutableId,
        parent: ExecutableId,
        span: Span,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let child_metadata = self
            .planned
            .plan
            .executable(child)
            .ok_or(LeafCompilationError::InvalidExecutable { executable: child })?;
        if child_metadata.parent() != Some(parent)
            || tree_layout.children(parent)?.binary_search(&child).is_err()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "nested function constant names a direct child executable",
                span: Some(span),
            });
        }
        let constant_index = constants.function_index(child)?;
        let (opcode, operands) = match u8::try_from(constant_index) {
            Ok(index) => (FinalOpcode::FClosure8, Operands::Const8(index)),
            Err(_) => (FinalOpcode::FClosure, Operands::Const(constant_index)),
        };
        Ok(PlannedInstruction::new(opcode, operands, span))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "identifier assignment forms share one ordered work-stack planner"
    )]
    fn plan_assignment_expression<'expression>(
        &self,
        assignment: &'expression AssignmentExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let AssignmentTarget::StaticMemberExpression(member) = &assignment.left {
            return Self::plan_static_member_assignment(assignment, member, constants, work);
        }
        if let AssignmentTarget::ComputedMemberExpression(member) = &assignment.left {
            return Self::plan_computed_member_assignment(assignment, member, work);
        }
        if let AssignmentTarget::ArrayAssignmentTarget(pattern) = &assignment.left {
            if assignment.operator != AssignmentOperator::Assign {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedExpression,
                    assignment.span,
                );
            }
            // The RHS is evaluated, duplicated, and destructured; the
            // original copy remains as the assignment expression's value.
            self.plan_array_assignment_elements(
                pattern,
                flow,
                work,
                layout,
                tree_layout,
                constants,
            )?;
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                assignment.span,
            )));
            work.push(ExpressionWork::Visit(&assignment.right));
            return Ok(());
        }
        if let AssignmentTarget::ObjectAssignmentTarget(pattern) = &assignment.left {
            if assignment.operator != AssignmentOperator::Assign {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedExpression,
                    assignment.span,
                );
            }
            // The RHS is evaluated, duplicated, and object-destructured; the
            // original copy remains as the assignment expression's value.
            self.plan_object_assignment_value(pattern, work, flow, layout, tree_layout, constants)?;
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                assignment.span,
            )));
            work.push(ExpressionWork::Visit(&assignment.right));
            return Ok(());
        }
        let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.left else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        };
        let needs_read = assignment.operator != AssignmentOperator::Assign;
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        self.validate_lowered_mutation_reference(reference, needs_read, identifier.span)?;
        let inferred_name = if matches!(
            assignment.operator,
            AssignmentOperator::Assign
                | AssignmentOperator::LogicalOr
                | AssignmentOperator::LogicalAnd
                | AssignmentOperator::LogicalNullish
        ) {
            Self::plan_inferred_reference_name_for_initializer(
                reference,
                &assignment.right,
                constants,
            )?
        } else {
            None
        };
        let (binding, frame_slot) = match reference {
            LoweredReference::Frame { binding, slot, .. } => (binding, slot),
            LoweredReference::RealmGlobal { slot, .. } => {
                return Self::plan_realm_global_assignment(
                    assignment,
                    slot,
                    inferred_name,
                    flow,
                    work,
                );
            }
        };

        match assignment.operator {
            AssignmentOperator::Assign => {
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
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
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
                if let Some(set_name) = inferred_name {
                    work.push(ExpressionWork::Emit(set_name));
                }
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

    fn plan_inferred_reference_name_for_initializer(
        reference: LoweredReference,
        initializer: &Expression<'arena>,
        constants: &CompiledConstantPool,
    ) -> Result<Option<PlannedInstruction>, LeafCompilationError> {
        let Some(span) = anonymous_named_evaluation_span(initializer) else {
            return Ok(None);
        };
        if anonymous_ordinary_function_span(initializer).is_none() {
            return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
        }
        let key = match reference {
            LoweredReference::Frame { binding, .. } => CompiledMetadataAtomKey::Binding(binding),
            LoweredReference::RealmGlobal { global, .. } => {
                CompiledMetadataAtomKey::RealmGlobal(global)
            }
        };
        Ok(Some(PlannedInstruction::new(
            FinalOpcode::SetName,
            Operands::Atom(constants.metadata_atom_index(key)?),
            span,
        )))
    }

    pub(in crate::lowering) fn plan_inferred_identifier_reference_name_for_initializer(
        &self,
        identifier: &IdentifierReference<'arena>,
        initializer: &Expression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<Option<PlannedInstruction>, LeafCompilationError> {
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
        Self::plan_inferred_reference_name_for_initializer(reference, initializer, constants)
    }

    pub(in crate::lowering) fn plan_inferred_assignment_target_name_for_initializer(
        &self,
        target: &AssignmentTarget<'arena>,
        initializer: &Expression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<Option<PlannedInstruction>, LeafCompilationError> {
        let AssignmentTarget::AssignmentTargetIdentifier(identifier) = target else {
            return Ok(None);
        };
        self.plan_inferred_identifier_reference_name_for_initializer(
            identifier,
            initializer,
            layout,
            tree_layout,
            constants,
        )
    }

    fn plan_update_expression<'expression>(
        &self,
        update: &'expression UpdateExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) = &update.argument
        else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        };
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        self.validate_lowered_mutation_reference(reference, true, identifier.span)?;
        let (binding, frame_slot) = match reference {
            LoweredReference::Frame { binding, slot, .. } => (binding, slot),
            LoweredReference::RealmGlobal { slot, .. } => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::PutVar,
                    Operands::VarRef(slot),
                    identifier.span,
                )));
                if update.prefix {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Dup,
                        Operands::None,
                        update.span,
                    )));
                }
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
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetVar,
                    Operands::VarRef(slot),
                    identifier.span,
                )));
                return Ok(());
            }
        };

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
        &self,
        unary: &'expression UnaryExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
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
        if unary.operator == UnaryOperator::Typeof {
            let mut argument = &unary.argument;
            while let Expression::ParenthesizedExpression(parenthesized) = argument {
                argument = &parenthesized.expression;
            }
            if let Expression::Identifier(identifier) = argument {
                let reference = self.lowered_reference(
                    identifier.reference_id.get(),
                    identifier.span,
                    layout,
                    tree_layout,
                )?;
                if let LoweredReference::RealmGlobal { slot, access, .. } = reference {
                    if !access.reads() || access.writes() {
                        return unsupported(
                            UnsupportedLeafFeature::UnsupportedReference,
                            identifier.span,
                        );
                    }
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Typeof,
                        Operands::None,
                        unary.span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetVarUndef,
                        Operands::VarRef(slot),
                        identifier.span,
                    )));
                    return Ok(());
                }
            }
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
                Self::plan_delete_expression(unary, constants, work)?;
            }
        }
        Ok(())
    }

    /// Lowers `delete` into the pinned `OP_delete` shape.
    ///
    /// `QuickJS` rewrites the preceding member read into a key push followed by
    /// `OP_delete` (`quickjs.c:27395-27437`), so the operand order here is the
    /// base object then the property key. `delete` of anything that is not a
    /// member expression evaluates its operand for effect and yields `true`,
    /// which is ECMAScript's non-Reference case; an unqualified identifier
    /// operand is an early error the front end already rejects in strict code
    /// and is not admitted here.
    fn plan_delete_expression<'expression>(
        unary: &'expression UnaryExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let mut argument = &unary.argument;
        while let Expression::ParenthesizedExpression(parenthesized) = argument {
            argument = &parenthesized.expression;
        }
        match argument {
            Expression::StaticMemberExpression(member) => {
                if member.optional {
                    return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Delete,
                    Operands::None,
                    unary.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::PushAtomValue,
                    Operands::Atom(constants.property_atom_index(member.property.span)?),
                    member.property.span,
                )));
                work.push(ExpressionWork::Visit(&member.object));
                Ok(())
            }
            Expression::ComputedMemberExpression(member) => {
                if member.optional {
                    return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Delete,
                    Operands::None,
                    unary.span,
                )));
                work.push(ExpressionWork::Visit(&member.expression));
                work.push(ExpressionWork::Visit(&member.object));
                Ok(())
            }
            // `delete identifier` is not a property delete: it consults the
            // binding's own deletability. Strict code rejects it as an early
            // error, and the admitted sloppy cases (`false` for a declared
            // binding, `true` for a missing one) need the pinned
            // `OP_delete_var` scope resolution rather than `OP_delete`. Until
            // that lowering exists this stays fail-closed instead of silently
            // reporting `true`.
            Expression::Identifier(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedExpression, unary.span)
            }
            _ => {
                // ECMAScript's non-Reference case: the operand is evaluated
                // for effect and `delete` yields `true`. The pinned oracle
                // agrees (`delete (1 + 1)` is `true`).
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::PushTrue,
                    Operands::None,
                    unary.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    argument.span(),
                )));
                work.push(ExpressionWork::Visit(argument));
                Ok(())
            }
        }
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
}

impl<'unit, 'arena, 'scope> std::ops::Deref for ExpressionPlanner<'_, 'unit, 'arena, 'scope> {
    type Target = CompilationContext<'unit, 'arena, 'scope>;

    fn deref(&self) -> &Self::Target {
        self.compiler
    }
}
