use quickjs_compiler::{
    CaptureSource, CompilationUnitKind, CompilerError, DeclarationKind, ExecutableKind,
    StoragePlacement, UnsupportedFeature, build_storage_plan,
};
use quickjs_frontend::{
    DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, SourceFragment,
    with_dynamic_function_source,
};

fn storage_result(
    kind: DynamicFunctionKind,
    body: &str,
) -> Result<quickjs_compiler::StoragePlan, CompilerError> {
    let source = DynamicFunctionSource::new(kind, &[], SourceFragment::new(body));
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        build_storage_plan(unit)
    })
    .expect("dynamic frontend")
}

#[test]
fn ordinary_dynamic_function_is_a_synchronous_script_with_a_named_child() {
    let plan = storage_result(DynamicFunctionKind::Function, "return anonymous;")
        .expect("ordinary dynamic Function storage");

    assert_eq!(plan.kind(), CompilationUnitKind::Script);
    assert_eq!(
        plan.executables()[0].kind(),
        ExecutableKind::Script {
            asynchronous: false
        }
    );
    assert_eq!(plan.executables()[0].parent(), None);
    assert_eq!(
        plan.executables()[1].parent(),
        Some(plan.executables()[0].id())
    );
    assert_eq!(plan.executables()[1].name(), Some("anonymous"));
}

#[test]
fn nonordinary_dynamic_function_families_remain_typed_fail_closed() {
    for kind in [
        DynamicFunctionKind::GeneratorFunction,
        DynamicFunctionKind::AsyncFunction,
        DynamicFunctionKind::AsyncGeneratorFunction,
    ] {
        let error = storage_result(kind, "").expect_err("family must remain unsupported");
        assert!(matches!(
            error,
            CompilerError::Unsupported {
                feature: UnsupportedFeature::DynamicFunctionKind(actual),
                ..
            } if actual == kind
        ));
    }
}

#[test]
fn direct_eval_inside_dynamic_code_remains_rejected() {
    let error = storage_result(DynamicFunctionKind::Function, "return eval('1');")
        .expect_err("direct eval must remain fail closed");

    assert!(matches!(
        error,
        CompilerError::Unsupported {
            feature: UnsupportedFeature::DirectEval,
            ..
        }
    ));
}

#[test]
fn escaped_program_lexicals_are_evaluation_local_while_var_and_function_are_global_object() {
    let plan = storage_result(
        DynamicFunctionKind::Function,
        "}); function declared() {} var objectBacked; let lexical; const fixed = 1; ({",
    )
    .expect("escaped Program declarations");
    let root = plan.executables()[0].id();
    let bindings = plan.bindings_for(root).expect("dynamic Script bindings");
    let lookup = |name: &str| {
        bindings
            .iter()
            .find(|binding| binding.name() == name)
            .expect("named root binding")
    };

    let object = lookup("objectBacked");
    assert_eq!(object.placement(), StoragePlacement::GlobalObject);
    assert_eq!(object.policy().kind(), DeclarationKind::Var);
    assert!(!object.policy().has_temporal_dead_zone());
    assert!(!object.is_frame_captured());

    let lexical = lookup("lexical");
    assert_eq!(lexical.placement(), StoragePlacement::Local);
    assert_eq!(lexical.policy().kind(), DeclarationKind::Let);
    assert!(lexical.policy().has_temporal_dead_zone());
    assert!(!lexical.is_frame_captured());

    let fixed = lookup("fixed");
    assert_eq!(fixed.placement(), StoragePlacement::Local);
    assert_eq!(fixed.policy().kind(), DeclarationKind::Const);
    assert!(fixed.policy().has_temporal_dead_zone());
    assert!(!fixed.is_frame_captured());

    let function = lookup("declared");
    assert_eq!(function.placement(), StoragePlacement::GlobalObject);
    assert_eq!(function.policy().kind(), DeclarationKind::Function);
    assert!(!function.policy().has_temporal_dead_zone());
    assert!(!function.is_frame_captured());
}

#[test]
fn unresolved_dynamic_names_keep_the_oxc_owner_and_access_role() {
    let plan = storage_result(
        DynamicFunctionKind::Function,
        "realmRead; realmWrite = 1; return realmNested;",
    )
    .expect("ordinary dynamic Function globals");
    let wrapper = plan.executables()[1].id();
    let globals = plan
        .unresolved_globals_for(wrapper)
        .expect("wrapper unresolved globals");

    assert_eq!(
        globals
            .iter()
            .map(|global| (
                global.name(),
                global.access().reads(),
                global.access().writes()
            ))
            .collect::<Vec<_>>(),
        [
            ("realmRead", true, false),
            ("realmWrite", false, true),
            ("realmNested", true, false),
        ]
    );
    assert!(globals.iter().all(|global| global.executable() == wrapper));
    assert!(
        plan.frame_captures_for(wrapper)
            .expect("wrapper captures")
            .is_empty(),
        "constructor-realm globals must never become caller-frame captures"
    );
}

#[test]
fn child_reference_to_escaped_program_lexical_captures_the_script_frame_cell() {
    let plan = storage_result(
        DynamicFunctionKind::Function,
        "}); let shared = 1; (function read(){ return shared; }); ({",
    )
    .expect("escaped global referenced by child");
    let root = plan.executables()[0].id();
    let child = plan
        .executables()
        .iter()
        .find(|executable| executable.name() == Some("read"))
        .expect("named child")
        .id();
    let shared = plan
        .bindings_for(root)
        .expect("root bindings")
        .iter()
        .find(|binding| binding.name() == "shared")
        .expect("shared global binding");
    let references = plan
        .resolved_references_for(child)
        .expect("child resolved references");

    assert_eq!(shared.placement(), StoragePlacement::Local);
    assert!(shared.is_frame_captured());
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].binding(), shared.id());
    assert!(references[0].access().reads());
    assert!(!references[0].access().writes());
    let captures = plan.frame_captures_for(child).expect("child captures");
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].executable(), child);
    assert_eq!(captures[0].binding(), shared.id());
    assert_eq!(captures[0].slot().index(), 0);
    assert_eq!(
        captures[0].source(),
        CaptureSource::ParentBinding(shared.id())
    );
}
