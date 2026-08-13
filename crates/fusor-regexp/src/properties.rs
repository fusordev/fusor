use icu::{
    casemap::CaseMapperBorrowed,
    properties::{
        CodePointMapData, CodePointMapDataBorrowed, CodePointSetData, CodePointSetDataBorrowed,
        PropertyParser,
        props::{GeneralCategory, GeneralCategoryGroup, Script},
        script::{ScriptWithExtensions, ScriptWithExtensionsBorrowed},
    },
};

use crate::{emoji, flags::MatchMode};

#[derive(Clone, Debug)]
pub(crate) struct UnicodeProperty {
    pub negative: bool,
    pub strings: bool,
    matcher: UnicodePropertyMatcher,
}

#[derive(Clone, Debug)]
enum UnicodePropertyMatcher {
    Any,
    Ascii,
    Assigned(CodePointMapDataBorrowed<'static, GeneralCategory>),
    Binary(CodePointSetDataBorrowed<'static>),
    GeneralCategory {
        data: CodePointMapDataBorrowed<'static, GeneralCategory>,
        expected: GeneralCategoryMatcher,
    },
    Script {
        data: CodePointMapDataBorrowed<'static, Script>,
        expected: Script,
    },
    ScriptExtensions {
        data: ScriptWithExtensionsBorrowed<'static>,
        expected: Script,
    },
    EmojiString(String),
    Never,
}

#[derive(Clone, Copy, Debug)]
enum GeneralCategoryMatcher {
    Exact(GeneralCategory),
    Group(GeneralCategoryGroup),
}

impl UnicodeProperty {
    pub(crate) fn compile(negative: bool, strings: bool, name: &str, value: Option<&str>) -> Self {
        let matcher = if strings {
            UnicodePropertyMatcher::EmojiString(name.to_owned())
        } else {
            compile_code_point_matcher(name, value)
        };
        Self {
            negative,
            strings,
            matcher,
        }
    }
}

fn compile_code_point_matcher(name: &str, value: Option<&str>) -> UnicodePropertyMatcher {
    match value {
        Some(value) if matches!(name, "General_Category" | "gc") => compile_general_category(value),
        Some(value) if matches!(name, "Script" | "sc") => compile_script(value, false),
        Some(value) if matches!(name, "Script_Extensions" | "scx") => compile_script(value, true),
        Some(_) => UnicodePropertyMatcher::Never,
        None => compile_lone_property(name),
    }
}

fn compile_lone_property(name: &str) -> UnicodePropertyMatcher {
    match name {
        "Any" => UnicodePropertyMatcher::Any,
        "ASCII" => UnicodePropertyMatcher::Ascii,
        "Assigned" => UnicodePropertyMatcher::Assigned(CodePointMapData::new()),
        _ => CodePointSetData::new_for_ecma262(name.as_bytes()).map_or_else(
            || compile_general_category(name),
            UnicodePropertyMatcher::Binary,
        ),
    }
}

fn compile_general_category(name: &str) -> UnicodePropertyMatcher {
    let data = CodePointMapData::new();
    if let Some(expected) = PropertyParser::<GeneralCategory>::new().get_strict(name) {
        UnicodePropertyMatcher::GeneralCategory {
            data,
            expected: GeneralCategoryMatcher::Exact(expected),
        }
    } else if let Some(expected) = PropertyParser::<GeneralCategoryGroup>::new().get_strict(name) {
        UnicodePropertyMatcher::GeneralCategory {
            data,
            expected: GeneralCategoryMatcher::Group(expected),
        }
    } else {
        UnicodePropertyMatcher::Never
    }
}

fn compile_script(name: &str, extensions: bool) -> UnicodePropertyMatcher {
    let Some(expected) = PropertyParser::<Script>::new().get_strict(name) else {
        return UnicodePropertyMatcher::Never;
    };
    if extensions {
        UnicodePropertyMatcher::ScriptExtensions {
            data: ScriptWithExtensions::new(),
            expected,
        }
    } else {
        UnicodePropertyMatcher::Script {
            data: CodePointMapData::new(),
            expected,
        }
    }
}

pub(crate) fn matches_sequence(
    property: &UnicodeProperty,
    sequence: &[u32],
    mode: MatchMode,
) -> bool {
    if let UnicodePropertyMatcher::EmojiString(name) = &property.matcher {
        return !property.negative && emoji::matches(name, sequence);
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
    match property.matcher {
        UnicodePropertyMatcher::Any => code_point <= 0x10_ffff,
        UnicodePropertyMatcher::Ascii => code_point <= 0x7f,
        UnicodePropertyMatcher::Assigned(data) => {
            data.get32(code_point) != GeneralCategory::Unassigned
        }
        UnicodePropertyMatcher::Binary(data) => data.contains32(code_point),
        UnicodePropertyMatcher::GeneralCategory { data, expected } => match expected {
            GeneralCategoryMatcher::Exact(expected) => data.get32(code_point) == expected,
            GeneralCategoryMatcher::Group(expected) => expected.contains(data.get32(code_point)),
        },
        UnicodePropertyMatcher::Script { data, expected } => data.get32(code_point) == expected,
        UnicodePropertyMatcher::ScriptExtensions { data, expected } => {
            data.has_script32(code_point, expected)
        }
        UnicodePropertyMatcher::EmojiString(_) | UnicodePropertyMatcher::Never => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_script_extensions_into_a_reusable_icu_matcher() {
        let property =
            UnicodeProperty::compile(false, false, "Script_Extensions", Some("Tolong_Siki"));
        assert!(matches!(
            property.matcher,
            UnicodePropertyMatcher::ScriptExtensions { .. }
        ));
        let unicode = MatchMode {
            ignore_case: false,
            multiline: false,
            dot_all: false,
            unicode: true,
            unicode_sets: false,
        };
        assert!(matches(&property, 0x11db0, unicode));
        assert!(!matches(&property, u32::from('A'), unicode));
    }
}
