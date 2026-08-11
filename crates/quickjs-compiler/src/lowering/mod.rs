use std::sync::Arc;

use oxc_ast::{
    AstKind,
    ast::{
        Argument, ArrayAssignmentTarget, ArrayExpression, ArrayExpressionElement, ArrayPattern,
        ArrowFunctionExpression, AssignmentExpression, AssignmentPattern, AssignmentTarget,
        AssignmentTargetMaybeDefault, AssignmentTargetProperty, AssignmentTargetPropertyIdentifier,
        AssignmentTargetRest, AssignmentTargetWithDefault, BindingIdentifier, BindingPattern,
        BindingRestElement, CallExpression, CatchClause, ChainElement, ChainExpression, Class,
        ClassElement, ComputedMemberExpression, ConditionalExpression, DoWhileStatement,
        Expression, ExpressionStatement, ForInStatement, ForOfStatement, ForStatement,
        ForStatementInit, ForStatementLeft, FormalParameter, Function, FunctionType,
        IdentifierReference, IfStatement, LabelIdentifier, LabeledStatement, LogicalExpression,
        MethodDefinition, MethodDefinitionKind, NewExpression, ObjectAssignmentTarget,
        ObjectExpression, ObjectPattern, ObjectProperty, ObjectPropertyKind,
        PrivateFieldExpression, PrivateInExpression, Program, PropertyDefinition,
        PropertyKey as OxcPropertyKey, PropertyKind, RegExpLiteral, ReturnStatement,
        SequenceExpression, SimpleAssignmentTarget, StaticMemberExpression,
        TaggedTemplateExpression, ThrowStatement, TryStatement, UnaryExpression, UpdateExpression,
        VariableDeclaration, VariableDeclarationKind, VariableDeclarator, WhileStatement,
    },
};
use oxc_semantic::{NodeId, ReferenceId, ScopeId, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use quickjs_bytecode::{
    AtomPoolIndex, BranchKind, BytecodeGraphVerificationLimits, CompilerClosureBinding,
    FinalOpcode, FunctionGraphVerificationLimits, MAX_FUNCTION_INDEX_ENTRIES, Operands,
    UnverifiedCompilerBytecodeGraph, VerificationLimits, verify_compiler_bytecode_graph,
};
#[cfg(test)]
use quickjs_bytecode::{
    Binary64Constant, CompilerCaptureLayout, CompilerCapturedBinding, FunctionIndexDomains,
    UnverifiedFunctionHeader,
};
use quickjs_frontend::{CompilationGoal, ParsedUnit, Span};

use crate::storage::{
    BindingId, CompilationUnitKind, DeclarationKind, Executable, ExecutableId, ExecutableKind,
    InitializationPolicy, NativeReferenceId, ReferenceAccess, StoragePlacement, UnresolvedGlobalId,
    WritePolicy,
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
    CompiledFunctionConstant, CompiledFunctionTree, CompiledLeafFunction, CompiledModuleBinding,
    CompiledModuleBindingSource, CompiledRealmGlobal, CompiledRealmGlobalSource, LocalSlot,
    LoweredLocal, ModuleBindingId, RealmGlobalId, SourceInstruction,
};
use atoms::{
    CompiledMetadataAtomKey, compiled_static_property_key, compiler_identifier_string,
    decode_compiler_string, record_property_candidate, record_property_candidate_for,
    record_string_candidate,
};
use constants::CompiledConstantPool;
pub use context::{CompilationContext, CompilationExecutable, SourceTextSubstitution};
#[cfg(test)]
use control_flow::exact_source_span;
use control_flow::{
    CompilerLabel, ControlFlowVerificationInputs, PlannedControlFlow, PlannedInstruction,
};
use error::unsupported;
pub use error::{LeafCompilationError, UnsupportedLeafFeature};
use function::FunctionPlanningContext;
use function_graph::{
    binding_has_scope, constructor_realm_lookup_policy, verified_storage_policy,
    verify_compiled_function_graph,
};
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
    StatementPlanningState, StatementWork, anonymous_class_expression_span,
    anonymous_named_evaluation_span, anonymous_ordinary_function_span, binary_opcode,
    compact_get_argument, compact_get_local, compact_put_local, exact_i32, exact_negated_i32,
    plan_external_put, plan_external_read, plan_push_integer, plan_put_slot,
};
use validation::OrdinaryFunctionForm;

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
        // Children are compiled first so parent closure metadata can refer to
        // complete descendants. The AST inventory does not guarantee that a
        // nested class created from an instance-field initializer receives a
        // contiguous preorder executable id, though. The immutable graph and
        // `CompiledFunctionTree::function` both require stable executable-id
        // order, so normalize it before final graph verification.
        functions.sort_unstable_by_key(CompiledFunction::executable);
        let functions: Arc<[CompiledFunction]> = functions.into();
        let function_graph = Arc::new(verify_compiled_function_graph(
            root,
            &functions,
            graph_limits,
        )?);
        let mut graph_input = UnverifiedCompilerBytecodeGraph::new(
            Arc::clone(&function_graph),
            functions
                .iter()
                .map(|function| function.metadata.clone())
                .collect::<Vec<_>>()
                .into(),
        );
        if let Some(record) =
            self.build_module_declaration_record(root, &functions, &tree_layout)?
        {
            graph_input = graph_input.with_module(Arc::new(record));
        }
        let verified_bytecode = Arc::new(
            verify_compiler_bytecode_graph(graph_input, bytecode_limits).map_err(|source| {
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
        let direct_eval = match self.unit.goal() {
            CompilationGoal::DirectEval(context) => Some(context),
            CompilationGoal::GlobalScript(_)
            | CompilationGoal::Module
            | CompilationGoal::IndirectEval(_)
            | CompilationGoal::DynamicFunction(_) => None,
        };
        let seed = FunctionTreeLayoutSeed::new(FunctionTreeLayoutSeedInput {
            plan: &self.planned.plan,
            allow_realm_globals: crate::is_supported_script_root_goal(self.unit.goal())
                || crate::is_supported_module_goal(self.unit.goal())
                || direct_eval.is_some(),
            direct_eval,
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
            if self
                .planned
                .identities
                .annex_b_functions
                .get(&node_id)
                .is_some_and(|binding| binding.synthetic_block)
            {
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
            &[],
            flow,
        )
    }

    fn plan_expression_with_abrupt_markers<'expression>(
        &self,
        expression: &'expression Expression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        abrupt_markers: &[plan::abrupt::AbruptMarker],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        ExpressionPlanner::new(self).plan_expression(
            expression,
            layout,
            tree_layout,
            constants,
            abrupt_markers,
            flow,
        )
    }

    fn build_module_declaration_record(
        &self,
        root: ExecutableId,
        functions: &[CompiledFunction],
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Option<quickjs_bytecode::UnverifiedModuleDeclarationRecord>, LeafCompilationError>
    {
        if !crate::is_supported_module_goal(self.unit.goal()) {
            return Ok(None);
        }
        let root_function = functions
            .get(root.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable: root })?;
        let constants = tree_layout.constant_pool(root)?;
        let mut bindings = Vec::with_capacity(root_function.module_bindings.len());
        for binding in root_function.module_bindings.iter() {
            let mut descriptor = quickjs_bytecode::UnverifiedModuleBindingDescriptor::new(
                binding.atom,
                u32::from(binding.slot),
                binding.policy,
                binding.origin,
            );
            if let Some(initializer) = binding.function_initializer {
                descriptor = descriptor.with_initializer(initializer);
            }
            if let Some(import) = binding.import.clone() {
                descriptor = descriptor.with_import(import);
            }
            bindings.push(descriptor);
        }
        let mut requests = Vec::with_capacity(self.unit.module_syntax().requests().len());
        for (index, request) in self.unit.module_syntax().requests().iter().enumerate() {
            let specifier =
                constants.metadata_atom_index(CompiledMetadataAtomKey::ModuleRequest(
                    u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                        domain: "module request index",
                    })?,
                ))?;
            requests.push(quickjs_bytecode::ModuleRequestDescriptor::new(
                specifier,
                request.attributes().is_some(),
            ));
        }
        Ok(Some(
            quickjs_bytecode::UnverifiedModuleDeclarationRecord::new(
                bindings.into(),
                requests.into(),
            ),
        ))
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

#[cfg(test)]
mod tests;
