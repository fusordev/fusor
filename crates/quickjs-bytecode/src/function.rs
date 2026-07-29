/*
 * QuickJS bytecode function metadata
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

//! Owned raw and validated function execution metadata.

use std::fmt;

const FUNCTION_KIND_SHIFT: u32 = 4;
const FUNCTION_KIND_MASK: u16 = 0b11 << FUNCTION_KIND_SHIFT;

pub(crate) const SERIALIZED_FUNCTION_FLAGS_MASK: u16 = 0x0fff;
pub(crate) const JS_MODE_MASK: u8 = 0x01;

/// The four function execution kinds encoded by `QuickJS`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum FunctionKind {
    /// An ordinary synchronous function.
    Normal = 0,
    /// A synchronous generator.
    Generator = 1,
    /// An asynchronous function.
    Async = 2,
    /// An asynchronous generator.
    AsyncGenerator = 3,
}

impl FunctionKind {
    pub(crate) const fn from_serialized_flags(flags: u16) -> Self {
        match flags & FUNCTION_KIND_MASK {
            0 => Self::Normal,
            value if value == 1_u16 << FUNCTION_KIND_SHIFT => Self::Generator,
            value if value == 2_u16 << FUNCTION_KIND_SHIFT => Self::Async,
            _ => Self::AsyncGenerator,
        }
    }
}

impl fmt::Display for FunctionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Normal => "normal",
            Self::Generator => "generator",
            Self::Async => "async",
            Self::AsyncGenerator => "async-generator",
        })
    }
}

/// A typed function-kind precondition for an opcode or packed flag.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionKindRequirement {
    /// Exactly an ordinary synchronous function.
    Normal,
    /// A synchronous or asynchronous generator.
    Generator,
    /// Exactly a synchronous generator.
    SynchronousGenerator,
    /// An asynchronous function or asynchronous generator.
    Async,
    /// Exactly an asynchronous generator.
    AsyncGenerator,
    /// Any function other than an ordinary synchronous function.
    NonNormal,
}

impl FunctionKindRequirement {
    /// Returns whether `kind` satisfies this requirement.
    #[must_use]
    pub const fn accepts(self, kind: FunctionKind) -> bool {
        match self {
            Self::Normal => matches!(kind, FunctionKind::Normal),
            Self::Generator => {
                matches!(kind, FunctionKind::Generator | FunctionKind::AsyncGenerator)
            }
            Self::SynchronousGenerator => matches!(kind, FunctionKind::Generator),
            Self::Async => matches!(kind, FunctionKind::Async | FunctionKind::AsyncGenerator),
            Self::AsyncGenerator => matches!(kind, FunctionKind::AsyncGenerator),
            Self::NonNormal => !matches!(kind, FunctionKind::Normal),
        }
    }
}

impl fmt::Display for FunctionKindRequirement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Normal => "a normal function",
            Self::Generator => "a generator function",
            Self::SynchronousGenerator => "a synchronous-generator function",
            Self::Async => "an async function",
            Self::AsyncGenerator => "an async-generator function",
            Self::NonNormal => "a non-normal function",
        })
    }
}

/// A packed function flag with a function-kind constraint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionHeaderFlag {
    /// The function receives an own prototype property.
    HasPrototype,
    /// The function is a derived class constructor.
    DerivedClassConstructor,
}

impl fmt::Display for FunctionHeaderFlag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::HasPrototype => "has-prototype",
            Self::DerivedClassConstructor => "derived-class-constructor",
        })
    }
}

/// A serialized function bit field whose allowed mask is validated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionBitField {
    /// The packed serialized function flags.
    SerializedFlags,
    /// The execution-mode byte.
    JsMode,
}

impl fmt::Display for FunctionBitField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SerializedFlags => "serialized function flags",
            Self::JsMode => "JS mode",
        })
    }
}

/// Raw serialized function metadata that has not crossed the verifier.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct UnverifiedFunctionHeader {
    serialized_flags: u16,
    js_mode: u8,
    defined_argument_count: u32,
    variable_reference_count: u32,
}

impl UnverifiedFunctionHeader {
    /// Creates an unverified function header.
    #[must_use]
    pub const fn new(
        serialized_flags: u16,
        js_mode: u8,
        defined_argument_count: u32,
        variable_reference_count: u32,
    ) -> Self {
        Self {
            serialized_flags,
            js_mode,
            defined_argument_count,
            variable_reference_count,
        }
    }

    /// Returns the raw packed function flags.
    #[must_use]
    pub const fn serialized_flags(self) -> u16 {
        self.serialized_flags
    }

    /// Returns the raw execution-mode byte.
    #[must_use]
    pub const fn js_mode(self) -> u8 {
        self.js_mode
    }

    /// Returns the raw count of source-defined arguments.
    #[must_use]
    pub const fn defined_argument_count(self) -> u32 {
        self.defined_argument_count
    }

    /// Returns the raw number of function-owned variable-reference cells.
    #[must_use]
    pub const fn variable_reference_count(self) -> u32 {
        self.variable_reference_count
    }
}

/// Validated packed function flags.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FunctionHeaderFlags(u16);

impl FunctionHeaderFlags {
    pub(crate) const fn from_validated_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Returns the validated packed bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Returns whether constructed functions receive a prototype property.
    #[must_use]
    pub const fn has_prototype(self) -> bool {
        self.has_bit(0)
    }

    /// Returns whether the parameter list has `QuickJS`'s simple form.
    #[must_use]
    pub const fn has_simple_parameter_list(self) -> bool {
        self.has_bit(1)
    }

    /// Returns whether this is a derived class constructor.
    #[must_use]
    pub const fn is_derived_class_constructor(self) -> bool {
        self.has_bit(2)
    }

    /// Returns whether the function needs a home object.
    #[must_use]
    pub const fn needs_home_object(self) -> bool {
        self.has_bit(3)
    }

    /// Returns whether `new.target` is permitted.
    #[must_use]
    pub const fn new_target_allowed(self) -> bool {
        self.has_bit(6)
    }

    /// Returns whether a `super()` call is permitted.
    #[must_use]
    pub const fn super_call_allowed(self) -> bool {
        self.has_bit(7)
    }

    /// Returns whether `super` property access is permitted.
    #[must_use]
    pub const fn super_allowed(self) -> bool {
        self.has_bit(8)
    }

    /// Returns whether the `arguments` binding is permitted.
    #[must_use]
    pub const fn arguments_allowed(self) -> bool {
        self.has_bit(9)
    }

    /// Returns whether serialized debug metadata is declared.
    #[must_use]
    pub const fn has_debug(self) -> bool {
        self.has_bit(10)
    }

    /// Returns whether the function was compiled for eval.
    #[must_use]
    pub const fn is_eval(self) -> bool {
        self.has_bit(11)
    }

    const fn has_bit(self, bit: u32) -> bool {
        self.0 & (1_u16 << bit) != 0
    }
}

/// Validated stored `QuickJS` function-mode bits.
///
/// Runtime-only async-frame and backtrace-barrier bits are deliberately absent
/// and belong in a future frame-mode type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FunctionMode(u8);

impl FunctionMode {
    pub(crate) const fn from_validated_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the validated mode bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether strict-mode execution is selected.
    #[must_use]
    pub const fn is_strict(self) -> bool {
        self.has_bit(0)
    }

    const fn has_bit(self, bit: u32) -> bool {
        self.0 & (1_u8 << bit) != 0
    }
}

/// Function execution metadata that has crossed the staged body verifier.
///
/// Debug payloads, variable-definition records, closure descriptors, and
/// child functions still require the future whole-function verifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VerifiedFunctionHeader {
    flags: FunctionHeaderFlags,
    mode: FunctionMode,
    kind: FunctionKind,
    defined_argument_count: u32,
    variable_reference_count: u32,
}

impl VerifiedFunctionHeader {
    pub(crate) const fn new(
        flags: FunctionHeaderFlags,
        mode: FunctionMode,
        kind: FunctionKind,
        defined_argument_count: u32,
        variable_reference_count: u32,
    ) -> Self {
        Self {
            flags,
            mode,
            kind,
            defined_argument_count,
            variable_reference_count,
        }
    }

    /// Returns the validated packed flags.
    #[must_use]
    pub const fn flags(self) -> FunctionHeaderFlags {
        self.flags
    }

    /// Returns the validated execution mode.
    #[must_use]
    pub const fn mode(self) -> FunctionMode {
        self.mode
    }

    /// Returns the decoded function kind.
    #[must_use]
    pub const fn kind(self) -> FunctionKind {
        self.kind
    }

    /// Returns the count of source-defined arguments.
    #[must_use]
    pub const fn defined_argument_count(self) -> u32 {
        self.defined_argument_count
    }

    /// Returns the number of function-owned variable-reference cells.
    #[must_use]
    pub const fn variable_reference_count(self) -> u32 {
        self.variable_reference_count
    }
}
