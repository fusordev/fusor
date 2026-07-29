# Dependency policy

QuickJS 2026-06-04 remains the sole JavaScript-runtime implementation
reference. General-purpose Rust crates are allowed, but a dependency must have
a narrow documented role and must not import an alternate JavaScript runtime,
VM, garbage collector, built-in implementation, or RegExp engine.

## Current production dependencies

### Oxc 0.142.0

Oxc is the user-selected JavaScript parser. The workspace exactly pins its
parser-facing crates so an upstream AST or diagnostic change cannot silently
alter compilation:

- official parser guide: <https://oxc.rs/docs/guide/usage/parser.html>;
- `oxc_allocator`
- `oxc_ast`
- `oxc_diagnostics`
- `oxc_parser`
- `oxc_semantic`
- `oxc_span`
- `oxc_syntax`

`oxc_parser` has default features disabled. In particular, its
`regular_expression` parser feature is not enabled: QuickJS-derived code will
implement RegExp pattern semantics. `oxc_regular_expression` is still present
transitively because `oxc_ast` uses its types, but the front-end tests verify
that validly delimited patterns are deferred to the runtime layer.

Every source unit is rejected if Oxc reports either a parser diagnostic or a
deferred semantic early error. TypeScript, JSX, V8 intrinsics, and Oxc's
unambiguous source mode are not exposed by the engine front end.

Successful units retain Oxc's `ModuleRecord` and complete `Semantic` model so
the compiler receives module requests, AST-node mapping, class/private-name
analysis, scopes, symbols, and resolved/unresolved references directly. These
are syntax-analysis inputs, not QuickJS runtime storage locations or
declaration-instantiation semantics.

`quickjs-compiler` consumes that retained semantic model directly only while
the front-end arena is live. Its first storage-planning boundary immediately
lowers the needed facts into compiler-owned dense IDs, copied spans, immutable
`Arc`-backed slices, and resolved-reference-to-binding edges grouped by their
using executable. It does not clone the Oxc semantic graph, expose Oxc
node/scope/symbol identities, or keep any arena reference in a successful plan.

The front end additionally lowers static module syntax into an
arena-independent, QuickJS-owned record. Oxc supplies the parsed request and
entry facts; the project-owned representation preserves source occurrence
order, import attributes, linking roles, and exact UTF-16 string code units
with immutable `Arc` backing. It does not duplicate or replace Oxc semantic
tables.

The selected compatibility policy permits narrow Oxc-vs-QuickJS parser
differences. Such differences must be recorded and covered by differential or
expectation fixtures; they do not authorize importing behavior from another
JavaScript runtime or changing QuickJS-derived runtime semantics.

### Diagnostics and source maps

The reusable diagnostics layer exactly pins:

- `miette` 7.6.0 (Apache-2.0) for optional, color-free human rendering;
- `sourcemap` 9.3.2 (BSD-3-Clause) for standard source-map v3 decoding and
  greatest-lower-bound token lookup;
- `serde_json` 1.0.151 (MIT OR Apache-2.0) for bounded structural validation
  before source-map decoding.

Canonical diagnostic codes, messages, severities, labels, source ownership,
line/column conversion, map-chain limits, and structured failures remain
project-owned APIs. Miette output is presentation rather than a compatibility
surface. The source-map dependency supplies the standard interchange codec; it
does not supply JavaScript parser, compiler, VM, runtime, or RegExp semantics.

### Immutable string ownership

Immutable JavaScript string leaves and rope nodes use the standard library's
thread-safe `Arc`. Immutable `JsString` handles remain `Send + Sync` for host
integration without making a JavaScript runtime, context, or heap transferable
between threads. A process-wide empty-string node and cloned string handles
share the same immutable backing representation.

`Arc` is backing-storage infrastructure, not the runtime's reachability
algorithm. It does not decide JavaScript object liveness, weak-reference
visibility, finalizer ordering, cycle removal, or ECMAScript job ordering. The
object heap retains QuickJS-derived logical reference counts and explicit cycle
deletion. Future runtime memory accounting must charge backing bytes at node
creation and release them from the node's synchronous destruction path. `Arc`
control-block allocation follows Rust's global allocator policy.

## Approved planned dependencies

- Tokio is required for host async I/O, timers, wakeups, and event-loop
  integration. It must not determine ECMAScript job ordering.
- `parking_lot` is required for shared mutable host-side state. It is not used
  to make runtimes, contexts, heaps, or JavaScript value handles cross-thread,
  and immutable string backing remains lock-free.

Dependency additions and upgrades require current official documentation,
license review, complete workspace gates, and focused compatibility tests.
