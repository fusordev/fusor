# Development oracles

`xtask differential` compares process status, stdout, and stderr for each
`.js`/`.mjs` fixture under `tests/differential`. Both processes have a bounded
runtime, and fixtures run in deterministic path order.

Build the pinned upstream release outside this repository, then run:

```console
cargo build -p qjs-cli
cargo xtask differential \
  --oracle /path/to/quickjs-2026-06-04/qjs \
  --candidate target/debug/qjs
```

The candidate command will become available with the executable vertical
slice. The oracle is optional development input and is never linked or copied
into the Rust artifacts.

`xtask parser-differential` compares the reusable in-process Oxc front end with
the pinned upstream `qjs` parser. Fixtures declare Script/Module mode and the
expected accept/reject result through their directory under `tests/parser`.

```console
cargo xtask parser-differential \
  --oracle /path/to/quickjs-2026-06-04/qjs
```

All parser fixtures are deliberately inert because the upstream CLI executes
them after parsing. The command first verifies the exact
`QuickJS version 2026-06-04` banner and counts only an upstream `SyntaxError` as
a syntax rejection; timeouts, signals, loader errors, and runtime exceptions
fail the harness. Runtime output remains the responsibility of
`xtask differential`.
