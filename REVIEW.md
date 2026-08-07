# Codebase review

Reviewed: 2026-08-01
Resolved: 2026-08-01
Updated: 2026-08-03

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

## Follow-up architecture review — 2026-08-03

### P1 — Realm intrinsic bootstrap is excessively hand-expanded — RESOLVED

**Location:** `crates/quickjs-runtime/src/runtime/realm.rs`

**Resolution:** Realm construction now uses validated typed specifications, a
derived atom and reservation plan, typed identity allocation, ordered generic
publication, and an allocation-free journaled transaction. The complete
installed graph is pinned by a normalized snapshot that was replayed against
the pre-refactor implementation; both produce the same 242-identity,
757-property fingerprint.

The runtime's Realm semantics are not themselves over-abstracted. The problem
is that one file currently combines the intrinsic schema, resource planning,
record reservation, atom interning, identity allocation, property publication,
committed Realm state, and rollback implementation.

The current file is approximately 4,500 lines and contains 31 `insert_*`
functions, 21 `publish_*` functions, and 46 references to `_ATOM_START` offset
constants. Adding an ordinary intrinsic can require corresponding edits to its
method table, dynamic-atom arithmetic, `RealmRecords`, `RealmGraph`, insertion,
publication, and reverse rollback. These are manually synchronized views of
the same intrinsic declaration.

The expansion originally protected important invariants:

- Realm creation preflights and reserves every recoverable allocation.
- A failed creation transaction leaves no Realm, heap node, property charge,
  binding, or dynamic atom behind.
- Intrinsic identities are strongly typed and Realm-local.
- Prototype relationships, own-key order, descriptors, names, and lengths are
  explicit and auditable.
- Runtime lookup uses direct IDs rather than strings or dynamic maps.

Those invariants must remain. The defect is the amount of duplicated
bookkeeping required to preserve them. Manual atom offsets and reverse-removal
lists are particularly fragile: forgetting one mirrored site can corrupt a
later family's atom lookup, leak a transaction-owned identity, or make resource
accounting disagree with the committed graph.

The desired design is an ordered, specification-derived intrinsic schema that
produces one exact reservation plan, one typed set of allocated identities, and
one property-publication sequence inside a journaled transaction. The committed
runtime representation must remain strongly typed and direct; this is not a
proposal to replace intrinsic IDs with a `HashMap<String, _>` or a plugin-style
dynamic registry.

#### Scope and non-goals

- [x] Refactor Realm construction without changing JavaScript behavior.
- [x] Preserve ECMA-262 semantics as the authority and retain explicitly
  documented pinned-QuickJS compatibility behavior.
- [x] Preserve exact intrinsic own-key order, descriptors, prototype chains,
  function identity, `name`, `length`, callability, and constructability.
- [x] Preserve failure atomicity and exact logical resource accounting.
- [x] Preserve direct typed runtime access to committed intrinsics.
- [x] Do not implement new built-ins as part of this refactor.
- [x] Do not combine the refactor with the Rust host-function bridge.
- [x] Do not introduce a runtime string-keyed intrinsic registry.
- [x] Do not introduce procedural macros or generated source until the schema
  has stabilized and remains independently auditable.
- [x] Complete and commit the active feature milestone before beginning this
  cross-cutting refactor.

#### Phase 0 — Characterize the current contract

- [x] Add a reusable Realm snapshot helper for tests.
- [x] Snapshot every installed intrinsic object's own keys in observable order.
- [x] Snapshot every installed property's complete descriptor.
- [x] Snapshot every intrinsic object's prototype identity.
- [x] Pin native function `name`, `length`, callability, and constructability.
- [x] Cover `%Object%`, `%Function%`, primitive constructors and prototypes,
  `%Array%`, iterator prototypes, Error constructors and prototypes, `%Math%`,
  `%JSON%`, `%Reflect%`, `%Symbol%`, and the global object.
- [x] Verify that two Realms have distinct Realm-local intrinsic identities.
- [x] Verify the identities that are intentionally runtime-shared, especially
  predefined atoms and well-known Symbols.
- [x] Verify that modifying one Realm's intrinsic property cannot affect a
  second Realm.
- [x] Record the exact `RuntimeUsage` delta of successful Realm creation.
- [x] Exercise every relevant `RuntimeLimits` boundary at "exactly enough" and
  "one short".
- [x] After each rejected creation, compare atoms, Realms, objects, functions,
  global bindings, property slots, and other usage counters to the exact
  pre-call snapshot.
- [x] Verify that a failed Realm creation does not consume the next committed
  `%Math.random%` seed.
- [x] Keep oracle/specification citations next to characterization cases whose
  behavior is not obvious from descriptors alone.

**Commit boundary:** characterization tests only; no production change.

#### Phase 1 — Introduce a typed intrinsic schema

- [x] Add `runtime/realm/schema.rs` while keeping `realm.rs` as the facade.
- [x] Define stable internal `IntrinsicObjectId` identities.
- [x] Define stable internal `IntrinsicFunctionId` identities.
- [x] Define typed IDs for runtime-created Realm names.
- [x] Represent repeated families with their existing semantic enums, for
  example `MathMethod`, `ArrayCallback`, and `ErrorKind`, instead of allocating
  anonymous integer slots.
- [x] Keep schema IDs independent from generational arena indices.
- [x] Keep schema types crate-private.
- [x] Define an `IntrinsicKeySpec` that distinguishes predefined string atoms,
  interned string names, Realm-created names, well-known Symbol keys, and array
  indices.
- [x] Define `PrototypeSpec` using typed object/function references.
- [x] Define `IntrinsicObjectSpec`.
- [x] Define `IntrinsicFunctionSpec`, including implementation, home Realm,
  `name`, `length`, constructability, and prototype requirements.
- [x] Define `IntrinsicPropertySpec` for both data and accessor descriptors.
- [x] Define `IntrinsicValueSpec` for primitives, exact Number bits, intrinsic
  object/function references, strings, and well-known Symbols.
- [x] Express property attributes explicitly; do not infer writable,
  enumerable, or configurable flags from method naming.
- [x] Preserve declaration order in the schema because it participates in
  observable own-key order.

#### Phase 2 — Validate the schema before allocation

- [x] Reject duplicate object and function IDs.
- [x] Reject dangling holder, prototype, getter, setter, and value references.
- [x] Reject a `WellKnownSymbol` key whose predefined atom is not a Symbol.
- [x] Reject unintended duplicate keys on one holder.
- [x] Validate every mandatory intrinsic identity.
- [x] Validate constructor/prototype relationships.
- [x] Validate native function identity properties and constructability flags.
- [x] Validate family-specific cardinality before building fixed arrays.
- [x] Permit intentional graph cycles, such as constructor/prototype links,
  without requiring allocation-order workarounds.
- [x] Run validation without allocating a Runtime heap node.
- [x] Unit-test every validator rejection independently.

**Commit boundary:** schema and validator exist, but the old bootstrap remains
the only production path.

#### Phase 3 — Replace dynamic-atom offset arithmetic

- [x] Add `runtime/realm/atoms.rs`.
- [x] Derive one ordered `RealmAtomPlan` from the validated specs.
- [x] Intern only names without predefined identities.
- [x] Exclude well-known Symbols from the string-atom plan.
- [x] Preserve the current atom interning order unless a characterization test
  proves that the order is unobservable and changing it is explicitly approved.
- [x] Resolve each intrinsic key through a typed atom binding rather than an
  integer offset owned by another family.
- [x] Calculate the exact dynamic atom count from the plan.
- [x] Calculate the exact UTF-16 storage requirement from the same plan where
  the atom budget requires it.
- [x] Delete the cascading `*_ATOM_START` constants.
- [x] Delete separately maintained `*_INTERNED_COUNT` constants.
- [x] Do not add `HashMap<String, Atom>` to Realm creation or runtime lookup.
- [x] Re-run atom-limit and cross-Realm characterization tests.

**Commit boundary:** typed ordered atom planning replaces offset arithmetic;
all other construction layers remain behaviorally unchanged.

#### Phase 4 — Derive the reservation plan

- [x] Add `runtime/realm/reservation.rs`.
- [x] Define `RealmReservationPlan` as the single source of truth for required
  dynamic atoms, Realms, objects, functions, global bindings, object-property
  slots, and transaction journal entries.
- [x] Derive ordinary native function `name` and `length` property capacity
  automatically.
- [x] Include constructor `prototype` and prototype `constructor` edges exactly
  once.
- [x] Include accessor identities and both accessor halves where present.
- [x] Include special properties, such as Array exotic `length`, through an
  explicit schema entry or narrowly scoped family hook.
- [x] Use checked arithmetic for all plan totals and map overflow to the
  existing structured resource failure policy.
- [x] Preflight every runtime limit from this plan before heap mutation.
- [x] Create ordinary two-property native records through one generic helper.
- [x] Remove repeated `*_records()` functions that only loop over
  `reserved_record(2)`.
- [x] Preserve any currently tested `RuntimeError::AllocationFailed`
  `resource` and `additional` values.

#### Phase 5 — Journal Realm construction

- [x] Add `runtime/realm/transaction.rs`.
- [x] Introduce `RealmBuildTransaction<'_>` with exclusive access to the
  Runtime during construction.
- [x] Pre-reserve the complete undo journal before the first heap mutation.
- [x] Record every successfully interned dynamic atom.
- [x] Record every inserted Realm, object, function, and global binding.
- [x] Ensure recording an undo entry after a successful mutation cannot
  allocate or fail.
- [x] Roll back uncommitted entries in strict reverse order.
- [x] Make rollback allocation-free and incapable of running JavaScript.
- [x] Keep rollback independent of GC, weak callbacks, and future finalizers.
- [x] Make `commit` consume or seal the transaction so `Drop` cannot undo a
  successful Realm.
- [x] Advance the next Realm-visible random seed only after commit.
- [x] Replace `RealmGraph::rollback` and the family-specific reverse-removal
  lists with the journal.
- [x] Add failure tests after atom, Realm, object, function, property, binding,
  and final-state preparation stages.

**Commit boundary:** journaled rollback replaces the manual graph mirror while
the existing family insertion/publication code still supplies the operations.

#### Phase 6 — Separate identity allocation from graph wiring

- [x] Add `runtime/realm/allocation.rs`.
- [x] Allocate transaction-private shell identities before publishing cyclic
  prototype and constructor edges.
- [x] Ensure no partially initialized identity can escape through a public
  `Realm` or `Context` handle.
- [x] Store identities in an `AllocatedIntrinsics` table indexed by typed IDs.
- [x] Permit each identity slot to be initialized exactly once.
- [x] Report a runtime invariant if an uninitialized slot is read.
- [x] Validate every mandatory slot before publication.
- [x] Preserve the special callable identity of `%Function.prototype%`.
- [x] Preserve `%ThrowTypeError%` identity and restricted descriptors.
- [x] Preserve Realm-local ownership of every native intrinsic function.
- [x] Convert the fully allocated table into the existing strongly typed
  committed `RealmIntrinsics`/`RealmState` representation.
- [x] Keep runtime intrinsic access as direct fields or typed array indexing.

#### Phase 7 — Publish properties from the schema

- [x] Add `runtime/realm/publication.rs`.
- [x] Resolve typed holders, keys, and values from the allocated table.
- [x] Publish data descriptors generically.
- [x] Publish accessor descriptors generically.
- [x] Publish ordinary native function `name` and `length` from function specs.
- [x] Publish constructor/prototype links from explicit property specs.
- [x] Preserve numeric, string, and Symbol own-key ordering.
- [x] Charge every property from the reservation plan exactly once.
- [x] End all arena borrows before any path that could later become a callback
  or proxy boundary.
- [x] Keep special hooks narrowly limited to semantics that cannot be expressed
  as ordinary property specifications.
- [x] Document every retained special hook and its reason.

#### Phase 8 — Migrate intrinsic families incrementally

Migrate one independently testable family per commit where practical. Once a
family uses the schema path, remove its old records, insertion, publication,
and rollback branches in the same commit; do not retain two production paths.

- [x] Migrate global numeric functions.
- [x] Migrate URI functions.
- [x] Migrate `%Math%`.
- [x] Migrate `%Reflect%`.
- [x] Migrate `%JSON%`.
- [x] Migrate Boolean intrinsics.
- [x] Migrate Number intrinsics and exact-bit constants.
- [x] Migrate BigInt intrinsics.
- [x] Migrate String intrinsics.
- [x] Migrate Symbol intrinsics and well-known Symbol properties.
- [x] Migrate Error and native Error-family intrinsics.
- [x] Migrate `%AggregateError%`-specific identities and properties.
- [x] Migrate iterator intrinsics.
- [x] Migrate Array search methods.
- [x] Migrate Array mutators.
- [x] Migrate Array copiers.
- [x] Migrate Array sorting and flattening methods.
- [x] Migrate Array callback and reduction methods.
- [x] Migrate Array statics, `splice`, `isArray`, and `@@species`.
- [x] Migrate the Array constructor/prototype and exotic `length` setup.
- [x] Migrate the Object/Function bootstrap kernel last, after all dependent
  families use typed references.
- [x] Remove each migrated family's `insert_*`, `publish_*`, record mirror, and
  manual rollback code immediately.

#### Phase 9 — Split the module by responsibility

- [x] Keep `runtime/realm.rs` as the lifecycle and public-runtime facade.
- [x] Move schema declarations to `runtime/realm/schema.rs`.
- [x] Move schema checks to `runtime/realm/validation.rs`.
- [x] Move atom planning to `runtime/realm/atoms.rs`.
- [x] Move resource planning to `runtime/realm/reservation.rs`.
- [x] Move rollback/commit logic to `runtime/realm/transaction.rs`.
- [x] Move identity materialization to `runtime/realm/allocation.rs`.
- [x] Move descriptor publication to `runtime/realm/publication.rs`.
- [x] Keep family specs under `runtime/realm/families/`.
- [x] Keep standard built-in algorithms outside the Realm bootstrap modules.
- [x] Remove obsolete large-function lint allowances when their functions are
  split or deleted.
- [x] Verify no circular module dependency gives schema code access to mutable
  Runtime internals.

Suggested family layout:

```text
runtime/realm/
  schema.rs
  validation.rs
  atoms.rs
  reservation.rs
  transaction.rs
  allocation.rs
  publication.rs
  families/
    mod.rs
    kernel.rs
    object.rs
    function.rs
    primitives.rs
    string.rs
    array.rs
    iterator.rs
    error.rs
    math.rs
    json.rs
    reflect.rs
```

#### Phase 10 — Document and enforce the new maintenance contract

- [x] Document Realm construction phases in `ARCHITECTURE.md`.
- [x] Document that spec declaration order contributes to own-key order.
- [x] Document that allocated shells are transaction-private until commit.
- [x] Document that rollback never executes JavaScript.
- [x] Document which intrinsic relationships require special hooks.
- [x] Keep intrinsic bootstrap separate from dynamically installed host
  functions and future module namespaces.
- [x] Update `PORTING.md` only to record the architecture refactor; do not mark
  any additional ECMAScript feature complete.
- [x] Add a maintenance regression proving that a simple ordinary intrinsic
  method can be added without changing atom counts, atom offsets, record arrays,
  insertion helpers, publication helpers, or rollback lists.

#### Follow-up — Rust function to JavaScript bridge

This is enabled by the cleanup but is a separate feature milestone.

**Status:** Deferred by design. This bridge is outside the Realm architecture
finding and no host-callback or asynchronous-host-function surface was added.

- [ ] Keep host callbacks out of the immutable standard-intrinsic schema.
- [ ] Add a Runtime-owned typed `HostFunctionId` registry.
- [ ] Add a host-function `FunctionImplementation` variant rather than turning
  `NativeFunctionKind` into an untyped callback container.
- [ ] Provide `Context::create_host_function` through a normal runtime
  allocation transaction.
- [ ] Provide public global/object property definition APIs.
- [ ] Define exact Rust result/error to JavaScript completion conversion.
- [ ] Start with a callback form that cannot capture untraced JavaScript roots.
- [ ] Add stateful closures only after host-owned roots participate in tracing
  and deterministic finalization.
- [ ] Add asynchronous host functions only after deterministic Promise-job
  ordering exists.

#### Suggested commit sequence

1. `Test complete Realm bootstrap invariants`
2. `Introduce typed Realm intrinsic specifications`
3. `Validate the complete intrinsic specification graph`
4. `Derive Realm-local atoms from intrinsic specifications`
5. `Derive Realm reservation records from intrinsic specs`
6. `Make Realm construction a journaled transaction`
7. `Separate Realm identity allocation from graph publication`
8. `Publish ordinary intrinsic properties from typed specs`
9. `Migrate global numeric and URI intrinsics to typed specs`
10. `Migrate Math, Reflect, and JSON Realm intrinsics`
11. `Migrate primitive and String Realm intrinsics`
12. `Migrate Symbol and Error Realm intrinsics`
13. `Migrate iterator and Array Realm intrinsics`
14. `Migrate the Object and Function Realm bootstrap kernel`
15. `Remove manual Realm bootstrap mirrors`
16. `Split Realm bootstrap by responsibility and document invariants`

Each migration commit must run its focused Realm/runtime tests before the full
workspace gates. A commit must not leave one family partially split between the
old and new paths.

#### Completion gates

- [x] All characterization snapshots remain byte-for-byte unchanged.
- [x] Pinned QuickJS differential results remain unchanged.
- [x] Every Realm resource limit remains exact at its admission boundary.
- [x] Every injected or limit-induced construction failure restores the exact
  pre-call logical usage.
- [x] Cross-Realm isolation and intended runtime-shared identities are
  unchanged.
- [x] No standard intrinsic is resolved through a runtime string map.
- [x] No `unsafe` is introduced.
- [x] No procedural macro or generated-source dependency is introduced.
- [x] A simple ordinary built-in requires one implementation dispatch entry,
  one schema entry, and behavior tests, without manually synchronized atom,
  record, insert, publish, and rollback edits.
- [x] The active Realm facade no longer combines schema, reservation,
  allocation, publication, and rollback implementations.
- [x] Full verification passes:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
cargo audit
```

## Compiler and verifier architecture review — 2026-08-03

### P1 — Compiler lowering is a 14,000-line multi-stage god module — RESOLVED

**Location:** `crates/quickjs-compiler/src/lowering.rs`

**Resolution:** The lowering facade is now 291 lines and delegates to owned
artifact, error, context, layout, pool, control-flow, validation, per-function,
planner, and graph-finalization modules. The public API and explicit iterative
worklists remain intact, and the pinned complete-artifact snapshot plus all
relevant QuickJS differential corpora are unchanged.

The file is currently approximately 14,047 lines, of which about 13,025 are
production code. It contains roughly 383 type, implementation, and function
declarations. The size is therefore not primarily caused by colocated tests.

More importantly, the single `CompilationContext` implementation spans roughly
9,000 lines and currently participates in all of these responsibilities:

- public compilation entry points and compiler artifact types;
- selected-executable and function-tree validation;
- literal, atom, metadata-atom, and constant candidate collection;
- statement scheduling and completion propagation;
- expression lowering;
- declaration and destructuring lowering;
- scope entry/exit, TDZ, arguments, and parameter instantiation;
- local, closure, and constructor-Realm-global reference lowering;
- capture, frame, function-tree, and Realm-global layout construction;
- planned control-flow labels, stack anchors, source spans, and assembly;
- compiler-function graph conversion and final verifier handoff;
- compiler error taxonomy and rendering.

The explicit iterative worklists are correct architectural choices and must be
preserved. The defect is that their state, AST-domain planners, layout builders,
constant-pool freezing, assembly, and public orchestration all live behind one
very broad implementation boundary. This makes new language work likely to
modify unrelated phases and makes it difficult to state which invariants have
already been established at any function boundary.

The existing code already exposes useful seams: `StatementWork`,
`StatementPlanningState`, the destructuring planners, `FunctionTreeLayout`,
`FrameLayout`, `RealmGlobalLayout`, `CompiledConstantPool`, and
`PlannedControlFlow`. The refactor should make those actual module and type
boundaries instead of replacing them with recursive visitors or a collection
of arbitrary helper files.

#### Lowering scope and non-goals

- [x] Preserve accepted and rejected syntax exactly during the structural
  refactor.
- [x] Preserve exact emitted bytecode, atoms, constants, metadata, source spans,
  function-tree order, and verified authority.
- [x] Preserve explicit iterative AST/worklist traversal.
- [x] Preserve failure atomicity and every compiler/verifier resource limit.
- [x] Preserve Oxc arena isolation; no Oxc node, scope, symbol, or reference ID
  may escape into compiled output.
- [x] Preserve the complete-graph verification boundary.
- [x] Do not add a new opcode or language feature in a mechanical split commit.
- [x] Do not introduce visitor recursion to make the module split easier.
- [x] Do not introduce traits whose only purpose is allowing one giant
  `CompilationContext` implementation to be scattered across files.
- [x] Do not create a generic `utils.rs` containing cross-stage mutable state.
- [x] Keep the public `quickjs-compiler` API stable through module re-exports.

#### Lowering phase 0 — Characterize compiler output

- [x] Add a helper that snapshots final encoded bytecode for a complete
  compiled function tree.
- [x] Snapshot disassembly, function-template preorder, atoms, constants,
  closure sources, Realm globals, frame domains, and function metadata.
- [x] Snapshot exact `PcSourceSpan` entries and source-name behavior.
- [x] Snapshot the intermediate compiler-function graph certificate.
- [x] Cover representative statements, expressions, bindings, destructuring,
  iteration, abrupt completion, nested closures, parameter environments, and
  dynamic-Function Script roots.
- [x] Cover compact and wide opcode forms around their encoding boundaries.
- [x] Pin exact `LeafCompilationError` variants and spans for representative
  failures from every phase.
- [x] Pin instruction, label, worklist, constant, atom, function-tree, and graph
  resource-boundary failures.
- [x] Run the existing differential corpora and record their baseline counts.

**Commit boundary:** tests and snapshot helpers only; no compiler production
change.

#### Lowering phase 1 — Create a module facade

- [x] Keep `lowering.rs` as the temporary public facade so existing imports and
  re-exports remain stable.
- [x] Add a `lowering/` directory for responsibility-owned child modules.
- [x] Move the in-file test module to `lowering/tests.rs` without changing
  fixtures or expectations.
- [x] Move public immutable result types to `lowering/artifacts.rs`.
- [x] Move `LeafCompilationError` and its formatting to `lowering/error.rs`.
- [x] Keep `CompilationContext` construction and public compile entry points in
  `lowering/context.rs`.
- [x] Re-export the existing public types from the same crate-level paths.
- [x] Make the first split mechanically reviewable: moved bodies must remain
  byte-for-byte equivalent except for paths and visibility.

#### Lowering phase 2 — Separate layout construction

- [x] Move `RealmGlobalLayout` and its builder to
  `lowering/layouts/realm_globals.rs`.
- [x] Move `FunctionTreeLayout` and child-edge/preorder logic to
  `lowering/layouts/function_tree.rs`.
- [x] Move `FrameLayout`, local/argument/capture slot types, and frame-domain
  checks to `lowering/layouts/frame.rs`.
- [x] Give each layout constructor one explicit immutable input object.
- [x] Give each layout constructor one typed, fully validated output.
- [x] Prevent planners from mutating layouts after construction.
- [x] Keep dense index and width checks with the layout that owns the domain.
- [x] Unit-test each layout without requiring AST emission.

#### Lowering phase 3 — Separate constants, atoms, and metadata freezing

- [x] Move literal/constant candidate types and deterministic freeze order to
  `lowering/constants.rs`.
- [x] Move property and metadata atom candidates to `lowering/atoms.rs`.
- [x] Keep value constants and nested-function constants type-distinct.
- [x] Preserve deterministic first-use/order keys exactly.
- [x] Preserve exact Number bits and UTF-16 string contents.
- [x] Keep property-key canonicalization in one owner module.
- [x] Make the frozen constant pool immutable before instruction planning.
- [x] Expose a narrow query interface for planners instead of the mutable
  candidate maps.
- [x] Retain resource and index-width checks in the owning pool modules.

#### Lowering phase 4 — Isolate planned control flow and assembly

- [x] Move `CompilerLabel`, `StackAnchor`, `PlannedInstruction`,
  `PlannedControlFlow`, and `FinishedControlFlow` to
  `lowering/control_flow.rs`.
- [x] Keep label owner spans and expected stack depths private to the builder.
- [x] Preserve statement stack-anchor validation.
- [x] Preserve branch widening and relocation behavior.
- [x] Preserve final instruction-to-source mapping.
- [x] Keep assembly failure mapping in this module rather than in AST planners.
- [x] Make `finish` the only path that yields an assembled, structurally
  verified control-flow artifact.
- [x] Retain existing label, unreachable-anchor, join-depth, and source-mapping
  regressions with the builder module.

#### Lowering phase 5 — Introduce a per-function lowering session

- [x] Keep `CompilationContext` immutable after storage planning.
- [x] Introduce a `FunctionLowerer`/`FunctionLoweringSession` for one selected
  executable.
- [x] Give the session explicit borrowed references to compilation input,
  function-tree layout, frame layout, and frozen pools.
- [x] Let the session own mutable statement/expression worklists and planned
  control flow.
- [x] Move per-function counters and temporary stacks out of
  `CompilationContext` method parameters.
- [x] Replace repeated high-arity planner parameters with narrow typed context
  objects; do not hide unrelated state in one catch-all struct.
- [x] Ensure one session cannot emit instructions for another executable.
- [x] Ensure child-function constants resolve only through the selected
  function-tree layout.
- [x] Return one staged `CompiledFunction`, never partial mutable compiler
  state.
- [x] Keep `CompilationContext` responsible only for selecting executables,
  constructing immutable layouts/pools, running sessions, and final graph
  verification.

#### Lowering phase 6 — Split AST-domain planners

- [x] Move statement scheduling and statement completion to
  `lowering/plan/statements.rs`.
- [x] Move loop, switch, label, break, and continue control regions to
  `lowering/plan/control.rs`.
- [x] Move try/catch/finally and abrupt-marker cleanup to
  `lowering/plan/abrupt.rs`.
- [x] Move expression lowering to `lowering/plan/expressions.rs`.
- [x] Move calls, constructors, member references, and assignments to a
  focused expression-call/reference module if `expressions.rs` remains too
  broad.
- [x] Move binding-pattern and assignment-pattern lowering to
  `lowering/plan/destructuring.rs`.
- [x] Move declaration/reference reads and writes to
  `lowering/plan/bindings.rs`.
- [x] Move parameter/body environment initialization to
  `lowering/plan/parameters.rs`.
- [x] Keep `StatementWork` and `ExpressionWork` iterative.
- [x] Ensure each work enum is owned by the module that advances it.
- [x] Keep iterator acquisition, target evaluation, step, close, and abrupt
  precedence visible in one auditable destructuring/iteration path.
- [x] Keep inferred-name emission adjacent to the initialization forms whose
  NamedEvaluation it implements.

#### Lowering phase 7 — Separate validation from emission

- [x] Move executable/function-form validation to
  `lowering/validation/functions.rs`.
- [x] Move admitted statement and expression feature validation to focused
  validation modules.
- [x] Make validation return typed facts consumed by lowering where this avoids
  repeating Oxc semantic queries.
- [x] Do not create a second independently drifting AST traversal solely for
  aesthetic separation.
- [x] Keep fail-closed unsupported-feature errors tied to exact source spans.
- [x] Keep storage-plan consistency checks at the storage/lowering boundary.
- [x] Ensure no production planner silently assumes a validation fact that is
  absent from its input type.

#### Lowering phase 8 — Separate graph finalization

- [x] Move compiled function-tree record construction to
  `lowering/function_graph.rs`.
- [x] Keep executable-to-template mapping deterministic and dense.
- [x] Validate one parent per non-root child.
- [x] Validate closure-source and Realm-global slot order.
- [x] Keep compiler graph verification before final bytecode authority
  construction.
- [x] Preserve exact source-span mapping for graph verification failures.
- [x] Make graph finalization consume complete staged functions, not mutable
  lowering sessions.

#### Lowering target layout

```text
lowering/
  mod.rs
  artifacts.rs
  context.rs
  function.rs
  validation/
    mod.rs
    functions.rs
    syntax.rs
  layouts/
    mod.rs
    frame.rs
    function_tree.rs
    realm_globals.rs
  atoms.rs
  constants.rs
  control_flow.rs
  function_graph.rs
  plan/
    mod.rs
    statements.rs
    expressions.rs
    calls.rs
    bindings.rs
    destructuring.rs
    parameters.rs
    control.rs
    abrupt.rs
  error.rs
  tests.rs
```

#### Suggested lowering commits

1. `Pin complete compiler lowering artifacts and failures`
2. `Move lowering tests and public artifacts behind a module facade`
3. `Extract lowering error definitions without behavior changes`
4. `Extract Realm-global, function-tree, and frame layouts`
5. `Extract deterministic constant and atom pool construction`
6. `Extract planned control-flow assembly`
7. `Introduce per-function lowering sessions`
8. `Extract statement and abrupt-control planning`
9. `Extract expression and call planning`
10. `Extract destructuring, binding, and parameter planning`
11. `Extract compiler graph finalization`
12. `Remove the monolithic CompilationContext lowering implementation`

#### Lowering completion gates

- [x] Every characterization artifact remains byte-for-byte unchanged.
- [x] Every characterized compiler error variant and span remains unchanged.
- [x] Differential corpus counts and output remain unchanged.
- [x] Explicit iterative worklists remain in production.
- [x] No AST recursion proportional to source nesting is introduced.
- [x] No Oxc-owned identity appears in a public compiled artifact.
- [x] `CompilationContext` no longer owns per-function mutable lowering state.
- [x] Layouts and frozen pools are immutable before instruction planning.
- [x] No function requires a collection of unrelated layout, pool, worklist,
  and flow arguments merely because it was moved to another file.
- [x] The module facade contains orchestration and re-exports rather than
  implementations for every language family.
- [x] All workspace gates pass.

### P1 — Control-flow verifier conflates model, diagnostics, structural validation, capability policy, and dataflow — RESOLVED

**Location:** `crates/quickjs-bytecode/src/verifier.rs`

**Resolution:** The verifier is split into private typed proof stages for the
model, limits, diagnostics, predecode, headers, layouts, operands, targets,
opcode policy, static control flow, and bounded stack dataflow. The 205-line
pipeline is the only constructor of public verified control flow, with exact
certificate, failure-precedence, allocation-failure, and exhaustive opcode
partition regressions retained.

The verifier file is approximately 3,452 lines, almost all production code,
with roughly 117 declarations. Although smaller than `lowering.rs`, it combines
the public unverified/verified control-flow model, limits, the complete error
taxonomy and formatting, decoding, boundary maps, header and compiler-layout
validation, operand-domain validation, control-flow target resolution, the
verifier capability table, and ordinary stack-depth dataflow.

This is security-sensitive authority code. A split is justified only if it
makes the proof stages and their inputs more explicit. Mechanical extraction
must not reorder validation, alter the first reported error, or accidentally
promote an intermediate structural result into execution authority.

The current `verify_control_flow_common` function already describes the desired
pipeline: limit/count checks, complete predecode, header/layout checks, static
semantics and successor resolution, then stack analysis. It should remain the
single orchestrator while its stages move behind typed internal boundaries.

#### Verifier scope and non-goals

- [x] Preserve the public verifier API and all crate-level re-exports.
- [x] Preserve exact verification order and first-error precedence.
- [x] Preserve exact `VerificationErrorKind`, bytecode PC, opcode, edge, index,
  domain, limit, and source values.
- [x] Preserve fail-closed handling of unsupported verifier capabilities.
- [x] Preserve complete decoding before authority is produced.
- [x] Preserve checked target-boundary and operand-domain validation.
- [x] Preserve bounded iterative stack dataflow.
- [x] Do not admit any additional opcode during the module split.
- [x] Do not merge verifier capability policy into base opcode metadata merely
  to reduce file size.
- [x] Do not expose a partially verified intermediate as `VerifiedControlFlow`
  or `VerifiedBytecode`.

#### Verifier phase 0 — Characterize proof behavior

- [x] Pin a successful `VerifiedControlFlow` snapshot: decoded PCs,
  instructions, successors, reachability, entry stack depths, computed maximum
  stack, transfer-evaluation count, domains, header, and validated layouts.
- [x] Pin invalid-byte decoding and truncated-instruction errors.
- [x] Pin instruction-boundary and target errors.
- [x] Pin primary and secondary operand-domain errors.
- [x] Pin header/count/layout mismatches.
- [x] Pin unsupported-capability errors for every
  `UnsupportedVerifierFeature`.
- [x] Pin stack underflow, inconsistent join, stack limit, non-empty compiler
  exit, and transfer-evaluation limit errors.
- [x] Pin validation-order cases in which one body violates more than one
  condition.
- [x] Pin allocation/resource errors for bitmap, instruction vector, and
  worklist growth.
- [x] Retain the exhaustive opcode capability-partition test.

**Commit boundary:** verifier tests only; no production change.

#### Verifier phase 1 — Extract public model, limits, and errors

- [x] Move unverified body and compiler-layout input types to
  `verifier/model.rs`.
- [x] Move `VerifiedInstruction`, `VerifiedSuccessors`, instruction indices,
  and `VerifiedControlFlow` to the same model layer or a focused
  `verified.rs` child.
- [x] Move `VerificationLimits` and resource-domain enums to
  `verifier/limits.rs`.
- [x] Move `VerificationError`, `VerificationErrorKind`, and all formatting to
  `verifier/error.rs`.
- [x] Preserve current public names and crate-root re-exports.
- [x] Keep constructors that could forge verified state private.
- [x] Keep `VerifiedControlFlow` fields private after extraction.

#### Verifier phase 2 — Extract complete predecode

- [x] Move instruction decoding and instruction-start bitmap construction to
  `verifier/predecode.rs`.
- [x] Introduce an internal `PredecodedBody` containing bytecode-relative
  decoded instructions and the boundary map.
- [x] Make `PredecodedBody` construction all-or-nothing.
- [x] Keep maximum-instruction enforcement during decoding.
- [x] Keep decode errors mapped to the same PC/opcode fields.
- [x] Keep fallible vector and bitmap allocation accounting exact.
- [x] Do not allow `PredecodedBody` to authorize execution.

#### Verifier phase 3 — Extract header and compiler-layout validation

- [x] Move function header, count, and domain validation to
  `verifier/header.rs`.
- [x] Move compiler capture-layout validation to `verifier/layouts.rs`.
- [x] Move compiler constant-layout validation to `verifier/layouts.rs`.
- [x] Keep scoped-local and mapped-arguments validation in the capture-layout
  owner.
- [x] Keep missing-layout checks dependent on compiler verification mode.
- [x] Return typed validated layouts whose constructors remain private.
- [x] Preserve exact structural count and limit errors.

#### Verifier phase 4 — Extract operands and targets

- [x] Move primary operand-domain validation to `verifier/operands.rs`.
- [x] Move compact implied local/argument/closure index handling with the
  operand validator.
- [x] Move secondary operand flag/range validation with the operand validator.
- [x] Move relative-target specification and checked resolution to
  `verifier/targets.rs`.
- [x] Move instruction-index and boundary-map queries with target resolution.
- [x] Keep gosub continuation validation in the target/control-flow owner.
- [x] Preserve exact `ControlFlowEdge` and invalid-target reason reporting.
- [x] Avoid rescanning decoded instructions through independent inconsistent
  lookup implementations.

#### Verifier phase 5 — Isolate verifier capability policy

- [x] Move `OpcodeSemantics`, `SuccessorShape`, and the exhaustive
  `opcode_semantics` table to `verifier/opcode_semantics.rs`.
- [x] Keep every `FinalOpcode` in exactly one capability partition.
- [x] Keep unsupported features explicit rather than using a default wildcard.
- [x] Document that this table expresses what this verification layer can
  currently prove, not immutable opcode encoding metadata.
- [x] Keep compiler-only conditional admissions for typed constant layouts,
  capture layouts, iterator markers, and packed stack offsets explicit in the
  static-semantics stage.
- [x] Retain compile/test exhaustiveness whenever `FinalOpcode` grows.

#### Verifier phase 6 — Extract static control-flow verification

- [x] Move the per-instruction static-semantics pass to
  `verifier/static_control_flow.rs`.
- [x] Validate operands before deriving successors, preserving current order.
- [x] Validate targets against the complete boundary map.
- [x] Validate opcode capability and function-kind admission.
- [x] Produce an internal structural result with validated successors but no
  stack certificate.
- [x] Keep intermediate constructors private.
- [x] Ensure unreachable instructions receive the same structural validation
  as reachable instructions.

#### Verifier phase 7 — Extract stack dataflow

- [x] Move `analyze_ordinary_stack`, propagation, and worklist reservation to
  `verifier/stack.rs`.
- [x] Accept only the private structurally validated control-flow result.
- [x] Preserve breadth-first/queue worklist behavior where it affects first
  error precedence.
- [x] Preserve exact transfer-evaluation counting.
- [x] Preserve stack-effect error mapping to the current instruction.
- [x] Preserve special `gosub`, catch, iterator-marker, and compiler-exit
  treatment.
- [x] Preserve reachability as `Option<u32>` entry depth.
- [x] Return a private stack certificate that is combined with structural data
  only by the pipeline orchestrator.

#### Verifier phase 8 — Reduce the facade to orchestration

- [x] Move `verify_control_flow` and `verify_compiler_control_flow` to
  `verifier/pipeline.rs` or retain them in `verifier/mod.rs` as thin entries.
- [x] Keep one `verify_control_flow_common` pipeline owner.
- [x] Make stage ordering explicit in code and module documentation.
- [x] Construct `VerifiedControlFlow` only after every stage succeeds.
- [x] Keep `VerifiedBytecode` authorization in its existing later
  whole-bytecode verifier; structural control flow alone remains
  non-executable.
- [x] Remove obsolete broad lint allowances after extraction.

#### Verifier target layout

```text
verifier/
  mod.rs
  model.rs
  limits.rs
  error.rs
  pipeline.rs
  predecode.rs
  header.rs
  layouts.rs
  operands.rs
  targets.rs
  opcode_semantics.rs
  static_control_flow.rs
  stack.rs
  tests.rs
```

#### Suggested verifier commits

1. `Pin complete control-flow verifier certificates and failures`
2. `Extract verifier model and limits without API changes`
3. `Extract verifier diagnostics without behavior changes`
4. `Extract complete bytecode predecode and boundary maps`
5. `Extract function header and compiler-layout validation`
6. `Extract operand-domain and control-flow target validation`
7. `Extract exhaustive verifier opcode capability policy`
8. `Extract static successor verification`
9. `Extract bounded stack dataflow analysis`
10. `Reduce the verifier facade to the typed proof pipeline`

#### Verifier completion gates

- [x] Successful certificates remain exactly equivalent.
- [x] Every characterized failure reports the same first error and location.
- [x] Every opcode remains in exactly one capability partition.
- [x] No unsupported opcode becomes admitted as a side effect of extraction.
- [x] Unreachable instructions remain structurally verified.
- [x] Transfer-evaluation and resource limits remain exact.
- [x] Intermediate proof-stage types cannot construct public verified
  authority outside the verifier pipeline.
- [x] `VerifiedControlFlow` remains non-executable without whole-bytecode
  verification.
- [x] All `quickjs-bytecode` tests and full workspace gates pass.

### Refactor sequencing

- [x] Finish and commit the active feature milestone before any structural
  compiler/verifier refactor.
- [x] Do not refactor Realm, lowering, and verifier in one commit or one
  partially working branch state.
- [x] Start with verifier characterization and mechanical extraction because
  its proof stages are already explicit and comparatively bounded.
- [x] Split lowering artifacts, layouts, frozen pools, and control-flow builder
  before changing `CompilationContext` ownership.
- [x] Introduce `FunctionLoweringSession` only after those immutable inputs have
  stable module boundaries.
- [x] Run focused crate tests after every extraction and all workspace gates
  before every commit.
- [x] Run the relevant pinned QuickJS differential corpora after any lowering
  boundary moves, even when the intended change is mechanical.
