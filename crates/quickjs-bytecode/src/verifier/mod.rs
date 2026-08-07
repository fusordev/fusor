/*
 * QuickJS bytecode control-flow verification
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Fail-closed verification of final bytecode control flow and ordinary
//! JavaScript-value stack depths.
//!
//! This module intentionally produces [`VerifiedControlFlow`], not an
//! execution-authorizing `VerifiedBytecode`. Atom namespaces, constant kinds,
//! nested functions, handlers, iterator markers, finally return addresses,
//! debug payloads, and source tables still require later verification.

mod error;
mod header;
mod layouts;
mod limits;
mod model;
mod opcode_semantics;
mod operands;
mod pipeline;
mod predecode;
mod stack;
mod static_control_flow;
mod targets;

pub use error::{
    ControlFlowEdge, FunctionCountDomain, InvalidControlFlowTargetReason, OperandIndexDomain,
    SecondaryOperandField, UnsupportedVerifierFeature, VerificationError, VerificationErrorKind,
};
pub use limits::{
    MAX_FUNCTION_INDEX_ENTRIES, MAX_OPERAND_STACK_DEPTH, VerificationLimits, VerificationResource,
};
pub use model::{
    CompilerCaptureLayout, CompilerCapturedBinding, CompilerConstantKind, CompilerConstantLayout,
    FunctionIndexDomains, FunctionPath, InstructionIndex, UnverifiedCompilerFunctionBody,
    UnverifiedFunctionBody, VerifiedControlFlow, VerifiedInstruction, VerifiedSuccessorKind,
    VerifiedSuccessors,
};
use model::{CompilerCapturedBindingIdentity, VerifiedSuccessorsRepr};
pub use pipeline::{verify_compiler_control_flow, verify_control_flow};

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
