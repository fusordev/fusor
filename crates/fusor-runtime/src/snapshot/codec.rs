//! Snapshot blob encoding (§8.3): deterministic little-endian framing
//! with bounds-checked reading and a self-written CRC-32 payload
//! checksum. No dependencies; failures are always typed
//! [`super::SnapshotError`] values, never panics.

use super::SnapshotError;

/// Reads one snapshot blob or section payload with bounds checks.
///
/// Every read beyond the input fails closed with
/// [`SnapshotError::Truncated`]; positions are always byte-exact.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    /// Wraps one byte slice.
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Reads one byte.
    pub(crate) fn read_u8(&mut self) -> Result<u8, SnapshotError> {
        let byte = *self.read_bytes(1)?.first().ok_or(SnapshotError::Truncated)?;
        Ok(byte)
    }

    /// Reads one little-endian `u16`.
    pub(crate) fn read_u16(&mut self) -> Result<u16, SnapshotError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Reads one little-endian `u32`.
    pub(crate) fn read_u32(&mut self) -> Result<u32, SnapshotError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads one little-endian `u64`.
    pub(crate) fn read_u64(&mut self) -> Result<u64, SnapshotError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Reads exactly `length` bytes.
    pub(crate) fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotError::Truncated)?;
        let slice = self.bytes.get(self.position..end).ok_or(SnapshotError::Truncated)?;
        self.position = end;
        Ok(slice)
    }

    /// Returns the unconsumed trailing byte count.
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
}

/// Appends one section frame: tag, little-endian payload byte length, the
/// payload, and the CRC-32 of the payload (§8.3).
pub(crate) fn write_section(buffer: &mut Vec<u8>, tag: u8, payload: &[u8]) {
    buffer.push(tag);
    buffer.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    buffer.extend_from_slice(payload);
    buffer.extend_from_slice(&crc32(payload).to_le_bytes());
}

/// Reads one section payload (length + bytes + checksum), verifying the
/// CRC-32 before returning it (§8.3: validate on load).
pub(crate) fn read_section_payload<'a>(
    reader: &mut Reader<'a>,
) -> Result<&'a [u8], SnapshotError> {
    let length = reader.read_u64()?;
    let length = usize::try_from(length).map_err(|_| SnapshotError::IntegrityViolation)?;
    let payload = reader.read_bytes(length)?;
    let stored = reader.read_u32()?;
    if crc32(payload) != stored {
        return Err(SnapshotError::IntegrityViolation);
    }
    Ok(payload)
}

/// The standard CRC-32 (IEEE, reflected, polynomial `0xEDB88320`), used as
/// the snapshot payload checksum. Self-written: the engine takes no new
/// dependencies (§8.3).
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
