use super::super::{
    Argument, AssignmentExpression, AssignmentOperator, CallExpression, CompiledConstantPool,
    ComputedMemberExpression, Expression, FinalOpcode, GetSpan, LeafCompilationError,
    NewExpression, Operands, PlannedInstruction, Span, StaticMemberExpression,
    TaggedTemplateExpression, UnsupportedLeafFeature, plan_push_integer, unsupported,
};
use super::expressions::{ExpressionPlanner, ExpressionWork};

#[derive(Clone, Copy)]
pub(in crate::lowering) enum MemberCallee<'expression, 'arena> {
    Static(&'expression StaticMemberExpression<'arena>),
    Computed(&'expression ComputedMemberExpression<'arena>),
}

pub(in crate::lowering) fn plan_direct_call(argument_count: u16, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match argument_count {
        0 => (FinalOpcode::Call0, Operands::NPopX),
        1 => (FinalOpcode::Call1, Operands::NPopX),
        2 => (FinalOpcode::Call2, Operands::NPopX),
        3 => (FinalOpcode::Call3, Operands::NPopX),
        argument_count => (FinalOpcode::Call, Operands::NPop { argument_count }),
    };
    PlannedInstruction::new(opcode, operands, span)
}

impl<'arena> ExpressionPlanner<'_, '_, 'arena, '_> {
    pub(in crate::lowering) fn plan_tagged_template_expression<'expression>(
        tagged: &'expression TaggedTemplateExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if tagged.type_arguments.is_some() {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, tagged.span);
        }
        if tagged.quasi.quasis.len() != tagged.quasi.expressions.len().saturating_add(1) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "tagged template has one more quasi than substitution",
                span: Some(tagged.span),
            });
        }
        let argument_count = tagged
            .quasi
            .expressions
            .len()
            .checked_add(1)
            .and_then(|count| u16::try_from(count).ok())
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "tagged template arguments",
            })?;
        let member = Self::member_callee(&tagged.tag)?;
        work.push(ExpressionWork::Emit(if member.is_some() {
            PlannedInstruction::new(
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count },
                tagged.span,
            )
        } else {
            plan_direct_call(argument_count, tagged.span)
        }));
        for expression in tagged.quasi.expressions.iter().rev() {
            work.push(ExpressionWork::Visit(expression));
        }
        work.push(ExpressionWork::Emit(
            constants.plan_template_object(tagged.span)?,
        ));
        match member {
            Some(MemberCallee::Static(member)) => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetField2,
                    Operands::Atom(constants.property_atom_index(member.property.span)?),
                    member.span,
                )));
                work.push(ExpressionWork::Visit(&member.object));
            }
            Some(MemberCallee::Computed(member)) => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetArrayEl2,
                    Operands::None,
                    member.span,
                )));
                work.push(ExpressionWork::Visit(&member.expression));
                work.push(ExpressionWork::Visit(&member.object));
            }
            None => work.push(ExpressionWork::Visit(&tagged.tag)),
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "call planning keeps the reverse work-list schedule visible as one operation"
    )]
    pub(in crate::lowering) fn plan_call_expression<'expression>(
        call: &'expression CallExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if call.optional || call.type_arguments.is_some() {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, call.span);
        }
        if let Some(spread) = call.arguments.iter().position(Argument::is_spread) {
            let member = Self::member_callee(&call.callee)?;
            let dense_prefix = spread;
            let argument_count = u16::try_from(dense_prefix).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "spread call prefix arguments",
                }
            })?;
            let dynamic_index = i32::try_from(dense_prefix).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "spread call dynamic index",
                }
            })?;
            // Execution order: callee first, then the dense prefix, then
            // `array_from`, the dynamic index, each remaining argument, the
            // index drop, the receiver insert, and finally `apply`.
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Apply,
                Operands::U16(0),
                call.span,
            )));
            if member.is_some() {
                // `obj func array` -> `func obj array`
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Perm3,
                    Operands::None,
                    call.span,
                )));
            } else {
                // `func array` -> `func undef array`
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    call.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Undefined,
                    Operands::None,
                    call.span,
                )));
            }
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                call.span,
            )));
            for argument in call.arguments.iter().skip(dense_prefix).rev() {
                if let Argument::SpreadElement(spread) = argument {
                    let expression = &spread.argument;
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Append,
                        Operands::None,
                        argument.span(),
                    )));
                    work.push(ExpressionWork::Visit(expression));
                } else {
                    let expression = argument.as_expression().ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "dynamic call argument is an expression",
                            span: Some(argument.span()),
                        },
                    )?;
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Inc,
                        Operands::None,
                        argument.span(),
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::DefineArrayEl,
                        Operands::None,
                        argument.span(),
                    )));
                    work.push(ExpressionWork::Visit(expression));
                }
            }
            work.push(ExpressionWork::Emit(plan_push_integer(
                dynamic_index,
                call.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::ArrayFrom,
                Operands::NPop { argument_count },
                call.span,
            )));
            for argument in call.arguments.iter().take(dense_prefix).rev() {
                let expression =
                    argument
                        .as_expression()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "dense call argument is an expression",
                            span: Some(argument.span()),
                        })?;
                work.push(ExpressionWork::Visit(expression));
            }
            match member {
                Some(MemberCallee::Static(member)) => {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetField2,
                        Operands::Atom(constants.property_atom_index(member.property.span)?),
                        member.span,
                    )));
                    work.push(ExpressionWork::Visit(&member.object));
                }
                Some(MemberCallee::Computed(member)) => {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetArrayEl2,
                        Operands::None,
                        member.span,
                    )));
                    work.push(ExpressionWork::Visit(&member.expression));
                    work.push(ExpressionWork::Visit(&member.object));
                }
                None => work.push(ExpressionWork::Visit(&call.callee)),
            }
            return Ok(());
        }

        let argument_count = u16::try_from(call.arguments.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "call arguments",
            }
        })?;
        let member = Self::member_callee(&call.callee)?;
        work.push(ExpressionWork::Emit(if member.is_some() {
            PlannedInstruction::new(
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count },
                call.span,
            )
        } else {
            plan_direct_call(argument_count, call.span)
        }));
        for argument in call.arguments.iter().rev() {
            let expression =
                argument
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "non-spread call argument is an expression",
                        span: Some(argument.span()),
                    })?;
            work.push(ExpressionWork::Visit(expression));
        }
        match member {
            Some(MemberCallee::Static(member)) => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetField2,
                    Operands::Atom(constants.property_atom_index(member.property.span)?),
                    member.span,
                )));
                work.push(ExpressionWork::Visit(&member.object));
            }
            Some(MemberCallee::Computed(member)) => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetArrayEl2,
                    Operands::None,
                    member.span,
                )));
                work.push(ExpressionWork::Visit(&member.expression));
                work.push(ExpressionWork::Visit(&member.object));
            }
            None => work.push(ExpressionWork::Visit(&call.callee)),
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the exact QuickJS spread-construction argument packing is planned as one reviewable transaction"
    )]
    pub(in crate::lowering) fn plan_new_expression<'expression>(
        constructor: &'expression NewExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if constructor.type_arguments.is_some() {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                constructor.span,
            );
        }
        if let Some(spread) = constructor.arguments.iter().position(Argument::is_spread) {
            let dense_prefix = spread;
            let argument_count = u16::try_from(dense_prefix).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "spread constructor prefix arguments",
                }
            })?;
            let dynamic_index = i32::try_from(dense_prefix).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "spread constructor dynamic index",
                }
            })?;
            // `new C(...a)` duplicates the callee so the pinned `apply`
            // operand order `func this array` holds with `this` equal to the
            // duplicated construction target.
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Apply,
                Operands::U16(1),
                constructor.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Perm3,
                Operands::None,
                constructor.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                constructor.span,
            )));
            for argument in constructor.arguments.iter().skip(dense_prefix).rev() {
                if let Argument::SpreadElement(spread) = argument {
                    let expression = &spread.argument;
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Append,
                        Operands::None,
                        argument.span(),
                    )));
                    work.push(ExpressionWork::Visit(expression));
                } else {
                    let expression = argument.as_expression().ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "dynamic constructor argument is an expression",
                            span: Some(argument.span()),
                        },
                    )?;
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Inc,
                        Operands::None,
                        argument.span(),
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::DefineArrayEl,
                        Operands::None,
                        argument.span(),
                    )));
                    work.push(ExpressionWork::Visit(expression));
                }
            }
            work.push(ExpressionWork::Emit(plan_push_integer(
                dynamic_index,
                constructor.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::ArrayFrom,
                Operands::NPop { argument_count },
                constructor.span,
            )));
            for argument in constructor.arguments.iter().take(dense_prefix).rev() {
                let expression =
                    argument
                        .as_expression()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "dense constructor argument is an expression",
                            span: Some(argument.span()),
                        })?;
                work.push(ExpressionWork::Visit(expression));
            }
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                constructor.callee.span(),
            )));
            work.push(ExpressionWork::Visit(&constructor.callee));
            return Ok(());
        }

        let argument_count = u16::try_from(constructor.arguments.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "constructor arguments",
            }
        })?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::CallConstructor,
            Operands::NPop { argument_count },
            constructor.span,
        )));
        for argument in constructor.arguments.iter().rev() {
            let expression =
                argument
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "non-spread constructor argument is an expression",
                        span: Some(argument.span()),
                    })?;
            work.push(ExpressionWork::Visit(expression));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            constructor.callee.span(),
        )));
        work.push(ExpressionWork::Visit(&constructor.callee));
        Ok(())
    }

    pub(in crate::lowering) fn member_callee<'expression>(
        callee: &'expression Expression<'arena>,
    ) -> Result<Option<MemberCallee<'expression, 'arena>>, LeafCompilationError> {
        let mut expression = callee;
        loop {
            match expression {
                Expression::ParenthesizedExpression(parenthesized) => {
                    expression = &parenthesized.expression;
                }
                Expression::StaticMemberExpression(member) if !member.optional => {
                    return Ok(Some(MemberCallee::Static(member)));
                }
                Expression::StaticMemberExpression(member) => {
                    return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
                }
                Expression::ComputedMemberExpression(member) if !member.optional => {
                    return Ok(Some(MemberCallee::Computed(member)));
                }
                Expression::ComputedMemberExpression(member) => {
                    return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
                }
                _ => return Ok(None),
            }
        }
    }

    pub(in crate::lowering) fn plan_static_member_read<'expression>(
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    pub(in crate::lowering) fn plan_computed_member_read<'expression>(
        member: &'expression ComputedMemberExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetArrayEl,
            Operands::None,
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    pub(in crate::lowering) fn plan_static_member_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if assignment.operator != AssignmentOperator::Assign || member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
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
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    pub(in crate::lowering) fn plan_computed_member_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression ComputedMemberExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if assignment.operator != AssignmentOperator::Assign || member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
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
        work.push(ExpressionWork::Visit(&member.expression));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }
}
