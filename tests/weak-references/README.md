# Weak-reference differential corpus

This bounded ES-first corpus pins the mutually supported deterministic
`WeakRef` and `FinalizationRegistry` behavior of the Rust engine, QuickJS
2026-06-04, and Node: metadata and brands, weak-target validation, constructor
requirements, `newTarget.prototype`, registry registration and removal, and
brand-first method validation.

Run the pinned QuickJS and Rust differential with:

```console
cargo xtask weak-references-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

Run the independent Node oracle with:

```console
node tests/weak-references/node-oracle.mjs
```

GC timing is intentionally excluded because ECMA-262 and hosts leave collection
and cleanup scheduling nondeterministic. Explicit collector regressions for
target clearing, kept-alive behavior, cell order, unregister-before-cleanup,
and resource failure atomicity live in
`crates/quickjs-runtime/tests/vm_weak_refs.rs`.
