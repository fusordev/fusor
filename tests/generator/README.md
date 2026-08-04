# Synchronous generator differential corpus

This bounded corpus covers the admitted plain-`yield` generator profile,
including suspension, resume modes, prototype chains, abrupt completion,
iterator closing, and `finally` preservation.

Run it against the pinned QuickJS 2026-06-04 interpreter:

```console
cargo xtask generator-differential --oracle /path/to/qjs
```

`yield*`, dynamic `GeneratorFunction` compilation, async functions, and async
generators remain outside this corpus and fail closed.
