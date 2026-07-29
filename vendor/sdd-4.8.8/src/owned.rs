use std::mem::{forget, needs_drop};
use std::ops::Deref;
use std::panic::UnwindSafe;
use std::ptr::NonNull;

use super::ref_counted::RefCounted;
use super::{Guard, Ptr, RawPtr, Tag};

/// [`Owned`] uniquely owns an instance.
///
/// The instance is passed to the EBR garbage collector when the [`Owned`] is dropped.
#[derive(Debug)]
pub struct Owned<T> {
    ptr: NonNull<RefCounted<T>>,
}

impl<T: 'static> Owned<T> {
    /// Creates a new [`Owned`].
    ///
    /// The type of the instance must be determined at compile-time and must not contain non-static
    /// references, as the instance can theoretically live as long as the process. For instance,
    /// `struct Disallowed<'l, T>(&'l T)` is not safe if it implements [`Drop`] because [`drop`] can
    /// be run after `'l`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let owned: Owned<usize> = Owned::new(31);
    /// ```
    #[inline]
    pub fn new(t: T) -> Self {
        Self::new_with(|| t)
    }

    /// Creates a new [`Owned`] with the provided function.
    ///
    /// This function is identical to [`Owned::new`] except that the value is constructed after
    /// memory allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let owned: Owned<String> = Owned::new_with(|| String::from("hello"));
    /// ```
    #[inline]
    pub fn new_with<F: FnOnce() -> T>(f: F) -> Owned<T> {
        Owned {
            ptr: RefCounted::new_unique(f),
        }
    }
}

impl<T> Owned<T> {
    /// Asserts that the type does not implement [`Drop`].
    const ASSERT_NO_DROP: () = assert!(!needs_drop::<T>());

    /// Creates a new [`Owned`].
    ///
    /// The type does not need a `'static` lifetime because it does not implement [`Drop`], and its
    /// instances will not be accessed after their lifetime ends.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let owned: Owned<usize> = Owned::new_checked(31);
    /// ```
    ///
    /// ```compile_fail
    /// use sdd::Owned;
    ///
    /// let owned: Owned<String> = Owned::new_checked(String::from("hello"));
    /// ```
    #[inline]
    pub fn new_checked(t: T) -> Self {
        let _: () = Self::ASSERT_NO_DROP;
        Self::new_with_checked(|| t)
    }

    /// Creates a new [`Owned`] with the provided function.
    ///
    /// This function is identical to [`Owned::new_checked`] except that the value is constructed
    /// after memory allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let str = String::from("hello");
    /// let owned: Owned<&str> = Owned::new_with_checked(|| str.as_str());
    /// ```
    ///
    /// ```compile_fail
    /// use sdd::Owned;
    ///
    /// let owned: Owned<String> = Owned::new_with_checked(|| String::from("hello"));
    /// ```
    #[inline]
    pub fn new_with_checked<F: FnOnce() -> T>(f: F) -> Owned<T> {
        let _: () = Self::ASSERT_NO_DROP;
        Owned {
            ptr: RefCounted::new_unique(f),
        }
    }

    /// Creates a new [`Owned`] without checking the lifetime of `T`.
    ///
    /// # Safety
    ///
    /// `T::drop` can be run after the [`Owned`] is dropped, therefore it is safe only if `T::drop`
    /// does not access short-lived data or [`needs_drop`] is `false` for `T`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let hello = String::from("hello");
    /// let owned: Owned<&str> = unsafe { Owned::new_unchecked(hello.as_str()) };
    /// ```
    #[inline]
    pub unsafe fn new_unchecked(t: T) -> Self {
        unsafe { Self::new_with_unchecked(|| t) }
    }

    /// Creates a new [`Owned`] with the provided function without checking the lifetime of `T`.
    ///
    /// This function is identical to [`Owned::new_unchecked`] except that the value is constructed
    /// after memory allocation.
    ///
    /// # Safety
    ///
    /// See [`Owned::new_unchecked`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let hello = String::from("hello");
    /// let owned: Owned<&str> = unsafe { Owned::new_with_unchecked(|| hello.as_str()) };
    /// ```
    #[inline]
    pub unsafe fn new_with_unchecked<F: FnOnce() -> T>(f: F) -> Owned<T> {
        Owned {
            ptr: RefCounted::new_unique(f),
        }
    }

    /// Returns a [`Ptr`] to the instance that may live as long as the supplied [`Guard`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Owned};
    ///
    /// let owned: Owned<usize> = Owned::new(37);
    /// let guard = Guard::new();
    /// let ptr = owned.get_guarded_ptr(&guard);
    /// drop(owned);
    ///
    /// assert_eq!(*ptr.as_ref().unwrap(), 37);
    /// ```
    #[inline]
    #[must_use]
    pub const fn get_guarded_ptr<'g>(&self, _guard: &'g Guard) -> Ptr<'g, T> {
        Ptr::from(self.ptr.as_ptr())
    }

    /// Returns a reference to the instance that may live as long as the supplied [`Guard`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Owned};
    ///
    /// let owned: Owned<usize> = Owned::new(37);
    /// let guard = Guard::new();
    /// let ref_b = owned.get_guarded_ref(&guard);
    /// drop(owned);
    ///
    /// assert_eq!(*ref_b, 37);
    /// ```
    #[inline]
    #[must_use]
    pub const fn get_guarded_ref<'g>(&self, _guard: &'g Guard) -> &'g T {
        unsafe { RefCounted::inst_non_null_ptr(self.ptr).as_ref() }
    }

    /// Returns a mutable reference to the instance.
    ///
    /// # Safety
    ///
    /// The method is `unsafe` since there can be a [`Ptr`] to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let mut owned: Owned<usize> = Owned::new(38);
    /// unsafe {
    ///     *owned.get_mut() += 1;
    /// }
    /// assert_eq!(*owned, 39);
    /// ```
    #[inline]
    pub const unsafe fn get_mut(&mut self) -> &mut T {
        unsafe { (*self.ptr.as_ptr()).get_mut_unique() }
    }

    /// Provides a raw pointer to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let owned: Owned<usize> = Owned::new(10);
    ///
    /// assert_eq!(unsafe { *owned.as_ptr() }, 10);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_ptr(&self) -> *const T {
        RefCounted::inst_non_null_ptr(self.ptr).as_ptr()
    }

    /// Provides a raw non-null pointer to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let owned: Owned<usize> = Owned::new(10);
    ///
    /// assert_eq!(unsafe { *owned.as_non_null_ptr().as_ref() }, 10);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_non_null_ptr(&self) -> NonNull<T> {
        RefCounted::inst_non_null_ptr(self.ptr)
    }

    /// Converts itself into a [`RawPtr`].
    ///
    /// The returned [`RawPtr`] must be converted back to [`Owned`] through [`Self::from_raw`] to
    /// avoid a memory leak.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let owned: Owned<usize> = Owned::new(10);
    /// let ptr = owned.into_raw();
    /// drop(unsafe { Owned::from_raw(ptr) });
    /// ```
    #[inline]
    #[must_use]
    pub const fn into_raw<'g>(self) -> RawPtr<'g, T> {
        let ptr = RawPtr::from(self.ptr.as_ptr());
        forget(self);
        ptr
    }

    /// Constructs an [`Owned`] from a [`RawPtr`].
    ///
    /// Returns `None` if the [`RawPtr`] is null.
    ///
    /// # Safety
    ///
    /// The pointed-to instance must be valid for the lifetime `'g`, and the returned [`Owned`]
    /// should be unique or must be forgotten through [`forget`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::mem::forget;
    ///
    /// use sdd::Owned;
    ///
    /// let owned: Owned<usize> = Owned::new(83);
    /// let ptr = owned.into_raw();
    /// let owned = unsafe { Owned::from_raw(ptr).unwrap() };
    /// let owned_copy = unsafe { Owned::from_raw(ptr).unwrap() };
    /// assert_eq!(*owned, 83);
    /// assert_eq!(*owned_copy, 83);
    ///
    /// drop(owned);
    ///
    /// // Accessing `owned_copy` after dropping `owned` may lead to undefined behavior.
    /// forget(owned_copy);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_raw(ptr: RawPtr<'_, T>) -> Option<Self> {
        if let Some(ptr) = NonNull::new(Tag::unset_tag(ptr.underlying_ptr()).cast_mut()) {
            return Some(Owned::from(ptr));
        }
        None
    }

    /// Drops the instance immediately.
    ///
    /// # Safety
    ///
    /// The caller must ensure that there is no [`Ptr`] pointing to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    /// use std::sync::atomic::AtomicBool;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// static DROPPED: AtomicBool = AtomicBool::new(false);
    /// struct T(&'static AtomicBool);
    /// impl Drop for T {
    ///     fn drop(&mut self) {
    ///         self.0.store(true, Relaxed);
    ///     }
    /// }
    ///
    /// let owned: Owned<T> = Owned::new(T(&DROPPED));
    /// assert!(!DROPPED.load(Relaxed));
    ///
    /// unsafe {
    ///     owned.drop_in_place();
    /// }
    ///
    /// assert!(DROPPED.load(Relaxed));
    /// ```
    #[inline]
    pub unsafe fn drop_in_place(self) {
        RefCounted::<T>::dealloc(self.ptr.as_ptr());
        forget(self);
    }

    /// Creates a new [`Owned`] from the given pointer.
    #[inline]
    pub(super) const fn from(ptr: NonNull<RefCounted<T>>) -> Self {
        Self { ptr }
    }

    /// Returns a pointer to the [`RefCounted`].
    #[inline]
    pub(super) const fn underlying_ptr(&self) -> *const RefCounted<T> {
        self.ptr.as_ptr()
    }
}

impl<T> AsRef<T> for Owned<T> {
    #[inline]
    fn as_ref(&self) -> &T {
        unsafe { &*self.ptr.as_ptr() }
    }
}

impl<T> Deref for Owned<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T> Drop for Owned<T> {
    #[inline]
    fn drop(&mut self) {
        RefCounted::pass_to_collector(self.ptr.as_ptr());
    }
}

// `T` needs to be `Sync` since sending `Owned<T>` is analogous to sending `&T`.
unsafe impl<T: Send + Sync> Send for Owned<T> {}

// `T` does not need to be `Send` since sending `T` is not possible with only `&Owned<T>`.
unsafe impl<T: Sync> Sync for Owned<T> {}

impl<T: UnwindSafe> UnwindSafe for Owned<T> {}
