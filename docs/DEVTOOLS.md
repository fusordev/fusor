# DevTools

`fusor` exposes one loopback-only Chrome DevTools Protocol target when started
with `--inspect` (default port `9229`):

```console
fusor repl --inspect
fusor run --inspect=9230 entry.mjs
fusor run --script --inspect-brk app.js
```

Chromium-compatible discovery endpoints are available at `/json/version` and
`/json/list`; attach a CDP client to the returned `webSocketDebuggerUrl`.
`--inspect-brk` pauses before the first verified instruction until the client
sends `Runtime.runIfWaitingForDebugger` or `Debugger.resume`.

## Protocol profile

The current protocol profile supports the `Runtime` inspection domain —
`Runtime.evaluate` (with `objectId` results, object previews,
`returnByValue` serialization, and structured `exceptionDetails`),
`Runtime.getProperties` (descriptors, accessor getter execution, prototype
chain walking, symbol keys), `Runtime.callFunctionOn`, `Runtime.releaseObject`
and `Runtime.releaseObjectGroup`, `Runtime.globalLexicalScopeNames`,
`Runtime.compileScript` (parse and compile only), `Runtime.getHeapUsage`, and
`Runtime.getIsolateId` — together with the `Debugger` script-source,
URL-breakpoint, pause, resume, and stepping controls. Stack locations retain
exact compiler source spans and are converted to CDP's zero-based UTF-16 line
and column coordinates. Inspection reads properties through the engine's own
builtins, so accessor getters and Proxy traps execute during inspection the
same way they do in Chromium.

## Known limits

- Evaluation is unavailable while the debugger is paused (the single runtime
  task owns the pause).
- `throwOnSideEffect` evaluates normally (the pinned reference has no
  side-effect-free execution mode).
- `awaitPromise` and the command-line API (`includeCommandLineAPI`) are
  accepted but ignored.
- `Runtime.globalLexicalScopeNames` lists global-object own properties only —
  `let`/`class` bindings live in the declarative global environment and are
  not enumerated.
