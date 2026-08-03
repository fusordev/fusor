/*
 * JavaScript property definition semantics derived from QuickJS.
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

//! `ValidateAndApplyPropertyDescriptor` over runtime-owned property slots.
//!
//! This module owns the descriptor-compatibility decision that
//! [`crate::PropertyDescriptor`] deliberately leaves to the runtime: whether a
//! requested descriptor may replace an existing own property, and what the
//! resulting slot looks like. It is pure. Callers perform the object mutation,
//! which keeps the decision testable in isolation and lets exotic objects reuse
//! it after their own pre-checks.

use crate::{
    ids::FunctionId,
    object::OwnProperty,
    property::{PropertyLayout, PropertyLayoutKind},
    value::StoredValue,
};

/// One requested descriptor field.
///
/// `Absent` and `Present` are distinguished because ECMAScript treats a field
/// that is missing differently from a field explicitly set to `undefined`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Requested<T> {
    Absent,
    Present(T),
}

impl<T> Requested<T> {
    /// Returns the requested value, or `default` when the field is absent.
    fn unwrap_or(self, default: T) -> T {
        match self {
            Self::Absent => default,
            Self::Present(value) => value,
        }
    }

    const fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }
}

/// A partially specified property descriptor addressed to one own property.
///
/// The accessor and data field groups are mutually exclusive by construction:
/// a descriptor is built through [`PropertyDefinition::data`] or
/// [`PropertyDefinition::accessor`], so the "mixed data and accessor fields"
/// rejection happens before this type exists.
pub(crate) struct PropertyDefinition {
    fields: DefinitionFields,
    enumerable: Requested<bool>,
    configurable: Requested<bool>,
}

#[allow(
    dead_code,
    reason = "the generic and accessor field groups are required by Object.defineProperty, whose resumable descriptor read is a separate milestone"
)]
enum DefinitionFields {
    /// Neither a value/writable nor a get/set field was supplied.
    Generic,
    Data {
        value: Requested<StoredValue>,
        writable: Requested<bool>,
    },
    Accessor {
        getter: Requested<Option<FunctionId>>,
        setter: Requested<Option<FunctionId>>,
    },
}

#[allow(
    dead_code,
    reason = "the generic and accessor constructors are required by Object.defineProperty, whose resumable descriptor read is a separate milestone"
)]
impl PropertyDefinition {
    /// Creates a descriptor carrying neither data nor accessor fields.
    pub(crate) const fn generic() -> Self {
        Self {
            fields: DefinitionFields::Generic,
            enumerable: Requested::Absent,
            configurable: Requested::Absent,
        }
    }

    /// Creates a data descriptor.
    pub(crate) const fn data(value: Requested<StoredValue>, writable: Requested<bool>) -> Self {
        Self {
            fields: DefinitionFields::Data { value, writable },
            enumerable: Requested::Absent,
            configurable: Requested::Absent,
        }
    }

    /// Creates an accessor descriptor.
    pub(crate) const fn accessor(
        getter: Requested<Option<FunctionId>>,
        setter: Requested<Option<FunctionId>>,
    ) -> Self {
        Self {
            fields: DefinitionFields::Accessor { getter, setter },
            enumerable: Requested::Absent,
            configurable: Requested::Absent,
        }
    }

    /// Sets the requested `enumerable` attribute.
    #[must_use]
    pub(crate) const fn with_enumerable(mut self, enumerable: Requested<bool>) -> Self {
        self.enumerable = enumerable;
        self
    }

    /// Sets the requested `configurable` attribute.
    #[must_use]
    pub(crate) const fn with_configurable(mut self, configurable: Requested<bool>) -> Self {
        self.configurable = configurable;
        self
    }

    const fn is_accessor(&self) -> bool {
        matches!(self.fields, DefinitionFields::Accessor { .. })
    }

    /// Returns whether any value, writable, get, or set field is present.
    const fn has_slot_fields(&self) -> bool {
        match &self.fields {
            DefinitionFields::Generic => false,
            DefinitionFields::Data { value, writable } => {
                value.is_present() || writable.is_present()
            }
            DefinitionFields::Accessor { getter, setter } => {
                getter.is_present() || setter.is_present()
            }
        }
    }

    /// Returns whether this data descriptor carries a value that an exotic
    /// object must validate before ordinary descriptor application.
    pub(crate) const fn has_present_data_value(&self) -> bool {
        matches!(
            self.fields,
            DefinitionFields::Data {
                value: Requested::Present(_),
                ..
            }
        )
    }

    /// Returns the requested data value, when that field is present.
    pub(crate) const fn present_data_value(&self) -> Option<&StoredValue> {
        match &self.fields {
            DefinitionFields::Data {
                value: Requested::Present(value),
                ..
            } => Some(value),
            DefinitionFields::Generic
            | DefinitionFields::Data {
                value: Requested::Absent,
                ..
            }
            | DefinitionFields::Accessor { .. } => None,
        }
    }

    /// Returns the requested writable attribute for a data descriptor.
    pub(crate) const fn requested_writable(&self) -> Option<bool> {
        match &self.fields {
            DefinitionFields::Data {
                writable: Requested::Present(writable),
                ..
            } => Some(*writable),
            DefinitionFields::Generic
            | DefinitionFields::Data {
                writable: Requested::Absent,
                ..
            }
            | DefinitionFields::Accessor { .. } => None,
        }
    }

    /// Returns whether this is an accessor descriptor.
    pub(crate) const fn is_accessor_descriptor(&self) -> bool {
        self.is_accessor()
    }
}

/// The result of validating a descriptor against an object's current state.
pub(crate) enum DefinitionDecision {
    /// The descriptor is incompatible with the existing property.
    ///
    /// Callers report `TypeError: property is not configurable` in strict mode
    /// and `false` otherwise, matching `JS_ThrowTypeErrorOrFalse`
    /// (`quickjs.c:10384`).
    Rejected,
    /// The descriptor requests no observable change; the object must not be
    /// mutated at all.
    Unchanged,
    /// The property must be created.
    Create(OwnProperty),
    /// The property must be replaced with this slot.
    Replace(OwnProperty),
}

/// Applies ECMAScript `ValidateAndApplyPropertyDescriptor` for a property that
/// does not yet exist.
///
/// Absent fields take their ECMAScript defaults: `undefined` value and
/// `false` for every attribute. A non-extensible object rejects.
pub(crate) fn validate_and_apply_new(
    definition: &PropertyDefinition,
    extensible: bool,
) -> DefinitionDecision {
    if !extensible {
        return DefinitionDecision::Rejected;
    }
    let enumerable = definition.enumerable.unwrap_or(false);
    let configurable = definition.configurable.unwrap_or(false);
    match &definition.fields {
        DefinitionFields::Accessor { getter, setter } => {
            DefinitionDecision::Create(OwnProperty::Accessor {
                layout: PropertyLayout::accessor(enumerable, configurable),
                getter: getter.unwrap_or(None),
                setter: setter.unwrap_or(None),
            })
        }
        // A generic descriptor creates a data property whose value is
        // `undefined`, exactly like an all-defaults data descriptor.
        DefinitionFields::Generic => DefinitionDecision::Create(OwnProperty::Data {
            layout: PropertyLayout::data(false, enumerable, configurable),
            value: StoredValue::Undefined,
        }),
        DefinitionFields::Data { value, writable } => {
            DefinitionDecision::Create(OwnProperty::Data {
                layout: PropertyLayout::data(writable.unwrap_or(false), enumerable, configurable),
                value: match value {
                    Requested::Absent => StoredValue::Undefined,
                    Requested::Present(value) => value.duplicate(),
                },
            })
        }
    }
}

/// Applies ECMAScript `ValidateAndApplyPropertyDescriptor` for an existing
/// property.
///
/// This mirrors the pinned `check_define_prop_flags` (`quickjs.c:10272`)
/// rejection rules followed by its slot update (`quickjs.c:10381-10530`):
///
/// - a non-configurable property rejects a request to become configurable,
///   to change `enumerable`, or to change between data and accessor kinds;
/// - a non-configurable, non-writable data property rejects a request to
///   become writable, and rejects a new value unless it is `SameValue`;
/// - every other field request is applied, leaving unspecified fields at their
///   current values.
pub(crate) fn validate_and_apply_existing(
    definition: &PropertyDefinition,
    existing: &OwnProperty,
) -> DefinitionDecision {
    let current = existing.layout();
    if !current.is_configurable() {
        if definition.configurable.unwrap_or(false) {
            return DefinitionDecision::Rejected;
        }
        if let Requested::Present(enumerable) = definition.enumerable
            && enumerable != current.is_enumerable()
        {
            return DefinitionDecision::Rejected;
        }
        if definition.has_slot_fields() {
            let is_accessor = current.kind() == PropertyLayoutKind::Accessor;
            if definition.is_accessor() != is_accessor {
                return DefinitionDecision::Rejected;
            }
            if !is_accessor
                && current.writable() != Some(true)
                && let DefinitionFields::Data { writable, .. } = &definition.fields
                && writable.unwrap_or(false)
            {
                return DefinitionDecision::Rejected;
            }
        }
    }

    let enumerable = definition.enumerable.unwrap_or(current.is_enumerable());
    let configurable = definition.configurable.unwrap_or(current.is_configurable());

    match (&definition.fields, existing) {
        // Attribute-only change on either kind of property.
        (DefinitionFields::Generic, OwnProperty::Data { value, layout }) => {
            let writable = layout.writable() == Some(true);
            decide_data(
                existing,
                PropertyLayout::data(writable, enumerable, configurable),
                value.duplicate(),
            )
        }
        (DefinitionFields::Generic, OwnProperty::Accessor { getter, setter, .. }) => {
            decide_accessor(
                existing,
                PropertyLayout::accessor(enumerable, configurable),
                *getter,
                *setter,
            )
        }

        // Data descriptor against a data property: unspecified fields keep
        // their current values.
        (
            DefinitionFields::Data {
                value: requested_value,
                writable: requested_writable,
            },
            OwnProperty::Data { value, layout },
        ) => {
            let writable = requested_writable.unwrap_or(layout.writable() == Some(true));
            let next = match requested_value {
                Requested::Absent => value.duplicate(),
                Requested::Present(requested) => {
                    // A non-configurable, non-writable data property accepts a
                    // SameValue rewrite as a no-op and rejects anything else.
                    // The rejection is already handled above only for the
                    // writable request, so the value check happens here.
                    if !layout.is_configurable()
                        && layout.writable() != Some(true)
                        && !requested.same_value(value)
                    {
                        return DefinitionDecision::Rejected;
                    }
                    requested.duplicate()
                }
            };
            decide_data(
                existing,
                PropertyLayout::data(writable, enumerable, configurable),
                next,
            )
        }

        // Data descriptor replacing an accessor property. Reachable only when
        // the property is configurable, because the kind change is rejected
        // above otherwise. Unspecified fields take ECMAScript defaults rather
        // than the accessor's, since an accessor has no value or writable.
        (DefinitionFields::Data { value, writable }, OwnProperty::Accessor { .. }) => {
            DefinitionDecision::Replace(OwnProperty::Data {
                layout: PropertyLayout::data(writable.unwrap_or(false), enumerable, configurable),
                value: match value {
                    Requested::Absent => StoredValue::Undefined,
                    Requested::Present(value) => value.duplicate(),
                },
            })
        }

        // Accessor descriptor against an accessor property: an absent get or
        // set field keeps the current function.
        (
            DefinitionFields::Accessor {
                getter: new_get,
                setter: new_set,
            },
            OwnProperty::Accessor { getter, setter, .. },
        ) => decide_accessor(
            existing,
            PropertyLayout::accessor(enumerable, configurable),
            new_get.unwrap_or(*getter),
            new_set.unwrap_or(*setter),
        ),

        // Accessor descriptor replacing a data property.
        (DefinitionFields::Accessor { getter, setter }, OwnProperty::Data { .. }) => {
            DefinitionDecision::Replace(OwnProperty::Accessor {
                layout: PropertyLayout::accessor(enumerable, configurable),
                getter: getter.unwrap_or(None),
                setter: setter.unwrap_or(None),
            })
        }
    }
}

/// Returns `Unchanged` when the requested data slot already matches, so a
/// no-op define never marks the heap as mutated.
fn decide_data(
    existing: &OwnProperty,
    layout: PropertyLayout,
    value: StoredValue,
) -> DefinitionDecision {
    if let OwnProperty::Data {
        layout: current,
        value: current_value,
    } = existing
        && *current == layout
        && current_value.same_value(&value)
    {
        return DefinitionDecision::Unchanged;
    }
    DefinitionDecision::Replace(OwnProperty::Data { layout, value })
}

/// Returns `Unchanged` when the requested accessor slot already matches.
fn decide_accessor(
    existing: &OwnProperty,
    layout: PropertyLayout,
    getter: Option<FunctionId>,
    setter: Option<FunctionId>,
) -> DefinitionDecision {
    if let OwnProperty::Accessor {
        layout: current,
        getter: current_getter,
        setter: current_setter,
    } = existing
        && *current == layout
        && *current_getter == getter
        && *current_setter == setter
    {
        return DefinitionDecision::Unchanged;
    }
    DefinitionDecision::Replace(OwnProperty::Accessor {
        layout,
        getter,
        setter,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        DefinitionDecision, PropertyDefinition, Requested, validate_and_apply_existing,
        validate_and_apply_new,
    };
    use crate::{
        JsNumber,
        object::OwnProperty,
        property::{PropertyLayout, PropertyLayoutKind},
        value::StoredValue,
    };

    fn number(value: f64) -> StoredValue {
        StoredValue::Number(JsNumber::from_f64(value))
    }

    fn data(
        value: StoredValue,
        writable: bool,
        enumerable: bool,
        configurable: bool,
    ) -> OwnProperty {
        OwnProperty::Data {
            layout: PropertyLayout::data(writable, enumerable, configurable),
            value,
        }
    }

    fn accessor(enumerable: bool, configurable: bool) -> OwnProperty {
        OwnProperty::Accessor {
            layout: PropertyLayout::accessor(enumerable, configurable),
            getter: None,
            setter: None,
        }
    }

    fn created(decision: DefinitionDecision) -> OwnProperty {
        match decision {
            DefinitionDecision::Create(property) => property,
            DefinitionDecision::Rejected
            | DefinitionDecision::Unchanged
            | DefinitionDecision::Replace(_) => panic!("expected a creation"),
        }
    }

    fn replaced(decision: DefinitionDecision) -> OwnProperty {
        match decision {
            DefinitionDecision::Replace(property) => property,
            DefinitionDecision::Rejected
            | DefinitionDecision::Unchanged
            | DefinitionDecision::Create(_) => panic!("expected a replacement"),
        }
    }

    /// The pinned oracle reports `data value=undefined w=false e=false c=false`
    /// for `Object.defineProperty(o, "x", {})`.
    #[test]
    fn an_empty_descriptor_creates_an_undefined_non_writable_data_property() {
        let decision = validate_and_apply_new(&PropertyDefinition::generic(), true);
        let OwnProperty::Data { layout, value } = created(decision) else {
            panic!("expected a data property");
        };
        assert!(matches!(value, StoredValue::Undefined));
        assert_eq!(layout, PropertyLayout::data(false, false, false));
    }

    /// The oracle reports `accessor get=fn set=undefined e=false c=false`.
    #[test]
    fn a_getter_only_descriptor_creates_a_non_enumerable_accessor() {
        let definition = PropertyDefinition::accessor(Requested::Present(None), Requested::Absent);
        let OwnProperty::Accessor { layout, .. } =
            created(validate_and_apply_new(&definition, true))
        else {
            panic!("expected an accessor property");
        };
        assert_eq!(layout.kind(), PropertyLayoutKind::Accessor);
        assert_eq!(layout, PropertyLayout::accessor(false, false));
    }

    /// The oracle throws `TypeError: object is not extensible`.
    #[test]
    fn a_non_extensible_object_rejects_a_new_property() {
        let definition =
            PropertyDefinition::data(Requested::Present(number(1.0)), Requested::Absent);
        assert!(matches!(
            validate_and_apply_new(&definition, false),
            DefinitionDecision::Rejected
        ));
    }

    #[test]
    fn a_non_configurable_property_rejects_becoming_configurable() {
        let definition = PropertyDefinition::generic().with_configurable(Requested::Present(true));
        assert!(matches!(
            validate_and_apply_existing(&definition, &data(number(1.0), false, false, false)),
            DefinitionDecision::Rejected
        ));
    }

    #[test]
    fn a_non_configurable_property_rejects_an_enumerable_change_but_accepts_a_match() {
        let existing = data(number(1.0), false, false, false);
        let changed = PropertyDefinition::generic().with_enumerable(Requested::Present(true));
        assert!(matches!(
            validate_and_apply_existing(&changed, &existing),
            DefinitionDecision::Rejected
        ));

        let same = PropertyDefinition::generic().with_enumerable(Requested::Present(false));
        assert!(matches!(
            validate_and_apply_existing(&same, &existing),
            DefinitionDecision::Unchanged
        ));
    }

    #[test]
    fn a_non_configurable_non_writable_property_rejects_becoming_writable() {
        let definition = PropertyDefinition::data(Requested::Absent, Requested::Present(true));
        assert!(matches!(
            validate_and_apply_existing(&definition, &data(number(1.0), false, false, false)),
            DefinitionDecision::Rejected
        ));
    }

    /// A `SameValue` rewrite is a no-op; a different value rejects. The oracle
    /// accepts `NaN` over `NaN` and rejects `-0` over `0`.
    #[test]
    fn a_frozen_data_property_accepts_only_a_same_value_rewrite() {
        let existing = data(number(1.0), false, false, false);
        let same = PropertyDefinition::data(Requested::Present(number(1.0)), Requested::Absent);
        assert!(matches!(
            validate_and_apply_existing(&same, &existing),
            DefinitionDecision::Unchanged
        ));

        let different =
            PropertyDefinition::data(Requested::Present(number(2.0)), Requested::Absent);
        assert!(matches!(
            validate_and_apply_existing(&different, &existing),
            DefinitionDecision::Rejected
        ));

        let nan = data(number(f64::NAN), false, false, false);
        let same_nan =
            PropertyDefinition::data(Requested::Present(number(f64::NAN)), Requested::Absent);
        assert!(matches!(
            validate_and_apply_existing(&same_nan, &nan),
            DefinitionDecision::Unchanged
        ));

        let positive_zero = data(number(0.0), false, false, false);
        let negative_zero =
            PropertyDefinition::data(Requested::Present(number(-0.0)), Requested::Absent);
        assert!(matches!(
            validate_and_apply_existing(&negative_zero, &positive_zero),
            DefinitionDecision::Rejected
        ));
    }

    #[test]
    fn a_non_configurable_property_rejects_a_kind_change_in_either_direction() {
        let to_accessor = PropertyDefinition::accessor(Requested::Present(None), Requested::Absent);
        assert!(matches!(
            validate_and_apply_existing(&to_accessor, &data(number(1.0), true, false, false)),
            DefinitionDecision::Rejected
        ));

        let to_data = PropertyDefinition::data(Requested::Present(number(1.0)), Requested::Absent);
        assert!(matches!(
            validate_and_apply_existing(&to_data, &accessor(false, false)),
            DefinitionDecision::Rejected
        ));
    }

    /// The oracle reports `data value=2 w=true e=false c=false`: a writable but
    /// non-configurable property accepts a new value.
    #[test]
    fn a_writable_non_configurable_property_accepts_a_new_value() {
        let definition =
            PropertyDefinition::data(Requested::Present(number(2.0)), Requested::Absent);
        let OwnProperty::Data { layout, value } = replaced(validate_and_apply_existing(
            &definition,
            &data(number(1.0), true, false, false),
        )) else {
            panic!("expected a data property");
        };
        assert_eq!(layout, PropertyLayout::data(true, false, false));
        assert!(value.same_value(&number(2.0)));
    }

    /// The oracle reports `data value=1 w=false e=false c=false`: dropping
    /// `writable` on a non-configurable property is permitted.
    #[test]
    fn a_writable_non_configurable_property_may_become_non_writable() {
        let definition = PropertyDefinition::data(Requested::Absent, Requested::Present(false));
        let OwnProperty::Data { layout, value } = replaced(validate_and_apply_existing(
            &definition,
            &data(number(1.0), true, false, false),
        )) else {
            panic!("expected a data property");
        };
        assert_eq!(layout, PropertyLayout::data(false, false, false));
        assert!(value.same_value(&number(1.0)));
    }

    /// The oracle reports `accessor get=fn set=undefined e=true c=true`: a
    /// configurable data property converted to an accessor keeps its
    /// `enumerable` and `configurable` attributes.
    #[test]
    fn a_configurable_kind_change_preserves_the_current_attributes() {
        let definition = PropertyDefinition::accessor(Requested::Present(None), Requested::Absent);
        let OwnProperty::Accessor { layout, .. } = replaced(validate_and_apply_existing(
            &definition,
            &data(number(1.0), true, true, true),
        )) else {
            panic!("expected an accessor property");
        };
        assert_eq!(layout, PropertyLayout::accessor(true, true));
    }

    /// The oracle reports `data value=2 w=false e=true c=true`: converting an
    /// accessor to a data property defaults `writable` to `false` while
    /// retaining `enumerable` and `configurable`.
    #[test]
    fn an_accessor_converted_to_data_defaults_writable_to_false() {
        let definition =
            PropertyDefinition::data(Requested::Present(number(2.0)), Requested::Absent);
        let OwnProperty::Data { layout, value } = replaced(validate_and_apply_existing(
            &definition,
            &accessor(true, true),
        )) else {
            panic!("expected a data property");
        };
        assert_eq!(layout, PropertyLayout::data(false, true, true));
        assert!(value.same_value(&number(2.0)));
    }

    /// An absent accessor field keeps the current function, so a descriptor
    /// that requests nothing new reports no change.
    #[test]
    fn an_absent_accessor_field_keeps_the_current_function() {
        let existing = OwnProperty::Accessor {
            layout: PropertyLayout::accessor(false, true),
            getter: None,
            setter: None,
        };
        let definition = PropertyDefinition::accessor(Requested::Absent, Requested::Present(None));
        assert!(matches!(
            validate_and_apply_existing(&definition, &existing),
            DefinitionDecision::Unchanged
        ));
    }
}
