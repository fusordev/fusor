mod support;

use std::sync::Arc;

use fusor_bytecode::VerificationLimits;
use fusor_compiler::{CompilationContext, CompiledFunctionTree};
use fusor_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};

use support::snapshot_compiled_function_tree;

fn compile_representative_tree() -> CompiledFunctionTree {
    let source = "function outer({seed = 1}, ...items) {\
        let total = seed;\
        const [head = 0, ...tail] = items;\
        for (const value of tail) {\
            try {\
                if (value) { total += value; } else { continue; }\
            } finally { total += 1; }\
        }\
        function inner(delta = head) { return total + delta; }\
        return inner;\
    }";
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context =
                CompilationContext::new_with_source_name(unit, Arc::from("characterization.js"))
                    .expect("representative storage planning must succeed");
            let root = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("outer"))
                .expect("representative root function must exist");
            context
                .compile_tree(&root, VerificationLimits::default())
                .expect("representative function tree must compile")
        },
    )
    .expect("representative source must parse")
}

fn compile_dynamic_function_tree() -> CompiledFunctionTree {
    let parameters = [
        SourceFragment::new("left"),
        SourceFragment::new("right = 2"),
    ];
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &parameters,
        SourceFragment::new("const sum = left + right; return function inner(){ return sum; };"),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new_with_source_name(
            unit,
            Arc::from("dynamic-characterization.js"),
        )
        .expect("dynamic storage planning must succeed");
        context
            .compile_dynamic_function_script(VerificationLimits::default())
            .expect("dynamic function tree must compile")
    })
    .expect("dynamic function source must parse")
}

#[test]
fn complete_lowering_artifacts_have_a_stable_snapshot() {
    let mut snapshot =
        snapshot_compiled_function_tree("representative", &compile_representative_tree());
    snapshot.push('\n');
    snapshot.push_str(&snapshot_compiled_function_tree(
        "dynamic function",
        &compile_dynamic_function_tree(),
    ));

    let expected =
        include_str!("support/snapshots/complete-lowering-artifacts.txt").replace("\r\n", "\n");
    if snapshot != expected {
        let first_difference = snapshot
            .bytes()
            .zip(expected.bytes())
            .position(|(actual, expected)| actual != expected);
        panic!(
            "lowering snapshot changed: actual bytes {}, expected bytes {}, first difference {:?}",
            snapshot.len(),
            expected.len(),
            first_difference
        );
    }
}
