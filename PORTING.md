# Porting plan

## Compatibility target

The target is QuickJS 2026-06-04:

- ES2025 script and module semantics, including Annex B;
- the embeddable runtime/context/value model;
- reference-counted deterministic destruction with cycle removal;
- stack bytecode, closures, exceptions, generators, async jobs, and modules;
- BigInt, RegExp, Unicode, JSON, Date, Proxy, Map/Set, Promise, TypedArray,
  Atomics, and SharedArrayBuffer;
- `qjs`, a Rust-native `qjsc`, and the documented `std`/`os` host surface.

QuickJS binary layout and C API compatibility are not goals. Rust callers
receive an idiomatic, lifetime-safe API. The optional N-API boundary separately
targets that external C ABI through a Rust-written adapter. Any host behavior
that cannot be reproduced portably or safely will be documented with a
compatibility test.

## Planned crate topology

Every production crate must be independently reusable and documented:

- `quickjs-diagnostics`: source registry, stable diagnostic codes, exact spans,
  standard source-map chaining, and optional Miette rendering;
- `quickjs-frontend`: Oxc configuration, parse goals, diagnostics, and the
  arena-safe AST boundary;
- `quickjs-bytecode`: owned instruction schema, verifier, serializer,
  disassembler, constants, atoms, and debug tables;
- `quickjs-compiler`: Oxc AST/semantic lowering into verified bytecode;
- `quickjs-runtime`: values, heap, contexts/realms, VM, built-ins, modules,
  jobs, limits, interrupts, and embedding APIs;
- `quickjs-tokio`: optional Tokio host driver for async I/O, timers, workers,
  and event-loop integration;
- `quickjs-inspector`: runtime debugger API and transport-independent Chrome
  DevTools Protocol adapter;
- `quickjs-wasm`: optional Wasmtime-backed JavaScript WebAssembly surface;
- `quickjs-napi-core` and `quickjs-napi-abi`: low-priority safe N-API
  semantics plus an isolated Rust C-ABI boundary;
- `quickjs-typescript-strip`: low-priority, source-mapped, erasable
  TypeScript preprocessing;
- `quickjs-serde`: low-priority, policy-driven conversion between rooted
  JavaScript object graphs and Rust `serde` data models;
- `quickjs`: ergonomic facade with deliberate feature flags;
- thin `qjs`, `qjsc`, and bytecode-viewer binary crates built entirely on
  those libraries.

`xtask` and fuzz/benchmark harnesses are repository tooling rather than
production crates. Production crates remain usable without the CLIs.

## Acceptance gates

A milestone is complete only when all of its checked items pass in CI.

### M0 — reproducible foundation

- [x] Record the upstream release and archive digest.
- [x] Require workspace-owned core crates to forbid `unsafe`; audit
      dependencies separately. Only the future isolated N-API ABI crate may
      opt out for documented foreign-pointer operations.
- [x] Add formatting, linting, test, documentation, and dependency-audit CI.
- [x] Add an optional differential runner for the upstream `qjs` executable.
- [x] Consume the exact pinned published Oxc parser and semantic crates
      directly, with registry checksums retained in `Cargo.lock` and no
      vendored or patched source.

### M1 — Oxc source front end

- [x] Pin the current Oxc parser crates and define the arena-safe AST boundary.
- [x] Run production callback entries in an isolated scoped parser/semantic
      worker with a dedicated stack; retain the caller-arena `parse` API as an
      explicitly non-isolated low-level entry.
- [x] Represent global Script, Module, indirect/direct eval, and all dynamic
      Function-constructor goals losslessly; unsupported contextual adapters
      fail before Oxc without semantic downgrade.
- [ ] Parse JavaScript scripts, modules, eval input, and Function-constructor
      bodies with explicit modes; do not enable TypeScript or JSX.
  - [x] Adapt host-forced-strict Script through a tracked zero-span semantic
        directive so Oxc binds strict scopes before analysis while source text,
        hashbang, body, and real directive spans remain unchanged.
  - [x] Adapt asynchronous global Script without patching Oxc: retain Script
        identity and HTML comments, admit top-level `await` and dynamic import,
        and reject module declarations, `import.meta`, and root `await`
        identifiers or labels with stable project-owned diagnostics.
  - [x] Parse all four dynamic Function-constructor families through the exact
        QuickJS wrapper as a complete Oxc Script, retaining a byte-exact
        fragment map and fail-closed preparation limits.
- [x] Reject parser and deferred semantic diagnostics with byte-accurate source
      spans.
- [x] Retain Oxc `ModuleRecord` and complete `Semantic` data on every successful
      parsed unit for compiler lowering.
- [x] Lower static module requests into an arena-independent owned record with
      source occurrence order, exact UTF-16 strings, import attributes,
      typed request indices, and local/indirect/star linking roles.
- [ ] Differentially test Oxc acceptance, early errors, strict mode, Annex B,
      and ES2025 syntax against the pinned QuickJS release.
  - [x] Differentially test all four dynamic Function-constructor families
        against the pinned `qjsc -c` compiler oracle, including
        exact-wrapper-sensitive comments, wrapper escape, strict/contextual
        parameters, Unicode, and malformed input without executing JavaScript.
- [ ] Record every parser compatibility gap and either close it or mark it as
      an intentional, regression-tested Oxc difference before claiming M1.

### M2 — compiler and VM core

- [x] Port the complete final and compiler-temporary opcode schema, operand
      widths, and fixed/dynamic stack-effect metadata from `quickjs-opcode.h`.
- [x] Checked owned stack-bytecode codec with typed operands, deterministic
      encoding, bounded transactional construction, and a total decoder.
- [x] Function-local `AtomPoolIndex` operands for all five atom-bearing
      formats, with unchanged deterministic encoding and explicit deferred
      pool-bounds validation.
- [x] Bounded human-readable disassembly with typed operands, resolved stack
      effects, stable text, and structured malformed-input/limit failures.
- [ ] Control-flow and abstract-stack verifier with computed maximum stack
      depth.
  - [x] First fail-closed slice: complete predecode, all currently modeled
        static target/index/secondary-operand checks, ordinary reachable
        stack-height analysis, exact maximum comparison, and a deliberately
        non-executable `VerifiedControlFlow` certificate.
  - [x] Validate serialized function execution flags, mode bits, defined
        argument and variable-reference counts and their available-binding
        relationship; retain a typed function kind; and enforce suspension and
        return-opcode kind compatibility.
- [ ] Instruction/function PC-to-source tables, source-map chaining, and
      precise source lookup for diagnostics and stack frames.
  - [x] First final-instruction source table: strictly ordered instruction PCs
        retain byte-exact Oxc spans and owned source text across arena teardown
        for the ordinary leaf-function lowering slice.
  - [x] Compiler verifier diagnostics resolve their exact final bytecode PC
        through that relocated table; inconsistent joins retain both incoming
        and target source spans, while root failures retain no invented
        instruction location.
- [ ] Bindings, lexical environments, closures, calls, constructors, and
      direct/indirect `eval`.
  - [x] First arena-independent binding-storage plan: direct Oxc semantic
        consumption, native dense executable/binding/reference identities,
        resolved-reference-to-binding edges, deterministic owned `Arc` slices,
        Script/Module/import/default-export placement, declaration
        initialization/write/TDZ policy, and typed fail-closed rejection for
        semantic cases not yet lowered. Descendant frame captures now
        propagate iteratively through every intermediate executable as
        deterministic parent-binding or parent-capture edges; global and
        module cells remain in their distinct storage domains.
  - [x] First verified ordinary leaf-function vertical: an arena-borrowing
        `CompilationContext` retains private Oxc node/symbol/reference identity
        maps and issues context-provenant executable selections. Function
        declarations and anonymous `function` expressions in Script units
        assign typed argument/local slots, emit the exact final
        `set_loc_uninitialized; get_arg; put_loc; get_loc_check; return`
        family, track stack depth, and return only an owned non-executable
        `VerifiedControlFlow` certificate.
  - [x] Establish the pool-free straight-line base: multiple
        simple `var`/`let`/`const` declarations, reverse-order TDZ setup,
        immediate Boolean/null/int32 and compact `BigInt` values, the empty
        string, exact argument/local reads and writes, all value-only unary and
        binary operators needing no pools, sequence/expression statements, and
        explicit or implicit returns. Expression lowering uses an iterative
        work list and validates the whole body before emitting bytes.
        Atom-backed constants remain fail-closed until their owned records
        exist.
  - [x] Add compiler-owned control flow without recursion guards: provenance-
        checked symbolic labels, duplicate/unbound/end-target rejection,
        shortest-upward QuickJS-width branch relaxation with bounded
        instruction visits, relocated source PCs, and a compiler verifier
        entry that derives the exact reachable stack maximum, validates joins,
        and rejects residual values at reachable exits. Conditional
        expressions and value-preserving `&&`/`||`/`??` lower iteratively,
        including shared natural left-chain joins and whole-path fail-closed
        validation.
  - [x] Lower the first structured-statement family with no recursion guard:
        exact Oxc creator scopes drive reverse-slot TDZ initialization at
        function or lexical-block entry; paired scope and loop work items
        lower blocks, `if`/`else`, `while`, `do`/`while`, and unlabeled
        `break`/`continue`; loop-body lexicals reset on every re-entry; and
        unreachable source paths are still validated and structurally
        terminated before verification. Compiler-owned labels retain their
        source spans, and every reachable statement anchor is independently
        checked at verified stack depth zero after branch relocation.
  - [x] Add the captured-cell and classic-`for` substrate: compiler-owned
        capture layouts distinguish arguments, function-lifetime locals, and
        scoped locals; the staged verifier admits `close_loc` only for an
        explicitly declared scoped capture while serialized bodies and
        reference-construction opcodes remain fail-closed. Deepest leaf
        functions read and write forwarded cells through checked `var_ref`
        opcodes. Mutable identifier assignment, compound/logical assignment,
        and prefix/postfix update preserve JavaScript expression values.
        Classic `for` uses the iterative statement queue for
        initializer/test/body/update flow, rotates captured loop-head cells
        after initialization and before every update including `continue`,
        and closes the final cell on exit.
  - [x] Add typed nested-function constants and iterative tree compilation:
        compiler bodies declare exact value/function constant kinds, and the
        staged verifier conditionally admits `push_const*`/`fclosure*` while
        serialized constant operations remain fail-closed. `compile_tree`
        freezes a flat immutable executable-preorder tree, keeps direct child
        constants in source order, normalizes each capture to a parent-owned
        variable-reference cell or parent closure slot, and crosses the compact
        `fclosure8` boundary exactly. Body function declarations instantiate
        before user code with last-declaration-wins semantics, including
        argument redeclarations; strict block declarations instantiate on
        every scope entry. All compiler traversal uses explicit iterative work
        stacks. Labeled control, `for-in`, `for-of`, inferred
        anonymous-function names, non-string atom namespaces, other value
        families, and immutable-write throws remain fail-closed.
  - [x] Add exact binary64 Number constants to the compiler-owned heterogeneous
        pool. Number literals requiring pool storage and direct child templates
        share one immutable `Arc`-backed source-order index namespace with no
        deduplication. Both `push_const*` and `fclosure*` use compact indices
        `0..=255` and full-width indices `>= 256`. The owned graph checks every
        entry against the body-declared kind and charges every entry to
        constant budgets, but only `Function` entries participate in
        topology, depth, reachability, and closure-edge work. The compiler and
        graph verifier use explicit work lists with no `recursion_guard`
        dependency. `Binary64Constant` preserves all non-NaN binary64 bits and
        canonicalizes NaN only for deterministic compiler artifacts; runtime
        Number, `DataView`, and typed-array payload semantics remain separate.
        Directly negated `2^31` is normalized to `push_i32(i32::MIN)` without
        retaining an unused positive Number entry. Ownership and candidates
        are precomputed in one semantic-node pass with per-owner compact lookup
        tables rather than rescanning the graph for each function.
  - [x] Add exact source strings and owned function-local atom tables. A shared
        frontend decoder converts Oxc cooked strings to arena-independent
        UTF-16, preserving lone surrogates. `CompilerString` freezes them into
        canonical Latin-1 or UTF-16 `Arc` storage with the QuickJS length cap.
        Empty strings use `push_empty_string`; canonical decimal strings from
        `"0"` through `"2147483647"` remain non-deduplicated String values in
        the heterogeneous pool; all other nonempty strings use deduplicated
        `push_atom_value` entries. Quoted and no-substitution template literals
        share routing and atom contents. Directives leave no dead payloads; this
        intentionally omits QuickJS's unobservable numeric-directive constant
        artifact while preserving directive semantics.
        Whole-graph verification owns exact atom tables, rejects duplicates,
        and bounds aggregate atom entries and compact string payload bytes.
  - [x] Cross-check compiler function trees as bounded flat graphs:
        plan-global executable identities are explicitly remapped to dense
        template identities; aggregate body and capture-edge work is charged
        before detailed scans; function targets, cycles, reachability, depth,
        and normalized capture sources are verified with iterative work
        queues. The immutable `Arc<VerifiedCompilerFunctionGraph>` is retained
        as a staged certificate with `CompiledFunctionTree`, while selected
        roots needing an omitted parent environment fail closed.
  - [x] Final-verify complete metadata for the current ordinary Oxc compiler
        profile into immutable, `Arc`-backed `VerifiedBytecode`. The final pass
        checks exact function headers, vardef names/policies/scope links,
        dense own variable references, imported closure descriptors, child
        ownership/names, and parent-edge closure name/policy/source agreement.
        Function declarations name their exact child constant; one isolated
        `fclosure*; put_arg*` or `fclosure*; put_loc*` pair must initialize it,
        with function-instantiation pairs in the entry prefix and block pairs
        following explicit lexical activation. An iterative CFG analysis
        separates binding value state from captured-cell open/closed state and
        rejects TDZ, initialization, write-policy, inactive-capture, and cell
        rotation violations. Six aggregate limits bound definitions, closure
        descriptors, retained source, source mappings, frame-state cells, and
        policy-transfer work. The authority retains sorted conservative
        runtime requirement families and exact source snapshots after Oxc
        teardown. The authority remains runtime-independent; the first
        same-runtime materialization profile is tracked separately below.
        Serialized bytecode, full exceptional typed-stack verification,
        incoming source-map chaining, and direct eval remain pending.
  - [x] Normalize strict block-function initialization in two private phases:
        activate the lexical/captured cell, then install the declaration
        closure in a verifier-isolated linear group before user code. This
        preserves successful JavaScript behavior while making cell lifetime
        explicit. Annex B remains rejected rather than approximated.
  - [x] Add the first runtime-installed verified interpreter profile:
        `Context::instantiate` scans every opcode in every template before a
        failure-atomic code/root-function commit in an existing validated
        realm; `Context::call` starts only at verified instruction zero, checks
        certified entry stack depths, and follows only verified successors.
        The profile executes primitive
        constants, arguments/locals/captures, nested and forwarded closures,
        TDZ checks, `close_loc` rotation, branches, returns, truthiness,
        `typeof`, strict equality, nullish tests, ordinary objects, static data
        property operations, strict `this`, and static-property method calls
        across compact/full encodings. Frames and all traversals use explicit
        vectors. BigInt, coercive numeric operations, computed/accessor/exotic
        object operations, dynamic operators, and constructor/tail-call
        opcodes remain fail closed.
  - [x] Add direct ordinary JavaScript-to-JavaScript calls end to end:
        lowering evaluates the callee then arguments left-to-right and emits
        `call0`–`call3` or full `call`; final authority records the explicit
        `Calls` requirement; runtime dispatch parks callers at their verified
        call PCs and pushes child frames onto the existing vector. Missing
        formals become `undefined`, presently unobservable extra arguments are
        evaluated then discarded, frame/value ceilings and instruction fuel
        are cumulative, recursive execution never consumes the Rust stack,
        and non-callable values throw exact `TypeError: not a function`.
        Escaping child exceptions retain immediate-to-outer caller PCs and
        source spans. Static-property methods now use the same frame vector and
        preserve their raw receiver; optional/spread/apply, constructors, tail
        calls, sloppy-`this` normalization, `arguments`, and direct eval remain
        fail closed.
  - [ ] Execute ordinary `Function(...)` and `new Function(...)` without eval:
        convert already-evaluated arguments in order, retain the exact
        QuickJS-compatible wrapper and fragment map, compile the complete
        generated Script through published Oxc plus `oxc_semantic`, verify the
        complete result, and install/execute it in the constructor realm's
        global environment without capturing the caller frame. Wrapper escape
        means this path must return the Script completion rather than extract
        an assumed child function AST. Direct eval in generated code remains
        rejected; Eval/ApplyEval are never used. GeneratorFunction,
        AsyncFunction, and AsyncGeneratorFunction remain fail closed.
    - [x] Add the host/internal ordinary-Function entry for already-coerced
          fragments: isolated Oxc parsing and semantics, complete Script-root
          lowering, named-self initialization, whole-graph verification,
          constructor-realm global receiver, exact completion publication,
          and failure-safe internal-root retirement. Preserve wrapper escape
          and reject direct eval and all nonordinary constructor families.
    - [ ] Add realm-global declaration instantiation and unresolved-name
          lookup, sloppy-function `this` normalization, ordered JavaScript
          `ToString` argument conversion, the global `Function` call/construct
          object, and `new_target.prototype` post-processing.
- [ ] General abrupt completion, catch/throw/finally, rooted exception values,
      stack traces, iterators, and generators.
  - [x] First escaping exception path: local/captured TDZ access returns a
        structured `ReferenceError` with the exact QuickJS message, function
        template, verified bytecode PC, source name, and retained source span.
  - [x] Direct-call failures add `TypeError: not a function`; child exceptions
        preserve their origin and retain verified caller call-site frames.
  - [x] Add arbitrary explicit `throw` end to end: Oxc `ThrowStatement`
        lowering emits a verified one-value terminal; the VM transports
        engine errors and explicit values through one private typed abrupt
        path while unwinding the existing frame vector; provenance allocation
        precedes publication of an escaping heap root; cloned exceptions share
        one `Arc` root header; and handler/finally opcodes remain fail-closed.
- [ ] Deterministic debug/line tables.

### M3 — values and object model

- [x] JavaScript UTF-16 string primitive with lone-surrogate preservation,
      Latin-1 leaves, depth-bounded ropes, and QuickJS-compatible length limits.
- [x] JavaScript Number representation with signed-zero preservation, int32
      fast paths, overflow promotion, and all three numeric equality modes.
- [x] Canonical property-key array-index recognition through `2^32 - 2`.
- [x] Runtime-local owning atoms, weak UTF-16 content interning, exact
      predefined atoms, global/unique/well-known symbols, private names,
      validated public property keys, and bounded logical usage.
- [x] Opaque generic/data/accessor descriptor classification with exact field
      presence, new-property completion defaults, and value-independent
      ordinary data/accessor layouts.
- [x] Add the runtime ownership foundation: immutable `Arc` public root
      headers, allocation-free deferred release, runtime-identified typed
      generational arenas, runtime-local functions/cells, exact logical
      resource ceilings, and iterative safe-point tracing that reclaims
      transient closures and function/cell cycles. Deterministic strong-count
      release, the complete future exotic/weak/finalizable graph, and
      finalization remain pending below.
- [x] Add the first ordinary-object execution slice: typed object IDs and
      `Arc` public roots, one realm-owned `Object.prototype`, fallibly grown
      data-only shapes/slots, object literals, duplicate-key replacement,
      static reads and simple writes across objects/functions, strict
      receiver-aware method calls, exact nullish/primitive errors, aggregate
      object/property limits, and iterative tracing across prototype,
      property, function, and cell edges. Computed keys, accessors, prototype
      mutation, exotics, and transition interning remain pending.
- [ ] ECMAScript primitive conversions, parsing/printing, and remaining numeric
      edge cases.
- [ ] Complete property descriptors, interned shapes, mutable prototypes, and
      exotic objects.
- [ ] Dense/sparse arrays and typed indexed access.
- [ ] Deterministic reference ownership plus cycle collection with explicit
      roots and finalization rules.
- [ ] Runtime memory limit, stack limit, interrupt hook, and diagnostics.

### M4 — built-ins and asynchronous semantics

- [ ] Fundamental objects, functions, errors, reflection, and proxies.
- [ ] Number, BigInt, String, RegExp, Date, JSON, and structured data.
- [ ] Collections, ArrayBuffer, TypedArray, DataView, Atomics, and shared data.
- [ ] Resizable/transferable ArrayBuffer, iterator helpers, Set methods,
      Map/WeakMap upsert, `Atomics.pause`, and `Math.sumPrecise`.
- [ ] Uint8Array base64/hex codecs and duplicate named RegExp capture groups.
- [ ] Promise jobs, async functions/generators, weak references, and
      finalization registries.
- [ ] Tokio-backed timers, I/O readiness, cancellation, and wakeups behind a
      QuickJS-compatible, deterministic JavaScript job queue.
- [ ] Compressed Unicode property, normalization, and case-mapping tables.

### M5 — modules, embedding, and tools

- [x] First runtime/realm/context foundation with bounded realm creation,
      same-runtime handle validation, verified function installation,
      primitive value constructors, and host invocation.
- [ ] Full embedding API with Rust-native host functions, classes, modules,
      callbacks, and exception conversion.
- [ ] Module linking, cyclic graphs, dynamic import, and top-level await.
- [ ] Evaluate Oxc Resolver as an implementation aid without inheriting its
      Node defaults; preserve QuickJS relative/system module-name semantics.
- [ ] Minimal `qjs` runtime and ESM-aware REPL with multiline input, static
      and dynamic modules, top-level await, limits, and clean Tokio shutdown.
- [ ] `qjs` CLI script/module detection and documented options.
- [ ] Rust-native `qjsc` artifact generation with no C compiler dependency.
- [ ] Bytecode viewer CLI with verified function metadata, resolved atoms and
      constants, control-flow targets, and source-map annotations.
- [ ] CDP inspection adapter with safe-point pause/step/breakpoints, scopes,
      object previews, exceptions, console events, and source-mapped locations.
- [ ] Portable `std`/`os` modules with documented platform and safety policy.

### M6 — conformance and performance

- [ ] Upstream built-in language, closure, BigInt, and module suites.
- [ ] test262 runner at `5c8206929d81b2d3d727ca6aac56c18358c8d790`,
      with the upstream patch, configuration, exclusions, and baseline report.
- [ ] Differential corpus against QuickJS 2026-06-04.
- [ ] Fuzz parser, bytecode verifier, serializer, and runtime boundaries.
- [ ] Startup, memory, interpreter, and compile-time benchmark baselines.
- [ ] Zero unexplained crashes or undefined behavior under supported sanitizers
      and interpreters.
- [ ] Public API review: SemVer policy, feature matrix, structured errors,
      rustdoc examples, embedding examples, and no undocumented panics.
- [ ] Source-map audit: byte offsets, Unicode line/column conversion,
      generated-to-original chaining, stack traces, eval/modules, and malformed
      map handling.
- [ ] Production audit: supported-platform matrix, resource limits,
      cancellation/shutdown, malformed-input hardening, dependency policy, and
      reproducible release artifacts.

### M7 — optional compatibility layers

- [ ] Wasmtime-backed JavaScript `WebAssembly` API with bounded compilation,
      execution, memory, tables, imports/exports, and exception conversion.
- [ ] Safe N-API semantic core plus an isolated, audited Rust C-ABI adapter
      with no C/C++ source, bindgen, or C compiler.
- [ ] Opt-in erasable TypeScript stripping that emits a mandatory source map
      before entering the ordinary JavaScript frontend.
- [ ] Low-priority `serde` bridge for bounded, policy-controlled
      JavaScript-object-to-Rust serialization and Rust-to-JavaScript
      deserialization, including cycles, accessors, symbols, BigInt, typed
      arrays, and exception behavior.
- [ ] Feature, platform, conformance, diagnostics, cancellation, and security
      matrices for every optional layer.

## Engineering rules

1. Add a failing regression or conformance test before each semantic change.
2. Use QuickJS 2026-06-04 as the only JavaScript-runtime implementation
   reference. Oxc is the explicitly selected parser; do not consult, copy,
   adapt, or depend on another JavaScript engine, port, VM, garbage collector,
   or RegExp implementation. General-purpose Rust crates are permitted when
   they do not supply alternate JavaScript runtime semantics.
3. Keep parser, compiler, VM, and host concerns separable even when they share
   a crate initially.
4. Encode bytecode operands and heap handles with validated newtypes.
5. Performance changes may depart from QuickJS's private representation, but
   must retain observable behavior, start from a profile, and add a benchmark.
   Never use `unsafe` as an optimization escape hatch.
6. Preserve upstream copyright notices for translated source and generated
   tables.
7. Commit milestones as small, bisectable changes after all relevant gates
   pass.
8. Track the latest stable Rust toolchain. Nightly-only work must be isolated,
   justified, and must not silently become a runtime requirement.
9. Use Tokio for host async I/O and event-loop driving, never as a substitute
   for the ECMAScript job queue or QuickJS Promise ordering.
10. Carry source identity and spans from Oxc through bytecode and runtime stack
    frames. Keep stable structured diagnostics separate from Miette/CLI
    rendering so embedders never need to scrape text.
