# Codebase review

Reviewed: 2026-08-01
Resolved: 2026-08-01

This review focuses on the current bound-function host-call path, array
destructuring assignment lowering, Function intrinsic descriptors, and the
corresponding porting-status claims.

Four of the five findings were confirmed and fixed. One (the array-assignment
evaluation order) was checked against ECMAScript and the pinned QuickJS oracle
and rejected as incorrect; the pinned behavior is now covered by explicit
ordering regressions so the question cannot be reopened by inspection alone.

## Findings

### P1 — Bound native calls discard the bound receiver — FIXED

**Location:** `crates/quickjs-runtime/src/vm.rs:1558`

`Context::call` unwraps a bound function and records `bound_this` in
`receiver`, but the native-function branch passed only the materialized
arguments to `execute_native_entry`. That entry therefore received its usual
`undefined` receiver. Calling a native target through a public bound value,
such as `Function.prototype.call.bind(f)`, lost `f` and behaved as an
unbound call.

`execute_native_entry` now takes the accumulated `receiver` and builds its
`CallInputs` with it (`crates/quickjs-runtime/src/vm/native.rs:610-630`).

Regressions: `host_calls_pass_the_bound_receiver_to_native_targets`,
`host_calls_pass_the_bound_receiver_to_bytecode_targets`, and
`host_calls_use_the_innermost_bound_receiver` in
`crates/quickjs-runtime/tests/vm_bind.rs`.

### P1 — Nested bound native calls drop earlier bound arguments — FIXED

**Location:** `crates/quickjs-runtime/src/vm.rs:1571-1584`

Each bound layer appended its arguments to the original public `arguments`
slice. It did not append them to `owned_arguments` accumulated by an outer
bound layer. Consequently, a host call of `f.bind(null, 1).bind(null, 2)`
reached `f` with `1` plus the public arguments, but omitted `2`.

Each layer now merges its `bound_arguments` with the current accumulated
buffer, appending the public arguments only on the first (outermost) layer.

Regressions: `host_calls_accumulate_nested_bound_arguments` and
`host_calls_accumulate_nested_bound_arguments_for_native_targets`.

Every one of these five host-call regressions fails against the unmodified
production code and passes after the fix.

### P2 — Array assignment member target ordering — REJECTED, NOT A DEFECT

**Location:** `crates/quickjs-compiler/src/lowering.rs:5313-5391`

The finding claimed ECMAScript requires obtaining the next iterator value
before evaluating the assignment target, and that the existing
target-before-`ForOfNext` schedule is therefore wrong. That is backwards.

ECMAScript's IteratorDestructuringAssignmentEvaluation for
`AssignmentElement : DestructuringAssignmentTarget Initializer?` evaluates
`leftRef` **first**, then calls `IteratorStepValue`. Its note is explicit:

> Left to right evaluation order is maintained by evaluating a
> DestructuringAssignmentTarget that is not a destructuring pattern prior to
> accessing the iterator or evaluating the Initializer.

`AssignmentRestElement` evaluates `leftRef` before its collection loop for the
same reason, so the rest-target planner is also correct.

The pinned QuickJS 2026-06-04 reference agrees: `get_lvalue` runs before
`OP_for_of_next` (`quickjs.c:26596-26612`). Executing
`[base().x] = iterable` under the pinned `qjs` binary observes
`getIterator,base,next0`; V8 observes the same order.

The existing lowering was already correct, so no production change was made.
Four ordering regressions now pin the behavior with observable `next`, base,
and computed-key effects, matching the oracle byte for byte:
`array_assignment_member_bases_evaluate_before_the_iterator_step`,
`array_assignment_computed_keys_evaluate_before_the_iterator_step`,
`array_assignment_rest_targets_evaluate_before_collecting_values`, and
`array_assignment_member_bases_interleave_with_each_iterator_step` in
`crates/quickjs-runtime/tests/vm_destructuring.rs`.

### P2 — `Function.prototype[Symbol.hasInstance]` is mutable — FIXED

**Location:** `crates/quickjs-runtime/src/runtime/realm.rs:1405-1409`

The intrinsic was published with `METHOD_PROPERTY`, whose descriptor is
writable and configurable. QuickJS pins this property as non-writable and
non-configurable (`quickjs.c:39511-39523`). Its previous descriptor let
assignment replace the inherited `instanceof` behavior for all functions.

The property now uses `FROZEN_PROPERTY`.

Two stale tests encoded the old buggy behavior and were corrected against the
pinned oracle rather than preserved:

- `instanceof_consults_a_custom_symbol_has_instance_method` assigned through
  the inherited slot, which the oracle silently discards. It now defines the
  predicate as an own property.
- Corpus case 16 in `tests/function-bind/manifest.json` expected
  `true:false`; the oracle produces `false:false`. The case now covers both
  the discarded inherited assignment and a working own definition.

Regressions: `sloppy_assignment_cannot_replace_inherited_has_instance`,
`strict_assignment_to_inherited_has_instance_throws` (pinned
`TypeError: 'Symbol.hasInstance' is read-only`), and
`own_has_instance_definitions_override_the_frozen_inherited_slot`, plus new
corpus case `function-prototype-has-instance-descriptor-is-frozen` under the
new `symbol-has-instance-descriptor` required coverage tag.

### P2 — `PORTING.md` marks affected semantics complete — RESOLVED

**Location:** `PORTING.md:75-81`

Because the underlying defects are now fixed and covered by regressions that
demonstrably fail without the fixes, the completion claims are accurate rather
than demoted. The claims were made specific instead of being left vague:

- The lowering entry states the pinned array-assignment evaluation order and
  cites both the specification operation and `quickjs.c:26596-26612`.
- The runtime dispatch entry states that host calls unwrap bound functions
  with the same observable result as interpreter dispatch, covering both the
  innermost bound receiver and argument accumulation.
- The descriptor entry records the frozen
  `Function.prototype[Symbol.hasInstance]` and cites
  `quickjs.c:39511-39523`.

## Verification

```console
cargo fmt --all --check                                              # clean
cargo clippy --workspace --all-targets --all-features -- -D warnings # clean
cargo test --workspace                                               # all pass
cargo xtask function-bind-differential      --oracle <pinned qjs>    # 21/21, 23 tags
cargo xtask control-flow-differential       --oracle <pinned qjs>    # 63/63
cargo xtask function-apply-differential     --oracle <pinned qjs>    # 15/15
cargo xtask iterator-differential           --oracle <pinned qjs>    # 40/40
cargo xtask number-radix-differential       --oracle <pinned qjs>    # 991/991
```

The Error corpus (18/35) and the call-spread manifest's
`noncallable-target-message` oracle disagreement are pre-existing and
unchanged; both reproduce identically with these changes stashed. The Error
gap is already tracked in `PORTING.md` as dependent on the pending
`Object`/`Reflect` surface.
