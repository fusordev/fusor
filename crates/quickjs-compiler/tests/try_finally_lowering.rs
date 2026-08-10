use quickjs_bytecode::{FinalOpcode, VerificationLimits};
use quickjs_compiler::{CompilationContext, CompiledFunctionTree, CompiledLeafFunction};
use quickjs_frontend::{
    CompilationGoal, DynamicFunctionKind, DynamicFunctionSource, FrontendLimits, FrontendOptions,
    GlobalScriptGoal, SourceFragment, with_dynamic_function_source, with_parsed_program,
};

fn compile(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("ordinary try/finally lowering")
        },
    )
    .expect("front-end acceptance")
}

fn compile_tree(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("ordinary try/finally tree lowering")
        },
    )
    .expect("front-end acceptance")
}

fn compile_dynamic_script(body: &str) -> CompiledFunctionTree {
    let source = DynamicFunctionSource::new(
        DynamicFunctionKind::Function,
        &[],
        SourceFragment::new(body),
    );
    with_dynamic_function_source(source, FrontendLimits::default(), |unit, _| {
        let context = CompilationContext::new(unit).expect("dynamic storage plan");
        context
            .compile_dynamic_function_script(VerificationLimits::default())
            .expect("dynamic Script lowering")
    })
    .expect("dynamic front-end acceptance")
}

fn opcodes(compiled: &CompiledLeafFunction) -> Vec<FinalOpcode> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect()
}

fn is_goto(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
    )
}

#[test]
fn normal_and_exceptional_paths_share_one_finalizer_subroutine() {
    let compiled = compile("function f(value){try{value;}finally{value;}}", "f");
    let opcodes = opcodes(compiled.root());

    assert!(opcodes.windows(5).any(|window| {
        window[..4]
            == [
                FinalOpcode::Drop,
                FinalOpcode::Undefined,
                FinalOpcode::Gosub,
                FinalOpcode::Drop,
            ]
            && is_goto(window[4])
    }));
    assert!(
        opcodes
            .windows(2)
            .any(|window| window == [FinalOpcode::Gosub, FinalOpcode::Throw])
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::Catch)
            .count(),
        1
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::Ret)
            .count(),
        1
    );
}

#[test]
fn normal_finalizer_expressions_do_not_replace_script_completion() {
    let tree = compile_dynamic_script("}); try { 1; } finally { 2; } if (false) (function() {");
    let opcodes = opcodes(tree.root());

    assert!(
        opcodes
            .windows(2)
            .any(|window| window == [FinalOpcode::Push1, FinalOpcode::PutLoc0]),
        "the protected expression updates Script completion"
    );
    assert!(
        opcodes
            .windows(2)
            .any(|window| window == [FinalOpcode::Push2, FinalOpcode::PutLoc0]),
        "the finalizer records its own Script completion until it returns normally"
    );
    assert!(
        opcodes.windows(5).any(|window| {
            window
                == [
                    FinalOpcode::GetLoc0,
                    FinalOpcode::PutLoc1,
                    FinalOpcode::Undefined,
                    FinalOpcode::PutLoc0,
                    FinalOpcode::Gosub,
                ]
        }),
        "the protected completion is saved before entering the finalizer"
    );
    assert!(
        opcodes.windows(3).any(|window| window
            == [
                FinalOpcode::Gosub,
                FinalOpcode::GetLoc1,
                FinalOpcode::PutLoc0
            ]),
        "a normally completing finalizer restores the protected completion"
    );
}

#[test]
fn return_preserves_its_value_across_each_finalizer() {
    let value = compile(
        "function valueReturn(value){try{return value;}finally{value;}}",
        "valueReturn",
    );
    assert!(opcodes(value.root()).windows(3).any(|window| {
        window
            == [
                FinalOpcode::NipCatch,
                FinalOpcode::Gosub,
                FinalOpcode::Return,
            ]
    }));

    let undefined = compile(
        "function voidReturn(value){try{return;}finally{value;}}",
        "voidReturn",
    );
    assert!(opcodes(undefined.root()).windows(4).any(|window| {
        window
            == [
                FinalOpcode::Undefined,
                FinalOpcode::NipCatch,
                FinalOpcode::Gosub,
                FinalOpcode::Return,
            ]
    }));

    let nested = compile(
        "function nested(value){try{try{return value;}finally{value;}}finally{value;}}",
        "nested",
    );
    assert!(
        opcodes(nested.root()).windows(5).any(|window| {
            window
                == [
                    FinalOpcode::NipCatch,
                    FinalOpcode::Gosub,
                    FinalOpcode::NipCatch,
                    FinalOpcode::Gosub,
                    FinalOpcode::Return,
                ]
        }),
        "inner finalizer runs before the outer finalizer"
    );
}

#[test]
fn throw_reaches_the_handler_before_running_the_finalizer() {
    let compiled = compile("function f(value){try{throw value;}finally{value;}}", "f");
    let direct_opcodes = opcodes(compiled.root());
    assert!(
        direct_opcodes
            .windows(2)
            .any(|window| window == [FinalOpcode::Gosub, FinalOpcode::Throw])
    );
    assert_eq!(
        direct_opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::Throw)
            .count(),
        2,
        "the protected throw and the handler rethrow remain distinct"
    );

    let nested = compile(
        "function nested(value){try{try{throw value;}finally{value;}}catch(error){return error;}}",
        "nested",
    );
    let nested_opcodes = opcodes(nested.root());
    assert!(
        nested_opcodes
            .windows(2)
            .any(|window| window == [FinalOpcode::Gosub, FinalOpcode::Throw])
    );
    assert_eq!(
        nested_opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::Catch)
            .count(),
        2,
        "the inner finalizer rethrow reaches the outer catch"
    );
}

#[test]
fn generated_rethrow_cleans_the_crossed_outer_for_in_marker() {
    let compiled = compile(
        "function f(object){for(const key in object){try{throw key;}finally{void key;}}}",
        "f",
    );
    let opcodes = opcodes(compiled.root());

    assert!(
        opcodes
            .windows(3)
            .any(|window| { window == [FinalOpcode::Gosub, FinalOpcode::Nip, FinalOpcode::Throw] }),
        "the handler rethrow must clean the for-in marker after running the finalizer: {opcodes:?}"
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::Throw)
            .count(),
        2,
        "the protected source throw and generated handler rethrow remain distinct"
    );
}

#[test]
fn break_and_continue_crossing_finally_use_the_normal_cleanup_protocol() {
    for (name, keyword) in [("breakOuter", "break"), ("continueOuter", "continue")] {
        let source = format!(
            "function {name}(value){{outer:while(value){{try{{{keyword} outer;}}finally{{value;}}}}}}"
        );
        let compiled = compile(&source, name);
        assert!(
            opcodes(compiled.root()).windows(5).any(|window| {
                window[..4]
                    == [
                        FinalOpcode::Drop,
                        FinalOpcode::Undefined,
                        FinalOpcode::Gosub,
                        FinalOpcode::Drop,
                    ]
                    && is_goto(window[4])
            }),
            "{keyword} must run the crossed finalizer before its jump"
        );
    }
}

#[test]
fn break_and_continue_in_finally_override_and_discard_the_pending_subroutine() {
    for (name, keyword) in [("breakOverride", "break"), ("continueOverride", "continue")] {
        let source = format!(
            "function {name}(value){{outer:while(value){{try{{value;}}finally{{{keyword} outer;}}}}}}"
        );
        let compiled = compile(&source, name);
        assert!(
            opcodes(compiled.root()).windows(3).any(|window| {
                window[0] == FinalOpcode::Drop
                    && window[1] == FinalOpcode::Drop
                    && is_goto(window[2])
            }),
            "{keyword} from a finalizer must discard its return address and pending completion"
        );
    }
}

#[test]
fn catch_and_finally_protects_both_try_and_catch_bodies() {
    let compiled = compile(
        "function f(value){try{if(value)throw value;}catch(error){if(error)throw error;}finally{value;}}",
        "f",
    );
    let opcodes = opcodes(compiled.root());
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::Catch)
            .count(),
        2
    );
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::Gosub)
            .count(),
        3
    );
}

#[test]
fn return_and_throw_in_finally_override_the_pending_completion() {
    let returned = compile(
        "function returned(){try{return 1;}finally{return 2;}}",
        "returned",
    );
    let returned_opcodes = opcodes(returned.root());
    assert!(returned_opcodes.windows(4).any(|window| {
        window
            == [
                FinalOpcode::Push2,
                FinalOpcode::Nip,
                FinalOpcode::Nip,
                FinalOpcode::Return,
            ]
    }));
    assert!(!returned_opcodes.contains(&FinalOpcode::Ret));

    let thrown = compile(
        "function thrown(){try{return 1;}finally{throw 2;}}",
        "thrown",
    );
    let thrown_opcodes = opcodes(thrown.root());
    assert!(thrown_opcodes.windows(4).any(|window| window
        == [
            FinalOpcode::Push2,
            FinalOpcode::Nip,
            FinalOpcode::Nip,
            FinalOpcode::Throw,
        ]));
    assert!(!thrown_opcodes.contains(&FinalOpcode::Ret));
}

#[test]
fn return_from_finally_removes_its_pair_before_outer_catch_and_for_in_cleanup() {
    let outer_catch = compile(
        "function outerCatch(){try{try{}finally{return 1;}}catch{return 2;}}",
        "outerCatch",
    );
    assert!(opcodes(outer_catch.root()).windows(4).any(|window| {
        window
            == [
                FinalOpcode::Nip,
                FinalOpcode::Nip,
                FinalOpcode::NipCatch,
                FinalOpcode::Return,
            ]
    }));

    let outer_for_in = compile(
        "function outerForIn(object){for(const key in object){try{}finally{return key;}}}",
        "outerForIn",
    );
    assert!(
        opcodes(outer_for_in.root()).windows(4).any(|window| {
            window
                == [
                    FinalOpcode::Nip,
                    FinalOpcode::Nip,
                    FinalOpcode::Nip,
                    FinalOpcode::Return,
                ]
        }),
        "the finalizer pair is removed before the for-in marker"
    );

    let nested = compile(
        "function nested(object){try{for(const key in object){try{}finally{return key;}}}catch{return 0;}}",
        "nested",
    );
    assert!(
        opcodes(nested.root()).windows(5).any(|window| {
            window
                == [
                    FinalOpcode::Nip,
                    FinalOpcode::Nip,
                    FinalOpcode::Nip,
                    FinalOpcode::NipCatch,
                    FinalOpcode::Return,
                ]
        }),
        "inner finalizer, for-in, then outer catch cleanup remains ordered"
    );
}

#[test]
fn throw_from_finally_removes_its_pair_before_outer_for_in_cleanup() {
    let source =
        "function thrown(object){for(const key in object){try{return key;}finally{throw key;}}}";
    let compiled = compile(source, "thrown");
    assert!(
        opcodes(compiled.root()).windows(4).any(|window| {
            window
                == [
                    FinalOpcode::Nip,
                    FinalOpcode::Nip,
                    FinalOpcode::Nip,
                    FinalOpcode::Throw,
                ]
        }),
        "the finalizer pair is removed before the outer for-in marker"
    );
}

#[test]
fn captured_cells_close_before_protected_and_finalizer_cleanup() {
    let protected = compile_tree(
        "function outer(value){let saved;target:{try{{let cell=value;saved=function inner(){return cell;};break target;}}finally{value;}}return saved;}",
        "outer",
    );
    assert!(opcodes(protected.root()).windows(6).any(|window| {
        window[..5]
            == [
                FinalOpcode::CloseLoc,
                FinalOpcode::Drop,
                FinalOpcode::Undefined,
                FinalOpcode::Gosub,
                FinalOpcode::Drop,
            ]
            && is_goto(window[5])
    }));

    let finalizer = compile_tree(
        "function outer(value){let saved;try{value;}finally{let cell=value;saved=function inner(){return cell;};}return saved;}",
        "outer",
    );
    assert!(
        opcodes(finalizer.root())
            .windows(2)
            .any(|window| window == [FinalOpcode::CloseLoc, FinalOpcode::Ret])
    );
}

#[test]
fn async_try_finally_uses_the_verified_finalizer_program() {
    let compiled = compile(
        "async function asyncFinally(value){try{return await value;}finally{value;}}",
        "asyncFinally",
    );
    let opcodes = opcodes(compiled.root());

    assert!(opcodes.contains(&FinalOpcode::Await));
    assert!(opcodes.contains(&FinalOpcode::Ret));
    assert!(opcodes.contains(&FinalOpcode::ReturnAsync));
}
