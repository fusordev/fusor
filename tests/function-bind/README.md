# `Function.prototype.bind`, bound functions, and `instanceof` differential corpus

`manifest.json` is a bounded runtime corpus for the ordinary
`Function.prototype.bind` / bound-function / `Symbol.hasInstance` /
`instanceof` milestone. Run it against the pinned official QuickJS release:

```text
cargo xtask function-bind-differential \
  --oracle /path/to/quickjs-2026-06-04/qjs
```

The suite checks the intrinsic's `name`/`length`, writability, and
non-enumerability; exact native source text and non-constructability error;
target-callability order; the pinned bound-function `length` rules (missing,
integer, fractional-truncating, `NaN`, and non-Number target lengths; bound
argument subtraction; bound-of-bound chaining; native targets); the `"bound "`
name prefix; receiver override against `call`/`apply` forwarding; bound
argument prepending; construction with bound arguments and `newTarget`
substitution; the `"bound apply is not a constructor"` error naming;
`typeof` and `toString` rendering; the ordinary `instanceof` prototype chain
with primitive left operands; the exact non-callable right-operand
`TypeError`; custom `@@hasInstance` methods on functions and plain objects;
the `Function.prototype[Symbol.hasInstance]` ordinary path; and bound-target
unwrapping inside `instanceof`.

The manifest is pinned to QuickJS 2026-06-04. The runner first executes every
case in a fresh resource-bounded official `qjs` process and rejects the corpus
if any checked-in observation has drifted. It then compiles the same ordinary
dynamic-`Function` body through the public Rust facade and runs only the fully
verified function graph in a fresh bounded runtime worker. Before the runtime
milestone lands, candidate mismatches are intentional fail-first evidence;
the command succeeds only when every candidate observation matches the pinned
oracle.

The command shares the hardened runtime-differential transport with the
control-flow, error, function-apply, and iterator suites: strict manifest
fields and required coverage tags, bounded source/result sizes, fresh
processes and realms, wall-clock timeouts, instruction/resource limits,
bounded mismatch output, and fail-closed `eval` text rejection.
