# Weak-collections differential corpus

This bounded ES-first corpus pins the mutually supported `WeakMap` and
`WeakSet` behavior of the Rust engine, QuickJS 2026-06-04, and Node v24.19.0:
core metadata and brands, objects and non-registered Symbols as weak keys,
registered-Symbol rejection, constructor ordering and iterator-close
boundaries, `newTarget.prototype`, primitive queries, and brand-first errors.

Run the pinned QuickJS and Rust differential with:

```console
cargo xtask weak-collections-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

Run the independent Node oracle with:

```console
node tests/weak-collections/node-oracle.mjs
```

QuickJS 2026-06-04 additionally exposes `WeakMap.prototype.getOrInsert` and
`getOrInsertComputed`; Node v24.19.0 does not. Their complete metadata,
validation ordering, and reentrant update semantics are covered directly by
`crates/quickjs-runtime/tests/vm_weak_collections.rs`.
