use std::sync::Arc;

use oxc_ast::ast::{Function, FunctionBody, Program};
use quickjs_bytecode::{
    AtomPoolIndex, ClosureVariableDefinition as VerifiedClosureVariableDefinition, CompilerAtom,
    CompilerBindingKind as VerifiedBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerConstantLayout, CompilerExecutableKind,
    CompilerInitializationPolicy as VerifiedInitializationPolicy, CompilerSource,
    CompilerWritePolicy as VerifiedWritePolicy, FunctionIndexDomains, PcSourceSpan, ScopeLink,
    SourceByteSpan, UnverifiedFunctionHeader, UnverifiedFunctionMetadata, VariableDefinition,
    VerificationLimits,
};
use quickjs_frontend::Span;

use super::{
    CompilationContext, CompiledClosureVariable, CompiledConstant, CompiledConstantPool,
    CompiledFunction, CompiledMetadataAtomKey, CompiledRealmGlobal, FrameLayout, FrameLayoutInput,
    FunctionTreeLayout, LeafCompilationError, LocalSlot, LoweredLocal, OrdinaryFunctionForm,
    PlannedControlFlow, StatementCompletion, StatementControlStack, StatementPlanningState,
    StatementWork, UnsupportedLeafFeature, checked_function_entry_count,
};
use crate::storage::{ExecutableId, ExecutableKind};

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

const fn source_byte_span(span: Span) -> SourceByteSpan {
    SourceByteSpan::new(span.start, span.end)
}

#[derive(Clone, Copy)]
pub(in crate::lowering) struct FunctionPlanningContext<'layout> {
    pub(in crate::lowering) executable: ExecutableId,
    pub(in crate::lowering) layout: &'layout FrameLayout,
    pub(in crate::lowering) tree_layout: &'layout FunctionTreeLayout,
    pub(in crate::lowering) constants: &'layout CompiledConstantPool,
}

impl FunctionPlanningContext<'_> {
    fn validate_owner(self) -> Result<(), LeafCompilationError> {
        if self.layout.executable != self.executable {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function lowering session owns exactly one executable frame",
                span: None,
            });
        }
        Ok(())
    }
}

enum FunctionTerminal {
    Ordinary,
    Generator,
    Async,
    Script(LocalSlot),
}

/// Mutable lowering state for exactly one selected executable.
pub(in crate::lowering) struct FunctionLoweringSession<
    'compiler,
    'statement,
    'unit,
    'arena,
    'scope,
    'layout,
> {
    compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
    planning: FunctionPlanningContext<'layout>,
    body_span: Span,
    state: StatementPlanningState<'statement, 'arena>,
    flow: PlannedControlFlow,
    terminal: FunctionTerminal,
}

impl<'compiler, 'statement, 'unit, 'arena, 'scope, 'layout>
    FunctionLoweringSession<'compiler, 'statement, 'unit, 'arena, 'scope, 'layout>
{
    pub(in crate::lowering) fn for_function(
        compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
        function: &'statement Function<'arena>,
        body: &'statement FunctionBody<'arena>,
        planning: FunctionPlanningContext<'layout>,
        limits: VerificationLimits,
    ) -> Result<Self, LeafCompilationError> {
        planning.validate_owner()?;
        let function_scope = compiler.created_scope(
            function.scope_id.get(),
            function.node_id.get(),
            function.span,
        )?;
        let flow = PlannedControlFlow::new(limits);
        let terminal = if function.generator {
            FunctionTerminal::Generator
        } else if function.r#async {
            FunctionTerminal::Async
        } else {
            FunctionTerminal::Ordinary
        };
        let mut work = vec![
            StatementWork::PopScope(function_scope),
            StatementWork::VisitList {
                statements: &body.statements,
                next: 0,
            },
        ];
        if function.generator {
            work.push(StatementWork::Emit(super::PlannedInstruction::new(
                quickjs_bytecode::FinalOpcode::InitialYield,
                quickjs_bytecode::Operands::None,
                function.span,
            )));
        }
        work.push(StatementWork::PushScope {
            scope: function_scope,
            creator: function.node_id.get(),
            span: function.span,
        });
        Ok(Self {
            compiler,
            planning,
            body_span: body.span,
            state: StatementPlanningState {
                work,
                active_scopes: Vec::new(),
                controls: StatementControlStack::default(),
                abrupt_markers: Vec::new(),
                completion: StatementCompletion::Discard,
            },
            flow,
            terminal,
        })
    }

    pub(in crate::lowering) fn for_program(
        compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
        program: &'statement Program<'arena>,
        completion: LocalSlot,
        planning: FunctionPlanningContext<'layout>,
        limits: VerificationLimits,
    ) -> Result<Self, LeafCompilationError> {
        planning.validate_owner()?;
        let program_scope =
            compiler.created_scope(program.scope_id.get(), program.node_id.get(), program.span)?;
        Ok(Self {
            compiler,
            planning,
            body_span: program.span,
            state: StatementPlanningState {
                work: vec![
                    StatementWork::PopScope(program_scope),
                    StatementWork::VisitList {
                        statements: &program.body,
                        next: 0,
                    },
                    StatementWork::PushScope {
                        scope: program_scope,
                        creator: program.node_id.get(),
                        span: program.span,
                    },
                ],
                active_scopes: Vec::new(),
                controls: StatementControlStack::default(),
                abrupt_markers: Vec::new(),
                completion: StatementCompletion::Script(completion),
            },
            flow: PlannedControlFlow::new(limits),
            terminal: FunctionTerminal::Script(completion),
        })
    }

    pub(in crate::lowering) fn lower(mut self) -> Result<PlannedControlFlow, LeafCompilationError> {
        while let Some(task) = self.state.work.pop() {
            self.compiler.process_statement_work(
                task,
                self.body_span,
                &self.planning,
                &mut self.flow,
                &mut self.state,
            )?;
        }
        if !self.state.active_scopes.is_empty()
            || !self.state.controls.is_empty()
            || !self.state.abrupt_markers.is_empty()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: match self.terminal {
                    FunctionTerminal::Ordinary => {
                        "statement planning closes every scope and control region"
                    }
                    FunctionTerminal::Generator => {
                        "generator planning closes every scope and control region"
                    }
                    FunctionTerminal::Async => {
                        "async-function planning closes every scope and control region"
                    }
                    FunctionTerminal::Script(_) => {
                        "Program planning closes every scope and control region"
                    }
                },
                span: Some(self.body_span),
            });
        }
        match self.terminal {
            FunctionTerminal::Ordinary => self.flow.ensure_terminal(self.body_span)?,
            FunctionTerminal::Generator | FunctionTerminal::Async => {
                self.flow.ensure_generator_terminal(self.body_span)?;
            }
            FunctionTerminal::Script(completion) => self
                .flow
                .ensure_script_terminal(completion, self.body_span)?,
        }
        Ok(self.flow)
    }
}

impl CompilationContext<'_, '_, '_> {
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
            } => self.validate_script(executable, tree_layout, limits),
            _ => self.validate_function(executable, tree_layout, limits),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "function form, metadata, storage, and verified flow are assembled at one audited boundary"
    )]
    fn validate_function(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, function, form, generator, asynchronous) =
            self.selected_function(executable_id)?;
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
                if generator && asynchronous {
                    CompilerExecutableKind::AsyncGeneratorFunction
                } else if generator {
                    CompilerExecutableKind::GeneratorFunction
                } else if asynchronous {
                    CompilerExecutableKind::AsyncFunction
                } else {
                    CompilerExecutableKind::OrdinaryFunction
                },
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
                if generator && asynchronous {
                    CompilerExecutableKind::AsyncGeneratorMethod
                } else if generator {
                    CompilerExecutableKind::GeneratorMethod
                } else if asynchronous {
                    CompilerExecutableKind::AsyncMethod
                } else {
                    CompilerExecutableKind::OrdinaryMethod
                },
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

    fn validate_script(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, program, executable_kind) =
            if crate::is_supported_global_script_goal(self.unit.goal()) {
                let (executable, program) = self.selected_global_script(executable_id)?;
                (executable, program, CompilerExecutableKind::GlobalScript)
            } else {
                let (executable, program) = self.selected_dynamic_function_script(executable_id)?;
                (
                    executable,
                    program,
                    CompilerExecutableKind::DynamicFunctionScript,
                )
            };
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
            executable_kind,
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

const fn executable_header(
    kind: CompilerExecutableKind,
    strict: bool,
    simple_parameter_list: bool,
    defined_argument_count: u32,
    variable_reference_count: u32,
) -> UnverifiedFunctionHeader {
    let header = match kind {
        CompilerExecutableKind::GlobalScript => {
            UnverifiedFunctionHeader::global_script(strict, variable_reference_count)
        }
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
        CompilerExecutableKind::GeneratorFunction => {
            UnverifiedFunctionHeader::generator_source_function_with_variable_references(
                strict,
                defined_argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::GeneratorMethod => {
            UnverifiedFunctionHeader::generator_method_with_variable_references(
                strict,
                defined_argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::AsyncFunction => {
            UnverifiedFunctionHeader::async_source_function_with_variable_references(
                strict,
                defined_argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::AsyncMethod => {
            UnverifiedFunctionHeader::async_method_with_variable_references(
                strict,
                defined_argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::AsyncGeneratorFunction => {
            UnverifiedFunctionHeader::async_generator_source_function_with_variable_references(
                strict,
                defined_argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::AsyncGeneratorMethod => {
            UnverifiedFunctionHeader::async_generator_method_with_variable_references(
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

impl CompilationContext<'_, '_, '_> {
    pub(in crate::lowering) fn compile_function(
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
}

#[cfg(test)]
mod tests {
    use quickjs_frontend::{
        CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
    };

    use crate::lowering::{
        CompilationContext, FrameLayout, FrameLayoutInput, FunctionPlanningContext,
        LeafCompilationError,
    };

    #[test]
    fn planning_context_rejects_a_frame_from_another_executable() {
        with_parsed_program(
            "function outer(){ function child(){} }",
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let plan = context.storage_plan();
                let executable = |name| {
                    plan.executables()
                        .iter()
                        .find(|executable| executable.name() == Some(name))
                        .expect("named executable")
                        .id()
                };
                let outer = executable("outer");
                let child = executable("child");
                let layout = FrameLayout::new(FrameLayoutInput::new(plan, outer))
                    .expect("outer frame layout");
                let tree_layout = context
                    .function_tree_layout()
                    .expect("function tree layout");
                let planning = FunctionPlanningContext {
                    executable: child,
                    layout: &layout,
                    tree_layout: &tree_layout,
                    constants: tree_layout.constant_pool(child).expect("child constants"),
                };

                assert!(matches!(
                    planning.validate_owner(),
                    Err(LeafCompilationError::SemanticInvariant {
                        invariant: "function lowering session owns exactly one executable frame",
                        span: None,
                    })
                ));
            },
        )
        .expect("front-end acceptance");
    }
}
