//! Memory allocator for [`Collector`].

use std::alloc::{Layout, dealloc};
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicPtr, AtomicUsize};

use super::collector::Collector;

/// Acquires or allocates a [`Collector`].
///
/// If the global arena is full, this function allocates a new [`Collector`] on the heap.
pub(super) fn acquire() -> NonNull<Collector> {
    // Try to acquire a slot in the global arena.
    for (arena_slot, bitmap) in GLOBAL_COLLECTOR_ARENA.0.iter().enumerate() {
        let acquired_slot = bitmap.acquire_slot();
        if acquired_slot != u8::MAX {
            let collector_arena = &GLOBAL_COLLECTOR_ARENA.1[arena_slot];
            let mut ptr = collector_arena.load(Acquire);
            unsafe {
                if ptr.is_null() {
                    // Try to allocate a new `CollectorArena`.
                    let new_ptr = allocate_collector_arena();
                    match collector_arena.compare_exchange(null_mut(), new_ptr, AcqRel, Acquire) {
                        Ok(_) => ptr = new_ptr,
                        Err(actual_ptr) => {
                            ptr = actual_ptr;
                            new_ptr.drop_in_place();
                            dealloc(new_ptr.cast::<u8>(), Layout::new::<CollectorArena>());
                        }
                    }
                }
                let collector = &mut (*ptr).0[acquired_slot as usize];
                collector.reset(u8::try_from(arena_slot).unwrap_or(u8::MAX));
                return NonNull::from(collector);
            }
        }
    }

    // Fallback to heap.
    NonNull::from(Box::leak(Box::new(Collector::new())))
}

/// Releases a [`Collector`].
pub(super) fn release(ptr: *mut Collector) {
    Collector::clear::<false>(ptr);
    let slot = unsafe { (*ptr).slot() };
    if slot == u8::MAX {
        unsafe {
            ptr.drop_in_place();
            dealloc(ptr.cast::<u8>(), Layout::new::<Collector>());
        }
    } else {
        let arena_slot = &GLOBAL_COLLECTOR_ARENA.0[slot as usize];
        let collector_arena_addr = GLOBAL_COLLECTOR_ARENA.1[slot as usize].load(Relaxed).addr();
        let collector_slot = (ptr.addr() - collector_arena_addr) / size_of::<Collector>();
        if arena_slot.release_slot(collector_slot) {
            let ptr = GLOBAL_COLLECTOR_ARENA.1[slot as usize].swap(null_mut(), Release);
            arena_slot.reset();
            unsafe {
                ptr.drop_in_place();
                dealloc(ptr.cast::<u8>(), Layout::new::<CollectorArena>());
            }
        }
    }
}

/// A fixed-size array of [`Collector`] instances.
#[repr(align(4096))]
struct CollectorArena([Collector; usize::BITS as usize]);

/// Allocates and initializes an arena without materializing it on the stack.
fn allocate_collector_arena() -> *mut CollectorArena {
    let mut allocation = Box::<CollectorArena>::new_uninit();
    let arena_ptr = allocation.as_mut_ptr();
    // SAFETY: `arena_ptr` comes from a live, correctly aligned `Box`
    // allocation. `addr_of_mut!` does not read the uninitialized field.
    let collectors = unsafe { std::ptr::addr_of_mut!((*arena_ptr).0).cast::<Collector>() };
    for index in 0..usize::BITS as usize {
        // SAFETY: `allocation` has storage for exactly `usize::BITS`
        // collectors. Every slot is written once, and `Collector::new` is a
        // non-panicking const constructor.
        unsafe {
            collectors.add(index).write(Collector::new());
        }
    }
    // SAFETY: every collector in the arena was initialized above.
    Box::into_raw(unsafe { allocation.assume_init() })
}

/// Bitmap representing slot occupancy.
struct Bitmap(AtomicUsize);

impl Bitmap {
    /// Acquires a slot.
    ///
    /// Returns `u8::MAX` if there are no free slots.
    #[inline]
    fn acquire_slot(&self) -> u8 {
        let mut free_slot = 0;
        if self
            .0
            .fetch_update(
                AcqRel,
                Acquire,
                #[inline]
                |bitmap| {
                    free_slot = bitmap.trailing_ones();
                    if free_slot == usize::BITS {
                        None
                    } else {
                        Some(bitmap | (1_usize << free_slot))
                    }
                },
            )
            .is_ok()
        {
            return u8::try_from(free_slot).unwrap_or(u8::MAX);
        }
        u8::MAX
    }

    /// Releases the slot.
    ///
    /// Returns `true` if the bitmap has become empty; the bitmap is marked fully occupied instead
    /// to prevent other threads from acquiring slots until [`Self::reset`] is called.
    #[inline]
    fn release_slot(&self, slot: usize) -> bool {
        debug_assert!(slot < usize::BITS as usize);
        let mut is_empty = false;
        let _result: bool = self
            .0
            .fetch_update(
                AcqRel,
                Acquire,
                #[inline]
                |mut bitmap| {
                    bitmap &= !(1usize << slot);
                    is_empty = bitmap == 0;
                    if is_empty {
                        Some(usize::MAX)
                    } else {
                        Some(bitmap)
                    }
                },
            )
            .is_ok();
        is_empty
    }

    /// Resets the state.
    #[inline]
    fn reset(&self) {
        debug_assert_eq!(self.0.load(Acquire), usize::MAX);
        self.0.store(0, Release);
    }
}

/// Number of [`CollectorArena`] slots in the global arena.
const ARENA_LEN: usize = 32;

/// Global arena of [`Collector`] instances.
///
/// The arena can manage up to `32 * 32` [`Collector`] instances on a 32-bit system, and `64 * 32`
/// on a 64-bit system. The arena itself is `8 * 32` bytes on a 32-bit system and `16 * 32` bytes on
/// a 64-bit system.
static GLOBAL_COLLECTOR_ARENA: ([Bitmap; ARENA_LEN], [AtomicPtr<CollectorArena>; ARENA_LEN]) = (
    [const { Bitmap(AtomicUsize::new(0)) }; ARENA_LEN],
    [const { AtomicPtr::new(null_mut()) }; ARENA_LEN],
);
