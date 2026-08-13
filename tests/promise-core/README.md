# Promise core differential corpus

This bounded corpus compares the admitted intrinsic Promise core with the
pinned QuickJS 2026-06-04 `qjs` oracle. It checks constructor and thenable-`Get`
order, deferred reactions, branding, generic capabilities, `@@species`, and the
generic `catch` and `finally` paths. It also pins the full Promise constructor
surface (`all`, `allSettled`, `any`, `try`, `race`, and `withResolvers`),
combinator input ordering and empty behavior, settlement records,
`AggregateError.errors`, and abrupt iterator closing. Runtime-owned FIFO job
draining, nested-job fixed points, and the ES-first hostile-thenable regression
are covered separately by `fusor-runtime` tests.

Run it with:

```console
cargo xtask promise-core-differential \
  --oracle /private/tmp/quickjs-2026-06-04/qjs
```
