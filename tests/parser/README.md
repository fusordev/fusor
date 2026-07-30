# Parser compatibility corpus

This corpus compares the in-process Oxc front end with the pinned QuickJS
2026-06-04 parser. [`manifest.json`](manifest.json) is the authoritative,
machine-checked coverage ledger: every JavaScript fixture must appear exactly
once, every declared expectation must agree with its directory, and every
currently declared non-`eval` goal and frontend claim must remain covered.
Direct and indirect `eval` are explicitly excluded until their caller-scope
integration is implemented. The claim set is a growing compatibility matrix,
not yet a declaration that every QuickJS grammar production is represented.

Directory names are part of the test contract:

- `accept/script`: both parsers must accept as Script;
- `accept/module`: both parsers must accept as Module;
- `accept/strict-script`: both parsers must accept as a host-forced strict
  Script;
- `accept/async-script`: both parsers must accept as an asynchronous global
  Script;
- `accept/strict-async-script`: both parsers must accept with both host flags;
- `reject/script`: both parsers must reject as Script;
- `reject/module`: both parsers must reject as Module;
- the corresponding `reject/strict-script`, `reject/async-script`, and
  `reject/strict-async-script` directories exercise host-flag early errors.

Narrow accepted frontend differences can be recorded without weakening either
side's expectation:

- `candidate-accept/<goal>` means the Oxc frontend must accept and QuickJS must
  reject;
- `candidate-reject/<goal>` means the Oxc frontend must reject and QuickJS must
  accept.

Every candidate difference must have a unique manifest ID, its derived
direction, a nonempty rationale, and the fixture that reproduces it. A
difference entry is rejected if the two declared expectations become equal, so
resolved gaps cannot remain in the ledger.

The required feature families cover source/lexical grammar, bindings,
functions, expressions, classes and object literals, statements, Annex B,
modules, and the pinned ES2025 target profile. Required claims refine those
families into concrete grammar and early-error surfaces. Claims require both a
QuickJS-accepted and QuickJS-rejected fixture when both outcomes are
meaningful; intrinsically positive or negative claims declare only their
applicable polarity. Manifest evidence points only to the pinned QuickJS
source, tests, and compatibility configuration; it is provenance, not a
substitute for the executable oracle.

Every fixture must be deterministic, side-effect-free, unable to create child
processes or workers, and terminate when the pinned upstream `qjs` oracle
executes it in explicit `--script` or `--module` mode. A nonzero oracle result
counts as a syntax rejection only when QuickJS reports `SyntaxError`; timeouts,
signals, loader failures, and runtime exceptions fail the harness. Runtime and
RegExp-semantic differential cases belong in their dedicated corpora rather
than this syntax boundary.

Accordingly, `QJS-OXC-001` intentionally records Oxc accepting a malformed
RegExp pattern. This frontend validates the RegExp literal boundary and flags,
while QuickJS-compatible pattern semantics remain delegated to the future
RegExp layer. The candidate fixture keeps that boundary visible and executable
instead of silently treating it as parser compatibility.

`QJS-OXC-002` records published Oxc accepting an ES-valid `continue` to an
outer label in a chain that directly labels one iteration statement. Pinned
QuickJS only treats the innermost label as continuable. The Rust compiler
preserves Oxc's resolved semantics for complete chained-label support; the
bounded runtime control-flow differential excludes this intentional syntax
difference and covers the common QuickJS-compatible label surface. A narrow
post-Oxc semantic check still rejects chains that terminate in a regular
statement or `switch`; those are invalid `continue` targets in both engines.

Strict Script fixtures run the pinned oracle with `--strict`. Async Script
fixtures load the source and call the pinned `std.evalScript` with
`{ async: true }`; strict+async fixtures prepend a synthetic strict directive
only inside that oracle adapter, immediately after a source-start hashbang when
present. Candidate parsing uses the corresponding lossless
`GlobalScriptGoal`, so the original fixture source and spans are not rewritten.

Run the complete compatibility check with:

```sh
cargo xtask parser-differential \
  --oracle /private/tmp/quickjs-2026-06-04/qjs
```

The command first validates the manifest and pinned oracle release, then
executes all fixtures with bounded output and time.
