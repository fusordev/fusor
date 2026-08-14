//! Host resource table (§5.6): `Rc`-owned resources shared by every op.
//!
//! Resources are host-layer concepts — never JavaScript values, never on the
//! engine heap, never traced by the GC. JavaScript sees only the numeric
//! [`ResourceId`]. `Rc` (not `Arc`) enforces the single-owner rule: a
//! resource is only ever touched on the owner task.

use std::collections::HashMap;
use std::rc::Rc;

/// A host resource living in the [`ResourceTable`].
///
/// Implement for every host-side capability (files, sockets, and later
/// resources). The default [`Self::close`] does nothing; override it to
/// release the underlying capability deterministically.
pub trait Resource {
    /// The stable resource kind name used in diagnostics.
    fn name(&self) -> &'static str;

    /// Releases the underlying capability.
    ///
    /// Called once: either by an explicit [`ResourceTable::close`], by the
    /// final `Rc` drop, or by [`ResourceTable::close_all`] at shutdown.
    /// The default does nothing.
    fn close(self: Rc<Self>) {}
}

/// A monotonically increasing, never-reused resource identity.
///
/// Wrapping ids are rejected at allocation time (`u32::MAX` is reserved),
/// so a stale id can never alias a newer resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResourceId(u32);

impl ResourceId {
    /// Returns the numeric id JavaScript observes.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Reconstructs an id from the numeric value JavaScript supplies.
    ///
    /// Intended for the `#[op]` macro's `ResourceId` parameter
    /// specialization; callers must verify the id against
    /// [`ResourceTable::get`] / `ops::lookup_resource` before use.
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        Self(value)
    }
}

/// Resource-table failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceTableError {
    /// Every `u32` id has been issued; no further resources can be added.
    IdDomainExhausted,
    /// No resource table is installed in the op-state registry (the host
    /// builder installs one during assembly, §5.6).
    NotInstalled,
}

impl std::fmt::Display for ResourceTableError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdDomainExhausted => formatter.write_str("resource id domain is exhausted"),
            Self::NotInstalled => {
                formatter.write_str("no resource table is installed in the op-state registry")
            }
        }
    }
}

impl std::error::Error for ResourceTableError {}

/// The host-runtime-wide resource table shared by every op (§5.6).
#[derive(Default)]
pub struct ResourceTable {
    entries: HashMap<ResourceId, Rc<dyn Resource>>,
    next_id: u32,
}

impl std::fmt::Debug for ResourceTable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceTable")
            .field("entries", &self.entries.len())
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl ResourceTable {
    /// Creates an empty table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            next_id: 0,
        }
    }

    /// Registers one resource and returns its fresh id.
    ///
    /// Ids are monotonically increasing and never reused.
    ///
    /// # Errors
    ///
    /// Returns the resource unchanged when the id domain is exhausted
    /// (fail closed, `u32::MAX` is reserved).
    pub fn add(&mut self, resource: Rc<dyn Resource>) -> Result<ResourceId, ResourceTableError> {
        if self.next_id == u32::MAX {
            return Err(ResourceTableError::IdDomainExhausted);
        }
        let id = ResourceId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.entries.insert(id, resource);
        Ok(id)
    }

    /// Borrows one live resource.
    #[must_use]
    pub fn get(&self, id: ResourceId) -> Option<&Rc<dyn Resource>> {
        self.entries.get(&id)
    }

    /// Mutably borrows one live resource.
    ///
    /// Only available while the caller holds no other `Rc` clone of the
    /// resource (a shared borrow makes this return `None`), which keeps the
    /// single-owner discipline honest.
    #[must_use]
    pub fn get_mut(&mut self, id: ResourceId) -> Option<&mut (dyn Resource + 'static)> {
        let resource = self.entries.get_mut(&id)?;
        Rc::get_mut(resource)
    }

    /// Removes one resource and runs its [`Resource::close`].
    ///
    /// Returns `false` when the id is unknown. When other `Rc` clones still
    /// exist, the resource is removed from the table but `close` runs at the
    /// final `Rc` drop instead.
    #[must_use]
    pub fn close(&mut self, id: ResourceId) -> bool {
        let Some(resource) = self.entries.remove(&id) else {
            return false;
        };
        // JavaScript-observable close semantics: the hook runs immediately,
        // consuming one reference. Live `Rc` clones keep the value alive but
        // observe a logically closed resource.
        let hook = Rc::clone(&resource);
        hook.close();
        true
    }

    /// Closes every resource (shutdown sequence step ③, §7.4).
    pub fn close_all(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        for (_, resource) in entries {
            if Rc::strong_count(&resource) == 1 {
                resource.close();
            }
        }
    }

    /// Returns the number of live resources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
