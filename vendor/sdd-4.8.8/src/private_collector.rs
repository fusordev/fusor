use std::alloc::{Layout, alloc, dealloc};
use std::mem::forget;
use std::panic::catch_unwind;
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use super::collector::Collector;
use super::link::Link;
use super::ref_counted::RefCounted;
use super::{Guard, Owned, Shared};

/// Private garbage collector to limit the lifetime of allocated memory chunks.
///
/// When the [`PrivateCollector`] is dropped, it performs an aggressive reclamation; it deallocates
/// all remaining memory chunks that it has collected in its lifetime without ensuring that there
/// are no active [`Ptr`](super::Ptr) or [`RawPtr`](super::RawPtr) pointing to any of the memory
/// chunks.
///
/// # Safety
///
/// Using [`PrivateCollector`] for types that do not implement [`Send`] is unsafe, as their
/// instances may be dropped on an arbitrary thread. See [`collect_owned`](Self::collect_owned) and
/// [`collect_shared`](Self::collect_shared) for more safety notes.
#[derive(Debug)]
pub struct PrivateCollector {
    /// Identifier.
    ///
    /// The memory address of the [`u8`] instance is used as its identifier.
    id: AtomicPtr<u8>,
    /// Backup garbage bag in case heap allocation fails.
    backup_garbage_bag: AtomicPtr<Link>,
}

impl PrivateCollector {
    /// Creates a new [`PrivateCollector`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::PrivateCollector;
    ///
    /// let private_collector = PrivateCollector::new();
    /// ```
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            id: AtomicPtr::new(null_mut()),
            backup_garbage_bag: AtomicPtr::new(null_mut()),
        }
    }

    /// Collects the memory owned by the supplied [`Owned`].
    ///
    /// # Safety
    ///
    /// This method is unsafe because it manually manages the lifetime of the underlying memory. The
    /// caller must ensure that the [`PrivateCollector`] is not dropped while any
    /// [`Ptr`](super::Ptr) or [`RawPtr`](super::RawPtr) still point to the memory held by the
    /// [`Owned`].
    ///
    /// Additionally, `T` must implement [`Send`] because its instances may be dropped on an
    /// arbitrary thread: for example, when the [`PrivateCollector`] is moved across threads or
    /// shared among multiple threads. The [`Send`] constraint is intentionally not enforced at
    /// compile time for more flexibility.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Owned, PrivateCollector};
    ///
    /// let private_collector = PrivateCollector::new();
    /// let owned = Owned::new(10);
    ///
    /// unsafe { private_collector.collect_owned(owned, &Guard::new()); }
    /// ```
    #[inline]
    pub unsafe fn collect_owned<T>(&self, owned: Owned<T>, guard: &Guard) {
        let ptr = RefCounted::link_ptr(owned.underlying_ptr()).cast_mut();
        forget(owned);
        self.push(ptr, guard);
    }

    /// Collects the supplied [`Shared`].
    ///
    /// Returns `true` if the last reference was dropped and the memory was successfully collected.
    ///
    /// # Safety
    ///
    /// This method is unsafe because it manually manages the lifetime of the underlying memory. The
    /// caller must ensure that the [`PrivateCollector`] is not dropped while any
    /// [`Ptr`](super::Ptr) or [`RawPtr`](super::RawPtr) still point to the memory held by the
    /// [`Shared`].
    ///
    /// Additionally, `T` must implement [`Send`] because its instances may be dropped on an
    /// arbitrary thread: for example, when the [`PrivateCollector`] is moved across threads or
    /// shared among multiple threads. The [`Send`] constraint is intentionally not enforced at
    /// compile time for more flexibility.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, PrivateCollector, Shared};
    ///
    /// let private_collector = PrivateCollector::new();
    /// let shared = Shared::new(10);
    ///
    /// let collected = unsafe { private_collector.collect_shared(shared, &Guard::new()) };
    /// assert!(collected);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn collect_shared<T>(&self, shared: Shared<T>, guard: &Guard) -> bool {
        let ptr = shared.underlying_ptr();
        forget(shared);

        if unsafe { (*ptr).drop_ref() } {
            self.push(RefCounted::link_ptr(ptr).cast_mut(), guard);
            true
        } else {
            false
        }
    }

    /// Pushes a [`Link`] into the garbage collector.
    #[inline]
    fn push(&self, ptr: *mut Link, guard: &Guard) {
        let (head, tail, len) = if self.backup_garbage_bag.load(Relaxed).is_null() {
            (ptr, ptr, 1)
        } else {
            self.export_then_splice(ptr)
        };
        if catch_unwind(|| {
            let id = self.id();
            guard.collect_private(head, tail, len, id);
        })
        .is_err()
        {
            self.fallback(head, tail);
        }
    }

    /// Returns the identifier.
    #[inline]
    fn id(&self) -> usize {
        let id = self.id.load(Relaxed);
        if id.is_null() {
            self.alloc_id()
        } else {
            id.addr()
        }
    }

    /// Allocates an identifier.
    #[cold]
    #[inline(never)]
    fn alloc_id(&self) -> usize {
        let new_id = NonNull::new(unsafe { alloc(Layout::new::<u8>()) })
            .unwrap_or_else(|| panic!("Memory allocation failed"));
        unsafe {
            new_id.write(0);
        }

        match self
            .id
            .compare_exchange(null_mut(), new_id.as_ptr(), Release, Relaxed)
        {
            Ok(_) => new_id.as_ptr().addr(),
            Err(id) => {
                unsafe {
                    dealloc(new_id.as_ptr(), Layout::new::<u8>());
                }
                id.addr()
            }
        }
    }

    /// Falls back to using the backup garbage bag.
    #[cold]
    #[inline(never)]
    fn fallback(&self, head: *mut Link, tail: *mut Link) {
        let result = self
            .backup_garbage_bag
            .fetch_update(Release, Relaxed, |ptr| unsafe {
                (*tail).set_next_ptr(ptr, Relaxed);
                Some(head)
            })
            .is_ok();
        debug_assert!(result);
    }

    /// Exports memory chunks in the backup garbage bag and splices the supplied pointer.
    ///
    /// Returns the head, tail, and length of the linked list.
    #[cold]
    #[inline(never)]
    fn export_then_splice(&self, ptr: *mut Link) -> (*mut Link, *mut Link, usize) {
        let Ok(head) = self
            .backup_garbage_bag
            .fetch_update(Relaxed, Acquire, |ptr| {
                if ptr.is_null() {
                    None
                } else {
                    Some(null_mut())
                }
            })
        else {
            return (ptr, ptr, 1);
        };

        let mut tail = head;
        let mut link_len = 2;
        loop {
            let next = unsafe { (*tail).next_ptr(Relaxed) };
            if next.is_null() {
                break;
            }
            link_len += 1;
            tail = next;
        }
        unsafe {
            (*tail).set_next_ptr(ptr, Relaxed);
            (head, ptr, link_len)
        }
    }
}

impl Default for PrivateCollector {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PrivateCollector {
    #[inline]
    fn drop(&mut self) {
        let id = self.id.swap(null_mut(), Acquire);
        if !id.is_null() {
            let guard = Guard::new();
            guard.purge(id.addr());
            unsafe {
                dealloc(id, Layout::new::<u8>());
            }
        }
        let link = self.backup_garbage_bag.swap(null_mut(), Acquire);
        Collector::dealloc(link);
    }
}
