//! Whole-graph verification for the currently supported compiler bytecode.
//!
//! Body verification proves one function's encoding, index domains, control
//! flow, and ordinary-value stack behavior. This module additionally owns and
//! validates the function-template constant graph and every imported closure
//! source before producing [`VerifiedCompilerFunctionGraph`].
//!
//! This certificate remains compiler-facing and is deliberately not execution
//! authority: runtime-visible names, binding policies, some value families,
//! and exception/debug metadata are not represented yet.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt,
    sync::Arc,
};

use crate::{
    AtomPoolIndex, BytecodePc, CompilerAtom, CompilerConstantKind, CompilerString, FinalOpcode,
    VerifiedControlFlow,
};

/// Provisional maximum number of compiler function templates in one graph.
pub const MAX_FUNCTION_GRAPH_TEMPLATES: u32 = 65_535;

/// Provisional maximum function-constant nesting depth.
pub const MAX_FUNCTION_GRAPH_NESTING_DEPTH: u32 = 256;

const DEFAULT_MAX_GRAPH_BYTECODE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_GRAPH_INSTRUCTIONS: u64 = 8_388_608;
const DEFAULT_MAX_GRAPH_CONSTANTS: u64 = 1_048_576;
const DEFAULT_MAX_GRAPH_ATOMS: u64 = 1_048_576;
const DEFAULT_MAX_GRAPH_STRING_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_GRAPH_CLOSURE_VARIABLES: u64 = 1_048_576;
const DEFAULT_MAX_GRAPH_CLOSURE_EDGE_EVALUATIONS: u64 = 33_554_432;
const DEFAULT_MAX_GRAPH_TRANSFER_EVALUATIONS: u64 = 33_554_432;

/// Exact binary64 payload for one compiler-owned Number constant.
///
/// Every non-NaN bit pattern is preserved, including signed zero,
/// subnormals, and infinities. Compiler-owned NaN encodings are normalized to
/// one deterministic quiet NaN. This is a compiler-artifact policy, not a
/// general runtime Number, `DataView`, or typed-array storage representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct Binary64Constant(u64);

impl Binary64Constant {
    /// Canonical quiet-NaN bits retained by compiler constant pools.
    pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

    /// Creates an exact constant from a binary64 bit pattern.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        if (bits & 0x7fff_ffff_ffff_ffff) > 0x7ff0_0000_0000_0000 {
            Self(Self::CANONICAL_NAN_BITS)
        } else {
            Self(bits)
        }
    }

    /// Creates an exact constant from a binary64 value.
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        Self::from_bits(value.to_bits())
    }

    /// Returns the retained canonical bit pattern.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// Reconstructs the retained binary64 value.
    #[must_use]
    pub const fn to_f64(self) -> f64 {
        f64::from_bits(self.0)
    }

    /// Formats the retained Number with the decimal/exponent spelling used by
    /// JavaScript `ToString` in the pinned `QuickJS` release.
    #[must_use]
    pub fn to_javascript_string(self) -> String {
        format_binary64_for_javascript(self.to_f64())
    }
}

/// Invalid canonical decimal payload for a compiler-owned `BigInt` constant.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerBigIntError {
    /// A `BigInt` constant must contain at least one digit.
    Empty,
    /// Only zero itself may begin with a zero digit.
    LeadingZero,
    /// A code unit was not an ASCII decimal digit.
    InvalidDigit {
        /// Zero-based code-unit position.
        index: usize,
        /// Rejected UTF-16 code unit.
        code_unit: u16,
    },
}

impl fmt::Display for CompilerBigIntError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("compiler BigInt decimal payload is empty"),
            Self::LeadingZero => {
                formatter.write_str("compiler BigInt decimal payload has a leading zero")
            }
            Self::InvalidDigit { index, code_unit } => write!(
                formatter,
                "compiler BigInt decimal payload has non-digit code unit {code_unit:#06x} at {index}"
            ),
        }
    }
}

impl Error for CompilerBigIntError {}

/// Exact non-negative decimal value of one compiler-owned `BigInt` literal.
///
/// Unary negation remains bytecode, so constant payloads never carry a sign.
/// The private validated representation prevents runtime installation from
/// accepting an arbitrary or non-canonical numeric string.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompilerBigInt {
    decimal: CompilerString,
}

impl CompilerBigInt {
    /// Validates a canonical unsigned decimal spelling.
    ///
    /// # Errors
    ///
    /// Rejects empty, signed, non-decimal, or leading-zero spellings.
    pub fn try_from_decimal(decimal: CompilerString) -> Result<Self, CompilerBigIntError> {
        let mut units = decimal.code_units().enumerate();
        let Some((_, first)) = units.next() else {
            return Err(CompilerBigIntError::Empty);
        };
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&first) {
            return Err(CompilerBigIntError::InvalidDigit {
                index: 0,
                code_unit: first,
            });
        }
        if first == u16::from(b'0') && decimal.len() != 1 {
            return Err(CompilerBigIntError::LeadingZero);
        }
        for (index, code_unit) in units {
            if !(u16::from(b'0')..=u16::from(b'9')).contains(&code_unit) {
                return Err(CompilerBigIntError::InvalidDigit { index, code_unit });
            }
        }
        Ok(Self { decimal })
    }

    /// Returns the canonical unsigned decimal spelling.
    #[must_use]
    pub const fn decimal(&self) -> &CompilerString {
        &self.decimal
    }

    /// Returns the retained compact decimal payload size.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.decimal.payload_bytes()
    }
}

/// Invalid compiler-owned tagged-template site payload.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompilerTemplateObjectError {
    /// Every template literal has at least one template element.
    Empty,
    /// An Array length cannot represent the template element count.
    TooManyElements {
        /// Rejected template element count.
        observed: usize,
    },
}

impl fmt::Display for CompilerTemplateObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("compiler template object has no elements"),
            Self::TooManyElements { observed } => write!(
                formatter,
                "compiler template object has {observed} elements, exceeding the Array length domain"
            ),
        }
    }
}

impl Error for CompilerTemplateObjectError {}

/// One cooked/raw pair retained for a tagged-template parse node.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompilerTemplateElement {
    cooked: Option<CompilerString>,
    raw: CompilerString,
}

impl CompilerTemplateElement {
    /// Creates one exact template element.
    ///
    /// `cooked` is absent only when the tagged source contains an invalid
    /// escape sequence; `raw` always preserves the template source value.
    #[must_use]
    pub const fn new(cooked: Option<CompilerString>, raw: CompilerString) -> Self {
        Self { cooked, raw }
    }

    /// Returns the cooked value, or `None` for an invalid escape sequence.
    #[must_use]
    pub const fn cooked(&self) -> Option<&CompilerString> {
        self.cooked.as_ref()
    }

    /// Returns the exact raw template value.
    #[must_use]
    pub const fn raw(&self) -> &CompilerString {
        &self.raw
    }

    fn payload_bytes(&self) -> u64 {
        let cooked = self
            .cooked
            .as_ref()
            .map_or(0, |value| usize_to_u64(value.payload_bytes()));
        cooked.saturating_add(usize_to_u64(self.raw.payload_bytes()))
    }
}

/// Exact immutable payload of one tagged-template parse node.
///
/// The runtime uses this value constant as the site identity and lazily
/// materializes its realm-local frozen cooked and raw Arrays exactly once.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompilerTemplateObject {
    elements: Arc<[CompilerTemplateElement]>,
}

impl CompilerTemplateObject {
    /// Validates and owns a nonempty template element list.
    ///
    /// # Errors
    ///
    /// Rejects an empty list or a count outside the ECMAScript Array length
    /// domain used by `GetTemplateObject`.
    pub fn try_from_elements(
        elements: Arc<[CompilerTemplateElement]>,
    ) -> Result<Self, CompilerTemplateObjectError> {
        if elements.is_empty() {
            return Err(CompilerTemplateObjectError::Empty);
        }
        if u32::try_from(elements.len()).is_err() {
            return Err(CompilerTemplateObjectError::TooManyElements {
                observed: elements.len(),
            });
        }
        Ok(Self { elements })
    }

    /// Returns the source-ordered cooked/raw template elements.
    #[must_use]
    pub fn elements(&self) -> &[CompilerTemplateElement] {
        &self.elements
    }

    /// Returns all retained cooked and raw compact text payload bytes.
    #[must_use]
    pub fn payload_bytes(&self) -> u64 {
        self.elements.iter().fold(0_u64, |total, element| {
            total.saturating_add(element.payload_bytes())
        })
    }
}

fn format_binary64_for_javascript(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }

    let rendered = value.to_string();
    let (negative, unsigned) = rendered
        .strip_prefix('-')
        .map_or((false, rendered.as_str()), |unsigned| (true, unsigned));
    let (mantissa, explicit_exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, 0), |(mantissa, exponent)| {
            (mantissa, parse_decimal_exponent(exponent))
        });
    let decimal_position = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits = mantissa
        .bytes()
        .filter(|byte| *byte != b'.')
        .map(char::from)
        .collect::<String>();
    let first_significant = digits.bytes().position(|byte| byte != b'0').unwrap_or(0);
    let scientific_exponent = explicit_exponent
        .saturating_add(i32::try_from(decimal_position).unwrap_or(i32::MAX))
        .saturating_sub(i32::try_from(first_significant).unwrap_or(i32::MAX))
        .saturating_sub(1);
    digits.drain(..first_significant);
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }

    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if !(-6..21).contains(&scientific_exponent) {
        let mut characters = digits.chars();
        if let Some(first) = characters.next() {
            output.push(first);
        }
        let remainder = characters.as_str();
        if !remainder.is_empty() {
            output.push('.');
            output.push_str(remainder);
        }
        output.push('e');
        if scientific_exponent >= 0 {
            output.push('+');
        }
        output.push_str(&scientific_exponent.to_string());
        return output;
    }

    if scientific_exponent < 0 {
        output.push_str("0.");
        let zeros = usize::try_from(-scientific_exponent - 1).unwrap_or(0);
        output.extend(std::iter::repeat_n('0', zeros));
        output.push_str(&digits);
        return output;
    }

    let integer_digits = usize::try_from(scientific_exponent + 1).unwrap_or(usize::MAX);
    if integer_digits >= digits.len() {
        output.push_str(&digits);
        output.extend(std::iter::repeat_n('0', integer_digits - digits.len()));
    } else {
        output.push_str(&digits[..integer_digits]);
        output.push('.');
        output.push_str(&digits[integer_digits..]);
    }
    output
}

fn parse_decimal_exponent(value: &str) -> i32 {
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |digits| (true, digits));
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    let exponent = digits.bytes().fold(0_i32, |exponent, digit| {
        exponent
            .saturating_mul(10)
            .saturating_add(i32::from(digit.saturating_sub(b'0')))
    });
    if negative { -exponent } else { exponent }
}

/// One ordinary compiler-owned constant value.
///
/// This enum keeps the value namespace extensible without weakening the
/// function/value distinction already certified by body verification.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CompilerConstantValue {
    /// An ECMAScript Number represented by exact binary64 bits.
    Number(Binary64Constant),
    /// An ECMAScript String represented by exact UTF-16 code units.
    String(CompilerString),
    /// An ECMAScript `BigInt` represented by canonical unsigned decimal digits.
    BigInt(CompilerBigInt),
    /// One tagged-template site and its exact cooked/raw element values.
    TemplateObject(CompilerTemplateObject),
}

/// Dense identity of one function template in a compiler graph.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct FunctionTemplateId(u32);

impl FunctionTemplateId {
    /// Creates an unverified dense template identity.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the numeric graph-local index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    fn index(self) -> Option<usize> {
        usize::try_from(self.0).ok()
    }
}

impl fmt::Display for FunctionTemplateId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One owned entry in a compiler function's heterogeneous constant pool.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CompilerConstant {
    /// An ordinary JavaScript value.
    Value(CompilerConstantValue),
    /// A nested bytecode-function template.
    Function(FunctionTemplateId),
}

impl CompilerConstant {
    /// Returns the body-verifier kind represented by this owned payload.
    #[must_use]
    pub const fn kind(&self) -> CompilerConstantKind {
        match self {
            Self::Value(_) => CompilerConstantKind::Value,
            Self::Function(_) => CompilerConstantKind::Function,
        }
    }
}

/// One normalized source for a function's closure-domain slot.
///
/// Parent-owned cells and the parent's imported environment remain distinct.
/// A constructor-realm global source is permitted only on the graph root; a
/// descendant forwards that realm-owned slot through [`Self::ParentClosure`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CompilerClosureSource {
    /// Dense variable-reference cell owned by the parent activation.
    ParentVariableReference(u32),
    /// Dense imported-closure slot on the parent function object.
    ParentClosure(u32),
    /// Function-local atom naming either an unresolved lookup or a
    /// configurable indirect-eval `var` in the constructor realm.
    ConstructorRealmGlobal(AtomPoolIndex),
    /// One live binding supplied by the calling activation of direct `eval`.
    ///
    /// `index` addresses the ordered external environment snapshot while
    /// `environment_size` binds the authority to the exact snapshot shape.
    DirectEvalBinding {
        /// Zero-based entry in the caller-environment snapshot.
        index: u32,
        /// Exact number of entries required from that snapshot.
        environment_size: u32,
    },
}

impl fmt::Display for CompilerClosureSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParentVariableReference(index) => {
                write!(formatter, "parent variable-reference {index}")
            }
            Self::ParentClosure(index) => write!(formatter, "parent closure {index}"),
            Self::ConstructorRealmGlobal(atom) => {
                write!(formatter, "constructor-realm global atom {}", atom.get())
            }
            Self::DirectEvalBinding {
                index,
                environment_size,
            } => write!(
                formatter,
                "direct-eval binding {index} in environment of size {environment_size}"
            ),
        }
    }
}

/// One function template whose cross-function metadata is not yet verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedCompilerFunction {
    control_flow: Arc<VerifiedControlFlow>,
    atoms: Option<Arc<[CompilerAtom]>>,
    constants: Arc<[CompilerConstant]>,
    closure_sources: Arc<[CompilerClosureSource]>,
}

impl UnverifiedCompilerFunction {
    /// Creates one unverified compiler function-template record.
    #[must_use]
    pub const fn new(
        control_flow: Arc<VerifiedControlFlow>,
        constants: Arc<[CompilerConstant]>,
        closure_sources: Arc<[CompilerClosureSource]>,
    ) -> Self {
        Self {
            control_flow,
            atoms: None,
            constants,
            closure_sources,
        }
    }

    /// Installs the exact function-local atom payloads.
    #[must_use]
    pub fn with_atom_pool(mut self, atoms: Arc<[CompilerAtom]>) -> Self {
        self.atoms = Some(atoms);
        self
    }

    /// Returns the independently verified body certificate.
    #[must_use]
    pub fn control_flow(&self) -> &Arc<VerifiedControlFlow> {
        &self.control_flow
    }

    /// Returns owned atoms in function-local pool order.
    #[must_use]
    pub fn atoms(&self) -> Option<&[CompilerAtom]> {
        self.atoms.as_deref()
    }

    /// Returns owned constants in constant-pool order.
    #[must_use]
    pub fn constants(&self) -> &[CompilerConstant] {
        &self.constants
    }

    /// Returns imported closure sources in child slot order.
    #[must_use]
    pub fn closure_sources(&self) -> &[CompilerClosureSource] {
        &self.closure_sources
    }
}

/// Flat compiler function graph awaiting cross-function verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnverifiedCompilerFunctionGraph {
    root: FunctionTemplateId,
    functions: Arc<[UnverifiedCompilerFunction]>,
}

impl UnverifiedCompilerFunctionGraph {
    /// Creates an unverified flat graph.
    #[must_use]
    pub const fn new(
        root: FunctionTemplateId,
        functions: Arc<[UnverifiedCompilerFunction]>,
    ) -> Self {
        Self { root, functions }
    }

    /// Returns the proposed root identity.
    #[must_use]
    pub const fn root(&self) -> FunctionTemplateId {
        self.root
    }

    /// Returns every proposed function record in dense identity order.
    #[must_use]
    pub fn functions(&self) -> &[UnverifiedCompilerFunction] {
        &self.functions
    }
}

/// Aggregate resource governed by whole-function graph verification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FunctionGraphResource {
    /// Distinct function-template records.
    Functions,
    /// Longest root-to-function constant path.
    NestingDepth,
    /// Encoded bytecode bytes across all functions.
    BytecodeBytes,
    /// Decoded instructions across all functions.
    Instructions,
    /// Constant-pool slots across all functions.
    Constants,
    /// Content-interned atom slots across all functions.
    Atoms,
    /// Compact text payload bytes retained by Strings, atoms, and `BigInt` values.
    StringPayloadBytes,
    /// Imported closure-variable slots across all functions.
    ClosureVariables,
    /// Child closure sources checked across every parent edge.
    ClosureEdgeEvaluations,
    /// Reachable abstract transfer evaluations across all functions.
    TransferEvaluations,
    /// Temporary entries used to validate graph topology.
    TopologyEntries,
    /// Temporary entries used to validate closure-source uniqueness.
    ClosureSourceEntries,
    /// Temporary entries used to validate atom uniqueness.
    AtomDedupEntries,
    /// Frozen verified function records.
    VerifiedFunctions,
}

impl fmt::Display for FunctionGraphResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Functions => "function templates",
            Self::NestingDepth => "function nesting depth",
            Self::BytecodeBytes => "graph bytecode bytes",
            Self::Instructions => "graph instructions",
            Self::Constants => "graph constants",
            Self::Atoms => "graph atoms",
            Self::StringPayloadBytes => "graph text payload bytes",
            Self::ClosureVariables => "graph closure variables",
            Self::ClosureEdgeEvaluations => "graph closure-edge evaluations",
            Self::TransferEvaluations => "graph transfer evaluations",
            Self::TopologyEntries => "graph topology entries",
            Self::ClosureSourceEntries => "closure-source validation entries",
            Self::AtomDedupEntries => "atom-pool validation entries",
            Self::VerifiedFunctions => "verified function records",
        })
    }
}

/// Explicit aggregate limits for compiler function-graph verification.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FunctionGraphVerificationLimits {
    max_functions: u32,
    max_nesting_depth: u32,
    max_bytecode_bytes: u64,
    max_instructions: u64,
    max_constants: u64,
    max_atoms: u64,
    max_string_payload_bytes: u64,
    max_closure_variables: u64,
    max_closure_edge_evaluations: u64,
    max_transfer_evaluations: u64,
}

impl FunctionGraphVerificationLimits {
    /// Creates an explicit graph verification profile.
    ///
    /// The initial closure-edge work limit equals
    /// `max_transfer_evaluations`; callers may tune it independently with
    /// [`Self::with_max_closure_edge_evaluations`].
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "every independent untrusted graph budget is explicit"
    )]
    pub const fn new(
        max_functions: u32,
        max_nesting_depth: u32,
        max_bytecode_bytes: u64,
        max_instructions: u64,
        max_constants: u64,
        max_atoms: u64,
        max_string_payload_bytes: u64,
        max_closure_variables: u64,
        max_transfer_evaluations: u64,
    ) -> Self {
        Self {
            max_functions,
            max_nesting_depth,
            max_bytecode_bytes,
            max_instructions,
            max_constants,
            max_atoms,
            max_string_payload_bytes,
            max_closure_variables,
            max_closure_edge_evaluations: max_transfer_evaluations,
            max_transfer_evaluations,
        }
    }

    /// Returns a copy with a different function-count maximum.
    #[must_use]
    pub const fn with_max_functions(mut self, maximum: u32) -> Self {
        self.max_functions = maximum;
        self
    }

    /// Returns a copy with a different nesting-depth maximum.
    #[must_use]
    pub const fn with_max_nesting_depth(mut self, maximum: u32) -> Self {
        self.max_nesting_depth = maximum;
        self
    }

    /// Returns a copy with a different aggregate bytecode-byte maximum.
    #[must_use]
    pub const fn with_max_bytecode_bytes(mut self, maximum: u64) -> Self {
        self.max_bytecode_bytes = maximum;
        self
    }

    /// Returns a copy with a different aggregate instruction maximum.
    #[must_use]
    pub const fn with_max_instructions(mut self, maximum: u64) -> Self {
        self.max_instructions = maximum;
        self
    }

    /// Returns a copy with a different aggregate constant maximum.
    #[must_use]
    pub const fn with_max_constants(mut self, maximum: u64) -> Self {
        self.max_constants = maximum;
        self
    }

    /// Returns a copy with a different aggregate atom maximum.
    #[must_use]
    pub const fn with_max_atoms(mut self, maximum: u64) -> Self {
        self.max_atoms = maximum;
        self
    }

    /// Returns a copy with a different aggregate compact text-payload maximum in bytes.
    #[must_use]
    pub const fn with_max_string_payload_bytes(mut self, maximum: u64) -> Self {
        self.max_string_payload_bytes = maximum;
        self
    }

    /// Returns a copy with a different aggregate closure-variable maximum.
    #[must_use]
    pub const fn with_max_closure_variables(mut self, maximum: u64) -> Self {
        self.max_closure_variables = maximum;
        self
    }

    /// Returns a copy with a different closure-edge work maximum.
    #[must_use]
    pub const fn with_max_closure_edge_evaluations(mut self, maximum: u64) -> Self {
        self.max_closure_edge_evaluations = maximum;
        self
    }

    /// Returns a copy with a different aggregate transfer-work maximum.
    #[must_use]
    pub const fn with_max_transfer_evaluations(mut self, maximum: u64) -> Self {
        self.max_transfer_evaluations = maximum;
        self
    }

    /// Returns the function-count maximum.
    #[must_use]
    pub const fn max_functions(self) -> u32 {
        self.max_functions
    }

    /// Returns the nesting-depth maximum.
    #[must_use]
    pub const fn max_nesting_depth(self) -> u32 {
        self.max_nesting_depth
    }

    /// Returns the aggregate bytecode-byte maximum.
    #[must_use]
    pub const fn max_bytecode_bytes(self) -> u64 {
        self.max_bytecode_bytes
    }

    /// Returns the aggregate instruction maximum.
    #[must_use]
    pub const fn max_instructions(self) -> u64 {
        self.max_instructions
    }

    /// Returns the aggregate constant maximum.
    #[must_use]
    pub const fn max_constants(self) -> u64 {
        self.max_constants
    }

    /// Returns the aggregate atom maximum.
    #[must_use]
    pub const fn max_atoms(self) -> u64 {
        self.max_atoms
    }

    /// Returns the aggregate retained compact text-payload maximum in bytes.
    #[must_use]
    pub const fn max_string_payload_bytes(self) -> u64 {
        self.max_string_payload_bytes
    }

    /// Returns the aggregate closure-variable maximum.
    #[must_use]
    pub const fn max_closure_variables(self) -> u64 {
        self.max_closure_variables
    }

    /// Returns the closure-edge work maximum.
    #[must_use]
    pub const fn max_closure_edge_evaluations(self) -> u64 {
        self.max_closure_edge_evaluations
    }

    /// Returns the aggregate transfer-work maximum.
    #[must_use]
    pub const fn max_transfer_evaluations(self) -> u64 {
        self.max_transfer_evaluations
    }
}

impl Default for FunctionGraphVerificationLimits {
    fn default() -> Self {
        Self::new(
            MAX_FUNCTION_GRAPH_TEMPLATES,
            MAX_FUNCTION_GRAPH_NESTING_DEPTH,
            DEFAULT_MAX_GRAPH_BYTECODE_BYTES,
            DEFAULT_MAX_GRAPH_INSTRUCTIONS,
            DEFAULT_MAX_GRAPH_CONSTANTS,
            DEFAULT_MAX_GRAPH_ATOMS,
            DEFAULT_MAX_GRAPH_STRING_PAYLOAD_BYTES,
            DEFAULT_MAX_GRAPH_CLOSURE_VARIABLES,
            DEFAULT_MAX_GRAPH_TRANSFER_EVALUATIONS,
        )
        .with_max_closure_edge_evaluations(DEFAULT_MAX_GRAPH_CLOSURE_EDGE_EVALUATIONS)
    }
}

/// Measured aggregate usage retained by [`VerifiedCompilerFunctionGraph`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct FunctionGraphUsage {
    functions: u64,
    bytecode_bytes: u64,
    instructions: u64,
    constants: u64,
    atoms: u64,
    string_payload_bytes: u64,
    closure_variables: u64,
    closure_edge_evaluations: u64,
    transfer_evaluations: u64,
}

impl FunctionGraphUsage {
    /// Returns the number of distinct function records.
    #[must_use]
    pub const fn functions(self) -> u64 {
        self.functions
    }

    /// Returns total encoded bytecode bytes.
    #[must_use]
    pub const fn bytecode_bytes(self) -> u64 {
        self.bytecode_bytes
    }

    /// Returns total decoded instructions.
    #[must_use]
    pub const fn instructions(self) -> u64 {
        self.instructions
    }

    /// Returns total constant slots.
    #[must_use]
    pub const fn constants(self) -> u64 {
        self.constants
    }

    /// Returns total content-interned atom slots.
    #[must_use]
    pub const fn atoms(self) -> u64 {
        self.atoms
    }

    /// Returns total compact payload bytes retained by strings, atoms, and `BigInt` values.
    #[must_use]
    pub const fn string_payload_bytes(self) -> u64 {
        self.string_payload_bytes
    }

    /// Returns total imported closure-variable slots.
    #[must_use]
    pub const fn closure_variables(self) -> u64 {
        self.closure_variables
    }

    /// Returns child capture checks charged across every parent edge.
    #[must_use]
    pub const fn closure_edge_evaluations(self) -> u64 {
        self.closure_edge_evaluations
    }

    /// Returns total reachable transfer evaluations.
    #[must_use]
    pub const fn transfer_evaluations(self) -> u64 {
        self.transfer_evaluations
    }
}

/// One fully cross-checked compiler function template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCompilerFunction {
    control_flow: Arc<VerifiedControlFlow>,
    atoms: Arc<[CompilerAtom]>,
    constants: Arc<[CompilerConstant]>,
    closure_sources: Arc<[CompilerClosureSource]>,
}

impl VerifiedCompilerFunction {
    /// Returns the verified body certificate.
    #[must_use]
    pub fn control_flow(&self) -> &VerifiedControlFlow {
        &self.control_flow
    }

    /// Returns verified content-interned atoms in pool order.
    #[must_use]
    pub fn atoms(&self) -> &[CompilerAtom] {
        &self.atoms
    }

    /// Returns verified heterogeneous constants in pool order.
    #[must_use]
    pub fn constants(&self) -> &[CompilerConstant] {
        &self.constants
    }

    /// Returns verified imported closure sources in child slot order.
    #[must_use]
    pub fn closure_sources(&self) -> &[CompilerClosureSource] {
        &self.closure_sources
    }
}

/// Immutable cross-function certificate for the supported compiler subset.
///
/// The graph is flat: child constants are dense identities rather than nested
/// owning pointers, while ordinary values retain immutable typed payloads.
/// Serialized bytecode and unsupported opcode capabilities cannot construct
/// this type. It does not yet authorize runtime execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedCompilerFunctionGraph {
    root: FunctionTemplateId,
    root_index: usize,
    functions: Arc<[VerifiedCompilerFunction]>,
    max_nesting_depth: u32,
    usage: FunctionGraphUsage,
}

impl VerifiedCompilerFunctionGraph {
    /// Returns the graph-local root identity.
    #[must_use]
    pub const fn root_id(&self) -> FunctionTemplateId {
        self.root
    }

    /// Returns the root function template.
    #[must_use]
    pub fn root(&self) -> &VerifiedCompilerFunction {
        &self.functions[self.root_index]
    }

    /// Returns every verified template in dense identity order.
    #[must_use]
    pub fn functions(&self) -> &[VerifiedCompilerFunction] {
        &self.functions
    }

    /// Resolves one graph-local function identity.
    #[must_use]
    pub fn function(&self, id: FunctionTemplateId) -> Option<&VerifiedCompilerFunction> {
        self.functions.get(id.index()?)
    }

    /// Returns the longest verified root-to-template path.
    #[must_use]
    pub const fn max_nesting_depth(&self) -> u32 {
        self.max_nesting_depth
    }

    /// Returns retained aggregate resource usage.
    #[must_use]
    pub const fn usage(&self) -> FunctionGraphUsage {
        self.usage
    }
}

/// Structured whole-function graph verification failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionGraphVerificationError {
    function: Option<FunctionTemplateId>,
    kind: FunctionGraphVerificationErrorKind,
}

impl FunctionGraphVerificationError {
    fn graph(kind: FunctionGraphVerificationErrorKind) -> Self {
        Self {
            function: None,
            kind,
        }
    }

    fn at_function(function: FunctionTemplateId, kind: FunctionGraphVerificationErrorKind) -> Self {
        Self {
            function: Some(function),
            kind,
        }
    }

    /// Returns the primary function identity when the failure is local.
    #[must_use]
    pub const fn function(&self) -> Option<FunctionTemplateId> {
        self.function
    }

    /// Returns the structured failure kind.
    #[must_use]
    pub const fn kind(&self) -> &FunctionGraphVerificationErrorKind {
        &self.kind
    }
}

impl fmt::Display for FunctionGraphVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(function) = self.function {
            write!(formatter, "function {function}: ")?;
        }
        self.kind.fmt(formatter)
    }
}

impl Error for FunctionGraphVerificationError {}

/// Exact reason a compiler function graph did not gain a cross-function
/// certificate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionGraphVerificationErrorKind {
    /// The graph has no function records.
    EmptyGraph,
    /// The proposed root does not name a supplied function.
    RootOutOfBounds {
        /// Rejected root identity.
        root: FunctionTemplateId,
        /// Number of supplied functions.
        functions: u64,
    },
    /// The selected root imports cells from an omitted external parent.
    RootRequiresEnvironment {
        /// Number of imported cells required by the root.
        closure_variables: u32,
    },
    /// An aggregate graph budget was exceeded.
    LimitExceeded {
        /// Exhausted resource.
        resource: FunctionGraphResource,
        /// Inclusive configured maximum.
        limit: u64,
        /// Observed value.
        observed: u64,
    },
    /// Exact aggregate resource accounting overflowed its encoded width.
    ResourceUsageOverflow {
        /// Resource whose exact usage did not fit in `u64`.
        resource: FunctionGraphResource,
    },
    /// Temporary verifier storage could not be allocated.
    AllocationFailed {
        /// Allocation purpose.
        resource: FunctionGraphResource,
        /// Requested entry count.
        requested: u64,
    },
    /// A body did not retain explicit compiler capture metadata.
    MissingCompilerCaptureLayout,
    /// A body did not retain explicit compiler constant metadata.
    MissingCompilerConstantLayout,
    /// A nonempty body atom domain has no supplied atom table.
    MissingAtomPool {
        /// Declared atom entries.
        declared: u32,
    },
    /// Actual atom-pool entries do not match the body domain.
    AtomCountMismatch {
        /// Body-declared atom count.
        declared: u32,
        /// Supplied atom entries.
        entries: u64,
    },
    /// A compiler function's atom pool contains the same string twice.
    DuplicateAtom {
        /// First atom-pool index containing the string.
        first: u32,
        /// Repeated atom-pool index.
        duplicate: u32,
    },
    /// A compiler function's atom pool contains an empty string.
    EmptyAtom {
        /// Rejected atom-pool index.
        index: u32,
    },
    /// A compiler function's atom pool contains a tagged-integer spelling.
    TaggedIntegerAtom {
        /// Rejected atom-pool index.
        index: u32,
    },
    /// A static-property-only atom contains an ordinary atom spelling.
    InvalidStaticPropertyOnlyAtom {
        /// Rejected atom-pool index.
        index: u32,
    },
    /// A static-property-only atom is referenced by another opcode family.
    StaticPropertyOnlyAtomOperand {
        /// Rejected atom-pool index.
        index: u32,
        /// Instruction carrying the invalid reference.
        pc: BytecodePc,
        /// Opcode carrying the invalid reference.
        opcode: FinalOpcode,
    },
    /// A constructor-realm global source references a static-property-only atom.
    StaticPropertyOnlyAtomClosureSource {
        /// Closure-domain slot containing the invalid source.
        closure: u32,
        /// Rejected atom-pool index.
        index: u32,
    },
    /// Actual constant-pool entries do not match the body domain.
    ConstantCountMismatch {
        /// Body-declared constant count.
        declared: u32,
        /// Supplied graph entries.
        entries: u64,
    },
    /// Actual closure sources do not match the body domain.
    ClosureVariableCountMismatch {
        /// Body-declared imported closure count.
        declared: u32,
        /// Supplied graph entries.
        entries: u64,
    },
    /// A constructor-realm global closure source names an atom outside the
    /// function-local pool.
    ClosureSourceAtomOutOfBounds {
        /// Closure-domain slot containing the source.
        closure: u32,
        /// Rejected function-local atom.
        atom: AtomPoolIndex,
        /// Function-local atom count.
        atoms: u32,
    },
    /// A non-root function tries to originate a constructor-realm global
    /// source instead of forwarding the root-owned slot.
    ConstructorRealmGlobalSourceNotRoot {
        /// Closure-domain slot containing the source.
        closure: u32,
    },
    /// A non-root function tries to originate a direct-eval caller binding
    /// instead of forwarding the root-owned slot.
    DirectEvalBindingSourceNotRoot {
        /// Closure-domain slot containing the source.
        closure: u32,
    },
    /// A direct-eval caller-binding source addresses outside its declared
    /// external environment.
    DirectEvalBindingOutOfBounds {
        /// Closure-domain slot containing the source.
        closure: u32,
        /// Rejected external-environment index.
        index: u32,
        /// Declared external-environment size.
        environment_size: u32,
    },
    /// Direct-eval caller-binding sources disagree about the external
    /// environment shape bound into the authority.
    DirectEvalEnvironmentSizeMismatch {
        /// First declared external-environment size.
        expected: u32,
        /// Closure-domain slot containing the disagreement.
        closure: u32,
        /// Conflicting external-environment size.
        actual: u32,
    },
    /// A compiler function imports the same immediate-parent cell twice.
    DuplicateClosureSource {
        /// First child closure slot using the source.
        first: u32,
        /// Repeated child closure slot.
        duplicate: u32,
        /// Repeated normalized source.
        source: CompilerClosureSource,
    },
    /// An owned constant payload does not match the body-declared kind.
    ConstantKindMismatch {
        /// Constant-pool index.
        index: u32,
        /// Kind retained by body verification.
        declared: CompilerConstantKind,
        /// Kind of the supplied owned payload.
        actual: CompilerConstantKind,
    },
    /// A function constant points outside the flat graph.
    FunctionConstantOutOfBounds {
        /// Constant-pool index.
        index: u32,
        /// Rejected target identity.
        target: FunctionTemplateId,
        /// Number of supplied functions.
        functions: u64,
    },
    /// A child capture recipe indexes outside one of its parent domains.
    ClosureSourceOutOfBounds {
        /// Child function template.
        child: FunctionTemplateId,
        /// Child closure-variable slot.
        closure: u32,
        /// Rejected normalized source.
        source: CompilerClosureSource,
        /// Parent-domain length.
        len: u32,
    },
    /// Function constants contain a directed cycle.
    Cycle {
        /// One function blocked when a cycle prevents topological processing.
        function: FunctionTemplateId,
    },
    /// A supplied function is not reachable from the selected root.
    UnreachableFunction {
        /// Unreachable function identity.
        function: FunctionTemplateId,
    },
}

impl fmt::Display for FunctionGraphVerificationErrorKind {
    #[allow(
        clippy::too_many_lines,
        reason = "each structured verifier error has one local exact message"
    )]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyGraph => formatter.write_str("compiler function graph is empty"),
            Self::RootOutOfBounds { root, functions } => write!(
                formatter,
                "root function {root} is outside the supplied function count {functions}"
            ),
            Self::RootRequiresEnvironment { closure_variables } => write!(
                formatter,
                "selected root requires {closure_variables} imported closure variables from an external parent"
            ),
            Self::LimitExceeded {
                resource,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} was exceeded by observed value {observed}"
            ),
            Self::ResourceUsageOverflow { resource } => {
                write!(
                    formatter,
                    "exact {resource} usage exceeds the u64 accounting domain"
                )
            }
            Self::AllocationFailed {
                resource,
                requested,
            } => write!(
                formatter,
                "could not allocate {requested} entries for {resource}"
            ),
            Self::MissingCompilerCaptureLayout => {
                formatter.write_str("body has no explicit compiler capture layout")
            }
            Self::MissingCompilerConstantLayout => {
                formatter.write_str("body has no explicit compiler constant layout")
            }
            Self::MissingAtomPool { declared } => write!(
                formatter,
                "body declares {declared} atoms, but the compiler graph has no atom pool"
            ),
            Self::AtomCountMismatch { declared, entries } => write!(
                formatter,
                "actual atom count {entries} does not equal body domain {declared}"
            ),
            Self::DuplicateAtom { first, duplicate } => write!(
                formatter,
                "atom slots {first} and {duplicate} contain the same string"
            ),
            Self::EmptyAtom { index } => {
                write!(formatter, "atom slot {index} contains the empty string")
            }
            Self::TaggedIntegerAtom { index } => write!(
                formatter,
                "atom slot {index} contains a tagged-integer spelling"
            ),
            Self::InvalidStaticPropertyOnlyAtom { index } => write!(
                formatter,
                "static-property-only atom slot {index} contains an ordinary atom spelling"
            ),
            Self::StaticPropertyOnlyAtomOperand { index, pc, opcode } => write!(
                formatter,
                "static-property-only atom slot {index} is referenced by {opcode} at PC {pc}"
            ),
            Self::StaticPropertyOnlyAtomClosureSource { closure, index } => write!(
                formatter,
                "constructor-realm global closure slot {closure} references static-property-only atom slot {index}"
            ),
            Self::ConstantCountMismatch { declared, entries } => write!(
                formatter,
                "actual constant count {entries} does not equal body domain {declared}"
            ),
            Self::ClosureVariableCountMismatch { declared, entries } => write!(
                formatter,
                "actual closure-source count {entries} does not equal body domain {declared}"
            ),
            Self::ClosureSourceAtomOutOfBounds {
                closure,
                atom,
                atoms,
            } => write!(
                formatter,
                "closure slot {closure} names constructor-realm global atom {} outside atom count {atoms}",
                atom.get()
            ),
            Self::ConstructorRealmGlobalSourceNotRoot { closure } => write!(
                formatter,
                "non-root closure slot {closure} originates a constructor-realm global source"
            ),
            Self::DirectEvalBindingSourceNotRoot { closure } => write!(
                formatter,
                "non-root closure slot {closure} originates a direct-eval caller binding"
            ),
            Self::DirectEvalBindingOutOfBounds {
                closure,
                index,
                environment_size,
            } => write!(
                formatter,
                "closure slot {closure} addresses direct-eval binding {index} outside environment size {environment_size}"
            ),
            Self::DirectEvalEnvironmentSizeMismatch {
                expected,
                closure,
                actual,
            } => write!(
                formatter,
                "closure slot {closure} declares direct-eval environment size {actual}, expected {expected}"
            ),
            Self::DuplicateClosureSource {
                first,
                duplicate,
                source,
            } => write!(
                formatter,
                "closure slots {first} and {duplicate} both import {source}"
            ),
            Self::ConstantKindMismatch {
                index,
                declared,
                actual,
            } => write!(
                formatter,
                "constant {index} has owned kind {actual}, but the body declares {declared}"
            ),
            Self::FunctionConstantOutOfBounds {
                index,
                target,
                functions,
            } => write!(
                formatter,
                "constant {index} targets function {target} outside function count {functions}"
            ),
            Self::ClosureSourceOutOfBounds {
                child,
                closure,
                source,
                len,
            } => write!(
                formatter,
                "child function {child} closure {closure} uses {source} outside parent domain length {len}"
            ),
            Self::Cycle { function } => {
                write!(
                    formatter,
                    "a function constant cycle blocks topological processing at {function}"
                )
            }
            Self::UnreachableFunction { function } => {
                write!(
                    formatter,
                    "function {function} is unreachable from the graph root"
                )
            }
        }
    }
}

/// Verifies compiler constant payloads, function edges, capture recipes,
/// topology, and aggregate budgets without recursive traversal.
///
/// # Errors
///
/// Returns a structured failure without exposing a partially verified graph.
pub fn verify_compiler_function_graph(
    graph: UnverifiedCompilerFunctionGraph,
    limits: FunctionGraphVerificationLimits,
) -> Result<VerifiedCompilerFunctionGraph, FunctionGraphVerificationError> {
    let UnverifiedCompilerFunctionGraph { root, functions } = graph;
    let function_count = usize_to_u64(functions.len());
    if functions.is_empty() {
        return Err(FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::EmptyGraph,
        ));
    }
    enforce_limit(
        FunctionGraphResource::Functions,
        u64::from(limits.max_functions),
        function_count,
        None,
    )?;
    let mut usage = preflight_graph_usage(&functions, function_count, limits)?;
    let Some(root_index) = root.index().filter(|&index| index < functions.len()) else {
        return Err(FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::RootOutOfBounds {
                root,
                functions: function_count,
            },
        ));
    };
    enforce_limit(
        FunctionGraphResource::NestingDepth,
        u64::from(limits.max_nesting_depth),
        1,
        Some(root),
    )?;

    validate_function_records(&functions)?;
    validate_root_closure_sources(root, root_index, &functions)?;
    let topological_order = build_topological_order(&functions, root)?;
    debug_assert_eq!(topological_order.len(), functions.len());
    let max_nesting_depth =
        validate_nesting_depth(&functions, root_index, &topological_order, limits)?;
    usage.closure_edge_evaluations = validate_closure_edges(&functions, limits)?;

    let mut verified = Vec::new();
    verified.try_reserve_exact(functions.len()).map_err(|_| {
        FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::AllocationFailed {
                resource: FunctionGraphResource::VerifiedFunctions,
                requested: function_count,
            },
        )
    })?;
    verified.extend(functions.iter().map(|function| {
        VerifiedCompilerFunction {
            control_flow: Arc::clone(&function.control_flow),
            atoms: function
                .atoms
                .as_ref()
                .map_or_else(|| Arc::from([]), Arc::clone),
            constants: Arc::clone(&function.constants),
            closure_sources: Arc::clone(&function.closure_sources),
        }
    }));

    Ok(VerifiedCompilerFunctionGraph {
        root,
        root_index,
        functions: verified.into(),
        max_nesting_depth,
        usage,
    })
}

fn validate_function_records(
    functions: &[UnverifiedCompilerFunction],
) -> Result<(), FunctionGraphVerificationError> {
    for (index, function) in functions.iter().enumerate() {
        let id = function_id(index)?;
        let flow = &function.control_flow;
        if flow.compiler_capture_layout().is_none() {
            return Err(FunctionGraphVerificationError::at_function(
                id,
                FunctionGraphVerificationErrorKind::MissingCompilerCaptureLayout,
            ));
        }
        let constant_layout = flow.compiler_constant_layout().ok_or_else(|| {
            FunctionGraphVerificationError::at_function(
                id,
                FunctionGraphVerificationErrorKind::MissingCompilerConstantLayout,
            )
        })?;
        let domains = flow.domains();
        let atom_entries = function
            .atoms
            .as_ref()
            .map_or(0, |atoms| usize_to_u64(atoms.len()));
        if domains.atom_pool_len() != 0 && function.atoms.is_none() {
            return Err(FunctionGraphVerificationError::at_function(
                id,
                FunctionGraphVerificationErrorKind::MissingAtomPool {
                    declared: domains.atom_pool_len(),
                },
            ));
        }
        if atom_entries != u64::from(domains.atom_pool_len()) {
            return Err(FunctionGraphVerificationError::at_function(
                id,
                FunctionGraphVerificationErrorKind::AtomCountMismatch {
                    declared: domains.atom_pool_len(),
                    entries: atom_entries,
                },
            ));
        }
        let constant_entries = usize_to_u64(function.constants.len());
        if constant_entries != u64::from(domains.constant_pool_len()) {
            return Err(FunctionGraphVerificationError::at_function(
                id,
                FunctionGraphVerificationErrorKind::ConstantCountMismatch {
                    declared: domains.constant_pool_len(),
                    entries: constant_entries,
                },
            ));
        }
        let closure_entries = usize_to_u64(function.closure_sources.len());
        if closure_entries != u64::from(domains.closure_var_count()) {
            return Err(FunctionGraphVerificationError::at_function(
                id,
                FunctionGraphVerificationErrorKind::ClosureVariableCountMismatch {
                    declared: domains.closure_var_count(),
                    entries: closure_entries,
                },
            ));
        }
        if let Some(atoms) = &function.atoms {
            validate_atoms(id, atoms, flow, &function.closure_sources)?;
        }
        validate_realm_global_source_atoms(id, &function.closure_sources, domains.atom_pool_len())?;
        validate_unique_closure_sources(id, &function.closure_sources)?;
        for (constant_index, (constant, declared)) in function
            .constants
            .iter()
            .zip(constant_layout.kinds().iter().copied())
            .enumerate()
        {
            let actual = constant.kind();
            if actual != declared {
                return Err(FunctionGraphVerificationError::at_function(
                    id,
                    FunctionGraphVerificationErrorKind::ConstantKindMismatch {
                        index: usize_to_u32(constant_index),
                        declared,
                        actual,
                    },
                ));
            }
        }
        for (constant_index, target) in function_constant_targets(&function.constants) {
            constant_target_index(id, constant_index, target, functions.len())?;
        }
    }
    Ok(())
}

fn validate_realm_global_source_atoms(
    function: FunctionTemplateId,
    sources: &[CompilerClosureSource],
    atom_count: u32,
) -> Result<(), FunctionGraphVerificationError> {
    for (closure, &source) in sources.iter().enumerate() {
        let CompilerClosureSource::ConstructorRealmGlobal(atom) = source else {
            continue;
        };
        if atom.get() >= atom_count {
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::ClosureSourceAtomOutOfBounds {
                    closure: usize_to_u32(closure),
                    atom,
                    atoms: atom_count,
                },
            ));
        }
    }
    Ok(())
}

fn validate_root_closure_sources(
    root: FunctionTemplateId,
    root_index: usize,
    functions: &[UnverifiedCompilerFunction],
) -> Result<(), FunctionGraphVerificationError> {
    for (index, function) in functions.iter().enumerate() {
        if index == root_index {
            continue;
        }
        for (closure, source) in function.closure_sources.iter().enumerate() {
            let kind = match source {
                CompilerClosureSource::ConstructorRealmGlobal(_) => Some(
                    FunctionGraphVerificationErrorKind::ConstructorRealmGlobalSourceNotRoot {
                        closure: usize_to_u32(closure),
                    },
                ),
                CompilerClosureSource::DirectEvalBinding { .. } => Some(
                    FunctionGraphVerificationErrorKind::DirectEvalBindingSourceNotRoot {
                        closure: usize_to_u32(closure),
                    },
                ),
                CompilerClosureSource::ParentVariableReference(_)
                | CompilerClosureSource::ParentClosure(_) => None,
            };
            if let Some(kind) = kind {
                return Err(FunctionGraphVerificationError::at_function(
                    function_id(index)?,
                    kind,
                ));
            }
        }
    }

    let root_function = &functions[root_index];
    let mut direct_environment_size = None;
    for (closure, source) in root_function.closure_sources.iter().enumerate() {
        match *source {
            CompilerClosureSource::ConstructorRealmGlobal(_) => {}
            CompilerClosureSource::DirectEvalBinding {
                index,
                environment_size,
            } => {
                if index >= environment_size {
                    return Err(FunctionGraphVerificationError::at_function(
                        root,
                        FunctionGraphVerificationErrorKind::DirectEvalBindingOutOfBounds {
                            closure: usize_to_u32(closure),
                            index,
                            environment_size,
                        },
                    ));
                }
                if let Some(expected) = direct_environment_size {
                    if environment_size != expected {
                        return Err(FunctionGraphVerificationError::at_function(
                            root,
                            FunctionGraphVerificationErrorKind::DirectEvalEnvironmentSizeMismatch {
                                expected,
                                closure: usize_to_u32(closure),
                                actual: environment_size,
                            },
                        ));
                    }
                } else {
                    direct_environment_size = Some(environment_size);
                }
            }
            CompilerClosureSource::ParentVariableReference(_)
            | CompilerClosureSource::ParentClosure(_) => {
                return Err(FunctionGraphVerificationError::at_function(
                    root,
                    FunctionGraphVerificationErrorKind::RootRequiresEnvironment {
                        closure_variables: root_function.control_flow.domains().closure_var_count(),
                    },
                ));
            }
        }
    }
    Ok(())
}

fn preflight_graph_usage(
    functions: &[UnverifiedCompilerFunction],
    function_count: u64,
    limits: FunctionGraphVerificationLimits,
) -> Result<FunctionGraphUsage, FunctionGraphVerificationError> {
    let mut usage = FunctionGraphUsage {
        functions: function_count,
        ..FunctionGraphUsage::default()
    };
    for (index, function) in functions.iter().enumerate() {
        let id = function_id(index)?;
        charge_usage(
            &mut usage.bytecode_bytes,
            usize_to_u64(function.control_flow.bytecode().len()),
            FunctionGraphResource::BytecodeBytes,
            limits.max_bytecode_bytes,
            id,
        )?;
        charge_usage(
            &mut usage.instructions,
            usize_to_u64(function.control_flow.instructions().len()),
            FunctionGraphResource::Instructions,
            limits.max_instructions,
            id,
        )?;
        charge_usage(
            &mut usage.constants,
            usize_to_u64(function.constants.len()),
            FunctionGraphResource::Constants,
            limits.max_constants,
            id,
        )?;
        charge_usage(
            &mut usage.atoms,
            function
                .atoms
                .as_ref()
                .map_or(0, |atoms| usize_to_u64(atoms.len())),
            FunctionGraphResource::Atoms,
            limits.max_atoms,
            id,
        )?;
        charge_usage(
            &mut usage.string_payload_bytes,
            function_string_payload_bytes(function, id)?,
            FunctionGraphResource::StringPayloadBytes,
            limits.max_string_payload_bytes,
            id,
        )?;
        charge_usage(
            &mut usage.closure_variables,
            usize_to_u64(function.closure_sources.len()),
            FunctionGraphResource::ClosureVariables,
            limits.max_closure_variables,
            id,
        )?;
        charge_usage(
            &mut usage.transfer_evaluations,
            function.control_flow.transfer_evaluations(),
            FunctionGraphResource::TransferEvaluations,
            limits.max_transfer_evaluations,
            id,
        )?;
    }
    Ok(usage)
}

fn validate_unique_closure_sources(
    function: FunctionTemplateId,
    sources: &[CompilerClosureSource],
) -> Result<(), FunctionGraphVerificationError> {
    let mut seen = HashSet::new();
    seen.try_reserve(sources.len()).map_err(|_| {
        FunctionGraphVerificationError::at_function(
            function,
            FunctionGraphVerificationErrorKind::AllocationFailed {
                resource: FunctionGraphResource::ClosureSourceEntries,
                requested: usize_to_u64(sources.len()),
            },
        )
    })?;
    for (duplicate, &source) in sources.iter().enumerate() {
        if !seen.insert(source) {
            let first = sources[..duplicate]
                .iter()
                .position(|candidate| *candidate == source)
                .unwrap_or(duplicate);
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::DuplicateClosureSource {
                    first: usize_to_u32(first),
                    duplicate: usize_to_u32(duplicate),
                    source,
                },
            ));
        }
    }
    Ok(())
}

fn validate_atoms(
    function: FunctionTemplateId,
    atoms: &[CompilerAtom],
    flow: &VerifiedControlFlow,
    closure_sources: &[CompilerClosureSource],
) -> Result<(), FunctionGraphVerificationError> {
    let mut seen = HashMap::new();
    seen.try_reserve(atoms.len()).map_err(|_| {
        FunctionGraphVerificationError::at_function(
            function,
            FunctionGraphVerificationErrorKind::AllocationFailed {
                resource: FunctionGraphResource::AtomDedupEntries,
                requested: usize_to_u64(atoms.len()),
            },
        )
    })?;
    for (duplicate, atom) in atoms.iter().enumerate() {
        let duplicate = usize_to_u32(duplicate);
        let is_empty = atom.string().is_empty();
        let is_tagged_integer = atom.string().is_tagged_integer_atom();
        if atom.is_static_property_only() && !(is_empty || is_tagged_integer) {
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::InvalidStaticPropertyOnlyAtom {
                    index: duplicate,
                },
            ));
        }
        if !atom.is_static_property_only() && is_empty {
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::EmptyAtom { index: duplicate },
            ));
        }
        if !atom.is_static_property_only() && is_tagged_integer {
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::TaggedIntegerAtom { index: duplicate },
            ));
        }
        if let Some(&first) = seen.get(atom.string()) {
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::DuplicateAtom { first, duplicate },
            ));
        }
        seen.insert(atom.string(), duplicate);
    }

    for instruction in flow.instructions() {
        let decoded = instruction.decoded();
        let Some(index) = decoded.instruction().operands().atom_pool_index() else {
            continue;
        };
        let Some(atom) = usize::try_from(index.get())
            .ok()
            .and_then(|index| atoms.get(index))
        else {
            continue;
        };
        if atom.is_static_property_only()
            && !matches!(
                decoded.instruction().opcode(),
                FinalOpcode::DefineField
                    | FinalOpcode::DefineMethod
                    | FinalOpcode::DefineClass
                    | FinalOpcode::SetName
            )
        {
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::StaticPropertyOnlyAtomOperand {
                    index: index.get(),
                    pc: decoded.pc(),
                    opcode: decoded.instruction().opcode(),
                },
            ));
        }
    }

    for (closure, source) in closure_sources.iter().enumerate() {
        let CompilerClosureSource::ConstructorRealmGlobal(index) = source else {
            continue;
        };
        if usize::try_from(index.get())
            .ok()
            .and_then(|index| atoms.get(index))
            .is_some_and(CompilerAtom::is_static_property_only)
        {
            return Err(FunctionGraphVerificationError::at_function(
                function,
                FunctionGraphVerificationErrorKind::StaticPropertyOnlyAtomClosureSource {
                    closure: usize_to_u32(closure),
                    index: index.get(),
                },
            ));
        }
    }
    Ok(())
}

fn function_string_payload_bytes(
    function: &UnverifiedCompilerFunction,
    id: FunctionTemplateId,
) -> Result<u64, FunctionGraphVerificationError> {
    let mut total = 0_u64;
    for atom in function
        .atoms
        .as_ref()
        .into_iter()
        .flat_map(|atoms| atoms.iter())
    {
        total = checked_resource_add(
            total,
            usize_to_u64(atom.string().payload_bytes()),
            FunctionGraphResource::StringPayloadBytes,
            id,
        )?;
    }
    for constant in function.constants.iter() {
        let payload_bytes = match constant {
            CompilerConstant::Value(CompilerConstantValue::String(value)) => {
                usize_to_u64(value.payload_bytes())
            }
            CompilerConstant::Value(CompilerConstantValue::BigInt(value)) => {
                usize_to_u64(value.payload_bytes())
            }
            CompilerConstant::Value(CompilerConstantValue::TemplateObject(value)) => {
                value.payload_bytes()
            }
            CompilerConstant::Value(CompilerConstantValue::Number(_))
            | CompilerConstant::Function(_) => 0,
        };
        total = checked_resource_add(
            total,
            payload_bytes,
            FunctionGraphResource::StringPayloadBytes,
            id,
        )?;
    }
    Ok(total)
}

fn validate_closure_edges(
    functions: &[UnverifiedCompilerFunction],
    limits: FunctionGraphVerificationLimits,
) -> Result<u64, FunctionGraphVerificationError> {
    let mut evaluations = 0_u64;
    for (parent_index, parent) in functions.iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        for (constant_index, child_id) in function_constant_targets(&parent.constants) {
            let child_index =
                constant_target_index(parent_id, constant_index, child_id, functions.len())?;
            let Some(child) = functions.get(child_index) else {
                return Err(FunctionGraphVerificationError::at_function(
                    parent_id,
                    FunctionGraphVerificationErrorKind::FunctionConstantOutOfBounds {
                        index: usize_to_u32(constant_index),
                        target: child_id,
                        functions: usize_to_u64(functions.len()),
                    },
                ));
            };
            charge_usage(
                &mut evaluations,
                usize_to_u64(child.closure_sources.len()),
                FunctionGraphResource::ClosureEdgeEvaluations,
                limits.max_closure_edge_evaluations,
                parent_id,
            )?;
            for (closure_index, &source) in child.closure_sources.iter().enumerate() {
                let (source_index, len) = match source {
                    CompilerClosureSource::ParentVariableReference(index) => (
                        index,
                        parent
                            .control_flow
                            .function_header()
                            .variable_reference_count(),
                    ),
                    CompilerClosureSource::ParentClosure(index) => {
                        (index, parent.control_flow.domains().closure_var_count())
                    }
                    CompilerClosureSource::ConstructorRealmGlobal(_) => {
                        continue;
                    }
                    CompilerClosureSource::DirectEvalBinding { .. } => continue,
                };
                if source_index >= len {
                    return Err(FunctionGraphVerificationError::at_function(
                        parent_id,
                        FunctionGraphVerificationErrorKind::ClosureSourceOutOfBounds {
                            child: child_id,
                            closure: usize_to_u32(closure_index),
                            source,
                            len,
                        },
                    ));
                }
            }
        }
    }
    Ok(evaluations)
}

fn build_topological_order(
    functions: &[UnverifiedCompilerFunction],
    root: FunctionTemplateId,
) -> Result<Vec<usize>, FunctionGraphVerificationError> {
    let requested = usize_to_u64(functions.len());
    let mut indegrees = try_zeroed_u64(functions.len(), FunctionGraphResource::TopologyEntries)?;
    for (parent_index, function) in functions.iter().enumerate() {
        let parent = function_id(parent_index)?;
        for (constant_index, target) in function_constant_targets(&function.constants) {
            let index = constant_target_index(parent, constant_index, target, functions.len())?;
            indegrees[index] = indegrees[index].checked_add(1).ok_or_else(|| {
                FunctionGraphVerificationError::graph(
                    FunctionGraphVerificationErrorKind::LimitExceeded {
                        resource: FunctionGraphResource::TopologyEntries,
                        limit: u64::MAX,
                        observed: u64::MAX,
                    },
                )
            })?;
        }
    }

    let mut ready = VecDeque::new();
    ready.try_reserve(functions.len()).map_err(|_| {
        FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::AllocationFailed {
                resource: FunctionGraphResource::TopologyEntries,
                requested,
            },
        )
    })?;
    for (index, &indegree) in indegrees.iter().enumerate() {
        if indegree == 0 {
            ready.push_back(index);
        }
    }
    let mut order = Vec::new();
    order.try_reserve_exact(functions.len()).map_err(|_| {
        FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::AllocationFailed {
                resource: FunctionGraphResource::TopologyEntries,
                requested,
            },
        )
    })?;
    while let Some(parent_index) = ready.pop_front() {
        order.push(parent_index);
        let parent = function_id(parent_index)?;
        let function = &functions[parent_index];
        for (constant_index, target) in function_constant_targets(&function.constants) {
            let target_index =
                constant_target_index(parent, constant_index, target, functions.len())?;
            let indegree = &mut indegrees[target_index];
            *indegree = indegree.saturating_sub(1);
            if *indegree == 0 {
                ready.push_back(target_index);
            }
        }
    }
    if order.len() != functions.len() {
        let function = indegrees
            .iter()
            .position(|&indegree| indegree != 0)
            .and_then(|index| u32::try_from(index).ok())
            .map_or(root, FunctionTemplateId::new);
        return Err(FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::Cycle { function },
        ));
    }
    Ok(order)
}

fn validate_nesting_depth(
    functions: &[UnverifiedCompilerFunction],
    root_index: usize,
    order: &[usize],
    limits: FunctionGraphVerificationLimits,
) -> Result<u32, FunctionGraphVerificationError> {
    let mut depths = try_zeroed_u32(functions.len(), FunctionGraphResource::TopologyEntries)?;
    depths[root_index] = 1;
    let mut maximum = 1_u32;
    for &parent_index in order {
        let parent_id = function_id(parent_index)?;
        let parent_depth = depths[parent_index];
        if parent_depth == 0
            || !functions[parent_index]
                .constants
                .iter()
                .any(|constant| matches!(constant, CompilerConstant::Function(_)))
        {
            continue;
        }
        let child_depth = parent_depth.checked_add(1).ok_or_else(|| {
            FunctionGraphVerificationError::at_function(
                parent_id,
                FunctionGraphVerificationErrorKind::LimitExceeded {
                    resource: FunctionGraphResource::NestingDepth,
                    limit: u64::from(limits.max_nesting_depth),
                    observed: u64::from(u32::MAX) + 1,
                },
            )
        })?;
        for (constant_index, child_id) in
            function_constant_targets(&functions[parent_index].constants)
        {
            let child_index =
                constant_target_index(parent_id, constant_index, child_id, functions.len())?;
            if child_depth > depths[child_index] {
                depths[child_index] = child_depth;
                maximum = maximum.max(child_depth);
                enforce_limit(
                    FunctionGraphResource::NestingDepth,
                    u64::from(limits.max_nesting_depth),
                    u64::from(child_depth),
                    Some(child_id),
                )?;
            }
        }
    }
    if let Some(index) = depths.iter().position(|&depth| depth == 0) {
        return Err(FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::UnreachableFunction {
                function: function_id(index)?,
            },
        ));
    }
    Ok(maximum)
}

fn function_constant_targets(
    constants: &[CompilerConstant],
) -> impl Iterator<Item = (usize, FunctionTemplateId)> + '_ {
    constants
        .iter()
        .enumerate()
        .filter_map(|(index, constant)| match constant {
            CompilerConstant::Function(target) => Some((index, *target)),
            CompilerConstant::Value(_) => None,
        })
}

fn constant_target_index(
    parent: FunctionTemplateId,
    constant_index: usize,
    target: FunctionTemplateId,
    function_count: usize,
) -> Result<usize, FunctionGraphVerificationError> {
    target
        .index()
        .filter(|&index| index < function_count)
        .ok_or_else(|| {
            FunctionGraphVerificationError::at_function(
                parent,
                FunctionGraphVerificationErrorKind::FunctionConstantOutOfBounds {
                    index: usize_to_u32(constant_index),
                    target,
                    functions: usize_to_u64(function_count),
                },
            )
        })
}

fn charge_usage(
    total: &mut u64,
    increment: u64,
    resource: FunctionGraphResource,
    limit: u64,
    function: FunctionTemplateId,
) -> Result<(), FunctionGraphVerificationError> {
    let observed = checked_resource_add(*total, increment, resource, function)?;
    enforce_limit(resource, limit, observed, Some(function))?;
    *total = observed;
    Ok(())
}

fn checked_resource_add(
    total: u64,
    increment: u64,
    resource: FunctionGraphResource,
    function: FunctionTemplateId,
) -> Result<u64, FunctionGraphVerificationError> {
    total.checked_add(increment).ok_or_else(|| {
        FunctionGraphVerificationError::at_function(
            function,
            FunctionGraphVerificationErrorKind::ResourceUsageOverflow { resource },
        )
    })
}

fn enforce_limit(
    resource: FunctionGraphResource,
    limit: u64,
    observed: u64,
    function: Option<FunctionTemplateId>,
) -> Result<(), FunctionGraphVerificationError> {
    if observed <= limit {
        return Ok(());
    }
    let kind = FunctionGraphVerificationErrorKind::LimitExceeded {
        resource,
        limit,
        observed,
    };
    Err(match function {
        Some(function) => FunctionGraphVerificationError::at_function(function, kind),
        None => FunctionGraphVerificationError::graph(kind),
    })
}

fn try_zeroed_u64(
    len: usize,
    resource: FunctionGraphResource,
) -> Result<Vec<u64>, FunctionGraphVerificationError> {
    let mut values = Vec::new();
    values.try_reserve_exact(len).map_err(|_| {
        FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::AllocationFailed {
                resource,
                requested: usize_to_u64(len),
            },
        )
    })?;
    values.resize(len, 0);
    Ok(values)
}

fn try_zeroed_u32(
    len: usize,
    resource: FunctionGraphResource,
) -> Result<Vec<u32>, FunctionGraphVerificationError> {
    let mut values = Vec::new();
    values.try_reserve_exact(len).map_err(|_| {
        FunctionGraphVerificationError::graph(
            FunctionGraphVerificationErrorKind::AllocationFailed {
                resource,
                requested: usize_to_u64(len),
            },
        )
    })?;
    values.resize(len, 0);
    Ok(values)
}

fn function_id(index: usize) -> Result<FunctionTemplateId, FunctionGraphVerificationError> {
    u32::try_from(index)
        .map(FunctionTemplateId::new)
        .map_err(|_| {
            FunctionGraphVerificationError::graph(
                FunctionGraphVerificationErrorKind::LimitExceeded {
                    resource: FunctionGraphResource::Functions,
                    limit: u64::from(u32::MAX),
                    observed: usize_to_u64(index).saturating_add(1),
                },
            )
        })
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        FunctionGraphResource, FunctionGraphVerificationErrorKind, FunctionTemplateId, charge_usage,
    };

    #[test]
    fn resource_accounting_overflow_fails_even_with_the_largest_limit() {
        let mut total = u64::MAX;
        let error = charge_usage(
            &mut total,
            1,
            FunctionGraphResource::StringPayloadBytes,
            u64::MAX,
            FunctionTemplateId::new(0),
        )
        .expect_err("overflow cannot become an accepted saturated usage");

        assert_eq!(total, u64::MAX);
        assert_eq!(
            error.kind(),
            &FunctionGraphVerificationErrorKind::ResourceUsageOverflow {
                resource: FunctionGraphResource::StringPayloadBytes,
            }
        );
    }
}
