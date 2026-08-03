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
  deterministic no-`Intl` `toLocaleString`, `[Symbol.toStringTag]`, `asIntN`,
  and `asUintN`.
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
- [x] Unicode-backed `String.prototype.toLowerCase`, `toUpperCase`, their
  no-`Intl` locale-named forms, `normalize`, and `localeCompare`, using pinned
  pure-Rust ICU4X 2.2.0 data. Default casing applies the full, context-sensitive
  Unicode mappings, including final sigma and multi-code-point expansions; the
  locale-named forms select the deterministic root locale and ignore their
  ECMA-402-reserved arguments. Normalization supports NFC, NFD, NFKC, and NFKD.
  Scalar runs are transformed independently around lone surrogates so exact
  ECMAScript UTF-16 is preserved. The no-`Intl` comparator orders NFC
  representatives lexically by UTF-16, giving a deterministic total order,
  returning zero for every canonically equivalent pair, and keeping
  compatibility-only equivalents distinct. Receiver/form/comparison coercions
  reuse the resumable String machine, while Unicode input and output scans
  consume shared instruction fuel.
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
  substituting one and delegates its descriptor map to the same two-phase
  `ObjectDefineProperties` operation as `Object.defineProperties`.
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
- [x] `Array.prototype.toReversed` and `with` as the first change-by-copy
  methods. Both use a fresh realm Array rather than species construction and
  read through holes with ordinary `Get`, so every output index is present even
  when its value is `undefined`. `toReversed` observes source getters from the
  last index to the first; `with` performs resumable index coercion before any
  element read, rejects an out-of-range relative index, and skips the replaced
  source getter. Their shared copier now also applies resumable `ToLength` when
  an array-like `length` getter returns an object instead of assuming a primitive.
- [x] `Array.prototype.toSpliced` through the same change-by-copy continuation.
  It distinguishes absent `start`, absent `skipCount`, and explicit `undefined`,
  performs both observable numeric conversions before reading an element,
  copies the prefix and suffix with read-through-hole `Get`, and stores insertion
  values without coercion. The result length is checked against the safe-integer
  ceiling before `ArrayCreate`; large array-like source indices above the Array
  index domain are read through their ordinary decimal String property keys.
- [x] `Array.prototype.copyWithin` as a generic, resumable in-place copy. It
  converts `length`, target, start, and end in specification order, selects a
  backward traversal only for an overlapping destination, and implements each
  step as `HasProperty` followed by `Get` plus strict `Set`, or
  `DeletePropertyOrThrow` for an absent source. The element cursor retains only
  one move regardless of array-like length, so shared instruction fuel bounds
  scans up to `2^53 - 1`; ordinary decimal keys above the Array-index domain and
  primitive `ToObject` receivers remain observable without special cases.
- [x] `Array.prototype.sort` and `toSorted` through one resumable
  `SortIndexedProperties` machine. Comparator validation precedes `ToObject`;
  length and indexed collection complete before a stable bottom-up merge sort
  begins. Each user comparison and each default `ToString` is a suspension
  boundary, comparator results receive resumable `ToNumber`, `NaN` is a stable
  tie, and UTF-16 lexical comparison charges its scan to shared fuel. In-place
  `sort` skips holes, performs strict ordered writes, then applies
  `DeletePropertyOrThrow`; `toSorted` allocates first, reads through holes, and
  defines a fresh dense base Array without touching the source.
- [x] `Array.prototype.flat` and `flatMap` through an explicit, traced
  `FlattenIntoArray` frame stack rather than host recursion. Root and nested
  lengths are snapshotted with resumable `ToLength`, indexed traversal uses
  `HasProperty` before `Get`, holes disappear, only real Arrays descend, and
  `flatMap` invokes its validated mapper only for present root elements before
  flattening the result by one level. `flat` performs observable depth coercion
  before allocation and supports finite, clamped-negative, and infinite depth.
  Both methods now use `ArraySpeciesCreate`, including the exact constructor and
  `%Symbol.species%` getter order, custom construction with length `0`, the
  current cross-realm intrinsic-Array rule, null fallback, and generic
  non-Array receivers. `%Array%[Symbol.species]` is installed as the specified
  non-enumerable configurable accessor, and each mapper, getter, conversion,
  property creation, and constructor call is a suspension or fuel boundary.
- [x] Deterministic ECMA-262 locale-string behavior for the no-`Intl` profile.
  `Object.prototype.toLocaleString` performs the specified dynamic `Invoke` of
  `toString` and returns its result without coercion. Number and BigInt use
  their ordinary decimal rendering, which ECMA-262 explicitly permits when
  ECMA-402 is absent. `Array.prototype.toLocaleString` selects `","` as its
  implementation-defined separator, reads every index with `Get`, emits an
  empty field for nullish elements and holes, then dynamically invokes each
  element's `toLocaleString` and converts the result to String. The Array path
  is resumable across length conversion, getters, calls, and result conversion,
  retains its live heap edges for tracing, and charges every scan to shared
  instruction fuel; reserved locale/options arguments are deliberately ignored
  and are not forwarded under the ECMA-262 fallback algorithm.
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
- [x] The ECMAScript 2025 ordinary-object `%Reflect%` surface: all thirteen
  methods have their specification names, lengths, descriptors, property order,
  and non-constructable identities on a non-callable ordinary object with
  `%Object.prototype%` and `@@toStringTag`. `apply` and `construct` share the
  resumable `CreateListFromArrayLike` machine while preserving their distinct
  validation and nullish-list rules. Keyed methods validate the target before
  resumable `ToPropertyKey`; `get` and `set` preserve an explicit receiver;
  descriptor, delete, prototype, extensibility, and own-key operations expose
  their internal-method Boolean/result shapes rather than the throwing `Object`
  wrappers. `Reflect.set` also reaches the existing resumable Array `length`
  conversion and reports a rejected exotic write as `false`. Array `length`
  definitions run their two observable numeric conversions, preserve partial
  shrink results at non-configurable indices, and install a requested
  non-writable final state even when that shrink returns `false`.
- [ ] Remaining RegExp-dependent String method surface, shape sharing/transition
  interning, remaining exotics (arguments, Proxy), dense indexed storage,
  deterministic finalization, and diagnostics.

### Built-ins and asynchronous semantics

- [x] Initial Error family: Error, native Error subclasses, AggregateError,
  constructor/prototype graphs, causes, `Error.isError`, `toString`, iterator
  ordering/close behavior, and snapshotted engine-error stacks.
- [x] Close the Error compatibility corpus, including `Reflect.construct`
  construction paths and script-thrown Error observation through ordinary
  JavaScript property reads: 35/35 cases and all 59 required feature tags match
  the pinned oracle.
- [x] Extend ordinary `Object` reflection with `Object.is`, `Object.hasOwn`,
  `Object.getOwnPropertySymbols`, and `Object.getOwnPropertyDescriptors`, with
  the specification `SameValue` comparison, `ToObject`-before-`ToPropertyKey`
  ordering, symbol-only own-key projection, primitive String exotic keys, fresh
  descriptor materialization, exact built-in identities, and pinned constructor
  property order.
- [x] Add `Object.values` and `Object.entries` through one resumable
  `EnumerableOwnProperties` machine: it fixes the string-key snapshot before
  getter re-entry, re-reads each current descriptor, skips deleted or hidden
  keys, observes newly enumerable snapshotted keys, propagates getter throws,
  and boxes primitive Strings. The shared `CopyDataProperties` path now also
  snapshots Symbols and rechecks descriptors, fixing object rest/spread under
  getter-driven mutation.
- [x] Implement `Object.assign` with target `ToObject` validation before source
  access, per-source all-own-key snapshots, current descriptor rechecks,
  primitive String and Symbol copying, and resumable getter/strict-setter
  re-entry. Failures preserve already committed writes, and Array `length`
  targets reuse the ordinary nested conversion and shrink machinery.
- [x] Implement `Object.defineProperties` and descriptor-bearing
  `Object.create` through one resumable two-phase `ObjectDefineProperties`
  machine: snapshot and convert every current enumerable descriptor before the
  first mutation, then define in key order with partial completion only during
  the application phase. Getter re-entry, Symbols, descriptor validation, and
  Array `length` conversion retain ordinary specification behavior.
- [x] Implement `Object.fromEntries` as an iterative, resumable
  `AddEntriesFromIterable`: allocate the ordinary result before iterator
  acquisition, read entry indices `0` then `1` before `ToPropertyKey`, define
  full data properties for String and Symbol keys, and perform exceptional
  `IteratorClose` while preserving the original abrupt completion.
- [x] Implement `Object.groupBy` through the resumable `GroupBy` abstract
  operation: validate the callback before iterator acquisition, alternate
  callback calls and `ToPropertyKey`, close only post-yield abrupt completions,
  retain first-seen keyed lists, then materialize realm Arrays on a fresh
  null-prototype ordinary object after normal iterator completion.
- [x] Add `AddRestrictedFunctionProperties` with one anonymous, non-extensible
  realm-owned `%ThrowTypeError%`: `Function.prototype.caller` and `arguments`
  share it as getter and setter, remain configurable and non-enumerable, and
  expose frozen empty-name/zero-length thrower identity properties.
- [x] Implement `JSON.parse` on a realm-owned `%JSON%` object: an iterative
  UTF-16 parser accepts exactly the ECMA-404 JSON grammar, preserves escapes,
  lone surrogates, duplicate-member evaluation, and `__proto__` as data, then
  materializes ordinary objects and Arrays in source order. A resumable
  post-order `InternalizeJSONProperty` machine re-reads mutated properties,
  preserves getter and callback abrupt completions, applies delete-or-define
  results, and supplies the ECMAScript 2026 primitive `context.source` record.
- [x] Add the ECMAScript 2026 raw-JSON surface. `JSON.rawJSON` performs the
  observable `ToString`, applies the specification's boundary checks, accepts
  only an exact primitive ECMA-404 text, and atomically creates a frozen,
  null-prototype object with the unforgeable `[[IsRawJSON]]` brand and exact
  enumerable `rawJSON` data property. `JSON.isRawJSON` tests that internal slot
  rather than forgeable shape, and the realm publishes both method identities
  in standard property order.
- [x] Implement `JSON.stringify` with an iterative serialization worklist:
  replacer-list getters and boxed coercions, `space` conversion, property
  getters, `toJSON`, replacer calls, wrapper unboxing, raw-JSON embedding,
  key/length snapshots, cycle detection, well-formed UTF-16 quoting, and
  compact or indented container assembly all preserve specification order and
  abrupt completion. Linear scans and output passes consume explicit fuel, and
  continuation accounting retains every suspended collection and heap edge.
- [x] Implement the coercing global numeric functions `isFinite`, `isNaN`,
  `parseFloat`, and `parseInt`. Their resumable conversions preserve `ToNumber`
  and `ToString` abrupt completions and `parseInt`'s input-before-radix order;
  the prefix scanners operate directly on UTF-16, preserve negative zero,
  implement exact power-of-two and decimal binary64 rounding, and charge scan
  work to the shared fuel budget. `Number.parseFloat` and `Number.parseInt`
  reuse the corresponding realm-global function identities.
- [x] Implement `encodeURI`, `encodeURIComponent`, `decodeURI`, and
  `decodeURIComponent` from the ECMA-262 `Encode` and `Decode` algorithms.
  Encoding walks exact UTF-16 code points, rejects unpaired surrogates, uses
  uppercase UTF-8 percent octets, and distinguishes the RFC 2396 complete-URI
  reserved set from component text. Decoding preserves only reserved ASCII
  escapes for complete URIs and rejects truncated, overlong, surrogate, and
  out-of-range UTF-8 as realm-owned `URIError`; all four perform resumable
  `ToString` first and charge their bounded scans to shared execution fuel.
- [x] Install `Object`, `Number`, `BigInt`, and `Array` `toLocaleString` with
  exact realm-owned method identities and ECMA-262 fallback semantics in the
  no-`Intl` profile. Object delegates through an observable `toString` lookup;
  Number and BigInt use deterministic ordinary decimal output; Array performs
  the specified generic length/index walk, nullish empty fields, dynamic
  element invocation, and result coercion with explicit suspension, tracing,
  and fuel boundaries.
- [x] Implement the remaining non-RegExp Unicode String built-ins with ICU4X
  compiled data: full context-sensitive default/root-locale case conversion,
  all four normalization forms, and a deterministic no-`Intl` `localeCompare`
  that compares NFC representatives and therefore honours canonical
  equivalence. Lone UTF-16 surrogates remain unchanged, ECMA-402-reserved
  arguments remain unused, and every observable coercion stays resumable.
- [x] Implement synchronous `Array.from` and `Array.of` as generic factories.
  Constructor selection is species-independent; iterable `Array.from`
  allocates before calling the iterator method, maps before defining each own
  data property, and performs exceptional `IteratorClose` only for mapper and
  definition failures. Its array-like fallback preserves `GetMethod`,
  `ToObject`, `LengthOfArrayLike`, constructor, indexed `Get`, mapper, and final
  strict `length`-set order. `Array.of` constructs with the item count, defines
  every indexed item, and performs the same final strict set. All getter,
  constructor, iterator, mapper, and setter boundaries remain resumable.
- [x] Implement `String.raw` as the generic `Get(template, "raw")` and
  `LengthOfArrayLike` algorithm. It performs both required `ToObject` steps,
  snapshots only the substitution list supplied by the call, then alternates
  indexed literal `Get` and observable `ToString` conversions while ignoring
  missing or excess substitutions exactly as specified. Property getters and
  primitive conversions suspend through the ordinary VM machinery, UTF-16
  concatenation preserves lone surrogates, scans and output consume shared
  instruction fuel, and the constructor exposes the pinned
  `length,name,fromCharCode,fromCodePoint,raw,prototype` own-key order.
- [x] Materialize publicly instantiated verified root functions with the same
  `OrdinaryFunctionCreate`, `SetFunctionLength`, `SetFunctionName`, and
  `MakeConstructor` surface already used for nested functions. Constructable
  roots now expose exact `length,name,prototype` order and descriptors, own a
  fresh `%Object.prototype%`-linked prototype whose `constructor` points back
  to the function, and participate in aggregate heap/property preflight,
  rollback, tracing, and reclamation without changing internal dynamic-Script
  roots.
- [x] Install the ordinary, non-callable `%Math%` object and its first
  specification-order method tranche: `min`, `max`, `abs`, `floor`, `ceil`,
  `round`, `sqrt`, `acos`, `asin`, and `atan`. Every argument crosses the
  resumable `ToNumber` boundary; variadic extrema convert the complete list
  left-to-right even after `NaN`, preserve the required signed-zero winner,
  and process primitive lists iteratively so large calls neither recurse on
  the Rust stack nor escape shared instruction fuel. Unary algorithms preserve
  the specified NaN, infinity, and signed-zero cases, including `round` ties
  toward positive infinity. The realm graph publishes exact property order,
  descriptors, names, arities, `@@toStringTag`, resource accounting, and
  failure-atomic rollback; remaining `%Math%` methods and constants stay open.
- [x] Extend `%Math%` through the next contiguous specification-order tranche:
  `atan2`, `cos`, `exp`, `log`, `pow`, `sin`, `tan`, `trunc`, and `sign`.
  Binary methods convert both arguments left-to-right through resumable
  `ToNumber`, retain the unconverted right operand across suspension, and
  preserve abrupt-completion ordering. `pow` shares the VM's
  `Number::exponentiate` implementation with `**`; `atan2` and every unary
  method preserve the specified NaN, infinity, quadrant, and signed-zero edge
  cases. Realm publication keeps the pinned own-key order, exact descriptors,
  resource ceilings, tracing, and failure-atomic rollback; the remaining
  `%Math%` methods and constants stay open.
- [x] Extend `%Math%` through the contiguous hyperbolic, precision-logarithm,
  and cube-root tranche: `cosh`, `sinh`, `tanh`, `acosh`, `asinh`, `atanh`,
  `expm1`, `log1p`, `log2`, `log10`, and `cbrt`. Each method uses the shared
  resumable `ToNumber` path and preserves the specification's NaN, infinity,
  domain, and signed-zero branches before returning its binary64
  implementation approximation. Publication and regression coverage retain
  the pinned own-key order, exact function metadata, atom/property ceilings,
  and failure-atomic realm construction; the remaining `%Math%` surface starts
  at `hypot`.
- [x] Implement `Math.hypot` and realm-local `Math.random`. `hypot` converts
  every argument left-to-right before resolving its result, so later abrupt
  completions remain observable and infinity wins over NaN only after all
  coercions; pairwise binary64 `hypot` avoids naive intermediate overflow and
  underflow. `random` uses a non-zero realm-owned xorshift64* stream, returns
  uniformly spaced values in `[0, 1)`, ignores arguments without coercion, and
  assigns a distinct sequence only after realm construction commits. Both
  methods preserve pinned metadata/order, tracing, exact resource ceilings,
  and rollback invariants; the remaining `%Math%` methods start at `f16round`.
- [x] Implement `Math.f16round`, `Math.fround`, `Math.imul`, and `Math.clz32`.
  `f16round` converts binary64 directly to binary16 with
  round-to-nearest-ties-even, including subnormal, overflow, signed-zero, and
  the specification's double-rounding counterexample, without routing through
  binary32. `fround` performs the specified binary32 round trip; `imul`
  converts both operands left-to-right with `ToUint32` and returns the signed
  low 32-bit product; `clz32` shares the same conversion and counts the exact
  32-bit representation. Pinned order/metadata, abrupt coercions, resource
  ceilings, and realm rollback remain covered; only `sumPrecise` and the
  `%Math%` constants remain open.
- [x] Implement `Math.sumPrecise` with the specification's synchronous
  iterator protocol and strict Number-only element requirement. Iteration is
  resumable across `@@iterator`, `next`, `done`, and `value`; non-number and
  count-limit failures perform `IteratorClose`, while abrupt iterator-step
  property access does not. A fixed-width signed superaccumulator covers the
  complete binary64 exponent range, preserves the specified empty and
  negative-zero states, combines infinities and NaN correctly, and rounds only
  once to nearest ties-to-even. Exceptional close now also preserves a frozen
  stack for the original engine-created error. Shared fuel, tracing, exact
  metadata/order, resource ceilings, and realm rollback remain covered; only
  the `%Math%` constants remain open.
- [ ] Complete remaining QuickJS legacy Function metadata and exotic
  reflection semantics (including Proxy), then remaining built-ins,
  RegExp/Date, collections, binary data,
  Atomics, RegExp Unicode tables, promises, async functions/generators, weak
  references, and finalization registries.
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
and Error 35/35 (59/59 required feature tags). The Error harness observes an
explicitly thrown value's `name` and `message` by calling a small ordinary
dynamic JavaScript observer, so normalization follows script-visible property
semantics instead of inferring an engine-side prototype brand.

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
