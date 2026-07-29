# Porting plan

## Compatibility target

The target is QuickJS 2026-06-04:

- ES2025 script and module semantics, including Annex B;
- the embeddable runtime/context/value model;
- reference-counted deterministic destruction with cycle removal;
- stack bytecode, closures, exceptions, generators, async jobs, and modules;
- BigInt, RegExp, Unicode, JSON, Date, Proxy, Map/Set, Promise, TypedArray,
  Atomics, and SharedArrayBuffer;
- `qjs`, a Rust-native `qjsc`, and the documented `std`/`os` host surface.

Binary layout and C ABI compatibility are not goals. Rust callers receive an
idiomatic, lifetime-safe API. Any host behavior that cannot be reproduced
portably or safely will be documented with a compatibility test.

## Planned crate topology

Every production crate must be independently reusable and documented:

- `quickjs-diagnostics`: source registry, stable diagnostic codes, exact spans,
  standard source-map chaining, and optional Miette rendering;
- `quickjs-frontend`: Oxc configuration, parse goals, diagnostics, and the
  arena-safe AST boundary;
- `quickjs-bytecode`: owned instruction schema, verifier, serializer,
  disassembler, constants, atoms, and debug tables;
- `quickjs-compiler`: Oxc AST/semantic lowering into verified bytecode;
- `quickjs-runtime`: values, heap, contexts/realms, VM, built-ins, modules,
  jobs, limits, interrupts, and embedding APIs;
- `quickjs-tokio`: optional Tokio host driver for async I/O, timers, workers,
  and event-loop integration;
- `quickjs`: ergonomic facade with deliberate feature flags;
- thin `qjs` and `qjsc` binary crates built entirely on those libraries.

`xtask` and fuzz/benchmark harnesses are repository tooling rather than
production crates. Production crates remain usable without the CLIs.

## Acceptance gates

A milestone is complete only when all of its checked items pass in CI.

### M0 — reproducible foundation

- [x] Record the upstream release and archive digest.
- [x] Establish an `unsafe`-free Rust workspace.
- [x] Add formatting, linting, test, documentation, and dependency-audit CI.
- [x] Add an optional differential runner for the upstream `qjs` executable.

### M1 — Oxc source front end

- [x] Pin the current Oxc parser crates and define the arena-safe AST boundary.
- [ ] Parse JavaScript scripts, modules, eval input, and Function-constructor
      bodies with explicit modes; do not enable TypeScript or JSX.
- [x] Reject parser and deferred semantic diagnostics with byte-accurate source
      spans.
- [ ] Differentially test Oxc acceptance, early errors, strict mode, Annex B,
      and ES2025 syntax against the pinned QuickJS release.
- [ ] Record and close any parser compatibility gaps before claiming M1.

### M2 — compiler and VM core

- [x] Port the complete final and compiler-temporary opcode schema, operand
      widths, and fixed/dynamic stack-effect metadata from `quickjs-opcode.h`.
- [ ] Checked owned stack-bytecode format with computed maximum stack depth.
- [ ] Instruction/function PC-to-source tables, source-map chaining, and
      precise source lookup for diagnostics and stack frames.
- [ ] Bindings, lexical environments, closures, calls, constructors, and
      direct/indirect `eval`.
- [ ] Abrupt completion, exceptions, stack traces, iterators, and generators.
- [ ] Bytecode verifier and deterministic debug/line tables.

### M3 — values and object model

- [x] JavaScript UTF-16 string primitive with lone-surrogate preservation,
      Latin-1 leaves, depth-bounded ropes, and QuickJS-compatible length limits.
- [x] JavaScript Number representation with signed-zero preservation, int32
      fast paths, overflow promotion, and all three numeric equality modes.
- [ ] ECMAScript primitive conversions, parsing/printing, and remaining numeric
      edge cases.
- [ ] Interned atoms, symbols, property descriptors, shapes, prototypes, and
      exotic objects.
- [ ] Dense/sparse arrays and typed indexed access.
- [ ] Deterministic reference ownership plus cycle collection with explicit
      roots and finalization rules.
- [ ] Runtime memory limit, stack limit, interrupt hook, and diagnostics.

### M4 — built-ins and asynchronous semantics

- [ ] Fundamental objects, functions, errors, reflection, and proxies.
- [ ] Number, BigInt, String, RegExp, Date, JSON, and structured data.
- [ ] Collections, ArrayBuffer, TypedArray, DataView, Atomics, and shared data.
- [ ] Resizable/transferable ArrayBuffer, iterator helpers, Set methods,
      Map/WeakMap upsert, `Atomics.pause`, and `Math.sumPrecise`.
- [ ] Uint8Array base64/hex codecs and duplicate named RegExp capture groups.
- [ ] Promise jobs, async functions/generators, weak references, and
      finalization registries.
- [ ] Tokio-backed timers, I/O readiness, cancellation, and wakeups behind a
      QuickJS-compatible, deterministic JavaScript job queue.
- [ ] Compressed Unicode property, normalization, and case-mapping tables.

### M5 — modules, embedding, and tools

- [ ] Realm/context API and Rust-native host functions/classes/modules.
- [ ] Module linking, cyclic graphs, dynamic import, and top-level await.
- [ ] Evaluate Oxc Resolver as an implementation aid without inheriting its
      Node defaults; preserve QuickJS relative/system module-name semantics.
- [ ] `qjs` CLI, REPL, script/module detection, and documented options.
- [ ] Rust-native `qjsc` artifact generation with no C compiler dependency.
- [ ] Portable `std`/`os` modules with documented platform and safety policy.

### M6 — conformance and performance

- [ ] Upstream built-in language, closure, BigInt, and module suites.
- [ ] test262 runner at `5c8206929d81b2d3d727ca6aac56c18358c8d790`,
      with the upstream patch, configuration, exclusions, and baseline report.
- [ ] Differential corpus against QuickJS 2026-06-04.
- [ ] Fuzz parser, bytecode verifier, serializer, and runtime boundaries.
- [ ] Startup, memory, interpreter, and compile-time benchmark baselines.
- [ ] Zero unexplained crashes or undefined behavior under supported sanitizers
      and interpreters.
- [ ] Public API review: SemVer policy, feature matrix, structured errors,
      rustdoc examples, embedding examples, and no undocumented panics.
- [ ] Source-map audit: byte offsets, Unicode line/column conversion,
      generated-to-original chaining, stack traces, eval/modules, and malformed
      map handling.
- [ ] Production audit: supported-platform matrix, resource limits,
      cancellation/shutdown, malformed-input hardening, dependency policy, and
      reproducible release artifacts.

## Engineering rules

1. Add a failing regression or conformance test before each semantic change.
2. Use QuickJS 2026-06-04 as the only JavaScript-runtime implementation
   reference. Oxc is the explicitly selected parser; do not consult, copy,
   adapt, or depend on another JavaScript engine, port, VM, garbage collector,
   or RegExp implementation. General-purpose Rust crates are permitted when
   they do not supply alternate JavaScript runtime semantics.
3. Keep parser, compiler, VM, and host concerns separable even when they share
   a crate initially.
4. Encode bytecode operands and heap handles with validated newtypes.
5. Performance changes may depart from QuickJS's private representation, but
   must retain observable behavior, start from a profile, and add a benchmark.
   Never use `unsafe` as an optimization escape hatch.
6. Preserve upstream copyright notices for translated source and generated
   tables.
7. Commit milestones as small, bisectable changes after all relevant gates
   pass.
8. Track the latest stable Rust toolchain. Nightly-only work must be isolated,
   justified, and must not silently become a runtime requirement.
9. Use Tokio for host async I/O and event-loop driving, never as a substitute
   for the ECMAScript job queue or QuickJS Promise ordering.
10. Carry source identity and spans from Oxc through bytecode and runtime stack
    frames. Keep stable structured diagnostics separate from Miette/CLI
    rendering so embedders never need to scrape text.
