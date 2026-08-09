# Test262 filter policy

The complete Test262 suite is not vendored. The manually dispatched GitHub
workflow clones the current upstream default branch for each run. The runner
requires an unmodified checkout with LF line endings and applies this
repository's policy from `test262.conf`:

- `[features]` entries marked `=skip` remain skipped;
- `[exclude]` paths remain excluded, including ECMA-402 and unfinished Annex B;
- narrower `[include]` paths override a parent exclusion as compatibility
  tranches become complete.

The policy is intentionally independent of a particular upstream revision, so
new upstream tests are classified by the same engine-support boundary. The
report records the checked-out commit and the policy fingerprint.

`test262.patch` and `test262_errors.txt` are retained verbatim as QuickJS
`2026-06-04` provenance artifacts. They are not applied to, or used to filter,
the current upstream Test262 checkout.

To inspect a bounded local selection without executing the full suite:

```sh
cargo xtask test262 --suite /path/to/test262 --inventory-only
cargo xtask test262 --suite /path/to/test262 --filter built-ins/Array --report target/test262.json
```

Run the complete configured suite with an optimized runner. Unfiltered,
unlimited execution fails fast in a debug build because generated Unicode
property tests make that profile impractically slow:

```sh
cargo run --release --quiet -p xtask -- test262 --suite /path/to/test262 --report target/test262.json
```

ECMA-402 paths are inventoried as low-priority skips.
