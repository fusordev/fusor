use icu_casemap::CaseMapperBorrowed;
use icu_properties::{
    CodePointMapData, CodePointSetData, PropertyParser,
    props::{GeneralCategory, GeneralCategoryGroup, Script},
    script::ScriptWithExtensions,
};

use crate::{emoji, flags::MatchMode, program::UnicodeProperty};

pub(crate) fn matches_sequence(
    property: &UnicodeProperty,
    sequence: &[u32],
    mode: MatchMode,
) -> bool {
    if property.strings {
        return !property.negative && emoji::matches(&property.name, sequence);
    }
    let [code_point] = sequence else {
        return false;
    };
    matches(property, *code_point, mode)
}

pub(crate) fn matches(property: &UnicodeProperty, code_point: u32, mode: MatchMode) -> bool {
    if !mode.ignore_case {
        return base_matches(property, code_point) != property.negative;
    }

    let equivalents = case_equivalents(code_point);
    let any_inside = equivalents
        .iter()
        .copied()
        .any(|equivalent| base_matches(property, equivalent));
    if !property.negative {
        any_inside
    } else if mode.unicode_sets {
        !any_inside
    } else {
        equivalents
            .iter()
            .copied()
            .any(|equivalent| !base_matches(property, equivalent))
    }
}

fn base_matches(property: &UnicodeProperty, code_point: u32) -> bool {
    match property.value.as_deref() {
        Some(value) if matches!(property.name.as_str(), "General_Category" | "gc") => {
            general_category_matches(value, code_point)
        }
        Some(value) if matches!(property.name.as_str(), "Script" | "sc") => {
            script_matches(value, code_point, false)
        }
        Some(value) if matches!(property.name.as_str(), "Script_Extensions" | "scx") => {
            script_matches(value, code_point, true)
        }
        Some(_) => false,
        None => lone_property_matches(&property.name, code_point),
    }
}

fn case_equivalents(code_point: u32) -> [u32; 5] {
    let Some(character) = char::from_u32(code_point) else {
        return [code_point; 5];
    };
    let mapper = CaseMapperBorrowed::new();
    [
        code_point,
        u32::from(mapper.simple_fold(character)),
        u32::from(mapper.simple_lowercase(character)),
        u32::from(mapper.simple_uppercase(character)),
        u32::from(mapper.simple_titlecase(character)),
    ]
}

fn lone_property_matches(name: &str, code_point: u32) -> bool {
    match name {
        "Any" => code_point <= 0x10_ffff,
        "ASCII" => code_point <= 0x7f,
        "Assigned" => {
            CodePointMapData::<GeneralCategory>::new().get32(code_point)
                != GeneralCategory::Unassigned
        }
        _ => {
            if let Some(property) = CodePointSetData::new_for_ecma262(name.as_bytes()) {
                property.contains32(code_point)
            } else {
                general_category_matches(name, code_point)
            }
        }
    }
}

fn general_category_matches(name: &str, code_point: u32) -> bool {
    let actual = CodePointMapData::<GeneralCategory>::new().get32(code_point);
    if let Some(expected) = PropertyParser::<GeneralCategory>::new().get_strict(name) {
        actual == expected
    } else {
        PropertyParser::<GeneralCategoryGroup>::new()
            .get_strict(name)
            .is_some_and(|group| group.contains(actual))
    }
}

fn script_matches(name: &str, code_point: u32, extensions: bool) -> bool {
    let Some(expected) = PropertyParser::<Script>::new().get_strict(name) else {
        return false;
    };
    if extensions {
        ScriptWithExtensions::new().has_script32(code_point, expected)
    } else {
        CodePointMapData::<Script>::new().get32(code_point) == expected
    }
}
