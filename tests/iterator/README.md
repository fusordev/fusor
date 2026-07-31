# Iterator differential corpus

This bounded corpus compares synchronous iterator consumers in the pure-Rust
runtime with the pinned QuickJS 2026-06-04 `qjs` oracle. The corpus currently
contains 40 cases under a strict 51-feature-tag contract. Its admitted
consumers are array-literal spread and ordinary synchronous `for-of`. Both are
deliberately generic: built-in Array and String iterators and ordinary
JavaScript-authored iterators use the same abstract operation pipeline.

The synchronous `for-of` matrix fixes protocol order and retained receivers;
`var`, `let`, `const`, identifier, static-member, and computed-member heads;
fresh lexical captures; natural exhaustion; same-loop `continue`; normal
`break` and `return` closing; nested labeled close order; pending body and
binding exceptions; and the no-close boundary for `next`, `done`, and `value`
failures. It also pins the distinct QuickJS `Symbol.iterator` acquisition
errors for `null` and `undefined`. `for await` and destructuring heads remain
outside this milestone and must stay fail-closed.

Run it with:

```sh
cargo xtask iterator-differential --oracle /path/to/quickjs-2026-06-04/qjs
```

The manifest has a strict feature-tag contract so protocol ordering, abrupt
completion, and iterator closing cannot silently disappear from the gate.
