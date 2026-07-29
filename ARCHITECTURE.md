# Architecture

This document records the implementation boundaries and invariants for the
pure-Rust port of QuickJS 2026-06-04. Observable behavior follows the pinned
QuickJS release except for explicitly documented Oxc parser differences.
Rust-native representations and measured optimizations are allowed when
differential tests preserve the selected compatibility behavior.

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

Core engine, compiler, runtime, host, and tool crates forbid `unsafe` Rust.
The optional N-API ABI adapter is the sole planned project exception, limited
to documented foreign-pointer operations. Oxc, Tokio, and other dependencies
are audited separately; workspace linting cannot make transitive claims about
their internals.

## Compilation boundary

The compilation pipeline is:

1. Register the source and any incoming standard source map.
2. Select Script, Module, eval, or Function-constructor grammar explicitly.
3. Parse JavaScript with Oxc. Oxc RegExp pattern parsing stays disabled.
4. Reject every parser diagnostic, including recoverable diagnostics.
5. Run Oxc semantic early-error checks, retain its complete semantic model,
   and reject every diagnostic.
6. Reject syntax accepted by the current Oxc release but outside the pinned
   QuickJS/ES2025 profile.
7. Consume Oxc's retained semantic model directly while building
   QuickJS-owned declaration, storage, and module-linking plans.
8. Lower AST nodes into typed pseudo-instructions with copied source origins.
9. Run QuickJS-derived variable resolution, label relaxation, peepholes, stack
   analysis, and debug-table construction.
10. Verify and freeze the bytecode, then drop the Oxc allocator.

`ParsedUnit` keeps Oxc's arena-backed `Program` and `ModuleRecord` beside the
complete `Semantic` result. The `Program` header is allocated in the caller's
Oxc arena before semantic traversal, so semantic node references remain stable
without a self-referential Rust owner. No Oxc arena reference may survive
compilation. Static Oxc resolution is only an input: `with`, direct eval,
Annex B bindings, and global declaration instantiation require
QuickJS-compatible dynamic handling.

The first compiler-owned lowering result is `StoragePlan`. While the
`ParsedUnit` arena is alive, `quickjs-compiler` queries Oxc `Semantic` directly
for scopes, symbols, declarations, and references. It then freezes only native
dense executable/binding/reference IDs, exact copied spans, and immutable
`Arc`-backed names and slices. Oxc node, scope, and symbol IDs never cross this
boundary, and the Oxc semantic graph is neither cloned nor retained. Every
resolved source reference records its native binding ID, using executable,
copied span, and read/write access; unresolved globals remain a separate native
reference domain. Executable preorder and per-executable source ordering are
deterministic. This initial total slice fails with typed errors for eval,
`with`, non-simple parameters, Annex B block functions, classes, synthetic
function bindings (including Oxc-resolved `arguments` collisions), and
cross-executable captures rather than emitting a partial plan. Namespace
imports remain module-owned declaration cells, named/default imports remain
import cells, and expression or anonymous-function default exports receive a
distinct synthetic module-local `*default*` cell with the appropriate
initialization policy. The asterisks are part of QuickJS's internal atom, so a
source identifier named `_default_` cannot collide with it.

`CompilationContext` keeps the transient Oxc `NodeId → ExecutableId`,
`SymbolId → BindingId`, and `ReferenceId → native reference` tables beside an
`Arc<StoragePlan>` only while the arena is alive. Lowering never reconstructs
identity from names or spans and never treats a unit-global `BindingId` as an
argument or local slot. The first end-to-end ordinary leaf-function family is
Script-only and accepts function declarations and anonymous `function`
expressions. Its pool-free body slice handles simple
`var`/`let`/`const` declarations, TDZ setup, immediate Boolean/null/int32 and
compact `BigInt` values, the empty string, resolved argument/local reads, unary
and binary operators, short-circuit `&&`/`||`/`??`, conditional expressions,
sequence and expression statements, lexical blocks, `if`/`else`, `while`,
`do`/`while`, unlabeled `break`/`continue`, and explicit or implicit returns.
It lowers expressions and statements with iterative work lists, validates the
complete selected body into typed pseudo-instructions before byte emission,
assigns typed frame slots, and immediately produces a non-executable
`VerifiedControlFlow` certificate. Scope entry reads Oxc's creator
`ScopeId` directly, checks its creator `NodeId`, and emits TDZ initialization
only for bindings owned by that exact scope, in reverse local-slot order.
Paired scope work items keep loop re-entry and future captured-local cleanup
explicit without a recursion guard. Constants, atoms, closures, labeled
control, and `for` families stay rejected until their owned records or full
cleanup and per-iteration environment semantics exist.

`BytecodeAssembler` keeps symbolic label handles provenance-bound to one
assembler through immutable `Arc` identity. Labels never enter final operands.
The compiler wraps each handle with its owning Oxc span. Statement labels also
declare an exact empty-stack entry requirement; after final layout and
whole-CFG verification, every reachable statement anchor must have verified
depth zero, while structurally valid unreachable anchors remain accepted.
After whole-body planning, the assembler rejects foreign, duplicate, unbound,
or end-of-stream targets, starts branches at their shortest forms, and
monotonically widens conditionals to 8/32-bit and gotos to 8/16/32-bit
displacements using the QuickJS `opcode_pc + 1` base. This computes the least
valid fixed point even when two branch widths mutually enable their short
forms. Planned instructions and per-pass relaxation visits are bounded before
the assembler encodes once through `BytecodeBuilder`. It returns the relocated
PC of every instruction so source entries are built only after branch widths
are final. The compiler verifier entry independently derives reachable stack
maxima and equal-depth joins and requires reachable terminals to empty the
ordinary value stack; the serialized-bytecode entry separately retains its
exact stored-versus-computed stack-size comparison and QuickJS exit semantics.

Module functions, object methods/accessors, and named function expressions fail
closed until their distinct surrounding-storage, header, and self-binding
behavior is implemented. The compiled artifact keeps the exact source text,
storage plan, local layout, source table, and certificate in immutable `Arc`
storage after the Oxc arena is dropped. Lowering accepts only an opaque
executable selection issued by that context, so a same-index selection from
another context is rejected. Unsupported bodies, unresolved names, captures,
nested executables, and async/generator functions fail before byte emission.

Every successful unit also owns a `ModuleSyntaxRecord` for the module data that
must survive the Oxc allocator. Static requests remain in source occurrence
order, so repeated specifiers retain distinct typed indices, literal spans, and
per-occurrence import attributes. Import and local/indirect/star export entries
retain their linking roles and actual export-site spans. Decoded module strings
use immutable `Arc<[u16]>` backing so lone surrogates accepted by QuickJS are
not collapsed into Rust replacement characters. This record deliberately does
not copy Oxc scopes, symbols, references, or class semantics.

Oxc acceptance, rejection, or diagnostic wording may intentionally differ from
the pinned QuickJS parser. Every accepted difference must be narrow,
documented, and regression-tested; it must preserve byte-accurate source
mapping and must not silently substitute different runtime semantics.
TypeScript, JSX, and alternate JavaScript-runtime behavior remain outside the
default parser profile.

Dynamic Function constructors use a dedicated adapter rather than parsing a
naked body. It joins separately coerced parameter fragments with the exact
QuickJS wrapper punctuation, parses the complete generated Script with Oxc,
and retains a byte-exact synthetic/copied fragment map on success and on any
parser, profile, or semantic failure after preparation. Preflight resource
failures allocate no wrapper. The compatibility release permits source to
escape that wrapper, so the adapter deliberately does not require the Script
AST to contain exactly one function expression.

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

The current staged implementation exposes only `VerifiedControlFlow`: a
non-executable certificate for complete predecode, instruction boundaries,
validated execution-header bits and counts, function-local operand bounds,
secondary operand domains, static successors, suspension and return
function-kind compatibility, and reachable ordinary-value stack heights.
Opcodes that require typed constants, raw function slots, handlers, finally
return addresses, iterator markers, or packed stack offsets fail closed. The VM
boundary continues to require the future whole-function `VerifiedBytecode`.
The symbolic assembler chooses the componentwise shortest valid final branch
layout. This can differ from a conservative QuickJS peephole boundary while
preserving the same signed displacement rules and JavaScript behavior.

The complete trust boundary, typed abstract stack, control-flow rules,
resource limits, and acceptance suite are normative in
[BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md).

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

Immutable string leaves and rope nodes use standard-library `Arc` ownership.
This keeps immutable `JsString` handles safe to move through host integration
queues without making a JavaScript runtime, context, or heap transferable
between threads. A backing node is destroyed synchronously when its last strong
handle is dropped. String-node destruction cannot run JavaScript and cannot
determine object liveness, finalizer timing, weak visibility, or cycle removal.
A backing allocation ledger must charge bytes when a node is created and
release them from that node's destruction path; dropping a public value root
is not by itself proof that shared string storage was reclaimed.

Shared mutable host-side state uses `parking_lot` locks. Locks must never guard
or make cross-thread access possible for a JavaScript runtime, context, heap, or
value handle, and immutable `Arc<Repr>` string backing has no lock.

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
- Each runtime owns one `AtomTable`. Owning `Atom` handles use identity
  equality and carry a weak owner marker, so operations reject foreign and
  orphaned identities. String atoms and global-symbol-registry entries are
  content-interned in separate randomized namespaces; unique symbols and
  private names are never content-interned.
- Interner buckets contain weak entry handles rather than string keys. A miss
  copies UTF-16 contents into a compact string so a short atom cannot retain an
  input rope. Dead slots remain charged until the touched bucket or an explicit
  bounded sweep removes them.
- Runtime startup installs the pinned release's 242 predefined identities in
  exact order: 228 strings, one private brand, and 13 well-known symbols.
- Property keys use immediate canonical array indices or table-validated public
  string/symbol atoms. Private names have a separate identity namespace and
  cannot be constructed as public property keys.
- Incomplete property descriptors retain independent presence for `value`,
  `writable`, `get`, `set`, `enumerable`, and `configurable`. Classification
  produces an opaque generic/data/accessor descriptor, so callers cannot forge
  a kind contradicted by its present fields. Completion in this foundation is
  explicitly limited to creation of a new ordinary property; accessor
  callability and existing-property compatibility remain later object-model
  checks.
- Bytecode never stores an `Atom` pointer or runtime identity. Atom operands
  are validated function-local pool indices; serialized units carry bounded
  atom contents and namespace metadata, and loading reinterns or creates each
  pool entry exactly once in the destination runtime.
- Shapes are immutable and transition-interned. Deletion, flag changes, or
  prototype mutation move an object to an uninterned/dictionary shape.
- Ordinary data/accessor layouts have an opaque value-independent
  representation; accessor layouts cannot carry a writable flag. Future
  property slots are typed as data, accessor, binding cell, or lazy value.
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

The leaf compiler maps instruction-local verifier failures back through its
strictly ordered final-PC table with exact `BytecodePc` lookup. It never treats
a byte offset as an instruction ordinal. Join-depth failures additionally map
the target PC as a related span; function-level verifier failures with no PC do
not fabricate an instruction span.

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
