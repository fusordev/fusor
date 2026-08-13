use std::sync::Arc;

use fusor_bytecode::{
    CompilerString, CompilerTemplateElement, CompilerTemplateObject, CompilerTemplateObjectError,
};

fn string(text: &str) -> CompilerString {
    CompilerString::try_from_code_units(text.encode_utf16().collect::<Vec<_>>().into())
        .expect("small template string")
}

#[test]
fn compiler_template_objects_preserve_cooked_raw_and_invalid_escape_values() {
    let object = CompilerTemplateObject::try_from_elements(Arc::from([
        CompilerTemplateElement::new(Some(string("a\n")), string("a\\n")),
        CompilerTemplateElement::new(None, string("\\unicode")),
    ]))
    .expect("nonempty template object");

    let [escaped, invalid] = object.elements() else {
        panic!("fixture has two elements");
    };
    assert_eq!(
        escaped.cooked().and_then(CompilerString::latin1_units),
        Some(b"a\n".as_slice())
    );
    assert_eq!(escaped.raw().latin1_units(), Some(b"a\\n".as_slice()));
    assert_eq!(invalid.cooked(), None);
    assert_eq!(invalid.raw().latin1_units(), Some(b"\\unicode".as_slice()));
    assert_eq!(object.payload_bytes(), 13);
}

#[test]
fn compiler_template_objects_reject_an_empty_site() {
    assert_eq!(
        CompilerTemplateObject::try_from_elements(Arc::from([])),
        Err(CompilerTemplateObjectError::Empty)
    );
}
