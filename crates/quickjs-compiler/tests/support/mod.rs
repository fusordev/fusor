use std::fmt::Write as _;

use quickjs_bytecode::{
    DisassemblyLimits, FunctionTemplateId, InstructionDecoder, VerifiedBytecodeFunction,
    render_disassembly,
};
use quickjs_compiler::{CompiledFunction, CompiledFunctionTree};

pub fn snapshot_compiled_function_tree(label: &str, tree: &CompiledFunctionTree) -> String {
    let mut output = String::new();
    writeln!(output, "tree {label}").expect("string formatting must succeed");
    writeln!(output, "root: {:?}", tree.root_executable()).expect("string formatting must succeed");
    writeln!(output, "source: {:?}", tree.source_text()).expect("string formatting must succeed");
    writeln!(
        output,
        "function graph: root={:?} depth={} usage={:?}",
        tree.function_graph().root_id(),
        tree.function_graph().max_nesting_depth(),
        tree.function_graph().usage(),
    )
    .expect("string formatting must succeed");
    writeln!(
        output,
        "execution requirements: {:#?}",
        tree.verified_bytecode().requirements()
    )
    .expect("string formatting must succeed");

    for (index, function) in tree.functions().iter().enumerate() {
        snapshot_compiled_function(&mut output, tree, index, function);
    }

    while output.ends_with("\n\n") {
        output.pop();
    }
    output
}

fn snapshot_compiled_function(
    output: &mut String,
    tree: &CompiledFunctionTree,
    index: usize,
    function: &CompiledFunction,
) {
    let template = FunctionTemplateId::new(
        u32::try_from(index).expect("snapshot function count must fit the template domain"),
    );
    let verified = tree
        .verified_bytecode()
        .function(template)
        .expect("compiled function must have final metadata");
    let graph_function = tree
        .function_graph()
        .function(template)
        .expect("compiled function must have an intermediate graph certificate");

    writeln!(output, "function {index}").expect("string formatting must succeed");
    writeln!(output, "executable: {:?}", function.executable())
        .expect("string formatting must succeed");
    writeln!(
        output,
        "graph control flow matches: {}",
        graph_function.control_flow() == function.control_flow()
    )
    .expect("string formatting must succeed");
    writeln!(output, "graph atoms: {:#?}", graph_function.atoms())
        .expect("string formatting must succeed");
    writeln!(output, "graph constants: {:#?}", graph_function.constants())
        .expect("string formatting must succeed");
    writeln!(
        output,
        "graph closure sources: {:#?}",
        graph_function.closure_sources()
    )
    .expect("string formatting must succeed");
    writeln!(output, "locals: {:#?}", function.locals()).expect("string formatting must succeed");
    writeln!(output, "atoms: {:#?}", function.atoms()).expect("string formatting must succeed");
    writeln!(output, "constants: {:#?}", function.constants())
        .expect("string formatting must succeed");
    writeln!(
        output,
        "closure variables: {:#?}",
        function.closure_variables()
    )
    .expect("string formatting must succeed");
    writeln!(output, "realm globals: {:#?}", function.realm_globals())
        .expect("string formatting must succeed");
    writeln!(
        output,
        "source instructions: {:#?}",
        function.source_instructions()
    )
    .expect("string formatting must succeed");
    snapshot_control_flow(output, function);
    snapshot_metadata(output, verified);
    snapshot_disassembly(output, function);
}

fn snapshot_control_flow(output: &mut String, function: &CompiledFunction) {
    let control_flow = function.control_flow();
    writeln!(output, "bytecode: {:?}", control_flow.bytecode())
        .expect("string formatting must succeed");
    writeln!(output, "instructions: {:#?}", control_flow.instructions())
        .expect("string formatting must succeed");
    writeln!(
        output,
        "control-flow summary: max_stack={} transfers={} domains={:?} header={:?}",
        control_flow.computed_stack_size(),
        control_flow.transfer_evaluations(),
        control_flow.domains(),
        control_flow.function_header(),
    )
    .expect("string formatting must succeed");
    writeln!(
        output,
        "capture layout: {:#?}",
        control_flow.compiler_capture_layout()
    )
    .expect("string formatting must succeed");
    writeln!(
        output,
        "constant layout: {:#?}",
        control_flow.compiler_constant_layout()
    )
    .expect("string formatting must succeed");
}

fn snapshot_disassembly(output: &mut String, function: &CompiledFunction) {
    let mut disassembly = String::new();
    render_disassembly(
        InstructionDecoder::new(function.control_flow().bytecode()),
        &mut disassembly,
        DisassemblyLimits::new(10_000, 1_000_000),
    )
    .expect("verified compiler bytecode must disassemble within snapshot limits");
    writeln!(output, "disassembly:\n{disassembly}").expect("string formatting must succeed");
}

fn snapshot_metadata(output: &mut String, verified: VerifiedBytecodeFunction<'_>) {
    let metadata = verified.metadata();
    writeln!(output, "metadata kind: {:?}", metadata.executable_kind())
        .expect("string formatting must succeed");
    writeln!(
        output,
        "metadata function name: {:?}",
        metadata.function_name()
    )
    .expect("string formatting must succeed");
    writeln!(output, "metadata variables: {:#?}", metadata.variables())
        .expect("string formatting must succeed");
    writeln!(output, "metadata closures: {:#?}", metadata.closures())
        .expect("string formatting must succeed");
    writeln!(
        output,
        "metadata source: display={:?} function={:?} function_span={:?} name_span={:?} mappings={:#?}",
        metadata.source().display_name(),
        metadata.source().function_source(),
        metadata.source().function_span(),
        metadata.source().name_span(),
        metadata.source().mappings(),
    )
    .expect("string formatting must succeed");
}
