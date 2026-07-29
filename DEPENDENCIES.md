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

## Approved planned dependencies

- Tokio is required for host async I/O, timers, wakeups, and event-loop
  integration. It must not determine ECMAScript job ordering.
- Miette may render structured diagnostics. Library errors must retain stable
  codes, exact messages, spans, and sources independently of that renderer.
- A standard source-map crate may be used for version-3 map decoding/encoding
  and chaining. Bytecode PC/source tables remain owned by this project.

Dependency additions and upgrades require current official documentation,
license review, complete workspace gates, and focused compatibility tests.
