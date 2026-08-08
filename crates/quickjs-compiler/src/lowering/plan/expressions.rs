use super::super::{
    ArrayExpression, ArrayExpressionElement, ArrowFunctionExpression, AssignmentExpression,
    AssignmentOperator, AssignmentTarget, AstKind, AtomPoolIndex, BinaryOperator, BindingId,
    BranchKind, CallExpression, ChainElement, ChainExpression, Class, ClassElement,
    CompilationContext, CompiledConstantPool, CompiledMetadataAtomKey, CompilerClosureBinding,
    CompilerLabel, ComputedMemberExpression, ConditionalExpression, DeclarationKind, ExecutableId,
    ExecutableKind, Expression, FinalOpcode, FrameLayout, FrameSlot, Function,
    FunctionPlanningContext, FunctionTreeLayout, GetSpan, IdentifierReference,
    InitializationPolicy, LeafCompilationError, LogicalExpression, LogicalOperator,
    LoweredReference, MethodDefinition, MethodDefinitionKind, NodeId, ObjectExpression,
    ObjectProperty, ObjectPropertyKind, Operands, OxcPropertyKey, PlannedControlFlow,
    PlannedInstruction, PrivateFieldExpression, PrivateInExpression, PropertyDefinition,
    PropertyKind, SequenceExpression, SimpleAssignmentTarget, Span, StatementCompletion,
    StatementControlStack, StatementPlanningState, StatementWork, StaticMemberExpression,
    StoragePlacement, UnaryExpression, UnaryOperator, UnsupportedLeafFeature, UpdateExpression,
    UpdateOperator, compiled_static_property_key, plan_external_put, plan_external_read,
    plan_put_slot, unsupported,
};
use super::abrupt::{AbruptMarker, AbruptMarkerKind};
use super::calls::MemberCallee;
use oxc_ast::ast::{SpreadElement, StaticBlock};
use std::collections::HashSet;

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

pub(in crate::lowering) fn anonymous_class_expression_span(
    mut expression: &Expression<'_>,
) -> Option<Span> {
    while let Expression::ParenthesizedExpression(parenthesized) = expression {
        expression = &parenthesized.expression;
    }
    match expression {
        Expression::ClassExpression(class) if class.id.is_none() => Some(class.span),
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

const fn update_opcode(update: &UpdateExpression<'_>) -> FinalOpcode {
    match (update.operator, update.prefix) {
        (UpdateOperator::Increment, true) => FinalOpcode::Inc,
        (UpdateOperator::Decrement, true) => FinalOpcode::Dec,
        (UpdateOperator::Increment, false) => FinalOpcode::PostInc,
        (UpdateOperator::Decrement, false) => FinalOpcode::PostDec,
    }
}

const fn super_member_update_permutation(prefix: bool) -> FinalOpcode {
    if prefix {
        FinalOpcode::Insert4
    } else {
        // `post_inc` / `post_dec` leave `old, new`; `perm5` changes
        // `receiver, base, key, old, new` into `old, receiver, base, key, new`.
        FinalOpcode::Perm5
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
    SuperPropertyBase {
        span: Span,
        call_receiver: bool,
    },
    InitializeInstanceFields,
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
                } => self.plan_optional_chain(
                    chain,
                    preserve_final_reference,
                    layout,
                    constants,
                    flow,
                    &mut work,
                )?,
                ExpressionWork::CallAfterCallee { call, method } => {
                    Self::plan_call_after_callee(call, method, &mut work)?;
                }
                ExpressionWork::SuperPropertyBase {
                    span,
                    call_receiver,
                } => self.plan_super_property_base(span, call_receiver, layout, flow)?,
                ExpressionWork::InitializeInstanceFields => {
                    self.plan_instance_field_initializations(
                        layout.executable,
                        layout,
                        tree_layout,
                        constants,
                        flow,
                    )?;
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
                        Expression::PrivateFieldExpression(member) => {
                            self.plan_private_member_read(member, layout, &mut work)?;
                        }
                        Expression::PrivateInExpression(private_in) => {
                            self.plan_private_in_expression(private_in, layout, &mut work)?;
                        }
                        Expression::ChainExpression(chain) => {
                            self.plan_optional_chain(
                                chain, false, layout, constants, flow, &mut work,
                            )?;
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
                            self.plan_update_expression(
                                update,
                                layout,
                                tree_layout,
                                constants,
                                &mut work,
                            )?;
                        }
                        Expression::CallExpression(call) => {
                            self.plan_call_expression(call, layout, constants, &mut work)?;
                        }
                        Expression::TaggedTemplateExpression(tagged) => {
                            self.plan_tagged_template_expression(
                                tagged, layout, constants, &mut work,
                            )?;
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
                        Expression::ClassExpression(class) => {
                            self.plan_base_class_expression(
                                class,
                                layout,
                                tree_layout,
                                constants,
                                flow,
                            )?;
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
                            let (opcode, operands) = if self
                                .static_class_initializer_class_for_span(new_target.span)?
                                .is_some()
                            {
                                (FinalOpcode::Undefined, Operands::None)
                            } else {
                                (FinalOpcode::SpecialObject, Operands::U8(3))
                            };
                            flow.emit(PlannedInstruction::new(opcode, operands, new_target.span))?;
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

    pub(in crate::lowering) fn plan_base_class_declaration(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let identifier = class.id.as_ref().ok_or(LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::UnsupportedDeclaration,
            span: class.span,
        })?;
        self.plan_base_class_definition(class, layout, tree_layout, constants, flow)?;
        self.plan_base_class_declaration_binding(identifier, layout, tree_layout, flow)?;
        self.plan_scope_exit(layout.executable, class.scope_id(), layout, flow)
    }

    pub(in crate::lowering) fn plan_base_class_expression(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        self.plan_base_class_definition(class, layout, tree_layout, constants, flow)?;
        self.plan_scope_exit(layout.executable, class.scope_id(), layout, flow)
    }

    /// Lowers the verified class slice: base and derived constructors,
    /// source-level direct `super(...)`, public methods/accessors with either
    /// static or computed names, public static fields with lexical `this`,
    /// `new.target`, and `super` property access, and initializer-free public
    /// instance fields. Computed and initialized public instance fields and
    /// static blocks have their own certified execution paths.
    fn plan_class_heritage(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<bool, LeafCompilationError> {
        let Some(heritage) = &class.super_class else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                class.span,
            ))?;
            return Ok(false);
        };

        // ClassDefinitionEvaluation validates a constructor before it reads
        // `.prototype`, but `null` uses the separate null-prototype path.
        // Keep both values on the operand stack for `define_class`:
        // `[superclass-or-null, prototype-parent-or-null]`.
        self.plan_expression(heritage, layout, tree_layout, constants, &[], flow)?;
        let null_heritage = flow.new_label(heritage.span())?;
        let heritage_ready = flow.new_label(heritage.span())?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            heritage.span(),
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::IsNull,
            Operands::None,
            heritage.span(),
        ))?;
        flow.branch(BranchKind::IfTrue, &null_heritage, heritage.span())?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::CheckCtor,
            Operands::None,
            heritage.span(),
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            heritage.span(),
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::GetField,
            Operands::Atom(constants.class_heritage_prototype_atom_index(class.span)?),
            heritage.span(),
        ))?;
        flow.branch(BranchKind::Goto, &heritage_ready, heritage.span())?;
        flow.bind(&null_heritage)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Null,
            Operands::None,
            heritage.span(),
        ))?;
        flow.bind(&heritage_ready)?;
        Ok(true)
    }

    fn plan_private_member_read<'expression>(
        &self,
        member: &'expression PrivateFieldExpression<'arena>,
        layout: &FrameLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
        }
        let (binding, slot) = self.private_name_binding_for_access(
            member.node_id.get(),
            member.field.name.as_str(),
            member.span,
            layout,
        )?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            slot,
            member.field.span,
        )?));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    /// Plans a private member reference for `CallMethod`: retain the base as
    /// receiver, then resolve the private slot without consulting prototypes.
    pub(in crate::lowering) fn plan_private_member_callee<'expression>(
        &self,
        member: &'expression PrivateFieldExpression<'arena>,
        layout: &FrameLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
        }
        let (binding, slot) = self.private_name_binding_for_access(
            member.node_id.get(),
            member.field.name.as_str(),
            member.span,
            layout,
        )?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            slot,
            member.field.span,
        )?));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member.object.span(),
        )));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn private_name_binding_for_access(
        &self,
        access_node: NodeId,
        name: &str,
        span: Span,
        layout: &FrameLayout,
    ) -> Result<(BindingId, FrameSlot), LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        for ancestor in nodes.ancestor_ids(access_node) {
            let AstKind::Class(class) = nodes.kind(ancestor) else {
                continue;
            };
            for element in &class.body.body {
                let (element_node, identifier) = match element {
                    ClassElement::PropertyDefinition(field) => {
                        let OxcPropertyKey::PrivateIdentifier(identifier) = &field.key else {
                            continue;
                        };
                        (field.node_id.get(), identifier)
                    }
                    ClassElement::MethodDefinition(method) => {
                        let OxcPropertyKey::PrivateIdentifier(identifier) = &method.key else {
                            continue;
                        };
                        (method.node_id.get(), identifier)
                    }
                    _ => continue,
                };
                if identifier.name.as_str() != name {
                    continue;
                }
                let binding = self
                    .planned
                    .identities
                    .class_private_name_bindings
                    .get(&element_node)
                    .copied()
                    .ok_or(LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedExpression,
                        span,
                    })?;
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "private field access binding exists",
                        span: Some(span),
                    },
                )?;
                if storage.policy().kind() != DeclarationKind::ClassPrivateName
                    || storage.placement() != StoragePlacement::Local
                {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "private field access uses immutable class private-name storage",
                        span: Some(span),
                    });
                }
                let slot = layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "private field access binding has a frame slot",
                        span: Some(span),
                    })?;
                return Ok((binding, slot));
            }
        }
        unsupported(UnsupportedLeafFeature::UnsupportedExpression, span)
    }

    fn plan_private_in_expression<'expression>(
        &self,
        private_in: &'expression PrivateInExpression<'arena>,
        layout: &FrameLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let (binding, slot) = self.private_name_binding_for_access(
            private_in.node_id.get(),
            private_in.left.name.as_str(),
            private_in.span,
            layout,
        )?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PrivateIn,
            Operands::None,
            private_in.span,
        )));
        work.push(ExpressionWork::Visit(&private_in.right));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            slot,
            private_in.left.span,
        )?));
        Ok(())
    }

    fn plan_private_member_assignment<'expression>(
        &self,
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression PrivateFieldExpression<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let (binding, slot) = self.private_name_binding_for_access(
            member.node_id.get(),
            member.field.name.as_str(),
            member.span,
            layout,
        )?;
        match assignment.operator {
            AssignmentOperator::Assign => {
                self.plan_private_simple_assignment(assignment, member, binding, slot, work)?;
            }
            AssignmentOperator::LogicalOr
            | AssignmentOperator::LogicalAnd
            | AssignmentOperator::LogicalNullish => {
                self.plan_private_logical_assignment(
                    assignment, member, binding, slot, flow, work,
                )?;
            }
            operator => {
                self.plan_private_compound_assignment(
                    assignment, member, binding, slot, operator, work,
                )?;
            }
        }
        Ok(())
    }

    fn plan_private_simple_assignment<'expression>(
        &self,
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression PrivateFieldExpression<'arena>,
        binding: BindingId,
        slot: FrameSlot,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        // Preserve the RHS as the assignment completion below the
        // receiver/name/value triple consumed by `put_private_field`.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert3,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            slot,
            member.field.span,
        )?));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_private_logical_assignment<'expression>(
        &self,
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression PrivateFieldExpression<'arena>,
        binding: BindingId,
        slot: FrameSlot,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let short_circuit = flow.new_label(assignment.span)?;
        let done = flow.new_label(assignment.span)?;
        let branch_kind = if assignment.operator == AssignmentOperator::LogicalOr {
            BranchKind::IfTrue
        } else {
            BranchKind::IfFalse
        };

        // `dup2; get_private_field` retains the receiver/name pair. A
        // short-circuit removes it and returns `old`; the write path drops
        // `old`, evaluates the RHS, then returns the stored RHS.
        work.push(ExpressionWork::Bind(done.clone()));
        for opcode in [
            FinalOpcode::Drop,
            FinalOpcode::Swap,
            FinalOpcode::Drop,
            FinalOpcode::Swap,
        ] {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                opcode,
                Operands::None,
                member.span,
            )));
        }
        work.push(ExpressionWork::Bind(short_circuit.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: assignment.span,
        });
        Self::plan_private_write_after_value(assignment, member, work);
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Branch {
            kind: branch_kind,
            target: short_circuit,
            span: assignment.span,
        });
        if assignment.operator == AssignmentOperator::LogicalNullish {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::IsUndefinedOrNull,
                Operands::None,
                member.span,
            )));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member.span,
        )));
        self.plan_private_read_reference(member, binding, slot, work)?;
        Ok(())
    }

    fn plan_private_compound_assignment<'expression>(
        &self,
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression PrivateFieldExpression<'arena>,
        binding: BindingId,
        slot: FrameSlot,
        operator: AssignmentOperator,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let binary =
            operator
                .to_binary_operator()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "nonlogical private assignment has a binary operator",
                    span: Some(assignment.span),
                })?;
        Self::plan_private_compound_write_after_value(
            assignment,
            member,
            binary_opcode(binary),
            work,
        );
        self.plan_private_read_reference(member, binding, slot, work)
    }

    fn plan_private_compound_write_after_value<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression PrivateFieldExpression<'arena>,
        binary: FinalOpcode,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) {
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert3,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            binary,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
    }

    fn plan_private_write_after_value<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression PrivateFieldExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) {
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert3,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
    }

    fn plan_private_read_reference<'expression>(
        &self,
        member: &'expression PrivateFieldExpression<'arena>,
        binding: BindingId,
        slot: FrameSlot,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup2,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            slot,
            member.field.span,
        )?));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_private_member_update<'expression>(
        &self,
        update: &'expression UpdateExpression<'arena>,
        member: &'expression PrivateFieldExpression<'arena>,
        layout: &FrameLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        }
        let (binding, slot) = self.private_name_binding_for_access(
            member.node_id.get(),
            member.field.name.as_str(),
            member.span,
            layout,
        )?;
        // `dup2; get_private_field` preserves `[receiver, name]`. A prefix
        // update duplicates the new value before the private store; a postfix
        // update leaves `old, new`, and `perm4` changes
        // `[receiver, name, old, new]` into `[old, receiver, name, new]`.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            if update.prefix {
                FinalOpcode::Insert3
            } else {
                FinalOpcode::Perm4
            },
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            update_opcode(update),
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetPrivateField,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup2,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            slot,
            member.field.span,
        )?));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "class definition lowering preserves its stack topology in one audited sequence"
    )]
    fn plan_base_class_definition(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if !class.decorators.is_empty() {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, class.span);
        }

        let mut constructor = None;
        for element in &class.body.body {
            match element {
                ClassElement::MethodDefinition(method) => {
                    Self::validate_base_class_method(method)?;
                    if method.kind == MethodDefinitionKind::Constructor
                        && constructor.replace(method.as_ref()).is_some()
                    {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "class has at most one constructor method",
                            span: Some(method.span),
                        });
                    }
                }
                ClassElement::PropertyDefinition(field) => {
                    Self::validate_base_class_field(field)?;
                }
                ClassElement::StaticBlock(_) => {}
                _ => return unsupported(UnsupportedLeafFeature::UnsupportedBody, element.span()),
            }
        }
        self.plan_base_class_name_scope_entry(class, layout, flow)?;
        let has_heritage = self.plan_class_heritage(class, layout, tree_layout, constants, flow)?;
        if let Some(constructor) = constructor {
            flow.emit(self.plan_function_closure(
                &constructor.value,
                layout.executable,
                tree_layout,
                constants,
            )?)?;
        } else {
            let child = self
                .planned
                .identities
                .default_class_constructors
                .get(&class.node_id())
                .copied()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "base class without a constructor has a synthesized template",
                    span: Some(class.body.span),
                })?;
            flow.emit(self.plan_child_function_closure(
                child,
                layout.executable,
                class.body.span,
                tree_layout,
                constants,
            )?)?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefineClass,
            Operands::AtomU8 {
                atom: constants.property_atom_index(class.span)?,
                value: u8::from(has_heritage),
            },
            class.span,
        ))?;
        if self.class_uses_computed_name_context(class) {
            // Before elements install methods or fields, the evaluated key is
            // immediately below the fresh constructor/prototype pair:
            // `[prefix, key, constructor, prototype]`. Rotate only those
            // values so `set_name_computed` sees `[key, constructor]`, then
            // restore the class-definition stack shape.
            for opcode in [
                FinalOpcode::Swap,
                FinalOpcode::Perm3,
                FinalOpcode::SetNameComputed,
                FinalOpcode::Perm3,
                FinalOpcode::Swap,
            ] {
                flow.emit(PlannedInstruction::new(opcode, Operands::None, class.span))?;
            }
        }
        if class.id.is_some() {
            self.plan_base_class_name_initialization(class, layout, flow)?;
        }
        self.plan_base_class_private_name_initializations(class, layout, constants, flow)?;
        self.plan_base_class_static_receiver_initialization(class, layout, flow)?;

        for element in &class.body.body {
            match element {
                ClassElement::MethodDefinition(method) => {
                    self.plan_base_class_method(method, layout, tree_layout, constants, flow)?;
                }
                ClassElement::PropertyDefinition(field) => {
                    if field.computed {
                        self.plan_base_class_computed_field_key(
                            field,
                            layout,
                            tree_layout,
                            constants,
                            flow,
                        )?;
                    }
                }
                ClassElement::StaticBlock(_) => {}
                _ => {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "validated class body has only methods, fields, and static blocks",
                        span: Some(element.span()),
                    });
                }
            }
        }
        for element in &class.body.body {
            match element {
                ClassElement::PropertyDefinition(field) if field.r#static => {
                    self.plan_base_class_static_field(field, layout, tree_layout, constants, flow)?;
                }
                ClassElement::StaticBlock(block) => {
                    self.plan_base_class_static_block(
                        block,
                        class,
                        layout,
                        tree_layout,
                        constants,
                        flow,
                    )?;
                }
                ClassElement::MethodDefinition(_) | ClassElement::PropertyDefinition(_) => {}
                _ => {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "validated class body has only methods, fields, and static blocks",
                        span: Some(element.span()),
                    });
                }
            }
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            class.span,
        ))
    }

    fn class_uses_computed_name_context(&self, class: &Class<'arena>) -> bool {
        if class.id.is_some() {
            return false;
        }
        let nodes = self.unit.semantic().nodes();
        let mut parent = nodes.parent_id(class.node_id.get());
        while matches!(nodes.kind(parent), AstKind::ParenthesizedExpression(_)) {
            parent = nodes.parent_id(parent);
        }
        match nodes.kind(parent) {
            AstKind::ObjectProperty(property) => property.computed,
            AstKind::PropertyDefinition(field) => field.computed,
            _ => false,
        }
    }

    fn validate_base_class_method(
        method: &MethodDefinition<'arena>,
    ) -> Result<(), LeafCompilationError> {
        if !method.decorators.is_empty() {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, method.span);
        }
        if method.kind == MethodDefinitionKind::Constructor {
            if method.r#static || method.computed {
                return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, method.span);
            }
            return Ok(());
        }
        if matches!(method.key, OxcPropertyKey::PrivateIdentifier(_)) {
            if !matches!(
                method.kind,
                MethodDefinitionKind::Method
                    | MethodDefinitionKind::Get
                    | MethodDefinitionKind::Set
            ) || method.computed
            {
                return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, method.span);
            }
            return Ok(());
        }
        if method.computed {
            if method.key.as_expression().is_none() {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedDeclaration,
                    method.key.span(),
                );
            }
            return Ok(());
        }
        if compiled_static_property_key(&method.key)?.is_none() {
            return Err(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                span: method.key.span(),
            });
        }
        Ok(())
    }

    fn validate_base_class_field(
        field: &PropertyDefinition<'arena>,
    ) -> Result<(), LeafCompilationError> {
        if !field.decorators.is_empty() {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, field.span);
        }
        if matches!(field.key, OxcPropertyKey::PrivateIdentifier(_)) {
            return Ok(());
        }
        if !field.r#static {
            if field.computed {
                field
                    .key
                    .as_expression()
                    .ok_or(LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                        span: field.key.span(),
                    })?;
                return Ok(());
            }
            compiled_static_property_key(&field.key)?.ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                span: field.key.span(),
            })?;
            return Ok(());
        }
        if field.computed {
            if field.key.as_expression().is_none() {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedDeclaration,
                    field.key.span(),
                );
            }
        } else {
            compiled_static_property_key(&field.key)?.ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                span: field.key.span(),
            })?;
        }
        if field.value.is_none() {
            return Ok(());
        }
        Ok(())
    }

    fn plan_base_class_private_name_initializations(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let mut initialized = HashSet::new();
        for element in &class.body.body {
            let (node_id, identifier) = match element {
                ClassElement::PropertyDefinition(field) => {
                    let OxcPropertyKey::PrivateIdentifier(identifier) = &field.key else {
                        continue;
                    };
                    (field.node_id.get(), identifier)
                }
                ClassElement::MethodDefinition(method) => {
                    let OxcPropertyKey::PrivateIdentifier(identifier) = &method.key else {
                        continue;
                    };
                    (method.node_id.get(), identifier)
                }
                _ => continue,
            };
            let binding = self
                .planned
                .identities
                .class_private_name_bindings
                .get(&node_id)
                .copied()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "private element has a class-scope name binding",
                    span: Some(identifier.span),
                })?;
            if !initialized.insert(binding) {
                continue;
            }
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "private element name binding exists",
                    span: Some(identifier.span),
                },
            )?;
            if storage.executable() != layout.executable
                || storage.placement() != StoragePlacement::Local
                || storage.policy().kind() != DeclarationKind::ClassPrivateName
                || !storage.policy().has_temporal_dead_zone()
            {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "private element name is lexical class local storage",
                    span: Some(identifier.span),
                });
            }
            let FrameSlot::Local(slot) =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "private element name binding has a local frame slot",
                        span: Some(identifier.span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "private element name binding uses local storage",
                    span: Some(identifier.span),
                });
            };
            flow.emit(PlannedInstruction::new(
                FinalOpcode::PrivateSymbol,
                Operands::Atom(constants.property_atom_index(identifier.span)?),
                identifier.span,
            ))?;
            flow.emit(plan_put_slot(FrameSlot::Local(slot), identifier.span))?;
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "class method lowering keeps public and private class-definition stack contracts in one auditable sequence"
    )]
    fn plan_base_class_method(
        &self,
        method: &MethodDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if method.kind == MethodDefinitionKind::Constructor {
            return Ok(());
        }
        let is_private_method = matches!(method.key, OxcPropertyKey::PrivateIdentifier(_));
        if is_private_method {
            if method.r#static {
                return self.plan_base_class_static_private_method(
                    method,
                    layout,
                    tree_layout,
                    constants,
                    flow,
                );
            }
            let OxcPropertyKey::PrivateIdentifier(identifier) = &method.key else {
                unreachable!("private method key is PrivateIdentifier");
            };
            let (_, function_slot) = self.private_class_method_function_binding(method, layout)?;
            let FrameSlot::Local(_) = function_slot else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "private method closure is stored in its class local frame",
                    span: Some(method.span),
                });
            };
            flow.emit(self.plan_function_closure(
                &method.value,
                layout.executable,
                tree_layout,
                constants,
            )?)?;
            // `set_home_object` consumes `[function, home]` and preserves that
            // order. The class-definition stack is `[constructor, prototype,
            // function]`, so swap twice to retain its canonical shape.
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                method.span,
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SetHomeObject,
                Operands::None,
                method.span,
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                method.span,
            ))?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SetName,
                Operands::Atom(constants.property_atom_index(identifier.span)?),
                method.span,
            ))?;
            flow.emit(plan_put_slot(function_slot, identifier.span))?;
            return Ok(());
        }
        let flags = match method.kind {
            MethodDefinitionKind::Method => 0,
            MethodDefinitionKind::Get => 1,
            MethodDefinitionKind::Set => 2,
            MethodDefinitionKind::Constructor => unreachable!("constructors were skipped"),
        };
        if method.computed {
            let key = method
                .key
                .as_expression()
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                    span: method.key.span(),
                })?;
            if method.r#static {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    method.span,
                ))?;
            }
            self.plan_expression(key, layout, tree_layout, constants, &[], flow)?;
            flow.emit(self.plan_function_closure(
                &method.value,
                layout.executable,
                tree_layout,
                constants,
            )?)?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::DefineMethodComputed,
                Operands::U8(flags),
                method.span,
            ))?;
            if method.r#static {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    method.span,
                ))?;
            }
            return Ok(());
        }
        let key = compiled_static_property_key(&method.key)?.ok_or(
            LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                span: method.key.span(),
            },
        )?;
        if method.r#static {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                method.span,
            ))?;
        }
        flow.emit(self.plan_function_closure(
            &method.value,
            layout.executable,
            tree_layout,
            constants,
        )?)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefineMethod,
            Operands::AtomU8 {
                atom: constants.property_atom_index(key.span)?,
                value: flags,
            },
            method.span,
        ))?;
        if method.r#static {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                method.span,
            ))?;
        }
        Ok(())
    }

    fn plan_base_class_static_private_method(
        &self,
        method: &MethodDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if !method.r#static
            || !matches!(
                method.kind,
                MethodDefinitionKind::Method
                    | MethodDefinitionKind::Get
                    | MethodDefinitionKind::Set
            )
            || !matches!(method.key, OxcPropertyKey::PrivateIdentifier(_))
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "static private-method planner receives a static private method",
                span: Some(method.span),
            });
        }
        let OxcPropertyKey::PrivateIdentifier(identifier) = &method.key else {
            unreachable!("static private method key is PrivateIdentifier");
        };
        let (name_binding, name_slot) = self.private_class_method_name_binding(method, layout)?;
        let (function_binding, function_slot) =
            self.private_class_method_function_binding(method, layout)?;
        let (FrameSlot::Local(_), FrameSlot::Local(_)) = (name_slot, function_slot) else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "static private method name and closure use class local storage",
                span: Some(method.span),
            });
        };
        flow.emit(self.plan_function_closure(
            &method.value,
            layout.executable,
            tree_layout,
            constants,
        )?)?;
        // Reorder `[constructor, prototype, function]` into
        // `[prototype, function, constructor]` so `set_home_object` records
        // the constructor (the static method home object), then restore the
        // canonical class-definition stack before storing the closure.
        for opcode in [
            FinalOpcode::Swap,
            FinalOpcode::Perm3,
            FinalOpcode::Swap,
            FinalOpcode::Perm3,
            FinalOpcode::SetHomeObject,
            FinalOpcode::Perm3,
            FinalOpcode::Swap,
            FinalOpcode::Perm3,
            FinalOpcode::Swap,
        ] {
            flow.emit(PlannedInstruction::new(opcode, Operands::None, method.span))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::SetName,
            Operands::Atom(constants.property_atom_index(identifier.span)?),
            method.span,
        ))?;
        flow.emit(plan_put_slot(function_slot, identifier.span))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            method.span,
        ))?;
        flow.emit(self.plan_read_slot(name_binding, name_slot, method.key.span())?)?;
        flow.emit(self.plan_read_slot(function_binding, function_slot, method.key.span())?)?;
        let private_element_kind = match method.kind {
            MethodDefinitionKind::Method => 1,
            MethodDefinitionKind::Get => 2,
            MethodDefinitionKind::Set => 3,
            MethodDefinitionKind::Constructor => unreachable!("constructors were skipped"),
        };
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefinePrivateField,
            Operands::U8(private_element_kind),
            method.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            method.span,
        ))
    }

    fn plan_base_class_static_field(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if matches!(field.key, OxcPropertyKey::PrivateIdentifier(_)) {
            return self.plan_base_class_static_private_field(
                field,
                layout,
                tree_layout,
                constants,
                flow,
            );
        }
        if field.computed {
            return self.plan_base_class_computed_static_field(
                field,
                layout,
                tree_layout,
                constants,
                flow,
            );
        }
        let key =
            compiled_static_property_key(&field.key)?.ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                span: field.key.span(),
            })?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            field.span,
        ))?;
        if let Some(value) = &field.value {
            let inferred_name = Self::plan_inferred_static_property_name_for_initializer(
                value,
                constants.property_atom_index(key.span)?,
            )?;
            self.plan_expression(value, layout, tree_layout, constants, &[], flow)?;
            if let Some(inferred_name) = inferred_name {
                flow.emit(inferred_name)?;
            }
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefineField,
            Operands::Atom(constants.property_atom_index(key.span)?),
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            field.span,
        ))
    }

    fn plan_base_class_static_private_field(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if !field.r#static || !matches!(field.key, OxcPropertyKey::PrivateIdentifier(_)) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "static private-field planner receives a static private field",
                span: Some(field.span),
            });
        }
        let (binding, slot) = self.private_class_field_name_binding(field, layout)?;
        let FrameSlot::Local(_) = slot else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "static private field name is stored in its class local frame",
                span: Some(field.key.span()),
            });
        };
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            field.span,
        ))?;
        flow.emit(self.plan_read_slot(binding, slot, field.key.span())?)?;
        if let Some(value) = &field.value {
            let inferred_name = Self::plan_inferred_static_property_name_for_initializer(
                value,
                constants.property_atom_index(field.key.span())?,
            )?;
            self.plan_expression(value, layout, tree_layout, constants, &[], flow)?;
            if let Some(inferred_name) = inferred_name {
                flow.emit(inferred_name)?;
            }
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefinePrivateField,
            Operands::U8(0),
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            field.span,
        ))
    }

    fn plan_base_class_static_block(
        &self,
        block: &StaticBlock<'arena>,
        class: &Class<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(block.scope_id.get(), block.node_id.get(), block.span)?;
        let planning = FunctionPlanningContext {
            executable: layout.executable,
            layout,
            tree_layout,
            constants,
        };
        let mut state = StatementPlanningState {
            work: vec![
                StatementWork::PopScope(scope),
                StatementWork::VisitList {
                    statements: &block.body,
                    next: 0,
                },
                StatementWork::PushScope {
                    scope,
                    creator: block.node_id.get(),
                    span: block.span,
                },
            ],
            // The class scope is already active for the surrounding class
            // definition. It provides the synthetic lexical class receiver
            // used by `this` and `super` in the block.
            active_scopes: vec![class.scope_id()],
            controls: StatementControlStack::default(),
            abrupt_markers: Vec::new(),
            completion: StatementCompletion::Discard,
        };
        while let Some(task) = state.work.pop() {
            self.process_statement_work(task, block.span, &planning, flow, &mut state)?;
        }
        if state.active_scopes.as_slice() != [class.scope_id()]
            || !state.controls.is_empty()
            || !state.abrupt_markers.is_empty()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "static block closes its scope and control regions",
                span: Some(block.span),
            });
        }
        Ok(())
    }

    fn plan_base_class_computed_static_field(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let (binding, slot) = self.computed_class_field_key_binding(field, layout)?;
        let FrameSlot::Local(_) = slot else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "class definition retains its computed static field key locally",
                span: Some(field.key.span()),
            });
        };
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            field.span,
        ))?;
        flow.emit(self.plan_read_slot(binding, slot, field.key.span())?)?;
        if let Some(value) = &field.value {
            let inferred_name = Self::plan_inferred_computed_property_name_for_initializer(value)?;
            self.plan_expression(value, layout, tree_layout, constants, &[], flow)?;
            if let Some(inferred_name) = inferred_name {
                flow.emit(inferred_name)?;
            }
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefineArrayEl,
            Operands::None,
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            field.span,
        ))
    }

    fn plan_base_class_computed_field_key(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let key = field
            .key
            .as_expression()
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                span: field.key.span(),
            })?;
        let (binding, slot) = self.computed_class_field_key_binding(field, layout)?;
        let FrameSlot::Local(_) = slot else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "class definition stores each computed field key locally",
                span: Some(field.key.span()),
            });
        };
        self.plan_expression(key, layout, tree_layout, constants, &[], flow)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ToPropKey,
            Operands::None,
            field.key.span(),
        ))?;
        flow.emit(plan_put_slot(slot, field.key.span()))?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "computed class-field key binding exists after planning",
                    span: Some(field.key.span()),
                })?;
        if storage.policy().kind() != DeclarationKind::ClassFieldKey {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "computed class-field key writes only its synthetic binding",
                span: Some(field.key.span()),
            });
        }
        Ok(())
    }

    pub(in crate::lowering) fn plan_instance_field_initializations(
        &self,
        executable: ExecutableId,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let Some(definitions) = self.instance_field_definitions(executable)? else {
            return Ok(());
        };
        let synthesized_default = matches!(
            self.planned
                .plan
                .executable(executable)
                .ok_or(LeafCompilationError::InvalidExecutable { executable })?
                .kind(),
            ExecutableKind::ClassDefaultConstructor
        );
        for element_node in definitions.elements {
            match self.unit.semantic().nodes().kind(element_node) {
                AstKind::PropertyDefinition(field) => self.plan_instance_field_initialization(
                    field,
                    layout,
                    tree_layout,
                    constants,
                    synthesized_default,
                    flow,
                )?,
                AstKind::MethodDefinition(method) => self
                    .plan_private_instance_method_initialization(
                        method,
                        layout,
                        synthesized_default,
                        flow,
                    )?,
                _ => {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "instance element identity remains a field or private method",
                        span: None,
                    });
                }
            }
        }
        Ok(())
    }

    fn plan_instance_field_initialization(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        synthesized_default: bool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if matches!(field.key, OxcPropertyKey::PrivateIdentifier(_)) {
            return self.plan_private_instance_field_initialization(
                field,
                layout,
                tree_layout,
                constants,
                synthesized_default,
                flow,
            );
        }
        if field.computed {
            return self.plan_computed_instance_field_initialization(
                field,
                layout,
                tree_layout,
                constants,
                synthesized_default,
                flow,
            );
        }
        let key =
            compiled_static_property_key(&field.key)?.ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                span: field.key.span(),
            })?;
        let atom = constants.property_atom_index(key.span)?;
        if synthesized_default {
            // The upstream no-op delimits one deferred field-initializer
            // region in an otherwise source-less default constructor. The
            // bytecode authority checks paired regions before admitting
            // `init_ctor` for a synthesized derived class.
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PushThis,
            Operands::None,
            field.span,
        ))?;
        if let Some(value) = &field.value {
            let inferred_name =
                Self::plan_inferred_static_property_name_for_initializer(value, atom)?;
            self.plan_expression(value, layout, tree_layout, constants, &[], flow)?;
            if let Some(inferred_name) = inferred_name {
                flow.emit(inferred_name)?;
            }
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefineField,
            Operands::Atom(atom),
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            field.span,
        ))?;
        if synthesized_default {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                field.span,
            ))?;
        }
        Ok(())
    }

    fn plan_private_instance_field_initialization(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        synthesized_default: bool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let (binding, slot) = self.private_instance_field_name_binding(field, layout)?;
        let FrameSlot::Capture(_) = slot else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "instance constructor captures its private field name",
                span: Some(field.key.span()),
            });
        };
        if synthesized_default {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PushThis,
            Operands::None,
            field.span,
        ))?;
        flow.emit(self.plan_read_slot(binding, slot, field.key.span())?)?;
        if let Some(value) = &field.value {
            self.plan_expression(value, layout, tree_layout, constants, &[], flow)?;
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefinePrivateField,
            Operands::U8(0),
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            field.span,
        ))?;
        if synthesized_default {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                field.span,
            ))?;
        }
        Ok(())
    }

    fn plan_private_instance_method_initialization(
        &self,
        method: &MethodDefinition<'arena>,
        layout: &FrameLayout,
        synthesized_default: bool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let (name_binding, name_slot) = self.private_class_method_name_binding(method, layout)?;
        let (function_binding, function_slot) =
            self.private_class_method_function_binding(method, layout)?;
        let (FrameSlot::Capture(_), FrameSlot::Capture(_)) = (name_slot, function_slot) else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "instance constructor captures private method name and function",
                span: Some(method.span),
            });
        };
        if synthesized_default {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                method.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PushThis,
            Operands::None,
            method.span,
        ))?;
        flow.emit(self.plan_read_slot(name_binding, name_slot, method.key.span())?)?;
        flow.emit(self.plan_read_slot(function_binding, function_slot, method.key.span())?)?;
        let private_element_kind = match method.kind {
            MethodDefinitionKind::Method => 1,
            MethodDefinitionKind::Get => 2,
            MethodDefinitionKind::Set => 3,
            MethodDefinitionKind::Constructor => unreachable!("constructors were skipped"),
        };
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefinePrivateField,
            Operands::U8(private_element_kind),
            method.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            method.span,
        ))?;
        if synthesized_default {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                method.span,
            ))?;
        }
        Ok(())
    }

    fn plan_computed_instance_field_initialization(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        synthesized_default: bool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let (binding, slot) = self.computed_class_field_key_binding(field, layout)?;
        let FrameSlot::Capture(_) = slot else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "instance constructor captures its computed field key",
                span: Some(field.key.span()),
            });
        };
        if synthesized_default {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::PushThis,
            Operands::None,
            field.span,
        ))?;
        flow.emit(self.plan_read_slot(binding, slot, field.key.span())?)?;
        if let Some(value) = &field.value {
            let inferred_name = Self::plan_inferred_computed_property_name_for_initializer(value)?;
            self.plan_expression(value, layout, tree_layout, constants, &[], flow)?;
            if let Some(inferred_name) = inferred_name {
                flow.emit(inferred_name)?;
            }
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                field.span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::DefineArrayEl,
            Operands::None,
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            field.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            field.span,
        ))?;
        if synthesized_default {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                field.span,
            ))?;
        }
        Ok(())
    }

    fn computed_class_field_key_binding(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
    ) -> Result<(BindingId, FrameSlot), LeafCompilationError> {
        if !field.computed {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "computed class-field key binding belongs to a computed field",
                span: Some(field.span),
            });
        }
        let binding = self
            .planned
            .identities
            .class_field_key_bindings
            .get(&field.node_id.get())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "computed field has a class-scope key binding",
                span: Some(field.key.span()),
            })?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "computed class-field key binding exists",
                    span: Some(field.key.span()),
                })?;
        if storage.policy().kind() != DeclarationKind::ClassFieldKey
            || storage.placement() != StoragePlacement::Local
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "computed class-field key binding is immutable local storage",
                span: Some(field.key.span()),
            });
        }
        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "computed class-field key binding has a frame slot",
                span: Some(field.key.span()),
            })?;
        Ok((binding, slot))
    }

    fn private_instance_field_name_binding(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
    ) -> Result<(BindingId, FrameSlot), LeafCompilationError> {
        if field.r#static || !matches!(field.key, OxcPropertyKey::PrivateIdentifier(_)) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "private field name binding belongs to a private instance field",
                span: Some(field.span),
            });
        }
        self.private_class_field_name_binding(field, layout)
    }

    fn private_class_field_name_binding(
        &self,
        field: &PropertyDefinition<'arena>,
        layout: &FrameLayout,
    ) -> Result<(BindingId, FrameSlot), LeafCompilationError> {
        if !matches!(field.key, OxcPropertyKey::PrivateIdentifier(_)) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "private field name binding belongs to a private class field",
                span: Some(field.span),
            });
        }
        let binding = self
            .planned
            .identities
            .class_private_name_bindings
            .get(&field.node_id.get())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "private class field has a class-scope name binding",
                span: Some(field.key.span()),
            })?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "private class field name binding exists",
                    span: Some(field.key.span()),
                })?;
        if storage.policy().kind() != DeclarationKind::ClassPrivateName
            || storage.placement() != StoragePlacement::Local
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "private class field name is immutable local storage",
                span: Some(field.key.span()),
            });
        }
        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "private class field name binding has a frame slot",
                span: Some(field.key.span()),
            })?;
        Ok((binding, slot))
    }

    fn private_class_method_name_binding(
        &self,
        method: &MethodDefinition<'arena>,
        layout: &FrameLayout,
    ) -> Result<(BindingId, FrameSlot), LeafCompilationError> {
        if !matches!(
            method.kind,
            MethodDefinitionKind::Method | MethodDefinitionKind::Get | MethodDefinitionKind::Set
        ) || !matches!(method.key, OxcPropertyKey::PrivateIdentifier(_))
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "private method name binding belongs to a supported private method",
                span: Some(method.span),
            });
        }
        let binding = self
            .planned
            .identities
            .class_private_name_bindings
            .get(&method.node_id.get())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "private method has a class-scope name binding",
                span: Some(method.key.span()),
            })?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "private method name binding exists",
                    span: Some(method.key.span()),
                })?;
        if storage.policy().kind() != DeclarationKind::ClassPrivateName
            || storage.placement() != StoragePlacement::Local
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "private method name is immutable local storage",
                span: Some(method.key.span()),
            });
        }
        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "private method name binding has a frame slot",
                span: Some(method.key.span()),
            })?;
        Ok((binding, slot))
    }

    fn private_class_method_function_binding(
        &self,
        method: &MethodDefinition<'arena>,
        layout: &FrameLayout,
    ) -> Result<(BindingId, FrameSlot), LeafCompilationError> {
        if !matches!(
            method.kind,
            MethodDefinitionKind::Method | MethodDefinitionKind::Get | MethodDefinitionKind::Set
        ) || !matches!(method.key, OxcPropertyKey::PrivateIdentifier(_))
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "private method function binding belongs to a supported private method",
                span: Some(method.span),
            });
        }
        let binding = self
            .planned
            .identities
            .class_private_method_bindings
            .get(&method.node_id.get())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "private method has a class-scope function binding",
                span: Some(method.key.span()),
            })?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "private method function binding exists",
                    span: Some(method.key.span()),
                })?;
        if storage.policy().kind() != DeclarationKind::ClassPrivateName
            || storage.placement() != StoragePlacement::Local
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "private method function is immutable local storage",
                span: Some(method.key.span()),
            });
        }
        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "private method function binding has a frame slot",
                span: Some(method.key.span()),
            })?;
        Ok((binding, slot))
    }

    fn base_class_name_binding(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
    ) -> Result<super::super::LocalSlot, LeafCompilationError> {
        let binding = self
            .planned
            .identities
            .class_name_bindings
            .get(&class.node_id())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "named class has a synthetic inner-name binding",
                span: Some(class.span),
            })?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "synthetic class-name binding exists",
                    span: Some(class.span),
                })?;
        let FrameSlot::Local(slot) =
            layout
                .slot(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "synthetic class-name binding has a frame slot",
                    span: Some(class.span),
                })?
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "synthetic class-name binding uses local storage",
                span: Some(class.span),
            });
        };
        if storage.executable() != layout.executable
            || storage.placement() != StoragePlacement::Local
            || !storage.policy().has_temporal_dead_zone()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "synthetic class-name binding is lexical local storage",
                span: Some(class.span),
            });
        }
        Ok(slot)
    }

    fn plan_base_class_name_scope_entry(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scope = class.scope_id();
        let bindings = self.planned.plan.bindings_for(layout.executable).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: layout.executable,
            },
        )?;
        let mut slots = Vec::new();
        for storage in bindings {
            if self.scope_for_binding(storage.id())? != scope
                || storage.policy().initialization() != InitializationPolicy::AtDeclaration
                || !storage.policy().has_temporal_dead_zone()
            {
                continue;
            }
            let declaration_span = storage.declaration_spans().first().copied().ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "class-scope lexical binding has a declaration span",
                    span: Some(class.span),
                },
            )?;
            let FrameSlot::Local(slot) =
                layout
                    .slot(storage.id())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "class-scope lexical binding has a frame slot",
                        span: Some(declaration_span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "class-scope lexical binding uses local storage",
                    span: Some(declaration_span),
                });
            };
            slots.push((slot, declaration_span));
        }
        slots.sort_unstable_by_key(|(slot, _)| slot.index());
        for (slot, declaration_span) in slots.into_iter().rev() {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(slot.index()),
                declaration_span,
            ))?;
        }
        Ok(())
    }

    fn plan_base_class_name_initialization(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let slot = self.base_class_name_binding(class, layout)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            class.span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            class.span,
        ))?;
        flow.emit(plan_put_slot(FrameSlot::Local(slot), class.span))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            class.span,
        ))
    }

    fn plan_base_class_static_receiver_initialization(
        &self,
        class: &Class<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let Some(binding) = self
            .planned
            .identities
            .class_static_receiver_bindings
            .get(&class.node_id())
            .copied()
        else {
            return Ok(());
        };
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "class static-receiver binding exists",
                    span: Some(class.span),
                })?;
        if storage.executable() != layout.executable
            || storage.placement() != StoragePlacement::Local
            || storage.policy().kind() != DeclarationKind::ClassStaticReceiver
            || !storage.policy().has_temporal_dead_zone()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "class static-receiver binding is lexical local storage",
                span: Some(class.span),
            });
        }
        let FrameSlot::Local(slot) =
            layout
                .slot(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "class static-receiver binding has a frame slot",
                    span: Some(class.span),
                })?
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "class static-receiver binding uses local storage",
                span: Some(class.span),
            });
        };
        // The post-define stack is `[constructor, prototype]`. Preserve that
        // exact pair while initializing the class-scoped lexical receiver.
        for instruction in [
            PlannedInstruction::new(FinalOpcode::Swap, Operands::None, class.span),
            PlannedInstruction::new(FinalOpcode::Dup, Operands::None, class.span),
            plan_put_slot(FrameSlot::Local(slot), class.span),
            PlannedInstruction::new(FinalOpcode::Swap, Operands::None, class.span),
        ] {
            flow.emit(instruction)?;
        }
        Ok(())
    }

    fn plan_base_class_declaration_binding(
        &self,
        identifier: &super::super::BindingIdentifier<'arena>,
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
                    invariant: "class declaration binding exists",
                    span: Some(identifier.span),
                })?;
        match storage.placement() {
            StoragePlacement::GlobalLexical => {
                self.validate_realm_global_class_declaration(storage, identifier.span)?;
                let global = tree_layout.realm_globals.for_binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "Program class binding has a realm-global identity",
                        span: Some(identifier.span),
                    },
                )?;
                let slot = tree_layout.realm_globals.closure_slot(
                    &self.planned.plan,
                    layout.executable,
                    global,
                )?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::PutVarInit,
                    Operands::VarRef(slot),
                    identifier.span,
                ))
            }
            StoragePlacement::Local => {
                let slot = layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedBinding,
                        span: identifier.span,
                    })?;
                self.validate_class_declaration_storage(binding, slot, identifier.span)?;
                flow.emit(plan_put_slot(slot, identifier.span))
            }
            StoragePlacement::Argument { .. }
            | StoragePlacement::GlobalObject
            | StoragePlacement::ModuleLocal
            | StoragePlacement::ModuleImport => {
                unsupported(UnsupportedLeafFeature::UnsupportedBinding, identifier.span)
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the complete chain-level short-circuit schedule stays visible in execution order"
    )]
    fn plan_optional_chain<'expression>(
        &self,
        chain: &'expression ChainExpression<'arena>,
        preserve_final_reference: bool,
        layout: &FrameLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        enum Step<'expression, 'arena> {
            Static(&'expression StaticMemberExpression<'arena>),
            Computed(&'expression ComputedMemberExpression<'arena>),
            Private(&'expression PrivateFieldExpression<'arena>),
            Call(&'expression CallExpression<'arena>),
        }

        impl Step<'_, '_> {
            const fn optional(&self) -> bool {
                match self {
                    Self::Static(member) => member.optional,
                    Self::Computed(member) => member.optional,
                    Self::Private(member) => member.optional,
                    Self::Call(call) => call.optional,
                }
            }

            const fn span(&self) -> Span {
                match self {
                    Self::Static(member) => member.span,
                    Self::Computed(member) => member.span,
                    Self::Private(member) => member.span,
                    Self::Call(call) => call.span,
                }
            }

            const fn is_member(&self) -> bool {
                matches!(self, Self::Static(_) | Self::Computed(_) | Self::Private(_))
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
            ChainElement::PrivateFieldExpression(member) => {
                steps.push(Step::Private(member));
                &member.object
            }
            ChainElement::TSNonNullExpression(_) => {
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
                Expression::PrivateFieldExpression(member) => {
                    steps.push(Step::Private(member));
                    root = &member.object;
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
            Some(MemberCallee::Private(member)) => {
                let (binding, slot) = self.private_name_binding_for_access(
                    member.node_id.get(),
                    member.field.name.as_str(),
                    member.span,
                    layout,
                )?;
                planned.push(ExpressionWork::Visit(&member.object));
                planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    member.object.span(),
                )));
                planned.push(ExpressionWork::Emit(self.plan_read_slot(
                    binding,
                    slot,
                    member.field.span,
                )?));
                planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetPrivateField,
                    Operands::None,
                    member.span,
                )));
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
                Step::Private(member) => {
                    let preserve_receiver = steps.get(index + 1).is_some_and(Step::is_call)
                        || (final_step && preserve_final_reference);
                    let (binding, slot) = self.private_name_binding_for_access(
                        member.node_id.get(),
                        member.field.name.as_str(),
                        member.span,
                        layout,
                    )?;
                    if preserve_receiver {
                        planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                            FinalOpcode::Dup,
                            Operands::None,
                            member.object.span(),
                        )));
                    }
                    planned.push(ExpressionWork::Emit(self.plan_read_slot(
                        binding,
                        slot,
                        member.field.span,
                    )?));
                    planned.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetPrivateField,
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
        if let Some(receiver) = self.static_class_receiver_read(span, layout)? {
            return Ok(receiver);
        }
        Ok(PlannedInstruction::new(
            FinalOpcode::PushThis,
            Operands::None,
            span,
        ))
    }

    fn static_class_receiver_read(
        &self,
        span: Span,
        layout: &FrameLayout,
    ) -> Result<Option<PlannedInstruction>, LeafCompilationError> {
        let Some(class_node) = self.static_class_initializer_class_for_span(span)? else {
            return Ok(None);
        };
        let binding = self
            .planned
            .identities
            .class_static_receiver_bindings
            .get(&class_node)
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "static field lexical receiver has a class receiver binding",
                span: Some(span),
            })?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "class static-receiver binding exists",
                    span: Some(span),
                })?;
        if storage.policy().kind() != DeclarationKind::ClassStaticReceiver
            || storage.placement() != StoragePlacement::Local
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "static field lexical receiver uses its immutable class receiver binding",
                span: Some(span),
            });
        }
        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "static field lexical receiver binding has a frame slot",
                span: Some(span),
            })?;
        Ok(Some(self.plan_read_slot(binding, slot, span)?))
    }

    fn plan_super_property_base(
        &self,
        span: Span,
        call_receiver: bool,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let Some(receiver) = self.static_class_receiver_read(span, layout)? {
            flow.emit(receiver)?;
            if call_receiver {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    span,
                ))?;
            }
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                span,
            ))?;
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::PushThis,
                Operands::None,
                span,
            ))?;
            if call_receiver {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    span,
                ))?;
            }
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SpecialObject,
                Operands::U8(5),
                span,
            ))?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::GetSuper,
            Operands::None,
            span,
        ))?;
        Ok(())
    }

    fn static_class_initializer_class_for_span(
        &self,
        span: Span,
    ) -> Result<Option<NodeId>, LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        for (node_id, node) in nodes.iter_enumerated() {
            let node_span = match node.kind() {
                AstKind::ThisExpression(expression) => expression.span,
                AstKind::NewTarget(expression) => expression.span,
                AstKind::Super(expression) => expression.span,
                _ => continue,
            };
            if node_span == span {
                return self.static_class_initializer_class_for_node(node_id);
            }
        }
        Err(LeafCompilationError::SemanticInvariant {
            invariant: "lexical receiver or new.target expression retains a semantic node",
            span: Some(span),
        })
    }

    fn static_class_initializer_class_for_node(
        &self,
        node_id: NodeId,
    ) -> Result<Option<NodeId>, LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        let node_span = nodes.kind(node_id).span();
        for ancestor in nodes.ancestor_ids(node_id) {
            match nodes.kind(ancestor) {
                AstKind::Function(_) => return Ok(None),
                AstKind::PropertyDefinition(field)
                    if field.r#static
                        && field.value.as_ref().is_some_and(|value| {
                            value.span().start <= node_span.start
                                && node_span.end <= value.span().end
                        }) =>
                {
                    let AstKind::ClassBody(body) = nodes.parent_kind(field.node_id.get()) else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "static field belongs to a class body",
                            span: Some(field.span),
                        });
                    };
                    let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "static field class body belongs to a class",
                            span: Some(body.span),
                        });
                    };
                    return Ok(Some(class.node_id.get()));
                }
                AstKind::StaticBlock(block) => {
                    let AstKind::ClassBody(body) = nodes.parent_kind(block.node_id.get()) else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "static block belongs to a class body",
                            span: Some(block.span),
                        });
                    };
                    let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "static block class body belongs to a class",
                            span: Some(body.span),
                        });
                    };
                    return Ok(Some(class.node_id.get()));
                }
                _ => {}
            }
        }
        Ok(None)
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
                let ObjectPropertyKind::SpreadProperty(spread) = property else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "object property kind is either a property or spread element",
                        span: Some(property.span()),
                    });
                };
                Self::plan_object_spread_property(spread, work);
                continue;
            };
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
            if !property.shorthand && key.value.latin1_units() == Some(b"__proto__") {
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

    fn plan_object_spread_property<'expression>(
        spread: &'expression SpreadElement<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) {
        // CopyDataProperties retains its three operands. Keep the literal
        // target below the source and an `undefined` exclusion marker, then
        // discard the latter two after the resumable copy completes. The work
        // stack is LIFO, so this evaluates every spread argument in source
        // order with ordinary properties.
        for opcode in [FinalOpcode::Drop, FinalOpcode::Drop] {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                opcode,
                Operands::None,
                spread.span,
            )));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::CopyDataProperties,
            // target depth 2, source depth 1, excluded depth 0.
            Operands::U8(0b0000_0110),
            spread.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            spread.span,
        )));
        work.push(ExpressionWork::Visit(&spread.argument));
    }

    fn plan_inferred_static_property_name_for_initializer(
        initializer: &Expression<'arena>,
        atom: AtomPoolIndex,
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
        Ok(Some(PlannedInstruction::new(
            FinalOpcode::SetName,
            Operands::Atom(atom),
            span,
        )))
    }

    fn plan_inferred_computed_property_name_for_initializer(
        initializer: &Expression<'arena>,
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
        Ok(Some(PlannedInstruction::new(
            FinalOpcode::SetNameComputed,
            Operands::None,
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
            if anonymous_class_expression_span(&property.value).is_some() {
                None
            } else if anonymous_ordinary_function_span(&property.value).is_none() {
                return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
            } else {
                Some(PlannedInstruction::new(
                    FinalOpcode::SetNameComputed,
                    Operands::None,
                    span,
                ))
            }
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
        if let AssignmentTarget::PrivateFieldExpression(member) = &assignment.left {
            return self.plan_private_member_assignment(assignment, member, layout, flow, work);
        }
        if let AssignmentTarget::StaticMemberExpression(member) = &assignment.left {
            if matches!(
                assignment.operator,
                AssignmentOperator::LogicalOr
                    | AssignmentOperator::LogicalAnd
                    | AssignmentOperator::LogicalNullish
            ) {
                return Self::plan_static_member_logical_assignment(
                    assignment, member, constants, flow, work,
                );
            }
            if assignment.operator != AssignmentOperator::Assign {
                return Self::plan_static_member_compound_assignment(
                    assignment, member, constants, work,
                );
            }
            return Self::plan_static_member_assignment(assignment, member, constants, work);
        }
        if let AssignmentTarget::ComputedMemberExpression(member) = &assignment.left {
            if matches!(
                assignment.operator,
                AssignmentOperator::LogicalOr
                    | AssignmentOperator::LogicalAnd
                    | AssignmentOperator::LogicalNullish
            ) {
                return Self::plan_computed_member_logical_assignment(
                    assignment, member, flow, work,
                );
            }
            if assignment.operator != AssignmentOperator::Assign {
                return Self::plan_computed_member_compound_assignment(assignment, member, work);
            }
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
            LoweredReference::RealmGlobal { slot, binding, .. } => {
                return Self::plan_realm_global_assignment(
                    assignment,
                    slot,
                    binding,
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

    fn plan_static_super_member_compound_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let binary = assignment.operator.to_binary_operator().ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "nonlogical super member assignment has a binary operator",
                span: Some(assignment.span),
            },
        )?;
        // `dup3; get_super_value` reads once while preserving the exact
        // receiver/base/key reference for the corresponding [[Set]].
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert4,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            binary_opcode(binary),
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup3,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PushAtomValue,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.property.span,
        )));
        work.push(ExpressionWork::SuperPropertyBase {
            span: member.object.span(),
            call_receiver: false,
        });
        Ok(())
    }

    fn plan_computed_super_member_compound_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression ComputedMemberExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let binary = assignment.operator.to_binary_operator().ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "nonlogical computed super assignment has a binary operator",
                span: Some(assignment.span),
            },
        )?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert4,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            binary_opcode(binary),
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup3,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ToPropKey,
            Operands::None,
            member.expression.span(),
        )));
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::SuperPropertyBase {
            span: member.object.span(),
            call_receiver: false,
        });
        Ok(())
    }

    fn plan_static_super_member_logical_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let (short_circuit, done, branch_kind) =
            Self::super_member_logical_labels(assignment, flow)?;

        // The branch receives `receiver, base, key, old`. Its write path drops
        // `old`; its short-circuit path removes the saved reference triple.
        // Both join at `done` with exactly the assignment completion.
        Self::push_super_member_short_circuit_cleanup(
            &short_circuit,
            &done,
            assignment.span,
            member.span,
            work,
        );
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert4,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        Self::push_super_member_logical_branch(
            assignment,
            member.span,
            branch_kind,
            &short_circuit,
            work,
        );
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup3,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PushAtomValue,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.property.span,
        )));
        work.push(ExpressionWork::SuperPropertyBase {
            span: member.object.span(),
            call_receiver: false,
        });
        Ok(())
    }

    fn plan_computed_super_member_logical_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression ComputedMemberExpression<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let (short_circuit, done, branch_kind) =
            Self::super_member_logical_labels(assignment, flow)?;

        Self::push_super_member_short_circuit_cleanup(
            &short_circuit,
            &done,
            assignment.span,
            member.span,
            work,
        );
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert4,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        Self::push_super_member_logical_branch(
            assignment,
            member.span,
            branch_kind,
            &short_circuit,
            work,
        );
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup3,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ToPropKey,
            Operands::None,
            member.expression.span(),
        )));
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::SuperPropertyBase {
            span: member.object.span(),
            call_receiver: false,
        });
        Ok(())
    }

    fn plan_static_super_member_update<'expression>(
        update: &'expression UpdateExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            super_member_update_permutation(update.prefix),
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            update_opcode(update),
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup3,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PushAtomValue,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.property.span,
        )));
        work.push(ExpressionWork::SuperPropertyBase {
            span: member.object.span(),
            call_receiver: false,
        });
        Ok(())
    }

    fn plan_computed_super_member_update<'expression>(
        update: &'expression UpdateExpression<'arena>,
        member: &'expression ComputedMemberExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            super_member_update_permutation(update.prefix),
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            update_opcode(update),
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetSuperValue,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup3,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::ToPropKey,
            Operands::None,
            member.expression.span(),
        )));
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::SuperPropertyBase {
            span: member.object.span(),
            call_receiver: false,
        });
        Ok(())
    }

    fn super_member_logical_labels(
        assignment: &AssignmentExpression<'arena>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(CompilerLabel, CompilerLabel, BranchKind), LeafCompilationError> {
        let short_circuit = flow.new_label(assignment.span)?;
        let done = flow.new_label(assignment.span)?;
        let branch_kind = match assignment.operator {
            AssignmentOperator::LogicalOr => BranchKind::IfTrue,
            AssignmentOperator::LogicalAnd | AssignmentOperator::LogicalNullish => {
                BranchKind::IfFalse
            }
            _ => {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "logical super member assignment has a short-circuit branch",
                    span: Some(assignment.span),
                });
            }
        };
        Ok((short_circuit, done, branch_kind))
    }

    fn push_super_member_short_circuit_cleanup<'expression>(
        short_circuit: &CompilerLabel,
        done: &CompilerLabel,
        assignment_span: Span,
        member_span: Span,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) {
        work.push(ExpressionWork::Bind(done.clone()));
        for _ in 0..3 {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                member_span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                member_span,
            )));
        }
        work.push(ExpressionWork::Bind(short_circuit.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: done.clone(),
            span: assignment_span,
        });
    }

    fn push_super_member_logical_branch<'expression>(
        assignment: &AssignmentExpression<'arena>,
        member_span: Span,
        branch_kind: BranchKind,
        short_circuit: &CompilerLabel,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) {
        work.push(ExpressionWork::Branch {
            kind: branch_kind,
            target: short_circuit.clone(),
            span: assignment.span,
        });
        if assignment.operator == AssignmentOperator::LogicalNullish {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::IsUndefinedOrNull,
                Operands::None,
                member_span,
            )));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member_span,
        )));
    }

    fn plan_static_member_logical_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            return Self::plan_static_super_member_logical_assignment(
                assignment, member, constants, flow, work,
            );
        }
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let short_circuit = flow.new_label(assignment.span)?;
        let done = flow.new_label(assignment.span)?;
        let branch_kind = match assignment.operator {
            AssignmentOperator::LogicalOr => BranchKind::IfTrue,
            AssignmentOperator::LogicalAnd | AssignmentOperator::LogicalNullish => {
                BranchKind::IfFalse
            }
            _ => {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "logical member assignment has a short-circuit branch",
                    span: Some(assignment.span),
                });
            }
        };

        // Keep the member base below the old value until the short-circuit
        // decision is made. The write path drops that old value, whereas the
        // short-circuit path removes the retained base with `swap; drop`; both
        // paths therefore reach `done` with precisely the assignment completion.
        work.push(ExpressionWork::Bind(done.clone()));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Bind(short_circuit.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: assignment.span,
        });
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert2,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Branch {
            kind: branch_kind,
            target: short_circuit,
            span: assignment.span,
        });
        if assignment.operator == AssignmentOperator::LogicalNullish {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::IsUndefinedOrNull,
                Operands::None,
                member.span,
            )));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_static_member_compound_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            return Self::plan_static_super_member_compound_assignment(
                assignment, member, constants, work,
            );
        }
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let binary = assignment.operator.to_binary_operator().ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "nonlogical member assignment has a binary operator",
                span: Some(assignment.span),
            },
        )?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert2,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            binary_opcode(binary),
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_computed_member_logical_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression ComputedMemberExpression<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            return Self::plan_computed_super_member_logical_assignment(
                assignment, member, flow, work,
            );
        }
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let short_circuit = flow.new_label(assignment.span)?;
        let done = flow.new_label(assignment.span)?;
        let branch_kind = match assignment.operator {
            AssignmentOperator::LogicalOr => BranchKind::IfTrue,
            AssignmentOperator::LogicalAnd | AssignmentOperator::LogicalNullish => {
                BranchKind::IfFalse
            }
            _ => {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "logical computed member assignment has a short-circuit branch",
                    span: Some(assignment.span),
                });
            }
        };

        // `dup2; get_array_el` preserves the raw base/key pair for the
        // possible write while reading the old value. The key conversion is
        // deliberately observable once for the read and again for the write.
        // The short-circuit and write paths both leave exactly one completion.
        work.push(ExpressionWork::Bind(done.clone()));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Swap,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Bind(short_circuit.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: assignment.span,
        });
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutArrayEl,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert3,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Branch {
            kind: branch_kind,
            target: short_circuit,
            span: assignment.span,
        });
        if assignment.operator == AssignmentOperator::LogicalNullish {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::IsUndefinedOrNull,
                Operands::None,
                member.span,
            )));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetArrayEl,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup2,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_computed_member_compound_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression ComputedMemberExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            return Self::plan_computed_super_member_compound_assignment(assignment, member, work);
        }
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        let binary = assignment.operator.to_binary_operator().ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "nonlogical computed member assignment has a binary operator",
                span: Some(assignment.span),
            },
        )?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutArrayEl,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert3,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            binary_opcode(binary),
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetArrayEl,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup2,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::Visit(&member.object));
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
        if anonymous_class_expression_span(initializer).is_some() {
            return Ok(None);
        }
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

    fn plan_static_member_update<'expression>(
        update: &'expression UpdateExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            return Self::plan_static_super_member_update(update, member, constants, work);
        }
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        }
        // `dup; get_field` preserves the base for the write. A prefix update
        // duplicates the new value before `put_field`; a postfix update leaves
        // `old, new`, and `perm3` changes `[base, old, new]` into
        // `[old, base, new]` so the original value remains as the completion.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            if update.prefix {
                FinalOpcode::Insert2
            } else {
                FinalOpcode::Perm3
            },
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            update_opcode(update),
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_computed_member_update<'expression>(
        update: &'expression UpdateExpression<'arena>,
        member: &'expression ComputedMemberExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if matches!(&member.object, Expression::Super(_)) {
            return Self::plan_computed_super_member_update(update, member, work);
        }
        if member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        }
        // `dup2; get_array_el` preserves the base and raw key for the write.
        // A prefix update keeps its new value as the completion; a postfix
        // update moves the old value below the saved reference triple.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutArrayEl,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            if update.prefix {
                FinalOpcode::Insert3
            } else {
                FinalOpcode::Perm4
            },
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            update_opcode(update),
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetArrayEl,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup2,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_update_expression<'expression>(
        &self,
        update: &'expression UpdateExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let SimpleAssignmentTarget::PrivateFieldExpression(member) = &update.argument {
            return self.plan_private_member_update(update, member, layout, work);
        }
        let identifier = match &update.argument {
            SimpleAssignmentTarget::StaticMemberExpression(member) => {
                return Self::plan_static_member_update(update, member, constants, work);
            }
            SimpleAssignmentTarget::ComputedMemberExpression(member) => {
                return Self::plan_computed_member_update(update, member, work);
            }
            SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => identifier,
            _ => {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedExpression,
                    update.argument.span(),
                );
            }
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
            LoweredReference::RealmGlobal { slot, binding, .. } => {
                work.push(ExpressionWork::Emit(plan_external_put(
                    binding,
                    slot,
                    identifier.span,
                )?));
                if update.prefix {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Dup,
                        Operands::None,
                        update.span,
                    )));
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    update_opcode(update),
                    Operands::None,
                    update.span,
                )));
                work.push(ExpressionWork::Emit(plan_external_read(
                    binding,
                    slot,
                    false,
                    identifier.span,
                )));
                return Ok(());
            }
        };

        self.push_slot_write(binding, frame_slot, update.prefix, identifier.span, work)?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            update_opcode(update),
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
                if let LoweredReference::RealmGlobal {
                    slot,
                    binding: CompilerClosureBinding::RealmGlobal(_),
                    access,
                    ..
                } = reference
                {
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
                    LoweredReference::Frame { .. }
                    | LoweredReference::RealmGlobal {
                        binding: CompilerClosureBinding::Captured(_),
                        ..
                    } => {
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
