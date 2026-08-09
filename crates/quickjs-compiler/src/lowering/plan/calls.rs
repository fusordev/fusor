use super::super::{
    Argument, AssignmentExpression, AssignmentOperator, AstKind, CallExpression, ChainExpression,
    CompilationGoal, CompiledConstantPool, ComputedMemberExpression, ExecutableId, ExecutableKind,
    Expression, FinalOpcode, FrameLayout, FunctionTreeLayout, GetSpan, IdentifierReference,
    LeafCompilationError, NewExpression, Operands, PlannedInstruction, PrivateFieldExpression,
    Span, StaticMemberExpression, TaggedTemplateExpression, UnsupportedLeafFeature,
    binding_has_scope, plan_push_integer, unsupported,
};
use super::expressions::{ExpressionPlanner, ExpressionWork};

#[derive(Clone, Copy)]
pub(in crate::lowering) enum MemberCallee<'expression, 'arena> {
    Static(&'expression StaticMemberExpression<'arena>),
    Computed(&'expression ComputedMemberExpression<'arena>),
    Chain(&'expression ChainExpression<'arena>),
    Private(&'expression PrivateFieldExpression<'arena>),
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
    fn with_identifier_callee<'expression>(
        &self,
        mut callee: &'expression Expression<'arena>,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Option<&'expression IdentifierReference<'arena>>, LeafCompilationError> {
        while let Expression::ParenthesizedExpression(parenthesized) = callee {
            callee = &parenthesized.expression;
        }
        let Expression::Identifier(identifier) = callee else {
            return Ok(None);
        };
        Ok((!self
            .with_object_sources_for_reference(
                identifier.reference_id.get(),
                identifier.span,
                tree_layout,
            )?
            .is_empty())
        .then_some(identifier))
    }

    fn is_contextual_direct_eval_derived_constructor(
        &self,
        executable: ExecutableId,
        span: Span,
    ) -> Result<bool, LeafCompilationError> {
        let executable = self.planned.plan.executable(executable).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "direct eval derived-constructor executable exists",
                span: Some(span),
            },
        )?;
        Ok(matches!(executable.kind(), ExecutableKind::Script { .. })
            && matches!(
                self.unit.goal(),
                CompilationGoal::DirectEval(context)
                    if context.capabilities().allows_super_call()
            ))
    }

    fn adjusted_eval_scope_index(
        &self,
        call: &CallExpression<'arena>,
        layout: &FrameLayout,
    ) -> Result<u16, LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        let call_node = call.node_id.get();
        let creator = self
            .planned
            .identities
            .node_by_executable
            .get(layout.executable.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "eval caller executable has an Oxc node identity",
                span: Some(call.span),
            })?;
        for ancestor in nodes.ancestor_ids(call_node) {
            if ancestor == creator {
                break;
            }
            match nodes.kind(ancestor) {
                AstKind::FormalParameters(_) => return Ok(0),
                AstKind::FunctionBody(_) => break,
                _ => {}
            }
        }

        let scoping = self.unit.semantic().scoping();
        let mut scope = Some(nodes.get_node(call_node).scope_id());
        while let Some(active_scope) = scope {
            for (index, local) in layout.locals.iter().enumerate().rev() {
                let binding = self.planned.plan.binding(local.binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "eval scope local binding exists",
                        span: Some(call.span),
                    },
                )?;
                if binding_has_scope(binding.policy())
                    && self.scope_for_binding(binding.id())? == active_scope
                {
                    let adjusted =
                        index
                            .checked_add(2)
                            .ok_or(LeafCompilationError::CapacityExceeded {
                                domain: "adjusted eval scope indices",
                            })?;
                    return u16::try_from(adjusted).map_err(|_| {
                        LeafCompilationError::CapacityExceeded {
                            domain: "adjusted eval scope indices",
                        }
                    });
                }
            }
            scope = scoping.scope_parent_id(active_scope);
        }
        Ok(1)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "tag planning keeps receiver lookup and the reverse work-list schedule together"
    )]
    pub(in crate::lowering) fn plan_tagged_template_expression<'expression>(
        &self,
        tagged: &'expression TaggedTemplateExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
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
        let with_identifier = self.with_identifier_callee(&tagged.tag, tree_layout)?;
        work.push(ExpressionWork::Emit(
            if member.is_some() || with_identifier.is_some() {
                PlannedInstruction::new(
                    FinalOpcode::CallMethod,
                    Operands::NPop { argument_count },
                    tagged.span,
                )
            } else {
                plan_direct_call(argument_count, tagged.span)
            },
        ));
        for expression in tagged.quasi.expressions.iter().rev() {
            work.push(ExpressionWork::Visit(expression));
        }
        work.push(ExpressionWork::Emit(
            constants.plan_template_object(tagged.span)?,
        ));
        match member {
            Some(MemberCallee::Static(member)) => {
                if matches!(&member.object, Expression::Super(_)) {
                    // A `super` method reference has an explicit receiver
                    // below the lookup triple so getter invocation and the
                    // eventual call both observe the actual `this` value.
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetSuperValue,
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
                        call_receiver: true,
                    });
                } else {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetField2,
                        Operands::Atom(constants.property_atom_index(member.property.span)?),
                        member.span,
                    )));
                    work.push(ExpressionWork::Visit(&member.object));
                }
            }
            Some(MemberCallee::Computed(member)) => {
                if matches!(&member.object, Expression::Super(_)) {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetSuperValue,
                        Operands::None,
                        member.span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::ToPropKey,
                        Operands::None,
                        member.expression.span(),
                    )));
                    work.push(ExpressionWork::SuperPropertyBaseAfterKey {
                        span: member.object.span(),
                    });
                    work.push(ExpressionWork::Visit(&member.expression));
                    work.push(ExpressionWork::SuperPropertyReceiver {
                        span: member.object.span(),
                        call_receiver: true,
                    });
                } else {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetArrayEl2,
                        Operands::None,
                        member.span,
                    )));
                    work.push(ExpressionWork::Visit(&member.expression));
                    work.push(ExpressionWork::Visit(&member.object));
                }
            }
            Some(MemberCallee::Chain(chain)) => {
                work.push(ExpressionWork::VisitOptionalChain {
                    chain,
                    preserve_final_reference: true,
                });
            }
            Some(MemberCallee::Private(member)) => {
                self.plan_private_member_callee(member, layout, work)?;
            }
            None => match with_identifier {
                Some(identifier) => {
                    work.push(ExpressionWork::IdentifierCallReference(identifier));
                }
                None => work.push(ExpressionWork::Visit(&tagged.tag)),
            },
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "call planning keeps the reverse work-list schedule visible as one operation"
    )]
    pub(in crate::lowering) fn plan_call_expression<'expression>(
        &self,
        call: &'expression CallExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if call.optional || call.type_arguments.is_some() {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, call.span);
        }
        if matches!(&call.callee, Expression::Super(_)) {
            if call.arguments.iter().any(Argument::is_spread) {
                return self.plan_super_spread_call(call, layout, work);
            }
            let argument_count = u16::try_from(call.arguments.len()).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "super constructor arguments",
                }
            })?;
            // `super(args)` evaluates the derived constructor's [[Prototype]]
            // before its arguments, then constructs it with the active
            // `new.target`. `check_ctor_return; drop` preserves the returned
            // object as the expression value while its certified runtime path
            // initializes this constructor frame's receiver.
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                call.span,
            )));
            let constructor = self.lexical_derived_constructor(layout.executable)?.ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "super constructor call belongs to a class constructor",
                    span: Some(call.span),
                },
            )?;
            if !self.is_contextual_direct_eval_derived_constructor(constructor, call.span)? {
                let instance_fields = self.instance_field_definitions(constructor)?.ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "super constructor call resolves its derived class constructor",
                        span: Some(call.span),
                    },
                )?;
                if !instance_fields.derived {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "super constructor call belongs to a derived class constructor",
                        span: Some(call.span),
                    });
                }
                if !instance_fields.elements.is_empty() {
                    work.push(ExpressionWork::InitializeInstanceFields {
                        constructor,
                        span: call.span,
                    });
                }
            }
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::CheckCtorReturn,
                Operands::None,
                call.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::CallConstructor,
                Operands::NPop { argument_count },
                call.span,
            )));
            for argument in call.arguments.iter().rev() {
                let expression =
                    argument
                        .as_expression()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "non-spread super constructor argument is an expression",
                            span: Some(argument.span()),
                        })?;
                work.push(ExpressionWork::Visit(expression));
            }
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::SpecialObject,
                Operands::U8(3),
                call.callee.span(),
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::GetSuper,
                Operands::None,
                call.callee.span(),
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::SpecialObject,
                Operands::U8(4),
                call.callee.span(),
            )));
            return Ok(());
        }
        let direct_eval_scope = (!call.optional && call.callee.is_specific_id("eval"))
            .then(|| self.adjusted_eval_scope_index(call, layout))
            .transpose()?;
        let with_identifier = self.with_identifier_callee(&call.callee, tree_layout)?;
        if let Some(spread) = call.arguments.iter().position(Argument::is_spread) {
            let member = if direct_eval_scope.is_some() {
                None
            } else {
                Self::member_callee(&call.callee)?
            };
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
            // index drop, the ordinary receiver insert when needed, and
            // finally `apply` or identity-checked `apply_eval`.
            let eval_reference_call = direct_eval_scope.is_some() && with_identifier.is_some();
            if eval_reference_call {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    call.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    call.span,
                )));
            }
            let call_instruction = PlannedInstruction::new(
                direct_eval_scope.map_or(FinalOpcode::Apply, |_| FinalOpcode::ApplyEval),
                Operands::U16(direct_eval_scope.unwrap_or(0)),
                call.span,
            );
            work.push(ExpressionWork::Emit(if eval_reference_call {
                call_instruction.with_eval_reference_call()
            } else {
                call_instruction
            }));
            if direct_eval_scope.is_some() {
                // `apply_eval` normally consumes `func array`. A verified
                // reference call carries `receiver func array`; the matching
                // cleanup above discards the retained receiver after return.
            } else if member.is_some() || with_identifier.is_some() {
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
                Some(MemberCallee::Chain(chain)) => {
                    work.push(ExpressionWork::VisitOptionalChain {
                        chain,
                        preserve_final_reference: true,
                    });
                }
                Some(MemberCallee::Private(member)) => {
                    self.plan_private_member_callee(member, layout, work)?;
                }
                None => match with_identifier {
                    Some(identifier) => {
                        work.push(ExpressionWork::IdentifierCallReference(identifier));
                    }
                    None => work.push(ExpressionWork::Visit(&call.callee)),
                },
            }
            return Ok(());
        }

        let argument_count = u16::try_from(call.arguments.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "call arguments",
            }
        })?;
        let member = Self::member_callee(&call.callee)?;
        let eval_reference_call = direct_eval_scope.is_some() && with_identifier.is_some();
        if eval_reference_call {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                call.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                call.span,
            )));
        }
        let call_instruction = if let Some(scope_index) = direct_eval_scope {
            let instruction = PlannedInstruction::new(
                FinalOpcode::Eval,
                Operands::NPopU16 {
                    argument_count,
                    scope_index,
                },
                call.span,
            );
            if eval_reference_call {
                instruction.with_eval_reference_call()
            } else {
                instruction
            }
        } else if member.is_some() || with_identifier.is_some() {
            PlannedInstruction::new(
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count },
                call.span,
            )
        } else {
            plan_direct_call(argument_count, call.span)
        };
        work.push(ExpressionWork::Emit(call_instruction));
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
                if matches!(&member.object, Expression::Super(_)) {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetSuperValue,
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
                        call_receiver: true,
                    });
                } else {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetField2,
                        Operands::Atom(constants.property_atom_index(member.property.span)?),
                        member.span,
                    )));
                    work.push(ExpressionWork::Visit(&member.object));
                }
            }
            Some(MemberCallee::Computed(member)) => {
                if matches!(&member.object, Expression::Super(_)) {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetSuperValue,
                        Operands::None,
                        member.span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::ToPropKey,
                        Operands::None,
                        member.expression.span(),
                    )));
                    work.push(ExpressionWork::SuperPropertyBaseAfterKey {
                        span: member.object.span(),
                    });
                    work.push(ExpressionWork::Visit(&member.expression));
                    work.push(ExpressionWork::SuperPropertyReceiver {
                        span: member.object.span(),
                        call_receiver: true,
                    });
                } else {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetArrayEl2,
                        Operands::None,
                        member.span,
                    )));
                    work.push(ExpressionWork::Visit(&member.expression));
                    work.push(ExpressionWork::Visit(&member.object));
                }
            }
            Some(MemberCallee::Chain(chain)) => {
                work.push(ExpressionWork::VisitOptionalChain {
                    chain,
                    preserve_final_reference: true,
                });
            }
            Some(MemberCallee::Private(member)) => {
                self.plan_private_member_callee(member, layout, work)?;
            }
            None => match with_identifier {
                Some(identifier) => {
                    work.push(ExpressionWork::IdentifierCallReference(identifier));
                }
                None => work.push(ExpressionWork::Visit(&call.callee)),
            },
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "super spread keeps its class capability and argument-list packing visible in one reverse work-list transaction"
    )]
    fn plan_super_spread_call<'expression>(
        &self,
        call: &'expression CallExpression<'arena>,
        layout: &FrameLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let dense_prefix = call.arguments.iter().position(Argument::is_spread).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "super spread call has a spread argument",
                span: Some(call.span),
            },
        )?;
        let argument_count =
            u16::try_from(dense_prefix).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "super spread prefix arguments",
            })?;
        let dynamic_index =
            i32::try_from(dense_prefix).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "super spread dynamic index",
            })?;

        // `apply(2)` is the typed super-construction form. Its operand stack
        // is `[superclass, active-new-target, argument-list]`; unlike ordinary
        // construction spread it must retain the derived caller's new.target.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            call.span,
        )));
        let constructor = self.lexical_derived_constructor(layout.executable)?.ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "super spread call belongs to a class constructor",
                span: Some(call.span),
            },
        )?;
        if !self.is_contextual_direct_eval_derived_constructor(constructor, call.span)? {
            let instance_fields = self.instance_field_definitions(constructor)?.ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "super spread call resolves its derived class constructor",
                    span: Some(call.span),
                },
            )?;
            if !instance_fields.derived {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "super spread call belongs to a derived class constructor",
                    span: Some(call.span),
                });
            }
            if !instance_fields.elements.is_empty() {
                work.push(ExpressionWork::InitializeInstanceFields {
                    constructor,
                    span: call.span,
                });
            }
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::CheckCtorReturn,
            Operands::None,
            call.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Apply,
            Operands::U16(2),
            call.span,
        )));
        // The array packing cursor is retained beside the argument list by
        // `array_from` and its append operations. It is not an `apply(2)`
        // operand, so discard it before the certified super-construction
        // transaction; the earlier `drop` after `check_ctor_return` instead
        // discards that operation's completion marker.
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            call.span,
        )));

        for argument in call.arguments.iter().skip(dense_prefix).rev() {
            if let Argument::SpreadElement(spread) = argument {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Append,
                    Operands::None,
                    argument.span(),
                )));
                work.push(ExpressionWork::Visit(&spread.argument));
            } else {
                let expression =
                    argument
                        .as_expression()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "dynamic super argument is an expression",
                            span: Some(argument.span()),
                        })?;
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
                        invariant: "dense super argument is an expression",
                        span: Some(argument.span()),
                    })?;
            work.push(ExpressionWork::Visit(expression));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::SpecialObject,
            Operands::U8(3),
            call.callee.span(),
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetSuper,
            Operands::None,
            call.callee.span(),
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::SpecialObject,
            Operands::U8(4),
            call.callee.span(),
        )));
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "spread argument packing remains one exact post-callee execution schedule"
    )]
    pub(in crate::lowering) fn plan_call_after_callee<'expression>(
        call: &'expression CallExpression<'arena>,
        method: bool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if call.type_arguments.is_some() {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, call.span);
        }
        if let Some(spread) = call.arguments.iter().position(Argument::is_spread) {
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
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Apply,
                Operands::U16(0),
                call.span,
            )));
            if method {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Perm3,
                    Operands::None,
                    call.span,
                )));
            } else {
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
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Append,
                        Operands::None,
                        argument.span(),
                    )));
                    work.push(ExpressionWork::Visit(&spread.argument));
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
            return Ok(());
        }

        let argument_count = u16::try_from(call.arguments.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "call arguments",
            }
        })?;
        work.push(ExpressionWork::Emit(if method {
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
                Expression::PrivateFieldExpression(member) if !member.optional => {
                    return Ok(Some(MemberCallee::Private(member)));
                }
                Expression::PrivateFieldExpression(member) => {
                    return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
                }
                Expression::ChainExpression(chain)
                    if matches!(
                        chain.expression,
                        super::super::ChainElement::StaticMemberExpression(_)
                            | super::super::ChainElement::ComputedMemberExpression(_)
                    ) =>
                {
                    return Ok(Some(MemberCallee::Chain(chain)));
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
        if matches!(&member.object, Expression::Super(_)) {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::GetSuperValue,
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
            return Ok(());
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
        if matches!(&member.object, Expression::Super(_)) {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::GetSuperValue,
                Operands::None,
                member.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::ToPropKey,
                Operands::None,
                member.expression.span(),
            )));
            work.push(ExpressionWork::SuperPropertyBaseAfterKey {
                span: member.object.span(),
            });
            work.push(ExpressionWork::Visit(&member.expression));
            work.push(ExpressionWork::SuperPropertyReceiver {
                span: member.object.span(),
                call_receiver: false,
            });
            return Ok(());
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
        if matches!(&member.object, Expression::Super(_)) {
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
                FinalOpcode::PushAtomValue,
                Operands::Atom(constants.property_atom_index(member.property.span)?),
                member.property.span,
            )));
            work.push(ExpressionWork::SuperPropertyBase {
                span: member.object.span(),
                call_receiver: false,
            });
            return Ok(());
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
        if matches!(&member.object, Expression::Super(_)) {
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
            // A computed super reference retains its raw key through RHS
            // evaluation. PutValue performs ToPropertyKey only afterwards.
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                member.span,
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::ToPropKey,
                Operands::None,
                member.expression.span(),
            )));
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Swap,
                Operands::None,
                member.span,
            )));
            work.push(ExpressionWork::Visit(&assignment.right));
            work.push(ExpressionWork::SuperPropertyBaseAfterKey {
                span: member.object.span(),
            });
            work.push(ExpressionWork::Visit(&member.expression));
            work.push(ExpressionWork::SuperPropertyReceiver {
                span: member.object.span(),
                call_receiver: false,
            });
            return Ok(());
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
