use std::mem::forget;
use std::panic::UnwindSafe;
use std::ptr::{NonNull, null, null_mut};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::{self, Relaxed};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicPtr;

use super::ref_counted::{RefCounted, post_load_fence, safe_load_ordering, safe_store_ordering};
use super::{Guard, Owned, Ptr, Tag};

/// [`AtomicOwned`] and [`Owned`] are smart pointers that exclusively own an instance of type `T`.
#[derive(Debug)]
pub struct AtomicOwned<T> {
    ptr: AtomicPtr<RefCounted<T>>,
}

/// A pair of [`Owned`] and [`Ptr`] of the same type.
pub type OwnedPtrPair<'g, T> = (Option<Owned<T>>, Ptr<'g, T>);

impl<T: 'static> AtomicOwned<T> {
    /// Creates a new [`AtomicOwned`] from an instance of `T`.
    ///
    /// The type of the instance must be determined at compile-time and must not contain non-static
    /// references, as the instance can theoretically live as long as the process. For instance,
    /// `struct Disallowed<'l, T>(&'l T)` is not safe if it implements [`Drop`] because [`drop`] can
    /// be run after `'l`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::AtomicOwned;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(10);
    /// ```
    #[inline]
    pub fn new(t: T) -> Self {
        Self {
            ptr: AtomicPtr::new(RefCounted::new_unique(|| t).as_ptr()),
        }
    }
}

impl<T> AtomicOwned<T> {
    /// Converts an [`Owned`] instance into an [`AtomicOwned`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicOwned, Owned};
    ///
    /// let owned: Owned<usize> = Owned::new(10);
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::from(owned);
    /// ```
    #[cfg(not(feature = "loom"))]
    #[inline]
    #[must_use]
    pub const fn from(owned: Owned<T>) -> Self {
        let ptr = owned.underlying_ptr();
        forget(owned);
        let ptr: std::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(ptr.cast_mut());
        Self { ptr }
    }

    /// Converts an [`Owned`] instance into an [`AtomicOwned`] for loom testing.
    #[cfg(feature = "loom")]
    #[inline]
    #[must_use]
    pub fn from(owned: Owned<T>) -> Self {
        let ptr = owned.underlying_ptr();
        forget(owned);
        let ptr: loom::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(ptr.cast_mut());
        Self { ptr }
    }

    /// Creates a null [`AtomicOwned`] that does not own any instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::AtomicOwned;
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// ```
    #[cfg(not(feature = "loom"))]
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        let ptr: std::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(null_mut());
        Self { ptr }
    }

    /// Creates a null [`AtomicOwned`] that does not own any instance (loom variant).
    #[cfg(feature = "loom")]
    #[inline]
    #[must_use]
    pub fn null() -> Self {
        let ptr: loom::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(null_mut());
        Self { ptr }
    }

    /// Returns `true` if the [`AtomicOwned`] is null and does not own any instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicOwned, Tag};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// atomic_owned.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed);
    /// assert!(atomic_owned.is_null(Relaxed));
    /// ```
    #[inline]
    #[must_use]
    pub fn is_null(&self, order: Ordering) -> bool {
        Tag::unset_tag(self.ptr.load(order)).is_null()
    }

    /// Loads a pointer value from the [`AtomicOwned`] with the specified memory ordering.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicOwned, Guard};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(11);
    /// let guard = Guard::new();
    /// let ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 11);
    /// ```
    #[inline]
    #[must_use]
    pub fn load<'g>(&self, order: Ordering, _guard: &'g Guard) -> Ptr<'g, T> {
        let ptr = Ptr::from(self.ptr.load(safe_load_ordering(order)));
        post_load_fence(order);
        ptr
    }

    /// Atomically stores the given value into the [`AtomicOwned`], returning the previous value.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicOwned, Guard, Owned, Tag};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(14);
    /// let guard = Guard::new();
    /// let (old, tag) = atomic_owned.swap((Some(Owned::new(15)), Tag::Second), Relaxed);
    /// assert_eq!(tag, Tag::None);
    /// assert_eq!(*old.unwrap(), 14);
    /// let (old, tag) = atomic_owned.swap((None, Tag::First), Relaxed);
    /// assert_eq!(tag, Tag::Second);
    /// assert_eq!(*old.unwrap(), 15);
    /// let (old, tag) = atomic_owned.swap((None, Tag::None), Relaxed);
    /// assert_eq!(tag, Tag::First);
    /// assert!(old.is_none());
    /// ```
    #[inline]
    pub fn swap(
        &self,
        new: (Option<Owned<T>>, Tag),
        mut order: Ordering,
    ) -> (Option<Owned<T>>, Tag) {
        let desired = Tag::update_tag(
            new.0.as_ref().map_or_else(null, Owned::underlying_ptr),
            new.1,
        )
        .cast_mut();
        order = safe_load_ordering(safe_store_ordering(order));
        let prev = self.ptr.swap(desired, order);
        post_load_fence(order);
        let tag = Tag::into_tag(prev);
        let prev_ptr = Tag::unset_tag(prev).cast_mut();
        forget(new);
        (NonNull::new(prev_ptr).map(Owned::from), tag)
    }

    /// Returns the current [`Tag`] of the [`AtomicOwned`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicOwned, Tag};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// assert_eq!(atomic_owned.tag(Relaxed), Tag::None);
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
    /// use sdd::{AtomicOwned, Tag};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::null();
    /// assert!(atomic_owned.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed));
    /// assert_eq!(atomic_owned.tag(Relaxed), Tag::Both);
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

    /// Atomically stores `new` into the [`AtomicOwned`] if the current value matches `current`.
    ///
    /// Returns the previously held value and the updated [`Ptr`] on success.
    ///
    /// # Errors
    ///
    /// Returns `Err` containing the supplied [`Owned`] and the current [`Ptr`] on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicOwned, Guard, Owned, Tag};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// atomic_owned.update_tag_if(Tag::Both, |_| true, Relaxed, Relaxed);
    /// assert!(atomic_owned.compare_exchange(
    ///     ptr, (Some(Owned::new(18)), Tag::First), Relaxed, Relaxed, &guard).is_err());
    ///
    /// ptr.set_tag(Tag::Both);
    /// let old: Owned<usize> = atomic_owned.compare_exchange(
    ///     ptr, (Some(Owned::new(18)), Tag::First), Relaxed, Relaxed, &guard).unwrap().0.unwrap();
    /// assert_eq!(*old, 17);
    /// drop(old);
    ///
    /// assert!(atomic_owned.compare_exchange(
    ///     ptr, (Some(Owned::new(19)), Tag::None), Relaxed, Relaxed, &guard).is_err());
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    /// ```
    #[inline]
    pub fn compare_exchange<'g>(
        &self,
        current: Ptr<'g, T>,
        new: (Option<Owned<T>>, Tag),
        mut success: Ordering,
        failure: Ordering,
        _guard: &'g Guard,
    ) -> Result<OwnedPtrPair<'g, T>, OwnedPtrPair<'g, T>> {
        let desired = Tag::update_tag(
            new.0.as_ref().map_or_else(null, Owned::underlying_ptr),
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
                let prev_owned = NonNull::new(Tag::unset_tag(prev).cast_mut()).map(Owned::from);
                forget(new);
                Ok((prev_owned, Ptr::from(desired)))
            }
            Err(actual) => Err((new.0, Ptr::from(actual))),
        }
    }

    /// Atomically stores `new` into the [`AtomicOwned`] if the current value matches `current`.
    ///
    /// This method may spuriously fail even when the comparison succeeds.
    ///
    /// Returns the previously held value and the updated [`Ptr`] on success.
    ///
    /// # Errors
    ///
    /// Returns `Err` containing the supplied [`Owned`] and the current [`Ptr`] on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicOwned, Owned, Guard, Tag};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(17);
    /// let guard = Guard::new();
    ///
    /// let mut ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 17);
    ///
    /// while let Err((_, actual)) = atomic_owned.compare_exchange_weak(
    ///     ptr,
    ///     (Some(Owned::new(18)), Tag::First),
    ///     Relaxed,
    ///     Relaxed,
    ///     &guard) {
    ///     ptr = actual;
    /// }
    ///
    /// let mut ptr = atomic_owned.load(Relaxed, &guard);
    /// assert_eq!(*ptr.as_ref().unwrap(), 18);
    /// ```
    #[inline]
    pub fn compare_exchange_weak<'g>(
        &self,
        current: Ptr<'g, T>,
        new: (Option<Owned<T>>, Tag),
        mut success: Ordering,
        failure: Ordering,
        _guard: &'g Guard,
    ) -> Result<OwnedPtrPair<'g, T>, OwnedPtrPair<'g, T>> {
        let desired = Tag::update_tag(
            new.0.as_ref().map_or_else(null, Owned::underlying_ptr),
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
                let prev_owned = NonNull::new(Tag::unset_tag(prev).cast_mut()).map(Owned::from);
                forget(new);
                Ok((prev_owned, Ptr::from(desired)))
            }
            Err(actual) => Err((new.0, Ptr::from(actual))),
        }
    }

    /// Consumes `self` and converts it into an [`Owned`] instance.
    ///
    /// Returns `None` if the [`AtomicOwned`] was null and did not own an instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicOwned, Owned};
    ///
    /// let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(55);
    /// let owned: Owned<usize> = atomic_owned.into_owned(Relaxed).unwrap();
    /// assert_eq!(*owned, 55);
    /// ```
    #[inline]
    #[must_use]
    pub fn into_owned(self, order: Ordering) -> Option<Owned<T>> {
        let ptr = self.ptr.swap(null_mut(), safe_load_ordering(order));
        post_load_fence(order);
        if let Some(underlying_ptr) = NonNull::new(Tag::unset_tag(ptr).cast_mut()) {
            return Some(Owned::from(underlying_ptr));
        }
        None
    }
}

impl<T> Default for AtomicOwned<T> {
    #[inline]
    fn default() -> Self {
        Self::null()
    }
}

impl<T> Drop for AtomicOwned<T> {
    #[inline]
    fn drop(&mut self) {
        if let Some(ptr) = NonNull::new(Tag::unset_tag(self.ptr.load(Relaxed)).cast_mut()) {
            drop(Owned::from(ptr));
        }
    }
}

// `T` needs to be `Sync` since sending `AtomicOwned<T>` is analogous to sending `&T`.
unsafe impl<T: Send + Sync> Send for AtomicOwned<T> {}

unsafe impl<T: Send + Sync> Sync for AtomicOwned<T> {}

impl<T: UnwindSafe> UnwindSafe for AtomicOwned<T> {}
