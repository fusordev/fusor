use quickjs_runtime::{
    ArrayIndex, AtomError, AtomKind, AtomLimits, AtomTable, AtomUsage, JsString, MAX_ARRAY_INDEX,
    PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS, PREDEFINED_INTERNER_SLOTS,
    PredefinedAtom, PropertyKey,
};

fn string(text: &str) -> JsString {
    JsString::from_utf8(text).unwrap()
}

fn table() -> AtomTable {
    AtomTable::new(AtomLimits::default()).unwrap()
}

#[test]
fn predefined_atoms_are_seeded_into_their_exact_namespaces() {
    let mut table = table();
    assert_eq!(
        table.usage(),
        AtomUsage {
            live_atoms: PREDEFINED_ATOM_COUNT,
            live_description_code_units: PREDEFINED_DESCRIPTION_CODE_UNITS,
            interner_slots: PREDEFINED_INTERNER_SLOTS,
        }
    );

    let name = table.predefined(PredefinedAtom::Name);
    assert_eq!(name.kind(), AtomKind::String);
    assert_eq!(name.predefined_atom(), Some(PredefinedAtom::Name));
    assert_eq!(table.intern_string(&string("name")).unwrap(), name);
    assert_eq!(
        table.predefined(PredefinedAtom::PrivateBrand).kind(),
        AtomKind::Private
    );
    assert_eq!(
        table.predefined(PredefinedAtom::SymbolIterator).kind(),
        AtomKind::Symbol
    );
}

#[test]
fn content_interning_preserves_utf16_and_namespace_identity() {
    let mut table = table();
    let lone_surrogate = JsString::from_code_units([0xd800, u16::from(b'x')]).unwrap();
    let same_units = JsString::from_code_units(lone_surrogate.code_units()).unwrap();

    let string_a = table.intern_string(&lone_surrogate).unwrap();
    let string_b = table.intern_string(&same_units).unwrap();
    let global_a = table.intern_global_symbol(&lone_surrogate).unwrap();
    let global_b = table.intern_global_symbol(&same_units).unwrap();

    assert_eq!(string_a, string_b);
    assert_eq!(global_a, global_b);
    assert_ne!(string_a, global_a);
    assert_eq!(
        string_a
            .description()
            .unwrap()
            .code_units()
            .collect::<Vec<_>>(),
        vec![0xd800, u16::from(b'x')]
    );
}

#[test]
fn property_keys_split_array_indices_from_interned_strings() {
    let mut table = table();
    let startup = table.usage();

    for (text, value) in [("0", 0), ("4294967294", MAX_ARRAY_INDEX)] {
        let key = table.property_key_from_string(&string(text)).unwrap();
        assert_eq!(key.as_index(), ArrayIndex::new(value));
        assert_eq!(key.as_atom(), None);
    }
    assert_eq!(table.usage(), startup);

    for text in ["00", "-0", "4294967295"] {
        let key = table.property_key_from_string(&string(text)).unwrap();
        assert_eq!(key.as_index(), None);
        assert_eq!(
            key.as_atom().map(quickjs_runtime::Atom::kind),
            Some(AtomKind::String)
        );
    }
}

#[test]
fn unique_symbols_distinguish_missing_empty_and_repeated_descriptions() {
    let mut table = table();
    let description = string("symbol description");
    let first = table.new_unique_symbol(Some(&description)).unwrap();
    let second = table.new_unique_symbol(Some(&description)).unwrap();
    let missing = table.new_unique_symbol(None).unwrap();
    let empty = table.new_unique_symbol(Some(&JsString::empty())).unwrap();

    assert_ne!(first, second);
    assert_eq!(first.description(), second.description());
    assert_eq!(missing.description(), None);
    assert_eq!(empty.description(), Some(&JsString::empty()));
    assert_ne!(missing, empty);
}

#[test]
fn public_property_conversion_validates_membership_and_kind() {
    let mut first = table();
    let second = table();
    let description = string("property identity");
    let symbol = first.new_unique_symbol(Some(&description)).unwrap();
    let private = first.new_private_name(&description).unwrap();
    let string_atom = first.intern_string(&description).unwrap();

    assert_eq!(
        first.property_key_from_symbol(&symbol).unwrap().as_atom(),
        Some(&symbol)
    );
    assert_eq!(
        first.property_key_from_symbol(&private),
        Err(AtomError::PrivateNameIsNotPropertyKey)
    );
    assert_eq!(
        first.property_key_from_symbol(&string_atom),
        Err(AtomError::ExpectedSymbol {
            actual: AtomKind::String,
        })
    );
    assert_eq!(
        second.property_key_from_symbol(&symbol),
        Err(AtomError::ForeignAtom)
    );
}

#[test]
fn dropped_tables_leave_detectable_orphan_handles() {
    let atom = {
        let mut table = table();
        table.new_unique_symbol(None).unwrap()
    };
    let other = table();

    assert!(atom.is_orphaned());
    assert_eq!(other.validate(&atom), Err(AtomError::OrphanedAtom));
}

#[test]
fn dead_entries_are_charged_only_until_last_handle_drop() {
    let mut table = table();
    let startup = table.usage();
    let atom = table.intern_string(&string("collectable key")).unwrap();
    let clone = atom.clone();

    drop(atom);
    assert_eq!(table.usage().live_atoms, startup.live_atoms + 1);
    drop(clone);
    assert_eq!(table.usage().live_atoms, startup.live_atoms);
    assert_eq!(table.usage().interner_slots, startup.interner_slots + 1);
    assert_eq!(table.collect_dead(), 1);
    assert_eq!(table.usage(), startup);
}

#[test]
fn full_slot_limit_collects_dead_entries_from_other_hash_buckets() {
    let limits = AtomLimits::new(
        PREDEFINED_ATOM_COUNT + 1,
        PREDEFINED_DESCRIPTION_CODE_UNITS + 100,
        PREDEFINED_INTERNER_SLOTS + 1,
    );
    let mut table = AtomTable::new(limits).unwrap();
    let first = table.intern_string(&string("dead bucket one")).unwrap();
    drop(first);
    assert_eq!(table.usage().interner_slots, limits.max_interner_slots);

    let second_description = string("live bucket two");
    let second = table.intern_string(&second_description).unwrap();
    assert_eq!(second.description(), Some(&second_description));
    assert_eq!(table.usage().live_atoms, PREDEFINED_ATOM_COUNT + 1);
    assert_eq!(table.usage().interner_slots, limits.max_interner_slots);
    assert_eq!(table.collect_dead(), 0);
}

#[test]
fn failed_insertion_does_not_commit_planned_dead_slot_reclamation() {
    let limits = AtomLimits::new(
        PREDEFINED_ATOM_COUNT + 1,
        PREDEFINED_DESCRIPTION_CODE_UNITS + 1,
        PREDEFINED_INTERNER_SLOTS + 1,
    );
    let mut table = AtomTable::new(limits).unwrap();
    let ephemeral = table.intern_string(&string("~")).unwrap();
    drop(ephemeral);

    let before = table.usage();
    assert_eq!(before.live_atoms, PREDEFINED_ATOM_COUNT);
    assert_eq!(before.interner_slots, limits.max_interner_slots);
    assert!(matches!(
        table.intern_string(&string("xx")),
        Err(AtomError::DescriptionCodeUnitLimit { .. })
    ));
    assert_eq!(
        table.usage(),
        before,
        "a failed operation must not commit its read-only reclaim plan"
    );
    assert_eq!(table.collect_dead(), 1);
    assert_eq!(table.usage().interner_slots, PREDEFINED_INTERNER_SLOTS);
}

#[test]
fn exact_limits_are_inclusive_and_failed_insertions_do_not_charge_usage() {
    let limits = AtomLimits::new(
        PREDEFINED_ATOM_COUNT + 1,
        PREDEFINED_DESCRIPTION_CODE_UNITS + 1,
        PREDEFINED_INTERNER_SLOTS + 1,
    );
    let mut table = AtomTable::new(limits).unwrap();
    let atom = table.intern_string(&string("~")).unwrap();
    assert_eq!(
        table.usage(),
        AtomUsage {
            live_atoms: limits.max_live_atoms,
            live_description_code_units: limits.max_live_description_code_units,
            interner_slots: limits.max_interner_slots,
        }
    );

    let full = table.usage();
    assert!(matches!(
        table.new_unique_symbol(None),
        Err(AtomError::LiveAtomLimit { .. })
    ));
    assert_eq!(table.usage(), full);

    drop(atom);
    assert_eq!(table.usage().live_atoms, PREDEFINED_ATOM_COUNT);
}

#[test]
fn property_key_debug_and_equality_remain_value_or_identity_based() {
    let mut table = table();
    let index = table.property_key_from_string(&string("42")).unwrap();
    assert_eq!(index, PropertyKey::from_index(ArrayIndex::new(42).unwrap()));

    let symbol = table.new_unique_symbol(None).unwrap();
    let atom_key = table.property_key_from_symbol(&symbol).unwrap();
    assert_eq!(atom_key.as_atom(), Some(&symbol));
}
