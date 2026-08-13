/// Metadata atom location named by a verification failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MetadataAtomField {
    /// Function display name.
    FunctionName,
    /// Argument or local name.
    VariableName(u32),
    /// Imported closure name.
    ClosureName(u32),
    /// Module binding name.
    ModuleBindingName(u32),
    /// Module request specifier.
    ModuleRequestSpecifier(u32),
    /// Module named import export name.
    ModuleImportName(u32),
}

/// Frame slot named by a binding-policy failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BindingSlot {
    /// Formal argument slot.
    Argument(u32),
    /// Function local slot.
    Local(u32),
    /// Imported closure slot.
    Closure(u32),
}

/// Exact reason an opcode conflicts with retained binding policy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BindingPolicyViolationReason {
    /// A supplied declaration-policy combination is not defined.
    InvalidDeclarationPolicy,
    /// Argument metadata is not the simple mutable-parameter profile.
    InvalidArgumentDefinition,
    /// The scope flag disagrees with the declaration policy.
    ScopeFlagMismatch,
    /// A checked operation targets a non-TDZ binding.
    UnexpectedCheckedAccess,
    /// An unchecked operation targets a TDZ binding.
    UncheckedTemporalDeadZoneAccess,
    /// A write targets an immutable binding.
    ImmutableWrite,
    /// Lexical initialization was attempted outside the uninitialized state.
    InvalidLexicalInitialization,
    /// A reachable lexical access can occur before scope initialization.
    MissingLexicalScopeInitialization,
}

/// Structured final compiler-bytecode verification failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BytecodeVerificationError {
    function: Option<FunctionTemplateId>,
    kind: BytecodeVerificationErrorKind,
}

impl BytecodeVerificationError {
    fn graph(kind: BytecodeVerificationErrorKind) -> Self {
        Self {
            function: None,
            kind,
        }
    }

    fn function(function: FunctionTemplateId, kind: BytecodeVerificationErrorKind) -> Self {
        Self {
            function: Some(function),
            kind,
        }
    }

    /// Returns the affected function, when local.
    #[must_use]
    pub const fn function_id(&self) -> Option<FunctionTemplateId> {
        self.function
    }

    /// Returns the exact structured failure.
    #[must_use]
    pub const fn kind(&self) -> &BytecodeVerificationErrorKind {
        &self.kind
    }
}

impl fmt::Display for BytecodeVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(function) = self.function {
            write!(formatter, "function {function}: ")?;
        }
        self.kind.fmt(formatter)
    }
}

impl Error for BytecodeVerificationError {}

/// Exact reason final compiler-bytecode verification failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BytecodeVerificationErrorKind {
    /// Metadata count differs from the staged function count.
    FunctionMetadataCountMismatch {
        /// Staged function records.
        functions: u64,
        /// Supplied metadata records.
        metadata: u64,
    },
    /// An aggregate final-verifier budget was exceeded.
    LimitExceeded {
        /// Exhausted resource.
        resource: BytecodeGraphResource,
        /// Inclusive configured maximum.
        limit: u64,
        /// Observed value.
        observed: u64,
    },
    /// Temporary or frozen verifier storage could not be reserved.
    AllocationFailed {
        /// Allocation purpose.
        resource: BytecodeGraphResource,
        /// Requested entries.
        requested: u64,
    },
    /// A Global Script record is not the graph root.
    GlobalScriptNotRoot,
    /// A Global Script record declares a call-argument domain.
    GlobalScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// A Global Script record carries function-name metadata or a named
    /// function self binding.
    GlobalScriptHasFunctionName,
    /// An indirect-eval Script record is not the graph root.
    IndirectEvalScriptNotRoot,
    /// An indirect-eval Script record declares a call-argument domain.
    IndirectEvalScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// An indirect-eval Script record carries function-name metadata or a
    /// named-function self binding.
    IndirectEvalScriptHasFunctionName,
    /// A direct-eval Script record is not the graph root.
    DirectEvalScriptNotRoot,
    /// A direct-eval Script record declares a call-argument domain.
    DirectEvalScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// A direct-eval Script record carries function-name metadata or a
    /// named-function self binding.
    DirectEvalScriptHasFunctionName,
    /// A dynamic-Function Script record is not the graph root.
    DynamicFunctionScriptNotRoot,
    /// A dynamic-Function Script record declares a call-argument domain.
    DynamicFunctionScriptHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// A dynamic-Function Script record carries function-name metadata or a
    /// named-function self binding.
    DynamicFunctionScriptHasFunctionName,
    /// A Module root record is not the graph root.
    ModuleNotRoot,
    /// A Module root record declares a call-argument domain.
    ModuleHasArguments {
        /// Header-defined arguments.
        defined: u32,
        /// Frame argument slots.
        arguments: u32,
    },
    /// A Module root record carries function-name metadata or a named function
    /// self binding.
    ModuleHasFunctionName,
    /// A Module declaration record is required for a Module root but absent.
    ModuleDeclarationRecordMissing,
    /// A Module declaration record appears on a non-Module root.
    ModuleDeclarationRecordUnexpected,
    /// A module binding descriptor's closure slot is outside the root closure
    /// domain.
    ModuleBindingSlotOutOfBounds {
        /// Binding descriptor index.
        binding: u32,
        /// Closure slot index.
        slot: u32,
        /// Root closure domain size.
        closures: u32,
    },
    /// A module binding descriptor's closure slot does not select a
    /// module-origin captured cell with a matching policy.
    ModuleBindingSlotMismatch {
        /// Binding descriptor index.
        binding: u32,
        /// Closure slot index.
        slot: u32,
    },
    /// Module binding descriptor slots are not dense and unique.
    ModuleBindingSlotOrder {
        /// Binding descriptor index.
        binding: u32,
        /// Closure slot index.
        slot: u32,
    },
    /// A module binding's policy is inconsistent with its declared origin.
    ModuleBindingPolicyMismatch {
        /// Binding descriptor index.
        binding: u32,
    },
    /// A module binding's function initializer does not name a function
    /// constant in the root.
    ModuleBindingInitializerMismatch {
        /// Binding descriptor index.
        binding: u32,
        /// Constant index, when supplied.
        constant: Option<u32>,
    },
    /// A module import descriptor references an out-of-range request.
    ModuleImportRequestOutOfBounds {
        /// Binding descriptor index.
        binding: u32,
        /// Request index.
        request: u32,
        /// Request count.
        requests: u32,
    },
    /// A module request specifier atom is not named.
    ModuleRequestSpecifierMissing {
        /// Request index.
        request: u32,
    },
    /// An object method or accessor carries a source name before
    /// `define_method` assigns its property-derived observable name.
    OrdinaryMethodHasFunctionName,
    /// An arrow carries compiler source-name metadata or a self binding even
    /// though observable names are assigned only when its closure is created.
    OrdinaryArrowHasFunctionName,
    /// A constructor-realm global source appears outside a Script authority
    /// root.
    ConstructorRealmGlobalSourceRequiresDynamicFunctionScript {
        /// Closure-domain slot containing the source.
        closure: u32,
    },
    /// A caller-binding source appears outside a direct-eval Script authority
    /// root.
    DirectEvalBindingSourceRequiresDirectEvalScript {
        /// Closure-domain slot containing the source.
        closure: u32,
    },
    /// A global opcode targets a captured slot, or a captured-cell opcode
    /// targets a realm-global slot.
    ClosureBindingOpcodeMismatch {
        /// Affected closure-domain slot.
        closure: u32,
        /// Final bytecode position.
        pc: BytecodePc,
        /// Rejected opcode.
        opcode: FinalOpcode,
    },
    /// `delete_var` names no object-backed or unresolved realm-global
    /// reference declared by this function's verified closure metadata.
    RealmGlobalDeleteBindingMissing {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Function-local atom named by `delete_var`.
        atom: AtomPoolIndex,
    },
    /// Function flags or mode are outside the selected compiler profile.
    UnsupportedFunctionHeader,
    /// Source-defined arguments do not equal simple parameter positions.
    DefinedArgumentCountMismatch {
        /// Header-defined argument count.
        defined: u32,
        /// Frame argument count.
        arguments: u32,
    },
    /// Argument-plus-local definition count differs from frame domains.
    VariableDefinitionCountMismatch {
        /// Required definition count.
        declared: u64,
        /// Supplied definitions.
        entries: u64,
    },
    /// The synthesized `arguments` binding marker does not match its metadata
    /// or bytecode initializer.
    ArgumentsObjectMetadataMismatch {
        /// Marked variable definition, when one was supplied.
        definition: Option<u32>,
        /// `special_object` initializer site, when one was encoded.
        pc: Option<BytecodePc>,
    },
    /// Closure definition count differs from the staged closure domain.
    ClosureDefinitionCountMismatch {
        /// Required closure count.
        declared: u32,
        /// Supplied descriptors.
        entries: u64,
    },
    /// A required variable or closure name is absent.
    MissingMetadataAtom {
        /// Missing atom field.
        field: MetadataAtomField,
    },
    /// A metadata atom index is outside its function-local pool.
    MetadataAtomOutOfBounds {
        /// Invalid atom field.
        field: MetadataAtomField,
        /// Rejected index.
        index: u32,
        /// Function-local atom count.
        len: u32,
    },
    /// A metadata field references an atom certified only for static object
    /// property definition.
    StaticPropertyOnlyMetadataAtom {
        /// Invalid atom field.
        field: MetadataAtomField,
        /// Rejected index.
        index: u32,
    },
    /// A variable definition has an invalid policy or opcode relationship.
    BindingPolicyViolation {
        /// Affected frame slot.
        slot: BindingSlot,
        /// Final bytecode position when opcode-related.
        pc: Option<BytecodePc>,
        /// Exact mismatch.
        reason: BindingPolicyViolationReason,
    },
    /// Parameter-expression scope metadata is deferred.
    ArgumentScopeMetadataUnsupported {
        /// Definition containing the sentinel.
        definition: u32,
    },
    /// A local scope link is out of range.
    ScopeLinkOutOfBounds {
        /// Definition containing the link.
        definition: u32,
        /// Rejected local index.
        target: u32,
        /// Local count.
        locals: u32,
    },
    /// A scope link crosses between lexical and function-scoped definitions.
    ScopeLinkKindMismatch {
        /// Definition containing the link.
        definition: u32,
        /// Target with an incompatible scope category.
        target: u32,
    },
    /// A local scope-link chain contains a cycle.
    ScopeLinkCycle {
        /// One local in the cycle.
        local: u32,
    },
    /// An adjusted `eval` scope operand points past the local domain.
    EvalScopeIndexOutOfBounds {
        /// Final bytecode position of the eval operation.
        pc: BytecodePc,
        /// Rejected adjusted scope operand.
        scope_index: u16,
        /// Local-variable count.
        locals: u32,
    },
    /// An adjusted `eval` scope operand selects a function-scoped local.
    EvalScopeHeadNotLexical {
        /// Final bytecode position of the eval operation.
        pc: BytecodePc,
        /// Rejected zero-based local index.
        local: u32,
    },
    /// A variable-reference index is out of range.
    VariableReferenceOutOfBounds {
        /// Definition containing the reference.
        definition: u32,
        /// Rejected reference.
        reference: u32,
        /// Declared reference count.
        len: u32,
    },
    /// Two vardefs claim the same own variable-reference index.
    DuplicateVariableReference {
        /// Repeated reference index.
        reference: u32,
    },
    /// Captured vardefs do not form exactly the dense declared domain.
    VariableReferenceDomainMismatch {
        /// Declared references.
        declared: u32,
        /// Captured vardefs.
        captured: u32,
    },
    /// A vardef disagrees with the staged capture layout.
    CaptureLayoutMismatch {
        /// Dense variable-reference index.
        reference: u32,
    },
    /// Function-binding metadata does not name one valid function constant.
    FunctionInitializerMetadataMismatch {
        /// Argument-or-local definition index.
        definition: u32,
        /// Supplied constant index, when present.
        constant: Option<u32>,
    },
    /// Function-binding bytecode does not contain one isolated initializer pair.
    FunctionInitializerOpcodeMismatch {
        /// Argument-or-local definition index.
        definition: u32,
        /// Required function constant.
        constant: u32,
        /// Matching `FClosure` plus `Put` pairs.
        matches: u32,
    },
    /// Two vardefs claim the same function initializer constant.
    FunctionInitializerConstantReused {
        /// Reused function constant.
        constant: u32,
        /// First vardef using the constant.
        first: u32,
        /// Duplicate vardef using the constant.
        duplicate: u32,
    },
    /// A function-instantiation initializer is outside the isolated entry prefix.
    FunctionInitializerPlacementMismatch {
        /// Argument-or-local definition index.
        definition: u32,
        /// Actual closure PC.
        pc: BytecodePc,
    },
    /// A constructor-realm function declaration does not name one valid
    /// function constant.
    RealmGlobalFunctionInitializerMetadataMismatch {
        /// Root closure-domain slot.
        closure: u32,
        /// Supplied constant index, when present.
        constant: Option<u32>,
    },
    /// Constructor-realm function bytecode does not contain one isolated
    /// initializer pair.
    RealmGlobalFunctionInitializerOpcodeMismatch {
        /// Root closure-domain slot.
        closure: u32,
        /// Required function constant.
        constant: u32,
        /// Matching `FClosure` plus `PutVar` pairs.
        matches: u32,
    },
    /// A constructor-realm function initializer is outside the isolated entry
    /// prefix.
    RealmGlobalFunctionInitializerPlacementMismatch {
        /// Root closure-domain slot.
        closure: u32,
        /// Actual closure PC.
        pc: BytecodePc,
    },
    /// A realm-global lexical binding has the wrong number of certified
    /// declaration-initialization sites.
    RealmGlobalLexicalInitializerCountMismatch {
        /// Affected closure-domain slot.
        closure: u32,
        /// Matching `PutVarInit` sites.
        matches: u32,
    },
    /// A function template is not owned by exactly one parent constant edge.
    FunctionTemplateOwnershipMismatch {
        /// Function template with invalid ownership.
        child: FunctionTemplateId,
        /// Observed incoming function-constant edges.
        incoming: u64,
    },
    /// Closure source or declaration metadata differs from its parent.
    ClosureMetadataMismatch {
        /// Child function.
        child: FunctionTemplateId,
        /// Child closure slot.
        closure: u32,
    },
    /// Source display name is empty.
    EmptySourceDisplayName,
    /// A source span is reversed, out of range, or not on UTF-8 boundaries.
    InvalidSourceSpan {
        /// Rejected span.
        span: SourceByteSpan,
    },
    /// Function-name atom and source span presence differ.
    FunctionNameSourceMismatch,
    /// Function-name span is outside the function source span.
    FunctionNameOutsideFunction,
    /// Source mapping count differs from the instruction count.
    SourceMappingCountMismatch {
        /// Verified instructions.
        instructions: u64,
        /// Supplied mappings.
        mappings: u64,
    },
    /// More strict-source PCs were supplied than verified instructions.
    StrictModeInstructionCountOutOfBounds {
        /// Supplied strict-source PCs.
        strict_instructions: u64,
        /// Verified instructions.
        instructions: u64,
    },
    /// Strict-source PCs are not strictly increasing.
    StrictModePcNotIncreasing {
        /// Position of the rejected PC.
        index: u32,
        /// Previous PC.
        previous: BytecodePc,
        /// Rejected PC.
        current: BytecodePc,
    },
    /// A strict-source PC is not a verified instruction boundary.
    StrictModePcNotInstruction {
        /// Position of the rejected PC.
        index: u32,
        /// Rejected PC.
        pc: BytecodePc,
    },
    /// A mapping PC differs from the corresponding verified instruction.
    SourcePcMismatch {
        /// Mapping position.
        mapping: u32,
        /// Supplied PC.
        declared: BytecodePc,
        /// Verified PC.
        actual: BytecodePc,
    },
    /// An instruction source span falls outside its function source.
    InstructionSourceOutsideFunction {
        /// Mapping position.
        mapping: u32,
    },
    /// The staged body contains an opcode outside the compiler profile.
    UnsupportedCompilerOpcode {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Rejected opcode.
        opcode: FinalOpcode,
    },
    /// A function contains more `gosub` sites than the pinned compatibility
    /// profile permits.
    GosubSiteCountOutOfRange {
        /// Observed `gosub` sites in the function.
        sites: u64,
        /// Inclusive compatibility maximum.
        maximum: u32,
    },
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal finally return-address marker.
    FinallyReturnStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow entered a finalizer ordinarily, merged distinct finalizer
    /// identities, or mixed a finally return marker with a JavaScript value.
    FinallyReturnJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained a malformed internal finally return marker.
    FinallyReturnMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal captured-binding Reference.
    CapturedReferenceStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow merged distinct captured-binding References or mixed a
    /// Reference component with an ordinary JavaScript value.
    CapturedReferenceJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained an internal captured-binding Reference.
    CapturedReferenceMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal `for-in` iterator marker.
    ForInIteratorStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow merged distinct `for-in` iterator identities or mixed an
    /// iterator marker with an ordinary JavaScript value.
    ForInIteratorJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained an internal `for-in` iterator marker.
    ForInIteratorMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal synchronous `for-of` iterator record.
    ForOfIteratorStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow merged distinct synchronous `for-of` record identities or
    /// mixed an iterator record with ordinary JavaScript values.
    ForOfIteratorJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained a malformed synchronous `for-of` record.
    ForOfIteratorMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// An opcode forged, consumed, copied, stored, called, returned, or
    /// reordered an internal catch marker.
    CatchMarkerStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// Control flow merged distinct catch identities, entered a handler
    /// normally, or mixed a catch marker with an ordinary JavaScript value.
    CatchMarkerJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with the established typed stack.
        incoming_from: BytecodePc,
    },
    /// A terminal path retained an internal catch marker.
    CatchMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// `define_method` is not paired with one immediately preceding typed
    /// ordinary-method closure.
    DefineMethodTemplateMismatch {
        /// Final bytecode position of `define_method`.
        pc: BytecodePc,
    },
    /// `define_class` is not paired with one immediately preceding typed
    /// base-class constructor closure.
    DefineClassTemplateMismatch {
        /// Final bytecode position of `define_class`.
        pc: BytecodePc,
    },
    /// An inferred-name opcode is not paired with an anonymous ordinary
    /// closure or isolated base-class definition on its unique incoming
    /// edge, or a computed name is detached from its data-property definition.
    SetNameTemplateMismatch {
        /// Final bytecode position of the inferred-name opcode.
        pc: BytecodePc,
    },
    /// `define_method` does not target one certified object-literal or class
    /// slot with the matching enumerability on every incoming control-flow
    /// path.
    DefineMethodTargetMismatch {
        /// Final bytecode position of `define_method`.
        pc: BytecodePc,
    },
    /// A base-class constructor closure was not consumed by exactly one
    /// paired `define_class` instruction.
    ClassConstructorTemplateOwnershipMismatch {
        /// Class-constructor template with the invalid ownership count.
        child: FunctionTemplateId,
        /// Number of paired `define_class` instructions that consumed it.
        definitions: u32,
    },
    /// `define_array_el` did not receive a key converted by `to_propkey`
    /// before its value was evaluated on the same fresh object literal.
    DefineArrayElementKeyMismatch {
        /// Final bytecode position of `define_array_el`.
        pc: BytecodePc,
    },
    /// `append` or one of its compiler-owned setup operations did not retain
    /// one unaliased `array_from` destination and its checked integer cursor.
    AppendOperandStackMismatch {
        /// Final bytecode position.
        pc: BytecodePc,
        /// Opcode whose typed inputs were invalid.
        opcode: FinalOpcode,
    },
    /// A terminal path retained compiler-internal array-append provenance.
    AppendMarkerAtExit {
        /// Terminal bytecode position.
        pc: BytecodePc,
    },
    /// Control flow merged distinct linear array-append ownership states.
    AppendProvenanceJoinMismatch {
        /// Join target.
        target: BytecodePc,
        /// Incoming edge that disagreed with established provenance.
        incoming_from: BytecodePc,
    },
    /// An ordinary-method template closure is not consumed by its one
    /// compiler-shaped `define_method` site.
    OrdinaryMethodTemplatePlacementMismatch {
        /// Final bytecode position of the method closure.
        pc: BytecodePc,
        /// Child template selected by the closure.
        child: FunctionTemplateId,
    },
    /// An ordinary-method template is not defined by exactly one parent site.
    OrdinaryMethodTemplateOwnershipMismatch {
        /// Method or accessor template.
        child: FunctionTemplateId,
        /// Validated `define_method` sites targeting the template.
        definitions: u32,
    },
}

impl fmt::Display for BytecodeVerificationErrorKind {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FunctionMetadataCountMismatch {
                functions,
                metadata,
            } => write!(
                formatter,
                "metadata count {metadata} does not equal function count {functions}"
            ),
            Self::LimitExceeded {
                resource,
                limit,
                observed,
            } => write!(
                formatter,
                "{resource} limit {limit} was exceeded by observed value {observed}"
            ),
            Self::AllocationFailed {
                resource,
                requested,
            } => write!(
                formatter,
                "could not allocate {requested} entries for {resource}"
            ),
            Self::GlobalScriptNotRoot => {
                formatter.write_str("Global Script executable is not the graph root")
            }
            Self::GlobalScriptHasArguments { defined, arguments } => write!(
                formatter,
                "Global Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::GlobalScriptHasFunctionName => formatter
                .write_str("Global Script carries function-name metadata or a self binding"),
            Self::IndirectEvalScriptNotRoot => {
                formatter.write_str("indirect-eval Script executable is not the graph root")
            }
            Self::IndirectEvalScriptHasArguments { defined, arguments } => write!(
                formatter,
                "indirect-eval Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::IndirectEvalScriptHasFunctionName => formatter
                .write_str("indirect-eval Script carries function-name metadata or a self binding"),
            Self::DirectEvalScriptNotRoot => {
                formatter.write_str("direct-eval Script executable is not the graph root")
            }
            Self::DirectEvalScriptHasArguments { defined, arguments } => write!(
                formatter,
                "direct-eval Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::DirectEvalScriptHasFunctionName => formatter
                .write_str("direct-eval Script carries function-name metadata or a self binding"),
            Self::DynamicFunctionScriptNotRoot => {
                formatter.write_str("dynamic-Function Script executable is not the graph root")
            }
            Self::DynamicFunctionScriptHasArguments { defined, arguments } => write!(
                formatter,
                "dynamic-Function Script declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::DynamicFunctionScriptHasFunctionName => formatter.write_str(
                "dynamic-Function Script carries function-name metadata or a self binding",
            ),
            Self::ModuleNotRoot => formatter.write_str("Module executable is not the graph root"),
            Self::ModuleHasArguments { defined, arguments } => write!(
                formatter,
                "Module root declares {defined} defined arguments and {arguments} frame arguments"
            ),
            Self::ModuleHasFunctionName => {
                formatter.write_str("Module root carries function-name metadata or a self binding")
            }
            Self::ModuleDeclarationRecordMissing => {
                formatter.write_str("Module root is missing its declaration record")
            }
            Self::ModuleDeclarationRecordUnexpected => {
                formatter.write_str("declaration record appears on a non-Module root")
            }
            Self::ModuleBindingSlotOutOfBounds {
                binding,
                slot,
                closures,
            } => write!(
                formatter,
                "module binding {binding} slot {slot} is outside root closure count {closures}"
            ),
            Self::ModuleBindingSlotMismatch { binding, slot } => write!(
                formatter,
                "module binding {binding} slot {slot} does not select a matching module cell"
            ),
            Self::ModuleBindingSlotOrder { binding, slot } => write!(
                formatter,
                "module binding {binding} slot {slot} is not dense and unique"
            ),
            Self::ModuleBindingPolicyMismatch { binding } => write!(
                formatter,
                "module binding {binding} policy is inconsistent with its origin"
            ),
            Self::ModuleBindingInitializerMismatch { binding, constant } => {
                write!(formatter, "module binding {binding} initializer")?;
                if let Some(constant) = constant {
                    write!(formatter, " constant {constant} is not a function template")?;
                } else {
                    formatter.write_str(" is absent for a function declaration")?;
                }
                Ok(())
            }
            Self::ModuleImportRequestOutOfBounds {
                binding,
                request,
                requests,
            } => write!(
                formatter,
                "module binding {binding} import request {request} is outside request count {requests}"
            ),
            Self::ModuleRequestSpecifierMissing { request } => {
                write!(formatter, "module request {request} has no specifier atom")
            }
            Self::OrdinaryMethodHasFunctionName => formatter.write_str(
                "ordinary method carries a source name before define_method initialization",
            ),
            Self::OrdinaryArrowHasFunctionName => {
                formatter.write_str("ordinary arrow carries source-name metadata or a self binding")
            }
            Self::ConstructorRealmGlobalSourceRequiresDynamicFunctionScript { closure } => write!(
                formatter,
                "closure slot {closure} originates a constructor-realm global outside a Script root"
            ),
            Self::DirectEvalBindingSourceRequiresDirectEvalScript { closure } => write!(
                formatter,
                "closure slot {closure} originates a caller binding outside a direct-eval Script root"
            ),
            Self::ClosureBindingOpcodeMismatch {
                closure,
                pc,
                opcode,
            } => write!(
                formatter,
                "opcode {opcode} at PC {pc} is incompatible with closure slot {closure}'s storage origin"
            ),
            Self::RealmGlobalDeleteBindingMissing { pc, atom } => write!(
                formatter,
                "delete_var at PC {pc} names atom {} without an eligible verified realm-global binding",
                atom.get()
            ),
            Self::UnsupportedFunctionHeader => {
                formatter.write_str("function header is outside its compiler executable profile")
            }
            Self::DefinedArgumentCountMismatch { defined, arguments } => write!(
                formatter,
                "defined argument count {defined} is incompatible with argument domain {arguments} and the parameter-list form"
            ),
            Self::VariableDefinitionCountMismatch { declared, entries } => write!(
                formatter,
                "variable definition count {entries} does not equal frame count {declared}"
            ),
            Self::ArgumentsObjectMetadataMismatch { definition, pc } => write!(
                formatter,
                "arguments-object metadata definition {definition:?} disagrees with initializer site {pc:?}"
            ),
            Self::ClosureDefinitionCountMismatch { declared, entries } => write!(
                formatter,
                "closure definition count {entries} does not equal closure domain {declared}"
            ),
            Self::MissingMetadataAtom { field } => {
                write!(formatter, "required metadata atom {field:?} is absent")
            }
            Self::MetadataAtomOutOfBounds { field, index, len } => write!(
                formatter,
                "metadata atom {field:?} index {index} is outside atom count {len}"
            ),
            Self::StaticPropertyOnlyMetadataAtom { field, index } => write!(
                formatter,
                "metadata atom {field:?} references static-property-only atom slot {index}"
            ),
            Self::BindingPolicyViolation { slot, pc, reason } => {
                write!(formatter, "binding policy {reason:?} for {slot:?}")?;
                if let Some(pc) = pc {
                    write!(formatter, " at PC {pc}")?;
                }
                Ok(())
            }
            Self::ArgumentScopeMetadataUnsupported { definition } => write!(
                formatter,
                "definition {definition} uses deferred parameter-expression scope metadata"
            ),
            Self::ScopeLinkOutOfBounds {
                definition,
                target,
                locals,
            } => write!(
                formatter,
                "definition {definition} links to local {target} outside local count {locals}"
            ),
            Self::ScopeLinkKindMismatch { definition, target } => write!(
                formatter,
                "definition {definition} links to local {target} with a different scope category"
            ),
            Self::ScopeLinkCycle { local } => {
                write!(
                    formatter,
                    "scope-link chain contains a cycle at local {local}"
                )
            }
            Self::EvalScopeIndexOutOfBounds {
                pc,
                scope_index,
                locals,
            } => write!(
                formatter,
                "eval scope index {scope_index} at PC {pc} is outside adjusted local count {locals}"
            ),
            Self::EvalScopeHeadNotLexical { pc, local } => write!(
                formatter,
                "eval scope head local {local} at PC {pc} is not lexical"
            ),
            Self::VariableReferenceOutOfBounds {
                definition,
                reference,
                len,
            } => write!(
                formatter,
                "definition {definition} variable reference {reference} is outside count {len}"
            ),
            Self::DuplicateVariableReference { reference } => {
                write!(
                    formatter,
                    "variable reference {reference} is assigned twice"
                )
            }
            Self::VariableReferenceDomainMismatch { declared, captured } => write!(
                formatter,
                "captured vardef count {captured} does not equal variable-reference count {declared}"
            ),
            Self::CaptureLayoutMismatch { reference } => write!(
                formatter,
                "vardef for variable reference {reference} disagrees with capture layout"
            ),
            Self::FunctionInitializerMetadataMismatch {
                definition,
                constant,
            } => write!(
                formatter,
                "function definition {definition} has invalid initializer constant {constant:?}"
            ),
            Self::FunctionInitializerOpcodeMismatch {
                definition,
                constant,
                matches,
            } => write!(
                formatter,
                "function definition {definition} constant {constant} has {matches} initializer pairs"
            ),
            Self::FunctionInitializerConstantReused {
                constant,
                first,
                duplicate,
            } => write!(
                formatter,
                "function constant {constant} initializes definitions {first} and {duplicate}"
            ),
            Self::FunctionInitializerPlacementMismatch { definition, pc } => write!(
                formatter,
                "function definition {definition} initializer at PC {pc} is outside the isolated entry prefix"
            ),
            Self::RealmGlobalFunctionInitializerMetadataMismatch { closure, constant } => write!(
                formatter,
                "realm-global function closure {closure} has invalid initializer constant {constant:?}"
            ),
            Self::RealmGlobalFunctionInitializerOpcodeMismatch {
                closure,
                constant,
                matches,
            } => write!(
                formatter,
                "realm-global function closure {closure} constant {constant} has {matches} initializer pairs"
            ),
            Self::RealmGlobalFunctionInitializerPlacementMismatch { closure, pc } => write!(
                formatter,
                "realm-global function closure {closure} initializer at PC {pc} is outside the isolated entry prefix"
            ),
            Self::RealmGlobalLexicalInitializerCountMismatch { closure, matches } => write!(
                formatter,
                "realm-global lexical closure {closure} has {matches} put_var_init sites"
            ),
            Self::FunctionTemplateOwnershipMismatch { child, incoming } => write!(
                formatter,
                "function template {child} has {incoming} incoming ownership edges"
            ),
            Self::ClosureMetadataMismatch { child, closure } => write!(
                formatter,
                "child function {child} closure {closure} disagrees with its parent"
            ),
            Self::EmptySourceDisplayName => formatter.write_str("source display name is empty"),
            Self::InvalidSourceSpan { span } => {
                write!(formatter, "invalid source span {span:?}")
            }
            Self::FunctionNameSourceMismatch => {
                formatter.write_str("function name atom and source span presence differ")
            }
            Self::FunctionNameOutsideFunction => {
                formatter.write_str("function name span is outside function span")
            }
            Self::SourceMappingCountMismatch {
                instructions,
                mappings,
            } => write!(
                formatter,
                "source mapping count {mappings} does not equal instruction count {instructions}"
            ),
            Self::StrictModeInstructionCountOutOfBounds {
                strict_instructions,
                instructions,
            } => write!(
                formatter,
                "strict-source instruction count {strict_instructions} exceeds instruction count {instructions}"
            ),
            Self::StrictModePcNotIncreasing {
                index,
                previous,
                current,
            } => write!(
                formatter,
                "strict-source PC {index} uses {current}, which does not follow {previous}"
            ),
            Self::StrictModePcNotInstruction { index, pc } => write!(
                formatter,
                "strict-source PC {index} uses {pc}, which is not an instruction boundary"
            ),
            Self::SourcePcMismatch {
                mapping,
                declared,
                actual,
            } => write!(
                formatter,
                "source mapping {mapping} uses PC {declared}, expected {actual}"
            ),
            Self::InstructionSourceOutsideFunction { mapping } => write!(
                formatter,
                "source mapping {mapping} is outside its function span"
            ),
            Self::UnsupportedCompilerOpcode { pc, opcode } => {
                write!(
                    formatter,
                    "opcode {opcode:?} at PC {pc} is outside compiler profile"
                )
            }
            Self::GosubSiteCountOutOfRange { sites, maximum } => write!(
                formatter,
                "gosub site count {sites} exceeds compatibility maximum {maximum}"
            ),
            Self::FinallyReturnStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed finally return-address stack"
            ),
            Self::FinallyReturnJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed finally return-address stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::FinallyReturnMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains a malformed finally return-address marker"
            ),
            Self::CapturedReferenceStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed captured-binding reference stack"
            ),
            Self::CapturedReferenceJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed captured-binding reference stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::CapturedReferenceMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains an internal captured-binding reference"
            ),
            Self::ForInIteratorStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed for-in iterator stack"
            ),
            Self::ForInIteratorJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed for-in iterator stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::ForInIteratorMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains an internal for-in iterator marker"
            ),
            Self::ForOfIteratorStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed for-of iterator stack"
            ),
            Self::ForOfIteratorJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed for-of iterator stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::ForOfIteratorMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains an internal for-of iterator marker"
            ),
            Self::CatchMarkerStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} violates the typed catch-marker stack"
            ),
            Self::CatchMarkerJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "typed catch-marker stack at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::CatchMarkerAtExit { pc } => {
                write!(
                    formatter,
                    "terminal at PC {pc} retains an internal catch marker"
                )
            }
            Self::DefineMethodTemplateMismatch { pc } => write!(
                formatter,
                "define_method at PC {pc} is not paired with one typed method closure"
            ),
            Self::DefineClassTemplateMismatch { pc } => write!(
                formatter,
                "define_class at PC {pc} is not paired with one typed class-constructor closure"
            ),
            Self::SetNameTemplateMismatch { pc } => write!(
                formatter,
                "inferred-name opcode at PC {pc} is not paired with one anonymous ordinary-function closure and its required definition"
            ),
            Self::DefineMethodTargetMismatch { pc } => write!(
                formatter,
                "define_method at PC {pc} does not target one certified object or class slot"
            ),
            Self::ClassConstructorTemplateOwnershipMismatch { child, definitions } => write!(
                formatter,
                "class-constructor template {child:?} is consumed by {definitions} define_class instructions"
            ),
            Self::DefineArrayElementKeyMismatch { pc } => write!(
                formatter,
                "define_array_el at PC {pc} does not use a key converted before its value on one fresh object literal"
            ),
            Self::AppendOperandStackMismatch { pc, opcode } => write!(
                formatter,
                "opcode {opcode:?} at PC {pc} does not retain one fresh array_from destination and checked append cursor"
            ),
            Self::AppendMarkerAtExit { pc } => write!(
                formatter,
                "terminal at PC {pc} retains compiler-internal array append provenance"
            ),
            Self::AppendProvenanceJoinMismatch {
                target,
                incoming_from,
            } => write!(
                formatter,
                "linear array append provenance at PC {target} disagrees with the edge from PC {incoming_from}"
            ),
            Self::OrdinaryMethodTemplatePlacementMismatch { pc, child } => write!(
                formatter,
                "ordinary-method template {child} closure at PC {pc} is not consumed by define_method"
            ),
            Self::OrdinaryMethodTemplateOwnershipMismatch { child, definitions } => write!(
                formatter,
                "ordinary-method template {child} has {definitions} define_method sites"
            ),
        }
    }
}
