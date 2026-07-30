# QuickJS in pure Rust

This repository is a source-level port of
[QuickJS](https://bellard.org/quickjs/) to safe, pure Rust. The compatibility
target is the official **2026-06-04** release and its ES2025 language surface.

> [!IMPORTANT]
> The port is in its bootstrap phase. Its first verified interpreter profile is
> executable, but it is not yet a complete JavaScript engine or a drop-in
> replacement for QuickJS.

The current ordinary-function compiler profile now freezes its staged
function graph, exact binding/closure metadata, and retained source snapshots
as immutable, `Arc`-backed `VerifiedBytecode`. A runtime context can
transactionally install that authority into a same-runtime realm and host-call
the resulting function. The admitted profile executes primitive constants,
arguments, locals, captured cells, nested closures, TDZ checks, cell rotation,
branches, returns, direct ordinary JavaScript-to-JavaScript calls, truthiness,
`typeof`, strict equality, nullish tests, ordinary object literals, static data
property reads/writes, strict receiver-aware static method calls, and arbitrary
explicit `throw` values. Functions and ordinary objects use typed `Arc`-backed
public roots, and the iterative collector traces their properties, prototypes,
closures, and binding cells. Nested calls, recursion, and abrupt unwinding use
an explicit frame vector with cumulative frame/value ceilings and shared fuel.
Installation scans every instruction in every template before mutation.
Computed/accessor/exotic object operations, optional/spread/apply calls,
BigInt, general coercive operations, dynamic operators, serialized bytecode,
every form of eval, and catch/finally typed-stack semantics remain deferred
and fail closed. Ordinary `new` calls now execute constructor-capable bytecode
functions and materialize their `name`, `length`, and
`prototype.constructor` graph. The `quickjs` facade supplies one immutable
`Arc` Oxc compiler service to the global `Function` call/constructor path: it
retains the exact wrapper/map, compiles the complete generated Script,
executes only whole-graph `VerifiedBytecode` with a constructor-realm global
receiver, and returns the exact Script completion. Unresolved names use typed
constructor-realm lookup/write slots, and sloppy dynamic functions normalize
`this` lazily against their installed constructor realm. Escaped Program
`var` and function declarations now create configurable constructor-realm data
properties, with correct existing-property handling, function hoisting,
duplicate-last-wins initialization, and failure-atomic descriptor preflight.
Escaped `let` and `const` remain
evaluation-local TDZ cells and can survive only through escaping closures.
The intrinsic descriptors, call/new realm selection, wrapper escape,
`newTarget.prototype` adjustment, SyntaxError boundary, and primitive
undefined/null/Boolean/Number/String source coercions are implemented.
Object/function `ToPrimitive`, configurable accessor replacement, persistent
global lexical collisions, and the rest of `Function.prototype` remain
fail-closed. Per-session compilation-count and generated-source limits bound
nested construction. No dynamic-Function path uses eval or captures a caller
lexical frame.

## Contract

- No C or C++ source, bindgen output, or C compiler in the engine build or
  runtime path. The optional N-API adapter is the sole isolated foreign-ABI
  boundary and is written in Rust.
- The pinned QuickJS release is the sole JavaScript-runtime implementation
  reference. Oxc is the explicitly selected JavaScript parser; no other
  engine, port, VM, garbage collector, or RegExp implementation is consulted
  or reused.
- General-purpose Rust crates may provide infrastructure, but they are not
  semantic references. Observable JavaScript behavior comes from the pinned
  QuickJS release and its compatibility tests.
- Core engine, compiler, runtime, host, and tool crates forbid `unsafe` Rust.
  Any C-ABI pointer handling required by the optional N-API adapter is confined
  to its boundary crate and audited separately.
- Preserve observable ECMAScript behavior, not QuickJS's private in-memory
  representation.
- Rust-native performance changes are allowed when differential tests preserve
  behavior and benchmarks demonstrate the tradeoff.
- Tokio is the host async I/O and event-loop substrate. The QuickJS-derived
  runtime retains authority over ECMAScript jobs and Promise ordering.
- Match QuickJS's documented omissions: proper tail calls and
  `Atomics.waitAsync` are out of scope until the upstream target implements
  them; ECMA-402 `Intl` is a separate optional layer.
- Treat QuickJS bytecode as a version-private reference format. The Rust port
  will use a checked, memory-safe bytecode format rather than load untrusted
  upstream bytecode.
- Keep every milestone runnable, tested, and recorded in Git.
- Keep production logic in reusable library crates with documented, stable
  APIs. The `qjs` and `qjsc` binaries remain thin consumers of those libraries.
- Preserve exact structured error data and source spans, provide
  human-readable Miette rendering, and carry source maps through compilation
  and stack traces.

The runtime/compiler boundaries and safety invariants live in
[ARCHITECTURE.md](ARCHITECTURE.md), and the implementation plan and
compatibility gates live in [PORTING.md](PORTING.md). The exact upstream
provenance is recorded in [UPSTREAM.md](UPSTREAM.md), the hardened bytecode
trust boundary is specified in [BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md),
the external-crate policy is recorded in
[DEPENDENCIES.md](DEPENDENCIES.md), and the ESM REPL, bytecode viewer, CDP,
Wasmtime, N-API, and TypeScript-strip surfaces are specified in
[EXTENSIONS.md](EXTENSIONS.md).

## Development

The repository follows the latest stable Rust toolchain. Nightly may be used
only for an isolated, documented requirement; stable remains the release
baseline. The standard local gates are:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

The upstream C engine may be built separately as a development oracle for
differential testing. It is never linked into or shipped with this project.

## License

MIT. The original QuickJS copyright and permission notice are preserved in
[LICENSE](LICENSE).
