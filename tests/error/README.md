# Error differential corpus

`manifest.json` is the bounded executable corpus for the first M4 Error
vertical. It pins the observable behavior of QuickJS 2026-06-04 for `Error`,
`EvalError`, `RangeError`, `ReferenceError`, `SyntaxError`, `TypeError`,
`URIError`, `AggregateError`, and QuickJS's `InternalError`:

- call and construction behavior, family prototype chains, branding, and
  constructor/prototype descriptors, including QuickJS `Error.isError`;
- omitted, empty, coerced, and abruptly failing messages;
- absent, inherited, explicit, accessor-backed, and abruptly failing `cause`
  options, including property descriptors and evaluation order;
- custom `newTarget.prototype`, intrinsic fallback, and AggregateError
  `newTarget` behavior;
- generic `Error.prototype.toString`, its empty/default branches, getter and
  coercion order, abrupt completion, primitive receivers, and descriptor;
- AggregateError iterable collection, `errors` descriptors, ordering,
  acquisition/step failures, iterator closing, and original-error precedence;
- own-property order plus QuickJS's writable, non-enumerable, configurable,
  headerless and snapshotted `stack`; and
- thrown/caught Error objects plus fresh realm-owned intrinsic prototypes.

Run the gate against the pinned interpreter:

```text
cargo xtask error-differential \
  --oracle /private/tmp/quickjs-2026-06-04/qjs
```

Every case is the body of an ordinary dynamic `Function`. The runner first
checks the manifest expectation against a separately bounded pinned-QJS
process, then executes the same body through the fully verified Rust candidate
in a fresh bounded worker/runtime/realm. The paired realm-marker cases make
accidental intrinsic reuse observable, while the non-object `newTarget`
prototype case checks fallback to the realm-owned family intrinsic. Host-driven
cross-realm object transfer remains outside this JavaScript-only corpus.

The shared runtime-differential harness enforces an exact manifest schema and
release string, mandatory suite-specific coverage tags, unique bounded IDs,
bounded ASCII observations, bounded source and manifest sizes, fixed candidate
instruction/dynamic-compilation/runtime limits, and per-process wall-clock and
stream limits. Oracle cases additionally run with a 64 MiB memory limit and a
1 MiB stack limit in unique temporary files. The Error manifest rejects direct
`eval`, Unicode identifier escapes, and asynchronous `async`/`await` text.
