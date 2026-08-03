//! Ordered Realm-local atom planning and typed bindings.

use crate::Atom;

use super::{
    ARRAY_CALLBACK_METHODS, ARRAY_COPIER_METHODS, ARRAY_FLATTEN_METHODS, ARRAY_MUTATOR_METHODS,
    ARRAY_REDUCTION_METHODS, ARRAY_SEARCH_METHODS, ARRAY_SORT_METHODS, AtomError, AtomTable,
    BIGINT_INTERNED_STATICS, DYNAMIC_SYMBOL_STATIC_PROPERTIES, JsString, MATH_CONSTANTS,
    MathMethod, NUMBER_FORMAT_METHODS, NUMBER_PREDICATE_STATICS, NUMBER_VALUE_STATICS,
    NativeFunctionKind, OBJECT_PROTOTYPE_REFLECTION, OBJECT_STATIC_METHODS, RealmNames, Runtime,
    RuntimeError, RuntimeResource, STRING_FROM_STATICS, STRING_PROTOTYPE_METHODS, URI_FUNCTIONS,
    allocation_failed, schema::RealmNameId,
};

#[derive(Clone, Copy)]
enum RealmAtomName<'a> {
    Existing(&'a JsString),
    Literal(&'static str),
}

impl RealmAtomName<'_> {
    fn description_code_units(self) -> usize {
        match self {
            Self::Existing(name) => {
                usize::try_from(name.len()).expect("u32 string lengths fit usize")
            }
            Self::Literal(name) => name.encode_utf16().count(),
        }
    }

    fn same_description(self, other: Self) -> bool {
        match (self, other) {
            (Self::Existing(left), Self::Existing(right)) => left == right,
            (Self::Literal(left), Self::Literal(right)) => left == right,
            (Self::Existing(left), Self::Literal(right))
            | (Self::Literal(right), Self::Existing(left)) => {
                left.code_units().eq(right.encode_utf16())
            }
        }
    }
}

#[derive(Clone, Copy)]
struct RealmAtomSpec<'a> {
    id: RealmNameId,
    name: RealmAtomName<'a>,
}

/// The exact ordered set of strings one Realm creation interns.
pub(super) struct RealmAtomPlan<'a> {
    entries: Vec<RealmAtomSpec<'a>>,
    bindings: Vec<(RealmNameId, usize)>,
    description_code_units: usize,
}

impl<'a> RealmAtomPlan<'a> {
    pub(super) fn try_new(names: &'a RealmNames) -> Result<Self, RuntimeError> {
        let mut declaration_count = 0_usize;
        visit_realm_atom_specs(names, |_| {
            declaration_count = declaration_count
                .checked_add(1)
                .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
            Ok(())
        })?;

        let mut entries = Vec::new();
        entries
            .try_reserve_exact(declaration_count)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, declaration_count))?;
        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(declaration_count)
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, declaration_count))?;
        let mut description_code_units = 0_usize;
        visit_realm_atom_specs(names, |spec| {
            debug_assert!(
                bindings
                    .iter()
                    .all(|(id, _): &(RealmNameId, usize)| *id != spec.id)
            );
            let index = entries
                .iter()
                .position(|entry: &RealmAtomSpec<'_>| entry.name.same_description(spec.name));
            let index = if let Some(index) = index {
                index
            } else {
                description_code_units = description_code_units
                    .checked_add(spec.name.description_code_units())
                    .ok_or_else(|| {
                        allocation_failed(RuntimeResource::ObjectProperties, usize::MAX)
                    })?;
                entries.push(spec);
                entries.len() - 1
            };
            bindings.push((spec.id, index));
            Ok(())
        })?;
        debug_assert_eq!(bindings.len(), declaration_count);

        Ok(Self {
            entries,
            bindings,
            description_code_units,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) const fn description_code_units(&self) -> usize {
        self.description_code_units
    }
}

/// Runtime-local atoms resolved by stable schema names rather than offsets.
pub(super) struct RealmAtomBindings {
    atoms: Vec<Atom>,
    bindings: Vec<(RealmNameId, usize)>,
}

impl RealmAtomBindings {
    pub(super) fn atom(&self, id: RealmNameId) -> &Atom {
        let index = self
            .bindings
            .iter()
            .find_map(|(candidate, index)| (*candidate == id).then_some(*index))
            .expect("validated Realm atom IDs are completely bound");
        &self.atoms[index]
    }

    pub(super) fn rollback(self, atoms: &mut AtomTable) {
        for atom in self.atoms.into_iter().rev() {
            atoms.rollback_interned_string(atom);
        }
    }

    pub(super) fn atoms(&self) -> impl Iterator<Item = &Atom> {
        self.atoms.iter()
    }
}

impl Runtime {
    pub(super) fn intern_realm_atom_plan(
        &mut self,
        plan: &RealmAtomPlan<'_>,
    ) -> Result<RealmAtomBindings, RuntimeError> {
        debug_assert_eq!(
            plan.description_code_units,
            plan.entries
                .iter()
                .map(|entry| entry.name.description_code_units())
                .sum::<usize>()
        );
        let mut atoms = Vec::new();
        atoms
            .try_reserve_exact(plan.len())
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, plan.len()))?;
        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(plan.bindings.len())
            .map_err(|_| {
                allocation_failed(RuntimeResource::ObjectProperties, plan.bindings.len())
            })?;
        bindings.extend_from_slice(&plan.bindings);

        let outcome = (|| -> Result<(), RuntimeError> {
            for spec in &plan.entries {
                let owned;
                let name = match spec.name {
                    RealmAtomName::Existing(name) => name,
                    RealmAtomName::Literal(literal) => {
                        owned = JsString::from_utf8(literal).map_err(AtomError::from)?;
                        &owned
                    }
                };
                let atom = self.atoms.intern_string(name)?;
                atoms.push(atom);
            }
            Ok(())
        })();

        if let Err(error) = outcome {
            RealmAtomBindings { atoms, bindings }.rollback(&mut self.atoms);
            return Err(error);
        }
        Ok(RealmAtomBindings { atoms, bindings })
    }
}

fn visit_realm_atom_specs<'a>(
    names: &'a RealmNames,
    mut visit: impl FnMut(RealmAtomSpec<'a>) -> Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    let existing = |id, name| RealmAtomSpec {
        id,
        name: RealmAtomName::Existing(name),
    };
    let literal = |id, name| RealmAtomSpec {
        id,
        name: RealmAtomName::Literal(name),
    };

    for spec in [
        existing(RealmNameId::Call, &names.call),
        existing(RealmNameId::Entries, &names.entries),
        existing(RealmNameId::KeyFor, &names.key_for),
        existing(RealmNameId::Description, &names.description),
        existing(RealmNameId::IsError, &names.is_error),
        existing(RealmNameId::Bind, &names.bind),
    ] {
        visit(spec)?;
    }
    for (name, symbol) in DYNAMIC_SYMBOL_STATIC_PROPERTIES {
        visit(literal(RealmNameId::SymbolStatic(symbol), name))?;
    }
    for method in OBJECT_STATIC_METHODS {
        if let Some(name) = method.interned_name {
            visit(literal(RealmNameId::ObjectStatic(method.kind), name))?;
        }
    }
    for (name, kind) in [
        (BIGINT_INTERNED_STATICS[0], NativeFunctionKind::BigIntAsIntN),
        (
            BIGINT_INTERNED_STATICS[1],
            NativeFunctionKind::BigIntAsUintN,
        ),
    ] {
        visit(literal(RealmNameId::BigIntStatic(kind), name))?;
    }
    for method in STRING_PROTOTYPE_METHODS {
        if let Some(name) = method.interned_name {
            visit(literal(RealmNameId::StringMethod(method.method), name))?;
        }
    }
    for (name, _) in NUMBER_VALUE_STATICS {
        visit(literal(RealmNameId::NumberValue(name), name))?;
    }
    for (name, predicate) in NUMBER_PREDICATE_STATICS {
        visit(literal(RealmNameId::NumberPredicate(predicate), name))?;
    }
    visit(literal(RealmNameId::ArrayIsArray, "isArray"))?;
    for (name, method) in STRING_FROM_STATICS {
        visit(literal(RealmNameId::StringStatic(method), name))?;
    }
    for (name, search) in ARRAY_SEARCH_METHODS {
        visit(literal(RealmNameId::ArraySearch(search), name))?;
    }
    for (name, kind, _) in OBJECT_PROTOTYPE_REFLECTION {
        visit(literal(RealmNameId::ObjectPrototypeMethod(kind), name))?;
    }
    for method in ARRAY_MUTATOR_METHODS {
        visit(literal(RealmNameId::ArrayMutator(method), method.name()))?;
    }
    for method in ARRAY_COPIER_METHODS {
        visit(literal(RealmNameId::ArrayCopier(method), method.name()))?;
    }
    for method in NUMBER_FORMAT_METHODS {
        visit(literal(RealmNameId::NumberFormat(method), method.name()))?;
    }
    for method in ARRAY_CALLBACK_METHODS {
        visit(literal(RealmNameId::ArrayCallback(method), method.name()))?;
    }
    for method in ARRAY_REDUCTION_METHODS {
        visit(literal(RealmNameId::ArrayReduction(method), method.name()))?;
    }
    visit(literal(RealmNameId::ArraySplice, "splice"))?;
    visit(existing(RealmNameId::Reflect, &names.reflect))?;
    visit(existing(RealmNameId::JsonIsRawJson, &names.is_raw_json))?;
    visit(existing(RealmNameId::JsonParse, &names.parse))?;
    visit(existing(RealmNameId::JsonStringify, &names.stringify))?;
    visit(literal(RealmNameId::ParseFloat, "parseFloat"))?;
    visit(literal(RealmNameId::ParseInt, "parseInt"))?;
    for (name, function) in URI_FUNCTIONS {
        visit(literal(RealmNameId::Uri(function), name))?;
    }
    for method in ARRAY_SORT_METHODS {
        visit(literal(RealmNameId::ArraySort(method), method.name()))?;
    }
    for method in ARRAY_FLATTEN_METHODS {
        visit(literal(RealmNameId::ArrayFlatten(method), method.name()))?;
    }
    for method in MathMethod::ALL {
        visit(literal(RealmNameId::MathMethod(method), method.name()))?;
    }
    for (name, _) in MATH_CONSTANTS {
        visit(literal(RealmNameId::MathConstant(name), name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeLimits;

    #[test]
    fn atom_plan_derives_the_characterized_count_and_utf16_budget() {
        let runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let names = RealmNames::try_new(&runtime.atoms).expect("Realm names");
        let plan = RealmAtomPlan::try_new(&names).expect("atom plan");

        assert_eq!(plan.len(), 159);
        assert_eq!(plan.description_code_units(), 1_232);
    }

    #[test]
    fn atom_plan_binds_shared_names_by_typed_identity() {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let names = RealmNames::try_new(&runtime.atoms).expect("Realm names");
        let plan = RealmAtomPlan::try_new(&names).expect("atom plan");
        let bindings = runtime
            .intern_realm_atom_plan(&plan)
            .expect("atom bindings");

        assert!(
            bindings
                .atom(RealmNameId::Entries)
                .description()
                .is_some_and(|name| name == &names.entries)
        );
        assert!(
            bindings
                .atom(RealmNameId::MathMethod(MathMethod::Abs))
                .description()
                .is_some_and(|name| name == &JsString::from_utf8("abs").expect("abs"))
        );
    }
}
