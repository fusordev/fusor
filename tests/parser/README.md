# Parser compatibility corpus

This corpus compares the in-process Oxc front end with the pinned QuickJS
2026-06-04 parser.

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

Every fixture must be deterministic, side-effect-free, unable to create child
processes or workers, and terminate when the pinned upstream `qjs` oracle
executes it in explicit `--script` or `--module` mode. A nonzero oracle result
counts as a syntax rejection only when QuickJS reports `SyntaxError`; timeouts,
signals, loader failures, and runtime exceptions fail the harness. Runtime and
RegExp-semantic differential cases belong in their dedicated corpora rather
than this syntax boundary.

Strict Script fixtures run the pinned oracle with `--strict`. Async Script
fixtures load the source and call the pinned `std.evalScript` with
`{ async: true }`; strict+async fixtures prepend a synthetic strict directive
only inside that oracle adapter, immediately after a source-start hashbang when
present. Candidate parsing uses the corresponding lossless
`GlobalScriptGoal`, so the original fixture source and spans are not rewritten.
