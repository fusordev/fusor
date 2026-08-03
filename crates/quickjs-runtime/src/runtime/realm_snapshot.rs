//! Normalized installed-Realm snapshots for bootstrap regression tests.

use std::collections::{HashMap, VecDeque};

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
    kind: NodeKindSnapshot,
    prototype: Option<usize>,
    extensible: bool,
    properties: Vec<PropertySnapshot>,
}

#[derive(Debug, Eq, PartialEq)]
enum NodeKindSnapshot {
    OrdinaryObject,
    ArrayObject {
        length: u32,
    },
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
        getter: Option<usize>,
        setter: Option<usize>,
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
    Reference(usize),
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
    pub(super) fn capture(runtime: &Runtime, realm: RealmId) -> Self {
        let state = runtime.realms.get(realm).expect("snapshot Realm is live");
        let mut indices = HashMap::new();
        let mut pending = VecDeque::new();
        register_reference(
            HeapReference::Object(state.global_object),
            &mut indices,
            &mut pending,
        );
        register_reference(
            HeapReference::Object(state.object_prototype),
            &mut indices,
            &mut pending,
        );
        let RealmIntrinsics::Ready {
            function_prototype,
            throw_type_error,
            function_constructor,
            errors,
            boolean,
            number,
            bigint,
            string,
            array,
            symbol,
            iterators,
        } = state.intrinsics
        else {
            panic!("snapshot Realm intrinsics are ready");
        };
        for function in [
            function_prototype,
            throw_type_error,
            function_constructor,
            errors.to_string,
            errors.is_error,
            boolean.constructor,
            number.constructor,
            bigint.constructor,
            string.constructor,
            array.constructor,
            symbol.constructor,
            iterators.array_values,
        ] {
            register_reference(
                HeapReference::Function(function),
                &mut indices,
                &mut pending,
            );
        }
        for object in [
            boolean.prototype,
            number.prototype,
            bigint.prototype,
            string.prototype,
            array.prototype,
            symbol.prototype,
            iterators.iterator_prototype,
            iterators.array_iterator_prototype,
            iterators.string_iterator_prototype,
        ] {
            register_reference(HeapReference::Object(object), &mut indices, &mut pending);
        }
        for kind in ErrorIntrinsicKind::ALL {
            let intrinsic = errors.intrinsic(kind);
            register_reference(
                HeapReference::Object(intrinsic.prototype),
                &mut indices,
                &mut pending,
            );
            register_reference(
                HeapReference::Function(intrinsic.constructor),
                &mut indices,
                &mut pending,
            );
        }
        let mut nodes = Vec::new();

        while let Some(reference) = pending.pop_front() {
            let expected_index = nodes.len();
            assert_eq!(indices.get(&reference), Some(&expected_index));
            nodes.push(snapshot_node(
                runtime,
                realm,
                reference,
                &mut indices,
                &mut pending,
            ));
        }

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
    indices: &mut HashMap<HeapReference, usize>,
    pending: &mut VecDeque<HeapReference>,
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
        .map(|prototype| register_reference(prototype, indices, pending));
    let properties = snapshot_properties(record, indices, pending);
    NodeSnapshot {
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
    indices: &mut HashMap<HeapReference, usize>,
    pending: &mut VecDeque<HeapReference>,
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
                    PropertyValueSnapshot::Data(snapshot_value(&value, indices, pending))
                }
                OwnProperty::Accessor { getter, setter, .. } => PropertyValueSnapshot::Accessor {
                    getter: getter.map(|id| {
                        register_reference(HeapReference::Function(id), indices, pending)
                    }),
                    setter: setter.map(|id| {
                        register_reference(HeapReference::Function(id), indices, pending)
                    }),
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
    indices: &mut HashMap<HeapReference, usize>,
    pending: &mut VecDeque<HeapReference>,
) -> ValueSnapshot {
    match value {
        StoredValue::Undefined => ValueSnapshot::Undefined,
        StoredValue::Null => ValueSnapshot::Null,
        StoredValue::Boolean(value) => ValueSnapshot::Boolean(*value),
        StoredValue::Number(value) => ValueSnapshot::Number(value.as_f64().to_bits()),
        StoredValue::BigInt(value) => ValueSnapshot::BigInt(bigint_decimal(value)),
        StoredValue::String(value) => ValueSnapshot::String(string_code_units(value)),
        StoredValue::Symbol(value) => ValueSnapshot::Symbol(snapshot_atom(value)),
        StoredValue::Function(id) => ValueSnapshot::Reference(register_reference(
            HeapReference::Function(*id),
            indices,
            pending,
        )),
        StoredValue::Object(id) => ValueSnapshot::Reference(register_reference(
            HeapReference::Object(*id),
            indices,
            pending,
        )),
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

fn register_reference(
    reference: HeapReference,
    indices: &mut HashMap<HeapReference, usize>,
    pending: &mut VecDeque<HeapReference>,
) -> usize {
    if let Some(index) = indices.get(&reference) {
        return *index;
    }
    let index = indices.len();
    indices.insert(reference, index);
    pending.push_back(reference);
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AtomUsage, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
        PREDEFINED_INTERNER_SLOTS,
    };

    use crate::runtime::{RealmIntrinsics, RuntimeLimits, RuntimeUsage};

    const REALM_NODES: usize = 242;
    const REALM_PROPERTIES: u64 = 757;
    const REALM_SNAPSHOT_FINGERPRINT: u64 = 8_747_040_734_372_787_780;

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
                live_atoms: PREDEFINED_ATOM_COUNT + 159,
                live_description_code_units: PREDEFINED_DESCRIPTION_CODE_UNITS + 1_232,
                interner_slots: PREDEFINED_INTERNER_SLOTS + 159,
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
