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
