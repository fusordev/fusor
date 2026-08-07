# `String.prototype.split` differential corpus

This bounded ES2025-first corpus pins the mutually supported deterministic
`String.prototype.split` behavior of the Rust engine, QuickJS 2026-06-04, and
Node: method shape, descriptors, non-overlapping plain-string matches, empty
pieces, UTF-16 code-unit boundaries, `ToUint32` limit handling, `@@split`
dispatch, fallback conversion order, error classes, and abrupt ordering.

Run the pinned QuickJS and Rust differential with:

```console
cargo xtask string-split-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

Run the independent Node oracle with:

```console
node tests/string-split/node-oracle.mjs
```

Native `RegExp` matching remains outside this corpus until the runtime installs
the RegExp intrinsic. RegExp-like protocol objects cover the observable
`@@split` ordering required at this milestone. Primitive `@@split` lookup is
covered by the Rust conformance regression but excluded here because pinned
QuickJS skips the ES2025 `GetMethod` lookup that Node and the Rust engine perform.
