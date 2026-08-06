# Porting roadmap

## Target and non-negotiable boundaries

Port the observable JavaScript and host behavior of [QuickJS 2026-06-04](UPSTREAM.md)
to safe, pure Rust: ES2025 Script/Module, Annex B, and explicitly admitted
later ECMA-262 features. ECMA-262 is normative; the pinned QuickJS release is
the compatibility/diagnostic target; Oxc is parsing and semantic analysis only.
QuickJS and Node are differential oracles, never runtime dependencies.

This is a source-level port, not a C API or byte-layout clone. The core links
neither C nor C++. Unsupported source semantics reject before execution; only
whole-graph [`VerifiedBytecode`](BYTECODE_VERIFIER.md) executes. A passing
focused test is evidence for its named behavior, never whole-engine
conformance.

| Crate | Owns |
| --- | --- |
| `quickjs-diagnostics` | sources, stable diagnostics, spans, source maps |
| `quickjs-frontend` | published Oxc records and parsing goals |
| `quickjs-bytecode` | instruction model, codec, verifier, debug data |
| `quickjs-compiler` | iterative Oxc-to-verified-bytecode lowering |
| `quickjs-regexp` | bounded ES RegExp grammar and UTF-16 execution |
| `quickjs-runtime` | values, heap, realms, VM, built-ins, limits |
| `quickjs` | thin embedding/tool facade |

Tooling, Tokio driving, inspector, Wasm, N-API, TypeScript erasure, and Serde
conversion are optional layers, not runtime dependencies. See
[ARCHITECTURE.md](ARCHITECTURE.md) for ownership boundaries.

## Status and ordered work

Complete frontend/diagnostics and the language/compiler/execution gates before
broad Test262 alignment. Checked entries have focused regressions.

### Frontend and diagnostics

- [x] Published Oxc `0.142.0` is directly pinned. Script, Module,
  strict/async-global, and dynamic-Function goals retain owned source,
  binding, and module records; RegExp literal errors use `quickjs-regexp`.
- [x] The compatibility ledger covers admitted grammar and reachable
  diagnostics.
- [ ] Finish chained source maps and the remaining public diagnostic/API audit.

### Compiler, bytecode, and execution

- [x] Typed opcode metadata, bounded codec/disassembly, resource certificates,
  total decoding, and whole-child-graph verification. Raw or serialized
  bytecode does not execute.
- [x] Iterative lowering/execution for the admitted ordinary profile: closures,
  control flow, calls/spread, destructuring, exceptions, sync/async functions
  and generators, `yield*`, templates, optional chains, construction, and
  `new.target`.
- [x] Global Script evaluation preserves realm `var`, lexical bindings, TDZ,
  declaration conflicts, and source identity across evaluations.
- [~] Classes execute named base and derived declarations/expressions; named
  evaluation in the admitted contexts; class-name cells; inheritance (including
  `extends null`); explicit/synthesized constructors; `super(...)` including
  spread; public static/instance fields; computed field keys; methods,
  accessors, static initialization blocks; and direct/computed `super` property
  reads, calls, simple/compound/logical writes, and updates. Field and static
  block lexical `this`, `super`, `new.target`, arrows, source order, and derived
  receiver timing have focused coverage.

  Private **instance data fields** (`#x = initializer`) are now admitted
  end-to-end: each class evaluation creates a fresh opaque private name, the
  constructor and class methods capture that identity, and direct read/simple
  write use VM-only own slots. Those slots are non-enumerable/non-configurable,
  cannot be obtained by string reflection, do not walk prototypes, and do not
  invoke Proxy traps. Missing brands and primitive receivers throw `TypeError`.
- [ ] Class closure: arrow-contained `super()`, private methods/accessors,
  static private elements, `#x in object`, private-element anonymous
  `SetFunctionName`, compound/logical/update private writes, and decorators.
  Oxc parsing these forms is not execution support.
- [ ] Direct/indirect `eval`, `with`, Annex B block functions, remaining opcode
  families, and complete debug/source tables. `eval` and unverified bytecode
  remain fail closed.

### Values, objects, and functions

- [x] UTF-16 strings; binary64/BigInt/Symbol; canonical keys and conversions;
  ordinary descriptors/prototypes/integrity; functions and bound construction;
  arrays/holes; iterator close; and global lexical environments.
- [x] Proxy internal methods/invariants, reflection, shape/transition interning,
  dense indexed storage, and the admitted `Object`/`Reflect` diagnostics.
- [ ] Audit remaining exotic and reflection/diagnostic paths as their compiler
  operands become reachable.

### Built-ins and asynchronous semantics

- [x] Globals; Object/Reflect; Error families; Boolean/Number/BigInt/Symbol;
  Array; JSON/Math; String; RegExp; Map/Set/weak collections; Promise; sync and
  async generators; and a runtime-owned FIFO job queue. Observable operations
  suspend through resumable continuations.
- [x] Date supports TimeClip, normative ISO/local parsing, UTC/local getters
  and setters, primitive/JSON behavior, and non-Intl locale fallback over the
  shared `temporal_rs = 0.2.5` kernel.
- [~] Temporal shares that kernel: `%Temporal.Instant%`,
  `%Temporal.Duration%`, and `Date.prototype.toTemporalInstant` have focused
  coverage. After the current class closure, finish Duration rounding, Instant
  arithmetic/zoned operations, remaining Temporal types, binary data/typed
  arrays, and Atomics.
- [ ] ECMA-402 / `quickjs-intl` is deliberately low priority. If resumed,
  isolate it behind direct ICU4X rather than mixing locale behavior into the
  runtime core.

### Modules, conformance, and host layers

- [ ] Module linking/evaluation, cycles, resolver semantics, dynamic `import`,
  and top-level `await`. Parsing a Module is not execution.
- [ ] Embedding API, ESM REPL, `qjs`, Rust-native `qjsc`, bytecode viewer, CDP
  adapter, and portable `std`/`os` modules.
- [x] `cargo xtask test262` pins Test262
  `5c8206929d81b2d3d727ca6aac56c18358c8d790` and fingerprints release patches,
  configuration, expected errors, mode inventory, and fresh-realm JSON results.
- [ ] Once all preceding language/compiler/module gates close, run Test262 by
  feature cohort; investigate every admitted failure against ECMA-262 and
  QuickJS/Node; remove temporary skips; then run the full configured suite.
- [ ] Establish startup/memory/interpreter/compile benchmarks and complete
  release, resource, cancellation, dependency, and public-API audits.

## Compatibility differences

- `QJS-OXC-001`: Oxc determines RegExp literal boundaries/flags; the owned
  RegExp layer owns grammar, early errors, and execution.
- `QJS-OXC-002`: Oxc accepts one chained-label `continue` target that QuickJS
  rejects; a post-semantic check restores the target-profile error.
- `QJS-OXC-003`: QuickJS overflows near 695 nested parentheses; this frontend
  uses an independent bounded stack because the limit is non-normative.
- `QJS-OXC-004`: QuickJS rejects an instance field named `prototype`; ECMA-262
  reserves it only for static fields, which this port follows.
- `QJS-BIGINT-001`: `BigInt.asUintN` reduces modulo `2**bits` as ECMA-262
  requires, rather than preserving a negative QuickJS input.
- `QJS-PROMISE-001`: hostile synchronous `then` calls share the required
  `[[AlreadyCalled]]` record in `Promise.allSettled`.
- `QJS-ASYNC-GENERATOR-001`: a handled async `yield*` `.return()` preserves a
  thenable value property rather than assimilating it.
- `QJS-MAP-001`: `getOrInsertComputed` rescans and updates a callback-created
  key in place rather than deleting/re-appending it.
- `QJS-STRING-001`: primitive pattern/search/separator values observe inherited
  `@@match`/`@@search`/`@@replace`/`@@split` through `GetMethod`.
- `QJS-TEMPLATE-001`: untagged templates use intrinsic concatenation with
  immediate `ToString`, not observable `String.prototype.concat`.
- `QJS-REGEXP-001`: Unicode-set string disjunction in lookbehind follows
  ECMA-262/Node rather than QuickJS rejection.
- `QJS-REGEXP-002`: `RegExp.escape` retains non-whitespace Unicode scalars
  rather than hex-escaping them.

## Completion gates and engineering rules

During development run only the changed package and focused integration tests;
do not repeatedly run the workspace suite. Before release, a full-conformance
claim, or goal completion, run:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo audit
```

1. Follow ECMA-262 first; document every intentional compatibility difference.
2. Reject unsupported semantics rather than approximating them; execute only
   whole-graph verified bytecode.
3. Keep parser, compiler, verifier, VM, built-in, and host ownership separate.
4. Use explicit worklists and typed continuations; never let Rust recursion,
   locks, or Tokio define JavaScript behavior.
5. Runtime/context/heap/JS handles are thread-affine and `!Send + !Sync`.
6. Keep the core safe; foreign pointers belong only to an audited boundary.
7. Performance changes require profiles and differential evidence; `unsafe` is
   never an optimization escape. Proper tail calls and `Atomics.waitAsync`
   remain out of scope.
