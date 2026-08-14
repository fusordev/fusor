use std::ops::Range;

use fusor_frontend::DirectEvalContext;
use fusor_frontend::Span;

use crate::storage::{BindingId, Executable, ExecutableId, StoragePlan};

use super::super::{
    CompiledConstantPool, LeafCompilationError, checked_function_entry_count,
    checked_function_index,
};
use super::{
    ModuleBindingLayout, ModuleBindingLayoutInput, RealmGlobalLayout, RealmGlobalLayoutInput,
};

#[derive(Clone, Copy)]
pub(in crate::lowering) struct FunctionTreeLayoutSeedInput<'plan, 'scope> {
    pub(in crate::lowering) plan: &'plan StoragePlan,
    pub(in crate::lowering) allow_realm_globals: bool,
    pub(in crate::lowering) direct_eval: Option<DirectEvalContext<'scope>>,
}

pub(in crate::lowering) struct FunctionTreeLayoutInput {
    pub(in crate::lowering) seed: FunctionTreeLayoutSeed,
    pub(in crate::lowering) constant_pools: Box<[CompiledConstantPool]>,
    pub(in crate::lowering) function_declarations: Box<[Option<ExecutableId>]>,
}

pub(in crate::lowering) struct FunctionTreeLayoutSeed {
    child_ranges: Box<[Range<usize>]>,
    children: Box<[ExecutableId]>,
    variable_references: Box<[Option<u16>]>,
    pub(in crate::lowering) realm_globals: RealmGlobalLayout,
    pub(in crate::lowering) module_bindings: ModuleBindingLayout,
}

pub(in crate::lowering) struct FunctionTreeLayout {
    child_ranges: Box<[Range<usize>]>,
    children: Box<[ExecutableId]>,
    constant_pools: Box<[CompiledConstantPool]>,
    variable_references: Box<[Option<u16>]>,
    function_declarations: Box<[Option<ExecutableId>]>,
    pub(in crate::lowering) realm_globals: RealmGlobalLayout,
    pub(in crate::lowering) module_bindings: ModuleBindingLayout,
}

struct FunctionChildLayout {
    child_ranges: Box<[Range<usize>]>,
    children: Box<[ExecutableId]>,
}

impl FunctionTreeLayoutSeed {
    pub(in crate::lowering) fn new(
        input: FunctionTreeLayoutSeedInput<'_, '_>,
    ) -> Result<Self, LeafCompilationError> {
        let FunctionTreeLayoutSeedInput {
            plan,
            allow_realm_globals,
            direct_eval,
        } = input;
        let executables = plan.executables();
        let FunctionChildLayout {
            child_ranges,
            children,
        } = Self::build_child_layout(executables)?;
        let variable_references = Self::build_variable_references(plan, executables)?;
        Ok(Self {
            child_ranges,
            children,
            variable_references,
            realm_globals: RealmGlobalLayout::new(RealmGlobalLayoutInput {
                plan,
                enabled: allow_realm_globals,
                direct_eval,
            })?,
            module_bindings: ModuleBindingLayout::new(ModuleBindingLayoutInput {
                plan,
                enabled: matches!(plan.kind(), crate::storage::CompilationUnitKind::Module),
            })?,
        })
    }

    fn build_child_layout(
        executables: &[Executable],
    ) -> Result<FunctionChildLayout, LeafCompilationError> {
        let child_counts = Self::count_children(executables)?;
        let (child_ranges, child_total) = Self::build_child_ranges(child_counts)?;
        let children = Self::populate_child_tables(executables, &child_ranges, child_total)?;
        Ok(FunctionChildLayout {
            child_ranges: child_ranges.into_boxed_slice(),
            children,
        })
    }

    fn count_children(executables: &[Executable]) -> Result<Vec<usize>, LeafCompilationError> {
        let mut child_counts = vec![0_usize; executables.len()];
        for (expected_index, executable) in executables.iter().enumerate() {
            if executable.id().index() != expected_index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "executable identities are dense and ordered",
                    span: Some(executable.span()),
                });
            }
            let Some(parent) = executable.parent() else {
                continue;
            };
            if parent.index() >= expected_index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "executable parent precedes its child",
                    span: Some(executable.span()),
                });
            }
            let count = child_counts
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            *count = count
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function constants",
                })?;
        }
        Ok(child_counts)
    }

    fn build_child_ranges(
        child_counts: Vec<usize>,
    ) -> Result<(Vec<Range<usize>>, usize), LeafCompilationError> {
        let mut child_ranges = Vec::with_capacity(child_counts.len());
        let mut child_total = 0_usize;
        for count in child_counts {
            let start = child_total;
            child_total =
                child_total
                    .checked_add(count)
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "function constants",
                    })?;
            child_ranges.push(start..child_total);
        }
        Ok((child_ranges, child_total))
    }

    fn populate_child_tables(
        executables: &[Executable],
        child_ranges: &[Range<usize>],
        child_total: usize,
    ) -> Result<Box<[ExecutableId]>, LeafCompilationError> {
        let mut children = vec![None; child_total];
        let mut child_cursors = child_ranges
            .iter()
            .map(|range| range.start)
            .collect::<Vec<_>>();
        for executable in executables {
            let Some(parent) = executable.parent() else {
                continue;
            };
            let cursor = child_cursors
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            let range = child_ranges
                .get(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            if !range.contains(cursor) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "child cursor remains inside its parent range",
                    span: Some(executable.span()),
                });
            }
            let target =
                children
                    .get_mut(*cursor)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "child cursor indexes the flat child table",
                        span: Some(executable.span()),
                    })?;
            if target.replace(executable.id()).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "child executable has one flat table position",
                    span: Some(executable.span()),
                });
            }
            *cursor = cursor
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function constants",
                })?;
        }
        let children = children
            .into_iter()
            .enumerate()
            .map(|(index, child)| {
                child.ok_or_else(|| LeafCompilationError::SemanticInvariant {
                    invariant: "flat child table is completely populated",
                    span: Self::child_owner_span(executables, child_ranges, index),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(children.into_boxed_slice())
    }

    fn child_owner_span(
        executables: &[Executable],
        child_ranges: &[Range<usize>],
        child_index: usize,
    ) -> Option<Span> {
        let owner_index = child_ranges.partition_point(|range| range.end <= child_index);
        child_ranges
            .get(owner_index)
            .filter(|range| range.contains(&child_index))?;
        executables.get(owner_index).map(Executable::span)
    }

    fn build_variable_references(
        plan: &StoragePlan,
        executables: &[Executable],
    ) -> Result<Box<[Option<u16>]>, LeafCompilationError> {
        let mut variable_references = vec![None; plan.bindings().len()];
        for executable in executables {
            let bindings = plan.bindings_for(executable.id()).ok_or(
                LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                },
            )?;
            let mut capture_count = 0_usize;
            for binding in bindings {
                if !binding.is_frame_captured() {
                    continue;
                }
                let index = checked_function_index(capture_count, "function variable references")?;
                let slot = variable_references.get_mut(binding.id().index()).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured binding indexes variable-reference layout",
                        span: binding.declaration_spans().first().copied(),
                    },
                )?;
                if slot.replace(index).is_some() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "captured binding has one variable-reference index",
                        span: binding.declaration_spans().first().copied(),
                    });
                }
                capture_count =
                    capture_count
                        .checked_add(1)
                        .ok_or(LeafCompilationError::CapacityExceeded {
                            domain: "function variable references",
                        })?;
            }
            checked_function_entry_count(capture_count, "function variable references")?;
        }
        Ok(variable_references.into_boxed_slice())
    }

    pub(in crate::lowering) fn children(
        &self,
        executable: ExecutableId,
    ) -> Result<&[ExecutableId], LeafCompilationError> {
        children_for(&self.child_ranges, &self.children, executable)
    }

    #[cfg(test)]
    fn variable_reference(&self, binding: BindingId) -> Option<u16> {
        self.variable_references
            .get(binding.index())
            .copied()
            .flatten()
    }

    #[cfg(test)]
    fn subtree_preorder(
        &self,
        root: ExecutableId,
    ) -> Result<Vec<ExecutableId>, LeafCompilationError> {
        subtree_preorder_for(&self.child_ranges, &self.children, root)
    }
}

impl FunctionTreeLayout {
    pub(in crate::lowering) fn new(
        input: FunctionTreeLayoutInput,
    ) -> Result<Self, LeafCompilationError> {
        let FunctionTreeLayoutInput {
            seed,
            constant_pools,
            function_declarations,
        } = input;
        if constant_pools.len() != seed.child_ranges.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "constant pools cover every executable",
                span: None,
            });
        }
        if function_declarations.len() != seed.variable_references.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function declaration layout covers every compiler binding",
                span: None,
            });
        }
        Ok(Self {
            child_ranges: seed.child_ranges,
            children: seed.children,
            constant_pools,
            variable_references: seed.variable_references,
            function_declarations,
            realm_globals: seed.realm_globals,
            module_bindings: seed.module_bindings,
        })
    }

    pub(in crate::lowering) fn children(
        &self,
        executable: ExecutableId,
    ) -> Result<&[ExecutableId], LeafCompilationError> {
        children_for(&self.child_ranges, &self.children, executable)
    }

    pub(in crate::lowering) fn constant_pool(
        &self,
        executable: ExecutableId,
    ) -> Result<&CompiledConstantPool, LeafCompilationError> {
        self.constant_pools
            .get(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })
    }

    pub(in crate::lowering) fn variable_reference(&self, binding: BindingId) -> Option<u16> {
        self.variable_references
            .get(binding.index())
            .copied()
            .flatten()
    }

    pub(in crate::lowering) fn function_declaration(
        &self,
        binding: BindingId,
    ) -> Option<ExecutableId> {
        self.function_declarations
            .get(binding.index())
            .copied()
            .flatten()
    }

    pub(in crate::lowering) fn subtree_preorder(
        &self,
        root: ExecutableId,
    ) -> Result<Vec<ExecutableId>, LeafCompilationError> {
        subtree_preorder_for(&self.child_ranges, &self.children, root)
    }
}

fn subtree_preorder_for(
    child_ranges: &[Range<usize>],
    children: &[ExecutableId],
    root: ExecutableId,
) -> Result<Vec<ExecutableId>, LeafCompilationError> {
    children_for(child_ranges, children, root)?;
    let mut visited = vec![false; child_ranges.len()];
    let mut preorder = Vec::new();
    let mut work = vec![root];
    while let Some(executable) = work.pop() {
        let seen = visited
            .get_mut(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if std::mem::replace(seen, true) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function subtree has one acyclic parent path",
                span: None,
            });
        }
        preorder.push(executable);
        for child in children_for(child_ranges, children, executable)?
            .iter()
            .rev()
        {
            work.push(*child);
        }
    }
    Ok(preorder)
}

fn children_for<'layout>(
    child_ranges: &'layout [Range<usize>],
    children: &'layout [ExecutableId],
    executable: ExecutableId,
) -> Result<&'layout [ExecutableId], LeafCompilationError> {
    let range = child_ranges
        .get(executable.index())
        .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
    children
        .get(range.clone())
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "executable child range indexes the flat child table",
            span: None,
        })
}

#[cfg(test)]
mod tests {
    use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

    use crate::lowering::CompilationContext;

    use super::{FunctionTreeLayoutSeed, FunctionTreeLayoutSeedInput};

    #[test]
    fn seed_owns_preorder_edges_and_dense_variable_reference_indices() {
        let source = "function outer(){ let captured=1; function first(){ return captured; } \
                      function second(){ function leaf(){ return captured; } return leaf; } }";
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let plan = context.storage_plan();
                let seed = FunctionTreeLayoutSeed::new(FunctionTreeLayoutSeedInput {
                    plan,
                    allow_realm_globals: false,
                    direct_eval: None,
                })
                .expect("function tree seed");
                let named = |name| {
                    plan.executables()
                        .iter()
                        .find(|executable| executable.name() == Some(name))
                        .expect("named executable")
                        .id()
                };
                let outer = named("outer");
                let first = named("first");
                let second = named("second");
                let leaf = named("leaf");

                assert_eq!(
                    seed.children(outer).expect("outer children"),
                    [first, second]
                );
                assert_eq!(seed.children(second).expect("second children"), [leaf]);
                assert!(seed.children(first).expect("first children").is_empty());
                assert_eq!(
                    seed.subtree_preorder(outer).expect("outer preorder"),
                    [outer, first, second, leaf]
                );

                let captured = plan
                    .bindings_for(outer)
                    .expect("outer bindings")
                    .iter()
                    .find(|binding| binding.name() == "captured")
                    .expect("captured binding");
                assert_eq!(seed.variable_reference(captured.id()), Some(0));
            },
        )
        .expect("front-end acceptance");
    }
}
