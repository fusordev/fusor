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

#[test]
fn op_declarations_carry_the_default_snake_case_name() {
    let declaration = op_read_text::declaration();
    assert_eq!(declaration.name, "op_read_text");
    assert!(!declaration.is_async);
    assert_eq!(declaration.parameter_types, &["String"]);
}

#[test]
fn async_ops_mark_their_declaration() {
    let declaration = op_sleep::declaration();
    assert_eq!(declaration.name, "op_sleep");
    assert!(declaration.is_async);
    assert_eq!(declaration.parameter_types, &["u64"]);
}

#[test]
fn the_registry_rejects_same_name_conflicts() {
    let mut registry = OpRegistry::new();
    registry.register(op_read_text::declaration(), op_read_text::call);
    registry.register(op_read_text::declaration(), op_read_text::call);
    let conflict = registry
        .take_conflict()
        .expect("same-name conflict must be recorded at assembly time");
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
    let coded = OpError::new("failed").with_code(14007);
    assert_eq!(coded.code, Some(14007));
    assert_eq!(
        OpError::type_error(2, "expected a string").to_string(),
        "TypeError: parameter 2: expected a string"
    );
}
