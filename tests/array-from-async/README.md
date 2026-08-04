# `Array.fromAsync` differential corpus

This bounded ES-first corpus pins `Array.fromAsync` surface metadata,
evaluation order, async-iterator preference, sync fallback, array-like value
awaiting, mapper awaiting, strict final `length`, and exceptional
`AsyncIteratorClose` boundaries.

Pinned QuickJS 2026-06-04 does not expose `Array.fromAsync`, so it cannot serve
as the behavioral oracle for this post-profile intrinsic. The pinned absence is
checked with:

```console
/path/to/qjs -e 'print(Object.getOwnPropertyNames(Array).join(","))'
```

The mutually supported behavior is compared with Node v24.19.0:

```console
node tests/array-from-async/node-oracle.mjs
```

Two current-spec close boundaries remain engine-only checks: Node v24.19.0
calls `return()` after a rejected `next()` and after a failing final `length`
write, although ECMA-262 exits those steps directly. A rejecting async
`return()` after mapper rejection also leaves that Node release in an endless
microtask loop, so the differential corpus uses a successful close while the
engine suite pins original-error precedence separately.

The same boundaries, plus forced collection of a suspended operation, are
covered by `quickjs-runtime/tests/vm_array_statics.rs`.
