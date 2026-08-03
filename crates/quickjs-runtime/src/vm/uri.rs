/*
 * JavaScript URI handling semantics derived from QuickJS.
 *
 * Copyright (c) 2017-2018 Fabrice Bellard
 * Copyright (c) 2017-2018 Charlie Gordon
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! ECMA-262 URI encoding and decoding over exact UTF-16 code units.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

pub(super) fn finish_uri_function(
    function: UriFunction,
    text: &JsString,
    realm: RealmId,
    origin: &JsStackFrame,
    execution_budget: &mut ExecutionBudget,
) -> Result<NativeDispatch, NativeFailure> {
    execution_budget.charge_instructions(u64::from(text.len()).saturating_add(1))?;
    let result = if function.is_encode() {
        encode_uri_text(text, function.is_component(), realm, origin)?
    } else {
        decode_uri_text(text, function.is_component(), realm, origin)?
    };
    Ok(NativeDispatch::Immediate(StoredValue::String(result)))
}

fn encode_uri_text(
    text: &JsString,
    component: bool,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsString, NativeFailure> {
    let mut output = Vec::new();
    reserve_uri_output(&mut output, text.len() as usize)?;
    let mut units = text.code_units().peekable();
    while let Some(unit) = units.next() {
        if is_uri_unescaped(unit, component) {
            push_uri_unit(&mut output, unit)?;
            continue;
        }

        let code_point = if is_low_surrogate(unit) {
            return Err(NativeFailure::Abrupt(uri_error(
                realm,
                origin,
                "invalid character",
            )?));
        } else if is_high_surrogate(unit) {
            let Some(low) = units.peek().copied().filter(|unit| is_low_surrogate(*unit)) else {
                return Err(NativeFailure::Abrupt(uri_error(
                    realm,
                    origin,
                    "expecting surrogate pair",
                )?));
            };
            units.next();
            0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00)
        } else {
            u32::from(unit)
        };

        let (octets, count) = utf8_octets(code_point);
        reserve_uri_output(&mut output, count.saturating_mul(3))?;
        for octet in &octets[..count] {
            output.push(u16::from(b'%'));
            output.push(hex_upper(octet >> 4));
            output.push(hex_upper(octet & 0x0f));
        }
    }
    Ok(JsString::from_code_units(output)?)
}

fn decode_uri_text(
    text: &JsString,
    component: bool,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<JsString, NativeFailure> {
    let mut output = Vec::new();
    let mut index = 0_u32;
    while index < text.len() {
        let unit = text
            .code_unit_at(index)
            .expect("the URI index remains within the input");
        if unit != u16::from(b'%') {
            push_uri_unit(&mut output, unit)?;
            index = index.saturating_add(1);
            continue;
        }

        let first = parse_percent_octet(text, index, realm, origin)?;
        if first < 0x80 {
            if !component && is_uri_reserved(u16::from(first)) {
                reserve_uri_output(&mut output, 3)?;
                output.push(u16::from(b'%'));
                output.push(
                    text.code_unit_at(index + 1)
                        .expect("the percent octet was validated"),
                );
                output.push(
                    text.code_unit_at(index + 2)
                        .expect("the percent octet was validated"),
                );
            } else {
                push_uri_unit(&mut output, u16::from(first))?;
            }
            index = index.saturating_add(3);
            continue;
        }

        let count = first.leading_ones();
        if count == 1 || count > 4 {
            return Err(NativeFailure::Abrupt(uri_error(
                realm,
                origin,
                "malformed UTF-8",
            )?));
        }
        let (mut code_point, minimum) = match count {
            2 => (u32::from(first & 0x1f), 0x80),
            3 => (u32::from(first & 0x0f), 0x800),
            4 => (u32::from(first & 0x07), 0x1_0000),
            _ => {
                return Err(NativeFailure::Abrupt(uri_error(
                    realm,
                    origin,
                    "malformed UTF-8",
                )?));
            }
        };
        let mut next = index.saturating_add(3);
        for _ in 1..count {
            if text.code_unit_at(next) != Some(u16::from(b'%')) {
                return Err(NativeFailure::Abrupt(uri_error(
                    realm,
                    origin,
                    "expecting %",
                )?));
            }
            let octet = parse_percent_octet(text, next, realm, origin)?;
            if octet & 0xc0 != 0x80 {
                return Err(NativeFailure::Abrupt(uri_error(
                    realm,
                    origin,
                    "malformed UTF-8",
                )?));
            }
            code_point = (code_point << 6) | u32::from(octet & 0x3f);
            next = next.saturating_add(3);
        }
        if code_point < minimum || code_point > 0x10_ffff || (0xd800..=0xdfff).contains(&code_point)
        {
            return Err(NativeFailure::Abrupt(uri_error(
                realm,
                origin,
                "malformed UTF-8",
            )?));
        }
        push_utf16_code_point(&mut output, code_point)?;
        index = next;
    }
    Ok(JsString::from_code_units(output)?)
}

fn parse_percent_octet(
    text: &JsString,
    index: u32,
    realm: RealmId,
    origin: &JsStackFrame,
) -> Result<u8, NativeFailure> {
    if text.code_unit_at(index) != Some(u16::from(b'%')) {
        return Err(NativeFailure::Abrupt(uri_error(
            realm,
            origin,
            "expecting %",
        )?));
    }
    let Some(high) = text
        .code_unit_at(index.saturating_add(1))
        .and_then(hex_value)
    else {
        return Err(NativeFailure::Abrupt(uri_error(
            realm,
            origin,
            "expecting hex digit",
        )?));
    };
    let Some(low) = text
        .code_unit_at(index.saturating_add(2))
        .and_then(hex_value)
    else {
        return Err(NativeFailure::Abrupt(uri_error(
            realm,
            origin,
            "expecting hex digit",
        )?));
    };
    Ok((high << 4) | low)
}

fn push_utf16_code_point(output: &mut Vec<u16>, code_point: u32) -> Result<(), NativeFailure> {
    if code_point <= 0xffff {
        push_uri_unit(
            output,
            u16::try_from(code_point).expect("a BMP code point fits in one UTF-16 code unit"),
        )
    } else {
        let scalar = code_point - 0x1_0000;
        reserve_uri_output(output, 2)?;
        output.push(
            0xd800
                + u16::try_from(scalar >> 10)
                    .expect("a supplementary code point's high ten bits fit in UTF-16"),
        );
        output.push(
            0xdc00
                + u16::try_from(scalar & 0x03ff)
                    .expect("a supplementary code point's low ten bits fit in UTF-16"),
        );
        Ok(())
    }
}

fn utf8_octets(code_point: u32) -> ([u8; 4], usize) {
    let mut octets = [0_u8; 4];
    let count = if code_point < 0x80 {
        octets[0] = utf8_byte(code_point);
        1
    } else if code_point < 0x800 {
        octets[0] = 0xc0 | utf8_byte(code_point >> 6);
        octets[1] = 0x80 | utf8_byte(code_point & 0x3f);
        2
    } else if code_point < 0x1_0000 {
        octets[0] = 0xe0 | utf8_byte(code_point >> 12);
        octets[1] = 0x80 | utf8_byte((code_point >> 6) & 0x3f);
        octets[2] = 0x80 | utf8_byte(code_point & 0x3f);
        3
    } else {
        octets[0] = 0xf0 | utf8_byte(code_point >> 18);
        octets[1] = 0x80 | utf8_byte((code_point >> 12) & 0x3f);
        octets[2] = 0x80 | utf8_byte((code_point >> 6) & 0x3f);
        octets[3] = 0x80 | utf8_byte(code_point & 0x3f);
        4
    };
    (octets, count)
}

fn is_uri_unescaped(unit: u16, component: bool) -> bool {
    is_ascii_alphanumeric(unit)
        || matches!(
            unit,
            0x002d | 0x005f | 0x002e | 0x0021 | 0x007e | 0x002a | 0x0027 | 0x0028 | 0x0029
        )
        || (!component && is_uri_reserved(unit))
}

fn is_ascii_alphanumeric(unit: u16) -> bool {
    (u16::from(b'0')..=u16::from(b'9')).contains(&unit)
        || (u16::from(b'A')..=u16::from(b'Z')).contains(&unit)
        || (u16::from(b'a')..=u16::from(b'z')).contains(&unit)
}

fn is_uri_reserved(unit: u16) -> bool {
    matches!(
        unit,
        0x003b
            | 0x002f
            | 0x003f
            | 0x003a
            | 0x0040
            | 0x0026
            | 0x003d
            | 0x002b
            | 0x0024
            | 0x002c
            | 0x0023
    )
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xd800..=0xdbff).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xdc00..=0xdfff).contains(&unit)
}

fn hex_value(unit: u16) -> Option<u8> {
    match unit {
        unit if (u16::from(b'0')..=u16::from(b'9')).contains(&unit) => {
            Some(utf8_byte(u32::from(unit - u16::from(b'0'))))
        }
        unit if (u16::from(b'a')..=u16::from(b'f')).contains(&unit) => {
            Some(utf8_byte(u32::from(unit - u16::from(b'a') + 10)))
        }
        unit if (u16::from(b'A')..=u16::from(b'F')).contains(&unit) => {
            Some(utf8_byte(u32::from(unit - u16::from(b'A') + 10)))
        }
        _ => None,
    }
}

fn hex_upper(value: u8) -> u16 {
    u16::from(if value < 10 {
        b'0' + value
    } else {
        b'A' + value - 10
    })
}

fn utf8_byte(value: u32) -> u8 {
    u8::try_from(value).expect("a masked UTF-8 field fits in one byte")
}

fn push_uri_unit(output: &mut Vec<u16>, unit: u16) -> Result<(), NativeFailure> {
    reserve_uri_output(output, 1)?;
    output.push(unit);
    Ok(())
}

fn reserve_uri_output(output: &mut Vec<u16>, additional: usize) -> Result<(), NativeFailure> {
    let requested = output.len().saturating_add(additional);
    if requested > MAX_STRING_CODE_UNITS as usize {
        return Err(JsStringError::TooLong {
            requested: u64::try_from(requested).unwrap_or(u64::MAX),
            maximum: MAX_STRING_CODE_UNITS,
        }
        .into());
    }
    output.try_reserve(additional).map_err(|_| {
        NativeFailure::Execution(ExecutionError::AllocationFailed {
            resource: RuntimeResource::FrameValues,
            additional,
        })
    })
}

fn uri_error(
    realm: RealmId,
    origin: &JsStackFrame,
    message: &str,
) -> Result<PendingException, NativeFailure> {
    Ok(PendingException {
        realm,
        payload: PendingExceptionPayload::EngineError {
            kind: ExceptionKind::UriError,
            message: JsString::from_utf8(message)?,
        },
        origin: origin.clone(),
    })
}
