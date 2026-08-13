use oxc_semantic::ScopeId;

use super::super::{
    ArgumentSlot, AstKind, BindingId, BindingPattern, BranchKind, CompilationContext,
    CompiledConstantPool, DeclarationKind, DestructuringBindingInitialization, Executable,
    ExecutableId, ExecutableKind, Expression, ExpressionPlanner, FinalOpcode, FrameLayout,
    FrameSlot, Function, FunctionPlanningContext, FunctionTreeLayout, FunctionType,
    InitializationPolicy, LeafCompilationError, NodeId, Operands, PlannedControlFlow,
    PlannedInstruction, ScopeEntryInitialization, Span, StoragePlacement, UnsupportedLeafFeature,
    WritePolicy, checked_function_index, compact_get_argument, compact_get_local,
    plan_external_put, plan_put_slot, unsupported,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::lowering) enum LogicalCompilerScope {
    Function,
    Body,
    Oxc(ScopeId),
}

impl<'arena> CompilationContext<'_, 'arena, '_> {
    pub(in crate::lowering) fn created_scope(
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
    pub(in crate::lowering) fn plan_scope_entry(
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
            let has_instantiation_function = entries.iter().any(|entry| {
                matches!(
                    entry,
                    ScopeEntryInitialization::Function { scoped: false, .. }
                )
            });
            self.emit_parameter_binding_activations(executable, planning.layout, flow)?;
            self.emit_arguments_object_initializer(executable, planning.layout, flow)?;
            self.emit_parameter_pattern_initializers(executable, planning, flow)?;
            self.emit_parameter_body_binding_copies(executable, planning.layout, flow)?;
            let executable_metadata = self
                .planned
                .plan
                .executable(executable)
                .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
            if executable_metadata.has_parameter_expressions()
                && executable_metadata.has_direct_eval()
            {
                flow.mark_parameter_initialization_end(span)?;
            }
            if has_instantiation_function {
                flow.mark_function_initializer_prefix_start(span)?;
            }
            self.emit_realm_global_function_initializers(
                executable,
                planning.tree_layout,
                planning.constants,
                flow,
            )?;
            self.emit_module_function_initializers(
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
        let parameters = match self.unit.semantic().nodes().kind(node) {
            AstKind::Function(function) => function.params.as_ref(),
            AstKind::ArrowFunctionExpression(arrow) => arrow.params.as_ref(),
            AstKind::Program(_) => return Ok(()),
            _ => {
                return unsupported(UnsupportedLeafFeature::NonOrdinaryFunction, metadata.span());
            }
        };
        for (index, parameter) in parameters.items.iter().enumerate() {
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
                &[],
                flow,
            )?;
        }
        if let Some(rest) = &parameters.rest {
            let first_argument = u16::try_from(parameters.items.len()).map_err(|_| {
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
                &[],
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

    #[allow(clippy::too_many_lines)]
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
            if matches!(
                storage.placement(),
                StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport
            ) {
                // Module bindings are linked by the runtime, not frame-scoped.
                continue;
            }
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
            if self
                .planned
                .identities
                .annex_b_functions
                .values()
                .any(|function| function.synthetic_block && function.lexical == binding)
            {
                continue;
            }
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
        let supported = match storage.placement() {
            StoragePlacement::GlobalObject => {
                matches!(
                    (storage.policy().kind(), storage.policy().initialization()),
                    (
                        DeclarationKind::Var,
                        InitializationPolicy::UndefinedAtInstantiation
                    ) | (
                        DeclarationKind::Function,
                        InitializationPolicy::FunctionAtInstantiation
                    )
                ) && storage.policy().writes() == WritePolicy::Mutable
                    && !storage.policy().has_temporal_dead_zone()
            }
            StoragePlacement::GlobalLexical => {
                matches!(
                    (storage.policy().kind(), storage.policy().initialization()),
                    (
                        DeclarationKind::Let | DeclarationKind::Const | DeclarationKind::Class,
                        InitializationPolicy::AtDeclaration
                    )
                ) && storage.policy().has_temporal_dead_zone()
                    && matches!(
                        (storage.policy().kind(), storage.policy().writes()),
                        (
                            DeclarationKind::Let | DeclarationKind::Class,
                            WritePolicy::Mutable
                        ) | (DeclarationKind::Const, WritePolicy::Immutable)
                    )
            }
            StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport => {
                // Module bindings are materialized and linked by the runtime
                // linker; they never receive scope-entry frame initialization.
                return Ok(true);
            }
            StoragePlacement::Argument { .. } | StoragePlacement::Local => return Ok(false),
        };
        if !supported {
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
        if !crate::is_supported_realm_global_binding_goal(self.unit.goal())
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
                invariant: "realm-global function initializers belong to the Script root",
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
            let Some(binding) = descriptor.declaration else {
                continue;
            };
            let declaration = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "external function initializer has a declared binding",
                    span: Some(descriptor.first_span),
                },
            )?;
            if declaration.policy().kind() != DeclarationKind::Function {
                continue;
            }
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
            let opcode = match descriptor.binding {
                quickjs_bytecode::CompilerClosureBinding::Captured(_) => FinalOpcode::PutVarRef,
                quickjs_bytecode::CompilerClosureBinding::RealmGlobal(_) => FinalOpcode::PutVar,
            };
            flow.emit(PlannedInstruction::new(
                opcode,
                Operands::VarRef(slot),
                descriptor.first_span,
            ))?;
        }
        Ok(())
    }

    fn emit_module_function_initializers(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if !crate::is_supported_module_goal(self.unit.goal()) || executable.index() != 0 {
            return Ok(());
        }
        let root = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if root.parent().is_some() || !matches!(root.kind(), ExecutableKind::Module) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "module function initializers belong to the Module root",
                span: Some(root.span()),
            });
        }
        let realm_global_count = tree_layout.realm_globals.imports_for(executable)?.len();
        for &id in tree_layout.module_bindings.imports_for(executable)? {
            let descriptor = tree_layout.module_bindings.binding(id).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "root module initializer has a binding descriptor",
                    span: Some(root.span()),
                },
            )?;
            if descriptor.origin != quickjs_bytecode::ModuleBindingOrigin::Local
                || descriptor.policy.kind() != quickjs_bytecode::CompilerBindingKind::Function
            {
                continue;
            }
            let declaration = descriptor.declaration.ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "module function initializer retains its declared binding identity",
                    span: Some(descriptor.first_span),
                },
            )?;
            let child = tree_layout.function_declaration(declaration).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "module function declaration selects its initializer",
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
            let slot = tree_layout.module_bindings.closure_slot(
                &self.planned.plan,
                executable,
                id,
                realm_global_count,
            )?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::PutVarRef,
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

    pub(in crate::lowering) fn plan_scope_exit(
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
            if matches!(
                storage.placement(),
                StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport
            ) {
                // Module bindings are linked by the runtime, not frame-scoped.
                continue;
            }
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

    /// Re-arms a for-in/of loop scope's TDZ cells at the back edge. Each
    /// iteration writes the head bindings (identifier or
    /// destructuring) as fresh initializations. Captured cells first detach
    /// through `close_loc`; re-arming every TDZ local here then makes the new
    /// direct binding uninitialized before the next head write, while the
    /// detached cell retains the preceding iteration's value.
    pub(in crate::lowering) fn plan_iteration_rotation(
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
                    invariant: "iteration rotation exact-scope binding belongs to that scope",
                    span: Some(scoping.symbol_span(symbol)),
                });
            }
            let declaration_span = scoping.symbol_span(symbol);
            let binding = self.binding_for_identifier(Some(symbol), declaration_span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "iteration rotation compiler binding exists",
                    span: Some(declaration_span),
                },
            )?;
            if storage.executable() != executable {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "iteration rotation binding belongs to the selected executable",
                    span: Some(declaration_span),
                });
            }
            if !storage.policy().has_temporal_dead_zone() {
                continue;
            }
            let FrameSlot::Local(slot) =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "iteration rotation TDZ binding has a frame slot",
                        span: Some(declaration_span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "iteration rotation TDZ binding uses a local slot",
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

    #[allow(
        clippy::too_many_lines,
        reason = "ordinary, Annex B block, and synthetic-if function declarations share one closure-publication boundary"
    )]
    pub(in crate::lowering) fn plan_function_declaration(
        &self,
        function: &Function<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        active_scope: Option<ScopeId>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let Some(annex_b) = self
            .planned
            .identities
            .annex_b_functions
            .get(&function.node_id.get())
            .copied()
        else {
            return self.validate_function_declaration(
                function,
                layout.executable,
                tree_layout,
                active_scope,
            );
        };
        if !annex_b.synthetic_block {
            self.validate_function_declaration(
                function,
                layout.executable,
                tree_layout,
                active_scope,
            )?;
            return self.plan_annex_b_function_copy(
                annex_b.lexical,
                annex_b.variable,
                function.span,
                layout,
                tree_layout,
                flow,
            );
        }

        let identifier = function
            .id
            .as_ref()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Annex B function declaration has a binding identifier",
                span: Some(function.span),
            })?;
        let binding = self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "synthetic Annex B lexical binding exists",
                    span: Some(identifier.span),
                })?;
        if binding != annex_b.lexical
            || storage.executable() != layout.executable
            || storage.placement() != StoragePlacement::Local
            || storage.policy().kind() != DeclarationKind::Let
            || storage.policy().initialization() != InitializationPolicy::AtDeclaration
            || storage.policy().writes() != WritePolicy::Mutable
            || !storage.policy().has_temporal_dead_zone()
            || active_scope != Some(annex_b.lexical_scope)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "synthetic Annex B function uses one scoped lexical cell",
                span: Some(identifier.span),
            });
        }
        let FrameSlot::Local(slot) =
            layout
                .slot(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "synthetic Annex B lexical binding has a local slot",
                    span: Some(identifier.span),
                })?
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "synthetic Annex B lexical binding uses local storage",
                span: Some(identifier.span),
            });
        };
        let child = ExpressionPlanner::new(self).executable_for_function(function)?;
        self.validate_function_declaration_child(
            function,
            identifier.span,
            layout.executable,
            child,
            tree_layout,
        )?;
        flow.emit(PlannedInstruction::new(
            FinalOpcode::SetLocUninitialized,
            Operands::Loc(slot.index()),
            identifier.span,
        ))?;
        flow.emit(ExpressionPlanner::new(self).plan_child_function_closure(
            child,
            layout.executable,
            function.span,
            tree_layout,
            constants,
        )?)?;
        flow.emit(plan_put_slot(FrameSlot::Local(slot), identifier.span))?;
        self.plan_annex_b_function_copy(
            binding,
            annex_b.variable,
            identifier.span,
            layout,
            tree_layout,
            flow,
        )?;
        if storage.is_frame_captured() {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::CloseLoc,
                Operands::Loc(slot.index()),
                identifier.span,
            ))?;
        }
        Ok(())
    }

    fn plan_annex_b_function_copy(
        &self,
        lexical: BindingId,
        variable: Option<BindingId>,
        span: Span,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let Some(variable) = variable else {
            return Ok(());
        };
        let FrameSlot::Local(lexical_slot) =
            layout
                .slot(lexical)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "Annex B lexical function has an owner-frame slot",
                    span: Some(span),
                })?
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Annex B lexical function uses owner-local storage",
                span: Some(span),
            });
        };
        let (opcode, operands) = compact_get_local(lexical_slot);
        flow.emit(PlannedInstruction::new(opcode, operands, span))?;
        if let Some(slot) = layout.slot(variable) {
            return flow.emit(plan_put_slot(slot, span));
        }
        let global = tree_layout.realm_globals.for_binding(variable).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "Annex B variable target has frame or realm-global storage",
                span: Some(span),
            },
        )?;
        let slot = tree_layout.realm_globals.closure_slot(
            &self.planned.plan,
            layout.executable,
            global,
        )?;
        let descriptor = tree_layout.realm_globals.binding(global).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "Annex B variable target has a realm-global descriptor",
                span: Some(span),
            },
        )?;
        flow.emit(plan_external_put(descriptor.binding, slot, span))
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
        if storage.placement() == StoragePlacement::GlobalObject
            && !crate::is_supported_realm_global_binding_goal(self.unit.goal())
        {
            if crate::is_supported_direct_eval_goal(self.unit.goal()) {
                return unsupported(
                    UnsupportedLeafFeature::DirectEvalVariableEnvironment,
                    identifier.span,
                );
            }
            return unsupported(UnsupportedLeafFeature::GlobalEnvironment, identifier.span);
        }
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
        self.validate_function_declaration_child(
            function,
            identifier.span,
            parent,
            child,
            tree_layout,
        )
    }

    fn validate_function_declaration_child(
        &self,
        function: &Function<'arena>,
        name_span: Span,
        parent: ExecutableId,
        child: ExecutableId,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<(), LeafCompilationError> {
        let child_metadata = self
            .planned
            .plan
            .executable(child)
            .ok_or(LeafCompilationError::InvalidExecutable { executable: child })?;
        if child_metadata.parent() != Some(parent)
            || tree_layout.children(parent)?.binary_search(&child).is_err()
            || child_metadata.name_span() != Some(name_span)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function declaration has one typed direct-child constant",
                span: Some(function.span),
            });
        }
        Ok(())
    }
}
