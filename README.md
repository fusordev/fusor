# QuickJS in pure Rust

This repository is a source-level port of
[QuickJS](https://bellard.org/quickjs/) to safe, pure Rust. The compatibility
target is the official **2026-06-04** release and its ES2025 language surface.

> [!IMPORTANT]
> The port is in its bootstrap phase. It is not yet a JavaScript engine and is
> not a drop-in replacement for QuickJS.

## Contract

- No C, C++, FFI, bindgen, or C compiler in the build or runtime path.
- `unsafe` Rust is forbidden at the workspace level.
- Preserve observable ECMAScript behavior, not QuickJS's private in-memory
  representation.
- Match QuickJS's documented omissions: proper tail calls and
  `Atomics.waitAsync` are out of scope until the upstream target implements
  them; ECMA-402 `Intl` is a separate optional layer.
- Treat QuickJS bytecode as a version-private reference format. The Rust port
  will use a checked, memory-safe bytecode format rather than load untrusted
  upstream bytecode.
- Keep every milestone runnable, tested, and recorded in Git.

The implementation plan and compatibility gates live in
[PORTING.md](PORTING.md). The exact upstream provenance is recorded in
[UPSTREAM.md](UPSTREAM.md).

## Development

The repository pins its Rust toolchain. The standard local gates are:

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
