# QuickJS in pure Rust

A safe, source-level Rust port of [QuickJS](https://bellard.org/quickjs/),
targeting the upstream **2026-06-04** release and its ES2025 semantics.

> [!IMPORTANT]
> This is an in-progress port, not yet a complete JavaScript engine or a
> drop-in QuickJS replacement. Unsupported behavior is intended to fail closed.

## Scope

- Pure Rust engine core: no C/C++ source, bindgen output, or C compiler in the
  build or runtime path. The optional N-API adapter is the isolated foreign-ABI
  boundary.
- QuickJS is the sole runtime-semantics reference. Oxc is used only for parsing
  and semantic analysis; the upstream C engine is a differential-test oracle,
  never a linked or shipped dependency.
- Only whole-function, typed, graph-verified bytecode may execute. Raw and
  serialized bytecode, and direct `eval`, remain fail closed.
- The core crates forbid `unsafe`; Rust-native changes must preserve observable
  behavior under differential tests.

## Current profile

The implemented, tested subset includes ordinary functions and closures;
bindings and TDZ; control flow, exceptions, and `try`/`catch`/`finally`;
ordinary objects and accessors; arrays, synchronous iterators, array spread,
and `for-of`; operators and coercion; and the initial Boolean, Number, String,
Symbol, Function, Array, and Error families.

Key gaps remain: complete Error and reflection surfaces, most built-ins,
BigInt domains, `eval`, destructuring iterators, async/generators, modules,
Promises/jobs, RegExp, binary data, and the public embedding/tooling surface.
See [PORTING.md](PORTING.md) for the authoritative checklist and compatibility
boundaries.

## Workspace

| Crate | Responsibility |
| --- | --- |
| `quickjs-diagnostics` | Sources, spans, diagnostics, and source maps |
| `quickjs-frontend` | Oxc parsing and owned frontend records |
| `quickjs-bytecode` | Instructions, verifier, codec, and debug data |
| `quickjs-compiler` | Oxc lowering to verified bytecode |
| `quickjs-runtime` | Values, heap, realms, VM, and built-ins |
| `quickjs` | Ergonomic host facade |

Architecture and trust-boundary details are in
[ARCHITECTURE.md](ARCHITECTURE.md) and
[BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md). See [UPSTREAM.md](UPSTREAM.md)
for the pinned reference and [DEPENDENCIES.md](DEPENDENCIES.md) for dependency
policy.

## Development

Use the current stable Rust toolchain (the workspace pins its minimum version).
Run the normal local gates from the repository root:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

For a changed compatibility area, run its matching differential corpus against
the pinned upstream QuickJS oracle, for example:

```console
cargo xtask parser-differential \\
  --oracle /path/to/quickjs-2026-06-04/qjs
cargo xtask dynamic-function-differential \\
  --oracle /path/to/quickjs-2026-06-04/qjsc
```

Additional corpora cover Number radix conversion, control flow,
`Function.prototype.apply`/`bind`, iterators, call spread, and Errors. Their
manifests are expanding compatibility gates, not claims of exhaustive QuickJS
coverage.

## License

MIT. The original QuickJS copyright and permission notice are retained in
[LICENSE](LICENSE).
