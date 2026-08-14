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
//!   tag 2 = objects (ordinary objects only in this slice, §8.2):
//!     count u64 LE, then per object:
//!       kind u8 (0 = Ordinary)
//!       prototype u8 (0 = none, 1 = object, 2 = function) + index u32 LE
//!       extensible u8, is_html_dda u8
//!       shape: property count u32 LE, then per property:
//!         key u8 (0 = array index + u32 LE, 1 = atom) then for atoms:
//!           kind u8 (0 = String, 1 = GlobalSymbol), description units
//!           (unit count u32 LE, UTF-16 u16 LE)
//!         layout u8 (0 = data, 1 = accessor) + bits u8
//!       slots: aligned with the shape, per slot:
//!         tag u8 (0 = data) + the value encoding:
//!           0 Undefined, 1 Null, 2 Boolean + u8, 3 Number + f64 LE,
//!           4 BigInt + limb count u32 + u32 limbs LE,
//!           5 String units, 6 GlobalSymbol units, 7 Object + index u32,
//!           8 Function + index u32
//! ```
//!
//! The atoms section records the live dynamic atoms deterministically;
//! restored entries stay in the interner exactly as long as heap content
//! references them (the interner holds weak slots), so the section's
//! role is deduplication for the object sections and accounting sanity,
//! not retention. Sections appear in tag order; the object section may
//! only follow the atoms section because restored property keys re-intern
//! against it.
//!
//! Heap content the current format does not cover yet fails closed with
//! [`SnapshotError::Unsupported`] (intermediate slices, §8.2): exotic
//! object kinds, accessor slots, function values, and identity symbols.

mod codec;

use std::{error::Error, fmt};

use crate::{
    ArrayIndex, AtomError, AtomKind, JsBigInt, JsNumber, JsString, JsStringError, PredefinedAtom,
    PropertyKey, PropertyLayout, Runtime, ids::ObjectId,
    object::{HeapObject, HeapObjectKind, ObjectRecord, PropertySlot, ShapeProperty},
    value::{HeapReference, StoredValue},
};

/// The snapshot magic: every blob starts with these bytes.
pub const SNAPSHOT_MAGIC: [u8; 8] = *b"FUSORSNP";

/// The current snapshot format stamp (§8.1: no version compatibility —
/// any other stamp is rejected).
pub const SNAPSHOT_FORMAT_STAMP: u32 = 1;

/// The atoms section tag.
const SECTION_ATOMS: u8 = 1;

/// The objects section tag.
const SECTION_OBJECTS: u8 = 2;

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
    /// The serializer met heap content the current format does not cover
    /// yet (an intermediate slice, §8.2): exotic object kinds, accessor
    /// slots, function values, or identity symbols. Restoration of such
    /// content lands with its serializer slice.
    Unsupported {
        /// The arena index of the first unsupported entry.
        index: usize,
        /// The unsupported content name.
        what: &'static str,
    },
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
            Self::Unsupported { index, what } => {
                write!(formatter, "snapshot does not cover {what} at heap index {index} yet")
            }
            Self::String(source) => write!(formatter, "snapshot string rebuild failed: {source}"),
            Self::Atom(source) => write!(formatter, "snapshot atom restore failed: {source}"),
        }
    }
}

impl Error for SnapshotError {}

impl Runtime {
    /// Serializes the heap into one snapshot blob (§8.1, §8.3).
    ///
    /// The current format covers the dynamic atoms table and ordinary
    /// objects (shapes, data properties, primitive values, object
    /// references). Heap content beyond that — exotic object kinds,
    /// accessor slots, function values, identity symbols — fails closed
    /// with [`SnapshotError::Unsupported`] until its serializer slice
    /// lands (§8.2).
    ///
    /// # Errors
    ///
    /// Returns a typed [`SnapshotError`] for unsupported heap content.
    /// This function never panics.
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
        if self.objects.iter().next().is_some() {
            let payload = encode_objects(self)?;
            codec::write_section(&mut buffer, SECTION_OBJECTS, &payload);
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
                SECTION_OBJECTS => {
                    let staged = decode_objects(payload)?;
                    restore_objects(self, staged)?;
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

/// One staged (not yet resolved) property key from the objects section.
enum StagedKey {
    Index(u32),
    Atom(AtomKind, Vec<u16>),
}

/// One staged (not yet resolved) slot value from the objects section.
enum StagedValue {
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    BigInt(Vec<u32>),
    String(Vec<u16>),
    GlobalSymbol(Vec<u16>),
    Object(usize),
    Function(usize),
}

/// One staged object waiting for its arena slot.
struct StagedObject {
    prototype: Option<(u8, usize)>,
    extensible: bool,
    is_html_dda: bool,
    shape: Vec<(StagedKey, PropertyLayout)>,
    slots: Vec<StagedValue>,
}

/// Encodes every object into the objects-section payload (format above);
/// unsupported content fails closed (§8.2).
fn encode_objects(runtime: &Runtime) -> Result<Vec<u8>, SnapshotError> {
    let mut payload = Vec::new();
    let count = runtime.objects.iter().count();
    payload.extend_from_slice(&(count as u64).to_le_bytes());
    for (id, object) in runtime.objects.iter() {
        match object.kind() {
            HeapObjectKind::Ordinary => payload.push(0),
            _ => {
                return Err(SnapshotError::Unsupported {
                    index: id.index(),
                    what: "an exotic object kind",
                });
            }
        }
        let record = &object.record;
        match record.prototype() {
            None => payload.push(0),
            Some(HeapReference::Object(target)) => {
                payload.push(1);
                payload.extend_from_slice(&(target.index() as u32).to_le_bytes());
            }
            Some(HeapReference::Function(_)) => {
                return Err(SnapshotError::Unsupported {
                    index: id.index(),
                    what: "a function prototype",
                });
            }
        }
        payload.push(u8::from(record.is_extensible()));
        payload.push(u8::from(record.is_html_dda()));
        let shape = record.shape();
        payload.extend_from_slice(&(shape.len() as u32).to_le_bytes());
        for property in shape.iter() {
            encode_property_key(&mut payload, property.key());
            encode_layout(&mut payload, property.layout());
        }
        for slot in record.slots() {
            match slot {
                PropertySlot::Data(value) => {
                    payload.push(0);
                    encode_stored_value(&mut payload, value).map_err(|what| {
                        SnapshotError::Unsupported {
                            index: id.index(),
                            what,
                        }
                    })?;
                }                PropertySlot::Accessor { .. } => {
                    return Err(SnapshotError::Unsupported {
                        index: id.index(),
                        what: "an accessor property slot",
                    });
                }
            }
        }
    }
    Ok(payload)
}

/// Encodes one property key (format above).
fn encode_property_key(buffer: &mut Vec<u8>, key: &PropertyKey) {
    if let Some(index) = key.as_index() {
        buffer.push(0);
        buffer.extend_from_slice(&index.value().to_le_bytes());
    } else if let Some(atom) = key.as_atom() {
        buffer.push(1);
        encode_atom_content(buffer, atom);
    }
}

/// Encodes one atom's (kind, description) content inline.
fn encode_atom_content(buffer: &mut Vec<u8>, atom: &crate::Atom) {
    buffer.push(match atom.kind() {
        AtomKind::String => 0,
        AtomKind::GlobalSymbol => 1,
        AtomKind::Symbol | AtomKind::Private => 2,
    });
    let units: Vec<u16> = atom
        .description()
        .map(|description| description.code_units().collect())
        .unwrap_or_default();
    buffer.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for unit in units {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
}

/// Encodes one property layout (format above).
fn encode_layout(buffer: &mut Vec<u8>, layout: &PropertyLayout) {
    match layout.kind() {
        crate::PropertyLayoutKind::Data => {
            buffer.push(0);
            let mut bits = 0u8;
            if layout.writable() == Some(true) {
                bits |= 1;
            }
            if layout.enumerable() {
                bits |= 2;
            }
            if layout.configurable() {
                bits |= 4;
            }
            buffer.push(bits);
        }
        crate::PropertyLayoutKind::Accessor => {
            buffer.push(1);
            let mut bits = 0u8;
            if layout.enumerable() {
                bits |= 2;
            }
            if layout.configurable() {
                bits |= 4;
            }
            buffer.push(bits);
        }
    }
}

/// Encodes one stored value (format above); returns the unsupported
/// content name on failure.
fn encode_stored_value(buffer: &mut Vec<u8>, value: &StoredValue) -> Result<(), &'static str> {
    match value {
        StoredValue::Undefined => buffer.push(0),
        StoredValue::Null => buffer.push(1),
        StoredValue::Boolean(value) => {
            buffer.push(2);
            buffer.push(u8::from(*value));
        }
        StoredValue::Number(value) => {
            buffer.push(3);
            buffer.extend_from_slice(&value.as_f64().to_le_bytes());
        }
        StoredValue::BigInt(value) => {
            buffer.push(4);
            let limbs = value.limbs();
            buffer.extend_from_slice(&(limbs.len() as u32).to_le_bytes());
            for limb in limbs {
                buffer.extend_from_slice(&limb.to_le_bytes());
            }
        }
        StoredValue::String(value) => {
            buffer.push(5);
            let units: Vec<u16> = value.code_units().collect();
            buffer.extend_from_slice(&(units.len() as u32).to_le_bytes());
            for unit in units {
                buffer.extend_from_slice(&unit.to_le_bytes());
            }
        }
        StoredValue::Symbol(atom) => match atom.kind() {
            AtomKind::GlobalSymbol => {
                buffer.push(6);
                encode_atom_content(buffer, atom);
            }
            AtomKind::String | AtomKind::Symbol | AtomKind::Private => {
                return Err("an identity symbol value");
            }
        },
        StoredValue::Object(target) => {
            buffer.push(7);
            buffer.extend_from_slice(&(target.index() as u32).to_le_bytes());
        }
        StoredValue::Function(_) => return Err("a function value"),
    }
    Ok(())
}

/// Decodes the objects-section payload into staged objects (format
/// above).
fn decode_objects(payload: &[u8]) -> Result<Vec<StagedObject>, SnapshotError> {
    let mut reader = codec::Reader::new(payload);
    let count = reader.read_u64()?;
    let mut staged = Vec::new();
    for _ in 0..count {
        let kind = reader.read_u8()?;
        if kind != 0 {
            return Err(SnapshotError::FormatMismatch { found: u32::from(kind) });
        }
        let prototype = match reader.read_u8()? {
            0 => None,
            1 => Some((1, reader.read_u32()? as usize)),
            2 => Some((2, reader.read_u32()? as usize)),
            other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
        };
        let extensible = reader.read_u8()? != 0;
        let is_html_dda = reader.read_u8()? != 0;
        let property_count = reader.read_u32()?;
        let mut shape = Vec::new();
        for _ in 0..property_count {
            let key = match reader.read_u8()? {
                0 => StagedKey::Index(reader.read_u32()?),
                1 => {
                    let kind = match reader.read_u8()? {
                        0 => AtomKind::String,
                        1 => AtomKind::GlobalSymbol,
                        other => {
                            return Err(SnapshotError::FormatMismatch { found: u32::from(other) });
                        }
                    };
                    let unit_count = reader.read_u32()?;
                    let mut units = Vec::new();
                    for _ in 0..unit_count {
                        units.push(reader.read_u16()?);
                    }
                    StagedKey::Atom(kind, units)
                }
                other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
            };
            let layout = match reader.read_u8()? {
                0 => {
                    let bits = reader.read_u8()?;
                    PropertyLayout::data(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0)
                }
                1 => {
                    let bits = reader.read_u8()?;
                    PropertyLayout::accessor(bits & 2 != 0, bits & 4 != 0)
                }
                other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
            };
            shape.push((key, layout));
        }
        let mut slots = Vec::new();
        for _ in 0..property_count {
            let slot_tag = reader.read_u8()?;
            if slot_tag != 0 {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(slot_tag),
                });
            }
            slots.push(decode_staged_value(&mut reader)?);
        }
        staged.push(StagedObject {
            prototype,
            extensible,
            is_html_dda,
            shape,
            slots,
        });
    }
    Ok(staged)
}

/// Decodes one staged slot value (format above).
fn decode_staged_value(reader: &mut codec::Reader<'_>) -> Result<StagedValue, SnapshotError> {
    Ok(match reader.read_u8()? {
        0 => StagedValue::Undefined,
        1 => StagedValue::Null,
        2 => StagedValue::Boolean(reader.read_u8()? != 0),
        3 => {
            let bytes = reader.read_bytes(8)?;
            let bytes: [u8; 8] = bytes.try_into().map_err(|_| SnapshotError::Truncated)?;
            StagedValue::Number(f64::from_le_bytes(bytes))
        }
        4 => {
            let count = reader.read_u32()?;
            let mut limbs = Vec::new();
            for _ in 0..count {
                limbs.push(reader.read_u32()?);
            }
            StagedValue::BigInt(limbs)
        }
        5 => {
            let unit_count = reader.read_u32()?;
            let mut units = Vec::new();
            for _ in 0..unit_count {
                units.push(reader.read_u16()?);
            }
            StagedValue::String(units)
        }
        6 => {
            let unit_count = reader.read_u32()?;
            let mut units = Vec::new();
            for _ in 0..unit_count {
                units.push(reader.read_u16()?);
            }
            StagedValue::GlobalSymbol(units)
        }
        7 => StagedValue::Object(reader.read_u32()? as usize),
        8 => StagedValue::Function(reader.read_u32()? as usize),
        other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
    })
}

/// Resolves the staged objects into the object arena (§8.3): objects
/// insert in decode order so their restored identities match the encoded
/// indices, then each record fills with its resolved shape, slots, and
/// prototype.
fn restore_objects(
    runtime: &mut Runtime,
    staged: Vec<StagedObject>,
) -> Result<(), SnapshotError> {
    let mut ids = Vec::new();
    for _ in &staged {
        let placeholder = HeapObject::ordinary(ObjectRecord::from_parts(
            None,
            true,
            false,
            std::sync::Arc::new(Vec::new()),
            Some(runtime.shape_interner.clone()),
            Vec::new(),
        ));
        let id = runtime
            .objects
            .try_insert(placeholder)
            .map_err(|_| SnapshotError::IntegrityViolation)?;
        ids.push(id);
    }
    for (staged, id) in staged.into_iter().zip(ids.iter().copied()) {
        let mut shape_properties = Vec::new();
        for (key, layout) in staged.shape {
            let key = match key {
                StagedKey::Index(index) => {
                    let index = ArrayIndex::new(index).ok_or(SnapshotError::IntegrityViolation)?;
                    PropertyKey::from_index(index)
                }
                StagedKey::Atom(kind, units) => {
                    let description =
                        JsString::from_code_units(units).map_err(SnapshotError::String)?;
                    let atom = match kind {
                        AtomKind::String => runtime
                            .atoms
                            .intern_string(&description)
                            .map_err(SnapshotError::Atom)?,
                        AtomKind::GlobalSymbol => runtime
                            .atoms
                            .intern_global_symbol(&description)
                            .map_err(SnapshotError::Atom)?,
                        AtomKind::Symbol | AtomKind::Private => {
                            return Err(SnapshotError::IntegrityViolation);
                        }
                    };
                    PropertyKey::from_validated_atom(atom)
                }
            };
            shape_properties.push(ShapeProperty::from_parts(key, layout));
        }
        let mut slots = Vec::new();
        for value in staged.slots {
            slots.push(match value {
                StagedValue::Undefined => PropertySlot::Data(StoredValue::Undefined),
                StagedValue::Null => PropertySlot::Data(StoredValue::Null),
                StagedValue::Boolean(value) => PropertySlot::Data(StoredValue::Boolean(value)),
                StagedValue::Number(value) => {
                    PropertySlot::Data(StoredValue::Number(JsNumber::from_f64(value)))
                }
                StagedValue::BigInt(limbs) => PropertySlot::Data(StoredValue::BigInt(
                    std::sync::Arc::new(JsBigInt::from_normalized_limbs(limbs)),
                )),
                StagedValue::String(units) => {
                    let string =
                        JsString::from_code_units(units).map_err(SnapshotError::String)?;
                    PropertySlot::Data(StoredValue::String(string))
                }
                StagedValue::GlobalSymbol(units) => {
                    let description =
                        JsString::from_code_units(units).map_err(SnapshotError::String)?;
                    let atom = runtime
                        .atoms
                        .intern_global_symbol(&description)
                        .map_err(SnapshotError::Atom)?;
                    PropertySlot::Data(StoredValue::Symbol(atom))
                }
                StagedValue::Object(index) => {
                    let target = *ids.get(index).ok_or(SnapshotError::IntegrityViolation)?;
                    PropertySlot::Data(StoredValue::Object(target))
                }
                StagedValue::Function(_) => return Err(SnapshotError::IntegrityViolation),
            });
        }
        let prototype = match staged.prototype {
            None => None,
            Some((1, index)) => {
                let target = *ids.get(index).ok_or(SnapshotError::IntegrityViolation)?;
                Some(HeapReference::Object(target))
            }
            Some(_) => return Err(SnapshotError::IntegrityViolation),
        };
        let object = runtime
            .objects
            .get_mut(id)
            .ok_or(SnapshotError::IntegrityViolation)?;
        *object = HeapObject::ordinary(ObjectRecord::from_parts(
            prototype,
            staged.extensible,
            staged.is_html_dda,
            std::sync::Arc::new(shape_properties),
            Some(runtime.shape_interner.clone()),
            slots,
        ));
    }
    Ok(())
}
