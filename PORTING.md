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
- [x] Date: TimeClip, normative ISO/local parsing, UTC/local getters and setters, and primitive/JSON behavior over the shared `temporal_rs = 0.2.5` kernel. Its three locale-string methods share the `Intl.DateTimeFormat` option and formatting path.
- [x] Temporal (`temporal_rs = 0.2.5`) is complete for the admitted non-Intl runtime scope: the namespace and `Temporal.Now`; Duration, Instant, PlainDate, PlainDateTime, PlainTime, PlainMonthDay, PlainYearMonth, and ZonedDateTime; and `Date.prototype.toTemporalInstant`. Constructors, accessors, branded/string/property-bag conversion, arithmetic, comparison, field replacement, rounding/differences, calendar/time-zone projection, optioned ISO/JSON output, and primitive rejection preserve the applicable ECMA-262 ordering and validation boundaries. Observable reads and coercions use rooted resumable continuations; `Temporal.Now` uses the host UTC clock and system time zone while explicit time-zone arguments share the normative slot-value conversion path.
- [x] Pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972` passes all 9,116 admitted Temporal cases across 4,603 files with zero runner errors. The 90 policy skips are explicit: 70 baseline exclusions and 20 `Intl.Era-monthcode` modes. Focused completion evidence includes Duration 1080/1080, PlainDateTime 1502/1502, PlainTime 962/962, PlainMonthDay 398/398, PlainYearMonth 1018/1018, ZonedDateTime 1796/1796, `Temporal.Now` 132/132, and namespace metadata 6/6.
- [~] Binary data: fixed/resizable `ArrayBuffer`, fixed/growable `SharedArrayBuffer`, and `DataView` (including Float16/BigInt and resizable-view witnesses) are complete. Typed arrays have dense backing/GC/indexed-exotic rules, the hidden `%TypedArray%` hierarchy, all source constructors/accessors/`@@species`, overlap-safe `set`/`copyWithin`/`slice`/`subarray`, iterators and searches, copying methods, callbacks, reductions, `filter`, and resumable stable numeric `sort`/`toSorted`. `%Atomics%` now covers current non-agent ES semantics: RMW/load on integer views (including ordinary `ArrayBuffer`), `isLockFree`, `pause`, and single-agent `wait`/`notify`; latest focused upstream coverage is 426/426 admitted cases. Next: runtime-owned multi-agent waiter lists/wakeups, `waitAsync`, and Immutable ArrayBuffer: `[[ArrayBufferIsImmutable]]`, `transferToImmutable`/`sliceToImmutable`, immutable view write rejection, and immutable-aware detach/transfer/species paths.
- [~] ECMA-402 is isolated behind `quickjs-intl` with direct ICU4X 2.2 data; JavaScript coercion and observable property order remain runtime-owned. The pinned Intl-only baseline admits 6,692 modes across 3,357 files: 3,256 pass, 3,436 fail, 22 host-API modes skip, and zero runner errors.
- [x] `%Intl%` and `Intl.getCanonicalLocales` implement resumable `CanonicalizeLocaleList`, UTS #35 aliases/extensions, ordered deduplication, Locale-slot bypass, and standard descriptors. Pinned `intl402/Intl/getCanonicalLocales` is 76/76.
- [x] `%Intl.Locale%` covers the constructor's observable coercion/option order and subclass prototype selection; canonical slots/accessors; `maximize`/`minimize`; and the seven Locale-info methods. ICU4X supplies canonicalization, likely subtags, script direction, and week data; pinned CLDR 48 tables supply calendar/hour-cycle preferences with `rg`/region/`sd`/likely-region priority. Pinned `intl402/Locale` is 334/334 admitted modes across 168 files, with two host-API modes explicitly skipped and zero runner errors.
- [x] `%Intl.Collator%` covers callable/subclass construction, ordered locale and option coercion, Unicode `co`/`kf`/`kn` resolution, supported locales, exact resolved slots, and cached bound comparison functions backed by ICU4X tailoring, sensitivity, numeric ordering, case order, and punctuation handling. Pinned `intl402/Collator` is 128/128 across 65 files, with two host-API skips and zero runner errors.
- [x] `%Intl.NumberFormat%` covers callable/subclass and legacy-chain construction; ordered option coercion; Unicode numbering systems; decimal, percent, currency, and unit styles; exact Number/BigInt/string mathematical values; all rounding modes, priorities, and increments; standard, scientific, engineering, and compact notation; bound formatting, parts, ranges, and resolved slots. Number/BigInt locale strings delegate to it, and Array locale strings forward both Intl arguments. Pinned `intl402/NumberFormat` is 496/496 across 249 files, with two host-API skips and zero runner errors.
- [x] `%Intl.DateTimeFormat%` covers callable/subclass and legacy-chain construction; normative option order; Unicode calendar, hour-cycle, and numbering-system resolution; system, named, and offset time zones; styles and sparse components; bound formatting, parts, ranges, and resolved slots; Temporal-kind defaults and extreme proleptic years; and all three Date locale services. Pinned `intl402/DateTimeFormat` admits 486 modes across 244 files with two host-API skips and zero runner errors: 484/486 pass the standard five-second cap, and the exhaustive Temporal comparison's two modes pass 2/2 under a focused 60-second cap.
- [x] `%Intl.PluralRules%` covers subclass construction; normative locale/option coercion; shared exact-number digit rounding and notation operands; cardinal and ordinal selection; range-category resolution; supported locales; and fresh, ordered resolved categories. ICU4X supplies CLDR rules and ranges, with the published Manx cardinal rules filling its compiled-data omission. Pinned `intl402/PluralRules` is 104/104 admitted modes across 53 files, with two host-API skips and zero runner errors.
- [~] `Intl.supportedValuesOf` implements resumable key coercion, fresh Realm arrays, aligned Collator inventories, mandated calendar/numbering-system/unit inventories, and canonical ICU4X time-zone enumeration. Its pinned cohort is 44/50 with zero runner errors; the six residual modes await DisplayNames or RelativeTimeFormat.

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
