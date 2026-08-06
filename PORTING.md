# Porting roadmap

## Contract

Port the observable JavaScript and host behavior of [QuickJS 2026-06-04](UPSTREAM.md)
to safe, pure Rust. The target is its ES2025 Script/Module profile, including
Annex B, plus later ECMA-262 features that are explicitly admitted and tested.

Authority, in order:

1. ECMA-262 (and the named standard for each non-JavaScript surface) is
   normative.
2. The pinned QuickJS release defines the compatibility target, host surface,
   and diagnostic reference. A spec-first intentional difference gets a stable
   ID and regression below.
3. Oxc is parser/semantic analysis only; QuickJS and Node are differential
   oracles, never runtime dependencies.

This is a source-level port, not a C API or byte-layout clone. Rust callers get
a lifetime-safe API; optional foreign ABI support stays isolated. Unsupported
semantics reject before execution. The core links neither C nor C++.

## Architecture

| Crate | Responsibility |
| --- | --- |
| `quickjs-diagnostics` | Sources, stable diagnostics, spans, source maps |
| `quickjs-frontend` | Published Oxc parsing/semantics and owned records |
| `quickjs-bytecode` | Instructions, codec, verifier, disassembly, atoms/debug data |
| `quickjs-compiler` | Iterative Oxc lowering to verified bytecode |
| `quickjs-regexp` | Bounded ES RegExp grammar and UTF-16 execution |
| `quickjs-runtime` | Values, heap, realms, VM, built-ins, limits, interrupts |
| `quickjs` | Facade for thin tools |

Tooling (`xtask`, fuzzers, benchmarks, pinned oracles) is never production
code. Tokio host driving, inspector, Wasm, N-API, TypeScript erasure, and Serde
conversion are optional layers. See [ARCHITECTURE.md](ARCHITECTURE.md) and
[BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md) for ownership and admission
boundaries.

## Status and order of work

A checked item has focused regressions; it never claims whole-engine
conformance. **Complete frontend/diagnostics and language/compiler gates before
broad Test262.**

### Frontend and diagnostics

- [x] Published Oxc `0.142.0` is pinned directly. Script, Module,
  strict/async-global, and dynamic-Function goals retain owned source, binding,
  and module records; RegExp literal errors use `quickjs-regexp`.
- [x] The compatibility ledger covers admitted grammar and reachable
  diagnostics.
- [ ] Finish chained source maps plus the public diagnostic/API audit.

### Compiler, bytecode, and execution

- [x] Typed opcode metadata, checked codec/disassembly, bounded construction,
  total decoding, resource certificates, and whole-child-graph verification.
  Only `VerifiedBytecode` executes.
- [x] Iterative lowering/execution for the admitted ordinary profile:
  closures, control flow, calls/spread, destructuring, exceptions, sync/async
  functions and generators, `yield*`, templates, optional chains, and ordinary
  construction/`new.target`.
- [x] Global Script evaluation preserves realm `var`, lexical bindings, TDZ,
  declaration conflicts, and source identity across evaluations.
- [~] Classes: named base declarations/expressions, simple-binding anonymous
  base expressions (including harmless parentheses), direct identifier
  assignments (including `||=`, `&&=`, and `??=`), and uncomputed
  object-property expressions and binding-pattern defaults with an explicit or
  synthesized default constructor and public static/instance methods or
  accessors execute through typed `define_class`. Anonymous base classes also
  accept computed object-property or static-field names through a
  non-escaping typed `define_class`/`set_name_computed` sequence.
  Other ordinary expression contexts and static/computed member assignments
  use a typed empty class-name atom; only NamedEvaluation contexts infer a
  name.
  Member closures capture the spec-required immutable inner class-name cell,
  distinct from the mutable declaration binding. Constructors are strict,
  construct with the installed class prototype, and reject direct calls.
  Derived classes support `extends` (including `null`): heritage evaluates
  once, checks constructability before reading `.prototype`, and installs both
  inheritance links. Synthesized defaults forward every supplied argument;
  source-written derived constructors admit direct non-spread `super(...)` in
  their own body through a typed active-constructor/superclass/new-target
  capability. Synchronous class constructors and methods retain
  `[[HomeObject]]` and support direct static/computed `super` property reads,
  calls, and simple assignments, preserving the actual `this` for inherited
  getters, setters, and method calls. The receiver initializes only after
  superclass construction; early or repeated `super()` and `this` before
  `super()` throw `ReferenceError`; derived returns accept an object or
  `undefined` only.
  The certificate accepts literal `undefined` for base classes, the
  exact derived-heritage branch, and non-escaping typed constructor templates.
  Source writes to the captured class name reach a typed runtime `TypeError`
  without changing that cell. Public computed method/accessor keys use the
  same typed definition path; ordinary, generator, async, and async-generator
  method templates are certified by their defining class element. Uncomputed
  public static fields, and computed static fields with non-class values,
  evaluate into own constructor properties when their initializers contain no
  `this`, `super`, or `new.target`.
- [ ] Class closure: `super` properties in async/generator or object-literal
  methods; compound/logical assignments and updates; spread or arrow-contained
  `super()`; instance fields; computed-key anonymous-class
  name inference outside direct object/static-field definitions, static-field
  initializers using `this`, `super`, or `new.target`, private elements,
  decorators, and static blocks. Do not relabel these as
  supported merely because Oxc parses them.
- [ ] Direct/indirect `eval`, `with`, Annex B block functions, remaining opcode
  families, and complete debug/source tables. Unverified or serialized bytecode
  and unsupported `eval` remain fail closed.

### Values, objects, and functions

- [x] UTF-16 strings, binary64/BigInt/Symbol values, canonical keys,
  conversions/operators, ordinary descriptors, prototypes, integrity, functions
  and bound construction, arrays/holes, iterator closing, and global lexical
  environments.
- [x] Proxy internal methods/invariants, reflection, shape/transition interning,
  dense indexed storage, and complete admitted `Object`/`Reflect` diagnostics.
- [ ] Finish remaining exotic and reflection/diagnostic audit paths as their
  compiler operands become reachable.

### Built-ins and asynchronous semantics

- [x] Globals; Object/Reflect; Error families; Boolean/Number/BigInt/Symbol;
  Array; JSON/Math; String; RegExp; Map/Set/weak collections; Promise; sync and
  async generators; and a runtime-owned FIFO job queue. Observable operations
  use resumable continuations where host/property access can suspend.
- [x] Date supports TimeClip, normative ISO/local parsing, UTC/local getters and
  setters, primitive/JSON behavior, and non-Intl locale fallback over
  `temporal_rs = 0.2.5`.
- [~] Temporal shares that kernel: `%Temporal.Instant%`,
  `%Temporal.Duration%`, and `Date.prototype.toTemporalInstant` have focused
  coverage. Continue only after class language closure; then finish Duration
  rounding, Instant arithmetic/zoned operations, remaining Temporal types,
  binary data/typed arrays, and Atomics.
- [ ] ECMA-402 is deliberately low priority. If implemented, isolate it in
  `quickjs-intl` over direct ICU4X; keep observable algorithms in the runtime.

### Modules, conformance, and host layers

- [ ] Implement module linking/evaluation, cycles, resolver semantics, dynamic
  import, and top-level `await`; parsing a Module is not execution.
- [ ] Finish embedding API, ESM REPL, `qjs`, Rust-native `qjsc`, bytecode
  viewer, CDP adapter, and portable `std`/`os` modules.
- [x] `cargo xtask test262` is pinned to Test262
  `5c8206929d81b2d3d727ca6aac56c18358c8d790` and fingerprints release patches,
  configuration, expected errors, mode inventory, and fresh-realm JSON results.
- [ ] After every preceding language/compiler/module gate is closed, run Test262
  by feature cohort, investigate every admitted failure against ECMA-262 and
  QuickJS/Node, remove temporary skips, then run the full configured suite.
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
- `QJS-BIGINT-001`: `BigInt.asUintN` is reduced modulo `2**bits`, as required by
  ECMA-262, rather than preserving a negative QuickJS input.
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

## Completion gates

Every semantic change starts with a focused regression and, where useful, a
pinned QuickJS/Node differential case. During development, run only the changed
package and relevant integration tests; do **not** repeatedly run the workspace
suite. Before a release, a full-conformance claim, or goal completion, run:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo audit
```

Passing a bounded differential corpus proves only its declared manifest. Full
engine completion additionally requires the language/module gates above and
Test262 evidence; no focused green suite is a substitute.

## Engineering rules

1. Follow ECMA-262 first; document every intentional compatibility difference.
2. Reject unsupported semantics rather than approximating them. Execute only
   whole-graph verified bytecode.
3. Keep parser, compiler, verifier, VM, built-in, and host ownership separate;
   use validated newtypes for operands and heap handles.
4. Use explicit worklists and typed continuations for recursive/suspendable
   algorithms. Never let Rust recursion, locks, or Tokio define JS semantics.
5. Runtime/context/heap/JS handles remain thread-affine and `!Send + !Sync`.
6. Keep the core safe; foreign pointers belong only to an audited boundary.
7. Performance work needs profiles and preserves observable behavior under
   differential tests; `unsafe` is never an optimization escape.
8. Proper tail calls and `Atomics.waitAsync` remain out of scope.
