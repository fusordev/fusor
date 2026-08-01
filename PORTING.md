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
- [ ] Remaining conversions, BigInt domains, String/Number surface, complete
  descriptors and shapes, mutable prototypes/exotics, arrays/indexed storage,
  deterministic finalization, limits/interrupts, and diagnostics.

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
