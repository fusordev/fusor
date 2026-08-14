//! Heap snapshot serialization (§8): a self-written compact binary codec
//! with load-time validation. The format is versioned by a magic and a
//! format stamp; anything unknown fails closed with a typed
//! [`SnapshotError`] — there is deliberately no version compatibility in
//! the alpha line (§8.1).
//!
//! The serializer lives inside the engine because it needs heap-internal
//! access; the host side only provides the builder APIs (subproject 6).
//! Identity-only state is excluded from the blob by construction:
//! Rust closures (host functions), runtime resources, and unique
//! symbols/private names (§8.2).
//!
//! Format (stamp 1):
//!
//! ```text
//! magic     8 bytes  "FUSORSNP"
//! stamp     u32 LE   SNAPSHOT_FORMAT_STAMP
//! sections  u32 LE   section count
//! per section: tag u8, payload byte length u64 LE, payload, CRC-32 u32 LE
//!   tag 1 = atoms: count u64 LE, then per atom:
//!     kind u8 (0 = String, 1 = GlobalSymbol),
//!     description: unit count u32 LE, UTF-16 code units u16 LE
//! ```
//!
//! The atoms section records the live dynamic atoms deterministically;
//! restored entries stay in the interner exactly as long as heap content
//! references them (the interner holds weak slots), so the section's
//! role is deduplication for the object sections and accounting sanity,
//! not retention.

mod codec;

use std::{error::Error, fmt};

use crate::{AtomError, AtomKind, JsString, JsStringError, PredefinedAtom, Runtime};

/// The snapshot magic: every blob starts with these bytes.
pub const SNAPSHOT_MAGIC: [u8; 8] = *b"FUSORSNP";

/// The current snapshot format stamp (§8.1: no version compatibility —
/// any other stamp is rejected).
pub const SNAPSHOT_FORMAT_STAMP: u32 = 1;

/// The atoms section tag.
const SECTION_ATOMS: u8 = 1;

/// Snapshot serialization and restoration failures (§8.3, §8.6 negative
/// matrix). Every failure is typed; none panic.
#[derive(Debug)]
pub enum SnapshotError {
    /// The blob does not start with [`SNAPSHOT_MAGIC`] (not a snapshot).
    MagicMismatch,
    /// The format stamp is unknown or a section tag is unrecognized
    /// (alpha: no version compatibility, §8.1).
    FormatMismatch {
        /// The stamp or tag that was rejected.
        found: u32,
    },
    /// The blob ends inside a frame or section.
    Truncated,
    /// The payload checksum or a structural bound is inconsistent
    /// (tampered or corrupt blob).
    IntegrityViolation,
    /// The restore target is not a fresh runtime (its atom table already
    /// holds dynamic entries, §8.3: restore fills an empty skeleton).
    AlreadyPopulated,
    /// A restored atom description could not be rebuilt.
    String(JsStringError),
    /// The atom table rejected a restore (limits or allocation).
    Atom(AtomError),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MagicMismatch => formatter.write_str("not a snapshot blob (bad magic)"),
            Self::FormatMismatch { found } => {
                write!(formatter, "unsupported snapshot format stamp or section tag {found}")
            }
            Self::Truncated => formatter.write_str("the snapshot blob is truncated"),
            Self::IntegrityViolation => formatter.write_str("the snapshot blob failed its checksum"),
            Self::AlreadyPopulated => formatter.write_str(
                "the restore target runtime is not fresh (its atom table already has dynamic entries)",
            ),
            Self::String(source) => write!(formatter, "snapshot string rebuild failed: {source}"),
            Self::Atom(source) => write!(formatter, "snapshot atom restore failed: {source}"),
        }
    }
}

impl Error for SnapshotError {}

impl Runtime {
    /// Serializes the heap into one snapshot blob (§8.1, §8.3).
    ///
    /// The current format covers the dynamic atoms table; the object,
    /// shape, function, module, realm, and binding-cell sections land with
    /// their serializer slices (§8.2). Identity-only state (host closures,
    /// resources, unique symbols) is excluded by construction.
    ///
    /// # Errors
    ///
    /// Returns a [`SnapshotError`] when a serialized description cannot
    /// be encoded. This function never panics.
    pub fn snapshot(&self) -> Result<Vec<u8>, SnapshotError> {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&SNAPSHOT_MAGIC);
        buffer.extend_from_slice(&SNAPSHOT_FORMAT_STAMP.to_le_bytes());
        let count_position = buffer.len();
        buffer.extend_from_slice(&0u32.to_le_bytes());
        let mut sections = 0u32;
        let atoms = self.atoms.snapshot_atoms();
        if !atoms.is_empty() {
            let payload = encode_atoms(&atoms);
            codec::write_section(&mut buffer, SECTION_ATOMS, &payload);
            sections += 1;
        }
        buffer[count_position..count_position + 4].copy_from_slice(&sections.to_le_bytes());
        Ok(buffer)
    }

    /// Restores one snapshot blob into this runtime (§8.1, §8.3).
    ///
    /// The target must be a fresh runtime: restoration fills the empty
    /// skeleton section by section, validating every frame on load. No
    /// microtasks drain and no JavaScript runs during restore.
    ///
    /// # Errors
    ///
    /// Returns a typed [`SnapshotError`] for a magic/stamp mismatch, a
    /// truncated or tampered blob, or a non-fresh target. This function
    /// never panics.
    pub fn from_snapshot(&mut self, blob: &[u8]) -> Result<(), SnapshotError> {
        let mut reader = codec::Reader::new(blob);
        if reader.read_bytes(SNAPSHOT_MAGIC.len())? != SNAPSHOT_MAGIC {
            return Err(SnapshotError::MagicMismatch);
        }
        let stamp = reader.read_u32()?;
        if stamp != SNAPSHOT_FORMAT_STAMP {
            return Err(SnapshotError::FormatMismatch { found: stamp });
        }
        if self.atom_usage().live_atoms != PredefinedAtom::COUNT as u32 {
            return Err(SnapshotError::AlreadyPopulated);
        }
        let sections = reader.read_u32()?;
        for _ in 0..sections {
            let tag = reader.read_u8()?;
            let payload = codec::read_section_payload(&mut reader)?;
            match tag {
                SECTION_ATOMS => {
                    let atoms = decode_atoms(payload)?;
                    self.atoms.restore_atoms(&atoms).map_err(SnapshotError::Atom)?;
                }
                other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
            }
        }
        if reader.remaining() != 0 {
            return Err(SnapshotError::IntegrityViolation);
        }
        Ok(())
    }
}

/// Encodes one atom list into the atoms-section payload (format above).
/// Identity atoms (unique symbols, private names) never enter snapshots
/// (§8.2) and are filtered defensively; the count covers only the
/// encodable entries.
pub(crate) fn encode_atoms(atoms: &[(AtomKind, JsString)]) -> Vec<u8> {
    let encodable = atoms
        .iter()
        .filter(|(kind, _)| matches!(kind, AtomKind::String | AtomKind::GlobalSymbol));
    let mut payload = Vec::new();
    let mut count = 0u64;
    for (kind, description) in encodable {
        count += 1;
        payload.push(match kind {
            AtomKind::String => 0,
            AtomKind::GlobalSymbol => 1,
            AtomKind::Symbol | AtomKind::Private => continue,
        });
        let units: Vec<u16> = description.code_units().collect();
        payload.extend_from_slice(&(units.len() as u32).to_le_bytes());
        for unit in units {
            payload.extend_from_slice(&unit.to_le_bytes());
        }
    }
    let mut framed = Vec::new();
    framed.extend_from_slice(&count.to_le_bytes());
    framed.extend_from_slice(&payload);
    framed
}

/// Decodes the atoms-section payload (format above).
pub(crate) fn decode_atoms(payload: &[u8]) -> Result<Vec<(AtomKind, JsString)>, SnapshotError> {
    let mut reader = codec::Reader::new(payload);
    let count = reader.read_u64()?;
    let mut atoms = Vec::new();
    for _ in 0..count {
        let kind = match reader.read_u8()? {
            0 => AtomKind::String,
            1 => AtomKind::GlobalSymbol,
            other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
        };
        let unit_count = reader.read_u32()?;
        let mut units = Vec::new();
        for _ in 0..unit_count {
            units.push(reader.read_u16()?);
        }
        let description = JsString::from_code_units(units).map_err(SnapshotError::String)?;
        atoms.push((kind, description));
    }
    Ok(atoms)
}
