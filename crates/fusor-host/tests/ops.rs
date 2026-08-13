//! `#[op]` declaration expansion and the assembly registry (§5.1, §5.4).

use fusor_host::ops::{OpError, OpRegistry};
use fusor_ops::op;

#[op]
fn op_read_text(path: String) -> Result<String, OpError> {
    let _ = path;
    Ok("content".to_owned())
}

#[op(async)]
async fn op_sleep(ms: u64) -> Result<(), OpError> {
    let _ = ms;
    Ok(())
}

#[op(name = "custom_read")]
fn op_renamed(value: u8) -> Result<u8, OpError> {
    Ok(value)
}

#[test]
fn op_declarations_carry_the_default_snake_case_name() {
    let declaration = __fusor_op_declaration_op_read_text();
    assert_eq!(declaration.name, "op_read_text");
    assert!(!declaration.is_async);
    assert_eq!(declaration.parameter_types, &["String"]);
}

#[test]
fn async_ops_mark_their_declaration() {
    let declaration = __fusor_op_declaration_op_sleep();
    assert_eq!(declaration.name, "op_sleep");
    assert!(declaration.is_async);
    assert_eq!(declaration.parameter_types, &["u64"]);
}

#[test]
fn name_overrides_replace_the_function_name() {
    let declaration = __fusor_op_declaration_op_renamed();
    assert_eq!(declaration.name, "custom_read");
    assert_eq!(declaration.parameter_types, &["u8"]);
}

#[test]
fn the_registry_rejects_same_name_conflicts() {
    let mut registry = OpRegistry::new();
    registry
        .register(__fusor_op_declaration_op_read_text())
        .expect("first registration");
    let conflict = registry
        .register(__fusor_op_declaration_op_read_text())
        .expect_err("same-name conflict must fail at assembly time");
    assert_eq!(conflict.name, "op_read_text");
    assert_eq!(
        registry
            .get("op_read_text")
            .expect("first declaration stays")
            .name,
        "op_read_text"
    );
}

#[test]
fn the_op_error_contract_formats_classes_and_codes() {
    assert_eq!(OpError::new("plain").to_string(), "plain");
    assert_eq!(
        OpError::of_class("TypeError", "wrong kind").to_string(),
        "TypeError: wrong kind"
    );
    let coded = OpError::new("failed").with_code("FUS-IO-0001");
    assert_eq!(coded.code, Some("FUS-IO-0001"));
    assert_eq!(
        OpError::type_error(2, "expected a string").to_string(),
        "TypeError: parameter 2: expected a string"
    );
}
