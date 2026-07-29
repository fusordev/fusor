# Dynamic Function parser compatibility corpus

This corpus compares `quickjs-frontend`'s Oxc-backed dynamic Function adapter
with the pinned QuickJS 2026-06-04 `qjsc` compiler.

Run it explicitly with:

```text
cargo xtask dynamic-function-differential --oracle /path/to/qjsc
```

Fixtures are JSON objects under `accept/` or `reject/`:

```json
{
  "kind": "function",
  "parameters": ["left", "right"],
  "body": "return left + right;"
}
```

`kind` is one of `function`, `generator`, `async`, or `async-generator`.
Parameter strings remain separate constructor arguments; the body is the final
argument. Directory placement declares the acceptance expectation for both
QuickJS and the Rust candidate.

Fixtures must use literal, scalar-valid UTF-8. JSON `\u` escapes are rejected so
lone UTF-16 surrogates can never be decoded through a lossy Rust `String`
boundary. The runner also bounds fixture count, nesting, file bytes, fragment
count/bytes, generated wrapper bytes, compiler output bytes, and per-oracle
execution time and stdout/stderr bytes.

The candidate's exact generated wrapper is written into a dedicated, uniquely
created temporary directory. `qjsc -c` parses and compiles it only to generated
C data; no C compiler is invoked and no JavaScript is executed. This keeps
wrapper-escape fixtures parse-only. The runner validates the qjsc release
banner, inspects the bounded output artifact, and reports any cleanup failure.
