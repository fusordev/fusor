use quickjs_bytecode::{AtomPoolIndex, BytecodePc, FinalOpcode, Operands, VerificationLimits};
use quickjs_compiler::{
    CompilationContext, CompiledLeafFunction, LeafCompilationError, UnsupportedLeafFeature,
};
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("ordinary object lowering must succeed")
        },
    )
    .expect("front-end acceptance")
}

fn compile_error(source: &str, name: &str) -> LeafCompilationError {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning must succeed");
            let executable = context
                .executables()
                .find(|executable| executable.metadata().name() == Some(name))
                .expect("named function executable");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect_err("unsupported object form must fail closed")
        },
    )
    .expect("front-end acceptance")
}

fn instructions(compiled: &CompiledLeafFunction) -> Vec<(FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let instruction = instruction.decoded().instruction();
            (instruction.opcode(), instruction.operands())
        })
        .collect()
}

fn source_slice_at<'source>(
    compiled: &CompiledLeafFunction,
    source: &'source str,
    pc: BytecodePc,
) -> &'source str {
    let span = compiled
        .source_instructions()
        .iter()
        .find(|entry| entry.pc() == pc)
        .expect("source entry at final instruction PC")
        .span();
    &source[span.start as usize..span.end as usize]
}

fn atom_code_units(compiled: &CompiledLeafFunction, index: u32) -> Vec<u16> {
    compiled.atoms()[index as usize]
        .string()
        .code_units()
        .collect()
}

#[test]
fn empty_object_literal_uses_the_ordinary_object_opcode() {
    let compiled = compile("function make(){return {};}", "make");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn static_data_properties_are_defined_in_source_order() {
    let compiled = compile("function make(){return {alpha:1,beta:2};}", "make");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::Object, Operands::None),
            (FinalOpcode::Push1, Operands::NoneInt),
            (
                FinalOpcode::DefineField,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (FinalOpcode::Push2, Operands::NoneInt),
            (
                FinalOpcode::DefineField,
                Operands::Atom(AtomPoolIndex::new(1)),
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "alpha".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        atom_code_units(&compiled, 1),
        "beta".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);
}

#[test]
fn static_property_read_uses_the_function_local_atom() {
    let compiled = compile("function read(object){return object.value;}", "read");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetField, Operands::Atom(AtomPoolIndex::new(0)),),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "value".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 1);
}

#[test]
fn static_property_assignment_preserves_and_returns_the_rhs() {
    let source = "function write(object,value){return object.field=value;}";
    let compiled = compile(source, "write");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetArg1, Operands::NoneArg),
            (FinalOpcode::Insert2, Operands::None),
            (FinalOpcode::PutField, Operands::Atom(AtomPoolIndex::new(0)),),
            (FinalOpcode::Return, Operands::None),
        ],
        "Insert2 retains the RHS below the object/value pair consumed by PutField"
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "field".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 3);

    let put = compiled
        .control_flow()
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::PutField)
        .expect("PutField");
    assert!(
        source_slice_at(&compiled, source, put.decoded().pc()).contains("object.field"),
        "the property write source entry points at the static assignment target"
    );
}

#[test]
fn static_method_call_keeps_the_base_object_as_receiver() {
    let source = "function invoke(object){return object.method();}";
    let compiled = compile(source, "invoke");

    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (
                FinalOpcode::GetField2,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count: 0 },
            ),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        atom_code_units(&compiled, 0),
        "method".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(compiled.control_flow().computed_stack_size(), 2);

    let call = compiled
        .control_flow()
        .instructions()
        .iter()
        .find(|instruction| instruction.decoded().instruction().opcode() == FinalOpcode::CallMethod)
        .expect("CallMethod");
    assert_eq!(
        source_slice_at(&compiled, source, call.decoded().pc()),
        "object.method()"
    );
}

#[test]
fn parentheses_preserve_a_member_reference_but_sequences_do_not() {
    let parenthesized = compile(
        "function invoke(object){return ((object.method))();}",
        "invoke",
    );
    assert_eq!(
        instructions(&parenthesized),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (
                FinalOpcode::GetField2,
                Operands::Atom(AtomPoolIndex::new(0)),
            ),
            (
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count: 0 },
            ),
            (FinalOpcode::Return, Operands::None),
        ],
        "parentheses do not erase a JavaScript reference or its receiver"
    );

    let sequence = compile(
        "function invoke(object){return (0,object.method)();}",
        "invoke",
    );
    assert_eq!(
        instructions(&sequence),
        [
            (FinalOpcode::Push0, Operands::NoneInt),
            (FinalOpcode::Drop, Operands::None),
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::GetField, Operands::Atom(AtomPoolIndex::new(0)),),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::Return, Operands::None),
        ],
        "a sequence expression yields a value rather than a bound reference"
    );
}

#[test]
fn strict_this_expression_uses_push_this() {
    let source = "function current(){\"use strict\";return this;}";
    let compiled = compile(source, "current");

    assert!(compiled.control_flow().function_header().mode().is_strict());
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::PushThis, Operands::None),
            (FinalOpcode::Return, Operands::None),
        ]
    );
    assert_eq!(
        source_slice_at(&compiled, source, BytecodePc::new(0)),
        "this"
    );
}

#[test]
fn strict_direct_call_remains_receiverless() {
    let compiled = compile(
        "function invoke(callback){\"use strict\";return callback();}",
        "invoke",
    );

    assert!(compiled.control_flow().function_header().mode().is_strict());
    assert_eq!(
        instructions(&compiled),
        [
            (FinalOpcode::GetArg0, Operands::NoneArg),
            (FinalOpcode::Call0, Operands::NPopX),
            (FinalOpcode::Return, Operands::None),
        ],
        "a strict direct call must not synthesize a base-object receiver"
    );
}

#[test]
fn unsupported_object_forms_fail_closed_at_the_relevant_source() {
    let cases = [
        (
            "function make(key,value){return {[key]:value};}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "key",
        ),
        (
            "function make(object,key){return object[key];}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "key",
        ),
        (
            "function make(object,key,value){return object[key]=value;}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "key",
        ),
        (
            "function make(object,key){return object[key]();}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "key",
        ),
        (
            "function make(value){return {...value};}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "...value",
        ),
        (
            "function make(){return {get value(){return 1;}};}",
            UnsupportedLeafFeature::ObjectMethodOrAccessor,
            "return 1",
        ),
        (
            "function make(){return {set value(next){next;}};}",
            UnsupportedLeafFeature::ObjectMethodOrAccessor,
            "next",
        ),
        (
            "function make(){return {method(){return 1;}};}",
            UnsupportedLeafFeature::ObjectMethodOrAccessor,
            "return 1",
        ),
        (
            "function make(value){return {__proto__:value};}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "__proto__",
        ),
        (
            "function make(value){return {\"__proto__\":value};}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "\"__proto__\"",
        ),
        (
            "function make(){return {handler:function(){}};}",
            UnsupportedLeafFeature::InferredFunctionName,
            "function(){}",
        ),
        (
            "function make(){return this;}",
            UnsupportedLeafFeature::UnsupportedExpression,
            "this",
        ),
    ];

    for (source, expected_feature, expected_fragment) in cases {
        let LeafCompilationError::Unsupported { feature, span } = compile_error(source, "make")
        else {
            panic!("expected unsupported object form for {source}");
        };
        assert_eq!(feature, expected_feature, "{source}");
        let highlighted = &source[span.start as usize..span.end as usize];
        assert!(
            highlighted.contains(expected_fragment),
            "expected diagnostic span containing {expected_fragment:?}, found {highlighted:?}: {source}"
        );
    }
}
