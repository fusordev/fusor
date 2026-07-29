# Architecture

This document records the implementation boundaries and invariants for the
pure-Rust port of QuickJS 2026-06-04. Observable behavior follows the pinned
QuickJS release. Rust-native representations and measured optimizations are
allowed when differential tests preserve that behavior.

## Layering

Production dependencies point inward:

```text
quickjs-diagnostics
        ↑
quickjs-frontend (Oxc)
        ↑
quickjs-compiler ─────────→ quickjs-bytecode
                                 ↑
qjs-dtoa ─┐                     │
qjs-unicode ─→ quickjs-runtime ─┘
qjs-regexp ┘          ↑
                      │
               quickjs-tokio
                      ↑
                    quickjs
                  ↙         ↘
                qjs         qjsc
```

The runtime does not depend on Oxc or Tokio. The compiler consumes an Oxc AST
and produces an owned verified bytecode unit. The Tokio adapter delivers owned
host completions to the runtime owner; it never accesses the JavaScript heap.

All project crates forbid `unsafe` Rust. Oxc, Tokio, and other dependencies are
audited separately; workspace linting cannot make transitive claims about
their internals.

## Compilation boundary

The compilation pipeline is:

1. Register the source and any incoming standard source map.
2. Select Script, Module, eval, or Function-constructor grammar explicitly.
3. Parse JavaScript with Oxc. Oxc RegExp pattern parsing stays disabled.
4. Reject every parser diagnostic, including recoverable diagnostics.
5. Run Oxc semantic early-error checks and reject every diagnostic.
6. Reject syntax accepted by the current Oxc release but outside the pinned
   QuickJS/ES2025 profile.
7. Copy Oxc scope/symbol/reference information into an owned `BindingPlan`.
8. Lower AST nodes into typed pseudo-instructions with copied source origins.
9. Run QuickJS-derived variable resolution, label relaxation, peepholes, stack
   analysis, and debug-table construction.
10. Verify and freeze the bytecode, then drop the Oxc allocator.

No Oxc AST reference may survive compilation. Static Oxc resolution is only an
input: `with`, direct eval, Annex B bindings, and global declaration
instantiation require QuickJS-compatible dynamic handling.

## Bytecode

The instruction set and compiler passes derive from `quickjs-opcode.h` and the
corresponding QuickJS compiler/VM code.

Only `VerifiedBytecode` may execute. Verification checks:

- known opcodes and complete operand boundaries;
- constant, atom, local, argument, closure, and module indices;
- jump targets on instruction boundaries;
- identical operand-stack depth at control-flow joins;
- no stack underflow and a computed maximum stack depth;
- exception/finally handler structure;
- function metadata and child-function references;
- source-map PCs on valid instruction boundaries.

Malformed serialized bytecode returns a structured verifier error. It never
reaches unchecked indexing in the VM.

## Runtime ownership

A runtime owns one object heap. Contexts/realms inside that runtime may share
objects. Runtime, context, and value handles are `!Send + !Sync`; separate
runtimes cannot exchange JavaScript values.

Heap objects use generational typed IDs:

```text
Id<K> {
    runtime_id,
    index,
    generation,
}
```

Every access validates runtime identity, generation, expected kind, and live
state. Internal nodes hold IDs and stored values, never public rooted `Value`
handles or an owning reference back to the runtime.

Public values own root slots. Dropping the last root records a deferred release
without borrowing the heap. Runtime safe points drain those releases after
callbacks and other borrows end.

## Reference counting and cycles

Memory management preserves QuickJS's two layers:

1. Logical strong counts release acyclic objects deterministically. Zero-count
   nodes enter a non-recursive free queue.
2. A trial-deletion pass removes unreachable cycles:
   - copy real counts to trial counts;
   - subtract all internal strong edges;
   - mark candidates reachable from positive trial roots;
   - mark the remainder as zombies before callbacks;
   - detach their outgoing edges;
   - perform weak cleanup and restricted finalization with no arena borrow;
   - release detached edges and reclaim slots.

One exhaustive edge visitor powers counting, trial deletion, destruction, and
debug consistency checks. Weak IDs never contribute to strong counts.

No getter, proxy trap, host function, loader, interrupt callback, promise
tracker, or finalizer runs while an arena/node borrow is live. Finalizers
cannot resurrect zombie nodes.

## Values and objects

- JavaScript strings preserve UTF-16 code units and unpaired surrogates.
  Internal forms may include Latin-1, UTF-16, and bounded ropes.
- Property keys use immediate array indices or interned atoms. Private and
  unique symbols preserve identity.
- Shapes are immutable and transition-interned. Deletion, flag changes, or
  prototype mutation move an object to an uninterned/dictionary shape.
- Property slots are typed as data, accessor, binding cell, or lazy value.
  Slot count, shape entries, flags, and property variants must agree.
- Ordinary locals remain frame slots. Captured, module, mapped-arguments, and
  eval-visible bindings use heap binding cells.
- Suspended generators and async functions own their arguments, locals,
  operand stack, and captured-cell IDs.
- Number parsing/printing, BigInt, Unicode, and RegExp behavior are direct
  QuickJS-derived ports rather than substitutes from another engine.

## Exceptions and failures

The implementation keeps distinct domains:

- `JsAbrupt`: catchable JavaScript throws and internal return/break/continue
  control flow;
- compile diagnostics: stable code, canonical message, severity, source span,
  labels, notes, and help;
- verifier errors: function, bytecode PC, opcode, and violated invariant;
- host errors: I/O, loader, timeout, cancellation, channel, and task failures;
- engine faults: stale/cross-runtime handles or violated internal invariants.

JavaScript throws remain rooted JavaScript values. Compiler, verifier, host, or
engine failures are converted to a JS error only at an explicit API boundary.
Miette is presentation, not semantic truth.

## Sources and maps

The source registry owns display names, text, line indices, and incoming
standard source maps. Oxc byte spans become source-aware spans before its arena
is dropped.

Line lookup supports separate column encodings:

- UTF-8 byte;
- Unicode scalar;
- UTF-16 code unit.

Generated instructions carry a primary origin and may carry a synthetic parent
origin. Peepholes and label relaxation preserve or deliberately merge those
origins. Final bytecode records sorted PC-to-source transitions.

Stack traces and diagnostics resolve:

```text
bytecode PC → generated source span → incoming source-map chain → original
```

Source-map chaining has depth and cycle guards and retains the generated
location as a fallback. QuickJS's compressed `pc2line` representation is
implemented at bytecode serialization boundaries; the runtime may keep richer
span data in memory.

## Tokio host loop

Tokio supplies timers, I/O readiness, task wakeups, bounded channels, and
worker plumbing. It never executes JavaScript or resolves promises directly.

Host tasks return owned `Send` completions identified by operation ID and
generation. The runtime owner consumes them, updates promise capabilities, and
enqueues JavaScript jobs.

One event-loop turn is:

1. process one top-level command, timer, or observed host completion;
2. drain the QuickJS-derived FIFO job queue to a fixed point;
3. drain deferred heap releases;
4. repeat or await one Tokio wakeup.

Completions arriving during a microtask checkpoint remain buffered until that
checkpoint ends. Timer ties use engine-owned creation IDs rather than Tokio
scheduler order. Tests inject completions and a virtual clock.

Standalone execution uses a current-thread Tokio runtime and `LocalSet`.
Embedded execution runs at the top level of a caller-provided `LocalSet` or on
a dedicated engine thread. It never nests `block_on` inside async code.

Cancellation invalidates an operation generation, aborts cancellable host
work, and ignores stale completions. No heap borrow crosses `.await`.

## First vertical slices

The first executable slice evaluates:

```js
let o = {};
o.self = o;
o.answer = 40;
o.answer + 2;
```

It must return integer `42`, produce verified source-mapped bytecode, create
atom/shape transitions, keep the self-cycle alive under immediate RC, and
return live heap counts to baseline after roots are dropped and a cycle pass
runs. A `throw 7` regression verifies stack cleanup and PC-to-source lookup.

The next slice creates a host timer Promise. A Tokio wakeup must be observed on
the runtime owner, the FIFO job queue must drain deterministically, and a
cancelled operation's late completion must be ignored.
