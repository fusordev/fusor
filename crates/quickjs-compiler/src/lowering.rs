use std::{collections::HashMap, sync::Arc};

use oxc_ast::{
    AstKind,
    ast::{
        Argument, ArrayAssignmentTarget, ArrayExpression, ArrayExpressionElement, ArrayPattern,
        AssignmentExpression, AssignmentTarget, AssignmentTargetMaybeDefault,
        AssignmentTargetProperty, AssignmentTargetRest, BindingIdentifier, BindingPattern,
        BindingRestElement, BlockStatement, CallExpression, CatchClause, ComputedMemberExpression,
        ConditionalExpression, DoWhileStatement, Expression, ExpressionStatement, ForInStatement,
        ForOfStatement, ForStatement, ForStatementInit, ForStatementLeft, Function, FunctionType,
        IdentifierReference, IfStatement, LabelIdentifier, LabeledStatement, LogicalExpression,
        NewExpression, ObjectAssignmentTarget, ObjectExpression, ObjectPattern, ObjectProperty,
        ObjectPropertyKind, Program, PropertyKey as OxcPropertyKey, PropertyKind, ReturnStatement,
        SequenceExpression, SimpleAssignmentTarget, Statement, StaticMemberExpression,
        SwitchStatement, ThrowStatement, TryStatement, UnaryExpression, UpdateExpression,
        VariableDeclaration, VariableDeclarationKind, VariableDeclarator, WhileStatement,
    },
};
use oxc_semantic::{NodeId, ReferenceId, ScopeId, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use quickjs_bytecode::{
    AtomPoolIndex, Binary64Constant, BranchKind, BytecodeGraphVerificationLimits,
    ClosureVariableDefinition as VerifiedClosureVariableDefinition, CompilerAtom,
    CompilerBindingKind as VerifiedBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerCapturedBinding, CompilerClosureSource as CompilerGraphClosureSource,
    CompilerConstantLayout, CompilerExecutableKind,
    CompilerInitializationPolicy as VerifiedInitializationPolicy, CompilerSource,
    CompilerWritePolicy as VerifiedWritePolicy, FinalOpcode, FunctionGraphVerificationLimits,
    FunctionIndexDomains, MAX_FUNCTION_INDEX_ENTRIES, Operands, PcSourceSpan, ScopeLink,
    SourceByteSpan, UnverifiedCompilerBytecodeGraph, UnverifiedFunctionHeader,
    UnverifiedFunctionMetadata, VariableDefinition, VerificationLimits,
    verify_compiler_bytecode_graph,
};
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
use function::{FunctionLoweringSession, FunctionPlanningContext};
use function_graph::verify_compiled_function_graph;
use layouts::{
    ArgumentSlot, FrameLayout, FrameLayoutInput, FrameSlot, FunctionTreeLayout,
    FunctionTreeLayoutInput, FunctionTreeLayoutSeed, FunctionTreeLayoutSeedInput,
};
use plan::{
    AbruptMarker, AbruptMarkerKind, AbruptMarkerTag, ControlRegion,
    DestructuringBindingInitialization, ExpressionPlanner, ExpressionWork, LogicalCompilerScope,
    LoopJump, LoweredReference, ScopeEntryInitialization, StatementCompletion,
    StatementControlStack, StatementPlanningState, StatementWork, SwitchControlLabels,
    TryFinallyCatchPlan, TryFinallyLabels, switch_scaffold_instruction_count,
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

    fn compile_function(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<CompiledFunction, LeafCompilationError> {
        let validated = self.validate_executable(executable, tree_layout, limits)?;
        let ValidatedFunction {
            executable_kind,
            strict,
            argument_count,
            defined_argument_count,
            local_count,
            capture_count,
            capture_layout,
            locals,
            constants,
            atoms,
            closure_variables,
            realm_globals,
            function_name,
            variable_definitions,
            closure_definitions,
            function_span,
            function_name_span,
            flow,
        } = validated;
        let atom_count =
            u32::try_from(atoms.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "atom pool entries",
            })?;
        let constant_count =
            u32::try_from(constants.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "constant pool entries",
            })?;
        let domains = FunctionIndexDomains::new(
            atom_count,
            constant_count,
            argument_count,
            local_count,
            capture_count,
        );
        let variable_reference_count = checked_function_entry_count(
            capture_layout.bindings().len(),
            "function variable references",
        )?;
        let header = executable_header(
            executable_kind,
            strict,
            self.planned
                .plan
                .executable(executable)
                .ok_or(LeafCompilationError::InvalidExecutable { executable })?
                .has_simple_parameter_list(),
            defined_argument_count,
            variable_reference_count,
        );
        let constant_layout = CompilerConstantLayout::new(
            constants
                .iter()
                .map(CompiledConstant::kind)
                .collect::<Vec<_>>()
                .into(),
        );
        let finished = flow.finish()?;
        let (source_instructions, control_flow) = finished.verify_with_layouts(
            domains,
            header,
            capture_layout,
            constant_layout,
            limits,
        )?;
        let source_mappings = source_instructions
            .iter()
            .map(|instruction| {
                PcSourceSpan::new(instruction.pc(), source_byte_span(instruction.span()))
            })
            .collect::<Vec<_>>();
        let metadata = UnverifiedFunctionMetadata::new(
            function_name,
            variable_definitions.into(),
            closure_definitions.into(),
            CompilerSource::new(
                Arc::clone(&self.source_name),
                Arc::clone(&self.source_text),
                function_span,
                function_name_span,
                source_mappings.into(),
            ),
        )
        .with_executable_kind(executable_kind);

        Ok(CompiledFunction {
            executable,
            storage_plan: Arc::clone(&self.planned.plan),
            source_text: Arc::clone(&self.source_text),
            locals: locals.into(),
            atoms,
            constants,
            closure_variables: closure_variables.into(),
            realm_globals: realm_globals.into(),
            source_instructions: source_instructions.into(),
            control_flow: Arc::new(control_flow),
            metadata,
        })
    }

    fn validate_executable(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        match metadata.kind() {
            ExecutableKind::Script {
                asynchronous: false,
            } => self.validate_dynamic_function_script(executable, tree_layout, limits),
            _ => self.validate_function(executable, tree_layout, limits),
        }
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

    fn validate_function(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, function, form) = self.selected_ordinary_function(executable_id)?;
        let layout = FrameLayout::new(FrameLayoutInput::new(&self.planned.plan, executable_id))?;
        let body = function
            .body
            .as_ref()
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBody,
                span: function.span,
            })?;
        let constants = tree_layout.constant_pool(executable_id)?;
        let planning = FunctionPlanningContext {
            executable: executable_id,
            layout: &layout,
            tree_layout,
            constants,
        };
        let flow = FunctionLoweringSession::for_function(self, function, body, planning, limits)?
            .lower()?;
        let function_scope = self.created_scope(
            function.scope_id.get(),
            function.node_id.get(),
            function.span,
        )?;
        let capture_layout =
            self.compiler_capture_layout(executable_id, function_scope, &layout, tree_layout)?;
        let closure_variables = self.compiled_closure_variables(executable_id, tree_layout)?;
        let realm_globals = self.compiled_realm_globals(executable_id, tree_layout, constants)?;
        let (executable_kind, function_span, function_name, function_name_span) = match form {
            OrdinaryFunctionForm::Function => (
                CompilerExecutableKind::OrdinaryFunction,
                function.span,
                executable
                    .name()
                    .map(|_| constants.metadata_atom_index(CompiledMetadataAtomKey::FunctionName))
                    .transpose()?,
                executable.name_span().map(source_byte_span),
            ),
            OrdinaryFunctionForm::ObjectMethod {
                property_span: source_span,
            } => (
                CompilerExecutableKind::OrdinaryMethod,
                source_span,
                None,
                None,
            ),
        };
        let variable_definitions = self.compiled_variable_definitions(
            executable_id,
            function_scope,
            &layout,
            tree_layout,
            constants,
        )?;
        let closure_definitions =
            self.compiled_closure_definitions(&closure_variables, &realm_globals, constants)?;
        let capture_count = checked_function_entry_count(
            closure_variables
                .len()
                .checked_add(realm_globals.len())
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function closure variables",
                })?,
            "function closure variables",
        )?;

        Ok(ValidatedFunction {
            executable_kind,
            strict: executable.is_strict(),
            argument_count: executable.parameter_count(),
            defined_argument_count: executable.defined_parameter_count(),
            local_count: layout.local_count,
            capture_count,
            capture_layout,
            locals: layout
                .locals
                .iter()
                .map(|local| LoweredLocal {
                    binding: local.binding,
                    slot: local.slot,
                })
                .collect(),
            constants: Arc::clone(constants.entries()),
            atoms: Arc::clone(constants.atoms()),
            closure_variables,
            realm_globals,
            function_name,
            variable_definitions,
            closure_definitions,
            function_span: source_byte_span(function_span),
            function_name_span,
            flow,
        })
    }

    fn validate_dynamic_function_script(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, program) = self.selected_dynamic_function_script(executable_id)?;
        let layout = FrameLayout::new(
            FrameLayoutInput::new(&self.planned.plan, executable_id).with_internal_locals(1),
        )?;
        let completion = layout.internal_local(0)?;
        let constants = tree_layout.constant_pool(executable_id)?;
        let planning = FunctionPlanningContext {
            executable: executable_id,
            layout: &layout,
            tree_layout,
            constants,
        };
        let flow =
            FunctionLoweringSession::for_program(self, program, completion, planning, limits)?
                .lower()?;
        let program_scope =
            self.created_scope(program.scope_id.get(), program.node_id.get(), program.span)?;
        let capture_layout =
            self.compiler_capture_layout(executable_id, program_scope, &layout, tree_layout)?;
        let closure_variables = self.compiled_closure_variables(executable_id, tree_layout)?;
        if !closure_variables.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Function Script root imports no caller closure",
                span: Some(program.span),
            });
        }
        let realm_globals = self.compiled_realm_globals(executable_id, tree_layout, constants)?;
        let mut variable_definitions = self.compiled_variable_definitions(
            executable_id,
            program_scope,
            &layout,
            tree_layout,
            constants,
        )?;
        variable_definitions.push(script_completion_variable_definition(constants)?);
        let closure_definitions =
            self.compiled_closure_definitions(&closure_variables, &realm_globals, constants)?;
        let capture_count =
            checked_function_entry_count(realm_globals.len(), "function closure variables")?;

        Ok(ValidatedFunction {
            executable_kind: CompilerExecutableKind::DynamicFunctionScript,
            strict: executable.is_strict(),
            argument_count: 0,
            defined_argument_count: 0,
            local_count: layout.local_count,
            capture_count,
            capture_layout,
            locals: layout
                .locals
                .iter()
                .map(|local| LoweredLocal {
                    binding: local.binding,
                    slot: local.slot,
                })
                .collect(),
            constants: Arc::clone(constants.entries()),
            atoms: Arc::clone(constants.atoms()),
            closure_variables,
            realm_globals,
            function_name: None,
            variable_definitions,
            closure_definitions,
            function_span: source_byte_span(program.span),
            function_name_span: None,
            flow,
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
        reason = "the iterative statement dispatcher keeps one explicit work-stack loop"
    )]
    fn process_statement_work<'statement>(
        &self,
        task: StatementWork<'statement, 'arena>,
        body_span: Span,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        match task {
            StatementWork::VisitList { statements, next } => {
                if let Some(statement) = statements.get(next) {
                    state.work.push(StatementWork::VisitList {
                        statements,
                        next: next + 1,
                    });
                    state.work.push(StatementWork::Visit(statement));
                }
            }
            StatementWork::PushScope {
                scope,
                creator,
                span,
            } => {
                self.plan_scope_entry(scope, creator, span, planning, flow)?;
                state.active_scopes.push(scope);
            }
            StatementWork::PopScope(expected) => {
                if state.active_scopes.len() > 1 {
                    self.plan_scope_exit(planning.executable, expected, planning.layout, flow)?;
                }
                let actual =
                    state
                        .active_scopes
                        .pop()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "statement scope stack is nonempty on exit",
                            span: Some(body_span),
                        })?;
                if actual != expected {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "statement scopes exit in last-in-first-out order",
                        span: Some(body_span),
                    });
                }
            }
            StatementWork::CloseScope(scope) => {
                self.plan_scope_exit(planning.executable, scope, planning.layout, flow)?;
            }
            StatementWork::PushStatementStackBase { span } => {
                flow.push_statement_stack_base(span)?;
            }
            StatementWork::PopStatementStackBase { span } => {
                flow.pop_statement_stack_base(span)?;
            }
            StatementWork::PushControl(mut control) => {
                let owned_iteration_marker = control.owned_iteration_marker;
                if owned_iteration_marker.is_some() {
                    state.abrupt_markers.try_reserve(1).map_err(|_| {
                        LeafCompilationError::CapacityExceeded {
                            domain: "statement abrupt-marker stack",
                        }
                    })?;
                }
                let abrupt_marker_depth = state
                    .abrupt_markers
                    .len()
                    .checked_add(usize::from(owned_iteration_marker.is_some()))
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "statement abrupt-marker depth",
                    })?;
                control.abrupt_marker_depth = Some(abrupt_marker_depth);
                let owned_marker_scope_depth = control.owned_marker_scope_depth;
                state.controls.push(control, body_span)?;
                if let Some(marker) = owned_iteration_marker {
                    let scope_depth = owned_marker_scope_depth.ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "owned iteration marker has a scope depth",
                            span: Some(body_span),
                        },
                    )?;
                    if scope_depth > state.active_scopes.len() {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "owned iteration marker scope remains active",
                            span: Some(body_span),
                        });
                    }
                    state
                        .abrupt_markers
                        .push(AbruptMarker::new(marker.abrupt_kind(), scope_depth));
                }
            }
            StatementWork::PopControl => {
                let control = state.controls.pop(body_span)?;
                let expected_depth =
                    control
                        .abrupt_marker_depth
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "active statement control has an abrupt-marker depth",
                            span: Some(body_span),
                        })?;
                if state.abrupt_markers.len() != expected_depth {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "statement control exits at its abrupt-marker depth",
                        span: Some(body_span),
                    });
                }
                if let Some(marker) = control.owned_iteration_marker
                    && state.abrupt_markers.pop().map(|marker| marker.tag()) != Some(marker.tag())
                {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "iteration control owns the innermost abrupt marker",
                        span: Some(body_span),
                    });
                }
            }
            StatementWork::PushAbruptMarker(kind) => {
                state.abrupt_markers.try_reserve(1).map_err(|_| {
                    LeafCompilationError::CapacityExceeded {
                        domain: "statement abrupt-marker stack",
                    }
                })?;
                state
                    .abrupt_markers
                    .push(AbruptMarker::new(kind, state.active_scopes.len()));
            }
            StatementWork::PopAbruptMarker(expected) => {
                if state.abrupt_markers.pop().map(|marker| marker.tag()) != Some(expected) {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "statement abrupt markers exit in last-in-first-out order",
                        span: Some(body_span),
                    });
                }
            }
            StatementWork::SetCompletion(completion) => {
                state.completion = completion;
            }
            StatementWork::ForInHead(left) => self.plan_for_in_head(
                left,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?,
            StatementWork::ForOfHead(left) => {
                self.plan_for_of_head(left, planning.layout)?;
            }
            StatementWork::ForInAssignment(left) => self.plan_for_in_assignment(
                left,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?,
            StatementWork::ForOfAssignment(left) => self.plan_for_of_assignment(
                left,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?,
            StatementWork::ForOfRotate(scope) => {
                self.plan_for_of_rotation(planning.executable, scope, planning.layout, flow)?;
            }
            StatementWork::Expression(expression) => self.plan_expression(
                expression,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?,
            StatementWork::Declaration(declaration) => self.validate_declaration(
                declaration,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?,
            StatementWork::Emit(instruction) => flow.emit(instruction)?,
            StatementWork::Branch { kind, target, span } => {
                flow.branch(kind, &target, span)?;
            }
            StatementWork::Bind(label) => flow.bind(&label)?,
            StatementWork::SwitchDispatch {
                statement,
                labels,
                next,
            } => Self::schedule_next_switch_dispatch(statement, labels, next, &mut state.work)?,
            StatementWork::SwitchTrampoline {
                statement,
                labels,
                next,
            } => Self::schedule_next_switch_trampoline(statement, labels, next, &mut state.work)?,
            StatementWork::SwitchNoMatch { labels, done, span } => {
                Self::schedule_switch_no_match(&labels, done, span, &mut state.work);
            }
            StatementWork::SwitchBody {
                statement,
                labels,
                next,
            } => Self::schedule_next_switch_body(statement, labels, next, &mut state.work)?,
            StatementWork::Visit(statement) => self.plan_statement(
                statement,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
                state,
            )?,
            StatementWork::VisitBlock(block) => {
                self.schedule_block_statement(block, state)?;
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the statement-kind dispatcher keeps lowering decisions in one exhaustive match"
    )]
    fn plan_statement<'statement>(
        &self,
        statement: &'statement Statement<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        match statement {
            Statement::BlockStatement(block) => {
                self.schedule_block_statement(block, state)?;
            }
            Statement::FunctionDeclaration(function) => {
                self.validate_function_declaration(
                    function,
                    layout.executable,
                    tree_layout,
                    state.active_scopes.last().copied(),
                )?;
            }
            Statement::VariableDeclaration(declaration) => {
                self.validate_declaration(declaration, layout, tree_layout, constants, flow)?;
            }
            Statement::ExpressionStatement(statement) => {
                Self::schedule_expression_statement(statement, state.completion, &mut state.work);
            }
            Statement::DebuggerStatement(statement) => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Nop,
                    Operands::None,
                    statement.span,
                ))?;
            }
            Statement::EmptyStatement(_) => {}
            Statement::ReturnStatement(statement) => {
                Self::schedule_return_statement(
                    statement,
                    &state.abrupt_markers,
                    flow,
                    &mut state.work,
                )?;
            }
            Statement::ThrowStatement(statement) => {
                Self::schedule_throw_statement(statement, &state.abrupt_markers, &mut state.work)?;
            }
            Statement::IfStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_if_statement(statement, flow, &mut state.work)?;
            }
            Statement::WhileStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_while_statement(
                    statement,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                    Vec::new(),
                )?;
            }
            Statement::DoWhileStatement(statement) => {
                Self::schedule_do_while_statement(
                    statement,
                    state.completion,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                    Vec::new(),
                )?;
            }
            Statement::ForStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::ForInStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_in_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::ForOfStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_of_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::BreakStatement(statement) => {
                self.plan_control_jump(
                    statement.label.as_ref(),
                    statement.span,
                    LoopJump::Break,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::ContinueStatement(statement) => {
                self.plan_control_jump(
                    statement.label.as_ref(),
                    statement.span,
                    LoopJump::Continue,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::LabeledStatement(statement) => {
                self.plan_labeled_statement(statement, flow, state)?;
            }
            Statement::SwitchStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_switch_statement(statement, Vec::new(), flow, state)?;
            }
            Statement::TryStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_try_statement(statement, layout, flow, state)?;
            }
            _ => {
                return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span());
            }
        }
        Ok(())
    }

    fn schedule_block_statement<'statement>(
        &self,
        block: &'statement BlockStatement<'arena>,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(block.scope_id.get(), block.node_id.get(), block.span)?;
        state.work.push(StatementWork::PopScope(scope));
        state.work.push(StatementWork::VisitList {
            statements: &block.body,
            next: 0,
        });
        state.work.push(StatementWork::PushScope {
            scope,
            creator: block.node_id.get(),
            span: block.span,
        });
        Ok(())
    }

    fn schedule_expression_statement<'statement>(
        statement: &'statement ExpressionStatement<'arena>,
        completion: StatementCompletion,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        let (opcode, operands) = match completion {
            StatementCompletion::Discard => (FinalOpcode::Drop, Operands::None),
            StatementCompletion::Script(slot) => compact_put_local(slot),
        };
        work.push(StatementWork::Emit(PlannedInstruction::new(
            opcode,
            operands,
            statement.expression.span(),
        )));
        work.push(StatementWork::Expression(&statement.expression));
    }

    fn reset_script_completion(
        completion: StatementCompletion,
        span: Span,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let StatementCompletion::Script(slot) = completion else {
            return Ok(());
        };
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            span,
        ))?;
        let (opcode, operands) = compact_put_local(slot);
        flow.emit(PlannedInstruction::new(opcode, operands, span))
    }

    fn schedule_return_statement<'statement>(
        statement: &'statement ReturnStatement<'arena>,
        abrupt_markers: &[AbruptMarker],
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let crosses_finalizer = abrupt_markers
            .iter()
            .any(|marker| matches!(&marker.kind, AbruptMarkerKind::Catch { finalizer: Some(_) }));
        let has_pending_finally_subroutine = abrupt_markers
            .iter()
            .any(|marker| matches!(&marker.kind, AbruptMarkerKind::FinallySubroutine));
        let closes_iterator = abrupt_markers
            .iter()
            .any(|marker| matches!(&marker.kind, AbruptMarkerKind::ForOf));
        let has_physical_marker = abrupt_markers.iter().any(|marker| {
            matches!(
                &marker.kind,
                AbruptMarkerKind::Catch { .. } | AbruptMarkerKind::ForIn | AbruptMarkerKind::ForOf
            )
        });
        if let Some(argument) = &statement.argument {
            Self::reserve_return_work(abrupt_markers, work)?;
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Return,
                Operands::None,
                statement.span,
            )));
            Self::schedule_value_return_cleanup(abrupt_markers, statement.span, work);
            work.push(StatementWork::Expression(argument));
        } else if crosses_finalizer
            || closes_iterator
            || (has_pending_finally_subroutine && has_physical_marker)
        {
            Self::reserve_return_work(abrupt_markers, work)?;
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Return,
                Operands::None,
                statement.span,
            )));
            Self::schedule_value_return_cleanup(abrupt_markers, statement.span, work);
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                statement.span,
            )));
        } else {
            for marker in abrupt_markers.iter().rev() {
                match &marker.kind {
                    AbruptMarkerKind::Catch { .. } | AbruptMarkerKind::ForIn => {
                        flow.emit(PlannedInstruction::new(
                            FinalOpcode::Drop,
                            Operands::None,
                            statement.span,
                        ))?;
                    }
                    AbruptMarkerKind::ForOf => {
                        flow.emit(PlannedInstruction::new(
                            FinalOpcode::IteratorClose,
                            Operands::None,
                            statement.span,
                        ))?;
                    }
                    AbruptMarkerKind::FinallySubroutine => {}
                }
            }
            flow.emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                statement.span,
            ))?;
        }
        Ok(())
    }

    fn reserve_return_work<'statement>(
        abrupt_markers: &[AbruptMarker],
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let capacity = abrupt_markers
            .len()
            .checked_mul(4)
            .and_then(|capacity| capacity.checked_add(2))
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "statement return cleanup",
            })?;
        work.try_reserve(capacity)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })
    }

    fn schedule_value_return_cleanup<'statement>(
        abrupt_markers: &[AbruptMarker],
        span: Span,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        for marker in abrupt_markers {
            match &marker.kind {
                AbruptMarkerKind::Catch { finalizer } => {
                    if let Some(finalizer) = finalizer {
                        work.push(StatementWork::Branch {
                            kind: BranchKind::Gosub,
                            target: finalizer.clone(),
                            span,
                        });
                    }
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::NipCatch,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::ForIn => {
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Nip,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::ForOf => {
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::IteratorClose,
                        Operands::None,
                        span,
                    )));
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Undefined,
                        Operands::None,
                        span,
                    )));
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Rot3r,
                        Operands::None,
                        span,
                    )));
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::NipCatch,
                        Operands::None,
                        span,
                    )));
                }
                AbruptMarkerKind::FinallySubroutine => {
                    for _ in 0..2 {
                        work.push(StatementWork::Emit(PlannedInstruction::new(
                            FinalOpcode::Nip,
                            Operands::None,
                            span,
                        )));
                    }
                }
            }
        }
    }

    fn schedule_throw_statement<'statement>(
        statement: &'statement ThrowStatement<'arena>,
        abrupt_markers: &[AbruptMarker],
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let removable_markers = abrupt_markers
            .iter()
            .rposition(|marker| marker.tag() == AbruptMarkerTag::Catch)
            .map_or(abrupt_markers, |catch| &abrupt_markers[catch + 1..]);
        let preserves_for_of_marker = removable_markers
            .iter()
            .any(|marker| marker.tag() == AbruptMarkerTag::ForOf);
        let cleanup_instructions = if preserves_for_of_marker {
            0
        } else {
            removable_markers
                .iter()
                .try_fold(0_usize, |count, marker| {
                    count.checked_add(match marker.tag() {
                        AbruptMarkerTag::ForIn => 1,
                        AbruptMarkerTag::FinallySubroutine => 2,
                        AbruptMarkerTag::Catch | AbruptMarkerTag::ForOf => 0,
                    })
                })
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "statement throw cleanup",
                })?
        };
        work.try_reserve(cleanup_instructions.saturating_add(2))
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Throw,
            Operands::None,
            statement.span,
        )));
        if !preserves_for_of_marker {
            for marker in removable_markers {
                let nips = match marker.tag() {
                    AbruptMarkerTag::ForIn => 1,
                    AbruptMarkerTag::FinallySubroutine => 2,
                    AbruptMarkerTag::Catch | AbruptMarkerTag::ForOf => 0,
                };
                for _ in 0..nips {
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Nip,
                        Operands::None,
                        statement.span,
                    )));
                }
            }
        }
        work.push(StatementWork::Expression(&statement.argument));
        Ok(())
    }

    fn plan_try_statement<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(finalizer) = &statement.finalizer {
            return self.plan_try_finally_statement(statement, finalizer, layout, flow, state);
        }
        self.plan_catch_only_statement(statement, layout, flow, state)
    }

    fn plan_catch_only_statement<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let handler = statement
            .handler
            .as_ref()
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBody,
                span: statement.span,
            })?;
        let catch_scope =
            self.created_scope(handler.scope_id.get(), handler.node_id.get(), handler.span)?;
        let catch_body_scope = self.created_scope(
            handler.body.scope_id.get(),
            handler.body.node_id.get(),
            handler.body.span,
        )?;
        let binding = self.plan_catch_binding(handler, catch_body_scope, layout)?;

        let handler_target = flow.new_statement_label_with_offset(handler.span, 1)?;
        let done = flow.new_statement_label(statement.span)?;
        state
            .work
            .try_reserve(15)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        state.work.push(StatementWork::Bind(done.clone()));
        state.work.push(StatementWork::PopScope(catch_scope));
        state.work.push(StatementWork::VisitBlock(&handler.body));
        state.work.push(StatementWork::Emit(binding));
        state.work.push(StatementWork::PushScope {
            scope: catch_scope,
            creator: handler.node_id.get(),
            span: handler.span,
        });
        state.work.push(StatementWork::Bind(handler_target.clone()));
        state.work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: statement.span,
        });
        state.work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        state
            .work
            .push(StatementWork::PopAbruptMarker(AbruptMarkerTag::Catch));
        state.work.push(StatementWork::PopStatementStackBase {
            span: statement.span,
        });
        state.work.push(StatementWork::VisitBlock(&statement.block));
        state
            .work
            .push(StatementWork::PushAbruptMarker(AbruptMarkerKind::Catch {
                finalizer: None,
            }));
        state.work.push(StatementWork::PushStatementStackBase {
            span: statement.span,
        });
        state.work.push(StatementWork::Branch {
            kind: BranchKind::Catch,
            target: handler_target,
            span: statement.span,
        });
        Ok(())
    }

    fn plan_try_finally_statement<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        finalizer: &'statement BlockStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let labels = TryFinallyLabels {
            handler: flow.new_statement_label_with_offset(statement.span, 1)?,
            finalizer: flow.new_statement_label_with_offset(finalizer.span, 2)?,
            done: flow.new_statement_label(statement.span)?,
        };
        let catch_plan = self.create_try_finally_catch_plan(statement, layout, flow)?;

        state
            .work
            .try_reserve(if catch_plan.is_some() { 48 } else { 32 })
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;

        Self::push_finalizer_subroutine(&mut state.work, finalizer, &labels, state.completion);
        Self::push_try_finally_handler_path(&mut state.work, statement, catch_plan, &labels);
        Self::push_try_finally_body(&mut state.work, statement, &labels);
        Ok(())
    }

    fn create_try_finally_catch_plan<'statement>(
        &self,
        statement: &'statement TryStatement<'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<Option<TryFinallyCatchPlan<'statement, 'arena>>, LeafCompilationError> {
        let Some(handler) = &statement.handler else {
            return Ok(None);
        };
        let scope =
            self.created_scope(handler.scope_id.get(), handler.node_id.get(), handler.span)?;
        let body_scope = self.created_scope(
            handler.body.scope_id.get(),
            handler.body.node_id.get(),
            handler.body.span,
        )?;
        let binding = self.plan_catch_binding(handler, body_scope, layout)?;
        let rethrow = flow.new_statement_label_with_offset(handler.body.span, 1)?;
        Ok(Some(TryFinallyCatchPlan {
            handler,
            scope,
            binding,
            rethrow,
        }))
    }

    fn push_finalizer_subroutine<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        finalizer: &'statement BlockStatement<'arena>,
        labels: &TryFinallyLabels,
        completion: StatementCompletion,
    ) {
        work.push(StatementWork::Bind(labels.done.clone()));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Ret,
            Operands::None,
            finalizer.span,
        )));
        for _ in 0..2 {
            work.push(StatementWork::PopStatementStackBase {
                span: finalizer.span,
            });
        }
        work.push(StatementWork::PopAbruptMarker(
            AbruptMarkerTag::FinallySubroutine,
        ));
        work.push(StatementWork::SetCompletion(completion));
        work.push(StatementWork::VisitBlock(finalizer));
        work.push(StatementWork::SetCompletion(StatementCompletion::Discard));
        work.push(StatementWork::PushAbruptMarker(
            AbruptMarkerKind::FinallySubroutine,
        ));
        for _ in 0..2 {
            work.push(StatementWork::PushStatementStackBase {
                span: finalizer.span,
            });
        }
        work.push(StatementWork::Bind(labels.finalizer.clone()));
    }

    fn push_try_finally_handler_path<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        statement: &'statement TryStatement<'arena>,
        catch_plan: Option<TryFinallyCatchPlan<'statement, 'arena>>,
        labels: &TryFinallyLabels,
    ) {
        if let Some(catch) = catch_plan {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Throw,
                Operands::None,
                catch.handler.body.span,
            )));
            work.push(StatementWork::Branch {
                kind: BranchKind::Gosub,
                target: labels.finalizer.clone(),
                span: catch.handler.body.span,
            });
            work.push(StatementWork::Bind(catch.rethrow.clone()));

            Self::push_normal_finalizer_path(
                work,
                &labels.finalizer,
                &labels.done,
                catch.handler.body.span,
            );
            work.push(StatementWork::PopScope(catch.scope));
            work.push(StatementWork::PopAbruptMarker(AbruptMarkerTag::Catch));
            work.push(StatementWork::PopStatementStackBase {
                span: catch.handler.body.span,
            });
            work.push(StatementWork::VisitBlock(&catch.handler.body));
            work.push(StatementWork::PushAbruptMarker(AbruptMarkerKind::Catch {
                finalizer: Some(labels.finalizer.clone()),
            }));
            work.push(StatementWork::PushStatementStackBase {
                span: catch.handler.body.span,
            });
            work.push(StatementWork::Branch {
                kind: BranchKind::Catch,
                target: catch.rethrow,
                span: catch.handler.body.span,
            });
            work.push(StatementWork::Emit(catch.binding));
            work.push(StatementWork::PushScope {
                scope: catch.scope,
                creator: catch.handler.node_id.get(),
                span: catch.handler.span,
            });
            work.push(StatementWork::Bind(labels.handler.clone()));
        } else {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Throw,
                Operands::None,
                statement.span,
            )));
            work.push(StatementWork::Branch {
                kind: BranchKind::Gosub,
                target: labels.finalizer.clone(),
                span: statement.span,
            });
            work.push(StatementWork::Bind(labels.handler.clone()));
        }
    }

    fn push_try_finally_body<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        statement: &'statement TryStatement<'arena>,
        labels: &TryFinallyLabels,
    ) {
        Self::push_normal_finalizer_path(
            work,
            &labels.finalizer,
            &labels.done,
            statement.block.span,
        );
        work.push(StatementWork::PopAbruptMarker(AbruptMarkerTag::Catch));
        work.push(StatementWork::PopStatementStackBase {
            span: statement.block.span,
        });
        work.push(StatementWork::VisitBlock(&statement.block));
        work.push(StatementWork::PushAbruptMarker(AbruptMarkerKind::Catch {
            finalizer: Some(labels.finalizer.clone()),
        }));
        work.push(StatementWork::PushStatementStackBase {
            span: statement.block.span,
        });
        work.push(StatementWork::Branch {
            kind: BranchKind::Catch,
            target: labels.handler.clone(),
            span: statement.span,
        });
    }

    fn push_normal_finalizer_path<'statement>(
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        finalizer: &CompilerLabel,
        done: &CompilerLabel,
        span: Span,
    ) {
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: done.clone(),
            span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        )));
        work.push(StatementWork::Branch {
            kind: BranchKind::Gosub,
            target: finalizer.clone(),
            span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            span,
        )));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        )));
    }

    fn plan_catch_binding(
        &self,
        handler: &CatchClause<'arena>,
        catch_body_scope: ScopeId,
        layout: &FrameLayout,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        Ok(match &handler.param {
            None => PlannedInstruction::new(FinalOpcode::Drop, Operands::None, handler.span),
            Some(parameter) => {
                let BindingPattern::BindingIdentifier(identifier) = &parameter.pattern else {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedBinding,
                        parameter.pattern.span(),
                    );
                };
                let binding =
                    self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "catch binding has compiler storage",
                        span: Some(identifier.span),
                    },
                )?;
                if storage.executable() != layout.executable
                    || storage.placement() != StoragePlacement::Local
                    || storage.policy().kind() != DeclarationKind::Catch
                    || storage.policy().initialization() != InitializationPolicy::Catch
                    || storage.policy().writes() != WritePolicy::Mutable
                    || storage.policy().has_temporal_dead_zone()
                {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedBinding,
                        identifier.span,
                    );
                }
                if self.scope_for_binding(binding)? != catch_body_scope {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "catch binding belongs to the catch-body scope",
                        span: Some(identifier.span),
                    });
                }
                let slot = layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedBinding,
                        span: identifier.span,
                    })?;
                plan_put_slot(slot, identifier.span)
            }
        })
    }

    fn plan_for_statement<'statement>(
        &self,
        statement: &'statement ForStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        Self::schedule_for_statement(
            statement,
            scope,
            flow,
            &mut state.work,
            state.active_scopes.len(),
            labels,
        )
    }

    fn plan_for_in_statement<'statement>(
        &self,
        statement: &'statement ForInStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        Self::schedule_for_in_statement(
            statement,
            scope,
            flow,
            &mut state.work,
            state.active_scopes.len(),
            labels,
        )
    }

    fn plan_for_of_statement<'statement>(
        &self,
        statement: &'statement ForOfStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        if statement.r#await {
            return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span);
        }
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        Self::schedule_for_of_statement(
            statement,
            scope,
            flow,
            &mut state.work,
            state.active_scopes.len(),
            labels,
        )
    }

    fn plan_labeled_statement<'statement>(
        &self,
        statement: &'statement LabeledStatement<'arena>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let mut labels = Vec::new();
        labels
            .try_reserve(1)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "labeled statement chain",
            })?;
        labels.push(statement.label.name.as_str());
        let mut body = &statement.body;
        while let Statement::LabeledStatement(nested) = body {
            labels
                .try_reserve(1)
                .map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "labeled statement chain",
                })?;
            labels.push(nested.label.name.as_str());
            body = &nested.body;
        }

        match body {
            Statement::WhileStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_while_statement(
                    statement,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                    labels,
                )
            }
            Statement::DoWhileStatement(statement) => Self::schedule_do_while_statement(
                statement,
                state.completion,
                flow,
                &mut state.work,
                state.active_scopes.len(),
                labels,
            ),
            Statement::ForStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_statement(statement, labels, flow, state)
            }
            Statement::ForInStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_in_statement(statement, labels, flow, state)
            }
            Statement::ForOfStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_of_statement(statement, labels, flow, state)
            }
            Statement::SwitchStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_switch_statement(statement, labels, flow, state)
            }
            body => {
                let done = flow.new_statement_label(statement.span)?;
                let control = ControlRegion::breakable(
                    labels,
                    done.clone(),
                    false,
                    state.active_scopes.len(),
                );
                state.work.push(StatementWork::Bind(done));
                state.work.push(StatementWork::PopControl);
                state.work.push(StatementWork::Visit(body));
                state.work.push(StatementWork::PushControl(control));
                Ok(())
            }
        }
    }

    fn schedule_if_statement<'statement>(
        statement: &'statement IfStatement<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(alternate_statement) = &statement.alternate {
            let alternate = flow.new_statement_label(alternate_statement.span())?;
            let done = flow.new_statement_label(statement.span)?;
            work.push(StatementWork::Bind(done.clone()));
            work.push(StatementWork::Visit(alternate_statement));
            work.push(StatementWork::Bind(alternate.clone()));
            work.push(StatementWork::Branch {
                kind: BranchKind::Goto,
                target: done,
                span: statement.span,
            });
            work.push(StatementWork::Visit(&statement.consequent));
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: alternate,
                span: statement.test.span(),
            });
        } else {
            let done = flow.new_statement_label(statement.span)?;
            work.push(StatementWork::Bind(done.clone()));
            work.push(StatementWork::Visit(&statement.consequent));
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: done,
                span: statement.test.span(),
            });
        }
        work.push(StatementWork::Expression(&statement.test));
        Ok(())
    }

    fn schedule_while_statement<'statement>(
        statement: &'statement WhileStatement<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::iteration(labels, done.clone(), test.clone(), scope_depth);
        work.push(StatementWork::Bind(done.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: test.clone(),
            span: statement.span,
        });
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfFalse,
            target: done,
            span: statement.test.span(),
        });
        work.push(StatementWork::Expression(&statement.test));
        work.push(StatementWork::Bind(test));
        Ok(())
    }

    fn schedule_do_while_statement<'statement>(
        statement: &'statement DoWhileStatement<'arena>,
        completion: StatementCompletion,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let iteration = flow.new_statement_label(statement.body.span())?;
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::iteration(labels, done.clone(), test.clone(), scope_depth);
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfTrue,
            target: iteration.clone(),
            span: statement.test.span(),
        });
        work.push(StatementWork::Expression(&statement.test));
        work.push(StatementWork::Bind(test));
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        if let StatementCompletion::Script(slot) = completion {
            let (opcode, operands) = compact_put_local(slot);
            work.push(StatementWork::Emit(PlannedInstruction::new(
                opcode,
                operands,
                statement.span,
            )));
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                statement.span,
            )));
        }
        work.push(StatementWork::Bind(iteration));
        Ok(())
    }

    fn schedule_for_statement<'statement>(
        statement: &'statement ForStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let test = flow.new_statement_label(
            statement
                .test
                .as_ref()
                .map_or(statement.span, GetSpan::span),
        )?;
        let rotate = flow.new_statement_label(
            statement
                .update
                .as_ref()
                .map_or(statement.span, GetSpan::span),
        )?;
        let done = flow.new_statement_label(statement.span)?;
        let loop_scope_depth =
            enclosing_scope_depth
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "statement scope depth",
                })?;
        let control =
            ControlRegion::iteration(labels, done.clone(), rotate.clone(), loop_scope_depth);

        work.push(StatementWork::PopScope(scope));
        work.push(StatementWork::Bind(done.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: test.clone(),
            span: statement.span,
        });
        if let Some(update) = &statement.update {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                update.span(),
            )));
            work.push(StatementWork::Expression(update));
        }
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Bind(rotate));
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        if let Some(test_expression) = &statement.test {
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: done,
                span: test_expression.span(),
            });
            work.push(StatementWork::Expression(test_expression));
        }
        work.push(StatementWork::Bind(test));
        work.push(StatementWork::CloseScope(scope));
        if let Some(initializer) = &statement.init {
            match initializer {
                ForStatementInit::VariableDeclaration(declaration) => {
                    work.push(StatementWork::Declaration(declaration));
                }
                initializer => {
                    let expression = initializer.to_expression();
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        expression.span(),
                    )));
                    work.push(StatementWork::Expression(expression));
                }
            }
        }
        work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        Ok(())
    }

    fn schedule_for_in_statement<'statement>(
        statement: &'statement ForInStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let next = flow.new_statement_label_with_offset(statement.right.span(), 1)?;
        let assign = flow.new_statement_label_with_offset(statement.left.span(), 2)?;
        let rotate = flow.new_statement_label_with_offset(statement.span, 1)?;
        let cleanup = flow.new_statement_label_with_offset(statement.span, 1)?;
        let done = flow.new_statement_label(statement.span)?;
        let loop_scope_depth =
            enclosing_scope_depth
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "statement scope depth",
                })?;
        let control = ControlRegion::for_in_iteration(
            labels,
            cleanup.clone(),
            rotate.clone(),
            loop_scope_depth,
        );

        work.try_reserve(25)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::PopScope(scope));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Bind(cleanup.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: next.clone(),
            span: statement.span,
        });
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Bind(rotate.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: rotate,
            span: statement.body.span(),
        });
        work.push(StatementWork::PopStatementStackBase {
            span: statement.span,
        });
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        work.push(StatementWork::PushStatementStackBase {
            span: statement.span,
        });
        work.push(StatementWork::ForInAssignment(&statement.left));
        work.push(StatementWork::Bind(assign.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: cleanup,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfFalse,
            target: assign,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForInNext,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Bind(next));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForInStart,
            Operands::None,
            statement.right.span(),
        )));
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Expression(&statement.right));
        work.push(StatementWork::ForInHead(&statement.left));
        work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        Ok(())
    }

    fn schedule_for_of_statement<'statement>(
        statement: &'statement ForOfStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
        labels: Vec<&'statement str>,
    ) -> Result<(), LeafCompilationError> {
        let next = flow.new_statement_label_with_offset(statement.right.span(), 3)?;
        let assign = flow.new_statement_label_with_offset(statement.left.span(), 4)?;
        let rotate = flow.new_statement_label_with_offset(statement.span, 3)?;
        let cleanup = flow.new_statement_label_with_offset(statement.span, 3)?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::for_of_iteration(
            labels,
            cleanup.clone(),
            next.clone(),
            enclosing_scope_depth,
        );

        work.try_reserve(32)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::PopScope(scope));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::IteratorClose,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Bind(cleanup.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: next.clone(),
            span: statement.span,
        });
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Bind(rotate.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: rotate,
            span: statement.body.span(),
        });
        for _ in 0..3 {
            work.push(StatementWork::PopStatementStackBase {
                span: statement.span,
            });
        }
        work.push(StatementWork::PopControl);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushControl(control));
        for _ in 0..3 {
            work.push(StatementWork::PushStatementStackBase {
                span: statement.span,
            });
        }
        work.push(StatementWork::ForOfAssignment(&statement.left));
        work.push(StatementWork::Bind(assign.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: cleanup,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfFalse,
            target: assign,
            span: statement.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForOfNext,
            Operands::U8(0),
            statement.span,
        )));
        work.push(StatementWork::ForOfRotate(scope));
        work.push(StatementWork::Bind(next));
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::ForOfStart,
            Operands::None,
            statement.right.span(),
        )));
        // The RHS closes the initial lexical environment so closures created
        // there keep the TDZ cell rather than the first iteration's cell.
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Expression(&statement.right));
        work.push(StatementWork::ForOfHead(&statement.left));
        work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        Ok(())
    }

    fn plan_switch_statement<'statement>(
        &self,
        statement: &'statement SwitchStatement<'arena>,
        labels: Vec<&'statement str>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        let scope_depth = state.active_scopes.len().checked_add(1).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "statement scope depth",
            },
        )?;
        let done = flow.new_statement_label(statement.span)?;
        let control = ControlRegion::breakable(labels, done.clone(), true, scope_depth);
        let switch_labels = Arc::new(Self::prepare_switch_control_labels(statement, flow)?);

        state
            .work
            .try_reserve(10)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        state.work.push(StatementWork::PopScope(scope));
        state.work.push(StatementWork::Bind(done.clone()));
        state.work.push(StatementWork::PopControl);
        state.work.push(StatementWork::SwitchBody {
            statement,
            labels: Arc::clone(&switch_labels),
            next: 0,
        });
        state.work.push(StatementWork::SwitchNoMatch {
            labels: Arc::clone(&switch_labels),
            done,
            span: statement.span,
        });
        state.work.push(StatementWork::SwitchTrampoline {
            statement,
            labels: Arc::clone(&switch_labels),
            next: 0,
        });
        state.work.push(StatementWork::SwitchDispatch {
            statement,
            labels: switch_labels,
            next: 0,
        });
        state.work.push(StatementWork::PushControl(control));
        state.work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        state
            .work
            .push(StatementWork::Expression(&statement.discriminant));
        Ok(())
    }

    fn prepare_switch_control_labels(
        statement: &SwitchStatement<'arena>,
        flow: &mut PlannedControlFlow,
    ) -> Result<SwitchControlLabels, LeafCompilationError> {
        let mut default_index = None;
        for (index, case) in statement.cases.iter().enumerate() {
            if case.test.is_none() && default_index.replace(index).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "Oxc accepts at most one switch default clause",
                    span: Some(case.span),
                });
            }
        }
        let tested_count = statement
            .cases
            .len()
            .checked_sub(usize::from(default_index.is_some()))
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "switch tested case count",
            })?;
        let scaffold_instructions = switch_scaffold_instruction_count(
            statement.cases.len(),
            tested_count,
            default_index.is_some(),
        )?;
        flow.ensure_additional_instruction_capacity(scaffold_instructions, statement.span)?;

        let mut body = Vec::new();
        body.try_reserve_exact(statement.cases.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "switch body labels",
            }
        })?;
        let mut matched = Vec::new();
        matched
            .try_reserve_exact(statement.cases.len())
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "switch match labels",
            })?;
        for case in &statement.cases {
            body.push(flow.new_statement_label(case.span)?);
            matched.push(flow.new_statement_label_with_offset(case.span, 1)?);
        }
        let no_match = if default_index.is_none() {
            Some(flow.new_statement_label_with_offset(statement.span, 1)?)
        } else {
            None
        };
        let fallback = match default_index {
            Some(index) => {
                matched
                    .get(index)
                    .cloned()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "switch default index names a case",
                        span: Some(statement.span),
                    })?
            }
            None => no_match
                .clone()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "switch without a default has a no-match target",
                    span: Some(statement.span),
                })?,
        };
        Ok(SwitchControlLabels {
            body,
            matched,
            fallback,
            no_match,
        })
    }

    fn schedule_next_switch_dispatch<'statement>(
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        mut next: usize,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        while let Some(case) = statement.cases.get(next) {
            let index = next;
            next = next
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "switch dispatch case index",
                })?;
            let Some(test) = &case.test else {
                continue;
            };
            let target = labels.matched.get(index).cloned().ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a match target",
                    span: Some(case.span),
                },
            )?;
            work.try_reserve(5)
                .map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "statement work stack",
                })?;
            work.push(StatementWork::SwitchDispatch {
                statement,
                labels,
                next,
            });
            work.push(StatementWork::Branch {
                kind: BranchKind::IfTrue,
                target,
                span: test.span(),
            });
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::StrictEq,
                Operands::None,
                test.span(),
            )));
            work.push(StatementWork::Expression(test));
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                test.span(),
            )));
            return Ok(());
        }
        work.try_reserve(1)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: labels.fallback.clone(),
            span: statement.span,
        });
        Ok(())
    }

    fn schedule_next_switch_trampoline<'statement>(
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        next: usize,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let Some(case) = statement.cases.get(next) else {
            return Ok(());
        };
        let match_target =
            labels
                .matched
                .get(next)
                .cloned()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a match trampoline",
                    span: Some(case.span),
                })?;
        let body_target =
            labels
                .body
                .get(next)
                .cloned()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a body target",
                    span: Some(case.span),
                })?;
        let next = next
            .checked_add(1)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "switch trampoline case index",
            })?;
        work.try_reserve(4)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::SwitchTrampoline {
            statement,
            labels,
            next,
        });
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: body_target,
            span: case.span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            case.span,
        )));
        work.push(StatementWork::Bind(match_target));
        Ok(())
    }

    fn schedule_switch_no_match<'statement>(
        labels: &SwitchControlLabels,
        done: CompilerLabel,
        span: Span,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        let Some(no_match) = &labels.no_match else {
            return;
        };
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span,
        });
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            span,
        )));
        work.push(StatementWork::Bind(no_match.clone()));
    }

    fn schedule_next_switch_body<'statement>(
        statement: &'statement SwitchStatement<'arena>,
        labels: Arc<SwitchControlLabels>,
        next: usize,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let Some(case) = statement.cases.get(next) else {
            return Ok(());
        };
        let body_target =
            labels
                .body
                .get(next)
                .cloned()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "every switch case has a body target",
                    span: Some(case.span),
                })?;
        let next = next
            .checked_add(1)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "switch body case index",
            })?;
        work.try_reserve(3)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement work stack",
            })?;
        work.push(StatementWork::SwitchBody {
            statement,
            labels,
            next,
        });
        work.push(StatementWork::VisitList {
            statements: &case.consequent,
            next: 0,
        });
        work.push(StatementWork::Bind(body_target));
        Ok(())
    }

    fn plan_control_jump<'statement>(
        &self,
        label: Option<&'statement LabelIdentifier<'arena>>,
        statement_span: Span,
        jump: LoopJump,
        state: &StatementPlanningState<'statement, 'arena>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let (_, control) = state
            .controls
            .resolve(label.map(|label| label.name.as_str()), jump)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: if label.is_some() {
                    jump.missing_label_invariant()
                } else {
                    jump.missing_region_invariant()
                },
                span: Some(statement_span),
            })?;
        let target = jump
            .target(control)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: jump.invalid_labeled_target_invariant(),
                span: Some(statement_span),
            })?;
        if control.scope_depth > state.active_scopes.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: jump.scope_invariant(),
                span: Some(statement_span),
            });
        }
        let abrupt_marker_depth =
            control
                .abrupt_marker_depth
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "active statement control has an abrupt-marker depth",
                    span: Some(statement_span),
                })?;
        let crossed_markers = state.abrupt_markers.get(abrupt_marker_depth..).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "statement control abrupt-marker depth is active",
                span: Some(statement_span),
            },
        )?;
        let open_scope_depth = self.plan_crossed_abrupt_marker_exits(
            crossed_markers,
            &state.active_scopes,
            statement_span,
            layout,
            flow,
        )?;
        if control.scope_depth > open_scope_depth {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: jump.scope_invariant(),
                span: Some(statement_span),
            });
        }
        for scope in state.active_scopes[control.scope_depth..open_scope_depth]
            .iter()
            .rev()
        {
            self.plan_scope_exit(layout.executable, *scope, layout, flow)?;
        }
        flow.branch(BranchKind::Goto, target, statement_span)
    }

    fn plan_crossed_abrupt_marker_exits(
        &self,
        crossed_markers: &[AbruptMarker],
        active_scopes: &[ScopeId],
        statement_span: Span,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<usize, LeafCompilationError> {
        let mut open_scope_depth = active_scopes.len();
        for marker in crossed_markers.iter().rev() {
            if marker.scope_depth > open_scope_depth {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "abrupt marker scope depth remains active",
                    span: Some(statement_span),
                });
            }
            for scope in active_scopes[marker.scope_depth..open_scope_depth]
                .iter()
                .rev()
            {
                self.plan_scope_exit(layout.executable, *scope, layout, flow)?;
            }
            open_scope_depth = marker.scope_depth;
            Self::emit_abrupt_marker_cleanup(&marker.kind, statement_span, flow)?;
        }
        Ok(open_scope_depth)
    }

    fn emit_abrupt_marker_cleanup(
        marker: &AbruptMarkerKind,
        span: Span,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        match marker {
            AbruptMarkerKind::Catch { finalizer: None } | AbruptMarkerKind::ForIn => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    span,
                ))?;
            }
            AbruptMarkerKind::Catch {
                finalizer: Some(finalizer),
            } => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Undefined,
                    Operands::None,
                    span,
                ))?;
                flow.branch(BranchKind::Gosub, finalizer, span)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    span,
                ))?;
            }
            AbruptMarkerKind::ForOf => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::IteratorClose,
                    Operands::None,
                    span,
                ))?;
            }
            AbruptMarkerKind::FinallySubroutine => {
                for _ in 0..2 {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        span,
                    ))?;
                }
            }
        }
        Ok(())
    }

    fn created_scope(
        &self,
        scope: Option<ScopeId>,
        creator: NodeId,
        span: Span,
    ) -> Result<ScopeId, LeafCompilationError> {
        let scope = scope.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "Oxc scope creator has a semantic scope identity",
            span: Some(span),
        })?;
        let scoping = self.unit.semantic().scoping();
        if scope.index() >= scoping.scopes_len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope identity indexes retained semantics",
                span: Some(span),
            });
        }
        if scoping.get_node_id(scope) != creator {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope identity names its creator node",
                span: Some(span),
            });
        }
        Ok(scope)
    }

    #[allow(clippy::too_many_lines)]
    fn plan_scope_entry(
        &self,
        scope: ScopeId,
        creator: NodeId,
        span: Span,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        if scoping.get_node_id(scope) != creator {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope entry names its creator node",
                span: Some(span),
            });
        }
        let executable = planning.executable;
        let function_creator = self
            .planned
            .identities
            .node_by_executable
            .get(executable.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "scope-entry executable has an Oxc node identity",
                span: Some(span),
            })?;
        let function_scope = creator == function_creator;
        let mut entries = self.scope_entry_initializations(
            executable,
            scope,
            planning.layout,
            planning.tree_layout,
        )?;
        entries.sort_unstable_by_key(ScopeEntryInitialization::order_key);
        if function_scope {
            self.emit_parameter_binding_activations(executable, planning.layout, flow)?;
            self.emit_arguments_object_initializer(executable, planning.layout, flow)?;
            self.emit_parameter_pattern_initializers(executable, planning, flow)?;
            self.emit_parameter_body_binding_copies(executable, planning.layout, flow)?;
            self.emit_realm_global_function_initializers(
                executable,
                planning.tree_layout,
                planning.constants,
                flow,
            )?;
            for entry in entries
                .iter()
                .rev()
                .copied()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Uninitialized { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
            Self::emit_scoped_function_activations(&entries, flow)?;
            for entry in entries
                .iter()
                .copied()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Function { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
        } else {
            Self::emit_scoped_function_activations(&entries, flow)?;
            for entry in entries
                .iter()
                .rev()
                .copied()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Uninitialized { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
            for entry in entries
                .into_iter()
                .rev()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Function { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
        }
        Ok(())
    }

    fn emit_parameter_binding_activations(
        &self,
        executable: ExecutableId,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if !metadata.has_parameter_expressions() {
            return Ok(());
        }
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let mut parameters = bindings
            .iter()
            .filter(|binding| {
                binding.policy().kind() == DeclarationKind::Parameter
                    && binding.policy().initialization() == InitializationPolicy::Argument
                    && binding.policy().has_temporal_dead_zone()
            })
            .map(|binding| {
                let span = binding.declaration_spans().first().copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "parameter-expression binding has a source anchor",
                        span: Some(metadata.span()),
                    },
                )?;
                let FrameSlot::Local(slot) =
                    layout
                        .slot(binding.id())
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "parameter-expression binding has a local slot",
                            span: Some(span),
                        })?
                else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "parameter-expression binding uses local storage",
                        span: Some(span),
                    });
                };
                Ok((slot, span))
            })
            .collect::<Result<Vec<_>, _>>()?;
        parameters.sort_unstable_by_key(|(slot, _)| slot.index());
        for (slot, span) in parameters {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(slot.index()),
                span,
            ))?;
        }
        Ok(())
    }

    fn emit_scoped_function_activations(
        entries: &[ScopeEntryInitialization],
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        for entry in entries.iter().rev().copied() {
            let ScopeEntryInitialization::Function {
                slot,
                span,
                scoped: true,
                ..
            } = entry
            else {
                continue;
            };
            let FrameSlot::Local(slot) = slot else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scoped function declaration uses a local slot",
                    span: Some(span),
                });
            };
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(slot.index()),
                span,
            ))?;
        }
        Ok(())
    }

    fn emit_arguments_object_initializer(
        &self,
        executable: ExecutableId,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let mut arguments = bindings
            .iter()
            .filter(|binding| binding.is_arguments_object());
        let Some(binding) = arguments.next() else {
            return Ok(());
        };
        if arguments.next().is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "one arguments-object binding per function",
                span: binding.declaration_spans().first().copied(),
            });
        }
        let span = binding.declaration_spans().first().copied().ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "arguments-object binding has a source anchor",
                span: None,
            },
        )?;
        let slot = layout
            .slot(binding.id())
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "arguments-object binding has a frame slot",
                span: Some(span),
            })?;
        if !matches!(slot, FrameSlot::Local(_)) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "arguments-object binding is function-local",
                span: Some(span),
            });
        }
        let executable = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let arguments_kind =
            u8::from(!executable.is_strict() && executable.has_simple_parameter_list());
        flow.emit(PlannedInstruction::new(
            FinalOpcode::SpecialObject,
            Operands::U8(arguments_kind),
            span,
        ))?;
        flow.emit(plan_put_slot(slot, span))
    }

    fn emit_parameter_pattern_initializers(
        &self,
        executable: ExecutableId,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if metadata.has_simple_parameter_list() {
            return Ok(());
        }
        let node = self
            .planned
            .identities
            .node_by_executable
            .get(executable.index())
            .copied()
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let function = match self.unit.semantic().nodes().kind(node) {
            AstKind::Function(function) => function,
            AstKind::Program(_) => return Ok(()),
            _ => {
                return unsupported(UnsupportedLeafFeature::NonOrdinaryFunction, metadata.span());
            }
        };
        for (index, parameter) in function.params.items.iter().enumerate() {
            if !metadata.has_parameter_expressions()
                && matches!(parameter.pattern, BindingPattern::BindingIdentifier(_))
            {
                continue;
            }
            let slot = ArgumentSlot(checked_function_index(
                index,
                "function parameter initialization slots",
            )?);
            let (opcode, operands) = compact_get_argument(slot);
            flow.emit(PlannedInstruction::new(opcode, operands, parameter.span))?;
            if let Some(initializer) = &parameter.initializer {
                self.emit_parameter_default_initializer(
                    &parameter.pattern,
                    initializer,
                    parameter.span,
                    planning,
                    flow,
                )?;
            }
            self.plan_destructuring_pattern_value(
                &parameter.pattern,
                DestructuringBindingInitialization::Parameter,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?;
        }
        if let Some(rest) = &function.params.rest {
            let first_argument = u16::try_from(function.params.items.len()).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "formal rest first argument",
                }
            })?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Rest,
                Operands::U16(first_argument),
                rest.span,
            ))?;
            self.plan_destructuring_pattern_value(
                &rest.rest.argument,
                DestructuringBindingInitialization::Parameter,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?;
        }
        Ok(())
    }

    fn emit_parameter_default_initializer(
        &self,
        pattern: &BindingPattern<'arena>,
        initializer: &Expression<'arena>,
        parameter_span: Span,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let skip = flow.new_label(parameter_span)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            parameter_span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            parameter_span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::StrictEq,
            Operands::None,
            parameter_span,
        ))?;
        flow.branch(BranchKind::IfFalse, &skip, parameter_span)?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            parameter_span,
        ))?;
        let inferred_name = match pattern {
            BindingPattern::BindingIdentifier(identifier) => self
                .plan_inferred_function_name_for_initializer(
                    identifier,
                    initializer,
                    planning.constants,
                )?,
            BindingPattern::AssignmentPattern(_)
            | BindingPattern::ArrayPattern(_)
            | BindingPattern::ObjectPattern(_) => None,
        };
        self.plan_expression(
            initializer,
            planning.layout,
            planning.tree_layout,
            planning.constants,
            flow,
        )?;
        if let Some(set_name) = inferred_name {
            flow.emit(set_name)?;
        }
        flow.bind(&skip)
    }

    fn emit_parameter_body_binding_copies(
        &self,
        executable: ExecutableId,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if !metadata.has_parameter_expressions() {
            return Ok(());
        }
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        for destination in bindings.iter().filter(|binding| {
            binding.policy().kind() == DeclarationKind::Var && !binding.is_arguments_object()
        }) {
            let source = bindings.iter().find(|candidate| {
                candidate.name() == destination.name()
                    && (candidate.policy().kind() == DeclarationKind::Parameter
                        || candidate.is_arguments_object())
            });
            let Some(source) = source else {
                continue;
            };
            let span = destination.declaration_spans().first().copied().ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "body variable copy has a declaration span",
                    span: Some(metadata.span()),
                },
            )?;
            let source_slot =
                layout
                    .slot(source.id())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "parameter-environment copy source has a frame slot",
                        span: Some(span),
                    })?;
            let destination_slot =
                layout
                    .slot(destination.id())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "body variable copy destination has a frame slot",
                        span: Some(span),
                    })?;
            flow.emit(ExpressionPlanner::new(self).plan_read_slot(
                source.id(),
                source_slot,
                span,
            )?)?;
            flow.emit(plan_put_slot(destination_slot, span))?;
        }
        Ok(())
    }

    fn scope_entry_initializations(
        &self,
        executable: ExecutableId,
        scope: ScopeId,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Vec<ScopeEntryInitialization>, LeafCompilationError> {
        let mut entries = Vec::new();
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        for storage in bindings {
            if self.scope_for_binding(storage.id())? != scope {
                continue;
            }
            let binding = storage.id();
            let declaration_span = storage.declaration_spans().first().copied().ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-entry compiler binding has a declaration span",
                    span: None,
                },
            )?;
            if Self::realm_global_scope_entry_is_runtime_instantiated(storage, declaration_span)? {
                continue;
            }
            let frame_slot =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "scope-entry binding has a frame slot",
                        span: Some(declaration_span),
                    })?;
            match storage.policy().initialization() {
                InitializationPolicy::AtDeclaration
                    if storage.policy().has_temporal_dead_zone() =>
                {
                    let FrameSlot::Local(slot) = frame_slot else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "scope-entry lexical binding uses a local slot",
                            span: Some(declaration_span),
                        });
                    };
                    entries.push(ScopeEntryInitialization::Uninitialized {
                        slot,
                        span: declaration_span,
                    });
                }
                InitializationPolicy::FunctionAtInstantiation
                | InitializationPolicy::FunctionAtScopeEntry => {
                    if storage.policy().kind() != DeclarationKind::Function
                        || storage.policy().has_temporal_dead_zone()
                        || matches!(frame_slot, FrameSlot::Capture(_))
                    {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "scope-entry function declaration has writable frame storage",
                            span: Some(declaration_span),
                        });
                    }
                    let child = tree_layout.function_declaration(binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "scope-entry function binding has a declaration executable",
                            span: Some(declaration_span),
                        },
                    )?;
                    let child_span = self
                        .planned
                        .plan
                        .executable(child)
                        .map_or(declaration_span, Executable::span);
                    entries.push(ScopeEntryInitialization::Function {
                        slot: frame_slot,
                        child,
                        span: child_span,
                        scoped: storage.policy().initialization()
                            == InitializationPolicy::FunctionAtScopeEntry,
                    });
                }
                InitializationPolicy::AtDeclaration => {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedDeclaration,
                        declaration_span,
                    );
                }
                InitializationPolicy::Argument
                | InitializationPolicy::UndefinedAtInstantiation
                | InitializationPolicy::FunctionName
                | InitializationPolicy::Catch
                | InitializationPolicy::ModuleImport
                | InitializationPolicy::ModuleNamespace => {}
            }
        }
        Ok(entries)
    }

    fn realm_global_scope_entry_is_runtime_instantiated(
        storage: &crate::storage::BindingStorage,
        span: Span,
    ) -> Result<bool, LeafCompilationError> {
        if storage.placement() != StoragePlacement::GlobalObject {
            return Ok(false);
        }
        let supported_policy = matches!(
            (storage.policy().kind(), storage.policy().initialization()),
            (
                DeclarationKind::Var,
                InitializationPolicy::UndefinedAtInstantiation
            ) | (
                DeclarationKind::Function,
                InitializationPolicy::FunctionAtInstantiation
            )
        );
        if !supported_policy
            || storage.policy().writes() != WritePolicy::Mutable
            || storage.policy().has_temporal_dead_zone()
        {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, span);
        }
        Ok(true)
    }

    fn emit_realm_global_function_initializers(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            || executable.index() != 0
        {
            return Ok(());
        }
        let root = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if root.parent().is_some()
            || !matches!(
                root.kind(),
                ExecutableKind::Script {
                    asynchronous: false
                }
            )
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm function initializers belong to the dynamic Script root",
                span: Some(root.span()),
            });
        }

        for &global in tree_layout.realm_globals.imports_for(executable)? {
            let descriptor = tree_layout.realm_globals.binding(global).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "root realm-global initializer has a binding descriptor",
                    span: Some(root.span()),
                },
            )?;
            if descriptor.policy.kind() != VerifiedBindingKind::Function {
                continue;
            }
            let binding =
                descriptor
                    .declaration
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "constructor-realm function initializer has a declared binding",
                        span: Some(descriptor.first_span),
                    })?;
            let child = tree_layout.function_declaration(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "constructor-realm function initializer selects its last child",
                    span: Some(descriptor.first_span),
                },
            )?;
            let child_span = self
                .planned
                .plan
                .executable(child)
                .map_or(descriptor.first_span, Executable::span);
            flow.emit(ExpressionPlanner::new(self).plan_child_function_closure(
                child,
                executable,
                child_span,
                tree_layout,
                constants,
            )?)?;
            let slot =
                tree_layout
                    .realm_globals
                    .closure_slot(&self.planned.plan, executable, global)?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::PutVar,
                Operands::VarRef(slot),
                descriptor.first_span,
            ))?;
        }
        Ok(())
    }

    fn emit_scope_entry_initialization(
        &self,
        executable: ExecutableId,
        entry: ScopeEntryInitialization,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        match entry {
            ScopeEntryInitialization::Uninitialized { slot, span } => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::SetLocUninitialized,
                    Operands::Loc(slot.index()),
                    span,
                ))?;
            }
            ScopeEntryInitialization::Function {
                slot, child, span, ..
            } => {
                flow.emit(ExpressionPlanner::new(self).plan_child_function_closure(
                    child,
                    executable,
                    span,
                    tree_layout,
                    constants,
                )?)?;
                flow.emit(plan_put_slot(slot, span))?;
            }
        }
        Ok(())
    }

    fn plan_scope_exit(
        &self,
        executable: ExecutableId,
        scope: ScopeId,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let mut captured_locals = Vec::new();
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        for storage in bindings {
            if self.scope_for_binding(storage.id())? != scope {
                continue;
            }
            let binding = storage.id();
            let declaration_span = storage.declaration_spans().first().copied().ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-exit compiler binding has a declaration span",
                    span: None,
                },
            )?;
            if !storage.is_frame_captured() {
                continue;
            }
            let FrameSlot::Local(slot) =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "captured scope-exit binding has a frame slot",
                        span: Some(declaration_span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "captured scope-exit binding uses a local slot",
                    span: Some(declaration_span),
                });
            };
            captured_locals.push((slot, declaration_span));
        }
        captured_locals.sort_unstable_by_key(|(slot, _)| slot.index());
        for (slot, declaration_span) in captured_locals.into_iter().rev() {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::CloseLoc,
                Operands::Loc(slot.index()),
                declaration_span,
            ))?;
        }
        Ok(())
    }

    /// Re-arms the for-of loop scope's non-captured TDZ cells at the back
    /// edge. Each iteration writes the head bindings (identifier or
    /// destructuring) as fresh initializations; captured cells rotate through
    /// `close_loc`, and the non-captured cells return to the uninitialized
    /// state exactly like the captured rotation, so every iteration's write
    /// is a valid initialization.
    fn plan_for_of_rotation(
        &self,
        executable: ExecutableId,
        scope: ScopeId,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        let mut rotated_locals = Vec::new();
        for symbol in scoping.iter_bindings_in(scope) {
            if scoping.symbol_scope_id(symbol) != scope {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "for-of rotation exact-scope binding belongs to that scope",
                    span: Some(scoping.symbol_span(symbol)),
                });
            }
            let declaration_span = scoping.symbol_span(symbol);
            let binding = self.binding_for_identifier(Some(symbol), declaration_span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "for-of rotation compiler binding exists",
                    span: Some(declaration_span),
                },
            )?;
            if storage.executable() != executable {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "for-of rotation binding belongs to the selected executable",
                    span: Some(declaration_span),
                });
            }
            if storage.is_frame_captured() || !storage.policy().has_temporal_dead_zone() {
                continue;
            }
            let FrameSlot::Local(slot) =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "for-of rotation TDZ binding has a frame slot",
                        span: Some(declaration_span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "for-of rotation TDZ binding uses a local slot",
                    span: Some(declaration_span),
                });
            };
            rotated_locals.push((slot, declaration_span));
        }
        rotated_locals.sort_unstable_by_key(|(slot, _)| slot.index());
        for (slot, declaration_span) in rotated_locals {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::SetLocUninitialized,
                Operands::Loc(slot.index()),
                declaration_span,
            ))?;
        }
        Ok(())
    }

    fn validate_function_declaration(
        &self,
        function: &Function<'arena>,
        parent: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        active_scope: Option<ScopeId>,
    ) -> Result<(), LeafCompilationError> {
        if function.r#type != FunctionType::FunctionDeclaration {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function-declaration statement has declaration function type",
                span: Some(function.span),
            });
        }
        let identifier = function
            .id
            .as_ref()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Script function declaration has a binding identifier",
                span: Some(function.span),
            })?;
        let binding = self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "function declaration binding has compiler storage",
                    span: Some(identifier.span),
                })?;
        if storage.executable() != parent
            || storage.policy().kind() != DeclarationKind::Function
            || !matches!(
                storage.policy().initialization(),
                InitializationPolicy::FunctionAtInstantiation
                    | InitializationPolicy::FunctionAtScopeEntry
            )
        {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                identifier.span,
            );
        }
        let binding_scope = self.scope_for_binding(binding)?;
        if active_scope != Some(binding_scope) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function declaration executes in its binding scope",
                span: Some(identifier.span),
            });
        }
        let child = ExpressionPlanner::new(self).executable_for_function(function)?;
        let child_metadata = self
            .planned
            .plan
            .executable(child)
            .ok_or(LeafCompilationError::InvalidExecutable { executable: child })?;
        if child_metadata.parent() != Some(parent)
            || tree_layout.children(parent)?.binary_search(&child).is_err()
            || child_metadata.name_span() != Some(identifier.span)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function declaration has one typed direct-child constant",
                span: Some(function.span),
            });
        }
        Ok(())
    }

    fn plan_for_in_head(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let ForStatementLeft::VariableDeclaration(declaration) = left else {
            if left.as_assignment_target().is_none() {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, left.span());
            }
            return Ok(());
        };
        let (identifier, initializer) = self.validate_for_in_declaration(declaration, layout)?;
        let Some(initializer) = initializer else {
            return Ok(());
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
        self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
        self.emit_for_in_declaration_write(declaration.kind, identifier, layout, tree_layout, flow)
    }

    fn plan_for_of_head(
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
            self.validate_realm_global_var_declaration(declaration.kind, storage, identifier.span)?;
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

    fn plan_for_in_assignment(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let ForStatementLeft::VariableDeclaration(declaration) = left {
            let (identifier, _) = self.validate_for_in_declaration(declaration, layout)?;
            return self.emit_for_in_declaration_write(
                declaration.kind,
                identifier,
                layout,
                tree_layout,
                flow,
            );
        }

        let target =
            left.as_assignment_target()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "for-in non-declaration head is an assignment target",
                    span: Some(left.span()),
                })?;
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                let reference = self.lowered_reference(
                    identifier.reference_id.get(),
                    identifier.span,
                    layout,
                    tree_layout,
                )?;
                self.validate_lowered_mutation_reference(reference, false, identifier.span)?;
                match reference {
                    LoweredReference::Frame { binding, slot, .. } => {
                        for instruction in ExpressionPlanner::new(self).plan_write_slot(
                            binding,
                            slot,
                            false,
                            identifier.span,
                        )? {
                            flow.emit(instruction)?;
                        }
                    }
                    LoweredReference::RealmGlobal { slot, .. } => {
                        flow.emit(PlannedInstruction::new(
                            FinalOpcode::PutVar,
                            Operands::VarRef(slot),
                            identifier.span,
                        ))?;
                    }
                }
            }
            AssignmentTarget::StaticMemberExpression(member) if !member.optional => {
                self.plan_expression(&member.object, layout, tree_layout, constants, flow)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Swap,
                    Operands::None,
                    member.span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::PutField,
                    Operands::Atom(constants.property_atom_index(member.property.span)?),
                    member.span,
                ))?;
            }
            AssignmentTarget::ComputedMemberExpression(member) if !member.optional => {
                self.plan_expression(&member.object, layout, tree_layout, constants, flow)?;
                self.plan_expression(&member.expression, layout, tree_layout, constants, flow)?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Rot3l,
                    Operands::None,
                    member.span,
                ))?;
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::PutArrayEl,
                    Operands::None,
                    member.span,
                ))?;
            }
            _ => {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, target.span());
            }
        }
        Ok(())
    }

    /// Stores the per-iteration for-of value into the loop head. Identifier
    /// and member heads share the for-in path; destructuring heads run the
    /// declaration or assignment pattern machinery directly on the value
    /// already on the stack (the loop's `for_of_next` step pushed it above
    /// the verified record, whose offset stays zero).
    fn plan_for_of_assignment(
        &self,
        left: &ForStatementLeft<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let ForStatementLeft::VariableDeclaration(declaration) = left {
            let pattern = Self::validate_for_of_declaration(declaration)?;
            if matches!(pattern, BindingPattern::BindingIdentifier(_)) {
                return self.plan_for_in_assignment(left, layout, tree_layout, constants, flow);
            }
            return self.plan_destructuring_pattern_value(
                pattern,
                DestructuringBindingInitialization::Declaration(declaration.kind),
                layout,
                tree_layout,
                constants,
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
            | AssignmentTarget::ObjectAssignmentTarget(_) => {
                let mut work = Vec::new();
                self.plan_assignment_target_value(
                    target,
                    &mut work,
                    flow,
                    layout,
                    tree_layout,
                    constants,
                )?;
                while let Some(task) = work.pop() {
                    match task {
                        ExpressionWork::Emit(instruction) => flow.emit(instruction)?,
                        ExpressionWork::Branch { kind, target, span } => {
                            flow.branch(kind, &target, span)?;
                        }
                        ExpressionWork::Bind(label) => flow.bind(&label)?,
                        ExpressionWork::Visit(expression) => {
                            self.plan_expression(expression, layout, tree_layout, constants, flow)?;
                        }
                    }
                }
                Ok(())
            }
            AssignmentTarget::AssignmentTargetIdentifier(_)
            | AssignmentTarget::StaticMemberExpression(_)
            | AssignmentTarget::ComputedMemberExpression(_) => {
                self.plan_for_in_assignment(left, layout, tree_layout, constants, flow)
            }
            AssignmentTarget::TSAsExpression(_)
            | AssignmentTarget::TSSatisfiesExpression(_)
            | AssignmentTarget::TSNonNullExpression(_)
            | AssignmentTarget::TSTypeAssertion(_)
            | AssignmentTarget::PrivateFieldExpression(_) => {
                unsupported(UnsupportedLeafFeature::UnsupportedExpression, target.span())
            }
        }
    }

    fn validate_for_in_declaration<'declaration>(
        &self,
        declaration: &'declaration VariableDeclaration<'arena>,
        layout: &FrameLayout,
    ) -> Result<
        (
            &'declaration BindingIdentifier<'arena>,
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
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                declarator.id.span(),
            );
        };
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
            self.validate_realm_global_var_declaration(declaration.kind, storage, identifier.span)?;
        } else {
            let slot = layout
                .slot(binding)
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                    span: identifier.span,
                })?;
            self.validate_declaration_storage(declaration.kind, binding, slot, identifier.span)?;
        }
        Ok((identifier, declarator.init.as_ref()))
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
            self.validate_realm_global_var_declaration(declaration_kind, storage, identifier.span)?;
            let global = tree_layout.realm_globals.for_binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "for-in Program var has a constructor-realm global identity",
                    span: Some(identifier.span),
                },
            )?;
            let slot = tree_layout.realm_globals.closure_slot(
                &self.planned.plan,
                layout.executable,
                global,
            )?;
            return flow.emit(PlannedInstruction::new(
                FinalOpcode::PutVar,
                Operands::VarRef(slot),
                identifier.span,
            ));
        }

        let slot = layout
            .slot(binding)
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBinding,
                span: identifier.span,
            })?;
        flow.emit(plan_put_slot(slot, identifier.span))
    }

    fn validate_declaration(
        &self,
        declaration: &VariableDeclaration<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
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
                        flow,
                    )?;
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
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
            if storage.placement() == StoragePlacement::GlobalObject {
                self.validate_realm_global_var_declaration(
                    declaration_kind,
                    storage,
                    identifier.span,
                )?;
                if let Some(initializer) = &declarator.init {
                    let set_name = self.plan_inferred_function_name_for_initializer(
                        identifier,
                        initializer,
                        constants,
                    )?;
                    self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
                    if let Some(set_name) = set_name {
                        flow.emit(set_name)?;
                    }
                    let global = tree_layout.realm_globals.for_binding(binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "declared Program var has a constructor-realm global identity",
                            span: Some(identifier.span),
                        },
                    )?;
                    let slot = tree_layout.realm_globals.closure_slot(
                        &self.planned.plan,
                        layout.executable,
                        global,
                    )?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::PutVar,
                        Operands::VarRef(slot),
                        identifier.span,
                    ))?;
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
                    self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
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
        reason = "array-pattern declaration planning carries the same explicit frame, tree, constant, and flow authority as every other declaration form"
    )]
    fn plan_array_destructuring_declaration<'pattern, 'expression>(
        &self,
        pattern: &'pattern ArrayPattern<'arena>,
        initializer: &'expression Expression<'arena>,
        declaration_kind: VariableDeclarationKind,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
        self.plan_array_destructuring_value(
            pattern,
            DestructuringBindingInitialization::Declaration(declaration_kind),
            layout,
            tree_layout,
            constants,
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
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        self.plan_array_destructuring_elements(
            pattern.elements.iter(),
            pattern.rest.as_deref(),
            binding_initialization,
            layout,
            tree_layout,
            constants,
            flow,
            pattern.span,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "object-pattern declaration planning carries the same explicit frame, tree, constant, and flow authority as every other declaration form"
    )]
    fn plan_object_destructuring_declaration<'pattern, 'expression>(
        &self,
        pattern: &'pattern ObjectPattern<'arena>,
        initializer: &'expression Expression<'arena>,
        declaration_kind: VariableDeclarationKind,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
        self.plan_object_destructuring_value(
            pattern,
            DestructuringBindingInitialization::Declaration(declaration_kind),
            layout,
            tree_layout,
            constants,
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
                self.plan_expression(key, layout, tree_layout, constants, flow)?;
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
        flow: &mut PlannedControlFlow,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ForOfStart,
            Operands::None,
            span,
        ))?;
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
                flow,
            )?;
        }
        flow.emit(PlannedInstruction::new(
            FinalOpcode::IteratorClose,
            Operands::None,
            span,
        ))
    }

    fn plan_destructuring_rest<'pattern>(
        &self,
        rest: &'pattern BindingRestElement<'arena>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
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
            flow,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "destructuring pattern value storage carries the same explicit frame, tree, constant, and flow authority as every other declaration form"
    )]
    fn plan_destructuring_pattern_value<'pattern>(
        &self,
        pattern: &'pattern BindingPattern<'arena>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        match pattern {
            BindingPattern::BindingIdentifier(identifier) => self
                .plan_destructuring_binding_identifier(
                    identifier,
                    binding_initialization,
                    layout,
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
                self.plan_expression(&assignment.right, layout, tree_layout, constants, flow)?;
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
                    flow,
                )
            }
            BindingPattern::ArrayPattern(pattern) => self.plan_array_destructuring_value(
                pattern,
                binding_initialization,
                layout,
                tree_layout,
                constants,
                flow,
            ),
            BindingPattern::ObjectPattern(pattern) => self.plan_object_destructuring_value(
                pattern,
                binding_initialization,
                layout,
                tree_layout,
                constants,
                flow,
            ),
        }
    }

    fn plan_destructuring_binding_identifier(
        &self,
        identifier: &BindingIdentifier<'arena>,
        binding_initialization: DestructuringBindingInitialization,
        layout: &FrameLayout,
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
        if storage.placement() == StoragePlacement::GlobalObject {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                identifier.span,
            );
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

    fn plan_inferred_function_name_for_initializer(
        &self,
        identifier: &BindingIdentifier<'arena>,
        initializer: &Expression<'arena>,
        constants: &CompiledConstantPool,
    ) -> Result<Option<PlannedInstruction>, LeafCompilationError> {
        let Some(span) = anonymous_named_evaluation_span(initializer) else {
            return Ok(None);
        };
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
    fn plan_array_assignment_elements<'pattern>(
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

    fn plan_assignment_target_value<'pattern>(
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
    fn plan_object_assignment_value<'pattern>(
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

    fn validate_realm_global_var_declaration(
        &self,
        declaration_kind: VariableDeclarationKind,
        storage: &crate::storage::BindingStorage,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let merged_global_policy = matches!(
            (storage.policy().kind(), storage.policy().initialization()),
            (
                DeclarationKind::Var,
                InitializationPolicy::UndefinedAtInstantiation
            ) | (
                DeclarationKind::Function,
                InitializationPolicy::FunctionAtInstantiation
            )
        );
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            || declaration_kind != VariableDeclarationKind::Var
            || !merged_global_policy
            || storage.policy().writes() != WritePolicy::Mutable
            || storage.policy().has_temporal_dead_zone()
        {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, span);
        }
        Ok(())
    }

    fn validate_declaration_storage(
        &self,
        declaration_kind: VariableDeclarationKind,
        binding: BindingId,
        frame_slot: FrameSlot,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "declared compiler binding exists",
                    span: Some(span),
                })?;
        let valid = match declaration_kind {
            VariableDeclarationKind::Let => {
                matches!(storage.policy().kind(), DeclarationKind::Let)
                    && storage.policy().has_temporal_dead_zone()
                    && matches!(frame_slot, FrameSlot::Local(_))
            }
            VariableDeclarationKind::Const => {
                matches!(storage.policy().kind(), DeclarationKind::Const)
                    && storage.policy().has_temporal_dead_zone()
                    && matches!(frame_slot, FrameSlot::Local(_))
            }
            VariableDeclarationKind::Var => {
                matches!(
                    storage.policy().kind(),
                    DeclarationKind::Var | DeclarationKind::Parameter | DeclarationKind::Function
                ) && !storage.policy().has_temporal_dead_zone()
            }
            VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing => false,
        };
        if !valid {
            return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
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

    fn selected_ordinary_function(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Function<'arena>, OrdinaryFunctionForm), LeafCompilationError> {
        let executable = self.planned.plan.executable(executable_id).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: executable_id,
            },
        )?;
        let node_id = self
            .planned
            .identities
            .node_by_executable
            .get(executable_id.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "executable has an Oxc node identity",
                span: Some(executable.span()),
            })?;
        if self
            .planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            != Some(executable_id)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc node and executable identities are bijective",
                span: Some(executable.span()),
            });
        }

        let AstKind::Function(function) = self.unit.semantic().nodes().kind(node_id) else {
            return unsupported(
                UnsupportedLeafFeature::NonOrdinaryFunction,
                executable.span(),
            );
        };
        if function.r#async || function.generator {
            return unsupported(UnsupportedLeafFeature::NonOrdinaryFunction, function.span);
        }
        let is_declaration = function.r#type == FunctionType::FunctionDeclaration;
        let is_expression = function.r#type == FunctionType::FunctionExpression;
        if !is_declaration && !is_expression {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedFunctionForm,
                function.span,
            );
        }
        let form = object_method_or_accessor_span(self.unit, node_id)
            .map_or(OrdinaryFunctionForm::Function, |property_span| {
                OrdinaryFunctionForm::ObjectMethod { property_span }
            });
        if !matches!(
            executable.kind(),
            ExecutableKind::Function {
                asynchronous: false,
                generator: false,
            }
        ) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary Oxc function has ordinary executable metadata",
                span: Some(function.span),
            });
        }
        if self.planned.plan.kind() != CompilationUnitKind::Script {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                function.span,
            );
        }
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            && let Some(reference) = self
                .planned
                .plan
                .unresolved_globals_for(executable_id)
                .and_then(|references| references.first())
        {
            return unsupported(
                UnsupportedLeafFeature::UnresolvedReference,
                reference.span(),
            );
        }
        Ok((executable, function, form))
    }

    fn selected_dynamic_function_script(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Program<'arena>), LeafCompilationError> {
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) {
            return unsupported(
                UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                self.unit.program().span,
            );
        }
        let executable = self.planned.plan.executable(executable_id).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: executable_id,
            },
        )?;
        let node_id = self
            .planned
            .identities
            .node_by_executable
            .get(executable_id.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Script executable has an Oxc node identity",
                span: Some(executable.span()),
            })?;
        if self
            .planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            != Some(executable_id)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Script Oxc node and executable identities are bijective",
                span: Some(executable.span()),
            });
        }
        let AstKind::Program(program) = self.unit.semantic().nodes().kind(node_id) else {
            return unsupported(
                UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                executable.span(),
            );
        };
        if executable_id.index() != 0
            || executable.parent().is_some()
            || executable.parameter_count() != 0
            || executable.is_strict()
            || !matches!(
                executable.kind(),
                ExecutableKind::Script {
                    asynchronous: false
                }
            )
            || self.planned.plan.kind() != CompilationUnitKind::Script
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary dynamic Function has one synchronous sloppy Script root",
                span: Some(program.span),
            });
        }
        Ok((executable, program))
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

fn object_method_or_accessor_span(unit: &ParsedUnit<'_, '_>, node_id: NodeId) -> Option<Span> {
    let AstKind::ObjectProperty(property) = unit.semantic().nodes().parent_kind(node_id) else {
        return None;
    };
    let Expression::FunctionExpression(value) = &property.value else {
        return None;
    };
    (value.node_id.get() == node_id
        && (property.method || !matches!(property.kind, PropertyKind::Init)))
    .then_some(property.span)
}

const fn executable_header(
    kind: CompilerExecutableKind,
    strict: bool,
    simple_parameter_list: bool,
    defined_argument_count: u32,
    variable_reference_count: u32,
) -> UnverifiedFunctionHeader {
    let header = match kind {
        CompilerExecutableKind::OrdinaryFunction => {
            UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(
                strict,
                defined_argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::OrdinaryMethod => {
            UnverifiedFunctionHeader::ordinary_method_with_variable_references(
                strict,
                defined_argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            UnverifiedFunctionHeader::dynamic_function_script(variable_reference_count)
        }
    };
    header.with_simple_parameter_list(simple_parameter_list)
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

struct ValidatedFunction {
    executable_kind: CompilerExecutableKind,
    strict: bool,
    argument_count: u32,
    defined_argument_count: u32,
    local_count: u32,
    capture_count: u32,
    capture_layout: CompilerCaptureLayout,
    locals: Vec<LoweredLocal>,
    atoms: Arc<[CompilerAtom]>,
    constants: Arc<[CompiledConstant]>,
    closure_variables: Vec<CompiledClosureVariable>,
    realm_globals: Vec<CompiledRealmGlobal>,
    function_name: Option<AtomPoolIndex>,
    variable_definitions: Vec<VariableDefinition>,
    closure_definitions: Vec<VerifiedClosureVariableDefinition>,
    function_span: SourceByteSpan,
    function_name_span: Option<SourceByteSpan>,
    flow: PlannedControlFlow,
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
