# Control-flow, switch, `for-in`, catch, and finally differential corpus

`manifest.json` is a bounded runtime corpus for the labeled-statement,
`switch`, synchronous `for-in`, and synchronous `try`/`catch`/`finally`
milestones. Catch cases cover explicit and engine-created errors, cross-frame
unwind, rethrow, captured bindings, iterator cleanup, Error branding, and
ordinary dynamic `Function` syntax failures. Finally cases cover normal,
return, throw, break, and continue completion; catch+finally; nesting;
overrides; captured cells; and script-completion preservation. Run it against
the pinned official QuickJS release:

```text
cargo xtask control-flow-differential \
  --oracle /path/to/quickjs-2026-06-04/qjs
```

Each case supplies the body of an ordinary dynamic `Function`, one or more
strict coverage tags, and the normalized result expected from the pinned
release. The Rust candidate compiles the same body through the public
`quickjs::construct_dynamic_function` facade, executes only its fully verified
function graph in a fresh bounded runtime, and compares the primitive result or
engine-created exception with the oracle.

The runner validates the exact manifest schema and QuickJS release, requires
all milestone coverage tags, rejects duplicate identifiers and unknown fields,
and forbids `eval` text and escaped identifiers. It bounds manifest/body/source
sizes, oracle output, oracle wall time, candidate instruction fuel, runtime
resources, result sizes, and mismatch reporting. Each candidate case runs in a
fresh hidden xtask worker with bounded source on stdin and bounded result
streams; the same per-process wall timeout kills and reaps work that cannot be
interrupted by VM instruction fuel inside one operation. Each oracle case runs
in a fresh `qjs` process with an exact 64 MiB memory limit and 1 MiB stack limit
from a uniquely created bounded temporary source file, so cases cannot share
globals and the accepted source limit does not depend on the host's command-line
argument limit. The oracle harness and its invocation are enclosed in an IIFE;
an executable dynamic-`Function` scope probe verifies that case code cannot see
the harness's lexical bindings.
