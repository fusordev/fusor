//! Immutable exact-string artifacts shared by compiler constants and atoms.

use std::{error::Error, fmt, iter::FusedIterator, slice, sync::Arc};

/// Maximum UTF-16 length accepted by the compatible `QuickJS` string model.
pub const MAX_COMPILER_STRING_CODE_UNITS: usize = (1 << 30) - 1;

/// A proposed compiler string exceeds the compatible string-length domain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CompilerStringLengthError {
    observed: usize,
}

impl CompilerStringLengthError {
    /// Returns the rejected code-unit length.
    #[must_use]
    pub const fn observed(self) -> usize {
        self.observed
    }

    /// Returns the inclusive compatible maximum.
    #[must_use]
    pub const fn maximum(self) -> usize {
        MAX_COMPILER_STRING_CODE_UNITS
    }
}

impl fmt::Display for CompilerStringLengthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "compiler string length {} exceeds maximum {} UTF-16 code units",
            self.observed, MAX_COMPILER_STRING_CODE_UNITS
        )
    }
}

impl Error for CompilerStringLengthError {}

/// Failure to freeze exact UTF-16 code units as an immutable compiler string.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerStringError {
    /// The logical string length exceeds the compatible domain.
    Length(CompilerStringLengthError),
    /// Compact Latin-1 storage could not reserve its payload.
    AllocationFailed {
        /// Number of payload bytes requested.
        requested: usize,
    },
}

impl fmt::Display for CompilerStringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length(source) => source.fmt(formatter),
            Self::AllocationFailed { requested } => write!(
                formatter,
                "failed to reserve {requested} bytes for a compact compiler string"
            ),
        }
    }
}

impl Error for CompilerStringError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Length(source) => Some(source),
            Self::AllocationFailed { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum CompilerStringStorage {
    Latin1(Arc<[u8]>),
    Utf16(Arc<[u16]>),
}

/// An immutable ECMAScript string represented as exact UTF-16 code units.
///
/// Values whose units all fit in Latin-1 use the compact representation used
/// by `QuickJS`. Wider strings preserve their original `u16` units verbatim,
/// including lone surrogates. Construction is canonical, so equality and
/// hashing are logical string equality despite the private dual-width storage.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompilerString {
    storage: CompilerStringStorage,
}

impl CompilerString {
    /// Canonicalizes owned UTF-16 code units into immutable compact storage.
    ///
    /// # Errors
    ///
    /// Rejects lengths outside the compatible `QuickJS` string domain or a
    /// failed compact-storage reservation.
    pub fn try_from_code_units(code_units: Arc<[u16]>) -> Result<Self, CompilerStringError> {
        if code_units.len() > MAX_COMPILER_STRING_CODE_UNITS {
            return Err(CompilerStringError::Length(CompilerStringLengthError {
                observed: code_units.len(),
            }));
        }
        if code_units.iter().any(|unit| u8::try_from(*unit).is_err()) {
            return Ok(Self {
                storage: CompilerStringStorage::Utf16(code_units),
            });
        }

        let mut latin1 = Vec::new();
        latin1.try_reserve_exact(code_units.len()).map_err(|_| {
            CompilerStringError::AllocationFailed {
                requested: code_units.len(),
            }
        })?;
        for unit in code_units.iter().copied() {
            let Ok(unit) = u8::try_from(unit) else {
                return Ok(Self {
                    storage: CompilerStringStorage::Utf16(code_units),
                });
            };
            latin1.push(unit);
        }
        Ok(Self {
            storage: CompilerStringStorage::Latin1(latin1.into()),
        })
    }

    /// Returns the number of ECMAScript UTF-16 code units.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.storage {
            CompilerStringStorage::Latin1(units) => units.len(),
            CompilerStringStorage::Utf16(units) => units.len(),
        }
    }

    /// Returns whether this string has no code units.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the exact compact payload size in bytes.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        match &self.storage {
            CompilerStringStorage::Latin1(units) => units.len(),
            CompilerStringStorage::Utf16(units) => units.len() * size_of::<u16>(),
        }
    }

    /// Returns the compact units when this string is entirely Latin-1.
    #[must_use]
    pub fn latin1_units(&self) -> Option<&[u8]> {
        match &self.storage {
            CompilerStringStorage::Latin1(units) => Some(units),
            CompilerStringStorage::Utf16(_) => None,
        }
    }

    /// Returns the exact wide units when any unit exceeds Latin-1.
    #[must_use]
    pub fn utf16_units(&self) -> Option<&[u16]> {
        match &self.storage {
            CompilerStringStorage::Latin1(_) => None,
            CompilerStringStorage::Utf16(units) => Some(units),
        }
    }

    /// Iterates over the logical UTF-16 code units without allocating.
    #[must_use]
    pub fn code_units(&self) -> CompilerStringCodeUnits<'_> {
        match &self.storage {
            CompilerStringStorage::Latin1(units) => CompilerStringCodeUnits::Latin1(units.iter()),
            CompilerStringStorage::Utf16(units) => CompilerStringCodeUnits::Utf16(units.iter()),
        }
    }
}

/// A zero-allocation iterator over an immutable compiler string's UTF-16 units.
#[derive(Clone, Debug)]
pub enum CompilerStringCodeUnits<'a> {
    /// Compact eight-bit storage.
    Latin1(slice::Iter<'a, u8>),
    /// Exact wide storage.
    Utf16(slice::Iter<'a, u16>),
}

impl Iterator for CompilerStringCodeUnits<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Latin1(units) => units.next().copied().map(u16::from),
            Self::Utf16(units) => units.next().copied(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl DoubleEndedIterator for CompilerStringCodeUnits<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Latin1(units) => units.next_back().copied().map(u16::from),
            Self::Utf16(units) => units.next_back().copied(),
        }
    }
}

impl ExactSizeIterator for CompilerStringCodeUnits<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Latin1(units) => units.len(),
            Self::Utf16(units) => units.len(),
        }
    }
}

impl FusedIterator for CompilerStringCodeUnits<'_> {}

/// One immutable entry in a compiler function's content-interned atom pool.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompilerAtom {
    string: CompilerString,
}

impl CompilerAtom {
    /// Wraps an exact string as an atom payload.
    #[must_use]
    pub const fn new(string: CompilerString) -> Self {
        Self { string }
    }

    /// Returns the exact string represented by this atom.
    #[must_use]
    pub const fn string(&self) -> &CompilerString {
        &self.string
    }
}
