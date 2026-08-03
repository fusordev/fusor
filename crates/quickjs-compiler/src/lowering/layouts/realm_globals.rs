use std::{collections::HashMap, ops::Range, sync::Arc};

use quickjs_bytecode::CompilerBindingPolicy;
use quickjs_frontend::Span;

use crate::storage::{
    BindingId, DeclarationKind, ExecutableId, InitializationPolicy, StoragePlacement, StoragePlan,
    UnresolvedGlobalId, WritePolicy,
};

use super::super::{
    LeafCompilationError, RealmGlobalId, UnsupportedLeafFeature, checked_function_entry_count,
    checked_function_index, constructor_realm_lookup_policy, unsupported, verified_storage_policy,
};

#[derive(Clone, Copy)]
pub(in crate::lowering) struct RealmGlobalLayoutInput<'plan> {
    pub(in crate::lowering) plan: &'plan StoragePlan,
    pub(in crate::lowering) enabled: bool,
}

pub(in crate::lowering) struct RealmGlobalBinding {
    pub(in crate::lowering) name: Arc<str>,
    pub(in crate::lowering) first_span: Span,
    pub(in crate::lowering) policy: CompilerBindingPolicy,
    pub(in crate::lowering) declaration: Option<BindingId>,
}

pub(in crate::lowering) struct RealmGlobalLayout {
    bindings: Box<[RealmGlobalBinding]>,
    by_binding: Box<[Option<RealmGlobalId>]>,
    by_unresolved: Box<[Option<RealmGlobalId>]>,
    import_ranges: Box<[Range<usize>]>,
    imports: Box<[RealmGlobalId]>,
}

struct RealmGlobalLayoutBuilder<'plan> {
    plan: &'plan StoragePlan,
    bindings: Vec<RealmGlobalBinding>,
    by_name: HashMap<Arc<str>, RealmGlobalId>,
    by_binding: Vec<Option<RealmGlobalId>>,
    by_unresolved: Vec<Option<RealmGlobalId>>,
    needs: Vec<Vec<RealmGlobalId>>,
}

impl<'plan> RealmGlobalLayoutBuilder<'plan> {
    fn new(input: RealmGlobalLayoutInput<'plan>) -> Self {
        let plan = input.plan;
        Self {
            plan,
            bindings: Vec::new(),
            by_name: HashMap::new(),
            by_binding: vec![None; plan.bindings().len()],
            by_unresolved: vec![None; plan.unresolved_globals().len()],
            needs: (0..plan.executables().len()).map(|_| Vec::new()).collect(),
        }
    }

    fn collect_declarations(&mut self) -> Result<(), LeafCompilationError> {
        for binding in self.plan.bindings() {
            match binding.placement() {
                StoragePlacement::GlobalObject => self.collect_declaration(binding)?,
                StoragePlacement::GlobalLexical => {
                    return unsupported(
                        UnsupportedLeafFeature::GlobalEnvironment,
                        binding
                            .declaration_spans()
                            .first()
                            .copied()
                            .unwrap_or_default(),
                    );
                }
                StoragePlacement::Argument { .. }
                | StoragePlacement::Local
                | StoragePlacement::ModuleLocal
                | StoragePlacement::ModuleImport => {}
            }
        }
        Ok(())
    }

    fn collect_declaration(
        &mut self,
        binding: &crate::storage::BindingStorage,
    ) -> Result<(), LeafCompilationError> {
        let first_span = binding.declaration_spans().first().copied().ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm declaration retains a source span",
                span: None,
            },
        )?;
        let supported_policy = matches!(
            (binding.policy().kind(), binding.policy().initialization()),
            (
                DeclarationKind::Var,
                InitializationPolicy::UndefinedAtInstantiation
            ) | (
                DeclarationKind::Function,
                InitializationPolicy::FunctionAtInstantiation
            )
        );
        if !supported_policy
            || binding.policy().writes() != WritePolicy::Mutable
            || binding.policy().has_temporal_dead_zone()
        {
            return unsupported(UnsupportedLeafFeature::GlobalEnvironment, first_span);
        }
        let owner = self.plan.executable(binding.executable()).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: binding.executable(),
            },
        )?;
        if binding.executable().index() != 0 || owner.parent().is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm declaration belongs to the dynamic Script root",
                span: Some(first_span),
            });
        }

        let name: Arc<str> = Arc::from(binding.name());
        if self.by_name.contains_key(&name) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "one declared constructor-realm binding per name",
                span: Some(first_span),
            });
        }
        let id = self.push_binding(RealmGlobalBinding {
            name: Arc::clone(&name),
            first_span,
            policy: verified_storage_policy(binding)?,
            declaration: Some(binding.id()),
        })?;
        self.by_name.insert(name, id);
        let mapping = self.by_binding.get_mut(binding.id().index()).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "binding identity indexes its realm-global mapping",
                span: Some(first_span),
            },
        )?;
        if mapping.replace(id).is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "one realm-global mapping per declared binding",
                span: Some(first_span),
            });
        }
        self.push_need(binding.executable(), id)
    }

    fn collect_unresolved(&mut self) -> Result<(), LeafCompilationError> {
        for reference in self.plan.unresolved_globals() {
            let name: Arc<str> = Arc::from(reference.name());
            let id = if let Some(&id) = self.by_name.get(&name) {
                id
            } else {
                let id = self.push_binding(RealmGlobalBinding {
                    name: Arc::clone(&name),
                    first_span: reference.span(),
                    policy: constructor_realm_lookup_policy(),
                    declaration: None,
                })?;
                self.by_name.insert(name, id);
                id
            };
            let mapping = self.by_unresolved.get_mut(reference.id().index()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "unresolved global identity indexes its realm-global mapping",
                    span: Some(reference.span()),
                },
            )?;
            if mapping.replace(id).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one realm-global mapping per unresolved reference",
                    span: Some(reference.span()),
                });
            }
            self.push_need(reference.executable(), id)?;
        }
        Ok(())
    }

    fn collect_resolved_needs(&mut self) -> Result<(), LeafCompilationError> {
        for reference in self.plan.resolved_references() {
            let Some(global) = self
                .by_binding
                .get(reference.binding().index())
                .copied()
                .flatten()
            else {
                continue;
            };
            self.push_need(reference.executable(), global)?;
        }
        Ok(())
    }

    fn push_binding(
        &mut self,
        binding: RealmGlobalBinding,
    ) -> Result<RealmGlobalId, LeafCompilationError> {
        let raw = u32::try_from(self.bindings.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "constructor-realm global names",
            }
        })?;
        let id = RealmGlobalId(raw);
        self.bindings.push(binding);
        Ok(id)
    }

    fn push_need(
        &mut self,
        executable: ExecutableId,
        global: RealmGlobalId,
    ) -> Result<(), LeafCompilationError> {
        self.needs
            .get_mut(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?
            .push(global);
        Ok(())
    }

    fn finish(mut self) -> Result<RealmGlobalLayout, LeafCompilationError> {
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
        for mut executable_needs in self.needs {
            executable_needs.sort_unstable();
            executable_needs.dedup();
            checked_function_entry_count(executable_needs.len(), "constructor-realm global slots")?;
            let start = imports.len();
            imports.extend(executable_needs);
            import_ranges.push(start..imports.len());
        }
        Ok(RealmGlobalLayout {
            bindings: self.bindings.into_boxed_slice(),
            by_binding: self.by_binding.into_boxed_slice(),
            by_unresolved: self.by_unresolved.into_boxed_slice(),
            import_ranges: import_ranges.into_boxed_slice(),
            imports: imports.into_boxed_slice(),
        })
    }
}

impl RealmGlobalLayout {
    pub(in crate::lowering) fn new(
        input: RealmGlobalLayoutInput<'_>,
    ) -> Result<Self, LeafCompilationError> {
        let plan = input.plan;
        if !input.enabled {
            return Ok(Self {
                bindings: Box::default(),
                by_binding: vec![None; plan.bindings().len()].into_boxed_slice(),
                by_unresolved: vec![None; plan.unresolved_globals().len()].into_boxed_slice(),
                import_ranges: vec![0..0; plan.executables().len()].into_boxed_slice(),
                imports: Box::default(),
            });
        }

        let mut builder = RealmGlobalLayoutBuilder::new(input);
        builder.collect_declarations()?;
        builder.collect_unresolved()?;
        builder.collect_resolved_needs()?;
        builder.finish()
    }

    pub(in crate::lowering) fn binding(&self, id: RealmGlobalId) -> Option<&RealmGlobalBinding> {
        self.bindings.get(id.index())
    }

    pub(in crate::lowering) fn for_unresolved(
        &self,
        id: UnresolvedGlobalId,
    ) -> Option<RealmGlobalId> {
        self.by_unresolved.get(id.index()).copied().flatten()
    }

    pub(in crate::lowering) fn for_binding(&self, id: BindingId) -> Option<RealmGlobalId> {
        self.by_binding.get(id.index()).copied().flatten()
    }

    pub(in crate::lowering) fn imports_for(
        &self,
        executable: ExecutableId,
    ) -> Result<&[RealmGlobalId], LeafCompilationError> {
        let range = self
            .import_ranges
            .get(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        self.imports
            .get(range.clone())
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm global range indexes flat imports",
                span: None,
            })
    }

    pub(in crate::lowering) fn closure_slot(
        &self,
        plan: &StoragePlan,
        executable: ExecutableId,
        global: RealmGlobalId,
    ) -> Result<u16, LeafCompilationError> {
        let captures = plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let imports = self.imports_for(executable)?;
        let offset = imports.binary_search(&global).map_err(|_| {
            LeafCompilationError::SemanticInvariant {
                invariant: "referenced constructor-realm global is imported by its executable",
                span: self.binding(global).map(|binding| binding.first_span),
            }
        })?;
        let index =
            captures
                .len()
                .checked_add(offset)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function closure variables",
                })?;
        checked_function_index(index, "function closure variables")
    }
}

#[cfg(test)]
mod tests {
    use quickjs_frontend::{
        DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
        with_dynamic_function_source,
    };

    use crate::lowering::{CompilationContext, RealmGlobalId};

    use super::{RealmGlobalLayout, RealmGlobalLayoutInput};

    #[test]
    fn layout_deduplicates_names_and_propagates_child_needs_to_every_parent() {
        let source = DynamicFunctionSource::new(
            DynamicFunctionKind::Function,
            &[],
            SourceFragment::new(
                "var declared; realmRead; function nested(){ return realmRead; } return nested;",
            ),
        );
        with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
            let context = CompilationContext::new(unit).expect("dynamic storage plan");
            let plan = context.storage_plan();
            let layout = RealmGlobalLayout::new(RealmGlobalLayoutInput {
                plan,
                enabled: true,
            })
            .expect("realm global layout");
            let realm_read_index = layout
                .bindings
                .iter()
                .position(|binding| binding.name.as_ref() == "realmRead")
                .expect("realmRead descriptor");
            assert_eq!(
                layout
                    .bindings
                    .iter()
                    .filter(|binding| binding.name.as_ref() == "realmRead")
                    .count(),
                1
            );
            let realm_read = RealmGlobalId(
                u32::try_from(realm_read_index).expect("test realm-global index fits u32"),
            );
            let nested = plan
                .executables()
                .iter()
                .find(|executable| executable.name() == Some("nested"))
                .expect("nested executable")
                .id();

            let mut current = Some(nested);
            while let Some(executable) = current {
                assert!(
                    layout
                        .imports_for(executable)
                        .expect("executable imports")
                        .contains(&realm_read),
                    "realm-global need must propagate through executable {executable:?}"
                );
                current = plan
                    .executable(executable)
                    .expect("executable metadata")
                    .parent();
            }
        })
        .expect("dynamic front-end acceptance");
    }
}
