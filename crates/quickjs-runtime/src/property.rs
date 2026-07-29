/*
 * JavaScript property descriptor representation derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Typed ordinary-property descriptors and layouts.
//!
//! This module covers structural descriptor classification and the defaults
//! used when creating a new ordinary property. Getter/setter callability
//! validation is deliberately deferred until the runtime has JavaScript values
//! and callable objects. Compatibility checks for redefining an existing
//! property are also deferred; [`PropertyDescriptor::complete_for_new_property`]
//! must not be used as a replacement for that algorithm.

use std::{error::Error, fmt};

/// Optional fields read from a JavaScript property descriptor.
///
/// `None` means the field was absent. `Some(value)` means it was present, even
/// when `value` is the runtime's JavaScript `undefined` value.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DescriptorFields<V> {
    /// The optional `value` field.
    pub value: Option<V>,
    /// The optional `writable` field after boolean conversion.
    pub writable: Option<bool>,
    /// The optional `get` field.
    pub get: Option<V>,
    /// The optional `set` field.
    pub set: Option<V>,
    /// The optional `enumerable` field after boolean conversion.
    pub enumerable: Option<bool>,
    /// The optional `configurable` field after boolean conversion.
    pub configurable: Option<bool>,
}

impl<V> DescriptorFields<V> {
    /// Creates a descriptor field set in which every field is absent.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            value: None,
            writable: None,
            get: None,
            set: None,
            enumerable: None,
            configurable: None,
        }
    }

    /// Classifies a borrowed field set without modifying it.
    ///
    /// Validation happens before any value is cloned, so a rejected mixed
    /// descriptor has no partial cloning side effect.
    ///
    /// # Errors
    ///
    /// Returns [`PropertyDescriptorError::MixedDataAndAccessorFields`] when a
    /// `get` or `set` field is present together with a `value` or `writable`
    /// field.
    pub fn classify(&self) -> Result<PropertyDescriptor<V>, PropertyDescriptorError>
    where
        V: Clone,
    {
        let kind = self.structural_kind()?;
        Ok(self.clone().into_descriptor_with_kind(kind))
    }

    /// Classifies and consumes a field set without cloning its values.
    ///
    /// # Errors
    ///
    /// Returns [`PropertyDescriptorError::MixedDataAndAccessorFields`] when a
    /// `get` or `set` field is present together with a `value` or `writable`
    /// field.
    pub fn into_descriptor(self) -> Result<PropertyDescriptor<V>, PropertyDescriptorError> {
        PropertyDescriptor::try_from_fields(self)
    }

    fn structural_kind(&self) -> Result<PropertyDescriptorKind, PropertyDescriptorError> {
        let has_value = self.value.is_some();
        let has_writable = self.writable.is_some();
        let has_get = self.get.is_some();
        let has_set = self.set.is_some();
        let has_data = has_value || has_writable;
        let has_accessor = has_get || has_set;

        if has_data && has_accessor {
            return Err(PropertyDescriptorError::MixedDataAndAccessorFields {
                has_value,
                has_writable,
                has_get,
                has_set,
            });
        }
        if has_accessor {
            Ok(PropertyDescriptorKind::Accessor)
        } else if has_data {
            Ok(PropertyDescriptorKind::Data)
        } else {
            Ok(PropertyDescriptorKind::Generic)
        }
    }

    fn into_descriptor_with_kind(self, kind: PropertyDescriptorKind) -> PropertyDescriptor<V> {
        let Self {
            value,
            writable,
            get,
            set,
            enumerable,
            configurable,
        } = self;

        match kind {
            PropertyDescriptorKind::Generic => {
                PropertyDescriptor(PropertyDescriptorRepr::Generic {
                    enumerable,
                    configurable,
                })
            }
            PropertyDescriptorKind::Data => PropertyDescriptor(PropertyDescriptorRepr::Data {
                value,
                writable,
                enumerable,
                configurable,
            }),
            PropertyDescriptorKind::Accessor => {
                PropertyDescriptor(PropertyDescriptorRepr::Accessor {
                    get,
                    set,
                    enumerable,
                    configurable,
                })
            }
        }
    }
}

impl<V> Default for DescriptorFields<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> TryFrom<DescriptorFields<V>> for PropertyDescriptor<V> {
    type Error = PropertyDescriptorError;

    fn try_from(fields: DescriptorFields<V>) -> Result<Self, Self::Error> {
        Self::try_from_fields(fields)
    }
}

/// The structural class of an incomplete property descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PropertyDescriptorKind {
    /// Neither data nor accessor fields are present.
    Generic,
    /// A `value` or `writable` field is present.
    Data,
    /// A `get` or `set` field is present.
    Accessor,
}

/// A structurally valid, possibly incomplete property descriptor.
///
/// The private representation makes mixing data and accessor fields
/// unrepresentable after classification. It also prevents callers from
/// forging a data descriptor without `value` or `writable`, or an accessor
/// descriptor without `get` or `set`. Construct descriptors through
/// [`DescriptorFields::classify`], [`DescriptorFields::into_descriptor`],
/// [`PropertyDescriptor::try_from_fields`], or [`TryFrom`].
///
/// A present getter or setter is not yet checked for callability.
///
/// Code outside the crate cannot forge a classified variant:
///
/// ```compile_fail
/// use quickjs_runtime::PropertyDescriptor;
///
/// let forged = PropertyDescriptor::<()>::Data {
///     value: None,
///     writable: None,
///     enumerable: None,
///     configurable: None,
/// };
/// ```
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct PropertyDescriptor<V>(PropertyDescriptorRepr<V>);

#[derive(Clone, Eq, Hash, PartialEq)]
enum PropertyDescriptorRepr<V> {
    Generic {
        enumerable: Option<bool>,
        configurable: Option<bool>,
    },
    Data {
        value: Option<V>,
        writable: Option<bool>,
        enumerable: Option<bool>,
        configurable: Option<bool>,
    },
    Accessor {
        get: Option<V>,
        set: Option<V>,
        enumerable: Option<bool>,
        configurable: Option<bool>,
    },
}

impl<V> PropertyDescriptor<V> {
    /// Validates and classifies an owned descriptor field set.
    ///
    /// # Errors
    ///
    /// Returns [`PropertyDescriptorError::MixedDataAndAccessorFields`] when a
    /// `get` or `set` field is present together with a `value` or `writable`
    /// field.
    pub fn try_from_fields(fields: DescriptorFields<V>) -> Result<Self, PropertyDescriptorError> {
        let kind = fields.structural_kind()?;
        Ok(fields.into_descriptor_with_kind(kind))
    }

    /// Returns the descriptor's structural class.
    #[must_use]
    pub const fn kind(&self) -> PropertyDescriptorKind {
        match &self.0 {
            PropertyDescriptorRepr::Generic { .. } => PropertyDescriptorKind::Generic,
            PropertyDescriptorRepr::Data { .. } => PropertyDescriptorKind::Data,
            PropertyDescriptorRepr::Accessor { .. } => PropertyDescriptorKind::Accessor,
        }
    }

    /// Returns the optional `value` field, or `None` for a non-data descriptor.
    ///
    /// Use [`Self::kind`] to distinguish an absent data field from a
    /// non-data descriptor. A present JavaScript `undefined` remains
    /// `Some(undefined)`.
    #[must_use]
    pub fn value(&self) -> Option<&V> {
        match &self.0 {
            PropertyDescriptorRepr::Data { value, .. } => value.as_ref(),
            PropertyDescriptorRepr::Generic { .. } | PropertyDescriptorRepr::Accessor { .. } => {
                None
            }
        }
    }

    /// Returns the optional `writable` field, or `None` for a non-data
    /// descriptor.
    ///
    /// Use [`Self::kind`] to distinguish an absent data field from a
    /// non-data descriptor.
    #[must_use]
    pub const fn writable(&self) -> Option<bool> {
        match &self.0 {
            PropertyDescriptorRepr::Data { writable, .. } => *writable,
            PropertyDescriptorRepr::Generic { .. } | PropertyDescriptorRepr::Accessor { .. } => {
                None
            }
        }
    }

    /// Returns the optional `get` field, or `None` for a non-accessor
    /// descriptor.
    ///
    /// Use [`Self::kind`] to distinguish an absent accessor field from a
    /// non-accessor descriptor. A present JavaScript `undefined` remains
    /// `Some(undefined)`.
    #[must_use]
    pub fn getter(&self) -> Option<&V> {
        match &self.0 {
            PropertyDescriptorRepr::Accessor { get, .. } => get.as_ref(),
            PropertyDescriptorRepr::Generic { .. } | PropertyDescriptorRepr::Data { .. } => None,
        }
    }

    /// Returns the optional `set` field, or `None` for a non-accessor
    /// descriptor.
    ///
    /// Use [`Self::kind`] to distinguish an absent accessor field from a
    /// non-accessor descriptor. A present JavaScript `undefined` remains
    /// `Some(undefined)`.
    #[must_use]
    pub fn setter(&self) -> Option<&V> {
        match &self.0 {
            PropertyDescriptorRepr::Accessor { set, .. } => set.as_ref(),
            PropertyDescriptorRepr::Generic { .. } | PropertyDescriptorRepr::Data { .. } => None,
        }
    }

    /// Returns the optional `enumerable` field.
    #[must_use]
    pub const fn enumerable(&self) -> Option<bool> {
        match &self.0 {
            PropertyDescriptorRepr::Generic { enumerable, .. }
            | PropertyDescriptorRepr::Data { enumerable, .. }
            | PropertyDescriptorRepr::Accessor { enumerable, .. } => *enumerable,
        }
    }

    /// Returns the optional `configurable` field.
    #[must_use]
    pub const fn configurable(&self) -> Option<bool> {
        match &self.0 {
            PropertyDescriptorRepr::Generic { configurable, .. }
            | PropertyDescriptorRepr::Data { configurable, .. }
            | PropertyDescriptorRepr::Accessor { configurable, .. } => *configurable,
        }
    }

    /// Completes this descriptor for creation of a new ordinary property.
    ///
    /// Missing attributes become `false`. A generic descriptor becomes a data
    /// descriptor. Missing data values, getters, and setters become clones of
    /// the caller-supplied JavaScript `undefined` value.
    ///
    /// This operation does not validate getter/setter callability and does not
    /// implement compatibility rules for redefining an existing property.
    #[must_use]
    pub fn complete_for_new_property(self, undefined: V) -> CompletedPropertyDescriptor<V>
    where
        V: Clone,
    {
        match self.0 {
            PropertyDescriptorRepr::Generic {
                enumerable,
                configurable,
            } => CompletedPropertyDescriptor::Data {
                value: undefined,
                writable: false,
                enumerable: enumerable.unwrap_or(false),
                configurable: configurable.unwrap_or(false),
            },
            PropertyDescriptorRepr::Data {
                value,
                writable,
                enumerable,
                configurable,
            } => CompletedPropertyDescriptor::Data {
                value: value.unwrap_or(undefined),
                writable: writable.unwrap_or(false),
                enumerable: enumerable.unwrap_or(false),
                configurable: configurable.unwrap_or(false),
            },
            PropertyDescriptorRepr::Accessor {
                get,
                set,
                enumerable,
                configurable,
            } => CompletedPropertyDescriptor::Accessor {
                get: get.unwrap_or_else(|| undefined.clone()),
                set: set.unwrap_or(undefined),
                enumerable: enumerable.unwrap_or(false),
                configurable: configurable.unwrap_or(false),
            },
        }
    }
}

impl<V: fmt::Debug> fmt::Debug for PropertyDescriptor<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            PropertyDescriptorRepr::Generic {
                enumerable,
                configurable,
            } => formatter
                .debug_struct("PropertyDescriptor")
                .field("kind", &PropertyDescriptorKind::Generic)
                .field("enumerable", enumerable)
                .field("configurable", configurable)
                .finish(),
            PropertyDescriptorRepr::Data {
                value,
                writable,
                enumerable,
                configurable,
            } => formatter
                .debug_struct("PropertyDescriptor")
                .field("kind", &PropertyDescriptorKind::Data)
                .field("value", value)
                .field("writable", writable)
                .field("enumerable", enumerable)
                .field("configurable", configurable)
                .finish(),
            PropertyDescriptorRepr::Accessor {
                get,
                set,
                enumerable,
                configurable,
            } => formatter
                .debug_struct("PropertyDescriptor")
                .field("kind", &PropertyDescriptorKind::Accessor)
                .field("get", get)
                .field("set", set)
                .field("enumerable", enumerable)
                .field("configurable", configurable)
                .finish(),
        }
    }
}

/// A completed descriptor suitable for creating a new ordinary property.
///
/// Missing fields no longer exist in this representation. The values used for
/// missing `value`, `get`, and `set` fields are supplied by the caller during
/// completion.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CompletedPropertyDescriptor<V> {
    /// A complete ordinary data descriptor.
    Data {
        /// The property value.
        value: V,
        /// Whether assignment may replace the value.
        writable: bool,
        /// Whether enumeration includes the property.
        enumerable: bool,
        /// Whether the property may be deleted or reconfigured.
        configurable: bool,
    },
    /// A complete ordinary accessor descriptor.
    Accessor {
        /// The getter value, including caller-supplied `undefined`.
        get: V,
        /// The setter value, including caller-supplied `undefined`.
        set: V,
        /// Whether enumeration includes the property.
        enumerable: bool,
        /// Whether the property may be deleted or reconfigured.
        configurable: bool,
    },
}

impl<V> CompletedPropertyDescriptor<V> {
    /// Returns the ordinary property layout kind.
    #[must_use]
    pub const fn kind(&self) -> PropertyLayoutKind {
        match self {
            Self::Data { .. } => PropertyLayoutKind::Data,
            Self::Accessor { .. } => PropertyLayoutKind::Accessor,
        }
    }

    /// Returns the data value, or `None` for an accessor descriptor.
    #[must_use]
    pub const fn value(&self) -> Option<&V> {
        match self {
            Self::Data { value, .. } => Some(value),
            Self::Accessor { .. } => None,
        }
    }

    /// Returns the getter, or `None` for a data descriptor.
    #[must_use]
    pub const fn getter(&self) -> Option<&V> {
        match self {
            Self::Accessor { get, .. } => Some(get),
            Self::Data { .. } => None,
        }
    }

    /// Returns the setter, or `None` for a data descriptor.
    #[must_use]
    pub const fn setter(&self) -> Option<&V> {
        match self {
            Self::Accessor { set, .. } => Some(set),
            Self::Data { .. } => None,
        }
    }

    /// Returns the data `writable` attribute, or `None` for an accessor.
    #[must_use]
    pub const fn writable(&self) -> Option<bool> {
        match self {
            Self::Data { writable, .. } => Some(*writable),
            Self::Accessor { .. } => None,
        }
    }

    /// Returns whether enumeration includes the property.
    #[must_use]
    pub const fn is_enumerable(&self) -> bool {
        match self {
            Self::Data { enumerable, .. } | Self::Accessor { enumerable, .. } => *enumerable,
        }
    }

    /// Returns whether the property may be deleted or reconfigured.
    #[must_use]
    pub const fn is_configurable(&self) -> bool {
        match self {
            Self::Data { configurable, .. } | Self::Accessor { configurable, .. } => *configurable,
        }
    }

    /// Returns the value-independent ordinary-property layout.
    #[must_use]
    pub const fn layout(&self) -> PropertyLayout {
        match self {
            Self::Data {
                writable,
                enumerable,
                configurable,
                ..
            } => PropertyLayout::data(*writable, *enumerable, *configurable),
            Self::Accessor {
                enumerable,
                configurable,
                ..
            } => PropertyLayout::accessor(*enumerable, *configurable),
        }
    }
}

/// The slot kind of an ordinary property layout.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PropertyLayoutKind {
    /// A slot containing a JavaScript value.
    Data,
    /// A slot containing getter and setter values.
    Accessor,
}

/// Value-independent metadata for one ordinary property slot.
///
/// The representation is private and has only data and accessor variants.
/// Accessor layouts therefore cannot carry a `writable` bit. Binding cells,
/// lazy properties, and the special array-length property intentionally have
/// no public constructors in this foundation; they require future
/// crate-private layouts paired with matching slot types.
///
/// Code outside the crate cannot assemble an accessor-plus-writable layout or
/// inspect raw flags:
///
/// ```compile_fail
/// use quickjs_runtime::{PropertyLayout, PropertyLayoutKind};
///
/// let invalid = PropertyLayout {
///     kind: PropertyLayoutKind::Accessor,
///     writable: true,
///     enumerable: false,
///     configurable: false,
/// };
/// ```
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct PropertyLayout(PropertyLayoutRepr);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum PropertyLayoutRepr {
    Data {
        writable: bool,
        enumerable: bool,
        configurable: bool,
    },
    Accessor {
        enumerable: bool,
        configurable: bool,
    },
}

impl PropertyLayout {
    /// Creates an ordinary data-property layout.
    #[must_use]
    pub const fn data(writable: bool, enumerable: bool, configurable: bool) -> Self {
        Self(PropertyLayoutRepr::Data {
            writable,
            enumerable,
            configurable,
        })
    }

    /// Creates an ordinary accessor-property layout.
    ///
    /// There is deliberately no `writable` argument.
    #[must_use]
    pub const fn accessor(enumerable: bool, configurable: bool) -> Self {
        Self(PropertyLayoutRepr::Accessor {
            enumerable,
            configurable,
        })
    }

    /// Returns the property slot kind.
    #[must_use]
    pub const fn kind(self) -> PropertyLayoutKind {
        match self.0 {
            PropertyLayoutRepr::Data { .. } => PropertyLayoutKind::Data,
            PropertyLayoutRepr::Accessor { .. } => PropertyLayoutKind::Accessor,
        }
    }

    /// Returns the data `writable` attribute, or `None` for an accessor.
    #[must_use]
    pub const fn writable(self) -> Option<bool> {
        match self.0 {
            PropertyLayoutRepr::Data { writable, .. } => Some(writable),
            PropertyLayoutRepr::Accessor { .. } => None,
        }
    }

    /// Returns whether enumeration includes the property.
    #[must_use]
    pub const fn is_enumerable(self) -> bool {
        match self.0 {
            PropertyLayoutRepr::Data { enumerable, .. }
            | PropertyLayoutRepr::Accessor { enumerable, .. } => enumerable,
        }
    }

    /// Returns whether the property may be deleted or reconfigured.
    #[must_use]
    pub const fn is_configurable(self) -> bool {
        match self.0 {
            PropertyLayoutRepr::Data { configurable, .. }
            | PropertyLayoutRepr::Accessor { configurable, .. } => configurable,
        }
    }
}

impl fmt::Debug for PropertyLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PropertyLayout")
            .field("kind", &self.kind())
            .field("writable", &self.writable())
            .field("enumerable", &self.is_enumerable())
            .field("configurable", &self.is_configurable())
            .finish()
    }
}

/// Structural property-descriptor validation failures.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum PropertyDescriptorError {
    /// A descriptor contained both data and accessor fields.
    MixedDataAndAccessorFields {
        /// Whether `value` was present.
        has_value: bool,
        /// Whether `writable` was present.
        has_writable: bool,
        /// Whether `get` was present.
        has_get: bool,
        /// Whether `set` was present.
        has_set: bool,
    },
}

impl fmt::Display for PropertyDescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MixedDataAndAccessorFields { .. } => formatter.write_str(
                "property descriptor cannot mix get/set fields with value/writable fields",
            ),
        }
    }
}

impl Error for PropertyDescriptorError {}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::{
        CompletedPropertyDescriptor, DescriptorFields, PropertyDescriptor, PropertyDescriptorError,
        PropertyDescriptorKind, PropertyLayout, PropertyLayoutKind,
    };

    #[derive(Debug, Eq, PartialEq)]
    struct NonCloneValue(u8);

    #[test]
    fn consuming_classification_does_not_require_clone() {
        let descriptor = PropertyDescriptor::try_from_fields(DescriptorFields {
            value: Some(NonCloneValue(3)),
            ..DescriptorFields::new()
        })
        .expect("data descriptor");

        assert_eq!(descriptor.kind(), PropertyDescriptorKind::Data);
        assert_eq!(descriptor.value(), Some(&NonCloneValue(3)));
        assert_eq!(descriptor.writable(), None);
        assert_eq!(descriptor.getter(), None);
        assert_eq!(descriptor.setter(), None);
    }

    #[test]
    fn rejected_borrowed_classification_does_not_clone_values() {
        #[derive(Debug)]
        struct CloneTracked(Rc<Cell<u32>>);

        impl Clone for CloneTracked {
            fn clone(&self) -> Self {
                self.0.set(self.0.get() + 1);
                Self(Rc::clone(&self.0))
            }
        }

        let clone_count = Rc::new(Cell::new(0));
        let fields = DescriptorFields {
            value: Some(CloneTracked(Rc::clone(&clone_count))),
            get: Some(CloneTracked(Rc::clone(&clone_count))),
            ..DescriptorFields::new()
        };

        assert!(matches!(
            fields.classify(),
            Err(PropertyDescriptorError::MixedDataAndAccessorFields { .. })
        ));
        assert_eq!(clone_count.get(), 0);
    }

    #[test]
    fn generic_and_missing_accessor_fields_receive_undefined() {
        let completed = DescriptorFields::<u8> {
            get: None,
            set: None,
            enumerable: Some(true),
            configurable: Some(true),
            value: None,
            writable: None,
        }
        .into_descriptor()
        .expect("generic descriptor")
        .complete_for_new_property(9);

        assert_eq!(
            completed,
            CompletedPropertyDescriptor::Data {
                value: 9,
                writable: false,
                enumerable: true,
                configurable: true,
            }
        );

        let accessor = DescriptorFields::<u8> {
            get: None,
            set: Some(4),
            ..DescriptorFields::new()
        }
        .into_descriptor()
        .expect("accessor descriptor")
        .complete_for_new_property(9);
        assert_eq!(
            accessor,
            CompletedPropertyDescriptor::Accessor {
                get: 9,
                set: 4,
                enumerable: false,
                configurable: false,
            }
        );
    }

    #[test]
    fn completed_descriptor_getters_and_layout_agree() {
        let data = CompletedPropertyDescriptor::Data {
            value: 5_u8,
            writable: true,
            enumerable: false,
            configurable: true,
        };
        assert_eq!(data.value(), Some(&5));
        assert_eq!(data.getter(), None);
        assert_eq!(data.setter(), None);
        assert_eq!(data.writable(), Some(true));
        assert!(!data.is_enumerable());
        assert!(data.is_configurable());
        assert_eq!(data.kind(), PropertyLayoutKind::Data);
        assert_eq!(data.layout(), PropertyLayout::data(true, false, true));

        let accessor = CompletedPropertyDescriptor::Accessor {
            get: 6_u8,
            set: 7,
            enumerable: true,
            configurable: false,
        };
        assert_eq!(accessor.value(), None);
        assert_eq!(accessor.getter(), Some(&6));
        assert_eq!(accessor.setter(), Some(&7));
        assert_eq!(accessor.writable(), None);
        assert!(accessor.is_enumerable());
        assert!(!accessor.is_configurable());
        assert_eq!(accessor.kind(), PropertyLayoutKind::Accessor);
        assert_eq!(accessor.layout(), PropertyLayout::accessor(true, false));
    }
}
