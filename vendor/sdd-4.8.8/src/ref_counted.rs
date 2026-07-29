use std::alloc::{Layout, dealloc};
use std::mem::offset_of;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed, Release};

use super::collector::Collector;
use super::link::Link;

/// [`RefCounted`] stores an instance of type `T`, and a [`Link`].
#[repr(C)] // Prevent `link` from being reordered before `instance`.
pub(super) struct RefCounted<T> {
    /// Instance of type `T`.
    instance: T,
    /// Link to the next garbage instance.
    link: Link,
}

impl<T> RefCounted<T> {
    /// Creates a new [`RefCounted`] that allows ownership sharing.
    #[inline]
    pub(super) fn new_shared<F: FnOnce() -> T>(f: F) -> NonNull<RefCounted<T>> {
        let mut allocation = Box::<Self>::new_uninit();
        allocation.write(Self {
            instance: f(),
            link: Link::new_shared(|ptr: *mut Link| {
                Self::dealloc(Link::cast::<Self>(ptr, offset_of!(Self, link)));
            }),
        });
        let allocation = unsafe { allocation.assume_init() };
        NonNull::from(Box::leak(allocation))
    }

    /// Creates a new [`RefCounted`] that disallows reference counting.
    ///
    /// The reference counter field is never used until the instance is retired.
    #[inline]
    pub(super) fn new_unique<F: FnOnce() -> T>(f: F) -> NonNull<RefCounted<T>> {
        let mut allocation = Box::<Self>::new_uninit();
        allocation.write(Self {
            instance: f(),
            link: Link::new_unique(|ptr: *mut Link| {
                Self::dealloc(Link::cast::<Self>(ptr, offset_of!(Self, link)));
            }),
        });
        let allocation = unsafe { allocation.assume_init() };
        NonNull::from(Box::leak(allocation))
    }

    /// Tries to add a strong reference to the underlying instance.
    ///
    /// `order` must be as strong as `Acquire` for the caller to correctly validate the newest
    /// state of the pointer.
    #[inline]
    pub(super) fn try_add_ref(&self, order: Ordering) -> bool {
        self.link
            .ref_cnt()
            .fetch_update(
                order,
                order,
                #[inline]
                |r| {
                    if r.addr() & 1 == 1 {
                        Some(r.map_addr(|addr| addr + 2))
                    } else {
                        None
                    }
                },
            )
            .is_ok()
    }

    /// Returns a mutable reference to the instance if the number of owners is `1`.
    #[inline]
    pub(super) fn get_mut_shared(&mut self) -> Option<&mut T> {
        if self.link.ref_cnt().load(Relaxed).addr() == 1 {
            Some(&mut self.instance)
        } else {
            None
        }
    }

    /// Returns a mutable reference to the instance if it is uniquely owned.
    #[inline]
    pub(super) const fn get_mut_unique(&mut self) -> &mut T {
        &mut self.instance
    }

    /// Drops and deallocates itself.
    #[inline]
    pub(super) fn dealloc(ptr: *mut Self) {
        unsafe {
            ptr.map_addr(|addr| addr + offset_of!(Self, instance))
                .cast::<T>()
                .drop_in_place();
            dealloc(ptr.cast::<u8>(), Layout::new::<Self>());
        }
    }

    /// Adds a strong reference to the underlying instance.
    #[inline]
    pub(super) fn add_ref(&self) {
        let mut current = self.link.ref_cnt().load(Relaxed);
        loop {
            debug_assert_eq!(current.addr() & 1, 1);
            debug_assert!(current.addr() <= usize::MAX - 2, "reference count overflow");
            match self.link.ref_cnt().compare_exchange_weak(
                current,
                current.map_addr(|addr| addr + 2),
                Relaxed,
                Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => {
                    current = actual;
                }
            }
        }
    }

    /// Drops a strong reference to the underlying instance.
    ///
    /// Returns `true` if the last reference was dropped.
    #[inline]
    pub(super) fn drop_ref(&self) -> bool {
        // It does not have to be a load-acquire as everything's synchronized via the global
        // epoch.
        let mut current = self.link.ref_cnt().load(Relaxed);
        loop {
            debug_assert_ne!(current.addr(), 0);
            match self.link.ref_cnt().compare_exchange_weak(
                current,
                current.map_addr(|addr| addr.saturating_sub(2)),
                Relaxed,
                Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => {
                    current = actual;
                }
            }
        }
        current.addr() == 1
    }

    /// Returns a pointer to the instance.
    #[inline]
    pub(super) const fn inst_ptr(self_ptr: *const Self) -> *const T {
        self_ptr.cast::<T>()
    }

    /// Returns a pointer to the [`Link`].
    #[inline]
    pub(super) fn link_ptr(self_ptr: *const Self) -> *const Link {
        self_ptr
            .map_addr(|addr| addr + offset_of!(Self, link))
            .cast::<Link>()
    }

    /// Returns a non-null pointer to the instance.
    #[inline]
    pub(super) const fn inst_non_null_ptr(self_ptr: NonNull<Self>) -> NonNull<T> {
        self_ptr.cast::<T>()
    }

    /// Passes a pointer to [`RefCounted`] to the garbage collector.
    #[inline]
    pub(super) fn pass_to_collector(ptr: *mut Self) {
        Collector::collect(Collector::current(), Self::link_ptr(ptr).cast_mut());
    }
}

impl<T> Deref for RefCounted<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.instance
    }
}

/// Returns a safe memory [`Ordering`] for loading a [`RefCounted`].
#[inline]
pub(super) const fn safe_load_ordering(mo: Ordering) -> Ordering {
    if cfg!(all(
        any(target_arch = "arm", target_arch = "aarch64"),
        not(any(miri, feature = "loom")),
    )) {
        // The condition was copied from `crossbeam-utils`:
        // https://github.com/crossbeam-rs/crossbeam-utils/blob/master/src/atomic/consume.rs#L28
        mo
    } else {
        // Dependent loads are never reordered in all the supported platforms, but to be on the
        // safe side, `Relaxed` and `Release` are upgraded to `Acquire` and `AcqRel`.
        match mo {
            Relaxed => Acquire,
            Release => AcqRel,
            _ => mo,
        }
    }
}

/// Puts an appropriate memory fence after a load operation.
#[inline]
pub(super) fn post_load_fence(mo: Ordering) {
    if cfg!(all(
        any(target_arch = "arm", target_arch = "aarch64"),
        not(any(miri, feature = "loom")),
    )) && mo == Relaxed
    {
        // See `safe_load_ordering`.
        std::sync::atomic::compiler_fence(Acquire);
    }
}

/// Returns a safe memory [`Ordering`] for storing a [`RefCounted`].
#[inline]
pub(super) const fn safe_store_ordering(mo: Ordering) -> Ordering {
    match mo {
        Relaxed => Release,
        Acquire => AcqRel,
        _ => mo,
    }
}
