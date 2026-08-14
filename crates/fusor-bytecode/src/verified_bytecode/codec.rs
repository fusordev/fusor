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

use crate::{
    AtomPoolIndex, Binary64Constant, CompilerAtom, CompilerBigInt, CompilerClosureSource,
    CompilerConstant, CompilerConstantValue, CompilerString, CompilerTemplateElement,
    CompilerTemplateObject, FunctionTemplateId,
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
}

impl fmt::Display for BytecodeCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MagicMismatch => formatter.write_str("not a bytecode payload (bad magic)"),
            Self::FormatMismatch { found } => {
                write!(formatter, "unsupported bytecode format stamp or section tag {found}")
            }
            Self::Truncated => formatter.write_str("the bytecode payload is truncated"),
            Self::String(message) => write!(formatter, "invalid bytecode pool value: {message}"),
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
            return Err(BytecodeCodecError::FormatMismatch { found: u32::from(tag) });
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
    let units: Vec<u16> = string.code_units().map(u16::from).collect();
    write_u32(buffer, units.len() as u32);
    for unit in units {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
}

/// Decodes one compiler string from its exact UTF-16 units.
fn decode_string(reader: &mut Reader<'_>) -> Result<CompilerString, BytecodeCodecError> {
    let length = reader.read_u32()?;
    let length = usize::try_from(length).map_err(|_| BytecodeCodecError::Truncated)?;
    let bytes = reader
        .read_bytes(length.checked_mul(2).ok_or(BytecodeCodecError::Truncated)?)?;
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
    let count = read_count(&mut reader)?;
    let mut atoms = Vec::new();
    atoms
        .try_reserve_exact(count)
        .map_err(|_| BytecodeCodecError::String("atom allocation failed".to_owned()))?;
    for _ in 0..count {
        let flag = reader.read_u8()?;
        let string = decode_string(&mut reader)?;
        let atom = match flag {
            0 => CompilerAtom::new(string),
            1 => CompilerAtom::new_static_property_only(string),
            other => return Err(BytecodeCodecError::FormatMismatch { found: u32::from(other) }),
        };
        atoms.push(atom);
    }
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(atoms)
}

/// Encodes one heterogeneous constant pool: `count`, then per constant a
/// kind byte (`1` = nested function, `0` = value), then the payload —
/// numbers as exact binary64 bits, strings and BigInt decimals as UTF-16
/// units, template objects as cooked/raw element pairs.
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
    let count = read_count(&mut reader)?;
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
                    1 => CompilerConstantValue::String(decode_string(&mut reader)?),
                    2 => CompilerConstantValue::BigInt(
                        CompilerBigInt::try_from_decimal(decode_string(&mut reader)?)
                            .map_err(|error| BytecodeCodecError::String(error.to_string()))?,
                    ),
                    3 => {
                        let element_count = read_count(&mut reader)?;
                        let mut elements = Vec::new();
                        elements.try_reserve_exact(element_count).map_err(|_| {
                            BytecodeCodecError::String("template allocation failed".to_owned())
                        })?;
                        for _ in 0..element_count {
                            let cooked = match reader.read_u8()? {
                                0 => None,
                                1 => Some(decode_string(&mut reader)?),
                                other => {
                                    return Err(BytecodeCodecError::FormatMismatch {
                                        found: u32::from(other),
                                    });
                                }
                            };
                            elements.push(CompilerTemplateElement::new(
                                cooked,
                                decode_string(&mut reader)?,
                            ));
                        }
                        CompilerConstantValue::TemplateObject(
                            CompilerTemplateObject::try_from_elements(Arc::from(elements))
                                .map_err(|error| {
                                    BytecodeCodecError::String(error.to_string())
                                })?,
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
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(constants)
}

/// Encodes one closure-source pool: `count`, then per entry a variant tag
/// and its dense payload.
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
    let count = read_count(&mut reader)?;
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
    if reader.remaining() != 0 {
        return Err(BytecodeCodecError::Truncated);
    }
    Ok(sources)
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
        let payload = frame_sections(&[
            (1, vec![1, 2, 3]),
            (2, Vec::new()),
            (3, vec![0xAB; 300]),
        ])
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
        let payload =
            frame_sections(&[(1, vec![7, 8])]).expect("framing");
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
