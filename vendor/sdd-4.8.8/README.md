# Scalable Delayed Dealloc

[![Cargo](https://img.shields.io/crates/v/sdd)](https://crates.io/crates/sdd)
![Crates.io](https://img.shields.io/crates/l/sdd)

A scalable lock-free delayed memory reclaimer that emulates garbage collection by tracking memory reachability.

The delayed deallocation algorithm is based on a variant of epoch-based reclamation where retired memory chunks are temporarily kept in thread-local storage until they are no longer reachable. It is similar to [`crossbeam_epoch`](https://docs.rs/crossbeam-epoch/), however, users will find `sdd` more straightforward to use as it provides both smart pointer and raw pointer types. For instance, `sdd::AtomicOwned`, `sdd::Owned`, `sdd::AtomicShared`, and `sdd::Shared` automatically retire the contained value when the last reference is dropped.

## Features

* Lock-free epoch-based reclamation. [^1]
* Provides both managed and unmanaged pointer types.
* [`Loom`](https://crates.io/crates/loom) support: `features = ["loom"]`.

## Examples

* `AtomicOwned` and `Owned` for managed instances.
* `AtomicShared` and `Shared` for reference-counted instances: slower than `AtomicOwned` and `AtomicRaw`.
* `AtomicRaw` for manual pointer operations: faster than `AtomicOwned` and `AtomicShared`.

```rust
use sdd::{AtomicOwned, AtomicRaw, AtomicShared, Guard, Owned, PrivateCollector, Ptr, RawPtr, Shared, Tag};
use std::sync::atomic::Ordering::Relaxed;

// `AtomicOwned<T>` and `Owned<T>` are smart pointers that exclusively own an instance of type `T`.
//
// `atomic_owned` owns `19`.
let atomic_owned: AtomicOwned<usize> = AtomicOwned::new(19);

// `AtomicShared<T>` and `Shared<T>` are smart pointers that retain shared ownership of an instance
// of type `T`.
//
// These types use an atomic counter to track references, which makes them generally slower than
// `AtomicOwned<T>` and `AtomicRaw<T>`.
//
// `atomic_shared` holds a reference to `17`.
let atomic_shared: AtomicShared<usize> = AtomicShared::new(17);

// `AtomicRaw<T>` is analogous to `AtomicPtr<T>` in the standard library.
//
// `atomic_raw` can be used for atomic raw pointer operations.
let atomic_raw: AtomicRaw<usize> = AtomicRaw::null();

// `guard` prevents the garbage collector from dropping reachable instances.
let guard = Guard::new();

// `ptr` is protected by `guard` and cannot outlive it.
let mut ptr: Ptr<usize> = atomic_owned.load(Relaxed, &guard);
assert_eq!(*ptr.as_ref().unwrap(), 19);

// `atomic_owned`, `atomic_shared`, and `atomic_raw` can be tagged.
atomic_owned.update_tag_if(Tag::First, |p| p.tag() == Tag::None, Relaxed, Relaxed);
atomic_shared.update_tag_if(Tag::Second, |p| p.tag() == Tag::None, Relaxed, Relaxed);
atomic_raw.compare_exchange(
    RawPtr::null(),
    RawPtr::null().with_tag(Tag::Both),
    Relaxed,
    Relaxed,
    &guard);

// `ptr` is not tagged, so the compare-and-swap operation fails.
assert!(atomic_owned.compare_exchange(
    ptr,
    (Some(Owned::new(18)), Tag::First),
    Relaxed,
    Relaxed,
    &guard).is_err());

// `ptr` can be tagged.
ptr.set_tag(Tag::First);

// The ownership of the contained instance is transferred to the return value.
let prev = atomic_shared.swap((Some(Shared::new(19)), Tag::None), Relaxed).0.unwrap();
assert_eq!(*prev, 17);

// `17` will be garbage-collected later.
drop(prev);

// `atomic_shared` can be converted into a `Shared`.
let shared: Shared<usize> = atomic_shared.into_shared(Relaxed).unwrap();
assert_eq!(*shared, 19);

// `Owned` can be converted into a `RawPtr` and set to `atomic_raw`.
let owned = Owned::new(7);
let raw_ptr = owned.into_raw();
atomic_raw.store(raw_ptr, Relaxed);

// `18` and `19` will be garbage-collected later.
drop(shared);
drop(atomic_owned);

// `19` is still valid as `guard` prevents the garbage collector from dropping it.
assert_eq!(*ptr.as_ref().unwrap(), 19);

// `7` can also be accessed through `atomic_raw`.
let raw_ptr = atomic_raw.load(Relaxed, &guard);

// Using a `RawPtr` requires an `unsafe` block.
assert_eq!(unsafe { *raw_ptr.into_ptr().as_ref().unwrap() }, 7);

// `RawPtr` can be converted into an `Owned` for automatic memory management.
let owned = unsafe { Owned::from_raw(raw_ptr).unwrap() };
drop(owned);

// Execution of a closure can be deferred until all current readers are gone.
guard.defer_execute(|| println!("deferred"));
drop(guard);

// `Owned` and `Shared` can be nested.
let shared_nested: Shared<Owned<Shared<usize>>> = Shared::new(Owned::new(Shared::new(20)));
assert_eq!(***shared_nested, 20);

// `PrivateCollector` is useful when memory must be reclaimed by a strict deadline.
let private_collector = PrivateCollector::new();
let owned = Owned::new(37);
let shared = Shared::new(43);

unsafe {
    // `37` and `43` cannot outlive `private_collector`.
    private_collector.collect_owned(owned, &Guard::new());
    assert!(private_collector.collect_shared(shared, &Guard::new()));
}

// Dropping `private_collector` immediately deallocates both `37` and `43`.
drop(private_collector);
```

## Memory Overhead

Retired instances are stored in intrusive queues in thread-local storage, requiring additional space for `(AtomicPtr<()> /* next */, fn(*mut ()) /* drop */)` per instance.

## Performance

The average time taken to enter and exit a protected region: less than a nanosecond on Apple M4 Pro.

## Applications

[`sdd`](https://crates.io/crates/sdd) provides widely used lock-free concurrent data structures, including [`LinkedList`](#linkedlist), [`Bag`](#bag), [`Queue`](#queue), and [`Stack`](#stack). Note that these are implemented using `AtomicShared` rather than `AtomicRaw` to prioritize functionality over performance. For performance-critical applications, consider implementing data structures with `AtomicRaw` instead.

### `LinkedList`

[`LinkedList`](#linkedlist) is a trait that implements lock-free concurrent singly linked list operations and provides a method for marking a linked list entry with a user-defined state.

```rust
use std::sync::atomic::Ordering::Relaxed;

use sdd::{AtomicShared, Guard, LinkedList, Shared};

#[derive(Default)]
struct L(AtomicShared<L>, usize);
impl LinkedList for L {
    fn link_ref(&self) -> &AtomicShared<L> {
        &self.0
    }
}

let guard = Guard::new();

let head: L = L::default();
let tail: Shared<L> = Shared::new(L(AtomicShared::null(), 1));

// A new entry is pushed.
assert!(head.push_back(tail.clone(), false, Relaxed, &guard).is_ok());
assert!(!head.is_marked(Relaxed));

// Users can mark a flag on an entry.
head.mark(Relaxed);
assert!(head.is_marked(Relaxed));

// `next_ptr` traverses to the next entry in the linked list.
let next_ptr = head.next_ptr(Relaxed, &guard);
assert_eq!(next_ptr.as_ref().unwrap().1, 1);

// Once `tail` is deleted, it becomes unreachable.
tail.delete_self(Relaxed);
assert!(head.next_ptr(Relaxed, &guard).is_null());
```

### `Bag`

[`Bag`](#bag) is a concurrent lock-free unordered container that is completely opaque, disallowing access to contained instances until they are popped. It is especially efficient when the number of contained instances is maintained under `ARRAY_LEN` (default: `usize::BITS / 2`).

```rust
use sdd::Bag;

let bag: Bag<usize> = Bag::default();

bag.push(1);
assert!(!bag.is_empty());
assert_eq!(bag.pop(), Some(1));
assert!(bag.is_empty());
```

### `Queue`

[`Queue`](#queue) is a concurrent lock-free first-in-first-out container.

```rust
use sdd::Queue;

let queue: Queue<usize> = Queue::default();

queue.push(1);
assert!(queue.push_if(2, |e| e.map_or(false, |x| **x == 1)).is_ok());
assert!(queue.push_if(3, |e| e.map_or(false, |x| **x == 1)).is_err());
assert_eq!(queue.pop().map(|e| **e), Some(1));
assert_eq!(queue.pop().map(|e| **e), Some(2));
assert!(queue.pop().is_none());
```

### `Stack`

[`Stack`](#stack) is a concurrent lock-free last-in-first-out container.

```rust
use sdd::Stack;

let stack: Stack<usize> = Stack::default();

stack.push(1);
stack.push(2);
assert_eq!(stack.pop().map(|e| **e), Some(2));
assert_eq!(stack.pop().map(|e| **e), Some(1));
assert!(stack.pop().is_none());
```

## [Changelog](https://codeberg.org/wvwwvwwv/scalable-delayed-dealloc/src/branch/main/CHANGELOG.md)

[^1]: `PrivateCollector` is not lock-free, though the lock is not contended.
