# Async function differential corpus

This bounded manifest compares async-function evaluation, `await`, Promise
settlement, intrinsic identity, methods, and dynamic `AsyncFunction`
construction with pinned QuickJS 2026-06-04:

```console
cargo xtask async-function-differential --oracle /path/to/qjs
```

Each body returns a `{ result, done }` record. The oracle waits for `done`; the
Rust candidate drains its bounded FIFO Promise-job queue and then reads
`result` in a separate dynamic function call.
