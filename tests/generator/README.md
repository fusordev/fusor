# Synchronous generator differential corpus

This bounded corpus covers the admitted synchronous-generator profile,
including plain `yield`, delegated `yield*`, suspension and resume modes,
prototype chains, abrupt completion, iterator closing, method forwarding,
iterator-result identity and validation, getter order, and `finally`
preservation. It also covers synchronous `GeneratorFunction` source
conversion, construction, metadata, execution, and `newTarget` prototypes.

Run it against the pinned QuickJS 2026-06-04 interpreter:

```console
cargo xtask generator-differential --oracle /path/to/qjs
```

Async functions and async generators remain outside this corpus and fail
closed.
