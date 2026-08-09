//! Non-observable byte/string codecs used by the `%Uint8Array%` methods.

#[allow(
    clippy::wildcard_imports,
    reason = "this private codec shares its parent VM module's implementation types"
)]
use super::*;

#[allow(
    clippy::too_many_lines,
    reason = "the loop intentionally mirrors the normative FromBase64 state machine in one auditable block"
)]
pub(super) fn decode_base64(
    input: &JsString,
    alphabet: Base64Alphabet,
    last_chunk: LastChunkHandling,
    max_length: usize,
) -> Result<DecodeResult, NativeFailure> {
    if max_length == 0 {
        return Ok(DecodeResult {
            read: 0,
            bytes: Vec::new(),
            error: false,
        });
    }
    let length = usize::try_from(input.len()).map_err(|_| EngineFault::RuntimeInvariant {
        message: "JavaScript String length did not fit usize",
    })?;
    let capacity = length
        .saturating_add(3)
        .checked_div(4)
        .unwrap_or(0)
        .saturating_mul(3)
        .min(max_length);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ArrayBufferBytes,
            additional: capacity,
        })?;
    let mut chunk = [0_u8; 4];
    let mut chunk_length = 0_usize;
    let mut read = 0_usize;
    let mut index = 0_usize;

    loop {
        index = skip_ascii_whitespace(input, index, length)?;
        if index == length {
            if chunk_length > 0 {
                match last_chunk {
                    LastChunkHandling::StopBeforePartial => {
                        return Ok(DecodeResult {
                            read,
                            bytes,
                            error: false,
                        });
                    }
                    LastChunkHandling::Strict => {
                        return Ok(DecodeResult {
                            read,
                            bytes,
                            error: true,
                        });
                    }
                    LastChunkHandling::Loose if chunk_length == 1 => {
                        return Ok(DecodeResult {
                            read,
                            bytes,
                            error: true,
                        });
                    }
                    LastChunkHandling::Loose => {
                        let appended =
                            append_final_base64_chunk(&mut bytes, chunk, chunk_length, false);
                        debug_assert!(appended, "loose decoding accepts unused trailing bits");
                    }
                }
            }
            return Ok(DecodeResult {
                read: length,
                bytes,
                error: false,
            });
        }

        let unit = code_unit_at(input, index)?;
        index = index.saturating_add(1);
        if unit == u16::from(b'=') {
            if chunk_length < 2 {
                return Ok(DecodeResult {
                    read,
                    bytes,
                    error: true,
                });
            }
            index = skip_ascii_whitespace(input, index, length)?;
            if chunk_length == 2 {
                if index == length {
                    return Ok(DecodeResult {
                        read,
                        bytes,
                        error: !matches!(last_chunk, LastChunkHandling::StopBeforePartial),
                    });
                }
                if code_unit_at(input, index)? == u16::from(b'=') {
                    index = skip_ascii_whitespace(input, index.saturating_add(1), length)?;
                }
            }
            if index < length {
                return Ok(DecodeResult {
                    read,
                    bytes,
                    error: true,
                });
            }
            if !append_final_base64_chunk(
                &mut bytes,
                chunk,
                chunk_length,
                matches!(last_chunk, LastChunkHandling::Strict),
            ) {
                return Ok(DecodeResult {
                    read,
                    bytes,
                    error: true,
                });
            }
            return Ok(DecodeResult {
                read: length,
                bytes,
                error: false,
            });
        }

        let Some(value) = base64_value(unit, alphabet) else {
            return Ok(DecodeResult {
                read,
                bytes,
                error: true,
            });
        };
        let remaining = max_length.saturating_sub(bytes.len());
        if (remaining == 1 && chunk_length == 2) || (remaining == 2 && chunk_length == 3) {
            return Ok(DecodeResult {
                read,
                bytes,
                error: false,
            });
        }
        chunk[chunk_length] = value;
        chunk_length += 1;
        if chunk_length == 4 {
            bytes.extend_from_slice(&decode_full_base64_chunk(chunk));
            chunk_length = 0;
            read = index;
            if bytes.len() == max_length {
                return Ok(DecodeResult {
                    read,
                    bytes,
                    error: false,
                });
            }
        }
    }
}

fn append_final_base64_chunk(
    bytes: &mut Vec<u8>,
    chunk: [u8; 4],
    chunk_length: usize,
    throw_on_extra_bits: bool,
) -> bool {
    debug_assert!(matches!(chunk_length, 2 | 3));
    let mut padded = chunk;
    padded[chunk_length..].fill(0);
    let decoded = decode_full_base64_chunk(padded);
    if chunk_length == 2 {
        if throw_on_extra_bits && decoded[1] != 0 {
            return false;
        }
        bytes.push(decoded[0]);
    } else {
        if throw_on_extra_bits && decoded[2] != 0 {
            return false;
        }
        bytes.extend_from_slice(&decoded[..2]);
    }
    true
}

fn decode_full_base64_chunk(chunk: [u8; 4]) -> [u8; 3] {
    [
        (chunk[0] << 2) | (chunk[1] >> 4),
        (chunk[1] << 4) | (chunk[2] >> 2),
        (chunk[2] << 6) | chunk[3],
    ]
}

fn base64_value(unit: u16, alphabet: Base64Alphabet) -> Option<u8> {
    match unit {
        unit if unit >= u16::from(b'A') && unit <= u16::from(b'Z') => {
            u8::try_from(unit - u16::from(b'A')).ok()
        }
        unit if unit >= u16::from(b'a') && unit <= u16::from(b'z') => {
            u8::try_from(unit - u16::from(b'a') + 26).ok()
        }
        unit if unit >= u16::from(b'0') && unit <= u16::from(b'9') => {
            u8::try_from(unit - u16::from(b'0') + 52).ok()
        }
        unit if unit == u16::from(b'+') && alphabet == Base64Alphabet::Standard => Some(62),
        unit if unit == u16::from(b'/') && alphabet == Base64Alphabet::Standard => Some(63),
        unit if unit == u16::from(b'-') && alphabet == Base64Alphabet::Url => Some(62),
        unit if unit == u16::from(b'_') && alphabet == Base64Alphabet::Url => Some(63),
        _ => None,
    }
}

fn skip_ascii_whitespace(
    input: &JsString,
    mut index: usize,
    length: usize,
) -> Result<usize, NativeFailure> {
    while index < length {
        let unit = code_unit_at(input, index)?;
        if !matches!(unit, 0x0009 | 0x000a | 0x000c | 0x000d | 0x0020) {
            break;
        }
        index += 1;
    }
    Ok(index)
}

fn code_unit_at(input: &JsString, index: usize) -> Result<u16, NativeFailure> {
    let index = u32::try_from(index).map_err(|_| EngineFault::RuntimeInvariant {
        message: "JavaScript String index exceeded u32",
    })?;
    input.code_unit_at(index).ok_or_else(|| {
        NativeFailure::Execution(
            EngineFault::RuntimeInvariant {
                message: "JavaScript String code-unit lookup escaped its length",
            }
            .into(),
        )
    })
}

pub(super) fn decode_hex(
    input: &JsString,
    max_length: usize,
) -> Result<DecodeResult, NativeFailure> {
    let length = usize::try_from(input.len()).map_err(|_| EngineFault::RuntimeInvariant {
        message: "JavaScript String length did not fit usize",
    })?;
    if length % 2 != 0 {
        return Ok(DecodeResult {
            read: 0,
            bytes: Vec::new(),
            error: true,
        });
    }
    let capacity = (length / 2).min(max_length);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ArrayBufferBytes,
            additional: capacity,
        })?;
    let mut read = 0_usize;
    while read < length && bytes.len() < max_length {
        let Some(high) = hex_value(code_unit_at(input, read)?) else {
            return Ok(DecodeResult {
                read,
                bytes,
                error: true,
            });
        };
        let Some(low) = hex_value(code_unit_at(input, read + 1)?) else {
            return Ok(DecodeResult {
                read,
                bytes,
                error: true,
            });
        };
        read += 2;
        bytes.push((high << 4) | low);
    }
    Ok(DecodeResult {
        read,
        bytes,
        error: false,
    })
}

fn hex_value(unit: u16) -> Option<u8> {
    match unit {
        unit if unit >= u16::from(b'0') && unit <= u16::from(b'9') => {
            u8::try_from(unit - u16::from(b'0')).ok()
        }
        unit if unit >= u16::from(b'a') && unit <= u16::from(b'f') => {
            u8::try_from(unit - u16::from(b'a') + 10).ok()
        }
        unit if unit >= u16::from(b'A') && unit <= u16::from(b'F') => {
            u8::try_from(unit - u16::from(b'A') + 10).ok()
        }
        _ => None,
    }
}

pub(super) fn hex_digit(value: u8) -> u8 {
    if value < 10 {
        b'0' + value
    } else {
        b'a' + value - 10
    }
}

pub(super) fn encode_base64(
    bytes: &[u8],
    alphabet: Base64Alphabet,
    omit_padding: bool,
) -> Result<JsString, NativeFailure> {
    let alphabet = match alphabet {
        Base64Alphabet::Standard => {
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        }
        Base64Alphabet::Url => b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
    };
    let padded_length = bytes
        .len()
        .saturating_add(2)
        .checked_div(3)
        .unwrap_or(0)
        .saturating_mul(4);
    let mut output = Vec::new();
    output
        .try_reserve_exact(padded_length)
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional: padded_length,
        })?;
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        output.push(alphabet[usize::from(chunk[0] >> 2)]);
        output.push(alphabet[usize::from(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4))]);
        output.push(alphabet[usize::from(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6))]);
        output.push(alphabet[usize::from(chunk[2] & 0x3f)]);
    }
    match chunks.remainder() {
        [] => {}
        [first] => {
            output.push(alphabet[usize::from(first >> 2)]);
            output.push(alphabet[usize::from((first & 0x03) << 4)]);
            if !omit_padding {
                output.extend_from_slice(b"==");
            }
        }
        [first, second] => {
            output.push(alphabet[usize::from(first >> 2)]);
            output.push(alphabet[usize::from(((first & 0x03) << 4) | (second >> 4))]);
            output.push(alphabet[usize::from((second & 0x0f) << 2)]);
            if !omit_padding {
                output.push(b'=');
            }
        }
        _ => unreachable!("chunks_exact remainder is shorter than three bytes"),
    }
    Ok(JsString::from_latin1(&output)?)
}
