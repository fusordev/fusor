use std::mem::{forget, needs_drop};
use std::ops::Deref;
use std::panic::UnwindSafe;
use std::ptr::NonNull;

use super::ref_counted::RefCounted;
use super::{Guard, Ptr};

/// [`Shared`] is a reference-counted handle to an instance.
///
/// The instance is passed to the EBR garbage collector when the last strong reference is dropped.
#[derive(Debug)]
pub struct Shared<T> {
    ptr: NonNull<RefCounted<T>>,
}

impl<T: 'static> Shared<T> {
    /// Creates a new [`Shared`].
    ///
    /// The type of the instance must be determined at compile-time and must not contain non-static
    /// references, as the instance can theoretically live as long as the process. For instance,
    /// `struct Disallowed<'l, T>(&'l T)` is not safe if it implements [`Drop`] because [`drop`] can
    /// be run after `'l`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let shared: Shared<usize> = Shared::new(31);
    /// ```
    #[inline]
    pub fn new(t: T) -> Self {
        Self::new_with(|| t)
    }

    /// Creates a new [`Shared`] with the provided function.
    ///
    /// This function is identical to [`Shared::new`] except that the value is constructed after
    /// memory allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let shared: Shared<String> = Shared::new_with(|| String::from("hello"));
    /// ```
    #[inline]
    pub fn new_with<F: FnOnce() -> T>(f: F) -> Shared<T> {
        Shared {
            ptr: RefCounted::new_shared(f),
        }
    }
}

impl<T> Shared<T> {
    /// Asserts that the type does not implement [`Drop`].
    const ASSERT_NO_DROP: () = assert!(!needs_drop::<T>());

    /// Creates a new [`Shared`].
    ///
    /// The type does not need a `'static` lifetime because it does not implement [`Drop`], and its
    /// instances will not be accessed after their lifetime ends.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let shared: Shared<usize> = Shared::new_checked(31);
    /// ```
    ///
    /// ```compile_fail
    /// use sdd::Shared;
    ///
    /// let shared: Shared<String> = Shared::new_checked(String::from("hello"));
    /// ```
    #[inline]
    pub fn new_checked(t: T) -> Self {
        let _: () = Self::ASSERT_NO_DROP;
        Self::new_with_checked(|| t)
    }

    /// Creates a new [`Shared`] with the provided function.
    ///
    /// This function is identical to [`Shared::new_checked`] except that the value is constructed
    /// after memory allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let str = String::from("hello");
    /// let shared: Shared<&str> = Shared::new_with_checked(|| str.as_str());
    /// ```
    ///
    /// ```compile_fail
    /// use sdd::Shared;
    ///
    /// let shared: Shared<String> = Shared::new_with_checked(|| String::from("hello"));
    /// ```
    #[inline]
    pub fn new_with_checked<F: FnOnce() -> T>(f: F) -> Shared<T> {
        let _: () = Self::ASSERT_NO_DROP;
        Shared {
            ptr: RefCounted::new_shared(f),
        }
    }

    /// Creates a new [`Shared`] without checking the lifetime of `T`.
    ///
    /// # Safety
    ///
    /// `T::drop` can be run after the last strong reference is dropped, therefore, it is safe only
    /// if `T::drop` does not access short-lived data or [`needs_drop`] is `false` for `T`.
    /// Otherwise, the instance must be manually dropped by invoking [`Self::drop_in_place`] within
    /// the lifetime of `T`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let hello = String::from("hello");
    /// let shared: Shared<&str> = unsafe { Shared::new_unchecked(hello.as_str()) };
    ///
    /// assert!(unsafe { shared.drop_in_place() });
    /// ```
    #[inline]
    pub unsafe fn new_unchecked(t: T) -> Self {
        unsafe { Self::new_with_unchecked(|| t) }
    }

    /// Creates a new [`Shared`] with the provided function without checking the lifetime of `T`.
    ///
    /// This function is identical to [`Shared::new_unchecked`] except that the value is constructed
    /// after memory allocation.
    ///
    /// # Safety
    ///
    /// See [`Shared::new_unchecked`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let hello = String::from("hello");
    /// let shared: Shared<&str> = unsafe { Shared::new_with_unchecked(|| hello.as_str()) };
    ///
    /// assert!(unsafe { shared.drop_in_place() });
    /// ```
    #[inline]
    pub unsafe fn new_with_unchecked<F: FnOnce() -> T>(f: F) -> Shared<T> {
        Shared {
            ptr: RefCounted::new_shared(f),
        }
    }

    /// Returns a [`Ptr`] to the instance that may live as long as the supplied [`Guard`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Guard, Shared};
    ///
    /// let shared: Shared<usize> = Shared::new(37);
    /// let guard = Guard::new();
    /// let ptr = shared.get_guarded_ptr(&guard);
    /// drop(shared);
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
    /// use sdd::{Guard, Shared};
    ///
    /// let shared: Shared<usize> = Shared::new(37);
    /// let guard = Guard::new();
    /// let ref_b = shared.get_guarded_ref(&guard);
    /// drop(shared);
    ///
    /// assert_eq!(*ref_b, 37);
    /// ```
    #[inline]
    #[must_use]
    pub const fn get_guarded_ref<'g>(&self, _guard: &'g Guard) -> &'g T {
        unsafe { RefCounted::inst_non_null_ptr(self.ptr).as_ref() }
    }

    /// Returns a mutable reference to the instance if the [`Shared`] is holding the only strong
    /// reference.
    ///
    /// # Safety
    ///
    /// The method is `unsafe` since there can be a [`Ptr`] to the instance without holding a
    /// strong reference.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let mut shared: Shared<usize> = Shared::new(38);
    /// unsafe {
    ///     *shared.get_mut().unwrap() += 1;
    /// }
    /// assert_eq!(*shared, 39);
    /// ```
    #[inline]
    pub unsafe fn get_mut(&mut self) -> Option<&mut T> {
        unsafe { self.ptr.as_ptr().as_mut().and_then(|r| r.get_mut_shared()) }
    }

    /// Provides a raw pointer to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let shared: Shared<usize> = Shared::new(10);
    /// let shared_clone: Shared<usize> = shared.clone();
    ///
    /// assert_eq!(shared.as_ptr(), shared_clone.as_ptr());
    /// assert_eq!(unsafe { *shared.as_ptr() }, unsafe { *shared_clone.as_ptr() });
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
    /// use sdd::Shared;
    ///
    /// let shared: Shared<usize> = Shared::new(10);
    /// let shared_clone: Shared<usize> = shared.clone();
    ///
    /// assert_eq!(shared.as_ptr(), shared_clone.as_ptr());
    /// assert_eq!(unsafe { *shared.as_non_null_ptr().as_ref() }, unsafe { *shared_clone.as_ptr() });
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_non_null_ptr(&self) -> NonNull<T> {
        RefCounted::inst_non_null_ptr(self.ptr)
    }

    /// Releases the strong reference by passing `self` to the given [`Guard`].
    ///
    /// Returns `true` if the last reference was released.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
    ///
    /// let shared: Shared<usize> = Shared::new(47);
    /// let shared_clone = shared.clone();
    /// assert!(!shared.release());
    /// assert!(shared_clone.release());
    /// ```
    #[inline]
    #[must_use]
    pub fn release(self) -> bool {
        let released = if unsafe { (*self.ptr.as_ptr()).drop_ref() } {
            RefCounted::pass_to_collector(self.ptr.as_ptr());
            true
        } else {
            false
        };
        forget(self);
        released
    }

    /// Drops the instance immediately if it has held the last reference to the instance.
    ///
    /// Returns `true` if the instance was dropped.
    ///
    /// # Safety
    ///
    /// The caller must ensure that there is no [`Ptr`] pointing to the instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Shared;
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
    /// let shared: Shared<T> = Shared::new(T(&DROPPED));
    /// let shared_clone = shared.clone();
    ///
    /// unsafe {
    ///     assert!(!shared.drop_in_place());
    ///     assert!(!DROPPED.load(Relaxed));
    ///     assert!(shared_clone.drop_in_place());
    ///     assert!(DROPPED.load(Relaxed));
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn drop_in_place(self) -> bool {
        unsafe {
            let dropped = if (*self.ptr.as_ptr()).drop_ref() {
                RefCounted::<T>::dealloc(self.ptr.as_ptr());
                true
            } else {
                false
            };
            forget(self);
            dropped
        }
    }

    /// Creates a new [`Shared`] from the given pointer.
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

impl<T> AsRef<T> for Shared<T> {
    #[inline]
    fn as_ref(&self) -> &T {
        unsafe { &*self.ptr.as_ptr() }
    }
}

impl<T> Clone for Shared<T> {
    #[inline]
    fn clone(&self) -> Self {
        unsafe { (*self.ptr.as_ptr()).add_ref() }
        Self { ptr: self.ptr }
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T> Drop for Shared<T> {
    #[inline]
    fn drop(&mut self) {
        if unsafe { (*self.ptr.as_ptr()).drop_ref() } {
            RefCounted::pass_to_collector(self.ptr.as_ptr());
        }
    }
}

impl<'g, T> TryFrom<Ptr<'g, T>> for Shared<T> {
    type Error = Ptr<'g, T>;

    #[inline]
    fn try_from(ptr: Ptr<'g, T>) -> Result<Self, Self::Error> {
        if let Some(shared) = ptr.get_shared() {
            Ok(shared)
        } else {
            Err(ptr)
        }
    }
}

// `T` needs to be `Sync` since sending `Shared<T>` is analogous to sending `&T`.
unsafe impl<T: Send + Sync> Send for Shared<T> {}

unsafe impl<T: Send + Sync> Sync for Shared<T> {}

impl<T: UnwindSafe> UnwindSafe for Shared<T> {}
