use std::marker::PhantomData;
use std::panic::UnwindSafe;
use std::ptr;

use super::ref_counted::RefCounted;
use super::{Ptr, Tag};

/// [`RawPtr`] is a raw pointer where the lifetime of the pointed-to instance is not managed.
///
/// [`RawPtr`] cannot be used to dereference the pointed-to instance unless it is converted into
/// [`Ptr`] through [`RawPtr::into_ptr`].
#[derive(Debug)]
pub struct RawPtr<'g, T> {
    ptr: *const RefCounted<T>,
    _phantom: PhantomData<&'g T>,
}

impl<'g, T> RawPtr<'g, T> {
    /// Creates a null [`RawPtr`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::RawPtr;
    ///
    /// let ptr: RawPtr<usize> = RawPtr::null();
    /// ```
    #[inline]
    #[must_use]
    pub const fn null() -> Self {
        Self {
            ptr: ptr::null(),
            _phantom: PhantomData,
        }
    }

    /// Returns `true` if the [`RawPtr`] is null.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::RawPtr;
    ///
    /// let ptr: RawPtr<usize> = RawPtr::null();
    /// assert!(ptr.is_null());
    /// ```
    #[inline]
    #[must_use]
    pub fn is_null(&self) -> bool {
        Tag::unset_tag(self.ptr).is_null()
    }

    /// Returns its [`Tag`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{RawPtr, Tag};
    ///
    /// let ptr: RawPtr<usize> = RawPtr::null();
    /// assert_eq!(ptr.tag(), Tag::None);
    /// ```
    #[inline]
    #[must_use]
    pub fn tag(&self) -> Tag {
        Tag::into_tag(self.ptr)
    }

    /// Sets a [`Tag`], overwriting the existing [`Tag`].
    ///
    /// Returns the previous tag value.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{RawPtr, Tag};
    ///
    /// let mut ptr: RawPtr<usize> = RawPtr::null();
    /// assert_eq!(ptr.set_tag(Tag::Both), Tag::None);
    /// assert_eq!(ptr.tag(), Tag::Both);
    /// ```
    #[inline]
    pub fn set_tag(&mut self, tag: Tag) -> Tag {
        let old_tag = Tag::into_tag(self.ptr);
        self.ptr = Tag::update_tag(self.ptr, tag);
        old_tag
    }

    /// Clears its [`Tag`].
    ///
    /// Returns the previous tag value.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{RawPtr, Tag};
    ///
    /// let mut ptr: RawPtr<usize> = RawPtr::null().with_tag(Tag::Both);
    /// assert_eq!(ptr.unset_tag(), Tag::Both);
    /// ```
    #[inline]
    pub fn unset_tag(&mut self) -> Tag {
        let old_tag = Tag::into_tag(self.ptr);
        self.ptr = Tag::unset_tag(self.ptr);
        old_tag
    }

    /// Returns a copy of `self` with a [`Tag`] set.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{RawPtr, Tag};
    ///
    /// let mut ptr: RawPtr<usize> = RawPtr::null();
    /// assert_eq!(ptr.tag(), Tag::None);
    ///
    /// let ptr_with_tag = ptr.with_tag(Tag::First);
    /// assert_eq!(ptr_with_tag.tag(), Tag::First);
    /// ```
    #[inline]
    #[must_use]
    pub fn with_tag(self, tag: Tag) -> Self {
        Self::from(Tag::update_tag(self.ptr, tag))
    }

    /// Returns a copy of `self` with its [`Tag`] erased.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::{Ptr, RawPtr, Tag};
    ///
    /// let mut ptr: RawPtr<usize> = RawPtr::null();
    /// ptr.set_tag(Tag::Second);
    /// assert_eq!(ptr.tag(), Tag::Second);
    ///
    /// let ptr_without_tag = ptr.without_tag();
    /// assert_eq!(ptr_without_tag.tag(), Tag::None);
    /// ```
    #[inline]
    #[must_use]
    pub fn without_tag(self) -> Self {
        Self::from(Tag::unset_tag(self.ptr))
    }

    /// Converts itself into a [`Ptr`] to dereference the instance.
    ///
    /// # Safety
    ///
    /// The pointed-to instance must be valid for the lifetime `'g`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sdd::Owned;
    ///
    /// let owned: Owned<usize> = Owned::new(83);
    /// let ptr = owned.into_raw();
    /// assert_eq!(unsafe { ptr.into_ptr().as_ref() }, Some(&83));
    /// drop(unsafe { Owned::from_raw(ptr) });
    /// ```
    #[inline]
    #[must_use]
    pub const unsafe fn into_ptr(self) -> Ptr<'g, T> {
        Ptr::from(self.ptr)
    }

    /// Creates a new [`RawPtr`] from a [`RefCounted`] pointer.
    #[inline]
    pub(super) const fn from(ptr: *const RefCounted<T>) -> Self {
        Self {
            ptr,
            _phantom: PhantomData,
        }
    }

    /// Returns a pointer to the [`RefCounted`], including tag bits.
    #[inline]
    pub(super) const fn underlying_ptr(self) -> *const RefCounted<T> {
        self.ptr
    }
}

impl<T> Clone for RawPtr<'_, T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for RawPtr<'_, T> {}

impl<T> Default for RawPtr<'_, T> {
    #[inline]
    fn default() -> Self {
        Self::null()
    }
}

impl<T> Eq for RawPtr<'_, T> {}

impl<T> PartialEq for RawPtr<'_, T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}

impl<T: UnwindSafe> UnwindSafe for RawPtr<'_, T> {}
