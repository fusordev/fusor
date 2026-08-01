# Codebase review

Reviewed: 2026-08-01

This review focuses on the current bound-function host-call path, array
destructuring assignment lowering, Function intrinsic descriptors, and the
corresponding porting-status claims. No changes were made to production code.

## Findings

### P1 — Bound native calls discard the bound receiver

**Location:** `crates/quickjs-runtime/src/vm.rs:1558`

`Context::call` unwraps a bound function and records `bound_this` in
`receiver`, but the native-function branch passes only the materialized
arguments to `execute_native_entry`. That entry therefore receives its usual
`undefined` receiver. Calling a native target through a public bound value,
such as `Function.prototype.call.bind(f)`, loses `f` and behaves as an
unbound call.

Pass the accumulated `receiver` to the native-entry path and construct its
`CallInputs` with that receiver. Add a regression that invokes a native target
through a bound public function.

### P1 — Nested bound native calls drop earlier bound arguments

**Location:** `crates/quickjs-runtime/src/vm.rs:1571-1584`

Each bound layer appends its arguments to the original public `arguments`
slice. It does not append them to `owned_arguments` accumulated by an outer
bound layer. Consequently, a host call of
`f.bind(null, 1).bind(null, 2)` reaches `f` with `1` plus the public arguments,
but omits `2`.

Merge each layer's `bound_arguments` with the current accumulated argument
buffer, then append the public arguments only once. Cover nested binds in the
host-call regression corpus.

### P2 — Array assignment member targets are evaluated before the iterator step

**Location:** `crates/quickjs-compiler/src/lowering.rs:5313-5391`

The array-assignment planner schedules the member target prelude so it runs
before `ForOfNext`. ECMAScript requires obtaining the next iterator value
before evaluating the assignment target. Therefore `[base().x] = iterable`
observes `base()` before an observable `next()` call. The rest-target planner
has the same ordering flaw: it evaluates `rest.target` before collecting the
remaining iterator values (`:5977-5989`).

Delay evaluation of a member target's base and computed key until the matching
iterator value (or the rest array) is ready to store. Add ordering tests with
observable `next`, base, and computed-key effects.

### P2 — `Function.prototype[Symbol.hasInstance]` is mutable

**Location:** `crates/quickjs-runtime/src/runtime/realm.rs:1405-1409`

The intrinsic is published with `METHOD_PROPERTY`, whose descriptor is
writable and configurable. The specification, and the pinned QuickJS behavior,
require this property to be non-writable and non-configurable. Its current
descriptor lets assignment replace the inherited `instanceof` behavior for
all functions.

Install this property using `FROZEN_PROPERTY` and add strict and non-strict
assignment descriptor regressions.

### P2 — `PORTING.md` marks affected semantics complete

**Location:** `PORTING.md:75-81`

The porting status lists destructuring and runtime calls/native dispatch as
complete, but the findings above show observable failures in both admitted
areas. This makes the document an unreliable completion gate and can cause
later work to assume the affected profile is conformant.

Move these claims back to an incomplete or explicitly-qualified status until
the regressions pass. The descriptor/symbol completion language at
`PORTING.md:87-98` should likewise call out the remaining Function intrinsic
descriptor gap.
