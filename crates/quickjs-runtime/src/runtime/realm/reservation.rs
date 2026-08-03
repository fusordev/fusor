//! Exact resource admission for one Realm construction transaction.

use super::{
    Runtime, RuntimeError, RuntimeResource, allocation_failed, atoms::RealmAtomPlan, check_limit,
    families::RealmFunctionSchema, usize_to_u64,
};

const REALMS_PER_REALM: usize = 1;
const PROPERTIES_PER_REALM: u64 = 718;

/// Single source of truth for Realm construction's bounded resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RealmReservationPlan {
    dynamic_atoms: usize,
    dynamic_atom_code_units: usize,
    realms: usize,
    objects: usize,
    functions: usize,
    object_properties: u64,
    journal_entries: usize,
}

impl RealmReservationPlan {
    pub(super) fn try_new(
        atom_plan: &RealmAtomPlan<'_>,
        schema: &RealmFunctionSchema,
    ) -> Result<Self, RuntimeError> {
        let objects = schema.object_count();
        let functions = schema.function_count();
        let journal_entries = atom_plan
            .len()
            .checked_add(REALMS_PER_REALM)
            .and_then(|value| value.checked_add(objects))
            .and_then(|value| value.checked_add(functions))
            .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
        Ok(Self {
            dynamic_atoms: atom_plan.len(),
            dynamic_atom_code_units: atom_plan.description_code_units(),
            realms: REALMS_PER_REALM,
            objects,
            functions,
            object_properties: PROPERTIES_PER_REALM,
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
            .map_err(|_| allocation_failed(RuntimeResource::HeapFunctions, self.functions))
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
    use crate::runtime::realm::RealmNames;

    #[test]
    fn reservation_plan_matches_the_characterized_realm_delta() {
        let names = RealmNames::try_new().expect("Realm names");
        let atoms = RealmAtomPlan::try_new(&names).expect("atom plan");
        let schema = RealmFunctionSchema::try_new().expect("function schema");
        let plan = RealmReservationPlan::try_new(&atoms, &schema).expect("reservation plan");

        assert_eq!(plan.dynamic_atoms, 159);
        assert_eq!(plan.dynamic_atom_code_units, 1_232);
        assert_eq!(plan.realms, 1);
        assert_eq!(plan.objects, 23);
        assert_eq!(plan.functions, 219);
        assert_eq!(plan.object_properties, 718);
        assert_eq!(plan.journal_entries, 402);
    }
}
