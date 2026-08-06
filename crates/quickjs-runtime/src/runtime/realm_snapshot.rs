//! Normalized installed-Realm snapshots for bootstrap regression tests.

use std::collections::HashMap;

use crate::{
    Atom, AtomKind, JsBigInt, JsString, PredefinedAtom, PropertyKey, PropertyLayout,
    object::{BoxedPrimitive, KeyPhases, ObjectRecord, OwnProperty},
};

use super::{
    ErrorIntrinsicKind, FunctionImplementation, HeapReference, RealmId, RealmIntrinsics, Runtime,
    StoredValue, usize_to_u64,
};

#[derive(Debug, Eq, PartialEq)]
pub(super) struct RealmSnapshot {
    nodes: Vec<NodeSnapshot>,
}

#[derive(Debug, Eq, PartialEq)]
struct NodeSnapshot {
    identity: String,
    kind: NodeKindSnapshot,
    prototype: Option<String>,
    extensible: bool,
    properties: Vec<PropertySnapshot>,
}

#[derive(Debug, Eq, PartialEq)]
enum NodeKindSnapshot {
    OrdinaryObject,
    ArrayObject {
        length: u32,
    },
    DateObject(u64),
    ErrorObject,
    BoxedBoolean(bool),
    BoxedNumber(u64),
    BoxedBigInt(String),
    BoxedString(Vec<u16>),
    BoxedSymbol(AtomSnapshot),
    NativeFunction {
        implementation: String,
        realm_local: bool,
        callable: bool,
        constructable: bool,
    },
}

#[derive(Debug, Eq, PartialEq)]
struct PropertySnapshot {
    key: KeySnapshot,
    layout: PropertyLayout,
    value: PropertyValueSnapshot,
}

#[derive(Debug, Eq, PartialEq)]
enum PropertyValueSnapshot {
    Data(ValueSnapshot),
    Accessor {
        getter: Option<String>,
        setter: Option<String>,
    },
}

#[derive(Debug, Eq, PartialEq)]
enum ValueSnapshot {
    Undefined,
    Null,
    Boolean(bool),
    Number(u64),
    BigInt(String),
    String(Vec<u16>),
    Symbol(AtomSnapshot),
    Reference(String),
}

#[derive(Debug, Eq, PartialEq)]
enum KeySnapshot {
    Index(u32),
    Atom(AtomSnapshot),
}

#[derive(Debug, Eq, PartialEq)]
struct AtomSnapshot {
    kind: AtomKind,
    predefined: Option<PredefinedAtom>,
    description: Option<Vec<u16>>,
}

impl RealmSnapshot {
    #[allow(
        clippy::too_many_lines,
        reason = "the normalized Realm snapshot enumerates every intrinsic identity in one audited table"
    )]
    pub(super) fn capture(runtime: &Runtime, realm: RealmId) -> Self {
        let state = runtime.realms.get(realm).expect("snapshot Realm is live");
        let RealmIntrinsics::Ready {
            errors,
            boolean,
            number,
            bigint,
            string,
            array,
            date,
            temporal,
            map,
            set,
            weak_map,
            weak_set,
            weak_ref,
            finalization_registry,
            promise,
            regexp,
            symbol,
            iterators,
            generators,
            async_functions,
            async_generators,
            ..
        } = state.intrinsics
        else {
            panic!("snapshot Realm intrinsics are ready");
        };
        let mut identities = HashMap::new();
        register_identity(
            HeapReference::Object(state.global_object),
            "%global%",
            &mut identities,
        );
        register_identity(
            HeapReference::Object(state.object_prototype),
            "%Object.prototype%",
            &mut identities,
        );
        for (object, identity) in [
            (boolean.prototype, "%Boolean.prototype%"),
            (number.prototype, "%Number.prototype%"),
            (bigint.prototype, "%BigInt.prototype%"),
            (string.prototype, "%String.prototype%"),
            (array.prototype, "%Array.prototype%"),
            (date.prototype, "%Date.prototype%"),
            (temporal.namespace, "%Temporal%"),
            (temporal.duration_prototype, "%Temporal.Duration.prototype%"),
            (temporal.instant_prototype, "%Temporal.Instant.prototype%"),
            (map.prototype, "%Map.prototype%"),
            (map.iterator_prototype, "%MapIterator.prototype%"),
            (set.prototype, "%Set.prototype%"),
            (set.iterator_prototype, "%SetIterator.prototype%"),
            (weak_map.prototype, "%WeakMap.prototype%"),
            (weak_set.prototype, "%WeakSet.prototype%"),
            (weak_ref.prototype, "%WeakRef.prototype%"),
            (
                finalization_registry.prototype,
                "%FinalizationRegistry.prototype%",
            ),
            (promise.prototype, "%Promise.prototype%"),
            (regexp.prototype, "%RegExp.prototype%"),
            (symbol.prototype, "%Symbol.prototype%"),
            (iterators.iterator_prototype, "%Iterator.prototype%"),
            (
                iterators.async_iterator_prototype,
                "%AsyncIterator.prototype%",
            ),
            (
                iterators.async_from_sync_iterator_prototype,
                "%AsyncFromSyncIterator.prototype%",
            ),
            (
                iterators.array_iterator_prototype,
                "%ArrayIterator.prototype%",
            ),
            (
                iterators.string_iterator_prototype,
                "%StringIterator.prototype%",
            ),
            (
                iterators.regexp_string_iterator_prototype,
                "%RegExpStringIterator.prototype%",
            ),
            (
                generators.function_prototype,
                "%GeneratorFunction.prototype%",
            ),
            (generators.generator_prototype, "%Generator.prototype%"),
            (
                async_functions.function_prototype,
                "%AsyncFunction.prototype%",
            ),
            (
                async_generators.function_prototype,
                "%AsyncGeneratorFunction.prototype%",
            ),
            (
                async_generators.generator_prototype,
                "%AsyncGenerator.prototype%",
            ),
        ] {
            register_identity(HeapReference::Object(object), identity, &mut identities);
        }
        for kind in ErrorIntrinsicKind::ALL {
            let intrinsic = errors.intrinsic(kind);
            register_identity(
                HeapReference::Object(intrinsic.prototype),
                format!("%{}.prototype%", kind.name()),
                &mut identities,
            );
        }
        let global = runtime
            .objects
            .get(state.global_object)
            .expect("snapshot global is live");
        for name in ["Reflect", "JSON", "Math"] {
            register_identity(
                HeapReference::Object(global_object_property(&global.record, name)),
                format!("%{name}%"),
                &mut identities,
            );
        }
        for (id, function) in runtime.functions.iter() {
            let FunctionImplementation::Native(native) = &function.implementation else {
                continue;
            };
            if native.realm == realm {
                register_identity(
                    HeapReference::Function(id),
                    format!("%{:?}%", native.kind),
                    &mut identities,
                );
            }
        }
        let mut references = identities.keys().copied().collect::<Vec<_>>();
        references.sort_unstable_by(|left, right| {
            identities
                .get(left)
                .expect("left identity")
                .cmp(identities.get(right).expect("right identity"))
        });
        let nodes = references
            .into_iter()
            .map(|reference| snapshot_node(runtime, realm, reference, &identities))
            .collect();

        Self { nodes }
    }

    pub(super) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub(super) fn property_count(&self) -> u64 {
        usize_to_u64(self.nodes.iter().map(|node| node.properties.len()).sum())
    }

    pub(super) fn fingerprint(&self) -> u64 {
        format!("{self:#?}")
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }
}

fn snapshot_node(
    runtime: &Runtime,
    realm: RealmId,
    reference: HeapReference,
    identities: &HashMap<HeapReference, String>,
) -> NodeSnapshot {
    let (kind, record) = match reference {
        HeapReference::Object(id) => {
            let object = runtime.objects.get(id).expect("snapshot object is live");
            (snapshot_object_kind(object), &object.record)
        }
        HeapReference::Function(id) => {
            let function = runtime
                .functions
                .get(id)
                .expect("snapshot function is live");
            let FunctionImplementation::Native(native) = &function.implementation else {
                panic!("the standard intrinsic graph contains only native functions");
            };
            (
                NodeKindSnapshot::NativeFunction {
                    implementation: format!("{:?}", native.kind),
                    realm_local: native.realm == realm,
                    callable: true,
                    constructable: native.kind.is_constructor(),
                },
                &function.object,
            )
        }
    };
    let prototype = record
        .prototype()
        .map(|prototype| reference_identity(prototype, identities));
    let properties = snapshot_properties(record, identities);
    NodeSnapshot {
        identity: reference_identity(reference, identities),
        kind,
        prototype,
        extensible: record.is_extensible(),
        properties,
    }
}

fn snapshot_object_kind(object: &super::HeapObject) -> NodeKindSnapshot {
    if let Some(array) = object.array_state() {
        return NodeKindSnapshot::ArrayObject {
            length: array.length(),
        };
    }
    if object.is_error() {
        return NodeKindSnapshot::ErrorObject;
    }
    if let Some(state) = object.date_state() {
        return NodeKindSnapshot::DateObject(state.value().as_f64().to_bits());
    }
    match object.boxed_primitive() {
        Some(BoxedPrimitive::Boolean(value)) => NodeKindSnapshot::BoxedBoolean(*value),
        Some(BoxedPrimitive::Number(value)) => {
            NodeKindSnapshot::BoxedNumber(value.as_f64().to_bits())
        }
        Some(BoxedPrimitive::BigInt(value)) => NodeKindSnapshot::BoxedBigInt(bigint_decimal(value)),
        Some(BoxedPrimitive::String(value)) => {
            NodeKindSnapshot::BoxedString(string_code_units(value))
        }
        Some(BoxedPrimitive::Symbol(value)) => NodeKindSnapshot::BoxedSymbol(snapshot_atom(value)),
        None => NodeKindSnapshot::OrdinaryObject,
    }
}

fn snapshot_properties(
    record: &ObjectRecord,
    identities: &HashMap<HeapReference, String>,
) -> Vec<PropertySnapshot> {
    let keys = record
        .try_own_key_snapshot(None, KeyPhases::ALL)
        .expect("Realm snapshot key allocation");
    (0..keys.len())
        .map(|index| {
            let key = keys.get(index).expect("snapshot key index").key();
            let property = record
                .own_property(key)
                .expect("every own-key snapshot entry has a descriptor");
            let layout = property.layout();
            let value = match property {
                OwnProperty::Data { value, .. } => {
                    PropertyValueSnapshot::Data(snapshot_value(&value, identities))
                }
                OwnProperty::Accessor { getter, setter, .. } => PropertyValueSnapshot::Accessor {
                    getter: getter
                        .map(|id| reference_identity(HeapReference::Function(id), identities)),
                    setter: setter
                        .map(|id| reference_identity(HeapReference::Function(id), identities)),
                },
            };
            PropertySnapshot {
                key: snapshot_key(key),
                layout,
                value,
            }
        })
        .collect()
}

fn snapshot_value(
    value: &StoredValue,
    identities: &HashMap<HeapReference, String>,
) -> ValueSnapshot {
    match value {
        StoredValue::Undefined => ValueSnapshot::Undefined,
        StoredValue::Null => ValueSnapshot::Null,
        StoredValue::Boolean(value) => ValueSnapshot::Boolean(*value),
        StoredValue::Number(value) => ValueSnapshot::Number(value.as_f64().to_bits()),
        StoredValue::BigInt(value) => ValueSnapshot::BigInt(bigint_decimal(value)),
        StoredValue::String(value) => ValueSnapshot::String(string_code_units(value)),
        StoredValue::Symbol(value) => ValueSnapshot::Symbol(snapshot_atom(value)),
        StoredValue::Function(id) => {
            ValueSnapshot::Reference(reference_identity(HeapReference::Function(*id), identities))
        }
        StoredValue::Object(id) => {
            ValueSnapshot::Reference(reference_identity(HeapReference::Object(*id), identities))
        }
    }
}

fn snapshot_key(key: &PropertyKey) -> KeySnapshot {
    if let Some(index) = key.as_index() {
        KeySnapshot::Index(index.get())
    } else {
        KeySnapshot::Atom(snapshot_atom(
            key.as_atom()
                .expect("validated property key is index or atom"),
        ))
    }
}

fn snapshot_atom(atom: &Atom) -> AtomSnapshot {
    AtomSnapshot {
        kind: atom.kind(),
        predefined: atom.predefined_atom(),
        description: atom.description().map(string_code_units),
    }
}

fn string_code_units(value: &JsString) -> Vec<u16> {
    value.code_units().collect()
}

fn bigint_decimal(value: &JsBigInt) -> String {
    value
        .to_string_radix(10)
        .expect("Realm snapshot BigInt rendering")
}

fn register_identity(
    reference: HeapReference,
    identity: impl Into<String>,
    identities: &mut HashMap<HeapReference, String>,
) {
    assert!(
        identities.insert(reference, identity.into()).is_none(),
        "every intrinsic identity is registered once"
    );
}

fn reference_identity(
    reference: HeapReference,
    identities: &HashMap<HeapReference, String>,
) -> String {
    identities
        .get(&reference)
        .unwrap_or_else(|| panic!("missing intrinsic identity for {reference:?}"))
        .clone()
}

fn global_object_property(record: &ObjectRecord, name: &str) -> super::ObjectId {
    let expected = name.encode_utf16().collect::<Vec<_>>();
    let keys = record
        .try_own_key_snapshot(None, KeyPhases::ALL)
        .expect("global key snapshot allocation");
    for index in 0..keys.len() {
        let key = keys.get(index).expect("global key index").key();
        let Some(atom) = key.as_atom() else {
            continue;
        };
        if atom.description().map(string_code_units).as_ref() != Some(&expected) {
            continue;
        }
        let Some(OwnProperty::Data {
            value: StoredValue::Object(object),
            ..
        }) = record.own_property(key)
        else {
            panic!("global {name} is an intrinsic object data property");
        };
        return object;
    }
    panic!("global {name} intrinsic exists")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AtomUsage, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
        PREDEFINED_INTERNER_SLOTS,
    };

    use crate::runtime::{RealmIntrinsics, RuntimeLimits, RuntimeUsage};

    const REALM_NODES: usize = 476;
    const REALM_PROPERTIES: u64 = 1_444;
    const REALM_SNAPSHOT_FINGERPRINT: u64 = 4_963_757_688_435_116_509;

    #[test]
    fn complete_realm_snapshot_pins_the_installed_intrinsic_graph() {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let before = runtime.usage();
        let realm = runtime.create_realm().expect("realm");
        let snapshot = RealmSnapshot::capture(&runtime, realm.0.id);

        assert_eq!(before, RuntimeUsage::default());
        assert_eq!(snapshot.node_count(), REALM_NODES);
        assert_eq!(snapshot.property_count(), REALM_PROPERTIES);
        assert_eq!(runtime.usage().object_properties(), REALM_PROPERTIES);
        assert_eq!(snapshot.fingerprint(), REALM_SNAPSHOT_FINGERPRINT);
    }

    #[test]
    fn realm_snapshots_normalize_local_identity_and_preserve_isolation() {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let first = runtime.create_realm().expect("first Realm");
        let first_snapshot = RealmSnapshot::capture(&runtime, first.0.id);
        let first_atoms = runtime.atom_usage();
        let second = runtime.create_realm().expect("second Realm");
        let second_snapshot = RealmSnapshot::capture(&runtime, second.0.id);

        assert_ne!(first.0.id, second.0.id);
        let first_state = runtime.realms.get(first.0.id).expect("first state");
        let second_state = runtime.realms.get(second.0.id).expect("second state");
        assert_ne!(first_state.global_object, second_state.global_object);
        let (
            RealmIntrinsics::Ready {
                function_prototype: first_function_prototype,
                ..
            },
            RealmIntrinsics::Ready {
                function_prototype: second_function_prototype,
                ..
            },
        ) = (first_state.intrinsics, second_state.intrinsics)
        else {
            panic!("committed Realm intrinsics are ready");
        };
        assert_ne!(first_function_prototype, second_function_prototype);
        assert_eq!(first_snapshot, second_snapshot);
        assert_eq!(runtime.atom_usage(), first_atoms);
        assert_eq!(
            first_atoms,
            AtomUsage {
                live_atoms: PREDEFINED_ATOM_COUNT + 266,
                live_description_code_units: PREDEFINED_DESCRIPTION_CODE_UNITS + 2_266,
                interner_slots: PREDEFINED_INTERNER_SLOTS + 266,
            }
        );

        let first_array = match first_state.intrinsics {
            RealmIntrinsics::Ready { array, .. } => array,
            RealmIntrinsics::Initializing => unreachable!(),
        };
        let first_global = first_state.global_object;
        let second_before_mutation = RealmSnapshot::capture(&runtime, second.0.id);
        let array_key = runtime.predefined_property_key(PredefinedAtom::Array);
        let constructor_key = runtime.predefined_property_key(PredefinedAtom::Constructor);
        assert!(
            runtime
                .objects
                .get_mut(first_global)
                .expect("first global")
                .record
                .replace_existing_data(&array_key, StoredValue::Undefined)
        );
        assert!(
            runtime
                .objects
                .get_mut(first_array.prototype)
                .expect("first Array.prototype")
                .record
                .replace_existing_data(&constructor_key, StoredValue::Undefined)
        );

        assert_ne!(RealmSnapshot::capture(&runtime, first.0.id), first_snapshot);
        assert_eq!(
            RealmSnapshot::capture(&runtime, second.0.id),
            second_before_mutation
        );
    }
}
