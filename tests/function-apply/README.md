# `Function.prototype.apply` differential corpus

`manifest.json` is a bounded runtime corpus for the ordinary
`Function.prototype.apply` milestone. Run it against the pinned official
QuickJS release:

```text
cargo xtask function-apply-differential \
  --oracle /path/to/quickjs-2026-06-04/qjs
```

The suite checks the intrinsic's `name`/`length`, writability, and
non-enumerability; exact native source text and non-constructability error;
target-validation order; nullish
and primitive argument-list handling, ordinary/function/boxed-String
array-like inputs, receiver forwarding, inherited and missing indexed
properties, observable length/index coercion order, mutation between indexed
reads, abrupt indexed-Get propagation, non-wrapping `ToLength` behavior, and both sides of QuickJS's
65,534-argument ceiling. Exact configurable data-descriptor shape is covered
by the runtime intrinsic tests because general JavaScript descriptor/delete
operations remain a separate frontend/runtime milestone.

The manifest is pinned to QuickJS 2026-06-04. The runner first executes every
case in a fresh resource-bounded official `qjs` process and rejects the corpus
if any checked-in observation has drifted. It then compiles the same ordinary
dynamic-`Function` body through the public Rust facade and runs only the fully
verified function graph in a fresh bounded runtime worker. Before the runtime
milestone lands, candidate mismatches are intentional fail-first evidence;
the command succeeds only when every candidate observation matches the pinned
oracle.

The command shares the hardened runtime-differential transport with the
control-flow suite: strict manifest fields and required coverage tags, bounded
source/result sizes, fresh processes and realms, wall-clock timeouts,
instruction/resource limits, bounded mismatch output, and fail-closed `eval`
text rejection.
