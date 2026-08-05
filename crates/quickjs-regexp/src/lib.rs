//! Safe, resource-bounded ECMAScript regular-expression compilation and execution.

mod compiler;
mod emoji;
mod executor;
mod flags;
mod program;
mod properties;

use std::{fmt, ops::Range};

use program::Program;

/// Limits applied before and while compiling one pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompileLimits {
    /// Maximum UTF-8 byte length accepted from the parser-facing source.
    pub max_pattern_bytes: usize,
    /// Maximum number of owned executor instructions.
    pub max_instructions: usize,
    /// Maximum number of capturing groups, excluding the whole match.
    pub max_captures: usize,
    /// Maximum group/quantifier nesting represented by the compiled program.
    pub max_nesting: usize,
}

impl Default for CompileLimits {
    fn default() -> Self {
        Self {
            max_pattern_bytes: 1 << 20,
            max_instructions: 1 << 20,
            max_captures: 254,
            max_nesting: 256,
        }
    }
}

/// Deterministic limits applied to one execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecLimits {
    /// Maximum executor transitions, including candidate-start attempts.
    pub max_steps: u64,
    /// Maximum simultaneously retained backtracking states.
    pub max_backtrack_states: usize,
}

impl Default for ExecLimits {
    fn default() -> Self {
        Self {
            max_steps: 10_000_000,
            max_backtrack_states: 1 << 20,
        }
    }
}

/// One successful match, with UTF-16 code-unit ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Whole-match and capturing-group ranges. Missing groups are `None`.
    pub captures: Vec<Option<Range<usize>>>,
}

impl Match {
    /// Returns the whole-match range.
    ///
    /// # Panics
    ///
    /// Panics if public capture storage was replaced with an invalid value that
    /// omits capture zero. Executor-produced matches always contain it.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.captures[0]
            .clone()
            .expect("a successful match always has capture zero")
    }
}

/// Pattern compilation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The source flags are invalid or duplicated.
    InvalidFlags,
    /// The ECMAScript pattern grammar rejected the source.
    Syntax(String),
    /// The pattern exceeded a configured structural limit.
    ResourceLimit(&'static str),
    /// The grammar is valid but its executor instruction is not implemented yet.
    Unsupported(&'static str),
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFlags => formatter.write_str("invalid regular expression flags"),
            Self::Syntax(message) => write!(formatter, "invalid regular expression: {message}"),
            Self::ResourceLimit(resource) => {
                write!(formatter, "regular expression {resource} limit exceeded")
            }
            Self::Unsupported(feature) => {
                write!(
                    formatter,
                    "regular expression feature is not executable: {feature}"
                )
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Execution failure distinct from an ordinary non-match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// The deterministic transition budget was exhausted.
    StepLimit,
    /// The retained backtracking-state limit was exhausted.
    BacktrackLimit,
}

impl fmt::Display for ExecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StepLimit => formatter.write_str("regular expression step limit exceeded"),
            Self::BacktrackLimit => {
                formatter.write_str("regular expression backtracking limit exceeded")
            }
        }
    }
}

impl std::error::Error for ExecError {}

/// Owned executable regular expression.
#[derive(Clone, Debug)]
pub struct CompiledRegExp {
    flags: String,
    program: Program,
}

impl CompiledRegExp {
    /// Parses and compiles an ECMAScript pattern literal body and flags.
    ///
    /// # Errors
    ///
    /// Returns an exact flag, grammar, unsupported-feature, or resource-limit
    /// failure before producing an executable matcher.
    pub fn compile(
        pattern: &str,
        flags: &str,
        limits: CompileLimits,
    ) -> Result<Self, CompileError> {
        let (program, flags) = compiler::compile(pattern, flags, limits)?;
        Ok(Self { flags, program })
    }

    /// Parses and compiles exact ECMAScript UTF-16 pattern and flag strings.
    ///
    /// This entry point preserves lone surrogates without using lossy Unicode
    /// conversion. It is intended for the `RegExp` constructor, whose inputs
    /// are JavaScript strings rather than source-code scalar text.
    ///
    /// # Errors
    ///
    /// Returns an exact flag, grammar, unsupported-feature, allocation, or
    /// resource-limit failure before producing an executable matcher.
    pub fn compile_utf16(
        pattern: &[u16],
        flags: &[u16],
        limits: CompileLimits,
    ) -> Result<Self, CompileError> {
        let flags = ascii_flags(flags)?;
        let parsed_flags = flags::RegExpFlags::parse(&flags)?;
        let pattern = parser_pattern_source(pattern, parsed_flags.unicode_mode(), limits)?;
        Self::compile(&pattern, &flags, limits)
    }

    /// Returns flags in the canonical ECMAScript accessor order.
    #[must_use]
    pub fn flags(&self) -> &str {
        &self.flags
    }

    /// Executes against exact UTF-16 code units from `start_index`.
    ///
    /// # Errors
    ///
    /// Returns [`ExecError`] when the deterministic transition or retained
    /// backtracking-state budget is exhausted.
    pub fn execute(
        &self,
        input: &[u16],
        start_index: usize,
        limits: ExecLimits,
    ) -> Result<Option<Match>, ExecError> {
        executor::execute(&self.program, input, start_index, limits)
    }
}

fn ascii_flags(flags: &[u16]) -> Result<String, CompileError> {
    let mut source = String::new();
    source
        .try_reserve_exact(flags.len())
        .map_err(|_| CompileError::ResourceLimit("flag source allocation"))?;
    for &unit in flags {
        let unit = u8::try_from(unit).map_err(|_| CompileError::InvalidFlags)?;
        if !unit.is_ascii() {
            return Err(CompileError::InvalidFlags);
        }
        source.push(char::from(unit));
    }
    Ok(source)
}

fn parser_pattern_source(
    pattern: &[u16],
    unicode_mode: bool,
    limits: CompileLimits,
) -> Result<String, CompileError> {
    let output_bytes = parser_pattern_source_bytes(pattern, unicode_mode)?;
    if output_bytes > limits.max_pattern_bytes {
        return Err(CompileError::ResourceLimit("source length"));
    }
    let mut source = String::new();
    source
        .try_reserve_exact(output_bytes)
        .map_err(|_| CompileError::ResourceLimit("source allocation"))?;
    let mut index = 0;
    let mut slash_run = 0_usize;
    while index < pattern.len() {
        let first = pattern[index];
        if unicode_mode
            && (0xd800..=0xdbff).contains(&first)
            && let Some(&second) = pattern.get(index + 1)
            && (0xdc00..=0xdfff).contains(&second)
        {
            let high = u32::from(first) - 0xd800;
            let low = u32::from(second) - 0xdc00;
            source.push(
                char::from_u32(0x1_0000 + (high << 10) + low)
                    .expect("a validated surrogate pair is a Unicode scalar"),
            );
            slash_run = 0;
            index += 2;
            continue;
        }
        if (0xd800..=0xdfff).contains(&first) {
            if slash_run % 2 == 1 {
                if unicode_mode {
                    return Err(CompileError::Syntax(
                        "invalid identity escape in Unicode mode".to_owned(),
                    ));
                }
                let removed = source.pop();
                debug_assert_eq!(removed, Some('\\'));
            }
            push_unicode_escape(&mut source, first);
            slash_run = 0;
            index += 1;
            continue;
        }
        let character = char::from_u32(u32::from(first))
            .expect("a non-surrogate UTF-16 unit is a Unicode scalar");
        source.push(character);
        if character == '\\' {
            slash_run += 1;
        } else {
            slash_run = 0;
        }
        index += 1;
    }
    Ok(source)
}

fn parser_pattern_source_bytes(pattern: &[u16], unicode_mode: bool) -> Result<usize, CompileError> {
    let mut bytes = 0_usize;
    let mut index = 0;
    let mut slash_run = 0_usize;
    while index < pattern.len() {
        let first = pattern[index];
        let (additional, consumed, next_slash_run) = if unicode_mode
            && (0xd800..=0xdbff).contains(&first)
            && pattern
                .get(index + 1)
                .is_some_and(|second| (0xdc00..=0xdfff).contains(second))
        {
            (4, 2, 0)
        } else if (0xd800..=0xdfff).contains(&first) {
            if unicode_mode && slash_run % 2 == 1 {
                return Err(CompileError::Syntax(
                    "invalid identity escape in Unicode mode".to_owned(),
                ));
            }
            (if slash_run % 2 == 1 { 5 } else { 6 }, 1, 0)
        } else {
            let character = char::from_u32(u32::from(first))
                .expect("a non-surrogate UTF-16 unit is a Unicode scalar");
            (
                character.len_utf8(),
                1,
                if character == '\\' { slash_run + 1 } else { 0 },
            )
        };
        bytes = bytes
            .checked_add(additional)
            .ok_or(CompileError::ResourceLimit("source length"))?;
        slash_run = next_slash_run;
        index += consumed;
    }
    Ok(bytes)
}

fn push_unicode_escape(source: &mut String, unit: u16) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    source.push('\\');
    source.push('u');
    for shift in [12, 8, 4, 0] {
        let digit = usize::from((unit >> shift) & 0x0f);
        source.push(char::from(HEX[digit]));
    }
}
