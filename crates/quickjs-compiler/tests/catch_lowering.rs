use quickjs_bytecode::{FinalOpcode, VerificationLimits};
use quickjs_compiler::{
    CompilationContext, CompiledFunctionTree, CompiledLeafFunction, LeafCompilationError,
    UnsupportedLeafFeature,
};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
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
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("catch-only lowering")
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
                .expect("catch-only tree lowering")
        },
    )
    .expect("front-end acceptance")
}

fn compile_error(source: &str, name: &str) -> LeafCompilationError {
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
                .compile_leaf(&executable, VerificationLimits::default())
                .expect_err("unsupported catch form must fail closed")
        },
    )
    .expect("front-end acceptance")
}

fn opcodes(compiled: &CompiledLeafFunction) -> Vec<FinalOpcode> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| instruction.decoded().instruction().opcode())
        .collect()
}

fn is_put_local(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc
    )
}

#[test]
fn catch_only_accepts_optional_and_simple_identifier_bindings() {
    let optional = compile(
        "function optional(){try{throw 1;}catch{return 2;}}",
        "optional",
    );
    let identifier = compile(
        "function identifier(){try{throw 1;}catch(error){return error;}}",
        "identifier",
    );

    for compiled in [&optional, &identifier] {
        let opcodes = opcodes(compiled);
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&opcode| opcode == FinalOpcode::Catch)
                .count(),
            1
        );
        assert!(opcodes.contains(&FinalOpcode::Throw));
        assert!(opcodes.contains(&FinalOpcode::Return));
    }
    assert!(
        !opcodes(&optional).contains(&FinalOpcode::NipCatch),
        "a direct throw leaves its nearest catch marker installed"
    );
    assert!(
        opcodes(&optional)
            .windows(2)
            .any(|window| window == [FinalOpcode::Drop, FinalOpcode::Push2]),
        "an optional catch binding drops the incoming exception"
    );
    assert!(
        opcodes(&identifier)
            .iter()
            .any(|opcode| matches!(opcode, FinalOpcode::PutLoc0 | FinalOpcode::PutLoc)),
        "a simple catch binding consumes the incoming exception into its local"
    );
}

#[test]
fn catch_only_rejects_finalizers_and_destructuring_at_the_exact_form() {
    let finalizer_source = "function f(){try{}catch{}finally{}}";
    let LeafCompilationError::Unsupported { feature, span } = compile_error(finalizer_source, "f")
    else {
        panic!("finally must remain typed fail-closed");
    };
    assert_eq!(feature, UnsupportedLeafFeature::UnsupportedBody);
    assert_eq!(
        &finalizer_source[span.start as usize..span.end as usize],
        "{}"
    );

    let destructuring_source = "function f(){try{}catch({message}){return message;}}";
    let LeafCompilationError::Unsupported { feature, span } =
        compile_error(destructuring_source, "f")
    else {
        panic!("destructuring catch bindings must remain typed fail-closed");
    };
    assert_eq!(feature, UnsupportedLeafFeature::UnsupportedBinding);
    assert_eq!(
        &destructuring_source[span.start as usize..span.end as usize],
        "{message}"
    );
}

#[test]
fn nested_catch_and_for_in_abrupt_cleanup_follows_marker_nesting() {
    let value_return = compile(
        "function valueReturn(object){try{for(const key in object)return key;}catch{return 0;}}",
        "valueReturn",
    );
    assert!(
        opcodes(&value_return).windows(3).any(|window| {
            window == [FinalOpcode::Nip, FinalOpcode::NipCatch, FinalOpcode::Return]
        }),
        "value return removes the inner for-in marker before the outer catch marker"
    );

    let void_return = compile(
        "function voidReturn(object){try{for(const key in object)return;}catch{}}",
        "voidReturn",
    );
    assert!(
        opcodes(&void_return).windows(3).any(|window| {
            window
                == [
                    FinalOpcode::Drop,
                    FinalOpcode::Drop,
                    FinalOpcode::ReturnUndef,
                ]
        }),
        "void return drops the inner for-in marker before the outer catch marker"
    );

    let thrown = compile(
        "function thrown(object){try{for(const key in object)throw key;}catch{return 0;}}",
        "thrown",
    );
    assert!(
        opcodes(&thrown)
            .windows(2)
            .any(|window| window == [FinalOpcode::Nip, FinalOpcode::Throw]),
        "throw removes only the for-in marker above its nearest catch"
    );

    let catch_inside_for_in = compile(
        "function catchInsideForIn(object){for(const key in object){try{return key;}catch{return 0;}}}",
        "catchInsideForIn",
    );
    assert!(
        opcodes(&catch_inside_for_in).windows(3).any(|window| {
            window == [FinalOpcode::NipCatch, FinalOpcode::Nip, FinalOpcode::Return]
        }),
        "reversed nesting removes the inner catch marker before the outer for-in marker"
    );
}

#[test]
fn labeled_loop_jumps_drop_each_crossed_marker() {
    for (name, keyword) in [("breakOuter", "break"), ("continueOuter", "continue")] {
        let source = format!(
            "function {name}(object){{outer:while(true){{try{{for(const key in object){{{keyword} outer;}}}}catch{{}}break;}}}}"
        );
        let compiled = compile(&source, name);
        assert!(
            opcodes(&compiled).windows(3).any(|window| {
                window[0] == FinalOpcode::Drop
                    && window[1] == FinalOpcode::Drop
                    && matches!(
                        window[2],
                        FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
                    )
            }),
            "{keyword} must drop the inner for-in marker and outer catch marker"
        );
    }
}

#[test]
fn captured_catch_binding_closes_at_the_catch_scope_exit() {
    let tree = compile_tree(
        "function outer(){let saved;try{throw 1;}catch(error){saved=function inner(){return error;};}return saved;}",
        "outer",
    );
    let root = tree.root();
    let opcodes = opcodes(root);
    let close = opcodes
        .iter()
        .position(|&opcode| opcode == FinalOpcode::CloseLoc)
        .expect("captured catch local is closed");
    let final_return = opcodes
        .iter()
        .rposition(|&opcode| opcode == FinalOpcode::Return)
        .expect("outer return");

    assert!(close < final_return);
}

#[test]
fn catch_binding_initialization_follows_hoisted_handler_functions() {
    let source = "function outer(){\"use strict\";try{throw 1;}catch(error){function helper(){return error;}return helper;}}";
    let tree = compile_tree(source, "outer");
    let root = tree.root();
    let instructions = root.control_flow().instructions();
    let closure = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::FClosure | FinalOpcode::FClosure8
            )
        })
        .expect("handler function initializer");
    let catch_put = instructions
        .iter()
        .position(|instruction| {
            let decoded = instruction.decoded();
            let Some(mapping) = root
                .source_instructions()
                .iter()
                .find(|mapping| mapping.pc() == decoded.pc())
            else {
                return false;
            };
            let span = mapping.span();
            is_put_local(decoded.instruction().opcode())
                && &source[span.start as usize..span.end as usize] == "error"
        })
        .expect("catch parameter initialization");

    assert!(is_put_local(
        instructions[closure + 1].decoded().instruction().opcode()
    ));
    assert!(
        catch_put < closure,
        "the handler-first catch write precedes catch-body function initialization"
    );
}
