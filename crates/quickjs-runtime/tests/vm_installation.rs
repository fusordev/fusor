use std::sync::Arc;

use quickjs_bytecode::{FinalOpcode, FunctionTemplateId};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};
use quickjs_runtime::{
    AtomLimits, ExecutionLimits, InstallError, PREDEFINED_ATOM_COUNT,
    PREDEFINED_DESCRIPTION_CODE_UNITS, PREDEFINED_INTERNER_SLOTS, Runtime, RuntimeLimits,
};

fn compile(source: &str, root_name: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("install-test.js"))
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

#[test]
fn atom_failure_rolls_back_the_complete_installation() {
    let authority = compile(
        "function fail(argument){\
            let local=\"runtime-value\";\
            return local;\
        }",
        "fail",
    );
    let atom_limits = AtomLimits::new(
        PREDEFINED_ATOM_COUNT + 91,
        PREDEFINED_DESCRIPTION_CODE_UNITS + 776,
        PREDEFINED_INTERNER_SLOTS + 91,
    );
    let mut runtime =
        Runtime::try_new(RuntimeLimits::default().with_atom_limits(atom_limits)).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before_runtime = runtime.usage();
    let before_atoms = runtime.atom_usage();
    let error = {
        let mut context = runtime.context(&realm).expect("context");
        context
            .instantiate(authority)
            .expect_err("atom ceiling must reject installation")
    };
    assert!(matches!(error, InstallError::Atom(_)));
    assert_eq!(runtime.usage(), before_runtime);
    assert_eq!(runtime.atom_usage(), before_atoms);
}

#[test]
fn complete_non_bigint_dynamic_operator_family_is_admitted_across_the_complete_graph() {
    let authority = compile(
        "function outer(){\
            function child(left,right){\
                let value=left;\
                +left;-left;~left;\
                ++value;--value;value++;value--;\
                left*right;left/right;left%right;left+right;left-right;left**right;\
                left<<right;left>>right;left>>>right;\
                left<right;left<=right;left>right;left>=right;\
                left==right;left!=right;left===right;left!==right;\
                left&right;left^right;return left|right;\
            }\
            return 0;\
        }",
        "outer",
    );
    let child = authority
        .function(FunctionTemplateId::new(1))
        .expect("nested child");
    let child_opcodes = child
        .function()
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect::<Vec<_>>();
    for expected in [
        FinalOpcode::Neg,
        FinalOpcode::Plus,
        FinalOpcode::Dec,
        FinalOpcode::Inc,
        FinalOpcode::PostDec,
        FinalOpcode::PostInc,
        FinalOpcode::Not,
        FinalOpcode::Mul,
        FinalOpcode::Div,
        FinalOpcode::Mod,
        FinalOpcode::Add,
        FinalOpcode::Sub,
        FinalOpcode::Pow,
        FinalOpcode::Shl,
        FinalOpcode::Sar,
        FinalOpcode::Shr,
        FinalOpcode::Lt,
        FinalOpcode::Lte,
        FinalOpcode::Gt,
        FinalOpcode::Gte,
        FinalOpcode::Eq,
        FinalOpcode::Neq,
        FinalOpcode::StrictEq,
        FinalOpcode::StrictNeq,
        FinalOpcode::And,
        FinalOpcode::Xor,
        FinalOpcode::Or,
    ] {
        assert!(
            child_opcodes.contains(&expected),
            "child must exercise {expected:?}"
        );
    }

    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    context
        .instantiate(authority)
        .expect("the complete non-BigInt dynamic operator family is supported");
}

/// A nested `BigInt` literal installs rather than failing closed.
///
/// This previously asserted the opposite: `push_bigint_i32` was admitted by the
/// verifier but rejected at installation, so any function containing a `BigInt`
/// literal was uninstallable. The value domain now executes it, and
/// `crates/quickjs-runtime/tests/vm_bigint.rs` pins the observable behavior.
#[test]
fn nested_bigint_literals_install_and_execute() {
    let authority = compile(
        "function outer(){\
            function child(){return 1n;}\
            return 0;\
        }",
        "outer",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    context
        .instantiate(authority)
        .expect("a BigInt literal is executable");
}

#[test]
fn in_operator_remains_rejected_before_runtime_mutation() {
    let authority = compile(
        "function outer(){function child(left,right){return left in right;}return 0;}",
        "outer",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let before = runtime.usage();
    let error = {
        let mut context = runtime.context(&realm).expect("context");
        context
            .instantiate(authority)
            .expect_err("in operator remains deferred")
    };
    assert!(matches!(
        error,
        InstallError::UnsupportedOpcode {
            function,
            opcode: FinalOpcode::In,
            ..
        } if function == FunctionTemplateId::new(1)
    ));
    assert_eq!(runtime.usage(), before);
}

#[test]
fn instanceof_is_admitted_across_the_complete_graph() {
    let authority = compile(
        "function outer(){function child(left,right){return left instanceof right;}return 0;}",
        "outer",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    context
        .instantiate(authority)
        .expect("instanceof is admitted");
}

#[test]
fn one_arc_authority_installs_independently_into_two_runtimes() {
    let authority = compile("function value(){return \"shared-code\";}", "value");
    let mut first = Runtime::try_new(RuntimeLimits::default()).expect("first runtime");
    let first_realm = first.create_realm().expect("first realm");
    let mut second = Runtime::try_new(RuntimeLimits::default()).expect("second runtime");
    let second_realm = second.create_realm().expect("second realm");

    let mut first_context = first.context(&first_realm).expect("first context");
    let first_function = first_context
        .instantiate(Arc::clone(&authority))
        .expect("first function");
    let first_value = first_context
        .call(&first_function, &[], ExecutionLimits::default())
        .expect("first result");

    let mut second_context = second.context(&second_realm).expect("second context");
    let second_function = second_context
        .instantiate(authority)
        .expect("second function");
    let second_value = second_context
        .call(&second_function, &[], ExecutionLimits::default())
        .expect("second result");

    for value in [&first_value, &second_value] {
        assert_eq!(
            value
                .as_string()
                .expect("live value")
                .expect("string")
                .to_utf8_lossy()
                .expect("UTF-8"),
            "shared-code"
        );
    }
    assert!(first_function.same_identity(&second_function).is_err());
}

#[test]
fn long_lived_context_drains_dropped_roots_before_installation_limits() {
    let first = compile("function first(){return 1;}", "first");
    let second = compile("function second(){return 2;}", "second");
    let mut runtime = Runtime::try_new(
        RuntimeLimits::default()
            .with_max_public_roots(1)
            .with_max_heap_functions(132),
    )
    .expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");

    let first = context.instantiate(first).expect("first function");
    drop(first);
    assert_eq!(context.runtime_usage().pending_releases(), 1);

    let second = context
        .instantiate(second)
        .expect("the dropped root must be drained before preflight");
    assert_eq!(context.runtime_usage().public_roots(), 1);
    assert_eq!(context.runtime_usage().pending_releases(), 0);
    drop(second);
}

#[test]
fn deferred_release_mailbox_reserves_for_every_outstanding_root() {
    let authority = compile("function value(){return 1;}", "value");
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let baseline = runtime.usage();
    {
        let mut context = runtime.context(&realm).expect("context");
        let roots = (0..8)
            .map(|_| {
                context
                    .instantiate(Arc::clone(&authority))
                    .expect("independent root")
            })
            .collect::<Vec<_>>();
        assert_eq!(context.runtime_usage().public_roots(), 8);

        drop(roots);
        assert_eq!(context.runtime_usage().pending_releases(), 8);
    }

    runtime.collect_cycles().expect("collection");
    assert_eq!(runtime.usage(), baseline);
}
