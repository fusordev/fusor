# Porting roadmap

## Target and boundaries

Port the observable JavaScript and host behavior of [QuickJS 2026-06-04](UPSTREAM.md) to safe, pure Rust: ES2025 Script/Module, explicitly admitted later ECMA-262 features, and the targeted Annex B web-compatibility subset listed below. ECMA-262 is normative; pinned QuickJS is the compatibility/diagnostic target; Oxc is parsing and semantic analysis only. QuickJS and Node are differential oracles, never runtime dependencies.

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

Tooling, inspector, Wasm, N-API, TypeScript erasure, Serde conversion, and general Tokio driving remain optional layers. The runtime uses Tokio `rt`/`sync`/`time` only to deliver finite `Atomics.waitAsync` deadline signals; it retains waiter, Promise, and job ordering. See [ARCHITECTURE.md](ARCHITECTURE.md) for the ownership boundaries.

## Ordered status

Finish frontend/diagnostics and language/compiler/execution before broad Test262 alignment. Checked items have focused regressions.

### Frontend and diagnostics

- [x] Published Oxc `0.142.0` is directly pinned; Script, Module, strict/async-global, and dynamic-Function goals retain owned source, binding, and module records. RegExp literal errors use `quickjs-regexp`.
- [x] The compatibility ledger covers admitted grammar and reachable diagnostics.
- [x] Registered sources resolve UTF-8 spans through bounded, cycle-checked source-map v3 chains with generated fallbacks. `evaluate_registered_script` preserves source identity across frontend, compiler, install, and runtime failures; stable diagnostics render through Miette, with verified JavaScript caller frames as independently sourced related diagnostics and exact source-text validation before mapping.

### Compiler, bytecode, and execution

- [x] Typed opcode metadata, bounded codec/disassembly, resource certificates, total decoding, and whole-child-graph verification. Lexical-environment and object/class/array provenance certificates are isolated verifier modules; limits bound semantic work, not implementation-size snapshots. Raw or serialized bytecode cannot execute; generator terminals may abandon only verifier-accounted expression state retained across suspension.
- [x] Iterative lowering/execution for the admitted ordinary profile: closures, control flow, calls/spread, destructuring, exceptions, sync/async functions and generators, `yield*`, templates, static/computed/shorthand/spread object data properties and methods/accessors, optional chains, construction, and `new.target`. Object spread uses certified `CopyDataProperties`: nullish sources are no-ops, primitives box, and enumerable string/Symbol own keys preserve source order.
- [~] Backend optimization starts with verifier-preserving symbolic CFG branch threading: ordinary conditional/unconditional branches bypass label-bound `goto` trampolines while retaining every instruction, source mapping, and statement-stack anchor. Constant propagation, unreachable-code excision, inline caches, and guarded intrinsic specialization remain pending.
- [ ] Implement ECMAScript proper tail calls as verified tail-call transfers: preserve observable call/`this`/argument semantics and resource limits while releasing or reusing the caller frame only in syntactic tail position. This is language semantics, not an optional JIT optimization.
- [x] Global Script evaluation preserves realm `var`, lexical bindings, TDZ, declaration conflicts, and source identity across evaluations.
- [~] Classes: global lexical declarations; named base/derived forms; inheritance (including `extends null`); explicit/synthesized constructors; `super(...)` including spread; public static/instance fields, methods/accessors, computed keys, and static blocks; plus direct/computed `super` reads, calls, writes, and updates. Computed field keys are retained once in source order before source-ordered static initialization; instance constructors capture only their own keys. Public fields dispatch through the receiver's `[[DefineOwnProperty]]`, including Proxy traps and abrupt completion; public methods/accessors are certified non-enumerable class definitions while object-literal methods remain enumerable. Field-created arrows inherit whole-graph-certified home objects; all arrows under derived constructors share the mutable lexical `this` binding, and repeated direct/arrow `super()` constructs before `BindThisValue` throws. Direct eval now inherits method and derived-constructor environments; eval `super()` is admitted when no instance elements need initialization. Pinned class-element cohorts are 1,954/2,122 expressions and 2,109/2,333 declarations; the admitted expression syntax/early-error subtree is 498/498, while most residuals are `eval`-coupled.
- [~] Private instance/static **data fields**, ordinary/generator/async/async-generator methods, and accessors have fresh opaque names and VM-only own slots; paired private getter/setter declarations share one name. Instance private methods/accessors install before any field initializer; methods are immutable, while accessors preserve receiver, reject missing or duplicate halves, and never walk prototypes or invoke Proxy traps. Reinitializing an existing private element throws. Private reads and calls participate in whole-chain optional short-circuiting without suppressing non-nullish brand checks. Generator methods retain enclosing array/object-spread state across `yield` and abandon it on abrupt `.return()`; the instance/static private-generator subtrees are 40/40 for each class form. Direct, compound, logical, prefix, and postfix private writes retain one receiver/name reference. Direct calls, `super`, bad-receiver `TypeError`, `#name in object`, nested-class capture/shadowing, and invisible/non-configurable slots are covered.
- [ ] Reuse one instance-element initializer body when an arrow or direct eval performs the first `super()`; these paths remain fail closed instead of skipping or duplicating fields. Then close the remaining non-`eval` gaps, `eval`-coupled class semantics, and diagnostic audit. Proposal decorators remain policy-excluded.
- [~] `%eval%` uses distinct whole-graph indirect/direct Script authorities. Identity-checked direct callsites retain caller/source strictness, adjusted lexical scope, ordinary-call fallback, and `GetThisEnvironment` context across nested eval and arrows for `new.target`, `super` properties, and no-instance-element `super()` calls. Sloppy function eval has a per-activation variable environment shared by nested eval and escaping closures; non-simple parameters use certified parameter/callee/body boundaries, including ordinary/arrow `arguments`, body-binding separation, dynamic shadowing of outer captures, and deletable eval-created vars/functions with missing-read/`typeof` behavior. Installation remains atomic with live-cell promotion and rollback; the global branch publishes configurable declarations with conflict checks. Sloppy `with` has capturable object-environment cells with ordered reads, `typeof`, calls/tags, deletes, mutable assignment/update, destructuring/iteration targets, and `var` initializers; reference resolution precedes the RHS and SetMutableBinding rechecks existence. Non-spread direct eval imports ordered active/captured `with` chains, preserves ordinary fallback receivers and `%Symbol.unscopables%`, and keeps them live for nested or escaping eval closures; `apply_eval` execution and immutable static fallbacks remain fail closed. At pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972`, `language/eval-code` passes 400/400 and `language/statements/with` passes 182/182. Strict and sloppy direct eval are both required.

### Values, objects, and built-ins

- [x] UTF-16 strings; binary64/BigInt/Symbol; canonical keys/conversions; ordinary descriptors/prototypes/integrity; functions/bound construction; arrays/holes; iterator close; global lexical environments; Proxy invariants; reflection; shape/transition interning; and dense indexed storage.
- [ ] Audit remaining exotic and reflection/diagnostic paths as compiler operands become reachable.
- [x] Globals; Object/Reflect; Error families; Boolean/Number/BigInt/Symbol; Array; JSON/Math; String; RegExp; Map/Set/weak collections; Promise; sync and async generators; and a runtime-owned FIFO job queue with resumable continuations.
- [~] `Iterator`, `Iterator.from`, sync transforms/consumers, `%Symbol.dispose%`, `concat`, `zip`, and `zipKeyed` use retained records, rooted continuations, and normative close/error ordering. `flatMap` closes inner before outer; `concat` captures methods eagerly but opens lazily; joint iteration eagerly acquires inputs/padding, implements shortest/longest/strict, reverse `IteratorCloseAll`, reentrancy, fresh arrays, and null-prototype keyed results. Pinned Test262: `from` 38/38, `toArray` 36/36, `map` 72/72, `filter` 74/74, `take` 60/60, `drop` 62/62, `flatMap` 88/88, `every` 66/66, `find` 64/64, `forEach` 54/54, `reduce` 60/60, `some` 66/66, `%Symbol.dispose%` 12/12, `concat` 64/64, `zip` 76/76 at the standard 10M fuel, and `zipKeyed` 88/88 at 12M (86/88 at 10M; only both modes of the large `basic-longest.js` harness exhaust that generic cap). Explicit-resource-management syntax and six obsolete `MAX_SAFE_INTEGER` limit modes remain policy-skipped; async helpers remain separate pending focused job-queue coverage.
- [~] Annex B remains excluded except for the requested ecosystem subset, implemented after `eval` and `with`: B.1.2; B.2.2.1, B.2.2.15, B.2.2.16; B.2.3.1-B.2.3.3; B.2.4.1; and B.3. The normative object-initializer `__proto__` setter already emits verified `set_proto`; computed, shorthand, method, and accessor keys remain ordinary definitions.
- [x] Date: TimeClip, normative ISO/local parsing, UTC/local getters and setters, primitive/JSON behavior, and non-Intl locale fallback over the shared `temporal_rs = 0.2.5` kernel.
- [x] Temporal (`temporal_rs = 0.2.5`) is complete for the admitted non-Intl runtime scope: the namespace and `Temporal.Now`; Duration, Instant, PlainDate, PlainDateTime, PlainTime, PlainMonthDay, PlainYearMonth, and ZonedDateTime; and `Date.prototype.toTemporalInstant`. Constructors, accessors, branded/string/property-bag conversion, arithmetic, comparison, field replacement, rounding/differences, calendar/time-zone projection, optioned ISO/JSON output, and primitive rejection preserve the applicable ECMA-262 ordering and validation boundaries. Observable reads and coercions use rooted resumable continuations; `Temporal.Now` uses the host UTC clock and system time zone while explicit time-zone arguments share the normative slot-value conversion path.
- [x] Pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972` passes all 9,116 admitted Temporal cases across 4,603 files with zero runner errors. The 90 policy skips are explicit: 70 baseline exclusions and 20 `Intl.Era-monthcode` modes. Focused completion evidence includes Duration 1080/1080, PlainDateTime 1502/1502, PlainTime 962/962, PlainMonthDay 398/398, PlainYearMonth 1018/1018, ZonedDateTime 1796/1796, `Temporal.Now` 132/132, and namespace metadata 6/6.
- [x] Binary data: fixed/resizable/immutable `ArrayBuffer`, fixed/growable `SharedArrayBuffer`, DataView (Float16/BigInt and resizable witnesses), and the complete typed-array surface. A thread-safe host handle aliases one Shared Data Block across runtimes; bytes, atomic RMW, growth, and FIFO waiter registration/notification share its critical section. Blocking waits use cross-runtime wakeups; `waitAsync` has rooted/limited waiter records, same-agent Promise-order preservation, and lazy Tokio deadline signals settled only by the runtime owner. At pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972`, focused policy runs pass ArrayBuffer 408/408, Atomics 512/512 (including `waitAsync` 68/68 locally admissible; 134 async-host cases remain runner-skipped), all 42 immutable-write modes, integer-indexed `[[Set]]` 86/86 admitted (20 host-API modes policy-skipped), and `%TypedArray%.from` 114/114 plus `of` 54/54.
- [ ] ECMA-402 / `quickjs-intl` is deliberately low priority. If resumed, isolate it behind direct ICU4X rather than mixing locale behavior into the runtime core.

### Modules, conformance, and host layers

- [ ] Module linking/evaluation, cycles, resolver semantics, dynamic `import`, and top-level `await`. Parsing a Module is not execution.
- [ ] Embedding API, ESM REPL, `qjs`, Rust-native `qjsc`, bytecode viewer, CDP adapter, and portable `std`/`os` modules.
- [x] The release-mode Test262 runner and manual-dispatch Ubuntu GitHub Actions workflow clone current upstream Test262, apply the custom non-Annex-B policy (with `Temporal` explicitly enabled), use explicitly sized bounded Rayon workers with 64 MiB stacks and a Tokio progress coordinator, cap CI at four case workers to bound memory, enforce an uncatchable five-second deadline per case plus a one-hour CI ceiling, reject impractically slow unbounded debug runs, stream flushed aggregate progress and the live pass rate every 1,000 completed cases, print the revision/full-suite pass rate, and upload a deterministic JSON report.
- [~] The last full custom-policy baseline at pinned Test262 `be13516fb6441b950ba8a3df97eb34062c186972` used historical policy fingerprint `208c24ffaa65423a`: 72,818/79,767 admitted cases pass (91.28%), 6,949 fail, and 23,145 are explicitly skipped. The current fingerprint is `c634861649544857` after admitting Immutable ArrayBuffer and `Atomics.waitAsync` and excluding the slow RegExp cohorts; these focused tranches deliberately did not rerun the full suite. Seventeen historical case modes across nine paths reached the five-second host interrupt and remain failures; skips include low-priority ECMA-402 and unsupported host/module/proposal features, so this is reproducible alignment evidence, not an unfiltered conformance claim.
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
7. Performance changes require profiles and differential evidence; `unsafe` is never an optimization escape. Proper tail calls retain their language-semantics gate above; shared-memory scheduling must preserve the runtime-owned waiter and Promise-job order.
