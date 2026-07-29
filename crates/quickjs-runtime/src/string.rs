/*
 * JavaScript string representation derived from QuickJS.
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

use std::{
    cmp::Ordering,
    error::Error,
    fmt,
    hash::{Hash, Hasher},
    ops::Range,
    sync::{Arc, OnceLock},
};

/// Maximum ECMAScript string length supported by the pinned `QuickJS` release.
///
/// Length is measured in UTF-16 code units.
pub const MAX_STRING_CODE_UNITS: u32 = (1 << 30) - 1;

const ROPE_SHORT_LEN: u32 = 512;
const ROPE_SHORT_LEFT_LEN: u32 = 8_192;
const ROPE_MAX_DEPTH: u8 = 60;
const ROPE_ITER_STACK_CAPACITY: usize = ROPE_MAX_DEPTH as usize + 1;
const ROPE_BUCKET_LENGTHS: [u32; 44] = [
    1,
    2,
    3,
    5,
    8,
    13,
    21,
    34,
    55,
    89,
    144,
    233,
    377,
    610,
    987,
    1_597,
    2_584,
    4_181,
    6_765,
    10_946,
    17_711,
    28_657,
    46_368,
    75_025,
    121_393,
    196_418,
    317_811,
    514_229,
    832_040,
    1_346_269,
    2_178_309,
    3_524_578,
    5_702_887,
    9_227_465,
    14_930_352,
    24_157_817,
    39_088_169,
    63_245_986,
    102_334_155,
    165_580_141,
    267_914_296,
    433_494_437,
    701_408_733,
    1_134_903_170,
];
const MAX_INITIAL_RESERVE: usize = 4_096;

static EMPTY: OnceLock<Arc<Repr>> = OnceLock::new();

/// An immutable ECMAScript string.
///
/// ECMAScript strings are sequences of UTF-16 code units, not Rust Unicode
/// scalar strings. This type therefore preserves lone surrogates. Latin-1
/// leaves and bounded ropes are representation optimizations and do not change
/// equality, ordering, hashing, or indexing.
#[derive(Clone)]
pub struct JsString(Arc<Repr>);

enum Repr {
    Latin1(Box<[u8]>),
    Utf16(Box<[u16]>),
    Rope {
        left: JsString,
        right: JsString,
        len: u32,
        depth: u8,
        wide: bool,
    },
}

impl Repr {
    fn len(&self) -> u32 {
        match self {
            Self::Latin1(units) => {
                u32::try_from(units.len()).expect("validated JavaScript string length")
            }
            Self::Utf16(units) => {
                u32::try_from(units.len()).expect("validated JavaScript string length")
            }
            Self::Rope { len, .. } => *len,
        }
    }

    const fn depth(&self) -> u8 {
        match self {
            Self::Latin1(_) | Self::Utf16(_) => 0,
            Self::Rope { depth, .. } => *depth,
        }
    }

    const fn is_wide(&self) -> bool {
        match self {
            Self::Latin1(_) => false,
            Self::Utf16(_) => true,
            Self::Rope { wide, .. } => *wide,
        }
    }

    const fn is_leaf(&self) -> bool {
        matches!(self, Self::Latin1(_) | Self::Utf16(_))
    }
}

impl JsString {
    /// Returns the shared empty string.
    #[must_use]
    pub fn empty() -> Self {
        Self(
            EMPTY
                .get_or_init(|| Arc::new(Repr::Latin1(Box::new([]))))
                .clone(),
        )
    }

    /// Copies Latin-1 code units into an immutable JavaScript string.
    ///
    /// # Errors
    ///
    /// Returns an error if the length exceeds the `QuickJS` compatibility limit
    /// or backing-buffer capacity cannot be reserved.
    pub fn from_latin1(units: &[u8]) -> Result<Self, JsStringError> {
        check_length(units.len())?;
        if units.is_empty() {
            return Ok(Self::empty());
        }
        let mut owned = Vec::new();
        reserve(&mut owned, units.len())?;
        owned.extend_from_slice(units);
        Ok(Self(Arc::new(Repr::Latin1(owned.into_boxed_slice()))))
    }

    /// Copies UTF-16 code units, preserving lone surrogates.
    ///
    /// A string whose units all fit in Latin-1 is stored in the narrower leaf
    /// representation.
    ///
    /// # Errors
    ///
    /// Returns an error if the length exceeds the `QuickJS` compatibility limit
    /// or backing-buffer capacity cannot be reserved.
    pub fn from_code_units(units: impl IntoIterator<Item = u16>) -> Result<Self, JsStringError> {
        let iterator = units.into_iter();
        let lower_bound = iterator.size_hint().0;
        if lower_bound > MAX_STRING_CODE_UNITS as usize {
            return Err(JsStringError::TooLong {
                requested: lower_bound as u64,
                maximum: MAX_STRING_CODE_UNITS,
            });
        }

        let initial = lower_bound.min(MAX_INITIAL_RESERVE);
        let mut latin1 = Vec::new();
        reserve(&mut latin1, initial)?;
        let mut utf16 = None::<Vec<u16>>;

        for unit in iterator {
            let current_len = utf16.as_ref().map_or(latin1.len(), Vec::len);
            if current_len == MAX_STRING_CODE_UNITS as usize {
                return Err(JsStringError::TooLong {
                    requested: u64::from(MAX_STRING_CODE_UNITS) + 1,
                    maximum: MAX_STRING_CODE_UNITS,
                });
            }

            if let Some(wide) = utf16.as_mut() {
                try_push(wide, unit)?;
            } else if let Ok(narrow) = u8::try_from(unit) {
                try_push(&mut latin1, narrow)?;
            } else {
                let mut wide = Vec::new();
                reserve(&mut wide, latin1.len().saturating_add(1))?;
                wide.extend(latin1.drain(..).map(u16::from));
                wide.push(unit);
                utf16 = Some(wide);
            }
        }

        if let Some(wide) = utf16 {
            Ok(Self(Arc::new(Repr::Utf16(wide.into_boxed_slice()))))
        } else if latin1.is_empty() {
            Ok(Self::empty())
        } else {
            Ok(Self(Arc::new(Repr::Latin1(latin1.into_boxed_slice()))))
        }
    }

    /// Encodes a valid Rust UTF-8 string as ECMAScript UTF-16 code units.
    ///
    /// # Errors
    ///
    /// Returns an error if the resulting UTF-16 length exceeds the
    /// compatibility limit or backing-buffer capacity cannot be reserved.
    pub fn from_utf8(text: &str) -> Result<Self, JsStringError> {
        Self::from_code_units(text.encode_utf16())
    }

    /// Returns the number of UTF-16 code units.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.0.len()
    }

    /// Returns whether the string has no code units.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns whether every code unit fits in Latin-1.
    #[must_use]
    pub fn is_latin1(&self) -> bool {
        !self.0.is_wide()
    }

    /// Returns one UTF-16 code unit.
    #[must_use]
    pub fn code_unit_at(&self, index: u32) -> Option<u16> {
        if index >= self.len() {
            return None;
        }

        let mut node = self.0.as_ref();
        let mut index = index;
        loop {
            match node {
                Repr::Latin1(units) => return Some(u16::from(units[index as usize])),
                Repr::Utf16(units) => return Some(units[index as usize]),
                Repr::Rope { left, right, .. } => {
                    if index < left.len() {
                        node = left.0.as_ref();
                    } else {
                        index -= left.len();
                        node = right.0.as_ref();
                    }
                }
            }
        }
    }

    /// Iterates over UTF-16 code units without flattening the string.
    #[must_use]
    pub fn code_units(&self) -> CodeUnits<'_> {
        CodeUnits::new(self)
    }

    /// Concatenates two strings.
    ///
    /// Short leaves are copied directly. Longer results use a depth-bounded
    /// rope; exceeding the pinned `QuickJS` depth threshold applies its
    /// Fibonacci-bucket rebalancing algorithm.
    ///
    /// # Errors
    ///
    /// Returns an error if the combined length exceeds the compatibility limit
    /// or backing-buffer capacity cannot be reserved.
    pub fn concat(&self, other: &Self) -> Result<Self, JsStringError> {
        checked_sum(self.len(), other.len())?;
        if self.is_empty() {
            return Ok(other.clone());
        }
        if other.is_empty() {
            return Ok(self.clone());
        }

        if other.0.is_leaf() && other.len() <= ROPE_SHORT_LEN {
            if self.0.is_leaf() && self.len() <= ROPE_SHORT_LEFT_LEN {
                return Self::copy_concat(self, other);
            }
            if let Repr::Rope { left, right, .. } = self.0.as_ref()
                && right.0.is_leaf()
                && right.len() <= ROPE_SHORT_LEN
            {
                let merged_right = Self::copy_concat(right, other)?;
                return Self::new_rope(left.clone(), merged_right);
            }
        } else if self.0.is_leaf()
            && let Repr::Rope { left, right, .. } = other.0.as_ref()
            && left.0.is_leaf()
            && left.len() <= ROPE_SHORT_LEN
        {
            let merged_left = Self::copy_concat(self, left)?;
            return Self::new_rope(merged_left, right.clone());
        }

        Self::new_rope(self.clone(), other.clone())
    }

    /// Copies a half-open UTF-16 code-unit range into a compact leaf.
    ///
    /// # Errors
    ///
    /// Returns an error for a reversed or out-of-bounds range or if allocation
    /// cannot be reserved.
    pub fn slice(&self, range: Range<u32>) -> Result<Self, JsStringError> {
        if range.start > range.end || range.end > self.len() {
            return Err(JsStringError::InvalidRange {
                start: range.start,
                end: range.end,
                len: self.len(),
            });
        }
        if range.start == 0 && range.end == self.len() {
            return Ok(self.clone());
        }
        Self::from_code_units(
            self.code_units()
                .skip(range.start as usize)
                .take((range.end - range.start) as usize),
        )
    }

    /// Converts to valid UTF-8, replacing lone surrogates with U+FFFD.
    ///
    /// This is a display/host-boundary conversion. It does not mutate or lose
    /// the original UTF-16 code units.
    ///
    /// # Errors
    ///
    /// Returns an error if output backing-buffer capacity cannot be reserved.
    pub fn to_utf8_lossy(&self) -> Result<String, JsStringError> {
        let capacity =
            (self.len() as usize)
                .checked_mul(3)
                .ok_or(JsStringError::AllocationFailed {
                    additional: usize::MAX,
                })?;
        let mut output = String::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| JsStringError::AllocationFailed {
                additional: capacity,
            })?;
        for character in char::decode_utf16(self.code_units()) {
            output.push(character.unwrap_or(char::REPLACEMENT_CHARACTER));
        }
        Ok(output)
    }

    /// Encodes the string as the byte sequence returned by `QuickJS`'s default
    /// C-string boundary.
    ///
    /// Valid surrogate pairs become one four-byte UTF-8 sequence. Lone
    /// surrogates are deliberately preserved as three-byte WTF-8 sequences, so
    /// the result is not necessarily valid UTF-8. Embedded zero code units are
    /// retained as zero bytes; no trailing C terminator is appended.
    ///
    /// # Errors
    ///
    /// Returns an error if output backing-buffer capacity cannot be reserved.
    pub fn to_wtf8_bytes(&self) -> Result<Vec<u8>, JsStringError> {
        self.to_quickjs_utf8_bytes(false)
    }

    /// Encodes each UTF-16 code unit independently as CESU-8.
    ///
    /// In particular, a valid surrogate pair becomes two three-byte sequences
    /// instead of one four-byte UTF-8 sequence. Lone surrogates are preserved.
    /// Embedded zero code units are retained as zero bytes; no trailing C
    /// terminator is appended.
    ///
    /// # Errors
    ///
    /// Returns an error if output backing-buffer capacity cannot be reserved.
    pub fn to_cesu8_bytes(&self) -> Result<Vec<u8>, JsStringError> {
        self.to_quickjs_utf8_bytes(true)
    }

    fn to_quickjs_utf8_bytes(&self, cesu8: bool) -> Result<Vec<u8>, JsStringError> {
        let capacity =
            (self.len() as usize)
                .checked_mul(3)
                .ok_or(JsStringError::AllocationFailed {
                    additional: usize::MAX,
                })?;
        let mut output = Vec::new();
        reserve(&mut output, capacity)?;
        let mut units = self.code_units().peekable();
        while let Some(unit) = units.next() {
            let code_point = if !cesu8 && is_high_surrogate(unit) {
                if let Some(&low) = units.peek()
                    && is_low_surrogate(low)
                {
                    let _ = units.next();
                    0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00)
                } else {
                    u32::from(unit)
                }
            } else {
                u32::from(unit)
            };
            encode_utf8_code_point(&mut output, code_point);
        }
        Ok(output)
    }

    fn copy_concat(left: &Self, right: &Self) -> Result<Self, JsStringError> {
        Self::from_code_units(left.code_units().chain(right.code_units()))
    }

    fn new_rope(left: Self, right: Self) -> Result<Self, JsStringError> {
        let len = checked_sum(left.len(), right.len())?;
        let depth = left.0.depth().max(right.0.depth()).saturating_add(1);
        if depth > ROPE_MAX_DEPTH {
            return Self::rebalance(left, right);
        }
        let wide = left.0.is_wide() || right.0.is_wide();
        Ok(Self(Arc::new(Repr::Rope {
            left,
            right,
            len,
            depth,
            wide,
        })))
    }

    fn rebalance(left: Self, right: Self) -> Result<Self, JsStringError> {
        let mut buckets: [Option<Self>; ROPE_BUCKET_LENGTHS.len()] = std::array::from_fn(|_| None);
        let mut stack = Vec::new();
        reserve(&mut stack, usize::from(ROPE_MAX_DEPTH) + 2)?;
        stack.push(right);
        stack.push(left);

        while let Some(node) = stack.pop() {
            let children = match node.0.as_ref() {
                Repr::Rope { left, right, .. } => Some((left.clone(), right.clone())),
                Repr::Latin1(_) | Repr::Utf16(_) => None,
            };
            if let Some((left, right)) = children {
                try_push(&mut stack, right)?;
                try_push(&mut stack, left)?;
            } else if !node.is_empty() {
                Self::rebalance_leaf(node, &mut buckets)?;
            }
        }

        let mut result = None;
        for bucket in buckets.into_iter().flatten() {
            result = Some(match result {
                None => bucket,
                Some(right) => Self::new_rope_node(bucket, right)?,
            });
        }
        Ok(result.unwrap_or_else(Self::empty))
    }

    fn rebalance_leaf(
        leaf: Self,
        buckets: &mut [Option<Self>; ROPE_BUCKET_LENGTHS.len()],
    ) -> Result<(), JsStringError> {
        let len = leaf.len();
        let mut aggregate = None;
        let mut index = 0;

        while len >= ROPE_BUCKET_LENGTHS[index + 1] {
            if let Some(bucket) = buckets[index].take() {
                aggregate = Some(match aggregate {
                    None => bucket,
                    Some(right) => Self::new_rope_node(bucket, right)?,
                });
            }
            index += 1;
        }

        let mut aggregate = match aggregate {
            None => leaf,
            Some(left) => Self::new_rope_node(left, leaf)?,
        };
        while let Some(bucket) = buckets.get_mut(index) {
            if let Some(left) = bucket.take() {
                aggregate = Self::new_rope_node(left, aggregate)?;
                index += 1;
            } else {
                *bucket = Some(aggregate);
                return Ok(());
            }
        }

        Err(JsStringError::TooLong {
            requested: u64::from(MAX_STRING_CODE_UNITS) + 1,
            maximum: MAX_STRING_CODE_UNITS,
        })
    }

    fn new_rope_node(left: Self, right: Self) -> Result<Self, JsStringError> {
        let len = checked_sum(left.len(), right.len())?;
        let depth = left.0.depth().max(right.0.depth()).saturating_add(1);
        let wide = left.0.is_wide() || right.0.is_wide();
        Ok(Self(Arc::new(Repr::Rope {
            left,
            right,
            len,
            depth,
            wide,
        })))
    }

    #[cfg(test)]
    fn depth(&self) -> u8 {
        self.0.depth()
    }
}

impl Default for JsString {
    fn default() -> Self {
        Self::empty()
    }
}

impl TryFrom<&str> for JsString {
    type Error = JsStringError;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        Self::from_utf8(text)
    }
}

impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.len() == other.len() && self.code_units().eq(other.code_units()))
    }
}

impl Eq for JsString {}

impl PartialOrd for JsString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsString {
    fn cmp(&self, other: &Self) -> Ordering {
        self.code_units().cmp(other.code_units())
    }
}

impl Hash for JsString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.len().hash(state);
        for unit in self.code_units() {
            unit.hash(state);
        }
    }
}

impl fmt::Debug for JsString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsString")
            .field("code_units", &self.len())
            .field("latin1", &self.is_latin1())
            .field("rope_depth", &self.0.depth())
            .finish_non_exhaustive()
    }
}

/// A non-recursive iterator over a [`JsString`]'s UTF-16 code units.
pub struct CodeUnits<'a> {
    stack: [Option<&'a Repr>; ROPE_ITER_STACK_CAPACITY],
    stack_len: usize,
    leaf: Option<LeafCursor<'a>>,
    remaining: usize,
}

enum LeafCursor<'a> {
    Latin1(&'a [u8], usize),
    Utf16(&'a [u16], usize),
}

impl<'a> CodeUnits<'a> {
    fn new(string: &'a JsString) -> Self {
        let mut stack = [None; ROPE_ITER_STACK_CAPACITY];
        stack[0] = Some(string.0.as_ref());
        Self {
            stack,
            stack_len: 1,
            leaf: None,
            remaining: string.len() as usize,
        }
    }
}

impl Iterator for CodeUnits<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(cursor) = &mut self.leaf {
                let next = match cursor {
                    LeafCursor::Latin1(units, index) => units.get(*index).copied().map(u16::from),
                    LeafCursor::Utf16(units, index) => units.get(*index).copied(),
                };
                if let Some(unit) = next {
                    match cursor {
                        LeafCursor::Latin1(_, index) | LeafCursor::Utf16(_, index) => *index += 1,
                    }
                    self.remaining -= 1;
                    return Some(unit);
                }
                self.leaf = None;
            }

            let mut node = {
                self.stack_len = self.stack_len.checked_sub(1)?;
                self.stack[self.stack_len]
                    .take()
                    .expect("string iterator stack entries are initialized")
            };
            loop {
                match node {
                    Repr::Latin1(units) => {
                        self.leaf = Some(LeafCursor::Latin1(units, 0));
                        break;
                    }
                    Repr::Utf16(units) => {
                        self.leaf = Some(LeafCursor::Utf16(units, 0));
                        break;
                    }
                    Repr::Rope { left, right, .. } => {
                        debug_assert!(
                            self.stack_len < ROPE_ITER_STACK_CAPACITY,
                            "rope depth invariant"
                        );
                        self.stack[self.stack_len] = Some(right.0.as_ref());
                        self.stack_len += 1;
                        node = left.0.as_ref();
                    }
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for CodeUnits<'_> {
    fn len(&self) -> usize {
        self.remaining
    }
}

impl std::iter::FusedIterator for CodeUnits<'_> {}

/// Failures while constructing or transforming a JavaScript string.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JsStringError {
    /// A requested length exceeded the pinned `QuickJS` limit.
    TooLong {
        /// Requested UTF-16 code-unit length.
        requested: u64,
        /// Maximum supported UTF-16 code-unit length.
        maximum: u32,
    },
    /// A growable backing buffer could not reserve capacity.
    AllocationFailed {
        /// Additional capacity requested from the allocator.
        additional: usize,
    },
    /// A code-unit range was reversed or outside the string.
    InvalidRange {
        /// Requested inclusive start.
        start: u32,
        /// Requested exclusive end.
        end: u32,
        /// Available code-unit length.
        len: u32,
    },
}

impl fmt::Display for JsStringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { requested, maximum } => write!(
                formatter,
                "JavaScript string length {requested} exceeds the supported maximum of {maximum} UTF-16 code units"
            ),
            Self::AllocationFailed { additional } => write!(
                formatter,
                "could not reserve {additional} additional elements in a JavaScript string backing buffer"
            ),
            Self::InvalidRange { start, end, len } => write!(
                formatter,
                "JavaScript string range {start}..{end} is invalid for length {len}"
            ),
        }
    }
}

impl Error for JsStringError {}

fn check_length(len: usize) -> Result<u32, JsStringError> {
    if len > MAX_STRING_CODE_UNITS as usize {
        return Err(JsStringError::TooLong {
            requested: len as u64,
            maximum: MAX_STRING_CODE_UNITS,
        });
    }
    Ok(u32::try_from(len).expect("length was bounded by the u32 compatibility limit"))
}

fn checked_sum(left: u32, right: u32) -> Result<u32, JsStringError> {
    let requested = u64::from(left) + u64::from(right);
    if requested > u64::from(MAX_STRING_CODE_UNITS) {
        return Err(JsStringError::TooLong {
            requested,
            maximum: MAX_STRING_CODE_UNITS,
        });
    }
    Ok(u32::try_from(requested).expect("sum was bounded by the u32 compatibility limit"))
}

fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), JsStringError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| JsStringError::AllocationFailed { additional })
}

fn try_push<T>(values: &mut Vec<T>, value: T) -> Result<(), JsStringError> {
    if values.len() == values.capacity() {
        values
            .try_reserve(1)
            .map_err(|_| JsStringError::AllocationFailed { additional: 1 })?;
    }
    values.push(value);
    Ok(())
}

const fn is_high_surrogate(unit: u16) -> bool {
    matches!(unit, 0xd800..=0xdbff)
}

const fn is_low_surrogate(unit: u16) -> bool {
    matches!(unit, 0xdc00..=0xdfff)
}

fn encode_utf8_code_point(output: &mut Vec<u8>, code_point: u32) {
    if code_point < 0x80 {
        output.push(utf8_byte(code_point));
    } else if code_point < 0x800 {
        output.push(utf8_byte(code_point >> 6) | 0xc0);
        output.push(utf8_byte(code_point & 0x3f) | 0x80);
    } else if code_point < 0x1_0000 {
        output.push(utf8_byte(code_point >> 12) | 0xe0);
        output.push(utf8_byte((code_point >> 6) & 0x3f) | 0x80);
        output.push(utf8_byte(code_point & 0x3f) | 0x80);
    } else {
        output.push(utf8_byte(code_point >> 18) | 0xf0);
        output.push(utf8_byte((code_point >> 12) & 0x3f) | 0x80);
        output.push(utf8_byte((code_point >> 6) & 0x3f) | 0x80);
        output.push(utf8_byte(code_point & 0x3f) | 0x80);
    }
}

fn utf8_byte(value: u32) -> u8 {
    u8::try_from(value).expect("UTF-8 bit fields fit in one byte")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{JsString, ROPE_MAX_DEPTH, ROPE_SHORT_LEFT_LEN, ROPE_SHORT_LEN};

    #[test]
    fn clones_and_empty_values_share_immutable_backing_nodes() {
        let value = JsString::from_utf8("shared").expect("string");
        let clone = value.clone();
        assert!(Arc::ptr_eq(&value.0, &clone.0));

        let first_empty = JsString::empty();
        let second_empty = JsString::empty();
        assert!(Arc::ptr_eq(&first_empty.0, &second_empty.0));
    }

    #[test]
    fn strings_are_send_and_sync() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<JsString>();
    }

    #[test]
    fn repeated_large_concatenation_never_exceeds_the_rope_depth_limit() {
        let chunk = JsString::from_latin1(&vec![b'x'; 513]).expect("chunk");
        let mut string = JsString::empty();
        for _ in 0..200 {
            string = string.concat(&chunk).expect("concat");
            assert!(string.depth() <= ROPE_MAX_DEPTH);
        }
        assert_eq!(string.len(), 102_600);
        assert!(string.code_units().all(|unit| unit == u16::from(b'x')));
    }

    #[test]
    fn rebalancing_preserves_heterogeneous_leaf_order_in_both_directions() {
        let chunks = (0..80_u16)
            .map(|marker| {
                JsString::from_code_units(std::iter::repeat_n(0x0100 + marker, 513)).expect("chunk")
            })
            .collect::<Vec<_>>();

        let mut left_skewed = JsString::empty();
        for chunk in &chunks {
            left_skewed = left_skewed.concat(chunk).expect("left-skewed concat");
        }
        let expected_left = chunks
            .iter()
            .flat_map(JsString::code_units)
            .collect::<Vec<_>>();
        assert_eq!(left_skewed.code_units().collect::<Vec<_>>(), expected_left);
        assert!(left_skewed.depth() <= ROPE_MAX_DEPTH);

        let mut right_skewed = JsString::empty();
        for chunk in &chunks {
            right_skewed = chunk.concat(&right_skewed).expect("right-skewed concat");
        }
        let expected_right = chunks
            .iter()
            .rev()
            .flat_map(JsString::code_units)
            .collect::<Vec<_>>();
        assert_eq!(
            right_skewed.code_units().collect::<Vec<_>>(),
            expected_right
        );
        assert!(right_skewed.depth() <= ROPE_MAX_DEPTH);
    }

    #[test]
    fn short_concat_thresholds_match_the_pinned_quickjs_release() {
        let left_at_limit =
            JsString::from_latin1(&vec![b'l'; ROPE_SHORT_LEFT_LEN as usize]).expect("left");
        let left_over_limit =
            JsString::from_latin1(&vec![b'l'; ROPE_SHORT_LEFT_LEN as usize + 1]).expect("left");
        let right_at_limit =
            JsString::from_latin1(&vec![b'r'; ROPE_SHORT_LEN as usize]).expect("right");
        let right_over_limit =
            JsString::from_latin1(&vec![b'r'; ROPE_SHORT_LEN as usize + 1]).expect("right");

        assert_eq!(
            left_at_limit
                .concat(&right_at_limit)
                .expect("copied concat")
                .depth(),
            0
        );
        assert_eq!(
            left_over_limit
                .concat(&right_at_limit)
                .expect("rope concat")
                .depth(),
            1
        );
        assert_eq!(
            JsString::from_latin1(b"left")
                .expect("left")
                .concat(&right_over_limit)
                .expect("rope concat")
                .depth(),
            1
        );
    }
}
