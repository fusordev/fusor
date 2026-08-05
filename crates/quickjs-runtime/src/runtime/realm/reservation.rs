//! Exact resource admission for one Realm construction transaction.

use super::{
    Runtime, RuntimeError, RuntimeResource, allocation_failed, atoms::RealmAtomPlan, check_limit,
    families::RealmIntrinsicSchema, usize_to_u64,
};

const REALMS_PER_REALM: usize = 1;
const GLOBAL_BINDINGS_PER_REALM: usize = 0;

/// Single source of truth for Realm construction's bounded resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RealmReservationPlan {
    dynamic_atoms: usize,
    dynamic_atom_code_units: usize,
    realms: usize,
    objects: usize,
    functions: usize,
    global_bindings: usize,
    object_properties: u64,
    journal_entries: usize,
}

impl RealmReservationPlan {
    pub(super) fn try_new(
        atom_plan: &RealmAtomPlan,
        schema: &RealmIntrinsicSchema,
    ) -> Result<Self, RuntimeError> {
        let objects = schema.object_count();
        let functions = schema.function_count();
        let automatic_identity_properties = schema
            .specs()
            .iter()
            .filter(|function| function.identity_publication.is_automatic())
            .count()
            .checked_mul(2)
            .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
        let object_properties = schema
            .properties()
            .len()
            .checked_add(automatic_identity_properties)
            .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
        let object_properties = u64::try_from(object_properties)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
        let journal_entries = atom_plan
            .len()
            .checked_add(REALMS_PER_REALM)
            .and_then(|value| value.checked_add(objects))
            .and_then(|value| value.checked_add(functions))
            .and_then(|value| value.checked_add(GLOBAL_BINDINGS_PER_REALM))
            .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
        Ok(Self {
            dynamic_atoms: atom_plan.len(),
            dynamic_atom_code_units: atom_plan.description_code_units(),
            realms: REALMS_PER_REALM,
            objects,
            functions,
            global_bindings: GLOBAL_BINDINGS_PER_REALM,
            object_properties,
            journal_entries,
        })
    }

    pub(super) fn preflight_and_reserve(self, runtime: &mut Runtime) -> Result<(), RuntimeError> {
        check_limit(
            RuntimeResource::Realms,
            runtime.limits.max_realms,
            usize_to_u64(runtime.realms.len()).saturating_add(usize_to_u64(self.realms)),
        )?;
        check_limit(
            RuntimeResource::HeapObjects,
            runtime.limits.max_heap_objects,
            usize_to_u64(runtime.objects.len()).saturating_add(usize_to_u64(self.objects)),
        )?;
        check_limit(
            RuntimeResource::HeapFunctions,
            runtime.limits.max_heap_functions,
            usize_to_u64(runtime.functions.len()).saturating_add(usize_to_u64(self.functions)),
        )?;
        check_limit(
            RuntimeResource::RealmGlobalBindings,
            runtime.limits.max_realm_global_bindings,
            usize_to_u64(runtime.global_bindings.len())
                .saturating_add(usize_to_u64(self.global_bindings)),
        )?;
        check_limit(
            RuntimeResource::ObjectProperties,
            runtime.limits.max_object_properties,
            runtime
                .object_properties
                .saturating_add(self.object_properties),
        )?;
        runtime
            .realms
            .try_reserve(self.realms)
            .map_err(|_| allocation_failed(RuntimeResource::Realms, self.realms))?;
        runtime
            .objects
            .try_reserve(self.objects)
            .map_err(|_| allocation_failed(RuntimeResource::HeapObjects, self.objects))?;
        runtime
            .functions
            .try_reserve(self.functions)
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, self.functions))?;
        runtime
            .global_bindings
            .try_reserve(self.global_bindings)
            .map_err(|_| {
                allocation_failed(RuntimeResource::RealmGlobalBindings, self.global_bindings)
            })
    }

    pub(super) const fn object_properties(self) -> u64 {
        self.object_properties
    }

    pub(super) const fn journal_entries(self) -> usize {
        self.journal_entries
    }

    pub(super) const fn objects(self) -> usize {
        self.objects
    }

    pub(super) const fn functions(self) -> usize {
        self.functions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_plan_matches_the_characterized_realm_delta() {
        let schema = RealmIntrinsicSchema::try_new().expect("function schema");
        let atoms = RealmAtomPlan::try_new(&schema).expect("atom plan");
        let plan = RealmReservationPlan::try_new(&atoms, &schema).expect("reservation plan");

        assert_eq!(plan.dynamic_atoms, 203);
        assert_eq!(plan.dynamic_atom_code_units, 1_598);
        assert_eq!(plan.realms, 1);
        assert_eq!(plan.objects, 40);
        assert_eq!(plan.functions, 340);
        assert_eq!(plan.global_bindings, 0);
        assert_eq!(plan.object_properties, 1_161);
        assert_eq!(plan.journal_entries, 584);
    }
}
