# Porting roadmap

## Contract

Port the observable JavaScript and host behavior of
[QuickJS 2026-06-04](UPSTREAM.md) to safe, pure Rust. The target covers the
release's ES2025 Script and Module profile, including Annex B, plus only those
later ECMA-262 features that are explicitly admitted and tested here.

Authority is ordered as follows:

1. ECMA-262 is normative for JavaScript semantics; ECMA-404 and other named
   standards govern their corresponding surfaces.
2. The pinned QuickJS release defines the compatibility surface, documented
   implementation choices, diagnostics where compatibility requires them, and
   the `std`/`os` host target. When it conflicts with the specification, follow
   the specification and record a stable, tested difference below.
3. Oxc supplies parsing and semantic analysis only. The upstream C engine and
   other engines may be differential oracles, but none supplies runtime
   semantics or is linked or shipped.

This is a source-level port, not a reproduction of QuickJS's binary layout or C
API. Rust callers receive a lifetime-safe API; an optional isolated N-API crate
may provide a foreign ABI. Unsupported behavior must reject before execution or
otherwise fail closed. The core has no C/C++ build or runtime dependency.

## Architecture

Production crates remain independently reusable:

| Crate | Responsibility |
| --- | --- |
| `quickjs-diagnostics` | Sources, stable diagnostics, spans, and source maps |
| `quickjs-frontend` | Published Oxc parsing/semantics and owned frontend records |
| `quickjs-bytecode` | Instructions, codec, verifier, disassembly, constants, atoms, and debug data |
| `quickjs-compiler` | Iterative Oxc lowering to verified bytecode |
| `quickjs-regexp` | Safe, bounded ES RegExp grammar lowering and UTF-16 execution |
| `quickjs-runtime` | Values, heap, realms, VM, built-ins, limits, interrupts, and embedding primitives |
| `quickjs` | Ergonomic facade used by thin command-line tools |

`xtask`, fuzzers, benchmarks, and pinned oracles are repository tooling, never
production dependencies. Optional lower-priority layers include a Tokio host
driver, inspector, Wasm, N-API, TypeScript erasure, and Serde conversion. See
[ARCHITECTURE.md](ARCHITECTURE.md) for ownership and trust boundaries and
[BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md) for the executable-bytecode
contract.

## Current status

A checked item means the named scope is implemented and regression-tested; it
does not imply complete ECMAScript or QuickJS compatibility.

### Frontend and diagnostics

- [x] The safe, reproducible workspace pins published Oxc `0.142.0` directly.
  Lossless Script, Module, strict/async-global, and dynamic-Function parse goals
  produce owned span, binding, and module records; unsupported adapters reject.
- [x] A closed compatibility ledger covers admitted grammar and reachable
  diagnostics. Every intentional Oxc difference has an ID and fixture.
- [ ] Complete source-map chaining and the remaining public diagnostic/API
  audit.

### Compiler, bytecode, and execution

- [x] Typed opcode metadata, checked codec/disassembly, bounded construction,
  total decoding, and whole-child-graph verification are complete. Only
  `VerifiedBytecode` may execute; admission validates structure and resources
  before runtime mutation.
- [x] Iterative lowering covers the admitted ordinary profile, closures,
  control flow, calls/spread, destructuring, exceptions, sync generators,
  async functions, async generators, and delegated `yield*`. Typed suspension
  and iterator records preserve resume, close, and `finally` order.
- [x] Explicit VM frame/continuation stacks handle bytecode and native re-entry,
  coercion, construction, abrupt completion, and traces. Deterministic fuel is
  separate from the pinned 10,000-instruction uncatchable host interrupt.
- [ ] Complete remaining opcode families, debug/source tables, and
  direct/indirect `eval`. Raw or serialized unverified bytecode and `eval`
  remain fail closed.

### Values, objects, and functions

- [x] Exact UTF-16 strings, binary64 Numbers, owned BigInts, Symbols, canonical
  keys, wrappers, conversions, operators, and signed-zero/NaN behavior.
- [x] Ordinary descriptors and integrity, prototype/extensibility semantics,
  key order, Array `length`/holes, String indices, functions/constructors,
  lexical capture/TDZ, dynamic Function families, `call`/`apply`/`bind`,
  `instanceof`, parameters, `arguments`, and admitted `NamedEvaluation`.
- [x] Typed same-site iterator state covers Array/String iteration, spread,
  destructuring, `for-of`, `IteratorClose`, and original-abrupt precedence.
- [x] Validated schemas derive Realm allocation and rollback. The normalized
  snapshot pins 387 Realm-local identities and 1,180 ordered properties.
- [x] Proxy and exotic-object integration covers all 11 internal methods,
  revocation/invariants, reflection, descriptors/integrity, constructors,
  iterators, Arrays, strings, RegExp, Promise, and JSON. Audited built-in
  `Get`/`HasProperty` and abstract `IsArray` sites are Proxy-aware; remaining
  direct object/Array checks are guarded ordinary or physical-storage paths.
- [x] Runtime-local weak shape and transition interning shares value-independent
  key/layout metadata while slots, key order, prototypes, extensibility, and GC
  edges remain per object.
- [x] Dense Array storage keeps default indexed values and holes outside shapes,
  transitions atomically to sparse properties for exceptional descriptors or
  far writes, and preserves exact length, key order, accounting, and GC edges.
- [x] The complete ES2025 Object/Reflect surface shares exotic internal methods,
  stable exception diagnostics, and resumable global accessor Get/Set paths.

### Built-ins

- [x] Globals; admitted `Object`; all 13 `Reflect` methods; `Error`, native
  subclasses, and `AggregateError`; `Boolean`, `Number`, `BigInt`, and `Symbol`.
- [x] Broad `String`, Unicode/normalization behavior, resumable
  `match`/`search`/`replace`/`replaceAll`/`split` protocols and fallbacks, Annex
  B HTML wrappers, and pinned ICU4X data. Shared corpora: `replaceAll` 6/6 cases
  and 14/14 tags; `split` 6/6 cases and 17/17 tags.
- [x] A safe, bounded RegExp core parses ES2025 grammar through pinned Oxc,
  lowers to an owned explicit-backtracking VM, preserves UTF-16 positions,
  covers lookaround/backreferences/Unicode sets and most properties of strings,
  losslessly compiles constructor UTF-16 sources (including legacy surrogate
  element semantics), and validates/executes literals through verified
  bytecode. The Realm installs the constructor, accessors, `escape`, `compile`,
  `exec`, `test`, and `toString`; execution shares VM fuel, bounds backtracking,
  updates `lastIndex`, and materializes captures, named groups, and `d` indices.
  Generic resumable `@@match`, `@@search`, `@@matchAll`, and `@@replace`
  preserve custom `exec`, strict `lastIndex`, empty-match UTF-16 advancement,
  and observable result access/coercion order. Match-all adds species-based lazy
  iteration and exact iterator GC roots; replace collects raw results before
  processing captures, callbacks, and all positional/named substitutions.
  `String.prototype.matchAll` enforces the global guard before dispatch. Shared
  core corpus: 63/63 QuickJS and Node cases. `@@split` and RGI ZWJ string
  properties still fail closed.
- [x] Full admitted `Array`, including generic array-like behavior and
  spec-ordered, resource-traced `fromAsync` iterator/array-like suspension.
- [x] `Map`: ordered SameValueZero storage, `AddEntriesFromIterable` close
  boundaries, live iterators, reentrant `forEach`, `groupBy`, `getOrInsert*`,
  and exact GC/resource accounting. Corpus: 6/6, 13/13 feature tags.
- [x] `Set`: ordered SameValueZero storage; exact constructor close boundaries;
  live iteration and `forEach`; ES-first set-like composition/predicates with
  branch-dependent order, mutation, and normal early close; intrinsic results;
  pinned `groupBy`; and exact fuel/GC/resource accounting. Corpus: 8/8, 21/21
  feature tags against QuickJS and Node.
- [x] `WeakMap`/`WeakSet`: objects and non-registered Symbols as weak keys;
  brand-first queries and exact constructor close boundaries; pinned
  `getOrInsert*`; resource-accounted storage; and failure-atomic fixed-point
  ephemeron GC for chains, cycles, and Symbol values. Shared corpus: 6/6,
  15/15 feature tags against QuickJS and Node; pinned upserts have focused VM
  coverage because Node v24.19 does not expose them.
- [x] `WeakRef`/`FinalizationRegistry`: exact pinned surfaces and brand/order
  checks; objects and non-registered Symbols as targets/tokens; resumable
  `newTarget.prototype`; kept-alive dereferences; ordered unregisterable cells;
  and failure-atomic GC-to-host-job cleanup. Shared deterministic corpus: 5/5,
  13/13 feature tags against QuickJS and Node; explicit collector regressions
  cover deferred cleanup, ordering, liveness, and resource ceilings.
- [x] Iterative `%JSON%` plus `rawJSON`; complete pinned `%Math%`, including
  exact `sumPrecise`; and intrinsic Promise core/combinators with bounded FIFO
  jobs and typed close/order continuations (29/29, 46/46 feature tags).
- [ ] Complete RegExp `@@split`; implement Date, Temporal, binary data/typed
  arrays, and Atomics.

### Jobs, asynchronous semantics, and modules

- [x] A bounded runtime-owned FIFO job queue, with complete GC edges and one
  turn budget, drains nested Promise work and finalization cleanup
  deterministically; Tokio never defines JavaScript order. Host rejection
  tracking reports first reject/late handle.
- [x] Sync generators, async functions, async generators, and async `yield*`
  preserve parameter timing, intrinsic chains, reentrancy, resume modes,
  thenable assimilation, iterator validation/close order, `catch`/`finally`,
  request order, and suspended-state GC edges.
- [ ] Implement module linking/evaluation, cycles, resolver semantics, dynamic
  import, and top-level `await`. Module parsing alone is not an execution claim.
- [ ] Finish the Rust embedding API, ESM REPL, `qjs`, Rust-native `qjsc`,
  bytecode viewer, CDP adapter, and portable `std`/`os` modules.

### Conformance, performance, and optional layers

- [ ] Maintain pinned Test262 `5c8206929d81b2d3d727ca6aac56c18358c8d790`
  with its patch/configuration/expected errors; expand differential and fuzzing.
- [ ] Establish startup, memory, interpreter, and compile benchmarks; require no
  unexplained supported-platform crashes or undefined behavior.
- [ ] Complete API, source-map, platform/resource, cancellation, dependency, and
  reproducible-release audits.
- [ ] Optional: Wasmtime WebAssembly, an audited safe N-API boundary, erasable
  TypeScript with source maps, and a bounded policy-driven Serde bridge.

## Compatibility differences

- `QJS-OXC-001`: Oxc determines RegExp literal boundaries and flags. The
  project-owned RegExp layer owns pattern grammar, early errors, and execution.
- `QJS-OXC-002`: Oxc accepts a chained-label `continue` target that pinned
  QuickJS rejects; a post-semantic check supplies the target-profile rejection.
- `QJS-OXC-003`: pinned QuickJS reports `stack overflow` around 695 nested
  parentheses. This frontend uses its own bounded isolated stack because that is
  an implementation resource limit, not ECMAScript grammar.
- `QJS-OXC-004`: pinned QuickJS rejects an instance field named `prototype`,
  although the specification reserves that name only for static fields. This
  port follows ECMA-262.
- `QJS-BIGINT-001`: pinned QuickJS can return a negative input unchanged from
  `BigInt.asUintN` when the width already spans it. ECMA-262 requires reduction
  modulo 2**bits and therefore a non-negative result; this port follows the
  specification.
- `QJS-PROMISE-001`: pinned QuickJS lets a hostile synchronous `then` invoke
  both `Promise.allSettled` element closures. ECMA-262 requires the pair to
  share one `[[AlreadyCalled]]` record, so this port preserves the first call.
- `QJS-ASYNC-GENERATOR-001`: when an active async `yield*` delegate handles
  `.return()` with `{ done: true, value: thenable }`, pinned QuickJS assimilates
  `value`. ECMA-262 and Node preserve that property value unchanged; this port
  follows the specification.
- `QJS-MAP-001`: if a `getOrInsertComputed` callback inserts the requested key,
  pinned QuickJS deletes that entry and appends the computed result. ECMA-262
  rescans and updates the callback-created entry in place; this port follows the
  specification and preserves its insertion position.
- `QJS-STRING-001`: pinned QuickJS skips inherited
  `@@match`/`@@search`/`@@replace`/`@@split` lookup for primitive pattern,
  search, or separator values. ES2025 `GetMethod` observes the primitive wrapper
  prototype; this port follows the specification and Node.
- `QJS-REGEXP-001`: pinned QuickJS rejects a Unicode-set string disjunction in
  lookbehind. ECMA-262's backwards matcher and Node accept it; this port follows
  the specification.
- `QJS-REGEXP-002`: pinned QuickJS hex-escapes non-ASCII characters such as
  `é` in `RegExp.escape`. ECMA-262 and Node leave non-whitespace Unicode scalar
  values unchanged; this port follows the specification.

## Completion gates

Every semantic change starts with a focused regression or conformance test. A
checked milestone must pass the applicable differential corpus and these normal
workspace gates:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo audit
```

Run the relevant `cargo xtask *-differential` command against the pinned
QuickJS `qjs` or `qjsc`. Current corpus results are:

| Corpus | Result |
| --- | ---: |
| Parser ledger | 196/196 |
| Dynamic Function source | 39/39 |
| Number radix | 991/991 |
| Control flow | 63/63 |
| Iterators | 40/40 |
| `Function.prototype.apply` | 15/15 |
| `Function.prototype.bind` | 21/21 |
| Call spread | 15/15 |
| Error | 35/35, 59/59 feature tags |
| Legacy `Object.prototype` | 15/15, 15/15 feature tags |
| Annex B String HTML | 6/6, 18/18 feature tags |
| `String.prototype.replaceAll` | 6/6, 14/14 feature tags (QuickJS and Node) |
| `String.prototype.split` | 6/6, 17/17 feature tags (QuickJS and Node) |
| RegExp core | 63/63 (QuickJS and Node) |
| Promise core | 29/29, 46/46 feature tags |
| `Array.fromAsync` | 5/5 (Node; absent in pinned QuickJS) |
| Map | 6/6, 13/13 feature tags (QuickJS and Node) |
| Set | 8/8, 21/21 feature tags (QuickJS and Node) |
| Weak collections | 6/6, 15/15 feature tags (QuickJS and Node) |
| Weak references | 5/5, 13/13 feature tags (QuickJS and Node) |
| Synchronous generators | 18/18, 43/43 feature tags |
| Async functions | 9/9, 18/18 feature tags |
| Async generators | 18/18, 41/41 feature tags (QuickJS and Node) |

The parser gate also fails for an uncovered pinned production, uncovered
reachable diagnostic, falsely unreachable diagnostic, or changed oracle
message. Passing these bounded corpora proves only their declared manifests,
not full engine conformance.

## Engineering rules

1. Implement ECMA-262 semantics first. Use the pinned QuickJS release for
   compatibility details, and assign every intentional conflict a stable ID and
   regression.
2. Reject unsupported semantics instead of approximating them. Only verified
   whole-graph bytecode may execute.
3. Use validated newtypes for bytecode operands and heap handles. Keep parser,
   compiler, verifier, VM, built-in, and host responsibilities separate.
4. Use explicit worklists and typed continuation state for recursive or
   suspendable algorithms. Do not use Rust recursion, locks, or Tokio scheduling
   to mask missing JavaScript semantics.
5. Runtime, Context, heap, and JavaScript-value handles remain thread-affine and
   `!Send + !Sync`; only owned host messages cross threads.
6. Keep the Rust core safe. Any foreign pointer handling is confined to an
   audited boundary crate.
7. Require profiles and benchmarks for performance work and preserve observable
   behavior under differential tests; `unsafe` is never an optimization escape.
8. Match documented upstream omissions: proper tail calls and
   `Atomics.waitAsync` remain out of scope, while `Intl` is a separate optional
   layer. Preserve upstream notices and keep production APIs documented.
