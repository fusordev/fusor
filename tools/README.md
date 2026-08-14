# Development oracles

`xtask differential` compares process status, stdout, and stderr for each
`.js`/`.mjs` fixture under `tests/differential`. Both processes have a bounded
runtime, and fixtures run in deterministic path order.

Build the pinned upstream release outside this repository, then run:

```console
cargo build -p fusor
cargo xtask differential \
  --oracle /path/to/quickjs-2026-06-04/qjs \
  --candidate target/debug/fusor
```

The oracle is optional development input and is never linked or copied
into the Rust artifacts.

## `fusor` (crates/fusor, bin target)

`cargo build -p fusor` produces `target/debug/fusor`:

```console
target/debug/fusor run file.mjs    # evaluate file.mjs as an ES module
target/debug/fusor file.mjs        # same, with the default subcommand
target/debug/fusor --script f.js   # evaluate f.js as a classic script
target/debug/fusor repl            # start the ESM REPL
```

`fusor <file>` exits 0 on success, 1 on a JavaScript or pipeline error
(message printed to stderr), and 2 on usage or file IO errors. Arguments
after `<file>` are exposed through `node:process` `argv`. `print` writes
to stdout through the CLI overlay's init shim (`fusor:cli`, delegating to
`Fusor.ops.op_core_print` — no host-installed global), so `xtask
differential` fixtures keep their bare `print` spelling.

### Module resolution (host sugar, non-normative)

The `NodeLikeResolver` maps specifiers to filesystem paths and a small
builtin table:

- `./`/`../` specifiers resolve against the referrer's directory, absolute
  specifiers are used as-is. Resolution is exact; an extension-less path
  falls back to `<path>.mjs` then `<path>.js`, in that order. Directory
  specifiers are rejected (no `index.js`/`package.json` lookup), and bare
  specifiers other than the builtin names are rejected (no `node_modules`
  lookup).
- Builtins: `node:assert` (`ok`, `equal`, `strictEqual`, `notStrictEqual`,
  `deepStrictEqual`, `throws`, `fail`), `node:path` (POSIX-only
  `join`/`dirname`/`basename`/`extname`/`normalize`/`resolve`/`isAbsolute`/
  `sep`/`delimiter`), and `node:process` (`argv`, `env` as an empty object,
  `platform`, `cwd()`), each importable with or without the `node:` prefix.
- The runtime linker currently resolves requests by raw specifier text, so
  modules are registered under the exact importing specifier rather than a
  canonical `file://` key (the canonical, lexically normalized `file://`
  path is still used as the display name). One specifier text resolving to
  two different files is detected and reported as a load error; the same
  file imported through two different texts is evaluated once per text.

### ESM REPL (host sugar, not a spec module record)

A session owns one realm and approximates docs/MODULES.md's "ESM REPL" within
the facade's single-shot module pipeline:

- Entries without top-level `import`/`export` syntax evaluate as classic
  scripts; their completion value is printed (numbers, quoted strings,
  `undefined`/`null`/booleans, shallow placeholders for objects) and global
  bindings persist for later entries.
- Entries with module syntax evaluate as modules named
  `file://<cwd>/__repl_entry_<n>.mjs`, so relative imports resolve against
  the current working directory. Complete single-line `import` statements
  from successful module entries accumulate into a prefix prepended to
  later module entries.
- Consequences: imported modules are re-registered and re-evaluated on
  every module entry (side effects repeat), script entries cannot see
  module-scoped bindings, and multi-line import statements are not
  accumulated. Dynamic `import()` and top-level await are not implemented
  by the runtime and fail with a clear error.
- `.exit` or Ctrl-D quits; input continues across lines while brackets are
  unbalanced (a basic heuristic that ignores strings and comments).
  Uncaught errors print to stderr and the session continues.

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
