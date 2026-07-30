use quickjs_compiler::{
    CompilationUnitKind, CompilerError, ExecutableKind, UnsupportedFeature, build_storage_plan,
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
