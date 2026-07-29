# Adapters and developer tooling

This document records required surfaces beyond the QuickJS 2026-06-04
compatibility core. QuickJS remains the sole JavaScript-runtime implementation
reference. Protocol specifications and general-purpose Rust crates may define
external interfaces, but they do not define JavaScript semantics.

## Delivery order

Required alongside the core engine:

1. a minimal ESM-capable runtime and REPL;
2. a human-oriented bytecode viewer;
3. a Chrome DevTools Protocol inspection adapter.

Lower-priority optional layers:

1. Wasmtime-backed WebAssembly support;
2. N-API compatibility;
3. built-in TypeScript stripping.

Each layer must be independently reusable. The executable tools remain thin
clients of library crates.

## Minimal runtime and ESM REPL

The first executable runtime grows from the vertical engine slice rather than
waiting for every built-in. Its REPL must support:

- multiline Script and Module input with explicit parse goals;
- static and dynamic ESM loading through the project-owned module loader;
- top-level `await` once async module evaluation is implemented;
- deterministic QuickJS-derived Promise-job draining on the runtime owner;
- pretty diagnostics and stack traces mapped back to original sources;
- interrupt, memory, stack, and pending-I/O limits;
- clean cancellation and shutdown of Tokio-backed host operations.

The UX may be familiar to developers, but module naming, resolution, linking,
evaluation, and error behavior remain deliberate project APIs. Oxc Resolver
may be evaluated as a path-resolution aid without importing undeclared host
defaults.

## Human bytecode viewer

`quickjs-bytecode` owns a reusable checked disassembler. A thin viewer tool
will add source-aware presentation after verified functions and debug tables
exist.

The viewer must:

- decode through the checked final-bytecode codec and never execute input;
- distinguish compiler labels from signed final displacements;
- show function metadata, constants, atoms, operands, stack effects, jump
  targets, handlers, and maximum stack depth;
- annotate instructions with generated and original source locations;
- mark malformed or unverifiable input at the exact bytecode PC;
- bound decoded instructions, nesting, strings, and rendered output;
- offer stable text for humans and structured output for tools.

## CDP inspection adapter

The inspector is a transport-independent library above the runtime debugger
API. A Tokio transport may carry protocol messages, but only the runtime owner
may inspect or mutate JavaScript state.

The initial inspection surface covers:

- runtime enable/evaluate and console events;
- debugger enable, pause, resume, step, and breakpoint operations;
- scripts, source text, source-map locations, stack frames, scopes, and object
  previews;
- exception pause state and source-mapped stack traces;
- bounded remote-object handles with explicit release;
- disconnect, cancellation, and stale-command behavior.

Pause requests are observed at VM safe points. No heap borrow crosses an
await, no transport task receives a JavaScript value, and debugger activity
cannot reorder ECMAScript jobs.

## Optional Wasmtime WebAssembly layer

An optional reusable crate will provide the JavaScript `WebAssembly` surface
using a currently verified, exactly pinned Wasmtime dependency. It is an
additive extension rather than part of the pinned QuickJS compatibility
baseline.

Wasmtime owns validation, compilation, instantiation, and WebAssembly
execution. The adapter owns JavaScript conversions, wrapper identity,
exceptions, imports/exports, memory/table/global views, async boundaries,
limits, cancellation, and source-facing diagnostics. Default features must be
minimized and platform support documented before the dependency is added.

## Optional N-API layer

N-API support is split into:

- a safe semantic core that maps environments, handles, references, scopes,
  callbacks, classes, errors, async work, and cleanup onto the embedding API;
- an isolated Rust-written C-ABI adapter.

The adapter contains no C or C++ source, bindgen output, or C compiler step.
Because a C ABI necessarily dereferences foreign pointers, any required
`unsafe` is confined to that adapter, documented per operation, and audited
separately. Core engine crates continue to forbid unsafe Rust. Invalid foreign
inputs must return defined status values rather than reach runtime invariants.

## Optional TypeScript stripping

TypeScript support is an explicit source-to-source preprocessing mode, not an
alternate runtime grammar. The ordinary JavaScript frontend continues to
reject TypeScript and JSX.

The initial layer strips erasable type syntax only, produces JavaScript plus a
mandatory standard source map, and then enters the normal Oxc-backed
JavaScript pipeline. It does not type-check, silently change module resolution,
or erase syntax whose runtime meaning is ambiguous. Diagnostics and stack
traces must map through the generated layer to the original TypeScript bytes.

## Shared acceptance rules

Every adapter or tool requires:

- stable structured errors plus human-readable rendering;
- explicit resource limits and malformed-input tests;
- source-map coverage for generated or transformed code;
- cancellation and shutdown tests where asynchronous work exists;
- focused differential tests when behavior overlaps the pinned QuickJS
  surface;
- complete formatting, lint, test, rustdoc, security, and license gates;
- an independently reviewable Git commit with no unrelated changes.
