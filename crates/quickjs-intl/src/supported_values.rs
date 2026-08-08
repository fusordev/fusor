//! Implementation-defined value inventories exposed by
//! `Intl.supportedValuesOf`.

use icu::time::zone::iana::IanaParserExtended;

const CALENDARS: &[&str] = &[
    "buddhist",
    "chinese",
    "coptic",
    "dangi",
    "ethioaa",
    "ethiopic",
    "gregory",
    "hebrew",
    "indian",
    "islamic-civil",
    "islamic-tbla",
    "islamic-umalqura",
    "iso8601",
    "japanese",
    "persian",
    "roc",
];

// ICU4X's collation service is not wired into the runtime yet. Keep this
// capability inventory empty until `%Intl.Collator%` is installed rather than
// advertising a value the runtime cannot consume.
const COLLATIONS: &[&str] = &[];

// Currency formatting and display names are not wired into the runtime yet.
// ECMA-402 permits this implementation-defined inventory to be empty.
const CURRENCIES: &[&str] = &[];

// ECMA-402 Table 10. Test262 intentionally pins every simple digit mapping,
// including additions newer than the CLDR data bundled by ICU4X 2.2.
const NUMBERING_SYSTEMS: &[&str] = &[
    "adlm", "ahom", "arab", "arabext", "bali", "beng", "bhks", "brah", "cakm", "cham", "deva",
    "diak", "fullwide", "gara", "gong", "gonm", "gujr", "gukh", "guru", "hanidec", "hmng", "hmnp",
    "java", "kali", "kawi", "khmr", "knda", "krai", "lana", "lanatham", "laoo", "latn", "lepc",
    "limb", "mathbold", "mathdbl", "mathmono", "mathsanb", "mathsans", "mlym", "modi", "mong",
    "mroo", "mtei", "mymr", "mymrepka", "mymrpao", "mymrshan", "mymrtlng", "nagm", "newa", "nkoo",
    "olck", "onao", "orya", "osma", "outlined", "rohg", "saur", "segment", "shrd", "sind", "sinh",
    "sora", "sund", "sunu", "takr", "talu", "tamldec", "telu", "thai", "tibt", "tirh", "tnsa",
    "tols", "vaii", "wara", "wcho",
];

const UNITS: &[&str] = &[
    "acre",
    "bit",
    "byte",
    "celsius",
    "centimeter",
    "day",
    "degree",
    "fahrenheit",
    "fluid-ounce",
    "foot",
    "gallon",
    "gigabit",
    "gigabyte",
    "gram",
    "hectare",
    "hour",
    "inch",
    "kilobit",
    "kilobyte",
    "kilogram",
    "kilometer",
    "liter",
    "megabit",
    "megabyte",
    "meter",
    "microsecond",
    "mile",
    "mile-scandinavian",
    "milliliter",
    "millimeter",
    "millisecond",
    "minute",
    "month",
    "nanosecond",
    "ounce",
    "percent",
    "petabyte",
    "pound",
    "second",
    "stone",
    "terabit",
    "terabyte",
    "week",
    "yard",
    "year",
];

const REQUIRED_NON_CONTINENTAL_TIME_ZONES: &[&str] = &[
    "Etc/GMT+1",
    "Etc/GMT+2",
    "Etc/GMT+3",
    "Etc/GMT+4",
    "Etc/GMT+5",
    "Etc/GMT+6",
    "Etc/GMT+7",
    "Etc/GMT+8",
    "Etc/GMT+9",
    "Etc/GMT+10",
    "Etc/GMT+11",
    "Etc/GMT+12",
    "Etc/GMT-1",
    "Etc/GMT-2",
    "Etc/GMT-3",
    "Etc/GMT-4",
    "Etc/GMT-5",
    "Etc/GMT-6",
    "Etc/GMT-7",
    "Etc/GMT-8",
    "Etc/GMT-9",
    "Etc/GMT-10",
    "Etc/GMT-11",
    "Etc/GMT-12",
    "Etc/GMT-13",
    "Etc/GMT-14",
    "UTC",
];

/// Returns the sorted, unique inventory for an ECMA-402
/// `Intl.supportedValuesOf` key, or `None` for an invalid key.
#[must_use]
pub fn supported_values(key: &str) -> Option<Vec<String>> {
    let values = match key {
        "calendar" => CALENDARS,
        "collation" => COLLATIONS,
        "currency" => CURRENCIES,
        "numberingSystem" => NUMBERING_SYSTEMS,
        "unit" => UNITS,
        "timeZone" => return Some(available_time_zones()),
        _ => return None,
    };
    Some(values.iter().map(|value| (*value).to_owned()).collect())
}

fn available_time_zones() -> Vec<String> {
    let mut values = IanaParserExtended::new()
        .iter()
        .filter_map(|entry| match entry.canonical {
            "Etc/Unknown" => None,
            "Etc/GMT" | "Etc/UTC" | "GMT" => Some("UTC".to_owned()),
            canonical => Some(canonical.to_owned()),
        })
        .collect::<Vec<_>>();
    values.extend(
        REQUIRED_NON_CONTINENTAL_TIME_ZONES
            .iter()
            .map(|value| (*value).to_owned()),
    );
    values.sort_unstable();
    values.dedup();
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_inventories_are_sorted_and_unique() {
        for key in [
            "calendar",
            "collation",
            "currency",
            "numberingSystem",
            "unit",
        ] {
            let values = supported_values(key).expect("valid key");
            let mut sorted = values.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(values, sorted, "{key}");
        }
    }

    #[test]
    fn time_zones_are_canonical_sorted_and_include_non_continental_zones() {
        let values = supported_values("timeZone").expect("time zones");
        let mut sorted = values.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(values, sorted);
        assert!(!values.iter().any(|value| value == "Etc/GMT"));
        assert!(!values.iter().any(|value| value == "Etc/UTC"));
        for required in REQUIRED_NON_CONTINENTAL_TIME_ZONES {
            assert!(values.iter().any(|value| value == required), "{required}");
        }
    }

    #[test]
    fn rejects_unknown_keys() {
        assert_eq!(supported_values("calendars"), None);
        assert_eq!(supported_values("calendar\0"), None);
    }
}
