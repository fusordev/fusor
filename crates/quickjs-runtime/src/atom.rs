/*
 * JavaScript atom representation derived from QuickJS.
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

//! Runtime-local JavaScript atoms and property keys.
//!
//! Atom handles own their entries through [`Arc`] while the table's [`Cell`]
//! accounting keeps the runtime-local graph `!Send + !Sync`. String atoms and
//! the global symbol registry are content-interned in separate namespaces. The
//! interner stores only weak entry handles, so collecting dead slots also
//! releases their descriptions.
//!
//! Growable collection storage and compact string copies use fallible reserve
//! operations. Allocation of `Arc` control blocks follows Rust's global
//! allocator policy.

use std::{
    cell::Cell,
    collections::HashMap,
    error::Error,
    fmt,
    hash::{BuildHasher, Hash, Hasher, RandomState},
    sync::{Arc, Weak},
};

use crate::{
    ArrayIndex, JsString, JsStringError,
    predefined_atoms::{PredefinedAtom, PredefinedAtomKind, PredefinedAtomSpec},
};

/// Maximum number of simultaneously live atoms accepted by `QuickJS`.
pub const MAX_ATOM_ENTRIES: u32 = (1 << 30) - 1;

/// Number of atoms installed when an atom table is created.
pub const PREDEFINED_ATOM_COUNT: u32 = 243;

/// UTF-16 code units held by the predefined atom descriptions.
pub const PREDEFINED_DESCRIPTION_CODE_UNITS: u64 = 2_092;

/// Content-interner slots held by predefined string atoms.
pub const PREDEFINED_INTERNER_SLOTS: u32 = 228;

/// The identity namespace of an atom.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AtomKind {
    /// A content-interned JavaScript string property name.
    String,
    /// A content-interned entry in the global symbol registry.
    GlobalSymbol,
    /// A unique symbol, including the well-known predefined symbols.
    Symbol,
    /// A unique private name.
    Private,
}

/// An owning, runtime-local atom handle.
///
/// Equality and hashing use identity, not description contents.
///
/// Atom ownership uses `Arc`, but the runtime-local accounting deliberately
/// prevents atom handles from crossing threads.
///
/// ```compile_fail
/// use quickjs_runtime::Atom;
///
/// fn require_send_and_sync<T: Send + Sync>() {}
/// require_send_and_sync::<Atom>();
/// ```
#[derive(Clone)]
pub struct Atom(Arc<AtomEntry>);

struct AtomEntry {
    owner: Weak<TableState>,
    kind: AtomKind,
    description: Option<JsString>,
    predefined: Option<PredefinedAtom>,
}

/// Non-owning identity used by weak collections.
///
/// Hashing and equality preserve Symbol identity without keeping the Atom
/// entry alive. This is deliberately crate-private: JavaScript can only
/// observe the identity again by presenting a live Symbol value.
#[derive(Clone)]
pub(crate) struct WeakAtom(Weak<AtomEntry>);

impl WeakAtom {
    pub(crate) fn from_atom(atom: &Atom) -> Self {
        Self(Arc::downgrade(&atom.0))
    }

    pub(crate) fn strong_count(&self) -> usize {
        self.0.strong_count()
    }

    pub(crate) fn upgrade(&self) -> Option<Atom> {
        self.0.upgrade().map(Atom)
    }
}

impl PartialEq for WeakAtom {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl Eq for WeakAtom {}

impl Hash for WeakAtom {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_ptr().hash(state);
    }
}

impl Atom {
    /// Returns the atom namespace.
    #[must_use]
    pub fn kind(&self) -> AtomKind {
        self.0.kind
    }

    /// Returns the atom description.
    ///
    /// A symbol created without a description returns `None`. That is distinct
    /// from a symbol whose description is the empty string.
    #[must_use]
    pub fn description(&self) -> Option<&JsString> {
        self.0.description.as_ref()
    }

    /// Returns the predefined atom represented by this identity, if any.
    #[must_use]
    pub fn predefined_atom(&self) -> Option<PredefinedAtom> {
        self.0.predefined
    }

    /// Tests identity without inspecting the atom description.
    #[must_use]
    pub fn is_same_identity(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Returns whether the owning atom table has already been dropped.
    #[must_use]
    pub fn is_orphaned(&self) -> bool {
        self.0.owner.upgrade().is_none()
    }
}

impl PartialEq for Atom {
    fn eq(&self, other: &Self) -> bool {
        self.is_same_identity(other)
    }
}

impl Eq for Atom {}

impl Hash for Atom {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Atom")
            .field("kind", &self.kind())
            .field(
                "description_code_units",
                &self.description().map(JsString::len),
            )
            .field("predefined", &self.predefined_atom())
            .field("orphaned", &self.is_orphaned())
            .finish_non_exhaustive()
    }
}

/// A JavaScript property key.
///
/// Use [`AtomTable::property_key_from_string`] or
/// [`AtomTable::property_key_from_symbol`] to construct validated keys. Private
/// names are deliberately rejected by the symbol conversion.
///
/// Code outside this crate cannot access or mutate the validated
/// representation:
///
/// ```compile_fail
/// use quickjs_runtime::PropertyKey;
///
/// fn expose_representation(key: PropertyKey) {
///     let PropertyKey(inner) = key;
/// }
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PropertyKey(PropertyKeyRepr);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum PropertyKeyRepr {
    Index(ArrayIndex),
    Atom(Atom),
}

impl PropertyKey {
    /// Creates an integer property key.
    #[must_use]
    pub const fn from_index(index: ArrayIndex) -> Self {
        Self(PropertyKeyRepr::Index(index))
    }

    fn from_atom(atom: Atom) -> Self {
        Self(PropertyKeyRepr::Atom(atom))
    }

    pub(crate) fn from_validated_atom(atom: Atom) -> Self {
        debug_assert_eq!(atom.kind(), AtomKind::String);
        Self::from_atom(atom)
    }

    pub(crate) fn from_validated_symbol(atom: Atom) -> Self {
        debug_assert!(matches!(
            atom.kind(),
            AtomKind::GlobalSymbol | AtomKind::Symbol
        ));
        Self::from_atom(atom)
    }

    /// Creates an internal private-name key. This is deliberately
    /// crate-private: ECMAScript `ToPropertyKey` and public reflection must
    /// continue to reject private names.
    pub(crate) fn from_private_atom(atom: Atom) -> Self {
        debug_assert_eq!(atom.kind(), AtomKind::Private);
        Self::from_atom(atom)
    }

    /// Returns the array index when this is an integer property key.
    #[must_use]
    pub const fn as_index(&self) -> Option<ArrayIndex> {
        match &self.0 {
            PropertyKeyRepr::Index(index) => Some(*index),
            PropertyKeyRepr::Atom(_) => None,
        }
    }

    /// Returns the validated public atom when this is an atom property key.
    #[must_use]
    pub fn as_atom(&self) -> Option<&Atom> {
        match &self.0 {
            PropertyKeyRepr::Index(_) => None,
            PropertyKeyRepr::Atom(atom) => Some(atom),
        }
    }
}

impl From<ArrayIndex> for PropertyKey {
    fn from(index: ArrayIndex) -> Self {
        Self::from_index(index)
    }
}

/// Resource ceilings for one atom table.
///
/// All ceilings are inclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomLimits {
    /// Maximum number of simultaneously live atoms.
    pub max_live_atoms: u32,
    /// Maximum total UTF-16 code units in live atom descriptions.
    pub max_live_description_code_units: u64,
    /// Maximum number of live or not-yet-collected weak interner slots.
    pub max_interner_slots: u32,
}

impl AtomLimits {
    /// Creates a set of inclusive atom resource ceilings.
    #[must_use]
    pub const fn new(
        max_live_atoms: u32,
        max_live_description_code_units: u64,
        max_interner_slots: u32,
    ) -> Self {
        Self {
            max_live_atoms,
            max_live_description_code_units,
            max_interner_slots,
        }
    }
}

impl Default for AtomLimits {
    fn default() -> Self {
        Self {
            max_live_atoms: MAX_ATOM_ENTRIES,
            max_live_description_code_units: u64::from(MAX_ATOM_ENTRIES),
            max_interner_slots: MAX_ATOM_ENTRIES,
        }
    }
}

/// Current logical atom-table resource usage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AtomUsage {
    /// Number of live atom entries.
    pub live_atoms: u32,
    /// UTF-16 code units in live atom descriptions.
    pub live_description_code_units: u64,
    /// Live or not-yet-collected weak interner slots.
    pub interner_slots: u32,
}

struct TableState {
    usage: Cell<AtomUsage>,
}

impl Drop for AtomEntry {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let current = owner.usage.get();
        let description_code_units = self
            .description
            .as_ref()
            .map_or(0, |description| u64::from(description.len()));
        owner.usage.set(AtomUsage {
            live_atoms: current.live_atoms.saturating_sub(1),
            live_description_code_units: current
                .live_description_code_units
                .saturating_sub(description_code_units),
            interner_slots: current.interner_slots,
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum InternNamespace {
    String,
    GlobalSymbol,
}

impl InternNamespace {
    const fn atom_kind(self) -> AtomKind {
        match self {
            Self::String => AtomKind::String,
            Self::GlobalSymbol => AtomKind::GlobalSymbol,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct BucketKey {
    namespace: InternNamespace,
    content_hash: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReclaimScope {
    None,
    TargetBucket,
    AllBuckets,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReclaimPlan {
    scope: ReclaimScope,
    total_dead: u32,
    target_dead: u32,
}

impl ReclaimPlan {
    const NONE: Self = Self {
        scope: ReclaimScope::None,
        total_dead: 0,
        target_dead: 0,
    };
}

type InternBucket = Vec<Weak<AtomEntry>>;

/// One runtime's bounded atom identities and weak content interners.
pub struct AtomTable {
    state: Arc<TableState>,
    limits: AtomLimits,
    hash_builder: RandomState,
    buckets: HashMap<BucketKey, InternBucket, RandomState>,
    predefined: Vec<Atom>,
}

impl AtomTable {
    /// Creates a bounded table and transactionally installs all predefined
    /// atoms.
    ///
    /// # Errors
    ///
    /// Returns a structured error for an invalid ceiling, a ceiling below
    /// predefined startup usage, or a recoverable backing-storage allocation
    /// failure.
    pub fn new(limits: AtomLimits) -> Result<Self, AtomError> {
        Self::try_new(limits)
    }

    /// Creates a bounded table and transactionally installs all predefined
    /// atoms.
    ///
    /// This is an alias of [`Self::new`] for callers that prefer explicit
    /// fallible-constructor naming.
    ///
    /// # Errors
    ///
    /// Returns a structured error for an invalid ceiling, a ceiling below
    /// predefined startup usage, or a recoverable backing-storage allocation
    /// failure.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "the user-selected Arc ownership is intentionally runtime-local through Cell"
    )]
    pub fn try_new(limits: AtomLimits) -> Result<Self, AtomError> {
        validate_limits(limits)?;
        validate_startup_limits(limits)?;

        let hash_builder = RandomState::new();
        let mut buckets = HashMap::with_hasher(RandomState::new());
        buckets
            .try_reserve(PREDEFINED_INTERNER_SLOTS as usize)
            .map_err(|_| AtomError::AllocationFailed {
                target: AtomAllocationTarget::InternerBuckets,
                additional: PREDEFINED_INTERNER_SLOTS as usize,
            })?;

        let mut predefined = Vec::new();
        predefined
            .try_reserve_exact(PredefinedAtom::COUNT)
            .map_err(|_| AtomError::AllocationFailed {
                target: AtomAllocationTarget::PredefinedAtoms,
                additional: PredefinedAtom::COUNT,
            })?;

        let mut table = Self {
            state: Arc::new(TableState {
                usage: Cell::new(AtomUsage::default()),
            }),
            limits,
            hash_builder,
            buckets,
            predefined,
        };

        for predefined_atom in PredefinedAtom::ALL {
            let spec = predefined_atom.spec();
            let description = JsString::from_utf8(spec.text)?;
            let atom = table.insert_predefined(predefined_atom, spec, description)?;
            table.predefined.push(atom);
        }

        Ok(table)
    }

    /// Returns the configured inclusive ceilings.
    #[must_use]
    pub const fn limits(&self) -> AtomLimits {
        self.limits
    }

    /// Returns exact logical usage.
    ///
    /// Dead weak interner slots remain included until they are touched by an
    /// insertion or removed by [`Self::collect_dead`].
    #[must_use]
    pub fn usage(&self) -> AtomUsage {
        self.state.usage.get()
    }

    /// Removes every dead weak slot and returns the number removed.
    pub fn collect_dead(&mut self) -> u32 {
        let mut removed = 0_u32;
        self.buckets.retain(|_, bucket| {
            removed = removed.saturating_add(prune_dead_slots(bucket));
            !bucket.is_empty()
        });
        self.decrease_interner_slots(removed);
        removed
    }

    /// Drops one transaction-owned string atom and removes its exact weak
    /// interner slot when no other owner retained the identity.
    pub(crate) fn rollback_interned_string(&mut self, atom: Atom) {
        debug_assert_eq!(atom.kind(), AtomKind::String);
        debug_assert!(
            atom.0
                .owner
                .upgrade()
                .is_some_and(|owner| { Arc::ptr_eq(&owner, &self.state) })
        );
        if atom.predefined_atom().is_some() {
            return;
        }
        let description = atom
            .description()
            .expect("string atom has a description")
            .clone();
        let key = BucketKey {
            namespace: InternNamespace::String,
            content_hash: self.content_hash(InternNamespace::String, &description),
        };
        let identity = Arc::as_ptr(&atom.0);
        drop(atom);

        let mut remove_bucket = false;
        let removed = self.buckets.get_mut(&key).is_some_and(|bucket| {
            let Some(index) = bucket.iter().position(|entry| {
                std::ptr::eq(entry.as_ptr(), identity) && entry.strong_count() == 0
            }) else {
                return false;
            };
            bucket.swap_remove(index);
            remove_bucket = bucket.is_empty();
            true
        });
        if remove_bucket {
            self.buckets.remove(&key);
        }
        self.decrease_interner_slots(u32::from(removed));
    }

    /// Content-interns a JavaScript string atom.
    ///
    /// An interner miss is copied into a compact string leaf so a small atom
    /// cannot retain a larger rope graph.
    ///
    /// # Errors
    ///
    /// Returns a structured limit or allocation error.
    pub fn intern_string(&mut self, description: &JsString) -> Result<Atom, AtomError> {
        self.intern_description(InternNamespace::String, description)
    }

    /// Content-interns a key in the global symbol registry.
    ///
    /// This namespace is disjoint from string atoms.
    ///
    /// # Errors
    ///
    /// Returns a structured limit or allocation error.
    pub fn intern_global_symbol(&mut self, description: &JsString) -> Result<Atom, AtomError> {
        self.intern_description(InternNamespace::GlobalSymbol, description)
    }

    /// Creates a fresh symbol, optionally without a description.
    ///
    /// `None` and `Some(empty_string)` remain observably distinct.
    ///
    /// # Errors
    ///
    /// Returns a structured limit or allocation error.
    pub fn new_unique_symbol(&mut self, description: Option<&JsString>) -> Result<Atom, AtomError> {
        self.insert_unique(AtomKind::Symbol, description, None)
    }

    /// Creates a fresh private name.
    ///
    /// Private identities cannot be converted into public [`PropertyKey`]s by
    /// this table.
    ///
    /// # Errors
    ///
    /// Returns a structured limit or allocation error.
    pub fn new_private_name(&mut self, description: &JsString) -> Result<Atom, AtomError> {
        self.insert_unique(AtomKind::Private, Some(description), None)
    }

    /// Returns one table-owned predefined atom.
    ///
    /// The predefined table is installed atomically by the constructor, and
    /// every `PredefinedAtom` has a valid zero-based index.
    #[must_use]
    pub fn predefined(&self, atom: PredefinedAtom) -> Atom {
        self.predefined[atom.index()].clone()
    }

    /// Converts a JavaScript string into its property-key representation.
    ///
    /// Canonical decimal indices use the unallocated integer representation.
    /// Every other spelling is content-interned as a string atom.
    ///
    /// # Errors
    ///
    /// Returns a structured limit or allocation error when a string atom is
    /// required.
    pub fn property_key_from_string(
        &mut self,
        description: &JsString,
    ) -> Result<PropertyKey, AtomError> {
        if let Some(index) = ArrayIndex::parse_property_key(description) {
            Ok(PropertyKey::from_index(index))
        } else {
            self.intern_string(description).map(PropertyKey::from_atom)
        }
    }

    /// Validates and converts a public symbol atom into a property key.
    ///
    /// # Errors
    ///
    /// Rejects orphaned or foreign atoms, string atoms, and private names.
    pub fn property_key_from_symbol(&self, atom: &Atom) -> Result<PropertyKey, AtomError> {
        self.validate(atom)?;
        match atom.kind() {
            AtomKind::GlobalSymbol | AtomKind::Symbol => Ok(PropertyKey::from_atom(atom.clone())),
            AtomKind::String => Err(AtomError::ExpectedSymbol {
                actual: AtomKind::String,
            }),
            AtomKind::Private => Err(AtomError::PrivateNameIsNotPropertyKey),
        }
    }

    /// Validates that an atom belongs to this live table.
    ///
    /// # Errors
    ///
    /// Distinguishes a dropped owner from a different live table.
    pub fn validate(&self, atom: &Atom) -> Result<(), AtomError> {
        let Some(owner) = atom.0.owner.upgrade() else {
            return Err(AtomError::OrphanedAtom);
        };
        if Arc::ptr_eq(&owner, &self.state) {
            Ok(())
        } else {
            Err(AtomError::ForeignAtom)
        }
    }

    fn insert_predefined(
        &mut self,
        predefined: PredefinedAtom,
        spec: &PredefinedAtomSpec,
        description: JsString,
    ) -> Result<Atom, AtomError> {
        match spec.kind {
            PredefinedAtomKind::String => {
                let hash = self.content_hash(InternNamespace::String, &description);
                self.publish_interned(InternNamespace::String, description, Some(predefined), hash)
            }
            PredefinedAtomKind::Private => {
                self.publish_unique(AtomKind::Private, Some(description), Some(predefined))
            }
            PredefinedAtomKind::Symbol => {
                self.publish_unique(AtomKind::Symbol, Some(description), Some(predefined))
            }
        }
    }

    fn intern_description(
        &mut self,
        namespace: InternNamespace,
        description: &JsString,
    ) -> Result<Atom, AtomError> {
        let content_hash = self.content_hash(namespace, description);
        let key = BucketKey {
            namespace,
            content_hash,
        };
        if let Some(atom) = self.find_interned(key, description) {
            return Ok(atom);
        }

        let reclaim = self.plan_reclaim(key);
        let next_usage =
            self.checked_usage_after_reclaim(1, description.len(), 1, reclaim.total_dead)?;
        let compact = JsString::from_code_units(description.code_units())?;
        self.publish_interned_prepared(key, compact, None, reclaim, next_usage)
    }

    fn find_interned(&self, key: BucketKey, description: &JsString) -> Option<Atom> {
        self.buckets.get(&key)?.iter().find_map(|weak| {
            let entry = weak.upgrade()?;
            (entry.kind == key.namespace.atom_kind()
                && entry.description.as_ref() == Some(description))
            .then_some(Atom(entry))
        })
    }

    fn plan_reclaim(&self, key: BucketKey) -> ReclaimPlan {
        let target_dead = self
            .buckets
            .get(&key)
            .map_or(0, |bucket| dead_slots(bucket));
        let slots_after_target = self.usage().interner_slots.saturating_sub(target_dead);

        if slots_after_target < self.limits.max_interner_slots {
            return if target_dead == 0 {
                ReclaimPlan::NONE
            } else {
                ReclaimPlan {
                    scope: ReclaimScope::TargetBucket,
                    total_dead: target_dead,
                    target_dead,
                }
            };
        }

        let total_dead = self.buckets.values().fold(0_u32, |total, bucket| {
            total.saturating_add(dead_slots(bucket))
        });
        ReclaimPlan {
            scope: ReclaimScope::AllBuckets,
            total_dead,
            target_dead,
        }
    }

    fn insert_unique(
        &mut self,
        kind: AtomKind,
        description: Option<&JsString>,
        predefined: Option<PredefinedAtom>,
    ) -> Result<Atom, AtomError> {
        let description_code_units = description.map_or(0, JsString::len);
        let next_usage = self.checked_usage_after(1, description_code_units, 0)?;
        let compact = description
            .map(|text| JsString::from_code_units(text.code_units()))
            .transpose()?;
        Ok(self.create_entry(kind, compact, predefined, next_usage))
    }

    fn publish_unique(
        &mut self,
        kind: AtomKind,
        description: Option<JsString>,
        predefined: Option<PredefinedAtom>,
    ) -> Result<Atom, AtomError> {
        let description_code_units = description.as_ref().map_or(0, JsString::len);
        let next_usage = self.checked_usage_after(1, description_code_units, 0)?;
        Ok(self.create_entry(kind, description, predefined, next_usage))
    }

    fn publish_interned(
        &mut self,
        namespace: InternNamespace,
        description: JsString,
        predefined: Option<PredefinedAtom>,
        content_hash: u64,
    ) -> Result<Atom, AtomError> {
        let next_usage = self.checked_usage_after(1, description.len(), 1)?;
        let key = BucketKey {
            namespace,
            content_hash,
        };
        self.publish_interned_prepared(key, description, predefined, ReclaimPlan::NONE, next_usage)
    }

    fn publish_interned_prepared(
        &mut self,
        key: BucketKey,
        description: JsString,
        predefined: Option<PredefinedAtom>,
        reclaim: ReclaimPlan,
        next_usage: AtomUsage,
    ) -> Result<Atom, AtomError> {
        let mut vacant_bucket = None;

        if let Some(bucket) = self.buckets.get_mut(&key) {
            if reclaim.target_dead == 0 {
                bucket
                    .try_reserve(1)
                    .map_err(|_| AtomError::AllocationFailed {
                        target: AtomAllocationTarget::InternerBucket,
                        additional: 1,
                    })?;
            }
        } else {
            self.buckets
                .try_reserve(1)
                .map_err(|_| AtomError::AllocationFailed {
                    target: AtomAllocationTarget::InternerBuckets,
                    additional: 1,
                })?;
            let mut bucket = Vec::new();
            bucket
                .try_reserve_exact(1)
                .map_err(|_| AtomError::AllocationFailed {
                    target: AtomAllocationTarget::InternerBucket,
                    additional: 1,
                })?;
            vacant_bucket = Some(bucket);
        }

        // Every operation after this point is infallible under Rust's global
        // allocator policy. Removing the target bucket lets the commit prune
        // other buckets without invalidating its already-reserved storage.
        let mut bucket = self
            .buckets
            .remove(&key)
            .or(vacant_bucket)
            .unwrap_or_default();
        let mut removed = match reclaim.scope {
            ReclaimScope::None => 0,
            ReclaimScope::TargetBucket | ReclaimScope::AllBuckets => prune_dead_slots(&mut bucket),
        };
        if reclaim.scope == ReclaimScope::AllBuckets {
            self.buckets.retain(|_, other| {
                removed = removed.saturating_add(prune_dead_slots(other));
                !other.is_empty()
            });
        }
        debug_assert_eq!(removed, reclaim.total_dead);

        let atom = create_entry(
            &self.state,
            key.namespace.atom_kind(),
            Some(description),
            predefined,
        );
        bucket.push(Arc::downgrade(&atom.0));
        self.buckets.insert(key, bucket);
        self.state.usage.set(next_usage);
        Ok(atom)
    }

    fn create_entry(
        &self,
        kind: AtomKind,
        description: Option<JsString>,
        predefined: Option<PredefinedAtom>,
        next_usage: AtomUsage,
    ) -> Atom {
        let atom = create_entry(&self.state, kind, description, predefined);
        self.state.usage.set(next_usage);
        atom
    }

    fn content_hash(&self, namespace: InternNamespace, description: &JsString) -> u64 {
        let mut hasher = self.hash_builder.build_hasher();
        namespace.hash(&mut hasher);
        description.hash(&mut hasher);
        hasher.finish()
    }

    fn checked_usage_after(
        &self,
        additional_atoms: u32,
        additional_description_code_units: u32,
        additional_interner_slots: u32,
    ) -> Result<AtomUsage, AtomError> {
        self.checked_usage_after_reclaim(
            additional_atoms,
            additional_description_code_units,
            additional_interner_slots,
            0,
        )
    }

    fn checked_usage_after_reclaim(
        &self,
        additional_atoms: u32,
        additional_description_code_units: u32,
        additional_interner_slots: u32,
        reclaimed_interner_slots: u32,
    ) -> Result<AtomUsage, AtomError> {
        let current = self.usage();
        let live_atoms =
            current
                .live_atoms
                .checked_add(additional_atoms)
                .ok_or(AtomError::LiveAtomLimit {
                    current: current.live_atoms,
                    additional: additional_atoms,
                    maximum: self.limits.max_live_atoms,
                })?;
        if live_atoms > self.limits.max_live_atoms {
            return Err(AtomError::LiveAtomLimit {
                current: current.live_atoms,
                additional: additional_atoms,
                maximum: self.limits.max_live_atoms,
            });
        }

        let additional_description_code_units = u64::from(additional_description_code_units);
        let live_description_code_units = current
            .live_description_code_units
            .checked_add(additional_description_code_units)
            .ok_or(AtomError::DescriptionCodeUnitLimit {
                current: current.live_description_code_units,
                additional: additional_description_code_units,
                maximum: self.limits.max_live_description_code_units,
            })?;
        if live_description_code_units > self.limits.max_live_description_code_units {
            return Err(AtomError::DescriptionCodeUnitLimit {
                current: current.live_description_code_units,
                additional: additional_description_code_units,
                maximum: self.limits.max_live_description_code_units,
            });
        }

        let effective_interner_slots = current
            .interner_slots
            .saturating_sub(reclaimed_interner_slots);
        let interner_slots = effective_interner_slots
            .checked_add(additional_interner_slots)
            .ok_or(AtomError::InternerSlotLimit {
                current: current.interner_slots,
                additional: additional_interner_slots,
                maximum: self.limits.max_interner_slots,
            })?;
        if interner_slots > self.limits.max_interner_slots {
            return Err(AtomError::InternerSlotLimit {
                current: current.interner_slots,
                additional: additional_interner_slots,
                maximum: self.limits.max_interner_slots,
            });
        }

        Ok(AtomUsage {
            live_atoms,
            live_description_code_units,
            interner_slots,
        })
    }

    fn decrease_interner_slots(&self, removed: u32) {
        if removed == 0 {
            return;
        }
        let current = self.usage();
        self.state.usage.set(AtomUsage {
            interner_slots: current.interner_slots.saturating_sub(removed),
            ..current
        });
    }
}

fn dead_slots(bucket: &[Weak<AtomEntry>]) -> u32 {
    bucket.iter().fold(0_u32, |count, entry| {
        count.saturating_add(u32::from(entry.strong_count() == 0))
    })
}

fn prune_dead_slots(bucket: &mut InternBucket) -> u32 {
    let before = bucket.len();
    bucket.retain(|entry| entry.strong_count() != 0);
    u32::try_from(before.saturating_sub(bucket.len())).unwrap_or(u32::MAX)
}

fn create_entry(
    owner: &Arc<TableState>,
    kind: AtomKind,
    description: Option<JsString>,
    predefined: Option<PredefinedAtom>,
) -> Atom {
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "atom entries share runtime-local Cell accounting without a lock"
    )]
    Atom(Arc::new(AtomEntry {
        owner: Arc::downgrade(owner),
        kind,
        description,
        predefined,
    }))
}

fn validate_limits(limits: AtomLimits) -> Result<(), AtomError> {
    if limits.max_live_atoms > MAX_ATOM_ENTRIES {
        return Err(AtomError::InvalidMaxLiveAtoms {
            configured: limits.max_live_atoms,
            maximum: MAX_ATOM_ENTRIES,
        });
    }
    Ok(())
}

fn validate_startup_limits(limits: AtomLimits) -> Result<(), AtomError> {
    if limits.max_live_atoms < PREDEFINED_ATOM_COUNT {
        return Err(AtomError::LiveAtomLimit {
            current: 0,
            additional: PREDEFINED_ATOM_COUNT,
            maximum: limits.max_live_atoms,
        });
    }
    if limits.max_live_description_code_units < PREDEFINED_DESCRIPTION_CODE_UNITS {
        return Err(AtomError::DescriptionCodeUnitLimit {
            current: 0,
            additional: PREDEFINED_DESCRIPTION_CODE_UNITS,
            maximum: limits.max_live_description_code_units,
        });
    }
    if limits.max_interner_slots < PREDEFINED_INTERNER_SLOTS {
        return Err(AtomError::InternerSlotLimit {
            current: 0,
            additional: PREDEFINED_INTERNER_SLOTS,
            maximum: limits.max_interner_slots,
        });
    }
    Ok(())
}

/// Atom collection whose growth could not be reserved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomAllocationTarget {
    /// The predefined owning atom list.
    PredefinedAtoms,
    /// The randomized interner's bucket map.
    InternerBuckets,
    /// One collision bucket in the randomized interner.
    InternerBucket,
}

/// Failures at atom-table construction and validation boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AtomError {
    /// The configured live-atom ceiling exceeds the `QuickJS` hard maximum.
    InvalidMaxLiveAtoms {
        /// Configured ceiling.
        configured: u32,
        /// `QuickJS` hard maximum.
        maximum: u32,
    },
    /// An insertion would exceed the live-atom ceiling.
    LiveAtomLimit {
        /// Usage before the attempted insertion.
        current: u32,
        /// Requested additional entries.
        additional: u32,
        /// Configured inclusive ceiling.
        maximum: u32,
    },
    /// An insertion would exceed the live-description ceiling.
    DescriptionCodeUnitLimit {
        /// Usage before the attempted insertion.
        current: u64,
        /// Requested additional UTF-16 code units.
        additional: u64,
        /// Configured inclusive ceiling.
        maximum: u64,
    },
    /// An insertion would exceed the interner-slot ceiling.
    InternerSlotLimit {
        /// Usage before the attempted insertion.
        current: u32,
        /// Requested additional weak slots.
        additional: u32,
        /// Configured inclusive ceiling.
        maximum: u32,
    },
    /// A growable atom collection could not reserve capacity.
    AllocationFailed {
        /// Collection that failed to grow.
        target: AtomAllocationTarget,
        /// Additional elements requested.
        additional: usize,
    },
    /// Compacting an atom description failed.
    String(JsStringError),
    /// The atom's owning table has already been dropped.
    OrphanedAtom,
    /// The atom belongs to a different live table.
    ForeignAtom,
    /// A string atom was supplied where a symbol was required.
    ExpectedSymbol {
        /// Actual atom namespace.
        actual: AtomKind,
    },
    /// A private identity was supplied where a public property key was needed.
    PrivateNameIsNotPropertyKey,
}

impl From<JsStringError> for AtomError {
    fn from(error: JsStringError) -> Self {
        Self::String(error)
    }
}

impl fmt::Display for AtomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMaxLiveAtoms {
                configured,
                maximum,
            } => write!(
                formatter,
                "atom live-entry limit {configured} exceeds the QuickJS maximum of {maximum}"
            ),
            Self::LiveAtomLimit {
                current,
                additional,
                maximum,
            } => write!(
                formatter,
                "atom insertion needs {additional} entries at usage {current}, exceeding the inclusive limit {maximum}"
            ),
            Self::DescriptionCodeUnitLimit {
                current,
                additional,
                maximum,
            } => write!(
                formatter,
                "atom insertion needs {additional} UTF-16 description code units at usage {current}, exceeding the inclusive limit {maximum}"
            ),
            Self::InternerSlotLimit {
                current,
                additional,
                maximum,
            } => write!(
                formatter,
                "atom insertion needs {additional} interner slots at usage {current}, exceeding the inclusive limit {maximum}"
            ),
            Self::AllocationFailed { target, additional } => write!(
                formatter,
                "could not reserve {additional} additional elements in {target}"
            ),
            Self::String(error) => error.fmt(formatter),
            Self::OrphanedAtom => formatter.write_str("atom owner has already been dropped"),
            Self::ForeignAtom => formatter.write_str("atom belongs to a different runtime table"),
            Self::ExpectedSymbol { actual } => {
                write!(formatter, "expected a symbol atom, found {actual:?}")
            }
            Self::PrivateNameIsNotPropertyKey => {
                formatter.write_str("private names cannot be used as public property keys")
            }
        }
    }
}

impl Error for AtomError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::String(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for AtomAllocationTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PredefinedAtoms => formatter.write_str("the predefined atom table"),
            Self::InternerBuckets => formatter.write_str("the atom interner bucket map"),
            Self::InternerBucket => formatter.write_str("an atom interner collision bucket"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc};

    use super::{
        AtomError, AtomKind, AtomLimits, AtomTable, AtomUsage, BucketKey, InternNamespace,
        MAX_ATOM_ENTRIES, PREDEFINED_ATOM_COUNT, PREDEFINED_DESCRIPTION_CODE_UNITS,
        PREDEFINED_INTERNER_SLOTS, PropertyKey,
    };
    use crate::{
        ArrayIndex, JsString, MAX_ARRAY_INDEX,
        predefined_atoms::{PredefinedAtom, PredefinedAtomKind},
    };

    fn string(text: &str) -> JsString {
        JsString::from_utf8(text).unwrap()
    }

    fn table() -> AtomTable {
        AtomTable::new(AtomLimits::default()).unwrap()
    }

    #[test]
    fn seeds_every_predefined_namespace_and_exact_usage() {
        let mut table = table();
        assert_eq!(
            table.usage(),
            AtomUsage {
                live_atoms: PREDEFINED_ATOM_COUNT,
                live_description_code_units: PREDEFINED_DESCRIPTION_CODE_UNITS,
                interner_slots: PREDEFINED_INTERNER_SLOTS,
            }
        );

        for predefined in PredefinedAtom::ALL {
            let spec = predefined.spec();
            let atom = table.predefined(predefined);
            let expected_kind = match spec.kind {
                PredefinedAtomKind::String => AtomKind::String,
                PredefinedAtomKind::Private => AtomKind::Private,
                PredefinedAtomKind::Symbol => AtomKind::Symbol,
            };
            assert_eq!(atom.kind(), expected_kind, "{predefined:?}");
            assert_eq!(atom.predefined_atom(), Some(predefined));
            assert_eq!(
                atom.description().unwrap().code_units().collect::<Vec<_>>(),
                spec.text.encode_utf16().collect::<Vec<_>>(),
                "{predefined:?}"
            );

            if spec.kind == PredefinedAtomKind::String {
                let reinterned = table.intern_string(&string(spec.text)).unwrap();
                assert!(atom.is_same_identity(&reinterned), "{predefined:?}");
            }
        }

        assert_eq!(
            table.predefined(PredefinedAtom::PrivateBrand).kind(),
            AtomKind::Private
        );
        assert_eq!(
            table.predefined(PredefinedAtom::SymbolToPrimitive).kind(),
            AtomKind::Symbol
        );
    }

    #[test]
    fn private_brand_and_ordinary_brand_have_distinct_identity() {
        let table = table();
        let ordinary = table.predefined(PredefinedAtom::Brand);
        let private = table.predefined(PredefinedAtom::PrivateBrand);

        assert_eq!(ordinary.description(), private.description());
        assert_eq!(ordinary.kind(), AtomKind::String);
        assert_eq!(private.kind(), AtomKind::Private);
        assert_ne!(ordinary, private);
    }

    #[test]
    fn string_interner_matches_utf16_rope_and_lone_surrogate_content() {
        let mut table = table();
        let left = JsString::from_latin1(&vec![b'a'; 8_193]).unwrap();
        let right = JsString::from_latin1(&vec![b'b'; 513]).unwrap();
        let rope = left.concat(&right).unwrap();
        let compact = JsString::from_code_units(rope.code_units()).unwrap();

        let from_rope = table.intern_string(&rope).unwrap();
        let from_compact = table.intern_string(&compact).unwrap();
        assert!(from_rope.is_same_identity(&from_compact));
        assert_eq!(from_rope.description().unwrap(), &compact);

        let lone_a = JsString::from_code_units([0xd800, u16::from(b'x')]).unwrap();
        let lone_b = JsString::from_code_units([0xd800, u16::from(b'x')]).unwrap();
        let atom_a = table.intern_string(&lone_a).unwrap();
        let atom_b = table.intern_string(&lone_b).unwrap();
        assert!(atom_a.is_same_identity(&atom_b));
        assert_eq!(
            atom_a
                .description()
                .unwrap()
                .code_units()
                .collect::<Vec<_>>(),
            vec![0xd800, u16::from(b'x')]
        );
    }

    #[test]
    fn property_strings_use_exact_array_index_boundaries() {
        let mut table = table();
        let startup = table.usage();

        for (text, expected) in [("0", 0), ("4294967294", MAX_ARRAY_INDEX)] {
            assert_eq!(
                table.property_key_from_string(&string(text)).unwrap(),
                PropertyKey::from_index(ArrayIndex::new(expected).unwrap())
            );
        }
        assert_eq!(table.usage(), startup);

        for text in ["00", "01", "-0", "4294967295"] {
            let key = table.property_key_from_string(&string(text)).unwrap();
            assert_eq!(key.as_atom().map(super::Atom::kind), Some(AtomKind::String));
        }
    }

    #[test]
    fn namespaces_and_optional_symbol_descriptions_preserve_identity() {
        let mut table = table();
        let description = string("namespace-key");

        let string_atom = table.intern_string(&description).unwrap();
        let global_a = table.intern_global_symbol(&description).unwrap();
        let global_b = table.intern_global_symbol(&description).unwrap();
        let unique_a = table.new_unique_symbol(Some(&description)).unwrap();
        let unique_b = table.new_unique_symbol(Some(&description)).unwrap();
        let private_a = table.new_private_name(&description).unwrap();
        let private_b = table.new_private_name(&description).unwrap();

        assert_ne!(string_atom, global_a);
        assert_eq!(global_a, global_b);
        assert_ne!(global_a, unique_a);
        assert_ne!(unique_a, unique_b);
        assert_ne!(private_a, private_b);
        assert_eq!(string_atom.kind(), AtomKind::String);
        assert_eq!(global_a.kind(), AtomKind::GlobalSymbol);
        assert_eq!(unique_a.kind(), AtomKind::Symbol);
        assert_eq!(private_a.kind(), AtomKind::Private);

        let missing = table.new_unique_symbol(None).unwrap();
        let empty = table.new_unique_symbol(Some(&JsString::empty())).unwrap();
        assert_ne!(missing, empty);
        assert_eq!(missing.description(), None);
        assert_eq!(empty.description(), Some(&JsString::empty()));
    }

    #[test]
    fn identity_hash_matches_identity_equality() {
        let mut table = table();
        let description = string("identity-hash-key");
        let first = table.intern_string(&description).unwrap();
        let same = table.intern_string(&description).unwrap();
        let unique = table.new_unique_symbol(Some(&description)).unwrap();
        let mut identities = HashSet::new();

        assert!(identities.insert(first));
        assert!(!identities.insert(same));
        assert!(identities.insert(unique));
    }

    #[test]
    fn dropping_last_handle_updates_usage_and_collection_removes_weak_slot() {
        let mut table = table();
        let startup = table.usage();
        let description = string("ephemeral atom key");
        let atom = table.intern_string(&description).unwrap();
        let clone = atom.clone();
        let weak = Arc::downgrade(&atom.0);

        assert_eq!(table.usage().live_atoms, startup.live_atoms + 1);
        assert_eq!(table.usage().interner_slots, startup.interner_slots + 1);
        drop(atom);
        assert_eq!(table.usage().live_atoms, startup.live_atoms + 1);
        drop(clone);
        assert_eq!(weak.strong_count(), 0);
        assert_eq!(table.usage().live_atoms, startup.live_atoms);
        assert_eq!(
            table.usage().live_description_code_units,
            startup.live_description_code_units
        );
        assert_eq!(table.usage().interner_slots, startup.interner_slots + 1);

        assert_eq!(table.collect_dead(), 1);
        assert_eq!(table.usage(), startup);

        let reinterned = table.intern_string(&description).unwrap();
        assert!(!weak.ptr_eq(&Arc::downgrade(&reinterned.0)));
    }

    #[test]
    fn rollback_removes_only_a_newly_dead_interned_slot() {
        let mut table = table();
        let startup = table.usage();
        let description = string("transactional atom");
        let atom = table.intern_string(&description).unwrap();

        table.rollback_interned_string(atom);

        assert_eq!(table.usage(), startup);
    }

    #[test]
    fn rollback_preserves_an_interned_atom_owned_elsewhere() {
        let mut table = table();
        let startup = table.usage();
        let description = string("shared transactional atom");
        let retained = table.intern_string(&description).unwrap();
        let transaction = retained.clone();
        let live = table.usage();

        table.rollback_interned_string(transaction);

        assert_eq!(table.usage(), live);
        assert_eq!(table.intern_string(&description).unwrap(), retained);
        drop(retained);
        assert_eq!(table.usage().live_atoms, startup.live_atoms);
        assert_eq!(
            table.usage().live_description_code_units,
            startup.live_description_code_units
        );
        assert_eq!(table.collect_dead(), 1);
        assert_eq!(table.usage(), startup);
    }

    #[test]
    fn rollback_preserves_a_reused_predefined_string_atom() {
        let mut table = table();
        let startup = table.usage();
        let predefined = table.predefined(PredefinedAtom::SetProperty);
        let transaction = table.intern_string(&string("set")).unwrap();

        assert_eq!(transaction, predefined);
        table.rollback_interned_string(transaction);

        assert_eq!(table.usage(), startup);
        assert_eq!(table.intern_string(&string("set")).unwrap(), predefined);
    }

    #[test]
    fn touching_a_dead_bucket_reuses_slot_without_explicit_collection() {
        let mut table = table();
        let startup = table.usage();
        let description = string("touched dead atom");
        let first = table.intern_string(&description).unwrap();
        let weak = Arc::downgrade(&first.0);
        drop(first);

        assert_eq!(table.usage().interner_slots, startup.interner_slots + 1);
        let second = table.intern_string(&description).unwrap();
        assert_eq!(weak.strong_count(), 0);
        assert_eq!(table.usage().interner_slots, startup.interner_slots + 1);
        assert_eq!(table.usage().live_atoms, startup.live_atoms + 1);
        assert!(!weak.ptr_eq(&Arc::downgrade(&second.0)));
    }

    #[test]
    fn validation_rejects_foreign_and_orphaned_atoms() {
        let mut first_table = table();
        let second_table = table();
        let atom = first_table
            .new_unique_symbol(Some(&string("owner")))
            .unwrap();

        assert_eq!(first_table.validate(&atom), Ok(()));
        assert_eq!(second_table.validate(&atom), Err(AtomError::ForeignAtom));

        drop(first_table);
        assert!(atom.is_orphaned());
        assert_eq!(second_table.validate(&atom), Err(AtomError::OrphanedAtom));
    }

    #[test]
    fn public_symbol_property_conversion_rejects_strings_and_private_names() {
        let mut table = table();
        let description = string("property symbol");
        let public = table.new_unique_symbol(Some(&description)).unwrap();
        let private = table.new_private_name(&description).unwrap();
        let string_atom = table.intern_string(&description).unwrap();

        assert_eq!(
            table.property_key_from_symbol(&public).unwrap().as_atom(),
            Some(&public)
        );
        assert_eq!(
            table.property_key_from_symbol(&private),
            Err(AtomError::PrivateNameIsNotPropertyKey)
        );
        assert_eq!(
            table.property_key_from_symbol(&string_atom),
            Err(AtomError::ExpectedSymbol {
                actual: AtomKind::String
            })
        );
    }

    #[test]
    fn startup_limits_are_inclusive_and_below_startup_is_rejected() {
        let exact = AtomLimits::new(
            PREDEFINED_ATOM_COUNT,
            PREDEFINED_DESCRIPTION_CODE_UNITS,
            PREDEFINED_INTERNER_SLOTS,
        );
        let mut table = AtomTable::new(exact).unwrap();
        assert_eq!(table.usage().live_atoms, PREDEFINED_ATOM_COUNT);

        let predefined = table.predefined(PredefinedAtom::Name);
        assert_eq!(table.intern_string(&string("name")).unwrap(), predefined);
        assert!(matches!(
            table.new_unique_symbol(None),
            Err(AtomError::LiveAtomLimit { .. })
        ));

        for limits in [
            AtomLimits::new(
                PREDEFINED_ATOM_COUNT - 1,
                PREDEFINED_DESCRIPTION_CODE_UNITS,
                PREDEFINED_INTERNER_SLOTS,
            ),
            AtomLimits::new(
                PREDEFINED_ATOM_COUNT,
                PREDEFINED_DESCRIPTION_CODE_UNITS - 1,
                PREDEFINED_INTERNER_SLOTS,
            ),
            AtomLimits::new(
                PREDEFINED_ATOM_COUNT,
                PREDEFINED_DESCRIPTION_CODE_UNITS,
                PREDEFINED_INTERNER_SLOTS - 1,
            ),
        ] {
            assert!(AtomTable::new(limits).is_err());
        }
        assert!(matches!(
            AtomTable::new(AtomLimits::new(
                MAX_ATOM_ENTRIES + 1,
                PREDEFINED_DESCRIPTION_CODE_UNITS,
                PREDEFINED_INTERNER_SLOTS
            )),
            Err(AtomError::InvalidMaxLiveAtoms { .. })
        ));
    }

    #[test]
    fn dynamic_limits_are_inclusive_and_failures_are_transactional() {
        let limits = AtomLimits::new(
            PREDEFINED_ATOM_COUNT + 1,
            PREDEFINED_DESCRIPTION_CODE_UNITS + 1,
            PREDEFINED_INTERNER_SLOTS + 1,
        );
        let mut table = AtomTable::new(limits).unwrap();
        let startup = table.usage();
        let one_code_unit = string("~");
        let atom = table.intern_string(&one_code_unit).unwrap();

        assert_eq!(
            table.usage(),
            AtomUsage {
                live_atoms: limits.max_live_atoms,
                live_description_code_units: limits.max_live_description_code_units,
                interner_slots: limits.max_interner_slots,
            }
        );
        let full = table.usage();
        assert!(matches!(
            table.new_unique_symbol(None),
            Err(AtomError::LiveAtomLimit { .. })
        ));
        assert_eq!(table.usage(), full);

        drop(atom);
        assert_eq!(table.usage().live_atoms, startup.live_atoms);
        let no_description = table.new_unique_symbol(None).unwrap();
        assert_eq!(
            table.usage().live_atoms,
            limits.max_live_atoms,
            "a reclaimed live-entry unit is reusable"
        );
        drop(no_description);

        let code_limited = AtomLimits::new(
            PREDEFINED_ATOM_COUNT + 10,
            PREDEFINED_DESCRIPTION_CODE_UNITS,
            PREDEFINED_INTERNER_SLOTS + 10,
        );
        let mut code_limited_table = AtomTable::new(code_limited).unwrap();
        let before = code_limited_table.usage();
        assert!(matches!(
            code_limited_table.intern_string(&string("not predefined")),
            Err(AtomError::DescriptionCodeUnitLimit { .. })
        ));
        assert_eq!(code_limited_table.usage(), before);

        let slot_limited = AtomLimits::new(
            PREDEFINED_ATOM_COUNT + 10,
            PREDEFINED_DESCRIPTION_CODE_UNITS + 100,
            PREDEFINED_INTERNER_SLOTS,
        );
        let mut slot_limited_table = AtomTable::new(slot_limited).unwrap();
        let before = slot_limited_table.usage();
        assert!(matches!(
            slot_limited_table.intern_string(&string("not predefined")),
            Err(AtomError::InternerSlotLimit { .. })
        ));
        assert_eq!(slot_limited_table.usage(), before);
    }

    #[test]
    fn collision_bucket_compares_full_content_and_namespace() {
        let mut table = table();
        let first_description = string("collision one");
        let second_description = string("collision two");
        let forced_hash = 7;
        let first = table
            .publish_interned(
                InternNamespace::String,
                first_description.clone(),
                None,
                forced_hash,
            )
            .unwrap();
        let second = table
            .publish_interned(
                InternNamespace::String,
                second_description.clone(),
                None,
                forced_hash,
            )
            .unwrap();
        let global = table
            .publish_interned(
                InternNamespace::GlobalSymbol,
                first_description.clone(),
                None,
                forced_hash,
            )
            .unwrap();

        assert_eq!(
            table.find_interned(
                BucketKey {
                    namespace: InternNamespace::String,
                    content_hash: forced_hash,
                },
                &first_description
            ),
            Some(first)
        );
        assert_eq!(
            table.find_interned(
                BucketKey {
                    namespace: InternNamespace::String,
                    content_hash: forced_hash,
                },
                &second_description
            ),
            Some(second)
        );
        assert_eq!(
            table.find_interned(
                BucketKey {
                    namespace: InternNamespace::GlobalSymbol,
                    content_hash: forced_hash,
                },
                &first_description
            ),
            Some(global)
        );
    }
}
