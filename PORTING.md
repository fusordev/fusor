# Porting plan

## Target and boundaries

Port [QuickJS 2026-06-04](UPSTREAM.md) to safe, pure Rust, targeting its
ES2025 script/module semantics (including Annex B), embeddable runtime model,
deterministic destruction with cycle removal, bytecode execution, standard
built-ins, modules, jobs, and the documented `std`/`os` host surface.

The project does **not** reproduce QuickJS binary layout or C API. Rust callers
get an idiomatic lifetime-safe API; an optional, isolated N-API adapter may
provide a C ABI. Behavior that cannot yet be reproduced safely must be
documented, tested, and fail closed.

QuickJS is the sole runtime-semantics reference. Oxc is the selected parser;
no alternate JavaScript engine, VM, GC, or RegExp implementation may supply
runtime semantics. The upstream C engine is a differential-testing oracle only
and is never linked or shipped.

## Architecture

Production crates remain independently reusable and documented:

- `quickjs-diagnostics`: sources, stable diagnostics, spans, source maps, and
  optional Miette rendering.
- `quickjs-frontend`: Oxc parsing, semantic analysis, parse goals, and owned
  frontend records.
- `quickjs-bytecode`: owned instructions, verifier, serializer, disassembler,
  constants, atoms, and debug tables.
- `quickjs-compiler`: Oxc lowering to verified bytecode.
- `quickjs-runtime`: values, heap, realms, VM, built-ins, modules, jobs,
  limits, interrupts, and embedding APIs.
- `quickjs`: ergonomic facade; thin `qjs`, `qjsc`, and bytecode-viewer CLIs
  consume the library crates.
- Optional/lower-priority crates: Tokio host driver, inspector, Wasm, N-API,
  TypeScript stripping, and Serde conversion.

`xtask`, fuzzers, and benchmarks are repository tooling, not production
dependencies. See [ARCHITECTURE.md](ARCHITECTURE.md) for trust boundaries and
[BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md) for the bytecode contract.

## Status

### Foundation and frontend

- [x] Reproducible Rust workspace; CI formatting, linting, tests, docs, audit,
  and optional oracle runners. Workspace-owned core crates forbid `unsafe`.
- [x] Directly pin published Oxc parser/semantic crates; no vendoring or
  patches.
- [x] Parse Script, Module, all dynamic Function-constructor forms, strict
  scripts, and asynchronous global scripts through explicit, lossless goals.
  Unsupported adapters reject before parsing.
- [x] Preserve byte-accurate diagnostics, owned semantic/module records,
  source-order static requests, import attributes, and binding roles.
- [x] Differential parser and Function-constructor manifests, including
  closed goal/feature/claim validation and a pinned compiler oracle.
- [x] The parser ledger is exhaustive and closed in four dimensions: parse
  goals, frontend claims, QuickJS grammar productions, and QuickJS parser
  diagnostics. Productions are enumerated from the pinned parser's own dispatch
  structure and each must be exercised by a fixture the oracle accepts. Every
  `SyntaxError` the pinned front end can raise while compiling a source text is
  either provoked by a fixture or recorded as unreachable with a reason; the
  observed oracle message is matched against the pinned format string on every
  run. Each intentional Oxc difference keeps an ID, rationale, and regression
  fixture.

Known intentional parser differences:

- `QJS-OXC-001`: Oxc determines RegExp literal boundaries/flags; the deferred
  QuickJS-derived layer owns pattern grammar.
- `QJS-OXC-002`: chained labels may accept a `continue` target that QuickJS
  rejects; a post-semantic check supplies the pinned QuickJS rejection.
- `QJS-OXC-003`: pinned QuickJS caps parser recursion near 695 nested
  parentheses and reports `stack overflow` (`quickjs.c:22720`); the frontend
  parses the same source on its isolated stack, since the bound is a QuickJS
  resource limit rather than ECMAScript grammar.
- `QJS-OXC-004`: pinned QuickJS rejects an instance field named `prototype`
  (`quickjs.c:25396`), which its own source marks as inconsistent with the
  specification; the frontend follows ECMAScript, which reserves `prototype`
  only for static fields.

Known intentional runtime differences:

- `QJS-BIGINT-001`: `js_bigint_asUintN` returns its argument unchanged whenever
  the requested width already spans the value (`quickjs.c:56075` and
  `quickjs.c:56092`), so the pinned `qjs` reports `BigInt.asUintN(64, -1n)` as
  `-1n` and `BigInt.asUintN(100, -1n)` as `-1n`. ECMAScript's `BigInt::asUintN`
  is defined modulo 2**bits and is therefore always non-negative; V8 reports
  `18446744073709551615n` and `1267650600228229401496703205375n`. Because the
  specification is the authority where the two disagree, this port follows
  ECMAScript. Widths below 64 agree with both engines.

Known intentional profile narrowings. These are not behavior differences: the
narrowed surface fails closed with a structured error rather than answering
incorrectly, so no script can observe a wrong result.

- `QJS-CREATE-001`: `Object.create` admits only its prototype argument. Honoring
  `propertyDescriptors` means running `ToPropertyDescriptor` for each key, which
  is resumable work this entry point cannot perform, so a present second
  argument reports `TypeError: property descriptors are not supported` instead of
  being silently ignored. The reported `length` stays `2` to match the pinned
  oracle, because arity is part of the observable shape.

Known intentional write-path differences:

- `QJS-WRITE-001`: writing an element through a String primitive receiver
  reports `TypeError: not an object`, where the pinned oracle boxes the
  receiver and reports `TypeError: '0' is read-only` (for example
  `Array.prototype.fill.call("abc", 0)`, and likewise `copyWithin`, `splice`,
  and `sort`). Both reject the write with a `TypeError`, so the surface fails
  closed; boxing primitives in the shared write path is deferred with the
  remaining exotics.

### Compiler, bytecode, and execution

- [x] Complete opcode metadata, checked codec/disassembly, typed operands,
  deterministic encoding, bounded construction, and total decoding.
- [x] Verifier foundations: predecode, targets/indices, stack-depth joins,
  maximum stack checking, function headers/kinds, and source-PC diagnostics.
- [x] Whole-graph verified bytecode is the only executable authority. Raw or
  serialized bytecode and direct `eval` remain fail closed.
- [x] Iterative Oxc lowering for ordinary functions, lexical bindings/captures,
  nested closures, expressions, statements, labels, `switch`, classic `for`,
  `for-in`, `for-of`, calls/spread, destructuring, and selected Error/native
  frame behavior. Compiler traversal and verification use explicit worklists.
  Array-assignment member and rest targets evaluate their base and computed key
  after the iterator is acquired and before the matching iterator step, which is
  the order ECMAScript's IteratorDestructuringAssignmentEvaluation and the
  pinned QuickJS reference (`quickjs.c:26596-26612`) both require; ordering
  regressions observe `next`, base, and computed-key effects.
- [x] Runtime installation, calls, exceptions, resumable native/bytecode
  dispatch, iterator close/error precedence, bounded resources, and
  verified-frame stack traces for the admitted profile. Host calls
  (`Context::call`) unwrap bound functions with the same observable result as
  interpreter dispatch: the innermost bound receiver reaches native and bytecode
  targets, and every bound layer's arguments accumulate before the caller's
  arguments are appended once.
- [x] Host interrupts: an embedder-installed handler is polled on a decrementing
  counter rather than on every instruction, reproducing `js_poll_interrupts` and
  its `JS_INTERRUPT_COUNTER_INIT` of 10,000 (`quickjs.c:512`, `quickjs.c:7877`).
  Fuel and interrupts stay separate because they answer different questions: fuel
  is a pre-committed deterministic budget, while an interrupt is a decision the
  host makes during execution, which is what makes wall-clock deadlines and user
  cancellation expressible. Upstream marks an interrupt uncatchable
  (`quickjs.c:7861`); this port preserves that structurally by reporting
  `ExecutionError::Interrupted` instead of a `JsException`, so it bypasses the
  JavaScript unwinder by construction.
- [ ] Complete verifier coverage, source/debug tables, dynamic `eval`, and
  remaining compiler/runtime opcode families.

### Values and objects

- [x] UTF-16 strings (including lone surrogates), Numbers with signed zero and
  int32 fast paths, property-key/index recognition, atoms/symbols, descriptors,
  bounded arenas, and iterative tracing/cycle reclamation foundations. Realm
  intrinsic descriptors follow the pinned upstream flags, including the
  non-writable, non-configurable `Function.prototype[Symbol.hasInstance]`
  (`quickjs.c:39511-39523`), so inherited `instanceof` behavior cannot be
  replaced by assignment.
- [x] First ordinary-object slice: object literals; data/accessor properties;
  ordinary reads/writes; receiver-aware calls; computed keys; and resumable
  getter/setter dispatch.
- [x] Operator/coercion profile: ordinary arithmetic, bitwise, comparison, and
  equality operators; resumable `ToPrimitive`; `StringToNumber`; radix
  conversion; and exact Number formatting tests for bases 2–36.
- [x] Boolean, Number, and String constructor/prototype verticals, including
  wrapper behavior, realm ownership, strict/sloppy receiver rules, and
  `Object.prototype` tagging/boxing as admitted by the current profile.
- [x] Descriptor authority and mutable object structure:
  `ValidateAndApplyPropertyDescriptor`/`OrdinaryDefineOwnProperty` decide every
  own-property definition, so a non-configurable property rejects a
  reconfiguration and a non-writable one accepts only a `SameValue` rewrite;
  `[[Delete]]`, `[[OwnPropertyKeys]]` with the full index/string/symbol phase
  order, `[[SetPrototypeOf]]` with its same-value-before-extensibility rule
  (`quickjs.c:7940`), `[[PreventExtensions]]`, and `SetIntegrityLevel`. The
  `delete` operator and object-literal `__proto__` reach these through the
  pinned `OP_delete` and `OP_set_proto` shapes. A realm-owned `Object`
  constructor publishes `getPrototypeOf`, `setPrototypeOf`, `preventExtensions`,
  `isExtensible`, `seal`, `freeze`, `isSealed`, `isFrozen`, `keys`, and
  `getOwnPropertyNames`. Shared `ToIntegerOrInfinity`, `ToLength`, and `ToIndex`
  replace the previously inlined length truncations.
- [x] `Array.prototype.join` and `Array.prototype.toString` as one resumable
  element loop mirroring `js_array_join` (`quickjs.c:42505`): the length is read
  once with `ToLength`, `null`/`undefined` elements and holes contribute
  nothing, each element's `ToString` and each accessor getter can re-enter the
  interpreter, and the separator defaults to `","` when absent or `undefined`.
  This closes the coercion divergence in which `String([1,2])` produced
  `"[object Array]"`.
- [x] BigInt domain: a project-owned two's-complement limb representation
  mirroring `JSBigInt` (`quickjs.c:490-495`), the full operator set with the two
  numeric domains kept separate (`cannot convert bigint to number` for a mixed
  pair, no unary `+`, no `>>>`), relational comparison and loose equality mixing
  by exact mathematical value rather than by rounding, `typeof`, truthiness,
  strict equality, `ToString`/`ToPropertyKey`, executable literals, an
  `Object(bigint)` wrapper with `[object BigInt]` tagging, and a realm-owned
  non-constructable `BigInt` with `toString`, `valueOf`,
  `[Symbol.toStringTag]`, `asIntN`, and `asUintN`.
- [x] Complete numeric conversions: the modular narrow conversions (`ToInt8`,
  `ToUint8`, `ToInt16`, `ToUint16`) share `ToUint32` and a truncation, while
  `ToUint8Clamp` saturates and rounds half to even because upstream uses `lrint`
  (`quickjs.c:13381`). `ToNumeric` admits a `BigInt` where `ToNumber` rejects one
  (`quickjs.c:13025`), and the `Number` constructor is its only caller
  (`quickjs.c:44595`), so `Number(1n)` is `1` while `1n | 0` still throws. The
  supporting `JsBigInt::to_f64` takes the top 54 significant bits and folds the
  remainder into a sticky flag, so `Number(9007199254740993n)` is
  `9007199254740992` and an out-of-range magnitude becomes a signed infinity.
  `CanonicalNumericIndexString` accepts only the exact `ToString` spelling, with
  `"-0"` answered directly (`quickjs.c:3675`).
- [x] `String.prototype` methods that need no `RegExp` or Unicode tables: `at`,
  `charAt`, `charCodeAt`, `codePointAt`, `concat`, `endsWith`, `includes`,
  `indexOf`, `lastIndexOf`, `padEnd`, `padStart`, `repeat`, `slice`,
  `startsWith`, `substr`, `substring`, `trim`, `trimEnd`, `trimStart`,
  `isWellFormed`, and `toWellFormed`. They share one resumable state machine
  because they share one shape: `RequireObjectCoercible`, then `ToString` of the
  receiver, then each declared argument left to right, and every one of those
  steps can re-enter the interpreter. The pinned oracle fixes that order, logging
  `recv,arg,pos` for `indexOf` with side-effecting conversions. Indices remain
  UTF-16 code-unit indices, so a lone surrogate stays observable.
- [x] `Number` statics and `Array.isArray`: the value properties
  (`MAX_VALUE`, `MIN_VALUE`, `EPSILON`, `MAX_SAFE_INTEGER`, `MIN_SAFE_INTEGER`,
  `POSITIVE_INFINITY`, `NEGATIVE_INFINITY`, `NaN`) are stored as exact binary64
  bit patterns rather than decimal literals and carry the pinned frozen
  descriptor, while `isInteger`, `isSafeInteger`, `isFinite`, and `isNaN` answer
  `false` for a non-Number without converting it, which is what separates them
  from the global `isNaN`. `Number.isInteger(2**53)` is `true` while
  `Number.isSafeInteger(2**53)` is `false`.
- [x] `Object.prototype.hasOwnProperty`, `isPrototypeOf`, and
  `propertyIsEnumerable`, plus `Object.create`. The first and third share one
  own-property resolution with `Object.getOwnPropertyDescriptor`, so all three
  agree on every exotic case: a primitive String reports its indices and
  `length`, and a hole is absent rather than `undefined`, which is the same
  distinction `Array.prototype.indexOf` relies on. `isPrototypeOf` starts its
  walk at the candidate's prototype, so nothing precedes itself, and charges the
  shared budget per link. `Object.create` represents a null prototype rather than
  substituting one; see `QJS-CREATE-001` for its narrowed descriptors argument.
- [x] `Array.prototype.push`, `pop`, `shift`, `unshift`, `reverse`, and `fill` as
  one resumable driver. Each reads `length` once with `ToLength`, performs a
  planned sequence of element steps, and writes `length` back; every read, write,
  and delete can enter an accessor, so each is a suspension point. Expressing the
  differences as an explicit step plan (`Move`, `Take`, `Drop`, `Store`, `Swap`)
  rather than as five implementations is what keeps hole handling uniform: an
  absent source is deleted at its destination, so `[1,,3].reverse()` stays sparse
  while `[,2].shift()` leaves index `0` present. The pinned oracle fixes the
  order, reporting `getlen|set1:x|setlen:2` for `push` and `getlen|get1|setlen:1`
  for `pop`. Growing past `2^53 - 1` reports upstream's misspelled
  `Array loo long` (`quickjs.c:41933`), which is observable and therefore
  reproduced. A real Array's exotic `length` reaches the array write path
  directly, because the ordinary path deliberately refuses a `length` write that
  has not run a resumable numeric conversion.
- [x] `Array.prototype.slice`, `concat`, and `at`, which read without mutating.
  `slice` and `concat` build a fresh Array while `at` answers one element, and all
  three share the same resumable element read. `concat` spreads only a real
  Array, and it applies that same test to its receiver, so
  `Array.prototype.concat.call({length:2,0:"a"},9)` has length `2` with the
  array-like itself at index `0`; nesting is never flattened. Holes survive into
  the result because an absent source index is skipped rather than written, and
  the destination length is set once at the end so a trailing hole still counts.
- [x] `Number.prototype.toFixed`, `toExponential`, and `toPrecision`, rendered
  from the *exact* value the binary64 holds rather than from its shortest decimal
  spelling. That distinction is observable and is why these use `JsBigInt`
  integer arithmetic instead of a floating-point formatter: `(1.005).toFixed(2)`
  is `"1.00"` because the stored value is just below 1.005, while
  `(1.55).toFixed(1)` is `"1.6"` because that one is just above, and a formatter
  working from the shortest spelling would round both up. Every binary64 is
  exactly `significand * 2^exponent`, so the digits follow from scaling by a power
  of ten, dividing by a power of two, and rounding the integer quotient half away
  from zero. Only `toFixed` validates its digit count before short-circuiting a
  non-finite value, which the oracle draws sharply: `(NaN).toFixed(101)` is a
  `RangeError` while `(NaN).toExponential(101)` is `"NaN"`.
- [x] `String.fromCharCode` and `String.fromCodePoint`, sharing the same
  resumable machine as the prototype methods because their arguments are also
  arbitrary objects. The two differ in coercion and range: `fromCharCode` applies
  `ToUint16` and wraps silently, so `String.fromCharCode(65601)` is `"A"`, while
  `fromCodePoint` requires an exact code point in `0..=0x10FFFF` and otherwise
  reports `RangeError: invalid code point`. A supplementary code point is encoded
  as a surrogate pair, so `String.fromCodePoint(0x1F600).length` is `2`.
- [x] `Array.prototype.indexOf`, `lastIndexOf`, and `includes` as one resumable
  element loop, since every element read can run a getter. They differ in exactly
  two observable ways, which are carried as data rather than as separate
  implementations. The comparison: the index searches use strict equality, so
  `[NaN].indexOf(NaN)` is `-1`, while `includes` uses `SameValueZero`, so
  `[NaN].includes(NaN)` is `true`; both treat the signed zeros as equal. Holes:
  the index searches test `HasProperty` first and skip a missing index, so
  `[1,,3].indexOf(undefined)` is `-1`, while `includes` reads every index and
  answers `true`. The length is read once with `ToLength`, and the loop stops at
  the first match, so a second matching getter never runs.
- [x] The callback-taking `Array.prototype` methods -- `forEach`, `map`,
  `filter`, `every`, `some`, `find`, `findIndex`, `findLast`, and
  `findLastIndex` -- as one resumable loop. Suspension is intrinsic here rather
  than incidental: the callback is a user call on every iteration, so the loop
  cannot be written any other way. Three behaviors separate the nine and all
  three are carried as data. Holes: the first five test `HasProperty` and skip a
  missing index, so `[1,,3].forEach` runs twice, while the `find` family visits
  every index and sees `undefined`, so `[1,,3].find` runs three times; `map`
  still counts a skipped hole so its result keeps the source's shape. Early exit:
  `every` stops on a falsy result and `some` and the `find` family stop on a
  truthy one. Result: `undefined`, a fresh Array, a Boolean, the element, or the
  index. The length is snapshotted with `ToLength` before the first callback, so a
  callback that grows the array is not revisited, while one that shrinks it still
  stops early because each index is re-tested.
- [x] `Array.prototype.reduce` and `reduceRight`, which share the same element
  read but thread an accumulator through the callback's result and pass four
  arguments rather than three. An absent initial value is distinct from an
  explicit `undefined` one: the former seeds from the first *present* element, so
  an empty or all-holes array reports `TypeError: empty array`, while the latter
  simply becomes the accumulator and `[1,2].reduce((a,v)=>a+v, undefined)` is
  `NaN`.
- [x] `Array.prototype.splice`, which is both a copier and a mutator. Every
  removed element is collected into a fresh Array before anything shifts, so a
  getter cannot observe a half-shifted array. The tail then moves by
  `insertions - removed`, walked from whichever end keeps a source from being
  overwritten before it is read: from the end when growing, from the front when
  shrinking. An absent `deleteCount` removes everything from `start` while an
  absent `start` removes nothing.

### Built-ins and asynchronous semantics

- [x] Initial Error family: Error, native Error subclasses, AggregateError,
  constructor/prototype graphs, causes, `Error.isError`, `toString`, iterator
  ordering/close behavior, and snapshotted engine-error stacks.
- [x] `Array.prototype.copyWithin` as a planned sequence of `Move` steps in the
  mutators' resumable driver: the length is read once with `ToLength`, the
  destination, source, and end arguments convert in that order, and the count
  `min(final - from, len - to)` saturates to an empty plan when negative, so
  `[1,2,3].copyWithin(0,5)` is unchanged. An overlap with the source below the
  destination is copied backward (`quickjs.c:43003-43004`), which the oracle
  pins as `len|g1|s2:b|g0|s1:a|`, and an absent source is deleted at its
  destination, so `[1,2,,5].copyWithin(1,2)` deletes index 1 while index 2
  stays present. The receiver is returned and the length is never written
  back.
- [x] The change-by-copy methods `with`, `toReversed`, and `toSpliced` as one
  resumable snapshot read: each source index is read with the pinned
  `JS_TryGetPropertyInt64` shape (`quickjs.c:9115-9142`), which reports an
  absent index as `undefined`, so the fresh result Array is dense rather than
  sparse. `with` converts its index with `JS_ToInt64Sat`, which truncates and
  saturates, and reports a rejected index after the negative adjustment
  (`quickjs.c:41859-41868`), so `[1].with(1e20, 0)` is
  `RangeError: invalid array index: 9223372036854775807`; the replaced index
  itself is never read. `toReversed` reads descending because the pinned
  source marks the order observable (`quickjs.c:42775`). `toSpliced` resolves
  its window the way `splice` does but reports an over-long result as
  `TypeError: invalid array length` (`quickjs.c:42932-42936`). `with` reuses
  `PredefinedAtom::With` rather than interning a duplicate, which the atom
  table's rollback invariant forbids.
- [x] `Array.prototype.flat` and `flatMap` as an explicit worklist of source
  frames replacing upstream's recursion in `JS_FlattenIntoArray`
  (`quickjs.c:43014-43074`): sources read ascending, innermost first, holes
  skipped so `[1,,[3]].flat()` has length `2`, and every read can enter a
  getter. `flatMap` validates its mapper after the length read
  (`quickjs.c:43086-43098`), calls it with `(element, index, source)` and the
  `thisArg` receiver, and maps only the outermost source, so
  `[1,2].flatMap(x=>[x,[x]])` flattens exactly one level. `flat`'s depth
  converts with `JS_ToInt32Sat`, so `flat(1.9)` flattens one level while
  `flat(NaN)` flattens none (`quickjs.c:43100-43103`). The destination is a
  fresh base Array, the same `Symbol.species` narrowing `concat` and `splice`
  already carry.
- [x] `Array.prototype.sort` and `toSorted` as an iterative merge sort over
  continuation state, replacing upstream's `rqsort` inside the comparator
  callback (`quickjs.c:43196-43280`). The number and order of comparisons is
  implementation-defined by ECMAScript and intentionally not pinned; the
  outcome is, because every comparison falls back to the element's original
  position (`quickjs.c:43187-43189`) and the sort is stable. `undefined`
  never reaches the comparator and moves to the end; holes are skipped during
  collection and deleted at the tail with upstream's throwing delete
  (`could not delete property`), while `toSorted` reads holes as `undefined`
  and answers a fresh dense Array. A non-Number comparator result converts
  with `ToNumber` and `NaN` means `0`; the default comparison computes each
  element's `ToString` at most once and compares UTF-16 code units. A pair
  sharing one bit pattern skips the comparator call entirely
  (`quickjs.c:43151-43153`), reproduced as `JsNumber::same_bits` and
  `JsString::shares_allocation`, so a throwing comparator on `[5,5,5,5]` is
  never invoked, and the write-back skips `Set` for an element that did not
  move (`quickjs.c:43249-43251`).
- [x] `Reflect.apply` and `Reflect.construct`, the first slice of the
  `Object`/`Function`/`Reflect` surface and the three cases the Error corpus was
  missing. `Reflect.apply` shares `Function.prototype.apply`'s argument-list
  read but validates the target first and rejects a nullish list with
  `TypeError: not a object` instead of treating it as empty (magic 2 in
  `js_function_apply`, `quickjs.c:41100-41107`). `Reflect.construct` validates a
  *supplied* `newTarget` before reading the list — a non-function reports
  `not a constructor` while a non-constructor function reports
  `<name> is not a constructor` (`quickjs.c:7799-7810`) — then reads the list,
  and only then checks the target, so a length getter runs before a non-function
  target reports `not a function` (`JS_CallConstructor2`,
  `quickjs.c:50195-50206`). An omitted `newTarget` defaults to the target. The
  namespace is an ordinary object inheriting `Object.prototype` with
  `Reflect[Symbol.toStringTag]`, arities 3 and 2, and a writable,
  non-enumerable, configurable global binding.
  `Runtime::function_is_constructor` answers `[[Construct]]` presence by walking
  bytecode/native/bound implementations iteratively, so a bind chain cannot
  recurse.
- [x] The remaining eleven `Reflect` methods, which completes the namespace:
  `get`, `set`, `has`, `deleteProperty`, `ownKeys`, `getPrototypeOf`,
  `setPrototypeOf`, `isExtensible`, `preventExtensions`, `defineProperty`, and
  `getOwnPropertyDescriptor`. Two properties separate every one from the
  matching `Object` static. First, the target must already be an object:
  ECMAScript 2015 relaxed the `Object` statics to accept primitives, but the
  mirrors keep `TypeError: not an object`, which the pinned oracle implements as
  the `reflect` magic flag (`quickjs.c:40026-40400`) and as an explicit tag test
  in the dedicated entry points (`quickjs.c:50215-50329`); the check precedes
  `ToPropertyKey`, so a key whose `toString` throws never runs for a primitive
  target. Second, a refusal is a `false` answer rather than a `TypeError`, so
  `Reflect.set(Object.freeze({a:1}),'a',2)` is `false` where the assignment
  operator would throw or silently succeed, and `preventExtensions` answers
  `true` rather than the target. `defineProperty` shares
  `Object.defineProperty`'s resumable descriptor read and differs only in
  dropping `JS_PROP_THROW` (`quickjs.c:40069-40080`), so a malformed descriptor
  is still a `TypeError` while a rejected definition is `false`. `ownKeys` is
  the only listing in the profile that emits the symbol phase, reporting a
  symbol key as the Symbol itself. `Reflect.set` implements `OrdinarySet` with a
  distinct receiver (`quickjs.c:9663-9930`): the target supplies the lookup that
  finds a setter, a read-only refusal, or an exotic `String` index, while the
  receiver stores the result and re-validates its own property, so an accessor
  or non-writable property there refuses and a non-extensible receiver refuses
  to gain a new one. A created receiver property is fully mutable while an
  existing one is updated by value alone, keeping its attributes. An array
  `length` on either side keeps its resumable `ToNumber` conversion, whose
  `RangeError: invalid array length` outranks the boolean answer, which the
  shared write path now reports through three modes — silent, throwing, and
  boolean — rather than one strict flag.
- [x] The `Object` value and key statics `is`, `hasOwn`, and
  `getOwnPropertySymbols`. `Object.is` is `SameValue`, which differs from `===`
  on exactly two inputs — `Object.is(NaN, NaN)` is `true` and
  `Object.is(0, -0)` is `false` — and converts nothing, so a `valueOf` on either
  operand never runs. `Object.hasOwn(target, key)` is
  `Object.prototype.hasOwnProperty` with the target moved out of the receiver,
  so it shares the same own-property resolution, including a primitive
  `String`'s exotic indices and `length`, and the same `cannot convert to
  object` for a nullish target (`quickjs.c:40402-40430`).
  `Object.getOwnPropertySymbols` is the symbol-only half of
  `[[OwnPropertyKeys]]`, sharing `Object.keys`' snapshot with the string and
  index phases disabled (`JS_GPN_SYMBOL_MASK`, `quickjs.c:40270-40276`); it
  reports each Symbol itself in creation order, includes a non-enumerable
  symbol-keyed property, and answers empty for a non-nullish primitive because a
  boxed wrapper never carries one.
- [ ] Remaining String/Number/Array method surface (`String.prototype` case
  conversions and `localeCompare`, the RegExp-dependent `match`, `matchAll`,
  `replace`, `search`, and `split`, `normalize`, `String.raw`, and the
  locale-dependent renderings), shape sharing/transition
  interning, remaining exotics (arguments, Proxy), dense indexed storage,
  deterministic finalization, and diagnostics.
- [ ] Complete the `Object` surface (`assign`, `values`, `entries`,
  `fromEntries`, `getOwnPropertyDescriptors`, `defineProperties`, `groupBy`,
  `Object.create`'s descriptors argument, `Object.prototype.toLocaleString`, and
  the `__proto__` accessor pair), then Proxy, remaining built-ins,
  RegExp/Date/JSON, collections, binary data, Atomics, Unicode tables, promises,
  async functions/generators, weak references, and finalization registries.
- [ ] Add deterministic QuickJS-compatible job ordering. Tokio may provide
  host I/O, timers, cancellation, and wakeups, but never Promise-job ordering.

### Modules, embedding, and tools

- [x] Initial runtime/realm/context foundation with bounded realm creation,
  same-runtime handle checks, verified-function installation, primitive values,
  and host invocation.
- [ ] Full Rust embedding API; module linking/cycles/dynamic import/top-level
  await; QuickJS-compatible resolver semantics; ESM REPL; `qjs`; Rust-native
  `qjsc`; bytecode viewer; CDP adapter; and portable `std`/`os` modules.

### Conformance, performance, and optional layers

- [ ] Run/maintain upstream suites, pinned test262
  `5c8206929d81b2d3d727ca6aac56c18358c8d790`, differential corpora, and
  fuzzing for parser, bytecode, serializer, and runtime boundaries.
- [ ] Establish startup, memory, interpreter, and compile benchmarks; require
  no unexplained supported-platform crashes or undefined behavior.
- [ ] Complete public API, source-map, platform/resource-limit, cancellation,
  dependency, and reproducible-release audits.
- [ ] Optional: Wasmtime WebAssembly, safe N-API semantics plus an audited ABI
  boundary, erasable TypeScript preprocessing with required source maps, and a
  bounded policy-driven Serde bridge.

## Completion gates

A milestone is complete only when its checked items pass in CI. Each semantic
change starts with a regression or conformance test. Relevant standard gates:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Run the applicable `cargo xtask *-differential` corpus against the pinned
QuickJS oracle for parser, dynamic Function, Number radix, control flow,
function apply/bind, iterators, call spread, and Errors. The parser manifest is
a closed compatibility gate: it fails when a pinned grammar production has no
accepted fixture, when a reachable pinned diagnostic has no fixture, when a
fixture declares an unreachable one, or when an observed oracle message does not
match the pinned format string.

Current corpus status: parser 196/196, Number radix 991/991, control flow 63/63,
iterators 40/40, function apply 15/15, function bind 21/21, call spread 15/15,
and Errors 35/35.

## Engineering rules

1. Preserve observable ECMAScript behavior, not QuickJS private representation.
2. Use validated newtypes for bytecode operands and heap handles; reject
   unsupported semantics rather than silently approximating them.
3. Keep parser, compiler, VM, and host concerns separate. Carry source
   identity/spans through bytecode and stack frames; retain structured errors
   independently of CLI rendering.
4. Keep the Rust core safe. Any N-API pointer handling is confined to its
   audited boundary crate.
5. Performance changes require a profile, benchmark, and preserved observable
   behavior; `unsafe` is never an optimization escape hatch.
6. Tokio is a host substrate only; the runtime owns ECMAScript jobs and Promise
   ordering.
7. Match documented upstream omissions: proper tail calls and `Atomics.waitAsync`
   remain out of scope; `Intl` is a separate optional layer.
8. Preserve upstream copyright notices. Keep changes small, bisectable, tested,
   and recorded in Git; production APIs must be documented and stable.
