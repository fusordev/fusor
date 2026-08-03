use std::{collections::HashMap, sync::Arc};

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
use oxc_semantic::{NodeId, ReferenceId, ScopeId, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use quickjs_bytecode::{
    AtomPoolIndex, Binary64Constant, BranchKind, BytecodeGraphVerificationLimits,
    ClosureVariableDefinition as VerifiedClosureVariableDefinition,
    CompilerBindingKind as VerifiedBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerCapturedBinding, CompilerClosureSource as CompilerGraphClosureSource,
    CompilerInitializationPolicy as VerifiedInitializationPolicy,
    CompilerWritePolicy as VerifiedWritePolicy, FinalOpcode, FunctionGraphVerificationLimits,
    MAX_FUNCTION_INDEX_ENTRIES, Operands, ScopeLink, SourceByteSpan,
    UnverifiedCompilerBytecodeGraph, VariableDefinition, VerificationLimits,
    verify_compiler_bytecode_graph,
};
#[cfg(test)]
use quickjs_bytecode::{FunctionIndexDomains, UnverifiedFunctionHeader};
use quickjs_frontend::{CompilationGoal, DynamicFunctionKind, ParsedUnit, Span};

use crate::storage::{
    BindingId, CaptureSource, CompilationUnitKind, DeclarationKind, DeclarationPolicy, Executable,
    ExecutableId, ExecutableKind, InitializationPolicy, NativeReferenceId, ReferenceAccess,
    StoragePlacement, UnresolvedGlobalId, WritePolicy,
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
    CompiledAtomCandidate, CompiledMetadataAtomCandidate, CompiledMetadataAtomKey,
    CompiledPropertyAtomKey, compiled_static_property_key, compiler_identifier_string,
    decode_compiler_string, record_property_candidate, record_property_candidate_for,
    record_string_candidate,
};
use constants::{CompiledConstantCandidate, CompiledConstantPool, CompiledConstantPoolInput};
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

    fn compiled_constant_pools(
        &self,
        tree_layout: &FunctionTreeLayoutSeed,
    ) -> Result<Box<[CompiledConstantPool]>, LeafCompilationError> {
        let executables = self.planned.plan.executables();
        let mut candidates = (0..executables.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        let mut atom_candidates = (0..executables.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        let mut metadata_atom_candidates = (0..executables.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        for child in executables {
            let Some(parent) = child.parent() else {
                continue;
            };
            let owner = candidates
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            owner.push(CompiledConstantCandidate::Function {
                executable: child.id(),
                span: child.span(),
            });
        }
        self.record_literal_candidates(&mut candidates, &mut atom_candidates)?;
        self.record_metadata_atom_candidates(tree_layout, &mut metadata_atom_candidates)?;

        let mut pools = Vec::with_capacity(executables.len());
        for (index, ((candidates, atoms), metadata_atoms)) in candidates
            .into_iter()
            .zip(atom_candidates)
            .zip(metadata_atom_candidates)
            .enumerate()
        {
            let executable =
                executables
                    .get(index)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "constant-pool owner indexes dense executable metadata",
                        span: None,
                    })?;
            pools.push(CompiledConstantPool::new(CompiledConstantPoolInput {
                children: tree_layout.children(executable.id())?,
                constant_candidates: candidates,
                atom_candidates: atoms,
                metadata_atom_candidates: metadata_atoms,
            })?);
        }
        Ok(pools.into_boxed_slice())
    }

    fn record_metadata_atom_candidates(
        &self,
        tree_layout: &FunctionTreeLayoutSeed,
        candidates: &mut [Vec<CompiledMetadataAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        for executable in self.planned.plan.executables() {
            let owner = candidates.get_mut(executable.id().index()).ok_or(
                LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                },
            )?;
            if let Some(name) = executable.name() {
                let span =
                    executable
                        .name_span()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "named executable retains its name span",
                            span: Some(executable.span()),
                        })?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::FunctionName,
                    value: compiler_identifier_string(name, span)?,
                    span,
                });
            } else if executable.name_span().is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "anonymous executable has no name span",
                    span: Some(executable.span()),
                });
            }
            if executable.id().index() == 0
                && self.unit.goal()
                    == CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            {
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::ScriptCompletion,
                    value: compiler_identifier_string("_ret_", executable.span())?,
                    span: executable.span(),
                });
            }
            self.record_raw_parameter_metadata_candidates(executable, owner)?;
            for binding in self.planned.plan.bindings_for(executable.id()).ok_or(
                LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                },
            )? {
                if !matches!(
                    binding.placement(),
                    StoragePlacement::Argument { .. } | StoragePlacement::Local
                ) {
                    continue;
                }
                let span = binding.declaration_spans().first().copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "frame binding retains a declaration span",
                        span: Some(executable.span()),
                    },
                )?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::Binding(binding.id()),
                    value: compiler_identifier_string(binding.name(), span)?,
                    span,
                });
            }
            for capture in self
                .planned
                .plan
                .frame_captures_for(executable.id())
                .ok_or(LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                })?
            {
                let binding = self.planned.plan.binding(capture.binding()).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured metadata binding exists",
                        span: Some(executable.span()),
                    },
                )?;
                let span = binding.declaration_spans().first().copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured binding retains a declaration span",
                        span: Some(executable.span()),
                    },
                )?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::Binding(binding.id()),
                    value: compiler_identifier_string(binding.name(), span)?,
                    span,
                });
            }
            for &global in tree_layout.realm_globals.imports_for(executable.id())? {
                let binding = tree_layout.realm_globals.binding(global).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "constructor-realm global import has a binding descriptor",
                        span: Some(executable.span()),
                    },
                )?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::RealmGlobal(global),
                    value: compiler_identifier_string(&binding.name, binding.first_span)?,
                    span: binding.first_span,
                });
            }
        }
        Ok(())
    }

    fn record_raw_parameter_metadata_candidates(
        &self,
        executable: &Executable,
        owner: &mut Vec<CompiledMetadataAtomCandidate>,
    ) -> Result<(), LeafCompilationError> {
        if executable.has_simple_parameter_list() {
            return Ok(());
        }
        let node = self
            .planned
            .identities
            .node_by_executable
            .get(executable.id().index())
            .copied()
            .ok_or(LeafCompilationError::InvalidExecutable {
                executable: executable.id(),
            })?;
        let parameters = match self.unit.semantic().nodes().kind(node) {
            AstKind::Function(function) => Some(function.params.as_ref()),
            AstKind::ArrowFunctionExpression(arrow) => Some(arrow.params.as_ref()),
            AstKind::Program(_) => None,
            _ => {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "non-simple parameter metadata belongs to a function",
                    span: Some(executable.span()),
                });
            }
        };
        let Some(parameters) = parameters else {
            return Ok(());
        };
        for (index, parameter) in parameters.items.iter().enumerate() {
            if !executable.has_parameter_expressions()
                && matches!(parameter.pattern, BindingPattern::BindingIdentifier(_))
            {
                continue;
            }
            let index =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "raw parameter metadata",
                })?;
            let name = format!("_arg_{index}_");
            owner.push(CompiledMetadataAtomCandidate {
                key: CompiledMetadataAtomKey::RawParameter(index),
                value: compiler_identifier_string(&name, parameter.span)?,
                span: parameter.span,
            });
        }
        Ok(())
    }

    fn record_literal_candidates(
        &self,
        candidates: &mut [Vec<CompiledConstantCandidate>],
        atom_candidates: &mut [Vec<CompiledAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        let mut owners = vec![None; nodes.len()];
        for (node_id, node) in nodes.iter_enumerated() {
            let owner = match node.kind() {
                AstKind::Program(_)
                | AstKind::Function(_)
                | AstKind::ArrowFunctionExpression(_) => self
                    .planned
                    .identities
                    .executable_by_node
                    .get(node_id.index())
                    .copied()
                    .flatten(),
                _ => {
                    let parent = nodes.parent_id(node_id);
                    if parent.index() >= node_id.index() {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "semantic parents precede children in node order",
                            span: Some(node.kind().span()),
                        });
                    }
                    owners.get(parent.index()).copied().flatten()
                }
            };
            let owner_slot =
                owners
                    .get_mut(node_id.index())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "semantic node identity indexes constant-pool ownership",
                        span: Some(node.kind().span()),
                    })?;
            *owner_slot = owner;
            if let Some(owner) = owner {
                self.record_node_literal_candidate(node_id, owner, candidates, atom_candidates)?;
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the literal/property candidate walk stays one exhaustive per-node audit"
    )]
    fn record_node_literal_candidate(
        &self,
        node_id: NodeId,
        owner: ExecutableId,
        candidates: &mut [Vec<CompiledConstantCandidate>],
        atom_candidates: &mut [Vec<CompiledAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        match nodes.kind(node_id) {
            AstKind::NumericLiteral(literal)
                if !is_noncomputed_static_property_key_node(self.unit, node_id)
                    && exact_i32(literal.value).is_none() =>
            {
                let parent = nodes.parent_id(node_id);
                let folded_negative_i32 = matches!(
                    nodes.kind(parent),
                    AstKind::UnaryExpression(unary)
                        if unary.operator == UnaryOperator::UnaryNegation
                            && literal.value != 0.0
                            && exact_negated_i32(literal.value).is_some()
                );
                if !folded_negative_i32 {
                    candidates
                        .get_mut(owner.index())
                        .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
                        .push(CompiledConstantCandidate::Number {
                            value: Binary64Constant::from_f64(literal.value),
                            span: literal.span,
                        });
                }
            }
            AstKind::StringLiteral(literal)
                if !matches!(nodes.parent_kind(node_id), AstKind::Directive(_))
                    && !is_noncomputed_static_property_key_node(self.unit, node_id) =>
            {
                let value = decode_compiler_string(
                    literal.value.as_str(),
                    literal.lone_surrogates,
                    literal.span,
                )?;
                record_string_candidate(owner, value, literal.span, candidates, atom_candidates)?;
            }
            AstKind::TemplateLiteral(template)
                if !matches!(
                    nodes.parent_kind(node_id),
                    AstKind::TaggedTemplateExpression(_)
                ) && template.expressions.is_empty()
                    && template.quasis.len() == 1 =>
            {
                let quasi = &template.quasis[0];
                if !quasi.tail {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "no-substitution template has one tail quasi",
                        span: Some(template.span),
                    });
                }
                let cooked =
                    quasi
                        .value
                        .cooked
                        .as_ref()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "untagged no-substitution template has a cooked value",
                            span: Some(template.span),
                        })?;
                let value =
                    decode_compiler_string(cooked.as_str(), quasi.lone_surrogates, template.span)?;
                record_string_candidate(owner, value, template.span, candidates, atom_candidates)?;
            }
            AstKind::ObjectProperty(property) => {
                if !property.computed
                    && !property.shorthand
                    && let Some(key) = compiled_static_property_key(&property.key)?
                {
                    record_property_candidate(owner, key.value, key.span, atom_candidates)?;
                }
            }
            AstKind::ObjectPattern(pattern) => {
                for property in &pattern.properties {
                    if !property.computed
                        && let Some(key) = compiled_static_property_key(&property.key)?
                    {
                        record_property_candidate(owner, key.value, key.span, atom_candidates)?;
                    }
                }
            }
            AstKind::ObjectAssignmentTarget(target) => {
                for property in &target.properties {
                    match property {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(
                            identifier,
                        ) => {
                            let key = compiler_identifier_string(
                                identifier.binding.name.as_str(),
                                identifier.binding.span,
                            )?;
                            record_property_candidate(
                                owner,
                                key,
                                identifier.binding.span,
                                atom_candidates,
                            )?;
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
                            if !property.computed
                                && let Some(key) = compiled_static_property_key(&property.name)?
                            {
                                record_property_candidate(
                                    owner,
                                    key.value,
                                    key.span,
                                    atom_candidates,
                                )?;
                            }
                        }
                    }
                }
            }
            AstKind::StaticMemberExpression(member) => {
                record_property_candidate(
                    owner,
                    compiler_identifier_string(
                        member.property.name.as_str(),
                        member.property.span,
                    )?,
                    member.property.span,
                    atom_candidates,
                )?;
            }
            AstKind::ArrayExpression(array) => {
                Self::record_array_property_candidates(owner, array, atom_candidates)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn record_array_property_candidates(
        owner: ExecutableId,
        array: &ArrayExpression<'arena>,
        atom_candidates: &mut [Vec<CompiledAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        let first_spread = array
            .elements
            .iter()
            .position(ArrayExpressionElement::is_spread);
        let first_static_index = if first_spread.is_some() {
            ExpressionPlanner::spread_array_dense_prefix_len(array)
        } else {
            let Some(first_elision) = array
                .elements
                .iter()
                .position(ArrayExpressionElement::is_elision)
            else {
                return Ok(());
            };
            first_elision
        };
        let static_end = first_spread.unwrap_or(array.elements.len());
        for (index, element) in array
            .elements
            .iter()
            .enumerate()
            .skip(first_static_index)
            .take(static_end - first_static_index)
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
            let span = expression.span();
            record_property_candidate_for(
                owner,
                compiler_identifier_string(&index.to_string(), span)?,
                span,
                CompiledPropertyAtomKey::ArrayIndex {
                    array: array.span,
                    index,
                },
                atom_candidates,
            )?;
        }
        let final_length_span = match first_spread {
            Some(_) => ExpressionPlanner::spread_array_final_length_span(array),
            None => array
                .elements
                .last()
                .filter(|element| element.is_elision())
                .map(GetSpan::span),
        };
        if let Some(final_length_span) = final_length_span {
            record_property_candidate_for(
                owner,
                compiler_identifier_string("length", final_length_span)?,
                final_length_span,
                CompiledPropertyAtomKey::ArrayLength { array: array.span },
                atom_candidates,
            )?;
        }
        Ok(())
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
    fn compiler_capture_layout(
        &self,
        executable: ExecutableId,
        _function_scope: ScopeId,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<CompilerCaptureLayout, LeafCompilationError> {
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let mut captured = Vec::new();
        for binding in bindings {
            if !binding.is_frame_captured() {
                continue;
            }
            let expected_index =
                checked_function_index(captured.len(), "function variable references")?;
            if tree_layout.variable_reference(binding.id()) != Some(expected_index) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "captured owner binding has its dense variable-reference index",
                    span: binding.declaration_spans().first().copied(),
                });
            }
            let frame_slot =
                layout
                    .slot(binding.id())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "captured owner binding has a frame slot",
                        span: binding.declaration_spans().first().copied(),
                    })?;
            let captured_binding = match frame_slot {
                FrameSlot::Argument(slot) => CompilerCapturedBinding::Argument(u32::from(slot.0)),
                FrameSlot::Local(slot) => {
                    if binding_has_scope(binding.policy()) {
                        CompilerCapturedBinding::ScopedLocal(u32::from(slot.index()))
                    } else {
                        CompilerCapturedBinding::FunctionLocal(u32::from(slot.index()))
                    }
                }
                FrameSlot::Capture(_) => {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "captured owner binding is not an imported capture",
                        span: binding.declaration_spans().first().copied(),
                    });
                }
            };
            captured.push(captured_binding);
        }
        let mut capture_layout = CompilerCaptureLayout::new(Arc::from(captured));
        let executable_metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if !executable_metadata.is_strict()
            && executable_metadata.has_simple_parameter_list()
            && bindings
                .iter()
                .any(crate::storage::BindingStorage::is_arguments_object)
        {
            capture_layout = capture_layout
                .with_mapped_arguments(Arc::from(executable_metadata.mapped_parameter_indices()));
        }
        Ok(capture_layout)
    }

    fn compiled_variable_definitions(
        &self,
        executable: ExecutableId,
        function_scope: ScopeId,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<Vec<VariableDefinition>, LeafCompilationError> {
        let executable_metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let argument_count =
            usize::try_from(executable_metadata.parameter_count()).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "function argument definitions",
                }
            })?;
        let mut arguments = vec![None; argument_count];
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        for binding in bindings {
            let StoragePlacement::Argument { parameter_index } = binding.placement() else {
                continue;
            };
            let index = usize::try_from(parameter_index).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "function argument definitions",
                }
            })?;
            let target =
                arguments
                    .get_mut(index)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "argument binding indexes its parameter position",
                        span: binding.declaration_spans().first().copied(),
                    })?;
            if target.is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one compiler binding per simple parameter position",
                    span: binding.declaration_spans().first().copied(),
                });
            }
            *target = Some(Self::compiled_variable_definition(
                binding,
                ScopeLink::End,
                false,
                tree_layout,
                constants,
            )?);
        }
        if executable_metadata.has_simple_parameter_list() {
            Self::complete_duplicate_parameter_definitions(
                executable_metadata,
                argument_count,
                &mut arguments,
            )?;
        } else {
            for (index, argument) in arguments.iter_mut().enumerate() {
                if argument.is_none() {
                    let index = u32::try_from(index).map_err(|_| {
                        LeafCompilationError::CapacityExceeded {
                            domain: "raw parameter definitions",
                        }
                    })?;
                    *argument = Some(raw_parameter_definition(constants, index)?);
                }
            }
        }

        let scope_links = self.compiled_local_scope_links(function_scope, layout)?;
        let capacity = argument_count.checked_add(layout.locals.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "function variable definitions",
            },
        )?;
        let mut definitions = Vec::with_capacity(capacity);
        definitions.extend(arguments.into_iter().flatten());
        for (local, scope_next) in layout.locals.iter().zip(scope_links) {
            let binding = self.planned.plan.binding(local.binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "local definition binding exists",
                    span: Some(executable_metadata.span()),
                },
            )?;
            definitions.push(Self::compiled_variable_definition(
                binding,
                scope_next,
                binding_has_scope(binding.policy()),
                tree_layout,
                constants,
            )?);
        }
        Ok(definitions)
    }

    fn complete_duplicate_parameter_definitions(
        executable: &Executable,
        argument_count: usize,
        arguments: &mut [Option<VariableDefinition>],
    ) -> Result<(), LeafCompilationError> {
        if executable.parameter_binding_indices().len() != argument_count {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "every simple parameter has a binding position",
                span: Some(executable.span()),
            });
        }
        for index in 0..argument_count {
            if arguments[index].is_some() {
                continue;
            }
            let representative = usize::try_from(executable.parameter_binding_indices()[index])
                .map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "function parameter bindings",
                })?;
            if representative == index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "a binding-owning parameter has an argument definition",
                    span: Some(executable.span()),
                });
            }
            let representative = arguments
                .get(representative)
                .and_then(Option::as_ref)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "duplicate parameter names a binding-owning formal position",
                    span: Some(executable.span()),
                })?;
            arguments[index] = Some(VariableDefinition::new(
                representative.name(),
                ScopeLink::End,
                representative.policy(),
                false,
                None,
            ));
        }
        Ok(())
    }

    fn compiled_variable_definition(
        binding: &crate::storage::BindingStorage,
        scope_next: ScopeLink,
        has_scope: bool,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<VariableDefinition, LeafCompilationError> {
        let variable_reference = tree_layout.variable_reference(binding.id()).map(u32::from);
        if binding.is_frame_captured() != variable_reference.is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "captured binding has one dense variable-reference index",
                span: binding.declaration_spans().first().copied(),
            });
        }
        let mut definition = VariableDefinition::new(
            Some(constants.metadata_atom_index(CompiledMetadataAtomKey::Binding(binding.id()))?),
            scope_next,
            verified_storage_policy(binding)?,
            has_scope,
            variable_reference,
        );
        if let Some(initializer) = tree_layout.function_declaration(binding.id()) {
            definition =
                definition.with_function_initializer(constants.function_index(initializer)?);
        }
        Ok(definition)
    }

    fn compiled_local_scope_links(
        &self,
        function_scope: ScopeId,
        layout: &FrameLayout,
    ) -> Result<Vec<ScopeLink>, LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        let mut groups = Vec::with_capacity(layout.locals.len());
        let mut preceding = Vec::with_capacity(layout.locals.len());
        let mut first_by_scope = HashMap::new();
        for (index, local) in layout.locals.iter().enumerate() {
            let binding = self.planned.plan.binding(local.binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-linked local binding exists",
                    span: None,
                },
            )?;
            let semantic_scope = self.scope_for_binding(binding.id())?;
            let group = if !binding_has_scope(binding.policy()) {
                LogicalCompilerScope::Function
            } else if semantic_scope == function_scope {
                LogicalCompilerScope::Body
            } else {
                LogicalCompilerScope::Oxc(semantic_scope)
            };
            let index =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "function local scope links",
                })?;
            preceding.push(first_by_scope.insert(group, index));
            groups.push(group);
        }

        let mut links = Vec::with_capacity(layout.locals.len());
        for (index, (&group, same_scope)) in groups.iter().zip(preceding).enumerate() {
            if let Some(previous) = same_scope {
                links.push(ScopeLink::Local(previous));
                continue;
            }
            let parent = match group {
                LogicalCompilerScope::Function | LogicalCompilerScope::Body => None,
                LogicalCompilerScope::Oxc(scope) => {
                    let mut parent = scoping.scope_parent_id(scope);
                    let mut found = None;
                    while let Some(candidate) = parent {
                        if candidate == function_scope {
                            found = first_by_scope.get(&LogicalCompilerScope::Body).copied();
                            break;
                        }
                        if let Some(first) = first_by_scope
                            .get(&LogicalCompilerScope::Oxc(candidate))
                            .copied()
                        {
                            found = Some(first);
                            break;
                        }
                        parent = scoping.scope_parent_id(candidate);
                    }
                    found
                }
            };
            let current =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "function local scope links",
                })?;
            if parent == Some(current) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "local scope link does not target itself",
                    span: None,
                });
            }
            links.push(parent.map_or(ScopeLink::End, ScopeLink::Local));
        }
        Ok(links)
    }

    fn compiled_closure_definitions(
        &self,
        closures: &[CompiledClosureVariable],
        realm_globals: &[CompiledRealmGlobal],
        constants: &CompiledConstantPool,
    ) -> Result<Vec<VerifiedClosureVariableDefinition>, LeafCompilationError> {
        let capacity = closures.len().checked_add(realm_globals.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "closure metadata definitions",
            },
        )?;
        let mut definitions = Vec::with_capacity(capacity);
        for closure in closures {
            let binding = self.planned.plan.binding(closure.binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "closure metadata binding exists",
                    span: None,
                },
            )?;
            let source = match closure.source {
                CompiledClosureSource::ParentVariableReference(index) => {
                    CompilerGraphClosureSource::ParentVariableReference(u32::from(index))
                }
                CompiledClosureSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            };
            definitions.push(VerifiedClosureVariableDefinition::new(
                Some(
                    constants
                        .metadata_atom_index(CompiledMetadataAtomKey::Binding(closure.binding))?,
                ),
                verified_storage_policy(binding)?,
                source,
            ));
        }
        for global in realm_globals {
            let name = global.atom;
            let source = match global.source {
                CompiledRealmGlobalSource::ConstructorRealm => {
                    CompilerGraphClosureSource::ConstructorRealmGlobal(name)
                }
                CompiledRealmGlobalSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            };
            let mut definition =
                VerifiedClosureVariableDefinition::realm_global(Some(name), global.policy, source);
            if let Some(initializer) = global.function_initializer {
                definition = definition.with_function_initializer(initializer);
            }
            definitions.push(definition);
        }
        Ok(definitions)
    }

    fn compiled_closure_variables(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Vec<CompiledClosureVariable>, LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let captures = self
            .planned
            .plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if captures.is_empty() {
            return Ok(Vec::new());
        }
        let parent = metadata
            .parent()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "capturing executable has an immediate parent",
                span: Some(metadata.span()),
            })?;
        let parent_captures = self
            .planned
            .plan
            .frame_captures_for(parent)
            .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
        let mut variables = Vec::with_capacity(captures.len());
        let mut sources = Vec::with_capacity(captures.len());
        for (expected_slot, capture) in captures.iter().enumerate() {
            if capture.slot().index() != expected_slot {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled closure-variable slots are dense and ordered",
                    span: self
                        .planned
                        .plan
                        .binding(capture.binding())
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
            let binding = self.planned.plan.binding(capture.binding()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "compiled closure variable has an original binding",
                    span: None,
                },
            )?;
            let source = match capture.source() {
                CaptureSource::ParentBinding(source_binding) => {
                    if source_binding != capture.binding() || binding.executable() != parent {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "parent-binding closure source names the captured parent binding",
                            span: binding.declaration_spans().first().copied(),
                        });
                    }
                    let index = tree_layout.variable_reference(source_binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "parent-binding closure source has a variable-reference cell",
                            span: binding.declaration_spans().first().copied(),
                        },
                    )?;
                    CompiledClosureSource::ParentVariableReference(index)
                }
                CaptureSource::ParentCapture(source_slot) => {
                    let source_capture = parent_captures.get(source_slot.index()).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "forwarded closure source indexes the parent environment",
                            span: binding.declaration_spans().first().copied(),
                        },
                    )?;
                    if source_capture.slot() != source_slot
                        || source_capture.binding() != capture.binding()
                    {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "forwarded closure source preserves the original binding identity",
                            span: binding.declaration_spans().first().copied(),
                        });
                    }
                    CompiledClosureSource::ParentClosure(checked_function_index(
                        source_slot.index(),
                        "parent closure variables",
                    )?)
                }
            };
            sources.push(source);
            variables.push(CompiledClosureVariable {
                binding: capture.binding(),
                slot: capture.slot(),
                source,
                policy: binding.policy(),
            });
        }
        sources.sort_unstable();
        if sources.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled closure sources are unique within one child",
                span: Some(metadata.span()),
            });
        }
        Ok(variables)
    }

    fn compiled_realm_globals(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<Vec<CompiledRealmGlobal>, LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let imports = tree_layout.realm_globals.imports_for(executable)?;
        let mut globals = Vec::with_capacity(imports.len());
        for &id in imports {
            let binding = tree_layout.realm_globals.binding(id).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "constructor-realm global import has a binding descriptor",
                    span: Some(metadata.span()),
                },
            )?;
            let slot =
                tree_layout
                    .realm_globals
                    .closure_slot(&self.planned.plan, executable, id)?;
            let source = if let Some(parent) = metadata.parent() {
                CompiledRealmGlobalSource::ParentClosure(tree_layout.realm_globals.closure_slot(
                    &self.planned.plan,
                    parent,
                    id,
                )?)
            } else {
                if self.unit.goal()
                    != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
                    || executable.index() != 0
                {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "only a dynamic Function Script root originates realm-global slots",
                        span: Some(metadata.span()),
                    });
                }
                CompiledRealmGlobalSource::ConstructorRealm
            };
            let function_initializer = if source == CompiledRealmGlobalSource::ConstructorRealm
                && binding.policy.kind() == VerifiedBindingKind::Function
            {
                let declaration = binding.declaration.ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant:
                            "constructor-realm function retains its declared binding identity",
                        span: Some(binding.first_span),
                    },
                )?;
                let child = tree_layout.function_declaration(declaration).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant:
                            "constructor-realm function declaration selects its last initializer",
                        span: Some(binding.first_span),
                    },
                )?;
                Some(constants.function_index(child)?)
            } else {
                None
            };
            globals.push(CompiledRealmGlobal {
                id,
                name: Arc::clone(&binding.name),
                atom: constants.metadata_atom_index(CompiledMetadataAtomKey::RealmGlobal(id))?,
                slot,
                source,
                policy: binding.policy,
                function_initializer,
            });
        }
        Ok(globals)
    }

    fn scope_for_binding(&self, binding: BindingId) -> Result<ScopeId, LeafCompilationError> {
        self.planned
            .identities
            .scope_by_binding
            .get(binding.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "compiler binding has an Oxc scope identity",
                span: self
                    .planned
                    .plan
                    .binding(binding)
                    .and_then(|storage| storage.declaration_spans().first().copied()),
            })
    }

    fn binding_for_identifier(
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

    fn lowered_reference(
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

    fn validate_lowered_mutation_reference(
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

fn is_noncomputed_static_property_key_node(unit: &ParsedUnit<'_, '_>, node_id: NodeId) -> bool {
    let AstKind::ObjectProperty(property) = unit.semantic().nodes().parent_kind(node_id) else {
        return false;
    };
    if property.computed {
        return false;
    }
    match &property.key {
        OxcPropertyKey::StringLiteral(literal) => literal.node_id.get() == node_id,
        OxcPropertyKey::NumericLiteral(literal) => literal.node_id.get() == node_id,
        OxcPropertyKey::BigIntLiteral(literal) => literal.node_id.get() == node_id,
        _ => false,
    }
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
