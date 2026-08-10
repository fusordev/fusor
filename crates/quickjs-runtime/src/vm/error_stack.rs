/*
 * JavaScript Error stack snapshots derived from QuickJS.
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
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */

//! Side-effect-free, headerless Error stack snapshots.

#[allow(
    clippy::wildcard_imports,
    reason = "this private VM sibling participates in the shared interpreter implementation namespace"
)]
use super::*;

/// Immutable call locations retained while an Error constructor runs.
///
/// Function names are deliberately not retained here. `QuickJS` reads each
/// frame's ordinary string-valued `name` only when it builds the final stack,
/// after message/cause conversion and `AggregateError` iteration have returned.
/// Keeping the function identity makes those mutations observable without
/// invoking a getter or any other JavaScript code.
pub(super) struct ErrorStackSnapshot {
    sites: Vec<ErrorStackSite>,
}

impl ErrorStackSnapshot {
    pub(super) fn retained_values(&self) -> u64 {
        usize_to_u64(self.sites.len())
    }

    /// Marks the function identities whose late-read names remain observable.
    pub(super) fn trace_roots(&self, mark: &mut dyn FnMut(CollectionRoot)) {
        for site in &self.sites {
            if let ErrorStackSite::Bytecode { function, .. } = site {
                mark(CollectionRoot::Heap(HeapReference::Function(*function)));
            }
        }
    }
}

enum ErrorStackSite {
    Bytecode {
        function: FunctionId,
        location: JsStackFrame,
        line: u64,
        column: u64,
    },
    /// The pinned `call (native)` / `apply (native)` entry between a target
    /// bytecode function and its caller.
    Native(SyntheticNativeFrame),
}

/// Captures the active JavaScript locations for a newly constructed Error.
///
/// `frames` contains bytecode frames only: the native Error constructor does
/// not have a [`Frame`] in this VM, so iterating from the newest frame outward
/// already implements `QuickJS`'s `SKIP_FIRST_LEVEL` constructor-frame rule.
/// `origin` is the exact call expression that entered the constructor and is
/// used for the newest frame; older frames are parked at their own call sites.
///
/// No JavaScript code runs. Source positions are computed iteratively and, for
/// frames sharing a retained source artifact, scan that artifact at most once
/// up to the largest referenced offset.
pub(super) fn capture_error_stack(
    runtime: &Runtime,
    frames: &[Frame],
    origin: &JsStackFrame,
) -> Result<ErrorStackSnapshot, ExecutionError> {
    let mut sites = Vec::new();
    sites
        .try_reserve_exact(frames.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ExceptionFrames,
            additional: frames.len(),
        })?;

    for (depth, frame) in frames.iter().rev().enumerate() {
        let location = if depth == 0 {
            if origin.function() != frame.template {
                return Err(EngineFault::RuntimeInvariant {
                    message: "Error stack origin does not name the active function template",
                }
                .into());
            }
            origin.clone()
        } else {
            active_frame_location(runtime, frame)?
        };
        sites.push(ErrorStackSite::Bytecode {
            function: frame.function,
            location,
            line: 0,
            column: 0,
        });
        // QuickJS inserts the pinned `call (native)` / `apply (native)`
        // entry between the target function and its caller whenever a
        // bytecode function was reached through `Function.prototype.call`
        // or `Function.prototype.apply`.
        if let Some(caller) = frame.native_caller {
            sites.push(ErrorStackSite::Native(caller));
        }
    }

    populate_source_positions(&mut sites)?;
    Ok(ErrorStackSnapshot { sites })
}

/// Renders one previously captured Error stack without executing JavaScript.
///
/// The result has no `Error: message` header. Every retained bytecode frame is
/// rendered exactly as `    at name (file:line:column)\n`; a missing, empty, or
/// non-data-string function name becomes `<anonymous>`. As in `QuickJS`'s
/// backtrace-only property reader, lookup examines the function and at most
/// one prototype level and ignores accessors.
pub(super) fn render_error_stack(
    runtime: &Runtime,
    snapshot: &ErrorStackSnapshot,
) -> Result<JsString, ExecutionError> {
    let mut rendered = JsString::empty();
    for site in &snapshot.sites {
        match site {
            ErrorStackSite::Bytecode {
                function,
                location,
                line,
                column,
            } => {
                let name = stack_function_name(runtime, *function)?;
                let line = render_stack_line(&name, location.source_name(), *line, *column)?;
                rendered = rendered.concat(&line)?;
            }
            ErrorStackSite::Native(kind) => {
                rendered = rendered.concat(&JsString::from_utf8("    at ")?)?;
                rendered = rendered.concat(&JsString::from_utf8(kind.label())?)?;
                rendered = rendered.concat(&JsString::from_utf8(" (native)\n")?)?;
            }
        }
    }
    Ok(rendered)
}

pub(super) fn active_frame_location(
    runtime: &Runtime,
    frame: &Frame,
) -> Result<JsStackFrame, ExecutionError> {
    let instruction = code(runtime, frame.code)?
        .authority
        .function(frame.template)
        .and_then(|function| {
            function
                .function()
                .control_flow()
                .instruction(frame.instruction)
        })
        .ok_or(EngineFault::MissingInstruction {
            function: frame.template,
            instruction: frame.instruction.get(),
        })?;
    Ok(instruction_location(
        runtime,
        frame,
        instruction.decoded().pc(),
    )?)
}

fn populate_source_positions(sites: &mut [ErrorStackSite]) -> Result<(), ExecutionError> {
    let mut order = Vec::new();
    order
        .try_reserve_exact(sites.len())
        .map_err(|_| ExecutionError::AllocationFailed {
            resource: RuntimeResource::ExceptionFrames,
            additional: sites.len(),
        })?;
    order.extend(0..sites.len());
    order.sort_unstable_by_key(|index| source_position_key(&sites[*index]));

    let mut current_source = None;
    let mut scanned = 0_usize;
    let mut line = 1_u64;
    let mut column = 1_u64;
    for index in order {
        let ErrorStackSite::Bytecode { location, .. } = &sites[index] else {
            continue;
        };
        let key = stack_source_identity(location);
        if current_source != Some(key) {
            current_source = Some(key);
            scanned = 0;
            line = 1;
            column = 1;
        }
        let offset = source_offset(location)?;
        advance_source_position(
            location.source_text(),
            &mut scanned,
            offset,
            &mut line,
            &mut column,
        )?;
        if let ErrorStackSite::Bytecode {
            line: site_line,
            column: site_column,
            ..
        } = &mut sites[index]
        {
            *site_line = line;
            *site_column = column;
        }
    }
    Ok(())
}

fn advance_source_position(
    source: &str,
    scanned: &mut usize,
    target: usize,
    line: &mut u64,
    column: &mut u64,
) -> Result<(), EngineFault> {
    if target > source.len() || !source.is_char_boundary(target) || *scanned > target {
        return Err(EngineFault::RuntimeInvariant {
            message: "verified Error stack source span is outside its retained source",
        });
    }
    while *scanned < target {
        let byte = source.as_bytes()[*scanned];
        if byte == b'\n' {
            *line = line.saturating_add(1);
            *column = 1;
        } else if !(0x80..0xc0).contains(&byte) {
            // QuickJS columns count Unicode scalar values, not UTF-8
            // continuation bytes or UTF-16 code units.
            *column = column.saturating_add(1);
        }
        *scanned += 1;
    }
    Ok(())
}

fn source_position_key(site: &ErrorStackSite) -> (*const u8, usize, u32) {
    let ErrorStackSite::Bytecode { location, .. } = site else {
        return (std::ptr::null(), 0, 0);
    };
    let source = location.source_text();
    (
        source.as_ptr(),
        source.len(),
        location.source_span().start(),
    )
}

fn stack_source_identity(location: &JsStackFrame) -> (*const u8, usize) {
    let source = location.source_text();
    (source.as_ptr(), source.len())
}

fn source_offset(location: &JsStackFrame) -> Result<usize, EngineFault> {
    usize::try_from(location.source_span().start()).map_err(|_| EngineFault::RuntimeInvariant {
        message: "verified Error stack source offset exceeds the host index domain",
    })
}

fn stack_function_name(
    runtime: &Runtime,
    function: FunctionId,
) -> Result<JsString, ExecutionError> {
    let name_key = runtime.predefined_property_key(PredefinedAtom::Name);
    let record = runtime.object_record(HeapReference::Function(function))?;
    match record.own_property(&name_key) {
        Some(OwnProperty::Data {
            value: StoredValue::String(name),
            ..
        }) => Ok(c_string_prefix(&name)?),
        Some(OwnProperty::Data { .. } | OwnProperty::Accessor { .. }) => Ok(JsString::empty()),
        None => {
            let Some(prototype) = record.prototype() else {
                return Ok(JsString::empty());
            };
            match runtime.object_record(prototype)?.own_property(&name_key) {
                Some(OwnProperty::Data {
                    value: StoredValue::String(name),
                    ..
                }) => Ok(c_string_prefix(&name)?),
                Some(OwnProperty::Data { .. } | OwnProperty::Accessor { .. }) | None => {
                    Ok(JsString::empty())
                }
            }
        }
    }
}

fn c_string_prefix(value: &JsString) -> Result<JsString, JsStringError> {
    let end = value
        .code_units()
        .position(|unit| unit == 0)
        .map_or(value.len(), |index| {
            u32::try_from(index).unwrap_or(value.len())
        });
    if end == value.len() {
        Ok(value.clone())
    } else {
        value.slice(0..end)
    }
}

fn render_stack_line(
    function_name: &JsString,
    source_name: &str,
    line: u64,
    column: u64,
) -> Result<JsString, JsStringError> {
    let mut rendered = JsString::from_utf8("    at ")?;
    let anonymous;
    let function_name = if function_name.is_empty() {
        anonymous = JsString::from_utf8("<anonymous>")?;
        &anonymous
    } else {
        function_name
    };
    rendered = rendered.concat(function_name)?;
    rendered = rendered.concat(&JsString::from_utf8(" (")?)?;
    let source_name = source_name.split('\0').next().unwrap_or_default();
    rendered = rendered.concat(&JsString::from_utf8(source_name)?)?;
    rendered = rendered.concat(&JsString::from_utf8(":")?)?;
    rendered = rendered.concat(&decimal_js_string(line)?)?;
    rendered = rendered.concat(&JsString::from_utf8(":")?)?;
    rendered = rendered.concat(&decimal_js_string(column)?)?;
    rendered.concat(&JsString::from_utf8(")\n")?)
}

fn decimal_js_string(mut value: u64) -> Result<JsString, JsStringError> {
    let mut bytes = [0_u8; 20];
    let mut cursor = bytes.len();
    loop {
        cursor -= 1;
        let digit = usize::try_from(value % 10).unwrap_or_default();
        bytes[cursor] = b"0123456789"[digit];
        value /= 10;
        if value == 0 {
            break;
        }
    }
    JsString::from_latin1(&bytes[cursor..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_positions_match_quickjs_scalar_columns_and_lf_lines() {
        let source = "aé\r\n😀x";
        assert_eq!(test_source_position(source, 0), (1, 1));
        assert_eq!(test_source_position(source, 1), (1, 2));
        assert_eq!(test_source_position(source, 3), (1, 3));
        assert_eq!(test_source_position(source, 4), (1, 4));
        assert_eq!(test_source_position(source, 5), (2, 1));
        assert_eq!(test_source_position(source, 9), (2, 2));
        assert_eq!(test_source_position(source, 10), (2, 3));
    }

    #[test]
    fn stack_line_is_headerless_and_uses_anonymous_for_empty_name() {
        let line = render_stack_line(&JsString::empty(), "unit.js", 12, 34)
            .expect("render stack line")
            .to_utf8_lossy()
            .expect("UTF-8 stack line");
        assert_eq!(line, "    at <anonymous> (unit.js:12:34)\n");
    }

    #[test]
    fn stack_line_honors_quickjs_c_string_termination() {
        let name = c_string_prefix(
            &JsString::from_code_units("before\0after".encode_utf16()).expect("function name"),
        )
        .expect("C string prefix");
        let line = render_stack_line(&name, "file.js\0ignored", u64::MAX, 0)
            .expect("render stack line")
            .to_utf8_lossy()
            .expect("UTF-8 stack line");
        assert_eq!(line, format!("    at before (file.js:{}:0)\n", u64::MAX));
    }

    fn test_source_position(source: &str, offset: usize) -> (u64, u64) {
        let mut scanned = 0;
        let mut line = 1;
        let mut column = 1;
        advance_source_position(source, &mut scanned, offset, &mut line, &mut column)
            .expect("valid source position");
        (line, column)
    }
}
