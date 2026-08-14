//! Final compiler-bytecode metadata verification and execution authority.
//!
//! The staged body and function-graph certificates deliberately omit runtime
//! binding and source metadata. This module closes that boundary for the
//! current ordinary-function compiler profile. Verification is pure, bounded,
//! iterative, and does not materialize runtime values or atoms.

use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt,
    sync::Arc,
};

use crate::{
    AtomPoolIndex, BytecodePc, CompilerClosureSource, FinalOpcode, FunctionKind,
    FunctionTemplateId, Operands, VerifiedCompilerFunction, VerifiedCompilerFunctionGraph,
    VerifiedControlFlow, VerifiedInstruction,
    verifier::{CompilerCaptureLayout, CompilerCapturedBinding, InstructionIndex},
};

mod codec;
mod lexical_environment;
mod object_provenance;

pub use codec::{
    BYTECODE_CODEC_MAGIC, BYTECODE_CODEC_STAMP, BytecodeCodecError, decode_atom_pool,
    decode_closure_sources, decode_constant_pool, decode_graph, encode_atom_pool,
    encode_closure_sources, encode_constant_pool, encode_graph, frame_sections, read_sections,
};

use lexical_environment::verify_lexical_arrow_environments;
use object_provenance::{charge_frame_state_entries, verify_object_definition_provenance};

include!("verified_bytecode/model.rs");
include!("verified_bytecode/error.rs");
include!("verified_bytecode/graph_verifier.rs");
include!("verified_bytecode/initializer_verifier.rs");
include!("verified_bytecode/module_verifier.rs");
include!("verified_bytecode/source_verifier.rs");
include!("verified_bytecode/class_field_verifier.rs");
include!("verified_bytecode/function_name_verifier.rs");
include!("verified_bytecode/opcode_profile.rs");
include!("verified_bytecode/internal_stack_model.rs");
include!("verified_bytecode/internal_stack_verifier.rs");
include!("verified_bytecode/internal_stack_transfer.rs");
include!("verified_bytecode/internal_stack_propagation.rs");
include!("verified_bytecode/binding_verifier.rs");
include!("verified_bytecode/execution_requirements.rs");
