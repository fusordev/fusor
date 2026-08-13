// Final verification of module instantiation metadata.
//
// A Module root carries one `UnverifiedModuleDeclarationRecord` describing the
// module-environment layout the runtime linker materializes: one cell per
// top-level binding, in declaration order, plus the static module requests.
// This file cross-checks that record against the verified root closure domain
// so a linker can trust every descriptor.

fn root_function(
    graph: &VerifiedCompilerFunctionGraph,
) -> Result<&VerifiedCompilerFunction, BytecodeVerificationError> {
    graph
        .function(graph.root_id())
        .ok_or_else(|| BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::VerifiedMetadata,
            requested: 1,
        }))
}

#[allow(clippy::too_many_lines)]
fn verify_module_declaration_record(
    graph: &VerifiedCompilerFunctionGraph,
    verified: &[VerifiedFunctionMetadata],
    authority_kind: CompilerExecutableKind,
    module: Option<Arc<UnverifiedModuleDeclarationRecord>>,
    _usage: &mut BytecodeGraphUsage,
    _limits: BytecodeGraphVerificationLimits,
) -> Result<Option<ModuleDeclarationRecord>, BytecodeVerificationError> {
    let Some(record) = module else {
        if authority_kind == CompilerExecutableKind::Module {
            return Err(BytecodeVerificationError::graph(
                BytecodeVerificationErrorKind::ModuleDeclarationRecordMissing,
            ));
        }
        return Ok(None);
    };
    if authority_kind != CompilerExecutableKind::Module {
        return Err(BytecodeVerificationError::graph(
            BytecodeVerificationErrorKind::ModuleDeclarationRecordUnexpected,
        ));
    }
    let root_id = graph.root_id();
    let root_index = usize::try_from(root_id.get()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::VerifiedMetadata,
            limit: u64::from(u32::MAX),
            observed: u64::from(root_id.get()),
        })
    })?;
    let root_function = root_function(graph)?;
    let root_metadata = verified.get(root_index).ok_or_else(|| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::FunctionMetadataCountMismatch {
            functions: usize_to_u64(verified.len()),
            metadata: usize_to_u64(verified.len()),
        })
    })?;
    let closures = root_metadata.closures();
    let closure_count = u32::try_from(closures.len()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::ClosureDefinitions,
            requested: usize_to_u64(closures.len()),
        })
    })?;
    let request_count = u32::try_from(record.requests.len()).map_err(|_| {
        BytecodeVerificationError::graph(BytecodeVerificationErrorKind::AllocationFailed {
            resource: BytecodeGraphResource::VerifiedMetadata,
            requested: usize_to_u64(record.requests.len()),
        })
    })?;

    for (request_index, request) in record.requests.iter().enumerate() {
        let index = u32::try_from(request_index).map_err(|_| {
            BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::VerifiedMetadata,
                limit: u64::from(u32::MAX),
                observed: usize_to_u64(request_index),
            })
        })?;
        verify_atom_bounds(
            root_id,
            request.specifier,
            MetadataAtomField::ModuleRequestSpecifier(index),
            root_function,
        )?;
    }

    let mut verified_bindings: Vec<ModuleBindingDescriptor> =
        Vec::with_capacity(record.bindings.len());
    let mut seen_slots: Vec<bool> = vec![false; closures.len()];
    for (binding_index, binding) in record.bindings.iter().enumerate() {
        let index = u32::try_from(binding_index).map_err(|_| {
            BytecodeVerificationError::graph(BytecodeVerificationErrorKind::LimitExceeded {
                resource: BytecodeGraphResource::VerifiedMetadata,
                limit: u64::from(u32::MAX),
                observed: usize_to_u64(binding_index),
            })
        })?;
        verify_atom_bounds(
            root_id,
            binding.name,
            MetadataAtomField::ModuleBindingName(index),
            root_function,
        )?;
        if binding.slot >= closure_count {
            return Err(BytecodeVerificationError::graph(
                BytecodeVerificationErrorKind::ModuleBindingSlotOutOfBounds {
                    binding: index,
                    slot: binding.slot,
                    closures: closure_count,
                },
            ));
        }
        let slot = binding.slot as usize;
        if seen_slots[slot] {
            return Err(BytecodeVerificationError::graph(
                BytecodeVerificationErrorKind::ModuleBindingSlotOrder {
                    binding: index,
                    slot: binding.slot,
                },
            ));
        }
        seen_slots[slot] = true;
        let closure = &closures[slot];
        let source_matches = matches!(
            closure.source(),
            CompilerClosureSource::Module { index: env_index } if env_index == index
        );
        let binding_matches = matches!(
            closure.binding(),
            CompilerClosureBinding::Captured(policy) if policy == binding.policy
        );
        if !source_matches || !binding_matches {
            return Err(BytecodeVerificationError::graph(
                BytecodeVerificationErrorKind::ModuleBindingSlotMismatch {
                    binding: index,
                    slot: binding.slot,
                },
            ));
        }
        verify_module_binding_policy(binding, index)?;
        verify_module_binding_import(root_id, root_function, binding, index, request_count)?;
        verified_bindings.push(ModuleBindingDescriptor {
            name: binding.name,
            slot: binding.slot,
            policy: binding.policy,
            origin: binding.origin,
            initializer: binding.initializer,
            import: binding.import.clone(),
        });
    }

    Ok(Some(ModuleDeclarationRecord {
        bindings: verified_bindings.into(),
        requests: Arc::clone(&record.requests),
    }))
}

fn module_origin_policy_supported(
    origin: ModuleBindingOrigin,
    policy: CompilerBindingPolicy,
) -> bool {
    match origin {
        ModuleBindingOrigin::Local => matches!(
            policy.kind(),
            CompilerBindingKind::Var
                | CompilerBindingKind::Let
                | CompilerBindingKind::Const
                | CompilerBindingKind::Function
        ),
        ModuleBindingOrigin::Import | ModuleBindingOrigin::Namespace => {
            matches!(policy.kind(), CompilerBindingKind::Const)
                && policy.writes() == CompilerWritePolicy::Immutable
                && policy.has_temporal_dead_zone()
        }
    }
}

fn verify_module_binding_policy(
    binding: &UnverifiedModuleBindingDescriptor,
    index: u32,
) -> Result<(), BytecodeVerificationError> {
    if !module_origin_policy_supported(binding.origin, binding.policy) {
        return Err(BytecodeVerificationError::graph(
            BytecodeVerificationErrorKind::ModuleBindingPolicyMismatch { binding: index },
        ));
    }
    let initialized_in_frame = match binding.origin {
        ModuleBindingOrigin::Local => true,
        ModuleBindingOrigin::Import | ModuleBindingOrigin::Namespace => false,
    };
    if initialized_in_frame {
        return Ok(());
    }
    if binding.initializer.is_some() {
        return Err(BytecodeVerificationError::graph(
            BytecodeVerificationErrorKind::ModuleBindingInitializerMismatch {
                binding: index,
                constant: binding.initializer,
            },
        ));
    }
    Ok(())
}

fn verify_module_binding_import(
    root_id: FunctionTemplateId,
    root_function: &VerifiedCompilerFunction,
    binding: &UnverifiedModuleBindingDescriptor,
    index: u32,
    request_count: u32,
) -> Result<(), BytecodeVerificationError> {
    match binding.origin {
        ModuleBindingOrigin::Local => {
            if binding.import.is_some() {
                return Err(BytecodeVerificationError::graph(
                    BytecodeVerificationErrorKind::ModuleBindingPolicyMismatch { binding: index },
                ));
            }
            Ok(())
        }
        ModuleBindingOrigin::Import | ModuleBindingOrigin::Namespace => {
            let import = binding.import.as_ref().ok_or_else(|| {
                BytecodeVerificationError::graph(
                    BytecodeVerificationErrorKind::ModuleBindingPolicyMismatch { binding: index },
                )
            })?;
            if import.request() >= request_count {
                return Err(BytecodeVerificationError::graph(
                    BytecodeVerificationErrorKind::ModuleImportRequestOutOfBounds {
                        binding: index,
                        request: import.request(),
                        requests: request_count,
                    },
                ));
            }
            if let Some(named) = import.named_atom() {
                verify_atom_bounds(
                    root_id,
                    named,
                    MetadataAtomField::ModuleImportName(index),
                    root_function,
                )?;
            }
            Ok(())
        }
    }
}
