# Architecture

This document records the implementation boundaries and invariants for the
Experimental JavaScript Engine, a pure-Rust port of QuickJS 2026-06-04.
Observable behavior follows the pinned
QuickJS release except for explicitly documented Oxc parser differences.
Rust-native representations and measured optimizations are allowed when
differential tests preserve the selected compatibility behavior.

## Layering

Production dependencies point inward:

```text
fusor-diagnostics
        ↑
fusor-frontend (Oxc)
        ↑
fusor-compiler ─────────→ fusor-bytecode
                                 ↑
qjs-dtoa ─┐                     │
qjs-unicode ─→ fusor-runtime ─┘
fusor-regexp ┘      ↑    ↑
                      │    └── Tokio rt/sync/time (waitAsync signals only)
               fusor-tokio (planned general host adapter)
                      ↑
                    fusor
                  ↙         ↘
                qjs         qjsc
```

The runtime does not depend on Oxc. The compiler consumes an Oxc AST and
produces an owned verified bytecode unit. The runtime depends on the
project-owned diagnostics data layer so verified exception provenance can
become stable host diagnostics without making Miette output semantic truth.
Its narrow Tokio dependency provides finite `Atomics.waitAsync` deadline
signals; only the runtime owner touches the JavaScript heap, settles promises,
and orders jobs. The planned general Tokio adapter follows the same
owned-completion boundary.

Core engine, compiler, runtime, host, and tool crates forbid `unsafe` Rust.
The optional N-API ABI adapter is the sole planned project exception, limited
to documented foreign-pointer operations. Oxc, Tokio, and other dependencies
are audited separately; workspace linting cannot make transitive claims about
their internals.

## Compilation boundary

The compilation pipeline is:

1. Register the source and any incoming standard source map.
2. Select Script, Module, eval, or Function-constructor grammar explicitly.
3. Parse JavaScript with Oxc. Oxc RegExp pattern parsing stays disabled.
4. Reject every parser diagnostic, including recoverable diagnostics.
5. Run Oxc semantic early-error checks, retain its complete semantic model,
   and reject every diagnostic.
6. Reject syntax accepted by the current Oxc release but outside the pinned
   QuickJS/ES2025 profile.
7. Consume Oxc's retained semantic model directly while building
   QuickJS-owned declaration, storage, and module-linking plans.
8. Lower AST nodes into typed pseudo-instructions with copied source origins.
9. Run QuickJS-derived variable resolution, label relaxation, peepholes, stack
   analysis, and debug-table construction.
10. Verify and freeze the current ordinary compiler profile as immutable,
    `Arc`-backed `VerifiedBytecode`, then drop the Oxc allocator.

`ParsedUnit` keeps Oxc's arena-backed `Program` and `ModuleRecord` beside the
complete `Semantic` result. The `Program` header is allocated in the caller's
Oxc arena before semantic traversal, so semantic node references remain stable
without a self-referential Rust owner. No Oxc arena reference may survive
compilation. Static Oxc resolution is only an input: `with`, direct eval,
Annex B bindings, and global declaration instantiation require
QuickJS-compatible dynamic handling.

Production callback entries create a scoped `fusor-frontend` worker with a
dedicated 64 MiB stack. Parsing, semantic construction, the Oxc arena, and the
arena-borrowing callback all remain on that worker; only a `Send` callback
result crosses back to the caller. This isolates published Oxc's internal
stack use from runtime and host event-loop threads. The lower-level
`parse(&Allocator, ...)` API deliberately preserves caller-owned arena access
and therefore runs on the caller's stack; it is not the stack-isolated entry
for untrusted or deeply nested source. Project-owned compiler and verifier
traversals use explicit work lists and do not need a recursion guard.

Published Oxc has no host-forced-strict Script switch. For that goal the front
end inserts a zero-span synthetic `"use strict"` directive before semantic
construction, causing Oxc to bind and validate the original Script as strict
from the root. The source text, Script source type, body, hashbang, real
directives, and their spans remain unchanged. `ParsedUnit::source_directives`
omits the semantic sentinel; `ParsedUnit::program` documents that its directive
list includes it.

Published Oxc also has no Script parser switch that admits top-level `await`.
The asynchronous-global-Script adapter first uses Oxc's Module grammar to
recognize `await`, falls back to Script grammar for Script-only forms, and
retries a byte-length-preserving copy when Script-recognized HTML comments must
be hidden from the Module lexer. The accepted program is reset to Script mode,
retains the original source and comment spans, and explicitly rejects module
declarations, `import.meta`, and root-context uses of `await` as an identifier
or label. Dynamic `import()` remains Script syntax. These are project-owned
goal diagnostics, not mislabeled Oxc diagnostics.

The first compiler-owned lowering result is `StoragePlan`. While the
`ParsedUnit` arena is alive, `fusor-compiler` queries Oxc `Semantic` directly
for scopes, symbols, declarations, and references. It then freezes only native
dense executable/binding/reference IDs, exact copied spans, and immutable
`Arc`-backed names and slices. Oxc node, scope, and symbol IDs never cross this
boundary, and the Oxc semantic graph is neither cloned nor retained. Every
resolved source reference records its native binding ID, using executable,
copied span, and read/write access; unresolved globals remain a separate native
reference domain. Executable preorder and per-executable source ordering are
deterministic. Cross-executable argument/local captures are propagated
iteratively through every intermediate executable. Each immutable
`FrameCapture` names its original binding, its dense slot in the capturing
executable, and either the immediate parent's binding or forwarded capture
slot. Global and module cells never enter this frame-capture domain. The slice
still fails with typed errors for eval, `with`, non-simple parameters, Annex B
block functions, classes, and synthetic function bindings (including
Oxc-resolved `arguments` collisions) rather than emitting a partial plan.
Namespace imports remain module-owned declaration cells, named/default imports
remain import cells, and expression or anonymous-function default exports
receive a distinct synthetic module-local `*default*` cell with the appropriate
initialization policy. The asterisks are part of QuickJS's internal atom, so a
source identifier named `_default_` cannot collide with it.

`CompilationContext` keeps the transient Oxc `NodeId → ExecutableId`,
`SymbolId → BindingId`, and `ReferenceId → native reference` tables beside an
`Arc<StoragePlan>` only while the arena is alive. Lowering never reconstructs
identity from names or spans and never treats a unit-global `BindingId` as an
argument or local slot. The first end-to-end ordinary function-tree family is
Script-only and accepts function declarations and anonymous `function`
expressions. Each function body's value-producing slice handles simple
`var`/`let`/`const` declarations, TDZ setup, immediate Boolean/null/int32 and
compact `BigInt` values, the empty string, exact binary64 Number constants,
resolved argument/local reads, unary and binary operators, short-circuit
`&&`/`||`/`??`, conditional expressions, sequence and expression statements,
mutable identifier assignment and update, lexical blocks, `if`/`else`,
`while`, `do`/`while`, classic `for`, `switch`, labeled and unlabeled
`break`/`continue`, exact-span `debugger` no-ops, and explicit `throw` plus
explicit or implicit returns. Pinned QuickJS has no debugger implementation and
skips the statement; the compiler retains it as `nop` so source lookup remains
exact without adding observable execution behavior. A deepest leaf may read or
write argument/local cells forwarded through ancestor capture slots. It
lowers expressions and statements with iterative work lists, validates the
complete selected body into typed pseudo-instructions before byte emission,
assigns typed frame and imported-capture slots, and immediately produces a
non-executable `VerifiedControlFlow` certificate. Scope entry reads Oxc's
creator `ScopeId`
directly, checks its creator `NodeId`, instantiates body function declarations
before user instructions, recreates block function declarations on every
scope entry, and emits TDZ initialization only for ordinary lexical bindings
owned by that exact scope. Duplicate body declarations retain every child
template but only the last declaration initializes the shared argument/local
slot. Scope exit and abrupt loop edges emit reverse-order `close_loc` for
captured scoped locals; returns rely on whole-frame teardown. Classic `for`
has one explicit loop-head scope and rotates its captured cells after
initialization and on every natural or `continue` edge before update, without
re-running TDZ initialization. All scheduling remains iterative and uses
explicit work stacks; there is no `recursion_guard` layer or dependency.
Breakable control regions distinguish regular labels, iterations, and
switches. Active labeled, breakable, and iteration targets are indexed so abrupt
control lookup does not rescan nested label chains. Switch dispatch retains its
discriminant only while it performs lazy source-ordered strict comparisons;
depth-one match trampolines drop it before branching to depth-zero consequent
labels. Dispatch, trampolines, and bodies are scheduled one case at a time
through `Arc`-shared immutable labels, rather than materializing a second
case-proportional work buffer. The shared switch lexical scope is entered after
discriminant evaluation and before the first case test, and consequents then
fall through in source order. Chained iteration labels preserve Oxc's ES
semantics, an intentional `FUS-OXC-002` difference from pinned QuickJS's
innermost-label-only `continue` limitation; a linear post-Oxc check repairs
invalid chained targets that Oxc otherwise accepts.

Strict block-function declarations use a narrow two-phase lexical
normalization: scope entry first activates the local or captured cell in an
uninitialized internal state, then an adjacent `fclosure*; put_loc*` pair
installs the declaration closure before any user instruction can observe the
binding. This makes captured-cell lifetime explicit to the verifier while
preserving successful JavaScript behavior. The intermediate state is not a
source-visible TDZ extension. Annex B block-function semantics remain rejected
rather than being approximated by this normalization.

`compile_tree` freezes the selected subtree as a flat immutable
`CompiledFunctionTree` in executable preorder. Compilation is child-first, but
uses no Rust call-stack recursion. Each parent's immutable `Arc`-backed
constant table is heterogeneous: Number literal occurrences, canonical decimal
String values from `"0"` through `"2147483647"`, and direct child templates
share one index namespace, remain in source order, and are not deduplicated.
All other nonempty source strings use an immutable function-local atom table,
deduplicated by exact UTF-16 contents; empty strings use `push_empty_string`.
Static object-literal keys additionally admit cooked quoted Strings and
canonical Number or BigInt spellings. Empty and tagged-integer spellings use a
typed static-property-only atom entry that whole-graph verification permits
only at `define_field` or `define_method`; it cannot become a runtime String,
binding name, or realm-global source. Number spelling is centralized on
`Binary64Constant`, while Oxc supplies exact arbitrary-size decimal BigInt key
text without requiring runtime BigInt values.
`push_const8` and `fclosure8` select constant indices `0..=255`; their
full-width forms select indices `>= 256`, while `push_atom_value` always carries
a typed full-width `AtomPoolIndex`.
`Binary64Constant` preserves every non-NaN bit pattern, including subnormals,
infinities, and signed zero.
It canonicalizes NaN only as a deterministic compiler-artifact policy; it is
not the representation contract for runtime Number values, `DataView`, or
typed-array storage, whose payload semantics remain separate.
Directly negated `2^31` is intentionally normalized to `push_i32(i32::MIN)`
without retaining an unused positive Number entry.
`CompilerString` canonicalizes immutable `Arc` storage to Latin-1 when possible
and otherwise retains exact UTF-16 units, including lone surrogates. The shared
frontend decoder is the sole boundary aware of Oxc's cooked-string marker
encoding. Nearest-executable ownership and all pool candidates are computed in
one semantic-node pass. Each pool keeps only compact direct-child, Number-span,
and runtime-string-span lookup tables, so compiling a tree does not rescan the
semantic graph for every function.

Every child capture descriptor is normalized to either the immediate parent's
own dense variable-reference cell or its imported closure environment. The
compiler explicitly remaps plan-global executable identities into a dense
flat graph and stores an `Arc<VerifiedCompilerFunctionGraph>` with the tree.
Whole-graph verification preflights aggregate byte, instruction, constant,
atom, compact string-payload, closure, and transfer-work budgets. It requires
each body atom domain to have an exact owned table and rejects duplicate
function-local atoms. Every heterogeneous constant entry counts toward pool and
resource limits, while only `Function` entries form graph edges for target,
cycle, reachability, nesting-depth, and capture-source checks. Capture work
remains bounded across shared parent edges. A selected
nested root with imported cells fails closed until an explicit verified root
environment exists. `compile_leaf` remains the explicit nested-function-free
API and rejects a selection with children. BigInt and RegExp literals now own
verified runtime records; non-string atom namespaces, raw class/function stack
entries, and remaining inferred anonymous-function cases stay rejected until
their owned records and semantics exist.

Synchronous `for-in` is an ordinary-profile exception to the otherwise empty
statement-stack rule. Lowering keeps one private cursor beneath the statement
stack, uses depth-aware statement anchors, and emits exact cleanup for nested
break, continue, return, and throw. A second bounded CFG analysis gives every
cursor origin a typed identity, carries key/done provenance through only the
matching `if_false` edge, and certifies an immediately consuming lexical-head
store. Declarative authority additionally requires every unchecked store for
that TDZ local to share the same cursor site. Correlated binding-and-cell
states then permit a repeated `let`/`const` write only after the captured cell
was closed. None of these private certificates or cursor values is exposed as
JavaScript.

Ordinary synchronous `for-of` is the second typed statement-stack exception.
Its private same-site record owns the iterator, retained `next` method, and an
active exceptional-close marker. The final verifier admits only the exact
offset-zero step loop, its false-edge head value, normal three-slot close, and
the return-preserving `nip_catch`/rotation/dummy/close sequence. It rejects
forged, split, copied, stored, joined, or terminal record values. Runtime step
failures deactivate the record before calling `next`, so `next`, `done`, and
`value` failures do not close; body and binding failures retain an active
record for iterative inner-to-outer exceptional close. Suspended iterator and
close states participate in frame-value limits and collection root tracing.

A final compiler-profile pass combines that staged graph with exact function
metadata and source snapshots and returns immutable, `Arc`-backed
`VerifiedBytecode`. It checks the ordinary source-function header, ordered
argument/local definitions, declaration policies, lexical scope links, dense
own variable-reference indices, one owning constant edge per non-root
function template, imported closure descriptors, child names, and
parent-to-child closure name/policy/source agreement. Every function
declaration records its exact child constant. The verifier proves one matching
`fclosure*; put_arg*` or `fclosure*; put_loc*` initializer, isolates
function-instantiation initializers in the entry prefix, and validates the
activation-plus-initializer group for scope-entry declarations so control flow
cannot jump into the store.

The same pass runs an iterative CFG analysis over correlated binding-value and
captured-cell product states. It rejects reachable missing TDZ/scope activation,
invalid initialization, immutable writes, capture of an inactive scoped
binding, and invalid close/reopen behavior. Six aggregate budgets bound
variable definitions, closure definitions, unique retained source bytes,
instruction source mappings, abstract frame-state cells, and binding-policy
transfers. Retained parent-edge closure checks are charged to the policy
budget before metadata analysis. It also records a sorted conservative list
of runtime capability families: core values, Numbers, Strings, BigInts,
closures, arrays, ordinary objects, dynamic property keys, direct calls,
abrupt completions, lexical bindings, realm-global bindings, object operators,
and dynamic operators. Those families describe
implementation requirements; they are not a whole-program value-type proof.
All final-verifier traversals use explicit work lists and fallible bounded
allocation, so neither Rust call-stack depth nor a recursion guard is part of
the trust boundary.

`VerifiedBytecode` is the code-and-metadata authority for this current ordinary
Oxc compiler profile. It remains runtime-independent and immutable, so one
`Arc<VerifiedBytecode>` may be installed independently into multiple runtimes
without a lock. `Context::instantiate` binds one installation to a validated
same-runtime realm; the selected root must have an empty external closure
environment. Child environments are subsequently derived only from verified
parent capture metadata. The ordinary compiler profile owns a bounded typed
catch/finally operand-stack certificate and supports synchronous `try`,
optional or simple identifier catch bindings, shared finalizer subroutines,
and nested abrupt completion. The certificate keeps pending JavaScript values,
catch markers, and finalizer return addresses in disjoint types and feeds
effective `gosub`/`ret` edges into binding and object-provenance analysis.
Serialized bytecode, destructuring catch bindings, direct eval, and the
remaining compiler profiles still fail closed.

`BytecodeAssembler` keeps symbolic label handles provenance-bound to one
assembler through immutable `Arc` identity. Labels never enter final operands.
The compiler wraps each handle with its owning Oxc span. Statement labels also
declare an exact empty-stack entry requirement; after final layout and
whole-CFG verification, every reachable statement anchor must have verified
depth zero, while structurally valid unreachable anchors remain accepted.
After whole-body planning, the assembler rejects foreign, duplicate, unbound,
or end-of-stream targets, starts branches at their shortest forms, and
monotonically widens conditionals to 8/32-bit and gotos to 8/16/32-bit
displacements using the QuickJS `opcode_pc + 1` base. This computes the least
valid fixed point even when two branch widths mutually enable their short
forms. Planned instructions and per-pass relaxation visits are bounded before
the assembler encodes once through `BytecodeBuilder`. It returns the relocated
PC of every instruction so source entries are built only after branch widths
are final. The compiler verifier entry independently derives reachable stack
maxima and equal-depth joins and requires reachable terminals to empty the
ordinary value stack; the serialized-bytecode entry separately retains its
exact stored-versus-computed stack-size comparison and QuickJS exit semantics.
Compiler bodies may attach an immutable capture layout whose dense order is
the frame's variable-reference index. Verification checks its declared count,
argument/local bounds, and binding uniqueness before conditionally admitting
`close_loc` for scoped locals. Missing and explicitly empty layouts remain
distinct. They may also attach an immutable constant-kind layout. The staged
verifier admits `fclosure8`/`fclosure` only for compiler-declared function
entries and admits `push_const8`/`push_const` only for ordinary value entries;
using a function entry as a raw pushed value remains fail-closed. Constant
bounds are checked before kinds, and complete predecode/static operand
validation still precedes reachable stack analysis. Serialized constant
opcodes, serialized `close_loc`, and all reference-construction opcodes remain
fail-closed pending a serialized graph format and verifier that own complete
constant, vardef, child, and closure metadata.

Module functions and named function expressions fail closed until their
distinct surrounding-storage and self-binding behavior is implemented.
Static identifier, quoted String, Number, and BigInt literal-named
object-literal concise methods/getters/setters lower as
nonconstructable `OrdinaryMethod` templates paired with one adjacent
`fclosure*; define_method` site. Computed keys, async/generator methods, and
`super`/home-object use remain fail closed.
Ordinary function values may also be stored in data properties and called
through a static member reference. The compiled artifact keeps the exact
source text, storage plan, local layout, exact atom and heterogeneous constant
pools, normalized capture descriptors, source table, and staged/final
certificates in immutable `Arc` storage after the Oxc arena is dropped.
Lowering accepts only an opaque executable selection issued by that context. A
same-index selection from another context is rejected. The dedicated
dynamic-Function authority lowers unresolved names through verified
constructor-realm slots; ordinary non-dynamic bodies, global/module references
outside that typed path, and async/generator functions still fail before byte
emission.

Every successful unit also owns a `ModuleSyntaxRecord` for the module data that
must survive the Oxc allocator. Static requests remain in source occurrence
order, so repeated specifiers retain distinct typed indices, literal spans, and
per-occurrence import attributes. Import and local/indirect/star export entries
retain their linking roles and actual export-site spans. Decoded module strings
use immutable `Arc<[u16]>` backing so lone surrogates accepted by QuickJS are
not collapsed into Rust replacement characters. This record deliberately does
not copy Oxc scopes, symbols, references, or class semantics.

Oxc acceptance, rejection, or diagnostic wording may intentionally differ from
the pinned QuickJS parser. Every accepted difference must be narrow,
documented, and regression-tested; it must preserve byte-accurate source
mapping and must not silently substitute different runtime semantics.
TypeScript, JSX, and alternate JavaScript-runtime behavior remain outside the
default parser profile.

The parser differential corpus has a checked manifest rather than an informal
fixture directory. It fixes the compatibility release and explicit `eval`
exclusion, enumerates every fixture exactly once, and uses closed compilation
goal, feature-family, and current grammar/early-error claim sets. Directory
expectations and manifest expectations must agree, and claims retain both
QuickJS acceptance and rejection coverage wherever both are meaningful. Any
Oxc/QuickJS mismatch must carry a unique direction, rationale, and regression
fixture, and that record becomes invalid if the expectations converge. The
current RegExp pattern difference is deliberate: Oxc recognizes the literal
boundary and flags, while `fusor-regexp` owns pattern grammar, early errors,
and execution. The claim set remains an expanding review contract and does not by
itself certify that every pinned QuickJS parser production has a fixture.

Dynamic Function constructors use a dedicated adapter rather than parsing a
naked body. It joins separately coerced parameter fragments with the exact
QuickJS wrapper punctuation, parses the complete generated Script with Oxc,
and retains a byte-exact synthetic/copied fragment map on success and on any
parser, profile, or semantic failure after preparation. Preflight resource
failures allocate no wrapper. The compatibility release permits source to
escape that wrapper, so the adapter deliberately does not require the Script
AST to contain exactly one function expression.

The runtime exposes that adapter through an immutable
`Arc<dyn OrdinaryDynamicFunctionCompiler>` service. Global `Function` and
`new Function` coerce arguments left-to-right, compile the complete Program
root and every nested template as one typed dynamic-Function Script authority,
install it in the native constructor's home realm, and push its unexposed root
onto the existing iterative frame vector. The compiler receives no runtime,
caller frame, or lexical environment. Nested construction shares instruction,
frame/value, compilation-count, and generated-source budgets. The root is
retired on every completion path; pre-execution failure also rolls back its
realm-environment journal. Wrapper escape remains observable, and named
`anonymous` self bindings are metadata-initialized to the returned function.

Unresolved names are compiled as explicit closure-domain slots whose root
source is the constructor realm rather than a caller capture. Descendants
forward those slots through verified parent-closure edges. Runtime lookup and
write observe the installed code's realm even when another realm initiates
the call; absent strict reads and writes throw exact `ReferenceError`s,
`typeof` uses the non-throwing form, and sloppy assignment creates a global
object property. Sloppy ordinary-function `this` is normalized once while its
frame is created: objects are preserved, nullish receivers use the installed
callee realm's global object, and Boolean, Number, or String values allocate a
branded wrapper whose prototype comes from that realm. Strict functions retain
the raw receiver. Symbol boxing remains fail closed.

Program `var` and function declarations use typed constructor-realm
global-object slots. Installation preflights the complete declaration set
before mutation: new properties are writable, enumerable, and configurable;
an existing `var` property is preserved; a configurable function property is
normalized to those three flags; and a compatible nonconfigurable function
property retains its flags. Incompatible function declarations become a
sourced JavaScript `TypeError`.
Function declarations are bound to one verified named child and one isolated
root-entry `fclosure; put_var` pair. The compiler selects the last duplicate
declaration, emits every function initializer before user statements, and
leaves later `var` initializers in source order.

Escaped `let` and `const` instead remain evaluation-local TDZ cells. A hoisted
global function may capture those cells before their certified
`set_loc_uninitialized` setup, and the verifier proves that setup completes
before user bytecode. Retiring the internal Script root preserves only cells,
functions, and installed code still reachable from the realm or a public
completion. The global intrinsic graph and exact data-property flags, call/new
realm selection, primitive undefined/null/Boolean/Number/String argument
coercions, parser/profile/semantic `SyntaxError`, generated-function
`name`/`length`/`prototype`, legacy bytecode construction, wrapper escape, and
the pinned post-completion `newTarget.prototype` adjustment are implemented.
The realm also owns nonconstructable `Object.prototype.toString`,
`Object.prototype.valueOf`, and `Function.prototype.toString` natives with
exact data-property flags; bytecode function stringification returns retained
verified source. It also owns a nonconstructable `Function.prototype.call`
native with exact `name = "call"`, `length = 1`, native source, and property
flags. The ordinary non-predefined `call` atom is interned transactionally;
the Function-prototype property is the method's only realm-rooted edge.
Object/function source arguments use an executor-owned
continuation with one retained slot per input. It performs
`Symbol.toPrimitive("string")`, then ordinary `toString`/`valueOf`, suspends
across native or verified-bytecode accessor and method calls, counts suspended
state against frame/value ceilings, and resumes without Rust recursion.
Accessor lookup stops at the first descriptor, invokes inherited getters with
the original source object, and treats a missing getter as `undefined`.
Boolean, Number, and String boxing are implemented by the typed wrapper graph
described below. `Number.prototype.toString` reuses the Number-hint conversion
continuation for observable radix coercion and formats validated bases 2
through 36 without exponent notation outside base ten. The remaining String
methods, Symbol boxing, persistent global lexical collision checks, and
`Function.prototype.bind`/`Symbol.hasInstance` stay fail closed.
The path never emits `eval`/`apply_eval` and rejects direct eval anywhere in
generated code. Direct and indirect eval remain wholly unimplemented.
GeneratorFunction, AsyncFunction, and AsyncGeneratorFunction also remain fail
closed.

## Bytecode

The instruction set and compiler passes derive from `quickjs-opcode.h` and the
corresponding QuickJS compiler/VM code.

No bytecode may execute without `VerifiedBytecode`. The complete
VM/serialized boundary ultimately checks:

- known opcodes and complete operand boundaries;
- constant, atom, local, argument, closure, and module indices;
- jump targets on instruction boundaries;
- identical operand-stack depth at control-flow joins;
- no stack underflow and a computed maximum stack depth;
- exception/finally handler structure;
- function metadata and child-function references;
- source-map PCs on valid instruction boundaries.

Malformed serialized bytecode will return a structured verifier error. It will
never reach unchecked indexing in the VM; serialized verification is not
implemented yet.

The staged implementation exposes `VerifiedControlFlow` for complete
body predecode, instruction boundaries, validated execution-header bits and
counts, function-local operand bounds, secondary operand domains, static
successors, suspension and return function-kind compatibility, and reachable
ordinary-value stack heights. Compiler bodies may supply capture and
constant-kind layouts for the narrow `close_loc`, `push_const*`, and
`fclosure*` cases described above. `VerifiedCompilerFunctionGraph` additionally
cross-checks exact function-local String atoms, the compiler's actual
heterogeneous Number/String/function pool, flat function targets, normalized
capture edges, topology, and aggregate budgets with explicit work lists.
Ordinary values never become topology edges. Both staged certificates remain
non-executable.

The final compiler-profile pass adds exact vardef, declaration-policy,
scope-link, initializer, closure, child-name, and retained-source metadata. It
proves declaration-closure initialization against the emitted
`fclosure*; put_*` pairs, runs iterative binding-value/cell-state analysis over
the CFG, freezes conservative `ExecutionRequirement` families, and returns
`VerifiedBytecode`. This is the complete code-and-metadata authority for the
currently admitted ordinary compiler profile.

The first interpreter profile transactionally materializes that authority into
a same-runtime realm. Before interning atoms or creating heap nodes, it scans
every instruction in every template—including unreachable instructions and
unused children—against an exact opcode allowlist. It installs immutable
templates, constants, and atoms, then creates runtime-local function and
binding-cell nodes. Dispatch begins only at verified instruction zero, checks
the certified entry stack depth at every step, and follows only
`VerifiedSuccessors`; branch offsets are never decoded again. Host calls and
direct ordinary JavaScript calls use one explicit frame vector and do not
consume the Rust call stack. A caller remains parked at its verified call PC
until the child returns, so results resume only at the certified fallthrough
and escaping exceptions retain exact caller locations.

The admitted execution families are primitive Number/String/Boolean/nullish
values, ordinary objects, stack operations, arguments and locals, imported
captures, closure creation, TDZ checks, `close_loc` cell rotation, branches,
returns, truthiness, `typeof`, strict equality, the nullish predicate, direct
`call` plus `call0`–`call3`, static-property method calls, and explicit
`throw`. The admitted dynamic-operator family additionally covers every
currently lowered non-BigInt unary, update, arithmetic, shift, bitwise,
relational, loose-equality, and strict-equality opcode.
`Function.prototype.call` forwards its callable receiver, raw
`thisArg`, and remaining arguments through an owned argument cursor. Each
native forwarding boundary attaches one zero-value identity continuation, so
self-targeting call chains remain on the same iterative dispatcher while
counting exactly against the active-frame ceiling. Object literals create
realm-owned ordinary objects and define static identifier, quoted String,
Number, and BigInt literal-named data properties plus synchronous
methods/getters/setters in source order. Typed
ordinary-method closures use the exact nonconstructable header and are
consumed by one verifier-certified `DefineMethod` site whose target retains one
object-literal origin across every incoming path. Quoted names are decoded to
exact cooked UTF-16; Number and BigInt names use canonical JavaScript strings,
while retained function source keeps the raw token spelling. Definition
derives the observable method or accessor name, preserves exact arity, and
merges or replaces data/accessor halves without charging a duplicate slot.
Canonical decimal property descriptions become immediate array-index keys
through `4294967294`; `4294967295` remains an ordinary string key. Static reads
and writes operate across ordinary objects and function objects. Computed
reads, writes, calls, data definitions, and synchronous method/accessor
definitions first run a resumable `ToPropertyKey` state machine. It preserves
the original receiver across inherited accessor lookup and native or bytecode
conversion calls, accepts exact runtime-local well-known and unique Symbol
identity, and resumes only at the verifier-certified successor. Setter calls
preserve the original receiver and sole assignment RHS, discard their
completion, and resume the assigning frame at the certified successor; the
assignment expression retains its RHS. Missing reads produce `undefined`;
nullish access, strict primitive writes, and strict writes to a getter-only
property produce exact `TypeError`s, including
`TypeError: no setter for property`; sloppy rejected writes are ignored. A
method call preserves a static member reference through parentheses, evaluates
lookup before arguments, and passes its base as the raw receiver; a sequence
expression deliberately yields an unbound value. Sloppy ordinary-function
frames normalize their receiver once against the installed callee realm before
execution: nullish values become its global object, objects keep identity, and
Boolean, Number, and String values become one branded wrapper reused by every
`PushThis`. Strict functions keep the raw receiver. Symbol sloppy receivers
remain fail closed. Calls fill missing formals with `undefined` and share
aggregate frame limits and instruction fuel.
Dense array literals lower iteratively to one verified `array_from` instruction
after every direct element has been prevalidated; holes and spread elements
remain fail closed. Execution evaluates elements left-to-right, preflights the
complete realm-owned allocation, then installs a branded array whose own
`length` data property is writable, non-enumerable, and non-configurable.
Canonical indexed definitions extend length, while `4294967295` remains an
ordinary string property. Length assignment performs resumable Number-hint
primitive conversion twice as observed by the pinned QuickJS implementation,
requires both passes to represent the same exact uint32 value, and removes
configurable indexed properties above a shorter length in bounded linear work.
Enumeration hides `length`, object tagging observes the array brand, and the
existing observable `Function.prototype.apply` scan consumes the array without
a special argument-list bypass. The `Array` constructor, `Array.prototype`
methods, holes, iterators, and destructuring remain deferred.
Call spread and construction spread lower to the pinned argument-array packing:
`array_from` for the dense prefix, a dynamic index, `define_array_el`/`append`
per argument, the index `drop`, then `perm3` (member/construction callees) or
`undefined; swap` (plain callees) before `apply`. The `Apply` opcode reuses the
exact `Function.prototype.apply` admission and index scan with a
construction flag that forwards `new.target` through bound unwrapping and
native-to-bytecode frame creation, and abrupt spread exhaustion performs
`IteratorClose` with original-error precedence.
An escaping throw carries its exact JavaScript value through the same frame
vector, allocates caller provenance before publishing any heap root, and
preserves caller order from immediate to outermost. Dynamic operators use a
second resumable state machine for default/Number-hint `ToPrimitive`, including
`Symbol.toPrimitive`, ordinary `valueOf`/`toString` fallback, exact hints,
left-to-right getter/call ordering, and abrupt completion. Number conversion
operates on UTF-16 without replacing lone surrogates, implements the pinned
whitespace and decimal/radix grammar, and feeds exact `ToInt32`/`ToUint32`.
Postfix updates return a verifier-accounted old/new pair so the lvalue write
consumes only the new Number. Number-to-String formatting is fallible end to
end. Decimal output keeps the pinned notation thresholds; bases 2 through 36
use a fixed-limb shortest-round-trip formatter and fixed notation. Observable
radix conversion resumes through the Number-hint operator state machine,
applies saturated signed-32-bit conversion, and reports the exact range error.
The checked Number-radix differential manifest exercises exact binary64 words
against the pinned `qjs`, covers each boundary word in all bases 2 through 36,
and adds a bounded fixed-seed sample through the public facade and verified VM.
Concatenation that exceeds the JavaScript String limit raises the exact
`InternalError` instead of escaping as a host allocation error. BigInt values
and mixed numeric domains,
async/generator methods, `super`/home-object semantics, realm-global setter
dispatch, prototype mutation, proxies and other exotics, derived/class and
nonordinary constructor forms, optional/tail calls, the remaining
Number built-ins, the remaining String built-ins, Symbol sloppy-`this` boxing
and wrapper/prototype conversions, `for-in` Symbol boxing and destructuring
heads, serialized input, raw function slots, destructuring catch bindings,
for-await and async iterator markers, and packed exceptional stack values remain
fail closed. Ordinary
accessors are typed slots: `GetField`
and `GetField2` stop at the first own or inherited accessor, execute native or
verified-bytecode getters through the same frame vector, preserve the original
receiver and `GetField2` base, and retain exact abrupt provenance. Getterless
accessors return `undefined`.

The symbolic assembler chooses the componentwise shortest valid final branch
layout. This can differ from a conservative QuickJS peephole boundary while
preserving the same signed displacement rules and JavaScript behavior.

The complete trust boundary, typed abstract stack, control-flow rules,
resource limits, and acceptance suite are normative in
[BYTECODE_VERIFIER.md](BYTECODE_VERIFIER.md).

## Runtime ownership

The target architecture gives each runtime one object heap, shared by its
contexts/realms. The current executable slice owns realm records plus
function, ordinary-object, and binding-cell arenas. Every realm owns an
internal `Object.prototype` root used by its object literals. Runtime, context,
realm, and value handles are `!Send + !Sync`, and separate runtimes cannot
exchange JavaScript values.

### Realm construction

Standard intrinsic construction is an ordered, typed transaction. Family
modules declare `IntrinsicObjectSpec`, `IntrinsicFunctionSpec`, and
`IntrinsicPropertySpec` entries; their declaration order is semantic because
it contributes to observable string and Symbol own-key order. The immutable
`RealmIntrinsicSchema` validates unique and mandatory identities, all graph
references, descriptor keys, native metadata, constructor/prototype pairs,
and fixed family cardinalities before any Runtime heap node is allocated.
Committed intrinsic lookup remains direct and typed; construction does not
leave a runtime string-keyed registry behind.

The validated schema feeds two complete plans. `RealmAtomPlan` derives the
ordered Realm-local string atoms while retaining the pinned legacy interning
order, and `RealmReservationPlan` derives atoms, Realms, objects, functions,
global bindings, property slots, and undo-journal capacity with checked
arithmetic. Ordinary native `name` and `length` slots are automatic. The
property charge is the schema property count plus those automatic identity
slots, and therefore matches the 757 installed object/function record slots;
there is no separately maintained per-Realm property constant.

`RealmBuildTransaction` pre-reserves its entire undo journal before mutation.
It then interns atoms, allocates transaction-private object and function
shells by typed identity, publishes generic data/accessor descriptors in
declaration order, converts the complete table into `RealmIntrinsics`, and
commits. No public `Realm` or `Context` handle can observe an initializing
shell. The Realm-visible `%Math.random%` seed and logical property charge
advance only after commit.

Dropping an uncommitted transaction removes recorded atoms, bindings,
functions, objects, and the Realm in strict reverse order. Journal recording
after each mutation is allocation-free, and rollback never executes
JavaScript, invokes GC, or runs a callback/finalizer. Standard intrinsic
bootstrap remains separate from dynamic host-function installation and future
module namespaces.

Only relationships that ordinary property declarations cannot establish
safely use narrow hooks: `%Function.prototype%` receives its callable shell
during kernel allocation, `%Array.prototype%` receives its exotic `length`
slot before materialization, and `%ThrowTypeError%` is made non-extensible
after its frozen identity descriptors are published. Constructor/prototype
links and all other ordinary descriptors remain explicit schema entries.

A normalized test snapshot walks all 242 Realm-local intrinsic identities and
pins prototypes, brands, extensibility, ordered keys, complete descriptors,
exact primitive/reference values, and native call/construct metadata. A
separate maintenance regression removes and restores `Error.isError` to prove
that atom and reservation changes are derived from the schema without atom
offsets, record arrays, insertion/publication mirrors, or rollback lists.

Runtime-local nodes use generational typed IDs:

```text
Id<K> {
    runtime_id,
    index,
    generation,
}
```

Every access validates runtime identity, generation, expected kind, and live
state. Internal nodes hold IDs and stored values, never public rooted `Value`
handles or an owning reference back to the runtime.

Public function and ordinary-object values use immutable `Arc` root headers.
Cloning a handle does not create another logical root; dropping the last clone
records one allocation-free deferred release without borrowing the heap.
Mutable runtime boundaries drain those releases at a safe point before
creating any new heap borrow; still-live public headers remain represented by
their root counts.

Backing vectors reserve fallibly and return structured allocation failures.
Stable Rust does not yet expose fallible `Arc` header allocation, so public
root headers, private object-shape owners, and immutable string nodes follow
the global allocator policy until the runtime memory-budget layer lands.

Immutable string leaves and rope nodes use standard-library `Arc` ownership.
This keeps immutable `JsString` handles safe to move through host integration
queues without making a JavaScript runtime, context, or heap transferable
between threads. A backing node is destroyed synchronously when its last strong
handle is dropped. String-node destruction cannot run JavaScript and cannot
determine object liveness, finalizer timing, weak visibility, or cycle removal.
A backing allocation ledger must charge bytes when a node is created and
release them from that node's destruction path; dropping a public value root
is not by itself proof that shared string storage was reclaimed.

When the first genuine shared mutable host-side owner is added, it will use
`parking_lot` locks. No such owner or lock exists in the current runtime slice.
Locks must never guard or make cross-thread access possible for a JavaScript
runtime, context, heap, or value handle, and immutable `Arc<Repr>` string
backing has no lock.

## Reference counting and cycles

The current executable slice owns function, ordinary-object, and binding-cell
nodes in private generational arenas. A dirty safe point drains deferred public
roots and iteratively traces function environments, cells, object/function
property slots, prototypes, and realm roots before the next host call or
installation; an explicit collection API exposes the same pass. This reclaims
transient values plus cycles spanning functions, cells, and objects, and
installed code/atoms are released when their last function disappears.
Collection preallocates all scratch state before mutation.

This is a foundational collector for the currently admitted graph, not the
complete QuickJS ownership model. Deterministic logical strong counts,
non-recursive zero-count release, weak visibility, finalization ordering, the
future exotic-object edge set, and collection while active frames are live
remain M3 work. Until that work lands, a single host call can temporarily
retain discarded heap nodes until the next safe boundary.

Getter dispatch is prepared only after every arena/node borrow has ended. No
proxy trap, host callback, loader, interrupt callback, promise tracker, or
finalizer may run while such a borrow is live. Finalizers will not be admitted
until zombie-state and resurrection rules are complete.

## Values and objects

- JavaScript strings preserve UTF-16 code units and unpaired surrogates.
  Internal forms may include Latin-1, UTF-16, and bounded ropes.
- Each runtime owns one `AtomTable`. Owning `Atom` handles use identity
  equality and carry a weak owner marker, so operations reject foreign and
  orphaned identities. String atoms and global-symbol-registry entries are
  content-interned in separate randomized namespaces; unique symbols and
  private names are never content-interned.
- Interner buckets contain weak entry handles rather than string keys. A miss
  copies UTF-16 contents into a compact string so a short atom cannot retain an
  input rope. Dead slots remain charged until the touched bucket or an explicit
  bounded sweep removes them.
- Runtime startup installs the pinned release's 242 predefined identities in
  exact order: 228 strings, one private brand, and 13 well-known symbols.
- Property keys use immediate canonical array indices or table-validated public
  string/symbol atoms. Private names have a separate identity namespace and
  cannot be constructed as public property keys.
- Incomplete property descriptors retain independent presence for `value`,
  `writable`, `get`, `set`, `enumerable`, and `configurable`. Classification
  produces an opaque generic/data/accessor descriptor, so callers cannot forge
  a kind contradicted by its present fields. The general descriptor API still
  limits completion to creation of a new ordinary property; general accessor
  callability and existing-property compatibility remain later object-model
  checks. The narrower compiler-shaped `DefineMethod` path separately requires
  a typed callable template, verifies its getter/setter merge invariants, and
  certifies one object-literal-derived target through a bounded CFG lattice.
- Bytecode never stores an `Atom` pointer or runtime identity. Atom operands
  are validated function-local pool indices; serialized units carry bounded
  atom contents and namespace metadata, and loading reinterns or creates each
  pool entry exactly once in the destination runtime. Empty or
  tagged-integer static property spellings carry a separate property-only role;
  graph verification rejects that role from every opcode except
  `DefineField`/`DefineMethod`, and final verification rejects it from all
  metadata.
- The current ordinary-object slice keeps each shape in a private
  `Arc<Vec<_>>` and each aligned slot typed as data or accessor. Property
  growth reserves the unique vectors fallibly before mutating either logical
  sequence; transition interning is not implemented yet. Static object-literal
  literal-key definition, accessor definition, and static setter dispatch are
  admitted. Deletion, general flag changes, computed definitions, realm-global
  setter dispatch, and prototype mutation remain fail closed.
- Object literals inherit their realm's internal `Object.prototype`, which
  owns exact `toString` and `valueOf` native data properties.
  `Function.prototype` likewise owns exact `toString` and `call`.
  Each realm additionally owns global Boolean, Number, and String constructors;
  false-branded `Boolean.prototype`, positive-zero-branded `Number.prototype`,
  and empty-string-branded `String.prototype`; and exact core
  `toString`/`valueOf` methods. The String prototype has the pinned own
  `length = 0` property. These intrinsic functions inherit
  `Function.prototype` and participate in the iterative heap trace. Boolean,
  Number, and String wrappers store typed internal payloads rather than
  inferring their brands from the prototype; the object representation also
  reserves a typed Symbol payload for the still-deferred Symbol family. All
  three constructors accept data- or accessor-valued `newTarget.prototype` and
  fall back to the matching prototype in the new target's realm for primitive
  values. Number and String conversion complete before that observable Get.
  String uses the ordinary String hint, observing `Symbol.toPrimitive`, then
  `toString`, then `valueOf`; its call path alone accepts a Symbol and returns
  the pinned descriptive spelling, while construction rejects Symbol values.
  Accessor-backed `newTarget.prototype` and `Object.prototype.toString`
  `Symbol.toStringTag` reads execute through typed native `Get` continuations.
  `Number.prototype.toString` validates its receiver before radix conversion;
  nontrivial radix objects resume through the same typed conversion machinery,
  and the retained Number is charged to the suspended frame without adding a
  heap edge.
  A String wrapper has a fixed non-writable own `length` and synthesizes
  enumerable, non-writable, non-configurable indexed properties from individual
  UTF-16 code units. Primitive String reads use the same UTF-16 index view
  without allocating a wrapper; in-range indices win over the prototype, while
  misses continue through the realm's String prototype. Strict rejected writes
  throw, sloppy rejected writes disappear, and non-index wrapper properties
  remain ordinary.
  Constructor continuations retain and charge the new target and allocate a
  wrapper only after the getter completes; tag continuations precompute the
  built-in tag, box primitive Booleans, Numbers, or Strings before the Get, and
  retain the exact temporary receiver while its getter runs. Completion
  traces realm, public, active-frame, captured-cell, and outer-continuation
  roots before reclaiming unreachable temporary graphs, so a genuinely escaped
  wrapper survives while repeated unescaped boxing remains within heap limits.
  Ordinary objects and function objects share typed data/accessor property
  storage, and both getter and setter function edges are traced.
  `DefineField` creates configurable, writable, enumerable data properties;
  `DefineMethod` creates configurable/enumerable methods or accessors, with
  writable method data properties and exact getter/setter-half merging. It
  initializes the function's exact property-derived name before publication,
  while the compiled arity supplies `length`; duplicate literal keys replace
  one slot without double charging.
- Ordinary data/accessor layouts have an opaque value-independent
  representation; accessor layouts cannot carry a writable flag. Current
  ordinary property slots are typed as data or accessor; binding-cell and lazy
  variants remain future work. Slot count, shape entries, flags, and property
  variants must agree.
- Ordinary locals remain frame slots. Captured, module, mapped-arguments, and
  eval-visible bindings use heap binding cells.
- Suspended generators and async functions own their arguments, locals,
  operand stack, and captured-cell IDs.
- Number parsing/printing, BigInt, Unicode, and RegExp behavior are project-owned
  implementations characterized against the pinned QuickJS and specification
  oracles rather than delegated to another engine at runtime.

## Exceptions and failures

The current implementation keeps distinct domains:

- public-handle errors: orphaned, foreign, stale, or wrong-kind handles;
- admitted JavaScript exception records: engine-created TDZ `ReferenceError`
  and exact direct-call `TypeError: not a function` payloads remain distinct
  from arbitrary explicit `throw` values. Every record retains the origin
  function/bytecode PC/source artifact plus caller call sites while explicit
  frames unwind. Zero-value `Function.prototype.call` continuations preserve
  the target and outer verified call site, and error-stack rendering now
  inserts the pinned `call (native)` / `apply (native)` entries for frames
  reached through those methods;
- compile diagnostics: stable code, canonical message, severity, source span,
  labels, notes, and help;
- verifier errors: function, bytecode PC, opcode, and violated invariant;
- host errors: I/O, loader, timeout, cancellation, channel, and task failures;
- engine faults: contradictions between verified authority and runtime state.

The VM carries engine-created errors and arbitrary thrown `StoredValue`s in one
private typed abrupt-completion transport. After `throw` pops its value, that
transport exclusively owns the value while active frames retain the remaining
edges. A caught completion unwinds the explicit frame vector to the nearest
verified marker, retires crossed dynamic roots, materializes engine-created
errors in the throwing realm, and enters the certified handler with exactly
one JavaScript value. Caller provenance is fallibly allocated for an escaping
completion, and the escaping value is immediately published as one public
`JsValue` root; collection is forbidden between those operations. Cloning the
exception shares that `Arc` root header, and dropping its last clone schedules
the normal deferred release. `StoredValue` crosses the public-handle boundary
through an exhaustive primitive-versus-`HeapReference` split covering both
functions and ordinary objects, and the release mailbox carries the typed heap
reference.

The first M4 Error checkpoint installs nine realm-owned constructor/prototype
pairs: `Error`, `EvalError`, `RangeError`, `ReferenceError`, `SyntaxError`,
`TypeError`, `URIError`, `InternalError`, and `AggregateError`. The complete
current realm graph has an exact resource shape of 19 objects, 42 functions,
and 192 properties. `Error` inherits `Function.prototype`, every native Error
constructor inherits `Error`, `Error.prototype` inherits `Object.prototype`,
and every native Error prototype inherits `Error.prototype`; prototype objects
remain unbranded. Globals, constructor metadata and `prototype`, prototype
`name`/`message`/`constructor`, `Error.prototype.toString`, and
`Error.isError` use the pinned own-property order. Global bindings are
writable, non-enumerable, and configurable. Constructor `length` and `name`
are non-writable, non-enumerable, and configurable, while constructor
`prototype` is non-writable, non-enumerable, and non-configurable. Prototype
members and the two Error methods are writable, non-enumerable, and
configurable.

All nine constructors share one resumable allocation path. Call and construct
both create a fresh branded object, while construction observes
`newTarget.prototype` and falls back to the matching intrinsic in the new
target's realm. Message conversion precedes the options `cause` presence test
and Get; only a present cause becomes an own property. The generic
`Error.prototype.toString` performs the observable `name` Get and conversion
before `message`, and `Error.isError` recognizes only the internal brand rather
than a prototype forgery. `AggregateError` completes message and cause before
acquiring `Symbol.iterator`, obtains and retains `next` once, allocates its
realm-owned Array afterward, and reads each result's `done` before `value`.
Acquisition failures do not close; failures after `next` acquisition perform
exceptional `IteratorClose`, preserving the original error when close also
fails.

Constructor-created errors append an own writable, non-enumerable,
configurable `stack` after `message`, optional `cause`, and optional `errors`.
The snapshot is headerless and contains only retained verified-bytecode frames
in `    at name (file:line:column)` form, plus the pinned synthetic
`call (native)` / `apply (native)` entries when a frame was reached through
`Function.prototype.call` or `Function.prototype.apply` (including argument
getters resumed by apply). Locations are captured without Rust
stack recursion, while function data names are read when the property is
rendered. Snapshot site storage is reserved on constructor entry, so an
uncatchable host allocation failure can precede the observable prototype,
message, cause, or Aggregate iterator work that QuickJS performs before its
backtrace allocation. Engine-created
errors freeze a headerless stack at the first exception-dispatch boundary,
before iterator or finally unwinding, then materialize the matching realm-owned
brand with own `message` and `stack` properties when caught. An escaping
explicitly thrown Error still lacks the host-side name/message normalization
used by the differential observation. Synchronous
finally completions run through verified frame-local `gosub`/`ret`
continuations; exception unwind discards crossed continuations, while return
and control-flow overrides remove only certified pending-value/address pairs.
Ordinary host/resource/engine failures are not mislabeled as JavaScript
throws. The registered host facade converts frontend batches, compiler spans,
installation failures, and execution failures into stable shared diagnostics.
Escaping exceptions resolve their verified origin and caller call sites through
registered source-map chains; each caller is a Miette related diagnostic so
different source files retain independent snippets. Frame display-name lookup
also requires exact retained source-text equality before any span is rendered.
The bounded Error differential has 35 cases and 59 strict feature
tags; this checkpoint matches 18 candidate cases. The remaining observations
are blocked on the pending `Object`/`Reflect` surface, the global `undefined`
binding, or escaping-Error normalization, so neither the Error family nor M4
is declared complete. Direct eval remains deferred and fail closed. Miette
remains presentation, not semantic truth, and hosts may continue consuming the
stable codes, messages, severities, help, and labels without its renderer.

The leaf compiler maps instruction-local verifier failures back through its
strictly ordered final-PC table with exact `BytecodePc` lookup. It never treats
a byte offset as an instruction ordinal. Join-depth failures additionally map
the target PC as a related span; function-level verifier failures with no PC do
not fabricate an instruction span.

## Sources and maps

The source registry owns display names, text, line indices, and incoming
standard source maps. Oxc byte spans become source-aware spans before its arena
is dropped.

For the current ordinary compiler profile, `VerifiedBytecode` owns the exact
source text and display name supplied by the compilation context, function and
optional name byte spans, and one exact span for every final instruction PC.
The verifier checks UTF-8 boundaries, containment, mapping cardinality, and PC
identity. It deliberately does not authenticate that supplied source against
the bytecode; source provenance remains a compiler-trusted invariant. At the
registered host boundary, retained runtime source text must exactly match the
registry entry before its span can enter the source-map resolver.

Line lookup supports separate column encodings:

- UTF-8 byte;
- Unicode scalar;
- UTF-16 code unit.

Generated instructions carry a primary origin and may carry a synthetic parent
origin. Peepholes and label relaxation preserve or deliberately merge those
origins. Final bytecode records sorted PC-to-source transitions.

The host diagnostic resolver follows:

```text
bytecode PC → generated source span → incoming source-map chain → original
```

Source-map point and span chaining have explicit depth/cycle guards and retain
the generated span when an original source is not registered or mapped
monotonically. The compressed serialized `pc2line` representation remains
pending. Constructor-created Error objects and caught engine errors continue to
format their JavaScript-visible headerless `stack` snapshots directly from
retained generated bytecode PCs and source spans; source-map remapping is a
separate host-diagnostic presentation step. Nested direct calls retain each
parked caller call site without consulting the Rust stack.

## Tokio host boundaries

The completed Binary Data slice lazily starts one current-thread Tokio timer
driver on a named host thread when a finite `Atomics.waitAsync` is registered.
Commands carry runtime-created deadlines and waiter IDs. Tokio only wins the
timeout state transition and sends an owned wake record; the runtime owner
roots and settles the intrinsic Promise. Same-agent `Atomics.notify` resolves
its selected FIFO waiters before later Promise jobs can overtake them, while
timeouts and cross-agent notifications remain buffered until a host checkpoint.
Each buffered wake is consumed as one host job followed by a Promise-FIFO
checkpoint, so nested reactions cannot be overtaken by a later wake.

The planned host adapter will use Tokio for timers, I/O readiness, task
wakeups, bounded channels, and worker plumbing. Tokio will never execute
JavaScript or resolve promises directly.

Host tasks will return owned `Send` completions identified by operation ID and
generation. The runtime owner will consume them, update promise capabilities,
and enqueue JavaScript jobs.

One planned event-loop turn is:

1. process one top-level command, timer, or observed host completion;
2. drain the QuickJS-derived FIFO job queue to a fixed point;
3. drain deferred heap releases;
4. repeat or await one Tokio wakeup.

Completions arriving during a microtask checkpoint will remain buffered until
that checkpoint ends. Timer ties will use engine-owned creation IDs rather than
Tokio scheduler order. Tests will inject completions and a virtual clock.

Standalone execution will use a current-thread Tokio runtime and `LocalSet`.
Embedded execution will run at the top level of a caller-provided `LocalSet` or
on a dedicated engine thread. It will never nest `block_on` inside async code.

Cancellation will invalidate an operation generation, abort cancellable host
work, and ignore stale completions. No heap borrow may cross `.await`.

## First vertical slices

The first executable slice compiles, installs, and host-invokes code such as:

```js
function outer(value) {
    function inner() {
        return value;
    }
    return inner;
}
```

It proves runtime-local realm installation, host and JavaScript calls,
forwarded closure cells, TDZ diagnostics, `close_loc` rotation, compact/full
operand forms, allocation-free public-root release, ordinary object allocation
and static data properties, identifier/String/Number/BigInt literal-named
synchronous object methods/getters/setters, strict receiver-aware calls,
iterative accessor dispatch, and safe-point collection of transient/cyclic
function, cell, and object graphs. General descriptor mutation, computed
properties, `super`/home-object semantics, and complete reflection remain
later slices. Synchronous `try`/`catch`/`finally`, realm-owned Error
constructors, `Error.prototype.toString`, `Error.isError`, `AggregateError`
iterator collection, constructor and engine-error stack snapshots, and branded
engine Error objects are part of the current cumulative slice. Synthetic
native stack frames, deleted-stack rebuild for explicitly thrown Errors, and
escaping-Error host normalization remain open.

The current cumulative slice also includes `Function.prototype.bind` with
verified bound functions, the resumable `instanceof` operator, and
`Function.prototype[Symbol.hasInstance]`. Bound-function dispatch unwraps
iteratively at every call boundary (verified bytecode calls, native
continuations, and host calls), prepends bound arguments, overrides the
receiver, and substitutes the target as `newTarget` during construction.
Bound functions carry exact QuickJS `length`/`name` metadata rules, and their
target/`this`/argument edges are traced by the same iterative cycle
collector. `instanceof` executes through a resumable `@@hasInstance`
method-lookup/call state machine with iterative bound-target unwrapping and an
ordinary bounded prototype-chain walk; the operator's non-callable
right-operand and non-object `prototype` errors match the pinned messages.
Persistent global lexical environments and their collision checks remain
open.

The next general asynchronous slice will reuse the proven boundary for a host
timer Promise: wakeups are observed on the runtime owner, the FIFO job queue
drains deterministically, and cancelled operations ignore late completions.
