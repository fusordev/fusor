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
- [x] Iterative lowering/execution for the admitted ordinary profile: closures, control flow, calls/spread, destructuring, exceptions, sync/async functions and generators, `yield*`, templates, static/computed/shorthand/spread object data properties and methods/accessors, optional chains, construction, and `new.target`. Object spread uses certified `CopyDataProperties`: nullish sources are no-ops, primitives box, and enumerable string/Symbol own keys preserve source order.
- [~] Backend optimization starts with verifier-preserving symbolic CFG branch threading: ordinary conditional/unconditional branches bypass label-bound `goto` trampolines while retaining every instruction, source mapping, and statement-stack anchor. Constant propagation, unreachable-code excision, inline caches, and guarded intrinsic specialization remain pending.
- [x] Global Script evaluation preserves realm `var`, lexical bindings, TDZ, declaration conflicts, and source identity across evaluations.
- [~] Classes: global declarations now use realm lexical bindings; named base/derived declarations and expressions; inheritance (including `extends null`); explicit/synthesized constructors; `super(...)` including spread; public static/instance fields and methods/accessors; computed field keys; static initialization blocks; and direct/computed `super` reads, calls, simple/compound/logical writes, and updates. Field/static-block lexical `this`, `super`, `new.target`, arrows, source order, and derived receiver timing have focused coverage.
- [~] Private instance/static **data fields**, ordinary/generator/async/async-generator methods, and accessors have fresh opaque names and VM-only own slots; paired private getter/setter declarations share one name. Methods are installed immutably; accessors preserve receiver, reject a missing getter/setter, and never walk prototypes or invoke Proxy traps. Direct, compound, logical, prefix, and postfix private writes retain one receiver/name reference. Direct calls, `super`, bad-receiver `TypeError`, `#name in object`, nested-class capture/shadowing, and invisible/non-configurable slots are covered.
- [ ] Close classes: decorators and the remaining private diagnostic audit. Arrow-contained `super()` is supported. Parsing is not execution support.
- [ ] Implement direct/indirect `eval`, `with`, remaining opcode families, and complete debug/source tables. `eval` and unverified bytecode remain fail closed; Annex B block-function forms remain rejected.

### Values, objects, and built-ins

- [x] UTF-16 strings; binary64/BigInt/Symbol; canonical keys/conversions; ordinary descriptors/prototypes/integrity; functions/bound construction; arrays/holes; iterator close; global lexical environments; Proxy invariants; reflection; shape/transition interning; and dense indexed storage.
- [ ] Audit remaining exotic and reflection/diagnostic paths as compiler operands become reachable.
- [x] Globals; Object/Reflect; Error families; Boolean/Number/BigInt/Symbol; Array; JSON/Math; String; RegExp; Map/Set/weak collections; Promise; sync and async generators; and a runtime-owned FIFO job queue with resumable continuations.
- [x] Annex B is intentionally absent: no legacy Object.prototype accessors, String HTML/`substr`/trim aliases, object-literal `__proto__` mutation, HTML comments, or legacy octal literals/escapes. A static `__proto__` key is an ordinary own property; use `Object.setPrototypeOf` for prototype mutation.
- [x] Date: TimeClip, normative ISO/local parsing, UTC/local getters and setters, primitive/JSON behavior, and non-Intl locale fallback over the shared `temporal_rs = 0.2.5` kernel.
- [~] Temporal: `%Temporal.Instant%`, `%Temporal.Duration%`, `Date.prototype.toTemporalInstant`, and branded `%Temporal.PlainDate%`/`%Temporal.PlainDateTime%` cores share `temporal_rs = 0.2.5`. PlainDate supports ordered conversion, string-only calendar validation, new-target allocation, ISO/string/branded/property-bag `from`/`compare`, calendar-derived accessors, ISO string/JSON/non-Intl locale output, `equals`, date-duration `add`/`subtract`, partial-field `with`, and `until`/`since`. PlainDateTime has its constructor, time defaults, core calendar/date/time accessors, ISO string/JSON/non-Intl locale output, primitive rejection, and ISO/string/branded/PlainDate/property-bag `from`, `compare`, and `equals`. Property bags preserve observable field conversion order; `from`, arithmetic, and `with` validate `overflow` after input conversion, while differences convert the date operand before all four difference settings. PlainDateTime arithmetic/differences/`with`, Calendar/ZonedDateTime, and remaining Temporal types are next.
- [~] Binary data: fixed/resizable `ArrayBuffer`, fixed/growable `SharedArrayBuffer`, and `DataView` (including Float16/BigInt and resizable-view witnesses) are complete. Typed arrays have dense backing/GC/indexed-exotic rules, the hidden `%TypedArray%` hierarchy, all source constructors/accessors/`@@species`, overlap-safe `set`/`copyWithin`/`slice`/`subarray`, iterators and searches, copying methods, callbacks, reductions, `filter`, and resumable stable numeric `sort`/`toSorted`. `%Atomics%` now covers current non-agent ES semantics: RMW/load on integer views (including ordinary `ArrayBuffer`), `isLockFree`, `pause`, and single-agent `wait`/`notify`; latest focused upstream coverage is 426/426 admitted cases. Next: runtime-owned multi-agent waiter lists/wakeups, `waitAsync`, and immutable ArrayBuffer support.
- [ ] ECMA-402 / `quickjs-intl` is deliberately low priority. If resumed, isolate it behind direct ICU4X rather than mixing locale behavior into the runtime core.

### Modules, conformance, and host layers

- [ ] Module linking/evaluation, cycles, resolver semantics, dynamic `import`, and top-level `await`. Parsing a Module is not execution.
- [ ] Embedding API, ESM REPL, `qjs`, Rust-native `qjsc`, bytecode viewer, CDP adapter, and portable `std`/`os` modules.
- [x] `cargo xtask test262` and the manual-dispatch GitHub Actions workflow clone current upstream Test262, apply the custom non-Annex-B policy (with `Temporal` explicitly enabled), use bounded Rayon workers with 64 MiB stacks and a Tokio progress coordinator, stream flushed `--verbose` case completion lines, print the revision/full-suite pass rate, and upload a deterministic JSON report.
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
7. Performance changes require profiles and differential evidence; `unsafe` is never an optimization escape. Proper tail calls remain out of scope; multi-agent `Atomics.wait`/`notify` wakeups and `Atomics.waitAsync` are deferred until the runtime-owned shared-memory waiter and timeout model is in place.
