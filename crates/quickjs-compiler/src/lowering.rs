use std::sync::Arc;

use oxc_ast::{
    AstKind,
    ast::{
        Argument, ArrayAssignmentTarget, ArrayExpression, ArrayExpressionElement, ArrayPattern,
        AssignmentExpression, AssignmentTarget, AssignmentTargetMaybeDefault,
        AssignmentTargetProperty, AssignmentTargetRest, BindingIdentifier, BindingPattern,
        BindingRestElement, CallExpression, CatchClause, ComputedMemberExpression,
        ConditionalExpression, DoWhileStatement, Expression, ExpressionStatement, ForInStatement,
        ForOfStatement, ForStatement, ForStatementInit, ForStatementLeft, Function, FunctionType,
        IdentifierReference, IfStatement, LabelIdentifier, LabeledStatement, LogicalExpression,
        NewExpression, ObjectAssignmentTarget, ObjectExpression, ObjectPattern, ObjectProperty,
        ObjectPropertyKind, Program, PropertyKey as OxcPropertyKey, PropertyKind, ReturnStatement,
        SequenceExpression, SimpleAssignmentTarget, StaticMemberExpression, ThrowStatement,
        TryStatement, UnaryExpression, UpdateExpression, VariableDeclaration,
        VariableDeclarationKind, VariableDeclarator, WhileStatement,
    },
};
use oxc_semantic::{NodeId, ReferenceId, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use quickjs_bytecode::{
    AtomPoolIndex, BranchKind, BytecodeGraphVerificationLimits,
    CompilerBindingKind as VerifiedBindingKind, CompilerBindingPolicy,
    CompilerInitializationPolicy as VerifiedInitializationPolicy,
    CompilerWritePolicy as VerifiedWritePolicy, FinalOpcode, FunctionGraphVerificationLimits,
    MAX_FUNCTION_INDEX_ENTRIES, Operands, ScopeLink, SourceByteSpan,
    UnverifiedCompilerBytecodeGraph, VariableDefinition, VerificationLimits,
    verify_compiler_bytecode_graph,
};
#[cfg(test)]
use quickjs_bytecode::{
    Binary64Constant, CompilerCaptureLayout, CompilerCapturedBinding, FunctionIndexDomains,
    UnverifiedFunctionHeader,
};
use quickjs_frontend::{CompilationGoal, DynamicFunctionKind, ParsedUnit, Span};

use crate::storage::{
    BindingId, CompilationUnitKind, DeclarationKind, DeclarationPolicy, Executable, ExecutableId,
    ExecutableKind, InitializationPolicy, NativeReferenceId, ReferenceAccess, StoragePlacement,
    UnresolvedGlobalId, WritePolicy,
};

mod artifacts;
mod atoms;
mod constants;
mod context;
mod control_flow;
mod error;
mod function;
mod function_graph;
mod layouts;
mod plan;
mod validation;

pub use artifacts::{
    CompiledClosureSource, CompiledClosureVariable, CompiledConstant, CompiledFunction,
    CompiledFunctionConstant, CompiledFunctionTree, CompiledLeafFunction, CompiledRealmGlobal,
    CompiledRealmGlobalSource, LocalSlot, LoweredLocal, RealmGlobalId, SourceInstruction,
};
use atoms::{
    CompiledMetadataAtomKey, compiled_static_property_key, compiler_identifier_string,
    decode_compiler_string, record_property_candidate, record_property_candidate_for,
    record_string_candidate,
};
use constants::CompiledConstantPool;
pub use context::{CompilationContext, CompilationExecutable};
#[cfg(test)]
use control_flow::exact_source_span;
use control_flow::{CompilerLabel, PlannedControlFlow, PlannedInstruction};
pub use error::{LeafCompilationError, UnsupportedLeafFeature};
use function::FunctionPlanningContext;
use function_graph::verify_compiled_function_graph;
use layouts::{
    ArgumentSlot, FrameLayout, FrameLayoutInput, FrameSlot, FunctionTreeLayout,
    FunctionTreeLayoutInput, FunctionTreeLayoutSeed, FunctionTreeLayoutSeedInput,
};
#[cfg(test)]
use oxc_ast::ast::Statement;
#[cfg(test)]
use plan::control::{ControlRegion, LoopJump};
use plan::{
    DestructuringBindingInitialization, ExpressionPlanner, ExpressionWork, LogicalCompilerScope,
    LoweredReference, ScopeEntryInitialization, StatementCompletion, StatementControlStack,
    StatementPlanningState, StatementWork,
};
use validation::{OrdinaryFunctionForm, object_method_or_accessor_span};

impl<'arena> CompilationContext<'_, 'arena, '_> {
    fn compile_subtree_with_all_limits(
        &self,
        root: ExecutableId,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        let tree_layout = self.function_tree_layout()?;
        let subtree = tree_layout.subtree_preorder(root)?;
        let mut functions = Vec::with_capacity(subtree.len());
        for executable in subtree.iter().rev().copied() {
            functions.push(self.compile_function(executable, &tree_layout, limits)?);
        }
        functions.reverse();
        let functions: Arc<[CompiledFunction]> = functions.into();
        let function_graph = Arc::new(verify_compiled_function_graph(
            root,
            &functions,
            graph_limits,
        )?);
        let verified_bytecode = Arc::new(
            verify_compiler_bytecode_graph(
                UnverifiedCompilerBytecodeGraph::new(
                    Arc::clone(&function_graph),
                    functions
                        .iter()
                        .map(|function| function.metadata.clone())
                        .collect::<Vec<_>>()
                        .into(),
                ),
                bytecode_limits,
            )
            .map_err(|source| {
                let span = source
                    .function_id()
                    .and_then(|template| usize::try_from(template.get()).ok())
                    .and_then(|index| functions.get(index))
                    .and_then(|function| {
                        function
                            .storage_plan
                            .executable(function.executable)
                            .map(Executable::span)
                    });
                LeafCompilationError::BytecodeGraphVerification { span, source }
            })?,
        );
        Ok(CompiledFunctionTree {
            root,
            storage_plan: Arc::clone(&self.planned.plan),
            source_text: Arc::clone(&self.source_text),
            functions,
            function_graph,
            verified_bytecode,
        })
    }

    fn reject_dynamic_function_subtree_entry(&self) -> Result<(), LeafCompilationError> {
        if matches!(self.unit.goal(), CompilationGoal::DynamicFunction(_)) {
            return unsupported(
                UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                self.unit.program().span,
            );
        }
        Ok(())
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

    fn function_tree_layout(&self) -> Result<FunctionTreeLayout, LeafCompilationError> {
        let seed = FunctionTreeLayoutSeed::new(FunctionTreeLayoutSeedInput {
            plan: &self.planned.plan,
            allow_realm_globals: self.unit.goal()
                == CompilationGoal::DynamicFunction(DynamicFunctionKind::Function),
        })?;
        let mut function_declarations =
            vec![None; self.planned.plan.bindings().len()].into_boxed_slice();
        for executable in self.planned.plan.executables() {
            let node_id = self
                .planned
                .identities
                .node_by_executable
                .get(executable.id().index())
                .copied()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "executable has an Oxc node identity",
                    span: Some(executable.span()),
                })?;
            let AstKind::Function(function) = self.unit.semantic().nodes().kind(node_id) else {
                continue;
            };
            if function.r#type != FunctionType::FunctionDeclaration {
                continue;
            }
            let Some(identifier) = &function.id else {
                continue;
            };
            let binding =
                self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "function declaration has compiler storage",
                    span: Some(identifier.span),
                },
            )?;
            if executable.parent() != Some(storage.executable()) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "function declaration binding belongs to its parent executable",
                    span: Some(identifier.span),
                });
            }
            let target = function_declarations.get_mut(binding.index()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "function declaration binding indexes instantiation layout",
                    span: None,
                },
            )?;
            *target = Some(executable.id());
        }
        let constant_pools = self.compiled_constant_pools(&seed)?;
        FunctionTreeLayout::new(FunctionTreeLayoutInput {
            seed,
            constant_pools,
            function_declarations,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the iterative expression dispatcher keeps one explicit work-stack loop"
    )]
    fn plan_expression<'expression>(
        &self,
        expression: &'expression Expression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        ExpressionPlanner::new(self).plan_expression(
            expression,
            layout,
            tree_layout,
            constants,
            flow,
        )
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

fn anonymous_named_evaluation_span(mut expression: &Expression<'_>) -> Option<Span> {
    while let Expression::ParenthesizedExpression(parenthesized) = expression {
        expression = &parenthesized.expression;
    }
    match expression {
        Expression::FunctionExpression(function) if function.id.is_none() => Some(function.span),
        Expression::ClassExpression(class) if class.id.is_none() => Some(class.span),
        _ => None,
    }
}

fn anonymous_ordinary_function_span(mut expression: &Expression<'_>) -> Option<Span> {
    while let Expression::ParenthesizedExpression(parenthesized) = expression {
        expression = &parenthesized.expression;
    }
    match expression {
        Expression::FunctionExpression(function) if function.id.is_none() => Some(function.span),
        _ => None,
    }
}

fn plan_literal(
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
        Expression::StringLiteral(literal) => constants.plan_string(literal.span),
        Expression::RegExpLiteral(literal) => {
            unsupported(UnsupportedLeafFeature::UnsupportedLiteral, literal.span)
        }
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
                        template.span,
                    )),
                    Some(_) => constants.plan_string(template.span),
                }
            } else {
                Err(LeafCompilationError::SemanticInvariant {
                    invariant: "no-substitution template has one tail quasi",
                    span: Some(template.span),
                })
            }
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

fn plan_direct_call(argument_count: u16, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match argument_count {
        0 => (FinalOpcode::Call0, Operands::NPopX),
        1 => (FinalOpcode::Call1, Operands::NPopX),
        2 => (FinalOpcode::Call2, Operands::NPopX),
        3 => (FinalOpcode::Call3, Operands::NPopX),
        argument_count => (FinalOpcode::Call, Operands::NPop { argument_count }),
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

fn verified_binding_policy(
    policy: DeclarationPolicy,
) -> Result<CompilerBindingPolicy, LeafCompilationError> {
    let kind = match policy.kind() {
        DeclarationKind::Parameter => VerifiedBindingKind::Parameter,
        DeclarationKind::Var => VerifiedBindingKind::Var,
        DeclarationKind::Let => VerifiedBindingKind::Let,
        DeclarationKind::Const => VerifiedBindingKind::Const,
        DeclarationKind::Function => VerifiedBindingKind::Function,
        DeclarationKind::FunctionName => VerifiedBindingKind::FunctionName,
        DeclarationKind::Catch => VerifiedBindingKind::Catch,
        DeclarationKind::Import
        | DeclarationKind::NamespaceImport
        | DeclarationKind::SyntheticDefault => {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary function metadata excludes module bindings",
                span: None,
            });
        }
    };
    let initialization = match policy.initialization() {
        InitializationPolicy::Argument => VerifiedInitializationPolicy::Argument,
        InitializationPolicy::UndefinedAtInstantiation => {
            VerifiedInitializationPolicy::UndefinedAtInstantiation
        }
        InitializationPolicy::AtDeclaration => VerifiedInitializationPolicy::AtDeclaration,
        InitializationPolicy::FunctionAtInstantiation => {
            VerifiedInitializationPolicy::FunctionAtInstantiation
        }
        InitializationPolicy::FunctionAtScopeEntry => {
            VerifiedInitializationPolicy::FunctionAtScopeEntry
        }
        InitializationPolicy::FunctionName => VerifiedInitializationPolicy::FunctionName,
        InitializationPolicy::Catch => VerifiedInitializationPolicy::Catch,
        InitializationPolicy::ModuleImport | InitializationPolicy::ModuleNamespace => {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary function metadata excludes module initialization",
                span: None,
            });
        }
    };
    let writes = match policy.writes() {
        WritePolicy::Mutable => VerifiedWritePolicy::Mutable,
        WritePolicy::Immutable => VerifiedWritePolicy::Immutable,
        WritePolicy::ImmutableInStrictCode => VerifiedWritePolicy::ImmutableInStrictCode,
        WritePolicy::Internal => {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary function metadata excludes internal module cells",
                span: None,
            });
        }
    };
    Ok(CompilerBindingPolicy::new(
        kind,
        initialization,
        writes,
        policy.has_temporal_dead_zone(),
    ))
}

fn verified_storage_policy(
    binding: &crate::storage::BindingStorage,
) -> Result<CompilerBindingPolicy, LeafCompilationError> {
    if matches!(binding.placement(), StoragePlacement::Argument { .. }) {
        return Ok(CompilerBindingPolicy::new(
            VerifiedBindingKind::Parameter,
            VerifiedInitializationPolicy::Argument,
            VerifiedWritePolicy::Mutable,
            false,
        ));
    }
    verified_binding_policy(binding.policy())
}

const fn constructor_realm_lookup_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        VerifiedBindingKind::GlobalReference,
        VerifiedInitializationPolicy::ConstructorRealmLookup,
        VerifiedWritePolicy::Mutable,
        false,
    )
}

fn script_completion_variable_definition(
    constants: &CompiledConstantPool,
) -> Result<VariableDefinition, LeafCompilationError> {
    Ok(VariableDefinition::new(
        Some(constants.metadata_atom_index(CompiledMetadataAtomKey::ScriptCompletion)?),
        ScopeLink::End,
        CompilerBindingPolicy::new(
            VerifiedBindingKind::Var,
            VerifiedInitializationPolicy::UndefinedAtInstantiation,
            VerifiedWritePolicy::Mutable,
            false,
        ),
        false,
        None,
    ))
}

fn raw_parameter_definition(
    constants: &CompiledConstantPool,
    index: u32,
) -> Result<VariableDefinition, LeafCompilationError> {
    Ok(VariableDefinition::new(
        Some(constants.metadata_atom_index(CompiledMetadataAtomKey::RawParameter(index))?),
        ScopeLink::End,
        CompilerBindingPolicy::new(
            VerifiedBindingKind::Parameter,
            VerifiedInitializationPolicy::Argument,
            VerifiedWritePolicy::Mutable,
            false,
        ),
        false,
        None,
    ))
}

const fn binding_has_scope(policy: DeclarationPolicy) -> bool {
    matches!(
        policy.kind(),
        DeclarationKind::Let | DeclarationKind::Const | DeclarationKind::Catch
    ) || matches!(
        policy.initialization(),
        InitializationPolicy::FunctionAtScopeEntry
    )
}

const fn source_byte_span(span: Span) -> SourceByteSpan {
    SourceByteSpan::new(span.start, span.end)
}

#[cfg(test)]
mod tests;
