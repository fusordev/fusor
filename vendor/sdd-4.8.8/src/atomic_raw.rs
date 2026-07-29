use std::panic::UnwindSafe;
use std::ptr::null_mut;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicPtr;

use super::ref_counted::RefCounted;
use super::{Guard, RawPtr, Tag};

/// [`AtomicRaw`] does not own the underlying instance, allowing the user to control the lifetime of
/// instances of type `T`.
///
/// Since [`AtomicRaw`] does not own the pointed-to instance, the user needs to convert the pointer
/// into an [`Owned`](super::Owned) through [`Owned::from_raw`](super::Owned::from_raw) in order to
/// reclaim the memory.
///
/// Additionally, memory ordering is not automatically enforced: for example,
/// [`swap`](AtomicRaw::swap) allows relaxed operations, while [`AtomicOwned`](super::AtomicOwned)
/// and [`AtomicShared`](super::AtomicShared) implicitly upgrade [`Relaxed`](Ordering::Relaxed) to
/// [`Release`](Ordering::Release).
#[derive(Debug)]
pub struct AtomicRaw<T> {
    ptr: AtomicPtr<RefCounted<T>>,
}

impl<T> AtomicRaw<T> {
    /// Creates a new [`AtomicRaw`] from a [`RawPtr`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicRaw, Owned};
    ///
    /// let owned = Owned::new(11);
    /// let ptr = owned.into_raw();
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::new(ptr);
    /// drop(unsafe { Owned::from_raw(ptr) });
    /// ```
    #[cfg(not(feature = "loom"))]
    #[inline]
    #[must_use]
    pub const fn new(ptr: RawPtr<'_, T>) -> Self {
        let ptr: std::sync::atomic::AtomicPtr<RefCounted<T>> =
            AtomicPtr::new(ptr.underlying_ptr().cast_mut());
        Self { ptr }
    }

    /// Creates a new [`AtomicRaw`] from a [`RawPtr`].
    #[cfg(feature = "loom")]
    #[inline]
    #[must_use]
    pub fn new(ptr: RawPtr<'_, T>) -> Self {
        let ptr: loom::sync::atomic::AtomicPtr<RefCounted<T>> =
            AtomicPtr::new(ptr.underlying_ptr().cast_mut());
        Self { ptr }
    }

    /// Creates a null [`AtomicRaw`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::AtomicRaw;
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    /// ```
    #[cfg(not(feature = "loom"))]
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        let ptr: std::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(null_mut());
        Self { ptr }
    }

    /// Creates a null [`AtomicRaw`].
    #[cfg(feature = "loom")]
    #[inline]
    #[must_use]
    pub fn null() -> Self {
        let ptr: loom::sync::atomic::AtomicPtr<RefCounted<T>> = AtomicPtr::new(null_mut());
        Self { ptr }
    }

    /// Returns `true` if the [`AtomicRaw`] is null.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicRaw, Tag};
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    /// assert!(atomic_raw.is_null(Relaxed));
    /// ```
    #[inline]
    #[must_use]
    pub fn is_null(&self, order: Ordering) -> bool {
        Tag::unset_tag(self.ptr.load(order)).is_null()
    }

    /// Loads a pointer value from the [`AtomicRaw`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicRaw, Guard, Owned};
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    /// let guard = Guard::new();
    /// let owned = Owned::new(11);
    /// let ptr = owned.into_raw();
    /// atomic_raw.store(ptr, Relaxed);
    ///
    /// assert_eq!(ptr, atomic_raw.load(Relaxed, &guard));
    /// drop(unsafe { Owned::from_raw(ptr) });
    /// ```
    #[inline]
    #[must_use]
    pub fn load<'g>(&self, order: Ordering, _guard: &'g Guard) -> RawPtr<'g, T> {
        RawPtr::from(self.ptr.load(order))
    }

    /// Stores a pointer value into the [`AtomicRaw`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicRaw, Owned};
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    ///
    /// let owned = Owned::new(11);
    /// let ptr = owned.into_raw();
    /// atomic_raw.store(ptr, Relaxed);
    /// drop(unsafe { Owned::from_raw(ptr) });
    /// ```
    #[inline]
    pub fn store(&self, ptr: RawPtr<'_, T>, order: Ordering) {
        self.ptr.store(ptr.underlying_ptr().cast_mut(), order);
    }

    /// Atomically stores the given value into the [`AtomicRaw`], returning the previous value.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicRaw, Guard, Owned};
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    ///
    /// let guard = Guard::new();
    /// let owned = Owned::new(11);
    /// let ptr = owned.into_raw();
    /// atomic_raw.store(ptr, Relaxed);
    ///
    /// let new_owned = Owned::new(17);
    /// let new_ptr = new_owned.into_raw();
    /// let swapped = atomic_raw.swap(new_ptr, Relaxed, &guard);
    /// assert_eq!(unsafe { *Owned::from_raw(swapped).unwrap() }, 11);
    /// drop(unsafe { Owned::from_raw(new_ptr) });
    /// ```
    #[inline]
    pub fn swap<'g>(
        &self,
        new: RawPtr<'g, T>,
        order: Ordering,
        _guard: &'g Guard,
    ) -> RawPtr<'g, T> {
        let desired = new.underlying_ptr().cast_mut();
        RawPtr::from(self.ptr.swap(desired, order))
    }

    /// Returns its [`Tag`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicRaw, Tag};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    /// assert_eq!(atomic_raw.tag(Relaxed), Tag::None);
    /// ```
    #[inline]
    #[must_use]
    pub fn tag(&self, order: Ordering) -> Tag {
        Tag::into_tag(self.ptr.load(order))
    }

    /// Sets a new [`Tag`] if the given condition is met.
    ///
    /// Returns `true` if the new [`Tag`] has been successfully set.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicRaw, Tag};
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    /// assert!(atomic_raw.update_tag_if(Tag::Both, |p| p.tag() == Tag::None, Relaxed, Relaxed));
    /// assert_eq!(atomic_raw.tag(Relaxed), Tag::Both);
    /// ```
    #[inline]
    pub fn update_tag_if<F: FnMut(RawPtr<T>) -> bool>(
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
                    if condition(RawPtr::from(ptr)) {
                        Some(Tag::update_tag(ptr, tag).cast_mut())
                    } else {
                        None
                    }
                },
            )
            .is_ok()
    }

    /// Fetches the [`RawPtr`], and applies a closure to it that returns an optional
    /// new [`RawPtr`].
    ///
    /// The closure is applied to the current value. If it returns `Some(new_ptr)`, the atomic
    /// pointer is updated to `new_ptr`, and it returns `Ok(prev_ptr)` containing the previous
    /// pointer held before the update. If the closure returns `None`, the update is not performed
    /// and the current value is returned as an `Err(prev_ptr)`
    ///
    /// # Errors
    ///
    /// Returns the previous [`RawPtr`] if the closure returned `None`, wrapped in `Err`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use sdd::{AtomicRaw, Guard, Owned, RawPtr};
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    ///
    /// let guard = Guard::new();
    /// let owned = Owned::new(11);
    /// let ptr = owned.into_raw();
    /// atomic_raw.store(ptr, Relaxed);
    ///
    /// assert_eq!(atomic_raw.fetch_update(Relaxed, Relaxed, |_| None, &guard), Err(ptr));
    /// assert_eq!(
    ///     atomic_raw.fetch_update(Relaxed, Relaxed, |_| Some(RawPtr::null()), &guard),
    ///     Ok(ptr)
    /// );
    /// drop(unsafe { Owned::from_raw(ptr) });
    /// ```
    #[inline]
    pub fn fetch_update<'g, F: FnMut(RawPtr<'g, T>) -> Option<RawPtr<'g, T>>>(
        &self,
        set_order: Ordering,
        fetch_order: Ordering,
        mut f: F,
        _guard: &'g Guard,
    ) -> Result<RawPtr<'g, T>, RawPtr<'g, T>> {
        self.ptr
            .fetch_update(
                set_order,
                fetch_order,
                #[inline]
                |raw| {
                    let result = f(RawPtr::from(raw));
                    result.map(|ptr| ptr.underlying_ptr().cast_mut())
                },
            )
            .map_err(|raw| RawPtr::from(raw))
            .map(|raw| RawPtr::from(raw))
    }

    /// Atomically stores `new` into the [`AtomicRaw`] if the current [`RawPtr`] matches `current`.
    ///
    /// Returns the previously held [`RawPtr`].
    ///
    /// # Errors
    ///
    /// Returns the previous [`RawPtr`] if the supplied [`RawPtr`] did not match `current`, wrapped in `Err`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{AtomicRaw, Guard, Owned, RawPtr};
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();
    ///
    /// let guard = Guard::new();
    /// let owned = Owned::new(11);
    /// let ptr = owned.into_raw();
    ///
    /// assert_eq!(
    ///     unsafe { atomic_raw.compare_exchange(ptr, RawPtr::null(), Relaxed, Relaxed, &guard) },
    ///     Err(RawPtr::null())
    /// );
    /// assert_eq!(
    ///     unsafe { atomic_raw.compare_exchange(RawPtr::null(), ptr, Relaxed, Relaxed, &guard) },
    ///     Ok(RawPtr::null())
    /// );
    /// drop(unsafe { Owned::from_raw(ptr) });
    /// ```
    #[inline]
    pub fn compare_exchange<'g>(
        &self,
        current: RawPtr<'g, T>,
        new: RawPtr<'g, T>,
        success: Ordering,
        failure: Ordering,
        _guard: &'g Guard,
    ) -> Result<RawPtr<'g, T>, RawPtr<'g, T>> {
        self.ptr
            .compare_exchange(
                current.underlying_ptr().cast_mut(),
                new.underlying_ptr().cast_mut(),
                success,
                failure,
            )
            .map_err(|raw| RawPtr::from(raw))
            .map(|raw| RawPtr::from(raw))
    }
}

impl<T> Default for AtomicRaw<T> {
    #[inline]
    fn default() -> Self {
        Self::null()
    }
}

unsafe impl<T: Send> Send for AtomicRaw<T> {}

unsafe impl<T: Send + Sync> Sync for AtomicRaw<T> {}

impl<T: UnwindSafe> UnwindSafe for AtomicRaw<T> {}
