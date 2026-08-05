# Pinned Test262 baseline

These three files are copied verbatim from QuickJS `2026-06-04` and form one
baseline with Test262 commit `5c8206929d81b2d3d727ca6aac56c18358c8d790`:

- `test262.patch` must already be applied to the external Test262 checkout;
- `test262.conf` supplies the upstream feature skips and explicit exclusions;
- `test262_errors.txt` records the upstream release's known failures.

The suite itself is not vendored. Prepare a checkout with LF line endings, pin
it to the commit above, apply `test262.patch`, then run:

```sh
cargo xtask test262 --suite /path/to/test262 --inventory-only
cargo xtask test262 --suite /path/to/test262 --filter built-ins/Array --report target/test262.json
```

The runner rejects another revision, `core.autocrlf=true`, or an unpatched
checkout. ECMA-402 paths are inventoried as low-priority skips.
