# Porting plan

## Target and boundaries

Port [QuickJS 2026-06-04](UPSTREAM.md) to safe, pure Rust, targeting its
ES2025 script/module semantics (including Annex B), embeddable runtime model,
deterministic destruction with cycle removal, bytecode execution, standard
built-ins, modules, jobs, and the documented `std`/`os` host surface.

The project does **not** reproduce QuickJS binary layout or C API. Rust callers
get an idiomatic lifetime-safe API; an optional, isolated N-API adapter may
provide a C ABI. Behavior that cannot yet be reproduced safely must be
documented, tested, and fail closed.

QuickJS is the sole runtime-semantics reference. Oxc is the selected parser;
no alternate JavaScript engine, VM, GC, or RegExp implementation may supply
runtime semantics. The upstream C engine is a differential-testing oracle only
and is never linked or shipped.

## Architecture

Production crates remain independently reusable and documented:

- `quickjs-diagnostics`: sources, stable diagnostics, spans, source maps, and
  optional Miette rendering.
- `quickjs-frontend`: Oxc parsing, semantic analysis, parse goals, and owned
  frontend records.
- `quickjs-bytecode`: owned instructions, verifier, serializer, disassembler,
  constants, atoms, and debug tables.
- `quickjs-compiler`: Oxc lowering to verified bytecode.
- `quickjs-runtime`: values, heap, realms, VM, built-ins, modules, jobs,
  limits, interrupts, and embedding APIs.
- `quickjs`: ergonomic facade; thin `qjs`, `qjsc`, and bytecode-viewer CLIs
  consume the library crates.
- Optional/lower-priority crates: Tokio host driver, inspector, Wasm, N-API,
  TypeScript stripping, and Serde conversion.

`xtask`, fuzzers, and benchmarks are repository tooling, not production
dependencies. See [ARCHITECTURE.md](ARCHITECTURE.md) for trust boundaries and
[BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md) for the bytecode contract.

## Status

### Foundation and frontend

- [x] Reproducible Rust workspace; CI formatting, linting, tests, docs, audit,
  and optional oracle runners. Workspace-owned core crates forbid `unsafe`.
- [x] Directly pin published Oxc parser/semantic crates; no vendoring or
  patches.
- [x] Parse Script, Module, all dynamic Function-constructor forms, strict
  scripts, and asynchronous global scripts through explicit, lossless goals.
  Unsupported adapters reject before parsing.
- [x] Preserve byte-accurate diagnostics, owned semantic/module records,
  source-order static requests, import attributes, and binding roles.
- [x] Differential parser and Function-constructor manifests, including
  closed goal/feature/claim validation and a pinned compiler oracle.
- [x] The parser ledger is exhaustive and closed in four dimensions: parse
  goals, frontend claims, QuickJS grammar productions, and QuickJS parser
  diagnostics. Productions are enumerated from the pinned parser's own dispatch
  structure and each must be exercised by a fixture the oracle accepts. Every
  `SyntaxError` the pinned front end can raise while compiling a source text is
  either provoked by a fixture or recorded as unreachable with a reason; the
  observed oracle message is matched against the pinned format string on every
  run. Each intentional Oxc difference keeps an ID, rationale, and regression
  fixture.

Known intentional parser differences:

- `QJS-OXC-001`: Oxc determines RegExp literal boundaries/flags; the deferred
  QuickJS-derived layer owns pattern grammar.
- `QJS-OXC-002`: chained labels may accept a `continue` target that QuickJS
  rejects; a post-semantic check supplies the pinned QuickJS rejection.
- `QJS-OXC-003`: pinned QuickJS caps parser recursion near 695 nested
  parentheses and reports `stack overflow` (`quickjs.c:22720`); the frontend
  parses the same source on its isolated stack, since the bound is a QuickJS
  resource limit rather than ECMAScript grammar.
- `QJS-OXC-004`: pinned QuickJS rejects an instance field named `prototype`
  (`quickjs.c:25396`), which its own source marks as inconsistent with the
  specification; the frontend follows ECMAScript, which reserves `prototype`
  only for static fields.

Known intentional runtime differences:

- `QJS-BIGINT-001`: `js_bigint_asUintN` returns its argument unchanged whenever
  the requested width already spans the value (`quickjs.c:56075` and
  `quickjs.c:56092`), so the pinned `qjs` reports `BigInt.asUintN(64, -1n)` as
  `-1n` and `BigInt.asUintN(100, -1n)` as `-1n`. ECMAScript's `BigInt::asUintN`
  is defined modulo 2**bits and is therefore always non-negative; V8 reports
  `18446744073709551615n` and `1267650600228229401496703205375n`. Because the
  specification is the authority where the two disagree, this port follows
  ECMAScript. Widths below 64 agree with both engines.

Known intentional profile narrowings. These are not behavior differences: the
narrowed surface fails closed with a structured error rather than answering
incorrectly, so no script can observe a wrong result.

- `QJS-CREATE-001`: `Object.create` admits only its prototype argument. Honoring
  `propertyDescriptors` means running `ToPropertyDescriptor` for each key, which
  is resumable work this entry point cannot perform, so a present second
  argument reports `TypeError: property descriptors are not supported` instead of
  being silently ignored. The reported `length` stays `2` to match the pinned
  oracle, because arity is part of the observable shape.

### Compiler, bytecode, and execution

- [x] Complete opcode metadata, checked codec/disassembly, typed operands,
  deterministic encoding, bounded construction, and total decoding.
- [x] Verifier foundations: predecode, targets/indices, stack-depth joins,
  maximum stack checking, function headers/kinds, and source-PC diagnostics.
- [x] Whole-graph verified bytecode is the only executable authority. Raw or
  serialized bytecode and direct `eval` remain fail closed.
- [x] Iterative Oxc lowering for ordinary functions, lexical bindings/captures,
  nested closures, expressions, statements, labels, `switch`, classic `for`,
  `for-in`, `for-of`, calls/spread, destructuring, and selected Error/native
  frame behavior. Compiler traversal and verification use explicit worklists.
  Array-assignment member and rest targets evaluate their base and computed key
  after the iterator is acquired and before the matching iterator step, which is
  the order ECMAScript's IteratorDestructuringAssignmentEvaluation and the
  pinned QuickJS reference (`quickjs.c:26596-26612`) both require; ordering
  regressions observe `next`, base, and computed-key effects.
- [x] Runtime installation, calls, exceptions, resumable native/bytecode
  dispatch, iterator close/error precedence, bounded resources, and
  verified-frame stack traces for the admitted profile. Host calls
  (`Context::call`) unwrap bound functions with the same observable result as
  interpreter dispatch: the innermost bound receiver reaches native and bytecode
  targets, and every bound layer's arguments accumulate before the caller's
  arguments are appended once.
- [x] Host interrupts: an embedder-installed handler is polled on a decrementing
  counter rather than on every instruction, reproducing `js_poll_interrupts` and
  its `JS_INTERRUPT_COUNTER_INIT` of 10,000 (`quickjs.c:512`, `quickjs.c:7877`).
  Fuel and interrupts stay separate because they answer different questions: fuel
  is a pre-committed deterministic budget, while an interrupt is a decision the
  host makes during execution, which is what makes wall-clock deadlines and user
  cancellation expressible. Upstream marks an interrupt uncatchable
  (`quickjs.c:7861`); this port preserves that structurally by reporting
  `ExecutionError::Interrupted` instead of a `JsException`, so it bypasses the
  JavaScript unwinder by construction.
- [ ] Complete verifier coverage, source/debug tables, dynamic `eval`, and
  remaining compiler/runtime opcode families.

### Values and objects

- [x] UTF-16 strings (including lone surrogates), Numbers with signed zero and
  int32 fast paths, property-key/index recognition, atoms/symbols, descriptors,
  bounded arenas, and iterative tracing/cycle reclamation foundations. Realm
  intrinsic descriptors follow the pinned upstream flags, including the
  non-writable, non-configurable `Function.prototype[Symbol.hasInstance]`
  (`quickjs.c:39511-39523`), so inherited `instanceof` behavior cannot be
  replaced by assignment.
- [x] First ordinary-object slice: object literals; data/accessor properties;
  ordinary reads/writes; receiver-aware calls; computed keys; and resumable
  getter/setter dispatch.
- [x] Operator/coercion profile: ordinary arithmetic, bitwise, comparison, and
  equality operators; resumable `ToPrimitive`; `StringToNumber`; radix
  conversion; and exact Number formatting tests for bases 2–36.
- [x] Boolean, Number, and String constructor/prototype verticals, including
  wrapper behavior, realm ownership, strict/sloppy receiver rules, and
  `Object.prototype` tagging/boxing as admitted by the current profile.
- [x] Descriptor authority and mutable object structure:
  `ValidateAndApplyPropertyDescriptor`/`OrdinaryDefineOwnProperty` decide every
  own-property definition, so a non-configurable property rejects a
  reconfiguration and a non-writable one accepts only a `SameValue` rewrite;
  `[[Delete]]`, `[[OwnPropertyKeys]]` with the full index/string/symbol phase
  order, `[[SetPrototypeOf]]` with its same-value-before-extensibility rule
  (`quickjs.c:7940`), `[[PreventExtensions]]`, and `SetIntegrityLevel`. The
  `delete` operator and object-literal `__proto__` reach these through the
  pinned `OP_delete` and `OP_set_proto` shapes. A realm-owned `Object`
  constructor publishes `getPrototypeOf`, `setPrototypeOf`, `preventExtensions`,
  `isExtensible`, `seal`, `freeze`, `isSealed`, `isFrozen`, `keys`, and
  `getOwnPropertyNames`. Shared `ToIntegerOrInfinity`, `ToLength`, and `ToIndex`
  replace the previously inlined length truncations.
- [x] `Array.prototype.join` and `Array.prototype.toString` as one resumable
  element loop mirroring `js_array_join` (`quickjs.c:42505`): the length is read
  once with `ToLength`, `null`/`undefined` elements and holes contribute
  nothing, each element's `ToString` and each accessor getter can re-enter the
  interpreter, and the separator defaults to `","` when absent or `undefined`.
  This closes the coercion divergence in which `String([1,2])` produced
  `"[object Array]"`.
- [x] BigInt domain: a project-owned two's-complement limb representation
  mirroring `JSBigInt` (`quickjs.c:490-495`), the full operator set with the two
  numeric domains kept separate (`cannot convert bigint to number` for a mixed
  pair, no unary `+`, no `>>>`), relational comparison and loose equality mixing
  by exact mathematical value rather than by rounding, `typeof`, truthiness,
  strict equality, `ToString`/`ToPropertyKey`, executable literals, an
  `Object(bigint)` wrapper with `[object BigInt]` tagging, and a realm-owned
  non-constructable `BigInt` with `toString`, `valueOf`,
  `[Symbol.toStringTag]`, `asIntN`, and `asUintN`.
- [x] Complete numeric conversions: the modular narrow conversions (`ToInt8`,
  `ToUint8`, `ToInt16`, `ToUint16`) share `ToUint32` and a truncation, while
  `ToUint8Clamp` saturates and rounds half to even because upstream uses `lrint`
  (`quickjs.c:13381`). `ToNumeric` admits a `BigInt` where `ToNumber` rejects one
  (`quickjs.c:13025`), and the `Number` constructor is its only caller
  (`quickjs.c:44595`), so `Number(1n)` is `1` while `1n | 0` still throws. The
  supporting `JsBigInt::to_f64` takes the top 54 significant bits and folds the
  remainder into a sticky flag, so `Number(9007199254740993n)` is
  `9007199254740992` and an out-of-range magnitude becomes a signed infinity.
  `CanonicalNumericIndexString` accepts only the exact `ToString` spelling, with
  `"-0"` answered directly (`quickjs.c:3675`).
- [x] `String.prototype` methods that need no `RegExp` or Unicode tables: `at`,
  `charAt`, `charCodeAt`, `codePointAt`, `concat`, `endsWith`, `includes`,
  `indexOf`, `lastIndexOf`, `padEnd`, `padStart`, `repeat`, `slice`,
  `startsWith`, `substr`, `substring`, `trim`, `trimEnd`, `trimStart`,
  `isWellFormed`, and `toWellFormed`. They share one resumable state machine
  because they share one shape: `RequireObjectCoercible`, then `ToString` of the
  receiver, then each declared argument left to right, and every one of those
  steps can re-enter the interpreter. The pinned oracle fixes that order, logging
  `recv,arg,pos` for `indexOf` with side-effecting conversions. Indices remain
  UTF-16 code-unit indices, so a lone surrogate stays observable.
- [x] `Number` statics and `Array.isArray`: the value properties
  (`MAX_VALUE`, `MIN_VALUE`, `EPSILON`, `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`,
  `POSITIVE_INFINITY`, `NEGATIVE_INFINITY`, `NaN`) are stored as exact binary64
  bit patterns rather than decimal literals and carry the pinned frozen
  descriptor, while `isInteger`, `isSafeInteger`, `isFinite`, and `isNaN` answer
  `false` for a non-Number without converting it, which is what separates them
  from the global `isNaN`. `Number.isInteger(2**53)` is `true` while
  `Number.isSafeInteger(2**53)` is `false`.
- [x] `Object.prototype.hasOwnProperty`, `isPrototypeOf`, and
  `propertyIsEnumerable`, plus `Object.create`. The first and third share one
  own-property resolution with `Object.getOwnPropertyDescriptor`, so all three
  agree on every exotic case: a primitive String reports its indices and
  `length`, and a hole is absent rather than `undefined`, which is the same
  distinction `Array.prototype.indexOf` relies on. `isPrototypeOf` starts its
  walk at the candidate's prototype, so nothing precedes itself, and charges the
  shared budget per link. `Object.create` represents a null prototype rather than
  substituting one; see `QJS-CREATE-001` for its narrowed descriptors argument.
- [x] `String.fromCharCode` and `String.fromCodePoint`, sharing the same
  resumable machine as the prototype methods because their arguments are also
  arbitrary objects. The two differ in coercion and range: `fromCharCode` applies
  `ToUint16` and wraps silently, so `String.fromCharCode(65601)` is `"A"`, while
  `fromCodePoint` requires an exact code point in `0..=0x10FFFF` and otherwise
  reports `RangeError: invalid code point`. A supplementary code point is encoded
  as a surrogate pair, so `String.fromCodePoint(0x1F600).length` is `2`.
- [x] `Array.prototype.indexOf`, `lastIndexOf`, and `includes` as one resumable
  element loop, since every element read can run a getter. They differ in exactly
  two observable ways, which are carried as data rather than as separate
  implementations. The comparison: the index searches use strict equality, so
  `[NaN].indexOf(NaN)` is `-1`, while `includes` uses `SameValueZero`, so
  `[NaN].includes(NaN)` is `true`; both treat the signed zeros as equal. Holes:
  the index searches test `HasProperty` first and skip a missing index, so
  `[1,,3].indexOf(undefined)` is `-1`, while `includes` reads every index and
  answers `true`. The length is read once with `ToLength`, and the loop stops at
  the first match, so a second matching getter never runs.
- [ ] Remaining String/Number/Array method surface (notably `toFixed`,
  `toPrecision`, and `toExponential`, which need exact decimal formatting, and
  the `Array.prototype` mutators), shape sharing/transition interning, remaining
  exotics (arguments, Proxy), dense indexed storage, deterministic finalization,
  and diagnostics.

### Built-ins and asynchronous semantics

- [x] Initial Error family: Error, native Error subclasses, AggregateError,
  constructor/prototype graphs, causes, `Error.isError`, `toString`, iterator
  ordering/close behavior, and snapshotted engine-error stacks.
- [ ] Close Error compatibility gaps, then implement Object/Function/Reflect,
  Proxy, remaining built-ins, RegExp/Date/JSON, collections, binary data,
  Atomics, Unicode tables, promises, async functions/generators, weak
  references, and finalization registries.
- [ ] Add deterministic QuickJS-compatible job ordering. Tokio may provide
  host I/O, timers, cancellation, and wakeups, but never Promise-job ordering.

### Modules, embedding, and tools

- [x] Initial runtime/realm/context foundation with bounded realm creation,
  same-runtime handle checks, verified-function installation, primitive values,
  and host invocation.
- [ ] Full Rust embedding API; module linking/cycles/dynamic import/top-level
  await; QuickJS-compatible resolver semantics; ESM REPL; `qjs`; Rust-native
  `qjsc`; bytecode viewer; CDP adapter; and portable `std`/`os` modules.

### Conformance, performance, and optional layers

- [ ] Run/maintain upstream suites, pinned test262
  `5c8206929d81b2d3d727ca6aac56c18358c8d790`, differential corpora, and
  fuzzing for parser, bytecode, serializer, and runtime boundaries.
- [ ] Establish startup, memory, interpreter, and compile benchmarks; require
  no unexplained supported-platform crashes or undefined behavior.
- [ ] Complete public API, source-map, platform/resource-limit, cancellation,
  dependency, and reproducible-release audits.
- [ ] Optional: Wasmtime WebAssembly, safe N-API semantics plus an audited ABI
  boundary, erasable TypeScript preprocessing with required source maps, and a
  bounded policy-driven Serde bridge.

## Completion gates

A milestone is complete only when its checked items pass in CI. Each semantic
change starts with a regression or conformance test. Relevant standard gates:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Run the applicable `cargo xtask *-differential` corpus against the pinned
QuickJS oracle for parser, dynamic Function, Number radix, control flow,
function apply/bind, iterators, call spread, and Errors. The parser manifest is
a closed compatibility gate: it fails when a pinned grammar production has no
accepted fixture, when a reachable pinned diagnostic has no fixture, when a
fixture declares an unreachable one, or when an observed oracle message does not
match the pinned format string.

Current corpus status: parser 196/196, Number radix 991/991, control flow 63/63,
iterators 40/40, function apply 15/15, function bind 21/21. The Error corpus
stands at 26/35; every remaining mismatch needs a built-in this profile does not
install yet (`Reflect`, and the `Function.prototype.call` reachable from a
descriptor lookup), so each fails closed as a missing property rather than
producing a wrong answer.

## Engineering rules

1. Preserve observable ECMAScript behavior, not QuickJS private representation.
2. Use validated newtypes for bytecode operands and heap handles; reject
   unsupported semantics rather than silently approximating them.
3. Keep parser, compiler, VM, and host concerns separate. Carry source
   identity/spans through bytecode and stack frames; retain structured errors
   independently of CLI rendering.
4. Keep the Rust core safe. Any N-API pointer handling is confined to its
   audited boundary crate.
5. Performance changes require a profile, benchmark, and preserved observable
   behavior; `unsafe` is never an optimization escape hatch.
6. Tokio is a host substrate only; the runtime owns ECMAScript jobs and Promise
   ordering.
7. Match documented upstream omissions: proper tail calls and `Atomics.waitAsync`
   remain out of scope; `Intl` is a separate optional layer.
8. Preserve upstream copyright notices. Keep changes small, bisectable, tested,
   and recorded in Git; production APIs must be documented and stable.
