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

- `oxc_allocator`
- `oxc_ast`
- `oxc_diagnostics`
- `oxc_parser`
- `oxc_semantic`
- `oxc_span`

`oxc_parser` has default features disabled. In particular, its
`regular_expression` parser feature is not enabled: QuickJS-derived code will
implement RegExp pattern semantics. `oxc_regular_expression` is still present
transitively because `oxc_ast` uses its types, but the front-end tests verify
that validly delimited patterns are deferred to the runtime layer.

Every source unit is rejected if Oxc reports either a parser diagnostic or a
deferred semantic early error. TypeScript, JSX, V8 intrinsics, and Oxc's
unambiguous source mode are not exposed by the engine front end.

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

## Approved planned dependencies

- Tokio is required for host async I/O, timers, wakeups, and event-loop
  integration. It must not determine ECMAScript job ordering.

Dependency additions and upgrades require current official documentation,
license review, complete workspace gates, and focused compatibility tests.
