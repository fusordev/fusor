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
- [ ] Implement ECMAScript proper tail calls as verified tail-call transfers: preserve observable call/`this`/argument semantics and resource limits while releasing or reusing the caller frame only in syntactic tail position. This is language semantics, not an optional JIT optimization.
- [x] Global Script evaluation preserves realm `var`, lexical bindings, TDZ, declaration conflicts, and source identity across evaluations.
- [~] Classes: global lexical declarations; named base/derived forms; inheritance (including `extends null`); explicit/synthesized constructors; `super(...)` including spread; public static/instance fields, methods/accessors, computed keys, and static blocks; plus direct/computed `super` reads, calls, writes, and updates. Public methods/accessors are certified non-enumerable class definitions while object-literal methods remain enumerable. Field/static-block lexical `this`, `super`, `new.target`, arrows, source order, and derived receiver timing have focused coverage. Pinned class-element cohorts are 1,932/2,122 expressions and 2,077/2,333 declarations; most residuals belong to `eval` and the remaining diagnostic gaps.
- [~] Private instance/static **data fields**, ordinary/generator/async/async-generator methods, and accessors have fresh opaque names and VM-only own slots; paired private getter/setter declarations share one name. Methods are installed immutably; accessors preserve receiver, reject a missing getter/setter, and never walk prototypes or invoke Proxy traps. Direct, compound, logical, prefix, and postfix private writes retain one receiver/name reference. Direct calls, `super`, bad-receiver `TypeError`, `#name in object`, nested-class capture/shadowing, and invisible/non-configurable slots are covered.
- [ ] Close normative classes with the remaining private/early-error diagnostic audit. Arrow-contained `super()` is supported; proposal decorators remain policy-excluded. Parsing alone is not execution support.
- [ ] Implement direct/indirect `eval`, `with`, remaining opcode families, and complete debug/source tables. `eval` and unverified bytecode remain fail closed; Annex B block-function forms remain rejected.

### Values, objects, and built-ins

- [x] UTF-16 strings; binary64/BigInt/Symbol; canonical keys/conversions; ordinary descriptors/prototypes/integrity; functions/bound construction; arrays/holes; iterator close; global lexical environments; Proxy invariants; reflection; shape/transition interning; and dense indexed storage.
- [ ] Audit remaining exotic and reflection/diagnostic paths as compiler operands become reachable.
- [x] Globals; Object/Reflect; Error families; Boolean/Number/BigInt/Symbol; Array; JSON/Math; String; RegExp; Map/Set/weak collections; Promise; sync and async generators; and a runtime-owned FIFO job queue with resumable continuations.
- [~] `Iterator`, `%Iterator.prototype%`, `Iterator.from`, `Iterator.prototype.toArray`, and shared `%IteratorHelperPrototype%` state for `map`/`filter`/`take`/`drop` implement retained direct records, lazy resumable coercion/callbacks, indexes, reentrancy rejection, GC tracing, and required close/error ordering. Pinned Test262 is 38/38 for `from`, 36/36 for `toArray`, 72/72 for `map`, 74/74 for `filter`, 60/60 admitted for `take`, and 62/62 admitted for `drop`; six modes per limit helper are policy-excluded because they retain the removed `MAX_SAFE_INTEGER` bound. Next add `flatMap` and consuming helpers; keep async helpers separate until job-queue interaction has focused coverage.
- [x] Annex B is intentionally absent: no legacy Object.prototype accessors, String HTML/`substr`/trim aliases, HTML comments, or legacy octal literals/escapes. The normative object-initializer `__proto__` setter emits verified `set_proto`; computed, shorthand, method, and accessor keys remain ordinary definitions.
- [x] Date: TimeClip, normative ISO/local parsing, UTC/local getters and setters, primitive/JSON behavior, and non-Intl locale fallback over the shared `temporal_rs = 0.2.5` kernel.
- [~] Temporal (`temporal_rs = 0.2.5`): `%Temporal.Instant%`, `%Temporal.Duration%`, `Date.prototype.toTemporalInstant`, and branded PlainDate/PlainDateTime/PlainTime cores have constructors, accessors, optioned ISO/JSON/non-Intl output, ordered ISO/string/branded/property-bag conversion, arithmetic, field replacement, rounding/differences, calendar changes, and primitive rejection as applicable. PlainDateTime and PlainTime are complete at 1502/1502 and 962/962 focused cases; PlainDate/PlainDateTime `toZonedDateTime` preserve observable option/property order. Duration is complete at 1080/1080: `ToTemporalRelativeTo` accepts branded values, ISO strings, and property bags, with time-zone bags routed through the resumable ZonedDateTime machinery. Every observable field/options read and coercion uses a rooted continuation; constructor validation fires at each argument's own conversion, arithmetic/from defer overflow validation, and Duration fields are ECMAScript Numbers.
- [~] PlainMonthDay and PlainYearMonth have ordered conversion, accessors, comparison/field replacement, `toPlainDate`, and optioned output; YearMonth adds arithmetic and differences. Their focused upstream cohorts are 398/398 and 1018/1018 respectively, including branded-calendar paths through all admitted Temporal values.
- [x] ZonedDateTime is complete: merged-spec constructor identifier coercion; string/branded/property-bag `from` with all 13 ordered fields, Temporal calendar/time-zone fast paths, lexical `offset` validation, and resumable `disambiguation`/`offset`/`overflow` reads before kernel validation; branded slots/projections; `equals`/static `compare`; JSON/non-Intl and optioned `toString`; `withTimeZone`; `withPlainTime`; transitions; duration `add`/`subtract`; `withCalendar`; partial-field `with` (reject-phase checks, validation before options normalization, default `offset: "prefer"`); time-zone-aware `round`; and `until`/`since` differences over the full ordered options bag, with `ToTemporalRelativeTo` accepting ZonedDateTime for Duration `total`/`compare`. Focused ZonedDateTime is 1796/1796 admitted cases (pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972`; six `Intl.Era-monthcode` modes remain policy-skipped).
- [~] Binary data: fixed/resizable `ArrayBuffer`, fixed/growable `SharedArrayBuffer`, and `DataView` (including Float16/BigInt and resizable-view witnesses) are complete. Typed arrays have dense backing/GC/indexed-exotic rules, the hidden `%TypedArray%` hierarchy, all source constructors/accessors/`@@species`, overlap-safe `set`/`copyWithin`/`slice`/`subarray`, iterators and searches, copying methods, callbacks, reductions, `filter`, and resumable stable numeric `sort`/`toSorted`. `%Atomics%` now covers current non-agent ES semantics: RMW/load on integer views (including ordinary `ArrayBuffer`), `isLockFree`, `pause`, and single-agent `wait`/`notify`; latest focused upstream coverage is 426/426 admitted cases. Next: runtime-owned multi-agent waiter lists/wakeups, `waitAsync`, and Immutable ArrayBuffer: `[[ArrayBufferIsImmutable]]`, `transferToImmutable`/`sliceToImmutable`, immutable view write rejection, and immutable-aware detach/transfer/species paths.
- [ ] ECMA-402 / `quickjs-intl` is deliberately low priority. If resumed, isolate it behind direct ICU4X rather than mixing locale behavior into the runtime core.

### Modules, conformance, and host layers

- [ ] Module linking/evaluation, cycles, resolver semantics, dynamic `import`, and top-level `await`. Parsing a Module is not execution.
- [ ] Embedding API, ESM REPL, `qjs`, Rust-native `qjsc`, bytecode viewer, CDP adapter, and portable `std`/`os` modules.
- [x] The release-mode Test262 runner and manual-dispatch Ubuntu GitHub Actions workflow clone current upstream Test262, apply the custom non-Annex-B policy (with `Temporal` explicitly enabled), use explicitly sized bounded Rayon workers with 64 MiB stacks and a Tokio progress coordinator, cap CI at four case workers to bound memory, enforce an uncatchable five-second deadline per case plus a one-hour CI ceiling, reject impractically slow unbounded debug runs, stream flushed aggregate progress and the live pass rate every 1,000 completed cases, print the revision/full-suite pass rate, and upload a deterministic JSON report.
- [~] Full custom-policy baseline at pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972` and policy fingerprint `208c24ffaa65423a`: 72,818/79,767 admitted cases pass (91.28%), 6,949 fail, and 23,145 are explicitly skipped. Seventeen case modes across nine paths reached the five-second host interrupt and remain accounted for as failures; skips include low-priority ECMA-402 and unsupported host/module/proposal features, so this is a reproducible alignment baseline, not an unfiltered conformance claim.
- [ ] Drive each admitted failure cohort against ECMA-262 and QuickJS/Node, remove temporary skips as features land, and rerun the full configured suite after the language/compiler/module gates close.
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
7. Performance changes require profiles and differential evidence; `unsafe` is never an optimization escape. Proper tail calls have their own language-semantics gate above; multi-agent `Atomics.wait`/`notify` wakeups and `Atomics.waitAsync` are deferred until the runtime-owned shared-memory waiter and timeout model is in place.
