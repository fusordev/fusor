use crate::storage::{BindingId, ExecutableId, StoragePlacement, StoragePlan};

use super::super::{
    LeafCompilationError, LocalSlot, checked_function_entry_count, checked_function_index,
};

#[derive(Clone, Copy)]
pub(in crate::lowering) struct FrameLayoutInput<'plan> {
    pub(in crate::lowering) plan: &'plan StoragePlan,
    pub(in crate::lowering) executable: ExecutableId,
    pub(in crate::lowering) internal_local_count: usize,
}

impl<'plan> FrameLayoutInput<'plan> {
    pub(in crate::lowering) const fn new(
        plan: &'plan StoragePlan,
        executable: ExecutableId,
    ) -> Self {
        Self {
            plan,
            executable,
            internal_local_count: 0,
        }
    }

    pub(in crate::lowering) const fn with_internal_locals(mut self, count: usize) -> Self {
        self.internal_local_count = count;
        self
    }
}

#[derive(Clone, Copy)]
pub(in crate::lowering) struct ArgumentSlot(pub(in crate::lowering) u16);

#[derive(Clone, Copy)]
pub(in crate::lowering) enum FrameSlot {
    Argument(ArgumentSlot),
    Local(LocalSlot),
    Capture(u16),
}

pub(in crate::lowering) struct FrameLocal {
    pub(in crate::lowering) binding: BindingId,
    pub(in crate::lowering) slot: LocalSlot,
}

#[derive(Clone, Copy)]
struct FrameBindingSlot {
    binding: BindingId,
    slot: FrameSlot,
}

pub(in crate::lowering) struct FrameLayout {
    pub(in crate::lowering) executable: ExecutableId,
    slots: Box<[FrameBindingSlot]>,
    pub(in crate::lowering) locals: Box<[FrameLocal]>,
    internal_locals: Box<[LocalSlot]>,
    pub(in crate::lowering) local_count: u32,
}

impl FrameLayout {
    pub(in crate::lowering) fn new(
        input: FrameLayoutInput<'_>,
    ) -> Result<Self, LeafCompilationError> {
        let FrameLayoutInput {
            plan,
            executable,
            internal_local_count,
        } = input;
        let bindings = plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let captures = plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let slot_capacity = bindings.len().checked_add(captures.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "function frame bindings",
            },
        )?;
        let mut slots = Vec::with_capacity(slot_capacity);
        let mut locals = Vec::new();
        let mut local_count = 0_u32;
        let executable_metadata = plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        checked_function_entry_count(
            executable_metadata.parameter_count(),
            "function argument slots",
        )?;
        for binding in bindings {
            let slot = match binding.placement() {
                StoragePlacement::Argument { parameter_index } => {
                    let parameter_index =
                        checked_function_index(parameter_index, "function argument slots")?;
                    Some(FrameSlot::Argument(ArgumentSlot(parameter_index)))
                }
                StoragePlacement::Local => {
                    let slot =
                        LocalSlot(checked_function_index(local_count, "function local slots")?);
                    local_count += 1;
                    locals.push(FrameLocal {
                        binding: binding.id(),
                        slot,
                    });
                    Some(FrameSlot::Local(slot))
                }
                StoragePlacement::GlobalObject
                | StoragePlacement::GlobalLexical
                | StoragePlacement::ModuleLocal
                | StoragePlacement::ModuleImport => None,
            };
            let Some(slot) = slot else {
                continue;
            };
            slots.push(FrameBindingSlot {
                binding: binding.id(),
                slot,
            });
        }
        checked_function_entry_count(captures.len(), "function capture slots")?;
        for (expected_capture_index, capture) in captures.iter().enumerate() {
            let capture_index =
                checked_function_index(capture.slot().index(), "function capture slots")?;
            if capture.slot().index() != expected_capture_index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "function capture slots are dense and ordered",
                    span: plan
                        .binding(capture.binding())
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
            slots.push(FrameBindingSlot {
                binding: capture.binding(),
                slot: FrameSlot::Capture(capture_index),
            });
        }
        slots.sort_unstable_by_key(|entry| entry.binding);
        for duplicate in slots.windows(2) {
            if duplicate[0].binding == duplicate[1].binding {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one frame or capture slot per compiler binding",
                    span: plan
                        .binding(duplicate[0].binding)
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
        }

        let (internal_locals, local_count) =
            build_internal_locals(local_count, internal_local_count)?;

        Ok(Self {
            executable,
            slots: slots.into_boxed_slice(),
            locals: locals.into_boxed_slice(),
            internal_locals,
            local_count,
        })
    }

    pub(in crate::lowering) fn internal_local(
        &self,
        index: usize,
    ) -> Result<LocalSlot, LeafCompilationError> {
        self.internal_locals
            .get(index)
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "internal local index belongs to the immutable frame layout",
                span: None,
            })
    }

    pub(in crate::lowering) fn slot(&self, binding: BindingId) -> Option<FrameSlot> {
        let index = self
            .slots
            .binary_search_by_key(&binding, |entry| entry.binding)
            .ok()?;
        Some(self.slots[index].slot)
    }
}

fn build_internal_locals(
    mut local_count: u32,
    internal_local_count: usize,
) -> Result<(Box<[LocalSlot]>, u32), LeafCompilationError> {
    let mut internal_locals = Vec::with_capacity(internal_local_count);
    for _ in 0..internal_local_count {
        let slot = LocalSlot(checked_function_index(local_count, "function local slots")?);
        local_count = local_count
            .checked_add(1)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "function local slots",
            })?;
        checked_function_entry_count(local_count, "function local slots")?;
        internal_locals.push(slot);
    }
    Ok((internal_locals.into_boxed_slice(), local_count))
}

#[cfg(test)]
mod tests {
    use quickjs_frontend::{
        CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
    };

    use crate::lowering::CompilationContext;

    use super::{FrameLayout, FrameLayoutInput, FrameSlot};

    #[test]
    fn construction_freezes_argument_local_capture_and_internal_domains() {
        let source = "function outer(argument){ let local=argument; \
                      function child(){ return local; } return child; }";
        with_parsed_program(
            source,
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
                let binding = |owner, name| {
                    plan.bindings_for(owner)
                        .expect("owner bindings")
                        .iter()
                        .find(|binding| binding.name() == name)
                        .expect("named binding")
                        .id()
                };
                let argument = binding(outer, "argument");
                let local = binding(outer, "local");

                let outer_layout = FrameLayout::new(FrameLayoutInput {
                    plan,
                    executable: outer,
                    internal_local_count: 1,
                })
                .expect("outer frame layout");
                assert!(matches!(
                    outer_layout.slot(argument),
                    Some(FrameSlot::Argument(slot)) if slot.0 == 0
                ));
                assert!(matches!(
                    outer_layout.slot(local),
                    Some(FrameSlot::Local(slot)) if slot.index() == 0
                ));
                assert_eq!(
                    outer_layout
                        .internal_local(0)
                        .expect("internal local")
                        .index(),
                    u16::try_from(outer_layout.locals.len()).expect("test local count fits u16")
                );
                assert_eq!(
                    outer_layout.local_count,
                    u32::try_from(outer_layout.locals.len() + 1)
                        .expect("test local count fits u32")
                );

                let child_layout = FrameLayout::new(FrameLayoutInput {
                    plan,
                    executable: child,
                    internal_local_count: 0,
                })
                .expect("child frame layout");
                assert!(matches!(
                    child_layout.slot(local),
                    Some(FrameSlot::Capture(0))
                ));
                assert!(child_layout.internal_local(0).is_err());
            },
        )
        .expect("front-end acceptance");
    }
}
