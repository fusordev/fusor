# Parser compatibility corpus

This corpus compares the in-process Oxc front end with the pinned QuickJS
2026-06-04 parser. [`manifest.json`](manifest.json) is the authoritative,
machine-checked coverage ledger: every JavaScript fixture must appear exactly
once, every declared expectation must agree with its directory, and every
currently declared non-`eval` goal, frontend claim, grammar production, and
pinned diagnostic must remain covered. Direct and indirect `eval` are
explicitly excluded until their caller-scope integration is implemented.

The ledger is closed in four dimensions, all enforced by
`cargo xtask parser-differential`:

- **Goals.** Every non-`eval` parse goal has fixtures.
- **Claims.** Every frontend claim has fixtures in each polarity it admits.
- **Grammar productions.** Every production the pinned parser recognizes is
  exercised by at least one fixture the oracle *accepts*. Rejection is not
  grammar coverage.
- **Pinned diagnostics.** Every `SyntaxError` the pinned front end can raise
  while compiling a source text is provoked by a fixture, or is recorded as
  unreachable with a reason. The oracle's observed message is compared against
  the pinned format string on every run, so the ledger cannot drift.

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

## Grammar productions

`xtask/src/parser_productions.rs` enumerates the productions the pinned parser
recognizes, each anchored to the `quickjs.c` function or `case` that parses it,
so the vocabulary tracks the parser rather than an outside grammar summary. A
production records which goals admit it, so `import` forms cannot be claimed by
a Script fixture and `with` cannot be claimed by a strict one.

Only fixtures the oracle accepts may declare productions. Every production must
have at least one such fixture, and unknown production names fail closed.

## Pinned diagnostics

`xtask/src/parser_diagnostics.rs` is generated from the pinned `quickjs.c` and
lists every diagnostic the front end can raise while compiling a source text:
all `js_parse_error*` call sites plus the compile-time `JS_ThrowSyntaxError`
sites that run before any of the source executes. Each entry records the pinned
format string, the call sites that raise it, the claims a fixture for it must
declare, and whether the corpus can provoke it.

Each rejecting fixture declares exactly one diagnostic: the one the oracle
actually reports. During the oracle run the observed `SyntaxError` text is
matched against the pinned format string, where `%c` matches exactly one
character and `%s`/`%.*s` match any run of characters, while every literal
character must match. A fixture therefore cannot claim a diagnostic it does not
provoke.

Nine call sites are recorded as unreachable, each with the reason:

- three `invalid UTF-8 sequence` sites (`quickjs.c:22320`, `22471`, `22542`)
  require invalid UTF-8 bytes, which corpus fixtures forbid;
- `quickjs.c:25537` follows a class-body loop that exits only when the current
  token is already `}`;
- `quickjs.c:26102` and `quickjs.c:36632` need `yield` to arrive as `TOK_IDENT`
  inside a generator, but unescaped `yield` tokenizes as `TOK_YIELD` and escaped
  spellings are rejected by earlier `is_reserved` checks;
- `quickjs.c:26653` and `quickjs.c:26672` sit behind guards every
  `js_parse_destructuring_element` caller already satisfies;
- `quickjs.c:26771` requires a build without the RegExp compiler intrinsic,
  which `qjs` always installs.

## Intentional frontend differences

`QJS-OXC-001` intentionally records Oxc accepting a malformed RegExp pattern.
This frontend validates the RegExp literal boundary and flags, while
QuickJS-compatible pattern semantics remain delegated to the future RegExp
layer. The candidate fixture keeps that boundary visible and executable instead
of silently treating it as parser compatibility.

`QJS-OXC-002` records published Oxc accepting an ES-valid `continue` to an
outer label in a chain that directly labels one iteration statement. Pinned
QuickJS only treats the innermost label as continuable. The Rust compiler
preserves Oxc's resolved semantics for complete chained-label support; the
bounded runtime control-flow differential excludes this intentional syntax
difference and covers the common QuickJS-compatible label surface. A narrow
post-Oxc semantic check still rejects chains that terminate in a regular
statement or `switch`; those are invalid `continue` targets in both engines.

`QJS-OXC-003` records the pinned parser's recursion limit. QuickJS reports
`stack overflow` beyond roughly 695 nested parentheses (`quickjs.c:22720`),
while the Oxc front end parses the same source on its isolated 64 MiB frontend
stack. The bound is a QuickJS resource limit rather than ECMAScript grammar.

`QJS-OXC-004` records the pinned parser rejecting an instance field named
`prototype` (`quickjs.c:25396`), a check its own source marks `XXX: spec: not
consistent with method name checks`. ECMAScript reserves `constructor` for
instance fields and `prototype` for static ones only, and V8 accepts the
fixture, so the frontend follows the specification.

## Fixture requirements

Every fixture must be deterministic, side-effect-free, unable to create child
processes or workers, and terminate when the pinned upstream `qjs` oracle
executes it in explicit `--script` or `--module` mode. A nonzero oracle result
counts as a syntax rejection only when QuickJS reports `SyntaxError`; timeouts,
signals, loader failures, and runtime exceptions fail the harness. Runtime and
RegExp-semantic differential cases belong in their dedicated corpora rather
than this syntax boundary.

Two fixtures deliberately approach the pinned parser's own resource limits:
`reject/script/too-many-call-arguments.js` passes more than the 65535 arguments
`quickjs.c:27143` allows, and `candidate-accept/script/parser-stack-overflow.js`
nests parentheses past the recursion bound. The harness fixture limit is
therefore 256 KiB.

Strict Script fixtures run the pinned oracle with `--strict`. Async Script
fixtures load the source and call the pinned `std.evalScript` with
`{ async: true }`; strict+async fixtures prepend a synthetic strict directive
only inside that oracle adapter, immediately after a source-start hashbang when
present. Candidate parsing uses the corresponding lossless
`GlobalScriptGoal`, so the original fixture source and spans are not rewritten.

The frontend side of the differential parses through the crate's isolated
frontend context, which is the path embedders use and the only one with a
bounded, documented stack.

Run the complete compatibility check with:

```sh
cargo xtask parser-differential \
  --oracle /private/tmp/quickjs-2026-06-04/qjs
```

The command first validates the manifest and pinned oracle release, then
executes all fixtures with bounded output and time, and prints coverage for
every closed ledger dimension.
