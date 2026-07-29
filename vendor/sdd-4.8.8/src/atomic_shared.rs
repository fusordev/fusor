use std::mem::forget;
use std::panic::UnwindSafe;
use std::ptr::{NonNull, null, null_mut};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::{self, Acquire, Relaxed};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicPtr;

use super::ref_counted::{RefCounted, post_load_fence, safe_load_ordering, safe_store_ordering};
use super::{Guard, Ptr, Shared, Tag};

/// [`AtomicShared`] and [`Shared`] are smart pointers that provide shared ownership of an instance
/// of type `T`.
#[derive(Debug)]
pub struct AtomicShared<T> {
    ptr: AtomicPtr<RefCounted<T>>,
}

/// A pair of [`Shared`] and [`Ptr`] of the same type.
pub type SharedPtrPair<'g, T> = (Option<Shared<T>>, Ptr<'g, T>);

impl<T: 'static> AtomicShared<T> {
    /// Creates a new [`AtomicShared`] from an instance of `T`.
    ///
    /// The type of the instance must be determined at compile-time and must not contain non-static
    /// references, as the instance can theoretically live as long as the process. For instance,
    /// `struct Disallowed<'l, T>(&'l T)` is not safe if it implements [`Drop`] because [`drop`] can
    /// be run after `'l`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::AtomicShared;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(10);
    /// ```
    #[inline]
    pub fn new(t: T) -> Self {
        Self {
            ptr: AtomicPtr::new(RefCounted::new_shared(|| t).as_ptr()),
        }
    }
}

impl<T> AtomicShared<T> {
    /// Converts a [`Shared`] instance into an [`AtomicShared`].
    ///
    /// # Panics
    ///
    /// Panics if the instance is being dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicShared, Shared};
    ///
    /// let shared: Shared<usize> = Shared::new(10);
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::from(shared);
    /// ```
    #[cfg(not(feature = "loom"))]
    #[inline]
    #[must_use]
    pub const fn from(shared: Shared<T>) -> Self {
        let ptr = shared.underlying_ptr();
        forget(shared);
        let ptr: std::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(ptr.cast_mut());
        Self { ptr }
    }

    /// Converts a [`Shared`] instance into an [`AtomicShared`] for loom testing.
    #[cfg(feature = "loom")]
    #[inline]
    #[must_use]
    pub fn from(shared: Shared<T>) -> Self {
        let ptr = shared.underlying_ptr();
        forget(shared);
        let ptr: loom::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(ptr.cast_mut());
        Self { ptr }
    }

    /// Creates a null [`AtomicShared`] that does not own any instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::AtomicShared;
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// ```
    #[cfg(not(feature = "loom"))]
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        let ptr: std::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(null_mut());
        Self { ptr }
    }

    /// Creates a null [`AtomicShared`] that does not own any instance.
    #[cfg(feature = "loom")]
    #[inline]
    #[must_use]
    pub fn null() -> Self {
        let ptr: loom::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(null_mut());
        Self { ptr }
    }

    /// Returns `true` if the [`AtomicShared`] is null.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Tag};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// atomic_shared.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed);
    /// assert!(atomic_shared.is_null(Relaxed));
    /// ```
    #[inline]
    #[must_use]
    pub fn is_null(&self, order: Ordering) -> bool {
        Tag::unset_tag(self.ptr.load(order)).is_null()
    }

    /// Loads a pointer value from the [`AtomicShared`] with the specified memory ordering.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Guard};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(11);
    /// let guard = Guard::new();
    /// let ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 11);
    /// ```
    #[inline]
    #[must_use]
    pub fn load<'g>(&self, order: Ordering, _guard: &'g Guard) -> Ptr<'g, T> {
        let ptr = Ptr::from(self.ptr.load(safe_load_ordering(order)));
        post_load_fence(order);
        ptr
    }

    /// Atomically stores the given value into the [`AtomicShared`], returning the previous value.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Guard, Shared, Tag};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(14);
    /// let guard = Guard::new();
    /// let (old, tag) = atomic_shared.swap((Some(Shared::new(15)), Tag::Second), Relaxed);
    /// assert_eq!(tag, Tag::None);
    /// assert_eq!(*old.unwrap(), 14);
    /// let (old, tag) = atomic_shared.swap((None, Tag::First), Relaxed);
    /// assert_eq!(tag, Tag::Second);
    /// assert_eq!(*old.unwrap(), 15);
    /// let (old, tag) = atomic_shared.swap((None, Tag::None), Relaxed);
    /// assert_eq!(tag, Tag::First);
    /// assert!(old.is_none());
    /// ```
    #[inline]
    pub fn swap(
        &self,
        new: (Option<Shared<T>>, Tag),
        mut order: Ordering,
    ) -> (Option<Shared<T>>, Tag) {
        let desired = Tag::update_tag(
            new.0.as_ref().map_or_else(null, Shared::underlying_ptr),
            new.1,
        )
        .cast_mut();
        order = safe_load_ordering(safe_store_ordering(order));
        let prev = self.ptr.swap(desired, order);
        post_load_fence(order);
        let tag = Tag::into_tag(prev);
        let prev_ptr = Tag::unset_tag(prev).cast_mut();
        forget(new);
        (NonNull::new(prev_ptr).map(Shared::from), tag)
    }

    /// Returns the current [`Tag`] of the [`AtomicShared`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Tag};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// assert_eq!(atomic_shared.tag(Relaxed), Tag::None);
    /// ```
    #[inline]
    #[must_use]
    pub fn tag(&self, order: Ordering) -> Tag {
        Tag::into_tag(self.ptr.load(order))
    }

    /// Conditionally sets a new [`Tag`] if the provided condition is satisfied.
    ///
    /// Returns `true` if the new [`Tag`] has been successfully set.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Tag};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::null();
    /// assert!(atomic_shared.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed));
    /// assert_eq!(atomic_shared.tag(Relaxed), Tag::Both);
    /// ```
    #[inline]
    pub fn update_tag_if<F: FnMut(Ptr<T>) -> bool>(
        &self,
        tag: Tag,
        mut condition: F,
        set_order: Ordering,
        fetch_order: Ordering,
    ) -> bool {
        self.ptr
            .fetch_update(
                set_order,
                fetch_order,
                #[inline]
                |ptr| {
                    if condition(Ptr::from(ptr)) {
                        Some(Tag::update_tag(ptr, tag).cast_mut())
                    } else {
                        None
                    }
                },
            )
            .is_ok()
    }

    /// Atomically stores `new` into the [`AtomicShared`] if the current value matches `current`.
    ///
    /// Returns the previously held value and the updated [`Ptr`] on success.
    ///
    /// # Errors
    ///
    /// Returns `Err` containing the supplied [`Shared`] and the current [`Ptr`] on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Guard, Shared, Tag};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// atomic_shared.update_tag_if(Tag::Both, |_| true, Relaxed, Relaxed);
    /// assert!(atomic_shared.compare_exchange(
    ///     ptr, (Some(Shared::new(18)), Tag::First), Relaxed, Relaxed, &guard).is_err());
    ///
    /// ptr.set_tag(Tag::Both);
    /// let old: Shared<usize> = atomic_shared.compare_exchange(
    ///     ptr,
    ///     (Some(Shared::new(18)), Tag::First),
    ///     Relaxed,
    ///     Relaxed,
    ///     &guard).unwrap().0.unwrap();
    /// assert_eq!(*old, 17);
    /// drop(old);
    ///
    /// assert!(atomic_shared.compare_exchange(
    ///     ptr, (Some(Shared::new(19)), Tag::None), Relaxed, Relaxed, &guard).is_err());
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    /// ```
    #[inline]
    pub fn compare_exchange<'g>(
        &self,
        current: Ptr<'g, T>,
        new: (Option<Shared<T>>, Tag),
        mut success: Ordering,
        failure: Ordering,
        _guard: &'g Guard,
    ) -> Result<SharedPtrPair<'g, T>, SharedPtrPair<'g, T>> {
        let desired = Tag::update_tag(
            new.0.as_ref().map_or_else(null, Shared::underlying_ptr),
            new.1,
        )
        .cast_mut();
        success = safe_load_ordering(safe_store_ordering(success));
        match self.ptr.compare_exchange(
            current.underlying_ptr().cast_mut(),
            desired,
            success,
            failure,
        ) {
            Ok(prev) => {
                post_load_fence(success);
                let prev_shared = NonNull::new(Tag::unset_tag(prev).cast_mut()).map(Shared::from);
                forget(new);
                Ok((prev_shared, Ptr::from(desired)))
            }
            Err(actual) => Err((new.0, Ptr::from(actual))),
        }
    }

    /// Atomically stores `new` into the [`AtomicShared`] if the current value matches `current`.
    ///
    /// This method may spuriously fail even when the comparison succeeds.
    ///
    /// Returns the previously held value and the updated [`Ptr`] on success.
    ///
    /// # Errors
    ///
    /// Returns `Err` containing the supplied [`Shared`] and the current [`Ptr`] on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Guard, Shared, Tag};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// while let Err((_, actual)) = atomic_shared.compare_exchange_weak(
    ///     ptr,
    ///     (Some(Shared::new(18)), Tag::First),
    ///     Relaxed,
    ///     Relaxed,
    ///     &guard) {
    ///     ptr = actual;
    /// }
    ///
    /// let mut ptr = atomic_shared.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 18);
    /// ```
    #[inline]
    pub fn compare_exchange_weak<'g>(
        &self,
        current: Ptr<'g, T>,
        new: (Option<Shared<T>>, Tag),
        mut success: Ordering,
        failure: Ordering,
        _guard: &'g Guard,
    ) -> Result<SharedPtrPair<'g, T>, SharedPtrPair<'g, T>> {
        let desired = Tag::update_tag(
            new.0.as_ref().map_or_else(null, Shared::underlying_ptr),
            new.1,
        )
        .cast_mut();
        success = safe_load_ordering(safe_store_ordering(success));
        match self.ptr.compare_exchange_weak(
            current.underlying_ptr().cast_mut(),
            desired,
            success,
            failure,
        ) {
            Ok(prev) => {
                post_load_fence(success);
                let prev_shared = NonNull::new(Tag::unset_tag(prev).cast_mut()).map(Shared::from);
                forget(new);
                Ok((prev_shared, Ptr::from(desired)))
            }
            Err(actual) => Err((new.0, Ptr::from(actual))),
        }
    }

    /// Clones `self` including tags using the specified memory ordering.
    ///
    /// If `self` is not null, this will always return a non-null [`AtomicShared`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Guard};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(59);
    /// let guard = Guard::new();
    /// let atomic_shared_clone = atomic_shared.clone(Relaxed, &guard);
    /// let ptr = atomic_shared_clone.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 59);
    /// ```
    #[inline]
    #[must_use]
    pub fn clone(&self, order: Ordering, guard: &Guard) -> AtomicShared<T> {
        self.get_shared(order, guard)
            .map_or_else(Self::null, |s| Self::from(s))
    }

    /// Attempts to create a [`Shared`] from `self` by acquiring a strong reference.
    ///
    /// If `self` is not null, this will always succeed.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Guard, Shared};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(47);
    /// let guard = Guard::new();
    /// let shared: Shared<usize> = atomic_shared.get_shared(Relaxed, &guard).unwrap();
    /// assert_eq!(*shared, 47);
    /// ```
    #[inline]
    #[must_use]
    pub fn get_shared(&self, order: Ordering, _guard: &Guard) -> Option<Shared<T>> {
        let mut ptr = Tag::unset_tag(self.ptr.load(safe_load_ordering(order)));
        post_load_fence(order);
        while !ptr.is_null() {
            if unsafe { (*ptr).try_add_ref(Acquire) } {
                return NonNull::new(ptr.cast_mut()).map(Shared::from);
            }
            ptr = Tag::unset_tag(self.ptr.load(safe_load_ordering(order)));
        }
        None
    }

    /// Consumes `self` and converts it into a [`Shared`] by releasing any remaining references.
    ///
    /// Returns `None` if `self` was null and did not hold a strong reference.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicShared, Shared};
    ///
    /// let atomic_shared: AtomicShared<usize> = AtomicShared::new(55);
    /// let shared: Shared<usize> = atomic_shared.into_shared(Relaxed).unwrap();
    /// assert_eq!(*shared, 55);
    /// ```
    #[inline]
    #[must_use]
    pub fn into_shared(self, order: Ordering) -> Option<Shared<T>> {
        let ptr = self.ptr.swap(null_mut(), safe_load_ordering(order));
        post_load_fence(order);
        if let Some(underlying_ptr) = NonNull::new(Tag::unset_tag(ptr).cast_mut()) {
            return Some(Shared::from(underlying_ptr));
        }
        None
    }
}

impl<T> Clone for AtomicShared<T> {
    #[inline]
    fn clone(&self) -> AtomicShared<T> {
        self.clone(Acquire, &Guard::new())
    }
}

impl<T> Default for AtomicShared<T> {
    #[inline]
    fn default() -> Self {
        Self::null()
    }
}

impl<T> Drop for AtomicShared<T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(ptr) = NonNull::new(Tag::unset_tag(self.ptr.load(Relaxed)).cast_mut()) {
            drop(Shared::from(ptr));
        }
    }
}

// `T` needs to be `Sync` since sending `AtomicShared<T>` is analogous to sending `&T`.
unsafe impl<T: Send + Sync> Send for AtomicShared<T> {}

unsafe impl<T: Send + Sync> Sync for AtomicShared<T> {}

impl<T: UnwindSafe> UnwindSafe for AtomicShared<T> {}
