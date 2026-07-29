use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::mem::{ManuallyDrop, offset_of, take};
use std::panic::catch_unwind;
use std::ptr::{self, NonNull, null, null_mut, replace};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release, SeqCst};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::fence;
use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize};

#[cfg(feature = "loom")]
use loom::sync::atomic::fence;
#[cfg(not(feature = "loom"))]
use saa::Lock;

use super::arena;
use super::link::Link;
use super::{Epoch, Tag};

/// [`Collector`] is a garbage collector that reclaims retired memory chunks in the thread.
/// when they are globally unreachable.
#[repr(align(128))]
pub(super) struct Collector {
    /// The current state of the collector.
    state: AtomicU8,
    /// The last known epoch value for this collector.
    epoch: Epoch,
    /// Number of operations to trigger an attempt to update the global epoch.
    next_epoch_update: u8,
    /// Slot in the arena allocator.
    arena_slot: u8,
    /// Number of active readers.
    num_readers: u32,
    /// Garbage bag.
    bag: GarbageBag,
    /// Number of retired memory chunks in private garbage bags.
    private_bag_len: AtomicUsize,
    /// Private garbage bags.
    private_bags: HashMap<usize, GarbageBag, BuildHasherDefault<PointerHasher>>,
    /// Lock for accessing private garbage bags.
    private_bag_lock: Lock,
    /// Pointer to the next [`Collector`].
    next: *mut Self,
    /// Link to the next memory chunk.
    link: Link,
}

/// The global epoch value and the head of the linked list of [`Collector`] instances.
#[derive(Default)]
pub(super) struct CollectorRoot {
    /// Global epoch.
    epoch: AtomicU8,
    /// Linked list head.
    head: AtomicPtr<Collector>,
}

/// [`Hasher`] for pointer values.
#[derive(Default)]
struct PointerHasher(u64);

/// [`Defer`] executes a closure when the scope exits.
struct Defer<T, F: FnOnce(T)> {
    drop_callback: ManuallyDrop<(T, F)>,
}

/// Garbage bag that temporarily stores memory chunks that are potentially accessible.
struct GarbageBag {
    /// Pointer to the previous garbage queue.
    prev: *mut Link,
    /// Pointer to the current garbage queue.
    current: *mut Link,
    /// Pointer to the next garbage queue.
    next: *mut Link,
}

/// [`CollectorAnchor`] helps allocate and clean up the thread-local [`Collector`].
struct CollectorAnchor;

/// Special lock implementation for `loom`.
#[cfg(feature = "loom")]
struct Lock(AtomicU8);

impl Collector {
    /// The number of quiescent states before triggering an epoch update.
    const CADENCE: u8 = 1_u8 << 7;

    /// Represents a quiescent state.
    const INACTIVE: u8 = Epoch::NUM_EPOCHS;

    /// Represents a terminated or invalid thread state.
    const INVALID: u8 = Epoch::NUM_EPOCHS << 1;

    /// Creates a new [`Collector`].
    #[inline]
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::INACTIVE),
            epoch: Epoch::from_u8(0),
            next_epoch_update: Self::CADENCE,
            arena_slot: u8::MAX,
            num_readers: 0,
            bag: GarbageBag::new(),
            private_bag_len: AtomicUsize::new(0),
            private_bags: HashMap::with_hasher(BuildHasherDefault::new()),
            private_bag_lock: Lock::new(),
            next: null_mut(),
            link: Link::new_unique(|ptr: *mut Link| {
                arena::release(Link::cast::<Self>(ptr, offset_of!(Self, link)));
            }),
        }
    }

    /// Resets the [`Collector`] for reuse.
    #[inline]
    pub(super) fn reset(&mut self, arena_slot: u8) {
        debug_assert!(self.bag.is_empty());

        self.state.store(Self::INACTIVE, Relaxed);
        self.next_epoch_update = Self::CADENCE;
        self.arena_slot = arena_slot;
    }

    /// Returns the slot ID in the arena allocator.
    #[inline]
    pub(super) const fn slot(&self) -> u8 {
        self.arena_slot
    }

    /// Accelerates garbage collection.
    #[inline]
    pub(super) const fn accelerate(this_ptr: NonNull<Self>) {
        unsafe {
            (*this_ptr.as_ptr()).next_epoch_update = 0;
        }
    }

    /// Returns the [`Collector`] attached to the current thread.
    #[inline]
    pub(super) fn current() -> NonNull<Self> {
        LOCAL_COLLECTOR.with(|local_collector| {
            let local_ptr = local_collector.get();
            unsafe {
                NonNull::new(*local_ptr).unwrap_or_else(|| {
                    let this_ptr = COLLECTOR_ANCHOR.with(CollectorAnchor::alloc);
                    (*local_ptr) = this_ptr.as_ptr();
                    this_ptr
                })
            }
        })
    }

    /// Notifies that a new [`Guard`](super::Guard) is being created.
    ///
    /// # Panics
    ///
    /// The method may panic if the number of readers has reached `u32::MAX`.
    #[inline]
    pub(super) fn new_guard(this_ptr: NonNull<Self>) {
        unsafe {
            if (*this_ptr.as_ptr()).num_readers == 0 {
                debug_assert_eq!(
                    (*this_ptr.as_ptr()).state.load(Relaxed) & Self::INACTIVE,
                    Self::INACTIVE
                );
                (*this_ptr.as_ptr()).num_readers = 1;

                // The epoch value can be any number between the last time a guard was created in
                // the thread and the most recent value of `GLOBAL_ROOT.epoch`.
                let new_epoch = Epoch::from_u8(GLOBAL_ROOT.epoch.load(Relaxed));

                // Every epoch update, pointer loading, memory retirement, and memory reclamation
                // event is always placed between a pair `SeqCst` memory barrier events (one in this
                // method, and the other one in `scan`) where those `SeqCst` memory barriers are
                // globally ordered by definition. This property ensures that a retired memory
                // region cannot be reclaimed until any threads holding a pointer to the region turn
                // inactive, because, the reclaimer needs to wait for at least two `SeqCst` barrier
                // events in `scan` to reclaim the memory region, and the fact that the other
                // threads were able to load a valid pointer means that the thread was in between
                // the same `SeqCst` barrier event pair or an older one; if the former, one of the
                // two `scan` events must have observed that the thread was active (this cannot be
                // achieved by `Release-Acquire` relationships), preventing the global epoch from
                // advancing more than once; if the latter, trivial.
                if cfg!(feature = "loom")
                    || cfg!(miri)
                    || cfg!(not(any(target_arch = "x86", target_arch = "x86_64")))
                {
                    (*this_ptr.as_ptr()).state.store(new_epoch.into(), Relaxed);
                    fence(SeqCst);
                } else {
                    // This special optimization is excerpted from
                    // [`crossbeam_epoch`](https://docs.rs/crossbeam-epoch/).
                    //
                    // The rationale behind the code is, it compiles to `lock xchg` that practically
                    // acts as a full memory barrier which is generally faster than `mfence`.
                    (*this_ptr.as_ptr()).state.swap(new_epoch.into(), SeqCst);
                }

                if (*this_ptr.as_ptr()).epoch != new_epoch {
                    (*this_ptr.as_ptr()).epoch = new_epoch;
                    if (*this_ptr.as_ptr()).next_epoch_update != Self::CADENCE {
                        Self::rotate_bags(this_ptr);
                    }
                }
            } else {
                debug_assert_eq!((*this_ptr.as_ptr()).state.load(Relaxed) & Self::INACTIVE, 0);
                assert_ne!(
                    (*this_ptr.as_ptr()).num_readers,
                    u32::MAX,
                    "Too many EBR guards"
                );
                (*this_ptr.as_ptr()).num_readers += 1;
            }
        }
    }

    /// Notifies that an existing [`Guard`](super::Guard) is being dropped.
    #[inline]
    pub(super) fn end_guard(this_ptr: NonNull<Self>) {
        unsafe {
            debug_assert_eq!((*this_ptr.as_ptr()).state.load(Relaxed) & Self::INACTIVE, 0);
            debug_assert_eq!(
                (*this_ptr.as_ptr()).state.load(Relaxed),
                u8::from((*this_ptr.as_ptr()).epoch)
            );

            if (*this_ptr.as_ptr()).num_readers == 1 {
                if (*this_ptr.as_ptr()).next_epoch_update == 0 {
                    Self::scan(this_ptr);
                    (*this_ptr.as_ptr()).next_epoch_update = Self::CADENCE - 1;
                } else if (*this_ptr.as_ptr()).next_epoch_update != Self::CADENCE
                    || Tag::into_tag(GLOBAL_ROOT.head.load(Relaxed)) == Tag::Second
                {
                    (*this_ptr.as_ptr()).next_epoch_update -= 1;
                }
                (*this_ptr.as_ptr()).num_readers = 0;

                // `Release` is needed to prevent any previous load operations in this thread from
                // passing through.
                (*this_ptr.as_ptr()).state.store(
                    u8::from((*this_ptr.as_ptr()).epoch) | Self::INACTIVE,
                    Release,
                );
            } else {
                (*this_ptr.as_ptr()).num_readers -= 1;
            }
        }
    }

    /// Returns the current global epoch value.
    #[inline]
    pub(super) fn current_epoch() -> Epoch {
        // It is called by an active `Guard` therefore it is after a `SeqCst` memory barrier. Each
        // epoch update is preceded by another `SeqCst` memory barrier, therefore those two events
        // are globally ordered. If the `SeqCst` event during the `Guard` creation happened before
        // the other `SeqCst` event, this will either load the last previous epoch value, or the
        // current value. If not, it is guaranteed that it reads the latest global epoch value.
        //
        // It is not possible to return the known epoch here since the global epoch value is rotated
        // and the local epoch may be outdated; this may lead to a situation where the caller thinks
        // that a new generation has been witnessed.
        Epoch::from_u8(GLOBAL_ROOT.epoch.load(Relaxed))
    }

    /// Returns `true` if the [`Collector`] potentially contains garbage.
    #[inline]
    pub(super) const fn has_garbage(this_ptr: NonNull<Self>) -> bool {
        unsafe { (*this_ptr.as_ptr()).next_epoch_update != Self::CADENCE }
    }

    /// Sets the flag indicating this thread has garbage, allowing epoch advancement.
    #[inline]
    pub(super) const fn set_has_garbage(this_ptr: NonNull<Self>) {
        unsafe {
            let next_epoch_update = (*this_ptr.as_ptr()).next_epoch_update;
            if next_epoch_update == Self::CADENCE {
                (*this_ptr.as_ptr()).next_epoch_update = Self::CADENCE - 1;
            }
        }
    }

    /// Collects retired memory chunks into appropriate garbage bags.
    #[inline]
    pub(super) fn collect(this_ptr: NonNull<Self>, ptr: *mut Link) {
        unsafe {
            (*this_ptr.as_ptr()).bag.push(ptr);
            Self::set_has_garbage(this_ptr);
        }
    }

    /// Collects retired memory chunks into appropriate garbage bags.
    #[inline]
    pub(super) fn collect_private(
        this_ptr: NonNull<Self>,
        head: *mut Link,
        tail: *mut Link,
        len: usize,
        id: usize,
    ) {
        unsafe {
            (*this_ptr.as_ptr()).private_bag_lock.lock_sync();
            let lock_guard = Defer::new((), |()| {
                (*this_ptr.as_ptr()).private_bag_lock.release_lock();
            });

            let garbage_bag = (*this_ptr.as_ptr())
                .private_bags
                .entry(id)
                .or_insert_with(GarbageBag::new);
            (*tail).set_next_ptr(garbage_bag.current, Relaxed);
            garbage_bag.current = head;

            // `fetch_add` must take place in a critical section.
            (*this_ptr.as_ptr()).private_bag_len.fetch_add(len, Relaxed);

            drop(lock_guard);
            Self::set_has_garbage(this_ptr);
        }
    }

    /// Purges retired memory chunks from the specified private garbage bag.
    #[inline]
    pub(super) fn purge(this_ptr: NonNull<Self>, id: usize) {
        unsafe {
            debug_assert_eq!((*this_ptr.as_ptr()).state.load(Relaxed) & Self::INACTIVE, 0);

            let mut current_ptr = GLOBAL_ROOT.head();
            while !current_ptr.is_null() {
                let next_ptr = (*current_ptr).next;
                (*current_ptr).private_bag_lock.lock_sync();
                let lock_guard = Defer::new(current_ptr, |current_ptr| {
                    (*current_ptr).private_bag_lock.release_lock();
                });

                if let Some(mut garbage_bag) = (*current_ptr).private_bags.remove(&id) {
                    (*current_ptr).private_bags.shrink_to_fit();
                    drop(lock_guard);

                    let mut link_len = 0;
                    for link in garbage_bag.pop_all() {
                        link_len += Self::dealloc(link);
                    }
                    (*current_ptr).private_bag_len.fetch_sub(link_len, Relaxed);
                }
                current_ptr = next_ptr;
            }
        }
    }

    /// Deallocates the [`Link`].
    ///
    /// Returns the number of deallocated memory chunks.
    pub(super) fn dealloc(mut link: *mut Link) -> usize {
        let mut link_len = 0;
        while !link.is_null() {
            link_len += 1;
            unsafe {
                let next_ptr = (*link).next_ptr(Relaxed);
                let _result: Result<(), _> = catch_unwind(|| {
                    // Silently ignore any errors.
                    let dealloc_fn = (*link).dealloc_fn();
                    dealloc_fn(link);
                });
                link = next_ptr;
            }
        }
        link_len
    }

    /// Clears all retired memory chunks for dropping or recycling the [`Collector`].
    ///
    /// Returns `false` if the cleanup failed.
    pub(super) fn clear<const COMPLETE_CLEANUP: bool>(this_ptr: *mut Self) -> bool {
        if COMPLETE_CLEANUP && !GLOBAL_ROOT.head().is_null() {
            // Complete cleanup failed due to a newly spawned thread.
            return false;
        }

        unsafe {
            while !(*this_ptr).bag.is_empty() || (*this_ptr).private_bag_len.load(Relaxed) != 0 {
                let garbage_queues = (*this_ptr).bag.pop_all();
                for link in garbage_queues {
                    Self::dealloc(link);
                }

                (*this_ptr).private_bag_lock.lock_sync();
                let lock_guard = Defer::new(this_ptr, |this_ptr| {
                    (*this_ptr).private_bag_lock.release_lock();
                });
                let mut private_bags = take(&mut (*this_ptr).private_bags);

                for garbage_bag in private_bags.values_mut() {
                    for link in garbage_bag.pop_all() {
                        let link_len = Self::dealloc(link);
                        (*this_ptr).private_bag_len.fetch_sub(link_len, Relaxed);
                    }
                }
                drop(lock_guard);

                // `Self::dealloc` may push more retired memory chunks into `bag`, and the fence
                // prevents the compiler from optimizing memory access to `bag`.
                std::sync::atomic::compiler_fence(Acquire);

                if COMPLETE_CLEANUP && !GLOBAL_ROOT.head().is_null() {
                    // A thread was spawned and the thread may hold a reference to a memory
                    // chunk in this `Collector`.
                    //
                    // - `drop` spawned a thread that has a pointer to a newly created instance.
                    // - The instance is pushed to this `Collector`.
                    // - The spawned thread may read the instance.
                    return false;
                }
            }

            let lock_guard = Defer::new(this_ptr, |this_ptr| {
                (*this_ptr).private_bag_lock.release_lock();
            });
            debug_assert!((*this_ptr).private_bags.iter().all(|(_, b)| b.is_empty()));
            (*this_ptr).private_bags.clear();
            (*this_ptr).private_bags.shrink_to_fit();
            drop(lock_guard);
        }

        true
    }

    /// Allocates a new [`Collector`] instance.
    #[inline]
    fn alloc() -> NonNull<Self> {
        let ptr = arena::acquire();
        Self::push(ptr.as_ptr());
        ptr
    }

    /// Pushes the [`Collector`] into the global linked list.
    #[inline]
    fn push(collector: *mut Self) {
        let mut current = GLOBAL_ROOT.head.load(Relaxed);
        loop {
            unsafe {
                (*collector).next = Tag::unset_tag(current).cast_mut();
            }

            // Keep the tag.
            let tag = Tag::into_tag(current);
            let new = Tag::update_tag(collector, tag).cast_mut();
            if let Err(actual) = GLOBAL_ROOT
                .head
                .compare_exchange(current, new, Release, Relaxed)
            {
                current = actual;
            } else {
                break;
            }
        }
    }

    /// Rotates the garbage bags and deallocates the old ones.
    #[inline]
    fn rotate_bags(this_ptr: NonNull<Self>) {
        unsafe {
            let (link, empty) = (*this_ptr.as_ptr()).bag.rotate();
            Self::dealloc(link);

            if (*this_ptr.as_ptr()).private_bag_len.load(Relaxed) != 0 {
                Self::rotate_private_bags(this_ptr, None);
            }

            if empty && (*this_ptr.as_ptr()).private_bag_len.load(Relaxed) == 0 {
                (*this_ptr.as_ptr()).next_epoch_update = Self::CADENCE;
            } else {
                (*this_ptr.as_ptr()).next_epoch_update = Self::CADENCE - 1;
            }
        }
    }

    /// Rotates the private garbage bags for all collectors.
    fn rotate_private_bags(this_ptr: NonNull<Self>, epoch: Option<Epoch>) {
        unsafe {
            (*this_ptr.as_ptr()).private_bag_lock.lock_sync();
            let _lock_guard = Defer::new((), |()| {
                (*this_ptr.as_ptr()).private_bag_lock.release_lock();
            });

            if let Some(epoch) = epoch {
                if (*this_ptr.as_ptr()).epoch == epoch {
                    return;
                }
                (*this_ptr.as_ptr()).epoch = epoch;
            }

            for garbage_bag in (*this_ptr.as_ptr()).private_bags.values_mut() {
                // Deallocation must happen in a critical section otherwise those memory chunks may
                // outlive their private collectors.
                let link = garbage_bag.rotate().0;
                let link_len = Self::dealloc(link);
                (*this_ptr.as_ptr())
                    .private_bag_len
                    .fetch_sub(link_len, Relaxed);
            }
        }
    }

    /// Scans the [`Collector`] linked list to advance the global epoch.
    fn scan(this_ptr: NonNull<Self>) {
        unsafe {
            debug_assert_eq!((*this_ptr.as_ptr()).state.load(Relaxed) & Self::INVALID, 0);

            if u8::from((*this_ptr.as_ptr()).epoch) != GLOBAL_ROOT.epoch.load(Relaxed) {
                // No need for further processing if the known epoch value is not up-to-date.
                return;
            }

            // Only one thread that acquires the chain lock is allowed to scan the thread-local
            // collectors.
            let lock_result = Self::lock_chain();
            let Ok(mut current_ptr) = lock_result else {
                return;
            };
            let guard = Defer::new((), |()| Self::unlock_chain());

            let known_epoch = (*this_ptr.as_ptr()).state.load(Relaxed);
            let mut post_ptr: *mut Collector = null_mut();
            let mut prev_ptr: *mut Collector = null_mut();
            while !current_ptr.is_null() {
                if this_ptr.as_ptr() == current_ptr {
                    prev_ptr = current_ptr;
                    current_ptr = (*this_ptr.as_ptr()).next;
                    continue;
                }

                // `Acquire` is needed in case the other thread is inactive so that this thread
                // needs to reclaim memory for the thread.
                let state = (*current_ptr).state.load(Acquire);
                let next_ptr = (*current_ptr).next;
                if (state & Self::INVALID) != 0 {
                    if (*current_ptr).private_bag_len.load(Relaxed) == 0 {
                        // The `Collector` can be collected.
                        let result = if prev_ptr.is_null() {
                            GLOBAL_ROOT
                                .head
                                .fetch_update(Release, Acquire, |p| {
                                    let tag = Tag::into_tag(p);
                                    debug_assert!(tag == Tag::First || tag == Tag::Both);
                                    if ptr::eq(Tag::unset_tag(p), current_ptr) {
                                        Some(Tag::update_tag(next_ptr, tag).cast_mut())
                                    } else {
                                        None
                                    }
                                })
                                .is_ok()
                        } else {
                            (*prev_ptr).next = next_ptr;
                            true
                        };
                        if result {
                            Self::collect(
                                this_ptr,
                                current_ptr
                                    .cast::<Link>()
                                    .map_addr(|addr| addr + offset_of!(Self, link)),
                            );
                            current_ptr = next_ptr;
                            continue;
                        }
                    } else if post_ptr.is_null() {
                        post_ptr = current_ptr;
                    }
                } else if (state & Self::INACTIVE) == 0 && state != known_epoch {
                    // Not ready for an epoch update.
                    return;
                }
                prev_ptr = current_ptr;
                current_ptr = next_ptr;
            }

            // A memory region can be retired after a `SeqCst` barrier in a `Guard`, and the memory
            // region can only be deallocated after the thread has observed three times of epoch
            // updates. This `SeqCst` fence ensures that the epoch update is strictly sequenced
            // after/before a `Guard`, enabling the event of the retirement of the memory region is
            // also globally ordered with epoch updates.
            fence(SeqCst);
            let next_epoch = Epoch::from_u8(known_epoch).next();
            GLOBAL_ROOT.epoch.store(next_epoch.into(), Relaxed);
            drop(guard);

            // Post-process invalid garbage collectors if they had non-empty private garbage bags.
            let mut mark = false;
            while !post_ptr.is_null() {
                let state = (*post_ptr).state.load(Acquire);
                if (state & Self::INVALID) != 0 {
                    mark = true;
                    if (*post_ptr).private_bag_len.load(Relaxed) != 0 {
                        debug_assert_ne!(this_ptr.as_ptr(), post_ptr);
                        Self::rotate_private_bags(
                            NonNull::new_unchecked(post_ptr),
                            Some(next_epoch),
                        );
                    }
                }
                post_ptr = (*post_ptr).next;
            }
            if mark {
                mark_scan_enforced();
            }
        }
    }

    /// Acquires the lock on the collector chain.
    #[inline]
    fn lock_chain() -> Result<*mut Self, *mut Self> {
        GLOBAL_ROOT
            .head
            .fetch_update(Acquire, Acquire, |p| {
                let tag = Tag::into_tag(p);
                if tag == Tag::First || tag == Tag::Both {
                    None
                } else {
                    Some(Tag::update_tag(p, Tag::First).cast_mut())
                }
            })
            .map(|p| Tag::unset_tag(p).cast_mut())
    }

    /// Releases the lock on the collector chain.
    #[inline]
    fn unlock_chain() {
        loop {
            let result = GLOBAL_ROOT.head.fetch_update(Release, Relaxed, |p| {
                let tag = Tag::into_tag(p);
                debug_assert!(tag == Tag::First || tag == Tag::Both);
                let new_tag = if tag == Tag::First {
                    Tag::None
                } else {
                    // Retain the mark.
                    Tag::Second
                };
                Some(Tag::update_tag(p, new_tag).cast_mut())
            });
            if result.is_ok() {
                break;
            }
        }
    }
}

impl CollectorRoot {
    /// Gets the head of the [`Collector`] linked list.
    #[inline]
    fn head(&self) -> *mut Collector {
        Tag::unset_tag(self.head.load(Acquire)).cast_mut()
    }
}

impl Hasher for PointerHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write_usize(&mut self, addr: usize) {
        self.0 = (addr ^ (addr / align_of::<usize>()) ^ (addr / align_of::<usize>() * 2)) as u64;
    }

    #[inline]
    fn write(&mut self, _bytes: &[u8]) {
        unimplemented!();
    }
}

impl<T, F: FnOnce(T)> Defer<T, F> {
    /// Creates a new [`Defer`] with the specified captured variables.
    #[inline]
    pub(crate) const fn new(captured: T, drop_callback: F) -> Self {
        Self {
            drop_callback: ManuallyDrop::new((captured, drop_callback)),
        }
    }
}

impl<T, F: FnOnce(T)> Drop for Defer<T, F> {
    #[inline]
    fn drop(&mut self) {
        let (c, f) = unsafe { ManuallyDrop::take(&mut self.drop_callback) };
        f(c);
    }
}

impl GarbageBag {
    /// Creates a new [`GarbageBag`].
    #[inline]
    const fn new() -> Self {
        Self {
            prev: null_mut(),
            current: null_mut(),
            next: null_mut(),
        }
    }

    /// Returns `true` if the [`GarbageBag`] contains no items.
    #[inline]
    const fn is_empty(&self) -> bool {
        self.prev.is_null() && self.current.is_null() && self.next.is_null()
    }

    /// Pushes a pointer onto the current queue.
    #[inline]
    fn push(&mut self, ptr: *mut Link) {
        unsafe {
            (*ptr).set_next_ptr(self.current, Relaxed);
            self.current = ptr;
        }
    }

    /// Pops all retired memory chunks from its queues.
    #[inline]
    const fn pop_all(&mut self) -> [*mut Link; 3] {
        unsafe {
            [
                replace(&raw mut self.prev, null_mut()),
                replace(&raw mut self.current, null_mut()),
                replace(&raw mut self.next, null_mut()),
            ]
        }
    }

    /// Rotates the garbage queues.
    #[inline]
    const fn rotate(&mut self) -> (*mut Link, bool) {
        let link = self.next;
        self.next = self.prev;
        self.prev = self.current;
        self.current = null_mut();

        let empty = self.next.is_null() && self.prev.is_null();
        (link, empty)
    }
}

impl CollectorAnchor {
    /// Allocates a new [`Collector`] instance.
    fn alloc(&self) -> NonNull<Collector> {
        let _: &CollectorAnchor = self;
        Collector::alloc()
    }

    /// Attempts to clean up the entire [`Collector`] chain if all entries are invalid.
    ///
    /// Returns `false` if a new `Collector` was added to the chain or if a `Collector` in a valid
    /// state was found.
    fn cleanup_chain(this_ptr: *mut Collector) -> bool {
        let lock_result = Collector::lock_chain();
        let Ok(collector_head) = lock_result else {
            return false;
        };
        let _guard = Defer::new((), |()| Collector::unlock_chain());

        unsafe {
            let mut current_ptr = collector_head;
            while !current_ptr.is_null() {
                if current_ptr != this_ptr
                    && (((*current_ptr).state.load(Acquire) & Collector::INVALID) == 0
                        || (*current_ptr).private_bag_len.load(Relaxed) != 0)
                {
                    return false;
                }
                current_ptr = (*current_ptr).next;
            }

            // `fn purge` is always called with an active `Collector`, therefore `fn purge` is not
            // affected.
            let result = GLOBAL_ROOT.head.fetch_update(Release, Relaxed, |p| {
                if Tag::unset_tag(p) == collector_head {
                    let tag = Tag::into_tag(p);
                    debug_assert!(tag == Tag::First || tag == Tag::Both);
                    Some(Tag::update_tag(null::<Collector>(), tag).cast_mut())
                } else {
                    None
                }
            });

            if result.is_ok() {
                let mut current_ptr = collector_head;
                while !current_ptr.is_null() {
                    let next_ptr = (*current_ptr).next;
                    if current_ptr != this_ptr {
                        // Do not release the current `Collector` as it may collect more memory
                        // chunks.
                        arena::release(current_ptr);
                    }
                    current_ptr = next_ptr;
                }
                true
            } else {
                false
            }
        }
    }
}

impl Drop for CollectorAnchor {
    /// Cleans up the local collector when dropped.
    #[inline]
    fn drop(&mut self) {
        unsafe {
            LOCAL_COLLECTOR.with(|local_collector| {
                let local_ptr = local_collector.get();
                if (*local_ptr).is_null() {
                    return;
                }

                if Self::cleanup_chain(*local_ptr) {
                    if Collector::clear::<true>(*local_ptr) {
                        let this_ptr = *local_ptr;
                        *local_ptr = null_mut();
                        arena::release(this_ptr);
                        return;
                    }
                    Collector::push(*local_ptr);
                }

                (**local_ptr).state.fetch_or(Collector::INVALID, Release);
                *local_ptr = null_mut();
                mark_scan_enforced();
            });
        }
    }
}

#[cfg(feature = "loom")]
impl Lock {
    const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    fn lock_sync(&self) -> bool {
        while self.0.compare_exchange(0, 1, Acquire, Acquire).is_err() {}
        true
    }

    fn release_lock(&self) -> bool {
        self.0.store(0, Release);
        true
    }
}

/// Marks the head of the chain to indicate that there is a potentially unreachable `Collector`.
fn mark_scan_enforced() {
    // `Tag::Second` indicates that there is a garbage `Collector`.
    let _result = GLOBAL_ROOT.head.fetch_update(Release, Relaxed, |p| {
        let new_tag = match Tag::into_tag(p) {
            Tag::None => Tag::Second,
            Tag::First => Tag::Both,
            Tag::Second | Tag::Both => return None,
        };
        Some(Tag::update_tag(p, new_tag).cast_mut())
    });
}

thread_local! {
    static LOCAL_COLLECTOR: UnsafeCell<*mut Collector> = const { UnsafeCell::new(null_mut()) };
    static COLLECTOR_ANCHOR: CollectorAnchor = const { CollectorAnchor };
}

/// The global and default [`CollectorRoot`].
static GLOBAL_ROOT: CollectorRoot = CollectorRoot {
    epoch: AtomicU8::new(0),
    head: AtomicPtr::new(null_mut()),
};
