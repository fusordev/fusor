# Iterator differential corpus

This bounded corpus compares synchronous iterator consumers in the pure-Rust
runtime with the pinned QuickJS 2026-06-04 `qjs` oracle. The first admitted
consumer is array-literal spread. It is deliberately generic: built-in Array
and String iterators and ordinary JavaScript-authored iterators use the same
abstract operation pipeline.

Run it with:

```sh
cargo xtask iterator-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

The manifest has a strict feature-tag contract so protocol ordering, abrupt
completion, and iterator closing cannot silently disappear from the gate.
