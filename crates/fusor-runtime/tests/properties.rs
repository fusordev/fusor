use std::{
    collections::HashSet,
    error::Error,
    hash::{DefaultHasher, Hash, Hasher},
};

use fusor_runtime::{
    CompletedPropertyDescriptor, DescriptorFields, PropertyDescriptorError, PropertyDescriptorKind,
    PropertyLayout, PropertyLayoutKind,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum TestValue {
    Undefined,
    Value(u8),
    Getter,
    Setter,
}

#[test]
fn absent_fields_remain_distinct_from_present_undefined_values() {
    let absent = DescriptorFields::<TestValue>::new();
    let absent = absent.classify().expect("generic descriptor");
    assert_eq!(absent.kind(), PropertyDescriptorKind::Generic);
    assert_eq!(absent.value(), None);
    assert_eq!(absent.writable(), None);
    assert_eq!(absent.getter(), None);
    assert_eq!(absent.setter(), None);

    let present_value = DescriptorFields {
        value: Some(TestValue::Undefined),
        ..DescriptorFields::new()
    };
    let data = present_value.classify().expect("data descriptor");
    assert_eq!(data.kind(), PropertyDescriptorKind::Data);
    assert_eq!(data.value(), Some(&TestValue::Undefined));
    assert_eq!(data.writable(), None);
    assert_eq!(data.getter(), None);
    assert_eq!(data.setter(), None);
    assert_eq!(
        data,
        present_value.classify().expect("equal data descriptor")
    );
    assert_eq!(
        format!("{data:?}"),
        "PropertyDescriptor { kind: Data, value: Some(Undefined), writable: None, enumerable: None, configurable: None }"
    );

    let present_getter = DescriptorFields {
        get: Some(TestValue::Undefined),
        ..DescriptorFields::new()
    };
    let accessor = present_getter.classify().expect("accessor descriptor");
    assert_eq!(accessor.kind(), PropertyDescriptorKind::Accessor);
    assert_eq!(accessor.value(), None);
    assert_eq!(accessor.writable(), None);
    assert_eq!(accessor.getter(), Some(&TestValue::Undefined));
    assert_eq!(accessor.setter(), None);
}

#[test]
fn every_structural_descriptor_class_is_detected() {
    let generic = DescriptorFields {
        enumerable: Some(true),
        configurable: Some(false),
        ..DescriptorFields::<TestValue>::new()
    };
    assert_eq!(
        generic.classify().expect("generic").kind(),
        PropertyDescriptorKind::Generic
    );

    for data in [
        DescriptorFields {
            value: Some(TestValue::Value(1)),
            ..DescriptorFields::new()
        },
        DescriptorFields {
            writable: Some(true),
            ..DescriptorFields::new()
        },
        DescriptorFields {
            value: Some(TestValue::Value(1)),
            writable: Some(false),
            ..DescriptorFields::new()
        },
    ] {
        assert_eq!(
            data.classify().expect("data").kind(),
            PropertyDescriptorKind::Data
        );
    }

    for accessor in [
        DescriptorFields {
            get: Some(TestValue::Getter),
            ..DescriptorFields::new()
        },
        DescriptorFields {
            set: Some(TestValue::Setter),
            ..DescriptorFields::new()
        },
        DescriptorFields {
            get: Some(TestValue::Getter),
            set: Some(TestValue::Setter),
            ..DescriptorFields::new()
        },
    ] {
        assert_eq!(
            accessor.classify().expect("accessor").kind(),
            PropertyDescriptorKind::Accessor
        );
    }
}

#[test]
fn every_data_accessor_presence_subset_is_rejected_transactionally() {
    let nonempty_data_subsets = [(true, false), (false, true), (true, true)];
    let nonempty_accessor_subsets = [(true, false), (false, true), (true, true)];
    let mut cases = 0;

    for (has_value, has_writable) in nonempty_data_subsets {
        for (has_get, has_set) in nonempty_accessor_subsets {
            let fields = DescriptorFields {
                value: has_value.then_some(TestValue::Value(1)),
                writable: has_writable.then_some(false),
                get: has_get.then_some(TestValue::Getter),
                set: has_set.then_some(TestValue::Setter),
                enumerable: Some(true),
                configurable: Some(false),
            };
            let before = fields.clone();
            assert_eq!(
                fields.classify(),
                Err(PropertyDescriptorError::MixedDataAndAccessorFields {
                    has_value,
                    has_writable,
                    has_get,
                    has_set,
                })
            );
            assert_eq!(fields, before);
            cases += 1;
        }
    }
    assert_eq!(cases, 9);
}

#[test]
fn new_property_completion_applies_exact_defaults_and_preserves_explicit_fields() {
    let defaults = DescriptorFields::<TestValue>::new()
        .into_descriptor()
        .expect("generic")
        .complete_for_new_property(TestValue::Undefined);
    assert_eq!(
        defaults,
        CompletedPropertyDescriptor::Data {
            value: TestValue::Undefined,
            writable: false,
            enumerable: false,
            configurable: false,
        }
    );

    let generic = DescriptorFields::<TestValue> {
        enumerable: Some(true),
        ..DescriptorFields::new()
    }
    .into_descriptor()
    .expect("generic")
    .complete_for_new_property(TestValue::Undefined);
    assert_eq!(
        generic,
        CompletedPropertyDescriptor::Data {
            value: TestValue::Undefined,
            writable: false,
            enumerable: true,
            configurable: false,
        }
    );

    let data_without_value = DescriptorFields::<TestValue> {
        writable: Some(true),
        ..DescriptorFields::new()
    }
    .into_descriptor()
    .expect("data")
    .complete_for_new_property(TestValue::Undefined);
    assert_eq!(
        data_without_value,
        CompletedPropertyDescriptor::Data {
            value: TestValue::Undefined,
            writable: true,
            enumerable: false,
            configurable: false,
        }
    );

    let data = DescriptorFields {
        value: Some(TestValue::Value(7)),
        writable: Some(true),
        configurable: Some(true),
        ..DescriptorFields::new()
    }
    .into_descriptor()
    .expect("data")
    .complete_for_new_property(TestValue::Undefined);
    assert_eq!(
        data,
        CompletedPropertyDescriptor::Data {
            value: TestValue::Value(7),
            writable: true,
            enumerable: false,
            configurable: true,
        }
    );

    let accessor = DescriptorFields {
        get: Some(TestValue::Getter),
        enumerable: Some(true),
        ..DescriptorFields::new()
    }
    .into_descriptor()
    .expect("accessor")
    .complete_for_new_property(TestValue::Undefined);
    assert_eq!(
        accessor,
        CompletedPropertyDescriptor::Accessor {
            get: TestValue::Getter,
            set: TestValue::Undefined,
            enumerable: true,
            configurable: false,
        }
    );
}

#[test]
fn layouts_expose_only_valid_ordinary_property_combinations() {
    let data = PropertyLayout::data(true, false, true);
    assert_eq!(data.kind(), PropertyLayoutKind::Data);
    assert_eq!(data.writable(), Some(true));
    assert!(!data.is_enumerable());
    assert!(data.is_configurable());

    let accessor = PropertyLayout::accessor(true, false);
    assert_eq!(accessor.kind(), PropertyLayoutKind::Accessor);
    assert_eq!(accessor.writable(), None);
    assert!(accessor.is_enumerable());
    assert!(!accessor.is_configurable());

    let layouts = HashSet::from([
        data,
        PropertyLayout::data(true, false, true),
        PropertyLayout::data(false, false, true),
        PropertyLayout::accessor(false, true),
        PropertyLayout::accessor(true, false),
    ]);
    assert_eq!(layouts.len(), 4);
    assert_eq!(hash(data), hash(PropertyLayout::data(true, false, true)));
}

#[test]
fn mixed_descriptor_errors_have_stable_structured_details() {
    let fields = DescriptorFields {
        value: Some(TestValue::Value(1)),
        writable: Some(false),
        get: Some(TestValue::Getter),
        set: None,
        enumerable: None,
        configurable: None,
    };

    let error = fields.into_descriptor().expect_err("mixed descriptor");
    assert_eq!(
        error,
        PropertyDescriptorError::MixedDataAndAccessorFields {
            has_value: true,
            has_writable: true,
            has_get: true,
            has_set: false,
        }
    );
    assert_eq!(
        error.to_string(),
        "property descriptor cannot mix get/set fields with value/writable fields"
    );
    assert!(error.source().is_none());
}

fn hash(value: PropertyLayout) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
