# QuickJS in pure Rust

This repository is a source-level port of
[QuickJS](https://bellard.org/quickjs/) to safe, pure Rust. The compatibility
target is the official **2026-06-04** release and its ES2025 language surface.

> [!IMPORTANT]
> The port is in its bootstrap phase. It is not yet a JavaScript engine and is
> not a drop-in replacement for QuickJS.

## Contract

- No C, C++, FFI, bindgen, or C compiler in the build or runtime path.
- The pinned QuickJS release is the sole JavaScript-runtime implementation
  reference. Oxc is the explicitly selected JavaScript parser; no other
  engine, port, VM, garbage collector, or RegExp implementation is consulted
  or reused.
- General-purpose Rust crates may provide infrastructure, but they are not
  semantic references. Observable JavaScript behavior comes from the pinned
  QuickJS release and its compatibility tests.
- `unsafe` Rust is forbidden at the workspace level.
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

The implementation plan and compatibility gates live in
[PORTING.md](PORTING.md). The exact upstream provenance is recorded in
[UPSTREAM.md](UPSTREAM.md), and the external-crate policy is recorded in
[DEPENDENCIES.md](DEPENDENCIES.md).

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
