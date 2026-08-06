use super::super::{
    ArrayExpression, ArrayExpressionElement, ArrowFunctionExpression, AssignmentExpression,
    AssignmentOperator, AssignmentTarget, AtomPoolIndex, BinaryOperator, BranchKind,
    CallExpression, ChainElement, ChainExpression, CompilationContext, CompiledConstantPool,
    CompiledMetadataAtomKey, CompilerLabel, ComputedMemberExpression, ConditionalExpression,
    ExecutableId, ExecutableKind, Expression, FinalOpcode, FrameLayout, Function,
    FunctionTreeLayout, GetSpan, IdentifierReference, LeafCompilationError, LogicalExpression,
    LogicalOperator, LoweredReference, ObjectExpression, ObjectProperty, ObjectPropertyKind,
    Operands, PlannedControlFlow, PlannedInstruction, PropertyKind, SequenceExpression,
    SimpleAssignmentTarget, Span, StaticMemberExpression, UnaryExpression, UnaryOperator,
    UnsupportedLeafFeature, UpdateExpression, UpdateOperator, compiled_static_property_key,
    object_method_or_accessor_span, unsupported,
};
use super::abrupt::{AbruptMarker, AbruptMarkerKind};
use super::calls::MemberCallee;

pub(in crate::lowering) fn anonymous_named_evaluation_span(
    mut expression: &Expression<'_>,
) -> Option<Span> {
    while let Expression::ParenthesizedExpression(parenthesized) = expression {
        expression = &parenthesized.expression;
    }
    match expression {
        Expression::FunctionExpression(function) if function.id.is_none() => Some(function.span),
        Expression::ArrowFunctionExpression(arrow) => Some(arrow.span),
        Expression::ClassExpression(class) if class.id.is_none() => Some(class.span),
        _ => None,
    }
}

pub(in crate::lowering) fn anonymous_ordinary_function_span(
    mut expression: &Expression<'_>,
) -> Option<Span> {
    while let Expression::ParenthesizedExpression(parenthesized) = expression {
        expression = &parenthesized.expression;
    }
    match expression {
        Expression::FunctionExpression(function) if function.id.is_none() => Some(function.span),
        Expression::ArrowFunctionExpression(arrow) => Some(arrow.span),
        _ => None,
    }
}

pub(in crate::lowering) fn plan_literal(
    expression: &Expression<'_>,
    constants: &CompiledConstantPool,
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
        Expression::NumericLiteral(literal) => match exact_i32(literal.value) {
            Some(value) => Ok(plan_push_integer(value, literal.span)),
            None => constants.plan_number(literal.value, literal.span),
        },
        Expression::BigIntLiteral(literal) => match literal.value.parse::<i32>() {
            Ok(value) => Ok(PlannedInstruction::new(
                FinalOpcode::PushBigIntI32,
                Operands::I32(value),
                literal.span,
            )),
            Err(_) => constants.plan_bigint(literal.span),
        },
        Expression::StringLiteral(literal) if literal.value.is_empty() => Ok(
            PlannedInstruction::new(FinalOpcode::PushEmptyString, Operands::None, literal.span),
        ),
        Expression::StringLiteral(literal) => constants.plan_string(literal.span),
        Expression::TemplateLiteral(template)
            if template.expressions.is_empty() && template.quasis.len() == 1 =>
        {
            let quasi = &template.quasis[0];
            if quasi.tail {
                match quasi.value.cooked.as_ref() {
                    None => Err(LeafCompilationError::SemanticInvariant {
                        invariant: "untagged no-substitution template has a cooked value",
                        span: Some(template.span),
                    }),
                    Some(cooked) if cooked.is_empty() => Ok(PlannedInstruction::new(
                        FinalOpcode::PushEmptyString,
                        Operands::None,
                        quasi.span,
                    )),
                    Some(_) => constants.plan_string(quasi.span),
                }
            } else {
                Err(LeafCompilationError::SemanticInvariant {
                    invariant: "no-substitution template has one tail quasi",
                    span: Some(template.span),
                })
            }
        }
        _ => return None,
    };
    Some(planned)
}

#[allow(clippy::cast_possible_truncation)]
pub(in crate::lowering) fn exact_i32(value: f64) -> Option<i32> {
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

pub(in crate::lowering) fn exact_negated_i32(value: f64) -> Option<i32> {
    exact_i32(-value)
}

pub(in crate::lowering) fn plan_push_integer(value: i32, span: Span) -> PlannedInstruction {
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

pub(in crate::lowering) const fn unary_opcode(operator: UnaryOperator) -> Option<FinalOpcode> {
    match operator {
        UnaryOperator::UnaryPlus => Some(FinalOpcode::Plus),
        UnaryOperator::UnaryNegation => Some(FinalOpcode::Neg),
        UnaryOperator::LogicalNot => Some(FinalOpcode::Lnot),
        UnaryOperator::BitwiseNot => Some(FinalOpcode::Not),
        UnaryOperator::Typeof => Some(FinalOpcode::Typeof),
        UnaryOperator::Void | UnaryOperator::Delete => None,
    }
}

pub(in crate::lowering) const fn binary_opcode(operator: BinaryOperator) -> FinalOpcode {
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

pub(in crate::lowering) enum ExpressionWork<'expression, 'arena> {
    Visit(&'expression Expression<'arena>),
    VisitOptionalChain {
        chain: &'expression ChainExpression<'arena>,
        preserve_final_reference: bool,
    },
    CallAfterCallee {
        call: &'expression CallExpression<'arena>,
        method: bool,
    },
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
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let mut work = vec![ExpressionWork::Visit(expression)];
        while let Some(task) = work.pop() {
            match task {
                ExpressionWork::Emit(instruction) => flow.emit(instruction)?,
                ExpressionWork::VisitOptionalChain {
                    chain,
                    preserve_final_reference,
                } => Self::plan_optional_chain(
                    chain,
                    preserve_final_reference,
                    constants,
                    flow,
                    &mut work,
                )?,
                ExpressionWork::CallAfterCallee { call, method } => {
                    Self::plan_call_after_callee(call, method, &mut work)?;
                }
                ExpressionWork::Branch { kind, target, span } => {
                    flow.branch(kind, &target, span)?;
                }
                ExpressionWork::Bind(label) => flow.bind(&label)?,
                ExpressionWork::Visit(expression) => {
                    if let Expression::RegExpLiteral(literal) = expression {
                        for instruction in constants.plan_regexp_literal(literal)? {
                            flow.emit(instruction)?;
                        }
                        continue;
                    }
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
                        Expression::TemplateLiteral(template) => {
                            Self::plan_untagged_template_literal(template, constants, &mut work)?;
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
                        Expression::ChainExpression(chain) => {
                            Self::plan_optional_chain(chain, false, constants, flow, &mut work)?;
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
                        Expression::TaggedTemplateExpression(tagged) => {
                            Self::plan_tagged_template_expression(tagged, constants, &mut work)?;
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
                        Expression::ArrowFunctionExpression(arrow) => {
                            flow.emit(self.plan_arrow_closure(
                                arrow,
                                layout.executable,
                                tree_layout,
                                constants,
                            )?)?;
                        }
                        Expression::YieldExpression(yield_expression) => {
                            let executable =
                                self.planned.plan.executable(layout.executable).ok_or(
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
                            if yield_expression.delegate {
                                Self::plan_delegated_yield(
                                    yield_expression,
                                    async_generator,
                                    constants,
                                    abrupt_markers,
                                    flow,
                                    &mut work,
                                )?;
                                continue;
                            }
                            let resumed = flow.new_label(yield_expression.span)?;
                            work.push(ExpressionWork::Bind(resumed.clone()));
                            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                FinalOpcode::ReturnAsync,
                                Operands::None,
                                yield_expression.span,
                            )));
                            Self::schedule_yield_return_cleanup(
                                abrupt_markers,
                                yield_expression.span,
                                &mut work,
                            );
                            work.push(ExpressionWork::Branch {
                                kind: BranchKind::IfFalse,
                                target: resumed,
                                span: yield_expression.span,
                            });
                            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                FinalOpcode::Yield,
                                Operands::None,
                                yield_expression.span,
                            )));
                            if async_generator {
                                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                    FinalOpcode::Await,
                                    Operands::None,
                                    yield_expression.span,
                                )));
                            }
                            if let Some(argument) = &yield_expression.argument {
                                work.push(ExpressionWork::Visit(argument));
                            } else {
                                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                    FinalOpcode::Undefined,
                                    Operands::None,
                                    yield_expression.span,
                                )));
                            }
                        }
                        Expression::AwaitExpression(await_expression) => {
                            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                FinalOpcode::Await,
                                Operands::None,
                                await_expression.span,
                            )));
                            work.push(ExpressionWork::Visit(&await_expression.argument));
                        }
                        Expression::ThisExpression(this) => {
                            flow.emit(self.plan_this_expression(this.span, layout)?)?;
                        }
                        Expression::NewTarget(new_target) => {
                            flow.emit(PlannedInstruction::new(
                                FinalOpcode::SpecialObject,
                                Operands::U8(3),
                                new_target.span,
                            ))?;
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

    #[expect(
        clippy::too_many_lines,
        reason = "the complete chain-level short-circuit schedule stays visible in execution order"
    )]
    fn plan_optional_chain<'expression>(
        chain: &'expression ChainExpression<'arena>,
        preserve_final_reference: bool,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        enum Step<'expression, 'arena> {
            Static(&'expression StaticMemberExpression<'arena>),
            Computed(&'expression ComputedMemberExpression<'arena>),
            Call(&'expression CallExpression<'arena>),
        }

        impl Step<'_, '_> {
            const fn optional(&self) -> bool {
                match self {
                    Self::Static(member) => member.optional,
                    Self::Computed(member) => member.optional,
                    Self::Call(call) => call.optional,
                }
            }

            const fn span(&self) -> Span {
                match self {
                    Self::Static(member) => member.span,
                    Self::Computed(member) => member.span,
                    Self::Call(call) => call.span,
                }
            }

            const fn is_member(&self) -> bool {
                matches!(self, Self::Static(_) | Self::Computed(_))
            }

            const fn is_call(&self) -> bool {
                matches!(self, Self::Call(_))
            }
        }

        let mut steps = Vec::new();
        let mut root = match &chain.expression {
            ChainElement::StaticMemberExpression(member) => {
                steps.push(Step::Static(member));
                &member.object
            }
            ChainElement::ComputedMemberExpression(member) => {
                steps.push(Step::Computed(member));
                &member.object
            }
            ChainElement::CallExpression(call) => {
                steps.push(Step::Call(call));
                &call.callee
            }
            ChainElement::TSNonNullExpression(_) | ChainElement::PrivateFieldExpression(_) => {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, chain.span);
            }
        };
        loop {
            match root {
                Expression::StaticMemberExpression(member) => {
                    steps.push(Step::Static(member));
                    root = &member.object;
                }
                Expression::ComputedMemberExpression(member) => {
                    steps.push(Step::Computed(member));
                    root = &member.object;
                }
                Expression::CallExpression(call) => {
                    steps.push(Step::Call(call));
                    root = &call.callee;
                }
                _ => break,
            }
        }
        if !steps.iter().any(Step::optional) {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, chain.span);
        }
        steps.reverse();

        let root_member = if steps.first().is_some_and(Step::is_call) {
            Self::member_callee(root)?
        } else {
            None
        };

        let end = flow.new_label(chain.span)?;
        let mut planned = Vec::new();
        match root_member {
            Some(MemberCallee::Static(member)) => {
                planned.push(ExpressionWork::Visit(&member.object));
                planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetField2,
                    Operands::Atom(constants.property_atom_index(member.property.span)?),
                    member.span,
                )));
            }
            Some(MemberCallee::Computed(member)) => {
                planned.push(ExpressionWork::Visit(&member.object));
                planned.push(ExpressionWork::Visit(&member.expression));
                planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetArrayEl2,
                    Operands::None,
                    member.span,
                )));
            }
            Some(MemberCallee::Chain(chain)) => {
                planned.push(ExpressionWork::VisitOptionalChain {
                    chain,
                    preserve_final_reference: true,
                });
            }
            None => planned.push(ExpressionWork::Visit(root)),
        }

        for (index, step) in steps.iter().enumerate() {
            let method = step.is_call()
                && if index == 0 {
                    root_member.is_some()
                } else {
                    steps[index - 1].is_member()
                };
            if step.optional() {
                Self::append_optional_chain_guard(
                    if method { 2 } else { 1 },
                    if preserve_final_reference { 2 } else { 1 },
                    step.span(),
                    &end,
                    flow,
                    &mut planned,
                )?;
            }
            let final_step = index + 1 == steps.len();
            match step {
                Step::Static(member) => {
                    let preserve_receiver = steps.get(index + 1).is_some_and(Step::is_call)
                        || (final_step && preserve_final_reference);
                    planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                        if preserve_receiver {
                            FinalOpcode::GetField2
                        } else {
                            FinalOpcode::GetField
                        },
                        Operands::Atom(constants.property_atom_index(member.property.span)?),
                        member.span,
                    )));
                }
                Step::Computed(member) => {
                    let preserve_receiver = steps.get(index + 1).is_some_and(Step::is_call)
                        || (final_step && preserve_final_reference);
                    planned.push(ExpressionWork::Visit(&member.expression));
                    planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                        if preserve_receiver {
                            FinalOpcode::GetArrayEl2
                        } else {
                            FinalOpcode::GetArrayEl
                        },
                        Operands::None,
                        member.span,
                    )));
                }
                Step::Call(call) => {
                    if call.type_arguments.is_some() {
                        return unsupported(
                            UnsupportedLeafFeature::UnsupportedExpression,
                            call.span,
                        );
                    }
                    planned.push(ExpressionWork::CallAfterCallee { call, method });
                }
            }
        }
        if preserve_final_reference && steps.last().is_some_and(Step::is_call) {
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                chain.span,
            )));
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                chain.span,
            )));
        }
        planned.push(ExpressionWork::Bind(end));
        work.extend(planned.into_iter().rev());
        Ok(())
    }

    fn append_optional_chain_guard<'expression>(
        input_values: usize,
        output_values: usize,
        span: Span,
        end: &CompilerLabel,
        flow: &mut PlannedControlFlow,
        planned: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let non_nullish = flow.new_label(span)?;
        planned.extend([
            ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                span,
            )),
            ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::IsUndefinedOrNull,
                Operands::None,
                span,
            )),
            ExpressionWork::Branch {
                kind: BranchKind::IfFalse,
                target: non_nullish.clone(),
                span,
            },
        ]);
        for _ in 0..input_values {
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                span,
            )));
        }
        for _ in 0..output_values {
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                span,
            )));
        }
        planned.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: end.clone(),
            span,
        });
        planned.push(ExpressionWork::Bind(non_nullish));
        Ok(())
    }

    fn schedule_yield_return_cleanup<'expression>(
        abrupt_markers: &[AbruptMarker],
        span: Span,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) {
        let mut cleanup = Vec::new();
        Self::append_yield_return_cleanup(abrupt_markers, span, &mut cleanup);
        work.extend(cleanup.into_iter().rev());
    }

    fn plan_untagged_template_literal<'expression>(
        template: &'expression oxc_ast::ast::TemplateLiteral<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if template.quasis.len() != template.expressions.len().saturating_add(1) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "untagged template has one more quasi than substitutions",
                span: Some(template.span),
            });
        }

        let mut planned = Vec::new();
        let first = template
            .quasis
            .first()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "untagged template has an initial quasi",
                span: Some(template.span),
            })?;
        planned.push(ExpressionWork::Emit(Self::plan_template_quasi(
            first, constants,
        )?));

        for (expression, quasi) in template
            .expressions
            .iter()
            .zip(template.quasis.iter().skip(1))
        {
            planned.push(ExpressionWork::Visit(expression));
            // `ToPropertyKey` is the final QuickJS opcode that performs
            // `ToPrimitive` with a String hint. For every non-Symbol it also
            // performs `ToString`; a Symbol remains a Symbol so the guaranteed
            // String-left `Add` below raises the required TypeError. This keeps
            // every conversion ahead of the next expression and avoids an
            // observable `String.prototype.concat` lookup.
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::ToPropKey,
                Operands::None,
                expression.span(),
            )));
            planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Add,
                Operands::None,
                expression.span(),
            )));

            let cooked =
                quasi
                    .value
                    .cooked
                    .as_ref()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "untagged template quasi has a cooked value",
                        span: Some(quasi.span),
                    })?;
            if !cooked.is_empty() {
                planned.push(ExpressionWork::Emit(constants.plan_string(quasi.span)?));
                planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Add,
                    Operands::None,
                    quasi.span,
                )));
            }
        }

        work.extend(planned.into_iter().rev());
        Ok(())
    }

    fn plan_template_quasi(
        quasi: &oxc_ast::ast::TemplateElement<'arena>,
        constants: &CompiledConstantPool,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let cooked =
            quasi
                .value
                .cooked
                .as_ref()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "untagged template quasi has a cooked value",
                    span: Some(quasi.span),
                })?;
        if cooked.is_empty() {
            Ok(PlannedInstruction::new(
                FinalOpcode::PushEmptyString,
                Operands::None,
                quasi.span,
            ))
        } else {
            constants.plan_string(quasi.span)
        }
    }

    fn append_yield_return_cleanup<'expression>(
        abrupt_markers: &[AbruptMarker],
        span: Span,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) {
        for marker in abrupt_markers.iter().rev() {
            match &marker.kind {
                AbruptMarkerKind::Catch { finalizer } => {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::NipCatch,
                        Operands::None,
                        span,
                    )));
                    if let Some(finalizer) = finalizer {
                        work.push(ExpressionWork::Branch {
                            kind: BranchKind::Gosub,
                            target: finalizer.clone(),
                            span,
                        });
                    }
                }
                AbruptMarkerKind::ForIn => {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Nip,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::ForOf => {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::NipCatch,
                        Operands::None,
                        span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Rot3r,
                        Operands::None,
                        span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Undefined,
                        Operands::None,
                        span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::IteratorClose,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::FinallySubroutine => {
                    for _ in 0..2 {
                        work.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::Nip,
                            Operands::None,
                            span,
                        )));
                    }
                }
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the ECMA-262 delegated-yield loop is emitted as one auditable control-flow template"
    )]
    fn plan_delegated_yield<'expression>(
        yield_expression: &'expression oxc_ast::ast::YieldExpression<'arena>,
        async_generator: bool,
        constants: &CompiledConstantPool,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let Some(argument) = yield_expression.argument.as_ref() else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "delegated yield has an assignment expression",
                span: Some(yield_expression.span),
            });
        };
        let span = yield_expression.span;
        let done_atom = constants.yield_star_done_atom_index(span)?;
        let value_atom = constants.yield_star_value_atom_index(span)?;
        let loop_label = flow.new_label(span)?;
        let yield_label = flow.new_label(span)?;
        let resume_abrupt = flow.new_label(span)?;
        let resume_throw = flow.new_label(span)?;
        let return_done = flow.new_label(span)?;
        let missing_throw = flow.new_label(span)?;
        let missing_throw_closed = flow.new_label(span)?;
        let delegate_done = flow.new_label(span)?;

        let emit = |opcode, operands| {
            ExpressionWork::Emit(PlannedInstruction::new(opcode, operands, span))
        };
        let mut sequence = vec![
            ExpressionWork::Visit(argument),
            emit(
                if async_generator {
                    FinalOpcode::ForAwaitOfStart
                } else {
                    FinalOpcode::ForOfStart
                },
                Operands::None,
            ),
            emit(FinalOpcode::Drop, Operands::None),
            emit(FinalOpcode::Undefined, Operands::None),
            emit(FinalOpcode::Undefined, Operands::None),
            ExpressionWork::Bind(loop_label.clone()),
            emit(FinalOpcode::IteratorNext, Operands::None),
        ];
        if async_generator {
            sequence.push(emit(FinalOpcode::Await, Operands::None));
        }
        sequence.extend([
            emit(FinalOpcode::IteratorCheckObject, Operands::None),
            emit(FinalOpcode::GetField2, Operands::Atom(done_atom)),
            ExpressionWork::Branch {
                kind: BranchKind::IfTrue,
                target: delegate_done.clone(),
                span,
            },
            ExpressionWork::Bind(yield_label.clone()),
        ]);
        if async_generator {
            sequence.push(emit(FinalOpcode::GetField, Operands::Atom(value_atom)));
            sequence.push(emit(FinalOpcode::AsyncYieldStar, Operands::None));
        } else {
            sequence.push(emit(FinalOpcode::YieldStar, Operands::None));
        }
        sequence.extend([
            emit(FinalOpcode::Dup, Operands::None),
            ExpressionWork::Branch {
                kind: BranchKind::IfTrue,
                target: resume_abrupt.clone(),
                span,
            },
            emit(FinalOpcode::Drop, Operands::None),
            ExpressionWork::Branch {
                kind: BranchKind::Goto,
                target: loop_label,
                span,
            },
            ExpressionWork::Bind(resume_abrupt.clone()),
            emit(FinalOpcode::Push2, Operands::NoneInt),
            emit(FinalOpcode::StrictEq, Operands::None),
            ExpressionWork::Branch {
                kind: BranchKind::IfTrue,
                target: resume_throw.clone(),
                span,
            },
            emit(FinalOpcode::IteratorCall, Operands::U8(0)),
            ExpressionWork::Branch {
                kind: BranchKind::IfTrue,
                target: return_done.clone(),
                span,
            },
        ]);
        if async_generator {
            sequence.push(emit(FinalOpcode::Await, Operands::None));
        }
        sequence.extend([
            emit(FinalOpcode::IteratorCheckObject, Operands::None),
            emit(FinalOpcode::GetField2, Operands::Atom(done_atom)),
            ExpressionWork::Branch {
                kind: BranchKind::IfFalse,
                target: yield_label.clone(),
                span,
            },
            emit(FinalOpcode::GetField, Operands::Atom(value_atom)),
            ExpressionWork::Bind(return_done.clone()),
            emit(FinalOpcode::Nip, Operands::None),
            emit(FinalOpcode::Nip, Operands::None),
            emit(FinalOpcode::Nip, Operands::None),
        ]);
        Self::append_yield_return_cleanup(abrupt_markers, span, &mut sequence);
        sequence.extend([
            emit(FinalOpcode::ReturnAsync, Operands::None),
            ExpressionWork::Bind(resume_throw.clone()),
            emit(FinalOpcode::IteratorCall, Operands::U8(1)),
            ExpressionWork::Branch {
                kind: BranchKind::IfTrue,
                target: missing_throw.clone(),
                span,
            },
        ]);
        if async_generator {
            sequence.push(emit(FinalOpcode::Await, Operands::None));
        }
        sequence.extend([
            emit(FinalOpcode::IteratorCheckObject, Operands::None),
            emit(FinalOpcode::GetField2, Operands::Atom(done_atom)),
            ExpressionWork::Branch {
                kind: BranchKind::IfFalse,
                target: yield_label,
                span,
            },
            ExpressionWork::Branch {
                kind: BranchKind::Goto,
                target: delegate_done.clone(),
                span,
            },
            ExpressionWork::Bind(missing_throw),
            emit(FinalOpcode::IteratorCall, Operands::U8(2)),
            ExpressionWork::Branch {
                kind: BranchKind::IfTrue,
                target: missing_throw_closed.clone(),
                span,
            },
        ]);
        if async_generator {
            sequence.push(emit(FinalOpcode::Await, Operands::None));
        }
        sequence.extend([
            emit(FinalOpcode::IteratorCheckObject, Operands::None),
            ExpressionWork::Bind(missing_throw_closed),
            emit(FinalOpcode::Drop, Operands::None),
            emit(FinalOpcode::Undefined, Operands::None),
            emit(FinalOpcode::Nip, Operands::None),
            emit(FinalOpcode::Nip, Operands::None),
            emit(FinalOpcode::Nip, Operands::None),
            emit(FinalOpcode::Drop, Operands::None),
            emit(
                FinalOpcode::ThrowError,
                Operands::AtomU8 {
                    atom: AtomPoolIndex::new(0),
                    value: 4,
                },
            ),
            ExpressionWork::Bind(delegate_done),
            emit(FinalOpcode::GetField, Operands::Atom(value_atom)),
            emit(FinalOpcode::Nip, Operands::None),
            emit(FinalOpcode::Nip, Operands::None),
            emit(FinalOpcode::Nip, Operands::None),
        ]);
        work.extend(sequence.into_iter().rev());
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
        let is_script_authority = crate::is_supported_script_root_goal(self.unit.goal());
        let is_object_method = self
            .planned
            .identities
            .node_by_executable
            .get(layout.executable.index())
            .copied()
            .and_then(|node_id| object_method_or_accessor_span(self.unit, node_id))
            .is_some();
        if !executable.is_strict() && !is_script_authority && !is_object_method {
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

    fn plan_arrow_closure(
        &self,
        arrow: &ArrowFunctionExpression<'arena>,
        parent: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let node_id = arrow.node_id.get();
        let child = self
            .planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "nested arrow node has a compiler executable identity",
                span: Some(arrow.span),
            })?;
        self.plan_child_function_closure(child, parent, arrow.span, tree_layout, constants)
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
                self.plan_delete_expression(unary, layout, tree_layout, constants, work)?;
            }
        }
        Ok(())
    }

    /// Lowers `delete` into the pinned `OP_delete` shape.
    ///
    /// `QuickJS` rewrites the preceding member read into a key push followed by
    /// `OP_delete` (`quickjs.c:27395-27437`), so the operand order here is the
    /// base object then the property key. A frame or Realm lexical identifier
    /// is non-deletable and folds to `false`. Realm object bindings and sloppy
    /// unresolved lookup retain `OP_delete_var`, allowing the runtime Global
    /// Environment Record to observe bindings and configurable properties
    /// installed by earlier Scripts.
    fn plan_delete_expression<'expression>(
        &self,
        unary: &'expression UnaryExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
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
            Expression::Identifier(identifier) => {
                let reference = self.lowered_reference(
                    identifier.reference_id.get(),
                    identifier.span,
                    layout,
                    tree_layout,
                )?;
                let opcode = match reference {
                    LoweredReference::Frame { .. } => {
                        PlannedInstruction::new(FinalOpcode::PushFalse, Operands::None, unary.span)
                    }
                    LoweredReference::RealmGlobal { global, .. } => {
                        let binding = tree_layout.realm_globals.binding(global).ok_or(
                            LeafCompilationError::SemanticInvariant {
                                invariant: "deleted realm-global binding exists",
                                span: Some(identifier.span),
                            },
                        )?;
                        if matches!(
                            binding.policy.kind(),
                            quickjs_bytecode::CompilerBindingKind::Let
                                | quickjs_bytecode::CompilerBindingKind::Const
                        ) {
                            PlannedInstruction::new(
                                FinalOpcode::PushFalse,
                                Operands::None,
                                unary.span,
                            )
                        } else {
                            PlannedInstruction::new(
                                FinalOpcode::DeleteVar,
                                Operands::Atom(constants.metadata_atom_index(
                                    CompiledMetadataAtomKey::RealmGlobal(global),
                                )?),
                                unary.span,
                            )
                        }
                    }
                };
                work.push(ExpressionWork::Emit(opcode));
                Ok(())
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
