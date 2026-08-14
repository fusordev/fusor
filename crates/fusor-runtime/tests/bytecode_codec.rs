//! Verified-bytecode graph-section codec round trip (§8.2): a compiled
//! Global Script's verified graph encodes as raw bytecode + pools +
//! layouts and decodes through the load-time re-verification pipeline
//! (§8.3), reproducing the original graph exactly (the `Eq` derive is the
//! oracle).

use std::sync::Arc;

use fusor_bytecode::{
    BytecodeCodecError, decode_verified_bytecode, encode_verified_bytecode,
};
use fusor_compiler::CompilationContext;
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

/// Compiles one Global Script into its verified bytecode authority.
fn compile(source: &str) -> Arc<fusor_bytecode::VerifiedBytecode> {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new_with_source_name(unit, Arc::from("codec.js"))
                .expect("storage plan");
            let tree = context
                .compile_global_script(fusor_bytecode::VerificationLimits::default())
                .expect("verified Global Script");
            Arc::new(tree.verified_bytecode().clone())
        },
    )
    .expect("frontend")
}

const RICH_SCRIPT: &str = "\
function tag(site) { return site.raw[0]; }\
function outer(x) {\
    var wide = { '中键': x };\
    return function inner(y) { return wide['中键'] + y; };\
}\
var closure = outer(41);\
var template = tag`a\\u{1F600}b`;\
var big = 12345678901234567890n;\
var numbers = [1.5, 0.1, 1e300];\
var text = 'hello fusor';\
globalThis.summary = [closure(1), template, String(big), numbers.length, text.length];\
String(globalThis.summary.join('|'));";

#[test]
fn a_verified_graph_round_trips_through_the_codec() {
    let authority = compile(RICH_SCRIPT);
    let payload = encode_verified_bytecode(&authority).expect("encode");
    let decoded = decode_verified_bytecode(&payload).expect("decode");
    for (decoded_function, original_function) in decoded.functions().zip(authority.functions()) {
        let decoded_function = decoded_function.function();
        let original_function = original_function.function();
        let decoded_flow = decoded_function.control_flow();
        let original_flow = original_function.control_flow();
        assert_eq!(decoded_flow.bytecode(), original_flow.bytecode(), "bytecode");
        assert_eq!(
            decoded_flow.computed_stack_size(),
            original_flow.computed_stack_size(),
            "computed stack size"
        );
        assert_eq!(
            decoded_flow.transfer_evaluations(),
            original_flow.transfer_evaluations(),
            "transfer evaluations"
        );
        assert_eq!(decoded_flow.domains(), original_flow.domains(), "domains");
        assert_eq!(
            decoded_flow.function_header(),
            original_flow.function_header(),
            "header"
        );
        assert_eq!(decoded_function.atoms(), original_function.atoms(), "atoms");
        assert_eq!(
            decoded_function.constants(),
            original_function.constants(),
            "constants"
        );
        assert_eq!(
            decoded_function.closure_sources(),
            original_function.closure_sources(),
            "closure sources"
        );
        assert_eq!(
            decoded_function.has_direct_eval(),
            original_function.has_direct_eval(),
            "direct eval"
        );
        assert_eq!(
            decoded_function.parameter_initialization_end(),
            original_function.parameter_initialization_end(),
            "parameter init end"
        );
        assert_eq!(
            decoded_function.function_initializer_prefix_start(),
            original_function.function_initializer_prefix_start(),
            "initializer prefix"
        );
        assert_eq!(
            decoded_function.eval_reference_call_instructions(),
            original_function.eval_reference_call_instructions(),
            "eval references"
        );
    }
    assert_eq!(decoded.metadata(), authority.metadata(), "metadata");
    assert_eq!(decoded.usage(), authority.usage(), "usage");
    assert_eq!(decoded.requirements(), authority.requirements(), "requirements");
    assert_eq!(&decoded, authority.as_ref(), "the re-verified authority is identical");
    // The decoded graph still authorizes the original behavior.
    assert_eq!(
        decoded.root().function().control_flow().bytecode(),
        authority.compiler_graph().root().control_flow().bytecode(),
        "the root bytecode is bit-identical"
    );
}

#[test]
fn graph_decoding_fails_closed_on_damage() {
    let authority = compile(RICH_SCRIPT);
    let payload = encode_verified_bytecode(&authority).expect("encode");

    // Truncation fails closed.
    assert!(matches!(
        decode_verified_bytecode(&payload[..payload.len() - 1]),
        Err(BytecodeCodecError::Truncated)
    ));

    // A corrupted bytecode byte fails the re-verification, never panics.
    let mut damaged = payload.clone();
    // FUSRBYTE header (8) + stamp (4) + count (4) + tag (1) + length (8) +
    // graph count (4): the graph payload's bytecode begins at offset 29+4.
    let bytecode_offset = 8 + 4 + 4 + 1 + 8 + 4 + 4;
    damaged[bytecode_offset + 1] ^= 0xFF;
    let error =
        decode_verified_bytecode(&damaged).expect_err("corrupted bytecode must be rejected");
    assert!(
        matches!(error, BytecodeCodecError::Verification(_)),
        "unexpected error: {error}"
    );
}
