//! Verified-bytecode graph-section codec round trip (§8.2): a compiled
//! Global Script's verified graph encodes as raw bytecode + pools +
//! layouts and decodes through the load-time re-verification pipeline
//! (§8.3), reproducing the original graph exactly (the `Eq` derive is the
//! oracle).

use std::sync::Arc;

use fusor_bytecode::{BytecodeCodecError, decode_graph, encode_graph};
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
    let graph = authority.compiler_graph();
    let payload = encode_graph(graph);
    let decoded = decode_graph(&payload).expect("decode");
    assert_eq!(&decoded, graph.as_ref(), "the re-verified graph is identical");
    // The decoded graph still authorizes the original behavior.
    assert_eq!(
        decoded.root().control_flow().bytecode(),
        graph.root().control_flow().bytecode(),
        "the root bytecode is bit-identical"
    );
}

#[test]
fn graph_decoding_fails_closed_on_damage() {
    let authority = compile(RICH_SCRIPT);
    let graph = authority.compiler_graph();
    let payload = encode_graph(graph);

    // Truncation fails closed.
    assert!(matches!(
        decode_graph(&payload[..payload.len() - 1]),
        Err(BytecodeCodecError::Truncated)
    ));

    // A corrupted bytecode byte fails the re-verification, never panics.
    let mut damaged = payload.clone();
    let bytecode_offset = 8; // count(4) + bytecode length(4)
    damaged[bytecode_offset + 1] ^= 0xFF;
    let error = decode_graph(&damaged).expect_err("corrupted bytecode must be rejected");
    assert!(
        matches!(error, BytecodeCodecError::Verification(_)),
        "unexpected error: {error}"
    );
}
