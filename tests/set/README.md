# `Set` differential corpus

This bounded ES-first corpus pins the mutually supported `Set` behavior of the
Rust engine, QuickJS 2026-06-04, and Node v24.19.0: surface metadata,
`SameValueZero`, insertion order, constructor ordering and iterator-close
boundaries, `newTarget.prototype`, live iterators, reentrant `forEach`,
`GetSetRecord`, branch-dependent composition, mutation during iteration,
predicate early close, and intrinsic-result construction.

Run the pinned QuickJS and Rust differential with:

```console
cargo xtask set-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

Run the independent Node oracle with:

```console
node tests/set/node-oracle.mjs
```

QuickJS 2026-06-04 additionally exposes `Set.groupBy`, returning a `Map`; Node
v24.19.0 does not. That pinned QuickJS extension and the complete QuickJS
property order are covered directly by
`crates/fusor-runtime/tests/vm_set.rs`.
