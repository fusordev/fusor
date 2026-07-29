# Parser compatibility corpus

This corpus compares the in-process Oxc front end with the pinned QuickJS
2026-06-04 parser.

Directory names are part of the test contract:

- `accept/script`: both parsers must accept as Script;
- `accept/module`: both parsers must accept as Module;
- `reject/script`: both parsers must reject as Script;
- `reject/module`: both parsers must reject as Module.

Every fixture must be deterministic, side-effect-free, unable to create child
processes or workers, and terminate when the pinned upstream `qjs` oracle
executes it in explicit `--script` or `--module` mode. A nonzero oracle result
counts as a syntax rejection only when QuickJS reports `SyntaxError`; timeouts,
signals, loader failures, and runtime exceptions fail the harness. Runtime and
RegExp-semantic differential cases belong in their dedicated corpora rather
than this syntax boundary.
