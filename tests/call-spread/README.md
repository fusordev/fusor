# Call spread differential corpus

This corpus pins `f(...arguments)` and `new C(...arguments)` behavior to
QuickJS 2026-06-04. Every case's `expect` was derived from the pinned release
source (`js_parse_call_expression` argument packing and `OP_apply` /
`js_function_apply` execution) and must be verified with:

```
cargo xtask call-spread-differential --oracle /path/to/qjs
```

Cases deliberately avoid features outside the current ordinary profile:

- the global `undefined` binding is unresolved, so iterator `done` results
  omit the `value` field rather than spelling `undefined`;
- `Array.prototype.push`/`join`/`shift` are not yet installed, so cases
  return arithmetic or string-concatenation results instead;
- function-expression name inference is still fail-closed, so object method
  values use named function expressions.
