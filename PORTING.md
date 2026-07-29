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

## Acceptance gates

A milestone is complete only when all of its checked items pass in CI.

### M0 — reproducible foundation

- [x] Record the upstream release and archive digest.
- [x] Establish an `unsafe`-free Rust workspace.
- [ ] Add formatting, linting, test, documentation, and dependency-audit CI.
- [ ] Add an optional differential runner for the upstream `qjs` executable.

### M1 — source front end

- [ ] UTF-8 source locations and diagnostics.
- [ ] ES2025 lexical grammar, including templates, numeric separators, BigInt,
      private names, comments, RegExp-vs-division context, and Unicode escapes.
- [ ] Recursive-descent parser with cover grammars, automatic semicolon
      insertion, strict mode, Annex B, scripts, and modules.
- [ ] Parser fixtures and negative syntax tests derived from test262 metadata.

### M2 — compiler and VM core

- [ ] Checked stack-bytecode format with computed maximum stack depth.
- [ ] Bindings, lexical environments, closures, calls, constructors, and
      direct/indirect `eval`.
- [ ] Abrupt completion, exceptions, stack traces, iterators, and generators.
- [ ] Bytecode verifier and deterministic debug/line tables.

### M3 — values and object model

- [ ] ECMAScript primitive conversions and numeric edge cases.
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
- [ ] Promise jobs, async functions/generators, weak references, and
      finalization registries.
- [ ] Compressed Unicode property, normalization, and case-mapping tables.

### M5 — modules, embedding, and tools

- [ ] Realm/context API and Rust-native host functions/classes/modules.
- [ ] Module linking, cyclic graphs, dynamic import, and top-level await.
- [ ] `qjs` CLI, REPL, script/module detection, and documented options.
- [ ] Rust-native `qjsc` artifact generation with no C compiler dependency.
- [ ] Portable `std`/`os` modules with documented platform and safety policy.

### M6 — conformance and performance

- [ ] Upstream built-in language, closure, BigInt, and module suites.
- [ ] test262 runner, pinned test262 revision, exclusions, and baseline report.
- [ ] Differential corpus against QuickJS 2026-06-04.
- [ ] Fuzz parser, bytecode verifier, serializer, and runtime boundaries.
- [ ] Startup, memory, interpreter, and compile-time benchmark baselines.
- [ ] Zero unexplained crashes or undefined behavior under supported sanitizers
      and interpreters.

## Engineering rules

1. Add a failing regression or conformance test before each semantic change.
2. Keep parser, compiler, VM, and host concerns separable even when they share
   a crate initially.
3. Encode bytecode operands and heap handles with validated newtypes.
4. Never use `unsafe` as an optimization escape hatch; profile first.
5. Preserve upstream copyright notices for translated source and generated
   tables.
6. Commit milestones as small, bisectable changes after all relevant gates
   pass.
