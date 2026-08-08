use std::{collections::HashMap, sync::Arc};

use crate::storage::{
    BindingId, CaptureSource, DeclarationKind, DeclarationPolicy, Executable, ExecutableId,
    InitializationPolicy, StoragePlacement, WritePolicy,
};
use oxc_semantic::ScopeId;
use quickjs_bytecode::{
    ClosureVariableDefinition as VerifiedClosureVariableDefinition,
    CompilerBindingKind as VerifiedBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerCapturedBinding, CompilerClosureSource as CompilerGraphClosureSource,
    CompilerConstant as CompilerGraphConstant,
    CompilerInitializationPolicy as VerifiedInitializationPolicy,
    CompilerWritePolicy as VerifiedWritePolicy, FunctionGraphVerificationLimits,
    FunctionTemplateId, ScopeLink, UnverifiedCompilerFunction, UnverifiedCompilerFunctionGraph,
    VariableDefinition, VerifiedCompilerFunctionGraph, verify_compiler_function_graph,
};

use super::layouts::RealmGlobalRootSource;
use super::{
    CompilationContext, CompiledClosureSource, CompiledClosureVariable, CompiledConstant,
    CompiledConstantPool, CompiledFunction, CompiledMetadataAtomKey, CompiledRealmGlobal,
    CompiledRealmGlobalSource, FrameLayout, FrameSlot, FunctionTreeLayout, LeafCompilationError,
    LogicalCompilerScope, checked_function_index,
};

fn verified_binding_policy(
    policy: DeclarationPolicy,
) -> Result<CompilerBindingPolicy, LeafCompilationError> {
    let kind = match policy.kind() {
        DeclarationKind::Parameter => VerifiedBindingKind::Parameter,
        DeclarationKind::Var => VerifiedBindingKind::Var,
        DeclarationKind::Let | DeclarationKind::Class => VerifiedBindingKind::Let,
        DeclarationKind::Const => VerifiedBindingKind::Const,
        DeclarationKind::ClassName => VerifiedBindingKind::ClassName,
        DeclarationKind::ClassFieldKey => VerifiedBindingKind::ClassFieldKey,
        DeclarationKind::ClassPrivateName => VerifiedBindingKind::ClassPrivateName,
        DeclarationKind::ClassStaticReceiver => VerifiedBindingKind::ClassStaticReceiver,
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

pub(in crate::lowering) fn verified_storage_policy(
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

pub(in crate::lowering) const fn constructor_realm_lookup_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        VerifiedBindingKind::GlobalReference,
        VerifiedInitializationPolicy::ConstructorRealmLookup,
        VerifiedWritePolicy::Mutable,
        false,
    )
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

pub(in crate::lowering) const fn binding_has_scope(policy: DeclarationPolicy) -> bool {
    matches!(
        policy.kind(),
        DeclarationKind::Let
            | DeclarationKind::Class
            | DeclarationKind::Const
            | DeclarationKind::ClassName
            | DeclarationKind::ClassFieldKey
            | DeclarationKind::ClassPrivateName
            | DeclarationKind::ClassStaticReceiver
            | DeclarationKind::Catch
    ) || matches!(
        policy.initialization(),
        InitializationPolicy::FunctionAtScopeEntry
    )
}

pub(in crate::lowering) fn verify_compiled_function_graph(
    root: ExecutableId,
    functions: &[CompiledFunction],
    limits: FunctionGraphVerificationLimits,
) -> Result<VerifiedCompilerFunctionGraph, LeafCompilationError> {
    if functions.first().map(CompiledFunction::executable) != Some(root) {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "compiled subtree begins with its selected root",
            span: None,
        });
    }
    let mut identities = Vec::with_capacity(functions.len());
    for (index, function) in functions.iter().enumerate() {
        if identities
            .last()
            .is_some_and(|(previous, _)| *previous >= function.executable())
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled subtree executables are strictly ordered",
                span: None,
            });
        }
        identities.push((function.executable(), checked_function_template_id(index)?));
    }
    let root = resolve_function_template_id(&identities, root)?;
    let root_index =
        usize::try_from(root.get()).map_err(|_| LeafCompilationError::SemanticInvariant {
            invariant: "graph-local root identity fits usize",
            span: None,
        })?;

    let (records, parent_counts) = build_unverified_graph_records(functions, &identities)?;
    for (index, &actual) in parent_counts.iter().enumerate() {
        let expected = u32::from(index != root_index);
        if actual != expected {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled function subtree has exactly one parent per child",
                span: None,
            });
        }
    }

    verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(root, records.into()),
        limits,
    )
    .map_err(|source| {
        let span = source
            .function()
            .and_then(|template| usize::try_from(template.get()).ok())
            .and_then(|index| functions.get(index))
            .and_then(|function| {
                function
                    .storage_plan
                    .executable(function.executable)
                    .map(Executable::span)
            });
        LeafCompilationError::FunctionGraphVerification { span, source }
    })
}

impl CompilationContext<'_, '_, '_> {
    pub(in crate::lowering) fn compiler_capture_layout(
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

    pub(in crate::lowering) fn compiled_variable_definitions(
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

    pub(in crate::lowering) fn compiled_closure_definitions(
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
            let source = compiler_graph_realm_global_source(global);
            let mut definition = match global.binding {
                quickjs_bytecode::CompilerClosureBinding::Captured(policy) => {
                    VerifiedClosureVariableDefinition::new(Some(name), policy, source)
                }
                quickjs_bytecode::CompilerClosureBinding::RealmGlobal(policy) => {
                    VerifiedClosureVariableDefinition::realm_global(Some(name), policy, source)
                }
            };
            if let Some(initializer) = global.function_initializer {
                definition = definition.with_function_initializer(initializer);
            }
            definitions.push(definition);
        }
        Ok(definitions)
    }

    pub(in crate::lowering) fn compiled_closure_variables(
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

    pub(in crate::lowering) fn compiled_realm_globals(
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
                if executable.index() != 0 {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "only a Script root originates realm-global slots",
                        span: Some(metadata.span()),
                    });
                }
                match binding.root_source {
                    RealmGlobalRootSource::ConstructorRealm
                        if crate::is_supported_script_compilation_goal(self.unit.goal()) =>
                    {
                        CompiledRealmGlobalSource::ConstructorRealm
                    }
                    RealmGlobalRootSource::DirectEvalBinding { index }
                        if crate::is_supported_direct_eval_goal(self.unit.goal()) =>
                    {
                        CompiledRealmGlobalSource::DirectEvalBinding {
                            index,
                            environment_size: tree_layout.realm_globals.direct_environment_size(),
                        }
                    }
                    RealmGlobalRootSource::DirectEvalVariable { index }
                        if crate::is_supported_direct_eval_goal(self.unit.goal()) =>
                    {
                        CompiledRealmGlobalSource::DirectEvalVariable {
                            index,
                            environment_size: tree_layout.realm_globals.direct_environment_size(),
                        }
                    }
                    RealmGlobalRootSource::ConstructorRealm
                    | RealmGlobalRootSource::DirectEvalBinding { .. }
                    | RealmGlobalRootSource::DirectEvalVariable { .. } => {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "Script root external-binding source matches its compilation goal",
                            span: Some(metadata.span()),
                        });
                    }
                }
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
                binding: binding.binding,
                policy: binding.policy,
                function_initializer,
            });
        }
        Ok(globals)
    }

    pub(in crate::lowering) fn scope_for_binding(
        &self,
        binding: BindingId,
    ) -> Result<ScopeId, LeafCompilationError> {
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
}

fn build_unverified_graph_records(
    functions: &[CompiledFunction],
    identities: &[(ExecutableId, FunctionTemplateId)],
) -> Result<(Vec<UnverifiedCompilerFunction>, Vec<u32>), LeafCompilationError> {
    let mut records = Vec::with_capacity(functions.len());
    let mut parent_counts = vec![0_u32; functions.len()];
    for function in functions {
        let mut constants = Vec::with_capacity(function.constants.len());
        for constant in function.constants.iter() {
            match constant {
                CompiledConstant::Value(value) => {
                    constants.push(CompilerGraphConstant::Value(value.clone()));
                }
                CompiledConstant::Function(function_constant) => {
                    let template =
                        resolve_function_template_id(identities, function_constant.executable())?;
                    let template_index = usize::try_from(template.get()).map_err(|_| {
                        LeafCompilationError::SemanticInvariant {
                            invariant: "graph-local template identity fits usize",
                            span: None,
                        }
                    })?;
                    let count = parent_counts.get_mut(template_index).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "function constant has a graph parent-count slot",
                            span: None,
                        },
                    )?;
                    *count =
                        count
                            .checked_add(1)
                            .ok_or(LeafCompilationError::CapacityExceeded {
                                domain: "compiler function parent edges",
                            })?;
                    constants.push(CompilerGraphConstant::Function(template));
                }
            }
        }
        let closure_capacity = function
            .closure_variables
            .len()
            .checked_add(function.realm_globals.len())
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "compiler graph closure sources",
            })?;
        let mut closure_sources = Vec::with_capacity(closure_capacity);
        for (index, closure) in function.closure_variables.iter().copied().enumerate() {
            if closure.slot().index() != index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled closure slots are dense and ordered",
                    span: None,
                });
            }
            closure_sources.push(match closure.source() {
                CompiledClosureSource::ParentVariableReference(index) => {
                    CompilerGraphClosureSource::ParentVariableReference(u32::from(index))
                }
                CompiledClosureSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            });
        }
        for (offset, global) in function.realm_globals.iter().enumerate() {
            let expected = function.closure_variables.len().checked_add(offset).ok_or(
                LeafCompilationError::CapacityExceeded {
                    domain: "compiler graph closure sources",
                },
            )?;
            if usize::from(global.slot()) != expected {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled realm-global slots follow captured closure slots",
                    span: None,
                });
            }
            closure_sources.push(compiler_graph_realm_global_source(global));
        }
        records.push(
            UnverifiedCompilerFunction::new(
                Arc::clone(&function.control_flow),
                constants.into(),
                closure_sources.into(),
            )
            .with_atom_pool(Arc::clone(&function.atoms))
            .with_direct_eval(
                function
                    .storage_plan
                    .executable(function.executable)
                    .ok_or(LeafCompilationError::InvalidExecutable {
                        executable: function.executable,
                    })?
                    .has_direct_eval(),
            ),
        );
    }
    Ok((records, parent_counts))
}

const fn compiler_graph_realm_global_source(
    global: &CompiledRealmGlobal,
) -> CompilerGraphClosureSource {
    match global.source() {
        CompiledRealmGlobalSource::ConstructorRealm => {
            CompilerGraphClosureSource::ConstructorRealmGlobal(global.atom())
        }
        CompiledRealmGlobalSource::DirectEvalBinding {
            index,
            environment_size,
        } => CompilerGraphClosureSource::DirectEvalBinding {
            index,
            environment_size,
        },
        CompiledRealmGlobalSource::DirectEvalVariable {
            index,
            environment_size,
        } => CompilerGraphClosureSource::DirectEvalVariable {
            index,
            environment_size,
        },
        CompiledRealmGlobalSource::ParentClosure(index) => {
            CompilerGraphClosureSource::ParentClosure(index as u32)
        }
    }
}

fn checked_function_template_id(index: usize) -> Result<FunctionTemplateId, LeafCompilationError> {
    u32::try_from(index)
        .map(FunctionTemplateId::new)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "compiler function graph templates",
        })
}

fn resolve_function_template_id(
    identities: &[(ExecutableId, FunctionTemplateId)],
    executable: ExecutableId,
) -> Result<FunctionTemplateId, LeafCompilationError> {
    identities
        .binary_search_by_key(&executable, |(candidate, _)| *candidate)
        .ok()
        .and_then(|index| identities.get(index))
        .map(|(_, template)| *template)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "function constant belongs to the compiled subtree",
            span: None,
        })
}
