use std::alloc::{Layout, dealloc};
use std::mem::{ManuallyDrop, offset_of};
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

/// [`Link`] is an intrusive linked list of memory chunks.
pub(super) struct Link {
    /// Next [`Link`] pointer or reference counter.
    next_or_ref: AtomicPtr<()>,
    /// Function to drop and deallocate the instance.
    dealloc_fn: fn(*mut Link),
}

/// [`DeferredClosure`] invokes the supplied closure when dropped.
pub(super) struct DeferredClosure<F: 'static + FnOnce()> {
    /// Function to invoke in the `drop` method.
    f: ManuallyDrop<F>,
    /// Enables it to be enqueued in a garbage queue.
    link: Link,
}

impl Link {
    /// Creates a new shared [`Link`].
    #[inline]
    pub(super) const fn new_shared(dealloc_fn: fn(*mut Link)) -> Self {
        Link {
            next_or_ref: AtomicPtr::new(ptr::without_provenance_mut(1)),
            dealloc_fn,
        }
    }

    /// Creates a new unique [`Link`].
    #[inline]
    pub(super) const fn new_unique(dealloc_fn: fn(*mut Link)) -> Self {
        Link {
            next_or_ref: AtomicPtr::new(ptr::without_provenance_mut(0)),
            dealloc_fn,
        }
    }

    /// Returns a reference to the reference counter.
    #[inline]
    pub(super) const fn ref_cnt(&self) -> &AtomicPtr<()> {
        &self.next_or_ref
    }

    /// Returns the drop function.
    #[inline]
    pub(super) const fn dealloc_fn(&self) -> fn(*mut Link) {
        self.dealloc_fn
    }

    /// Returns the next pointer.
    #[inline]
    pub(super) fn next_ptr(&self, mo: Ordering) -> *mut Link {
        self.next_or_ref.load(mo).cast::<Link>()
    }

    /// Sets the next pointer.
    #[inline]
    pub(super) fn set_next_ptr(&self, next_ptr: *mut Link, mo: Ordering) {
        self.next_or_ref.store(next_ptr.cast::<()>(), mo);
    }

    /// Casts the pointer to a different type.
    #[inline]
    pub(super) fn cast<T>(ptr: *mut Link, offset: usize) -> *mut T {
        #[allow(clippy::cast_ptr_alignment)]
        ptr.map_addr(|addr| addr - offset).cast::<T>()
    }
}

impl<F: 'static + FnOnce()> DeferredClosure<F> {
    /// Allocates a new [`DeferredClosure`].
    #[inline]
    pub fn alloc(f: F) -> *mut Link {
        let mut allocation = Box::<Self>::new_uninit();
        allocation.write(DeferredClosure {
            f: ManuallyDrop::new(f),
            link: Link::new_unique(|ptr: *mut Link| unsafe {
                let this_ptr = ptr
                    .map_addr(|addr| addr - offset_of!(Self, link))
                    .cast::<Self>();
                this_ptr.drop_in_place();
                dealloc(this_ptr.cast::<u8>(), Layout::new::<Self>());
            }),
        });
        let ptr = Box::into_raw(unsafe { allocation.assume_init() });
        ptr.map_addr(|addr| addr + offset_of!(Self, link))
            .cast::<Link>()
    }
}

impl<F: 'static + FnOnce()> Drop for DeferredClosure<F> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::take(&mut self.f)();
        }
    }
}
