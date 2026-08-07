//! Ordered Realm-local atom planning and typed bindings.

use crate::{
    Atom,
    runtime::{
        DatePrototypeMethod, DateStaticMethod, TemporalDurationPrototypeMethod,
        TemporalDurationStaticMethod, TemporalInstantPrototypeMethod, TemporalInstantStaticMethod,
    },
};

use super::{
    ARRAY_CALLBACK_METHODS, ARRAY_COPIER_METHODS, ARRAY_FLATTEN_METHODS, ARRAY_MUTATOR_METHODS,
    ARRAY_REDUCTION_METHODS, ARRAY_SEARCH_METHODS, ARRAY_SORT_METHODS, AtomError, AtomTable,
    BIGINT_INTERNED_STATICS, DYNAMIC_SYMBOL_STATIC_PROPERTIES, JsString, MATH_CONSTANTS, MapMethod,
    MathMethod, NUMBER_FORMAT_METHODS, NUMBER_PREDICATE_STATICS, NUMBER_VALUE_STATICS,
    NativeFunctionKind, OBJECT_PROTOTYPE_LEGACY_ACCESSORS, OBJECT_PROTOTYPE_REFLECTION,
    OBJECT_STATIC_METHODS, PromiseStatic, Runtime, RuntimeError, RuntimeResource,
    STRING_FROM_STATICS, STRING_PROTOTYPE_METHODS, SetMethod, URI_FUNCTIONS, allocation_failed,
    families::RealmIntrinsicSchema,
    schema::{
        IntrinsicDescriptorSpec, IntrinsicKeySpec, IntrinsicNameSpec, IntrinsicStringSpec,
        IntrinsicValueSpec, RealmNameId,
    },
};

#[derive(Clone, Copy)]
struct RealmAtomSpec {
    name: &'static str,
}

/// The exact ordered set of strings one Realm creation interns.
pub(super) struct RealmAtomPlan {
    entries: Vec<RealmAtomSpec>,
    bindings: Vec<(RealmNameId, usize)>,
    description_code_units: usize,
}

impl RealmAtomPlan {
    pub(super) fn try_new(schema: &RealmIntrinsicSchema) -> Result<Self, RuntimeError> {
        let required = required_realm_names(schema)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(required.len())
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, required.len()))?;
        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(required.len())
            .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, required.len()))?;
        let mut description_code_units = 0_usize;
        visit_realm_name_order(|id| {
            if !required.contains(&id) {
                return Ok(());
            }
            debug_assert!(
                bindings
                    .iter()
                    .all(|(candidate, _): &(RealmNameId, usize)| *candidate != id)
            );
            let name = realm_name_description(id);
            let index = entries
                .iter()
                .position(|entry: &RealmAtomSpec| entry.name == name);
            let index = if let Some(index) = index {
                index
            } else {
                description_code_units = description_code_units
                    .checked_add(name.encode_utf16().count())
                    .ok_or_else(|| {
                        allocation_failed(RuntimeResource::ObjectProperties, usize::MAX)
                    })?;
                entries.push(RealmAtomSpec { name });
                entries.len() - 1
            };
            bindings.push((id, index));
            Ok(())
        })?;
        assert_eq!(
            bindings.len(),
            required.len(),
            "every schema Realm name participates in the pinned atom order"
        );

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

fn required_realm_names(schema: &RealmIntrinsicSchema) -> Result<Vec<RealmNameId>, RuntimeError> {
    let capacity = schema
        .specs()
        .len()
        .checked_add(schema.properties().len().saturating_mul(2))
        .ok_or_else(|| allocation_failed(RuntimeResource::ObjectProperties, usize::MAX))?;
    let mut required = Vec::new();
    required
        .try_reserve_exact(capacity)
        .map_err(|_| allocation_failed(RuntimeResource::ObjectProperties, capacity))?;
    for function in schema.specs() {
        if let IntrinsicNameSpec::RealmName(id) = function.name {
            push_unique(&mut required, id);
        }
    }
    for property in schema.properties() {
        if let IntrinsicKeySpec::InternedString(id) | IntrinsicKeySpec::RealmCreatedName(id) =
            property.key
        {
            push_unique(&mut required, id);
        }
        if let IntrinsicDescriptorSpec::Data {
            value: IntrinsicValueSpec::String(IntrinsicStringSpec::RealmName(id)),
            ..
        } = property.descriptor
        {
            push_unique(&mut required, id);
        }
    }
    Ok(required)
}

fn push_unique(names: &mut Vec<RealmNameId>, id: RealmNameId) {
    if !names.contains(&id) {
        names.push(id);
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
        plan: &RealmAtomPlan,
    ) -> Result<RealmAtomBindings, RuntimeError> {
        debug_assert_eq!(
            plan.description_code_units,
            plan.entries
                .iter()
                .map(|entry| entry.name.encode_utf16().count())
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
                let name = JsString::from_utf8(spec.name).map_err(AtomError::from)?;
                let atom = self.atoms.intern_string(&name)?;
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

#[allow(
    clippy::too_many_lines,
    reason = "the canonical Realm atom sequence keeps all intrinsic families in one auditable order"
)]
fn visit_realm_name_order(
    mut visit: impl FnMut(RealmNameId) -> Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    visit_core_name_order(&mut visit)?;
    for (_, symbol) in DYNAMIC_SYMBOL_STATIC_PROPERTIES {
        visit(RealmNameId::SymbolStatic(symbol))?;
    }
    for method in OBJECT_STATIC_METHODS {
        if method.interned_name.is_some() {
            visit(RealmNameId::ObjectStatic(method.kind))?;
        }
    }
    for kind in [
        NativeFunctionKind::BigIntAsIntN,
        NativeFunctionKind::BigIntAsUintN,
    ] {
        visit(RealmNameId::BigIntStatic(kind))?;
    }
    for method in STRING_PROTOTYPE_METHODS {
        if method.interned_name.is_some() {
            visit(RealmNameId::StringMethod(method.method))?;
        }
    }
    for alias in ["trimRight", "trimLeft"] {
        visit(RealmNameId::StringAlias(alias))?;
    }
    for (name, _) in NUMBER_VALUE_STATICS {
        visit(RealmNameId::NumberValue(name))?;
    }
    for (_, predicate) in NUMBER_PREDICATE_STATICS {
        visit(RealmNameId::NumberPredicate(predicate))?;
    }
    visit(RealmNameId::ArrayIsArray)?;
    visit(RealmNameId::ArrayFromAsync)?;
    for method in DateStaticMethod::ALL {
        visit(RealmNameId::DateStatic(method))?;
    }
    for method in DatePrototypeMethod::ALL {
        if !matches!(
            method,
            DatePrototypeMethod::ValueOf
                | DatePrototypeMethod::ToString
                | DatePrototypeMethod::ToIsoString
                | DatePrototypeMethod::ToLocaleString
                | DatePrototypeMethod::ToJson
                | DatePrototypeMethod::SymbolToPrimitive
        ) {
            visit(RealmNameId::DatePrototype(method))?;
        }
    }
    visit(RealmNameId::Temporal)?;
    visit(RealmNameId::Duration)?;
    visit(RealmNameId::Instant)?;
    for method in TemporalDurationStaticMethod::ALL {
        visit(RealmNameId::TemporalDurationStatic(method))?;
    }
    for method in TemporalDurationPrototypeMethod::ALL {
        visit(RealmNameId::TemporalDurationPrototype(method))?;
    }
    for method in TemporalInstantStaticMethod::ALL {
        visit(RealmNameId::TemporalInstantStatic(method))?;
    }
    for method in [
        TemporalInstantPrototypeMethod::EpochMilliseconds,
        TemporalInstantPrototypeMethod::EpochNanoseconds,
        TemporalInstantPrototypeMethod::Add,
        TemporalInstantPrototypeMethod::Subtract,
        TemporalInstantPrototypeMethod::Equals,
    ] {
        visit(RealmNameId::TemporalInstantPrototype(method))?;
    }
    visit(RealmNameId::RegExpEscape)?;
    visit(RealmNameId::RegExpCompile)?;
    visit(RealmNameId::RegExpTest)?;
    for (_, method) in STRING_FROM_STATICS {
        visit(RealmNameId::StringStatic(method))?;
    }
    for (_, search) in ARRAY_SEARCH_METHODS {
        visit(RealmNameId::ArraySearch(search))?;
    }
    for (_, kind, _) in OBJECT_PROTOTYPE_REFLECTION {
        visit(RealmNameId::ObjectPrototypeMethod(kind))?;
    }
    for (_, kind, _) in OBJECT_PROTOTYPE_LEGACY_ACCESSORS {
        visit(RealmNameId::ObjectPrototypeMethod(kind))?;
    }
    for method in ARRAY_MUTATOR_METHODS {
        visit(RealmNameId::ArrayMutator(method))?;
    }
    for method in ARRAY_COPIER_METHODS {
        visit(RealmNameId::ArrayCopier(method))?;
    }
    for method in NUMBER_FORMAT_METHODS {
        visit(RealmNameId::NumberFormat(method))?;
    }
    for method in ARRAY_CALLBACK_METHODS {
        visit(RealmNameId::ArrayCallback(method))?;
    }
    for method in ARRAY_REDUCTION_METHODS {
        visit(RealmNameId::ArrayReduction(method))?;
    }
    for id in [
        RealmNameId::ArraySplice,
        RealmNameId::Reflect,
        RealmNameId::JsonIsRawJson,
        RealmNameId::JsonParse,
        RealmNameId::JsonStringify,
        RealmNameId::ParseFloat,
        RealmNameId::ParseInt,
    ] {
        visit(id)?;
    }
    for (_, function) in URI_FUNCTIONS {
        visit(RealmNameId::Uri(function))?;
    }
    for method in PromiseStatic::ALL {
        visit(RealmNameId::PromiseStatic(method))?;
    }
    visit_map_name_order(&mut visit)?;
    visit_set_name_order(&mut visit)?;
    for method in ARRAY_SORT_METHODS {
        visit(RealmNameId::ArraySort(method))?;
    }
    for method in ARRAY_FLATTEN_METHODS {
        visit(RealmNameId::ArrayFlatten(method))?;
    }
    for method in MathMethod::ALL {
        visit(RealmNameId::MathMethod(method))?;
    }
    for (name, _) in MATH_CONSTANTS {
        visit(RealmNameId::MathConstant(name))?;
    }
    Ok(())
}

fn visit_core_name_order(
    visit: &mut impl FnMut(RealmNameId) -> Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    for id in [
        RealmNameId::Call,
        RealmNameId::Entries,
        RealmNameId::KeyFor,
        RealmNameId::Description,
        RealmNameId::IsError,
        RealmNameId::Bind,
        RealmNameId::Deref,
        RealmNameId::Register,
        RealmNameId::Unregister,
        RealmNameId::ProxyRevocable,
    ] {
        visit(id)?;
    }
    Ok(())
}

fn visit_map_name_order(
    visit: &mut impl FnMut(RealmNameId) -> Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    for method in MapMethod::ALL {
        if matches!(
            method,
            MapMethod::Set
                | MapMethod::GetOrInsert
                | MapMethod::GetOrInsertComputed
                | MapMethod::Clear
                | MapMethod::ForEach
        ) {
            visit(RealmNameId::MapMethod(method))?;
        }
    }
    Ok(())
}

fn visit_set_name_order(
    visit: &mut impl FnMut(RealmNameId) -> Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    for method in SetMethod::ALL {
        if matches!(
            method,
            SetMethod::Clear
                | SetMethod::ForEach
                | SetMethod::IsDisjointFrom
                | SetMethod::IsSubsetOf
                | SetMethod::IsSupersetOf
                | SetMethod::Intersection
                | SetMethod::Difference
                | SetMethod::SymmetricDifference
                | SetMethod::Union
        ) {
            visit(RealmNameId::SetMethod(method))?;
        }
    }
    Ok(())
}

fn realm_name_description(id: RealmNameId) -> &'static str {
    match id {
        RealmNameId::Call => "call",
        RealmNameId::Entries => "entries",
        RealmNameId::KeyFor => "keyFor",
        RealmNameId::Description => "description",
        RealmNameId::IsError => "isError",
        RealmNameId::Bind => "bind",
        RealmNameId::Deref => "deref",
        RealmNameId::Register => "register",
        RealmNameId::Unregister => "unregister",
        RealmNameId::ProxyRevocable => "revocable",
        RealmNameId::Reflect => "Reflect",
        RealmNameId::JsonIsRawJson => "isRawJSON",
        RealmNameId::JsonParse => "parse",
        RealmNameId::JsonStringify => "stringify",
        RealmNameId::ParseFloat => "parseFloat",
        RealmNameId::ParseInt => "parseInt",
        RealmNameId::SymbolStatic(symbol) => DYNAMIC_SYMBOL_STATIC_PROPERTIES
            .into_iter()
            .find_map(|(name, candidate)| (candidate == symbol).then_some(name))
            .expect("every dynamic Symbol static has one declared name"),
        RealmNameId::Uri(function) => URI_FUNCTIONS
            .into_iter()
            .find_map(|(name, candidate)| (candidate == function).then_some(name))
            .expect("every URI intrinsic has one declared name"),
        RealmNameId::ObjectStatic(kind) => OBJECT_STATIC_METHODS
            .into_iter()
            .find_map(|method| {
                (method.kind == kind)
                    .then_some(method.interned_name)
                    .flatten()
            })
            .expect("every dynamic Object static has one declared name"),
        RealmNameId::BigIntStatic(NativeFunctionKind::BigIntAsIntN) => BIGINT_INTERNED_STATICS[0],
        RealmNameId::BigIntStatic(NativeFunctionKind::BigIntAsUintN) => BIGINT_INTERNED_STATICS[1],
        RealmNameId::BigIntStatic(_) => unreachable!("only BigInt statics use this name ID"),
        RealmNameId::StringMethod(method) => STRING_PROTOTYPE_METHODS
            .into_iter()
            .find_map(|candidate| {
                (candidate.method == method)
                    .then_some(candidate.interned_name)
                    .flatten()
            })
            .expect("every dynamic String method has one declared name"),
        RealmNameId::StringAlias(name)
        | RealmNameId::NumberValue(name)
        | RealmNameId::MathConstant(name) => name,
        RealmNameId::NumberPredicate(predicate) => NUMBER_PREDICATE_STATICS
            .into_iter()
            .find_map(|(name, candidate)| (candidate == predicate).then_some(name))
            .expect("every Number predicate has one declared name"),
        RealmNameId::StringStatic(method) => STRING_FROM_STATICS
            .into_iter()
            .find_map(|(name, candidate)| (candidate == method).then_some(name))
            .expect("every String static has one declared name"),
        RealmNameId::ArraySearch(search) => ARRAY_SEARCH_METHODS
            .into_iter()
            .find_map(|(name, candidate)| (candidate == search).then_some(name))
            .expect("every Array search has one declared name"),
        RealmNameId::ObjectPrototypeMethod(kind) => OBJECT_PROTOTYPE_REFLECTION
            .into_iter()
            .find_map(|(name, candidate, _)| (candidate == kind).then_some(name))
            .or_else(|| {
                OBJECT_PROTOTYPE_LEGACY_ACCESSORS
                    .into_iter()
                    .find_map(|(name, candidate, _)| (candidate == kind).then_some(name))
            })
            .expect("every Object prototype method has one declared name"),
        RealmNameId::ArrayMutator(method) => method.name(),
        RealmNameId::ArrayCopier(method) => method.name(),
        RealmNameId::NumberFormat(method) => method.name(),
        RealmNameId::ArrayCallback(method) => method.name(),
        RealmNameId::ArrayReduction(method) => method.name(),
        RealmNameId::ArraySplice => "splice",
        RealmNameId::ArrayIsArray => "isArray",
        RealmNameId::ArrayFromAsync => "fromAsync",
        RealmNameId::DateStatic(method) => method.name(),
        RealmNameId::DatePrototype(method) => method.name(),
        RealmNameId::Temporal => "Temporal",
        RealmNameId::Duration => "Duration",
        RealmNameId::Instant => "Instant",
        RealmNameId::TemporalDurationStatic(method) => method.name(),
        RealmNameId::TemporalDurationPrototype(method) => method.name(),
        RealmNameId::TemporalInstantStatic(method) => method.name(),
        RealmNameId::TemporalInstantPrototype(method) => method.name(),
        RealmNameId::RegExpEscape => "escape",
        RealmNameId::RegExpCompile => "compile",
        RealmNameId::RegExpTest => "test",
        RealmNameId::PromiseStatic(method) => method.name(),
        RealmNameId::MapMethod(method) => method.name(),
        RealmNameId::SetMethod(method) => method.name(),
        RealmNameId::ArraySort(method) => method.name(),
        RealmNameId::ArrayFlatten(method) => method.name(),
        RealmNameId::MathMethod(method) => method.name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeLimits;

    #[test]
    fn atom_plan_derives_the_characterized_count_and_utf16_budget() {
        let schema = RealmIntrinsicSchema::try_new().expect("Realm schema");
        let plan = RealmAtomPlan::try_new(&schema).expect("atom plan");

        assert_eq!(plan.len(), 270);
        assert_eq!(plan.description_code_units(), 2_281);
    }

    #[test]
    fn atom_plan_binds_shared_names_by_typed_identity() {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let schema = RealmIntrinsicSchema::try_new().expect("Realm schema");
        let plan = RealmAtomPlan::try_new(&schema).expect("atom plan");
        let bindings = runtime
            .intern_realm_atom_plan(&plan)
            .expect("atom bindings");

        assert!(
            bindings
                .atom(RealmNameId::Entries)
                .description()
                .is_some_and(|name| name == &JsString::from_utf8("entries").expect("entries"))
        );
        assert!(
            bindings
                .atom(RealmNameId::MathMethod(MathMethod::Abs))
                .description()
                .is_some_and(|name| name == &JsString::from_utf8("abs").expect("abs"))
        );
    }
}
