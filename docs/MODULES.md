# Module system design

This document records the ES Module architecture for the Experimental
JavaScript Engine.
ECMA-262 (ES2025, plus admitted proposals listed in [PORTING.md](PORTING.md))
is normative; QuickJS is a compatibility oracle only, not a design template.

## Boundaries

- `fusor-frontend` owns parsing (Module goal) and the owned
  `ModuleSyntaxRecord`: static requests, import entries, export entries,
  import attributes. Module early errors are rejected at the frontend.
- `fusor-compiler` lowers a Module goal to whole-graph verified bytecode
  with a `CompilerExecutableKind::Module` root and a verified *module
  instantiation record* (per-binding layout: name, cell, mutability,
  initialization policy, import source). Unsupported semantics reject at
  compile time; nothing is approximated.
- `fusor-runtime` owns module records, linking, evaluation, namespaces,
  the promise/job ordering for dynamic `import()` and top-level await, and
  the typed host-module-loader boundary. It performs no IO and no parsing.
- Host crates (`fusor`, tools) own resolution policy (FS layout, `node:`
  builtins) and all IO. Tokio may drive host-side asynchronous loading; the
  runtime retains promise and job ordering exactly as for
  `Atomics.waitAsync` deadline signals: host completions enter as explicit
  runtime events and never settle JavaScript state from another thread.

## Runtime data model

Per realm, a module registry maps a host-canonicalized *module key* to one
`SourceTextModuleRecord`. A record carries:

- the owned `ModuleSyntaxRecord` (requests, import/export entries) and the
  verified bytecode authority plus its module instantiation record;
- `[[Status]]` (New/Unlinked/Linking/Linked/Evaluating/EvaluatingAsync/
  Evaluated), DFS index/ancestor index, `[[CycleRoot]]`,
  `[[EvaluationError]]`, and the top-level-await fields
  (`[[HasTLA]]`, `[[AsyncEvaluation]]`, `[[TopLevelCapability]]`,
  `[[PendingAsyncDependencies]]`, `[[AsyncParentModules]]`);
- a persisted module environment and lazily materialized namespace object;
- `[[HostDefined]]` slot for the embedding (source identity, referrer key).

### Module environment and binding aliasing

A module environment is the persisted set of root-frame binding cells
described by the compiler's instantiation record. Cells are ordinary
runtime binding cells; TDZ uses the existing uninitialized slot.

Imports are immutable indirect bindings. At link time an imported binding
cell is *forwarded* to the exporter's cell (cell-table forwarding with
bounded chain collapse), so reads observe live values across the graph
without new opcodes. Writes to an import binding are rejected by the
compiler-emitted immutability check before any cell access, so forwarding
never lets an importer mutate an exporter binding. Unresolvable imports
throw at link time (resolution phase), exactly like an unresolved `export
... from` ambiguity.

Namespace objects are exotic objects holding the resolved export map
(name → target module + binding). `[[Get]]` reads through the target cell
(TDZ errors included); `[[Set]]` is unconditionally false; `[[OwnPropertyKeys]]`
is the sorted export list. `export * as ns` and `import * as ns` share the
same namespace object per module.

## Linking

`InnerModuleLinking` is an iterative explicit-stack DFS over resolved
requests: status transitions, stack discipline, and `[[DFSAncestorIndex]]`
propagation follow ECMA-262 16.2.1.6. A linking failure unwinds the stack
to Unlinked and resets only the modules on the stack. `InitializeEnvironment`
(per spec `InitializeEnvironment`) runs per strongly connected component in
dependency order: create cells, resolve and forward imports, materialize
namespace objects, instantiate hoisted function declarations (including
anonymous `export default function`/`class` into the synthetic default
binding).

## Evaluation

`InnerModuleEvaluation` (16.2.1.8) is likewise iterative. Synchronous
modules execute the module root frame with the persisted environment; the
completion value is discarded except for error propagation. Cyclic
evaluation errors propagate through `[[CycleRoot]]` exactly as specified.

Top-level await compiles the module body as an async root.
`ExecuteAsyncModule` / async cyclic evaluation use the runtime-owned promise
job queue; no Tokio task ever orders JavaScript jobs. Host-driven
asynchronous *loading* completes through explicit host turns.

## Host module loader boundary

The runtime exposes a typed boundary (never IO inside the runtime):

- `resolve(specifier, attributes, referrer) -> module key` — host policy;
  keys are realm-scoped and canonical (two keys equal ⇒ same record).
- `load(key) -> source` — synchronous or host-turn completion; the runtime
  then parses, compiles, links, and evaluates. Errors become SyntaxError /
  resolution rejections at the correct spec phase.
- `import.meta` population hook (`url`, `resolve`) per module.

Dynamic `import()` keeps its existing observable front half (intrinsic
promise, specifier conversion, `options.with` attribute reads) and crosses
this boundary at `HostLoadImportedModule`; the returned namespace fulfills
the promise. All proposal surface admitted by policy — import attributes,
JSON modules, `import defer`, text/bytes imports, source-phase imports —
plumbs attributes through this boundary; each lands as its own step.

## Host layers

- Node-like FS+builtin resolver (tool crate): `file:` URLs and
  relative/absolute specifiers against the referrer path, exact ESM
  resolution, `node:`-style builtin table, JSON/text loaders keyed by
  import attributes. Tokio-backed asynchronous reads complete as host
  turns.
- ESM REPL: a session owns one synthetic, incrementally extended module
  environment; each entry parses as a Module, links its static imports into
  that environment, then evaluates. This is documented host sugar, not a
  spec module record.
