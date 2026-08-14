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
//!   tag 3 = binding cells:
//!     count u32 LE, then per cell:
//!       value u8 (0 = uninitialized, 1 = value) + [the value encoding],
//!       forward u8 (0 = none, 1 = target cell index u32 LE)
//!   tag 4 = functions:
//!     code count u32 LE, then per code: byte length u32 LE + the
//!     verified-bytecode payload (FUSRBYTE codec)
//!     function count u32 LE, then per function:
//!       kind u8 (0 = JS bytecode, 1 = host)
//!       kind 0: code ordinal u32 LE, template u32 LE,
//!         environment count u32 LE + per entry:
//!           tag u8 (0 = Captured cell index u32 LE,
//!                   1 = RealmGlobal binding index u32 LE)
//!         eval shadows u8 (always 0 in this slice), eval environment u8,
//!         lexical receiver u8 + [the value encoding],
//!         lexical eval flags u8 u8,
//!         lexical new target u8 + function index u32 LE,
//!         lexical derived constructor u8 + function index u32 LE,
//!         lexical derived this u8 + cell index u32 LE,
//!         has_instance_elements u8,
//!         home object u8 (0 none, 1 object index u32 LE,
//!                         2 function index u32 LE)
//!         then the object record (same sub-format as tag 2)
//!       kind 1: host slot index u32 LE + the object record
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

use std::sync::Arc;
use std::{error::Error, fmt};

use fusor_bytecode::VerifiedBytecode;

use crate::{
    ArrayIndex, AtomError, AtomKind, JsBigInt, JsNumber, JsString, JsStringError, PredefinedAtom,
    PropertyKey, PropertyLayout, Runtime,
    ids::{FunctionId, ObjectId, RealmId},
    object::{HeapObject, HeapObjectKind, ObjectRecord, PropertySlot, ShapeProperty},
    runtime::{BindingCell, BytecodeFunction, EnvironmentBinding, FunctionImplementation, HeapFunction, InstalledCode},
    value::{HeapReference, SlotValue, StoredValue},
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

/// The binding-cells section tag.
const SECTION_CELLS: u8 = 3;

/// The functions section tag.
const SECTION_FUNCTIONS: u8 = 4;

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
    /// A restored verified-bytecode payload failed its load-time
    /// re-verification (§8.3).
    Bytecode(String),
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
            Self::Bytecode(source) => {
                write!(formatter, "snapshot bytecode re-verification failed: {source}")
            }
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
        if self.cells.len() > 0 {
            let payload = encode_cells(self)?;
            codec::write_section(&mut buffer, SECTION_CELLS, &payload);
            sections += 1;
        }
        if self.functions.len() > 0 {
            let payload = encode_functions(self)?;
            codec::write_section(&mut buffer, SECTION_FUNCTIONS, &payload);
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
        // Two-phase restore: every section decodes first, then the staged
        // records resolve in dependency order (objects before cells, cells
        // before functions once those sections land).
        let mut atoms = None;
        let mut staged_objects = None;
        let mut staged_cells = None;
        let mut staged_functions = None;
        for _ in 0..sections {
            let tag = reader.read_u8()?;
            let payload = codec::read_section_payload(&mut reader)?;
            match tag {
                SECTION_ATOMS => atoms = Some(decode_atoms(payload)?),
                SECTION_OBJECTS => staged_objects = Some(decode_objects(payload)?),
                SECTION_CELLS => staged_cells = Some(decode_cells(payload)?),
                SECTION_FUNCTIONS => staged_functions = Some(decode_functions(payload)?),
                other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
            }
        }
        if reader.remaining() != 0 {
            return Err(SnapshotError::IntegrityViolation);
        }
        if let Some(atoms) = atoms {
            self.atoms.restore_atoms(&atoms).map_err(SnapshotError::Atom)?;
        }
        let object_ids = match staged_objects {
            Some(staged) => restore_objects(self, staged)?,
            None => Vec::new(),
        };
        if let Some(staged) = staged_cells {
            restore_cells(self, staged, &object_ids)?;
        }
        if let Some((authorities, staged)) = staged_functions {
            restore_functions(self, authorities, staged, &object_ids)?;
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

/// One staged (not yet resolved) binding cell.
struct StagedCell {
    value: Option<StagedValue>,
    forward: Option<usize>,
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
        encode_object_record_payload(&mut payload, record).map_err(|what| {
            SnapshotError::Unsupported {
                index: id.index(),
                what,
            }
        })?;
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
        staged.push(decode_object_record_content(&mut reader)?);
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
) -> Result<Vec<ObjectId>, SnapshotError> {
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
        let record = resolve_object_record(runtime, staged, &ids, &[])?;
        let object = runtime
            .objects
            .get_mut(id)
            .ok_or(SnapshotError::IntegrityViolation)?;
        *object = HeapObject::ordinary(record);
    }
    Ok(ids)
}

/// Encodes every binding cell into the cells-section payload: per cell a
/// value tag (uninitialized or a stored value) and an optional forwarding
/// target.
fn encode_cells(runtime: &Runtime) -> Result<Vec<u8>, SnapshotError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(runtime.cells.len() as u32).to_le_bytes());
    for (id, cell) in runtime.cells.iter() {
        match &cell.value {
            SlotValue::Uninitialized => payload.push(0),
            SlotValue::Value(value) => {
                payload.push(1);
                encode_stored_value(&mut payload, value).map_err(|what| {
                    SnapshotError::Unsupported {
                        index: id.index(),
                        what,
                    }
                })?;
            }
        }
        match cell.forward {
            None => payload.push(0),
            Some(target) => {
                payload.push(1);
                payload.extend_from_slice(&(target.index() as u32).to_le_bytes());
            }
        }
    }
    Ok(payload)
}

/// Decodes the cells-section payload into staged cells (format above).
fn decode_cells(payload: &[u8]) -> Result<Vec<StagedCell>, SnapshotError> {
    let mut reader = codec::Reader::new(payload);
    let count = reader.read_u32()?;
    let mut staged = Vec::new();
    for _ in 0..count {
        let value = match reader.read_u8()? {
            0 => None,
            1 => Some(decode_staged_value(&mut reader)?),
            other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
        };
        let forward = match reader.read_u8()? {
            0 => None,
            1 => Some(reader.read_u32()? as usize),
            other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
        };
        staged.push(StagedCell { value, forward });
    }
    if reader.remaining() != 0 {
        return Err(SnapshotError::Truncated);
    }
    Ok(staged)
}

/// Resolves the staged cells into the cell arena: cells insert in decode
/// order so their restored identities match the encoded indices, values
/// resolve against the restored objects, then forwarding targets patch
/// once every cell exists.
fn restore_cells(
    runtime: &mut Runtime,
    staged: Vec<StagedCell>,
    object_ids: &[ObjectId],
) -> Result<(), SnapshotError> {
    let mut forwards = Vec::new();
    let mut ids = Vec::new();
    for cell in staged {
        if let Some(target) = cell.forward {
            forwards.push((ids.len(), target));
        }
        let value = match cell.value {
            None => SlotValue::Uninitialized,
            Some(value) => SlotValue::Value(resolve_staged_value(runtime, value, object_ids)?),
        };
        let id = runtime
            .cells
            .try_insert(BindingCell { value, forward: None })
            .map_err(|_| SnapshotError::IntegrityViolation)?;
        ids.push(id);
    }
    for (index, forward) in forwards {
        let target = *ids.get(forward).ok_or(SnapshotError::IntegrityViolation)?;
        let cell = runtime
            .cells
            .get_mut(ids[index])
            .ok_or(SnapshotError::IntegrityViolation)?;
        cell.forward = Some(target);
    }
    Ok(())
}

/// Resolves one staged slot value into a heap value, mapping object
/// references through the restored object identities. Function references
/// resolve once the functions section lands (fail closed until then).
fn resolve_staged_value(
    runtime: &mut Runtime,
    value: StagedValue,
    object_ids: &[ObjectId],
) -> Result<StoredValue, SnapshotError> {
    Ok(match value {
        StagedValue::Undefined => StoredValue::Undefined,
        StagedValue::Null => StoredValue::Null,
        StagedValue::Boolean(value) => StoredValue::Boolean(value),
        StagedValue::Number(value) => StoredValue::Number(JsNumber::from_f64(value)),
        StagedValue::BigInt(limbs) => StoredValue::BigInt(std::sync::Arc::new(
            JsBigInt::from_normalized_limbs(limbs),
        )),
        StagedValue::String(units) => {
            let string = JsString::from_code_units(units).map_err(SnapshotError::String)?;
            StoredValue::String(string)
        }
        StagedValue::GlobalSymbol(units) => {
            let description = JsString::from_code_units(units).map_err(SnapshotError::String)?;
            let atom = runtime
                .atoms
                .intern_global_symbol(&description)
                .map_err(SnapshotError::Atom)?;
            StoredValue::Symbol(atom)
        }
        StagedValue::Object(index) => {
            let target = *object_ids.get(index).ok_or(SnapshotError::IntegrityViolation)?;
            StoredValue::Object(target)
        }
        StagedValue::Function(_) => return Err(SnapshotError::IntegrityViolation),
    })
}


/// One staged (not yet resolved) function from the functions section.
struct StagedFunction {
    kind: StagedFunctionKind,
    record: StagedObject,
}

enum StagedFunctionKind {
    Bytecode {
        code: usize,
        template: u32,
        environment: Vec<(u8, u32)>,
        lexical_receiver: Option<StagedValue>,
        lexical_eval_in_function: bool,
        lexical_eval_in_class_field_initializer: bool,
        lexical_new_target: Option<usize>,
        lexical_derived_constructor: Option<usize>,
        lexical_derived_this: Option<u32>,
        has_instance_elements: bool,
        home_object: Option<(u8, u32)>,
    },
    Host {
        slot: u32,
    },
}

/// Encodes every heap function and the distinct installed-code
/// authorities they reference (format above). Direct-eval machinery,
/// engine intrinsics, and non-bytecode implementation kinds fail closed
/// (§8.2).
fn encode_functions(runtime: &Runtime) -> Result<Vec<u8>, SnapshotError> {
    let mut code_payloads: Vec<(usize, Vec<u8>)> = Vec::new();
    for (code_id, code) in runtime.code.iter() {
        let encoded = fusor_bytecode::encode_verified_bytecode(&code.authority).map_err(|error| {
            SnapshotError::Unsupported {
                index: code_id.index(),
                what: match error {
                    _ => "a verified bytecode authority",
                },
            }
        })?;
        code_payloads.push((code_id.index(), encoded));
    }
    let code_ordinals: std::collections::HashMap<usize, u32> = code_payloads
        .iter()
        .enumerate()
        .map(|(ordinal, (index, _))| (*index, ordinal as u32))
        .collect();
    let mut payload = Vec::new();
    payload.extend_from_slice(&(code_payloads.len() as u32).to_le_bytes());
    for (_, encoded) in &code_payloads {
        payload.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        payload.extend_from_slice(encoded);
    }
    payload.extend_from_slice(&(runtime.functions.len() as u32).to_le_bytes());
    for (function_id, function) in runtime.functions.iter() {
        match &function.implementation {
            FunctionImplementation::Bytecode(bytecode) => {
                payload.push(0);
                let ordinal = *code_ordinals
                    .get(&bytecode.code.index())
                    .ok_or(SnapshotError::IntegrityViolation)?;
                payload.extend_from_slice(&ordinal.to_le_bytes());
                payload.extend_from_slice(&bytecode.template.get().to_le_bytes());
                payload.extend_from_slice(&(bytecode.environment.len() as u32).to_le_bytes());
                for binding in &bytecode.environment {
                    match binding {
                        EnvironmentBinding::Captured(cell) => {
                            payload.push(0);
                            payload.extend_from_slice(&(cell.index() as u32).to_le_bytes());
                        }
                        EnvironmentBinding::RealmGlobal(binding) => {
                            payload.push(1);
                            payload.extend_from_slice(&(binding.index() as u32).to_le_bytes());
                        }
                    }
                }
                if !bytecode.environment_eval_shadows.is_empty()
                    || bytecode.eval_environment.is_some()
                {
                    return Err(SnapshotError::Unsupported {
                        index: function_id.index(),
                        what: "a direct-eval environment",
                    });
                }
                payload.push(0);
                payload.push(0);
                match &bytecode.lexical_receiver {
                    Some(value) => {
                        payload.push(1);
                        encode_stored_value(&mut payload, value).map_err(|what| {
                            SnapshotError::Unsupported {
                                index: function_id.index(),
                                what,
                            }
                        })?;
                    }
                    None => payload.push(0),
                }
                payload.push(u8::from(bytecode.lexical_eval_in_function));
                payload.push(u8::from(bytecode.lexical_eval_in_class_field_initializer));
                write_function_ref(&mut payload, bytecode.lexical_new_target);
                write_function_ref(&mut payload, bytecode.lexical_derived_constructor);
                match bytecode.lexical_derived_this {
                    Some(cell) => {
                        payload.push(1);
                        payload.extend_from_slice(&(cell.index() as u32).to_le_bytes());
                    }
                    None => payload.push(0),
                }
                payload.push(u8::from(bytecode.has_instance_elements));
                match bytecode.home_object {
                    None => payload.push(0),
                    Some(HeapReference::Object(object)) => {
                        payload.push(1);
                        payload.extend_from_slice(&(object.index() as u32).to_le_bytes());
                    }
                    Some(HeapReference::Function(function)) => {
                        payload.push(2);
                        payload.extend_from_slice(&(function.index() as u32).to_le_bytes());
                    }
                }
                encode_object_record_payload(&mut payload, &function.object)
                    .map_err(|what| SnapshotError::Unsupported {
                        index: function_id.index(),
                        what,
                    })?;
            }
            FunctionImplementation::Native(native) => match native.kind {
                crate::runtime::NativeFunctionKind::Host(slot) => {
                    payload.push(1);
                    payload.extend_from_slice(&(slot.index() as u32).to_le_bytes());
                    encode_object_record_payload(&mut payload, &function.object).map_err(|what| {
                        SnapshotError::Unsupported {
                            index: function_id.index(),
                            what,
                        }
                    })?;
                }
                _ => {
                    return Err(SnapshotError::Unsupported {
                        index: function_id.index(),
                        what: "an engine intrinsic function",
                    });
                }
            },
            _ => {
                return Err(SnapshotError::Unsupported {
                    index: function_id.index(),
                    what: "a non-bytecode function kind",
                });
            }
        }
    }
    Ok(payload)
}

fn write_function_ref(buffer: &mut Vec<u8>, target: Option<FunctionId>) {
    match target {
        Some(function) => {
            buffer.push(1);
            buffer.extend_from_slice(&(function.index() as u32).to_le_bytes());
        }
        None => buffer.push(0),
    }
}

/// Encodes one object record's content (prototype, flags, shape, slots)
/// shared by the objects and functions sections; returns the unsupported
/// content name on failure.
fn encode_object_record_payload(
    buffer: &mut Vec<u8>,
    record: &ObjectRecord,
) -> Result<(), &'static str> {
    match record.prototype() {
        None => buffer.push(0),
        Some(HeapReference::Object(target)) => {
            buffer.push(1);
            buffer.extend_from_slice(&(target.index() as u32).to_le_bytes());
        }
        Some(HeapReference::Function(_)) => {
            return Err("a function prototype");
        }
    }
    buffer.push(u8::from(record.is_extensible()));
    buffer.push(u8::from(record.is_html_dda()));
    let shape = record.shape();
    buffer.extend_from_slice(&(shape.len() as u32).to_le_bytes());
    for property in shape.iter() {
        encode_property_key(buffer, property.key());
        encode_layout(buffer, property.layout());
    }
    for slot in record.slots() {
        match slot {
            PropertySlot::Data(value) => {
                buffer.push(0);
                encode_stored_value(buffer, value)?;
            }
            PropertySlot::Accessor { .. } => {
                return Err("an accessor property slot");
            }
        }
    }
    Ok(())
}

/// Decodes one object record's content (the shared sub-format).
fn decode_object_record_content(
    reader: &mut codec::Reader<'_>,
) -> Result<StagedObject, SnapshotError> {
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
        slots.push(decode_staged_value(reader)?);
    }
    Ok(StagedObject {
        prototype,
        extensible,
        is_html_dda,
        shape,
        slots,
    })
}

/// Decodes the functions-section payload into the distinct authorities
/// and the staged functions.
fn decode_functions(
    payload: &[u8],
) -> Result<(Vec<Arc<VerifiedBytecode>>, Vec<StagedFunction>), SnapshotError> {
    let mut reader = codec::Reader::new(payload);
    let code_count = reader.read_u32()?;
    let mut authorities = Vec::new();
    for _ in 0..code_count {
        let length = reader.read_u32()? as usize;
        let encoded = reader.read_bytes(length)?;
        let authority = fusor_bytecode::decode_verified_bytecode(encoded)
            .map_err(|error| SnapshotError::Bytecode(error.to_string()))?;
        authorities.push(Arc::new(authority));
    }
    let function_count = reader.read_u32()?;
    let mut functions = Vec::new();
    for _ in 0..function_count {
        let kind = reader.read_u8()?;
        match kind {
            0 => {
                let code = reader.read_u32()? as usize;
                let template = reader.read_u32()?;
                let environment_count = reader.read_u32()?;
                let mut environment = Vec::new();
                for _ in 0..environment_count {
                    let tag = reader.read_u8()?;
                    let index = reader.read_u32()?;
                    match tag {
                        0 | 1 => environment.push((tag, index)),
                        other => {
                            return Err(SnapshotError::FormatMismatch { found: u32::from(other) });
                        }
                    }
                }
                let eval_shadows = reader.read_u8()?;
                let eval_environment = reader.read_u8()?;
                if eval_shadows != 0 || eval_environment != 0 {
                    return Err(SnapshotError::IntegrityViolation);
                }
                let lexical_receiver = match reader.read_u8()? {
                    0 => None,
                    1 => Some(decode_staged_value(&mut reader)?),
                    other => {
                        return Err(SnapshotError::FormatMismatch { found: u32::from(other) });
                    }
                };
                let lexical_eval_in_function = reader.read_u8()? != 0;
                let lexical_eval_in_class_field_initializer = reader.read_u8()? != 0;
                let lexical_new_target = read_function_ref(&mut reader)?;
                let lexical_derived_constructor = read_function_ref(&mut reader)?;
                let lexical_derived_this = match reader.read_u8()? {
                    0 => None,
                    1 => Some(reader.read_u32()?),
                    other => {
                        return Err(SnapshotError::FormatMismatch { found: u32::from(other) });
                    }
                };
                let has_instance_elements = reader.read_u8()? != 0;
                let home_object = match reader.read_u8()? {
                    0 => None,
                    1 => Some((1, reader.read_u32()?)),
                    2 => Some((2, reader.read_u32()?)),
                    other => {
                        return Err(SnapshotError::FormatMismatch { found: u32::from(other) });
                    }
                };
                let record = decode_object_record_content(&mut reader)?;
                functions.push(StagedFunction {
                    kind: StagedFunctionKind::Bytecode {
                        code,
                        template,
                        environment,
                        lexical_receiver,
                        lexical_eval_in_function,
                        lexical_eval_in_class_field_initializer,
                        lexical_new_target,
                        lexical_derived_constructor,
                        lexical_derived_this,
                        has_instance_elements,
                        home_object,
                    },
                    record,
                });
            }
            1 => {
                let slot = reader.read_u32()?;
                let record = decode_object_record_content(&mut reader)?;
                functions.push(StagedFunction {
                    kind: StagedFunctionKind::Host { slot },
                    record,
                });
            }
            other => return Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
        }
    }
    if reader.remaining() != 0 {
        return Err(SnapshotError::Truncated);
    }
    Ok((authorities, functions))
}

fn read_function_ref(reader: &mut codec::Reader<'_>) -> Result<Option<usize>, SnapshotError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.read_u32()? as usize)),
        other => Err(SnapshotError::FormatMismatch { found: u32::from(other) }),
    }
}

/// Resolves one staged object record against the restored object and
/// function identities (the shared record content used by both sections).
fn resolve_object_record(
    runtime: &mut Runtime,
    staged: StagedObject,
    object_ids: &[ObjectId],
    function_ids: &[FunctionId],
) -> Result<ObjectRecord, SnapshotError> {
    let mut shape_properties = Vec::new();
    for (key, layout) in staged.shape {
        let key = match key {
            StagedKey::Index(index) => {
                let index = ArrayIndex::new(index).ok_or(SnapshotError::IntegrityViolation)?;
                PropertyKey::from_index(index)
            }
            StagedKey::Atom(kind, units) => {
                let description = JsString::from_code_units(units).map_err(SnapshotError::String)?;
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
        let value = match value {
            StagedValue::Function(index) => {
                let target = *function_ids
                    .get(index)
                    .ok_or(SnapshotError::IntegrityViolation)?;
                StoredValue::Function(target)
            }
            other => resolve_staged_value(runtime, other, object_ids)?,
        };
        slots.push(PropertySlot::Data(value));
    }
    let prototype = match staged.prototype {
        None => None,
        Some((1, index)) => {
            let target = *object_ids.get(index).ok_or(SnapshotError::IntegrityViolation)?;
            Some(HeapReference::Object(target))
        }
        Some((2, index)) => {
            let target = *function_ids
                .get(index)
                .ok_or(SnapshotError::IntegrityViolation)?;
            Some(HeapReference::Function(target))
        }
        Some(_) => return Err(SnapshotError::IntegrityViolation),
    };
    Ok(ObjectRecord::from_parts(
        prototype,
        staged.extensible,
        staged.is_html_dda,
        std::sync::Arc::new(shape_properties),
        Some(runtime.shape_interner.clone()),
        slots,
    ))
}

struct PendingFunction {
    record: StagedObject,
    home_object: Option<(u8, u32)>,
    lexical_new_target: Option<usize>,
    lexical_derived_constructor: Option<usize>,
}

/// Restores the staged functions: authorities re-install (load-time
/// verification, §8.3), functions insert in decode order so their
/// identities match the encoded indices, then record contents and
/// cross-references patch once every id exists.
fn restore_functions(
    runtime: &mut Runtime,
    authorities: Vec<Arc<VerifiedBytecode>>,
    staged: Vec<StagedFunction>,
    object_ids: &[ObjectId],
) -> Result<(), SnapshotError> {
    let mut code_ids = Vec::new();
    for authority in authorities {
        let templates = runtime
            .stage_templates(&authority)
            .map_err(|_| SnapshotError::IntegrityViolation)?;
        let id = runtime
            .code
            .try_insert(InstalledCode {
                authority,
                realm: RealmId::ZERO,
                templates,
                live_functions: 0,
            })
            .map_err(|_| SnapshotError::IntegrityViolation)?;
        code_ids.push(id);
    }
    let mut function_ids = Vec::new();
    let mut pending = Vec::new();
    for function in staged {
        let implementation = match function.kind {
            StagedFunctionKind::Bytecode {
                code,
                template,
                environment,
                lexical_receiver,
                lexical_eval_in_function,
                lexical_eval_in_class_field_initializer,
                lexical_new_target,
                lexical_derived_constructor,
                lexical_derived_this,
                has_instance_elements,
                home_object,
            } => {
                let code = *code_ids.get(code).ok_or(SnapshotError::IntegrityViolation)?;
                let mut bindings = Vec::new();
                for (tag, index) in environment {
                    match tag {
                        0 => bindings.push(EnvironmentBinding::Captured(
                            runtime.cells.id_from_index(index as usize),
                        )),
                        1 => bindings.push(EnvironmentBinding::RealmGlobal(
                            runtime.global_bindings.id_from_index(index as usize),
                        )),
                        _ => return Err(SnapshotError::IntegrityViolation),
                    }
                }
                let lexical_receiver = match lexical_receiver {
                    Some(value) => Some(resolve_staged_value(runtime, value, object_ids)?),
                    None => None,
                };
                pending.push(PendingFunction {
                    record: function.record,
                    home_object,
                    lexical_new_target,
                    lexical_derived_constructor,
                });
                FunctionImplementation::Bytecode(BytecodeFunction {
                    code,
                    template: fusor_bytecode::FunctionTemplateId::new(template),
                    environment: bindings,
                    environment_eval_shadows: Vec::new(),
                    eval_environment: None,
                    lexical_receiver,
                    lexical_eval_in_function,
                    lexical_eval_in_class_field_initializer,
                    lexical_new_target: None,
                    lexical_derived_constructor: None,
                    lexical_derived_this: lexical_derived_this
                        .map(|index| runtime.cells.id_from_index(index as usize)),
                    has_instance_elements,
                    home_object: None,
                })
            }
            StagedFunctionKind::Host { slot } => {
                pending.push(PendingFunction {
                    record: function.record,
                    home_object: None,
                    lexical_new_target: None,
                    lexical_derived_constructor: None,
                });
                FunctionImplementation::Native(crate::runtime::NativeFunction {
                    realm: RealmId::ZERO,
                    kind: crate::runtime::NativeFunctionKind::Host(
                        crate::HostFunctionId::new(slot as usize),
                    ),
                })
            }
        };
        let id = runtime
            .insert_heap_function(HeapFunction {
                implementation,
                object: ObjectRecord::from_parts(
                    None,
                    true,
                    false,
                    std::sync::Arc::new(Vec::new()),
                    Some(runtime.shape_interner.clone()),
                    Vec::new(),
                ),
                public_roots: 0,
            })
            .map_err(|_| SnapshotError::IntegrityViolation)?;
        function_ids.push(id);
    }
    for (function_id, pending) in function_ids.iter().copied().zip(pending) {
        let record = resolve_object_record(runtime, pending.record, object_ids, &function_ids)?;
        let lexical_new_target = match pending.lexical_new_target {
            Some(index) => Some(
                *function_ids
                    .get(index as usize)
                    .ok_or(SnapshotError::IntegrityViolation)?,
            ),
            None => None,
        };
        let lexical_derived_constructor = match pending.lexical_derived_constructor {
            Some(index) => Some(
                *function_ids
                    .get(index as usize)
                    .ok_or(SnapshotError::IntegrityViolation)?,
            ),
            None => None,
        };
        let home_object = match pending.home_object {
            None => None,
            Some((1, index)) => {
                let target = *object_ids
                    .get(index as usize)
                    .ok_or(SnapshotError::IntegrityViolation)?;
                Some(HeapReference::Object(target))
            }
            Some((2, index)) => {
                let target = *function_ids
                    .get(index as usize)
                    .ok_or(SnapshotError::IntegrityViolation)?;
                Some(HeapReference::Function(target))
            }
            Some(_) => return Err(SnapshotError::IntegrityViolation),
        };
        let function = runtime
            .functions
            .get_mut(function_id)
            .ok_or(SnapshotError::IntegrityViolation)?;
        function.object = record;
        let FunctionImplementation::Bytecode(bytecode) = &mut function.implementation else {
            continue;
        };
        bytecode.home_object = home_object;
        bytecode.lexical_new_target = lexical_new_target;
        bytecode.lexical_derived_constructor = lexical_derived_constructor;
    }
    // live_functions accounting: count bytecode functions per code.
    let live_counts: Vec<(crate::ids::InstalledCodeId, u64)> = {
        let mut counts = std::collections::HashMap::new();
        for function in runtime.functions.iter() {
            let FunctionImplementation::Bytecode(bytecode) = &function.1.implementation else {
                continue;
            };
            *counts.entry(bytecode.code).or_insert(0) += 1;
        }
        counts.into_iter().collect()
    };
    for (code, live) in live_counts {
        if let Some(code) = runtime.code.get_mut(code) {
            code.live_functions = live;
        }
    }
    Ok(())
}
