use std::{
    collections::HashMap,
    error::Error,
    fmt,
    hash::{Hash, Hasher},
    sync::Arc,
};

use crate::SourceMap;

#[derive(Debug)]
struct RegistryIdentity;

/// An identifier issued by one particular [`SourceRegistry`].
///
/// The registry identity is part of equality and hashing. Passing an ID to a
/// different registry is rejected instead of accidentally selecting a source
/// with the same numeric index.
#[derive(Clone)]
pub struct SourceId {
    registry: Arc<RegistryIdentity>,
    index: u32,
}

impl SourceId {
    /// Returns the stable zero-based index within the issuing registry.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }
}

impl fmt::Debug for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SourceId")
            .field(&self.index)
            .finish()
    }
}

impl PartialEq for SourceId {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && Arc::ptr_eq(&self.registry, &other.registry)
    }
}

impl Eq for SourceId {}

impl Hash for SourceId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.registry).hash(state);
        self.index.hash(state);
    }
}

/// A validated half-open UTF-8 byte range.
///
/// Both endpoints are Unicode scalar boundaries in the source text used to
/// construct the span. Empty spans, including one at end-of-file, are valid.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteSpan {
    start: u32,
    end: u32,
}

impl ByteSpan {
    /// Validates a half-open byte range against `source`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::InvalidRange`] for reversed or out-of-bounds
    /// ranges, and [`SourceError::InvalidUtf8Boundary`] when an endpoint splits
    /// a UTF-8 encoded Unicode scalar.
    pub fn new(source: &str, start: usize, end: usize) -> Result<Self, SourceError> {
        if u32::try_from(source.len()).is_err() {
            return Err(SourceError::SourceTooLarge {
                bytes: source.len(),
            });
        }
        if start > end || end > source.len() {
            return Err(SourceError::InvalidRange {
                start,
                end,
                source_len: source.len(),
            });
        }
        if !source.is_char_boundary(start) {
            return Err(SourceError::InvalidUtf8Boundary { offset: start });
        }
        if !source.is_char_boundary(end) {
            return Err(SourceError::InvalidUtf8Boundary { offset: end });
        }
        let start = u32::try_from(start).map_err(|_| SourceError::SourceTooLarge {
            bytes: source.len(),
        })?;
        let end = u32::try_from(end).map_err(|_| SourceError::SourceTooLarge {
            bytes: source.len(),
        })?;
        Ok(Self { start, end })
    }

    /// Returns the inclusive start byte offset.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    /// Returns the exclusive end byte offset.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }

    /// Returns the byte length.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Returns whether this span contains no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A source-qualified, validated UTF-8 byte span.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceSpan {
    source_id: SourceId,
    bytes: ByteSpan,
}

impl SourceSpan {
    /// Returns the source containing this span.
    #[must_use]
    pub const fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// Returns the validated half-open byte span.
    #[must_use]
    pub const fn bytes(&self) -> ByteSpan {
        self.bytes
    }
}

/// A human-facing, one-based source position.
///
/// The column unit is selected explicitly through [`ColumnEncoding`] when the
/// position is computed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineColumn {
    line: usize,
    column: usize,
}

/// Column units supported by byte-to-position conversion.
///
/// All resulting columns are one-based. None of these variants represents
/// terminal display width.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ColumnEncoding {
    /// Count UTF-8 bytes from the beginning of the line.
    Utf8Byte,
    /// Count Unicode scalar values (`char` values).
    UnicodeScalar,
    /// Count UTF-16 code units, matching source-map v3 columns.
    Utf16CodeUnit,
}

impl LineColumn {
    /// Returns the one-based line.
    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    /// Returns the one-based column in the encoding selected during
    /// conversion.
    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }
}

#[derive(Clone, Debug)]
struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    fn new(source: &str) -> Result<Self, SourceError> {
        let _ = u32::try_from(source.len()).map_err(|_| SourceError::SourceTooLarge {
            bytes: source.len(),
        })?;
        let mut starts = vec![0];
        let mut offset = 0;
        while offset < source.len() {
            let remainder = &source[offset..];
            let Some(character) = remainder.chars().next() else {
                break;
            };
            let width = character.len_utf8();
            match character {
                '\r' if source.as_bytes().get(offset + 1) == Some(&b'\n') => {
                    offset += 2;
                    starts.push(u32::try_from(offset).map_err(|_| {
                        SourceError::SourceTooLarge {
                            bytes: source.len(),
                        }
                    })?);
                    continue;
                }
                '\r' | '\n' | '\u{2028}' | '\u{2029}' => {
                    offset += width;
                    starts.push(u32::try_from(offset).map_err(|_| {
                        SourceError::SourceTooLarge {
                            bytes: source.len(),
                        }
                    })?);
                    continue;
                }
                _ => {}
            }
            offset += width;
        }
        Ok(Self { starts })
    }

    fn line_index_at(&self, byte_offset: usize) -> usize {
        self.starts
            .partition_point(|start| (*start as usize) <= byte_offset)
            .saturating_sub(1)
    }

    fn position(
        &self,
        source: &str,
        byte_offset: usize,
        encoding: ColumnEncoding,
    ) -> Result<LineColumn, SourceError> {
        validate_offset(source, byte_offset)?;
        let line_index = self.line_index_at(byte_offset);
        let line_start = self.starts[line_index] as usize;
        let prefix = &source[line_start..byte_offset];
        let column = match encoding {
            ColumnEncoding::Utf8Byte => prefix.len(),
            ColumnEncoding::UnicodeScalar => prefix.chars().count(),
            ColumnEncoding::Utf16CodeUnit => prefix.chars().map(char::len_utf16).sum(),
        } + 1;
        Ok(LineColumn {
            line: line_index + 1,
            column,
        })
    }

    fn line_content_end(&self, source: &str, line_index: usize) -> usize {
        let next_start = self
            .starts
            .get(line_index + 1)
            .map_or(source.len(), |start| *start as usize);
        let line_start = self.starts[line_index] as usize;
        let line = &source[line_start..next_start];
        if line.ends_with("\r\n") {
            next_start - 2
        } else if line.ends_with(['\r', '\n']) {
            next_start - 1
        } else if line.ends_with(['\u{2028}', '\u{2029}']) {
            next_start - '\u{2028}'.len_utf8()
        } else {
            next_start
        }
    }

    pub(crate) fn byte_offset_for_source_map_position(
        &self,
        source: &str,
        line: u32,
        column: u32,
    ) -> Result<usize, SourceError> {
        let line_index = usize::try_from(line)
            .map_err(|_| SourceError::InvalidSourceMapPosition { line, column })?;
        let Some(&line_start) = self.starts.get(line_index) else {
            return Err(SourceError::InvalidSourceMapPosition { line, column });
        };
        let line_start = line_start as usize;
        let line_end = self.line_content_end(source, line_index);
        let mut utf16_column = 0_u32;
        for (relative, character) in source[line_start..line_end].char_indices() {
            if utf16_column == column {
                return Ok(line_start + relative);
            }
            let width: u32 = if character.len_utf16() == 1 { 1 } else { 2 };
            if column < utf16_column.saturating_add(width) {
                return Err(SourceError::InvalidSourceMapPosition { line, column });
            }
            utf16_column = utf16_column.saturating_add(width);
        }
        if utf16_column == column {
            Ok(line_end)
        } else {
            Err(SourceError::InvalidSourceMapPosition { line, column })
        }
    }
}

fn validate_offset(source: &str, offset: usize) -> Result<(), SourceError> {
    if offset > source.len() {
        return Err(SourceError::InvalidRange {
            start: offset,
            end: offset,
            source_len: source.len(),
        });
    }
    if !source.is_char_boundary(offset) {
        return Err(SourceError::InvalidUtf8Boundary { offset });
    }
    Ok(())
}

/// One immutable source unit owned by a [`SourceRegistry`].
#[derive(Clone, Debug)]
pub struct SourceFile {
    display_name: String,
    text: Arc<str>,
    line_index: LineIndex,
    incoming_source_map: Option<SourceMap>,
}

impl SourceFile {
    /// Returns the human-facing source name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the immutable source text.
    #[must_use]
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }

    /// Returns the optional map from this generated source to an earlier
    /// source.
    #[must_use]
    pub const fn incoming_source_map(&self) -> Option<&SourceMap> {
        self.incoming_source_map.as_ref()
    }

    /// Converts a validated UTF-8 byte offset to a one-based line and
    /// Unicode-scalar column.
    ///
    /// # Errors
    ///
    /// Returns an error for out-of-bounds offsets and offsets within a
    /// multi-byte UTF-8 sequence.
    pub fn position(&self, byte_offset: usize) -> Result<LineColumn, SourceError> {
        self.position_with_encoding(byte_offset, ColumnEncoding::UnicodeScalar)
    }

    /// Converts a validated UTF-8 byte offset using an explicit column unit.
    ///
    /// # Errors
    ///
    /// Returns an error for out-of-bounds offsets and offsets within a
    /// multi-byte UTF-8 sequence.
    pub fn position_with_encoding(
        &self,
        byte_offset: usize,
        encoding: ColumnEncoding,
    ) -> Result<LineColumn, SourceError> {
        self.line_index.position(&self.text, byte_offset, encoding)
    }

    pub(crate) fn validate_source_map_position(
        &self,
        line: u32,
        column: u32,
    ) -> Result<(), SourceError> {
        self.line_index
            .byte_offset_for_source_map_position(&self.text, line, column)
            .map(|_| ())
    }
}

/// An owned excerpt around a source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSnippet {
    source_name: String,
    text: String,
    highlight: ByteSpan,
    starts_at: LineColumn,
}

impl SourceSnippet {
    /// Returns the source display name.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Returns the excerpt text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the highlighted byte span relative to [`Self::text`].
    #[must_use]
    pub const fn highlight(&self) -> ByteSpan {
        self.highlight
    }

    /// Returns the absolute one-based position where the excerpt starts.
    #[must_use]
    pub const fn starts_at(&self) -> LineColumn {
        self.starts_at
    }
}

/// An owned collection of immutable source units.
///
/// Display names are unique so source-map chaining can resolve a map's
/// `sources` entry deterministically.
#[derive(Debug)]
pub struct SourceRegistry {
    identity: Arc<RegistryIdentity>,
    files: Vec<SourceFile>,
    names: HashMap<String, u32>,
}

impl SourceRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            identity: Arc::new(RegistryIdentity),
            files: Vec::new(),
            names: HashMap::new(),
        }
    }

    /// Registers source text without an incoming source map.
    ///
    /// # Errors
    ///
    /// Rejects empty or duplicate names, sources larger than `u32::MAX`
    /// bytes, and registries with more than `u32::MAX` entries.
    pub fn register(
        &mut self,
        display_name: impl Into<String>,
        text: impl Into<Arc<str>>,
    ) -> Result<SourceId, SourceError> {
        self.register_with_source_map(display_name, text, None)
    }

    /// Registers source text with an optional incoming standard source-map v3
    /// object.
    ///
    /// # Errors
    ///
    /// Has the same validation as [`Self::register`].
    pub fn register_with_source_map(
        &mut self,
        display_name: impl Into<String>,
        text: impl Into<Arc<str>>,
        incoming_source_map: Option<SourceMap>,
    ) -> Result<SourceId, SourceError> {
        let display_name = display_name.into();
        if display_name.is_empty() {
            return Err(SourceError::EmptySourceName);
        }
        if self.names.contains_key(&display_name) {
            return Err(SourceError::DuplicateSourceName(display_name));
        }
        let index = u32::try_from(self.files.len()).map_err(|_| SourceError::TooManySources)?;
        let text = text.into();
        let line_index = LineIndex::new(&text)?;
        self.files.push(SourceFile {
            display_name: display_name.clone(),
            text,
            line_index,
            incoming_source_map,
        });
        self.names.insert(display_name, index);
        Ok(SourceId {
            registry: Arc::clone(&self.identity),
            index,
        })
    }

    /// Replaces the incoming source map for a registered source.
    ///
    /// # Errors
    ///
    /// Returns an error when `source_id` was not issued by this registry.
    pub fn set_source_map(
        &mut self,
        source_id: &SourceId,
        source_map: Option<SourceMap>,
    ) -> Result<(), SourceError> {
        self.source_mut(source_id)?.incoming_source_map = source_map;
        Ok(())
    }

    /// Returns a registered source.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign or otherwise invalid ID.
    pub fn source(&self, source_id: &SourceId) -> Result<&SourceFile, SourceError> {
        if !Arc::ptr_eq(&self.identity, &source_id.registry) {
            return Err(SourceError::ForeignSourceId);
        }
        self.files
            .get(source_id.index as usize)
            .ok_or(SourceError::UnknownSourceId {
                index: source_id.index,
            })
    }

    fn source_mut(&mut self, source_id: &SourceId) -> Result<&mut SourceFile, SourceError> {
        if !Arc::ptr_eq(&self.identity, &source_id.registry) {
            return Err(SourceError::ForeignSourceId);
        }
        self.files
            .get_mut(source_id.index as usize)
            .ok_or(SourceError::UnknownSourceId {
                index: source_id.index,
            })
    }

    /// Resolves an exact source-map `sources` name to a registered source.
    #[must_use]
    pub fn source_id_by_name(&self, display_name: &str) -> Option<SourceId> {
        self.names.get(display_name).map(|index| SourceId {
            registry: Arc::clone(&self.identity),
            index: *index,
        })
    }

    /// Constructs a validated source-qualified span.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign ID or invalid UTF-8 byte range.
    pub fn span(
        &self,
        source_id: &SourceId,
        start: usize,
        end: usize,
    ) -> Result<SourceSpan, SourceError> {
        let source = self.source(source_id)?;
        let bytes = ByteSpan::new(&source.text, start, end)?;
        Ok(SourceSpan {
            source_id: source_id.clone(),
            bytes,
        })
    }

    /// Converts a byte offset in a registered source to a one-based position.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign ID or invalid byte offset.
    pub fn position(
        &self,
        source_id: &SourceId,
        byte_offset: usize,
    ) -> Result<LineColumn, SourceError> {
        self.source(source_id)?.position(byte_offset)
    }

    /// Converts a byte offset in a registered source using an explicit column
    /// unit.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign ID or invalid byte offset.
    pub fn position_with_encoding(
        &self,
        source_id: &SourceId,
        byte_offset: usize,
        encoding: ColumnEncoding,
    ) -> Result<LineColumn, SourceError> {
        self.source(source_id)?
            .position_with_encoding(byte_offset, encoding)
    }

    /// Extracts an owned line-aligned excerpt around `span`.
    ///
    /// `context_lines_before` and `context_lines_after` are saturating; asking
    /// for more context than exists simply returns the full available source.
    ///
    /// # Errors
    ///
    /// Returns an error if the span belongs to a different registry.
    pub fn snippet(
        &self,
        span: &SourceSpan,
        context_lines_before: usize,
        context_lines_after: usize,
    ) -> Result<SourceSnippet, SourceError> {
        let source = self.source(&span.source_id)?;
        let start = span.bytes.start as usize;
        let end = span.bytes.end as usize;
        let start_line = source.line_index.line_index_at(start);
        let last_highlighted_byte = if start == end {
            start
        } else {
            end.saturating_sub(1)
        };
        let end_line = source.line_index.line_index_at(last_highlighted_byte);
        let first_line = start_line.saturating_sub(context_lines_before);
        let final_line = end_line
            .saturating_add(context_lines_after)
            .min(source.line_index.starts.len().saturating_sub(1));
        let excerpt_start = source.line_index.starts[first_line] as usize;
        let excerpt_end = source
            .line_index
            .starts
            .get(final_line + 1)
            .map_or(source.text.len(), |offset| *offset as usize);
        let text = source.text[excerpt_start..excerpt_end].to_owned();
        let relative_start = start - excerpt_start;
        let relative_end = end - excerpt_start;
        let highlight = ByteSpan::new(&text, relative_start, relative_end)?;
        let starts_at =
            source.position_with_encoding(excerpt_start, ColumnEncoding::UnicodeScalar)?;
        Ok(SourceSnippet {
            source_name: source.display_name.clone(),
            text,
            highlight,
            starts_at,
        })
    }
}

impl Default for SourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Validation failures from source registration and location conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SourceError {
    /// A source name was empty.
    EmptySourceName,
    /// A source name was already registered.
    DuplicateSourceName(String),
    /// A source exceeded the `u32` byte-offset domain used by compiler spans.
    SourceTooLarge {
        /// Actual UTF-8 byte length.
        bytes: usize,
    },
    /// The registry cannot represent another source.
    TooManySources,
    /// The ID belongs to another registry.
    ForeignSourceId,
    /// The numeric portion of an otherwise local ID is unknown.
    UnknownSourceId {
        /// Invalid registry-local index.
        index: u32,
    },
    /// A half-open byte range was reversed or out of bounds.
    InvalidRange {
        /// Requested start.
        start: usize,
        /// Requested end.
        end: usize,
        /// Available UTF-8 byte length.
        source_len: usize,
    },
    /// A byte offset split a multi-byte UTF-8 encoding.
    InvalidUtf8Boundary {
        /// Invalid byte offset.
        offset: usize,
    },
    /// A zero-based source-map line/UTF-16 column was outside the source or
    /// split a surrogate pair.
    InvalidSourceMapPosition {
        /// Zero-based line.
        line: u32,
        /// Zero-based UTF-16 column.
        column: u32,
    },
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySourceName => formatter.write_str("source name must not be empty"),
            Self::DuplicateSourceName(name) => {
                write!(formatter, "source name `{name}` is already registered")
            }
            Self::SourceTooLarge { bytes } => write!(
                formatter,
                "source contains {bytes} UTF-8 bytes; at most {} are supported",
                u32::MAX
            ),
            Self::TooManySources => formatter.write_str("source registry is full"),
            Self::ForeignSourceId => {
                formatter.write_str("source ID belongs to a different registry")
            }
            Self::UnknownSourceId { index } => {
                write!(formatter, "source ID {index} is not registered")
            }
            Self::InvalidRange {
                start,
                end,
                source_len,
            } => write!(
                formatter,
                "byte range {start}..{end} is invalid for a {source_len}-byte source"
            ),
            Self::InvalidUtf8Boundary { offset } => {
                write!(formatter, "byte offset {offset} is not a UTF-8 boundary")
            }
            Self::InvalidSourceMapPosition { line, column } => write!(
                formatter,
                "source-map position {line}:{column} is outside the source or splits a UTF-16 surrogate pair"
            ),
        }
    }
}

impl Error for SourceError {}
