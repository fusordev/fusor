/// Verifies complete compiler metadata and freezes the VM's immutable
/// code-and-metadata input.
///
/// # Errors
///
/// Returns a structured error without exposing partial authority.
pub fn verify_compiler_bytecode_graph(
    input: UnverifiedCompilerBytecodeGraph,
    limits: BytecodeGraphVerificationLimits,
) -> Result<VerifiedBytecode, BytecodeVerificationError> {
    let UnverifiedCompilerBytecodeGraph {
        graph,
        metadata,
        module,
    } = input;
    let function_count = graph.functions().len();
    if metadata.len() != function_count {
        return Err(BytecodeVerificationError::graph(
            BytecodeVerificationErrorKind::FunctionMetadataCountMismatch {
                functions: usize_to_u64(function_count),
                metadata: usize_to_u64(metadata.len()),
            },
        ));
    }

    let mut usage = preflight_usage(&graph, &metadata, limits)?;
    let function_parents = verify_function_tree_ownership(&graph)?;
    let mut verified = Vec::new();
    verified.try_reserve_exact(function_count).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::VerifiedMetadata,
            requested: usize_to_u64(function_count),
        })
    })?;
    let mut requirements = Vec::new();
    requirements
        .try_reserve_exact(EXECUTION_REQUIREMENT_COUNT)
        .map_err(|_| {
            BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
                resource: BytecodeGraphResource::VerifiedMetadata,
                requested: usize_to_u64(EXECUTION_REQUIREMENT_COUNT),
            })
        })?;
    requirements.push(ExecutionRequirement::CoreValues);
    let root_index = usize::try_from(graph.root_id().get()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::VerifiedMetadata,
            limit: u64::from(u32::MAX),
            observed: u64::from(graph.root_id().get()),
        })
    })?;
    let authority_kind = metadata
        .get(root_index)
        .ok_or_else(|| {
            BytecodeVerificationError::graph(
                BytecodeVerificationErrorKind::FunctionMetadataCountMismatch {
                    functions: usize_to_u64(function_count),
                    metadata: usize_to_u64(metadata.len()),
                },
            )
        })?
        .executable_kind;

    for (index, (function, metadata)) in graph.functions().iter().zip(metadata.iter()).enumerate() {
        let id = function_id(index)?;
        let record = verify_function_metadata(
            id,
            &graph,
            function,
            metadata,
            authority_kind,
            limits,
            &mut usage,
        )?;
        collect_requirements(function, &record, &mut requirements);
        verified.push(record);
    }
    verify_closure_metadata(&graph, &verified)?;
    let lexical_derived_this =
        verify_lexical_arrow_environments(&graph, &verified, &function_parents)?;
    verify_class_field_key_bindings(&graph, &verified)?;
    verify_inferred_function_names(&graph, &verified)?;
    verify_method_definitions(&graph, &verified, limits, &mut usage)?;
    let module = verify_module_declaration_record(
        &graph,
        &verified,
        authority_kind,
        module,
        &mut usage,
        limits,
    )?;
    if module.is_some() {
        requirements.push(ExecutionRequirement::ModuleBindings);
    }

    requirements.sort_unstable();
    Ok(VerifiedBytecode {
        graph,
        metadata: Arc::new(verified),
        lexical_derived_this: lexical_derived_this.into(),
        requirements: requirements.into(),
        usage,
        module: module.map(Arc::new),
    })
}

fn preflight_usage(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[UnverifiedFunctionMetadata],
    limits: BytecodeGraphVerificationLimits,
) -> Result<BytecodeGraphUsage, BytecodeVerificationError> {
    let mut usage = BytecodeGraphUsage::default();
    charge(
        &mut usage.policy_transfers,
        graph.usage().closure_edge_evaluations(),
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    let mut source_texts = HashSet::new();
    let mut display_names = HashSet::new();
    source_texts.try_reserve(metadata.len()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::SourceBytes,
            requested: usize_to_u64(metadata.len()),
        })
    })?;
    display_names.try_reserve(metadata.len()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::SourceBytes,
            requested: usize_to_u64(metadata.len()),
        })
    })?;
    for (function, metadata) in graph.functions().iter().zip(metadata) {
        charge(
            &mut usage.variable_definitions,
            usize_to_u64(metadata.variables.len()),
            limits.max_variable_definitions,
            BytecodeGraphResource::VariableDefinitions,
        )?;
        charge(
            &mut usage.closure_definitions,
            usize_to_u64(metadata.closures.len()),
            limits.max_closure_definitions,
            BytecodeGraphResource::ClosureDefinitions,
        )?;
        charge(
            &mut usage.source_mappings,
            usize_to_u64(metadata.source.mappings.len()),
            limits.max_source_mappings,
            BytecodeGraphResource::SourceMappings,
        )?;
        let frame_tracked = metadata
            .variables
            .iter()
            .filter(|definition| requires_binding_state(definition))
            .count();
        let state_entries = usize_to_u64(function.control_flow().instructions().len())
            .checked_mul(usize_to_u64(frame_tracked))
            .ok_or_else(|| {
                BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
                    resource: BytecodeGraphResource::FrameStateEntries,
                    limit: limits.max_frame_state_entries,
                    observed: u64::MAX,
                })
            })?;
        charge(
            &mut usage.frame_state_entries,
            state_entries,
            limits.max_frame_state_entries,
            BytecodeGraphResource::FrameStateEntries,
        )?;
        if source_texts.insert(Arc::as_ptr(&metadata.source.text)) {
            charge(
                &mut usage.source_bytes,
                usize_to_u64(metadata.source.text.len()),
                limits.max_source_bytes,
                BytecodeGraphResource::SourceBytes,
            )?;
        }
        if display_names.insert(Arc::as_ptr(&metadata.source.display_name)) {
            charge(
                &mut usage.source_bytes,
                usize_to_u64(metadata.source.display_name.len()),
                limits.max_source_bytes,
                BytecodeGraphResource::SourceBytes,
            )?;
        }
    }
    Ok(usage)
}

fn verify_function_tree_ownership(
    graph: &VerifiedCompilerFunctionGraph,
) -> Result<Vec<Option<FunctionTemplateId>>, BytecodeVerificationError> {
    let functions = graph.functions();
    let mut incoming = try_filled_vec(
        graph.root_id(),
        functions.len(),
        0_u64,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    let mut parents = try_filled_vec(
        graph.root_id(),
        functions.len(),
        None,
        BytecodeGraphResource::VerifiedMetadata,
    )?;
    for (parent_index, parent) in functions.iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        for constant in parent.constants() {
            let crate::CompilerConstant::Function(child) = constant else {
                continue;
            };
            let Some(child_index) = usize::try_from(child.get())
                .ok()
                .filter(|&index| index < incoming.len())
            else {
                return Err(BytecodeVerificationError::function(
                    *child,
                    BytecodeVerificationErrorKind::FunctionTemplateOwnershipMismatch {
                        child: *child,
                        incoming: 0,
                    },
                ));
            };
            let count = &mut incoming[child_index];
            *count = count.saturating_add(1);
            parents[child_index] = Some(parent_id);
        }
    }
    for (index, &count) in incoming.iter().enumerate() {
        let child = function_id(index)?;
        let expected = u64::from(child != graph.root_id());
        if count != expected {
            return Err(BytecodeVerificationError::function(
                child,
                BytecodeVerificationErrorKind::FunctionTemplateOwnershipMismatch {
                    child,
                    incoming: count,
                },
            ));
        }
    }
    Ok(parents)
}

fn charge(
    total: &mut u64,
    amount: u64,
    limit: u64,
    resource: BytecodeGraphResource,
) -> Result<(), BytecodeVerificationError> {
    *total = total.checked_add(amount).ok_or_else(|| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
            resource,
            limit,
            observed: u64::MAX,
        })
    })?;
    if *total > limit {
        return Err(BytecodeVerificationError::graph(
            BytecodeVerificationErrorKind::LimitExceeded {
                resource,
                limit,
                observed: *total,
            },
        ));
    }
    Ok(())
}

fn try_filled_vec<T: Clone>(
    id: FunctionTemplateId,
    length: usize,
    value: T,
    resource: BytecodeGraphResource,
) -> Result<Vec<T>, BytecodeVerificationError> {
    let mut output = Vec::new();
    output.try_reserve_exact(length).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource,
                requested: usize_to_u64(length),
            },
        )
    })?;
    output.resize(length, value);
    Ok(output)
}

fn try_copy_slice<T: Copy>(
    id: FunctionTemplateId,
    input: &[T],
    resource: BytecodeGraphResource,
) -> Result<Vec<T>, BytecodeVerificationError> {
    let mut output = Vec::new();
    output.try_reserve_exact(input.len()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::AllocationFailed {
                resource,
                requested: usize_to_u64(input.len()),
            },
        )
    })?;
    output.extend_from_slice(input);
    Ok(output)
}

#[allow(
    clippy::too_many_lines,
    reason = "whole-function metadata validation remains one ordered admission boundary"
)]
fn verify_function_metadata(
    id: FunctionTemplateId,
    graph: &VerifiedCompilerFunctionGraph,
    function: &VerifiedCompilerFunction,
    metadata: &UnverifiedFunctionMetadata,
    authority_kind: CompilerExecutableKind,
    limits: BytecodeGraphVerificationLimits,
    usage: &mut BytecodeGraphUsage,
) -> Result<VerifiedFunctionMetadata, BytecodeVerificationError> {
    let flow = function.control_flow();
    verify_executable_kind(id, graph.root_id(), metadata)?;
    verify_header(id, metadata.executable_kind, flow)?;
    let domains = flow.domains();
    let declared_variables = u64::from(domains.argument_count()) + u64::from(domains.local_count());
    if usize_to_u64(metadata.variables.len()) != declared_variables {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableDefinitionCountMismatch {
                declared: declared_variables,
                entries: usize_to_u64(metadata.variables.len()),
            },
        ));
    }
    if usize_to_u64(metadata.closures.len()) != u64::from(domains.closure_var_count()) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::ClosureDefinitionCountMismatch {
                declared: domains.closure_var_count(),
                entries: usize_to_u64(metadata.closures.len()),
            },
        ));
    }
    if metadata.source.mappings.len() != flow.instructions().len() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::SourceMappingCountMismatch {
                instructions: usize_to_u64(flow.instructions().len()),
                mappings: usize_to_u64(metadata.source.mappings.len()),
            },
        ));
    }

    verify_optional_atom(
        id,
        metadata.function_name,
        MetadataAtomField::FunctionName,
        function,
    )?;
    verify_variables(id, function, &metadata.variables)?;
    verify_eval_scope_operands(id, flow, &metadata.variables)?;
    verify_closures(
        id,
        graph.root_id(),
        authority_kind,
        function,
        &metadata.closures,
    )?;
    verify_source(id, flow, metadata)?;
    verify_supported_opcodes(id, flow, metadata)?;
    let mut internal_stack = verify_internal_operand_stack(id, function, limits, usage)?;
    let realm_global_initializer_prefix = verify_realm_global_function_initializers(
        id,
        graph.root_id(),
        function,
        &metadata.closures,
        &internal_stack,
    )?;
    let function_initializer_prefix =
        realm_global_initializer_prefix.max(function.function_initializer_prefix_start() as usize);
    let initializer_sites = verify_function_initializers(
        id,
        function,
        &metadata.variables,
        function_initializer_prefix,
        &internal_stack,
    )?;
    classify_iteration_declarative_local_puts(
        id,
        flow,
        &metadata.variables,
        &mut internal_stack,
        limits,
        usage,
    )?;
    verify_binding_opcodes(
        id,
        flow,
        &metadata.variables,
        &metadata.closures,
        &internal_stack,
    )?;
    let binding_transfers = verify_binding_states(
        id,
        graph,
        function,
        &metadata.variables,
        &initializer_sites,
        &internal_stack,
        realm_global_initializer_prefix,
        usage.policy_transfers,
        limits.max_policy_transfers,
    )?;
    charge(
        &mut usage.policy_transfers,
        binding_transfers,
        limits.max_policy_transfers,
        BytecodeGraphResource::PolicyTransfers,
    )?;
    Ok(VerifiedFunctionMetadata {
        executable_kind: metadata.executable_kind,
        function_name: metadata.function_name,
        variables: Arc::clone(&metadata.variables),
        closures: Arc::clone(&metadata.closures),
        source: VerifiedCompilerSource {
            display_name: Arc::clone(&metadata.source.display_name),
            text: Arc::clone(&metadata.source.text),
            function_span: metadata.source.function_span,
            name_span: metadata.source.name_span,
            mappings: Arc::clone(&metadata.source.mappings),
            strict_mode_pcs: metadata.source.strict_mode_pcs.as_ref().map(Arc::clone),
        },
        internal_stack,
    })
}

#[allow(clippy::too_many_lines)]
fn verify_executable_kind(
    id: FunctionTemplateId,
    root: FunctionTemplateId,
    metadata: &UnverifiedFunctionMetadata,
) -> Result<(), BytecodeVerificationError> {
    match metadata.executable_kind {
        CompilerExecutableKind::GlobalScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::GlobalScriptNotRoot,
                ));
            }
            if metadata_has_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::GlobalScriptHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::IndirectEvalScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::IndirectEvalScriptNotRoot,
                ));
            }
            if metadata_has_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::IndirectEvalScriptHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::DirectEvalScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DirectEvalScriptNotRoot,
                ));
            }
            if metadata_has_local_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DirectEvalScriptHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::OrdinaryFunction
        | CompilerExecutableKind::GeneratorFunction
        | CompilerExecutableKind::AsyncFunction
        | CompilerExecutableKind::AsyncGeneratorFunction => Ok(()),
        CompilerExecutableKind::OrdinaryArrow | CompilerExecutableKind::AsyncArrow => {
            if metadata_has_local_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::OrdinaryArrowHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::OrdinaryMethod
        | CompilerExecutableKind::ClassInstanceInitializer
        | CompilerExecutableKind::GeneratorMethod
        | CompilerExecutableKind::AsyncMethod
        | CompilerExecutableKind::AsyncGeneratorMethod
        | CompilerExecutableKind::ClassConstructor => {
            if metadata_has_local_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::OrdinaryMethodHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DynamicFunctionScriptNotRoot,
                ));
            }
            if metadata_has_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DynamicFunctionScriptHasFunctionName,
                ));
            }
            Ok(())
        }
        CompilerExecutableKind::Module => {
            if id != root {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::ModuleNotRoot,
                ));
            }
            if metadata_has_function_name(metadata) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::ModuleHasFunctionName,
                ));
            }
            Ok(())
        }
    }
}

fn metadata_has_function_name(metadata: &UnverifiedFunctionMetadata) -> bool {
    metadata_has_local_function_name(metadata)
        || metadata
            .closures
            .iter()
            .any(|definition| definition.policy().kind() == CompilerBindingKind::FunctionName)
}

fn metadata_has_local_function_name(metadata: &UnverifiedFunctionMetadata) -> bool {
    metadata.function_name.is_some()
        || metadata
            .variables
            .iter()
            .any(|definition| definition.policy.kind() == CompilerBindingKind::FunctionName)
}

#[allow(
    clippy::too_many_lines,
    reason = "all compiler executable kinds and their exact pinned headers are audited together"
)]
fn verify_header(
    id: FunctionTemplateId,
    executable_kind: CompilerExecutableKind,
    flow: &VerifiedControlFlow,
) -> Result<(), BytecodeVerificationError> {
    let header = *flow.function_header();
    let arguments = flow.domains().argument_count();
    match executable_kind {
        CompilerExecutableKind::GlobalScript => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0400
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::GlobalScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::IndirectEvalScript => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0400
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::IndirectEvalScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::DirectEvalScript => {
            let flags = header.flags().bits();
            if header.kind() != FunctionKind::Normal
                || flags & !0x15c0 != 0
                || flags & 0x0400 == 0
                || (flags & 0x1000 != 0 && flags & 0x0080 == 0)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DirectEvalScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::OrdinaryFunction => {
            if header.kind() != FunctionKind::Normal
                || !matches!(header.flags().bits(), 0x0641 | 0x0643)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::OrdinaryArrow => {
            if header.kind() != FunctionKind::Normal
                || !matches!(header.flags().bits(), 0x0440 | 0x0442)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncArrow => {
            if header.kind() != FunctionKind::Async
                || !matches!(header.flags().bits(), 0x0460 | 0x0462)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::OrdinaryMethod => {
            if header.kind() != FunctionKind::Normal
                || !matches!(header.flags().bits(), 0x0740 | 0x0742)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::ClassInstanceInitializer => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0742
                || !header.mode().is_strict()
                || header.defined_argument_count() != 0
                || arguments != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
        }
        CompilerExecutableKind::ClassConstructor => {
            if header.kind() != FunctionKind::Normal
                || !matches!(header.flags().bits(), 0x0748 | 0x074a | 0x07cc | 0x07ce)
                || !header.mode().is_strict()
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::GeneratorFunction => {
            if header.kind() != FunctionKind::Generator
                || !matches!(header.flags().bits(), 0x0650 | 0x0652)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::GeneratorMethod => {
            if header.kind() != FunctionKind::Generator
                || !matches!(header.flags().bits(), 0x0750 | 0x0752)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncFunction => {
            if header.kind() != FunctionKind::Async
                || !matches!(header.flags().bits(), 0x0660 | 0x0662)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncMethod => {
            if header.kind() != FunctionKind::Async
                || !matches!(header.flags().bits(), 0x0760 | 0x0762)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncGeneratorFunction => {
            if header.kind() != FunctionKind::AsyncGenerator
                || !matches!(header.flags().bits(), 0x0670 | 0x0672)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::AsyncGeneratorMethod => {
            if header.kind() != FunctionKind::AsyncGenerator
                || !matches!(header.flags().bits(), 0x0770 | 0x0772)
                || header.mode().bits() & !1 != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() > arguments
                || (header.flags().has_simple_parameter_list()
                    && header.defined_argument_count() != arguments)
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DefinedArgumentCountMismatch {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0400
                || header.mode().bits() != 0
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DynamicFunctionScriptHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
        CompilerExecutableKind::Module => {
            if header.kind() != FunctionKind::Normal
                || header.flags().bits() != 0x0400
                || !header.mode().is_strict()
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::UnsupportedFunctionHeader,
                ));
            }
            if header.defined_argument_count() != 0 || arguments != 0 {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::ModuleHasArguments {
                        defined: header.defined_argument_count(),
                        arguments,
                    },
                ));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn verify_variables(
    id: FunctionTemplateId,
    function: &VerifiedCompilerFunction,
    variables: &[VariableDefinition],
) -> Result<(), BytecodeVerificationError> {
    let domains = function.control_flow().domains();
    let arguments = usize::try_from(domains.argument_count()).map_err(|_| {
        BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableDefinitionCountMismatch {
                declared: u64::from(domains.argument_count()),
                entries: usize_to_u64(variables.len()),
            },
        )
    })?;
    let locals = domains.local_count();
    let strict = function.control_flow().function_header().mode().is_strict();
    let variable_references = function
        .control_flow()
        .function_header()
        .variable_reference_count();
    let mut seen_references = try_filled_vec(
        id,
        variable_references as usize,
        false,
        BytecodeGraphResource::VariableDefinitions,
    )?;
    let mut initializer_definitions = Vec::new();
    initializer_definitions
        .try_reserve_exact(variables.len())
        .map_err(|_| {
            BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::AllocationFailed {
                    resource: BytecodeGraphResource::VariableDefinitions,
                    requested: usize_to_u64(variables.len()),
                },
            )
        })?;
    let mut arguments_object_definition = None;
    for (index, definition) in variables.iter().enumerate() {
        let definition_index = usize_to_u32(index);
        let slot = if index < arguments {
            BindingSlot::Argument(definition_index)
        } else {
            BindingSlot::Local(usize_to_u32(index - arguments))
        };
        verify_required_atom(
            id,
            definition.name,
            MetadataAtomField::VariableName(definition_index),
            function,
        )?;
        if definition.arguments_object {
            let valid = index >= arguments
                && arguments_object_definition.is_none()
                && atom_contents(definition.name, function.atoms())
                    .is_some_and(|name| name.code_units().eq("arguments".encode_utf16()))
                && definition.policy.kind() == CompilerBindingKind::Var
                && definition.policy.initialization()
                    == CompilerInitializationPolicy::UndefinedAtInstantiation
                && definition.policy.writes() == CompilerWritePolicy::Mutable
                && !definition.policy.has_temporal_dead_zone()
                && !definition.has_scope;
            if !valid {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::ArgumentsObjectMetadataMismatch {
                        definition: Some(definition_index),
                        pc: None,
                    },
                ));
            }
            arguments_object_definition = Some(definition_index);
        }
        if !definition.policy.is_valid_for_function(strict) {
            return Err(policy_error(
                id,
                slot,
                None,
                BindingPolicyViolationReason::InvalidDeclarationPolicy,
            ));
        }
        let requires_function_initializer = matches!(
            definition.policy.initialization,
            CompilerInitializationPolicy::FunctionAtInstantiation
                | CompilerInitializationPolicy::FunctionAtScopeEntry
        );
        match definition.function_initializer {
            None if requires_function_initializer => {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: definition_index,
                        constant: None,
                    },
                ));
            }
            Some(constant)
                if !requires_function_initializer
                    && definition.policy.kind != CompilerBindingKind::Parameter =>
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: definition_index,
                        constant: Some(constant),
                    },
                ));
            }
            Some(constant)
                if !matches!(
                    function.constants().get(constant as usize),
                    Some(crate::CompilerConstant::Function(_))
                ) =>
            {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: definition_index,
                        constant: Some(constant),
                    },
                ));
            }
            _ => {}
        }
        if let Some(constant) = definition.function_initializer {
            initializer_definitions.push((constant, definition_index));
        }
        if index < arguments {
            if definition.policy.kind != CompilerBindingKind::Parameter
                || definition.policy.temporal_dead_zone
                || definition.has_scope
                || definition.scope_next != ScopeLink::End
            {
                return Err(policy_error(
                    id,
                    slot,
                    None,
                    BindingPolicyViolationReason::InvalidArgumentDefinition,
                ));
            }
        } else {
            if definition.has_scope != definition.policy.has_scope() {
                return Err(policy_error(
                    id,
                    slot,
                    None,
                    BindingPolicyViolationReason::ScopeFlagMismatch,
                ));
            }
            match definition.scope_next {
                ScopeLink::End => {}
                ScopeLink::ArgumentScopeEnd => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ArgumentScopeMetadataUnsupported {
                            definition: definition_index,
                        },
                    ));
                }
                ScopeLink::Local(target) if target < locals => {
                    let target_definition = &variables[arguments + target as usize];
                    if definition.has_scope != target_definition.has_scope {
                        return Err(BytecodeVerificationError::function(
                            id,
                            BytecodeVerificationErrorKind::ScopeLinkKindMismatch {
                                definition: definition_index,
                                target,
                            },
                        ));
                    }
                }
                ScopeLink::Local(target) => {
                    return Err(BytecodeVerificationError::function(
                        id,
                        BytecodeVerificationErrorKind::ScopeLinkOutOfBounds {
                            definition: definition_index,
                            target,
                            locals,
                        },
                    ));
                }
            }
        }
        if let Some(reference) = definition.variable_reference {
            if reference >= variable_references {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::VariableReferenceOutOfBounds {
                        definition: definition_index,
                        reference,
                        len: variable_references,
                    },
                ));
            }
            let seen = &mut seen_references[reference as usize];
            if std::mem::replace(seen, true) {
                return Err(BytecodeVerificationError::function(
                    id,
                    BytecodeVerificationErrorKind::DuplicateVariableReference { reference },
                ));
            }
        }
    }
    initializer_definitions.sort_unstable_by_key(|&(constant, _)| constant);
    for pair in initializer_definitions.windows(2) {
        let [(constant, first), (duplicate_constant, duplicate)] = pair else {
            continue;
        };
        if constant == duplicate_constant {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionInitializerConstantReused {
                    constant: *constant,
                    first: *first,
                    duplicate: *duplicate,
                },
            ));
        }
    }
    verify_scope_links(id, &variables[arguments..])?;
    let captured = usize_to_u32(seen_references.iter().filter(|&&seen| seen).count());
    if captured != variable_references || seen_references.iter().any(|seen| !seen) {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::VariableReferenceDomainMismatch {
                declared: variable_references,
                captured,
            },
        ));
    }
    verify_capture_layout(id, function, variables, arguments)
}
