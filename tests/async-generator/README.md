# Async generator differential corpus

This bounded manifest compares async-generator evaluation, request queueing,
`await`/`yield`/`return` settlement, intrinsic identity, methods, and dynamic
`AsyncGeneratorFunction` construction with pinned QuickJS 2026-06-04:

```console
cargo xtask async-generator-differential --oracle /path/to/qjs
node tests/async-generator/node-oracle.mjs
```

Each body returns a `{ result, done }` record. The oracle waits for `done`; the
Rust candidate drains its bounded FIFO Promise-job queue and then reads
`result` in a separate dynamic function call.
