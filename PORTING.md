# Porting roadmap

## Target and boundaries

Port the observable JavaScript and host behavior of [QuickJS 2026-06-04](UPSTREAM.md) to safe, pure Rust: ES2025 Script/Module and explicitly admitted later ECMA-262 features, deliberately excluding Annex B compatibility. ECMA-262 is normative; pinned QuickJS is the compatibility/diagnostic target; Oxc is parsing and semantic analysis only. QuickJS and Node are differential oracles, never runtime dependencies.

This is a source-level port, not a C API or byte-layout clone. The core links neither C nor C++. Unsupported source semantics reject before execution, and only whole-graph [`VerifiedBytecode`](BYTECODE_VERIFIER.md) executes. Focused tests prove only their named behavior.

| Crate | Owns |
| --- | --- |
| `quickjs-diagnostics` | sources, diagnostics, spans, source maps |
| `quickjs-frontend` | published Oxc records and parsing goals |
| `quickjs-bytecode` | instructions, codec, verifier, debug data |
| `quickjs-compiler` | iterative Oxc-to-verified-bytecode lowering |
| `quickjs-regexp` | bounded ES RegExp grammar and UTF-16 execution |
| `quickjs-runtime` | values, heap, realms, VM, built-ins, limits |
| `quickjs` | embedding and tool facade |

Tooling, inspector, Wasm, N-API, TypeScript erasure, Serde conversion, and Tokio driving are optional layers, not runtime dependencies. See [ARCHITECTURE.md](ARCHITECTURE.md) for the ownership boundaries.

## Ordered status

Finish frontend/diagnostics and language/compiler/execution before broad Test262 alignment. Checked items have focused regressions.

### Frontend and diagnostics

- [x] Published Oxc `0.142.0` is directly pinned; Script, Module, strict/async-global, and dynamic-Function goals retain owned source, binding, and module records. RegExp literal errors use `quickjs-regexp`.
- [x] The compatibility ledger covers admitted grammar and reachable diagnostics.
- [ ] Complete chained source maps and the public diagnostic/API audit.

### Compiler, bytecode, and execution

- [x] Typed opcode metadata, bounded codec/disassembly, resource certificates, total decoding, and whole-child-graph verification. Raw or serialized bytecode cannot execute.
- [x] Iterative lowering/execution for the admitted ordinary profile: closures, control flow, calls/spread, destructuring, exceptions, sync/async functions and generators, `yield*`, templates, optional chains, construction, and `new.target`.
- [x] Global Script evaluation preserves realm `var`, lexical bindings, TDZ, declaration conflicts, and source identity across evaluations.
- [~] Classes: named base/derived declarations and expressions; inheritance (including `extends null`); explicit/synthesized constructors; `super(...)` including spread; public static/instance fields and methods/accessors; computed field keys; static initialization blocks; and direct/computed `super` reads, calls, simple/compound/logical writes, and updates. Field/static-block lexical `this`, `super`, `new.target`, arrows, source order, and derived receiver timing have focused coverage.
- [~] Private **instance data fields** and ordinary private methods have fresh opaque names and VM-only own slots. A private method closure is created once per class evaluation, receives `#name` and the class prototype as its home object, and is installed on each instance; direct calls, `super`, bad-receiver `TypeError`, and `#name in object` are covered. Slots are non-enumerable/non-configurable, invisible to string reflection, do not walk prototypes, and do not invoke Proxy traps.
- [ ] Close classes: private accessors and static private elements; compound/logical/update private writes; decorators; and the remaining private-function naming/diagnostic audit. Arrow-contained `super()` is supported. Parsing is not execution support.
- [ ] Implement direct/indirect `eval`, `with`, remaining opcode families, and complete debug/source tables. `eval` and unverified bytecode remain fail closed; Annex B block-function forms remain rejected.

### Values, objects, and built-ins

- [x] UTF-16 strings; binary64/BigInt/Symbol; canonical keys/conversions; ordinary descriptors/prototypes/integrity; functions/bound construction; arrays/holes; iterator close; global lexical environments; Proxy invariants; reflection; shape/transition interning; and dense indexed storage.
- [ ] Audit remaining exotic and reflection/diagnostic paths as compiler operands become reachable.
- [x] Globals; Object/Reflect; Error families; Boolean/Number/BigInt/Symbol; Array; JSON/Math; String; RegExp; Map/Set/weak collections; Promise; sync and async generators; and a runtime-owned FIFO job queue with resumable continuations.
- [x] Annex B is intentionally absent: no legacy Object.prototype accessors, String HTML/`substr`/trim aliases, object-literal `__proto__` mutation, HTML comments, or legacy octal literals/escapes. A static `__proto__` key is an ordinary own property; use `Object.setPrototypeOf` for prototype mutation.
- [x] Date: TimeClip, normative ISO/local parsing, UTC/local getters and setters, primitive/JSON behavior, and non-Intl locale fallback over the shared `temporal_rs = 0.2.5` kernel.
- [~] Temporal: `%Temporal.Instant%`, `%Temporal.Duration%`, `Date.prototype.toTemporalInstant`, Instant arithmetic/difference/rounding/string formatting, and Duration rounding/string formatting share `temporal_rs = 0.2.5`. Instant difference converts `other` before object-only options and preserves `largestUnit` → `roundingIncrement` → `roundingMode` → `smallestUnit`; Instant and Duration stringification preserve `fractionalSecondDigits` → `roundingMode` → `smallestUnit`, and Instant then reads `timeZone` for compiled IANA/fixed-offset formatting. Next: Zoned operations and remaining Temporal types.
- [~] Binary data: fixed/resizable `ArrayBuffer`, transfer/slice/resize, and full `DataView` construction, resizable-view witnesses, Float16, Number, and BigInt element access are complete. Next: typed-array integer-indexed exotics and constructors, then `SharedArrayBuffer` and Atomics.
- [ ] ECMA-402 / `quickjs-intl` is deliberately low priority. If resumed, isolate it behind direct ICU4X rather than mixing locale behavior into the runtime core.

### Modules, conformance, and host layers

- [ ] Module linking/evaluation, cycles, resolver semantics, dynamic `import`, and top-level `await`. Parsing a Module is not execution.
- [ ] Embedding API, ESM REPL, `qjs`, Rust-native `qjsc`, bytecode viewer, CDP adapter, and portable `std`/`os` modules.
- [x] `cargo xtask test262` and the manual-dispatch GitHub Actions workflow pin Test262 `5c8206929d81b2d3d727ca6aac56c18358c8d790`, apply the exact QuickJS baseline patch, run the full configured non-Annex-B suite (including Temporal), print its pass rate, and upload the deterministic JSON report.
- [ ] After the preceding language/compiler/module gates close, run Test262 by feature cohort; investigate every admitted failure against ECMA-262 and QuickJS/Node; remove temporary skips; then run the full configured suite.
- [ ] Establish startup/memory/interpreter/compile benchmarks and finish release, resource, cancellation, dependency, and public-API audits.

## Compatibility differences

| ID | Intentional behavior |
| --- | --- |
| `QJS-OXC-001` | Oxc determines RegExp literal boundaries/flags; the owned RegExp layer owns grammar, early errors, and execution. |
| `QJS-OXC-002` | A post-semantic check rejects one chained-label `continue` target accepted by Oxc but rejected by QuickJS. |
| `QJS-OXC-003` | This frontend uses an independent bounded nesting stack; QuickJS's near-695-parenthesis overflow is non-normative. |
| `QJS-OXC-004` | Instance field `prototype` follows ECMA-262; QuickJS rejects it although only static fields reserve it. |
| `QJS-BIGINT-001` | `BigInt.asUintN` reduces modulo `2**bits`, including negative inputs. |
| `QJS-PROMISE-001` | Hostile synchronous `then` calls share the required `[[AlreadyCalled]]` record in `Promise.allSettled`. |
| `QJS-ASYNC-GENERATOR-001` | Handled async `yield*` `.return()` preserves a thenable value property rather than assimilating it. |
| `QJS-MAP-001` | `getOrInsertComputed` rescans and updates a callback-created key in place rather than deleting/re-appending it. |
| `QJS-STRING-001` | Primitive pattern/search/separator values use inherited `@@match`/`@@search`/`@@replace`/`@@split` through `GetMethod`. |
| `QJS-TEMPLATE-001` | Untagged templates use intrinsic concatenation with immediate `ToString`, not observable `String.prototype.concat`. |
| `QJS-REGEXP-001` | Unicode-set string disjunction in lookbehind follows ECMA-262/Node rather than QuickJS rejection. |
| `QJS-REGEXP-002` | `RegExp.escape` retains non-whitespace Unicode scalars rather than hex-escaping them. |

## Completion gates and engineering rules

During development, run only changed-package and focused integration tests; do not repeatedly run the workspace suite. Before release, a full-conformance claim, or goal completion, run:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo audit
```

1. Follow ECMA-262 first; document intentional differences.
2. Reject unsupported semantics rather than approximating them; execute only whole-graph verified bytecode.
3. Keep parser, compiler, verifier, VM, built-in, and host ownership separate.
4. Use explicit worklists and typed continuations; Rust recursion, locks, and Tokio must not define JavaScript behavior.
5. Runtime/context/heap/JS handles are thread-affine and `!Send + !Sync`.
6. Keep the core safe; foreign pointers belong only to an audited boundary.
7. Performance changes require profiles and differential evidence; `unsafe` is never an optimization escape. Proper tail calls and `Atomics.waitAsync` remain out of scope.
