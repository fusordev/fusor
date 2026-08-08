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
- [x] Registered sources resolve UTF-8 spans through bounded, cycle-checked source-map v3 chains with generated fallbacks. `evaluate_registered_script` preserves source identity across frontend, compiler, install, and runtime failures; stable diagnostics render through Miette, with verified JavaScript caller frames as independently sourced related diagnostics and exact source-text validation before mapping.

### Compiler, bytecode, and execution

- [x] Typed opcode metadata, bounded codec/disassembly, resource certificates, total decoding, and whole-child-graph verification. Raw or serialized bytecode cannot execute; generator terminals may abandon only verifier-accounted expression state retained across suspension.
- [x] Iterative lowering/execution for the admitted ordinary profile: closures, control flow, calls/spread, destructuring, exceptions, sync/async functions and generators, `yield*`, templates, static/computed/shorthand/spread object data properties and methods/accessors, optional chains, construction, and `new.target`. Object spread uses certified `CopyDataProperties`: nullish sources are no-ops, primitives box, and enumerable string/Symbol own keys preserve source order.
- [~] Backend optimization starts with verifier-preserving symbolic CFG branch threading: ordinary conditional/unconditional branches bypass label-bound `goto` trampolines while retaining every instruction, source mapping, and statement-stack anchor. Constant propagation, unreachable-code excision, inline caches, and guarded intrinsic specialization remain pending.
- [ ] Implement ECMAScript proper tail calls as verified tail-call transfers: preserve observable call/`this`/argument semantics and resource limits while releasing or reusing the caller frame only in syntactic tail position. This is language semantics, not an optional JIT optimization.
- [x] Global Script evaluation preserves realm `var`, lexical bindings, TDZ, declaration conflicts, and source identity across evaluations.
- [~] Classes: global lexical declarations; named base/derived forms; inheritance (including `extends null`); explicit/synthesized constructors; `super(...)` including spread; public static/instance fields, methods/accessors, computed keys, and static blocks; plus direct/computed `super` reads, calls, writes, and updates. Public methods/accessors are certified non-enumerable class definitions while object-literal methods remain enumerable. Field/static-block lexical `this`, `super`, `new.target`, arrows, source order, and derived receiver timing have focused coverage. Pinned class-element cohorts are 1,946/2,122 expressions and 2,091/2,333 declarations; the admitted expression syntax/early-error subtree is 498/498, while most residuals are `eval`-coupled.
- [~] Private instance/static **data fields**, ordinary/generator/async/async-generator methods, and accessors have fresh opaque names and VM-only own slots; paired private getter/setter declarations share one name. Instance private methods/accessors install before any field initializer; methods are immutable, while accessors preserve receiver, reject a missing getter/setter, and never walk prototypes or invoke Proxy traps. Generator methods retain enclosing array/object-spread state across `yield` and abandon it on abrupt `.return()`; the instance/static private-generator subtrees are 40/40 for each class form. Direct, compound, logical, prefix, and postfix private writes retain one receiver/name reference. Direct calls, `super`, bad-receiver `TypeError`, `#name in object`, nested-class capture/shadowing, and invisible/non-configurable slots are covered.
- [ ] Close the remaining non-`eval` class execution gaps, then the `eval`-coupled class semantics and diagnostic audit. Arrow-contained `super()` is supported; proposal decorators remain policy-excluded. Parsing alone is not execution support.
- [ ] Implement direct/indirect `eval`, `with`, remaining opcode families, and complete debug/source tables. `eval` and unverified bytecode remain fail closed; Annex B block-function forms remain rejected.

### Values, objects, and built-ins

- [x] UTF-16 strings; binary64/BigInt/Symbol; canonical keys/conversions; ordinary descriptors/prototypes/integrity; functions/bound construction; arrays/holes; iterator close; global lexical environments; Proxy invariants; reflection; shape/transition interning; and dense indexed storage.
- [ ] Audit remaining exotic and reflection/diagnostic paths as compiler operands become reachable.
- [x] Globals; Object/Reflect; Error families; Boolean/Number/BigInt/Symbol; Array; JSON/Math; String; RegExp; Map/Set/weak collections; Promise; sync and async generators; and a runtime-owned FIFO job queue with resumable continuations.
- [~] `Iterator`, `Iterator.from`, sync transforms/consumers, `%Symbol.dispose%`, `concat`, `zip`, and `zipKeyed` use retained records, rooted continuations, and normative close/error ordering. `flatMap` closes inner before outer; `concat` captures methods eagerly but opens lazily; joint iteration eagerly acquires inputs/padding, implements shortest/longest/strict, reverse `IteratorCloseAll`, reentrancy, fresh arrays, and null-prototype keyed results. Pinned Test262: `from` 38/38, `toArray` 36/36, `map` 72/72, `filter` 74/74, `take` 60/60, `drop` 62/62, `flatMap` 88/88, `every` 66/66, `find` 64/64, `forEach` 54/54, `reduce` 60/60, `some` 66/66, `%Symbol.dispose%` 12/12, `concat` 64/64, `zip` 76/76 at the standard 10M fuel, and `zipKeyed` 88/88 at 12M (86/88 at 10M; only both modes of the large `basic-longest.js` harness exhaust that generic cap). Explicit-resource-management syntax and six obsolete `MAX_SAFE_INTEGER` limit modes remain policy-skipped; async helpers remain separate pending focused job-queue coverage.
- [x] Annex B is intentionally absent: no legacy Object.prototype accessors, String HTML/`substr`/trim aliases, HTML comments, or legacy octal literals/escapes. The normative object-initializer `__proto__` setter emits verified `set_proto`; computed, shorthand, method, and accessor keys remain ordinary definitions.
- [x] Date: TimeClip, normative ISO/local parsing, UTC/local getters and setters, primitive/JSON behavior, and non-Intl locale fallback over the shared `temporal_rs = 0.2.5` kernel.
- [~] Temporal (`temporal_rs = 0.2.5`): `%Temporal.Instant%`, `%Temporal.Duration%`, `Date.prototype.toTemporalInstant`, and branded PlainDate/PlainDateTime/PlainTime cores have constructors, accessors, optioned ISO/JSON/non-Intl output, ordered ISO/string/branded/property-bag conversion, arithmetic, field replacement, rounding/differences, calendar changes, and primitive rejection as applicable. PlainDate and PlainDateTime `toZonedDateTime` preserve observable option/property order and pass 92/92 and 58/58 focused cases. Every observable field/options read and coercion uses a rooted continuation; arithmetic/from defer overflow validation and Duration fields are ECMAScript Numbers.
- [~] PlainMonthDay and PlainYearMonth have ordered conversion, accessors, comparison/field replacement, `toPlainDate`, and optioned output; YearMonth adds arithmetic and differences. Their focused upstream cohorts are 398/398 and 1018/1018 respectively, including branded-calendar paths through all admitted Temporal values.
- [~] ZonedDateTime has its constructor; full string/branded/property-bag `from`; branded slots/projections; property-bag-capable `equals`/static `compare`; JSON/non-Intl output; optioned `toString`; string-only `withTimeZone`; shared `ToTemporalTime` `withPlainTime`; transitions; and duration `add`/`subtract`. Property bags preserve all 13 ordered fields, Temporal calendar/time-zone fast paths, lexical `offset` validation, then resumable `disambiguation`/`offset`/`overflow` reads before kernel validation. `from` is 178/178; `toString` 124/124; `withTimeZone` 32/32; `withPlainTime` 72/72; transitions 28/28; `add` 86/86; and `subtract` 84/84. Focused ZonedDateTime is 1204/1796 (67.04%, pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972`); constructor coercion, `with`/calendar mutation, differences, rounding, and remaining Temporal types are next.
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
