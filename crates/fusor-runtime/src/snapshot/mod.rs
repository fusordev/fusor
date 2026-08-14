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
//! Format (stamp 2):
//!
//! ```text
//! magic     8 bytes  "FUSORSNP"
//! stamp     u32 LE   SNAPSHOT_FORMAT_STAMP
//! sections  u32 LE   section count
//! per section: tag u8, payload byte length u64 LE, payload, CRC-32 u32 LE
//!   tag 1 = atoms: count u64 LE, then per atom:
//!     kind u8 (0 = String, 1 = GlobalSymbol),
//!     description: unit count u32 LE, UTF-16 code units u16 LE
//!   tag 2 = objects (user heap; the realm prefix is omitted, §8.2):
//!     count u64 LE, then per object:
//!       index u32 LE (arena index; records ascend, holes are omitted and
//!         become reusable vacant slots on restore)
//!       kind u8:
//!         0 Ordinary
//!         1 Array: length u32 LE, storage u8 (0 dense, 1 sparse),
//!           dense: element count u32 LE + per element
//!             present u8 + [the value encoding]
//!         2 Error: stack string (unit count u32 LE + UTF-16 u16 LE)
//!         3 Date: value f64 LE
//!         4 BoxedPrimitive: tag u8 (0 Boolean + u8, 1 Number + f64 LE,
//!           2 BigInt + limb count u32 + u32 limbs LE, 3 String units,
//!           4 GlobalSymbol units)
//!         5 RegExp: source units + flags units (the matcher recompiles)
//!         6 Map: live entry count u32 LE + per entry
//!           key [value encoding] + value [value encoding]
//!         7 Set: live entry count u32 LE + per entry key [value encoding]
//!         8 RawJson
//!         9 Arguments: map count u32 LE + per entry
//!           u8 (0 unmapped, 1 cell index u32 LE)
//!       prototype u8 (0 = none, 1 = object, 2 = function) + index u32 LE
//!       extensible u8, is_html_dda u8
//!       shape: property count u32 LE, then per property:
//!         key u8 (0 = array index + u32 LE,
//!                 1 = atom (kind u8 + description units),
//!                 2 = predefined atom ordinal u32 LE — includes the
//!                     well-known symbols, which are predefined here)
//!         layout u8 (0 = data, 1 = accessor) + bits u8
//!       slots: aligned with the shape, per slot:
//!         tag u8 (0 = data) + the value encoding:
//!           0 Undefined, 1 Null, 2 Boolean + u8, 3 Number + f64 LE,
//!           4 BigInt + limb count u32 + u32 limbs LE,
//!           5 String units, 6 GlobalSymbol units, 7 Object + index u32,
//!           8 Function + index u32
//!   tag 3 = binding cells:
//!     count u32 LE, then per cell:
//!       index u32 LE
//!       value u8 (0 = uninitialized, 1 = value) + [the value encoding],
//!       forward u8 (0 = none, 1 = target cell index u32 LE)
//!   tag 4 = functions:
//!     code count u32 LE, then per code: index u32 LE,
//!       realm u8 (0 = none, 1 = realm index u32 LE),
//!       byte length u32 LE + the verified-bytecode payload (FUSRBYTE codec)
//!     env count u32 LE, then per eval-variable environment node
//!     (shared DAG, parent ordinals precede children):
//!       kind u8 (0 Function, 1 ParameterInitializer,
//!                2 ParameterBoundary, 3 FunctionBody)
//!       parent u8 (0 = none, 1 = env ordinal u32 LE)
//!       binding count u32 LE, then per binding:
//!         name (unit count u32 LE + UTF-16 u16 LE), cell index u32 LE,
//!         deleted u8
//!     function count u32 LE, then per function:
//!       index u32 LE
//!       kind u8 (0 = JS bytecode, 1 = host)
//!       kind 0: code ordinal u32 LE, template u32 LE,
//!         environment count u32 LE + per entry:
//!           tag u8 (0 = Captured cell index u32 LE,
//!                   1 = RealmGlobal binding index u32 LE)
//!         eval environment u8 (0 = none, 1 = env ordinal u32 LE)
//!         eval shadows count u32 LE, then per shadow:
//!           present u8, head env ordinal u32 LE,
//!           boundary u8 (0 = none, 1 = env ordinal u32 LE)
//!         lexical receiver u8 + [the value encoding],
//!         lexical eval flags u8 u8,
//!         lexical new target u8 + function index u32 LE,
//!         lexical derived constructor u8 + function index u32 LE,
//!         lexical derived this u8 + cell index u32 LE,
//!         has_instance_elements u8,
//!         home object u8 (0 none, 1 object index u32 LE,
//!                         2 function index u32 LE)
//!         then the object record (same sub-format as tag 2)
//!       kind 1: realm u8 (0 = none, 1 = realm index u32 LE),
//!         host slot index u32 LE + the object record
//!   tag 5 = realms: count u32 LE, then per realm (arena order):
//!       object_prototype u32 LE, global_object u32 LE,
//!       math_random_state u64 LE,
//!       objects segment start/end u32 LE u32 LE,
//!       functions segment start/end u32 LE u32 LE,
//!       the realm's global-object record (the tag-2 sub-format: prototype,
//!       flags, shape, slots) — user mutations on `globalThis` restore on
//!       top of the replayed intrinsic graph
//!   tag 6 = global bindings: count u32 LE, then per binding:
//!       index u32 LE, realm index u32 LE,
//!       atom content (kind u8 + unit count u32 LE + UTF-16 u16 LE),
//!       state u8 (0 = Unresolved, 1 = Object,
//!                 2 = Lexical { cell index u32 LE, mutable u8 })
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
//! The realm section serializes the realm *table* only. The intrinsic
//! graph on `globalThis` is rebuilt deterministically by replaying
//! `create_realm` on restore (§8.2): each realm's records must tile a
//! first-generation prefix of the objects and functions arenas (recorded
//! as per-realm segments), the prefix is omitted from the heap sections,
//! and restore validates the replay against the recorded watermarks and
//! global-object identities. Arena records in the heap sections are
//! gap-encoded by index, so reclaimed slots keep every surviving
//! record's identity stable.
//!
//! Heap content the current format does not cover yet fails closed with
//! [`SnapshotError::Unsupported`] (intermediate slices, §8.2): promises,
//! proxies, array buffers and typed views, weak collections, Intl and
//! Temporal objects, iterators, identity-symbol keys, accessor slots,
//! module namespaces, and module registries. Snapshotting drains the
//! job queue to quiescence and collects garbage first, so unreachable
//! records — exhausted iterators, collected cycles — never enter the
//! blob.

mod codec;

use std::sync::Arc;
use std::{error::Error, fmt};

use fusor_bytecode::VerifiedBytecode;

use crate::{
    ArrayIndex, AtomError, AtomKind, JsBigInt, JsNumber, JsString, JsStringError, PredefinedAtom,
    PropertyKey, PropertyLayout, Realm, Runtime,
    ids::{FunctionId, ObjectId, RealmId},
    object::{
        ArgumentsState, ArrayState, ArrayStorage, BoxedPrimitive, DateState, ErrorState,
        HeapObject, HeapObjectKind, MapEntry, MapState, ObjectRecord, PropertySlot, RegExpState,
        SetState, ShapeProperty,
    },
    runtime::{
        BindingCell, BytecodeFunction, EnvironmentBinding, EvalBindingShadow,
        EvalVariableEnvironment, EvalVariableEnvironmentKind, FunctionImplementation, HeapFunction,
        InstalledCode, RealmGlobalBinding, RealmGlobalBindingState, SharedEvalVariableEnvironment,
    },
    value::{HeapReference, SlotValue, StoredValue},
};

/// The snapshot magic: every blob starts with these bytes.
pub const SNAPSHOT_MAGIC: [u8; 8] = *b"FUSORSNP";

/// The current snapshot format stamp (§8.1: no version compatibility —
/// any other stamp is rejected).
pub const SNAPSHOT_FORMAT_STAMP: u32 = 2;

/// The atoms section tag.
const SECTION_ATOMS: u8 = 1;

/// The objects section tag.
const SECTION_OBJECTS: u8 = 2;

/// The binding-cells section tag.
const SECTION_CELLS: u8 = 3;

/// The functions section tag.
const SECTION_FUNCTIONS: u8 = 4;

/// The realm-table section tag.
const SECTION_REALMS: u8 = 5;

/// The global-bindings section tag.
const SECTION_BINDINGS: u8 = 6;

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
    /// A restored `RegExp` pattern failed to recompile from its
    /// recorded source and flags (§8.2).
    RegExp(String),
    /// The job-queue drain before snapshotting failed (§8.2: pending
    /// microtasks settle before the heap freezes).
    Jobs(String),
    /// The pre-snapshot garbage collection failed (§8.2).
    Gc(String),
    /// The deterministic realm replay failed to rebuild one realm
    /// (§8.2: `create_realm` is replayed on restore).
    Realm(String),
    /// The rebuilt intrinsic graph diverged from the recorded realm
    /// segments — the blob was produced by different realm-build code
    /// or is structurally corrupt (load-time validation, §8.3).
    RealmMismatch {
        /// The realm-record field that did not match.
        what: &'static str,
        /// The value recorded in the blob.
        expected: usize,
        /// The value the replay produced.
        found: usize,
    },
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
            Self::RegExp(source) => {
                write!(formatter, "snapshot regexp recompilation failed: {source}")
            }
            Self::Jobs(source) => {
                write!(formatter, "snapshot job-queue drain failed: {source}")
            }
            Self::Gc(source) => {
                write!(formatter, "snapshot garbage collection failed: {source}")
            }
            Self::Realm(source) => {
                write!(formatter, "snapshot realm reconstruction failed: {source}")
            }
            Self::RealmMismatch {
                what,
                expected,
                found,
            } => write!(
                formatter,
                "snapshot realm replay diverged on {what}: recorded {expected}, rebuilt {found}"
            ),
        }
    }
}

impl Error for SnapshotError {}

/// One realm-table record (§8.2): the intrinsic graph itself is not
/// serialized — restore replays `create_realm` and validates the result
/// against these identities and segment watermarks.
#[derive(Clone, Copy, Debug)]
struct RealmRecord {
    object_prototype: usize,
    global_object: usize,
    math_random_state: u64,
    objects: (usize, usize),
    functions: (usize, usize),
}

/// Validates the realm-prefix precondition and collects the realm-table
/// records (§8.2): every realm's intrinsic graph must tile a contiguous
/// first-generation prefix of the objects and functions arenas, and the
/// arena itself must be churn-free. Anything else — user content before
/// or between realm creations, freed or reused realm records — fails
/// closed because the deterministic replay could not reproduce it.
fn validate_realms(runtime: &Runtime) -> Result<Vec<RealmRecord>, SnapshotError> {
    if !runtime.realms.is_dense_pristine() {
        return Err(SnapshotError::Unsupported {
            index: 0,
            what: "a freed realm record",
        });
    }
    let mut records = Vec::new();
    let mut objects_end = 0usize;
    let mut functions_end = 0usize;
    for (id, state) in runtime.realms.iter() {
        let segment = state.snapshot_segment;
        if segment.objects.0 != objects_end || segment.functions.0 != functions_end {
            return Err(SnapshotError::Unsupported {
                index: id.index(),
                what: if objects_end == 0 && functions_end == 0 {
                    "user heap content before the first realm"
                } else {
                    "user heap content between realm creations"
                },
            });
        }
        objects_end = segment.objects.1;
        functions_end = segment.functions.1;
        if !state.module_registry.is_empty() {
            return Err(SnapshotError::Unsupported {
                index: id.index(),
                what: "a module registry",
            });
        }
        records.push(RealmRecord {
            object_prototype: state.object_prototype.index(),
            global_object: state.global_object.index(),
            math_random_state: state.math_random_state,
            objects: segment.objects,
            functions: segment.functions,
        });
    }
    if !runtime.objects.is_pristine_prefix(objects_end)
        || !runtime.functions.is_pristine_prefix(functions_end)
    {
        return Err(SnapshotError::Unsupported {
            index: 0,
            what: "a freed or reused realm record",
        });
    }
    Ok(records)
}

/// Validates the runtime-level async state (§8.2): suspended generators
/// and in-flight async machinery live outside the arenas, so a snapshot
/// of a runtime with any of it would silently lose execution state.
/// Everything here fails closed until its serializer slice lands.
fn validate_runtime_state(runtime: &Runtime) -> Result<(), SnapshotError> {
    let unsupported = |what: &'static str| SnapshotError::Unsupported { index: 0, what };
    if !runtime.generator_states.is_empty() {
        return Err(unsupported("a suspended generator"));
    }
    if !runtime.async_function_states.is_empty() {
        return Err(unsupported("a suspended async function"));
    }
    if !runtime.async_generator_states.is_empty() {
        return Err(unsupported("a suspended async generator"));
    }
    if !runtime.array_from_async_states.is_empty() {
        return Err(unsupported("an in-flight Array.fromAsync"));
    }
    if !runtime.pending_dynamic_imports.is_empty() {
        return Err(unsupported("an in-flight dynamic import"));
    }
    if !runtime.deferred_import_waiters.is_empty() {
        return Err(unsupported("an in-flight import.defer"));
    }
    if !runtime.promise_jobs.is_empty() {
        return Err(unsupported("a pending promise job"));
    }
    if !runtime.atomics_waiters.is_empty() {
        return Err(unsupported("a pending Atomics.waitAsync"));
    }
    Ok(())
}

impl Runtime {
    /// Serializes the heap into one snapshot blob (§8.1, §8.3).
    ///
    /// Snapshotting first waits for the job queue to quiesce (pending
    /// microtasks settle before the heap freezes), then runs a full
    /// mark-and-sweep collection so unreachable records — exhausted
    /// iterators, collected cycles — drop out of the blob (§8.2).
    ///
    /// The realm table is serialized; the intrinsic graph on `globalThis`
    /// is not — restore replays `create_realm` deterministically and
    /// validates the replay against the recorded segments (§8.2). The
    /// current format covers the dynamic atoms table and the user heap:
    /// ordinary objects, arrays, errors, dates, boxed primitives,
    /// regexps, maps, sets, raw JSON, arguments, functions, and binding
    /// cells. Heap content beyond that — promises, proxies, array
    /// buffers and typed views, weak collections, Intl/Temporal objects,
    /// iterators, module registries — fails closed with
    /// [`SnapshotError::Unsupported`] until its serializer slice lands
    /// (§8.2).
    ///
    /// # Errors
    ///
    /// Returns a typed [`SnapshotError`] for unsupported heap content or
    /// a failed job-queue drain or collection. This function never
    /// panics.
    pub fn snapshot(&mut self) -> Result<Vec<u8>, SnapshotError> {
        crate::vm::promise::drain_host_jobs_with_limits(
            self,
            None,
            crate::ExecutionLimits::default(),
        )
        .map_err(|error| SnapshotError::Jobs(error.to_string()))?;
        self.collect_cycles()
            .map_err(|error| SnapshotError::Gc(error.to_string()))?;
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
        let realm_records = validate_realms(self)?;
        validate_runtime_state(self)?;
        let objects_watermark = realm_records.last().map_or(0, |record| record.objects.1);
        let functions_watermark = realm_records.last().map_or(0, |record| record.functions.1);
        if self.objects.len() > objects_watermark {
            let payload = encode_objects(self, objects_watermark)?;
            codec::write_section(&mut buffer, SECTION_OBJECTS, &payload);
            sections += 1;
        }
        if self.cells.len() > 0 {
            let payload = encode_cells(self)?;
            codec::write_section(&mut buffer, SECTION_CELLS, &payload);
            sections += 1;
        }
        if self.code.len() > 0 || self.functions.len() > functions_watermark {
            let payload = encode_functions(self, functions_watermark)?;
            codec::write_section(&mut buffer, SECTION_FUNCTIONS, &payload);
            sections += 1;
        }
        if !realm_records.is_empty() {
            let payload = encode_realms(self, &realm_records)?;
            codec::write_section(&mut buffer, SECTION_REALMS, &payload);
            sections += 1;
        }
        if self.global_bindings.len() > 0 {
            let payload = encode_bindings(self)?;
            codec::write_section(&mut buffer, SECTION_BINDINGS, &payload);
            sections += 1;
        }
        buffer[count_position..count_position + 4].copy_from_slice(&sections.to_le_bytes());
        Ok(buffer)
    }

    /// Restores one snapshot blob into this runtime (§8.1, §8.3).
    ///
    /// The target must be a fresh runtime: restoration fills the empty
    /// skeleton section by section, validating every frame on load. The
    /// realm table is restored by replaying `create_realm`; no
    /// microtasks drain and no JavaScript runs during restore.
    ///
    /// # Errors
    ///
    /// Returns a typed [`SnapshotError`] for a magic/stamp mismatch, a
    /// truncated or tampered blob, a non-fresh target, or a realm replay
    /// that diverges from the recorded segments. This function never
    /// panics.
    pub fn from_snapshot(&mut self, blob: &[u8]) -> Result<Vec<Realm>, SnapshotError> {
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
        // records resolve in dependency order (realms by replay, then
        // objects, cells, functions, bindings).
        let mut atoms = None;
        let mut staged_objects = None;
        let mut staged_cells = None;
        let mut staged_functions = None;
        let mut staged_realms = None;
        let mut staged_bindings = None;
        for _ in 0..sections {
            let tag = reader.read_u8()?;
            let payload = codec::read_section_payload(&mut reader)?;
            match tag {
                SECTION_ATOMS => atoms = Some(decode_atoms(payload)?),
                SECTION_OBJECTS => staged_objects = Some(decode_objects(payload)?),
                SECTION_CELLS => staged_cells = Some(decode_cells(payload)?),
                SECTION_FUNCTIONS => staged_functions = Some(decode_functions(payload)?),
                SECTION_REALMS => staged_realms = Some(decode_realms(payload)?),
                SECTION_BINDINGS => staged_bindings = Some(decode_bindings(payload)?),
                other => {
                    return Err(SnapshotError::FormatMismatch {
                        found: u32::from(other),
                    });
                }
            }
        }
        if reader.remaining() != 0 {
            return Err(SnapshotError::IntegrityViolation);
        }
        // Phase 1: replay `create_realm` for every realm record (§8.2).
        // The rebuilt intrinsic graph must reproduce the recorded
        // identities and arena watermarks exactly.
        let (realm_records, staged_globals) = staged_realms.unwrap_or_default();
        let realm_count = realm_records.len();
        let mut realms = Vec::new();
        for (ordinal, record) in realm_records.iter().enumerate() {
            let realm = self
                .create_realm()
                .map_err(|error| SnapshotError::Realm(error.to_string()))?;
            if realm.id().index() != ordinal {
                return Err(SnapshotError::IntegrityViolation);
            }
            if self.objects.len() != record.objects.1 {
                return Err(SnapshotError::RealmMismatch {
                    what: "objects watermark",
                    expected: record.objects.1,
                    found: self.objects.len(),
                });
            }
            if self.functions.len() != record.functions.1 {
                return Err(SnapshotError::RealmMismatch {
                    what: "functions watermark",
                    expected: record.functions.1,
                    found: self.functions.len(),
                });
            }
            let state = self
                .realms
                .get(realm.id())
                .ok_or(SnapshotError::IntegrityViolation)?;
            if state.global_object.index() != record.global_object
                || state.object_prototype.index() != record.object_prototype
            {
                return Err(SnapshotError::RealmMismatch {
                    what: "realm intrinsic identity",
                    expected: record.global_object,
                    found: state.global_object.index(),
                });
            }
            realms.push(realm);
        }
        let objects_watermark = realm_records.last().map_or(0, |record| record.objects.1);
        let functions_watermark = realm_records.last().map_or(0, |record| record.functions.1);
        // Phase 2: place every heap record at its recorded arena index
        // (objects and functions first so cross-references can resolve),
        // then patch the record contents in dependency order.
        if let Some(atoms) = atoms {
            self.atoms
                .restore_atoms(&atoms)
                .map_err(SnapshotError::Atom)?;
        }
        let (object_ids, pending_objects) = match staged_objects {
            Some(staged) => restore_objects(self, staged, objects_watermark)?,
            None => (
                (0..objects_watermark)
                    .map(|index| self.objects.id_from_index(index))
                    .collect(),
                Vec::new(),
            ),
        };
        let (function_ids, pending_functions) = match staged_functions {
            Some((authorities, environments, staged)) => restore_functions(
                self,
                authorities,
                environments,
                staged,
                functions_watermark,
                &realms,
            )?,
            None => (
                (0..functions_watermark)
                    .map(|index| self.functions.id_from_index(index))
                    .collect(),
                Vec::new(),
            ),
        };
        if let Some(staged) = staged_cells {
            restore_cells(self, staged, &object_ids, &function_ids)?;
        }
        resolve_object_records(self, pending_objects, &object_ids, &function_ids)?;
        resolve_function_records(self, pending_functions, &object_ids, &function_ids)?;
        if let Some(staged) = staged_bindings {
            restore_bindings(self, staged, realm_count)?;
        }
        // Phase 3: patch realm-local state (the math-random sequence and
        // the global binding maps rebuild from the restored bindings),
        // then lay the serialized global-object records — user mutations
        // on `globalThis` — over the replayed intrinsic graph.
        for ((record, staged_global), realm) in
            realm_records.iter().zip(staged_globals).zip(&realms)
        {
            let realm_id = realm.id();
            {
                let state = self
                    .realms
                    .get_mut(realm_id)
                    .ok_or(SnapshotError::IntegrityViolation)?;
                state.math_random_state = record.math_random_state;
                state.global_bindings.clear();
            }
            let resolved = resolve_object_record(self, staged_global, &object_ids, &function_ids)?;
            let state = self
                .realms
                .get(realm_id)
                .ok_or(SnapshotError::IntegrityViolation)?;
            let global = self
                .objects
                .get_mut(state.global_object)
                .ok_or(SnapshotError::IntegrityViolation)?;
            *global = HeapObject::ordinary(resolved);
        }
        for (id, binding) in self.global_bindings.iter() {
            let realm_id = realms
                .get(binding.realm.index())
                .map(Realm::id)
                .ok_or(SnapshotError::IntegrityViolation)?;
            let state = self
                .realms
                .get_mut(realm_id)
                .ok_or(SnapshotError::IntegrityViolation)?;
            state.global_bindings.insert(binding.name.clone(), id);
        }
        Ok(realms)
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
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
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
    Predefined(u32),
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

/// One staged object waiting for its arena slot at the recorded index.
struct StagedObject {
    index: usize,
    kind: StagedObjectKind,
    prototype: Option<(u8, usize)>,
    extensible: bool,
    is_html_dda: bool,
    shape: Vec<(StagedKey, PropertyLayout)>,
    slots: Vec<StagedValue>,
}

/// One staged exotic-kind state (the tag-2 kind payloads).
enum StagedObjectKind {
    Ordinary,
    Array {
        length: u32,
        storage: StagedArrayStorage,
    },
    Error {
        stack: Vec<u16>,
    },
    Date {
        value: f64,
    },
    Boxed(StagedBoxed),
    RegExp {
        source: Vec<u16>,
        flags: Vec<u16>,
    },
    Map(Vec<(StagedValue, StagedValue)>),
    Set(Vec<StagedValue>),
    RawJson,
    Arguments(Vec<Option<u32>>),
}

enum StagedArrayStorage {
    Dense(Vec<Option<StagedValue>>),
    Sparse,
}

enum StagedBoxed {
    Boolean(bool),
    Number(f64),
    BigInt(Vec<u32>),
    String(Vec<u16>),
    GlobalSymbol(Vec<u16>),
}

/// One staged (not yet resolved) binding cell.
struct StagedCell {
    index: usize,
    value: Option<StagedValue>,
    forward: Option<usize>,
}

/// Encodes every user object (arena index ≥ the realm watermark) into
/// the objects-section payload (format above); unsupported content fails
/// closed (§8.2). Records are gap-encoded by index so reclaimed slots do
/// not shift surviving identities.
fn encode_objects(runtime: &Runtime, watermark: usize) -> Result<Vec<u8>, SnapshotError> {
    let mut payload = Vec::new();
    let count = runtime
        .objects
        .iter()
        .filter(|(id, _)| id.index() >= watermark)
        .count();
    payload.extend_from_slice(&(count as u64).to_le_bytes());
    for (id, object) in runtime
        .objects
        .iter()
        .filter(|(id, _)| id.index() >= watermark)
    {
        payload.extend_from_slice(&(id.index() as u32).to_le_bytes());
        encode_object_kind(&mut payload, object.kind()).map_err(|what| {
            SnapshotError::Unsupported {
                index: id.index(),
                what,
            }
        })?;
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

/// Encodes one exotic-kind tag and payload (format above); returns the
/// unsupported content name on failure.
fn encode_object_kind(payload: &mut Vec<u8>, kind: &HeapObjectKind) -> Result<(), &'static str> {
    match kind {
        HeapObjectKind::Ordinary => payload.push(0),
        HeapObjectKind::Array(state) => {
            payload.push(1);
            payload.extend_from_slice(&state.length.to_le_bytes());
            match &state.storage {
                ArrayStorage::Dense { elements, .. } => {
                    payload.push(0);
                    payload.extend_from_slice(&(elements.len() as u32).to_le_bytes());
                    for element in elements {
                        match element {
                            None => payload.push(0),
                            Some(value) => {
                                payload.push(1);
                                encode_stored_value(payload, value)?;
                            }
                        }
                    }
                }
                ArrayStorage::Sparse => payload.push(1),
            }
        }
        HeapObjectKind::Error(state) => {
            payload.push(2);
            encode_string_units(payload, state.stack());
        }
        HeapObjectKind::Date(state) => {
            payload.push(3);
            payload.extend_from_slice(&state.value().as_f64().to_le_bytes());
        }
        HeapObjectKind::BoxedPrimitive(value) => {
            payload.push(4);
            match value {
                BoxedPrimitive::Boolean(value) => {
                    payload.push(0);
                    payload.push(u8::from(*value));
                }
                BoxedPrimitive::Number(value) => {
                    payload.push(1);
                    payload.extend_from_slice(&value.as_f64().to_le_bytes());
                }
                BoxedPrimitive::BigInt(value) => {
                    payload.push(2);
                    let limbs = value.limbs();
                    payload.extend_from_slice(&(limbs.len() as u32).to_le_bytes());
                    for limb in limbs {
                        payload.extend_from_slice(&limb.to_le_bytes());
                    }
                }
                BoxedPrimitive::String(value) => {
                    payload.push(3);
                    encode_string_units(payload, value);
                }
                BoxedPrimitive::Symbol(atom) => match atom.kind() {
                    AtomKind::GlobalSymbol => {
                        payload.push(4);
                        encode_atom_content(payload, atom);
                    }
                    _ => return Err("an identity symbol box"),
                },
            }
        }
        HeapObjectKind::RegExp(state) => {
            payload.push(5);
            encode_string_units(payload, state.source());
            encode_string_units(payload, state.flags());
        }
        HeapObjectKind::Map(state) => {
            payload.push(6);
            let live = state.len();
            payload.extend_from_slice(&(live as u32).to_le_bytes());
            for position in 0..state.retained_len() {
                let entry = state.entry(position).ok_or("a map entry index")?;
                if !entry.is_live() {
                    continue;
                }
                encode_stored_value(payload, entry.key())?;
                encode_stored_value(payload, entry.value())?;
            }
        }
        HeapObjectKind::Set(state) => {
            payload.push(7);
            let data = state.map_state();
            let live = data.len();
            payload.extend_from_slice(&(live as u32).to_le_bytes());
            for position in 0..data.retained_len() {
                let entry = data.entry(position).ok_or("a set entry index")?;
                if !entry.is_live() {
                    continue;
                }
                encode_stored_value(payload, entry.key())?;
            }
        }
        HeapObjectKind::RawJson => payload.push(8),
        HeapObjectKind::Arguments(state) => {
            payload.push(9);
            let map = state.parameter_map();
            payload.extend_from_slice(&(map.len() as u32).to_le_bytes());
            for entry in map {
                match entry {
                    None => payload.push(0),
                    Some(cell) => {
                        payload.push(1);
                        payload.extend_from_slice(&(cell.index() as u32).to_le_bytes());
                    }
                }
            }
        }
        HeapObjectKind::Promise(_) => return Err("a promise"),
        HeapObjectKind::Proxy(_) => return Err("a proxy"),
        HeapObjectKind::ArrayBuffer(_) | HeapObjectKind::DataView(_) => {
            return Err("an array buffer view");
        }
        HeapObjectKind::TypedArray(_) => return Err("a typed array"),
        HeapObjectKind::WeakMap(_) | HeapObjectKind::WeakSet(_) | HeapObjectKind::WeakRef(_) => {
            return Err("a weak collection");
        }
        HeapObjectKind::FinalizationRegistry(_) => return Err("a finalization registry"),
        HeapObjectKind::ModuleNamespace(_) => return Err("a module namespace"),
        HeapObjectKind::IntlLocale(_)
        | HeapObjectKind::IntlCollator(_)
        | HeapObjectKind::IntlNumberFormat(_)
        | HeapObjectKind::IntlDateTimeFormat(_)
        | HeapObjectKind::IntlPluralRules(_)
        | HeapObjectKind::IntlRelativeTimeFormat(_)
        | HeapObjectKind::IntlListFormat(_)
        | HeapObjectKind::IntlDisplayNames(_)
        | HeapObjectKind::IntlDurationFormat(_)
        | HeapObjectKind::IntlSegmenter(_)
        | HeapObjectKind::IntlSegments(_)
        | HeapObjectKind::IntlSegmentIterator(_) => return Err("an Intl object"),
        HeapObjectKind::TemporalInstant(_)
        | HeapObjectKind::TemporalDuration(_)
        | HeapObjectKind::TemporalPlainDate(_)
        | HeapObjectKind::TemporalPlainDateTime(_)
        | HeapObjectKind::TemporalPlainTime(_)
        | HeapObjectKind::TemporalPlainMonthDay(_)
        | HeapObjectKind::TemporalPlainYearMonth(_)
        | HeapObjectKind::TemporalZonedDateTime(_) => return Err("a Temporal object"),
        HeapObjectKind::ForInIterator(_)
        | HeapObjectKind::ArrayIterator(_)
        | HeapObjectKind::IteratorWrapper(_)
        | HeapObjectKind::StringIterator(_)
        | HeapObjectKind::RegExpStringIterator(_)
        | HeapObjectKind::MapIterator(_)
        | HeapObjectKind::SetIterator(_) => return Err("an iterator"),
    }
    Ok(())
}

/// Encodes one string's UTF-16 units (unit count + units).
fn encode_string_units(payload: &mut Vec<u8>, value: &JsString) {
    let units: Vec<u16> = value.code_units().collect();
    payload.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for unit in units {
        payload.extend_from_slice(&unit.to_le_bytes());
    }
}

/// Encodes one property key (format above): array indices inline,
/// predefined atoms (including the well-known symbols, which are
/// predefined in this engine) by ordinal, content-internable atoms by
/// kind and description. Identity symbols and private names fail closed
/// (§8.2).
fn encode_property_key(buffer: &mut Vec<u8>, key: &PropertyKey) -> Result<(), &'static str> {
    if let Some(index) = key.as_index() {
        buffer.push(0);
        buffer.extend_from_slice(&index.value().to_le_bytes());
        return Ok(());
    }
    let atom = key.as_atom().ok_or("a property key without identity")?;
    if let Some(predefined) = atom.predefined_atom() {
        buffer.push(2);
        buffer.extend_from_slice(&u32::from(predefined.ordinal()).to_le_bytes());
        return Ok(());
    }
    match atom.kind() {
        AtomKind::String | AtomKind::GlobalSymbol => {
            buffer.push(1);
            encode_atom_content(buffer, atom);
            Ok(())
        }
        AtomKind::Symbol | AtomKind::Private => Err("an identity symbol key"),
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
        StoredValue::Function(function) => {
            buffer.push(8);
            buffer.extend_from_slice(&(function.index() as u32).to_le_bytes());
        }
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
        let index = reader.read_u32()? as usize;
        let kind = decode_object_kind(&mut reader)?;
        staged.push(StagedObject {
            index,
            kind,
            ..decode_object_record_content(&mut reader)?
        });
    }
    Ok(staged)
}

/// Decodes one exotic-kind tag and payload (format above).
fn decode_object_kind(reader: &mut codec::Reader<'_>) -> Result<StagedObjectKind, SnapshotError> {
    Ok(match reader.read_u8()? {
        0 => StagedObjectKind::Ordinary,
        1 => {
            let length = reader.read_u32()?;
            let storage = match reader.read_u8()? {
                0 => {
                    let count = reader.read_u32()?;
                    let mut elements = Vec::new();
                    for _ in 0..count {
                        elements.push(match reader.read_u8()? {
                            0 => None,
                            1 => Some(decode_staged_value(reader)?),
                            other => {
                                return Err(SnapshotError::FormatMismatch {
                                    found: u32::from(other),
                                });
                            }
                        });
                    }
                    StagedArrayStorage::Dense(elements)
                }
                1 => StagedArrayStorage::Sparse,
                other => {
                    return Err(SnapshotError::FormatMismatch {
                        found: u32::from(other),
                    });
                }
            };
            StagedObjectKind::Array { length, storage }
        }
        2 => StagedObjectKind::Error {
            stack: decode_string_units(reader)?,
        },
        3 => StagedObjectKind::Date {
            value: f64::from_le_bytes(
                reader
                    .read_bytes(8)?
                    .try_into()
                    .map_err(|_| SnapshotError::Truncated)?,
            ),
        },
        4 => {
            let boxed = match reader.read_u8()? {
                0 => StagedBoxed::Boolean(reader.read_u8()? != 0),
                1 => StagedBoxed::Number(f64::from_le_bytes(
                    reader
                        .read_bytes(8)?
                        .try_into()
                        .map_err(|_| SnapshotError::Truncated)?,
                )),
                2 => {
                    let count = reader.read_u32()?;
                    let mut limbs = Vec::new();
                    for _ in 0..count {
                        limbs.push(reader.read_u32()?);
                    }
                    StagedBoxed::BigInt(limbs)
                }
                3 => StagedBoxed::String(decode_string_units(reader)?),
                4 => StagedBoxed::GlobalSymbol(decode_string_units(reader)?),
                other => {
                    return Err(SnapshotError::FormatMismatch {
                        found: u32::from(other),
                    });
                }
            };
            StagedObjectKind::Boxed(boxed)
        }
        5 => StagedObjectKind::RegExp {
            source: decode_string_units(reader)?,
            flags: decode_string_units(reader)?,
        },
        6 => {
            let count = reader.read_u32()?;
            let mut entries = Vec::new();
            for _ in 0..count {
                entries.push((decode_staged_value(reader)?, decode_staged_value(reader)?));
            }
            StagedObjectKind::Map(entries)
        }
        7 => {
            let count = reader.read_u32()?;
            let mut entries = Vec::new();
            for _ in 0..count {
                entries.push(decode_staged_value(reader)?);
            }
            StagedObjectKind::Set(entries)
        }
        8 => StagedObjectKind::RawJson,
        9 => {
            let count = reader.read_u32()?;
            let mut map = Vec::new();
            for _ in 0..count {
                map.push(match reader.read_u8()? {
                    0 => None,
                    1 => Some(reader.read_u32()?),
                    other => {
                        return Err(SnapshotError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                });
            }
            StagedObjectKind::Arguments(map)
        }
        other => {
            return Err(SnapshotError::FormatMismatch {
                found: u32::from(other),
            });
        }
    })
}

/// Decodes one UTF-16 string (unit count + units).
fn decode_string_units(reader: &mut codec::Reader<'_>) -> Result<Vec<u16>, SnapshotError> {
    let count = reader.read_u32()?;
    let mut units = Vec::new();
    for _ in 0..count {
        units.push(reader.read_u16()?);
    }
    Ok(units)
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
        other => {
            return Err(SnapshotError::FormatMismatch {
                found: u32::from(other),
            });
        }
    })
}

/// Decodes the realm-table section (format above); the second vector
/// carries each realm's staged global-object record in parallel.
fn decode_realms(payload: &[u8]) -> Result<(Vec<RealmRecord>, Vec<StagedObject>), SnapshotError> {
    let mut reader = codec::Reader::new(payload);
    let count = reader.read_u32()?;
    let mut records = Vec::new();
    let mut globals = Vec::new();
    for _ in 0..count {
        records.push(RealmRecord {
            object_prototype: reader.read_u32()? as usize,
            global_object: reader.read_u32()? as usize,
            math_random_state: reader.read_u64()?,
            objects: (reader.read_u32()? as usize, reader.read_u32()? as usize),
            functions: (reader.read_u32()? as usize, reader.read_u32()? as usize),
        });
        globals.push(decode_object_record_content(&mut reader)?);
    }
    if reader.remaining() != 0 {
        return Err(SnapshotError::Truncated);
    }
    Ok((records, globals))
}

/// One staged (not yet resolved) global binding.
struct StagedBinding {
    index: usize,
    realm: usize,
    name: (AtomKind, Vec<u16>),
    state: u8,
    cell: Option<u32>,
    mutable: bool,
}

/// Decodes the global-bindings section (format above).
fn decode_bindings(payload: &[u8]) -> Result<Vec<StagedBinding>, SnapshotError> {
    let mut reader = codec::Reader::new(payload);
    let count = reader.read_u32()?;
    let mut staged = Vec::new();
    for _ in 0..count {
        let index = reader.read_u32()? as usize;
        let realm = reader.read_u32()? as usize;
        let kind = match reader.read_u8()? {
            0 => AtomKind::String,
            1 => AtomKind::GlobalSymbol,
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let unit_count = reader.read_u32()?;
        let mut units = Vec::new();
        for _ in 0..unit_count {
            units.push(reader.read_u16()?);
        }
        let (state, cell, mutable) = match reader.read_u8()? {
            0 => (0, None, false),
            1 => (1, None, false),
            2 => (2, Some(reader.read_u32()?), reader.read_u8()? != 0),
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        staged.push(StagedBinding {
            index,
            realm,
            name: (kind, units),
            state,
            cell,
            mutable,
        });
    }
    if reader.remaining() != 0 {
        return Err(SnapshotError::Truncated);
    }
    Ok(staged)
}

/// Resolves the staged objects into the object arena (§8.3): objects
/// insert at their recorded indices (holes become reusable vacant
/// slots), so restored identities match the encoded arena exactly; each
/// record then fills with its resolved shape, slots, and prototype. The
/// returned vector maps arena indices to identities: the realm prefix
/// resolves to the replayed realm records, holes to `Id::ZERO`.
/// Inserts placeholder objects at their recorded indices (§8.3): holes
/// become reusable vacant slots, so restored identities match the
/// encoded arena exactly. The returned vector maps arena indices to
/// identities (realm prefix resolved, holes `Id::ZERO`); record contents
/// resolve once every function id exists in
/// [`resolve_object_records`].
fn restore_objects(
    runtime: &mut Runtime,
    staged: Vec<StagedObject>,
    watermark: usize,
) -> Result<(Vec<ObjectId>, Vec<(ObjectId, StagedObject)>), SnapshotError> {
    let mut ids: Vec<ObjectId> = (0..watermark)
        .map(|index| runtime.objects.id_from_index(index))
        .collect();
    let mut pending = Vec::new();
    for staged in staged {
        let index = staged.index;
        if index < watermark || index < ids.len() {
            return Err(SnapshotError::IntegrityViolation);
        }
        while ids.len() < index {
            ids.push(ObjectId::ZERO);
        }
        let placeholder = HeapObject::ordinary(ObjectRecord::from_parts(
            None,
            true,
            false,
            Arc::new(Vec::new()),
            Some(runtime.shape_interner.clone()),
            Vec::new(),
        ));
        let id = runtime
            .objects
            .restore_insert(index, placeholder)
            .ok_or(SnapshotError::IntegrityViolation)?;
        ids.push(id);
        pending.push((id, staged));
    }
    Ok((ids, pending))
}

/// Resolves staged object-record contents against the restored object
/// and function identities (the shared record content used by both
/// sections).
fn resolve_object_records(
    runtime: &mut Runtime,
    pending: Vec<(ObjectId, StagedObject)>,
    object_ids: &[ObjectId],
    function_ids: &[FunctionId],
) -> Result<(), SnapshotError> {
    for (id, staged) in pending {
        let StagedObject {
            index: _,
            kind,
            prototype,
            extensible,
            is_html_dda,
            shape,
            slots,
        } = staged;
        let record = resolve_object_record_content(
            runtime,
            prototype,
            extensible,
            is_html_dda,
            shape,
            slots,
            object_ids,
            function_ids,
        )?;
        let kind = resolve_object_kind(runtime, kind, object_ids, function_ids)?;
        let object = runtime
            .objects
            .get_mut(id)
            .ok_or(SnapshotError::IntegrityViolation)?;
        *object = HeapObject::restored(record, kind);
    }
    Ok(())
}

/// Resolves one staged exotic-kind state against the restored
/// identities (the tag-2 kind payloads). Regexp matchers recompile from
/// the recorded source and flags.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive exotic-kind resolution keeps every kind payload in order"
)]
fn resolve_object_kind(
    runtime: &mut Runtime,
    staged: StagedObjectKind,
    object_ids: &[ObjectId],
    function_ids: &[FunctionId],
) -> Result<HeapObjectKind, SnapshotError> {
    Ok(match staged {
        StagedObjectKind::Ordinary => HeapObjectKind::Ordinary,
        StagedObjectKind::Array { length, storage } => {
            let storage = match storage {
                StagedArrayStorage::Sparse => ArrayStorage::Sparse,
                StagedArrayStorage::Dense(elements) => {
                    let mut resolved = Vec::new();
                    for element in elements {
                        resolved.push(match element {
                            None => None,
                            Some(value) => Some(resolve_staged_value(
                                runtime,
                                value,
                                object_ids,
                                function_ids,
                            )?),
                        });
                    }
                    let present = resolved.iter().filter(|element| element.is_some()).count();
                    ArrayStorage::Dense {
                        elements: resolved,
                        present,
                    }
                }
            };
            HeapObjectKind::Array(ArrayState { length, storage })
        }
        StagedObjectKind::Error { stack } => HeapObjectKind::Error(ErrorState::new(
            JsString::from_code_units(stack).map_err(SnapshotError::String)?,
        )),
        StagedObjectKind::Date { value } => {
            HeapObjectKind::Date(DateState::new(JsNumber::from_f64(value)))
        }
        StagedObjectKind::Boxed(boxed) => HeapObjectKind::BoxedPrimitive(match boxed {
            StagedBoxed::Boolean(value) => BoxedPrimitive::Boolean(value),
            StagedBoxed::Number(value) => BoxedPrimitive::Number(JsNumber::from_f64(value)),
            StagedBoxed::BigInt(limbs) => {
                BoxedPrimitive::BigInt(Arc::new(JsBigInt::from_normalized_limbs(limbs)))
            }
            StagedBoxed::String(units) => BoxedPrimitive::String(
                JsString::from_code_units(units).map_err(SnapshotError::String)?,
            ),
            StagedBoxed::GlobalSymbol(units) => {
                let description =
                    JsString::from_code_units(units).map_err(SnapshotError::String)?;
                let atom = runtime
                    .atoms
                    .intern_global_symbol(&description)
                    .map_err(SnapshotError::Atom)?;
                BoxedPrimitive::Symbol(atom)
            }
        }),
        StagedObjectKind::RegExp { source, flags } => {
            let source = JsString::from_code_units(source).map_err(SnapshotError::String)?;
            let flags = JsString::from_code_units(flags).map_err(SnapshotError::String)?;
            let source_units: Vec<u16> = source.code_units().collect();
            let flags_units: Vec<u16> = flags.code_units().collect();
            let matcher = fusor_regexp::CompiledRegExp::compile_utf16(
                &source_units,
                &flags_units,
                Default::default(),
            )
            .map_err(|error| SnapshotError::RegExp(error.to_string()))?;
            HeapObjectKind::RegExp(RegExpState::new(source, flags, matcher))
        }
        StagedObjectKind::Map(entries) => {
            let mut resolved = Vec::new();
            for (key, value) in entries {
                resolved.push(MapEntry::live(
                    resolve_staged_value(runtime, key, object_ids, function_ids)?,
                    resolve_staged_value(runtime, value, object_ids, function_ids)?,
                ));
            }
            HeapObjectKind::Map(MapState::restored(resolved))
        }
        StagedObjectKind::Set(entries) => {
            let mut resolved = Vec::new();
            for key in entries {
                resolved.push(MapEntry::live(
                    resolve_staged_value(runtime, key, object_ids, function_ids)?,
                    StoredValue::Undefined,
                ));
            }
            HeapObjectKind::Set(SetState::restored(resolved))
        }
        StagedObjectKind::RawJson => HeapObjectKind::RawJson,
        StagedObjectKind::Arguments(map) => {
            let mut resolved = Vec::new();
            for entry in map {
                resolved.push(match entry {
                    None => None,
                    Some(index) => {
                        let cell = runtime.cells.id_from_index(index as usize);
                        if runtime.cells.get(cell).is_none() {
                            return Err(SnapshotError::IntegrityViolation);
                        }
                        Some(cell)
                    }
                });
            }
            HeapObjectKind::Arguments(ArgumentsState::mapped(resolved))
        }
    })
}

/// Encodes every binding cell into the cells-section payload: per cell
/// an arena index, a value tag (uninitialized or a stored value) and an
/// optional forwarding target.
fn encode_cells(runtime: &Runtime) -> Result<Vec<u8>, SnapshotError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(runtime.cells.len() as u32).to_le_bytes());
    for (id, cell) in runtime.cells.iter() {
        payload.extend_from_slice(&(id.index() as u32).to_le_bytes());
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
        let index = reader.read_u32()? as usize;
        let value = match reader.read_u8()? {
            0 => None,
            1 => Some(decode_staged_value(&mut reader)?),
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let forward = match reader.read_u8()? {
            0 => None,
            1 => Some(reader.read_u32()? as usize),
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        staged.push(StagedCell {
            index,
            value,
            forward,
        });
    }
    if reader.remaining() != 0 {
        return Err(SnapshotError::Truncated);
    }
    Ok(staged)
}

/// Resolves the staged cells into the cell arena: cells insert at their
/// recorded indices (holes become reusable vacant slots), values resolve
/// against the restored objects and functions, then forwarding targets
/// patch once every cell exists.
fn restore_cells(
    runtime: &mut Runtime,
    staged: Vec<StagedCell>,
    object_ids: &[ObjectId],
    function_ids: &[FunctionId],
) -> Result<(), SnapshotError> {
    let mut forwards = Vec::new();
    let mut ids: Vec<crate::ids::BindingCellId> = Vec::new();
    for cell in staged {
        let index = cell.index;
        if index < ids.len() {
            return Err(SnapshotError::IntegrityViolation);
        }
        while ids.len() < index {
            ids.push(crate::ids::BindingCellId::ZERO);
        }
        if let Some(target) = cell.forward {
            forwards.push((index, target));
        }
        let value = match cell.value {
            None => SlotValue::Uninitialized,
            Some(value) => SlotValue::Value(resolve_staged_value(
                runtime,
                value,
                object_ids,
                function_ids,
            )?),
        };
        let id = runtime
            .cells
            .restore_insert(
                index,
                BindingCell {
                    value,
                    forward: None,
                },
            )
            .ok_or(SnapshotError::IntegrityViolation)?;
        ids.push(id);
    }
    for (index, forward) in forwards {
        let target = *ids.get(forward).ok_or(SnapshotError::IntegrityViolation)?;
        if target == crate::ids::BindingCellId::ZERO {
            return Err(SnapshotError::IntegrityViolation);
        }
        let cell = runtime
            .cells
            .get_mut(ids[index])
            .ok_or(SnapshotError::IntegrityViolation)?;
        cell.forward = Some(target);
    }
    Ok(())
}

/// Resolves the staged global bindings into the bindings arena: bindings
/// insert at their recorded indices (holes become reusable vacant
/// slots), names re-intern by content, and lexical cells resolve against
/// the restored cell arena. The owning realm maps rebuild afterwards in
/// [`Runtime::from_snapshot`].
fn restore_bindings(
    runtime: &mut Runtime,
    staged: Vec<StagedBinding>,
    realm_count: usize,
) -> Result<(), SnapshotError> {
    let mut ids: Vec<crate::ids::RealmGlobalBindingId> = Vec::new();
    for binding in staged {
        let index = binding.index;
        if binding.realm >= realm_count || index < ids.len() {
            return Err(SnapshotError::IntegrityViolation);
        }
        while ids.len() < index {
            ids.push(crate::ids::RealmGlobalBindingId::ZERO);
        }
        let description =
            JsString::from_code_units(binding.name.1).map_err(SnapshotError::String)?;
        let name = match binding.name.0 {
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
        let realm = runtime.realms.id_from_index(binding.realm);
        let state = match binding.state {
            0 => RealmGlobalBindingState::Unresolved,
            1 => RealmGlobalBindingState::Object,
            2 => {
                let index = binding.cell.ok_or(SnapshotError::IntegrityViolation)? as usize;
                let cell = runtime.cells.id_from_index(index);
                if runtime.cells.get(cell).is_none() {
                    return Err(SnapshotError::IntegrityViolation);
                }
                RealmGlobalBindingState::Lexical {
                    cell,
                    mutable: binding.mutable,
                }
            }
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let id = runtime
            .global_bindings
            .restore_insert(index, RealmGlobalBinding { realm, name, state })
            .ok_or(SnapshotError::IntegrityViolation)?;
        ids.push(id);
    }
    Ok(())
}

/// Resolves one staged slot value into a heap value, mapping object and
/// function references through the restored identities. References into
/// reclaimed holes (`Id::ZERO` padding) fail closed.
fn resolve_staged_value(
    runtime: &mut Runtime,
    value: StagedValue,
    object_ids: &[ObjectId],
    function_ids: &[FunctionId],
) -> Result<StoredValue, SnapshotError> {
    Ok(match value {
        StagedValue::Undefined => StoredValue::Undefined,
        StagedValue::Null => StoredValue::Null,
        StagedValue::Boolean(value) => StoredValue::Boolean(value),
        StagedValue::Number(value) => StoredValue::Number(JsNumber::from_f64(value)),
        StagedValue::BigInt(limbs) => {
            StoredValue::BigInt(Arc::new(JsBigInt::from_normalized_limbs(limbs)))
        }
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
            let target = *object_ids
                .get(index)
                .ok_or(SnapshotError::IntegrityViolation)?;
            if target == ObjectId::ZERO {
                return Err(SnapshotError::IntegrityViolation);
            }
            StoredValue::Object(target)
        }
        StagedValue::Function(index) => {
            let target = *function_ids
                .get(index)
                .ok_or(SnapshotError::IntegrityViolation)?;
            if target == FunctionId::ZERO {
                return Err(SnapshotError::IntegrityViolation);
            }
            StoredValue::Function(target)
        }
    })
}

/// One staged (not yet resolved) function from the functions section.
struct StagedFunction {
    index: usize,
    kind: StagedFunctionKind,
    record: StagedObject,
}

enum StagedFunctionKind {
    Bytecode {
        code: usize,
        template: u32,
        environment: Vec<(u8, u32)>,
        eval_environment: Option<u32>,
        environment_eval_shadows: Vec<Option<(u32, Option<u32>)>>,
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
        realm: Option<usize>,
        slot: u32,
    },
}

/// Interns one shared eval-variable environment into the ordinal map,
/// interning parents first so parent ordinals precede children (decode
/// rebuilds topologically). Nodes deduplicate by `Rc` identity — the
/// graph is shared across functions by construction.
fn intern_environment(
    environment: &SharedEvalVariableEnvironment,
    ordinals: &mut std::collections::HashMap<usize, u32>,
    nodes: &mut Vec<SharedEvalVariableEnvironment>,
) -> Result<u32, SnapshotError> {
    let pointer = std::rc::Rc::as_ptr(environment) as usize;
    if let Some(ordinal) = ordinals.get(&pointer) {
        return Ok(*ordinal);
    }
    if nodes.len() > 1_000_000 {
        return Err(SnapshotError::IntegrityViolation);
    }
    if let Some(parent) = environment.borrow().parent.clone() {
        intern_environment(&parent, ordinals, nodes)?;
    }
    let ordinal = nodes.len() as u32;
    ordinals.insert(pointer, ordinal);
    nodes.push(std::rc::Rc::clone(environment));
    Ok(ordinal)
}

/// Encodes every user function (arena index ≥ the realm watermark) and
/// the distinct installed-code authorities they reference (format above),
/// gap-encoded by index. Each code record carries its installing realm;
/// host functions carry their realm explicitly. The shared eval-variable
/// environment DAG rides along as a node list. Engine intrinsics and
/// non-bytecode implementation kinds fail closed (§8.2).
fn encode_functions(runtime: &Runtime, watermark: usize) -> Result<Vec<u8>, SnapshotError> {
    let mut code_payloads: Vec<(usize, RealmId, Vec<u8>)> = Vec::new();
    for (code_id, code) in runtime.code.iter() {
        let encoded =
            fusor_bytecode::encode_verified_bytecode(&code.authority).map_err(|_error| {
                SnapshotError::Unsupported {
                    index: code_id.index(),
                    what: "a verified bytecode authority",
                }
            })?;
        code_payloads.push((code_id.index(), code.realm, encoded));
    }
    let code_ordinals: std::collections::HashMap<usize, u32> = code_payloads
        .iter()
        .enumerate()
        .map(|(ordinal, (index, _, _))| (*index, ordinal as u32))
        .collect();
    let mut payload = Vec::new();
    payload.extend_from_slice(&(code_payloads.len() as u32).to_le_bytes());
    for (index, realm, encoded) in &code_payloads {
        payload.extend_from_slice(&(*index as u32).to_le_bytes());
        if *realm == RealmId::ZERO {
            payload.push(0);
        } else {
            payload.push(1);
            payload.extend_from_slice(&(realm.index() as u32).to_le_bytes());
        }
        payload.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
        payload.extend_from_slice(encoded);
    }
    let user_functions: Vec<_> = runtime
        .functions
        .iter()
        .filter(|(id, _)| id.index() >= watermark)
        .collect();
    // Collect the shared eval-variable environment DAG across every
    // bytecode function (pre-pass: the node list precedes the function
    // records in the payload).
    let mut env_ordinals = std::collections::HashMap::new();
    let mut env_nodes: Vec<SharedEvalVariableEnvironment> = Vec::new();
    for (_, function) in &user_functions {
        let FunctionImplementation::Bytecode(bytecode) = &function.implementation else {
            continue;
        };
        if let Some(environment) = &bytecode.eval_environment {
            intern_environment(environment, &mut env_ordinals, &mut env_nodes)?;
        }
        for shadow in bytecode.environment_eval_shadows.iter().flatten() {
            intern_environment(&shadow.head, &mut env_ordinals, &mut env_nodes)?;
            if let Some(boundary) = &shadow.boundary {
                intern_environment(boundary, &mut env_ordinals, &mut env_nodes)?;
            }
        }
    }
    payload.extend_from_slice(&(env_nodes.len() as u32).to_le_bytes());
    for node in &env_nodes {
        let record = node.borrow();
        payload.push(match record.kind {
            EvalVariableEnvironmentKind::Function => 0,
            EvalVariableEnvironmentKind::ParameterInitializer => 1,
            EvalVariableEnvironmentKind::ParameterBoundary => 2,
            EvalVariableEnvironmentKind::FunctionBody => 3,
        });
        match &record.parent {
            None => payload.push(0),
            Some(parent) => {
                payload.push(1);
                let ordinal = *env_ordinals
                    .get(&(std::rc::Rc::as_ptr(parent) as usize))
                    .ok_or(SnapshotError::IntegrityViolation)?;
                payload.extend_from_slice(&ordinal.to_le_bytes());
            }
        }
        payload.extend_from_slice(&(record.bindings.len() as u32).to_le_bytes());
        for binding in &record.bindings {
            let units: Vec<u16> = binding.name.code_units().collect();
            payload.extend_from_slice(&(units.len() as u32).to_le_bytes());
            for unit in units {
                payload.extend_from_slice(&unit.to_le_bytes());
            }
            payload.extend_from_slice(&(binding.cell.index() as u32).to_le_bytes());
            payload.push(u8::from(binding.deleted));
        }
    }
    payload.extend_from_slice(&(user_functions.len() as u32).to_le_bytes());
    for (function_id, function) in user_functions {
        payload.extend_from_slice(&(function_id.index() as u32).to_le_bytes());
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
                match &bytecode.eval_environment {
                    None => payload.push(0),
                    Some(environment) => {
                        payload.push(1);
                        let ordinal =
                            intern_environment(environment, &mut env_ordinals, &mut env_nodes)?;
                        payload.extend_from_slice(&ordinal.to_le_bytes());
                    }
                }
                payload.extend_from_slice(
                    &(bytecode.environment_eval_shadows.len() as u32).to_le_bytes(),
                );
                for shadow in &bytecode.environment_eval_shadows {
                    match shadow {
                        None => payload.push(0),
                        Some(shadow) => {
                            payload.push(1);
                            let head = intern_environment(
                                &shadow.head,
                                &mut env_ordinals,
                                &mut env_nodes,
                            )?;
                            payload.extend_from_slice(&head.to_le_bytes());
                            match &shadow.boundary {
                                None => payload.push(0),
                                Some(boundary) => {
                                    payload.push(1);
                                    let boundary = intern_environment(
                                        boundary,
                                        &mut env_ordinals,
                                        &mut env_nodes,
                                    )?;
                                    payload.extend_from_slice(&boundary.to_le_bytes());
                                }
                            }
                        }
                    }
                }
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
                encode_object_record_payload(&mut payload, &function.object).map_err(|what| {
                    SnapshotError::Unsupported {
                        index: function_id.index(),
                        what,
                    }
                })?;
            }
            FunctionImplementation::Native(native) => match native.kind {
                crate::runtime::NativeFunctionKind::Host(slot) => {
                    payload.push(1);
                    if native.realm == RealmId::ZERO {
                        payload.push(0);
                    } else {
                        payload.push(1);
                        payload.extend_from_slice(&(native.realm.index() as u32).to_le_bytes());
                    }
                    payload.extend_from_slice(&(slot.index() as u32).to_le_bytes());
                    encode_object_record_payload(&mut payload, &function.object).map_err(
                        |what| SnapshotError::Unsupported {
                            index: function_id.index(),
                            what,
                        },
                    )?;
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

/// Encodes the realm table (format above): identities and segment
/// watermarks per realm, plus the realm's global-object record so user
/// mutations on `globalThis` restore on top of the replayed intrinsic
/// graph — the rest of the intrinsic graph is never serialized (§8.2).
fn encode_realms(runtime: &Runtime, records: &[RealmRecord]) -> Result<Vec<u8>, SnapshotError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for record in records {
        payload.extend_from_slice(&(record.object_prototype as u32).to_le_bytes());
        payload.extend_from_slice(&(record.global_object as u32).to_le_bytes());
        payload.extend_from_slice(&record.math_random_state.to_le_bytes());
        payload.extend_from_slice(&(record.objects.0 as u32).to_le_bytes());
        payload.extend_from_slice(&(record.objects.1 as u32).to_le_bytes());
        payload.extend_from_slice(&(record.functions.0 as u32).to_le_bytes());
        payload.extend_from_slice(&(record.functions.1 as u32).to_le_bytes());
        let global = runtime
            .objects
            .get(runtime.objects.id_from_index(record.global_object))
            .ok_or(SnapshotError::IntegrityViolation)?;
        encode_object_record_payload(&mut payload, &global.record).map_err(|what| {
            SnapshotError::Unsupported {
                index: record.global_object,
                what,
            }
        })?;
    }
    Ok(payload)
}

/// Encodes the global-bindings arena (format above): per binding the
/// arena index, owning realm index, name atom content, and state.
fn encode_bindings(runtime: &Runtime) -> Result<Vec<u8>, SnapshotError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(runtime.global_bindings.len() as u32).to_le_bytes());
    for (id, binding) in runtime.global_bindings.iter() {
        payload.extend_from_slice(&(id.index() as u32).to_le_bytes());
        payload.extend_from_slice(&(binding.realm.index() as u32).to_le_bytes());
        encode_atom_content(&mut payload, &binding.name);
        match &binding.state {
            RealmGlobalBindingState::Unresolved => payload.push(0),
            RealmGlobalBindingState::Object => payload.push(1),
            RealmGlobalBindingState::Lexical { cell, mutable } => {
                payload.push(2);
                payload.extend_from_slice(&(cell.index() as u32).to_le_bytes());
                payload.push(u8::from(*mutable));
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
        Some(HeapReference::Function(target)) => {
            buffer.push(2);
            buffer.extend_from_slice(&(target.index() as u32).to_le_bytes());
        }
    }
    buffer.push(u8::from(record.is_extensible()));
    buffer.push(u8::from(record.is_html_dda()));
    let shape = record.shape();
    buffer.extend_from_slice(&(shape.len() as u32).to_le_bytes());
    for property in shape.iter() {
        encode_property_key(buffer, property.key())?;
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
        other => {
            return Err(SnapshotError::FormatMismatch {
                found: u32::from(other),
            });
        }
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
                        return Err(SnapshotError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                };
                let unit_count = reader.read_u32()?;
                let mut units = Vec::new();
                for _ in 0..unit_count {
                    units.push(reader.read_u16()?);
                }
                StagedKey::Atom(kind, units)
            }
            2 => StagedKey::Predefined(reader.read_u32()?),
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
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
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
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
        // Filled by the section decoders; the shared record content is
        // index- and kind-agnostic.
        index: 0,
        kind: StagedObjectKind::Ordinary,
        prototype,
        extensible,
        is_html_dda,
        shape,
        slots,
    })
}

/// One staged (not yet resolved) eval-variable environment node.
struct StagedEnvironment {
    kind: u8,
    parent: Option<u32>,
    bindings: Vec<(Vec<u16>, u32, bool)>,
}

/// Decodes the functions-section payload into the distinct authorities,
/// the shared environment DAG, and the staged functions.
fn decode_functions(
    payload: &[u8],
) -> Result<
    (
        Vec<(usize, Option<usize>, Arc<VerifiedBytecode>)>,
        Vec<StagedEnvironment>,
        Vec<StagedFunction>,
    ),
    SnapshotError,
> {
    let mut reader = codec::Reader::new(payload);
    let code_count = reader.read_u32()?;
    let mut authorities = Vec::new();
    for _ in 0..code_count {
        let code_index = reader.read_u32()? as usize;
        let realm = match reader.read_u8()? {
            0 => None,
            1 => Some(reader.read_u32()? as usize),
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let length = reader.read_u32()? as usize;
        let encoded = reader.read_bytes(length)?;
        let authority = fusor_bytecode::decode_verified_bytecode(encoded)
            .map_err(|error| SnapshotError::Bytecode(error.to_string()))?;
        authorities.push((code_index, realm, Arc::new(authority)));
    }
    let env_count = reader.read_u32()?;
    let mut environments = Vec::new();
    for _ in 0..env_count {
        let kind = match reader.read_u8()? {
            kind @ 0..=3 => kind,
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let parent = match reader.read_u8()? {
            0 => None,
            1 => {
                let parent = reader.read_u32()?;
                if parent as usize >= environments.len() {
                    return Err(SnapshotError::IntegrityViolation);
                }
                Some(parent)
            }
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        };
        let binding_count = reader.read_u32()?;
        let mut bindings = Vec::new();
        for _ in 0..binding_count {
            let unit_count = reader.read_u32()?;
            let mut units = Vec::new();
            for _ in 0..unit_count {
                units.push(reader.read_u16()?);
            }
            let cell = reader.read_u32()?;
            let deleted = reader.read_u8()? != 0;
            bindings.push((units, cell, deleted));
        }
        environments.push(StagedEnvironment {
            kind,
            parent,
            bindings,
        });
    }
    let function_count = reader.read_u32()?;
    let mut functions = Vec::new();
    for _ in 0..function_count {
        let index = reader.read_u32()? as usize;
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
                            return Err(SnapshotError::FormatMismatch {
                                found: u32::from(other),
                            });
                        }
                    }
                }
                let eval_environment = match reader.read_u8()? {
                    0 => None,
                    1 => {
                        let ordinal = reader.read_u32()?;
                        if ordinal as usize >= environments.len() {
                            return Err(SnapshotError::IntegrityViolation);
                        }
                        Some(ordinal)
                    }
                    other => {
                        return Err(SnapshotError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                };
                let shadow_count = reader.read_u32()?;
                let mut environment_eval_shadows = Vec::new();
                for _ in 0..shadow_count {
                    environment_eval_shadows.push(match reader.read_u8()? {
                        0 => None,
                        1 => {
                            let head = reader.read_u32()?;
                            if head as usize >= environments.len() {
                                return Err(SnapshotError::IntegrityViolation);
                            }
                            let boundary = match reader.read_u8()? {
                                0 => None,
                                1 => {
                                    let boundary = reader.read_u32()?;
                                    if boundary as usize >= environments.len() {
                                        return Err(SnapshotError::IntegrityViolation);
                                    }
                                    Some(boundary)
                                }
                                other => {
                                    return Err(SnapshotError::FormatMismatch {
                                        found: u32::from(other),
                                    });
                                }
                            };
                            Some((head, boundary))
                        }
                        other => {
                            return Err(SnapshotError::FormatMismatch {
                                found: u32::from(other),
                            });
                        }
                    });
                }
                let lexical_receiver = match reader.read_u8()? {
                    0 => None,
                    1 => Some(decode_staged_value(&mut reader)?),
                    other => {
                        return Err(SnapshotError::FormatMismatch {
                            found: u32::from(other),
                        });
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
                        return Err(SnapshotError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                };
                let has_instance_elements = reader.read_u8()? != 0;
                let home_object = match reader.read_u8()? {
                    0 => None,
                    1 => Some((1, reader.read_u32()?)),
                    2 => Some((2, reader.read_u32()?)),
                    other => {
                        return Err(SnapshotError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                };
                let record = decode_object_record_content(&mut reader)?;
                functions.push(StagedFunction {
                    index,
                    kind: StagedFunctionKind::Bytecode {
                        code,
                        template,
                        environment,
                        eval_environment,
                        environment_eval_shadows,
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
                let realm = match reader.read_u8()? {
                    0 => None,
                    1 => Some(reader.read_u32()? as usize),
                    other => {
                        return Err(SnapshotError::FormatMismatch {
                            found: u32::from(other),
                        });
                    }
                };
                let slot = reader.read_u32()?;
                let record = decode_object_record_content(&mut reader)?;
                functions.push(StagedFunction {
                    index,
                    kind: StagedFunctionKind::Host { realm, slot },
                    record,
                });
            }
            other => {
                return Err(SnapshotError::FormatMismatch {
                    found: u32::from(other),
                });
            }
        }
    }
    if reader.remaining() != 0 {
        return Err(SnapshotError::Truncated);
    }
    Ok((authorities, environments, functions))
}

fn read_function_ref(reader: &mut codec::Reader<'_>) -> Result<Option<usize>, SnapshotError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.read_u32()? as usize)),
        other => Err(SnapshotError::FormatMismatch {
            found: u32::from(other),
        }),
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
    let StagedObject {
        index: _,
        kind: _,
        prototype,
        extensible,
        is_html_dda,
        shape,
        slots,
    } = staged;
    resolve_object_record_content(
        runtime,
        prototype,
        extensible,
        is_html_dda,
        shape,
        slots,
        object_ids,
        function_ids,
    )
}

/// Resolves one staged object record's content (prototype, flags,
/// shape, slots) — shared by the object and function record paths.
#[allow(
    clippy::too_many_arguments,
    reason = "one record content resolver serves every staged record path"
)]
fn resolve_object_record_content(
    runtime: &mut Runtime,
    prototype: Option<(u8, usize)>,
    extensible: bool,
    is_html_dda: bool,
    shape: Vec<(StagedKey, PropertyLayout)>,
    slots: Vec<StagedValue>,
    object_ids: &[ObjectId],
    function_ids: &[FunctionId],
) -> Result<ObjectRecord, SnapshotError> {
    let mut shape_properties = Vec::new();
    for (key, layout) in shape {
        let key = match key {
            StagedKey::Index(index) => {
                let index = ArrayIndex::new(index).ok_or(SnapshotError::IntegrityViolation)?;
                PropertyKey::from_index(index)
            }
            StagedKey::Predefined(ordinal) => {
                let atom = PredefinedAtom::from_ordinal(ordinal as u16)
                    .ok_or(SnapshotError::IntegrityViolation)?;
                let atom = runtime.atoms.predefined(atom);
                match atom.kind() {
                    AtomKind::String => PropertyKey::from_validated_atom(atom),
                    AtomKind::GlobalSymbol | AtomKind::Symbol => {
                        PropertyKey::from_validated_symbol(atom)
                    }
                    AtomKind::Private => return Err(SnapshotError::IntegrityViolation),
                }
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
    let mut resolved_slots = Vec::new();
    for value in slots {
        let value = match value {
            StagedValue::Function(index) => {
                let target = *function_ids
                    .get(index)
                    .ok_or(SnapshotError::IntegrityViolation)?;
                if target == FunctionId::ZERO {
                    return Err(SnapshotError::IntegrityViolation);
                }
                StoredValue::Function(target)
            }
            other => resolve_staged_value(runtime, other, object_ids, function_ids)?,
        };
        resolved_slots.push(PropertySlot::Data(value));
    }
    let prototype = match prototype {
        None => None,
        Some((1, index)) => {
            let target = *object_ids
                .get(index)
                .ok_or(SnapshotError::IntegrityViolation)?;
            if target == ObjectId::ZERO {
                return Err(SnapshotError::IntegrityViolation);
            }
            Some(HeapReference::Object(target))
        }
        Some((2, index)) => {
            let target = *function_ids
                .get(index)
                .ok_or(SnapshotError::IntegrityViolation)?;
            if target == FunctionId::ZERO {
                return Err(SnapshotError::IntegrityViolation);
            }
            Some(HeapReference::Function(target))
        }
        Some(_) => return Err(SnapshotError::IntegrityViolation),
    };
    Ok(ObjectRecord::from_parts(
        prototype,
        extensible,
        is_html_dda,
        Arc::new(shape_properties),
        Some(runtime.shape_interner.clone()),
        resolved_slots,
    ))
}

struct PendingFunction {
    record: StagedObject,
    home_object: Option<(u8, u32)>,
    lexical_new_target: Option<usize>,
    lexical_derived_constructor: Option<usize>,
    lexical_receiver: Option<StagedValue>,
}

/// Restores the staged functions: authorities re-install at their
/// recorded arena indices (load-time verification, §8.3) and functions
/// insert at their recorded indices with the realm prefix resolved to
/// the replayed realms. Returns the arena-index → identity mapping and
/// the record contents waiting on every function id (patched by
/// [`resolve_function_records`]).
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive staged-function placement keeps every arena index in order"
)]
fn restore_functions(
    runtime: &mut Runtime,
    authorities: Vec<(usize, Option<usize>, Arc<VerifiedBytecode>)>,
    environments: Vec<StagedEnvironment>,
    staged: Vec<StagedFunction>,
    watermark: usize,
    realms: &[Realm],
) -> Result<(Vec<FunctionId>, Vec<(FunctionId, PendingFunction)>), SnapshotError> {
    let realm_id = |index: usize| -> Result<RealmId, SnapshotError> {
        realms
            .get(index)
            .map(Realm::id)
            .ok_or(SnapshotError::IntegrityViolation)
    };
    // Rebuild the shared eval-variable environment DAG first (parent
    // ordinals precede children by construction, so child links resolve
    // topologically).
    let mut environment_ids: Vec<SharedEvalVariableEnvironment> = Vec::new();
    for environment in environments {
        let parent = match environment.parent {
            None => None,
            Some(ordinal) => Some(std::rc::Rc::clone(
                environment_ids
                    .get(ordinal as usize)
                    .ok_or(SnapshotError::IntegrityViolation)?,
            )),
        };
        let record = EvalVariableEnvironment {
            kind: match environment.kind {
                0 => EvalVariableEnvironmentKind::Function,
                1 => EvalVariableEnvironmentKind::ParameterInitializer,
                2 => EvalVariableEnvironmentKind::ParameterBoundary,
                3 => EvalVariableEnvironmentKind::FunctionBody,
                other => {
                    return Err(SnapshotError::FormatMismatch {
                        found: u32::from(other),
                    });
                }
            },
            parent,
            bindings: Vec::new(),
        };
        let node = std::rc::Rc::new(std::cell::RefCell::new(record));
        {
            let mut node = node.borrow_mut();
            for (units, cell, deleted) in environment.bindings {
                let name = JsString::from_code_units(units).map_err(SnapshotError::String)?;
                node.bindings.push(crate::runtime::EvalVariableBinding {
                    name,
                    cell: runtime.cells.id_from_index(cell as usize),
                    deleted,
                });
            }
        }
        environment_ids.push(node);
    }
    let mut code_ids: Vec<crate::ids::InstalledCodeId> = Vec::new();
    let mut code_ordinal_ids: Vec<crate::ids::InstalledCodeId> = Vec::new();
    for (index, realm, authority) in authorities {
        if index < code_ids.len() {
            return Err(SnapshotError::IntegrityViolation);
        }
        while code_ids.len() < index {
            code_ids.push(crate::ids::InstalledCodeId::ZERO);
        }
        let realm = match realm {
            None => RealmId::ZERO,
            Some(index) => realm_id(index)?,
        };
        let templates = runtime
            .stage_templates(&authority)
            .map_err(|_| SnapshotError::IntegrityViolation)?;
        let id = runtime
            .code
            .restore_insert(
                index,
                InstalledCode {
                    authority,
                    realm,
                    templates,
                    live_functions: 0,
                },
            )
            .ok_or(SnapshotError::IntegrityViolation)?;
        code_ids.push(id);
        code_ordinal_ids.push(id);
    }
    let mut function_ids: Vec<FunctionId> = (0..watermark)
        .map(|index| runtime.functions.id_from_index(index))
        .collect();
    let mut pending = Vec::new();
    for function in staged {
        let index = function.index;
        if index < watermark || index < function_ids.len() {
            return Err(SnapshotError::IntegrityViolation);
        }
        while function_ids.len() < index {
            function_ids.push(FunctionId::ZERO);
        }
        let implementation = match function.kind {
            StagedFunctionKind::Bytecode {
                code,
                template,
                environment,
                eval_environment,
                environment_eval_shadows,
                lexical_receiver,
                lexical_eval_in_function,
                lexical_eval_in_class_field_initializer,
                lexical_new_target,
                lexical_derived_constructor,
                lexical_derived_this,
                has_instance_elements,
                home_object,
            } => {
                let code = *code_ordinal_ids
                    .get(code)
                    .ok_or(SnapshotError::IntegrityViolation)?;
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
                let eval_environment = match eval_environment {
                    None => None,
                    Some(ordinal) => Some(std::rc::Rc::clone(
                        environment_ids
                            .get(ordinal as usize)
                            .ok_or(SnapshotError::IntegrityViolation)?,
                    )),
                };
                let mut eval_shadows = Vec::new();
                for shadow in environment_eval_shadows {
                    eval_shadows.push(match shadow {
                        None => None,
                        Some((head, boundary)) => Some(EvalBindingShadow {
                            head: std::rc::Rc::clone(
                                environment_ids
                                    .get(head as usize)
                                    .ok_or(SnapshotError::IntegrityViolation)?,
                            ),
                            boundary: match boundary {
                                None => None,
                                Some(boundary) => Some(std::rc::Rc::clone(
                                    environment_ids
                                        .get(boundary as usize)
                                        .ok_or(SnapshotError::IntegrityViolation)?,
                                )),
                            },
                        }),
                    });
                }
                pending.push(PendingFunction {
                    record: function.record,
                    home_object,
                    lexical_new_target,
                    lexical_derived_constructor,
                    lexical_receiver,
                });
                FunctionImplementation::Bytecode(BytecodeFunction {
                    code,
                    template: fusor_bytecode::FunctionTemplateId::new(template),
                    environment: bindings,
                    environment_eval_shadows: eval_shadows,
                    eval_environment,
                    lexical_receiver: None,
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
            StagedFunctionKind::Host { realm, slot } => {
                let realm = match realm {
                    None => RealmId::ZERO,
                    Some(index) => realm_id(index)?,
                };
                pending.push(PendingFunction {
                    record: function.record,
                    home_object: None,
                    lexical_new_target: None,
                    lexical_derived_constructor: None,
                    lexical_receiver: None,
                });
                FunctionImplementation::Native(crate::runtime::NativeFunction {
                    realm,
                    kind: crate::runtime::NativeFunctionKind::Host(crate::HostFunctionId::new(
                        slot as usize,
                    )),
                })
            }
        };
        let id = runtime
            .functions
            .restore_insert(
                index,
                HeapFunction {
                    implementation,
                    object: ObjectRecord::from_parts(
                        None,
                        true,
                        false,
                        Arc::new(Vec::new()),
                        Some(runtime.shape_interner.clone()),
                        Vec::new(),
                    ),
                    public_roots: 0,
                },
            )
            .ok_or(SnapshotError::IntegrityViolation)?;
        function_ids.push(id);
    }
    let pending: Vec<_> = function_ids
        .iter()
        .skip(watermark)
        .copied()
        .filter(|id| *id != FunctionId::ZERO)
        .zip(pending)
        .collect();
    Ok((function_ids, pending))
}

/// Patches the staged function records once every object and function id
/// exists: record contents (shapes, slots, prototypes) resolve, then the
/// deferred cross-references (`home_object`, `new.target` links, lexical
/// receivers) attach.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive cross-reference patch keeps every deferred edge in order"
)]
fn resolve_function_records(
    runtime: &mut Runtime,
    pending: Vec<(FunctionId, PendingFunction)>,
    object_ids: &[ObjectId],
    function_ids: &[FunctionId],
) -> Result<(), SnapshotError> {
    // The cells arena restores after the function placeholders: validate
    // every eval-environment cell reference now (fail closed, §8.3).
    let mut referenced_cells = std::collections::HashSet::new();
    for (_, function) in runtime.functions.iter() {
        let FunctionImplementation::Bytecode(bytecode) = &function.implementation else {
            continue;
        };
        if let Some(environment) = &bytecode.eval_environment {
            EvalVariableEnvironment::trace_cells(environment, |cell| {
                referenced_cells.insert(cell);
            });
        }
        for shadow in bytecode.environment_eval_shadows.iter().flatten() {
            EvalVariableEnvironment::trace_cells(&shadow.head, |cell| {
                referenced_cells.insert(cell);
            });
            if let Some(boundary) = &shadow.boundary {
                EvalVariableEnvironment::trace_cells(boundary, |cell| {
                    referenced_cells.insert(cell);
                });
            }
        }
    }
    for cell in referenced_cells {
        if runtime.cells.get(cell).is_none() {
            return Err(SnapshotError::IntegrityViolation);
        }
    }
    for (function_id, pending) in pending {
        let record = resolve_object_record(runtime, pending.record, object_ids, function_ids)?;
        let lexical_new_target = match pending.lexical_new_target {
            Some(index) => Some(
                *function_ids
                    .get(index)
                    .filter(|id| **id != FunctionId::ZERO)
                    .ok_or(SnapshotError::IntegrityViolation)?,
            ),
            None => None,
        };
        let lexical_derived_constructor = match pending.lexical_derived_constructor {
            Some(index) => Some(
                *function_ids
                    .get(index)
                    .filter(|id| **id != FunctionId::ZERO)
                    .ok_or(SnapshotError::IntegrityViolation)?,
            ),
            None => None,
        };
        let lexical_receiver = match pending.lexical_receiver {
            Some(value) => Some(resolve_staged_value(
                runtime,
                value,
                object_ids,
                function_ids,
            )?),
            None => None,
        };
        let home_object = match pending.home_object {
            None => None,
            Some((1, index)) => {
                let target = *object_ids
                    .get(index as usize)
                    .filter(|id| **id != ObjectId::ZERO)
                    .ok_or(SnapshotError::IntegrityViolation)?;
                Some(HeapReference::Object(target))
            }
            Some((2, index)) => {
                let target = *function_ids
                    .get(index as usize)
                    .filter(|id| **id != FunctionId::ZERO)
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
        bytecode.lexical_receiver = lexical_receiver;
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
