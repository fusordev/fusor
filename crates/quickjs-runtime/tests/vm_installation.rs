use std::sync::Arc;

use quickjs_bytecode::{FinalOpcode, FunctionTemplateId, VerificationLimits};
use quickjs_compiler::CompilationContext;
use quickjs_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};
use quickjs_runtime::{
    AtomLimits, ExecutionLimits, InstallError, PREDEFINED_ATOM_COUNT,
    PREDEFINED_DESCRIPTION_CODE_UNITS, PREDEFINED_INTERNER_SLOTS, Runtime, RuntimeLimits,
    RuntimeResource,
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
                .compile_tree(&root, VerificationLimits::default())
                .expect("verified function tree");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

fn compile_dynamic(parameters: &[&str], body: &str) -> Arc<quickjs_bytecode::VerifiedBytecode> {
    let parameters = parameters
        .iter()
        .map(|parameter| SourceFragment::new(parameter))
        .collect::<Vec<_>>();
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new(body),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        context
            .compile_dynamic_function_script(VerificationLimits::default())
            .map(|tree| Arc::new(tree.verified_bytecode().clone()))
    })
    .expect("dynamic frontend")
    .expect("dynamic compiler")
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
        PREDEFINED_ATOM_COUNT + 183,
        PREDEFINED_DESCRIPTION_CODE_UNITS + 1_414,
        PREDEFINED_INTERNER_SLOTS + 183,
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

#[test]
fn public_root_materializes_ordinary_function_metadata_and_constructor_prototype() {
    let subject = compile(
        "function subject(first,second){return first+second;}",
        "subject",
    );
    let inspect = compile_dynamic(
        &["candidate"],
        "let length=Object.getOwnPropertyDescriptor(candidate,'length');\
         let name=Object.getOwnPropertyDescriptor(candidate,'name');\
         let prototype=Object.getOwnPropertyDescriptor(candidate,'prototype');\
         let object=new candidate(2,3);\
         return Object.getOwnPropertyNames(candidate).join(',')+'|'+\
             candidate.length+'|'+candidate.name+'|'+\
             length.writable+','+length.enumerable+','+length.configurable+'|'+\
             name.writable+','+name.enumerable+','+name.configurable+'|'+\
             prototype.writable+','+prototype.enumerable+','+prototype.configurable+'|'+\
             (candidate.prototype.constructor===candidate)+'|'+\
             Object.getOwnPropertyNames(candidate.prototype).join(',')+'|'+\
             (Object.getPrototypeOf(candidate)===Function.prototype)+'|'+\
             (Object.getPrototypeOf(candidate.prototype)===Object.prototype)+'|'+\
             (object instanceof candidate)+'|'+candidate(2,3);",
    );
    let mut runtime = Runtime::try_new(RuntimeLimits::default()).expect("runtime");
    let realm = runtime.create_realm().expect("realm");
    let mut context = runtime.context(&realm).expect("context");
    let subject = context.instantiate(subject).expect("subject");
    let inspect = context
        .execute_dynamic_function_script(inspect, ExecutionLimits::default())
        .expect("inspect Script")
        .into_function()
        .expect("inspect function");

    let result = context
        .call(&inspect, &[subject.as_value()], ExecutionLimits::default())
        .expect("inspect public root");

    assert_eq!(
        result
            .as_string()
            .expect("live value")
            .expect("String")
            .to_utf8_lossy()
            .expect("UTF-8"),
        "length,name,prototype|2|subject|false,false,true|false,false,true|\
         true,false,false|true|constructor|true|true|true|5"
    );
}

#[test]
fn public_root_metadata_preflight_is_failure_atomic() {
    let authority = compile("function subject(first,second){}", "subject");
    for (limits, resource, limit, observed) in [
        (
            RuntimeLimits::default().with_max_heap_objects(31),
            RuntimeResource::HeapObjects,
            31,
            32,
        ),
        (
            RuntimeLimits::default().with_max_object_properties(908),
            RuntimeResource::ObjectProperties,
            908,
            912,
        ),
    ] {
        let mut runtime = Runtime::try_new(limits).expect("runtime");
        let realm = runtime.create_realm().expect("realm");
        let usage = runtime.usage();
        let atoms = runtime.atom_usage();

        let error = runtime
            .context(&realm)
            .expect("context")
            .instantiate(Arc::clone(&authority))
            .expect_err("root metadata must exceed the exact limit");

        assert!(
            matches!(
                error,
                InstallError::LimitExceeded {
                    resource: actual_resource,
                    limit: actual_limit,
                    observed: actual_observed,
                } if actual_resource == resource
                    && actual_limit == limit
                    && actual_observed == observed
            ),
            "unexpected installation failure: {error:?}"
        );
        assert_eq!(runtime.usage(), usage);
        assert_eq!(runtime.atom_usage(), atoms);
    }
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
            .with_max_heap_functions(265),
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
