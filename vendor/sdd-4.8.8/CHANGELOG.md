# Changelog

4.8.8

* Fix [`#14`](https://codeberg.org/wvwwvwwv/scalable-delayed-dealloc/issues/14): unsound `Sync` bounds in `AtomicOwned` and `Owned`.
 
4.8.7

* Fix [`#14`](https://codeberg.org/wvwwvwwv/scalable-delayed-dealloc/issues/14): unsound `Sync` bounds in `AtomicShared` and `Shared`.
* Update dependencies.

4.8.6

* Optimize `PrivateCollector`.

4.8.5

* Minor doc updates.

4.8.4

* API update: add `AtomicRaw::update_tag_if`.
* Minor doc updates.

4.8.3

* Minor doc updates.

4.8.2

* More robust OOM handling.

4.8.1

* Fix `loom` issues with the `scc` crate.

4.8.0

* API update: `PrivateCollector` enables forced bulk-reclamation of memory chunks: [`#6`](https://codeberg.org/wvwwvwwv/scalable-delayed-dealloc/issues/6).
* API update: add `{Owned, Shared}::{new_checked, new_with_checked}`.
* API update: remove `suspend`; the function is unsound since this allows instances of `!Send` types to move to another thread.
* Fix a data race in `AtomicOwned` and `AtomicShared` when storing `Owned` or `Shared` with `Relaxed`: [`#12`](https://codeberg.org/wvwwvwwv/scalable-delayed-dealloc/issues/12).
* Fix a data race in `ebr` when a thread is spawned in `drop`.

4.7.6

* Minor inline optimization.

4.7.5

* Fix performance regression in 4.6.6.

4.7.4

* Minor doc updates.

4.7.3

* Fix `MSRV` violation, affected versions = `[4.7.0, 4.7.2]`: [`#8`](https://codeberg.org/wvwwvwwv/scalable-delayed-dealloc/issues/8).

4.7.2

* API update: `AtomicRaw::store` no longer requires the caller to pass a reference to a `Guard`.

4.7.0 - 4.7.1

* Add `AtomicRaw` and `RawPtr` for atomic raw pointer operations.

4.6.6 - 4.6.7

* The size of `Collector` has been reduced by `50%`: `128B` -> `64B`.

4.6.5

* Fix a spurious error in `Queue::is_empty`.

4.6.4

* Optimize thread-local `Collector` traversal by using an arena allocator.

4.6.3

* Accelerate memory reclamation on `{Bag, Queue, Stack}::drop`.

4.6.1 - 4.6.2

* Add support for `MIRIFLAGS="-Zmiri-strict-provenance"`.
 
4.6.0

* Add `{Shared, Unique}::{new_with, new_with_unchecked}`.

4.5.3

* Minor optimization of scanning remote thread-local variables.

4.5.2

* Adjust epoch countdown parameters.

4.5.1

* Migrate to [`codeberg`](https://codeberg.org/wvwwvwwv/scalable-delayed-dealloc).
* Remove `LinkedEntry::take_inner`: the method is dangerous to use.

4.5.0

* Add `Guard::set_has_garbage`.

4.4.0

* Add `Ptr::as_{ptr|ref}_unchecked`.
* Add `{Owned|Shared}::as_non_null_ptr`.

4.3.5

* Prepare for an upcoming Rust breaking change: [`Rust#136702`](https://github.com/rust-lang/rust/issues/136702).

4.3.4

* Add `Ptr::as_ref_unchecked`.

4.3.3

* Minor code cleanup.

4.3.2

* Add `Bag::try_push`.

4.3.1

* Add lock-free concurrent data structures: `Bag`, `LinkedList`, `Queue`, and `Stack`.

4.2.5

* Add `Guard::has_garbage`.

4.2.4

* Minor optimization.

4.2.2 - 4.2.3

* `Guard::accelerate` now only accelerates garbage collection of the current thread without affecting other threads.

4.2.1

* `u8` can be converted to `Epoch`.

4.2.0

* `Epoch` uses a range of `[0, 63]` `u8` values instead of rotating four values.

4.1.2

* More const functions.

4.1.1

* Let `miri` not execute Intel-specific code paths.

4.1.0

* The size of `Option<Guard>` is now that of `Guard`.

4.0.1

* Minor improvements to documentation.

4.0.0

* Bump MSRV to 1.85.0 / Edition 2024.

3.0.10

* Minor epoch update policy optimization.
* Minor `NonNull` optimization on `Owned` and `Shared`.

3.0.9

* Fix unsound `Sync` implementations of `AtomicShared` and `Shared`; previously, the `Sync` implementation allowed an arbitrary thread to own or drop the contained instance.

3.0.8

* Minor `const` optimization.

3.0.7

* Fix a use-after-free issue when thread-local storage is dropped.

3.0.5

* Fix minor linting errors.

3.0.4

* Adjust tests to be more `Miri` friendly.

3.0.3

* Fix a rare memory ordering issue when dropping thread-local storage.

3.0.2

* Make `SDD` much more Miri-friendly.

3.0.1

* Compatible with the [`Miri`](https://github.com/rust-lang/miri) memory leak checker.
* Make `Collectible` private since it is unsafe.
* Remove `Guard::defer` which depends on `Collectible`.
* Remove `prepare`.

2.1.0

* Minor performance optimization.
* Remove `Owned::release`.

2.0.0

* `{Owned, Shared}::release` no longer receives a `Guard`.
* `Link` is now public.

1.7.0

* Add `loom` support.

1.6.0

* Add `Guard::accelerate`.

1.5.0

* Fix `Guard::epoch` to return the correct epoch value.

1.4.0

* `Epoch` is now a 4-state type (3 → 4).

1.3.0

* Add `Epoch`
* Add `Guard::epoch`.

1.2.0

* Remove `Collectible::drop_and_dealloc`.

1.1.0

* Add `prepare`.

1.0.1

* Relaxed trait bounds of `Guard::defer_execute`.

1.0.0

* Minor code cleanup.


0.2.0

* Make `Guard` `UnwindSafe`.

0.1.0

* Minor optimization.

0.0.1

* Initial commit: code copied from [`scalable-concurrent-containers`](https://github.com/wvwwvwwv/scalable-concurrent-containers).
