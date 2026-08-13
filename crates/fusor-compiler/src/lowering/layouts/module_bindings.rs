use std::sync::Arc;

use fusor_bytecode::{CompilerBindingPolicy, ModuleBindingOrigin, ModuleImportName};
use fusor_frontend::Span;

use crate::storage::{
    BindingId, DeclarationKind, ExecutableId, InitializationPolicy, StoragePlacement, StoragePlan,
};

use super::super::{
    LeafCompilationError, ModuleBindingId, UnsupportedLeafFeature, checked_function_entry_count,
    checked_function_index, unsupported,
};

#[derive(Clone, Copy)]
pub(in crate::lowering) struct ModuleBindingLayoutInput<'plan> {
    pub(in crate::lowering) plan: &'plan StoragePlan,
    pub(in crate::lowering) enabled: bool,
}

#[allow(dead_code)]
pub(in crate::lowering) struct ModuleBindingDescriptor {
    pub(in crate::lowering) name: Arc<str>,
    pub(in crate::lowering) first_span: Span,
    pub(in crate::lowering) policy: CompilerBindingPolicy,
    pub(in crate::lowering) origin: ModuleBindingOrigin,
    pub(in crate::lowering) declaration: Option<BindingId>,
    pub(in crate::lowering) import: Option<ModuleImportName>,
    pub(in crate::lowering) function_child: Option<ExecutableId>,
}

pub(in crate::lowering) struct ModuleBindingLayout {
    bindings: Box<[ModuleBindingDescriptor]>,
    by_binding: Vec<Option<ModuleBindingId>>,
    import_ranges: Box<[std::ops::Range<usize>]>,
    imports: Box<[ModuleBindingId]>,
}

impl ModuleBindingLayout {
    pub(in crate::lowering) fn new(
        input: ModuleBindingLayoutInput<'_>,
    ) -> Result<Self, LeafCompilationError> {
        let plan = input.plan;
        if !input.enabled {
            return Ok(Self {
                bindings: Box::default(),
                by_binding: vec![None; plan.bindings().len()],
                import_ranges: vec![0..0; plan.executables().len()].into_boxed_slice(),
                imports: Box::default(),
            });
        }
        let mut builder = ModuleBindingLayoutBuilder::new(plan);
        builder.collect_declarations()?;
        builder.collect_resolved_needs()?;
        builder.finish()
    }

    pub(in crate::lowering) fn binding(
        &self,
        id: ModuleBindingId,
    ) -> Option<&ModuleBindingDescriptor> {
        self.bindings.get(id.index())
    }

    pub(in crate::lowering) fn for_binding(&self, id: BindingId) -> Option<ModuleBindingId> {
        self.by_binding.get(id.index()).copied().flatten()
    }

    pub(in crate::lowering) fn imports_for(
        &self,
        executable: ExecutableId,
    ) -> Result<&[ModuleBindingId], LeafCompilationError> {
        let range = self
            .import_ranges
            .get(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        self.imports
            .get(range.clone())
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "module binding range indexes flat imports",
                span: None,
            })
    }

    /// Returns the closure-domain slot for one module binding in `executable`.
    /// `realm_global_count` is the number of realm-global closure descriptors
    /// already appended before module bindings in this executable.
    pub(in crate::lowering) fn closure_slot(
        &self,
        plan: &StoragePlan,
        executable: ExecutableId,
        id: ModuleBindingId,
        realm_global_count: usize,
    ) -> Result<u16, LeafCompilationError> {
        let captures = plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let imports = self.imports_for(executable)?;
        let offset =
            imports
                .binary_search(&id)
                .map_err(|_| LeafCompilationError::SemanticInvariant {
                    invariant: "referenced module binding is imported by its executable",
                    span: self.binding(id).map(|binding| binding.first_span),
                })?;
        let index = captures
            .len()
            .checked_add(realm_global_count)
            .and_then(|value| value.checked_add(offset))
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "function closure variables",
            })?;
        checked_function_index(index, "function closure variables")
    }
}

struct ModuleBindingLayoutBuilder<'plan> {
    plan: &'plan StoragePlan,
    bindings: Vec<ModuleBindingDescriptor>,
    by_binding: Vec<Option<ModuleBindingId>>,
    needs: Vec<Vec<ModuleBindingId>>,
}

impl<'plan> ModuleBindingLayoutBuilder<'plan> {
    fn new(plan: &'plan StoragePlan) -> Self {
        Self {
            plan,
            bindings: Vec::new(),
            by_binding: vec![None; plan.bindings().len()],
            needs: vec![Vec::new(); plan.executables().len()],
        }
    }

    fn collect_declarations(&mut self) -> Result<(), LeafCompilationError> {
        for binding in self.plan.bindings() {
            if !matches!(
                binding.placement(),
                StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport
            ) {
                continue;
            }
            let first_span = binding.declaration_spans().first().copied().ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "module declaration retains a source span",
                    span: None,
                },
            )?;
            self.validate_module_declaration(binding, first_span)?;
            let name: Arc<str> = Arc::from(binding.name());
            if self.bindings.iter().any(|existing| existing.name == name) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one declared module binding per name",
                    span: Some(first_span),
                });
            }
            let (origin, import) = module_binding_origin(binding)?;
            let policy = module_binding_verified_policy(binding)?;
            let id = self.push_binding(ModuleBindingDescriptor {
                name: Arc::clone(&name),
                first_span,
                policy,
                origin,
                declaration: Some(binding.id()),
                import,
                function_child: None,
            })?;
            let mapping = self.by_binding.get_mut(binding.id().index()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "binding identity indexes its module mapping",
                    span: Some(first_span),
                },
            )?;
            if mapping.replace(id).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one module mapping per declared binding",
                    span: Some(first_span),
                });
            }
            self.push_need(binding.executable(), id)?;
        }
        Ok(())
    }

    fn validate_module_declaration(
        &self,
        binding: &crate::storage::BindingStorage,
        first_span: Span,
    ) -> Result<(), LeafCompilationError> {
        let owner = self.plan.executable(binding.executable()).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: binding.executable(),
            },
        )?;
        if binding.executable().index() != 0 || owner.parent().is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "module declaration belongs to the Module root",
                span: Some(first_span),
            });
        }
        Ok(())
    }

    fn collect_resolved_needs(&mut self) -> Result<(), LeafCompilationError> {
        for reference in self.plan.resolved_references() {
            let binding = self.plan.binding(reference.binding()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "resolved reference binding exists",
                    span: Some(reference.span()),
                },
            )?;
            if !matches!(
                binding.placement(),
                StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport
            ) {
                continue;
            }
            let Some(id) = self.by_binding.get(binding.id().index()).copied().flatten() else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "module reference resolves a declared module binding",
                    span: Some(reference.span()),
                });
            };
            self.push_need(reference.executable(), id)?;
        }
        Ok(())
    }

    fn push_binding(
        &mut self,
        binding: ModuleBindingDescriptor,
    ) -> Result<ModuleBindingId, LeafCompilationError> {
        let raw = u32::try_from(self.bindings.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "module binding names",
            }
        })?;
        let id = ModuleBindingId(raw);
        self.bindings.push(binding);
        Ok(id)
    }

    fn push_need(
        &mut self,
        executable: ExecutableId,
        id: ModuleBindingId,
    ) -> Result<(), LeafCompilationError> {
        self.needs
            .get_mut(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?
            .push(id);
        Ok(())
    }

    fn finish(mut self) -> Result<ModuleBindingLayout, LeafCompilationError> {
        // Propagate descendant needs up to every ancestor so a nested function
        // that reads a module binding forwards the root-owned module cell.
        for index in (0..self.needs.len()).rev() {
            self.needs[index].sort_unstable();
            self.needs[index].dedup();
            let Some(parent) = self.plan.executables()[index].parent() else {
                continue;
            };
            let inherited = self.needs[index].clone();
            self.needs
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?
                .extend(inherited);
        }

        let mut import_ranges = Vec::with_capacity(self.needs.len());
        let mut imports = Vec::new();
        for mut executable_needs in self.needs.drain(..) {
            executable_needs.sort_unstable();
            executable_needs.dedup();
            checked_function_entry_count(executable_needs.len(), "module binding slots")?;
            let start = imports.len();
            imports.extend(executable_needs);
            import_ranges.push(start..imports.len());
        }
        Ok(ModuleBindingLayout {
            bindings: self.bindings.into_boxed_slice(),
            by_binding: self.by_binding,
            import_ranges: import_ranges.into_boxed_slice(),
            imports: imports.into_boxed_slice(),
        })
    }
}

fn module_binding_origin(
    binding: &crate::storage::BindingStorage,
) -> Result<(ModuleBindingOrigin, Option<ModuleImportName>), LeafCompilationError> {
    // The storage planner does not yet retain the import-entry linkage, so the
    // origin is derived from the declaration policy. Imports and namespace
    // imports are immutable cells linked by the runtime; everything else is a
    // module-local declaration cell.
    match binding.policy().kind() {
        DeclarationKind::Import | DeclarationKind::NamespaceImport => {
            // Import linkage (request index + import name) is attached by the
            // module declaration record builder, which has access to the
            // frontend module syntax. Here we only record the origin category.
            let origin = if binding.policy().kind() == DeclarationKind::NamespaceImport {
                ModuleBindingOrigin::Namespace
            } else {
                ModuleBindingOrigin::Import
            };
            Ok((origin, None))
        }
        DeclarationKind::Var
        | DeclarationKind::Let
        | DeclarationKind::Const
        | DeclarationKind::Class
        | DeclarationKind::Function
        | DeclarationKind::SyntheticDefault => Ok((ModuleBindingOrigin::Local, None)),
        DeclarationKind::FunctionName
        | DeclarationKind::ClassName
        | DeclarationKind::ClassFieldKey
        | DeclarationKind::ClassInstanceInitializer
        | DeclarationKind::ClassPrivateName
        | DeclarationKind::ClassStaticReceiver
        | DeclarationKind::WithObject
        | DeclarationKind::Parameter
        | DeclarationKind::Catch => unsupported(
            UnsupportedLeafFeature::UnsupportedBinding,
            binding
                .declaration_spans()
                .first()
                .copied()
                .unwrap_or(Span::new(0, 0)),
        ),
    }
}

/// Maps a module binding's storage policy to the verified bytecode policy.
/// Imports and namespace imports lower as immutable `const` cells (TDZ); the
/// synthetic default cell is mutable with a TDZ; module-local declarations map
/// to their language-level kinds.
pub(in crate::lowering) fn module_binding_verified_policy(
    binding: &crate::storage::BindingStorage,
) -> Result<CompilerBindingPolicy, LeafCompilationError> {
    use fusor_bytecode::{
        CompilerBindingKind as VerifiedBindingKind,
        CompilerInitializationPolicy as VerifiedInitializationPolicy,
        CompilerWritePolicy as VerifiedWritePolicy,
    };
    let kind = binding.policy().kind();
    match kind {
        DeclarationKind::Import | DeclarationKind::NamespaceImport => {
            Ok(CompilerBindingPolicy::new(
                VerifiedBindingKind::Const,
                VerifiedInitializationPolicy::AtDeclaration,
                VerifiedWritePolicy::Immutable,
                true,
            ))
        }
        DeclarationKind::Var => Ok(CompilerBindingPolicy::new(
            VerifiedBindingKind::Var,
            VerifiedInitializationPolicy::UndefinedAtInstantiation,
            VerifiedWritePolicy::Mutable,
            false,
        )),
        DeclarationKind::Let | DeclarationKind::Class | DeclarationKind::SyntheticDefault => {
            if binding.policy().initialization() == InitializationPolicy::FunctionAtInstantiation {
                Ok(CompilerBindingPolicy::new(
                    VerifiedBindingKind::Function,
                    VerifiedInitializationPolicy::FunctionAtInstantiation,
                    VerifiedWritePolicy::Mutable,
                    false,
                ))
            } else {
                Ok(CompilerBindingPolicy::new(
                    VerifiedBindingKind::Let,
                    VerifiedInitializationPolicy::AtDeclaration,
                    VerifiedWritePolicy::Mutable,
                    true,
                ))
            }
        }
        DeclarationKind::Const => Ok(CompilerBindingPolicy::new(
            VerifiedBindingKind::Const,
            VerifiedInitializationPolicy::AtDeclaration,
            VerifiedWritePolicy::Immutable,
            true,
        )),
        DeclarationKind::Function => Ok(CompilerBindingPolicy::new(
            VerifiedBindingKind::Function,
            VerifiedInitializationPolicy::FunctionAtInstantiation,
            VerifiedWritePolicy::Mutable,
            false,
        )),
        DeclarationKind::FunctionName
        | DeclarationKind::ClassName
        | DeclarationKind::ClassFieldKey
        | DeclarationKind::ClassInstanceInitializer
        | DeclarationKind::ClassPrivateName
        | DeclarationKind::ClassStaticReceiver
        | DeclarationKind::WithObject
        | DeclarationKind::Parameter
        | DeclarationKind::Catch => unsupported(
            UnsupportedLeafFeature::UnsupportedBinding,
            binding
                .declaration_spans()
                .first()
                .copied()
                .unwrap_or(Span::new(0, 0)),
        ),
    }
}
