use std::panic::{RefUnwindSafe, UnwindSafe};
use std::ptr::NonNull;

use super::Epoch;
use super::collector::Collector;
use super::link::DeferredClosure;
use super::link::Link;

/// [`Guard`] allows the user to read [`AtomicOwned`](super::AtomicOwned),
/// [`AtomicShared`](super::AtomicShared), and [`AtomicRaw`](super::AtomicRaw) while keeping the
/// underlying instance pinned in the current thread.
///
/// [`Guard`] internally prevents the global epoch from advancing past the value announced by
/// the current thread, thereby preventing reachable instances in the thread from being garbage
/// collected.
#[derive(Debug)]
pub struct Guard {
    collector_ptr: NonNull<Collector>,
}

impl Guard {
    /// Creates a new [`Guard`].
    ///
    /// # Panics
    ///
    /// Panics if the maximum number of [`Guard`] instances in a thread, `u32::MAX`, is exceeded.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Guard;
    ///
    /// let guard = Guard::new();
    /// ```
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        let collector_ptr = Collector::current();
        Collector::new_guard(collector_ptr);
        Self { collector_ptr }
    }

    /// Returns the epoch currently witnessed by the thread.
    ///
    /// This method can be used to determine whether a retired memory region is potentially
    /// reachable. A memory region retired in a witnessed [`Epoch`] can be deallocated only after
    /// the thread has observed three subsequent epochs. For instance, if the witnessed epoch
    /// value is `1` while the global epoch is `2`, and an instance is retired in the same thread,
    /// that instance can be dropped when the thread witnesses epoch `4`, which is three epochs
    /// away from `1`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Owned};
    /// use std::sync::atomic::AtomicBool;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// static DROPPED: AtomicBool = AtomicBool::new(false);
    ///
    /// struct D(&'static AtomicBool);
    ///
    /// impl Drop for D {
    ///     fn drop(&mut self) {
    ///         self.0.store(true, Relaxed);
    ///     }
    /// }
    ///
    /// let owned = Owned::new(D(&DROPPED));
    ///
    /// let epoch_before = Guard::new().epoch();
    ///
    /// drop(owned);
    /// assert!(!DROPPED.load(Relaxed));
    ///
    /// while Guard::new().epoch() == epoch_before {
    ///     assert!(!DROPPED.load(Relaxed));
    /// }
    ///
    /// while Guard::new().epoch() == epoch_before.next() {
    ///     assert!(!DROPPED.load(Relaxed));
    /// }
    ///
    /// while Guard::new().epoch() == epoch_before.next().next() {
    ///     assert!(!DROPPED.load(Relaxed));
    /// }
    ///
    /// assert!(DROPPED.load(Relaxed));
    /// assert_eq!(Guard::new().epoch(), epoch_before.next().next().next());
    /// ```
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> Epoch {
        Collector::current_epoch()
    }

    /// Returns `true` if the thread-local garbage collector may contain garbage.
    ///
    /// This method may return `true` even when no garbage exists if
    /// [`set_has_garbage`](Self::set_has_garbage) was recently called.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Shared};
    ///
    /// let guard = Guard::new();
    ///
    /// assert!(!guard.has_garbage());
    ///
    /// drop(Shared::new(1_usize));
    /// assert!(guard.has_garbage());
    /// ```
    #[inline]
    #[must_use]
    pub const fn has_garbage(&self) -> bool {
        Collector::has_garbage(self.collector_ptr)
    }

    /// Sets the garbage flag to allow the thread to advance the global epoch.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Guard;
    ///
    /// let guard = Guard::new();
    ///
    /// assert!(!guard.has_garbage());
    /// guard.set_has_garbage();
    /// assert!(guard.has_garbage());
    /// ```
    #[inline]
    pub const fn set_has_garbage(&self) {
        Collector::set_has_garbage(self.collector_ptr);
    }

    /// Signals to the [`Guard`] that it should try to advance to a new epoch when dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Guard;
    ///
    /// let guard = Guard::new();
    ///
    /// let epoch = guard.epoch();
    /// guard.accelerate();
    ///
    /// drop(guard);
    ///
    /// assert_ne!(epoch, Guard::new().epoch());
    /// ```
    #[inline]
    pub const fn accelerate(&self) {
        Collector::accelerate(self.collector_ptr);
    }

    /// Executes the supplied closure at a later point in time.
    ///
    /// The closure is guaranteed to execute after all [`Guard`] instances present when the method
    /// was invoked have been dropped, though the exact timing is non-deterministic.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Guard;
    ///
    /// let guard = Guard::new();
    /// guard.defer_execute(|| println!("deferred"));
    /// ```
    #[inline]
    pub fn defer_execute<F: 'static + FnOnce()>(&self, f: F) {
        Collector::collect(self.collector_ptr, DeferredClosure::alloc(f));
    }

    /// Collects memory chunks into the thread-local private garbage collector.
    pub(super) fn collect_private(&self, head: *mut Link, tail: *mut Link, len: usize, id: usize) {
        Collector::collect_private(self.collector_ptr, head, tail, len, id);
    }

    /// Purges all memory regions from the thread-local private garbage collector.
    pub(super) fn purge(&self, id: usize) {
        Collector::purge(self.collector_ptr, id);
    }
}

impl Default for Guard {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        Collector::end_guard(self.collector_ptr);
    }
}

impl RefUnwindSafe for Guard {}

impl UnwindSafe for Guard {}
