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
