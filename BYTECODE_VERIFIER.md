# Bytecode verifier

This document specifies the verifier for the pure-Rust port of QuickJS
2026-06-04. It is normative for `quickjs-bytecode` and the VM boundary.

Source references name files and line ranges in the pinned
`quickjs-2026-06-04` archive. A rule labelled **Upstream** describes behavior
or an invariant in that release. A rule labelled **Rust hardening** is a
stronger requirement of this port; it must not be attributed to upstream
QuickJS.

## Trust boundary

**Upstream.** QuickJS documents that its bytecode is version-specific, receives
no security check before execution, and must not be loaded from untrusted
sources (`doc/quickjs.texi:858-862`). Its object reader accepts serialized
function metadata, walks the byte stream to relocate atoms, and then installs
the constant pool without rerunning compiler stack analysis
(`quickjs.c:38498-38553`, `quickjs.c:38627-38823`). Compiler-produced bytecode
does run `compute_stack_size`, and the result becomes the stored stack size
(`quickjs.c:35998-36002`, `quickjs.c:36076-36084`).

**Rust hardening.**

- Raw bytes, decoded instructions, deserialized functions, caches, plugins,
  network input, and test fixtures are untrusted.
- Compiler output is not adversarial, but it crosses the same verifier before
  execution. There is no verifier bypass for the compiler.
- Only an immutable `VerifiedBytecode` may be installed in a function or
  passed to the VM. Raw and decoded-but-unverified representations expose no
  execution API.
- Verification is pure: it does not allocate VM objects, intern into a live
  runtime, invoke host code, or execute JavaScript.
- Failure returns a structured error containing function path, bytecode PC
  where applicable, opcode where available, and the violated invariant.
  Failure never returns a partially verified function graph.

Verification has five ordered phases:

1. charge graph and allocation limits;
2. predecode the complete instruction stream;
3. validate metadata, operands, indices, flags, and child functions;
4. run typed control-flow and stack verification;
5. freeze decoded instructions, boundary tables, computed stack size, and
   verified children into `VerifiedBytecode`.

### Implementation staging

The body-level implemented slice returns `VerifiedControlFlow`, not
`VerifiedBytecode`. It completely predecodes the function, validates every
static operand domain represented by its body-only input and every successor
even in unreachable code, validates the serialized execution-header flag and
mode domains, retains its typed function kind and counts, and analyzes
reachable ordinary JavaScript-value stack heights. The six suspension opcodes
are accepted only for their compatible function-kind families, while ordinary
and tail returns are limited to normal functions. Compiler bodies may attach a
constant-kind layout that conditionally admits ordinary value loads and nested
closure creation; serialized bodies still reject every constant opcode. The
slice rejects opcodes whose correct verification needs actual constant values,
verified child bodies, raw function slots, handler or iterator markers,
finally return addresses, or packed stack offsets. Its opaque certificate has
no execution API and cannot cross the VM trust boundary. The complete
typed-stack and whole-function rules below remain mandatory before
`VerifiedBytecode` exists.

The next compiler-only slice returns
`VerifiedCompilerFunctionGraph`. It takes a flat `Arc`-backed graph, requires
explicit body capture and constant layouts, owns exact content-interned
function-local String atoms, the actual heterogeneous Number/String/function
constant entries, function-template target identities, and normalized
immediate-parent capture sources. It rejects duplicate atoms, duplicate
normalized capture sources, cycles, and unreachable records, validates every
shared-parent edge, and charges aggregate body, compact string-payload, and
edge-work budgets. Every constant entry is counted and kind-checked, but only
`Function` entries form graph edges or contribute to topology and nesting
depth. Traversal and depth accounting use explicit work lists; they never
depend on Rust call-stack depth and require no `recursion_guard` layer or
dependency. A
selected root with imported closure variables is rejected because no verified
external environment was supplied. This certificate is still not
`VerifiedBytecode`: it lacks vardef/name/policy metadata, other value and atom
namespaces, typed handler/finally/iterator states, source/debug validation, and
the runtime function metadata required for exact behavior. It exposes no VM
execution entry point.

Serialized bodies provide a stored maximum stack size, and
`verify_control_flow` requires it to equal the recomputed reachable maximum.
Compiler-generated bodies have no serialized maximum yet, so the distinct
`verify_compiler_control_flow` entry recomputes and retains that value without
a comparison. It additionally requires every reachable terminal to leave the
ordinary value stack empty, catching compiler bugs that strand values at
`return` or `return_undef`. Serialized bodies retain QuickJS's stored-body
semantics and do not impose that compiler-only invariant. Both entries
otherwise share the same complete predecode, metadata, target, successor,
reachability, join-depth, and resource checks; neither returns execution
authority.

Compiler-generated bodies may explicitly attach an immutable
`CompilerCaptureLayout`. Its dense entry order defines this frame's
variable-reference indices, and each entry identifies a captured argument,
function-lifetime local, or scoped local. A nonzero declared
`variable_reference_count` requires this metadata; absence is distinct from an
explicitly validated empty layout. Verification checks count equality,
argument/local bounds, and unique frame-binding identities. Only compiler
bytecode with a matching scoped-local entry may use `close_loc`. Serialized
`close_loc` and every `make_*_ref` opcode remain fail-closed until the complete
vardef and closure descriptors are available.

Compiler-generated bodies may also explicitly attach an immutable
`CompilerConstantLayout`. Its dense entries classify each declared
constant-pool position as an ordinary value or a nested function template.
Absence is distinct from an explicitly validated empty layout, and its length
must exactly equal the declared constant count. Complete predecode and
constant-index bounds validation precede kind checks. `push_const8` and
`push_const` require a value entry; `fclosure8` and `fclosure` require a
function entry. Pushing a function entry remains rejected as a raw function
stack value. The layout does not contain actual values or child bodies and
therefore grants no execution authority.

The compiler graph pairs that layout with an immutable `Arc`-backed
heterogeneous pool and rejects any declared/actual kind mismatch at the exact
pool index. The compiler constructs Number literals requiring pool storage and
direct child templates in source order without deduplication; verification
preserves those positions rather than rebuilding the pool. As an intentional
compiler normalization, directly negated `2^31` emits `push_i32(i32::MIN)` and
does not retain an unused positive literal slot. A Number entry stores a
`Binary64Constant`: every non-NaN bit pattern is exact, while NaN is
canonicalized only as a deterministic compiler-artifact policy. This type must
not be reused as the general runtime Number, `DataView`, or typed-array storage
contract. Serialized constant operations remain fail-closed until the future
whole-function verifier owns and validates their complete pool.

**Rust hardening.** The compiler applies one further source-language invariant
to the returned certificate: each reachable structured-statement label must
begin at ordinary stack depth zero. These anchors are recorded symbolically,
relocated through the assembler's final instruction-PC table, and resolved with
`VerifiedControlFlow::instruction_index_at`; unreachable anchors have no entry
state and are accepted. Expression labels deliberately carry no absolute-depth
expectation because their valid depth depends on the surrounding expression.

## Complete predecode and instruction boundaries

**Upstream.** The final opcode table supplies fixed instruction sizes and fixed
or dynamic stack effects, and byte zero is the reserved `invalid` opcode.
Temporary-opcode enum values deliberately overlap final short-opcode bytes
(`quickjs.c:1124-1144`, `quickjs.c:22059-22066`). The opcode definitions and
their two domains are in
`quickjs-opcode.h:65-118`, `quickjs-opcode.h:150-215`,
`quickjs-opcode.h:262-302`, and `quickjs-opcode.h:305-356`.

**Rust hardening.** Compiler-temporary operations exist only in typed compiler
IR and cannot enter final encoding. Raw final bytes are interpreted
exclusively through the final short-opcode table; an overlapping byte is a
valid final short opcode, not detectably a temporary opcode. Predecode starts
at PC 0 and partitions the entire byte buffer:

- bytecode is non-empty;
- every raw opcode byte is nonzero, below final `OP_COUNT`, and decoded with
  final short-opcode metadata;
- the complete fixed-size operand payload is present;
- decoding advances by the metadata size with checked arithmetic and ends
  exactly at `bytecode_len`;
- an instruction-start bitmap and `PC -> instruction index` table are produced
  before any control-flow analysis;
- operands of unreachable instructions are validated too. Unreachable but
  well-formed final instructions are permitted; unreachable truncation,
  unknown opcodes, or opaque garbage are not;
- every non-terminating fallthrough ends at an instruction start strictly
  below `bytecode_len`.

The typed compiler finalization pass separately rejects any remaining
temporary IR instruction, and the serializer accepts only the final
instruction enum. Raw-byte verification cannot and must not try to infer which
overlapping enum domain originally produced a byte.

All arithmetic uses checked `i64` intermediates followed by a checked
conversion to `usize`. Integer wraparound is a verifier error.

### Relative target bases

**Upstream.** Let `p` be the instruction's opcode PC and let `d` be the
sign-extended encoded displacement.

| Encoding or opcode | Exact target | Other PC |
| --- | --- | --- |
| `label8` | `p + 1 + i8(d)` | instruction end is `p + 2` |
| `label16` | `p + 1 + i16(d)` | instruction end is `p + 3` |
| `label` (32-bit) | `p + 1 + i32(d)` | instruction end is `p + 5` |
| `atom_label_u8` used by `with_*` | `p + 5 + i32(d)` | instruction end is `p + 10` |
| `gosub` return continuation | not encoded | `p + 5` |

These bases follow upstream stack analysis
(`quickjs.c:35720-35776`), the interpreter's jump, `catch`, `gosub`, and `ret`
implementations (`quickjs.c:18848-18982`), and the `with_*` operand layout and
branch adjustment (`quickjs.c:20373-20472`).

**Rust hardening.**

- Every encoded target and every `gosub` continuation must be an instruction
  start in `0..bytecode_len`; targeting `bytecode_len` or an operand byte is
  rejected.
- A `catch` target may not be PC 0. Upstream also uses catch offset zero as an
  iterator-unwind sentinel (`quickjs.c:18996-19025`,
  `quickjs.c:20553-20569`); Rust represents those cases with different types
  and rejects the ambiguous serialized form.
- `gosub` must have an in-range continuation at `p + 5`, even though it has no
  direct runtime fallthrough.

## Metadata, indices, and secondary operands

Validation is performed for every decoded instruction, reachable or not.
Checked addition and multiplication precede every allocation or slice.

### Function and pool metadata

**Upstream.** A bytecode function stores argument, local, captured-reference,
closure, constant-pool, bytecode, and stack counts
(`quickjs.c:654-724`). The serializer writes a vardef count equal to
`arg_count + var_count`, but the reader accepts a separate `local_count`
(`quickjs.c:37733-37771`, `quickjs.c:38661-38678`).

QuickJS's live in-memory bytecode operands contain runtime `JSAtom`
identities, while its object writer rewrites all five atom-bearing operand
formats through a serialized atom table and relocates them back while reading
(`quickjs.c:32027-32036`, `quickjs.c:37525-37537`,
`quickjs.c:37613-37627`, `quickjs.c:38522-38543`).

The staged body certificate already owns and validates the execution-header
subset serialized by the object writer (`quickjs.c:37715-37730`,
`quickjs.c:38641-38658`):

| Packed flag bits | Meaning |
| --- | --- |
| 0 | has prototype |
| 1 | simple parameter list |
| 2 | derived class constructor |
| 3 | needs home object |
| 4–5 | function kind: normal, generator, async, or async generator |
| 6–11 | `new.target`, `super()` call, `super` property, `arguments`, debug, and eval flags |
| 12–15 | reserved and rejected |

The validated stored `js_mode` mask is `0x01`: strict only.
`JS_MODE_ASYNC` and `JS_MODE_BACKTRACE_BARRIER` are runtime frame state;
QuickJS synthesizes the former when creating a suspendable frame and sets the
latter temporarily around an eval call (`quickjs.c:20785-20798`,
`quickjs.c:37191-37205`). A future runtime frame-mode type must add them there,
not admit them through `VerifiedFunctionHeader`. The eval header flag does not
enable `eval` or `apply_eval`; their scope metadata remains a separate
fail-closed capability.

**Rust hardening.**

- Every Rust bytecode function owns a bounded ordered atom pool. Its encoded
  atom operands are structural `AtomPoolIndex` values into that function's
  pool, never predefined ordinals, tagged integer atoms, runtime atom
  identities, or pointers.
- A serialized graph-wide string/atom dictionary may deduplicate transport
  data, but the reader must materialize or validate each function's local pool
  before body verification. An index has no meaning in a parent, child, or
  sibling function merely because the numeric position exists there.
- The current compiler graph accepts only opaque owned String atoms and rejects
  duplicate exact contents within each local pool. Future serialized entries
  also carry explicit namespace/predefined metadata. Loading a verified
  function later reinterns or creates each local entry exactly once in the
  destination runtime. Verification itself never interns into a runtime.
- Nullable metadata atom fields use an explicit optional representation.
  Instruction operands and required metadata fields cannot encode null by
  smuggling a sentinel integer into `AtomPoolIndex`.
- `vardefs.len() == arg_count + var_count`;
- `defined_arg_count <= arg_count`;
- `var_ref_count <= arg_count + var_count`;
- `arg_count`, `var_count`, `var_ref_count`, and `closure_var_count` are each
  at most 65,534; all sums must fit `usize`;
- each present metadata atom index is below that function's atom-pool length,
  and absence is accepted only for fields whose upstream semantics permit it;
- `scope_next` is `ARG_SCOPE_END` (-2), -1, or a local index; every chain is
  in range and acyclic;
- each captured vardef has `var_ref_idx < var_ref_count`, captured vardefs have
  unique indices, and their indices are exactly `0..var_ref_count`;
- serialized `stack_size` is never trusted for allocation. It must equal the
  verifier's recomputed maximum or verification fails;
- debug/source-map executable PCs must be instruction starts. Range end PCs may
  equal `bytecode_len`, but no executable lookup PC may do so.

### Operand indices

**Rust hardening.** Every operand is checked against its semantic domain:

| Operand family | Required validation |
| --- | --- |
| `const`, `const8` | index is below `cpool.len()`; staged compiler input additionally requires a value-kind entry, while serialized input remains unsupported |
| `fclosure`, `fclosure8` | constant exists and is a bytecode-function constant; body-only compiler verification validates the declared kind, `VerifiedCompilerFunctionGraph` resolves and verifies the actual compiler child target, and serialized input remains pending the complete `VerifiedBytecode` graph |
| atom-bearing formats | `AtomPoolIndex::get()` is below the enclosing function's atom-pool length; the referenced entry's namespace is valid for the opcode |
| `loc`, `loc8`, `none_loc` | index is below `var_count` |
| `arg`, `none_arg` | index is below `arg_count` |
| `var_ref`, `none_var_ref` | index is below `closure_var_count`, not `var_ref_count` |
| `make_loc_ref` | atom-pool index and namespace are valid, local index is below `var_count`, and its vardef is captured |
| `make_arg_ref` | atom-pool index and namespace are valid, argument index is below `arg_count`, and its vardef is captured |
| `make_var_ref_ref` | atom-pool index and namespace are valid and index is below `closure_var_count` |
| `close_loc` | local index is below `var_count`; staged compiler input additionally requires an explicit matching scoped-local capture, while serialized input remains unsupported |
| `rest` | first argument index is at most `arg_count` |
| `eval`, `apply_eval` | decoded scope start is -2, -1, or a local index and its `scope_next` chain is valid |

Value and function operands share one constant-pool index domain. Compiler
lowering selects the compact `push_const8`/`fclosure8` forms for indices
`0..=255` and the full-width `push_const`/`fclosure` forms for indices
`>= 256`; neither kind receives a separate compact namespace.

The distinct local, argument, and closure-reference operand domains are visible
in the opcode table (`quickjs-opcode.h:159-177`,
`quickjs-opcode.h:196-199`, `quickjs-opcode.h:305-344`) and in the interpreter's
direct indexing (`quickjs.c:18583-18720`, `quickjs.c:18803-18832`).
Upstream disassembly also resolves `var_ref` operands against
`closure_var_count` (`quickjs.c:32364-32399`). `fclosure` passes its pool entry
to code that immediately treats it as a function-bytecode pointer, so type
validation is mandatory (`quickjs.c:17937-17943`,
`quickjs.c:18191-18198`, `quickjs.c:17395-17409`).

### Closure descriptors and child functions

Upstream closure descriptors distinguish parent locals, parent arguments,
parent closure references, globals, and module bindings
(`quickjs.c:611-633`). Runtime instantiation indexes the parent according to
that discriminator (`quickjs.c:17323-17354`).

**Rust hardening.**

- the closure discriminator and all reserved flag bits are valid;
- `LOCAL` indexes a captured parent local;
- `ARG` indexes a captured parent argument;
- `REF` and `GLOBAL_REF` index the parent's `closure_var` table;
- `GLOBAL`, `GLOBAL_DECL`, `MODULE_DECL`, and `MODULE_IMPORT` are accepted only
  in the corresponding eval/module context, and module indices are checked
  against the enclosing module tables;
- `is_const` implies `is_lexical`, and `var_kind` is a defined upstream kind;
- every bytecode-function constant is verified, whether or not an `fclosure`
  currently references it;
- child functions form an acyclic graph. A shared child's intrinsic body and
  metadata are verified once by identity, but every parent edge separately
  validates the child's closure descriptors against that parent; a graph back
  edge is rejected. All graph walks use bounded explicit work lists rather
  than recursive calls.

### Counts, packed offsets, enums, and flags

**Rust hardening.** Secondary operands and implied counts are checked as
follows:

- Dynamic call/eval pop counts are computed as
  `metadata_pops + encoded_argc` using checked `usize`; short `call0..call3`
  add their opcode-implied count. This is the calculation performed by
  upstream stack analysis (`quickjs.c:35681-35709`).
- `for_of_next(k)` requires at least `3 + k` slots and an iterator tuple at
  the referenced relative position; upstream computes its iterator offset as
  `-3-k` (`quickjs.c:19003-19011`).
- Each packed `copy_data_properties` target/source/exclusion offset must be
  within the current stack and select an ordinary JavaScript value
  (`quickjs.c:19705-19719`).
- `with_*`'s `is_with` byte is 0 or 1.
- `special_object` is one of the seven defined values; upstream aborts on any
  other value (`quickjs.c:17750-17759`, `quickjs.c:17992-18035`).
- `throw_error` kind is 0 through 4 (`quickjs.c:18358-18387`).
- `apply` magic is 0, 1, or 2; all other values are rejected
  (`quickjs.c:18275-18290`, `quickjs.c:41094-41118`).
- `iterator_call` flags are the compiler-produced values 0, 1, or 2; unknown
  combinations are rejected even though the C interpreter reads individual
  bits (`quickjs.c:19084-19116`, `quickjs.c:27960-28005`).
- `define_method` kind is method, getter, or setter, with only the enumerable
  bit additionally allowed; kind 3 and unknown bits are rejected
  (`quickjs.c:19346-19403`).
- `define_class` permits only `HAS_HERITAGE`
  (`quickjs.c:17450-17464`, `quickjs.c:19406-19418`).
- Serialized function flags, vardef flags, closure flags, and stored `js_mode`
  reject every disallowed bit. Stored function mode permits strict only; async
  and backtrace-barrier bits are synthesized only in runtime frame mode
  (`quickjs.c:403-405`, `quickjs.c:20785-20798`,
  `quickjs.c:37191-37205`).
- `has_prototype` and `is_derived_class_constructor` require a normal function
  kind and are mutually exclusive (`quickjs.c:25008-25015`,
  `quickjs.c:36513-36525`).
- `initial_yield` and `yield` require a generator kind; `yield_star` requires
  a synchronous generator; `async_yield_star` requires an async generator;
  `await` requires an async kind; and `return_async` requires a non-normal
  function. The upstream
  parser/compiler enforces async `await`, generator `yield`, initial yield only
  for generator kinds, and `return_async` for non-normal kinds
  (`quickjs.c:27559-27569`, `quickjs.c:27888-27914`,
  `quickjs.c:36759-36784`).
- `return`, `return_undef`, `tail_call`, and `tail_call_method` require a
  normal function. QuickJS routes them through ordinary frame cleanup, a path
  that must never be reached by a generator frame
  (`quickjs.c:18209-18264`, `quickjs.c:20573-20593`).

## Typed abstract stack

Upstream computes only a height and a current catch PC. It rejects underflow,
heights above 65,534, and joins with different heights or catch PCs
(`quickjs.c:35582-35633`, `quickjs.c:35681-35709`). It assumes catch markers are
removed only by a small set of opcodes (`quickjs.c:35768-35816`).

**Rust hardening.** The verifier instead carries a full abstract stack:

```text
Slot =
    JsValue
  | RawFunction(child_id)
  | Catch { handler_pc, marker_id }
  | IteratorCatch { marker_id }
  | DisabledIteratorCatch { marker_id }
  | ReturnAddress {
        subroutine_pc,
        continuations: { continuation_pc -> resume_shape_id }
    }
```

`Catch`, iterator markers, and return addresses are non-forgeable verifier/VM
types. They are never represented as ordinary JavaScript integers. A
`RawFunction` is the internal function-bytecode constant consumed by class
construction; it is not a JavaScript value.

Fixed-effect ordinary opcodes must pop `JsValue` slots and push `JsValue`
slots. Stack permutations use their exact upstream transformations. They may
not duplicate or move an internal slot. An internal slot may be consumed only
by an explicitly specified transition below. `RawFunction` may be consumed by
the matching class-definition operation; it may not be returned or passed to
an ordinary JavaScript operation.

`push_const`/`push_const8` pushes `RawFunction(child)` when the selected pool
entry is a bytecode-function constant and `JsValue` otherwise.
`fclosure`/`fclosure8` validates a bytecode-function pool entry and pushes the
resulting ordinary `JsValue` directly. `define_class` requires
`JsValue(parent), RawFunction(ctor)` and replaces them with
`JsValue(ctor), JsValue(prototype)`; the computed-name form preserves its
leading name value. These stack effects are defined in the opcode table
(`quickjs-opcode.h:69-70`, `quickjs-opcode.h:154-157`), and the upstream class
compiler deliberately uses `push_const` before `define_class`
(`quickjs.c:25222-25237`).

The active catch/iterator chain is derived from marker slots. Marker identity,
kind, stack position, and handler PC are therefore part of the state rather
than a side-channel height.

### Successors

Let `H` be the entry height. Metadata effects are applied before the listed
successors unless an opcode has a specialized transition.

| Opcode family | Successor state |
| --- | --- |
| ordinary | next PC with metadata pop/push effect |
| `goto*` | encoded target at height `H` |
| `if_true*`, `if_false*` | target and next at `H - 1` |
| `with_get_var`, `with_delete_var` | target at `H`; next at `H - 1` |
| `with_make_ref`, `with_get_ref` | target at `H + 1`; next at `H - 1` |
| `with_put_var` | target at `H - 2`; next at `H - 1` |
| `catch` | handler and next as specified below |
| `gosub`, `ret` | dynamic typed edges specified below |
| `for_of_start`, `for_await_of_start` | next at `H + 2` with iterator marker |
| `initial_yield`, `yield*`, `await` | resume at next PC with metadata effect |
| `tail_call*`, `return*`, `throw*` | no normal successor |

The unusual `with_*` heights and target edges match upstream's special stack
analysis (`quickjs.c:35751-35766`) and interpreter behavior
(`quickjs.c:20389-20471`).

### Catch transitions and unwinding

For input stack `S`, `catch handler` has two edges:

```text
normal next:  S, Catch(handler, id)
handler:      S, JsValue        // the thrown exception
```

The handler edge uses the outer handler chain. This models the runtime, which
pushes a catch-offset marker on normal entry, then pops that marker and pushes
the exception while unwinding (`quickjs.c:18948-18955`,
`quickjs.c:20553-20569`).

`drop`, `nip`, and `nip1` use their exact positional transformations. If their
discarded slot is an enabled `Catch` or `IteratorCatch`, it must be the
innermost active marker and the operation removes that handler. A
`DisabledIteratorCatch` is not removable by these generic operations. Any
other ordinary consumption of a marker is rejected.

`nip_catch` requires a top `JsValue` and an innermost enabled `Catch` or
`IteratorCatch` below it. It discards the slots between them, replaces the
marker with the saved value, and removes that handler. It may not cross a
return address, raw function, disabled iterator marker, or another internal
slot. This is the typed form of the runtime's backward marker scan
(`quickjs.c:19052-19067`).

No per-instruction exception edge is needed: the `catch` instruction seeds its
handler state once, as upstream stack analysis does. Iterator markers have no
handler PC; exception unwinding closes the iterator and continues to the next
outer marker (`quickjs.c:20553-20569`).

### Iterator transitions

```text
for_of_start / for_await_of_start:
    ..., JsValue
 -> ..., JsValue(iter), JsValue(next), IteratorCatch(id)

for_await_of_next:
    ..., JsValue(iter), JsValue(next), IteratorCatch(id)
 -> ..., JsValue(iter), JsValue(next), DisabledIteratorCatch(id), JsValue(obj)

iterator_get_value_done:
    ..., DisabledIteratorCatch(id), JsValue(obj)
 -> ..., IteratorCatch(id), JsValue(value), JsValue(done)
```

The disabled state is required because upstream temporarily replaces the
iterator catch marker with `undefined`, then reinstalls it after extracting
`value` and `done` (`quickjs.c:16727-16739`, `quickjs.c:16761-16779`).

The remaining iterator transfers are:

```text
for_of_next(k):
    S, JsValue(iter), JsValue(next), IteratorCatch(id), k * JsValue
 -> S, JsValue(iter), JsValue(next), IteratorCatch(id), k * JsValue,
       JsValue(value), JsValue(done)

iterator_next:
    S, JsValue(iter), JsValue(next), IteratorCatch(id), JsValue(argument)
 -> S, JsValue(iter), JsValue(next), IteratorCatch(id), JsValue(result)

iterator_call:
    S, JsValue(iter), JsValue(next), IteratorCatch(id), JsValue(argument)
 -> S, JsValue(iter), JsValue(next), IteratorCatch(id), JsValue(result),
       JsValue(method_missing)
```

For `for_of_next(k)`, the tuple is at offsets `[-3-k, -2-k, -1-k]`; all `k`
intervening slots must be ordinary values. `iterator_close` consumes
`JsValue(iter), JsValue(next), IteratorCatch` and removes that handler. It may
also consume three ordinary values for the upstream compiler's dummy-offset
cleanup form. These shapes follow the upstream iterator opcode effects and
runtime stack comments (`quickjs-opcode.h:202-211`,
`quickjs.c:19003-19018`, `quickjs.c:19039-19116`); the dummy form is emitted at
`quickjs.c:28325-28327`.

### `gosub` and `ret`

Upstream `gosub` pushes its `p + 5` continuation as an ordinary integer and
contains an explicit security warning; `ret` accepts any integer below
`bytecode_len` without checking an instruction boundary
(`quickjs.c:18957-18982`). Upstream stack analysis explores the subroutine with
one extra slot and separately seeds a synthetic continuation
(`quickjs.c:35746-35750`, `quickjs.c:35820-35822`).

**Rust hardening.**

- `gosub target` has no direct successor. It sends
  `S, ReturnAddress(target, {p+5 -> shape(S)})` to `target`.
- A return address cannot be forged, duplicated, converted to `JsValue`,
  consumed by another opcode, or moved by a stack permutation.
- Multiple `gosub` sites may join at the same subroutine. Their return-address
  slots union continuation maps only when the subroutine PC and every slot
  below the address have compatible types and marker identities.
- `ret` requires a `ReturnAddress` on top. After popping it, the remaining
  state must equal the recorded resume shape for every continuation. It emits
  one edge to each exact continuation.
- Growth of a continuation set requeues the joined PC and reaches a bounded
  fixpoint. A bare integer, missing address, extra stack value, changed marker
  chain, cross-subroutine address, or non-boundary continuation is rejected.

## Stack maximum and joins

**Upstream.** The maximum permitted operand-stack depth is 65,534
(`quickjs.c:208-212`), and compiler analysis rejects inconsistent stack height
or catch position at a join (`quickjs.c:35595-35618`).

**Rust hardening.**

- Every pop checks both height and required slot types.
- The maximum includes handler edges, iterator transitions, dynamic call
  effects, and the extra `ReturnAddress` at `gosub` targets.
- A computed maximum of 65,534 is accepted; 65,535 is rejected.
- A PC has one canonical abstract state. Joins require equal height and
  position-wise equal slot kinds. `Catch`, iterator, disabled-iterator, and raw
  function slots also require equal identity/target. `JsValue` joins with
  `JsValue`.
- Return-address slots for the same subroutine union compatible continuation
  maps. This is the only join that changes an existing state; a changed union
  is reprocessed.
- Different handler chains, marker positions, raw child identities,
  subroutine identities, or resume shapes are verifier errors, even when
  heights match.

## Mandatory resource limits

Limits are charged before per-entry validation and across the complete nested
function graph. The body and graph verifiers accept explicit limit profiles;
untrusted APIs start with this **provisional** default profile:

| Resource | Provisional default maximum |
| --- | ---: |
| serialized unit bytes | 128 MiB |
| function nesting depth | 256 |
| distinct bytecode functions | 65,535 |
| bytecode bytes in one function | 16 MiB |
| bytecode bytes in the graph | 64 MiB |
| instructions in one function | 4,194,304 |
| instructions in the graph | 8,388,608 |
| constants in one function | 262,144 |
| constant-pool slots in the graph | 1,048,576 |
| distinct constant graph nodes | 1,048,576 |
| atoms in the graph | 1,048,576 |
| aggregate String constant/atom payload bytes | 64 MiB |
| `gosub` sites in one function | 65,534 |
| compiler branch-relaxation instruction visits | 33,554,432 |
| total transfer-function evaluations | 33,554,432 |
| closure-source evaluations across parent edges | 33,554,432 |

These values are **provisional Rust hardening policy**, not upstream QuickJS
limits and not yet compatibility claims. Before the untrusted bytecode API is
stabilized, corpus measurements must record the largest per-function and
aggregate values for the pinned upstream suites, Test262 baseline, generated
stress fixtures, and representative application bundles. Ratified defaults
must leave documented headroom while retaining bounded worst-case memory and
transfer work; any changed numbers update this table and its limit tests. The
current compiler applies the transfer-evaluation number independently as its
pre-verification branch-relaxation visit limit, so both assembly and verifier
graph work are bounded without sharing a mutable counter.

A caller may lower the current profile. Raising it is available only through
an explicit trusted configuration, never as an implicit retry after
`LimitExceeded`. Structural maxima such as stack depth and 16-bit index
domains remain unchanged. Shared child function bodies are charged once,
while their capture sources are charged and checked separately for every
parent edge. Every `Value` and `Function` pool entry is charged to the constant
budget. Only `Function` entries create child edges; value-only pools do not
increase function nesting depth. Cycles are rejected through the iterative
topology pass rather than recursively charged.

## Acceptance tests

The verifier is complete only when the following tests are automated.

1. **Pinned corpus:** every supported compiler-produced final-bytecode fixture
   verifies; recomputed stack maxima equal the stored values.
2. **Decode:** reject empty input, invalid/unknown final opcodes, every
   truncated operand width, and malformed unreachable bytes. Verify that typed
   finalization rejects temporary IR, while every overlapping raw value is
   decoded as its final short opcode. Permit well-formed unreachable
   instructions.
3. **Targets:** cover forward and backward 8-, 16-, and 32-bit jumps, every
   conditional, `catch`, `gosub`, and every `with_*` target. Reject overflow,
   negative/out-of-range targets, `bytecode_len`, operand-byte targets, catch
   target zero, and a non-boundary `gosub` continuation.
4. **Stack:** reject underflow for every fixed and dynamic effect, branch joins
   with different heights, joins with different typed slots or catch chains,
   and serialized/computed stack mismatches. Accept maximum 65,534 and reject
   65,535.
5. **Catch:** cover nested handlers plus marker removal by `drop`, `nip`,
   `nip1`, and `nip_catch`; reject missing, crossed, duplicated, disabled, or
   mismatched markers.
6. **Iterators:** cover sync and async starts, `for_of_next` at offset zero and
   nonzero, async disable/reinstate, normal and dummy `iterator_close`, nested
   outer catches, and exception-unwind states.
7. **Finally:** cover one and many `gosub` callers, nested subroutines, and
   return-address set growth. Reject forged integers, `ret` without an address,
   a moved/duplicated address, extra or missing return stack slots, wrong
   resume shapes, and mixed subroutine identities.
8. **Indices:** for each index family, accept `count - 1` and reject `count`;
   include implied short indices, `var_ref` versus `closure_var_count`,
   captured-vardef bijection, heterogeneous value/function pools,
   declared/actual kind mismatches in both directions, shared compact index
   255 versus full-width index 256, wrong-type `fclosure` constants, class raw
   functions, invalid scope chains, and child closure descriptors.
9. **Flags:** exhaust valid and invalid values for `special_object`,
   `throw_error`, `apply`, `iterator_call`, method/class flags, function flags,
   function-kind opcode constraints, packed stack offsets, and reserved bits.
10. **Limits:** use deliberately small custom limits to hit every per-function
    and aggregate budget, nesting depth, shared-child accounting, graph cycle,
    `gosub` count, and worklist-step exhaustion without large allocations.
11. **Fuzzing:** mutate compiler-produced bytes and serialized metadata.
    Verification must be deterministic, must not panic or allocate beyond the
    configured budget, and the VM must never receive a value unless
    verification succeeded.
