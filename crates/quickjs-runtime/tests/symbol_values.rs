use std::sync::Arc;

use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    AtomError, AtomKind, AtomLimits, ExecutionLimits, HandleError, HandleKind, JsString,
    PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS, PREDEFINED_INTERNER_SLOTS, Runtime,
    RuntimeLimits, ValueKind,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("symbol-values.js"))
                    .expect("storage plan");
            let root = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(root_name))
                .expect("root function");
            let tree = context
                .compile_tree(&root, quickjs_bytecode::VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn boolean(value: &quickjs_runtime::JsValue) -> bool {
    value.as_boolean().expect("live value").expect("Boolean")
}

fn text(value: &quickjs_runtime::JsValue) -> String {
    value
        .as_string()
        .expect("live value")
        .expect("String")
        .to_utf8_lossy()
        .expect("UTF-8")
}

#[test]
fn public_symbols_preserve_optional_descriptions_and_unique_identity() {
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let empty_description = JsString::empty();
    let named_description = JsString::from_utf8("token").expect("description");

    let absent = context.symbol(None).expect("descriptionless symbol");
    let empty = context
        .symbol(Some(&empty_description))
        .expect("empty-description symbol");
    let named = context
        .symbol(Some(&named_description))
        .expect("named symbol");
    let another_named = context
        .symbol(Some(&named_description))
        .expect("another named symbol");

    assert_eq!(absent.kind().expect("live kind"), ValueKind::Symbol);
    assert_eq!(
        absent
            .as_symbol()
            .expect("live symbol")
            .expect("Symbol")
            .kind(),
        AtomKind::Symbol
    );
    assert_eq!(
        absent
            .as_symbol()
            .expect("live symbol")
            .expect("Symbol")
            .description(),
        None
    );
    assert_eq!(
        empty
            .as_symbol()
            .expect("live symbol")
            .expect("Symbol")
            .description(),
        Some(&empty_description)
    );
    assert_eq!(
        named
            .as_symbol()
            .expect("live symbol")
            .expect("Symbol")
            .description(),
        Some(&named_description)
    );
    assert!(
        !named
            .as_symbol()
            .expect("live symbol")
            .expect("Symbol")
            .is_same_identity(
                another_named
                    .as_symbol()
                    .expect("live symbol")
                    .expect("Symbol")
            )
    );

    let named_clone = named.clone();
    assert!(
        named
            .as_symbol()
            .expect("live symbol")
            .expect("Symbol")
            .is_same_identity(
                named_clone
                    .as_symbol()
                    .expect("live symbol")
                    .expect("Symbol")
            )
    );
    assert!(
        context
            .undefined()
            .as_symbol()
            .expect("live non-symbol")
            .is_none()
    );
}

#[test]
fn symbols_are_truthy_identity_compared_and_preserved_by_calls() {
    let identity = compile("function identity(value){return value;}", "identity");
    let same = compile("function same(left,right){return left===right;}", "same");
    let negate = compile("function negate(value){return !value;}", "negate");
    let type_of = compile("function typeOf(value){return typeof value;}", "typeOf");

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let identity = context.instantiate(identity).expect("identity");
    let same = context.instantiate(same).expect("same");
    let negate = context.instantiate(negate).expect("negate");
    let type_of = context.instantiate(type_of).expect("typeof");
    let description = JsString::from_utf8("identity").expect("description");
    let symbol = context.symbol(Some(&description)).expect("symbol");
    let other = context.symbol(Some(&description)).expect("other symbol");

    let returned = context
        .call(
            &identity,
            std::slice::from_ref(&symbol),
            ExecutionLimits::default(),
        )
        .expect("identity call");
    assert!(
        symbol
            .as_symbol()
            .expect("live symbol")
            .expect("Symbol")
            .is_same_identity(
                returned
                    .as_symbol()
                    .expect("live returned value")
                    .expect("returned Symbol")
            )
    );

    let equal = context
        .call(
            &same,
            &[symbol.clone(), returned],
            ExecutionLimits::default(),
        )
        .expect("same-symbol comparison");
    assert!(boolean(&equal));
    let unequal = context
        .call(&same, &[symbol.clone(), other], ExecutionLimits::default())
        .expect("different-symbol comparison");
    assert!(!boolean(&unequal));

    let negated = context
        .call(
            &negate,
            std::slice::from_ref(&symbol),
            ExecutionLimits::default(),
        )
        .expect("truthiness call");
    assert!(!boolean(&negated));
    let kind = context
        .call(
            &type_of,
            std::slice::from_ref(&symbol),
            ExecutionLimits::default(),
        )
        .expect("typeof call");
    assert_eq!(text(&kind), "symbol");
}

#[test]
fn symbol_handles_report_orphaning_after_runtime_drop() {
    let symbol = {
        let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let mut context = runtime.context(&realm).expect("context");
        context.symbol(None).expect("symbol")
    };

    assert_eq!(
        symbol.kind(),
        Err(HandleError::Orphaned {
            kind: HandleKind::Value
        })
    );
    assert_eq!(
        symbol.as_symbol(),
        Err(HandleError::Orphaned {
            kind: HandleKind::Value
        })
    );
}

#[test]
fn symbol_creation_returns_the_atom_limit_error_without_partial_usage() {
    let limits = AtomLimits::new(
        PREDEFINED_ATOM_COUNT + 23,
        PREDEFINED_DESCRIPTION_CODE_UNITS + 200,
        PREDEFINED_INTERNER_SLOTS + 23,
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_atom_limits(limits)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before = runtime.atom_usage();
    let error = {
        let mut context = runtime.context(&realm).expect("context");
        context.symbol(None).expect_err("atom limit")
    };

    assert_eq!(
        error,
        AtomError::LiveAtomLimit {
            current: PREDEFINED_ATOM_COUNT + 23,
            additional: 1,
            maximum: PREDEFINED_ATOM_COUNT + 23,
        }
    );
    assert_eq!(runtime.atom_usage(), before);
}
