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

QuickJS binary layout and C API compatibility are not goals. Rust callers
receive an idiomatic, lifetime-safe API. The optional N-API boundary separately
targets that external C ABI through a Rust-written adapter. Any host behavior
that cannot be reproduced portably or safely will be documented with a
compatibility test.

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
- `quickjs-inspector`: runtime debugger API and transport-independent Chrome
  DevTools Protocol adapter;
- `quickjs-wasm`: optional Wasmtime-backed JavaScript WebAssembly surface;
- `quickjs-napi-core` and `quickjs-napi-abi`: low-priority safe N-API
  semantics plus an isolated Rust C-ABI boundary;
- `quickjs-typescript-strip`: low-priority, source-mapped, erasable
  TypeScript preprocessing;
- `quickjs`: ergonomic facade with deliberate feature flags;
- thin `qjs`, `qjsc`, and bytecode-viewer binary crates built entirely on
  those libraries.

`xtask` and fuzz/benchmark harnesses are repository tooling rather than
production crates. Production crates remain usable without the CLIs.

## Acceptance gates

A milestone is complete only when all of its checked items pass in CI.

### M0 — reproducible foundation

- [x] Record the upstream release and archive digest.
- [x] Require workspace-owned core crates to forbid `unsafe`; audit
      dependencies separately. Only the future isolated N-API ABI crate may
      opt out for documented foreign-pointer operations.
- [x] Add formatting, linting, test, documentation, and dependency-audit CI.
- [x] Add an optional differential runner for the upstream `qjs` executable.
- [x] Selectively vendor the exact pinned 17-package Oxc-family closure with
      registry checksums, VCS provenance, licenses, local path overrides, and
      offline source-resolution verification.

### M1 — Oxc source front end

- [x] Pin the current Oxc parser crates and define the arena-safe AST boundary.
- [x] Represent global Script, Module, indirect/direct eval, and all dynamic
      Function-constructor goals losslessly; unsupported contextual adapters
      fail before Oxc without semantic downgrade.
- [ ] Parse JavaScript scripts, modules, eval input, and Function-constructor
      bodies with explicit modes; do not enable TypeScript or JSX.
  - [x] Parse all four dynamic Function-constructor families through the exact
        QuickJS wrapper as a complete Oxc Script, retaining a byte-exact
        fragment map and fail-closed preparation limits.
- [x] Reject parser and deferred semantic diagnostics with byte-accurate source
      spans.
- [x] Retain Oxc `ModuleRecord` and complete `Semantic` data on every successful
      parsed unit for compiler lowering.
- [x] Lower static module requests into an arena-independent owned record with
      source occurrence order, exact UTF-16 strings, import attributes,
      typed request indices, and local/indirect/star linking roles.
- [ ] Differentially test Oxc acceptance, early errors, strict mode, Annex B,
      and ES2025 syntax against the pinned QuickJS release.
  - [x] Differentially test all four dynamic Function-constructor families
        against the pinned `qjsc -c` compiler oracle, including
        exact-wrapper-sensitive comments, wrapper escape, strict/contextual
        parameters, Unicode, and malformed input without executing JavaScript.
- [ ] Record every parser compatibility gap and either close it or mark it as
      an intentional, regression-tested Oxc difference before claiming M1.

### M2 — compiler and VM core

- [x] Port the complete final and compiler-temporary opcode schema, operand
      widths, and fixed/dynamic stack-effect metadata from `quickjs-opcode.h`.
- [x] Checked owned stack-bytecode codec with typed operands, deterministic
      encoding, bounded transactional construction, and a total decoder.
- [x] Function-local `AtomPoolIndex` operands for all five atom-bearing
      formats, with unchanged deterministic encoding and explicit deferred
      pool-bounds validation.
- [x] Bounded human-readable disassembly with typed operands, resolved stack
      effects, stable text, and structured malformed-input/limit failures.
- [ ] Control-flow and abstract-stack verifier with computed maximum stack
      depth.
  - [x] First fail-closed slice: complete predecode, all currently modeled
        static target/index/secondary-operand checks, ordinary reachable
        stack-height analysis, exact maximum comparison, and a deliberately
        non-executable `VerifiedControlFlow` certificate.
  - [x] Validate serialized function execution flags, mode bits, defined
        argument and variable-reference counts and their available-binding
        relationship; retain a typed function kind; and enforce suspension and
        return-opcode kind compatibility.
- [ ] Instruction/function PC-to-source tables, source-map chaining, and
      precise source lookup for diagnostics and stack frames.
  - [x] First final-instruction source table: strictly ordered instruction PCs
        retain byte-exact Oxc spans and owned source text across arena teardown
        for the ordinary leaf-function lowering slice.
- [ ] Bindings, lexical environments, closures, calls, constructors, and
      direct/indirect `eval`.
  - [x] First arena-independent binding-storage plan: direct Oxc semantic
        consumption, native dense executable/binding/reference identities,
        resolved-reference-to-binding edges, deterministic owned `Arc` slices,
        Script/Module/import/default-export placement, declaration
        initialization/write/TDZ policy, and typed fail-closed rejection for
        semantic cases not yet lowered.
  - [x] First verified ordinary leaf-function vertical: an arena-borrowing
        `CompilationContext` retains private Oxc node/symbol/reference identity
        maps and issues context-provenant executable selections. Function
        declarations and anonymous `function` expressions in Script units
        assign typed argument/local slots, emit the exact final
        `set_loc_uninitialized; get_arg; put_loc; get_loc_check; return`
        family, track stack depth, and return only an owned non-executable
        `VerifiedControlFlow` certificate.
  - [x] Expand that vertical to pool-free straight-line bodies: multiple
        simple `var`/`let`/`const` declarations, reverse-order TDZ setup,
        immediate Boolean/null/int32 and compact `BigInt` values, the empty
        string, exact argument/local reads and writes, all value-only unary and
        binary operators needing no pools, sequence/expression statements, and
        explicit or implicit returns. Expression lowering uses an iterative
        work list and validates the whole body before emitting bytes; atom,
        constant, and closure pools remain fail-closed until their owned
        records exist.
- [ ] Abrupt completion, exceptions, stack traces, iterators, and generators.
- [ ] Deterministic debug/line tables.

### M3 — values and object model

- [x] JavaScript UTF-16 string primitive with lone-surrogate preservation,
      Latin-1 leaves, depth-bounded ropes, and QuickJS-compatible length limits.
- [x] JavaScript Number representation with signed-zero preservation, int32
      fast paths, overflow promotion, and all three numeric equality modes.
- [x] Canonical property-key array-index recognition through `2^32 - 2`.
- [x] Runtime-local owning atoms, weak UTF-16 content interning, exact
      predefined atoms, global/unique/well-known symbols, private names,
      validated public property keys, and bounded logical usage.
- [x] Opaque generic/data/accessor descriptor classification with exact field
      presence, new-property completion defaults, and value-independent
      ordinary data/accessor layouts.
- [ ] ECMAScript primitive conversions, parsing/printing, and remaining numeric
      edge cases.
- [ ] Property descriptors, shapes, prototypes, and exotic objects.
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
- [ ] Minimal `qjs` runtime and ESM-aware REPL with multiline input, static
      and dynamic modules, top-level await, limits, and clean Tokio shutdown.
- [ ] `qjs` CLI script/module detection and documented options.
- [ ] Rust-native `qjsc` artifact generation with no C compiler dependency.
- [ ] Bytecode viewer CLI with verified function metadata, resolved atoms and
      constants, control-flow targets, and source-map annotations.
- [ ] CDP inspection adapter with safe-point pause/step/breakpoints, scopes,
      object previews, exceptions, console events, and source-mapped locations.
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

### M7 — optional compatibility layers

- [ ] Wasmtime-backed JavaScript `WebAssembly` API with bounded compilation,
      execution, memory, tables, imports/exports, and exception conversion.
- [ ] Safe N-API semantic core plus an isolated, audited Rust C-ABI adapter
      with no C/C++ source, bindgen, or C compiler.
- [ ] Opt-in erasable TypeScript stripping that emits a mandatory source map
      before entering the ordinary JavaScript frontend.
- [ ] Feature, platform, conformance, diagnostics, cancellation, and security
      matrices for every optional layer.

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
