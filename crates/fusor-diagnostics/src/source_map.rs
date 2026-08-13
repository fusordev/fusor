use std::{
    collections::HashSet,
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use serde_json::Value;
use sourcemap::DecodedMap;

use crate::{ColumnEncoding, SourceError, SourceId, SourceRegistry, SourceSpan};

const DEFAULT_MAX_SOURCE_MAP_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_CHAIN_DEPTH: usize = 32;

/// A zero-based source-map v3 position.
///
/// Columns are UTF-16 code-unit offsets, as required by source-map v3. This is
/// deliberately distinct from the one-based Unicode-scalar [`crate::LineColumn`]
/// used for human-facing diagnostics.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceMapPosition {
    line: u32,
    column: u32,
}

impl SourceMapPosition {
    /// Creates a zero-based source-map position.
    #[must_use]
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }

    /// Returns the zero-based line.
    #[must_use]
    pub const fn line(self) -> u32 {
        self.line
    }

    /// Returns the zero-based UTF-16 column.
    #[must_use]
    pub const fn column(self) -> u32 {
        self.column
    }
}

/// One owned result from a source-map lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceMapMapping {
    source_name: String,
    original: SourceMapPosition,
    name: Option<String>,
    source_content: Option<Arc<str>>,
}

impl SourceMapMapping {
    /// Returns the source-map `sources` entry after applying `sourceRoot`.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Returns the mapped zero-based original position.
    #[must_use]
    pub const fn original(&self) -> SourceMapPosition {
        self.original
    }

    /// Returns the optional mapped identifier/function name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the optional `sourcesContent` entry retained by the map.
    #[must_use]
    pub fn source_content(&self) -> Option<&Arc<str>> {
        self.source_content.as_ref()
    }
}

/// An owned standard source-map v3 object.
///
/// Regular maps and indexed maps with embedded sections are supported.
/// Looking up a URL-only indexed section returns a structured unresolved-map
/// error because loading external URLs is a host policy, not a source-map
/// parser responsibility.
#[derive(Clone, Debug)]
pub struct SourceMap {
    inner: Arc<DecodedMap>,
}

impl SourceMap {
    /// Decodes a standard source-map v3 JSON document.
    ///
    /// Input is limited to 64 MiB by default. Use [`Self::from_slice_with_limit`]
    /// when a host has a different trusted resource policy.
    ///
    /// # Errors
    ///
    /// Returns a structured error for oversized input, invalid JSON,
    /// non-v3/extension formats, malformed mappings, or an unexpected decoder
    /// unwind.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, SourceMapError> {
        Self::from_slice_with_limit(bytes, DEFAULT_MAX_SOURCE_MAP_BYTES)
    }

    /// Decodes a standard source-map v3 document with a caller-selected byte
    /// limit.
    ///
    /// # Errors
    ///
    /// Has the same failure modes as [`Self::from_slice`].
    pub fn from_slice_with_limit(bytes: &[u8], max_bytes: usize) -> Result<Self, SourceMapError> {
        if bytes.len() > max_bytes {
            return Err(SourceMapError::new(
                SourceMapErrorKind::InputTooLarge,
                format!(
                    "source map contains {} bytes; the configured limit is {max_bytes}",
                    bytes.len()
                ),
            ));
        }

        let json_bytes = strip_optional_junk_header(bytes)?;
        let json: Value = serde_json::from_slice(json_bytes).map_err(|error| {
            SourceMapError::new(
                SourceMapErrorKind::InvalidJson,
                format!("source map is not valid JSON: {error}"),
            )
        })?;
        validate_v3_document(&json, "root")?;

        let decoded = catch_unwind(AssertUnwindSafe(|| sourcemap::decode_slice(bytes)))
            .map_err(|_| {
                SourceMapError::new(
                    SourceMapErrorKind::DecoderPanicked,
                    "source-map decoder aborted while processing user input",
                )
            })?
            .map_err(|error| {
                SourceMapError::new(
                    SourceMapErrorKind::MalformedMappings,
                    format!("source map is malformed: {error}"),
                )
            })?;

        if matches!(decoded, DecodedMap::Hermes(_)) {
            return Err(SourceMapError::new(
                SourceMapErrorKind::UnsupportedFormat,
                "Hermes source-map extensions are not standard source-map v3",
            ));
        }

        Ok(Self {
            inner: Arc::new(decoded),
        })
    }

    /// Looks up the greatest mapping not after `generated`.
    ///
    /// An unmapped segment or a position before the first token returns
    /// `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns a structured error if the underlying map unexpectedly unwinds.
    pub fn lookup(
        &self,
        generated: SourceMapPosition,
    ) -> Result<Option<SourceMapMapping>, SourceMapError> {
        catch_unwind(AssertUnwindSafe(|| {
            if let Some(url) = unresolved_section_url(&self.inner, generated.line, generated.column)
            {
                return Err(SourceMapError::new(
                    SourceMapErrorKind::UnresolvedSection,
                    format!(
                        "indexed source-map section `{url}` is external and has not been loaded"
                    ),
                ));
            }
            let Some(token) = self.inner.lookup_token(generated.line, generated.column) else {
                return Ok(None);
            };
            if !token.has_source()
                || token.get_src_line() == u32::MAX
                || token.get_src_col() == u32::MAX
            {
                return Ok(None);
            }
            let source_name = token.get_source().ok_or_else(|| {
                SourceMapError::new(
                    SourceMapErrorKind::MalformedMappings,
                    "source-map token references a missing source",
                )
            })?;
            let source_content = token
                .get_source_view()
                .map(|view| Arc::<str>::from(view.source()));
            Ok(Some(SourceMapMapping {
                source_name: source_name.to_owned(),
                original: SourceMapPosition::new(token.get_src_line(), token.get_src_col()),
                name: token.get_name().map(str::to_owned),
                source_content,
            }))
        }))
        .map_err(|_| {
            SourceMapError::new(
                SourceMapErrorKind::LookupPanicked,
                "source-map lookup aborted while processing decoded metadata",
            )
        })?
    }
}

fn unresolved_section_url(map: &DecodedMap, line: u32, column: u32) -> Option<String> {
    let DecodedMap::Index(index) = map else {
        return None;
    };
    let query = (line, column);
    let section = index
        .sections()
        .take_while(|section| section.get_offset() <= query)
        .last()?;
    let (offset_line, offset_column) = section.get_offset();
    let nested_line = line - offset_line;
    let nested_column = if line == offset_line {
        column - offset_column
    } else {
        column
    };
    match section.get_sourcemap() {
        Some(nested) => unresolved_section_url(nested, nested_line, nested_column),
        None => Some(section.get_url().unwrap_or("<unnamed section>").to_owned()),
    }
}

fn strip_optional_junk_header(bytes: &[u8]) -> Result<&[u8], SourceMapError> {
    let Some(first) = bytes.first() else {
        return Ok(bytes);
    };
    if !matches!(*first, b')' | b']' | b'}' | b'\'') {
        return Ok(bytes);
    }
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'\n' {
            return Ok(&bytes[index..]);
        }
        if byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err(SourceMapError::new(
                SourceMapErrorKind::InvalidJson,
                "source-map JSON guard header contains a bare carriage return",
            ));
        }
    }
    Ok(&bytes[bytes.len()..])
}

fn validate_v3_document(value: &Value, path: &str) -> Result<(), SourceMapError> {
    let object = value.as_object().ok_or_else(|| {
        SourceMapError::new(
            SourceMapErrorKind::InvalidDocument,
            format!("{path} source map must be a JSON object"),
        )
    })?;
    match object.get("version").and_then(Value::as_u64) {
        Some(3) => {}
        Some(version) => {
            return Err(SourceMapError::new(
                SourceMapErrorKind::UnsupportedVersion,
                format!("{path} source map uses version {version}; version 3 is required"),
            ));
        }
        None => {
            return Err(SourceMapError::new(
                SourceMapErrorKind::UnsupportedVersion,
                format!("{path} source map must declare numeric version 3"),
            ));
        }
    }

    if let Some(sections) = object.get("sections") {
        return validate_indexed_map(object, sections, path);
    }
    validate_regular_map(object, path)
}

fn validate_indexed_map(
    object: &serde_json::Map<String, Value>,
    sections: &Value,
    path: &str,
) -> Result<(), SourceMapError> {
    if object.contains_key("mappings") {
        return Err(SourceMapError::new(
            SourceMapErrorKind::InvalidDocument,
            format!("{path} indexed source map must not also contain `mappings`"),
        ));
    }
    let sections = sections.as_array().ok_or_else(|| {
        SourceMapError::new(
            SourceMapErrorKind::InvalidDocument,
            format!("{path} `sections` must be an array"),
        )
    })?;
    let mut previous_offset = None;
    for (index, section) in sections.iter().enumerate() {
        let section_path = format!("{path}.sections[{index}]");
        let offset = validate_indexed_section(section, &section_path)?;
        if previous_offset.is_some_and(|previous| offset <= previous) {
            return Err(SourceMapError::new(
                SourceMapErrorKind::InvalidDocument,
                format!("{section_path}.offset must be greater than the preceding section offset"),
            ));
        }
        previous_offset = Some(offset);
    }
    Ok(())
}

fn validate_indexed_section(section: &Value, path: &str) -> Result<(u32, u32), SourceMapError> {
    let section = section.as_object().ok_or_else(|| {
        SourceMapError::new(
            SourceMapErrorKind::InvalidDocument,
            format!("{path} must be an object"),
        )
    })?;
    let offset = section
        .get("offset")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            SourceMapError::new(
                SourceMapErrorKind::InvalidDocument,
                format!("{path}.offset must be an object"),
            )
        })?;
    let offset = ["line", "column"].map(|coordinate| {
        offset
            .get(coordinate)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                SourceMapError::new(
                    SourceMapErrorKind::InvalidDocument,
                    format!("{path}.offset.{coordinate} must be a u32"),
                )
            })
    });
    let [line, column] = offset;
    let position = (line?, column?);
    match (section.get("map"), section.get("url")) {
        (Some(map), None) => validate_v3_document(map, path)?,
        (None, Some(Value::String(_))) => {}
        _ => {
            return Err(SourceMapError::new(
                SourceMapErrorKind::InvalidDocument,
                format!("{path} must contain exactly one of `map` or string `url`"),
            ));
        }
    }
    Ok(position)
}

fn validate_regular_map(
    object: &serde_json::Map<String, Value>,
    path: &str,
) -> Result<(), SourceMapError> {
    let sources = object
        .get("sources")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            SourceMapError::new(
                SourceMapErrorKind::InvalidDocument,
                format!("{path} regular source map requires a `sources` array"),
            )
        })?;
    if sources.iter().any(|source| !source.is_string()) {
        return Err(SourceMapError::new(
            SourceMapErrorKind::InvalidDocument,
            format!("{path} `sources` entries must be strings"),
        ));
    }
    let names = object
        .get("names")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            SourceMapError::new(
                SourceMapErrorKind::InvalidDocument,
                format!("{path} regular source map requires a `names` array"),
            )
        })?;
    if names.iter().any(|name| !name.is_string()) {
        return Err(SourceMapError::new(
            SourceMapErrorKind::InvalidDocument,
            format!("{path} `names` entries must be strings"),
        ));
    }
    if !object.get("mappings").is_some_and(Value::is_string) {
        return Err(SourceMapError::new(
            SourceMapErrorKind::InvalidDocument,
            format!("{path} regular source map requires a string `mappings` field"),
        ));
    }
    if let Some(contents) = object.get("sourcesContent") {
        let contents = contents.as_array().ok_or_else(|| {
            SourceMapError::new(
                SourceMapErrorKind::InvalidDocument,
                format!("{path} `sourcesContent` must be an array"),
            )
        })?;
        if contents.len() != sources.len()
            || contents
                .iter()
                .any(|content| !(content.is_null() || content.is_string()))
        {
            return Err(SourceMapError::new(
                SourceMapErrorKind::InvalidDocument,
                format!("{path} `sourcesContent` must contain one string or null per source"),
            ));
        }
    }
    Ok(())
}

/// The final source location after zero or more source-map hops.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginalLocation {
    source_name: String,
    source_id: Option<SourceId>,
    position: SourceMapPosition,
    embedded_source: Option<Arc<str>>,
}

impl OriginalLocation {
    /// Returns the final source name.
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Returns the registered source ID when the final `sources` name matched
    /// exactly.
    #[must_use]
    pub const fn source_id(&self) -> Option<&SourceId> {
        self.source_id.as_ref()
    }

    /// Returns the final zero-based source-map position.
    #[must_use]
    pub const fn position(&self) -> SourceMapPosition {
        self.position
    }

    /// Returns the final mapping's retained `sourcesContent`, when present.
    #[must_use]
    pub fn embedded_source(&self) -> Option<&Arc<str>> {
        self.embedded_source.as_ref()
    }
}

/// A generated location resolved through an incoming source-map chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLocation {
    generated_source: SourceId,
    generated_position: SourceMapPosition,
    original: OriginalLocation,
    name: Option<String>,
    hops: usize,
}

/// A validated generated span and its deepest registered mapped span.
///
/// Source-map v3 mappings are point mappings. The start and end of a generated
/// span are therefore resolved independently. When both endpoints reach the
/// same registered original source in order, [`Self::mapped_span`] contains
/// that original range. Otherwise callers retain [`Self::generated_span`] as
/// the safe rendering fallback while [`Self::location`] still exposes the
/// deepest mapped start position and embedded source metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSpan {
    generated_span: SourceSpan,
    mapped_span: Option<SourceSpan>,
    location: ResolvedLocation,
}

impl ResolvedSpan {
    /// Returns the validated span in the generated source.
    #[must_use]
    pub const fn generated_span(&self) -> &SourceSpan {
        &self.generated_span
    }

    /// Returns the mapped range when both endpoints resolved into one
    /// registered original source.
    #[must_use]
    pub const fn mapped_span(&self) -> Option<&SourceSpan> {
        self.mapped_span.as_ref()
    }

    /// Returns the best span for a source-backed diagnostic.
    ///
    /// This is the mapped original range when available and the generated
    /// range otherwise.
    #[must_use]
    pub const fn display_span(&self) -> &SourceSpan {
        match &self.mapped_span {
            Some(span) => span,
            None => &self.generated_span,
        }
    }

    /// Returns the deepest mapping of the generated start position.
    #[must_use]
    pub const fn location(&self) -> &ResolvedLocation {
        &self.location
    }
}

impl ResolvedLocation {
    /// Returns the generated registered source.
    #[must_use]
    pub const fn generated_source(&self) -> &SourceId {
        &self.generated_source
    }

    /// Returns the original query position.
    #[must_use]
    pub const fn generated_position(&self) -> SourceMapPosition {
        self.generated_position
    }

    /// Returns the deepest resolved location, or the generated location when
    /// no mapping was available.
    #[must_use]
    pub const fn original(&self) -> &OriginalLocation {
        &self.original
    }

    /// Returns the deepest available mapped name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the number of successful source-map hops.
    #[must_use]
    pub const fn hops(&self) -> usize {
        self.hops
    }

    /// Returns whether at least one source map contributed a mapping.
    #[must_use]
    pub const fn is_mapped(&self) -> bool {
        self.hops != 0
    }
}

impl SourceRegistry {
    /// Converts a registered UTF-8 byte offset to a zero-based source-map v3
    /// line and UTF-16 column.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign source ID, an out-of-bounds byte offset,
    /// or an offset that splits a UTF-8 scalar.
    pub fn source_map_position(
        &self,
        source_id: &SourceId,
        byte_offset: usize,
    ) -> Result<SourceMapPosition, SourceError> {
        let position =
            self.position_with_encoding(source_id, byte_offset, ColumnEncoding::Utf16CodeUnit)?;
        let line = u32::try_from(position.line() - 1).map_err(|_| {
            SourceError::InvalidSourceMapPosition {
                line: u32::MAX,
                column: 0,
            }
        })?;
        let column = u32::try_from(position.column() - 1).map_err(|_| {
            SourceError::InvalidSourceMapPosition {
                line,
                column: u32::MAX,
            }
        })?;
        Ok(SourceMapPosition::new(line, column))
    }

    /// Converts a zero-based source-map v3 line and UTF-16 column to a
    /// registered UTF-8 byte offset.
    ///
    /// # Errors
    ///
    /// Returns an error for a foreign source ID, an out-of-range position, or
    /// a UTF-16 column that splits a surrogate pair.
    pub fn byte_offset_for_source_map_position(
        &self,
        source_id: &SourceId,
        position: SourceMapPosition,
    ) -> Result<usize, SourceError> {
        let source = self.source(source_id)?;
        source.byte_offset_for_source_map_position(position.line, position.column)
    }

    /// Resolves a generated position through at most 32 incoming source maps.
    ///
    /// A source with no map, a missing token, an unmapped segment, or an
    /// unregistered original source is a successful terminal result. Exact
    /// display-name matching is used for additional registered hops.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid generated/mapped positions, malformed map
    /// state, cycles, or excessive chain depth.
    pub fn resolve_original(
        &self,
        generated_source: &SourceId,
        generated_position: SourceMapPosition,
    ) -> Result<ResolvedLocation, SourceMapError> {
        self.resolve_original_with_limit(
            generated_source,
            generated_position,
            DEFAULT_MAX_CHAIN_DEPTH,
        )
    }

    /// Resolves a generated position with an explicit maximum number of map
    /// hops.
    ///
    /// # Errors
    ///
    /// Has the same failure modes as [`Self::resolve_original`], and reports
    /// [`SourceMapErrorKind::ChainDepthExceeded`] before performing more than
    /// `max_depth` successful hops.
    pub fn resolve_original_with_limit(
        &self,
        generated_source: &SourceId,
        generated_position: SourceMapPosition,
        max_depth: usize,
    ) -> Result<ResolvedLocation, SourceMapError> {
        let generated_file = self
            .source(generated_source)
            .map_err(SourceMapError::from_source)?;
        generated_file
            .validate_source_map_position(generated_position.line, generated_position.column)
            .map_err(SourceMapError::from_source)?;

        let mut current_id = generated_source.clone();
        let mut current_position = generated_position;
        let mut original = OriginalLocation {
            source_name: generated_file.display_name().to_owned(),
            source_id: Some(generated_source.clone()),
            position: generated_position,
            embedded_source: None,
        };
        let mut name = None;
        let mut hops = 0;
        let mut visited = HashSet::new();

        loop {
            let current_file = self
                .source(&current_id)
                .map_err(SourceMapError::from_source)?;
            current_file
                .validate_source_map_position(current_position.line, current_position.column)
                .map_err(SourceMapError::from_source)?;
            let Some(source_map) = current_file.incoming_source_map() else {
                break;
            };
            if !visited.insert((current_id.clone(), current_position)) {
                return Err(SourceMapError::new(
                    SourceMapErrorKind::ChainCycle,
                    format!(
                        "source-map chain cycles at {}:{}:{}",
                        current_file.display_name(),
                        current_position.line,
                        current_position.column
                    ),
                ));
            }
            let Some(mapping) = source_map.lookup(current_position)? else {
                break;
            };
            if hops == max_depth {
                return Err(SourceMapError::new(
                    SourceMapErrorKind::ChainDepthExceeded,
                    format!("source-map chain exceeds the configured depth of {max_depth}"),
                ));
            }
            hops += 1;
            if let Some(mapped_name) = &mapping.name {
                name = Some(mapped_name.clone());
            }
            let next_id = self.source_id_by_name(&mapping.source_name);
            original = OriginalLocation {
                source_name: mapping.source_name,
                source_id: next_id.clone(),
                position: mapping.original,
                embedded_source: mapping.source_content,
            };
            let Some(next_id) = next_id else {
                break;
            };
            current_id = next_id;
            current_position = original.position;
        }

        Ok(ResolvedLocation {
            generated_source: generated_source.clone(),
            generated_position,
            original,
            name,
            hops,
        })
    }

    /// Resolves both endpoints of a validated generated span through at most
    /// 32 incoming source maps.
    ///
    /// If both endpoints reach the same registered original source in order,
    /// the result exposes a mapped [`SourceSpan`]. URL-only, embedded-only,
    /// differently sourced, or non-monotonic endpoints preserve the generated
    /// span as the rendering fallback without discarding the mapped start
    /// location.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid registry provenance, source-map positions,
    /// malformed map state, cycles, or excessive chain depth.
    pub fn resolve_span(&self, span: &SourceSpan) -> Result<ResolvedSpan, SourceMapError> {
        self.resolve_span_with_limit(span, DEFAULT_MAX_CHAIN_DEPTH)
    }

    /// Resolves both endpoints of a generated span with an explicit maximum
    /// number of successful map hops per endpoint.
    ///
    /// # Errors
    ///
    /// Has the same failure modes as [`Self::resolve_span`].
    pub fn resolve_span_with_limit(
        &self,
        span: &SourceSpan,
        max_depth: usize,
    ) -> Result<ResolvedSpan, SourceMapError> {
        let generated_span = self
            .span(
                span.source_id(),
                span.bytes().start() as usize,
                span.bytes().end() as usize,
            )
            .map_err(SourceMapError::from_source)?;
        let start_position = self
            .source_map_position(span.source_id(), span.bytes().start() as usize)
            .map_err(SourceMapError::from_source)?;
        let location =
            self.resolve_original_with_limit(span.source_id(), start_position, max_depth)?;

        let mapped_span = if location.is_mapped() {
            if let Some(mapped_source) = location.original().source_id() {
                let start = self
                    .byte_offset_for_source_map_position(
                        mapped_source,
                        location.original().position(),
                    )
                    .map_err(SourceMapError::from_source)?;
                let end_position = self
                    .source_map_position(span.source_id(), span.bytes().end() as usize)
                    .map_err(SourceMapError::from_source)?;
                let end_location =
                    self.resolve_original_with_limit(span.source_id(), end_position, max_depth)?;
                let end = if end_location.original().source_id() == Some(mapped_source) {
                    self.byte_offset_for_source_map_position(
                        mapped_source,
                        end_location.original().position(),
                    )
                    .map_err(SourceMapError::from_source)?
                    .max(start)
                } else {
                    start
                };
                Some(
                    self.span(mapped_source, start, end)
                        .map_err(SourceMapError::from_source)?,
                )
            } else {
                None
            }
        } else {
            None
        };

        Ok(ResolvedSpan {
            generated_span,
            mapped_span,
            location,
        })
    }
}

/// Stable categories for [`SourceMapError`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum SourceMapErrorKind {
    /// Input exceeded the caller's resource limit.
    InputTooLarge,
    /// The bytes were not valid JSON.
    InvalidJson,
    /// The document was JSON but not a well-formed source-map shape.
    InvalidDocument,
    /// The document did not declare source-map version 3.
    UnsupportedVersion,
    /// The document selected a nonstandard extension format.
    UnsupportedFormat,
    /// The encoded mapping data was invalid.
    MalformedMappings,
    /// The dependency decoder unexpectedly unwound.
    DecoderPanicked,
    /// Lookup in an already decoded map unexpectedly unwound.
    LookupPanicked,
    /// An indexed map selected an external URL-only section that the host has
    /// not loaded.
    UnresolvedSection,
    /// A generated or mapped position was invalid for registered source text.
    InvalidPosition,
    /// Registered incoming maps formed a cycle.
    ChainCycle,
    /// Registered incoming maps exceeded a configured hop limit.
    ChainDepthExceeded,
}

impl SourceMapErrorKind {
    /// Returns a stable diagnostic code suitable for logs and tooling.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InputTooLarge => "fusor::sourcemap::input_too_large",
            Self::InvalidJson => "fusor::sourcemap::invalid_json",
            Self::InvalidDocument => "fusor::sourcemap::invalid_document",
            Self::UnsupportedVersion => "fusor::sourcemap::unsupported_version",
            Self::UnsupportedFormat => "fusor::sourcemap::unsupported_format",
            Self::MalformedMappings => "fusor::sourcemap::malformed_mappings",
            Self::DecoderPanicked => "fusor::sourcemap::decoder_panicked",
            Self::LookupPanicked => "fusor::sourcemap::lookup_panicked",
            Self::UnresolvedSection => "fusor::sourcemap::unresolved_section",
            Self::InvalidPosition => "fusor::sourcemap::invalid_position",
            Self::ChainCycle => "fusor::sourcemap::chain_cycle",
            Self::ChainDepthExceeded => "fusor::sourcemap::chain_depth_exceeded",
        }
    }
}

/// A structured, non-panicking source-map load/lookup/resolution failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceMapError {
    kind: SourceMapErrorKind,
    message: String,
    source: Option<SourceError>,
}

impl SourceMapError {
    fn new(kind: SourceMapErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    fn from_source(source: SourceError) -> Self {
        Self {
            kind: SourceMapErrorKind::InvalidPosition,
            message: format!("cannot resolve source-map position: {source}"),
            source: Some(source),
        }
    }

    /// Returns the stable error category.
    #[must_use]
    pub const fn kind(&self) -> SourceMapErrorKind {
        self.kind
    }

    /// Returns the stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// Returns the complete human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SourceMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SourceMapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
