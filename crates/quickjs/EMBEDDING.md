# Embedding & interop API (V8-flavored)

A host-facing API for embedding the pure-Rust QuickJS engine in Rust and
interoperating with JavaScript, modeled on the shape of the Rust `v8` crate
(`Isolate` / `Context` / `Local<T>` / templates / `TryCatch`) but adapted to
this engine's existing ownership model.

## Guiding decision: handles, not scopes

V8 ties values to `HandleScope`s with `Local<'s, T>` lifetimes. This engine
already has an equivalent, more Rust-idiomatic primitive: a *rooted* handle
(`JsValue(Arc<ValueRoot>)`, `!Send + !Sync`) that keeps its heap object alive
across scope exits until the handle is dropped. We do **not** port V8's
scope/lifetime machinery; we keep the rooted handles and give them a
V8-shaped method surface (`get`/`set`/`call`/`construct`/`to_string`/…). This
is the deliberate, documented divergence from the `v8` crate.

## API surface

### Values and conversions (on `JsValue`)

- `is_undefined()`, `is_null()`, `is_boolean()`, `is_number()`, `is_string()`,
  `is_object()`, `is_function()`, `is_array()`, `is_promise()`.
- `to_boolean(ctx)`, `to_number(ctx)`, `to_string(ctx)`, `to_object(ctx)`,
  `to_rust_string(ctx)` — JS-coercion-complete reads with typed errors.
- Constructors already live on `Context`: `undefined()`, `null()`,
  `boolean()`, `number()`, `string()`, `symbol()`.

### Objects (`Object`)

- `get(ctx, key)`, `set(ctx, key, value)`, `delete(ctx, key)`,
  `has(ctx, key)`, `define_own_property(ctx, key, desc)`,
  `own_property_names(ctx)`, `own_property_symbols(ctx)`, `own_keys(ctx)`.
- `prototype(ctx)`, `set_prototype(ctx, proto)`, `is_extensible(ctx)`.

### Functions (`Function`)

- `call(ctx, receiver, args) -> Result<JsValue, CallError>`.
- `construct(ctx, args) -> Result<JsValue, CallError>`.
- `name(ctx)`, `length(ctx)`.

### Native functions (Rust callbacks)

- `Context::create_host_function(name, callback) -> Result<Function, …>`
  registers a `Box<dyn Fn(&mut Context, HostCall) -> Result<JsValue, JsValue>>`
  as a real JavaScript function. `HostCall` exposes `this`, `arguments()`,
  `new_target`, and the `Context`.
- A thin `FunctionTemplate` facade mirrors V8's `FunctionTemplate::new +
  get_function`, and `ObjectTemplate` mirrors `set`/`new_instance`, on top of
  `create_host_function` and `Object` primitives.

### Errors: `TryCatch`

- A `TryCatch<'ctx>` captures the first thrown exception from a call and
  exposes `has_caught()`, `exception(ctx)`, `rethrow(ctx)`; the underlying
  `CallError::Thrown(JsValue)` carries the same information without a scope.

## Mapping to internals

| V8 concept | QuickJS primitive |
|---|---|
| `Isolate` | `Runtime` (heap; owns `Realm`s) |
| `Context` | `Context<'_>` (realm + runtime) |
| `Local<T>` | `JsValue` / `Object` / `Function` (rooted) |
| `Global<T>` | `JsValue` (already rooted; drop to unroot) |
| `FunctionTemplate` | `Context::create_host_function` |
| `TryCatch` | `CallError::Thrown(JsValue)` + `TryCatch` helper |

## Runtime plumbing required

1. **Host-function registry**: add `NativeFunctionKind::Host(HostFunctionId)`
   and a `HostFunctionId → Box<dyn HostCallback>` table on `Runtime`; dispatch
   it in `dispatch_native_call_with_frames`. The callback receives
   `StoredValue` arguments (converted to rooted `JsValue`s) and returns a
   `StoredValue` (or throws a `StoredValue`).
2. **Call / construct entry point**: a `pub` path that installs a prepared
   `CallInputs { receiver, arguments, new_target }` and runs a function to a
   rooted `JsValue`, mirroring `execute_global_script`'s completion→`JsValue`
   conversion and error→`CallError` conversion.
3. **Property access entry point**: wrap the existing internal
   `begin_internal_get` / `define_own_property` / own-keys paths in public
   `Context`/`Object` methods that return rooted values.

## Milestones

1. Host-function registry + `create_host_function` (foundational).
2. `Function::call` / `construct` + `CallError` + `TryCatch`.
3. `Object` property access (get/set/define/has/keys).
4. Value conversions + `FunctionTemplate`/`ObjectTemplate` facade polish.
