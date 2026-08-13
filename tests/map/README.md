# `Map` differential corpus

This bounded ES-first corpus pins the mutually supported `Map` behavior of the
Rust engine, QuickJS 2026-06-04, and Node v24.19.0: surface metadata,
`SameValueZero`, insertion order, constructor ordering and iterator-close
boundaries, `newTarget.prototype`, live iterators, reentrant `forEach`, and
`Map.groupBy`.

Run the pinned QuickJS and Rust differential with:

```console
cargo xtask map-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

Run the independent Node oracle with:

```console
node tests/map/node-oracle.mjs
```

QuickJS 2026-06-04 also exposes `Map.prototype.getOrInsert` and
`Map.prototype.getOrInsertComputed`; Node v24.19.0 does not. Their semantics and
the complete pinned QuickJS property order are therefore covered directly by
`crates/fusor-runtime/tests/vm_map.rs`. `FUS-MAP-001` records the one tested
compatibility difference: when the computed callback inserts the requested key,
ECMA-262 updates that entry in place while pinned QuickJS deletes and re-appends
it. The Rust engine follows the specification and preserves insertion position.
