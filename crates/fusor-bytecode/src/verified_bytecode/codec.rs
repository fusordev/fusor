//! Verified-bytecode snapshot codec foundation (§8.2: JS functions store
//! as verified bytecode in snapshots). This layer frames the
//! [`super::VerifiedBytecode`] serialization: magic, format stamp, and
//! length-prefixed sections with bounds-checked reading. The graph,
//! pool, metadata, and control-flow payload codecs land on top in their
//! own slices; every failure is a typed [`BytecodeCodecError`], never a
//! panic.
//!
//! Format (stamp 1):
//!
//! ```text
//! magic     8 bytes  "FUSRBYTE"
//! stamp     u32 LE   BYTECODE_CODEC_STAMP
//! sections  u32 LE   section count
//! per section: tag u8, payload byte length u64 LE, payload
//! ```
//!
//! The enclosing snapshot already checksums the section payload, so this
//! inner framing carries no checksum of its own. Sections appear in tag
//! order; tags are assigned as their payload codecs land (1 = graph,
//! 2 = metadata, 3 = control flow, 4 = module record, reserved).

use std::sync::Arc;
use std::{error::Error, fmt};

use super::ModuleImportNameKind;
use crate::compiler_graph::{
    UnverifiedCompilerFunction, UnverifiedCompilerFunctionGraph, VerifiedCompilerFunction,
    VerifiedCompilerFunctionGraph, verify_compiler_function_graph,
};
use crate::function::UnverifiedFunctionHeader;
use crate::verifier::{
    CompilerCaptureLayout, CompilerCapturedBinding, CompilerConstantKind, CompilerConstantLayout,
    FunctionIndexDomains, UnverifiedCompilerFunctionBody, VerificationLimits,
    verify_compiler_control_flow,
};
use crate::{
    AtomPoolIndex, Binary64Constant, CompilerAtom, CompilerBigInt, CompilerClosureSource,
    CompilerConstant, CompilerConstantValue, CompilerString, CompilerTemplateElement,
    CompilerTemplateObject, FunctionTemplateId,
};
use crate::{
    BytecodeGraphVerificationLimits, BytecodePc, ClosureVariableDefinition, CompilerBindingKind,
    CompilerBindingPolicy, CompilerClosureBinding, CompilerExecutableKind,
    CompilerInitializationPolicy, CompilerSource, CompilerWritePolicy, ModuleBindingOrigin,
    ModuleImportName, ModuleRequestDescriptor, PcSourceSpan, ScopeLink, SourceByteSpan,
    UnverifiedCompilerBytecodeGraph, UnverifiedFunctionMetadata, UnverifiedModuleBindingDescriptor,
    UnverifiedModuleDeclarationRecord, VariableDefinition, VerifiedBytecode,
    VerifiedFunctionMetadata, verify_compiler_bytecode_graph,
};

/// The bytecode codec magic.
pub const BYTECODE_CODEC_MAGIC: [u8; 8] = *b"FUSRBYTE";

/// The current bytecode codec format stamp (alpha: no version
/// compatibility, §8.1).
pub const BYTECODE_CODEC_STAMP: u32 = 1;

/// Bytecode codec failures (fail closed, no panics).
#[derive(Debug)]
pub enum BytecodeCodecError {
    /// The payload does not start with [`BYTECODE_CODEC_MAGIC`].
    MagicMismatch,
    /// The format stamp or a section tag is unknown.
    FormatMismatch {
        /// The stamp or tag that was rejected.
        found: u32,
    },
    /// The payload ends inside a frame or section.
    Truncated,
    /// A decoded pool value violated a canonical invariant (string length
    /// domain, allocation failure).
    String(String),
    /// The load-time re-verification (§8.3) rejected the decoded payload.
    Verification(String),
}

impl fmt::Display for BytecodeCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MagicMismatch => formatter.write_str("not a bytecode payload (bad magic)"),
            Self::FormatMismatch { found } => {
                write!(
                    formatter,
                    "unsupported bytecode format stamp or section tag {found}"
                )
            }
            Self::Truncated => formatter.write_str("the bytecode payload is truncated"),
            Self::String(message) => write!(formatter, "invalid bytecode pool value: {message}"),
            Self::Verification(message) => {
                write!(formatter, "bytecode re-verification failed: {message}")
            }
        }
    }
}

impl Error for BytecodeCodecError {}

/// Bounds-checked reader over one bytecode payload.
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, BytecodeCodecError> {
        Ok(*self
            .read_bytes(1)?
            .first()
            .ok_or(BytecodeCodecError::Truncated)?)
    }

    fn read_u32(&mut self) -> Result<u32, BytecodeCodecError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, BytecodeCodecError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], BytecodeCodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(BytecodeCodecError::Truncated)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(BytecodeCodecError::Truncated)?;
        self.position = end;
        Ok(slice)
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
}

/// Appends one section frame: tag, little-endian payload byte length,
/// payload.
fn write_section(buffer: &mut Vec<u8>, tag: u8, payload: &[u8]) {
    buffer.push(tag);
    buffer.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    buffer.extend_from_slice(payload);
}

/// Frames a list of sections into one payload, validating the
/// well-formedness invariant the reader relies on (sections in tag
/// order, one frame per tag).
///
/// # Errors
///
/// Returns [`BytecodeCodecError::FormatMismatch`] for duplicate or
/// out-of-order tags.
pub fn frame_sections(sections: &[(u8, Vec<u8>)]) -> Result<Vec<u8>, BytecodeCodecError> {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&BYTECODE_CODEC_MAGIC);
    buffer.extend_from_slice(&BYTECODE_CODEC_STAMP.to_le_bytes());
    buffer.extend_from_slice(&(sections.len() as u32).to_le_bytes());
    let mut previous_tag = 0u8;
    for (tag, payload) in sections {
        if *tag <= previous_tag {
            return Err(BytecodeCodecError::FormatMismatch {
                found: u32::from(*tag),
            });
        }
        previous_tag = *tag;
        write_section(&mut buffer, *tag, payload);
    }
    Ok(buffer)
}

/// Reads a framed payload's sections, returning `(tag, payload)` pairs in
/// order.
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for a magic/stamp mismatch,
/// duplicate or out-of-order tags, truncation, or trailing bytes.
pub fn read_sections(payload: &[u8]) -> Result<Vec<(u8, &[u8])>, BytecodeCodecError> {
    let mut reader = Reader::new(payload);
    if reader.read_bytes(BYTECODE_CODEC_MAGIC.len())? != BYTECODE_CODEC_MAGIC {
        return Err(BytecodeCodecError::MagicMismatch);
    }
    let stamp = reader.read_u32()?;
    if stamp != BYTECODE_CODEC_STAMP {
        return Err(BytecodeCodecError::FormatMismatch { found: stamp });
    }
    let count = reader.read_u32()?;
    let mut sections = Vec::new();
    let mut previous_tag = 0u8;
    for _ in 0..count {
        let tag = reader.read_u8()?;
        if tag <= previous_tag {
            return Err(BytecodeCodecError::FormatMismatch {
                found: u32::from(tag),
            });
        }
        previous_tag = tag;
        let length = reader.read_u64()?;
        let length = usize::try_from(length).map_err(|_| BytecodeCodecError::Truncated)?;
        sections.push((tag, reader.read_bytes(length)?));
    }
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(sections)
}

// ---- Pool payload codecs (the graph section's sub-payloads) ----

fn write_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(buffer: &mut Vec<u8>, value: u64) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

/// Reads one length-prefixed collection count.
fn read_count(reader: &mut Reader<'_>) -> Result<usize, BytecodeCodecError> {
    let count = reader.read_u32()?;
    usize::try_from(count).map_err(|_| BytecodeCodecError::Truncated)
}

/// Encodes one compiler string as exact UTF-16 code units (length-prefixed
/// little-endian pairs). Decoding re-canonicalizes through
/// [`CompilerString::try_from_code_units`], whose equality is logical, so
/// Latin-1 and wide storage round-trip regardless of the original width.
fn encode_string(buffer: &mut Vec<u8>, string: &CompilerString) {
    let units: Vec<u16> = string.code_units().collect();
    write_u32(buffer, units.len() as u32);
    for unit in units {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
}

/// Decodes one compiler string from its exact UTF-16 units.
fn decode_string(reader: &mut Reader<'_>) -> Result<CompilerString, BytecodeCodecError> {
    let length = reader.read_u32()?;
    let length = usize::try_from(length).map_err(|_| BytecodeCodecError::Truncated)?;
    let bytes = reader.read_bytes(length.checked_mul(2).ok_or(BytecodeCodecError::Truncated)?)?;
    let mut units = Vec::new();
    units
        .try_reserve_exact(length)
        .map_err(|_| BytecodeCodecError::String("string allocation failed".to_owned()))?;
    for pair in bytes.chunks_exact(2) {
        units.push(u16::from_le_bytes([pair[0], pair[1]]));
    }
    CompilerString::try_from_code_units(Arc::from(units))
        .map_err(|error| BytecodeCodecError::String(error.to_string()))
}

/// Encodes one compiler atom pool: `count`, then per atom a
/// static-property-only flag byte and the exact string units.
#[must_use]
pub fn encode_atom_pool(atoms: &[CompilerAtom]) -> Vec<u8> {
    let mut buffer = Vec::new();
    write_u32(&mut buffer, atoms.len() as u32);
    for atom in atoms {
        buffer.push(u8::from(atom.is_static_property_only()));
        encode_string(&mut buffer, atom.string());
    }
    buffer
}

/// Decodes one compiler atom pool payload.
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for truncation, an unknown flag,
/// a canonical string violation, or trailing bytes.
pub fn decode_atom_pool(payload: &[u8]) -> Result<Vec<CompilerAtom>, BytecodeCodecError> {
    let mut reader = Reader::new(payload);
    let atoms = decode_atom_pool_from(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(atoms)
}

/// Reads one atom pool from the reader's current position (the graph
/// record sub-payload form: no trailing-byte check).
fn decode_atom_pool_from(reader: &mut Reader<'_>) -> Result<Vec<CompilerAtom>, BytecodeCodecError> {
    let count = read_count(reader)?;
    let mut atoms = Vec::new();
    atoms
        .try_reserve_exact(count)
        .map_err(|_| BytecodeCodecError::String("atom allocation failed".to_owned()))?;
    for _ in 0..count {
        let flag = reader.read_u8()?;
        let string = decode_string(reader)?;
        let atom = match flag {
            0 => CompilerAtom::new(string),
            1 => CompilerAtom::new_static_property_only(string),
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        atoms.push(atom);
    }
    Ok(atoms)
}

/// Encodes one heterogeneous constant pool: `count`, then per constant a
/// kind byte (`1` = nested function, `0` = value), then the payload —
/// numbers as exact binary64 bits, strings and `BigInt` decimals as UTF-16
/// units, template objects as cooked/raw element pairs.
#[must_use]
pub fn encode_constant_pool(constants: &[CompilerConstant]) -> Vec<u8> {
    let mut buffer = Vec::new();
    write_u32(&mut buffer, constants.len() as u32);
    for constant in constants {
        match constant {
            CompilerConstant::Function(id) => {
                buffer.push(1);
                write_u32(&mut buffer, id.get());
            }
            CompilerConstant::Value(value) => {
                buffer.push(0);
                match value {
                    CompilerConstantValue::Number(number) => {
                        buffer.push(0);
                        write_u64(&mut buffer, number.to_bits());
                    }
                    CompilerConstantValue::String(string) => {
                        buffer.push(1);
                        encode_string(&mut buffer, string);
                    }
                    CompilerConstantValue::BigInt(bigint) => {
                        buffer.push(2);
                        encode_string(&mut buffer, bigint.decimal());
                    }
                    CompilerConstantValue::TemplateObject(template) => {
                        buffer.push(3);
                        let elements = template.elements();
                        write_u32(&mut buffer, elements.len() as u32);
                        for element in elements {
                            match element.cooked() {
                                Some(cooked) => {
                                    buffer.push(1);
                                    encode_string(&mut buffer, cooked);
                                }
                                None => buffer.push(0),
                            }
                            encode_string(&mut buffer, element.raw());
                        }
                    }
                }
            }
        }
    }
    buffer
}

/// Decodes one heterogeneous constant pool payload.
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for truncation, an unknown kind
/// or value tag, a canonical string/template violation, or trailing bytes.
pub fn decode_constant_pool(payload: &[u8]) -> Result<Vec<CompilerConstant>, BytecodeCodecError> {
    let mut reader = Reader::new(payload);
    let constants = decode_constant_pool_from(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(constants)
}

/// Reads one constant pool from the reader's current position (the graph
/// record sub-payload form: no trailing-byte check).
fn decode_constant_pool_from(
    reader: &mut Reader<'_>,
) -> Result<Vec<CompilerConstant>, BytecodeCodecError> {
    let count = read_count(reader)?;
    let mut constants = Vec::new();
    constants
        .try_reserve_exact(count)
        .map_err(|_| BytecodeCodecError::String("constant allocation failed".to_owned()))?;
    for _ in 0..count {
        let kind = reader.read_u8()?;
        let constant = match kind {
            1 => CompilerConstant::Function(FunctionTemplateId::new(reader.read_u32()?)),
            0 => {
                let tag = reader.read_u8()?;
                let value = match tag {
                    0 => CompilerConstantValue::Number(Binary64Constant::from_bits(
                        reader.read_u64()?,
                    )),
                    1 => CompilerConstantValue::String(decode_string(reader)?),
                    2 => CompilerConstantValue::BigInt(
                        CompilerBigInt::try_from_decimal(decode_string(reader)?)
                            .map_err(|error| BytecodeCodecError::String(error.to_string()))?,
                    ),
                    3 => {
                        let element_count = read_count(reader)?;
                        let mut elements = Vec::new();
                        elements.try_reserve_exact(element_count).map_err(|_| {
                            BytecodeCodecError::String("template allocation failed".to_owned())
                        })?;
                        for _ in 0..element_count {
                            let cooked = match reader.read_u8()? {
                                0 => None,
                                1 => Some(decode_string(reader)?),
                                other => {
                                    return Err(BytecodeCodecError::FormatMismatch {
                                        found: u32::from(other),
                                    });
                                }
                            };
                            elements
                                .push(CompilerTemplateElement::new(cooked, decode_string(reader)?));
                        }
                        CompilerConstantValue::TemplateObject(
                            CompilerTemplateObject::try_from_elements(Arc::from(elements))
                                .map_err(|error| BytecodeCodecError::String(error.to_string()))?,
                        )
                    }
                    other => {
                        return Err(BytecodeCodecError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                };
                CompilerConstant::Value(value)
            }
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        constants.push(constant);
    }
    Ok(constants)
}

/// Encodes one closure-source pool: `count`, then per entry a variant tag
/// and its dense payload.
#[must_use]
pub fn encode_closure_sources(sources: &[CompilerClosureSource]) -> Vec<u8> {
    let mut buffer = Vec::new();
    write_u32(&mut buffer, sources.len() as u32);
    for source in sources {
        match source {
            CompilerClosureSource::ParentVariableReference(index) => {
                buffer.push(0);
                write_u32(&mut buffer, *index);
            }
            CompilerClosureSource::ParentClosure(index) => {
                buffer.push(1);
                write_u32(&mut buffer, *index);
            }
            CompilerClosureSource::ConstructorRealmGlobal(atom) => {
                buffer.push(2);
                write_u32(&mut buffer, atom.get());
            }
            CompilerClosureSource::DirectEvalBinding {
                index,
                environment_size,
            } => {
                buffer.push(3);
                write_u32(&mut buffer, *index);
                write_u32(&mut buffer, *environment_size);
            }
            CompilerClosureSource::DirectEvalVariable {
                index,
                environment_size,
            } => {
                buffer.push(4);
                write_u32(&mut buffer, *index);
                write_u32(&mut buffer, *environment_size);
            }
            CompilerClosureSource::Module { index } => {
                buffer.push(5);
                write_u32(&mut buffer, *index);
            }
        }
    }
    buffer
}

/// Decodes one closure-source pool payload.
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for truncation, an unknown
/// variant tag, or trailing bytes.
pub fn decode_closure_sources(
    payload: &[u8],
) -> Result<Vec<CompilerClosureSource>, BytecodeCodecError> {
    let mut reader = Reader::new(payload);
    let sources = decode_closure_sources_from(&mut reader)?;
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(sources)
}

/// Reads one closure-source pool from the reader's current position (the
/// graph record sub-payload form: no trailing-byte check).
fn decode_closure_sources_from(
    reader: &mut Reader<'_>,
) -> Result<Vec<CompilerClosureSource>, BytecodeCodecError> {
    let count = read_count(reader)?;
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(count)
        .map_err(|_| BytecodeCodecError::String("closure source allocation failed".to_owned()))?;
    for _ in 0..count {
        let tag = reader.read_u8()?;
        let source = match tag {
            0 => CompilerClosureSource::ParentVariableReference(reader.read_u32()?),
            1 => CompilerClosureSource::ParentClosure(reader.read_u32()?),
            2 => CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(
                reader.read_u32()?,
            )),
            3 => CompilerClosureSource::DirectEvalBinding {
                index: reader.read_u32()?,
                environment_size: reader.read_u32()?,
            },
            4 => CompilerClosureSource::DirectEvalVariable {
                index: reader.read_u32()?,
                environment_size: reader.read_u32()?,
            },
            5 => CompilerClosureSource::Module {
                index: reader.read_u32()?,
            },
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        sources.push(source);
    }
    Ok(sources)
}

// ---- Graph section codec (section 1) ----

/// Encodes one verified compiler graph as its graph-section payload: the
/// root identity and one record per function — raw bytecode, the
/// serialized stack size, index domains, serialized header bits, the
/// capture/constant layouts, the three pools, and the scalar markers.
/// Derived aggregates (usage, nesting depth) are recomputed by the
/// verifier on decode.
#[must_use]
pub fn encode_graph(graph: &VerifiedCompilerFunctionGraph) -> Vec<u8> {
    let mut buffer = Vec::new();
    write_u32(&mut buffer, graph.functions().len() as u32);
    for function in graph.functions() {
        encode_function_record(&mut buffer, function);
    }
    write_u32(&mut buffer, graph.root_id().get());
    buffer
}

/// Encodes one function record.
fn encode_function_record(buffer: &mut Vec<u8>, function: &VerifiedCompilerFunction) {
    let flow = function.control_flow();
    let bytecode = flow.bytecode();
    write_u32(buffer, bytecode.len() as u32);
    buffer.extend_from_slice(bytecode);
    write_u32(buffer, flow.computed_stack_size());
    let domains = flow.domains();
    write_u32(buffer, domains.atom_pool_len());
    write_u32(buffer, domains.constant_pool_len());
    write_u32(buffer, domains.argument_count());
    write_u32(buffer, domains.local_count());
    write_u32(buffer, domains.closure_var_count());
    let header = flow.function_header();
    buffer.extend_from_slice(&header.flags().bits().to_le_bytes());
    buffer.push(header.mode().bits());
    write_u32(buffer, header.defined_argument_count());
    write_u32(buffer, header.variable_reference_count());
    match flow.compiler_capture_layout() {
        Some(layout) => {
            buffer.push(1);
            let bindings = layout.bindings();
            write_u32(buffer, bindings.len() as u32);
            for binding in bindings {
                match binding {
                    CompilerCapturedBinding::Argument(index) => {
                        buffer.push(0);
                        write_u32(buffer, *index);
                    }
                    CompilerCapturedBinding::FunctionLocal(index) => {
                        buffer.push(1);
                        write_u32(buffer, *index);
                    }
                    CompilerCapturedBinding::ScopedLocal(index) => {
                        buffer.push(2);
                        write_u32(buffer, *index);
                    }
                }
            }
            match layout.mapped_arguments() {
                Some(mapped) => {
                    buffer.push(1);
                    write_u32(buffer, mapped.len() as u32);
                    for index in mapped {
                        write_u32(buffer, *index);
                    }
                }
                None => buffer.push(0),
            }
        }
        None => buffer.push(0),
    }
    match flow.compiler_constant_layout() {
        Some(layout) => {
            buffer.push(1);
            let kinds = layout.kinds();
            write_u32(buffer, kinds.len() as u32);
            for kind in kinds {
                buffer.push(match kind {
                    CompilerConstantKind::Value => 0,
                    CompilerConstantKind::Function => 1,
                });
            }
        }
        None => buffer.push(0),
    }
    buffer.extend_from_slice(&encode_atom_pool(function.atoms()));
    buffer.extend_from_slice(&encode_constant_pool(function.constants()));
    buffer.extend_from_slice(&encode_closure_sources(function.closure_sources()));
    buffer.push(u8::from(function.has_direct_eval()));
    match function.parameter_initialization_end() {
        Some(boundary) => {
            buffer.push(1);
            write_u32(buffer, boundary);
        }
        None => buffer.push(0),
    }
    write_u32(buffer, function.function_initializer_prefix_start());
    let eval_references = function.eval_reference_call_instructions();
    write_u32(buffer, eval_references.len() as u32);
    for index in eval_references {
        write_u32(buffer, *index);
    }
}

/// Decodes one graph-section payload and rebuilds the verified graph by
/// re-running the body verifier per function and the whole-graph verifier
/// ("load-time verification", §8.3).
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for truncation, unknown tags,
/// canonical pool violations, a serialized-stack-size mismatch, or a
/// body/whole-graph re-verification failure.
pub fn decode_graph(payload: &[u8]) -> Result<VerifiedCompilerFunctionGraph, BytecodeCodecError> {
    let mut reader = Reader::new(payload);
    let count = read_count(&mut reader)?;
    let mut functions = Vec::new();
    functions
        .try_reserve_exact(count)
        .map_err(|_| BytecodeCodecError::String("function allocation failed".to_owned()))?;
    for _ in 0..count {
        functions.push(decode_function_record(&mut reader)?);
    }
    let root = FunctionTemplateId::new(reader.read_u32()?);
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    let graph = UnverifiedCompilerFunctionGraph::new(root, Arc::from(functions));
    verify_compiler_function_graph(graph, crate::FunctionGraphVerificationLimits::default())
        .map_err(|error| BytecodeCodecError::Verification(error.to_string()))
}

/// Decodes one function record and re-verifies its control flow with the
/// serialized stack-size check (§8.3 fail closed).
fn decode_function_record(
    reader: &mut Reader<'_>,
) -> Result<UnverifiedCompilerFunction, BytecodeCodecError> {
    let bytecode_length = read_count(reader)?;
    let bytecode = reader.read_bytes(bytecode_length)?.to_vec();
    let expected_stack_size = reader.read_u32()?;
    let domains = FunctionIndexDomains::new(
        reader.read_u32()?,
        reader.read_u32()?,
        reader.read_u32()?,
        reader.read_u32()?,
        reader.read_u32()?,
    );
    let serialized_flags = u16::from_le_bytes([reader.read_u8()?, reader.read_u8()?]);
    let js_mode = reader.read_u8()?;
    let header = UnverifiedFunctionHeader::new(
        serialized_flags,
        js_mode,
        reader.read_u32()?,
        reader.read_u32()?,
    );
    let capture_layout = match reader.read_u8()? {
        0 => None,
        1 => {
            let binding_count = read_count(reader)?;
            let mut bindings = Vec::new();
            bindings
                .try_reserve_exact(binding_count)
                .map_err(|_| BytecodeCodecError::String("capture allocation failed".to_owned()))?;
            for _ in 0..binding_count {
                bindings.push(match reader.read_u8()? {
                    0 => CompilerCapturedBinding::Argument(reader.read_u32()?),
                    1 => CompilerCapturedBinding::FunctionLocal(reader.read_u32()?),
                    2 => CompilerCapturedBinding::ScopedLocal(reader.read_u32()?),
                    other => {
                        return Err(BytecodeCodecError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                });
            }
            let mut layout = CompilerCaptureLayout::new(Arc::from(bindings));
            if reader.read_u8()? == 1 {
                let mapped_count = read_count(reader)?;
                let mut mapped = Vec::new();
                mapped.try_reserve_exact(mapped_count).map_err(|_| {
                    BytecodeCodecError::String("mapped arguments allocation failed".to_owned())
                })?;
                for _ in 0..mapped_count {
                    mapped.push(reader.read_u32()?);
                }
                layout = layout.with_mapped_arguments(Arc::from(mapped));
            }
            Some(layout)
        }
        other => {
            return Err(BytecodeCodecError::FormatMismatch {
                found: u32::from(other),
            });
        }
    };
    let constant_layout = match reader.read_u8()? {
        0 => None,
        1 => {
            let kind_count = read_count(reader)?;
            let mut kinds = Vec::new();
            kinds.try_reserve_exact(kind_count).map_err(|_| {
                BytecodeCodecError::String("constant layout allocation failed".to_owned())
            })?;
            for _ in 0..kind_count {
                kinds.push(match reader.read_u8()? {
                    0 => CompilerConstantKind::Value,
                    1 => CompilerConstantKind::Function,
                    other => {
                        return Err(BytecodeCodecError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                });
            }
            Some(CompilerConstantLayout::new(Arc::from(kinds)))
        }
        other => {
            return Err(BytecodeCodecError::FormatMismatch {
                found: u32::from(other),
            });
        }
    };
    let atoms = decode_atom_pool_from(reader)?;
    let constants = decode_constant_pool_from(reader)?;
    let closure_sources = decode_closure_sources_from(reader)?;
    let has_direct_eval = reader.read_u8()? != 0;
    let parameter_initialization_end = match reader.read_u8()? {
        0 => None,
        1 => Some(reader.read_u32()?),
        other => {
            return Err(BytecodeCodecError::FormatMismatch {
                found: u32::from(other),
            });
        }
    };
    let function_initializer_prefix_start = reader.read_u32()?;
    let eval_reference_count = read_count(reader)?;
    let mut eval_references = Vec::new();
    eval_references
        .try_reserve_exact(eval_reference_count)
        .map_err(|_| BytecodeCodecError::String("eval reference allocation failed".to_owned()))?;
    for _ in 0..eval_reference_count {
        eval_references.push(reader.read_u32()?);
    }

    let mut body = UnverifiedCompilerFunctionBody::new(bytecode, domains, header);
    if let Some(layout) = capture_layout {
        body = body.with_capture_layout(layout);
    }
    if let Some(layout) = constant_layout {
        body = body.with_constant_layout(layout);
    }
    let flow = verify_compiler_control_flow(body, VerificationLimits::default())
        .map_err(|error| BytecodeCodecError::Verification(error.to_string()))?;
    if flow.computed_stack_size() != expected_stack_size {
        return Err(BytecodeCodecError::Verification(format!(
            "serialized stack size mismatch: expected {expected_stack_size}, computed {}",
            flow.computed_stack_size()
        )));
    }

    Ok(UnverifiedCompilerFunction::new(
        Arc::new(flow),
        Arc::from(constants),
        Arc::from(closure_sources),
    )
    .with_atom_pool(Arc::from(atoms))
    .with_direct_eval(has_direct_eval)
    .with_parameter_initialization_end(parameter_initialization_end)
    .with_function_initializer_prefix_start(function_initializer_prefix_start)
    .with_eval_reference_call_instructions(Arc::from(eval_references)))
}

// ---- Metadata section codec (section 2) ----

/// Encodes the verified per-function metadata as the metadata-section
/// payload: executable kinds, names, variable definitions, closure
/// descriptors, and source records (exact text, spans, and PC mappings).
#[must_use]
pub fn encode_metadata(metadata: &[VerifiedFunctionMetadata]) -> Vec<u8> {
    let mut buffer = Vec::new();
    write_u32(&mut buffer, metadata.len() as u32);
    for record in metadata {
        encode_metadata_record(&mut buffer, record);
    }
    buffer
}

/// Encodes one metadata record.
fn encode_metadata_record(buffer: &mut Vec<u8>, record: &VerifiedFunctionMetadata) {
    buffer.push(executable_kind_tag(record.executable_kind()));
    write_option_atom(buffer, record.function_name());
    let variables = record.variables();
    write_u32(buffer, variables.len() as u32);
    for variable in variables {
        write_option_atom(buffer, variable.name());
        match variable.scope_next() {
            ScopeLink::End => buffer.push(0),
            ScopeLink::ArgumentScopeEnd => buffer.push(1),
            ScopeLink::Local(index) => {
                buffer.push(2);
                write_u32(buffer, index);
            }
        }
        write_policy(buffer, variable.policy());
        buffer.push(u8::from(variable.has_scope()));
        buffer.push(u8::from(variable.is_arguments_object()));
        write_option_u32(buffer, variable.variable_reference());
        write_option_u32(buffer, variable.function_initializer());
    }
    let closures = record.closures();
    write_u32(buffer, closures.len() as u32);
    for closure in closures {
        write_option_atom(buffer, closure.name());
        buffer.push(match closure.binding() {
            CompilerClosureBinding::Captured(_) => 0,
            CompilerClosureBinding::RealmGlobal(_) => 1,
        });
        write_policy(buffer, closure.binding().policy());
        let source = closure.source();
        buffer.push(closure_source_tag(source));
        write_closure_source_payload(buffer, source);
        buffer.push(u8::from(closure.is_arguments_object()));
        buffer.push(u8::from(closure.is_deletable_eval_variable()));
        write_option_u32(buffer, closure.function_initializer());
    }
    let source = record.source();
    write_utf8(buffer, source.display_name_arc().as_ref());
    write_utf8(buffer, source.text_arc().as_ref());
    write_span(buffer, source.function_span());
    match source.name_span() {
        Some(span) => {
            buffer.push(1);
            write_span(buffer, span);
        }
        None => buffer.push(0),
    }
    let mappings = source.mappings();
    write_u32(buffer, mappings.len() as u32);
    for mapping in mappings {
        write_u32(buffer, mapping.pc().get());
        write_span(buffer, mapping.span());
    }
    match source.strict_mode_pcs.as_ref() {
        Some(pcs) => {
            buffer.push(1);
            write_u32(buffer, pcs.len() as u32);
            for pc in pcs.as_ref() {
                write_u32(buffer, pc.get());
            }
        }
        None => buffer.push(0),
    }
}

/// Decodes one metadata-section payload.
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for truncation, unknown tags, or
/// trailing bytes.
pub fn decode_metadata(
    payload: &[u8],
) -> Result<Arc<[UnverifiedFunctionMetadata]>, BytecodeCodecError> {
    let mut reader = Reader::new(payload);
    let count = read_count(&mut reader)?;
    let mut metadata = Vec::new();
    metadata
        .try_reserve_exact(count)
        .map_err(|_| BytecodeCodecError::String("metadata allocation failed".to_owned()))?;
    // The compiler shares content-equal source texts across functions;
    // decode re-interns them so the verified result (and its charged
    // usage) matches byte-for-byte.
    let mut interned_texts = std::collections::HashMap::<String, Arc<str>>::new();
    let mut interned_names = std::collections::HashMap::<String, Arc<str>>::new();
    for _ in 0..count {
        let kind = executable_kind_from_tag(reader.read_u8()?)?;
        let function_name = read_option_atom(&mut reader)?;
        let variable_count = read_count(&mut reader)?;
        let mut variables = Vec::new();
        variables
            .try_reserve_exact(variable_count)
            .map_err(|_| BytecodeCodecError::String("variable allocation failed".to_owned()))?;
        for _ in 0..variable_count {
            let name = read_option_atom(&mut reader)?;
            let scope_next = match reader.read_u8()? {
                0 => ScopeLink::End,
                1 => ScopeLink::ArgumentScopeEnd,
                2 => ScopeLink::Local(reader.read_u32()?),
                other => {
                    return Err(BytecodeCodecError::FormatMismatch {
                        found: u32::from(other),
                    });
                }
            };
            let policy = read_policy(&mut reader)?;
            let has_scope = reader.read_u8()? != 0;
            let arguments_object = reader.read_u8()? != 0;
            let variable_reference = read_option_u32(&mut reader)?;
            let function_initializer = read_option_u32(&mut reader)?;
            let mut variable =
                VariableDefinition::new(name, scope_next, policy, has_scope, variable_reference)
                    .with_arguments_object(arguments_object);
            if let Some(constant) = function_initializer {
                variable = variable.with_function_initializer(constant);
            }
            variables.push(variable);
        }
        let closure_count = read_count(&mut reader)?;
        let mut closures = Vec::new();
        closures
            .try_reserve_exact(closure_count)
            .map_err(|_| BytecodeCodecError::String("closure allocation failed".to_owned()))?;
        for _ in 0..closure_count {
            let name = read_option_atom(&mut reader)?;
            let binding_tag = reader.read_u8()?;
            let policy = read_policy(&mut reader)?;
            let source = read_closure_source(&mut reader)?;
            let arguments_object = reader.read_u8()? != 0;
            let deletable = reader.read_u8()? != 0;
            let function_initializer = read_option_u32(&mut reader)?;
            let mut closure = match binding_tag {
                0 => ClosureVariableDefinition::new(name, policy, source),
                1 => ClosureVariableDefinition::realm_global(name, policy, source),
                other => {
                    return Err(BytecodeCodecError::FormatMismatch {
                        found: u32::from(other),
                    });
                }
            };
            closure = closure
                .with_arguments_object(arguments_object)
                .with_deletable_eval_variable(deletable);
            if let Some(constant) = function_initializer {
                closure = closure.with_function_initializer(constant);
            }
            closures.push(closure);
        }
        let display_name = read_utf8(&mut reader)?;
        let display_name = interned_names
            .entry(display_name.clone())
            .or_insert_with(|| Arc::from(display_name.as_str()))
            .clone();
        let text = read_utf8(&mut reader)?;
        let text = interned_texts
            .entry(text.clone())
            .or_insert_with(|| Arc::from(text.as_str()))
            .clone();
        let function_span = read_span(&mut reader)?;
        let name_span = match reader.read_u8()? {
            0 => None,
            1 => Some(read_span(&mut reader)?),
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let mapping_count = read_count(&mut reader)?;
        let mut mappings = Vec::new();
        mappings
            .try_reserve_exact(mapping_count)
            .map_err(|_| BytecodeCodecError::String("mapping allocation failed".to_owned()))?;
        for _ in 0..mapping_count {
            let pc = BytecodePc::new(reader.read_u32()?);
            mappings.push(PcSourceSpan::new(pc, read_span(&mut reader)?));
        }
        let strict_mode_pcs = match reader.read_u8()? {
            0 => None,
            1 => {
                let pc_count = read_count(&mut reader)?;
                let mut pcs = Vec::new();
                pcs.try_reserve_exact(pc_count).map_err(|_| {
                    BytecodeCodecError::String("strict pc allocation failed".to_owned())
                })?;
                for _ in 0..pc_count {
                    pcs.push(BytecodePc::new(reader.read_u32()?));
                }
                Some(Arc::from(pcs))
            }
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let source = CompilerSource::new(
            display_name,
            text,
            function_span,
            name_span,
            Arc::from(mappings),
        );
        let source = match strict_mode_pcs {
            Some(pcs) => source.with_strict_mode_pcs(pcs),
            None => source,
        };
        metadata.push(
            UnverifiedFunctionMetadata::new(
                function_name,
                Arc::from(variables),
                Arc::from(closures),
                source,
            )
            .with_executable_kind(kind),
        );
    }
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(Arc::from(metadata))
}

// ---- Full verified-bytecode payload (§8.2) ----

/// Encodes one complete verified bytecode authority: the graph section
/// (tag 1) and the metadata section (tag 2) under the FUSRBYTE framing.
///
/// # Errors
///
/// Returns [`BytecodeCodecError::UnsupportedModule`] for a bytecode that
/// carries a module declaration record (the module section lands with the
/// module slice).
pub fn encode_verified_bytecode(
    bytecode: &VerifiedBytecode,
) -> Result<Vec<u8>, BytecodeCodecError> {
    let mut sections = vec![
        (1, encode_graph(bytecode.compiler_graph())),
        (2, encode_metadata(bytecode.metadata())),
    ];
    if let Some(module) = bytecode.module() {
        sections.push((4, encode_module(module)));
    }
    frame_sections(&sections)
}

/// Decodes one complete verified-bytecode payload, re-verifying the graph
/// and metadata ("load-time verification", §8.3).
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for framing damage, canonical
/// pool violations, an unsupported module section, or a re-verification
/// failure.
pub fn decode_verified_bytecode(payload: &[u8]) -> Result<VerifiedBytecode, BytecodeCodecError> {
    let sections = read_sections(payload)?;
    let mut graph = None;
    let mut metadata = None;
    let mut module = None;
    for (tag, section_payload) in sections {
        match tag {
            1 => {
                graph = Some(decode_graph(section_payload)?);
            }
            2 => {
                metadata = Some(decode_metadata(section_payload)?);
            }
            4 => {
                module = Some(Arc::new(decode_module(section_payload)?));
            }
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        }
    }
    let graph = graph.ok_or(BytecodeCodecError::Truncated)?;
    let metadata = metadata.ok_or(BytecodeCodecError::Truncated)?;
    let input = UnverifiedCompilerBytecodeGraph::new(Arc::new(graph), metadata);
    let input = match module {
        Some(module) => input.with_module(module),
        None => input,
    };
    verify_compiler_bytecode_graph(input, BytecodeGraphVerificationLimits::default())
        .map_err(|error| BytecodeCodecError::Verification(error.to_string()))
}

// ---- Module section codec (section 4) ----

/// Encodes one verified module declaration record: binding descriptors
/// (name, slot, policy, origin, initializer, import name) and static
/// request descriptors.
#[must_use]
pub fn encode_module(module: &crate::ModuleDeclarationRecord) -> Vec<u8> {
    let mut buffer = Vec::new();
    let bindings = module.bindings();
    write_u32(&mut buffer, bindings.len() as u32);
    for binding in bindings {
        write_u32(&mut buffer, binding.name().get());
        write_u32(&mut buffer, binding.slot());
        write_policy(&mut buffer, binding.policy());
        buffer.push(match binding.origin() {
            ModuleBindingOrigin::Local => 0,
            ModuleBindingOrigin::Import => 1,
            ModuleBindingOrigin::Namespace => 2,
        });
        write_option_u32(&mut buffer, binding.initializer());
        match binding.import() {
            Some(import) => {
                buffer.push(1);
                write_u32(&mut buffer, import.request());
                match import.kind {
                    ModuleImportNameKind::Named(name) => {
                        buffer.push(0);
                        write_u32(&mut buffer, name.get());
                    }
                    ModuleImportNameKind::Default => buffer.push(1),
                    ModuleImportNameKind::Namespace => buffer.push(2),
                    ModuleImportNameKind::DeferredNamespace => buffer.push(3),
                }
            }
            None => buffer.push(0),
        }
    }
    let requests = module.requests();
    write_u32(&mut buffer, requests.len() as u32);
    for request in requests.iter().cloned() {
        let specifier = request.clone().specifier();
        let has_assertions = request.has_assertions();
        write_u32(&mut buffer, specifier.get());
        buffer.push(u8::from(has_assertions));
    }
    buffer
}

/// Decodes one module-section payload.
///
/// # Errors
///
/// Returns a typed [`BytecodeCodecError`] for truncation, unknown tags,
/// or trailing bytes.
pub fn decode_module(
    payload: &[u8],
) -> Result<UnverifiedModuleDeclarationRecord, BytecodeCodecError> {
    let mut reader = Reader::new(payload);
    let binding_count = read_count(&mut reader)?;
    let mut bindings = Vec::new();
    bindings
        .try_reserve_exact(binding_count)
        .map_err(|_| BytecodeCodecError::String("module binding allocation failed".to_owned()))?;
    for _ in 0..binding_count {
        let name = AtomPoolIndex::new(reader.read_u32()?);
        let slot = reader.read_u32()?;
        let policy = read_policy(&mut reader)?;
        let origin = match reader.read_u8()? {
            0 => ModuleBindingOrigin::Local,
            1 => ModuleBindingOrigin::Import,
            2 => ModuleBindingOrigin::Namespace,
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let initializer = read_option_u32(&mut reader)?;
        let import = match reader.read_u8()? {
            0 => None,
            1 => {
                let request = reader.read_u32()?;
                Some(match reader.read_u8()? {
                    0 => ModuleImportName::named(request, AtomPoolIndex::new(reader.read_u32()?)),
                    1 => ModuleImportName::default(request),
                    2 => ModuleImportName::namespace(request),
                    3 => ModuleImportName::deferred_namespace(request),
                    other => {
                        return Err(BytecodeCodecError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                })
            }
            other => {
                return Err(BytecodeCodecError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let mut binding = UnverifiedModuleBindingDescriptor::new(name, slot, policy, origin);
        if let Some(constant) = initializer {
            binding = binding.with_initializer(constant);
        }
        if let Some(import) = import {
            binding = binding.with_import(import);
        }
        bindings.push(binding);
    }
    let request_count = read_count(&mut reader)?;
    let mut requests = Vec::new();
    requests
        .try_reserve_exact(request_count)
        .map_err(|_| BytecodeCodecError::String("module request allocation failed".to_owned()))?;
    for _ in 0..request_count {
        let specifier = AtomPoolIndex::new(reader.read_u32()?);
        let has_assertions = reader.read_u8()? != 0;
        requests.push(ModuleRequestDescriptor::new(specifier, has_assertions));
    }
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(UnverifiedModuleDeclarationRecord::new(
        Arc::from(bindings),
        Arc::from(requests),
    ))
}

fn executable_kind_tag(kind: CompilerExecutableKind) -> u8 {
    match kind {
        CompilerExecutableKind::GlobalScript => 0,
        CompilerExecutableKind::IndirectEvalScript => 1,
        CompilerExecutableKind::DirectEvalScript => 2,
        CompilerExecutableKind::OrdinaryFunction => 3,
        CompilerExecutableKind::OrdinaryArrow => 4,
        CompilerExecutableKind::AsyncArrow => 5,
        CompilerExecutableKind::OrdinaryMethod => 6,
        CompilerExecutableKind::ClassInstanceInitializer => 7,
        CompilerExecutableKind::ClassConstructor => 8,
        CompilerExecutableKind::GeneratorFunction => 9,
        CompilerExecutableKind::GeneratorMethod => 10,
        CompilerExecutableKind::AsyncFunction => 11,
        CompilerExecutableKind::AsyncMethod => 12,
        CompilerExecutableKind::AsyncGeneratorFunction => 13,
        CompilerExecutableKind::AsyncGeneratorMethod => 14,
        CompilerExecutableKind::DynamicFunctionScript => 15,
        CompilerExecutableKind::Module => 16,
    }
}

fn executable_kind_from_tag(tag: u8) -> Result<CompilerExecutableKind, BytecodeCodecError> {
    match tag {
        0 => Ok(CompilerExecutableKind::GlobalScript),
        1 => Ok(CompilerExecutableKind::IndirectEvalScript),
        2 => Ok(CompilerExecutableKind::DirectEvalScript),
        3 => Ok(CompilerExecutableKind::OrdinaryFunction),
        4 => Ok(CompilerExecutableKind::OrdinaryArrow),
        5 => Ok(CompilerExecutableKind::AsyncArrow),
        6 => Ok(CompilerExecutableKind::OrdinaryMethod),
        7 => Ok(CompilerExecutableKind::ClassInstanceInitializer),
        8 => Ok(CompilerExecutableKind::ClassConstructor),
        9 => Ok(CompilerExecutableKind::GeneratorFunction),
        10 => Ok(CompilerExecutableKind::GeneratorMethod),
        11 => Ok(CompilerExecutableKind::AsyncFunction),
        12 => Ok(CompilerExecutableKind::AsyncMethod),
        13 => Ok(CompilerExecutableKind::AsyncGeneratorFunction),
        14 => Ok(CompilerExecutableKind::AsyncGeneratorMethod),
        15 => Ok(CompilerExecutableKind::DynamicFunctionScript),
        16 => Ok(CompilerExecutableKind::Module),
        other => Err(BytecodeCodecError::FormatMismatch {
            found: u32::from(other),
        }),
    }
}

fn write_option_atom(buffer: &mut Vec<u8>, atom: Option<AtomPoolIndex>) {
    match atom {
        Some(index) => {
            buffer.push(1);
            write_u32(buffer, index.get());
        }
        None => buffer.push(0),
    }
}

fn read_option_atom(reader: &mut Reader<'_>) -> Result<Option<AtomPoolIndex>, BytecodeCodecError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(AtomPoolIndex::new(reader.read_u32()?))),
        other => Err(BytecodeCodecError::FormatMismatch {
            found: u32::from(other),
        }),
    }
}

fn write_option_u32(buffer: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(number) => {
            buffer.push(1);
            write_u32(buffer, number);
        }
        None => buffer.push(0),
    }
}

fn read_option_u32(reader: &mut Reader<'_>) -> Result<Option<u32>, BytecodeCodecError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.read_u32()?)),
        other => Err(BytecodeCodecError::FormatMismatch {
            found: u32::from(other),
        }),
    }
}

fn write_policy(buffer: &mut Vec<u8>, policy: CompilerBindingPolicy) {
    buffer.push(binding_kind_tag(policy.kind()));
    buffer.push(initialization_tag(policy.initialization()));
    buffer.push(writes_tag(policy.writes()));
    buffer.push(u8::from(policy.has_temporal_dead_zone()));
}

fn read_policy(reader: &mut Reader<'_>) -> Result<CompilerBindingPolicy, BytecodeCodecError> {
    let kind = binding_kind_from_tag(reader.read_u8()?)?;
    let initialization = initialization_from_tag(reader.read_u8()?)?;
    let writes = writes_from_tag(reader.read_u8()?)?;
    let temporal_dead_zone = reader.read_u8()? != 0;
    Ok(CompilerBindingPolicy::new(
        kind,
        initialization,
        writes,
        temporal_dead_zone,
    ))
}

fn binding_kind_tag(kind: CompilerBindingKind) -> u8 {
    match kind {
        CompilerBindingKind::Parameter => 0,
        CompilerBindingKind::Var => 1,
        CompilerBindingKind::Let => 2,
        CompilerBindingKind::Const => 3,
        CompilerBindingKind::ClassName => 4,
        CompilerBindingKind::ClassFieldKey => 5,
        CompilerBindingKind::ClassInstanceInitializer => 6,
        CompilerBindingKind::ClassPrivateName => 7,
        CompilerBindingKind::ClassStaticReceiver => 8,
        CompilerBindingKind::WithObject => 9,
        CompilerBindingKind::Function => 10,
        CompilerBindingKind::FunctionName => 11,
        CompilerBindingKind::Catch => 12,
        CompilerBindingKind::GlobalReference => 13,
    }
}

fn binding_kind_from_tag(tag: u8) -> Result<CompilerBindingKind, BytecodeCodecError> {
    match tag {
        0 => Ok(CompilerBindingKind::Parameter),
        1 => Ok(CompilerBindingKind::Var),
        2 => Ok(CompilerBindingKind::Let),
        3 => Ok(CompilerBindingKind::Const),
        4 => Ok(CompilerBindingKind::ClassName),
        5 => Ok(CompilerBindingKind::ClassFieldKey),
        6 => Ok(CompilerBindingKind::ClassInstanceInitializer),
        7 => Ok(CompilerBindingKind::ClassPrivateName),
        8 => Ok(CompilerBindingKind::ClassStaticReceiver),
        9 => Ok(CompilerBindingKind::WithObject),
        10 => Ok(CompilerBindingKind::Function),
        11 => Ok(CompilerBindingKind::FunctionName),
        12 => Ok(CompilerBindingKind::Catch),
        13 => Ok(CompilerBindingKind::GlobalReference),
        other => Err(BytecodeCodecError::FormatMismatch {
            found: u32::from(other),
        }),
    }
}

fn initialization_tag(policy: CompilerInitializationPolicy) -> u8 {
    match policy {
        CompilerInitializationPolicy::Argument => 0,
        CompilerInitializationPolicy::UndefinedAtInstantiation => 1,
        CompilerInitializationPolicy::AtDeclaration => 2,
        CompilerInitializationPolicy::FunctionAtInstantiation => 3,
        CompilerInitializationPolicy::FunctionAtScopeEntry => 4,
        CompilerInitializationPolicy::FunctionName => 5,
        CompilerInitializationPolicy::Catch => 6,
        CompilerInitializationPolicy::ConstructorRealmLookup => 7,
    }
}

fn initialization_from_tag(tag: u8) -> Result<CompilerInitializationPolicy, BytecodeCodecError> {
    match tag {
        0 => Ok(CompilerInitializationPolicy::Argument),
        1 => Ok(CompilerInitializationPolicy::UndefinedAtInstantiation),
        2 => Ok(CompilerInitializationPolicy::AtDeclaration),
        3 => Ok(CompilerInitializationPolicy::FunctionAtInstantiation),
        4 => Ok(CompilerInitializationPolicy::FunctionAtScopeEntry),
        5 => Ok(CompilerInitializationPolicy::FunctionName),
        6 => Ok(CompilerInitializationPolicy::Catch),
        7 => Ok(CompilerInitializationPolicy::ConstructorRealmLookup),
        other => Err(BytecodeCodecError::FormatMismatch {
            found: u32::from(other),
        }),
    }
}

fn writes_tag(policy: CompilerWritePolicy) -> u8 {
    match policy {
        CompilerWritePolicy::Mutable => 0,
        CompilerWritePolicy::Immutable => 1,
        CompilerWritePolicy::ImmutableInStrictCode => 2,
    }
}

fn writes_from_tag(tag: u8) -> Result<CompilerWritePolicy, BytecodeCodecError> {
    match tag {
        0 => Ok(CompilerWritePolicy::Mutable),
        1 => Ok(CompilerWritePolicy::Immutable),
        2 => Ok(CompilerWritePolicy::ImmutableInStrictCode),
        other => Err(BytecodeCodecError::FormatMismatch {
            found: u32::from(other),
        }),
    }
}

fn closure_source_tag(source: CompilerClosureSource) -> u8 {
    match source {
        CompilerClosureSource::ParentVariableReference(_) => 0,
        CompilerClosureSource::ParentClosure(_) => 1,
        CompilerClosureSource::ConstructorRealmGlobal(_) => 2,
        CompilerClosureSource::DirectEvalBinding { .. } => 3,
        CompilerClosureSource::DirectEvalVariable { .. } => 4,
        CompilerClosureSource::Module { .. } => 5,
    }
}

fn write_closure_source_payload(buffer: &mut Vec<u8>, source: CompilerClosureSource) {
    match source {
        CompilerClosureSource::ParentVariableReference(index)
        | CompilerClosureSource::ParentClosure(index)
        | CompilerClosureSource::Module { index } => write_u32(buffer, index),
        CompilerClosureSource::ConstructorRealmGlobal(atom) => write_u32(buffer, atom.get()),
        CompilerClosureSource::DirectEvalBinding {
            index,
            environment_size,
        }
        | CompilerClosureSource::DirectEvalVariable {
            index,
            environment_size,
        } => {
            write_u32(buffer, index);
            write_u32(buffer, environment_size);
        }
    }
}

fn read_closure_source(
    reader: &mut Reader<'_>,
) -> Result<CompilerClosureSource, BytecodeCodecError> {
    match reader.read_u8()? {
        0 => Ok(CompilerClosureSource::ParentVariableReference(
            reader.read_u32()?,
        )),
        1 => Ok(CompilerClosureSource::ParentClosure(reader.read_u32()?)),
        2 => Ok(CompilerClosureSource::ConstructorRealmGlobal(
            AtomPoolIndex::new(reader.read_u32()?),
        )),
        3 => Ok(CompilerClosureSource::DirectEvalBinding {
            index: reader.read_u32()?,
            environment_size: reader.read_u32()?,
        }),
        4 => Ok(CompilerClosureSource::DirectEvalVariable {
            index: reader.read_u32()?,
            environment_size: reader.read_u32()?,
        }),
        5 => Ok(CompilerClosureSource::Module {
            index: reader.read_u32()?,
        }),
        other => Err(BytecodeCodecError::FormatMismatch {
            found: u32::from(other),
        }),
    }
}

fn write_utf8(buffer: &mut Vec<u8>, text: &str) {
    write_u32(buffer, text.len() as u32);
    buffer.extend_from_slice(text.as_bytes());
}

fn read_utf8(reader: &mut Reader<'_>) -> Result<String, BytecodeCodecError> {
    let length = read_count(reader)?;
    let bytes = reader.read_bytes(length)?;
    String::from_utf8(bytes.to_vec())
        .map_err(|_| BytecodeCodecError::String("invalid UTF-8 text".to_owned()))
}

fn write_span(buffer: &mut Vec<u8>, span: SourceByteSpan) {
    write_u32(buffer, span.start());
    write_u32(buffer, span.end());
}

fn read_span(reader: &mut Reader<'_>) -> Result<SourceByteSpan, BytecodeCodecError> {
    Ok(SourceByteSpan::new(reader.read_u32()?, reader.read_u32()?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        AtomPoolIndex, Binary64Constant, CompilerAtom, CompilerBigInt, CompilerClosureSource,
        CompilerConstant, CompilerConstantValue, CompilerString, CompilerTemplateElement,
        CompilerTemplateObject, FunctionTemplateId,
    };

    use super::*;

    #[test]
    fn section_framing_round_trips() {
        let payload = frame_sections(&[(1, vec![1, 2, 3]), (2, Vec::new()), (3, vec![0xAB; 300])])
            .expect("framing");
        assert!(payload.starts_with(&BYTECODE_CODEC_MAGIC));
        let sections = read_sections(&payload).expect("read");
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0], (1, &[1, 2, 3][..]));
        assert_eq!(sections[1].1.len(), 0);
        assert_eq!(sections[2].1.len(), 300);
    }

    #[test]
    fn framing_fails_closed_on_damage() {
        let payload = frame_sections(&[(1, vec![7, 8])]).expect("framing");
        let mut target;

        target = payload.clone();
        target[0] ^= 0xFF;
        assert!(matches!(
            read_sections(&target),
            Err(BytecodeCodecError::MagicMismatch)
        ));
        target = payload.clone();
        target[8] = 9;
        assert!(matches!(
            read_sections(&target),
            Err(BytecodeCodecError::FormatMismatch { found: 9 })
        ));
        assert!(matches!(
            read_sections(&payload[..payload.len() - 3]),
            Err(BytecodeCodecError::Truncated)
        ));
        // Duplicate tags fail closed (order invariant).
        assert!(matches!(
            frame_sections(&[(1, vec![1]), (1, vec![2])]),
            Err(BytecodeCodecError::FormatMismatch { found: 1 })
        ));
    }

    fn string(units: &[u16]) -> CompilerString {
        CompilerString::try_from_code_units(Arc::from(units)).expect("string")
    }

    #[test]
    fn atom_pools_round_trip() {
        let atoms = vec![
            CompilerAtom::new(string(&[b'a' as u16, 0x4E2D, 0x2F47])),
            CompilerAtom::new(string(&[0xD800])), // lone surrogate survives exactly
            CompilerAtom::new_static_property_only(string(&[])),
            CompilerAtom::new(string(&[7, 8, 9])),
        ];
        let payload = encode_atom_pool(&atoms);
        let decoded = decode_atom_pool(&payload).expect("decode");
        assert_eq!(decoded, atoms);
    }

    #[test]
    fn constant_pools_round_trip() {
        let constants = vec![
            CompilerConstant::Value(CompilerConstantValue::Number(
                Binary64Constant::from_bits(0x7FF8_0000_0000_0001), // NaN payload bits
            )),
            CompilerConstant::Value(CompilerConstantValue::String(string(&[1, 2]))),
            CompilerConstant::Value(CompilerConstantValue::BigInt(
                CompilerBigInt::try_from_decimal(string(&[b'9' as u16; 40])).expect("bigint"),
            )),
            CompilerConstant::Value(CompilerConstantValue::TemplateObject(
                CompilerTemplateObject::try_from_elements(Arc::from([
                    CompilerTemplateElement::new(Some(string(&[3])), string(&[4, 5])),
                ]))
                .expect("template"),
            )),
            CompilerConstant::Value(CompilerConstantValue::TemplateObject(
                CompilerTemplateObject::try_from_elements(Arc::from([
                    CompilerTemplateElement::new(None, string(&[6])),
                ]))
                .expect("template"),
            )),
            CompilerConstant::Function(FunctionTemplateId::new(3)),
        ];
        let payload = encode_constant_pool(&constants);
        let decoded = decode_constant_pool(&payload).expect("decode");
        assert_eq!(decoded, constants);
    }

    #[test]
    fn closure_sources_round_trip() {
        let sources = vec![
            CompilerClosureSource::ParentVariableReference(2),
            CompilerClosureSource::ParentClosure(1),
            CompilerClosureSource::ConstructorRealmGlobal(AtomPoolIndex::new(5)),
            CompilerClosureSource::DirectEvalBinding {
                index: 0,
                environment_size: 3,
            },
            CompilerClosureSource::DirectEvalVariable {
                index: 4,
                environment_size: 9,
            },
            CompilerClosureSource::Module { index: 6 },
        ];
        let payload = encode_closure_sources(&sources);
        let decoded = decode_closure_sources(&payload).expect("decode");
        assert_eq!(decoded, sources);
    }

    #[test]
    fn pool_decoding_fails_closed() {
        let atoms = vec![CompilerAtom::new(string(&[1, 2]))];
        let payload = encode_atom_pool(&atoms);
        assert!(matches!(
            decode_atom_pool(&payload[..payload.len() - 1]),
            Err(BytecodeCodecError::Truncated)
        ));
        // An unknown constant sub-tag fails closed (no panic).
        let mut damaged = encode_constant_pool(&[CompilerConstant::Value(
            CompilerConstantValue::Number(Binary64Constant::from_bits(1)),
        )]);
        damaged[5] = 0xEE;
        assert!(matches!(
            decode_constant_pool(&damaged),
            Err(BytecodeCodecError::FormatMismatch { .. })
        ));
        // An unknown closure-source tag fails closed.
        let mut damaged = encode_closure_sources(&[CompilerClosureSource::Module { index: 0 }]);
        damaged[4] = 0xEE;
        assert!(matches!(
            decode_closure_sources(&damaged),
            Err(BytecodeCodecError::FormatMismatch { .. })
        ));
        // A truncated string length fails closed.
        let mut damaged = encode_atom_pool(&atoms);
        damaged.truncate(damaged.len() - 3);
        assert!(matches!(
            decode_atom_pool(&damaged),
            Err(BytecodeCodecError::Truncated)
        ));
    }
}
