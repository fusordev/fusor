use std::{
    collections::HashSet,
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use serde_json::Value;
use sourcemap::DecodedMap;

use crate::{SourceError, SourceId, SourceRegistry};

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
            Self::InputTooLarge => "quickjs::sourcemap::input_too_large",
            Self::InvalidJson => "quickjs::sourcemap::invalid_json",
            Self::InvalidDocument => "quickjs::sourcemap::invalid_document",
            Self::UnsupportedVersion => "quickjs::sourcemap::unsupported_version",
            Self::UnsupportedFormat => "quickjs::sourcemap::unsupported_format",
            Self::MalformedMappings => "quickjs::sourcemap::malformed_mappings",
            Self::DecoderPanicked => "quickjs::sourcemap::decoder_panicked",
            Self::LookupPanicked => "quickjs::sourcemap::lookup_panicked",
            Self::UnresolvedSection => "quickjs::sourcemap::unresolved_section",
            Self::InvalidPosition => "quickjs::sourcemap::invalid_position",
            Self::ChainCycle => "quickjs::sourcemap::chain_cycle",
            Self::ChainDepthExceeded => "quickjs::sourcemap::chain_depth_exceeded",
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
