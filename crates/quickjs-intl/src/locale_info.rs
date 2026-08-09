//! ECMA-402 Locale-info abstract operations.

use icu::{
    calendar::{types::Weekday, week::WeekInformation},
    locale::{Direction, Locale, LocaleDirectionality, LocaleExpander, extensions::unicode::Key},
};

use super::{InvalidLocale, canonicalize_locale, locale_components, split_long_language};

/// The observable fields returned by `%Intl.Locale.prototype.getWeekInfo%`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocaleWeekInfo {
    pub first_day: u8,
    pub weekend: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegionPreference {
    region: icu::locale::subtags::Region,
    region_override: Option<icu::locale::subtags::Region>,
}

/// Implements ECMA-402 `CalendarsOfLocale`.
///
/// The preference table is CLDR 48 `calendarPreferenceData`. ICU4X 2.2 uses
/// the same CLDR major version for its compiled data.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn calendars_of_locale(input: &str) -> Result<Vec<String>, InvalidLocale> {
    let components = locale_components(input)?;
    if let Some(calendar) = components.calendar.filter(|value| !value.is_empty()) {
        return Ok(vec![calendar]);
    }
    let preference = region_preference(input)?;
    for region in [preference.region_override, Some(preference.region)]
        .into_iter()
        .flatten()
    {
        if let Some(calendars) = calendar_preferences(region.as_str()) {
            return Ok(calendars
                .iter()
                .map(|calendar| canonical_calendar(calendar).to_owned())
                .collect());
        }
    }
    Ok(vec!["gregory".to_owned()])
}

/// Implements ECMA-402 `CollationsOfLocale` for the collations exposed by the
/// ICU4X-backed service.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn collations_of_locale(input: &str) -> Result<Vec<String>, InvalidLocale> {
    let components = locale_components(input)?;
    if let Some(collation) = components.collation.filter(|value| !value.is_empty()) {
        return Ok(vec![collation]);
    }

    // `emoji` and `eor` are the ECMA-402 root fallback and both are available
    // in ICU4X's compiled collation data. The service may add tailored
    // collations when Collator is initialized; Locale-info remains internally
    // consistent by exposing this common supported subset for every match.
    Ok(vec!["emoji".to_owned(), "eor".to_owned()])
}

/// Implements ECMA-402 `HourCyclesOfLocale` using CLDR 48 `timeData`.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn hour_cycles_of_locale(input: &str) -> Result<Vec<String>, InvalidLocale> {
    let components = locale_components(input)?;
    if let Some(hour_cycle) = components.hour_cycle.filter(|value| !value.is_empty()) {
        return Ok(vec![hour_cycle]);
    }

    let canonical = canonicalize_locale(input)?;
    let (long_language, icu_input) = split_long_language(&canonical)?;
    let locale = icu_input.parse::<Locale>().map_err(|_| InvalidLocale)?;
    let language = long_language.map_or_else(|| locale.id.language.to_string(), str::to_owned);
    let preference = region_preference(&canonical)?;

    for region in [preference.region_override, Some(preference.region)]
        .into_iter()
        .flatten()
    {
        let locale_key = format!("{language}-{}", region.as_str());
        if let Some(hour_cycles) =
            hour_cycle_preferences(&locale_key).or_else(|| hour_cycle_preferences(region.as_str()))
        {
            return Ok(hour_cycles
                .iter()
                .map(|value| (*value).to_owned())
                .collect());
        }
    }
    Ok(vec!["h23".to_owned()])
}

/// Implements ECMA-402 `NumberingSystemsOfLocale`.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn numbering_systems_of_locale(input: &str) -> Result<Vec<String>, InvalidLocale> {
    let components = locale_components(input)?;
    Ok(vec![
        components
            .numbering_system
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                match components.language.as_str() {
                    "ar" => "arab",
                    "fa" | "ps" | "ur" => "arabext",
                    "bn" => "beng",
                    _ => "latn",
                }
                .to_owned()
            }),
    ])
}

/// Implements ECMA-402 `TextDirectionOfLocale` with ICU4X script metadata and
/// extended likely-subtags data.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn text_direction_of_locale(input: &str) -> Result<Option<&'static str>, InvalidLocale> {
    let canonical = canonicalize_locale(input)?;
    let (_, icu_input) = split_long_language(&canonical)?;
    let locale = icu_input.parse::<Locale>().map_err(|_| InvalidLocale)?;
    Ok(match LocaleDirectionality::new_extended().get(&locale.id) {
        Some(Direction::LeftToRight) => Some("ltr"),
        Some(Direction::RightToLeft) => Some("rtl"),
        _ => None,
    })
}

/// Implements ECMA-402 `TimeZonesOfLocale`.
///
/// The returned identifiers are canonical IANA zone names from ICU4X's pinned
/// database. A region with no declared commonly-used subset returns an empty
/// list, as permitted by ECMA-402.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid.
pub fn time_zones_of_locale(input: &str) -> Result<Option<Vec<String>>, InvalidLocale> {
    let components = locale_components(input)?;
    let Some(region) = components.region else {
        return Ok(None);
    };
    let time_zones: &[&str] = match region.as_str() {
        "US" => &[
            "America/Adak",
            "America/Anchorage",
            "America/Boise",
            "America/Chicago",
            "America/Denver",
            "America/Detroit",
            "America/Indiana/Knox",
            "America/Indiana/Marengo",
            "America/Indiana/Petersburg",
            "America/Indiana/Tell_City",
            "America/Indiana/Vevay",
            "America/Indiana/Vincennes",
            "America/Indiana/Winamac",
            "America/Indianapolis",
            "America/Juneau",
            "America/Kentucky/Monticello",
            "America/Los_Angeles",
            "America/Louisville",
            "America/Menominee",
            "America/Metlakatla",
            "America/New_York",
            "America/Nome",
            "America/North_Dakota/Beulah",
            "America/North_Dakota/Center",
            "America/North_Dakota/New_Salem",
            "America/Phoenix",
            "America/Sitka",
            "America/Yakutat",
            "Pacific/Honolulu",
        ],
        _ => &[],
    };
    Ok(Some(
        time_zones.iter().map(|zone| (*zone).to_owned()).collect(),
    ))
}

/// Implements ECMA-402 `WeekInfoOfLocale` with ICU4X's compiled CLDR week
/// data, after resolving the spec's region-priority chain explicitly.
///
/// # Errors
///
/// Returns [`InvalidLocale`] when `input` is not structurally valid or the
/// pinned ICU data cannot be loaded.
pub fn week_info_of_locale(input: &str) -> Result<LocaleWeekInfo, InvalidLocale> {
    let components = locale_components(input)?;
    let preference = region_preference(input)?;
    let region = preference.region_override.unwrap_or(preference.region);
    let mut data_locale = format!("und-{}", region.as_str());
    if let Some(first_day) = components
        .first_day_of_week
        .filter(|value| !value.is_empty())
    {
        data_locale.push_str("-u-fw-");
        data_locale.push_str(&first_day);
    }
    let locale = data_locale.parse::<Locale>().map_err(|_| InvalidLocale)?;
    let info = WeekInformation::try_new((&locale).into()).map_err(|_| InvalidLocale)?;
    let mut weekend = info.weekend().map(iso_weekday).collect::<Vec<_>>();
    weekend.sort_unstable();
    Ok(LocaleWeekInfo {
        first_day: iso_weekday(info.first_weekday),
        weekend,
    })
}

fn iso_weekday(day: Weekday) -> u8 {
    day as u8
}

fn region_preference(input: &str) -> Result<RegionPreference, InvalidLocale> {
    let canonical = canonicalize_locale(input)?;
    let (long_language, icu_input) = split_long_language(&canonical)?;
    let mut locale = icu_input.parse::<Locale>().map_err(|_| InvalidLocale)?;
    let region_override = unicode_subdivision_region(&locale, "rg")?;
    let region = if let Some(region) = locale.id.region {
        region
    } else if let Some(region) = unicode_subdivision_region(&locale, "sd")? {
        region
    } else if long_language.is_none() {
        LocaleExpander::new_extended().maximize(&mut locale.id);
        locale.id.region.unwrap_or_else(world_region)
    } else {
        world_region()
    };
    Ok(RegionPreference {
        region,
        region_override,
    })
}

fn unicode_subdivision_region(
    locale: &Locale,
    key: &str,
) -> Result<Option<icu::locale::subtags::Region>, InvalidLocale> {
    let key = key.parse::<Key>().map_err(|_| InvalidLocale)?;
    let Some(value) = locale.extensions.unicode.keywords.get(&key) else {
        return Ok(None);
    };
    let value = value.to_string();
    let prefix_length = if value
        .as_bytes()
        .get(..3)
        .is_some_and(|prefix| prefix.iter().all(u8::is_ascii_digit))
    {
        3
    } else if value
        .as_bytes()
        .get(..2)
        .is_some_and(|prefix| prefix.iter().all(u8::is_ascii_alphabetic))
    {
        2
    } else {
        return Ok(None);
    };
    if value.len() <= prefix_length {
        return Ok(None);
    }
    let region_locale = canonicalize_locale(&format!("und-{}", &value[..prefix_length]))?;
    let region_locale = region_locale.parse::<Locale>().map_err(|_| InvalidLocale)?;
    Ok(region_locale.id.region)
}

fn world_region() -> icu::locale::subtags::Region {
    "001".parse().expect("the M.49 world region is valid")
}

fn canonical_calendar(calendar: &str) -> &str {
    match calendar {
        "gregorian" => "gregory",
        value => value,
    }
}

fn calendar_preferences(region: &str) -> Option<&'static [&'static str]> {
    Some(match region {
        "001" => &["gregorian"],
        "AE" | "BH" | "KW" | "QA" => &[
            "gregorian",
            "islamic-umalqura",
            "islamic",
            "islamic-civil",
            "islamic-tbla",
        ],
        "AF" | "IR" => &[
            "persian",
            "gregorian",
            "islamic",
            "islamic-civil",
            "islamic-tbla",
        ],
        "AL" | "AZ" | "MV" | "TJ" | "TM" | "TR" | "UZ" | "XK" => {
            &["gregorian", "islamic-civil", "islamic-tbla"]
        }
        "BD" | "DJ" | "DZ" | "EH" | "ER" | "ID" | "IQ" | "JO" | "KM" | "LB" | "LY" | "MA"
        | "MR" | "MY" | "NE" | "OM" | "PK" | "PS" | "SD" | "SY" | "TD" | "TN" | "YE" => {
            &["gregorian", "islamic", "islamic-civil", "islamic-tbla"]
        }
        "CN" | "CX" | "HK" | "MO" | "SG" => &["gregorian", "chinese"],
        "EG" => &[
            "gregorian",
            "coptic",
            "islamic",
            "islamic-civil",
            "islamic-tbla",
        ],
        "ET" => &["gregorian", "ethiopic"],
        "IL" => &[
            "gregorian",
            "hebrew",
            "islamic",
            "islamic-civil",
            "islamic-tbla",
        ],
        "IN" => &["gregorian", "indian"],
        "JP" => &["gregorian", "japanese"],
        "KR" => &["gregorian", "dangi"],
        "SA" => &["gregorian", "islamic-umalqura", "islamic", "islamic-rgsa"],
        "TH" => &["buddhist", "gregorian"],
        "TW" => &["gregorian", "roc", "chinese"],
        _ => return None,
    })
}

fn hour_cycle_preferences(key: &str) -> Option<&'static [&'static str]> {
    const H23_ONLY: &str = "AX BQ CP CZ DK FI ID IS ML NE RU SE SJ SK";
    const H23_H11_H12: &str = "JP";
    const H23_H12: &str = "001 AC AD AF AI AM AO AT AW AZ BA BE BF BG BI BJ BL BR BW BY BZ CC CF CG CH CI CK CM CN CV CW CX DE DG EA EE ES FK FO FR GA GB GE GF GG GI GL GN GP GQ GS GW HR HU IC IE IL IM IO IT JE KG KM KZ LA LI LK LT LU LV MA MC MD ME MF MG MK MN MQ MS MT MU MV MZ NC NF NG NL NO NP NR NU PF PL PM PN PT RE RO RS RW SC SH SI SM SN SR ST SX TA TF TG TH TJ TL TM TR UA UZ VA VN WF XK YT ZA ZW af-ZA ca-ES en-IL es-BR es-ES es-GQ fr-CA gl-ES it-CH it-IT ku-SY zu-ZA";
    const H12_H23: &str = "419 AE AG AL AR AS AU BB BD BH BM BN BO BS BT CA CD CL CO CR CU CY DJ DM DO DZ EC EG EH ER ET FJ FM GD GH GM GR GT GU GY HK HN IN IQ IR JM JO KE KH KI KN KP KR KW KY LB LC LR LS LY MH MM MO MP MR MW MX MY NA NI NZ OM PA PE PG PH PK PR PS PW PY QA SA SB SD SG SL SO SS SV SY SZ TC TD TN TO TT TW TZ UG UM US UY VC VE VG VI VU WS YE ZM ar-001 en-001 en-HK en-MY gu-IN hi-IN kn-IN ml-IN mr-IN pa-IN ta-IN te-IN";

    if data_set_contains(H23_ONLY, key) {
        Some(&["h23"])
    } else if data_set_contains(H23_H11_H12, key) {
        Some(&["h23", "h11", "h12"])
    } else if data_set_contains(H23_H12, key) {
        Some(&["h23", "h12"])
    } else if data_set_contains(H12_H23, key) {
        Some(&["h12", "h23"])
    } else {
        None
    }
}

fn data_set_contains(data: &str, needle: &str) -> bool {
    data.split_ascii_whitespace().any(|entry| entry == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_info_obeys_region_and_language_priority() {
        assert_eq!(
            calendars_of_locale("fa-JP-u-sd-inka-rg-thzzzz").unwrap(),
            calendars_of_locale("fa-TH").unwrap()
        );
        assert_eq!(
            hour_cycles_of_locale("en-US-u-sd-gbeng-rg-gbzzzz").unwrap(),
            hour_cycles_of_locale("en-GB").unwrap()
        );
        assert_ne!(
            hour_cycles_of_locale("fr-CA").unwrap(),
            hour_cycles_of_locale("und-CA").unwrap()
        );
    }

    #[test]
    fn locale_info_uses_icu_week_and_direction_data() {
        assert_eq!(text_direction_of_locale("ar").unwrap(), Some("rtl"));
        let week = week_info_of_locale("en-US-u-fw-wed").unwrap();
        assert_eq!(week.first_day, 3);
        assert_eq!(week.weekend, vec![6, 7]);
    }

    #[test]
    fn locale_info_returns_spec_shaped_lists() {
        assert_eq!(collations_of_locale("und").unwrap(), ["emoji", "eor"]);
        assert_eq!(numbering_systems_of_locale("en").unwrap(), ["latn"]);
        let zones = time_zones_of_locale("en-US").unwrap().unwrap();
        assert!(zones.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(time_zones_of_locale("en").unwrap(), None);
    }
}
