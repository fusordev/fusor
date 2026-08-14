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

use std::{error::Error, fmt};

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
}

impl fmt::Display for BytecodeCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MagicMismatch => formatter.write_str("not a bytecode payload (bad magic)"),
            Self::FormatMismatch { found } => {
                write!(formatter, "unsupported bytecode format stamp or section tag {found}")
            }
            Self::Truncated => formatter.write_str("the bytecode payload is truncated"),
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

#[cfg(test)]
mod tests {
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
}
