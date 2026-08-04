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

- [x] Reproducible Rust workspace with safe core crates, CI gates, bounded
  resource APIs, and optional oracle runners.
- [x] Published Oxc `0.142.0` parser/semantic crates are pinned directly; no
  vendoring or local patches.
- [x] Lossless parse goals cover Script, Module, strict and asynchronous global
  scripts, and all dynamic Function-constructor families. Unsupported adapters
  reject before parsing.
- [x] Owned semantic/module records preserve byte spans, binding roles,
  source-order static requests, import attributes, and arena-independent data.
- [x] The parser compatibility ledger is closed over parse goals, frontend
  claims, pinned QuickJS grammar productions, and reachable parser diagnostics.
  Each intentional Oxc difference has a stable ID and fixture.
- [ ] Complete source-map chaining and the remaining public diagnostic/API
  audit.

### Compiler, bytecode, and execution

- [x] Complete opcode metadata, typed operands, deterministic checked codec and
  disassembly, bounded construction, and total decoding.
- [x] Only a whole-function, whole-child-graph `VerifiedBytecode` authority may
  be installed or executed. Admission predecodes every instruction and checks
  headers, operands, indices, targets, stack joins, capabilities, child graphs,
  and resource limits before runtime mutation.
- [x] Iterative lowering covers the admitted ordinary-function profile:
  bindings/captures, closures, expressions, statements, labels, `switch`,
  classic `for`, `for-in`, synchronous `for-of`, calls/spread, destructuring,
  exceptions, and `try`/`catch`/`finally`.
- [x] Synchronous generator functions, methods, and dynamic
  `GeneratorFunction` wrappers lower plain `yield` and delegated `yield*` to
  verified suspension programs. Typed iterator records preserve resume-mode
  forwarding, exact iterator-result identity, method absence, object
  validation, `finally`, and abrupt-close order.
- [x] Async functions, methods, and dynamic `AsyncFunction` wrappers lower
  `await` and async return to verified suspension programs. Calls allocate the
  intrinsic Promise capability, run synchronously to the first suspension, and
  resume through typed FIFO reactions; return/throw settle instead of escaping.
- [x] Async generator functions, methods, and dynamic
  `AsyncGeneratorFunction` wrappers lower direct `await`, `yield`, and return to
  verified suspension programs. Admission requires every direct async yield to
  be immediately dominated by its compiler-owned await; async `yield*` remains
  typed fail closed.
- [x] Runtime execution uses explicit frame and continuation stacks for
  bytecode/native calls, constructors, abrupt completion, iterator closing,
  coercion re-entry, and verified stack traces. Bound receiver/argument
  accumulation and destructuring iterator order are regression-tested.
- [x] Deterministic fuel and host interrupts are separate. The interrupt hook is
  polled on the pinned 10,000-instruction counter and reports an uncatchable
  `ExecutionError::Interrupted`.
- [ ] Complete remaining opcode families, debug/source tables, async-generator
  delegation, and direct/indirect `eval`. Raw or serialized unverified bytecode,
  async `yield*`, and `eval` remain fail closed.

### Values, objects, and functions

- [x] Exact UTF-16 strings, binary64 Numbers, project-owned BigInts, Symbols,
  canonical property keys, primitive wrappers, conversion algorithms, ordinary
  operators, and signed-zero/NaN behavior.
- [x] Ordinary data/accessor properties, descriptor validation, deletion,
  own-key phase order, prototype mutation, extensibility/integrity levels,
  object literal `__proto__`, Array exotic `length`, holes, and primitive String
  indexed properties.
- [x] Ordinary functions/constructors, lexical capture and TDZ, all four
  dynamic Function-constructor families, `call`/`apply`/`bind`, `instanceof`,
  rest/default/destructured parameters, separate parameter/body environments,
  strict and mapped `arguments` objects, `Function.prototype` legacy poison
  accessors backed by a shared `%ThrowTypeError%`, and admitted
  `NamedEvaluation` forms.
- [x] Synchronous iterator protocols, Array/String iterators, array and call
  spread, destructuring, `for-of`, `IteratorClose`, and original-abrupt-
  completion precedence use typed, same-site verifier state.
- [x] Realm bootstrap is declared by validated intrinsic schemas. Atom and
  resource plans, typed shell allocation, ordered publication, and allocation-
  free reverse rollback are derived from that schema. A normalized snapshot
  pins all 291 Realm-local identities and 899 ordered properties across Realms.
- [ ] Add Proxy and remaining exotics, shape/transition interning, dense indexed
  storage, deterministic finalization, and complete reflection/diagnostics.

### Built-ins

- [x] Globals: `undefined`, `NaN`, `Infinity`, `globalThis`, `isFinite`, `isNaN`,
  `parseFloat`, `parseInt`, and the four URI encode/decode functions.
- [x] `Object`: the admitted constructor statics, including descriptor,
  integrity, enumeration, copy, iterable, and `groupBy` operations; the common
  prototype methods, deterministic no-`Intl` `toLocaleString`, and legacy
  `__proto__`/accessor methods are present with specification evaluation order.
- [x] `Reflect`: all 13 ordinary-object methods, including resumable
  `apply`/`construct`, receiver-preserving `get`/`set`, and exact own-key order.
- [x] `Error`, native Error subclasses, and `AggregateError`, including causes,
  `Error.isError`, exceptional iterator close, and frozen engine-error stacks.
  The compatibility corpus is 35/35 with 59/59 required feature tags.
- [x] `Boolean`, `Number`, `BigInt`, and `Symbol` constructors/prototypes and
  their admitted statics; Number formatting/conversions and BigInt arithmetic
  cover their full implemented numeric domains.
- [x] `String` constructor/statics and a broad non-RegExp prototype subset,
  including UTF-16 search/slicing/padding, well-formedness, full Unicode casing,
  normalization, deterministic no-`Intl` `localeCompare`, and the plain-string
  plus `@@replace` protocol path of `replace`. All 13 Annex B `CreateHTML`
  wrappers and the identity-sharing `trimLeft`/`trimRight` aliases preserve
  specification coercion order and pinned own-key order. ICU4X data is pinned.
- [x] `Array` construction, `isArray`, synchronous `from`/`of`, species,
  iterators, mutators, searches, callbacks, reductions, sorting, flattening,
  change-by-copy methods, locale rendering, and generic array-like behavior,
  with holes and observable operation order preserved.
- [x] `%JSON%`: iterative `parse`/reviver and `stringify`, plus `rawJSON` and
  `isRawJSON`; lone surrogates, source records, getters/callbacks, cycles,
  replacers, indentation, and raw embedding are covered.
- [x] `%Math%`: all methods and constants installed by the pinned profile,
  including realm-local `random`, exact-width conversions, and the iterator-
  based exact `sumPrecise` accumulator.
- [x] Intrinsic Promise core: branded Promise objects, constructor executors,
  one-shot generic capabilities, thenable assimilation, generic
  `resolve`/`reject`, species-derived `then`/`catch`/`finally`, cleanup-result
  assimilation, bounded FIFO jobs, and the full constructor static family:
  `all`, `allSettled`, `any`, `try`, `race`, and `withResolvers`. Typed
  continuations preserve combinator input order, generic capabilities,
  thenable calls, remaining-element records, and abrupt iterator close. The
  pinned corpus is 29/29 with 46/46 feature tags.
- [ ] Complete RegExp-coupled String methods and `Array.fromAsync`; implement
  RegExp, Date, Temporal, Proxy, collections, binary data/typed arrays, Atomics,
  weak references, and finalization registries.

### Jobs, asynchronous semantics, and modules

- [x] A runtime-owned, resource-bounded FIFO Promise-job queue drains nested
  work to a fixed point after normal or JavaScript-abrupt host completion.
  Jobs, pending reactions, results, and resolving functions are explicit GC
  edges; the turn shares one fuel/interrupt budget. Tokio never defines
  JavaScript job order.
- [x] Host Promise rejection tracking reports `reject` at unhandled settlement
  and `handle` at the first later reaction. Borrowed callback values add no GC
  roots; hosts explicitly retain owned handles when needed.
- [x] Synchronous generators preserve parameter timing, realm-local intrinsic
  chains, reentrancy rejection, all suspension states, yielding `finally`,
  nested iterator close, and `yield*` delegation across `next`/`return`/`throw`.
  Suspended frames and cached delegate methods are GC-traced; iterator-result
  admission remains failure-atomic. Dynamic `GeneratorFunction` preserves
  source coercion order, syntax rejection, metadata, and `newTarget` prototype
  selection through the same bounded compiler service.
- [x] Async functions start synchronously, always resume `await` through the
  intrinsic Promise queue, preserve rejection as an abrupt completion at the
  await site, and settle their returned Promise through thenable assimilation.
  `%AsyncFunction%`, `%AsyncFunction.prototype%`, methods, dynamic construction,
  suspended-frame GC edges, and non-constructability are pinned by QuickJS and
  Node differentials.
- [x] Async generators use realm-local `%AsyncGeneratorFunction%`,
  `%AsyncGenerator%`, and `%AsyncIteratorPrototype%` chains plus a typed FIFO
  request queue. Calls defer the body; `next`/`return`/`throw` allocate intrinsic
  Promise capabilities, await yielded and returned values, preserve synchronous
  parameter timing and `catch`/`finally` order, drain completed requests, and
  trace suspended frames, awaits, capabilities, and reactions through GC.
  Dynamic construction preserves coercion order and `newTarget` fallback.
- [ ] Implement async-generator `yield*` over the async-iterator protocol;
  compiler and verifier reject it before execution meanwhile.
- [ ] Implement module linking/evaluation, cycles, resolver semantics, dynamic
  import, and top-level `await`. Module parsing alone is not an execution claim.
- [ ] Finish the Rust embedding API, ESM REPL, `qjs`, Rust-native `qjsc`,
  bytecode viewer, CDP adapter, and portable `std`/`os` modules.

### Conformance, performance, and optional layers

- [ ] Run and maintain the pinned Test262 baseline
  `5c8206929d81b2d3d727ca6aac56c18358c8d790` with the release patch,
  configuration, and expected-error list; expand differential and fuzz coverage
  at parser, bytecode, serializer, and runtime boundaries.
- [ ] Establish startup, memory, interpreter, and compile benchmarks; require no
  unexplained supported-platform crashes or undefined behavior.
- [ ] Complete public API, source-map, platform/resource-limit, cancellation,
  dependency, and reproducible-release audits.
- [ ] Optional: Wasmtime WebAssembly, an audited safe N-API boundary, erasable
  TypeScript preprocessing with mandatory source maps, and a bounded
  policy-driven Serde bridge.

## Compatibility differences

- `QJS-OXC-001`: Oxc determines RegExp literal boundaries and flags. The
  deferred project-owned RegExp layer owns pattern grammar and early errors.
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
| Promise core | 29/29, 46/46 feature tags |
| Synchronous generators | 18/18, 43/43 feature tags |
| Async functions | 9/9, 18/18 feature tags |
| Async generators | 12/12, 28/28 feature tags (QuickJS and Node) |

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
