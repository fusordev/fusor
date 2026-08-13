//! `%Uint8Array%` base64 and hexadecimal conversion methods.
//!
//! The option reads are explicit resumable boundaries because accessors may
//! re-enter the interpreter and detach or resize a receiver's backing buffer.
//! Decoding itself is non-observable and returns partial bytes alongside its
//! error marker so `setFromBase64` and `setFromHex` can publish the specified
//! prefix before throwing `SyntaxError`.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

mod codec;

use codec::{decode_base64, decode_hex, encode_base64, hex_digit};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Base64Alphabet {
    Standard,
    Url,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LastChunkHandling {
    Loose,
    Strict,
    StopBeforePartial,
}

enum Uint8ArrayBase64Operation {
    From { input: JsString },
    Set { target: ObjectId, input: JsString },
    Encode { target: ObjectId },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Uint8ArrayBase64OptionStage {
    Alphabet,
    FinalOption,
}

pub(super) struct Uint8ArrayBase64Continuation {
    operation: Uint8ArrayBase64Operation,
    options: StoredValue,
    alphabet: Base64Alphabet,
    stage: Uint8ArrayBase64OptionStage,
    realm: RealmId,
    origin: JsStackFrame,
}

impl Uint8ArrayBase64Continuation {
    pub(super) fn retained_values(&self) -> u64 {
        match self.operation {
            Uint8ArrayBase64Operation::Set { .. } => 3,
            Uint8ArrayBase64Operation::From { .. } | Uint8ArrayBase64Operation::Encode { .. } => 2,
        }
    }

    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        trace_stored_value_root(&self.options, mark);
        match self.operation {
            Uint8ArrayBase64Operation::Set { target, .. }
            | Uint8ArrayBase64Operation::Encode { target } => {
                mark(CollectionRoot::Heap(HeapReference::Object(target)));
            }
            Uint8ArrayBase64Operation::From { .. } => {}
        }
    }
}

struct DecodeResult {
    read: usize,
    bytes: Vec<u8>,
    error: bool,
}

#[allow(
    clippy::too_many_arguments,
    reason = "the intrinsic dispatcher preserves the standard native call context"
)]
pub(super) fn dispatch_uint8_array_method(
    runtime: &mut Runtime,
    method: Uint8ArrayMethod,
    realm: RealmId,
    receiver: &StoredValue,
    mut arguments: CallArguments,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match method {
        Uint8ArrayMethod::FromBase64 => {
            let input = require_string(
                arguments.take_first_or_undefined(),
                realm,
                &origin,
                "Uint8Array.fromBase64 requires a String",
            )?;
            let options = arguments.take_first_or_undefined();
            begin_uint8_array_base64_options(
                runtime,
                Uint8ArrayBase64Operation::From { input },
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        Uint8ArrayMethod::FromHex => {
            let input = require_string(
                arguments.take_first_or_undefined(),
                realm,
                &origin,
                "Uint8Array.fromHex requires a String",
            )?;
            finish_uint8_array_from_hex(runtime, &input, realm, &origin, execution_budget)
        }
        Uint8ArrayMethod::SetFromBase64 => {
            let target = validate_uint8_array(runtime, receiver, realm, &origin)?;
            reject_immutable_target(runtime, target, realm, &origin)?;
            let input = require_string(
                arguments.take_first_or_undefined(),
                realm,
                &origin,
                "Uint8Array.prototype.setFromBase64 requires a String",
            )?;
            let options = arguments.take_first_or_undefined();
            begin_uint8_array_base64_options(
                runtime,
                Uint8ArrayBase64Operation::Set { target, input },
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        Uint8ArrayMethod::SetFromHex => {
            let target = validate_uint8_array(runtime, receiver, realm, &origin)?;
            reject_immutable_target(runtime, target, realm, &origin)?;
            let input = require_string(
                arguments.take_first_or_undefined(),
                realm,
                &origin,
                "Uint8Array.prototype.setFromHex requires a String",
            )?;
            finish_uint8_array_set_from_hex(
                runtime,
                target,
                &input,
                realm,
                &origin,
                execution_budget,
            )
        }
        Uint8ArrayMethod::ToBase64 => {
            let target = validate_uint8_array(runtime, receiver, realm, &origin)?;
            let options = arguments.take_first_or_undefined();
            begin_uint8_array_base64_options(
                runtime,
                Uint8ArrayBase64Operation::Encode { target },
                options,
                realm,
                return_to,
                origin,
                execution_budget,
            )
        }
        Uint8ArrayMethod::ToHex => {
            let target = validate_uint8_array(runtime, receiver, realm, &origin)?;
            finish_uint8_array_to_hex(runtime, target, realm, &origin, execution_budget)
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the option machine retains its operation and native call context across accessors"
)]
fn begin_uint8_array_base64_options(
    runtime: &mut Runtime,
    operation: Uint8ArrayBase64Operation,
    options: StoredValue,
    realm: RealmId,
    return_to: Option<CallReturn>,
    origin: JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    if matches!(options, StoredValue::Undefined) {
        return finish_uint8_array_base64_operation(
            runtime,
            operation,
            Base64Alphabet::Standard,
            LastChunkHandling::Loose,
            false,
            realm,
            &origin,
            execution_budget,
        );
    }
    if options.heap_reference().is_none() {
        return uint8_array_type_error(realm, &origin, "options must be an Object");
    }
    read_uint8_array_base64_option(
        runtime,
        Uint8ArrayBase64Continuation {
            operation,
            options,
            alphabet: Base64Alphabet::Standard,
            stage: Uint8ArrayBase64OptionStage::Alphabet,
            realm,
            origin,
        },
        return_to,
        execution_budget,
    )
}

fn uint8_array_base64_continuation(state: Uint8ArrayBase64Continuation) -> NativeContinuation {
    NativeContinuation::Uint8ArrayBase64(Box::new(state))
}

fn read_uint8_array_base64_option(
    runtime: &mut Runtime,
    state: Uint8ArrayBase64Continuation,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let (atom, diagnostic_name) = match state.stage {
        Uint8ArrayBase64OptionStage::Alphabet => (PredefinedAtom::Alphabet, "alphabet"),
        Uint8ArrayBase64OptionStage::FinalOption => match state.operation {
            Uint8ArrayBase64Operation::From { .. } | Uint8ArrayBase64Operation::Set { .. } => {
                (PredefinedAtom::LastChunkHandling, "lastChunkHandling")
            }
            Uint8ArrayBase64Operation::Encode { .. } => {
                (PredefinedAtom::OmitPadding, "omitPadding")
            }
        },
    };
    charge_heap_property_lookup(runtime, &state.options, execution_budget)?;
    let name = JsString::from_utf8(diagnostic_name)?;
    let dispatch = begin_value_get(
        runtime,
        &state.options,
        runtime.predefined_property_key(atom),
        Some(&name),
        state.realm,
        return_to,
        state.origin.clone(),
        execution_budget,
    )?;
    continue_get_after(
        dispatch,
        state,
        uint8_array_base64_continuation,
        |state, value| {
            advance_uint8_array_base64_options(runtime, state, value, return_to, execution_budget)
        },
        "Uint8Array base64 option Get produced a structured result",
    )
}

pub(super) fn advance_uint8_array_base64_options(
    runtime: &mut Runtime,
    mut state: Uint8ArrayBase64Continuation,
    value: StoredValue,
    return_to: Option<CallReturn>,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match state.stage {
        Uint8ArrayBase64OptionStage::Alphabet => {
            state.alphabet = parse_base64_alphabet(value, state.realm, &state.origin)?;
            state.stage = Uint8ArrayBase64OptionStage::FinalOption;
            read_uint8_array_base64_option(runtime, state, return_to, execution_budget)
        }
        Uint8ArrayBase64OptionStage::FinalOption => match state.operation {
            operation @ (Uint8ArrayBase64Operation::From { .. }
            | Uint8ArrayBase64Operation::Set { .. }) => {
                let last_chunk = parse_last_chunk_handling(value, state.realm, &state.origin)?;
                finish_uint8_array_base64_operation(
                    runtime,
                    operation,
                    state.alphabet,
                    last_chunk,
                    false,
                    state.realm,
                    &state.origin,
                    execution_budget,
                )
            }
            operation @ Uint8ArrayBase64Operation::Encode { .. } => {
                finish_uint8_array_base64_operation(
                    runtime,
                    operation,
                    state.alphabet,
                    LastChunkHandling::Loose,
                    runtime.to_boolean(&value)?,
                    state.realm,
                    &state.origin,
                    execution_budget,
                )
            }
        },
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one completion point serves the three base64 methods after their shared option reads"
)]
fn finish_uint8_array_base64_operation(
    runtime: &mut Runtime,
    operation: Uint8ArrayBase64Operation,
    alphabet: Base64Alphabet,
    last_chunk: LastChunkHandling,
    omit_padding: bool,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    match operation {
        Uint8ArrayBase64Operation::From { input } => {
            execution_budget.charge_instructions(u64::from(input.len()).saturating_add(1))?;
            let result = decode_base64(&input, alphabet, last_chunk, usize::MAX)?;
            if result.error {
                return uint8_array_syntax_error(realm, origin, "invalid base64 string");
            }
            let target = allocate_uint8_array(runtime, realm, result.bytes.len(), origin)?;
            store_uint8_array_bytes(runtime, target, &result.bytes)?;
            Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
        }
        Uint8ArrayBase64Operation::Set { target, input } => {
            let length = uint8_array_length(runtime, target, realm, origin)?;
            execution_budget.charge_instructions(u64::from(input.len()).saturating_add(1))?;
            let result = decode_base64(&input, alphabet, last_chunk, length)?;
            store_uint8_array_bytes(runtime, target, &result.bytes)?;
            if result.error {
                return uint8_array_syntax_error(realm, origin, "invalid base64 string");
            }
            uint8_array_decode_result_object(runtime, realm, result.read, result.bytes.len())
        }
        Uint8ArrayBase64Operation::Encode { target } => {
            let bytes = get_uint8_array_bytes(runtime, target, realm, origin)?;
            execution_budget.charge_instructions(usize_to_u64(bytes.len()).saturating_add(1))?;
            let encoded = encode_base64(&bytes, alphabet, omit_padding)?;
            Ok(NativeDispatch::Immediate(StoredValue::String(encoded)))
        }
    }
}

fn finish_uint8_array_from_hex(
    runtime: &mut Runtime,
    input: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(u64::from(input.len()).saturating_add(1))?;
    let result = decode_hex(input, usize::MAX)?;
    if result.error {
        return uint8_array_syntax_error(realm, origin, "invalid hexadecimal string");
    }
    let target = allocate_uint8_array(runtime, realm, result.bytes.len(), origin)?;
    store_uint8_array_bytes(runtime, target, &result.bytes)?;
    Ok(NativeDispatch::Immediate(StoredValue::Object(target)))
}

fn finish_uint8_array_set_from_hex(
    runtime: &mut Runtime,
    target: ObjectId,
    input: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let length = uint8_array_length(runtime, target, realm, origin)?;
    execution_budget.charge_instructions(u64::from(input.len()).saturating_add(1))?;
    let result = decode_hex(input, length)?;
    store_uint8_array_bytes(runtime, target, &result.bytes)?;
    if result.error {
        return uint8_array_syntax_error(realm, origin, "invalid hexadecimal string");
    }
    uint8_array_decode_result_object(runtime, realm, result.read, result.bytes.len())
}

fn finish_uint8_array_to_hex(
    runtime: &Runtime,
    target: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    let bytes = get_uint8_array_bytes(runtime, target, realm, origin)?;
    execution_budget.charge_instructions(usize_to_u64(bytes.len()).saturating_add(1))?;
    let output_length = bytes
        .len()
        .checked_mul(2)
        .ok_or(ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: bytes.len(),
        })?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_length)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: output_length,
        })?;
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    Ok(NativeDispatch::Immediate(StoredValue::String(
        JsString::from_latin1(&output)?,
    )))
}

fn validate_uint8_array(
    runtime: &Runtime,
    receiver: &StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    let StoredValue::Object(object) = receiver else {
        return uint8_array_type_error(realm, origin, "receiver is not a Uint8Array");
    };
    let Some(state) = runtime.typed_array_state(*object)? else {
        return uint8_array_type_error(realm, origin, "receiver is not a Uint8Array");
    };
    if state.element() != TypedArrayElementType::Uint8 {
        return uint8_array_type_error(realm, origin, "receiver is not a Uint8Array");
    }
    Ok(*object)
}

fn reject_immutable_target(
    runtime: &Runtime,
    target: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<(), NativeFailure> {
    if runtime.is_typed_array_backing_buffer_immutable(target)? {
        return uint8_array_type_error(realm, origin, "Uint8Array backing buffer is immutable");
    }
    Ok(())
}

fn require_string(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<JsString, NativeFailure> {
    let StoredValue::String(value) = value else {
        return uint8_array_type_error(realm, origin, message);
    };
    Ok(value)
}

fn parse_base64_alphabet(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Base64Alphabet, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(Base64Alphabet::Standard),
        StoredValue::String(value) if string_equals_ascii(&value, b"base64") => {
            Ok(Base64Alphabet::Standard)
        }
        StoredValue::String(value) if string_equals_ascii(&value, b"base64url") => {
            Ok(Base64Alphabet::Url)
        }
        _ => uint8_array_type_error(realm, origin, "invalid base64 alphabet option"),
    }
}

fn parse_last_chunk_handling(
    value: StoredValue,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<LastChunkHandling, NativeFailure> {
    match value {
        StoredValue::Undefined => Ok(LastChunkHandling::Loose),
        StoredValue::String(value) if string_equals_ascii(&value, b"loose") => {
            Ok(LastChunkHandling::Loose)
        }
        StoredValue::String(value) if string_equals_ascii(&value, b"strict") => {
            Ok(LastChunkHandling::Strict)
        }
        StoredValue::String(value) if string_equals_ascii(&value, b"stop-before-partial") => {
            Ok(LastChunkHandling::StopBeforePartial)
        }
        _ => uint8_array_type_error(realm, origin, "invalid lastChunkHandling option"),
    }
}

fn string_equals_ascii(value: &JsString, expected: &[u8]) -> bool {
    usize::try_from(value.len()).is_ok_and(|length| length == expected.len())
        && value
            .code_units()
            .zip(expected.iter().copied())
            .all(|(actual, expected)| actual == u16::from(expected))
}

fn uint8_array_length(
    runtime: &Runtime,
    target: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<usize, NativeFailure> {
    let TypedArrayView::InBounds {
        length, element, ..
    } = runtime.typed_array_view(target)?
    else {
        return uint8_array_type_error(realm, origin, "Uint8Array is out of bounds");
    };
    if element != TypedArrayElementType::Uint8 {
        return Err(EngineFault::RuntimeInvariant {
            message: "validated Uint8Array changed its element type",
        }
        .into());
    }
    Ok(length)
}

fn get_uint8_array_bytes(
    runtime: &Runtime,
    target: ObjectId,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<Vec<u8>, NativeFailure> {
    let TypedArrayView::InBounds {
        buffer,
        byte_offset,
        length,
        element,
    } = runtime.typed_array_view(target)?
    else {
        return uint8_array_type_error(realm, origin, "Uint8Array is out of bounds");
    };
    if element != TypedArrayElementType::Uint8 {
        return Err(EngineFault::RuntimeInvariant {
            message: "validated Uint8Array changed its element type",
        }
        .into());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ArrayBufferBytes,
            additional: length,
        })?;
    let state = runtime
        .array_buffer_state(buffer)?
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Uint8Array backing buffer lost its internal slots",
        })?;
    state
        .with_data(|data| {
            let end = byte_offset
                .checked_add(length)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Uint8Array byte range overflowed",
                })?;
            let source = data
                .get(byte_offset..end)
                .ok_or(EngineFault::RuntimeInvariant {
                    message: "Uint8Array byte range escaped its backing store",
                })?;
            bytes.extend_from_slice(source);
            Ok::<(), EngineFault>(())
        })
        .ok_or(EngineFault::RuntimeInvariant {
            message: "Uint8Array backing buffer detached after validation",
        })??;
    Ok(bytes)
}

fn allocate_uint8_array(
    runtime: &mut Runtime,
    realm: RealmId,
    length: usize,
    origin: &JsStackFrame,
) -> Result<ObjectId, NativeFailure> {
    typed_array_create_same_type(runtime, realm, TypedArrayElementType::Uint8, length, origin)
}

fn store_uint8_array_bytes(
    runtime: &mut Runtime,
    target: ObjectId,
    bytes: &[u8],
) -> Result<(), NativeFailure> {
    for (index, byte) in bytes.iter().copied().enumerate() {
        let outcome = runtime.typed_array_store_index(
            target,
            index,
            TypedArrayElementValue::Number(JsNumber::from_u32(u32::from(byte))),
        )?;
        if outcome != TypedArrayStoreOutcome::Stored {
            return Err(EngineFault::RuntimeInvariant {
                message: "Uint8Array byte write lost its validated target",
            }
            .into());
        }
    }
    Ok(())
}

fn uint8_array_decode_result_object(
    runtime: &mut Runtime,
    realm: RealmId,
    read: usize,
    written: usize,
) -> Result<NativeDispatch, NativeFailure> {
    let read = u32::try_from(read).map_err(|_| EngineFault::RuntimeInvariant {
        message: "Uint8Array decoder read count exceeded String length range",
    })?;
    let written = u32::try_from(written).map_err(|_| EngineFault::RuntimeInvariant {
        message: "Uint8Array decoder write count exceeded String length range",
    })?;
    let result = runtime.allocate_ordinary_object(runtime.realm_object_prototype(realm)?)?;
    for (name, value) in [
        ("read", StoredValue::Number(JsNumber::from_u32(read))),
        ("written", StoredValue::Number(JsNumber::from_u32(written))),
    ] {
        let key = runtime.property_key_from_string(&JsString::from_utf8(name)?)?;
        runtime.append_data_property(
            HeapReference::Object(result),
            key,
            PropertyLayout::data(true, true, true),
            value,
        )?;
    }
    Ok(NativeDispatch::Immediate(StoredValue::Object(result)))
}

fn uint8_array_type_error<T>(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    typed_array_type_error(realm, origin, message)
}

fn uint8_array_syntax_error<T>(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<T, NativeFailure> {
    Err(NativeFailure::Abrupt(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::SyntaxError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    }))
}
