# `String.prototype.replaceAll` differential corpus

This bounded ES-first corpus pins the mutually supported deterministic
`String.prototype.replaceAll` behavior of the Rust engine, QuickJS 2026-06-04,
and Node: surface metadata and order, non-overlapping plain-string matches,
empty-search boundaries, `GetSubstitution`, callback arguments and result
coercion, UTF-16 positions, `IsRegExp`, global-flag enforcement, `@@replace`
dispatch, error classes, and abrupt ordering.

Run the pinned QuickJS and Rust differential with:

```console
cargo xtask string-replace-all-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

Run the independent Node oracle with:

```console
node tests/string-replace-all/node-oracle.mjs
```

Native `RegExp` matching remains outside this corpus until the runtime installs
the RegExp intrinsic. RegExp-like protocol objects cover the observable
`IsRegExp`, `flags`, and `@@replace` ordering required at this milestone.
Primitive `@@replace` lookup is covered by the Rust conformance regression but
excluded here because pinned QuickJS skips the ES `GetMethod` lookup that Node
and the Rust engine perform.
