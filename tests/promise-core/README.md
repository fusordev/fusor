# Promise core differential corpus

This bounded corpus compares the admitted intrinsic Promise core with the
pinned QuickJS 2026-06-04 `qjs` oracle. It checks synchronous constructor and
thenable-`Get` order, deferred reactions, receiver validation, branding, and
the generic `catch` and `finally` invocation paths. It also pins `newTarget.prototype`
selection, generic constructor capabilities, `@@species`, fallback,
validation, `finally` handler metadata and non-callable pass-through, and
abrupt-completion order. Runtime-owned FIFO draining and
nested-job fixed points are covered separately by `quickjs-runtime` two-turn
tests.

Run it with:

```console
cargo xtask promise-core-differential \
  --oracle /private/tmp/quickjs-2026-06-04/qjs
```
