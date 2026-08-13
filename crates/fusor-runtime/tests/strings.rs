use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use fusor_runtime::{JsString, JsStringError, MAX_STRING_CODE_UNITS};

#[test]
fn utf8_input_becomes_utf16_code_units() {
    let value = JsString::from_utf8("Aé😀").expect("string");
    assert_eq!(value.len(), 4);
    assert!(!value.is_latin1());
    assert_eq!(
        value.code_units().collect::<Vec<_>>(),
        [0x0041, 0x00e9, 0xd83d, 0xde00]
    );
    assert_eq!(value.to_utf8_lossy().expect("UTF-8"), "Aé😀");
}

#[test]
fn lone_surrogates_are_preserved_and_only_lossy_at_the_host_boundary() {
    let value = JsString::from_code_units([0xd800, b'x'.into(), 0xdc00]).expect("WTF-16");
    assert_eq!(
        value.code_units().collect::<Vec<_>>(),
        [0xd800, 0x0078, 0xdc00]
    );
    assert_eq!(value.to_utf8_lossy().expect("lossy UTF-8"), "�x�");
}

#[test]
fn a_surrogate_pair_decodes_across_a_rope_boundary() {
    let left =
        JsString::from_code_units(std::iter::repeat_n(u16::from(b'x'), 8_192).chain([0xd83d]))
            .expect("left");
    let right = JsString::from_code_units([0xde00]).expect("right");
    let rope = left.concat(&right).expect("rope");

    assert!(rope.to_utf8_lossy().expect("UTF-8").ends_with("x😀"));
}

#[test]
fn exact_host_encodings_preserve_lone_surrogates_and_cesu8_mode() {
    let value = JsString::from_code_units([0, 0x00e9, 0xd83d, 0xde00, 0xd800]).expect("string");

    assert_eq!(
        value.to_wtf8_bytes().expect("WTF-8"),
        [0x00, 0xc3, 0xa9, 0xf0, 0x9f, 0x98, 0x80, 0xed, 0xa0, 0x80]
    );
    assert_eq!(
        value.to_cesu8_bytes().expect("CESU-8"),
        [
            0x00, 0xc3, 0xa9, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80, 0xed, 0xa0, 0x80
        ]
    );
}

#[test]
fn latin1_storage_is_an_unobservable_optimization() {
    let narrow = JsString::from_latin1(&[0x41, 0xff]).expect("Latin-1");
    let from_units = JsString::from_code_units([0x0041, 0x00ff]).expect("units");
    assert!(narrow.is_latin1());
    assert!(from_units.is_latin1());
    assert_eq!(narrow, from_units);
    assert_eq!(narrow.code_unit_at(0), Some(0x41));
    assert_eq!(narrow.code_unit_at(1), Some(0xff));
    assert_eq!(narrow.code_unit_at(2), None);
}

#[test]
fn ropes_compare_hash_index_and_slice_by_code_unit() {
    let left = JsString::from_latin1(&vec![b'a'; 9_000]).expect("left");
    let right = JsString::from_code_units([0xd800, 0x0062]).expect("right");
    let rope = left.concat(&right).expect("rope");
    let flat = JsString::from_code_units(
        std::iter::repeat_n(u16::from(b'a'), 9_000).chain([0xd800, 0x0062]),
    )
    .expect("flat");

    assert_eq!(rope, flat);
    assert_eq!(hash(&rope), hash(&flat));
    assert_eq!(rope.code_unit_at(9_000), Some(0xd800));
    assert_eq!(
        rope.slice(8_999..9_002)
            .expect("slice")
            .code_units()
            .collect::<Vec<_>>(),
        [0x0061, 0xd800, 0x0062]
    );
}

#[test]
fn ordering_is_lexicographic_over_utf16_code_units() {
    let first = JsString::from_code_units([0xd800]).expect("first");
    let second = JsString::from_code_units([0xd801]).expect("second");
    let extension = JsString::from_code_units([0xd800, 0]).expect("extension");

    assert!(first < second);
    assert!(first < extension);
}

#[test]
fn invalid_slices_return_structured_errors() {
    let value = JsString::from_utf8("abc").expect("string");
    let reversed_start = 2;
    let reversed_end = 1;
    assert_eq!(
        value.slice(reversed_start..reversed_end),
        Err(JsStringError::InvalidRange {
            start: 2,
            end: 1,
            len: 3
        })
    );
    assert_eq!(
        value.slice(0..4),
        Err(JsStringError::InvalidRange {
            start: 0,
            end: 4,
            len: 3
        })
    );
}

#[test]
fn an_oversized_exact_iterator_is_rejected_before_iteration() {
    let oversized = std::iter::repeat_n(0_u16, MAX_STRING_CODE_UNITS as usize + 1);
    assert_eq!(
        JsString::from_code_units(oversized),
        Err(JsStringError::TooLong {
            requested: u64::from(MAX_STRING_CODE_UNITS) + 1,
            maximum: MAX_STRING_CODE_UNITS,
        })
    );
}

#[test]
fn code_unit_iterator_is_exact_sized_and_fused() {
    let left = JsString::from_latin1(&vec![b'a'; 9_000]).expect("left");
    let right = JsString::from_code_units([0xd800, 0x0062]).expect("right");
    let rope = left.concat(&right).expect("rope");
    let mut units = rope.code_units();

    assert_eq!(units.len(), 9_002);
    assert_eq!(units.next(), Some(u16::from(b'a')));
    assert_eq!(units.len(), 9_001);
    assert_eq!(units.by_ref().count(), 9_001);
    assert_eq!(units.len(), 0);
    assert_eq!(units.next(), None);
    assert_eq!(units.next(), None);
}

fn hash(value: &JsString) -> u64 {
    let mut state = DefaultHasher::new();
    value.hash(&mut state);
    state.finish()
}
