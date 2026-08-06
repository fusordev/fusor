use std::sync::Arc;

use oxc_ast::ast::{ArrowFunctionExpression, Function, FunctionBody, Program, Statement};
use oxc_span::GetSpan;
use quickjs_bytecode::{
    AtomPoolIndex, ClosureVariableDefinition as VerifiedClosureVariableDefinition, CompilerAtom,
    CompilerBindingKind as VerifiedBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerConstantLayout, CompilerExecutableKind,
    CompilerInitializationPolicy as VerifiedInitializationPolicy, CompilerSource,
    CompilerWritePolicy as VerifiedWritePolicy, FinalOpcode, FunctionIndexDomains, Operands,
    PcSourceSpan, ScopeLink, SourceByteSpan, UnverifiedFunctionHeader, UnverifiedFunctionMetadata,
    VariableDefinition, VerificationLimits,
};
use quickjs_frontend::Span;

use super::{
    AstKind, ClassElement, CompilationContext, CompiledClosureVariable, CompiledConstant,
    CompiledConstantPool, CompiledFunction, CompiledMetadataAtomKey, CompiledRealmGlobal,
    ExpressionPlanner, FrameLayout, FrameLayoutInput, FunctionTreeLayout, LeafCompilationError,
    LocalSlot, LoweredLocal, MethodDefinitionKind, NodeId, OrdinaryFunctionForm,
    PlannedControlFlow, PlannedInstruction, StatementCompletion, StatementControlStack,
    StatementPlanningState, StatementWork, UnsupportedLeafFeature, checked_function_entry_count,
    compiled_static_property_key, unsupported,
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

fn synthesized_class_constructor_flow(
    derived: bool,
    span: Span,
    limits: VerificationLimits,
) -> Result<PlannedControlFlow, LeafCompilationError> {
    let mut flow = PlannedControlFlow::new(limits);
    if derived {
        // The synthesized `constructor(...args) { super(...args); }`
        // delegates argument forwarding and deferred receiver setup to the
        // typed derived-constructor opcodes. `init_ctor` leaves the superclass
        // completion on the stack just as QuickJS does; the synthetic body
        // immediately discards it.
        flow.emit(PlannedInstruction::new(
            FinalOpcode::CheckCtor,
            Operands::None,
            span,
        ))?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::InitCtor,
            Operands::None,
            span,
        ))?;
    }
    Ok(flow)
}

/// Each public field in the admitted noncomputed class subset is initialized
/// in the constructor frame, preserving per-instance evaluation order.
pub(in crate::lowering) struct InstanceFieldDefinitions {
    pub(in crate::lowering) derived: bool,
    pub(in crate::lowering) fields: Vec<NodeId>,
}

impl CompilationContext<'_, '_, '_> {
    pub(in crate::lowering) fn instance_field_definitions(
        &self,
        executable: ExecutableId,
    ) -> Result<Option<InstanceFieldDefinitions>, LeafCompilationError> {
        let node_id = self
            .planned
            .identities
            .node_by_executable
            .get(executable.index())
            .copied()
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let nodes = self.unit.semantic().nodes();
        let class = match nodes.kind(node_id) {
            AstKind::Function(function) => {
                let AstKind::MethodDefinition(method) = nodes.parent_kind(function.node_id.get())
                else {
                    return Ok(None);
                };
                if method.kind != MethodDefinitionKind::Constructor
                    || method.value.node_id.get() != node_id
                {
                    return Ok(None);
                }
                let AstKind::ClassBody(body) = nodes.parent_kind(method.node_id.get()) else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "class constructor belongs to a class body",
                        span: Some(method.span),
                    });
                };
                let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "class constructor class body belongs to a class",
                        span: Some(body.span),
                    });
                };
                class
            }
            AstKind::Class(class)
                if self
                    .planned
                    .identities
                    .default_class_constructors
                    .get(&node_id)
                    .copied()
                    == Some(executable) =>
            {
                class
            }
            _ => return Ok(None),
        };
        let mut fields = Vec::new();
        for element in &class.body.body {
            let ClassElement::PropertyDefinition(field) = element else {
                continue;
            };
            if field.r#static {
                continue;
            }
            if !field.decorators.is_empty() {
                return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, field.span);
            }
            if field.computed {
                field
                    .key
                    .as_expression()
                    .ok_or(LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                        span: field.key.span(),
                    })?;
            } else {
                compiled_static_property_key(&field.key)?.ok_or(
                    LeafCompilationError::Unsupported {
                        feature: UnsupportedLeafFeature::UnsupportedDeclaration,
                        span: field.key.span(),
                    },
                )?;
            }
            fields.push(field.node_id.get());
        }
        Ok(Some(InstanceFieldDefinitions {
            derived: class.super_class.is_some(),
            fields,
        }))
    }
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
        let instance_fields = compiler.instance_field_definitions(planning.executable)?;
        let mut work = vec![
            StatementWork::PopScope(function_scope),
            StatementWork::VisitList {
                statements: &body.statements,
                next: 0,
            },
        ];
        if function.generator {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::InitialYield,
                Operands::None,
                function.span,
            )));
        }
        if let Some(instance_fields) = instance_fields
            && !instance_fields.derived
            && !instance_fields.fields.is_empty()
        {
            work.push(StatementWork::InitializeInstanceFields);
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

    pub(in crate::lowering) fn for_arrow(
        compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
        arrow: &'statement ArrowFunctionExpression<'arena>,
        planning: FunctionPlanningContext<'layout>,
        limits: VerificationLimits,
    ) -> Result<Self, LeafCompilationError> {
        planning.validate_owner()?;
        let function_scope =
            compiler.created_scope(arrow.scope_id.get(), arrow.node_id.get(), arrow.span)?;
        let flow = PlannedControlFlow::new(limits);
        let mut work = if arrow.expression {
            let [Statement::ExpressionStatement(expression)] = arrow.body.statements.as_slice()
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "concise arrow body has one expression statement",
                    span: Some(arrow.body.span),
                });
            };
            if !arrow.body.directives.is_empty() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "concise arrow body has no directives",
                    span: Some(arrow.body.span),
                });
            }
            vec![
                StatementWork::PopScope(function_scope),
                StatementWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Return,
                    Operands::None,
                    arrow.body.span,
                )),
                StatementWork::Expression(&expression.expression),
            ]
        } else {
            vec![
                StatementWork::PopScope(function_scope),
                StatementWork::VisitList {
                    statements: &arrow.body.statements,
                    next: 0,
                },
            ]
        };
        work.push(StatementWork::PushScope {
            scope: function_scope,
            creator: arrow.node_id.get(),
            span: arrow.span,
        });
        Ok(Self {
            compiler,
            planning,
            body_span: arrow.body.span,
            state: StatementPlanningState {
                work,
                active_scopes: Vec::new(),
                controls: StatementControlStack::default(),
                abrupt_markers: Vec::new(),
                completion: StatementCompletion::Discard,
            },
            flow,
            terminal: FunctionTerminal::Ordinary,
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
            ExecutableKind::Arrow {
                asynchronous: false,
            } => self.validate_arrow(executable, tree_layout, limits),
            ExecutableKind::ClassDefaultConstructor => {
                self.validate_default_class_constructor(executable, tree_layout, limits)
            }
            _ => self.validate_function(executable, tree_layout, limits),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "default constructor validation keeps its synthesized-frame invariants together"
    )]
    fn validate_default_class_constructor(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, class) = self.selected_default_class_constructor(executable_id)?;
        let layout = FrameLayout::new(FrameLayoutInput::new(&self.planned.plan, executable_id))?;
        if !layout.locals.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "synthesized default constructor has no local storage",
                span: Some(class.span),
            });
        }
        for child in tree_layout.children(executable_id)? {
            if !self.is_direct_instance_field_initializer_child(executable_id, *child)? {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "synthesized default constructor owns only direct instance-field initializer templates",
                    span: Some(class.span),
                });
            }
        }
        let constants = tree_layout.constant_pool(executable_id)?;
        let function_scope =
            self.created_scope(Some(class.scope_id()), class.node_id(), class.span)?;
        let capture_layout =
            self.compiler_capture_layout(executable_id, function_scope, &layout, tree_layout)?;
        let closure_variables = self.compiled_closure_variables(executable_id, tree_layout)?;
        let realm_globals = self.compiled_realm_globals(executable_id, tree_layout, constants)?;
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
                    domain: "default class constructor closure variables",
                })?,
            "default class constructor closure variables",
        )?;
        let derived_class_constructor = class.super_class.is_some();
        let instance_fields = self.instance_field_definitions(executable_id)?.ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "synthesized constructor owns its class fields",
                span: Some(class.span),
            },
        )?;
        if instance_fields.derived != derived_class_constructor {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "synthesized constructor field plan retains derived state",
                span: Some(class.span),
            });
        }
        let mut flow =
            synthesized_class_constructor_flow(derived_class_constructor, class.span, limits)?;
        if !instance_fields.fields.is_empty() {
            ExpressionPlanner::new(self).plan_instance_field_initializations(
                executable_id,
                &layout,
                tree_layout,
                constants,
                &mut flow,
            )?;
        }
        if derived_class_constructor {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                class.span,
            ))?;
        }
        flow.ensure_terminal(class.span)?;

        Ok(ValidatedFunction {
            executable_kind: CompilerExecutableKind::ClassConstructor,
            strict: executable.is_strict(),
            derived_class_constructor,
            argument_count: 0,
            defined_argument_count: 0,
            local_count: layout.local_count,
            capture_count,
            capture_layout,
            locals: Vec::new(),
            constants: Arc::clone(constants.entries()),
            atoms: Arc::clone(constants.atoms()),
            closure_variables,
            realm_globals,
            function_name: None,
            variable_definitions,
            closure_definitions,
            function_span: source_byte_span(class.span),
            function_name_span: None,
            flow,
        })
    }

    fn is_direct_instance_field_initializer_child(
        &self,
        parent: ExecutableId,
        child: ExecutableId,
    ) -> Result<bool, LeafCompilationError> {
        let child_node = self
            .planned
            .identities
            .node_by_executable
            .get(child.index())
            .copied()
            .ok_or(LeafCompilationError::InvalidExecutable { executable: child })?;
        let fields = self.instance_field_definitions(parent)?.ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "synthesized default constructor has instance-field definitions",
                span: None,
            },
        )?;
        let nodes = self.unit.semantic().nodes();
        for ancestor in nodes.ancestor_ids(child_node) {
            match nodes.kind(ancestor) {
                AstKind::Function(_) | AstKind::ArrowFunctionExpression(_) => return Ok(false),
                AstKind::PropertyDefinition(_) => return Ok(fields.fields.contains(&ancestor)),
                _ => {}
            }
        }
        Ok(false)
    }

    fn validate_arrow(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, arrow) = self.selected_arrow(executable_id)?;
        let layout = FrameLayout::new(FrameLayoutInput::new(&self.planned.plan, executable_id))?;
        let constants = tree_layout.constant_pool(executable_id)?;
        let planning = FunctionPlanningContext {
            executable: executable_id,
            layout: &layout,
            tree_layout,
            constants,
        };
        let flow = FunctionLoweringSession::for_arrow(self, arrow, planning, limits)?.lower()?;
        let function_scope =
            self.created_scope(arrow.scope_id.get(), arrow.node_id.get(), arrow.span)?;
        let capture_layout =
            self.compiler_capture_layout(executable_id, function_scope, &layout, tree_layout)?;
        let closure_variables = self.compiled_closure_variables(executable_id, tree_layout)?;
        let realm_globals = self.compiled_realm_globals(executable_id, tree_layout, constants)?;
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
                    domain: "arrow closure variables",
                })?,
            "arrow closure variables",
        )?;

        Ok(ValidatedFunction {
            executable_kind: CompilerExecutableKind::OrdinaryArrow,
            strict: executable.is_strict(),
            derived_class_constructor: false,
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
            function_name: None,
            variable_definitions,
            closure_definitions,
            function_span: source_byte_span(arrow.span),
            function_name_span: None,
            flow,
        })
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
        let (
            executable_kind,
            function_span,
            function_name,
            function_name_span,
            derived_class_constructor,
        ) = match form {
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
                false,
            ),
            OrdinaryFunctionForm::ObjectMethod {
                property_span: source_span,
            }
            | OrdinaryFunctionForm::ClassMethod {
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
                false,
            ),
            OrdinaryFunctionForm::ClassConstructor {
                class_span,
                derived,
            } => {
                if generator || asynchronous {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedFunctionForm,
                        class_span,
                    );
                }
                (
                    CompilerExecutableKind::ClassConstructor,
                    class_span,
                    None,
                    None,
                    derived,
                )
            }
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
            derived_class_constructor,
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
            derived_class_constructor: false,
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
    derived_class_constructor: bool,
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
    derived_class_constructor: bool,
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
        CompilerExecutableKind::OrdinaryArrow => {
            UnverifiedFunctionHeader::ordinary_arrow_with_variable_references(
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
        CompilerExecutableKind::ClassConstructor => {
            if derived_class_constructor {
                UnverifiedFunctionHeader::derived_class_constructor_with_variable_references(
                    strict,
                    defined_argument_count,
                    variable_reference_count,
                )
            } else {
                UnverifiedFunctionHeader::class_constructor_with_variable_references(
                    strict,
                    defined_argument_count,
                    variable_reference_count,
                )
            }
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
            derived_class_constructor,
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
            derived_class_constructor,
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
