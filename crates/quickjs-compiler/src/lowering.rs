use std::{collections::HashMap, error::Error, fmt, ops::Range, sync::Arc};

use oxc_ast::{
    AstKind,
    ast::{
        AssignmentExpression, AssignmentTarget, BindingPattern, BlockStatement, CallExpression,
        ConditionalExpression, DoWhileStatement, Expression, ExpressionStatement, ForStatement,
        ForStatementInit, Function, FunctionBody, FunctionType, IdentifierReference, IfStatement,
        LogicalExpression, NewExpression, ObjectExpression, ObjectPropertyKind, Program,
        PropertyKey as OxcPropertyKey, PropertyKind, ReturnStatement, SequenceExpression,
        SimpleAssignmentTarget, Statement, StaticMemberExpression, ThrowStatement, UnaryExpression,
        UpdateExpression, VariableDeclaration, VariableDeclarationKind, WhileStatement,
    },
};
use oxc_semantic::{NodeId, ReferenceId, ScopeId, SymbolId};
use oxc_span::GetSpan;
use oxc_syntax::operator::{
    AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator, UpdateOperator,
};
use quickjs_bytecode::{
    AssemblerError, AssemblerLabel, AssemblerLimits, AtomPoolIndex, Binary64Constant, BranchKind,
    BytecodeAssembler, BytecodeGraphVerificationLimits, BytecodePc, BytecodeVerificationError,
    ClosureVariableDefinition as VerifiedClosureVariableDefinition, CompilerAtom,
    CompilerBindingKind as VerifiedBindingKind, CompilerBindingPolicy, CompilerCaptureLayout,
    CompilerCapturedBinding, CompilerClosureSource as CompilerGraphClosureSource,
    CompilerConstant as CompilerGraphConstant, CompilerConstantKind, CompilerConstantLayout,
    CompilerConstantValue, CompilerExecutableKind,
    CompilerInitializationPolicy as VerifiedInitializationPolicy, CompilerSource, CompilerString,
    CompilerStringError, CompilerWritePolicy as VerifiedWritePolicy, EncodeError, FinalOpcode,
    FunctionGraphVerificationError, FunctionGraphVerificationLimits, FunctionIndexDomains,
    FunctionTemplateId, MAX_FUNCTION_INDEX_ENTRIES, Operands, PcSourceSpan, ScopeLink,
    SourceByteSpan, UnverifiedCompilerBytecodeGraph, UnverifiedCompilerFunction,
    UnverifiedCompilerFunctionBody, UnverifiedCompilerFunctionGraph, UnverifiedFunctionHeader,
    UnverifiedFunctionMetadata, VariableDefinition, VerificationError, VerificationErrorKind,
    VerificationLimits, VerifiedBytecode, VerifiedCompilerFunctionGraph, VerifiedControlFlow,
    verify_compiler_bytecode_graph, verify_compiler_control_flow, verify_compiler_function_graph,
};
use quickjs_frontend::{
    CompilationGoal, DynamicFunctionKind, OxcStringDecodeError, ParsedUnit, Span,
    decode_oxc_cooked_string,
};

use crate::storage::{
    BindingId, CaptureSlot, CaptureSource, CompilationUnitKind, CompilerError, DeclarationKind,
    DeclarationPolicy, Executable, ExecutableId, ExecutableKind, InitializationPolicy,
    NativeReferenceId, PlannedStorage, ReferenceAccess, StoragePlacement, StoragePlan,
    UnresolvedGlobalId, WritePolicy, build_planned_storage,
};

/// A validated function-local slot number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LocalSlot(u16);

impl LocalSlot {
    /// Returns the encoded zero-based local index.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }
}

/// One compiler binding assigned to a function-local slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LoweredLocal {
    binding: BindingId,
    slot: LocalSlot,
}

impl LoweredLocal {
    /// Returns the compiler binding stored in this slot.
    #[must_use]
    pub const fn binding(self) -> BindingId {
        self.binding
    }

    /// Returns the function-local slot.
    #[must_use]
    pub const fn slot(self) -> LocalSlot {
        self.slot
    }
}

/// The source span associated with one emitted final instruction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceInstruction {
    pc: BytecodePc,
    span: Span,
}

impl SourceInstruction {
    /// Returns the final instruction's starting bytecode position.
    #[must_use]
    pub const fn pc(self) -> BytecodePc {
        self.pc
    }

    /// Returns the byte span in the retained source text.
    #[must_use]
    pub const fn span(self) -> Span {
        self.span
    }
}

/// One owned entry in a compiled function's heterogeneous constant pool.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CompiledConstant {
    /// An ordinary JavaScript value.
    Value(CompilerConstantValue),
    /// A nested-function template.
    Function(CompiledFunctionConstant),
}

impl CompiledConstant {
    /// Returns the verifier-visible constant kind.
    #[must_use]
    pub const fn kind(&self) -> CompilerConstantKind {
        match self {
            Self::Value(_) => CompilerConstantKind::Value,
            Self::Function(_) => CompilerConstantKind::Function,
        }
    }

    /// Returns the value payload when this is an ordinary value constant.
    #[must_use]
    pub const fn value(&self) -> Option<&CompilerConstantValue> {
        match self {
            Self::Value(value) => Some(value),
            Self::Function(_) => None,
        }
    }

    /// Returns the template payload when this is a function constant.
    #[must_use]
    pub const fn function(&self) -> Option<CompiledFunctionConstant> {
        match self {
            Self::Value(_) => None,
            Self::Function(function) => Some(*function),
        }
    }
}

/// One nested-function template stored in a compiled function's constant pool.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CompiledFunctionConstant {
    executable: ExecutableId,
}

impl CompiledFunctionConstant {
    /// Returns the exact child executable represented by this pool entry.
    #[must_use]
    pub const fn executable(self) -> ExecutableId {
        self.executable
    }
}

/// Verified source of one imported closure cell.
///
/// Compiler output normalizes `QuickJS`'s parent argument/local descriptors to
/// the parent's dense own-variable-reference table. This lets runtime closure
/// construction address cells directly while the retained parent capture
/// layout still identifies the underlying argument or local slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CompiledClosureSource {
    /// A cell owned by the immediately enclosing activation.
    ParentVariableReference(u16),
    /// A cell imported by the immediately enclosing function object.
    ParentClosure(u16),
}

/// Dense compiler identity of one constructor-realm global name.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RealmGlobalId(u32);

impl RealmGlobalId {
    /// Returns the dense zero-based global-name index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Source of one constructor-realm global slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CompiledRealmGlobalSource {
    /// The dynamic Script root resolves this name in its constructor realm.
    ConstructorRealm,
    /// A child forwards the same realm-owned handle from its parent.
    ParentClosure(u16),
}

/// One dense constructor-realm global descriptor for a compiled function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledRealmGlobal {
    id: RealmGlobalId,
    name: Arc<str>,
    atom: AtomPoolIndex,
    slot: u16,
    source: CompiledRealmGlobalSource,
    policy: CompilerBindingPolicy,
    function_initializer: Option<u32>,
}

impl CompiledRealmGlobal {
    /// Returns the compilation-unit global-name identity.
    #[must_use]
    pub const fn id(&self) -> RealmGlobalId {
        self.id
    }

    /// Returns the exact declared or unresolved identifier text.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the function-local atom naming this realm binding.
    #[must_use]
    pub const fn atom(&self) -> AtomPoolIndex {
        self.atom
    }

    /// Returns the dense function-local closure-domain slot.
    #[must_use]
    pub const fn slot(&self) -> u16 {
        self.slot
    }

    /// Returns whether this function originates or forwards the realm handle.
    #[must_use]
    pub const fn source(&self) -> CompiledRealmGlobalSource {
        self.source
    }

    /// Returns whether this name is an unresolved lookup, property-backed
    /// `var`, or hoisted function declaration.
    #[must_use]
    pub const fn policy(&self) -> CompilerBindingPolicy {
        self.policy
    }

    /// Returns the root-only function-template initializer for a declared
    /// constructor-realm function.
    #[must_use]
    pub const fn function_initializer(&self) -> Option<u32> {
        self.function_initializer
    }
}

/// One dense imported-closure descriptor for a compiled function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompiledClosureVariable {
    binding: BindingId,
    slot: CaptureSlot,
    source: CompiledClosureSource,
    policy: DeclarationPolicy,
}

impl CompiledClosureVariable {
    /// Returns the original compiler binding represented by this cell.
    #[must_use]
    pub const fn binding(self) -> BindingId {
        self.binding
    }

    /// Returns the dense closure-variable slot in the child function.
    #[must_use]
    pub const fn slot(self) -> CaptureSlot {
        self.slot
    }

    /// Returns where the immediate parent provides the cell.
    #[must_use]
    pub const fn source(self) -> CompiledClosureSource {
        self.source
    }

    /// Returns the original binding's initialization and write policy.
    #[must_use]
    pub const fn policy(self) -> DeclarationPolicy {
        self.policy
    }
}

/// Owned output from validated executable-body lowering.
///
/// This per-function staging artifact is deliberately not execution authority
/// by itself. [`CompiledFunctionTree::verified_bytecode`] returns the final
/// code-and-metadata authority for the complete selected subtree. Program-root
/// synthetic locals are represented in verified metadata, not as source
/// [`LoweredLocal`] bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledFunction {
    executable: ExecutableId,
    storage_plan: Arc<StoragePlan>,
    source_text: Arc<str>,
    locals: Arc<[LoweredLocal]>,
    atoms: Arc<[CompilerAtom]>,
    constants: Arc<[CompiledConstant]>,
    closure_variables: Arc<[CompiledClosureVariable]>,
    realm_globals: Arc<[CompiledRealmGlobal]>,
    source_instructions: Arc<[SourceInstruction]>,
    control_flow: Arc<VerifiedControlFlow>,
    metadata: UnverifiedFunctionMetadata,
}

impl CompiledFunction {
    /// Returns the selected compiler-owned executable identity.
    #[must_use]
    pub const fn executable(&self) -> ExecutableId {
        self.executable
    }

    /// Returns the immutable storage plan that issued the executable identity.
    #[must_use]
    pub fn storage_plan(&self) -> &StoragePlan {
        &self.storage_plan
    }

    /// Returns the exact source text whose Oxc model was lowered.
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    /// Returns the selected executable's dense source-binding local layout.
    ///
    /// Compiler-internal locals such as dynamic Script completion storage are
    /// intentionally absent; the verified function domains and metadata remain
    /// the execution-authority layout.
    #[must_use]
    pub fn locals(&self) -> &[LoweredLocal] {
        &self.locals
    }

    /// Returns exact content-interned atoms in function-local index order.
    #[must_use]
    pub fn atoms(&self) -> &[CompilerAtom] {
        &self.atoms
    }

    /// Returns the complete typed constant pool in deterministic allocation order.
    #[must_use]
    pub fn constants(&self) -> &[CompiledConstant] {
        &self.constants
    }

    /// Returns imported closure cells in dense child slot order.
    #[must_use]
    pub fn closure_variables(&self) -> &[CompiledClosureVariable] {
        &self.closure_variables
    }

    /// Returns constructor-realm globals in their dense closure-slot order.
    #[must_use]
    pub fn realm_globals(&self) -> &[CompiledRealmGlobal] {
        &self.realm_globals
    }

    /// Returns source spans for final instructions in bytecode order.
    #[must_use]
    pub fn source_instructions(&self) -> &[SourceInstruction] {
        &self.source_instructions
    }

    /// Returns the non-executable staged verifier certificate.
    #[must_use]
    pub fn control_flow(&self) -> &VerifiedControlFlow {
        &self.control_flow
    }
}

/// Backward-compatible name for the nested-function-free
/// [`CompilationContext::compile_leaf`] result.
pub type CompiledLeafFunction = CompiledFunction;

/// Failure-atomic output for one complete compiled executable subtree.
///
/// Functions are stored in stable executable preorder. Every constant edge
/// names one member of this same tree. A selected nested root is accepted only
/// when it imports no cells from its omitted external parent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledFunctionTree {
    root: ExecutableId,
    storage_plan: Arc<StoragePlan>,
    source_text: Arc<str>,
    functions: Arc<[CompiledFunction]>,
    function_graph: Arc<VerifiedCompilerFunctionGraph>,
    verified_bytecode: Arc<VerifiedBytecode>,
}

impl CompiledFunctionTree {
    /// Returns the selected root executable.
    #[must_use]
    pub const fn root_executable(&self) -> ExecutableId {
        self.root
    }

    /// Returns the selected root function.
    #[must_use]
    pub fn root(&self) -> &CompiledFunction {
        &self.functions[0]
    }

    /// Returns all compiled functions in stable executable preorder.
    #[must_use]
    pub fn functions(&self) -> &[CompiledFunction] {
        &self.functions
    }

    /// Returns the cross-function certificate for this complete tree.
    ///
    /// Graph-local template identities index the same order as [`Self::functions`].
    /// The certificate remains non-executable until complete runtime metadata
    /// and typed-stack capabilities are verified.
    #[must_use]
    pub fn function_graph(&self) -> &VerifiedCompilerFunctionGraph {
        &self.function_graph
    }

    /// Returns immutable execution authority for this complete function tree.
    #[must_use]
    pub fn verified_bytecode(&self) -> &VerifiedBytecode {
        &self.verified_bytecode
    }

    /// Resolves one graph-local template identity to its compiler artifact.
    #[must_use]
    pub fn function_by_template(&self, template: FunctionTemplateId) -> Option<&CompiledFunction> {
        let index = usize::try_from(template.get()).ok()?;
        self.functions.get(index)
    }

    /// Resolves one compiled executable in the selected subtree.
    #[must_use]
    pub fn function(&self, executable: ExecutableId) -> Option<&CompiledFunction> {
        let index = self
            .functions
            .binary_search_by_key(&executable, CompiledFunction::executable)
            .ok()?;
        self.functions.get(index)
    }

    /// Returns the immutable storage plan shared by every function.
    #[must_use]
    pub fn storage_plan(&self) -> &StoragePlan {
        &self.storage_plan
    }

    /// Returns the exact retained source text.
    #[must_use]
    pub fn source_text(&self) -> &str {
        &self.source_text
    }
}

#[derive(Debug)]
struct ContextIdentity;

/// An owned executable selection issued by one [`CompilationContext`].
///
/// Its private context identity prevents a numerically equal
/// [`ExecutableId`] from another storage plan from selecting the wrong body.
#[derive(Clone, Debug)]
pub struct CompilationExecutable {
    context_identity: Arc<ContextIdentity>,
    executable: Executable,
}

impl CompilationExecutable {
    /// Returns the selected plan-local executable identity.
    #[must_use]
    pub const fn id(&self) -> ExecutableId {
        self.executable.id()
    }

    /// Returns the selected executable metadata.
    #[must_use]
    pub const fn metadata(&self) -> &Executable {
        &self.executable
    }
}

/// Arena-borrowing compiler state that keeps Oxc identities isolated.
///
/// No Oxc node, symbol, scope, or reference identity escapes through compiled
/// output. The context is immutable and does not need a lock.
pub struct CompilationContext<'unit, 'arena, 'scope> {
    unit: &'unit ParsedUnit<'arena, 'scope>,
    planned: PlannedStorage,
    source_text: Arc<str>,
    source_name: Arc<str>,
    identity: Arc<ContextIdentity>,
}

impl<'unit, 'arena, 'scope> CompilationContext<'unit, 'arena, 'scope> {
    /// Builds storage and private Oxc-to-compiler identity tables.
    ///
    /// # Errors
    ///
    /// Returns the storage planner's typed failure for unsupported dynamic
    /// binding behavior or inconsistent retained semantics.
    pub fn new(unit: &'unit ParsedUnit<'arena, 'scope>) -> Result<Self, CompilerError> {
        Self::new_with_source_name(unit, Arc::from("<input>"))
    }

    /// Builds compiler state with an explicit retained source display name.
    ///
    /// # Errors
    ///
    /// Returns the storage planner's typed failure or rejects an empty source
    /// identity before any compiled artifact is produced.
    pub fn new_with_source_name(
        unit: &'unit ParsedUnit<'arena, 'scope>,
        source_name: Arc<str>,
    ) -> Result<Self, CompilerError> {
        if source_name.is_empty() {
            return Err(CompilerError::SemanticInvariant {
                invariant: "nonempty compiler source display name",
                span: None,
            });
        }
        let planned = build_planned_storage(unit)?;
        let source_text = Arc::from(unit.program().source_text);
        Ok(Self {
            unit,
            planned,
            source_text,
            source_name,
            identity: Arc::new(ContextIdentity),
        })
    }

    /// Returns the arena-independent storage plan.
    #[must_use]
    pub fn storage_plan(&self) -> &StoragePlan {
        &self.planned.plan
    }

    /// Returns context-issued executable selections in storage-plan order.
    ///
    /// The selections are owned and contain no Oxc identity, but only this
    /// context accepts them for lowering.
    #[must_use]
    pub fn executables(
        &self,
    ) -> impl ExactSizeIterator<Item = CompilationExecutable> + DoubleEndedIterator + '_ {
        self.planned
            .plan
            .executables()
            .iter()
            .cloned()
            .map(|executable| CompilationExecutable {
                context_identity: Arc::clone(&self.identity),
                executable,
            })
    }

    /// Lowers one validated ordinary leaf-function family to final bytecode.
    ///
    /// The accepted Script-only family contains no nested executable. It
    /// supports simple local declarations, immediate primitive values,
    /// resolved argument/local reads and mutable writes, value operators
    /// including short-circuit and conditional expressions, lexical blocks,
    /// `if`/`else`, `while`, `do`/`while`, classic `for`, unlabeled
    /// `break`/`continue`, expression statements, and explicit or implicit
    /// returns. A leaf may own ordinary value constants and may read or write
    /// frame cells captured from an ancestor. The entire function is converted
    /// to typed symbolic instructions before branch relaxation emits any bytes.
    /// A selected static ordinary method/accessor may be staged here for
    /// inspection, but this leaf artifact is never execution authority; only
    /// [`Self::compile_tree`] on its owning parent can certify and publish the
    /// required object-literal `DefineMethod` site.
    ///
    /// # Errors
    ///
    /// Rejects foreign executable selections, unsupported source structure,
    /// inconsistent semantic identities, assembler resource or encoding
    /// failures, and verifier failures.
    pub fn compile_leaf(
        &self,
        selection: &CompilationExecutable,
        limits: VerificationLimits,
    ) -> Result<CompiledLeafFunction, LeafCompilationError> {
        self.reject_dynamic_function_subtree_entry()?;
        let executable = self.resolve_selection(selection)?;
        let tree_layout = self.function_tree_layout()?;
        if let Some(child) = tree_layout.children(executable)?.first() {
            let child = self
                .planned
                .plan
                .executable(*child)
                .ok_or(LeafCompilationError::InvalidExecutable { executable: *child })?;
            return unsupported(UnsupportedLeafFeature::NestedExecutable, child.span());
        }
        self.compile_function(executable, &tree_layout, limits)
    }

    /// Lowers an ordinary function and every nested ordinary function template.
    ///
    /// Compilation is child-first and iterative. The returned flat tree is in
    /// stable executable preorder. Each function's heterogeneous constant pool
    /// retains Number values and tagged-integer String values requiring pool
    /// storage plus direct nested-function templates in source order without
    /// deduplication. Other nonempty strings use a separate content-interned
    /// function-local atom table. Values and templates share one constant index
    /// domain: indices below 256 use compact instructions and later entries use
    /// wide instructions. Imported closure descriptors are normalized to the
    /// parent's own cell table or imported environment.
    ///
    /// # Errors
    ///
    /// Rejects foreign selections, unsupported executable kinds or nested
    /// syntax, inconsistent semantic identities or closure edges, resource
    /// limits, assembly failures, and staged verifier failures. No partial tree
    /// escapes on failure.
    pub fn compile_tree(
        &self,
        selection: &CompilationExecutable,
        limits: VerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.compile_tree_with_graph_limits(
            selection,
            limits,
            FunctionGraphVerificationLimits::default(),
        )
    }

    /// Lowers and cross-checks an ordinary function subtree with explicit
    /// aggregate graph limits.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::compile_tree`], plus a structured
    /// graph-verification failure when an aggregate limit is exceeded.
    pub fn compile_tree_with_graph_limits(
        &self,
        selection: &CompilationExecutable,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.compile_tree_with_all_limits(
            selection,
            limits,
            graph_limits,
            BytecodeGraphVerificationLimits::default(),
        )
    }

    /// Lowers and final-verifies an ordinary function subtree with every
    /// body, staged-graph, metadata, source, and frame-analysis limit explicit.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::compile_tree`], including a
    /// structured final-verifier failure when complete metadata is invalid or
    /// a final resource budget is exceeded.
    pub fn compile_tree_with_all_limits(
        &self,
        selection: &CompilationExecutable,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.reject_dynamic_function_subtree_entry()?;
        let root = self.resolve_selection(selection)?;
        self.compile_subtree_with_all_limits(root, limits, graph_limits, bytecode_limits)
    }

    /// Lowers the complete exact wrapper Script for an ordinary dynamic
    /// `Function` constructor invocation.
    ///
    /// The Program root and every nested function template are compiled and
    /// final-verified as one indivisible authority. No API on this context can
    /// extract the synthetic named wrapper function from a dynamic source unit.
    ///
    /// # Errors
    ///
    /// Rejects every non-dynamic source unit, nonordinary dynamic-function
    /// family, unsupported declaration or syntax, resource limit, and staged
    /// or final verification failure. Program `var` and function declarations
    /// plus unresolved identifier references are typed as constructor-realm
    /// globals; functions are initialized from the last duplicate declaration.
    /// Program `let` and `const` remain evaluation-local frame cells. Neither
    /// domain becomes a caller-frame capture.
    pub fn compile_dynamic_function_script(
        &self,
        limits: VerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.compile_dynamic_function_script_with_all_limits(
            limits,
            FunctionGraphVerificationLimits::default(),
            BytecodeGraphVerificationLimits::default(),
        )
    }

    /// Lowers a complete ordinary dynamic-Function Script with every staged
    /// and final graph limit explicit.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::compile_dynamic_function_script`].
    pub fn compile_dynamic_function_script_with_all_limits(
        &self,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) {
            return unsupported(
                UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                self.unit.program().span,
            );
        }
        let root = self
            .planned
            .plan
            .executables()
            .first()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Function storage plan has a Program root",
                span: Some(self.unit.program().span),
            })?
            .id();
        self.compile_subtree_with_all_limits(root, limits, graph_limits, bytecode_limits)
    }

    fn compile_subtree_with_all_limits(
        &self,
        root: ExecutableId,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        let tree_layout = self.function_tree_layout()?;
        let subtree = tree_layout.subtree_preorder(root)?;
        let mut functions = Vec::with_capacity(subtree.len());
        for executable in subtree.iter().rev().copied() {
            functions.push(self.compile_function(executable, &tree_layout, limits)?);
        }
        functions.reverse();
        let functions: Arc<[CompiledFunction]> = functions.into();
        let function_graph = Arc::new(verify_compiled_function_graph(
            root,
            &functions,
            graph_limits,
        )?);
        let verified_bytecode = Arc::new(
            verify_compiler_bytecode_graph(
                UnverifiedCompilerBytecodeGraph::new(
                    Arc::clone(&function_graph),
                    functions
                        .iter()
                        .map(|function| function.metadata.clone())
                        .collect::<Vec<_>>()
                        .into(),
                ),
                bytecode_limits,
            )
            .map_err(|source| {
                let span = source
                    .function_id()
                    .and_then(|template| usize::try_from(template.get()).ok())
                    .and_then(|index| functions.get(index))
                    .and_then(|function| {
                        function
                            .storage_plan
                            .executable(function.executable)
                            .map(Executable::span)
                    });
                LeafCompilationError::BytecodeGraphVerification { span, source }
            })?,
        );
        Ok(CompiledFunctionTree {
            root,
            storage_plan: Arc::clone(&self.planned.plan),
            source_text: Arc::clone(&self.source_text),
            functions,
            function_graph,
            verified_bytecode,
        })
    }

    fn reject_dynamic_function_subtree_entry(&self) -> Result<(), LeafCompilationError> {
        if matches!(self.unit.goal(), CompilationGoal::DynamicFunction(_)) {
            return unsupported(
                UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                self.unit.program().span,
            );
        }
        Ok(())
    }

    fn compile_function(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<CompiledFunction, LeafCompilationError> {
        let validated = self.validate_executable(executable, tree_layout, limits)?;
        let ValidatedFunction {
            executable_kind,
            strict,
            argument_count,
            local_count,
            capture_count,
            capture_layout,
            locals,
            constants,
            atoms,
            closure_variables,
            realm_globals,
            function_name,
            variable_definitions,
            closure_definitions,
            function_span,
            function_name_span,
            flow,
        } = validated;
        let atom_count =
            u32::try_from(atoms.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "atom pool entries",
            })?;
        let constant_count =
            u32::try_from(constants.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "constant pool entries",
            })?;
        let domains = FunctionIndexDomains::new(
            atom_count,
            constant_count,
            argument_count,
            local_count,
            capture_count,
        );
        let variable_reference_count = checked_function_entry_count(
            capture_layout.bindings().len(),
            "function variable references",
        )?;
        let header = executable_header(
            executable_kind,
            strict,
            argument_count,
            variable_reference_count,
        );
        let constant_layout = CompilerConstantLayout::new(
            constants
                .iter()
                .map(CompiledConstant::kind)
                .collect::<Vec<_>>()
                .into(),
        );
        let finished = flow.finish()?;
        let (source_instructions, control_flow) = finished.verify_with_layouts(
            domains,
            header,
            capture_layout,
            constant_layout,
            limits,
        )?;
        let source_mappings = source_instructions
            .iter()
            .map(|instruction| {
                PcSourceSpan::new(instruction.pc(), source_byte_span(instruction.span()))
            })
            .collect::<Vec<_>>();
        let metadata = UnverifiedFunctionMetadata::new(
            function_name,
            variable_definitions.into(),
            closure_definitions.into(),
            CompilerSource::new(
                Arc::clone(&self.source_name),
                Arc::clone(&self.source_text),
                function_span,
                function_name_span,
                source_mappings.into(),
            ),
        )
        .with_executable_kind(executable_kind);

        Ok(CompiledFunction {
            executable,
            storage_plan: Arc::clone(&self.planned.plan),
            source_text: Arc::clone(&self.source_text),
            locals: locals.into(),
            atoms,
            constants,
            closure_variables: closure_variables.into(),
            realm_globals: realm_globals.into(),
            source_instructions: source_instructions.into(),
            control_flow: Arc::new(control_flow),
            metadata,
        })
    }

    fn validate_executable(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        match metadata.kind() {
            ExecutableKind::Script {
                asynchronous: false,
            } => self.validate_dynamic_function_script(executable, tree_layout, limits),
            _ => self.validate_function(executable, tree_layout, limits),
        }
    }

    fn resolve_selection(
        &self,
        selection: &CompilationExecutable,
    ) -> Result<ExecutableId, LeafCompilationError> {
        let executable = selection.id();
        if !Arc::ptr_eq(&self.identity, &selection.context_identity) {
            return Err(LeafCompilationError::ForeignExecutable { executable });
        }
        let planned = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if planned != selection.metadata() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "context-issued executable metadata is immutable",
                span: Some(selection.metadata().span()),
            });
        }
        Ok(executable)
    }

    fn function_tree_layout(&self) -> Result<FunctionTreeLayout, LeafCompilationError> {
        let mut layout = FunctionTreeLayout::new(
            &self.planned.plan,
            self.unit.goal() == CompilationGoal::DynamicFunction(DynamicFunctionKind::Function),
        )?;
        for executable in self.planned.plan.executables() {
            let node_id = self
                .planned
                .identities
                .node_by_executable
                .get(executable.id().index())
                .copied()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "executable has an Oxc node identity",
                    span: Some(executable.span()),
                })?;
            let AstKind::Function(function) = self.unit.semantic().nodes().kind(node_id) else {
                continue;
            };
            if function.r#type != FunctionType::FunctionDeclaration {
                continue;
            }
            let Some(identifier) = &function.id else {
                continue;
            };
            let binding =
                self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "function declaration has compiler storage",
                    span: Some(identifier.span),
                },
            )?;
            if executable.parent() != Some(storage.executable()) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "function declaration binding belongs to its parent executable",
                    span: Some(identifier.span),
                });
            }
            layout.record_function_declaration(binding, executable.id())?;
        }
        let constant_pools = self.compiled_constant_pools(&layout)?;
        layout.install_constant_pools(constant_pools)?;
        Ok(layout)
    }

    fn validate_function(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, function, form) = self.selected_ordinary_function(executable_id)?;
        let layout = FrameLayout::new(&self.planned.plan, executable_id)?;
        let body = function
            .body
            .as_ref()
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBody,
                span: function.span,
            })?;
        let mut flow = PlannedControlFlow::new(limits);
        let constants = tree_layout.constant_pool(executable_id)?;
        let planning = FunctionPlanningContext {
            executable: executable_id,
            layout: &layout,
            tree_layout,
            constants,
        };
        self.validate_body(function, body, &planning, &mut flow)?;
        let function_scope = self.created_scope(
            function.scope_id.get(),
            function.node_id.get(),
            function.span,
        )?;
        let capture_layout =
            self.compiler_capture_layout(executable_id, function_scope, &layout, tree_layout)?;
        let closure_variables = self.compiled_closure_variables(executable_id, tree_layout)?;
        let realm_globals = self.compiled_realm_globals(executable_id, tree_layout, constants)?;
        let (executable_kind, function_span, function_name, function_name_span) = match form {
            OrdinaryFunctionForm::Function => (
                CompilerExecutableKind::OrdinaryFunction,
                function.span,
                executable
                    .name()
                    .map(|_| constants.metadata_atom_index(CompiledMetadataAtomKey::FunctionName))
                    .transpose()?,
                executable.name_span().map(source_byte_span),
            ),
            OrdinaryFunctionForm::ObjectMethod {
                property_span: source_span,
            } => (
                CompilerExecutableKind::OrdinaryMethod,
                source_span,
                None,
                None,
            ),
        };
        let variable_definitions = self.compiled_variable_definitions(
            executable_id,
            function_scope,
            &layout,
            tree_layout,
            constants,
        )?;
        let closure_definitions =
            self.compiled_closure_definitions(&closure_variables, &realm_globals, constants)?;
        let capture_count = checked_function_entry_count(
            closure_variables
                .len()
                .checked_add(realm_globals.len())
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function closure variables",
                })?,
            "function closure variables",
        )?;

        Ok(ValidatedFunction {
            executable_kind,
            strict: executable.is_strict(),
            argument_count: executable.parameter_count(),
            local_count: layout.local_count,
            capture_count,
            capture_layout,
            locals: layout
                .locals
                .iter()
                .map(|local| LoweredLocal {
                    binding: local.binding,
                    slot: local.slot,
                })
                .collect(),
            constants: Arc::clone(&constants.entries),
            atoms: Arc::clone(&constants.atoms),
            closure_variables,
            realm_globals,
            function_name,
            variable_definitions,
            closure_definitions,
            function_span: source_byte_span(function_span),
            function_name_span,
            flow,
        })
    }

    fn validate_dynamic_function_script(
        &self,
        executable_id: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        limits: VerificationLimits,
    ) -> Result<ValidatedFunction, LeafCompilationError> {
        let (executable, program) = self.selected_dynamic_function_script(executable_id)?;
        let mut layout = FrameLayout::new(&self.planned.plan, executable_id)?;
        let completion = layout.push_internal_local()?;
        let mut flow = PlannedControlFlow::new(limits);
        let constants = tree_layout.constant_pool(executable_id)?;
        let planning = FunctionPlanningContext {
            executable: executable_id,
            layout: &layout,
            tree_layout,
            constants,
        };
        self.validate_program(program, completion, &planning, &mut flow)?;
        let program_scope =
            self.created_scope(program.scope_id.get(), program.node_id.get(), program.span)?;
        let capture_layout =
            self.compiler_capture_layout(executable_id, program_scope, &layout, tree_layout)?;
        let closure_variables = self.compiled_closure_variables(executable_id, tree_layout)?;
        if !closure_variables.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Function Script root imports no caller closure",
                span: Some(program.span),
            });
        }
        let realm_globals = self.compiled_realm_globals(executable_id, tree_layout, constants)?;
        let mut variable_definitions = self.compiled_variable_definitions(
            executable_id,
            program_scope,
            &layout,
            tree_layout,
            constants,
        )?;
        variable_definitions.push(script_completion_variable_definition(constants)?);
        let closure_definitions =
            self.compiled_closure_definitions(&closure_variables, &realm_globals, constants)?;
        let capture_count =
            checked_function_entry_count(realm_globals.len(), "function closure variables")?;

        Ok(ValidatedFunction {
            executable_kind: CompilerExecutableKind::DynamicFunctionScript,
            strict: executable.is_strict(),
            argument_count: 0,
            local_count: layout.local_count,
            capture_count,
            capture_layout,
            locals: layout
                .locals
                .iter()
                .map(|local| LoweredLocal {
                    binding: local.binding,
                    slot: local.slot,
                })
                .collect(),
            constants: Arc::clone(&constants.entries),
            atoms: Arc::clone(&constants.atoms),
            closure_variables,
            realm_globals,
            function_name: None,
            variable_definitions,
            closure_definitions,
            function_span: source_byte_span(program.span),
            function_name_span: None,
            flow,
        })
    }

    fn compiled_constant_pools(
        &self,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Box<[CompiledConstantPool]>, LeafCompilationError> {
        let executables = self.planned.plan.executables();
        let mut candidates = (0..executables.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        let mut atom_candidates = (0..executables.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        let mut metadata_atom_candidates = (0..executables.len())
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        for child in executables {
            let Some(parent) = child.parent() else {
                continue;
            };
            let owner = candidates
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            owner.push(CompiledConstantCandidate::Function {
                executable: child.id(),
                span: child.span(),
            });
        }
        self.record_literal_candidates(&mut candidates, &mut atom_candidates)?;
        self.record_metadata_atom_candidates(tree_layout, &mut metadata_atom_candidates)?;

        let mut pools = Vec::with_capacity(executables.len());
        for (index, ((mut candidates, mut atoms), mut metadata_atoms)) in candidates
            .into_iter()
            .zip(atom_candidates)
            .zip(metadata_atom_candidates)
            .enumerate()
        {
            let executable =
                executables
                    .get(index)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "constant-pool owner indexes dense executable metadata",
                        span: None,
                    })?;
            candidates.sort_unstable_by_key(CompiledConstantCandidate::order_key);
            atoms.sort_unstable_by_key(CompiledAtomCandidate::order_key);
            metadata_atoms.sort_unstable_by_key(CompiledMetadataAtomCandidate::order_key);
            pools.push(CompiledConstantPool::from_candidates(
                tree_layout.children(executable.id())?,
                candidates,
                atoms,
                metadata_atoms,
            )?);
        }
        Ok(pools.into_boxed_slice())
    }

    fn record_metadata_atom_candidates(
        &self,
        tree_layout: &FunctionTreeLayout,
        candidates: &mut [Vec<CompiledMetadataAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        for executable in self.planned.plan.executables() {
            let owner = candidates.get_mut(executable.id().index()).ok_or(
                LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                },
            )?;
            if let Some(name) = executable.name() {
                let span =
                    executable
                        .name_span()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "named executable retains its name span",
                            span: Some(executable.span()),
                        })?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::FunctionName,
                    value: compiler_identifier_string(name, span)?,
                    span,
                });
            } else if executable.name_span().is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "anonymous executable has no name span",
                    span: Some(executable.span()),
                });
            }
            if executable.id().index() == 0
                && self.unit.goal()
                    == CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            {
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::ScriptCompletion,
                    value: compiler_identifier_string("_ret_", executable.span())?,
                    span: executable.span(),
                });
            }
            for binding in self.planned.plan.bindings_for(executable.id()).ok_or(
                LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                },
            )? {
                if !matches!(
                    binding.placement(),
                    StoragePlacement::Argument { .. } | StoragePlacement::Local
                ) {
                    continue;
                }
                let span = binding.declaration_spans().first().copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "frame binding retains a declaration span",
                        span: Some(executable.span()),
                    },
                )?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::Binding(binding.id()),
                    value: compiler_identifier_string(binding.name(), span)?,
                    span,
                });
            }
            for capture in self
                .planned
                .plan
                .frame_captures_for(executable.id())
                .ok_or(LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                })?
            {
                let binding = self.planned.plan.binding(capture.binding()).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured metadata binding exists",
                        span: Some(executable.span()),
                    },
                )?;
                let span = binding.declaration_spans().first().copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured binding retains a declaration span",
                        span: Some(executable.span()),
                    },
                )?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::Binding(binding.id()),
                    value: compiler_identifier_string(binding.name(), span)?,
                    span,
                });
            }
            for &global in tree_layout.realm_globals.imports_for(executable.id())? {
                let binding = tree_layout.realm_globals.binding(global).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "constructor-realm global import has a binding descriptor",
                        span: Some(executable.span()),
                    },
                )?;
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::RealmGlobal(global),
                    value: compiler_identifier_string(&binding.name, binding.first_span)?,
                    span: binding.first_span,
                });
            }
        }
        Ok(())
    }

    fn record_literal_candidates(
        &self,
        candidates: &mut [Vec<CompiledConstantCandidate>],
        atom_candidates: &mut [Vec<CompiledAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        let mut owners = vec![None; nodes.len()];
        for (node_id, node) in nodes.iter_enumerated() {
            let owner = match node.kind() {
                AstKind::Program(_)
                | AstKind::Function(_)
                | AstKind::ArrowFunctionExpression(_) => self
                    .planned
                    .identities
                    .executable_by_node
                    .get(node_id.index())
                    .copied()
                    .flatten(),
                _ => {
                    let parent = nodes.parent_id(node_id);
                    if parent.index() >= node_id.index() {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "semantic parents precede children in node order",
                            span: Some(node.kind().span()),
                        });
                    }
                    owners.get(parent.index()).copied().flatten()
                }
            };
            let owner_slot =
                owners
                    .get_mut(node_id.index())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "semantic node identity indexes constant-pool ownership",
                        span: Some(node.kind().span()),
                    })?;
            *owner_slot = owner;
            if let Some(owner) = owner {
                self.record_node_literal_candidate(node_id, owner, candidates, atom_candidates)?;
            }
        }
        Ok(())
    }

    fn record_node_literal_candidate(
        &self,
        node_id: NodeId,
        owner: ExecutableId,
        candidates: &mut [Vec<CompiledConstantCandidate>],
        atom_candidates: &mut [Vec<CompiledAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        match nodes.kind(node_id) {
            AstKind::NumericLiteral(literal)
                if !is_noncomputed_static_property_key_node(self.unit, node_id)
                    && exact_i32(literal.value).is_none() =>
            {
                let parent = nodes.parent_id(node_id);
                let folded_negative_i32 = matches!(
                    nodes.kind(parent),
                    AstKind::UnaryExpression(unary)
                        if unary.operator == UnaryOperator::UnaryNegation
                            && literal.value != 0.0
                            && exact_negated_i32(literal.value).is_some()
                );
                if !folded_negative_i32 {
                    candidates
                        .get_mut(owner.index())
                        .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
                        .push(CompiledConstantCandidate::Number {
                            value: Binary64Constant::from_f64(literal.value),
                            span: literal.span,
                        });
                }
            }
            AstKind::StringLiteral(literal)
                if !matches!(nodes.parent_kind(node_id), AstKind::Directive(_))
                    && !is_noncomputed_static_property_key_node(self.unit, node_id) =>
            {
                let value = decode_compiler_string(
                    literal.value.as_str(),
                    literal.lone_surrogates,
                    literal.span,
                )?;
                record_string_candidate(owner, value, literal.span, candidates, atom_candidates)?;
            }
            AstKind::TemplateLiteral(template)
                if !matches!(
                    nodes.parent_kind(node_id),
                    AstKind::TaggedTemplateExpression(_)
                ) && template.expressions.is_empty()
                    && template.quasis.len() == 1 =>
            {
                let quasi = &template.quasis[0];
                if !quasi.tail {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "no-substitution template has one tail quasi",
                        span: Some(template.span),
                    });
                }
                let cooked =
                    quasi
                        .value
                        .cooked
                        .as_ref()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "untagged no-substitution template has a cooked value",
                            span: Some(template.span),
                        })?;
                let value =
                    decode_compiler_string(cooked.as_str(), quasi.lone_surrogates, template.span)?;
                record_string_candidate(owner, value, template.span, candidates, atom_candidates)?;
            }
            AstKind::ObjectProperty(property) => {
                if !property.computed
                    && !property.shorthand
                    && let Some(key) = compiled_static_property_key(&property.key)?
                {
                    record_property_candidate(owner, key.value, key.span, atom_candidates)?;
                }
            }
            AstKind::StaticMemberExpression(member) => {
                record_property_candidate(
                    owner,
                    compiler_identifier_string(
                        member.property.name.as_str(),
                        member.property.span,
                    )?,
                    member.property.span,
                    atom_candidates,
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_body<'statement>(
        &self,
        function: &'statement Function<'arena>,
        body: &'statement FunctionBody<'arena>,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let function_scope = self.created_scope(
            function.scope_id.get(),
            function.node_id.get(),
            function.span,
        )?;
        let mut state = StatementPlanningState {
            work: vec![
                StatementWork::PopScope(function_scope),
                StatementWork::VisitList {
                    statements: &body.statements,
                    next: 0,
                },
                StatementWork::PushScope {
                    scope: function_scope,
                    creator: function.node_id.get(),
                    span: function.span,
                },
            ],
            active_scopes: Vec::new(),
            loop_controls: Vec::new(),
            completion: StatementCompletion::Discard,
        };

        while let Some(task) = state.work.pop() {
            self.process_statement_work(task, body.span, planning, flow, &mut state)?;
        }
        if !state.active_scopes.is_empty() || !state.loop_controls.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "statement planning closes every scope and loop region",
                span: Some(body.span),
            });
        }
        flow.ensure_terminal(body.span)?;
        Ok(())
    }

    fn validate_program<'statement>(
        &self,
        program: &'statement Program<'arena>,
        completion: LocalSlot,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let program_scope =
            self.created_scope(program.scope_id.get(), program.node_id.get(), program.span)?;
        let mut state = StatementPlanningState {
            work: vec![
                StatementWork::PopScope(program_scope),
                StatementWork::VisitList {
                    statements: &program.body,
                    next: 0,
                },
                StatementWork::PushScope {
                    scope: program_scope,
                    creator: program.node_id.get(),
                    span: program.span,
                },
            ],
            active_scopes: Vec::new(),
            loop_controls: Vec::new(),
            completion: StatementCompletion::Script(completion),
        };

        while let Some(task) = state.work.pop() {
            self.process_statement_work(task, program.span, planning, flow, &mut state)?;
        }
        if !state.active_scopes.is_empty() || !state.loop_controls.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Program planning closes every scope and loop region",
                span: Some(program.span),
            });
        }
        flow.ensure_script_terminal(completion, program.span)
    }

    fn process_statement_work<'statement>(
        &self,
        task: StatementWork<'statement, 'arena>,
        body_span: Span,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        match task {
            StatementWork::VisitList { statements, next } => {
                if let Some(statement) = statements.get(next) {
                    state.work.push(StatementWork::VisitList {
                        statements,
                        next: next + 1,
                    });
                    state.work.push(StatementWork::Visit(statement));
                }
            }
            StatementWork::PushScope {
                scope,
                creator,
                span,
            } => {
                self.plan_scope_entry(scope, creator, span, planning, flow)?;
                state.active_scopes.push(scope);
            }
            StatementWork::PopScope(expected) => {
                if state.active_scopes.len() > 1 {
                    self.plan_scope_exit(planning.executable, expected, planning.layout, flow)?;
                }
                let actual =
                    state
                        .active_scopes
                        .pop()
                        .ok_or(LeafCompilationError::SemanticInvariant {
                            invariant: "statement scope stack is nonempty on exit",
                            span: Some(body_span),
                        })?;
                if actual != expected {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "statement scopes exit in last-in-first-out order",
                        span: Some(body_span),
                    });
                }
            }
            StatementWork::CloseScope(scope) => {
                self.plan_scope_exit(planning.executable, scope, planning.layout, flow)?;
            }
            StatementWork::PushLoop(control) => state.loop_controls.push(control),
            StatementWork::PopLoop => {
                state
                    .loop_controls
                    .pop()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "statement loop stack is nonempty on exit",
                        span: Some(body_span),
                    })?;
            }
            StatementWork::Expression(expression) => self.plan_expression(
                expression,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?,
            StatementWork::Declaration(declaration) => self.validate_declaration(
                declaration,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
            )?,
            StatementWork::Emit(instruction) => flow.emit(instruction)?,
            StatementWork::Branch { kind, target, span } => {
                flow.branch(kind, &target, span)?;
            }
            StatementWork::Bind(label) => flow.bind(&label)?,
            StatementWork::Visit(statement) => self.plan_statement(
                statement,
                planning.layout,
                planning.tree_layout,
                planning.constants,
                flow,
                state,
            )?,
        }
        Ok(())
    }

    fn plan_statement<'statement>(
        &self,
        statement: &'statement Statement<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        match statement {
            Statement::BlockStatement(block) => {
                self.schedule_block_statement(block, state)?;
            }
            Statement::FunctionDeclaration(function) => {
                self.validate_function_declaration(
                    function,
                    layout.executable,
                    tree_layout,
                    state.active_scopes.last().copied(),
                )?;
            }
            Statement::VariableDeclaration(declaration) => {
                self.validate_declaration(declaration, layout, tree_layout, constants, flow)?;
            }
            Statement::ExpressionStatement(statement) => {
                Self::schedule_expression_statement(statement, state.completion, &mut state.work);
            }
            Statement::EmptyStatement(_) => {}
            Statement::ReturnStatement(statement) => {
                Self::schedule_return_statement(statement, flow, &mut state.work)?;
            }
            Statement::ThrowStatement(statement) => {
                Self::schedule_throw_statement(statement, &mut state.work);
            }
            Statement::IfStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_if_statement(statement, flow, &mut state.work)?;
            }
            Statement::WhileStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                Self::schedule_while_statement(
                    statement,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                )?;
            }
            Statement::DoWhileStatement(statement) => {
                Self::schedule_do_while_statement(
                    statement,
                    state.completion,
                    flow,
                    &mut state.work,
                    state.active_scopes.len(),
                )?;
            }
            Statement::ForStatement(statement) => {
                Self::reset_script_completion(state.completion, statement.span, flow)?;
                self.plan_for_statement(statement, flow, state)?;
            }
            Statement::BreakStatement(statement) => {
                self.plan_loop_jump(
                    statement.label.as_ref().map(|label| label.span),
                    statement.span,
                    LoopJump::Break,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::ContinueStatement(statement) => {
                self.plan_loop_jump(
                    statement.label.as_ref().map(|label| label.span),
                    statement.span,
                    LoopJump::Continue,
                    state,
                    layout,
                    flow,
                )?;
            }
            Statement::LabeledStatement(statement) => {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedBody,
                    statement.label.span,
                );
            }
            _ => {
                return unsupported(UnsupportedLeafFeature::UnsupportedBody, statement.span());
            }
        }
        Ok(())
    }

    fn schedule_block_statement<'statement>(
        &self,
        block: &'statement BlockStatement<'arena>,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(block.scope_id.get(), block.node_id.get(), block.span)?;
        state.work.push(StatementWork::PopScope(scope));
        state.work.push(StatementWork::VisitList {
            statements: &block.body,
            next: 0,
        });
        state.work.push(StatementWork::PushScope {
            scope,
            creator: block.node_id.get(),
            span: block.span,
        });
        Ok(())
    }

    fn schedule_expression_statement<'statement>(
        statement: &'statement ExpressionStatement<'arena>,
        completion: StatementCompletion,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        let (opcode, operands) = match completion {
            StatementCompletion::Discard => (FinalOpcode::Drop, Operands::None),
            StatementCompletion::Script(slot) => compact_put_local(slot),
        };
        work.push(StatementWork::Emit(PlannedInstruction::new(
            opcode,
            operands,
            statement.expression.span(),
        )));
        work.push(StatementWork::Expression(&statement.expression));
    }

    fn reset_script_completion(
        completion: StatementCompletion,
        span: Span,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let StatementCompletion::Script(slot) = completion else {
            return Ok(());
        };
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Undefined,
            Operands::None,
            span,
        ))?;
        let (opcode, operands) = compact_put_local(slot);
        flow.emit(PlannedInstruction::new(opcode, operands, span))
    }

    fn schedule_return_statement<'statement>(
        statement: &'statement ReturnStatement<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(argument) = &statement.argument {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Return,
                Operands::None,
                statement.span,
            )));
            work.push(StatementWork::Expression(argument));
        } else {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                statement.span,
            ))?;
        }
        Ok(())
    }

    fn schedule_throw_statement<'statement>(
        statement: &'statement ThrowStatement<'arena>,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) {
        work.push(StatementWork::Emit(PlannedInstruction::new(
            FinalOpcode::Throw,
            Operands::None,
            statement.span,
        )));
        work.push(StatementWork::Expression(&statement.argument));
    }

    fn plan_for_statement<'statement>(
        &self,
        statement: &'statement ForStatement<'arena>,
        flow: &mut PlannedControlFlow,
        state: &mut StatementPlanningState<'statement, 'arena>,
    ) -> Result<(), LeafCompilationError> {
        let scope = self.created_scope(
            statement.scope_id.get(),
            statement.node_id.get(),
            statement.span,
        )?;
        Self::schedule_for_statement(
            statement,
            scope,
            flow,
            &mut state.work,
            state.active_scopes.len(),
        )
    }

    fn schedule_if_statement<'statement>(
        statement: &'statement IfStatement<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(alternate_statement) = &statement.alternate {
            let alternate = flow.new_statement_label(alternate_statement.span())?;
            let done = flow.new_statement_label(statement.span)?;
            work.push(StatementWork::Bind(done.clone()));
            work.push(StatementWork::Visit(alternate_statement));
            work.push(StatementWork::Bind(alternate.clone()));
            work.push(StatementWork::Branch {
                kind: BranchKind::Goto,
                target: done,
                span: statement.span,
            });
            work.push(StatementWork::Visit(&statement.consequent));
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: alternate,
                span: statement.test.span(),
            });
        } else {
            let done = flow.new_statement_label(statement.span)?;
            work.push(StatementWork::Bind(done.clone()));
            work.push(StatementWork::Visit(&statement.consequent));
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: done,
                span: statement.test.span(),
            });
        }
        work.push(StatementWork::Expression(&statement.test));
        Ok(())
    }

    fn schedule_while_statement<'statement>(
        statement: &'statement WhileStatement<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        scope_depth: usize,
    ) -> Result<(), LeafCompilationError> {
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = LoopControl {
            break_target: done.clone(),
            continue_target: test.clone(),
            scope_depth,
        };
        work.push(StatementWork::Bind(done.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: test.clone(),
            span: statement.span,
        });
        work.push(StatementWork::PopLoop);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushLoop(control));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfFalse,
            target: done,
            span: statement.test.span(),
        });
        work.push(StatementWork::Expression(&statement.test));
        work.push(StatementWork::Bind(test));
        Ok(())
    }

    fn schedule_do_while_statement<'statement>(
        statement: &'statement DoWhileStatement<'arena>,
        completion: StatementCompletion,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        scope_depth: usize,
    ) -> Result<(), LeafCompilationError> {
        let iteration = flow.new_statement_label(statement.body.span())?;
        let test = flow.new_statement_label(statement.test.span())?;
        let done = flow.new_statement_label(statement.span)?;
        let control = LoopControl {
            break_target: done.clone(),
            continue_target: test.clone(),
            scope_depth,
        };
        work.push(StatementWork::Bind(done));
        work.push(StatementWork::Branch {
            kind: BranchKind::IfTrue,
            target: iteration.clone(),
            span: statement.test.span(),
        });
        work.push(StatementWork::Expression(&statement.test));
        work.push(StatementWork::Bind(test));
        work.push(StatementWork::PopLoop);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushLoop(control));
        if let StatementCompletion::Script(slot) = completion {
            let (opcode, operands) = compact_put_local(slot);
            work.push(StatementWork::Emit(PlannedInstruction::new(
                opcode,
                operands,
                statement.span,
            )));
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                statement.span,
            )));
        }
        work.push(StatementWork::Bind(iteration));
        Ok(())
    }

    fn schedule_for_statement<'statement>(
        statement: &'statement ForStatement<'arena>,
        scope: ScopeId,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<StatementWork<'statement, 'arena>>,
        enclosing_scope_depth: usize,
    ) -> Result<(), LeafCompilationError> {
        let test = flow.new_statement_label(
            statement
                .test
                .as_ref()
                .map_or(statement.span, GetSpan::span),
        )?;
        let rotate = flow.new_statement_label(
            statement
                .update
                .as_ref()
                .map_or(statement.span, GetSpan::span),
        )?;
        let done = flow.new_statement_label(statement.span)?;
        let loop_scope_depth =
            enclosing_scope_depth
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "statement scope depth",
                })?;
        let control = LoopControl {
            break_target: done.clone(),
            continue_target: rotate.clone(),
            scope_depth: loop_scope_depth,
        };

        work.push(StatementWork::PopScope(scope));
        work.push(StatementWork::Bind(done.clone()));
        work.push(StatementWork::Branch {
            kind: BranchKind::Goto,
            target: test.clone(),
            span: statement.span,
        });
        if let Some(update) = &statement.update {
            work.push(StatementWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                update.span(),
            )));
            work.push(StatementWork::Expression(update));
        }
        work.push(StatementWork::CloseScope(scope));
        work.push(StatementWork::Bind(rotate));
        work.push(StatementWork::PopLoop);
        work.push(StatementWork::Visit(&statement.body));
        work.push(StatementWork::PushLoop(control));
        if let Some(test_expression) = &statement.test {
            work.push(StatementWork::Branch {
                kind: BranchKind::IfFalse,
                target: done,
                span: test_expression.span(),
            });
            work.push(StatementWork::Expression(test_expression));
        }
        work.push(StatementWork::Bind(test));
        work.push(StatementWork::CloseScope(scope));
        if let Some(initializer) = &statement.init {
            match initializer {
                ForStatementInit::VariableDeclaration(declaration) => {
                    work.push(StatementWork::Declaration(declaration));
                }
                initializer => {
                    let expression = initializer.to_expression();
                    work.push(StatementWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Drop,
                        Operands::None,
                        expression.span(),
                    )));
                    work.push(StatementWork::Expression(expression));
                }
            }
        }
        work.push(StatementWork::PushScope {
            scope,
            creator: statement.node_id.get(),
            span: statement.span,
        });
        Ok(())
    }

    fn plan_loop_jump(
        &self,
        label_span: Option<Span>,
        statement_span: Span,
        jump: LoopJump,
        state: &StatementPlanningState<'_, '_>,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if let Some(label_span) = label_span {
            return unsupported(UnsupportedLeafFeature::UnsupportedBody, label_span);
        }
        let control =
            state
                .loop_controls
                .last()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: jump.missing_region_invariant(),
                    span: Some(statement_span),
                })?;
        if control.scope_depth > state.active_scopes.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: jump.scope_invariant(),
                span: Some(statement_span),
            });
        }
        for scope in state.active_scopes[control.scope_depth..].iter().rev() {
            self.plan_scope_exit(layout.executable, *scope, layout, flow)?;
        }
        flow.branch(BranchKind::Goto, jump.target(control), statement_span)
    }

    fn created_scope(
        &self,
        scope: Option<ScopeId>,
        creator: NodeId,
        span: Span,
    ) -> Result<ScopeId, LeafCompilationError> {
        let scope = scope.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "Oxc scope creator has a semantic scope identity",
            span: Some(span),
        })?;
        let scoping = self.unit.semantic().scoping();
        if scope.index() >= scoping.scopes_len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope identity indexes retained semantics",
                span: Some(span),
            });
        }
        if scoping.get_node_id(scope) != creator {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope identity names its creator node",
                span: Some(span),
            });
        }
        Ok(scope)
    }

    #[allow(clippy::too_many_lines)]
    fn plan_scope_entry(
        &self,
        scope: ScopeId,
        creator: NodeId,
        span: Span,
        planning: &FunctionPlanningContext<'_>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        if scoping.get_node_id(scope) != creator {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc scope entry names its creator node",
                span: Some(span),
            });
        }
        let executable = planning.executable;
        let function_creator = self
            .planned
            .identities
            .node_by_executable
            .get(executable.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "scope-entry executable has an Oxc node identity",
                span: Some(span),
            })?;
        let function_scope = creator == function_creator;
        let mut entries = self.scope_entry_initializations(
            executable,
            scope,
            planning.layout,
            planning.tree_layout,
        )?;
        entries.sort_unstable_by_key(ScopeEntryInitialization::order_key);
        if function_scope {
            self.emit_realm_global_function_initializers(
                executable,
                planning.tree_layout,
                planning.constants,
                flow,
            )?;
            for entry in entries
                .iter()
                .rev()
                .copied()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Uninitialized { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
            for entry in entries
                .iter()
                .copied()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Function { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
        } else {
            for entry in entries
                .iter()
                .rev()
                .copied()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Function { .. }))
            {
                let ScopeEntryInitialization::Function { slot, span, .. } = entry else {
                    unreachable!("filtered above");
                };
                let FrameSlot::Local(slot) = slot else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "block function declaration uses a local slot",
                        span: Some(span),
                    });
                };
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::SetLocUninitialized,
                    Operands::Loc(slot.index()),
                    span,
                ))?;
            }
            for entry in entries
                .iter()
                .rev()
                .copied()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Uninitialized { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
            for entry in entries
                .into_iter()
                .rev()
                .filter(|entry| matches!(entry, ScopeEntryInitialization::Function { .. }))
            {
                self.emit_scope_entry_initialization(
                    executable,
                    entry,
                    planning.tree_layout,
                    planning.constants,
                    flow,
                )?;
            }
        }
        Ok(())
    }

    fn scope_entry_initializations(
        &self,
        executable: ExecutableId,
        scope: ScopeId,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Vec<ScopeEntryInitialization>, LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        let mut entries = Vec::new();
        for symbol in scoping.iter_bindings_in(scope) {
            if scoping.symbol_scope_id(symbol) != scope {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "Oxc exact-scope binding belongs to that scope",
                    span: Some(scoping.symbol_span(symbol)),
                });
            }
            let declaration_span = scoping.symbol_span(symbol);
            let binding = self.binding_for_identifier(Some(symbol), declaration_span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-entry compiler binding exists",
                    span: Some(declaration_span),
                },
            )?;
            if storage.executable() != executable {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scope-entry binding belongs to the selected executable",
                    span: Some(declaration_span),
                });
            }
            if Self::realm_global_scope_entry_is_runtime_instantiated(storage, declaration_span)? {
                continue;
            }
            let frame_slot =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "scope-entry binding has a frame slot",
                        span: Some(declaration_span),
                    })?;
            match storage.policy().initialization() {
                InitializationPolicy::AtDeclaration
                    if storage.policy().has_temporal_dead_zone() =>
                {
                    let FrameSlot::Local(slot) = frame_slot else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "scope-entry lexical binding uses a local slot",
                            span: Some(declaration_span),
                        });
                    };
                    entries.push(ScopeEntryInitialization::Uninitialized {
                        slot,
                        span: declaration_span,
                    });
                }
                InitializationPolicy::FunctionAtInstantiation
                | InitializationPolicy::FunctionAtScopeEntry => {
                    if storage.policy().kind() != DeclarationKind::Function
                        || storage.policy().has_temporal_dead_zone()
                        || matches!(frame_slot, FrameSlot::Capture(_))
                    {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "scope-entry function declaration has writable frame storage",
                            span: Some(declaration_span),
                        });
                    }
                    let child = tree_layout.function_declaration(binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "scope-entry function binding has a declaration executable",
                            span: Some(declaration_span),
                        },
                    )?;
                    let child_span = self
                        .planned
                        .plan
                        .executable(child)
                        .map_or(declaration_span, Executable::span);
                    entries.push(ScopeEntryInitialization::Function {
                        slot: frame_slot,
                        child,
                        span: child_span,
                    });
                }
                InitializationPolicy::AtDeclaration => {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedDeclaration,
                        declaration_span,
                    );
                }
                InitializationPolicy::Argument
                | InitializationPolicy::UndefinedAtInstantiation
                | InitializationPolicy::FunctionName
                | InitializationPolicy::Catch
                | InitializationPolicy::ModuleImport
                | InitializationPolicy::ModuleNamespace => {}
            }
        }
        Ok(entries)
    }

    fn realm_global_scope_entry_is_runtime_instantiated(
        storage: &crate::storage::BindingStorage,
        span: Span,
    ) -> Result<bool, LeafCompilationError> {
        if storage.placement() != StoragePlacement::GlobalObject {
            return Ok(false);
        }
        let supported_policy = matches!(
            (storage.policy().kind(), storage.policy().initialization()),
            (
                DeclarationKind::Var,
                InitializationPolicy::UndefinedAtInstantiation
            ) | (
                DeclarationKind::Function,
                InitializationPolicy::FunctionAtInstantiation
            )
        );
        if !supported_policy
            || storage.policy().writes() != WritePolicy::Mutable
            || storage.policy().has_temporal_dead_zone()
        {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, span);
        }
        Ok(true)
    }

    fn emit_realm_global_function_initializers(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            || executable.index() != 0
        {
            return Ok(());
        }
        let root = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if root.parent().is_some()
            || !matches!(
                root.kind(),
                ExecutableKind::Script {
                    asynchronous: false
                }
            )
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm function initializers belong to the dynamic Script root",
                span: Some(root.span()),
            });
        }

        for &global in tree_layout.realm_globals.imports_for(executable)? {
            let descriptor = tree_layout.realm_globals.binding(global).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "root realm-global initializer has a binding descriptor",
                    span: Some(root.span()),
                },
            )?;
            if descriptor.policy.kind() != VerifiedBindingKind::Function {
                continue;
            }
            let binding =
                descriptor
                    .declaration
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "constructor-realm function initializer has a declared binding",
                        span: Some(descriptor.first_span),
                    })?;
            let child = tree_layout.function_declaration(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "constructor-realm function initializer selects its last child",
                    span: Some(descriptor.first_span),
                },
            )?;
            let child_span = self
                .planned
                .plan
                .executable(child)
                .map_or(descriptor.first_span, Executable::span);
            flow.emit(self.plan_child_function_closure(
                child,
                executable,
                child_span,
                tree_layout,
                constants,
            )?)?;
            let slot =
                tree_layout
                    .realm_globals
                    .closure_slot(&self.planned.plan, executable, global)?;
            flow.emit(PlannedInstruction::new(
                FinalOpcode::PutVar,
                Operands::VarRef(slot),
                descriptor.first_span,
            ))?;
        }
        Ok(())
    }

    fn emit_scope_entry_initialization(
        &self,
        executable: ExecutableId,
        entry: ScopeEntryInitialization,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        match entry {
            ScopeEntryInitialization::Uninitialized { slot, span } => {
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::SetLocUninitialized,
                    Operands::Loc(slot.index()),
                    span,
                ))?;
            }
            ScopeEntryInitialization::Function { slot, child, span } => {
                flow.emit(self.plan_child_function_closure(
                    child,
                    executable,
                    span,
                    tree_layout,
                    constants,
                )?)?;
                flow.emit(plan_put_slot(slot, span))?;
            }
        }
        Ok(())
    }

    fn plan_scope_exit(
        &self,
        executable: ExecutableId,
        scope: ScopeId,
        layout: &FrameLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        let mut captured_locals = Vec::new();
        for symbol in scoping.iter_bindings_in(scope) {
            if scoping.symbol_scope_id(symbol) != scope {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scope-exit exact-scope binding belongs to that scope",
                    span: Some(scoping.symbol_span(symbol)),
                });
            }
            let declaration_span = scoping.symbol_span(symbol);
            let binding = self.binding_for_identifier(Some(symbol), declaration_span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-exit compiler binding exists",
                    span: Some(declaration_span),
                },
            )?;
            if storage.executable() != executable {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "scope-exit binding belongs to the selected executable",
                    span: Some(declaration_span),
                });
            }
            if !storage.is_frame_captured() {
                continue;
            }
            let FrameSlot::Local(slot) =
                layout
                    .slot(binding)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "captured scope-exit binding has a frame slot",
                        span: Some(declaration_span),
                    })?
            else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "captured scope-exit binding uses a local slot",
                    span: Some(declaration_span),
                });
            };
            captured_locals.push((slot, declaration_span));
        }
        captured_locals.sort_unstable_by_key(|(slot, _)| slot.index());
        for (slot, declaration_span) in captured_locals.into_iter().rev() {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::CloseLoc,
                Operands::Loc(slot.index()),
                declaration_span,
            ))?;
        }
        Ok(())
    }

    fn validate_function_declaration(
        &self,
        function: &Function<'arena>,
        parent: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        active_scope: Option<ScopeId>,
    ) -> Result<(), LeafCompilationError> {
        if function.r#type != FunctionType::FunctionDeclaration {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function-declaration statement has declaration function type",
                span: Some(function.span),
            });
        }
        let identifier = function
            .id
            .as_ref()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Script function declaration has a binding identifier",
                span: Some(function.span),
            })?;
        let binding = self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "function declaration binding has compiler storage",
                    span: Some(identifier.span),
                })?;
        if storage.executable() != parent
            || storage.policy().kind() != DeclarationKind::Function
            || !matches!(
                storage.policy().initialization(),
                InitializationPolicy::FunctionAtInstantiation
                    | InitializationPolicy::FunctionAtScopeEntry
            )
        {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                identifier.span,
            );
        }
        let binding_scope = self.scope_for_binding(binding)?;
        if active_scope != Some(binding_scope) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function declaration executes in its binding scope",
                span: Some(identifier.span),
            });
        }
        let child = self.executable_for_function(function)?;
        let child_metadata = self
            .planned
            .plan
            .executable(child)
            .ok_or(LeafCompilationError::InvalidExecutable { executable: child })?;
        if child_metadata.parent() != Some(parent)
            || tree_layout.children(parent)?.binary_search(&child).is_err()
            || child_metadata.name_span() != Some(identifier.span)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function declaration has one typed direct-child constant",
                span: Some(function.span),
            });
        }
        Ok(())
    }

    fn validate_declaration(
        &self,
        declaration: &VariableDeclaration<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if declaration.declare
            || !matches!(
                declaration.kind,
                VariableDeclarationKind::Var
                    | VariableDeclarationKind::Let
                    | VariableDeclarationKind::Const
            )
        {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedDeclaration,
                declaration.span,
            );
        }

        for declarator in &declaration.declarations {
            let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedDeclaration,
                    declarator.span,
                );
            };
            let binding =
                self.binding_for_identifier(identifier.symbol_id.get(), identifier.span)?;
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "declared compiler binding exists",
                    span: Some(identifier.span),
                },
            )?;
            if storage.placement() == StoragePlacement::GlobalObject {
                self.validate_realm_global_var_declaration(
                    declaration.kind,
                    storage,
                    identifier.span,
                )?;
                if let Some(initializer) = &declarator.init {
                    if let Some(span) = anonymous_function_definition_span(initializer) {
                        return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
                    }
                    self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
                    let global = tree_layout.realm_globals.for_binding(binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "declared Program var has a constructor-realm global identity",
                            span: Some(identifier.span),
                        },
                    )?;
                    let slot = tree_layout.realm_globals.closure_slot(
                        &self.planned.plan,
                        layout.executable,
                        global,
                    )?;
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::PutVar,
                        Operands::VarRef(slot),
                        identifier.span,
                    ))?;
                }
                continue;
            }

            let frame_slot = layout
                .slot(binding)
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                    span: identifier.span,
                })?;
            self.validate_declaration_storage(
                declaration.kind,
                binding,
                frame_slot,
                identifier.span,
            )?;

            match &declarator.init {
                Some(initializer) => {
                    if let Some(span) = anonymous_function_definition_span(initializer) {
                        return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
                    }
                    self.plan_expression(initializer, layout, tree_layout, constants, flow)?;
                    flow.emit(plan_put_slot(frame_slot, identifier.span))?;
                }
                None if declaration.kind == VariableDeclarationKind::Let => {
                    flow.emit(PlannedInstruction::new(
                        FinalOpcode::Undefined,
                        Operands::None,
                        identifier.span,
                    ))?;
                    flow.emit(plan_put_slot(frame_slot, identifier.span))?;
                }
                None if declaration.kind == VariableDeclarationKind::Var => {}
                None => {
                    return unsupported(
                        UnsupportedLeafFeature::UnsupportedDeclaration,
                        declarator.span,
                    );
                }
            }
        }
        Ok(())
    }

    fn validate_realm_global_var_declaration(
        &self,
        declaration_kind: VariableDeclarationKind,
        storage: &crate::storage::BindingStorage,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let merged_global_policy = matches!(
            (storage.policy().kind(), storage.policy().initialization()),
            (
                DeclarationKind::Var,
                InitializationPolicy::UndefinedAtInstantiation
            ) | (
                DeclarationKind::Function,
                InitializationPolicy::FunctionAtInstantiation
            )
        );
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            || declaration_kind != VariableDeclarationKind::Var
            || !merged_global_policy
            || storage.policy().writes() != WritePolicy::Mutable
            || storage.policy().has_temporal_dead_zone()
        {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, span);
        }
        Ok(())
    }

    fn validate_declaration_storage(
        &self,
        declaration_kind: VariableDeclarationKind,
        binding: BindingId,
        frame_slot: FrameSlot,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "declared compiler binding exists",
                    span: Some(span),
                })?;
        let valid = match declaration_kind {
            VariableDeclarationKind::Let => {
                matches!(storage.policy().kind(), DeclarationKind::Let)
                    && storage.policy().has_temporal_dead_zone()
                    && matches!(frame_slot, FrameSlot::Local(_))
            }
            VariableDeclarationKind::Const => {
                matches!(storage.policy().kind(), DeclarationKind::Const)
                    && storage.policy().has_temporal_dead_zone()
                    && matches!(frame_slot, FrameSlot::Local(_))
            }
            VariableDeclarationKind::Var => {
                matches!(
                    storage.policy().kind(),
                    DeclarationKind::Var | DeclarationKind::Parameter
                ) && !storage.policy().has_temporal_dead_zone()
            }
            VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing => false,
        };
        if !valid {
            return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
        }
        Ok(())
    }

    fn plan_expression<'expression>(
        &self,
        expression: &'expression Expression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let mut work = vec![ExpressionWork::Visit(expression)];
        while let Some(task) = work.pop() {
            match task {
                ExpressionWork::Emit(instruction) => flow.emit(instruction)?,
                ExpressionWork::Branch { kind, target, span } => {
                    flow.branch(kind, &target, span)?;
                }
                ExpressionWork::Bind(label) => flow.bind(&label)?,
                ExpressionWork::Visit(expression) => {
                    if let Some(literal) = plan_literal(expression, constants) {
                        flow.emit(literal?)?;
                        continue;
                    }
                    match expression {
                        Expression::Identifier(identifier) => {
                            self.plan_identifier_read(identifier, layout, tree_layout, flow)?;
                        }
                        Expression::UnaryExpression(unary) => {
                            self.plan_unary_expression(
                                unary,
                                layout,
                                tree_layout,
                                &mut work,
                                flow,
                            )?;
                        }
                        Expression::BinaryExpression(binary) => {
                            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                                binary_opcode(binary.operator),
                                Operands::None,
                                binary.span,
                            )));
                            work.push(ExpressionWork::Visit(&binary.right));
                            work.push(ExpressionWork::Visit(&binary.left));
                        }
                        Expression::ParenthesizedExpression(parenthesized) => {
                            work.push(ExpressionWork::Visit(&parenthesized.expression));
                        }
                        Expression::SequenceExpression(sequence) => {
                            Self::plan_sequence_expression(sequence, &mut work)?;
                        }
                        Expression::ConditionalExpression(conditional) => {
                            Self::plan_conditional_expression(conditional, flow, &mut work)?;
                        }
                        Expression::LogicalExpression(logical) => {
                            Self::plan_logical_expression(logical, flow, &mut work)?;
                        }
                        Expression::ObjectExpression(object) => {
                            self.plan_object_expression(
                                object,
                                layout,
                                tree_layout,
                                constants,
                                &mut work,
                            )?;
                        }
                        Expression::StaticMemberExpression(member) => {
                            Self::plan_static_member_read(member, constants, &mut work)?;
                        }
                        Expression::AssignmentExpression(assignment) => {
                            self.plan_assignment_expression(
                                assignment,
                                layout,
                                tree_layout,
                                constants,
                                flow,
                                &mut work,
                            )?;
                        }
                        Expression::UpdateExpression(update) => {
                            self.plan_update_expression(update, layout, tree_layout, &mut work)?;
                        }
                        Expression::CallExpression(call) => {
                            Self::plan_call_expression(call, constants, &mut work)?;
                        }
                        Expression::NewExpression(constructor) => {
                            Self::plan_new_expression(constructor, &mut work)?;
                        }
                        Expression::FunctionExpression(function) => {
                            flow.emit(self.plan_function_closure(
                                function,
                                layout.executable,
                                tree_layout,
                                constants,
                            )?)?;
                        }
                        Expression::ThisExpression(this) => {
                            flow.emit(self.plan_this_expression(this.span, layout)?)?;
                        }
                        _ => {
                            return unsupported(
                                UnsupportedLeafFeature::UnsupportedExpression,
                                expression.span(),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn plan_identifier_read(
        &self,
        identifier: &IdentifierReference<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        if !reference.access().reads() || reference.access().writes() {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedReference,
                identifier.span,
            );
        }
        let instruction = match reference {
            LoweredReference::Frame { binding, slot, .. } => {
                self.plan_read_slot(binding, slot, identifier.span)?
            }
            LoweredReference::RealmGlobal { slot, .. } => PlannedInstruction::new(
                FinalOpcode::GetVar,
                Operands::VarRef(slot),
                identifier.span,
            ),
        };
        flow.emit(instruction)
    }

    fn plan_this_expression(
        &self,
        span: Span,
        layout: &FrameLayout,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let executable = self.planned.plan.executable(layout.executable).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: layout.executable,
            },
        )?;
        let is_dynamic_function_authority =
            self.unit.goal() == CompilationGoal::DynamicFunction(DynamicFunctionKind::Function);
        let is_object_method = self
            .planned
            .identities
            .node_by_executable
            .get(layout.executable.index())
            .copied()
            .and_then(|node_id| object_method_or_accessor_span(self.unit, node_id))
            .is_some();
        if !executable.is_strict() && !is_dynamic_function_authority && !is_object_method {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, span);
        }
        Ok(PlannedInstruction::new(
            FinalOpcode::PushThis,
            Operands::None,
            span,
        ))
    }

    fn plan_call_expression<'expression>(
        call: &'expression CallExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if call.optional || call.type_arguments.is_some() {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, call.span);
        }
        if let Some(spread) = call.arguments.iter().find(|argument| argument.is_spread()) {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, spread.span());
        }

        let argument_count = u16::try_from(call.arguments.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "call arguments",
            }
        })?;
        let static_member = Self::static_member_callee(&call.callee)?;
        work.push(ExpressionWork::Emit(if static_member.is_some() {
            PlannedInstruction::new(
                FinalOpcode::CallMethod,
                Operands::NPop { argument_count },
                call.span,
            )
        } else {
            plan_direct_call(argument_count, call.span)
        }));
        for argument in call.arguments.iter().rev() {
            let expression =
                argument
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "non-spread call argument is an expression",
                        span: Some(argument.span()),
                    })?;
            work.push(ExpressionWork::Visit(expression));
        }
        if let Some(member) = static_member {
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::GetField2,
                Operands::Atom(constants.property_atom_index(member.property.span)?),
                member.span,
            )));
            work.push(ExpressionWork::Visit(&member.object));
        } else {
            work.push(ExpressionWork::Visit(&call.callee));
        }
        Ok(())
    }

    fn plan_new_expression<'expression>(
        constructor: &'expression NewExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if constructor.type_arguments.is_some() {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                constructor.span,
            );
        }
        if let Some(spread) = constructor
            .arguments
            .iter()
            .find(|argument| argument.is_spread())
        {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, spread.span());
        }

        let argument_count = u16::try_from(constructor.arguments.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "constructor arguments",
            }
        })?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::CallConstructor,
            Operands::NPop { argument_count },
            constructor.span,
        )));
        for argument in constructor.arguments.iter().rev() {
            let expression =
                argument
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "non-spread constructor argument is an expression",
                        span: Some(argument.span()),
                    })?;
            work.push(ExpressionWork::Visit(expression));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Dup,
            Operands::None,
            constructor.callee.span(),
        )));
        work.push(ExpressionWork::Visit(&constructor.callee));
        Ok(())
    }

    fn static_member_callee<'expression>(
        callee: &'expression Expression<'arena>,
    ) -> Result<Option<&'expression StaticMemberExpression<'arena>>, LeafCompilationError> {
        let mut expression = callee;
        loop {
            match expression {
                Expression::ParenthesizedExpression(parenthesized) => {
                    expression = &parenthesized.expression;
                }
                Expression::StaticMemberExpression(member) if !member.optional => {
                    return Ok(Some(member));
                }
                Expression::StaticMemberExpression(member) => {
                    return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
                }
                _ => return Ok(None),
            }
        }
    }

    fn plan_object_expression<'expression>(
        &self,
        object: &'expression ObjectExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        for property in object.properties.iter().rev() {
            let ObjectPropertyKind::ObjectProperty(property) = property else {
                return unsupported(
                    UnsupportedLeafFeature::UnsupportedExpression,
                    property.span(),
                );
            };
            if property.computed || property.shorthand {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, property.span);
            }
            let Some(key) = compiled_static_property_key(&property.key)? else {
                return unsupported(
                    if property.method || property.kind != PropertyKind::Init {
                        UnsupportedLeafFeature::ObjectMethodOrAccessor
                    } else {
                        UnsupportedLeafFeature::UnsupportedExpression
                    },
                    property.key.span(),
                );
            };
            let method_kind = match (property.method, property.kind) {
                (true, PropertyKind::Init) => Some(ObjectMethodKind::Method),
                (false, PropertyKind::Get) => Some(ObjectMethodKind::Getter),
                (false, PropertyKind::Set) => Some(ObjectMethodKind::Setter),
                (false, PropertyKind::Init) => None,
                _ => {
                    return unsupported(
                        UnsupportedLeafFeature::ObjectMethodOrAccessor,
                        property.span,
                    );
                }
            };
            if let Some(kind) = method_kind {
                let Expression::FunctionExpression(function) = &property.value else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "object method or accessor value is a function expression",
                        span: Some(property.value.span()),
                    });
                };
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::DefineMethod,
                    Operands::AtomU8 {
                        atom: constants.property_atom_index(key.span)?,
                        value: kind.define_method_flags(),
                    },
                    property.span,
                )));
                work.push(ExpressionWork::Emit(self.plan_function_closure(
                    function,
                    layout.executable,
                    tree_layout,
                    constants,
                )?));
                continue;
            }
            if key.value.code_units().eq("__proto__".encode_utf16()) {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, key.span);
            }
            if let Some(span) = anonymous_function_definition_span(&property.value) {
                return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
            }
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::DefineField,
                Operands::Atom(constants.property_atom_index(key.span)?),
                property.span,
            )));
            work.push(ExpressionWork::Visit(&property.value));
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Object,
            Operands::None,
            object.span,
        )));
        Ok(())
    }

    fn plan_static_member_read<'expression>(
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if member.optional {
            return unsupported(UnsupportedLeafFeature::UnsupportedExpression, member.span);
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::GetField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_function_closure(
        &self,
        function: &Function<'arena>,
        parent: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let child = self.executable_for_function(function)?;
        self.plan_child_function_closure(child, parent, function.span, tree_layout, constants)
    }

    fn executable_for_function(
        &self,
        function: &Function<'arena>,
    ) -> Result<ExecutableId, LeafCompilationError> {
        let node_id = function.node_id.get();
        self.planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "nested function node has a compiler executable identity",
                span: Some(function.span),
            })
    }

    fn plan_child_function_closure(
        &self,
        child: ExecutableId,
        parent: ExecutableId,
        span: Span,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let child_metadata = self
            .planned
            .plan
            .executable(child)
            .ok_or(LeafCompilationError::InvalidExecutable { executable: child })?;
        if child_metadata.parent() != Some(parent)
            || tree_layout.children(parent)?.binary_search(&child).is_err()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "nested function constant names a direct child executable",
                span: Some(span),
            });
        }
        let constant_index = constants.function_index(child)?;
        let (opcode, operands) = match u8::try_from(constant_index) {
            Ok(index) => (FinalOpcode::FClosure8, Operands::Const8(index)),
            Err(_) => (FinalOpcode::FClosure, Operands::Const(constant_index)),
        };
        Ok(PlannedInstruction::new(opcode, operands, span))
    }

    fn plan_assignment_expression<'expression>(
        &self,
        assignment: &'expression AssignmentExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if let Some(span) = anonymous_function_definition_span(&assignment.right) {
            return unsupported(UnsupportedLeafFeature::InferredFunctionName, span);
        }
        if let AssignmentTarget::StaticMemberExpression(member) = &assignment.left {
            return Self::plan_static_member_assignment(assignment, member, constants, work);
        }
        let AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.left else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        };
        let needs_read = assignment.operator != AssignmentOperator::Assign;
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        self.validate_lowered_mutation_reference(reference, needs_read, identifier.span)?;
        let (binding, frame_slot) = match reference {
            LoweredReference::Frame { binding, slot, .. } => (binding, slot),
            LoweredReference::RealmGlobal { slot, .. } => {
                return Self::plan_realm_global_assignment(assignment, slot, flow, work);
            }
        };

        match assignment.operator {
            AssignmentOperator::Assign => {
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
                work.push(ExpressionWork::Visit(&assignment.right));
            }
            AssignmentOperator::LogicalOr
            | AssignmentOperator::LogicalAnd
            | AssignmentOperator::LogicalNullish => {
                let done = flow.new_label(assignment.span)?;
                let branch_kind = match assignment.operator {
                    AssignmentOperator::LogicalOr => BranchKind::IfTrue,
                    AssignmentOperator::LogicalAnd | AssignmentOperator::LogicalNullish => {
                        BranchKind::IfFalse
                    }
                    _ => {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "logical assignment has a short-circuit branch",
                            span: Some(assignment.span),
                        });
                    }
                };
                work.push(ExpressionWork::Bind(done.clone()));
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    identifier.span,
                )));
                work.push(ExpressionWork::Branch {
                    kind: branch_kind,
                    target: done,
                    span: assignment.span,
                });
                if assignment.operator == AssignmentOperator::LogicalNullish {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::IsUndefinedOrNull,
                        Operands::None,
                        identifier.span,
                    )));
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    identifier.span,
                )));
                work.push(ExpressionWork::Emit(self.plan_read_slot(
                    binding,
                    frame_slot,
                    identifier.span,
                )?));
            }
            operator => {
                let binary = operator.to_binary_operator().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "nonlogical compound assignment has a binary operator",
                        span: Some(assignment.span),
                    },
                )?;
                self.push_slot_write(binding, frame_slot, true, identifier.span, work)?;
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    binary_opcode(binary),
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(self.plan_read_slot(
                    binding,
                    frame_slot,
                    identifier.span,
                )?));
            }
        }
        Ok(())
    }

    fn plan_realm_global_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        slot: u16,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let read = PlannedInstruction::new(
            FinalOpcode::GetVar,
            Operands::VarRef(slot),
            assignment.left.span(),
        );
        let write = PlannedInstruction::new(
            FinalOpcode::PutVar,
            Operands::VarRef(slot),
            assignment.left.span(),
        );
        match assignment.operator {
            AssignmentOperator::Assign => {
                work.push(ExpressionWork::Emit(write));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Visit(&assignment.right));
            }
            AssignmentOperator::LogicalOr
            | AssignmentOperator::LogicalAnd
            | AssignmentOperator::LogicalNullish => {
                let done = flow.new_label(assignment.span)?;
                let branch_kind = match assignment.operator {
                    AssignmentOperator::LogicalOr => BranchKind::IfTrue,
                    AssignmentOperator::LogicalAnd | AssignmentOperator::LogicalNullish => {
                        BranchKind::IfFalse
                    }
                    _ => {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "logical assignment has a short-circuit branch",
                            span: Some(assignment.span),
                        });
                    }
                };
                work.push(ExpressionWork::Bind(done.clone()));
                work.push(ExpressionWork::Emit(write));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    assignment.left.span(),
                )));
                work.push(ExpressionWork::Branch {
                    kind: branch_kind,
                    target: done,
                    span: assignment.span,
                });
                if assignment.operator == AssignmentOperator::LogicalNullish {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::IsUndefinedOrNull,
                        Operands::None,
                        assignment.left.span(),
                    )));
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.left.span(),
                )));
                work.push(ExpressionWork::Emit(read));
            }
            operator => {
                let binary = operator.to_binary_operator().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "nonlogical compound assignment has a binary operator",
                        span: Some(assignment.span),
                    },
                )?;
                work.push(ExpressionWork::Emit(write));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    binary_opcode(binary),
                    Operands::None,
                    assignment.span,
                )));
                work.push(ExpressionWork::Visit(&assignment.right));
                work.push(ExpressionWork::Emit(read));
            }
        }
        Ok(())
    }

    fn plan_static_member_assignment<'expression>(
        assignment: &'expression AssignmentExpression<'arena>,
        member: &'expression StaticMemberExpression<'arena>,
        constants: &CompiledConstantPool,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if assignment.operator != AssignmentOperator::Assign || member.optional {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                assignment.left.span(),
            );
        }
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::PutField,
            Operands::Atom(constants.property_atom_index(member.property.span)?),
            member.span,
        )));
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            FinalOpcode::Insert2,
            Operands::None,
            assignment.span,
        )));
        work.push(ExpressionWork::Visit(&assignment.right));
        work.push(ExpressionWork::Visit(&member.object));
        Ok(())
    }

    fn plan_update_expression<'expression>(
        &self,
        update: &'expression UpdateExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) = &update.argument
        else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedExpression,
                update.argument.span(),
            );
        };
        let reference = self.lowered_reference(
            identifier.reference_id.get(),
            identifier.span,
            layout,
            tree_layout,
        )?;
        self.validate_lowered_mutation_reference(reference, true, identifier.span)?;
        let (binding, frame_slot) = match reference {
            LoweredReference::Frame { binding, slot, .. } => (binding, slot),
            LoweredReference::RealmGlobal { slot, .. } => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::PutVar,
                    Operands::VarRef(slot),
                    identifier.span,
                )));
                if update.prefix {
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Dup,
                        Operands::None,
                        update.span,
                    )));
                }
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    match (update.operator, update.prefix) {
                        (UpdateOperator::Increment, true) => FinalOpcode::Inc,
                        (UpdateOperator::Decrement, true) => FinalOpcode::Dec,
                        (UpdateOperator::Increment, false) => FinalOpcode::PostInc,
                        (UpdateOperator::Decrement, false) => FinalOpcode::PostDec,
                    },
                    Operands::None,
                    update.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::GetVar,
                    Operands::VarRef(slot),
                    identifier.span,
                )));
                return Ok(());
            }
        };

        self.push_slot_write(binding, frame_slot, update.prefix, identifier.span, work)?;
        work.push(ExpressionWork::Emit(PlannedInstruction::new(
            match (update.operator, update.prefix) {
                (UpdateOperator::Increment, true) => FinalOpcode::Inc,
                (UpdateOperator::Decrement, true) => FinalOpcode::Dec,
                (UpdateOperator::Increment, false) => FinalOpcode::PostInc,
                (UpdateOperator::Decrement, false) => FinalOpcode::PostDec,
            },
            Operands::None,
            update.span,
        )));
        work.push(ExpressionWork::Emit(self.plan_read_slot(
            binding,
            frame_slot,
            identifier.span,
        )?));
        Ok(())
    }

    fn plan_unary_expression<'expression>(
        &self,
        unary: &'expression UnaryExpression<'arena>,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
        flow: &mut PlannedControlFlow,
    ) -> Result<(), LeafCompilationError> {
        if unary.operator == UnaryOperator::UnaryNegation
            && let Expression::NumericLiteral(literal) = &unary.argument
            && literal.value != 0.0
            && let Some(integer) = exact_negated_i32(literal.value)
        {
            flow.emit(plan_push_integer(integer, unary.span))?;
            return Ok(());
        }
        if unary.operator == UnaryOperator::Typeof {
            let mut argument = &unary.argument;
            while let Expression::ParenthesizedExpression(parenthesized) = argument {
                argument = &parenthesized.expression;
            }
            if let Expression::Identifier(identifier) = argument {
                let reference = self.lowered_reference(
                    identifier.reference_id.get(),
                    identifier.span,
                    layout,
                    tree_layout,
                )?;
                if let LoweredReference::RealmGlobal { slot, access } = reference {
                    if !access.reads() || access.writes() {
                        return unsupported(
                            UnsupportedLeafFeature::UnsupportedReference,
                            identifier.span,
                        );
                    }
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::Typeof,
                        Operands::None,
                        unary.span,
                    )));
                    work.push(ExpressionWork::Emit(PlannedInstruction::new(
                        FinalOpcode::GetVarUndef,
                        Operands::VarRef(slot),
                        identifier.span,
                    )));
                    return Ok(());
                }
            }
        }
        match unary.operator {
            UnaryOperator::UnaryPlus
            | UnaryOperator::UnaryNegation
            | UnaryOperator::LogicalNot
            | UnaryOperator::BitwiseNot
            | UnaryOperator::Typeof => {
                let opcode = unary_opcode(unary.operator).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "supported unary operator has final opcode",
                        span: Some(unary.span),
                    },
                )?;
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    opcode,
                    Operands::None,
                    unary.span,
                )));
                work.push(ExpressionWork::Visit(&unary.argument));
            }
            UnaryOperator::Void => {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Undefined,
                    Operands::None,
                    unary.span,
                )));
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    unary.argument.span(),
                )));
                work.push(ExpressionWork::Visit(&unary.argument));
            }
            UnaryOperator::Delete => {
                return unsupported(UnsupportedLeafFeature::UnsupportedExpression, unary.span);
            }
        }
        Ok(())
    }

    fn plan_conditional_expression<'expression>(
        conditional: &'expression ConditionalExpression<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let alternate = flow.new_label(conditional.alternate.span())?;
        let done = flow.new_label(conditional.span)?;

        work.push(ExpressionWork::Bind(done.clone()));
        work.push(ExpressionWork::Visit(&conditional.alternate));
        work.push(ExpressionWork::Bind(alternate.clone()));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::Goto,
            target: done,
            span: conditional.span,
        });
        work.push(ExpressionWork::Visit(&conditional.consequent));
        work.push(ExpressionWork::Branch {
            kind: BranchKind::IfFalse,
            target: alternate,
            span: conditional.test.span(),
        });
        work.push(ExpressionWork::Visit(&conditional.test));
        Ok(())
    }

    fn plan_logical_expression<'expression>(
        logical: &'expression LogicalExpression<'arena>,
        flow: &mut PlannedControlFlow,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        let done = flow.new_label(logical.span)?;
        let mut operands = same_operator_left_chain(logical);
        let final_operand = operands
            .pop()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc logical expression has two operands",
                span: Some(logical.span),
            })?;
        let branch_kind = match logical.operator {
            LogicalOperator::Or => BranchKind::IfTrue,
            LogicalOperator::And | LogicalOperator::Coalesce => BranchKind::IfFalse,
        };

        work.push(ExpressionWork::Bind(done.clone()));
        work.push(ExpressionWork::Visit(final_operand));
        for operand in operands.into_iter().rev() {
            let span = operand.span();
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                span,
            )));
            work.push(ExpressionWork::Branch {
                kind: branch_kind,
                target: done.clone(),
                span,
            });
            if logical.operator == LogicalOperator::Coalesce {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::IsUndefinedOrNull,
                    Operands::None,
                    span,
                )));
            }
            work.push(ExpressionWork::Emit(PlannedInstruction::new(
                FinalOpcode::Dup,
                Operands::None,
                span,
            )));
            work.push(ExpressionWork::Visit(operand));
        }
        Ok(())
    }

    fn plan_sequence_expression<'expression>(
        sequence: &'expression SequenceExpression<'arena>,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        if sequence.expressions.is_empty() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc sequence expression is nonempty",
                span: Some(sequence.span),
            });
        }
        for (index, expression) in sequence.expressions.iter().enumerate().rev() {
            if index + 1 != sequence.expressions.len() {
                work.push(ExpressionWork::Emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    expression.span(),
                )));
            }
            work.push(ExpressionWork::Visit(expression));
        }
        Ok(())
    }

    fn plan_read_slot(
        &self,
        binding: BindingId,
        frame_slot: FrameSlot,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        match frame_slot {
            FrameSlot::Argument(slot) => {
                let (opcode, operands) = compact_get_argument(slot);
                Ok(PlannedInstruction::new(opcode, operands, span))
            }
            FrameSlot::Local(slot) => {
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "read compiler binding exists",
                        span: Some(span),
                    },
                )?;
                if storage.policy().has_temporal_dead_zone() {
                    Ok(PlannedInstruction::new(
                        FinalOpcode::GetLocCheck,
                        Operands::Loc(slot.index()),
                        span,
                    ))
                } else {
                    let (opcode, operands) = compact_get_local(slot);
                    Ok(PlannedInstruction::new(opcode, operands, span))
                }
            }
            FrameSlot::Capture(slot) => {
                let storage = self.planned.plan.binding(binding).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured read compiler binding exists",
                        span: Some(span),
                    },
                )?;
                if storage.policy().has_temporal_dead_zone() {
                    Ok(PlannedInstruction::new(
                        FinalOpcode::GetVarRefCheck,
                        Operands::VarRef(slot),
                        span,
                    ))
                } else {
                    let (opcode, operands) = compact_get_capture(slot);
                    Ok(PlannedInstruction::new(opcode, operands, span))
                }
            }
        }
    }

    fn plan_write_slot(
        &self,
        binding: BindingId,
        frame_slot: FrameSlot,
        preserve_value: bool,
        span: Span,
    ) -> Result<Vec<PlannedInstruction>, LeafCompilationError> {
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "written compiler binding exists",
                    span: Some(span),
                })?;
        if storage.policy().writes() != WritePolicy::Mutable {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }

        let mut instructions = Vec::with_capacity(2);
        let instruction = match frame_slot {
            FrameSlot::Argument(slot) => {
                let (opcode, operands) = if preserve_value {
                    compact_set_argument(slot)
                } else {
                    compact_put_argument(slot)
                };
                PlannedInstruction::new(opcode, operands, span)
            }
            FrameSlot::Local(slot) if storage.policy().has_temporal_dead_zone() => {
                PlannedInstruction::new(
                    if preserve_value {
                        FinalOpcode::SetLocCheck
                    } else {
                        FinalOpcode::PutLocCheck
                    },
                    Operands::Loc(slot.index()),
                    span,
                )
            }
            FrameSlot::Local(slot) => {
                let (opcode, operands) = if preserve_value {
                    compact_set_local(slot)
                } else {
                    compact_put_local(slot)
                };
                PlannedInstruction::new(opcode, operands, span)
            }
            FrameSlot::Capture(slot)
                if preserve_value && storage.policy().has_temporal_dead_zone() =>
            {
                instructions.push(PlannedInstruction::new(
                    FinalOpcode::Dup,
                    Operands::None,
                    span,
                ));
                PlannedInstruction::new(FinalOpcode::PutVarRefCheck, Operands::VarRef(slot), span)
            }
            FrameSlot::Capture(slot) if storage.policy().has_temporal_dead_zone() => {
                PlannedInstruction::new(FinalOpcode::PutVarRefCheck, Operands::VarRef(slot), span)
            }
            FrameSlot::Capture(slot) => {
                let (opcode, operands) = if preserve_value {
                    compact_set_capture(slot)
                } else {
                    compact_put_capture(slot)
                };
                PlannedInstruction::new(opcode, operands, span)
            }
        };
        instructions.push(instruction);
        Ok(instructions)
    }

    fn push_slot_write<'expression>(
        &self,
        binding: BindingId,
        frame_slot: FrameSlot,
        preserve_value: bool,
        span: Span,
        work: &mut Vec<ExpressionWork<'expression, 'arena>>,
    ) -> Result<(), LeafCompilationError> {
        for instruction in self
            .plan_write_slot(binding, frame_slot, preserve_value, span)?
            .into_iter()
            .rev()
        {
            work.push(ExpressionWork::Emit(instruction));
        }
        Ok(())
    }

    fn compiler_capture_layout(
        &self,
        executable: ExecutableId,
        _function_scope: ScopeId,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<CompilerCaptureLayout, LeafCompilationError> {
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let mut captured = Vec::new();
        for binding in bindings {
            if !binding.is_frame_captured() {
                continue;
            }
            let expected_index =
                checked_function_index(captured.len(), "function variable references")?;
            if tree_layout.variable_reference(binding.id()) != Some(expected_index) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "captured owner binding has its dense variable-reference index",
                    span: binding.declaration_spans().first().copied(),
                });
            }
            let frame_slot =
                layout
                    .slot(binding.id())
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "captured owner binding has a frame slot",
                        span: binding.declaration_spans().first().copied(),
                    })?;
            let captured_binding = match frame_slot {
                FrameSlot::Argument(slot) => CompilerCapturedBinding::Argument(u32::from(slot.0)),
                FrameSlot::Local(slot) => {
                    if binding_has_scope(binding.policy()) {
                        CompilerCapturedBinding::ScopedLocal(u32::from(slot.index()))
                    } else {
                        CompilerCapturedBinding::FunctionLocal(u32::from(slot.index()))
                    }
                }
                FrameSlot::Capture(_) => {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "captured owner binding is not an imported capture",
                        span: binding.declaration_spans().first().copied(),
                    });
                }
            };
            captured.push(captured_binding);
        }
        Ok(CompilerCaptureLayout::new(Arc::from(captured)))
    }

    fn compiled_variable_definitions(
        &self,
        executable: ExecutableId,
        function_scope: ScopeId,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<Vec<VariableDefinition>, LeafCompilationError> {
        let executable_metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let argument_count =
            usize::try_from(executable_metadata.parameter_count()).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "function argument definitions",
                }
            })?;
        let mut arguments = vec![None; argument_count];
        let bindings = self
            .planned
            .plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        for binding in bindings {
            let StoragePlacement::Argument { parameter_index } = binding.placement() else {
                continue;
            };
            let index = usize::try_from(parameter_index).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "function argument definitions",
                }
            })?;
            let target =
                arguments
                    .get_mut(index)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "argument binding indexes its parameter position",
                        span: binding.declaration_spans().first().copied(),
                    })?;
            if target.is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one compiler binding per simple parameter position",
                    span: binding.declaration_spans().first().copied(),
                });
            }
            *target = Some(Self::compiled_variable_definition(
                binding,
                ScopeLink::End,
                false,
                tree_layout,
                constants,
            )?);
        }
        if arguments.iter().any(Option::is_none) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "every simple parameter has an argument definition",
                span: Some(executable_metadata.span()),
            });
        }

        let scope_links = self.compiled_local_scope_links(function_scope, layout)?;
        let capacity = argument_count.checked_add(layout.locals.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "function variable definitions",
            },
        )?;
        let mut definitions = Vec::with_capacity(capacity);
        definitions.extend(arguments.into_iter().flatten());
        for (local, scope_next) in layout.locals.iter().zip(scope_links) {
            let binding = self.planned.plan.binding(local.binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "local definition binding exists",
                    span: Some(executable_metadata.span()),
                },
            )?;
            definitions.push(Self::compiled_variable_definition(
                binding,
                scope_next,
                binding_has_scope(binding.policy()),
                tree_layout,
                constants,
            )?);
        }
        Ok(definitions)
    }

    fn compiled_variable_definition(
        binding: &crate::storage::BindingStorage,
        scope_next: ScopeLink,
        has_scope: bool,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<VariableDefinition, LeafCompilationError> {
        let variable_reference = tree_layout.variable_reference(binding.id()).map(u32::from);
        if binding.is_frame_captured() != variable_reference.is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "captured binding has one dense variable-reference index",
                span: binding.declaration_spans().first().copied(),
            });
        }
        let mut definition = VariableDefinition::new(
            Some(constants.metadata_atom_index(CompiledMetadataAtomKey::Binding(binding.id()))?),
            scope_next,
            verified_storage_policy(binding)?,
            has_scope,
            variable_reference,
        );
        if let Some(initializer) = tree_layout.function_declaration(binding.id()) {
            definition =
                definition.with_function_initializer(constants.function_index(initializer)?);
        }
        Ok(definition)
    }

    fn compiled_local_scope_links(
        &self,
        function_scope: ScopeId,
        layout: &FrameLayout,
    ) -> Result<Vec<ScopeLink>, LeafCompilationError> {
        let scoping = self.unit.semantic().scoping();
        let mut groups = Vec::with_capacity(layout.locals.len());
        let mut preceding = Vec::with_capacity(layout.locals.len());
        let mut first_by_scope = HashMap::new();
        for (index, local) in layout.locals.iter().enumerate() {
            let binding = self.planned.plan.binding(local.binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "scope-linked local binding exists",
                    span: None,
                },
            )?;
            let semantic_scope = self.scope_for_binding(binding.id())?;
            let group = if !binding_has_scope(binding.policy()) {
                LogicalCompilerScope::Function
            } else if semantic_scope == function_scope {
                LogicalCompilerScope::Body
            } else {
                LogicalCompilerScope::Oxc(semantic_scope)
            };
            let index =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "function local scope links",
                })?;
            preceding.push(first_by_scope.insert(group, index));
            groups.push(group);
        }

        let mut links = Vec::with_capacity(layout.locals.len());
        for (index, (&group, same_scope)) in groups.iter().zip(preceding).enumerate() {
            if let Some(previous) = same_scope {
                links.push(ScopeLink::Local(previous));
                continue;
            }
            let parent = match group {
                LogicalCompilerScope::Function | LogicalCompilerScope::Body => None,
                LogicalCompilerScope::Oxc(scope) => {
                    let mut parent = scoping.scope_parent_id(scope);
                    let mut found = None;
                    while let Some(candidate) = parent {
                        if candidate == function_scope {
                            found = first_by_scope.get(&LogicalCompilerScope::Body).copied();
                            break;
                        }
                        if let Some(first) = first_by_scope
                            .get(&LogicalCompilerScope::Oxc(candidate))
                            .copied()
                        {
                            found = Some(first);
                            break;
                        }
                        parent = scoping.scope_parent_id(candidate);
                    }
                    found
                }
            };
            let current =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "function local scope links",
                })?;
            if parent == Some(current) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "local scope link does not target itself",
                    span: None,
                });
            }
            links.push(parent.map_or(ScopeLink::End, ScopeLink::Local));
        }
        Ok(links)
    }

    fn compiled_closure_definitions(
        &self,
        closures: &[CompiledClosureVariable],
        realm_globals: &[CompiledRealmGlobal],
        constants: &CompiledConstantPool,
    ) -> Result<Vec<VerifiedClosureVariableDefinition>, LeafCompilationError> {
        let capacity = closures.len().checked_add(realm_globals.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "closure metadata definitions",
            },
        )?;
        let mut definitions = Vec::with_capacity(capacity);
        for closure in closures {
            let binding = self.planned.plan.binding(closure.binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "closure metadata binding exists",
                    span: None,
                },
            )?;
            let source = match closure.source {
                CompiledClosureSource::ParentVariableReference(index) => {
                    CompilerGraphClosureSource::ParentVariableReference(u32::from(index))
                }
                CompiledClosureSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            };
            definitions.push(VerifiedClosureVariableDefinition::new(
                Some(
                    constants
                        .metadata_atom_index(CompiledMetadataAtomKey::Binding(closure.binding))?,
                ),
                verified_storage_policy(binding)?,
                source,
            ));
        }
        for global in realm_globals {
            let name = global.atom;
            let source = match global.source {
                CompiledRealmGlobalSource::ConstructorRealm => {
                    CompilerGraphClosureSource::ConstructorRealmGlobal(name)
                }
                CompiledRealmGlobalSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            };
            let mut definition =
                VerifiedClosureVariableDefinition::realm_global(Some(name), global.policy, source);
            if let Some(initializer) = global.function_initializer {
                definition = definition.with_function_initializer(initializer);
            }
            definitions.push(definition);
        }
        Ok(definitions)
    }

    fn compiled_closure_variables(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<Vec<CompiledClosureVariable>, LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let captures = self
            .planned
            .plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        if captures.is_empty() {
            return Ok(Vec::new());
        }
        let parent = metadata
            .parent()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "capturing executable has an immediate parent",
                span: Some(metadata.span()),
            })?;
        let parent_captures = self
            .planned
            .plan
            .frame_captures_for(parent)
            .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
        let mut variables = Vec::with_capacity(captures.len());
        let mut sources = Vec::with_capacity(captures.len());
        for (expected_slot, capture) in captures.iter().enumerate() {
            if capture.slot().index() != expected_slot {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled closure-variable slots are dense and ordered",
                    span: self
                        .planned
                        .plan
                        .binding(capture.binding())
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
            let binding = self.planned.plan.binding(capture.binding()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "compiled closure variable has an original binding",
                    span: None,
                },
            )?;
            let source = match capture.source() {
                CaptureSource::ParentBinding(source_binding) => {
                    if source_binding != capture.binding() || binding.executable() != parent {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "parent-binding closure source names the captured parent binding",
                            span: binding.declaration_spans().first().copied(),
                        });
                    }
                    let index = tree_layout.variable_reference(source_binding).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "parent-binding closure source has a variable-reference cell",
                            span: binding.declaration_spans().first().copied(),
                        },
                    )?;
                    CompiledClosureSource::ParentVariableReference(index)
                }
                CaptureSource::ParentCapture(source_slot) => {
                    let source_capture = parent_captures.get(source_slot.index()).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "forwarded closure source indexes the parent environment",
                            span: binding.declaration_spans().first().copied(),
                        },
                    )?;
                    if source_capture.slot() != source_slot
                        || source_capture.binding() != capture.binding()
                    {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "forwarded closure source preserves the original binding identity",
                            span: binding.declaration_spans().first().copied(),
                        });
                    }
                    CompiledClosureSource::ParentClosure(checked_function_index(
                        source_slot.index(),
                        "parent closure variables",
                    )?)
                }
            };
            sources.push(source);
            variables.push(CompiledClosureVariable {
                binding: capture.binding(),
                slot: capture.slot(),
                source,
                policy: binding.policy(),
            });
        }
        sources.sort_unstable();
        if sources.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled closure sources are unique within one child",
                span: Some(metadata.span()),
            });
        }
        Ok(variables)
    }

    fn compiled_realm_globals(
        &self,
        executable: ExecutableId,
        tree_layout: &FunctionTreeLayout,
        constants: &CompiledConstantPool,
    ) -> Result<Vec<CompiledRealmGlobal>, LeafCompilationError> {
        let metadata = self
            .planned
            .plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let imports = tree_layout.realm_globals.imports_for(executable)?;
        let mut globals = Vec::with_capacity(imports.len());
        for &id in imports {
            let binding = tree_layout.realm_globals.binding(id).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "constructor-realm global import has a binding descriptor",
                    span: Some(metadata.span()),
                },
            )?;
            let slot =
                tree_layout
                    .realm_globals
                    .closure_slot(&self.planned.plan, executable, id)?;
            let source = if let Some(parent) = metadata.parent() {
                CompiledRealmGlobalSource::ParentClosure(tree_layout.realm_globals.closure_slot(
                    &self.planned.plan,
                    parent,
                    id,
                )?)
            } else {
                if self.unit.goal()
                    != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
                    || executable.index() != 0
                {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "only a dynamic Function Script root originates realm-global slots",
                        span: Some(metadata.span()),
                    });
                }
                CompiledRealmGlobalSource::ConstructorRealm
            };
            let function_initializer = if source == CompiledRealmGlobalSource::ConstructorRealm
                && binding.policy.kind() == VerifiedBindingKind::Function
            {
                let declaration = binding.declaration.ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant:
                            "constructor-realm function retains its declared binding identity",
                        span: Some(binding.first_span),
                    },
                )?;
                let child = tree_layout.function_declaration(declaration).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant:
                            "constructor-realm function declaration selects its last initializer",
                        span: Some(binding.first_span),
                    },
                )?;
                Some(constants.function_index(child)?)
            } else {
                None
            };
            globals.push(CompiledRealmGlobal {
                id,
                name: Arc::clone(&binding.name),
                atom: constants.metadata_atom_index(CompiledMetadataAtomKey::RealmGlobal(id))?,
                slot,
                source,
                policy: binding.policy,
                function_initializer,
            });
        }
        Ok(globals)
    }

    fn scope_for_binding(&self, binding: BindingId) -> Result<ScopeId, LeafCompilationError> {
        self.planned
            .identities
            .scope_by_binding
            .get(binding.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "compiler binding has an Oxc scope identity",
                span: self
                    .planned
                    .plan
                    .binding(binding)
                    .and_then(|storage| storage.declaration_spans().first().copied()),
            })
    }

    fn selected_ordinary_function(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Function<'arena>, OrdinaryFunctionForm), LeafCompilationError> {
        let executable = self.planned.plan.executable(executable_id).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: executable_id,
            },
        )?;
        let node_id = self
            .planned
            .identities
            .node_by_executable
            .get(executable_id.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "executable has an Oxc node identity",
                span: Some(executable.span()),
            })?;
        if self
            .planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            != Some(executable_id)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc node and executable identities are bijective",
                span: Some(executable.span()),
            });
        }

        let AstKind::Function(function) = self.unit.semantic().nodes().kind(node_id) else {
            return unsupported(
                UnsupportedLeafFeature::NonOrdinaryFunction,
                executable.span(),
            );
        };
        if function.r#async || function.generator {
            return unsupported(UnsupportedLeafFeature::NonOrdinaryFunction, function.span);
        }
        let is_declaration = function.r#type == FunctionType::FunctionDeclaration;
        let is_expression = function.r#type == FunctionType::FunctionExpression;
        if !is_declaration && !is_expression {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedFunctionForm,
                function.span,
            );
        }
        let form = object_method_or_accessor_span(self.unit, node_id)
            .map_or(OrdinaryFunctionForm::Function, |property_span| {
                OrdinaryFunctionForm::ObjectMethod { property_span }
            });
        if !matches!(
            executable.kind(),
            ExecutableKind::Function {
                asynchronous: false,
                generator: false,
            }
        ) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary Oxc function has ordinary executable metadata",
                span: Some(function.span),
            });
        }
        if self.planned.plan.kind() != CompilationUnitKind::Script {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                function.span,
            );
        }
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function)
            && let Some(reference) = self
                .planned
                .plan
                .unresolved_globals_for(executable_id)
                .and_then(|references| references.first())
        {
            return unsupported(
                UnsupportedLeafFeature::UnresolvedReference,
                reference.span(),
            );
        }
        Ok((executable, function, form))
    }

    fn selected_dynamic_function_script(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Program<'arena>), LeafCompilationError> {
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) {
            return unsupported(
                UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                self.unit.program().span,
            );
        }
        let executable = self.planned.plan.executable(executable_id).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: executable_id,
            },
        )?;
        let node_id = self
            .planned
            .identities
            .node_by_executable
            .get(executable_id.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Script executable has an Oxc node identity",
                span: Some(executable.span()),
            })?;
        if self
            .planned
            .identities
            .executable_by_node
            .get(node_id.index())
            .copied()
            .flatten()
            != Some(executable_id)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Script Oxc node and executable identities are bijective",
                span: Some(executable.span()),
            });
        }
        let AstKind::Program(program) = self.unit.semantic().nodes().kind(node_id) else {
            return unsupported(
                UnsupportedLeafFeature::DynamicFunctionRequiresScriptRoot,
                executable.span(),
            );
        };
        if executable_id.index() != 0
            || executable.parent().is_some()
            || executable.parameter_count() != 0
            || executable.is_strict()
            || !matches!(
                executable.kind(),
                ExecutableKind::Script {
                    asynchronous: false
                }
            )
            || self.planned.plan.kind() != CompilationUnitKind::Script
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary dynamic Function has one synchronous sloppy Script root",
                span: Some(program.span),
            });
        }
        Ok((executable, program))
    }

    fn binding_for_identifier(
        &self,
        symbol_id: Option<SymbolId>,
        span: Span,
    ) -> Result<BindingId, LeafCompilationError> {
        let symbol_id = symbol_id.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "binding identifier has Oxc symbol identity",
            span: Some(span),
        })?;
        self.planned
            .identities
            .binding_by_symbol
            .get(symbol_id.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc symbol has compiler binding identity",
                span: Some(span),
            })
    }

    fn lowered_reference(
        &self,
        reference_id: Option<ReferenceId>,
        span: Span,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<LoweredReference, LeafCompilationError> {
        let reference_id = reference_id.ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "identifier reference has Oxc reference identity",
            span: Some(span),
        })?;
        let native = self
            .planned
            .identities
            .reference_by_id
            .get(reference_id.index())
            .copied()
            .flatten()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc reference has compiler identity",
                span: Some(span),
            })?;
        match native {
            NativeReferenceId::Resolved(resolved_id) => {
                let reference = self.planned.plan.resolved_reference(resolved_id).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "resolved compiler reference exists",
                        span: Some(span),
                    },
                )?;
                if reference.span() != span {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "resolved compiler reference retains its Oxc span",
                        span: Some(span),
                    });
                }
                let binding = self.planned.plan.binding(reference.binding()).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "resolved compiler binding exists",
                        span: Some(span),
                    },
                )?;
                match binding.placement() {
                    StoragePlacement::Argument { .. } | StoragePlacement::Local => {
                        let slot =
                            layout
                                .slot(binding.id())
                                .ok_or(LeafCompilationError::Unsupported {
                                    feature: UnsupportedLeafFeature::UnsupportedBinding,
                                    span,
                                })?;
                        Ok(LoweredReference::Frame {
                            binding: binding.id(),
                            slot,
                            access: reference.access(),
                        })
                    }
                    StoragePlacement::GlobalObject => self.lowered_realm_global_binding_reference(
                        binding.id(),
                        reference.access(),
                        span,
                        layout,
                        tree_layout,
                    ),
                    StoragePlacement::GlobalLexical => {
                        unsupported(UnsupportedLeafFeature::GlobalEnvironment, span)
                    }
                    StoragePlacement::ModuleLocal | StoragePlacement::ModuleImport => {
                        unsupported(UnsupportedLeafFeature::UnsupportedBinding, span)
                    }
                }
            }
            NativeReferenceId::Unresolved(unresolved_id) => {
                self.lowered_unresolved_reference(unresolved_id, span, layout, tree_layout)
            }
        }
    }

    fn lowered_unresolved_reference(
        &self,
        unresolved: UnresolvedGlobalId,
        span: Span,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<LoweredReference, LeafCompilationError> {
        let reference = self
            .planned
            .plan
            .unresolved_globals()
            .get(unresolved.index())
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "unresolved compiler reference exists",
                span: Some(span),
            })?;
        if reference.span() != span {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "unresolved compiler reference retains its Oxc span",
                span: Some(span),
            });
        }
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) {
            return unsupported(UnsupportedLeafFeature::UnresolvedReference, span);
        }
        let global = tree_layout.realm_globals.for_unresolved(unresolved).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "dynamic unresolved reference has a constructor-realm global identity",
                span: Some(span),
            },
        )?;
        let slot = tree_layout.realm_globals.closure_slot(
            &self.planned.plan,
            layout.executable,
            global,
        )?;
        Ok(LoweredReference::RealmGlobal {
            slot,
            access: reference.access(),
        })
    }

    fn lowered_realm_global_binding_reference(
        &self,
        binding: BindingId,
        access: ReferenceAccess,
        span: Span,
        layout: &FrameLayout,
        tree_layout: &FunctionTreeLayout,
    ) -> Result<LoweredReference, LeafCompilationError> {
        if self.unit.goal() != CompilationGoal::DynamicFunction(DynamicFunctionKind::Function) {
            return unsupported(UnsupportedLeafFeature::GlobalEnvironment, span);
        }
        let global = tree_layout.realm_globals.for_binding(binding).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "dynamic Program global binding has a realm-global identity",
                span: Some(span),
            },
        )?;
        let slot = tree_layout.realm_globals.closure_slot(
            &self.planned.plan,
            layout.executable,
            global,
        )?;
        Ok(LoweredReference::RealmGlobal { slot, access })
    }

    fn validate_lowered_mutation_reference(
        &self,
        reference: LoweredReference,
        needs_read: bool,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if !reference.access().writes() || reference.access().reads() != needs_read {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }
        if let LoweredReference::Frame { binding, .. } = reference {
            let storage = self.planned.plan.binding(binding).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "written compiler binding exists",
                    span: Some(span),
                },
            )?;
            if storage.policy().writes() != WritePolicy::Mutable {
                return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
            }
        }
        Ok(())
    }
}

fn verify_compiled_function_graph(
    root: ExecutableId,
    functions: &[CompiledFunction],
    limits: FunctionGraphVerificationLimits,
) -> Result<VerifiedCompilerFunctionGraph, LeafCompilationError> {
    if functions.first().map(CompiledFunction::executable) != Some(root) {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "compiled subtree begins with its selected root",
            span: None,
        });
    }
    let mut identities = Vec::with_capacity(functions.len());
    for (index, function) in functions.iter().enumerate() {
        if identities
            .last()
            .is_some_and(|(previous, _)| *previous >= function.executable())
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled subtree executables are strictly ordered",
                span: None,
            });
        }
        identities.push((function.executable(), checked_function_template_id(index)?));
    }
    let root = resolve_function_template_id(&identities, root)?;
    let root_index =
        usize::try_from(root.get()).map_err(|_| LeafCompilationError::SemanticInvariant {
            invariant: "graph-local root identity fits usize",
            span: None,
        })?;

    let (records, parent_counts) = build_unverified_graph_records(functions, &identities)?;
    for (index, &actual) in parent_counts.iter().enumerate() {
        let expected = u32::from(index != root_index);
        if actual != expected {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled function subtree has exactly one parent per child",
                span: None,
            });
        }
    }

    verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(root, records.into()),
        limits,
    )
    .map_err(|source| {
        let span = source
            .function()
            .and_then(|template| usize::try_from(template.get()).ok())
            .and_then(|index| functions.get(index))
            .and_then(|function| {
                function
                    .storage_plan
                    .executable(function.executable)
                    .map(Executable::span)
            });
        LeafCompilationError::FunctionGraphVerification { span, source }
    })
}

fn build_unverified_graph_records(
    functions: &[CompiledFunction],
    identities: &[(ExecutableId, FunctionTemplateId)],
) -> Result<(Vec<UnverifiedCompilerFunction>, Vec<u32>), LeafCompilationError> {
    let mut records = Vec::with_capacity(functions.len());
    let mut parent_counts = vec![0_u32; functions.len()];
    for function in functions {
        let mut constants = Vec::with_capacity(function.constants.len());
        for constant in function.constants.iter() {
            match constant {
                CompiledConstant::Value(value) => {
                    constants.push(CompilerGraphConstant::Value(value.clone()));
                }
                CompiledConstant::Function(function_constant) => {
                    let template =
                        resolve_function_template_id(identities, function_constant.executable())?;
                    let template_index = usize::try_from(template.get()).map_err(|_| {
                        LeafCompilationError::SemanticInvariant {
                            invariant: "graph-local template identity fits usize",
                            span: None,
                        }
                    })?;
                    let count = parent_counts.get_mut(template_index).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "function constant has a graph parent-count slot",
                            span: None,
                        },
                    )?;
                    *count =
                        count
                            .checked_add(1)
                            .ok_or(LeafCompilationError::CapacityExceeded {
                                domain: "compiler function parent edges",
                            })?;
                    constants.push(CompilerGraphConstant::Function(template));
                }
            }
        }
        let closure_capacity = function
            .closure_variables
            .len()
            .checked_add(function.realm_globals.len())
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "compiler graph closure sources",
            })?;
        let mut closure_sources = Vec::with_capacity(closure_capacity);
        for (index, closure) in function.closure_variables.iter().copied().enumerate() {
            if closure.slot().index() != index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled closure slots are dense and ordered",
                    span: None,
                });
            }
            closure_sources.push(match closure.source() {
                CompiledClosureSource::ParentVariableReference(index) => {
                    CompilerGraphClosureSource::ParentVariableReference(u32::from(index))
                }
                CompiledClosureSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            });
        }
        for (offset, global) in function.realm_globals.iter().enumerate() {
            let expected = function.closure_variables.len().checked_add(offset).ok_or(
                LeafCompilationError::CapacityExceeded {
                    domain: "compiler graph closure sources",
                },
            )?;
            if usize::from(global.slot()) != expected {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled realm-global slots follow captured closure slots",
                    span: None,
                });
            }
            closure_sources.push(match global.source() {
                CompiledRealmGlobalSource::ConstructorRealm => {
                    CompilerGraphClosureSource::ConstructorRealmGlobal(global.atom())
                }
                CompiledRealmGlobalSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            });
        }
        records.push(
            UnverifiedCompilerFunction::new(
                Arc::clone(&function.control_flow),
                constants.into(),
                closure_sources.into(),
            )
            .with_atom_pool(Arc::clone(&function.atoms)),
        );
    }
    Ok((records, parent_counts))
}

fn checked_function_template_id(index: usize) -> Result<FunctionTemplateId, LeafCompilationError> {
    u32::try_from(index)
        .map(FunctionTemplateId::new)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "compiler function graph templates",
        })
}

fn resolve_function_template_id(
    identities: &[(ExecutableId, FunctionTemplateId)],
    executable: ExecutableId,
) -> Result<FunctionTemplateId, LeafCompilationError> {
    identities
        .binary_search_by_key(&executable, |(candidate, _)| *candidate)
        .ok()
        .and_then(|index| identities.get(index))
        .map(|(_, template)| *template)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "function constant belongs to the compiled subtree",
            span: None,
        })
}

fn object_method_or_accessor_span(unit: &ParsedUnit<'_, '_>, node_id: NodeId) -> Option<Span> {
    let AstKind::ObjectProperty(property) = unit.semantic().nodes().parent_kind(node_id) else {
        return None;
    };
    let Expression::FunctionExpression(value) = &property.value else {
        return None;
    };
    (value.node_id.get() == node_id
        && (property.method || !matches!(property.kind, PropertyKind::Init)))
    .then_some(property.span)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinaryFunctionForm {
    Function,
    ObjectMethod { property_span: Span },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectMethodKind {
    Method,
    Getter,
    Setter,
}

impl ObjectMethodKind {
    const ENUMERABLE: u8 = 1 << 2;

    const fn define_method_flags(self) -> u8 {
        Self::ENUMERABLE
            | match self {
                Self::Method => 0,
                Self::Getter => 1,
                Self::Setter => 2,
            }
    }
}

fn same_operator_left_chain<'expression, 'arena>(
    logical: &'expression LogicalExpression<'arena>,
) -> Vec<&'expression Expression<'arena>> {
    let mut reversed = vec![&logical.right];
    let mut left = &logical.left;
    while let Expression::LogicalExpression(inner) = left
        && inner.operator == logical.operator
    {
        reversed.push(&inner.right);
        left = &inner.left;
    }
    reversed.push(left);
    reversed.reverse();
    reversed
}

#[derive(Clone, Copy)]
struct PlannedInstruction {
    opcode: FinalOpcode,
    operands: Operands,
    span: Span,
}

impl PlannedInstruction {
    const fn new(opcode: FinalOpcode, operands: Operands, span: Span) -> Self {
        Self {
            opcode,
            operands,
            span,
        }
    }
}

#[derive(Clone)]
struct CompilerLabel {
    assembler: AssemblerLabel,
    owner_span: Span,
    expected_stack_depth: Option<u32>,
}

enum ExpressionWork<'expression, 'arena> {
    Visit(&'expression Expression<'arena>),
    Emit(PlannedInstruction),
    Branch {
        kind: BranchKind,
        target: CompilerLabel,
        span: Span,
    },
    Bind(CompilerLabel),
}

enum StatementWork<'statement, 'arena> {
    Visit(&'statement Statement<'arena>),
    VisitList {
        statements: &'statement [Statement<'arena>],
        next: usize,
    },
    PushScope {
        scope: ScopeId,
        creator: NodeId,
        span: Span,
    },
    PopScope(ScopeId),
    CloseScope(ScopeId),
    PushLoop(LoopControl),
    PopLoop,
    Declaration(&'statement VariableDeclaration<'arena>),
    Expression(&'statement Expression<'arena>),
    Emit(PlannedInstruction),
    Branch {
        kind: BranchKind,
        target: CompilerLabel,
        span: Span,
    },
    Bind(CompilerLabel),
}

struct StatementPlanningState<'statement, 'arena> {
    work: Vec<StatementWork<'statement, 'arena>>,
    active_scopes: Vec<ScopeId>,
    loop_controls: Vec<LoopControl>,
    completion: StatementCompletion,
}

#[derive(Clone, Copy)]
enum StatementCompletion {
    Discard,
    Script(LocalSlot),
}

#[derive(Clone)]
struct LoopControl {
    break_target: CompilerLabel,
    continue_target: CompilerLabel,
    scope_depth: usize,
}

#[derive(Clone, Copy)]
enum LoopJump {
    Break,
    Continue,
}

impl LoopJump {
    const fn missing_region_invariant(self) -> &'static str {
        match self {
            Self::Break => "Oxc accepts unlabeled break only in a breakable region",
            Self::Continue => "Oxc accepts unlabeled continue only in an iteration",
        }
    }

    const fn scope_invariant(self) -> &'static str {
        match self {
            Self::Break => "break target scope encloses the abrupt statement",
            Self::Continue => "continue target scope encloses the abrupt statement",
        }
    }

    const fn target(self, control: &LoopControl) -> &CompilerLabel {
        match self {
            Self::Break => &control.break_target,
            Self::Continue => &control.continue_target,
        }
    }
}

const fn executable_header(
    kind: CompilerExecutableKind,
    strict: bool,
    argument_count: u32,
    variable_reference_count: u32,
) -> UnverifiedFunctionHeader {
    match kind {
        CompilerExecutableKind::OrdinaryFunction => {
            UnverifiedFunctionHeader::ordinary_source_function_with_variable_references(
                strict,
                argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::OrdinaryMethod => {
            UnverifiedFunctionHeader::ordinary_method_with_variable_references(
                strict,
                argument_count,
                variable_reference_count,
            )
        }
        CompilerExecutableKind::DynamicFunctionScript => {
            UnverifiedFunctionHeader::dynamic_function_script(variable_reference_count)
        }
    }
}

fn checked_function_entry_count<T>(
    count: T,
    domain: &'static str,
) -> Result<u32, LeafCompilationError>
where
    u32: TryFrom<T>,
{
    let count =
        u32::try_from(count).map_err(|_| LeafCompilationError::CapacityExceeded { domain })?;
    if count > MAX_FUNCTION_INDEX_ENTRIES {
        return Err(LeafCompilationError::CapacityExceeded { domain });
    }
    Ok(count)
}

fn checked_function_index<T>(index: T, domain: &'static str) -> Result<u16, LeafCompilationError>
where
    u32: TryFrom<T>,
{
    let index =
        u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded { domain })?;
    if index >= MAX_FUNCTION_INDEX_ENTRIES {
        return Err(LeafCompilationError::CapacityExceeded { domain });
    }
    u16::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded { domain })
}

#[derive(Clone, Copy)]
struct ArgumentSlot(u16);

#[derive(Clone, Copy)]
enum FrameSlot {
    Argument(ArgumentSlot),
    Local(LocalSlot),
    Capture(u16),
}

#[derive(Clone, Copy)]
enum LoweredReference {
    Frame {
        binding: BindingId,
        slot: FrameSlot,
        access: ReferenceAccess,
    },
    RealmGlobal {
        slot: u16,
        access: ReferenceAccess,
    },
}

impl LoweredReference {
    const fn access(self) -> ReferenceAccess {
        match self {
            Self::Frame { access, .. } | Self::RealmGlobal { access, .. } => access,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum LogicalCompilerScope {
    Function,
    Body,
    Oxc(ScopeId),
}

#[derive(Clone, Copy)]
enum ScopeEntryInitialization {
    Uninitialized {
        slot: LocalSlot,
        span: Span,
    },
    Function {
        slot: FrameSlot,
        child: ExecutableId,
        span: Span,
    },
}

impl ScopeEntryInitialization {
    const fn order_key(&self) -> (u8, u16) {
        match self {
            Self::Function {
                slot: FrameSlot::Argument(slot),
                ..
            } => (0, slot.0),
            Self::Uninitialized { slot, .. }
            | Self::Function {
                slot: FrameSlot::Local(slot),
                ..
            } => (1, slot.index()),
            Self::Function {
                slot: FrameSlot::Capture(slot),
                ..
            } => (2, *slot),
        }
    }
}

struct RealmGlobalBinding {
    name: Arc<str>,
    first_span: Span,
    policy: CompilerBindingPolicy,
    declaration: Option<BindingId>,
}

struct RealmGlobalLayout {
    bindings: Box<[RealmGlobalBinding]>,
    by_binding: Box<[Option<RealmGlobalId>]>,
    by_unresolved: Box<[Option<RealmGlobalId>]>,
    import_ranges: Box<[Range<usize>]>,
    imports: Box<[RealmGlobalId]>,
}

struct RealmGlobalLayoutBuilder {
    bindings: Vec<RealmGlobalBinding>,
    by_name: HashMap<Arc<str>, RealmGlobalId>,
    by_binding: Vec<Option<RealmGlobalId>>,
    by_unresolved: Vec<Option<RealmGlobalId>>,
    needs: Vec<Vec<RealmGlobalId>>,
}

impl RealmGlobalLayoutBuilder {
    fn new(plan: &StoragePlan) -> Self {
        Self {
            bindings: Vec::new(),
            by_name: HashMap::new(),
            by_binding: vec![None; plan.bindings().len()],
            by_unresolved: vec![None; plan.unresolved_globals().len()],
            needs: (0..plan.executables().len()).map(|_| Vec::new()).collect(),
        }
    }

    fn collect_declarations(&mut self, plan: &StoragePlan) -> Result<(), LeafCompilationError> {
        for binding in plan.bindings() {
            match binding.placement() {
                StoragePlacement::GlobalObject => {
                    self.collect_declaration(plan, binding)?;
                }
                StoragePlacement::GlobalLexical => {
                    return unsupported(
                        UnsupportedLeafFeature::GlobalEnvironment,
                        binding
                            .declaration_spans()
                            .first()
                            .copied()
                            .unwrap_or_default(),
                    );
                }
                StoragePlacement::Argument { .. }
                | StoragePlacement::Local
                | StoragePlacement::ModuleLocal
                | StoragePlacement::ModuleImport => {}
            }
        }
        Ok(())
    }

    fn collect_declaration(
        &mut self,
        plan: &StoragePlan,
        binding: &crate::storage::BindingStorage,
    ) -> Result<(), LeafCompilationError> {
        let first_span = binding.declaration_spans().first().copied().ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm declaration retains a source span",
                span: None,
            },
        )?;
        let supported_policy = matches!(
            (binding.policy().kind(), binding.policy().initialization()),
            (
                DeclarationKind::Var,
                InitializationPolicy::UndefinedAtInstantiation
            ) | (
                DeclarationKind::Function,
                InitializationPolicy::FunctionAtInstantiation
            )
        );
        if !supported_policy
            || binding.policy().writes() != WritePolicy::Mutable
            || binding.policy().has_temporal_dead_zone()
        {
            return unsupported(UnsupportedLeafFeature::GlobalEnvironment, first_span);
        }
        let owner = plan.executable(binding.executable()).ok_or(
            LeafCompilationError::InvalidExecutable {
                executable: binding.executable(),
            },
        )?;
        if binding.executable().index() != 0 || owner.parent().is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm declaration belongs to the dynamic Script root",
                span: Some(first_span),
            });
        }

        let name: Arc<str> = Arc::from(binding.name());
        if self.by_name.contains_key(&name) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "one declared constructor-realm binding per name",
                span: Some(first_span),
            });
        }
        let id = self.push_binding(RealmGlobalBinding {
            name: Arc::clone(&name),
            first_span,
            policy: verified_storage_policy(binding)?,
            declaration: Some(binding.id()),
        })?;
        self.by_name.insert(name, id);
        let mapping = self.by_binding.get_mut(binding.id().index()).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "binding identity indexes its realm-global mapping",
                span: Some(first_span),
            },
        )?;
        if mapping.replace(id).is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "one realm-global mapping per declared binding",
                span: Some(first_span),
            });
        }
        self.push_need(binding.executable(), id)
    }

    fn collect_unresolved(&mut self, plan: &StoragePlan) -> Result<(), LeafCompilationError> {
        for reference in plan.unresolved_globals() {
            let name: Arc<str> = Arc::from(reference.name());
            let id = if let Some(&id) = self.by_name.get(&name) {
                id
            } else {
                let id = self.push_binding(RealmGlobalBinding {
                    name: Arc::clone(&name),
                    first_span: reference.span(),
                    policy: constructor_realm_lookup_policy(),
                    declaration: None,
                })?;
                self.by_name.insert(name, id);
                id
            };
            let mapping = self.by_unresolved.get_mut(reference.id().index()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "unresolved global identity indexes its realm-global mapping",
                    span: Some(reference.span()),
                },
            )?;
            if mapping.replace(id).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one realm-global mapping per unresolved reference",
                    span: Some(reference.span()),
                });
            }
            self.push_need(reference.executable(), id)?;
        }
        Ok(())
    }

    fn collect_resolved_needs(&mut self, plan: &StoragePlan) -> Result<(), LeafCompilationError> {
        for reference in plan.resolved_references() {
            let Some(global) = self
                .by_binding
                .get(reference.binding().index())
                .copied()
                .flatten()
            else {
                continue;
            };
            self.push_need(reference.executable(), global)?;
        }
        Ok(())
    }

    fn push_binding(
        &mut self,
        binding: RealmGlobalBinding,
    ) -> Result<RealmGlobalId, LeafCompilationError> {
        let raw = u32::try_from(self.bindings.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "constructor-realm global names",
            }
        })?;
        let id = RealmGlobalId(raw);
        self.bindings.push(binding);
        Ok(id)
    }

    fn push_need(
        &mut self,
        executable: ExecutableId,
        global: RealmGlobalId,
    ) -> Result<(), LeafCompilationError> {
        self.needs
            .get_mut(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?
            .push(global);
        Ok(())
    }

    fn finish(mut self, plan: &StoragePlan) -> Result<RealmGlobalLayout, LeafCompilationError> {
        for index in (0..self.needs.len()).rev() {
            self.needs[index].sort_unstable();
            self.needs[index].dedup();
            let Some(parent) = plan.executables()[index].parent() else {
                continue;
            };
            let inherited = self.needs[index].clone();
            self.needs
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?
                .extend(inherited);
        }

        let mut import_ranges = Vec::with_capacity(self.needs.len());
        let mut imports = Vec::new();
        for mut executable_needs in self.needs {
            executable_needs.sort_unstable();
            executable_needs.dedup();
            checked_function_entry_count(executable_needs.len(), "constructor-realm global slots")?;
            let start = imports.len();
            imports.extend(executable_needs);
            import_ranges.push(start..imports.len());
        }
        Ok(RealmGlobalLayout {
            bindings: self.bindings.into_boxed_slice(),
            by_binding: self.by_binding.into_boxed_slice(),
            by_unresolved: self.by_unresolved.into_boxed_slice(),
            import_ranges: import_ranges.into_boxed_slice(),
            imports: imports.into_boxed_slice(),
        })
    }
}

impl RealmGlobalLayout {
    fn new(plan: &StoragePlan, enabled: bool) -> Result<Self, LeafCompilationError> {
        let executable_count = plan.executables().len();
        if !enabled {
            return Ok(Self {
                bindings: Box::default(),
                by_binding: vec![None; plan.bindings().len()].into_boxed_slice(),
                by_unresolved: vec![None; plan.unresolved_globals().len()].into_boxed_slice(),
                import_ranges: vec![0..0; executable_count].into_boxed_slice(),
                imports: Box::default(),
            });
        }

        let mut builder = RealmGlobalLayoutBuilder::new(plan);
        builder.collect_declarations(plan)?;
        builder.collect_unresolved(plan)?;
        builder.collect_resolved_needs(plan)?;
        builder.finish(plan)
    }

    fn binding(&self, id: RealmGlobalId) -> Option<&RealmGlobalBinding> {
        self.bindings.get(id.index())
    }

    fn for_unresolved(&self, id: UnresolvedGlobalId) -> Option<RealmGlobalId> {
        self.by_unresolved.get(id.index()).copied().flatten()
    }

    fn for_binding(&self, id: BindingId) -> Option<RealmGlobalId> {
        self.by_binding.get(id.index()).copied().flatten()
    }

    fn imports_for(
        &self,
        executable: ExecutableId,
    ) -> Result<&[RealmGlobalId], LeafCompilationError> {
        let range = self
            .import_ranges
            .get(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        self.imports
            .get(range.clone())
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "constructor-realm global range indexes flat imports",
                span: None,
            })
    }

    fn closure_slot(
        &self,
        plan: &StoragePlan,
        executable: ExecutableId,
        global: RealmGlobalId,
    ) -> Result<u16, LeafCompilationError> {
        let captures = plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let imports = self.imports_for(executable)?;
        let offset = imports.binary_search(&global).map_err(|_| {
            LeafCompilationError::SemanticInvariant {
                invariant: "referenced constructor-realm global is imported by its executable",
                span: self.binding(global).map(|binding| binding.first_span),
            }
        })?;
        let index =
            captures
                .len()
                .checked_add(offset)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function closure variables",
                })?;
        checked_function_index(index, "function closure variables")
    }
}

struct FunctionTreeLayout {
    child_ranges: Box<[Range<usize>]>,
    children: Box<[ExecutableId]>,
    constant_pools: Box<[CompiledConstantPool]>,
    variable_references: Box<[Option<u16>]>,
    function_declarations: Box<[Option<ExecutableId>]>,
    realm_globals: RealmGlobalLayout,
}

struct FunctionChildLayout {
    child_ranges: Box<[Range<usize>]>,
    children: Box<[ExecutableId]>,
}

impl FunctionTreeLayout {
    fn new(plan: &StoragePlan, allow_realm_globals: bool) -> Result<Self, LeafCompilationError> {
        let executables = plan.executables();
        let FunctionChildLayout {
            child_ranges,
            children,
        } = Self::build_child_layout(executables)?;
        let variable_references = Self::build_variable_references(plan, executables)?;
        Ok(Self {
            child_ranges,
            children,
            constant_pools: Box::default(),
            variable_references,
            function_declarations: vec![None; plan.bindings().len()].into_boxed_slice(),
            realm_globals: RealmGlobalLayout::new(plan, allow_realm_globals)?,
        })
    }

    fn build_child_layout(
        executables: &[Executable],
    ) -> Result<FunctionChildLayout, LeafCompilationError> {
        let child_counts = Self::count_children(executables)?;
        let (child_ranges, child_total) = Self::build_child_ranges(child_counts)?;
        let children = Self::populate_child_tables(executables, &child_ranges, child_total)?;
        Ok(FunctionChildLayout {
            child_ranges: child_ranges.into_boxed_slice(),
            children,
        })
    }

    fn count_children(executables: &[Executable]) -> Result<Vec<usize>, LeafCompilationError> {
        let mut child_counts = vec![0_usize; executables.len()];
        for (expected_index, executable) in executables.iter().enumerate() {
            if executable.id().index() != expected_index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "executable identities are dense and ordered",
                    span: Some(executable.span()),
                });
            }
            let Some(parent) = executable.parent() else {
                continue;
            };
            if parent.index() >= expected_index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "executable parent precedes its child",
                    span: Some(executable.span()),
                });
            }
            let count = child_counts
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            *count = count
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function constants",
                })?;
        }
        Ok(child_counts)
    }

    fn build_child_ranges(
        child_counts: Vec<usize>,
    ) -> Result<(Vec<Range<usize>>, usize), LeafCompilationError> {
        let mut child_ranges = Vec::with_capacity(child_counts.len());
        let mut child_total = 0_usize;
        for count in child_counts {
            let start = child_total;
            child_total =
                child_total
                    .checked_add(count)
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "function constants",
                    })?;
            child_ranges.push(start..child_total);
        }
        Ok((child_ranges, child_total))
    }

    fn populate_child_tables(
        executables: &[Executable],
        child_ranges: &[Range<usize>],
        child_total: usize,
    ) -> Result<Box<[ExecutableId]>, LeafCompilationError> {
        let mut children = vec![None; child_total];
        let mut child_cursors = child_ranges
            .iter()
            .map(|range| range.start)
            .collect::<Vec<_>>();
        for executable in executables {
            let Some(parent) = executable.parent() else {
                continue;
            };
            let cursor = child_cursors
                .get_mut(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            let range = child_ranges
                .get(parent.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable: parent })?;
            if !range.contains(cursor) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "child cursor remains inside its parent range",
                    span: Some(executable.span()),
                });
            }
            let target =
                children
                    .get_mut(*cursor)
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "child cursor indexes the flat child table",
                        span: Some(executable.span()),
                    })?;
            if target.replace(executable.id()).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "child executable has one flat table position",
                    span: Some(executable.span()),
                });
            }
            *cursor = cursor
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function constants",
                })?;
        }
        let children = children
            .into_iter()
            .enumerate()
            .map(|(index, child)| {
                child.ok_or_else(|| LeafCompilationError::SemanticInvariant {
                    invariant: "flat child table is completely populated",
                    span: Self::child_owner_span(executables, child_ranges, index),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(children.into_boxed_slice())
    }

    fn child_owner_span(
        executables: &[Executable],
        child_ranges: &[Range<usize>],
        child_index: usize,
    ) -> Option<Span> {
        let owner_index = child_ranges.partition_point(|range| range.end <= child_index);
        child_ranges
            .get(owner_index)
            .filter(|range| range.contains(&child_index))?;
        executables.get(owner_index).map(Executable::span)
    }

    fn build_variable_references(
        plan: &StoragePlan,
        executables: &[Executable],
    ) -> Result<Box<[Option<u16>]>, LeafCompilationError> {
        let mut variable_references = vec![None; plan.bindings().len()];
        for executable in executables {
            let bindings = plan.bindings_for(executable.id()).ok_or(
                LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                },
            )?;
            let mut capture_count = 0_usize;
            for binding in bindings {
                if !binding.is_frame_captured() {
                    continue;
                }
                let index = checked_function_index(capture_count, "function variable references")?;
                let slot = variable_references.get_mut(binding.id().index()).ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "captured binding indexes variable-reference layout",
                        span: binding.declaration_spans().first().copied(),
                    },
                )?;
                if slot.replace(index).is_some() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "captured binding has one variable-reference index",
                        span: binding.declaration_spans().first().copied(),
                    });
                }
                capture_count =
                    capture_count
                        .checked_add(1)
                        .ok_or(LeafCompilationError::CapacityExceeded {
                            domain: "function variable references",
                        })?;
            }
            checked_function_entry_count(capture_count, "function variable references")?;
        }
        Ok(variable_references.into_boxed_slice())
    }

    fn children(&self, executable: ExecutableId) -> Result<&[ExecutableId], LeafCompilationError> {
        let range = self
            .child_ranges
            .get(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        self.children
            .get(range.clone())
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "executable child range indexes the flat child table",
                span: None,
            })
    }

    fn install_constant_pools(
        &mut self,
        constant_pools: Box<[CompiledConstantPool]>,
    ) -> Result<(), LeafCompilationError> {
        if !self.constant_pools.is_empty() || constant_pools.len() != self.child_ranges.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "constant pools install exactly once for every executable",
                span: None,
            });
        }
        self.constant_pools = constant_pools;
        Ok(())
    }

    fn constant_pool(
        &self,
        executable: ExecutableId,
    ) -> Result<&CompiledConstantPool, LeafCompilationError> {
        self.constant_pools
            .get(executable.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable })
    }

    fn variable_reference(&self, binding: BindingId) -> Option<u16> {
        self.variable_references
            .get(binding.index())
            .copied()
            .flatten()
    }

    fn record_function_declaration(
        &mut self,
        binding: BindingId,
        executable: ExecutableId,
    ) -> Result<(), LeafCompilationError> {
        let target = self.function_declarations.get_mut(binding.index()).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "function declaration binding indexes instantiation layout",
                span: None,
            },
        )?;
        *target = Some(executable);
        Ok(())
    }

    fn function_declaration(&self, binding: BindingId) -> Option<ExecutableId> {
        self.function_declarations
            .get(binding.index())
            .copied()
            .flatten()
    }

    fn subtree_preorder(
        &self,
        root: ExecutableId,
    ) -> Result<Vec<ExecutableId>, LeafCompilationError> {
        self.children(root)?;
        let mut visited = vec![false; self.child_ranges.len()];
        let mut preorder = Vec::new();
        let mut work = vec![root];
        while let Some(executable) = work.pop() {
            let seen = visited
                .get_mut(executable.index())
                .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
            if std::mem::replace(seen, true) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "function subtree has one acyclic parent path",
                    span: None,
                });
            }
            preorder.push(executable);
            for child in self.children(executable)?.iter().rev() {
                work.push(*child);
            }
        }
        Ok(preorder)
    }
}

struct FrameLocal {
    binding: BindingId,
    slot: LocalSlot,
}

#[derive(Clone, Copy)]
struct FrameBindingSlot {
    binding: BindingId,
    slot: FrameSlot,
}

struct FrameLayout {
    executable: ExecutableId,
    slots: Vec<FrameBindingSlot>,
    locals: Vec<FrameLocal>,
    local_count: u32,
}

impl FrameLayout {
    fn new(plan: &StoragePlan, executable: ExecutableId) -> Result<Self, LeafCompilationError> {
        let bindings = plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let captures = plan
            .frame_captures_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        let slot_capacity = bindings.len().checked_add(captures.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "function frame bindings",
            },
        )?;
        let mut slots = Vec::with_capacity(slot_capacity);
        let mut locals = Vec::new();
        let mut local_count = 0_u32;
        let executable_metadata = plan
            .executable(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        checked_function_entry_count(
            executable_metadata.parameter_count(),
            "function argument slots",
        )?;
        for binding in bindings {
            let slot = match binding.placement() {
                StoragePlacement::Argument { parameter_index } => {
                    let parameter_index =
                        checked_function_index(parameter_index, "function argument slots")?;
                    Some(FrameSlot::Argument(ArgumentSlot(parameter_index)))
                }
                StoragePlacement::Local => {
                    let slot =
                        LocalSlot(checked_function_index(local_count, "function local slots")?);
                    local_count += 1;
                    locals.push(FrameLocal {
                        binding: binding.id(),
                        slot,
                    });
                    Some(FrameSlot::Local(slot))
                }
                StoragePlacement::GlobalObject => None,
                StoragePlacement::GlobalLexical
                | StoragePlacement::ModuleLocal
                | StoragePlacement::ModuleImport => {
                    let span = binding
                        .declaration_spans()
                        .first()
                        .copied()
                        .unwrap_or_default();
                    return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
                }
            };
            let Some(slot) = slot else {
                continue;
            };
            slots.push(FrameBindingSlot {
                binding: binding.id(),
                slot,
            });
        }
        checked_function_entry_count(captures.len(), "function capture slots")?;
        for (expected_capture_index, capture) in captures.iter().enumerate() {
            let capture_index =
                checked_function_index(capture.slot().index(), "function capture slots")?;
            if capture.slot().index() != expected_capture_index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "function capture slots are dense and ordered",
                    span: plan
                        .binding(capture.binding())
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
            slots.push(FrameBindingSlot {
                binding: capture.binding(),
                slot: FrameSlot::Capture(capture_index),
            });
        }
        slots.sort_unstable_by_key(|entry| entry.binding);
        for duplicate in slots.windows(2) {
            if duplicate[0].binding == duplicate[1].binding {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one frame or capture slot per compiler binding",
                    span: plan
                        .binding(duplicate[0].binding)
                        .and_then(|binding| binding.declaration_spans().first().copied()),
                });
            }
        }
        Ok(Self {
            executable,
            slots,
            locals,
            local_count,
        })
    }

    fn push_internal_local(&mut self) -> Result<LocalSlot, LeafCompilationError> {
        let slot = LocalSlot(checked_function_index(
            self.local_count,
            "function local slots",
        )?);
        self.local_count =
            self.local_count
                .checked_add(1)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "function local slots",
                })?;
        checked_function_entry_count(self.local_count, "function local slots")?;
        Ok(slot)
    }

    fn slot(&self, binding: BindingId) -> Option<FrameSlot> {
        let index = self
            .slots
            .binary_search_by_key(&binding, |entry| entry.binding)
            .ok()?;
        Some(self.slots[index].slot)
    }
}

struct ValidatedFunction {
    executable_kind: CompilerExecutableKind,
    strict: bool,
    argument_count: u32,
    local_count: u32,
    capture_count: u32,
    capture_layout: CompilerCaptureLayout,
    locals: Vec<LoweredLocal>,
    atoms: Arc<[CompilerAtom]>,
    constants: Arc<[CompiledConstant]>,
    closure_variables: Vec<CompiledClosureVariable>,
    realm_globals: Vec<CompiledRealmGlobal>,
    function_name: Option<AtomPoolIndex>,
    variable_definitions: Vec<VariableDefinition>,
    closure_definitions: Vec<VerifiedClosureVariableDefinition>,
    function_span: SourceByteSpan,
    function_name_span: Option<SourceByteSpan>,
    flow: PlannedControlFlow,
}

#[derive(Clone, Copy)]
struct FunctionPlanningContext<'layout> {
    executable: ExecutableId,
    layout: &'layout FrameLayout,
    tree_layout: &'layout FunctionTreeLayout,
    constants: &'layout CompiledConstantPool,
}

struct CompiledConstantPool {
    atoms: Arc<[CompilerAtom]>,
    entries: Arc<[CompiledConstant]>,
    function_indices: Box<[(ExecutableId, u32)]>,
    number_indices: Box<[(Span, u32)]>,
    string_indices: Box<[(Span, CompiledStringLocation)]>,
    property_atom_indices: Box<[(Span, u32)]>,
    metadata_atom_indices: Box<[(CompiledMetadataAtomKey, u32)]>,
}

enum CompiledConstantCandidate {
    Number {
        value: Binary64Constant,
        span: Span,
    },
    String {
        value: CompilerString,
        span: Span,
    },
    Function {
        executable: ExecutableId,
        span: Span,
    },
}

impl CompiledConstantCandidate {
    const fn order_key(&self) -> (u32, u32, u8) {
        match self {
            Self::Number { span, .. } => (span.start, span.end, 0),
            Self::String { span, .. } => (span.start, span.end, 1),
            Self::Function { span, .. } => (span.start, span.end, 2),
        }
    }
}

struct CompiledAtomCandidate {
    value: CompilerString,
    span: Span,
    purpose: CompiledAtomPurpose,
}

struct CompiledStaticPropertyKey {
    value: CompilerString,
    span: Span,
}

impl CompiledAtomCandidate {
    const fn order_key(&self) -> (u32, u32, CompiledAtomPurpose) {
        (self.span.start, self.span.end, self.purpose)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CompiledAtomPurpose {
    RuntimeString,
    Property,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CompiledMetadataAtomKey {
    FunctionName,
    ScriptCompletion,
    Binding(BindingId),
    RealmGlobal(RealmGlobalId),
}

struct CompiledMetadataAtomCandidate {
    key: CompiledMetadataAtomKey,
    value: CompilerString,
    span: Span,
}

impl CompiledMetadataAtomCandidate {
    const fn order_key(&self) -> (CompiledMetadataAtomKey, u32, u32) {
        (self.key, self.span.start, self.span.end)
    }
}

#[derive(Clone, Copy)]
enum CompiledStringLocation {
    Constant(u32),
    Atom(u32),
}

struct FrozenConstantCandidates {
    entries: Vec<CompiledConstant>,
    function_indices: Vec<(ExecutableId, u32)>,
    number_indices: Vec<(Span, u32)>,
    string_indices: Vec<(Span, CompiledStringLocation)>,
    property_atom_indices: Vec<(Span, u32)>,
}

fn freeze_constant_candidates(
    children: &[ExecutableId],
    candidates: Vec<CompiledConstantCandidate>,
    string_capacity: usize,
) -> Result<FrozenConstantCandidates, LeafCompilationError> {
    let mut frozen = FrozenConstantCandidates {
        entries: Vec::with_capacity(candidates.len()),
        function_indices: Vec::with_capacity(children.len()),
        number_indices: Vec::with_capacity(candidates.len().checked_sub(children.len()).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "constant candidates include every direct child",
                span: None,
            },
        )?),
        string_indices: Vec::with_capacity(string_capacity),
        property_atom_indices: Vec::with_capacity(string_capacity),
    };
    for (index, candidate) in candidates.into_iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "constant pool entries",
        })?;
        match candidate {
            CompiledConstantCandidate::Number { value, span } => {
                frozen
                    .entries
                    .push(CompiledConstant::Value(CompilerConstantValue::Number(
                        value,
                    )));
                frozen.number_indices.push((span, index));
            }
            CompiledConstantCandidate::String { value, span } => {
                if value.is_empty() || !value.is_tagged_integer_atom() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string value constants are nonempty tagged-integer spellings",
                        span: Some(span),
                    });
                }
                frozen
                    .entries
                    .push(CompiledConstant::Value(CompilerConstantValue::String(
                        value,
                    )));
                frozen
                    .string_indices
                    .push((span, CompiledStringLocation::Constant(index)));
            }
            CompiledConstantCandidate::Function { executable, span } => {
                if children.binary_search(&executable).is_err() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "constant-pool function is a direct child",
                        span: Some(span),
                    });
                }
                frozen.function_indices.push((executable, index));
                frozen
                    .entries
                    .push(CompiledConstant::Function(CompiledFunctionConstant {
                        executable,
                    }));
            }
        }
    }
    Ok(frozen)
}

fn freeze_atom_candidates(
    candidates: Vec<CompiledAtomCandidate>,
    string_indices: &mut Vec<(Span, CompiledStringLocation)>,
    property_atom_indices: &mut Vec<(Span, u32)>,
) -> Result<(Vec<CompilerAtom>, HashMap<CompilerString, u32>), LeafCompilationError> {
    let mut atoms = Vec::with_capacity(candidates.len());
    let mut interner = HashMap::with_capacity(candidates.len());
    for candidate in candidates {
        let static_property_only = candidate.purpose == CompiledAtomPurpose::Property
            && (candidate.value.is_empty() || candidate.value.is_tagged_integer_atom());
        if candidate.purpose == CompiledAtomPurpose::RuntimeString
            && (candidate.value.is_empty() || candidate.value.is_tagged_integer_atom())
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "runtime string atoms are nonempty non-tagged-integer spellings",
                span: Some(candidate.span),
            });
        }
        let atom_index = if let Some(&index) = interner.get(&candidate.value) {
            index
        } else {
            let next_count =
                atoms
                    .len()
                    .checked_add(1)
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "atom pool entries",
                    })?;
            checked_function_entry_count(next_count, "atom pool entries")?;
            let index =
                u32::try_from(atoms.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "atom pool entries",
                })?;
            atoms.push(if static_property_only {
                CompilerAtom::new_static_property_only(candidate.value.clone())
            } else {
                CompilerAtom::new(candidate.value.clone())
            });
            interner.insert(candidate.value, index);
            index
        };
        match candidate.purpose {
            CompiledAtomPurpose::RuntimeString => {
                string_indices.push((candidate.span, CompiledStringLocation::Atom(atom_index)));
            }
            CompiledAtomPurpose::Property => {
                property_atom_indices.push((candidate.span, atom_index));
            }
        }
    }
    Ok((atoms, interner))
}

fn freeze_metadata_atom_candidates(
    candidates: Vec<CompiledMetadataAtomCandidate>,
    atoms: &mut Vec<CompilerAtom>,
    interner: &mut HashMap<CompilerString, u32>,
) -> Result<Vec<(CompiledMetadataAtomKey, u32)>, LeafCompilationError> {
    let mut indices = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.value.is_empty() || candidate.value.is_tagged_integer_atom() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "metadata atom names are nonempty identifiers",
                span: Some(candidate.span),
            });
        }
        let atom_index = if let Some(&index) = interner.get(&candidate.value) {
            index
        } else {
            let next_count =
                atoms
                    .len()
                    .checked_add(1)
                    .ok_or(LeafCompilationError::CapacityExceeded {
                        domain: "atom pool entries",
                    })?;
            checked_function_entry_count(next_count, "atom pool entries")?;
            let index =
                u32::try_from(atoms.len()).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "atom pool entries",
                })?;
            atoms.push(CompilerAtom::new(candidate.value.clone()));
            interner.insert(candidate.value, index);
            index
        };
        indices.push((candidate.key, atom_index));
    }
    indices.sort_unstable_by_key(|(key, _)| *key);
    if indices.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "one metadata atom index per function field",
            span: None,
        });
    }
    Ok(indices)
}

fn validate_frozen_constant_candidates(
    children: &[ExecutableId],
    expected_count: u32,
    frozen: &mut FrozenConstantCandidates,
) -> Result<(), LeafCompilationError> {
    if u32::try_from(frozen.entries.len()) != Ok(expected_count) {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "constant-pool candidate count remains stable",
            span: None,
        });
    }
    frozen
        .function_indices
        .sort_unstable_by_key(|(executable, _)| *executable);
    if frozen.function_indices.len() != children.len()
        || !frozen
            .function_indices
            .iter()
            .map(|(executable, _)| *executable)
            .eq(children.iter().copied())
    {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "constant pool owns every direct child exactly once",
            span: None,
        });
    }
    frozen
        .string_indices
        .sort_unstable_by_key(|(span, _)| (span.start, span.end));
    if let Some(span) = frozen
        .string_indices
        .windows(2)
        .find_map(|pair| (pair[0].0 == pair[1].0).then_some(pair[0].0))
    {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "runtime string literal spans are unique within a function",
            span: Some(span),
        });
    }
    frozen
        .property_atom_indices
        .sort_unstable_by_key(|(span, _)| (span.start, span.end));
    if let Some(span) = frozen
        .property_atom_indices
        .windows(2)
        .find_map(|pair| (pair[0].0 == pair[1].0).then_some(pair[0].0))
    {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "static property spans are unique within a function",
            span: Some(span),
        });
    }
    Ok(())
}

impl CompiledConstantPool {
    fn from_candidates(
        children: &[ExecutableId],
        candidates: Vec<CompiledConstantCandidate>,
        atom_candidates: Vec<CompiledAtomCandidate>,
        metadata_atom_candidates: Vec<CompiledMetadataAtomCandidate>,
    ) -> Result<Self, LeafCompilationError> {
        let count = checked_function_entry_count(candidates.len(), "constant pool entries")?;
        if children.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "direct child executables are strictly ordered",
                span: None,
            });
        }
        let string_capacity = candidates.len().checked_add(atom_candidates.len()).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "string literal occurrences",
            },
        )?;
        let mut frozen = freeze_constant_candidates(children, candidates, string_capacity)?;
        let (mut atoms, mut atom_interner) = freeze_atom_candidates(
            atom_candidates,
            &mut frozen.string_indices,
            &mut frozen.property_atom_indices,
        )?;
        let metadata_atom_indices = freeze_metadata_atom_candidates(
            metadata_atom_candidates,
            &mut atoms,
            &mut atom_interner,
        )?;
        validate_frozen_constant_candidates(children, count, &mut frozen)?;
        Ok(Self {
            atoms: atoms.into(),
            entries: frozen.entries.into(),
            function_indices: frozen.function_indices.into_boxed_slice(),
            number_indices: frozen.number_indices.into_boxed_slice(),
            string_indices: frozen.string_indices.into_boxed_slice(),
            property_atom_indices: frozen.property_atom_indices.into_boxed_slice(),
            metadata_atom_indices: metadata_atom_indices.into_boxed_slice(),
        })
    }

    fn metadata_atom_index(
        &self,
        key: CompiledMetadataAtomKey,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        let position = self
            .metadata_atom_indices
            .binary_search_by_key(&key, |(candidate, _)| *candidate)
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "compiled metadata field has a function-local atom",
                span: None,
            })?;
        Ok(AtomPoolIndex::new(self.metadata_atom_indices[position].1))
    }

    fn plan_number(
        &self,
        value: f64,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let position = self
            .number_indices
            .binary_search_by_key(&(span.start, span.end), |(candidate, _)| {
                (candidate.start, candidate.end)
            })
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "non-integer numeric literal has one constant-pool entry",
                span: Some(span),
            })?;
        let index = self.number_indices[position].1;
        let Some(CompiledConstant::Value(CompilerConstantValue::Number(actual))) =
            usize::try_from(index)
                .ok()
                .and_then(|index| self.entries.get(index))
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "numeric constant index resolves to its binary64 payload",
                span: Some(span),
            });
        };
        if *actual != Binary64Constant::from_f64(value) {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "numeric constant retains its parsed binary64 payload",
                span: Some(span),
            });
        }
        let (opcode, operands) = match u8::try_from(index) {
            Ok(index) => (FinalOpcode::PushConst8, Operands::Const8(index)),
            Err(_) => (FinalOpcode::PushConst, Operands::Const(index)),
        };
        Ok(PlannedInstruction::new(opcode, operands, span))
    }

    fn plan_string(&self, span: Span) -> Result<PlannedInstruction, LeafCompilationError> {
        let position = self
            .string_indices
            .binary_search_by_key(&(span.start, span.end), |(candidate, _)| {
                (candidate.start, candidate.end)
            })
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "nonempty runtime string has one pool location",
                span: Some(span),
            })?;
        let instruction = match self.string_indices[position].1 {
            CompiledStringLocation::Constant(index) => {
                let Some(CompiledConstant::Value(CompilerConstantValue::String(value))) =
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| self.entries.get(index))
                else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string constant index resolves to an exact string payload",
                        span: Some(span),
                    });
                };
                if !value.is_tagged_integer_atom() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string constant retains its tagged-integer spelling",
                        span: Some(span),
                    });
                }
                match u8::try_from(index) {
                    Ok(index) => (FinalOpcode::PushConst8, Operands::Const8(index)),
                    Err(_) => (FinalOpcode::PushConst, Operands::Const(index)),
                }
            }
            CompiledStringLocation::Atom(index) => {
                let Some(atom) = usize::try_from(index)
                    .ok()
                    .and_then(|index| self.atoms.get(index))
                else {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string atom index resolves to an exact atom payload",
                        span: Some(span),
                    });
                };
                if atom.string().is_empty() || atom.string().is_tagged_integer_atom() {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "string atom retains its non-tagged spelling",
                        span: Some(span),
                    });
                }
                (
                    FinalOpcode::PushAtomValue,
                    Operands::Atom(AtomPoolIndex::new(index)),
                )
            }
        };
        Ok(PlannedInstruction::new(instruction.0, instruction.1, span))
    }

    fn property_atom_index(&self, span: Span) -> Result<AtomPoolIndex, LeafCompilationError> {
        let position = self
            .property_atom_indices
            .binary_search_by_key(&(span.start, span.end), |(candidate, _)| {
                (candidate.start, candidate.end)
            })
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "static property has one function-local atom",
                span: Some(span),
            })?;
        Ok(AtomPoolIndex::new(self.property_atom_indices[position].1))
    }

    fn function_index(&self, executable: ExecutableId) -> Result<u32, LeafCompilationError> {
        self.function_indices
            .binary_search_by_key(&executable, |(candidate, _)| *candidate)
            .ok()
            .map(|position| self.function_indices[position].1)
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "direct child executable has a constant-pool index",
                span: None,
            })
    }
}

fn decode_compiler_string(
    value: &str,
    lone_surrogates: bool,
    span: Span,
) -> Result<CompilerString, LeafCompilationError> {
    let code_units = decode_oxc_cooked_string(value, lone_surrogates)
        .map_err(|source| LeafCompilationError::CookedStringDecoding { span, source })?;
    CompilerString::try_from_code_units(code_units)
        .map_err(|source| LeafCompilationError::CompilerString { span, source })
}

fn compiler_identifier_string(
    value: &str,
    span: Span,
) -> Result<CompilerString, LeafCompilationError> {
    CompilerString::try_from_code_units(value.encode_utf16().collect::<Vec<_>>().into())
        .map_err(|source| LeafCompilationError::CompilerString { span, source })
}

fn compiled_static_property_key(
    key: &OxcPropertyKey<'_>,
) -> Result<Option<CompiledStaticPropertyKey>, LeafCompilationError> {
    let (value, span) = match key {
        OxcPropertyKey::StaticIdentifier(identifier) => (
            compiler_identifier_string(identifier.name.as_str(), identifier.span)?,
            identifier.span,
        ),
        OxcPropertyKey::StringLiteral(literal) => (
            decode_compiler_string(
                literal.value.as_str(),
                literal.lone_surrogates,
                literal.span,
            )?,
            literal.span,
        ),
        OxcPropertyKey::NumericLiteral(literal) => {
            let value = Binary64Constant::from_f64(literal.value).to_javascript_string();
            (
                compiler_identifier_string(&value, literal.span)?,
                literal.span,
            )
        }
        OxcPropertyKey::BigIntLiteral(literal) => (
            compiler_identifier_string(literal.value.as_str(), literal.span)?,
            literal.span,
        ),
        _ => return Ok(None),
    };
    Ok(Some(CompiledStaticPropertyKey { value, span }))
}

fn is_noncomputed_static_property_key_node(unit: &ParsedUnit<'_, '_>, node_id: NodeId) -> bool {
    let AstKind::ObjectProperty(property) = unit.semantic().nodes().parent_kind(node_id) else {
        return false;
    };
    if property.computed {
        return false;
    }
    match &property.key {
        OxcPropertyKey::StringLiteral(literal) => literal.node_id.get() == node_id,
        OxcPropertyKey::NumericLiteral(literal) => literal.node_id.get() == node_id,
        OxcPropertyKey::BigIntLiteral(literal) => literal.node_id.get() == node_id,
        _ => false,
    }
}

fn record_string_candidate(
    owner: ExecutableId,
    value: CompilerString,
    span: Span,
    constants: &mut [Vec<CompiledConstantCandidate>],
    atoms: &mut [Vec<CompiledAtomCandidate>],
) -> Result<(), LeafCompilationError> {
    if value.is_empty() {
        return Ok(());
    }
    if value.is_tagged_integer_atom() {
        constants
            .get_mut(owner.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
            .push(CompiledConstantCandidate::String { value, span });
    } else {
        atoms
            .get_mut(owner.index())
            .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
            .push(CompiledAtomCandidate {
                value,
                span,
                purpose: CompiledAtomPurpose::RuntimeString,
            });
    }
    Ok(())
}

fn record_property_candidate(
    owner: ExecutableId,
    value: CompilerString,
    span: Span,
    atoms: &mut [Vec<CompiledAtomCandidate>],
) -> Result<(), LeafCompilationError> {
    atoms
        .get_mut(owner.index())
        .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
        .push(CompiledAtomCandidate {
            value,
            span,
            purpose: CompiledAtomPurpose::Property,
        });
    Ok(())
}

#[derive(Clone, Copy)]
struct StackAnchor {
    instruction_index: usize,
    span: Span,
    expected_depth: u32,
}

#[derive(Clone, Copy, Debug)]
struct ResolvedStackAnchor {
    pc: BytecodePc,
    span: Span,
    expected_depth: u32,
}

struct PlannedControlFlow {
    assembler: BytecodeAssembler,
    instruction_spans: Vec<Span>,
    label_spans: Vec<Span>,
    stack_anchors: Vec<StackAnchor>,
    last_instruction_can_fall_through: Option<bool>,
    label_bound_after_last_instruction: bool,
}

#[derive(Debug)]
struct FinishedControlFlow {
    bytecode: Vec<u8>,
    source_instructions: Vec<SourceInstruction>,
    stack_anchors: Vec<ResolvedStackAnchor>,
}

impl PlannedControlFlow {
    fn new(limits: VerificationLimits) -> Self {
        let assembler_limits = AssemblerLimits::new(
            limits.max_bytecode_bytes_per_function(),
            limits.max_instructions_per_function(),
            limits.max_transfer_evaluations(),
        );
        Self {
            assembler: BytecodeAssembler::with_limits(assembler_limits),
            instruction_spans: Vec::new(),
            label_spans: Vec::new(),
            stack_anchors: Vec::new(),
            last_instruction_can_fall_through: None,
            label_bound_after_last_instruction: false,
        }
    }

    fn emit(&mut self, instruction: PlannedInstruction) -> Result<(), LeafCompilationError> {
        self.assembler
            .push(instruction.opcode, instruction.operands)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(instruction.span),
                source,
            })?;
        self.instruction_spans.push(instruction.span);
        self.last_instruction_can_fall_through = Some(!matches!(
            instruction.opcode,
            FinalOpcode::Return | FinalOpcode::ReturnUndef | FinalOpcode::Throw
        ));
        self.label_bound_after_last_instruction = false;
        Ok(())
    }

    fn new_label(&mut self, span: Span) -> Result<CompilerLabel, LeafCompilationError> {
        self.new_label_with_expected_depth(span, None)
    }

    fn new_statement_label(&mut self, span: Span) -> Result<CompilerLabel, LeafCompilationError> {
        self.new_label_with_expected_depth(span, Some(0))
    }

    fn new_label_with_expected_depth(
        &mut self,
        span: Span,
        expected_stack_depth: Option<u32>,
    ) -> Result<CompilerLabel, LeafCompilationError> {
        let assembler = self.assembler.new_label().map_err(|source| {
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source,
            }
        })?;
        self.label_spans.push(span);
        Ok(CompilerLabel {
            assembler,
            owner_span: span,
            expected_stack_depth,
        })
    }

    fn branch(
        &mut self,
        kind: BranchKind,
        target: &CompilerLabel,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        self.assembler
            .branch(kind, &target.assembler)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source,
            })?;
        self.instruction_spans.push(span);
        self.last_instruction_can_fall_through = Some(kind != BranchKind::Goto);
        self.label_bound_after_last_instruction = false;
        Ok(())
    }

    fn bind(&mut self, label: &CompilerLabel) -> Result<(), LeafCompilationError> {
        self.assembler.bind(&label.assembler).map_err(|source| {
            LeafCompilationError::BytecodeAssembly {
                span: Some(label.owner_span),
                source,
            }
        })?;
        if let Some(expected_depth) = label.expected_stack_depth {
            self.stack_anchors.push(StackAnchor {
                instruction_index: self.instruction_spans.len(),
                span: label.owner_span,
                expected_depth,
            });
        }
        self.label_bound_after_last_instruction = true;
        Ok(())
    }

    fn ensure_terminal(&mut self, span: Span) -> Result<(), LeafCompilationError> {
        if self.label_bound_after_last_instruction
            || self.last_instruction_can_fall_through.unwrap_or(true)
        {
            self.emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                span,
            ))?;
        }
        Ok(())
    }

    fn ensure_script_terminal(
        &mut self,
        completion: LocalSlot,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if self.label_bound_after_last_instruction
            || self.last_instruction_can_fall_through.unwrap_or(true)
        {
            let (opcode, operands) = compact_get_local(completion);
            self.emit(PlannedInstruction::new(opcode, operands, span))?;
            self.emit(PlannedInstruction::new(
                FinalOpcode::Return,
                Operands::None,
                span,
            ))?;
        }
        Ok(())
    }

    fn finish(self) -> Result<FinishedControlFlow, LeafCompilationError> {
        let Self {
            assembler,
            instruction_spans: spans,
            label_spans,
            stack_anchors,
            last_instruction_can_fall_through: _,
            label_bound_after_last_instruction: _,
        } = self;
        let assembled = match assembler.finish() {
            Ok(assembled) => assembled,
            Err(AssemblerError::Encoding {
                instruction_index,
                source,
            }) => {
                let span = spans.get(instruction_index as usize).copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "assembler encoding failure indexes a planned source span",
                        span: None,
                    },
                )?;
                return Err(LeafCompilationError::BytecodeEncoding { span, source });
            }
            Err(source) => {
                let span = source
                    .instruction_index()
                    .and_then(|index| spans.get(index as usize).copied())
                    .or_else(|| {
                        source
                            .label_index()
                            .and_then(|index| label_spans.get(index as usize).copied())
                    });
                return Err(LeafCompilationError::BytecodeAssembly { span, source });
            }
        };
        let (bytecode, instruction_pcs) = assembled.into_parts();
        if instruction_pcs.len() != spans.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "assembler returns one final PC per planned instruction",
                span: spans.last().copied(),
            });
        }
        let mut resolved_stack_anchors = Vec::with_capacity(stack_anchors.len());
        for anchor in stack_anchors {
            let Some(pc) = instruction_pcs.get(anchor.instruction_index).copied() else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "statement stack anchor resolves to a final instruction",
                    span: Some(anchor.span),
                });
            };
            resolved_stack_anchors.push(ResolvedStackAnchor {
                pc,
                span: anchor.span,
                expected_depth: anchor.expected_depth,
            });
        }
        let source_instructions = instruction_pcs
            .into_iter()
            .zip(spans)
            .map(|(pc, span)| SourceInstruction { pc, span })
            .collect();
        Ok(FinishedControlFlow {
            bytecode,
            source_instructions,
            stack_anchors: resolved_stack_anchors,
        })
    }
}

impl FinishedControlFlow {
    #[cfg(test)]
    fn verify(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        self.verify_with_capture_layout(domains, header, CompilerCaptureLayout::default(), limits)
    }

    #[cfg(test)]
    fn verify_with_capture_layout(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        capture_layout: CompilerCaptureLayout,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        self.verify_with_layouts(
            domains,
            header,
            capture_layout,
            CompilerConstantLayout::default(),
            limits,
        )
    }

    fn verify_with_layouts(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        capture_layout: CompilerCaptureLayout,
        constant_layout: CompilerConstantLayout,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        let Self {
            bytecode,
            source_instructions,
            stack_anchors,
        } = self;
        let control_flow = match verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(bytecode, domains, header)
                .with_capture_layout(capture_layout)
                .with_constant_layout(constant_layout),
            limits,
        ) {
            Ok(control_flow) => control_flow,
            Err(source) => {
                let span = match source.pc() {
                    Some(pc) => Some(exact_source_span(&source_instructions, pc).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "verifier instruction PC resolves to an exact source instruction",
                            span: None,
                        },
                    )?),
                    None => None,
                };
                let related_span = match source.kind() {
                VerificationErrorKind::InconsistentStackAtJoin { target, .. } => {
                        Some(exact_source_span(&source_instructions, *target).ok_or(
                            LeafCompilationError::SemanticInvariant {
                                invariant:
                                    "verifier join target resolves to an exact source instruction",
                                span: None,
                            },
                        )?)
                }
                _ => None,
            };
                return Err(LeafCompilationError::BytecodeVerification {
                    span,
                    related_span,
                    source,
                });
            }
        };

        for anchor in stack_anchors {
            let Some(index) = control_flow.instruction_index_at(anchor.pc) else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "resolved statement stack anchor remains an instruction start",
                    span: Some(anchor.span),
                });
            };
            let Some(instruction) = control_flow.instruction(index) else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "resolved statement stack anchor has a verified instruction",
                    span: Some(anchor.span),
                });
            };
            let Some(actual) = instruction.entry_stack_depth() else {
                continue;
            };
            if actual != anchor.expected_depth {
                return Err(LeafCompilationError::BytecodeStackInvariant {
                    span: anchor.span,
                    pc: anchor.pc,
                    expected: anchor.expected_depth,
                    actual,
                });
            }
        }

        Ok((source_instructions, control_flow))
    }
}

fn exact_source_span(source_instructions: &[SourceInstruction], pc: BytecodePc) -> Option<Span> {
    source_instructions
        .binary_search_by_key(&pc, |instruction| instruction.pc())
        .ok()
        .map(|index| source_instructions[index].span())
}

fn compact_get_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::GetArg0, Operands::NoneArg),
        1 => (FinalOpcode::GetArg1, Operands::NoneArg),
        2 => (FinalOpcode::GetArg2, Operands::NoneArg),
        3 => (FinalOpcode::GetArg3, Operands::NoneArg),
        index => (FinalOpcode::GetArg, Operands::Arg(index)),
    }
}

fn compact_put_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::PutArg0, Operands::NoneArg),
        1 => (FinalOpcode::PutArg1, Operands::NoneArg),
        2 => (FinalOpcode::PutArg2, Operands::NoneArg),
        3 => (FinalOpcode::PutArg3, Operands::NoneArg),
        index => (FinalOpcode::PutArg, Operands::Arg(index)),
    }
}

fn compact_set_argument(slot: ArgumentSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::SetArg0, Operands::NoneArg),
        1 => (FinalOpcode::SetArg1, Operands::NoneArg),
        2 => (FinalOpcode::SetArg2, Operands::NoneArg),
        3 => (FinalOpcode::SetArg3, Operands::NoneArg),
        index => (FinalOpcode::SetArg, Operands::Arg(index)),
    }
}

fn compact_get_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::GetLoc0, Operands::NoneLoc),
        1 => (FinalOpcode::GetLoc1, Operands::NoneLoc),
        2 => (FinalOpcode::GetLoc2, Operands::NoneLoc),
        3 => (FinalOpcode::GetLoc3, Operands::NoneLoc),
        index => match u8::try_from(index) {
            Ok(short) => (FinalOpcode::GetLoc8, Operands::Loc8(short)),
            Err(_) => (FinalOpcode::GetLoc, Operands::Loc(index)),
        },
    }
}

fn compact_put_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::PutLoc0, Operands::NoneLoc),
        1 => (FinalOpcode::PutLoc1, Operands::NoneLoc),
        2 => (FinalOpcode::PutLoc2, Operands::NoneLoc),
        3 => (FinalOpcode::PutLoc3, Operands::NoneLoc),
        index => match u8::try_from(index) {
            Ok(short) => (FinalOpcode::PutLoc8, Operands::Loc8(short)),
            Err(_) => (FinalOpcode::PutLoc, Operands::Loc(index)),
        },
    }
}

fn compact_set_local(slot: LocalSlot) -> (FinalOpcode, Operands) {
    match slot.0 {
        0 => (FinalOpcode::SetLoc0, Operands::NoneLoc),
        1 => (FinalOpcode::SetLoc1, Operands::NoneLoc),
        2 => (FinalOpcode::SetLoc2, Operands::NoneLoc),
        3 => (FinalOpcode::SetLoc3, Operands::NoneLoc),
        index => match u8::try_from(index) {
            Ok(short) => (FinalOpcode::SetLoc8, Operands::Loc8(short)),
            Err(_) => (FinalOpcode::SetLoc, Operands::Loc(index)),
        },
    }
}

fn compact_get_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::GetVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::GetVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::GetVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::GetVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::GetVarRef, Operands::VarRef(index)),
    }
}

fn compact_put_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::PutVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::PutVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::PutVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::PutVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::PutVarRef, Operands::VarRef(index)),
    }
}

fn compact_set_capture(slot: u16) -> (FinalOpcode, Operands) {
    match slot {
        0 => (FinalOpcode::SetVarRef0, Operands::NoneVarRef),
        1 => (FinalOpcode::SetVarRef1, Operands::NoneVarRef),
        2 => (FinalOpcode::SetVarRef2, Operands::NoneVarRef),
        3 => (FinalOpcode::SetVarRef3, Operands::NoneVarRef),
        index => (FinalOpcode::SetVarRef, Operands::VarRef(index)),
    }
}

fn plan_put_slot(slot: FrameSlot, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match slot {
        FrameSlot::Argument(slot) => compact_put_argument(slot),
        FrameSlot::Local(slot) => compact_put_local(slot),
        FrameSlot::Capture(slot) => compact_put_capture(slot),
    };
    PlannedInstruction::new(opcode, operands, span)
}

fn anonymous_function_definition_span(mut expression: &Expression<'_>) -> Option<Span> {
    while let Expression::ParenthesizedExpression(parenthesized) = expression {
        expression = &parenthesized.expression;
    }
    match expression {
        Expression::FunctionExpression(function) if function.id.is_none() => Some(function.span),
        _ => None,
    }
}

fn plan_literal(
    expression: &Expression<'_>,
    constants: &CompiledConstantPool,
) -> Option<Result<PlannedInstruction, LeafCompilationError>> {
    let planned = match expression {
        Expression::BooleanLiteral(literal) => Ok(PlannedInstruction::new(
            if literal.value {
                FinalOpcode::PushTrue
            } else {
                FinalOpcode::PushFalse
            },
            Operands::None,
            literal.span,
        )),
        Expression::NullLiteral(literal) => Ok(PlannedInstruction::new(
            FinalOpcode::Null,
            Operands::None,
            literal.span,
        )),
        Expression::NumericLiteral(literal) => match exact_i32(literal.value) {
            Some(value) => Ok(plan_push_integer(value, literal.span)),
            None => constants.plan_number(literal.value, literal.span),
        },
        Expression::BigIntLiteral(literal) => literal
            .value
            .parse::<i32>()
            .map(|value| {
                PlannedInstruction::new(
                    FinalOpcode::PushBigIntI32,
                    Operands::I32(value),
                    literal.span,
                )
            })
            .map_err(|_| LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedLiteral,
                span: literal.span,
            }),
        Expression::StringLiteral(literal) if literal.value.is_empty() => Ok(
            PlannedInstruction::new(FinalOpcode::PushEmptyString, Operands::None, literal.span),
        ),
        Expression::StringLiteral(literal) => constants.plan_string(literal.span),
        Expression::RegExpLiteral(literal) => {
            unsupported(UnsupportedLeafFeature::UnsupportedLiteral, literal.span)
        }
        Expression::TemplateLiteral(template)
            if template.expressions.is_empty() && template.quasis.len() == 1 =>
        {
            let quasi = &template.quasis[0];
            if quasi.tail {
                match quasi.value.cooked.as_ref() {
                    None => Err(LeafCompilationError::SemanticInvariant {
                        invariant: "untagged no-substitution template has a cooked value",
                        span: Some(template.span),
                    }),
                    Some(cooked) if cooked.is_empty() => Ok(PlannedInstruction::new(
                        FinalOpcode::PushEmptyString,
                        Operands::None,
                        template.span,
                    )),
                    Some(_) => constants.plan_string(template.span),
                }
            } else {
                Err(LeafCompilationError::SemanticInvariant {
                    invariant: "no-substitution template has one tail quasi",
                    span: Some(template.span),
                })
            }
        }
        Expression::TemplateLiteral(template) => {
            unsupported(UnsupportedLeafFeature::UnsupportedLiteral, template.span)
        }
        _ => return None,
    };
    Some(planned)
}

#[allow(clippy::cast_possible_truncation)]
fn exact_i32(value: f64) -> Option<i32> {
    if value.is_finite()
        && value.fract() == 0.0
        && value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX)
    {
        Some(value as i32)
    } else {
        None
    }
}

fn exact_negated_i32(value: f64) -> Option<i32> {
    exact_i32(-value)
}

fn plan_push_integer(value: i32, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match value {
        -1 => (FinalOpcode::PushMinus1, Operands::NoneInt),
        0 => (FinalOpcode::Push0, Operands::NoneInt),
        1 => (FinalOpcode::Push1, Operands::NoneInt),
        2 => (FinalOpcode::Push2, Operands::NoneInt),
        3 => (FinalOpcode::Push3, Operands::NoneInt),
        4 => (FinalOpcode::Push4, Operands::NoneInt),
        5 => (FinalOpcode::Push5, Operands::NoneInt),
        6 => (FinalOpcode::Push6, Operands::NoneInt),
        7 => (FinalOpcode::Push7, Operands::NoneInt),
        value => match i8::try_from(value) {
            Ok(value) => (FinalOpcode::PushI8, Operands::I8(value)),
            Err(_) => match i16::try_from(value) {
                Ok(value) => (FinalOpcode::PushI16, Operands::I16(value)),
                Err(_) => (FinalOpcode::PushI32, Operands::I32(value)),
            },
        },
    };
    PlannedInstruction::new(opcode, operands, span)
}

fn plan_direct_call(argument_count: u16, span: Span) -> PlannedInstruction {
    let (opcode, operands) = match argument_count {
        0 => (FinalOpcode::Call0, Operands::NPopX),
        1 => (FinalOpcode::Call1, Operands::NPopX),
        2 => (FinalOpcode::Call2, Operands::NPopX),
        3 => (FinalOpcode::Call3, Operands::NPopX),
        argument_count => (FinalOpcode::Call, Operands::NPop { argument_count }),
    };
    PlannedInstruction::new(opcode, operands, span)
}

const fn unary_opcode(operator: UnaryOperator) -> Option<FinalOpcode> {
    match operator {
        UnaryOperator::UnaryPlus => Some(FinalOpcode::Plus),
        UnaryOperator::UnaryNegation => Some(FinalOpcode::Neg),
        UnaryOperator::LogicalNot => Some(FinalOpcode::Lnot),
        UnaryOperator::BitwiseNot => Some(FinalOpcode::Not),
        UnaryOperator::Typeof => Some(FinalOpcode::Typeof),
        UnaryOperator::Void | UnaryOperator::Delete => None,
    }
}

const fn binary_opcode(operator: BinaryOperator) -> FinalOpcode {
    match operator {
        BinaryOperator::Equality => FinalOpcode::Eq,
        BinaryOperator::Inequality => FinalOpcode::Neq,
        BinaryOperator::StrictEquality => FinalOpcode::StrictEq,
        BinaryOperator::StrictInequality => FinalOpcode::StrictNeq,
        BinaryOperator::LessThan => FinalOpcode::Lt,
        BinaryOperator::LessEqualThan => FinalOpcode::Lte,
        BinaryOperator::GreaterThan => FinalOpcode::Gt,
        BinaryOperator::GreaterEqualThan => FinalOpcode::Gte,
        BinaryOperator::Addition => FinalOpcode::Add,
        BinaryOperator::Subtraction => FinalOpcode::Sub,
        BinaryOperator::Multiplication => FinalOpcode::Mul,
        BinaryOperator::Division => FinalOpcode::Div,
        BinaryOperator::Remainder => FinalOpcode::Mod,
        BinaryOperator::Exponential => FinalOpcode::Pow,
        BinaryOperator::ShiftLeft => FinalOpcode::Shl,
        BinaryOperator::ShiftRight => FinalOpcode::Sar,
        BinaryOperator::ShiftRightZeroFill => FinalOpcode::Shr,
        BinaryOperator::BitwiseOR => FinalOpcode::Or,
        BinaryOperator::BitwiseXOR => FinalOpcode::Xor,
        BinaryOperator::BitwiseAnd => FinalOpcode::And,
        BinaryOperator::In => FinalOpcode::In,
        BinaryOperator::Instanceof => FinalOpcode::InstanceOf,
    }
}

fn unsupported<T>(feature: UnsupportedLeafFeature, span: Span) -> Result<T, LeafCompilationError> {
    Err(LeafCompilationError::Unsupported { feature, span })
}

fn verified_binding_policy(
    policy: DeclarationPolicy,
) -> Result<CompilerBindingPolicy, LeafCompilationError> {
    let kind = match policy.kind() {
        DeclarationKind::Parameter => VerifiedBindingKind::Parameter,
        DeclarationKind::Var => VerifiedBindingKind::Var,
        DeclarationKind::Let => VerifiedBindingKind::Let,
        DeclarationKind::Const => VerifiedBindingKind::Const,
        DeclarationKind::Function => VerifiedBindingKind::Function,
        DeclarationKind::FunctionName => VerifiedBindingKind::FunctionName,
        DeclarationKind::Catch => VerifiedBindingKind::Catch,
        DeclarationKind::Import
        | DeclarationKind::NamespaceImport
        | DeclarationKind::SyntheticDefault => {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary function metadata excludes module bindings",
                span: None,
            });
        }
    };
    let initialization = match policy.initialization() {
        InitializationPolicy::Argument => VerifiedInitializationPolicy::Argument,
        InitializationPolicy::UndefinedAtInstantiation => {
            VerifiedInitializationPolicy::UndefinedAtInstantiation
        }
        InitializationPolicy::AtDeclaration => VerifiedInitializationPolicy::AtDeclaration,
        InitializationPolicy::FunctionAtInstantiation => {
            VerifiedInitializationPolicy::FunctionAtInstantiation
        }
        InitializationPolicy::FunctionAtScopeEntry => {
            VerifiedInitializationPolicy::FunctionAtScopeEntry
        }
        InitializationPolicy::FunctionName => VerifiedInitializationPolicy::FunctionName,
        InitializationPolicy::Catch => VerifiedInitializationPolicy::Catch,
        InitializationPolicy::ModuleImport | InitializationPolicy::ModuleNamespace => {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary function metadata excludes module initialization",
                span: None,
            });
        }
    };
    let writes = match policy.writes() {
        WritePolicy::Mutable => VerifiedWritePolicy::Mutable,
        WritePolicy::Immutable => VerifiedWritePolicy::Immutable,
        WritePolicy::ImmutableInStrictCode => VerifiedWritePolicy::ImmutableInStrictCode,
        WritePolicy::Internal => {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary function metadata excludes internal module cells",
                span: None,
            });
        }
    };
    Ok(CompilerBindingPolicy::new(
        kind,
        initialization,
        writes,
        policy.has_temporal_dead_zone(),
    ))
}

fn verified_storage_policy(
    binding: &crate::storage::BindingStorage,
) -> Result<CompilerBindingPolicy, LeafCompilationError> {
    if matches!(binding.placement(), StoragePlacement::Argument { .. }) {
        return Ok(CompilerBindingPolicy::new(
            VerifiedBindingKind::Parameter,
            VerifiedInitializationPolicy::Argument,
            VerifiedWritePolicy::Mutable,
            false,
        ));
    }
    verified_binding_policy(binding.policy())
}

const fn constructor_realm_lookup_policy() -> CompilerBindingPolicy {
    CompilerBindingPolicy::new(
        VerifiedBindingKind::GlobalReference,
        VerifiedInitializationPolicy::ConstructorRealmLookup,
        VerifiedWritePolicy::Mutable,
        false,
    )
}

fn script_completion_variable_definition(
    constants: &CompiledConstantPool,
) -> Result<VariableDefinition, LeafCompilationError> {
    Ok(VariableDefinition::new(
        Some(constants.metadata_atom_index(CompiledMetadataAtomKey::ScriptCompletion)?),
        ScopeLink::End,
        CompilerBindingPolicy::new(
            VerifiedBindingKind::Var,
            VerifiedInitializationPolicy::UndefinedAtInstantiation,
            VerifiedWritePolicy::Mutable,
            false,
        ),
        false,
        None,
    ))
}

const fn binding_has_scope(policy: DeclarationPolicy) -> bool {
    matches!(
        policy.kind(),
        DeclarationKind::Let | DeclarationKind::Const | DeclarationKind::Catch
    ) || matches!(
        policy.initialization(),
        InitializationPolicy::FunctionAtScopeEntry
    )
}

const fn source_byte_span(span: Span) -> SourceByteSpan {
    SourceByteSpan::new(span.start, span.end)
}

/// Syntax or storage behavior outside the currently executable compiler slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedLeafFeature {
    /// The selected executable is not a synchronous ordinary function.
    NonOrdinaryFunction,
    /// A dynamic Function unit must compile only through its complete Script root.
    DynamicFunctionRequiresScriptRoot,
    /// An anonymous function needs exact inferred-name initialization.
    InferredFunctionName,
    /// The Oxc function form is neither a declaration nor function expression.
    UnsupportedFunctionForm,
    /// An object method or accessor is outside the admitted static,
    /// synchronous, identifier-or-literal-named profile.
    ObjectMethodOrAccessor,
    /// The selected function contains another executable body.
    NestedExecutable,
    /// Module-owned storage is outside this Script-only lowering slice.
    UnsupportedCompilationUnit,
    /// A statement requires unsupported control flow or scope entry behavior.
    UnsupportedBody,
    /// A declaration is not a simple `var`, `let`, or `const` binding.
    UnsupportedDeclaration,
    /// An expression requires method, optional, spread, or constructor calls,
    /// properties, non-identifier mutation, or another unsupported family.
    UnsupportedExpression,
    /// A literal requires a constant, atom, `BigInt`, or `RegExp` pool entry.
    UnsupportedLiteral,
    /// A binding cannot be represented by this frame layout.
    UnsupportedBinding,
    /// Program-level bindings require the constructor realm's global environment.
    GlobalEnvironment,
    /// A reference access or binding write policy is not supported.
    UnsupportedReference,
    /// An identifier remained unresolved after Oxc semantics.
    UnresolvedReference,
}

/// Failure to lower or verify one executable body or complete subtree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeafCompilationError {
    /// The executable selection was issued by another compilation context.
    ForeignExecutable {
        /// The foreign selection's plan-local identity.
        executable: ExecutableId,
    },
    /// A context-issued executable no longer resolves in its immutable plan.
    InvalidExecutable {
        /// The rejected plan-local identity.
        executable: ExecutableId,
    },
    /// The selected source requires behavior outside this lowering slice.
    Unsupported {
        /// The unsupported behavior.
        feature: UnsupportedLeafFeature,
        /// Exact source span requiring it.
        span: Span,
    },
    /// Retained Oxc semantics or compiler identities violated an invariant.
    SemanticInvariant {
        /// Stable invariant label.
        invariant: &'static str,
        /// Related source span, when available.
        span: Option<Span>,
    },
    /// A dense bytecode domain exceeded its encoded width.
    CapacityExceeded {
        /// Stable capacity-domain label.
        domain: &'static str,
    },
    /// Oxc's retained cooked-string transport encoding was malformed.
    CookedStringDecoding {
        /// Exact source span of the affected literal.
        span: Span,
        /// Exact decoder failure.
        source: OxcStringDecodeError,
    },
    /// A cooked string could not be frozen as an exact compiler value.
    CompilerString {
        /// Exact source span of the affected literal.
        span: Span,
        /// Exact string-construction failure.
        source: CompilerStringError,
    },
    /// A typed final instruction could not be encoded.
    BytecodeEncoding {
        /// Source span responsible for the instruction.
        span: Span,
        /// Exact encoder failure.
        source: EncodeError,
    },
    /// Symbolic labels or branch relaxation could not produce final bytecode.
    BytecodeAssembly {
        /// Related instruction or compiler-owned label span, when available.
        span: Option<Span>,
        /// Exact assembler failure.
        source: AssemblerError,
    },
    /// A reachable compiler-owned statement anchor had the wrong entry stack.
    BytecodeStackInvariant {
        /// Source span that owns the statement anchor.
        span: Span,
        /// Final relocated bytecode position of the anchor.
        pc: BytecodePc,
        /// Compiler-required operand-stack depth.
        expected: u32,
        /// Verified reachable operand-stack depth.
        actual: u32,
    },
    /// The emitted body failed staged control-flow verification.
    BytecodeVerification {
        /// Exact instruction span for the verifier PC, when the error has one.
        span: Option<Span>,
        /// Exact join-target span for a two-position verifier failure.
        related_span: Option<Span>,
        /// Exact verifier failure.
        source: VerificationError,
    },
    /// The complete compiler function graph failed cross-function checks.
    FunctionGraphVerification {
        /// Source function span for a graph-local failure, when available.
        span: Option<Span>,
        /// Exact aggregate or cross-function verifier failure.
        source: FunctionGraphVerificationError,
    },
    /// Complete runtime metadata failed final bytecode verification.
    BytecodeGraphVerification {
        /// Source function span for a graph-local failure, when available.
        span: Option<Span>,
        /// Exact final-verifier failure.
        source: BytecodeVerificationError,
    },
}

impl fmt::Display for LeafCompilationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignExecutable { executable } => {
                write!(formatter, "foreign executable {}", executable.index())
            }
            Self::InvalidExecutable { executable } => {
                write!(formatter, "invalid executable {}", executable.index())
            }
            Self::Unsupported { feature, span } => {
                write!(
                    formatter,
                    "unsupported leaf feature {feature:?} at {span:?}"
                )
            }
            Self::SemanticInvariant { invariant, span } => {
                write!(formatter, "compiler invariant `{invariant}` failed")?;
                if let Some(span) = span {
                    write!(formatter, " at {span:?}")?;
                }
                Ok(())
            }
            Self::CapacityExceeded { domain } => {
                write!(formatter, "compiler capacity exceeded for {domain}")
            }
            Self::CookedStringDecoding { span, source } => {
                write!(
                    formatter,
                    "cooked string decoding failed at {span:?}: {source}"
                )
            }
            Self::CompilerString { span, source } => {
                write!(formatter, "compiler string failed at {span:?}: {source}")
            }
            Self::BytecodeEncoding { span, source } => {
                write!(formatter, "bytecode encoding failed at {span:?}: {source}")
            }
            Self::BytecodeAssembly { span, source } => {
                write!(formatter, "bytecode assembly failed")?;
                if let Some(span) = span {
                    write!(formatter, " at {span:?}")?;
                }
                write!(formatter, ": {source}")
            }
            Self::BytecodeStackInvariant {
                span,
                pc,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "compiler stack invariant failed at {span:?} (PC {pc}): \
                     expected depth {expected}, got {actual}"
                )
            }
            Self::BytecodeVerification {
                span,
                related_span,
                source,
            } => {
                write!(formatter, "{source}")?;
                if let Some(span) = span {
                    write!(formatter, " at source {span:?}")?;
                }
                if let Some(related_span) = related_span {
                    write!(formatter, " (related source {related_span:?})")?;
                }
                Ok(())
            }
            Self::FunctionGraphVerification { span, source } => {
                source.fmt(formatter)?;
                if let Some(span) = span {
                    write!(formatter, " at source {span:?}")?;
                }
                Ok(())
            }
            Self::BytecodeGraphVerification { span, source } => {
                source.fmt(formatter)?;
                if let Some(span) = span {
                    write!(formatter, " at source {span:?}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for LeafCompilationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BytecodeEncoding { source, .. } => Some(source),
            Self::BytecodeAssembly { source, .. } => Some(source),
            Self::BytecodeVerification { source, .. } => Some(source),
            Self::FunctionGraphVerification { source, .. } => Some(source),
            Self::BytecodeGraphVerification { source, .. } => Some(source),
            Self::CookedStringDecoding { source, .. } => Some(source),
            Self::CompilerString { source, .. } => Some(source),
            Self::ForeignExecutable { .. }
            | Self::InvalidExecutable { .. }
            | Self::Unsupported { .. }
            | Self::SemanticInvariant { .. }
            | Self::CapacityExceeded { .. }
            | Self::BytecodeStackInvariant { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickjs_frontend::{
        CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
    };

    #[test]
    fn function_index_capacity_checks_count_and_index_boundaries() {
        assert_eq!(
            checked_function_entry_count(MAX_FUNCTION_INDEX_ENTRIES, "test count"),
            Ok(MAX_FUNCTION_INDEX_ENTRIES)
        );
        assert_eq!(
            checked_function_index(MAX_FUNCTION_INDEX_ENTRIES - 1, "test index"),
            Ok(u16::try_from(MAX_FUNCTION_INDEX_ENTRIES - 1).expect("u16 index"))
        );
        assert!(matches!(
            checked_function_entry_count(u64::from(MAX_FUNCTION_INDEX_ENTRIES) + 1, "test count"),
            Err(LeafCompilationError::CapacityExceeded {
                domain: "test count"
            })
        ));
        assert!(matches!(
            checked_function_index(MAX_FUNCTION_INDEX_ENTRIES, "test index"),
            Err(LeafCompilationError::CapacityExceeded {
                domain: "test index"
            })
        ));
    }

    #[test]
    fn constant_pool_ownership_includes_the_program_and_nearest_function() {
        with_parsed_program(
            "1.5; function child(){ return 2.5; }",
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let root = context
                    .executables()
                    .find(|candidate| candidate.metadata().parent().is_none())
                    .expect("program executable")
                    .id();
                let child = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("child"))
                    .expect("child executable")
                    .id();
                let layout = context
                    .function_tree_layout()
                    .expect("function tree layout");

                let root_pool = layout.constant_pool(root).expect("program constant pool");
                assert_eq!(
                    root_pool.entries.as_ref(),
                    [
                        CompiledConstant::Value(CompilerConstantValue::Number(
                            Binary64Constant::from_f64(1.5),
                        )),
                        CompiledConstant::Function(CompiledFunctionConstant { executable: child }),
                    ]
                );
                let child_pool = layout.constant_pool(child).expect("child constant pool");
                assert_eq!(
                    child_pool.entries.as_ref(),
                    [CompiledConstant::Value(CompilerConstantValue::Number(
                        Binary64Constant::from_f64(2.5),
                    ))]
                );
            },
        )
        .expect("front-end acceptance");
    }

    #[test]
    fn own_capture_layout_distinguishes_argument_function_and_scoped_cells() {
        let source = "function outer(arg){ var functionLocal=1; { let scoped=2; \
                      const capture=function(){ return arg+functionLocal+scoped; }; } }";
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let layout = FrameLayout::new(context.storage_plan(), executable.id())
                    .expect("frame layout");
                let tree_layout = context
                    .function_tree_layout()
                    .expect("function tree layout");
                let capture_layout = context
                    .compiler_capture_layout(
                        executable.id(),
                        function.scope_id.get().expect("function scope"),
                        &layout,
                        &tree_layout,
                    )
                    .expect("capture layout");

                assert_eq!(
                    capture_layout.bindings(),
                    [
                        CompilerCapturedBinding::Argument(0),
                        CompilerCapturedBinding::FunctionLocal(0),
                        CompilerCapturedBinding::ScopedLocal(1),
                    ]
                );
            },
        )
        .expect("front-end acceptance");
    }

    #[test]
    fn captured_block_exit_closes_exact_scope_locals_in_reverse_slot_order() {
        let source = "function outer(){ { let first=1; let second=2; \
                      const capture=function(){ return first+second; }; } }";
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::BlockStatement(block) = &body.statements[0] else {
                    panic!("captured block");
                };
                let block_scope = context
                    .created_scope(block.scope_id.get(), block.node_id.get(), block.span)
                    .expect("block scope");
                let function_scope = context
                    .created_scope(function.scope_id.get(), function.node_id.get(), function.span)
                    .expect("function scope");
                let layout =
                    FrameLayout::new(context.storage_plan(), executable.id()).expect("frame layout");
                let tree_layout = context.function_tree_layout().expect("function tree layout");
                let capture_layout = context
                    .compiler_capture_layout(
                        executable.id(),
                        function_scope,
                        &layout,
                        &tree_layout,
                    )
                    .expect("capture layout");
                assert_eq!(
                    capture_layout.bindings(),
                    [
                        CompilerCapturedBinding::ScopedLocal(0),
                        CompilerCapturedBinding::ScopedLocal(1),
                    ]
                );

                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                context
                    .plan_scope_exit(executable.id(), block_scope, &layout, &mut flow)
                    .expect("scope exit");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::ReturnUndef,
                    Operands::None,
                    block.span,
                ))
                .expect("terminal");
                let header =
                    UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                        false,
                        0,
                        2,
                    );
                let (_, verified) = flow
                    .finish()
                    .expect("assembly")
                    .verify_with_capture_layout(
                        FunctionIndexDomains::new(0, 0, 0, layout.local_count, 0),
                        header,
                        capture_layout,
                        VerificationLimits::default(),
                    )
                    .expect("verified close_loc");
                assert_eq!(
                    verified
                        .instructions()
                        .iter()
                        .map(|instruction| {
                            let instruction = instruction.decoded().instruction();
                            (instruction.opcode(), instruction.operands())
                        })
                        .collect::<Vec<_>>(),
                    [
                        (FinalOpcode::CloseLoc, Operands::Loc(1)),
                        (FinalOpcode::CloseLoc, Operands::Loc(0)),
                        (FinalOpcode::ReturnUndef, Operands::None),
                    ]
                );
            },
        )
        .expect("front-end acceptance");
    }

    fn abrupt_cleanup_fixture(
        source: &str,
    ) -> (
        Vec<SourceInstruction>,
        VerifiedControlFlow,
        Vec<CompilerCapturedBinding>,
        Span,
        Span,
    ) {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::WhileStatement(loop_statement) = &body.statements[0] else {
                    panic!("while statement");
                };
                let Statement::BlockStatement(loop_body) = &loop_statement.body else {
                    panic!("loop body");
                };
                let Statement::BlockStatement(inner) = &loop_body.body[2] else {
                    panic!("inner block");
                };
                let Statement::BreakStatement(break_statement) = &inner.body[2] else {
                    panic!("break statement");
                };
                let function_scope = function.scope_id.get().expect("function scope");
                let loop_scope = loop_body.scope_id.get().expect("loop body scope");
                let inner_scope = inner.scope_id.get().expect("inner scope");
                let layout =
                    FrameLayout::new(context.storage_plan(), executable.id()).expect("frame layout");
                let tree_layout = context.function_tree_layout().expect("function tree layout");
                let capture_layout = context
                    .compiler_capture_layout(
                        executable.id(),
                        function_scope,
                        &layout,
                        &tree_layout,
                    )
                    .expect("capture layout");
                let captured_bindings = capture_layout.bindings().to_vec();

                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                let done = flow
                    .new_statement_label(loop_statement.span)
                    .expect("done label");
                let state = StatementPlanningState {
                    work: Vec::new(),
                    active_scopes: vec![function_scope, loop_scope, inner_scope],
                    loop_controls: vec![LoopControl {
                        break_target: done.clone(),
                        continue_target: done.clone(),
                        scope_depth: 1,
                    }],
                    completion: StatementCompletion::Discard,
                };
                context
                    .plan_loop_jump(
                        None,
                        break_statement.span,
                        LoopJump::Break,
                        &state,
                        &layout,
                        &mut flow,
                    )
                    .expect("abrupt cleanup");
                flow.bind(&done).expect("done binding");
                emit_return_undefined(&mut flow, loop_statement.span, "terminal");
                let header =
                    UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                        false,
                        0,
                        2,
                    );
                let (source_instructions, verified) = flow
                    .finish()
                    .expect("assembly")
                    .verify_with_capture_layout(
                        FunctionIndexDomains::new(0, 0, 0, layout.local_count, 0),
                        header,
                        capture_layout,
                        VerificationLimits::default(),
                    )
                    .expect("verified abrupt cleanup");
                let inner_binding_span = unit.semantic().scoping().symbol_span(
                    unit.semantic()
                        .scoping()
                        .iter_bindings_in(inner_scope)
                        .next()
                        .expect("inner binding"),
                );
                (
                    source_instructions,
                    verified,
                    captured_bindings,
                    inner_binding_span,
                    break_statement.span,
                )
            },
        )
        .expect("front-end acceptance")
    }

    #[test]
    fn abrupt_loop_exit_closes_captured_scope_suffix_from_inner_to_outer() {
        let source = "function outer(){ while(true){ let outerValue=1; \
                      const outerCapture=function(){return outerValue;}; \
                      { let innerValue=2; \
                      const innerCapture=function(){return innerValue;}; break; } } }";
        let (source_instructions, verified, captured, inner_span, break_span) =
            abrupt_cleanup_fixture(source);

        assert_eq!(
            captured,
            [
                CompilerCapturedBinding::ScopedLocal(0),
                CompilerCapturedBinding::ScopedLocal(2),
            ]
        );
        assert_eq!(
            verified
                .instructions()
                .iter()
                .map(|instruction| {
                    let instruction = instruction.decoded().instruction();
                    (instruction.opcode(), instruction.operands())
                })
                .collect::<Vec<_>>(),
            [
                (FinalOpcode::CloseLoc, Operands::Loc(2)),
                (FinalOpcode::CloseLoc, Operands::Loc(0)),
                (FinalOpcode::Goto8, Operands::Label8(1)),
                (FinalOpcode::ReturnUndef, Operands::None),
            ]
        );
        assert_eq!(
            exact_source_span(&source_instructions, BytecodePc::new(0)),
            Some(inner_span)
        );
        assert_eq!(
            exact_source_span(&source_instructions, BytecodePc::new(6)),
            Some(break_span)
        );
    }

    #[test]
    fn classic_for_schedule_places_rotation_before_test_update_and_final_exit() {
        let source = "function f(){ for(let i=0;i<2;i++){} }";
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::ForStatement(statement) = &body.statements[0] else {
                    panic!("classic for");
                };
                let scope = statement.scope_id.get().expect("for scope");
                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                let mut work = Vec::new();
                CompilationContext::schedule_for_statement(
                    statement, scope, &mut flow, &mut work, 1,
                )
                .expect("iterative for schedule");
                let execution = work.iter().rev().collect::<Vec<_>>();
                let close_positions = execution
                    .iter()
                    .enumerate()
                    .filter_map(|(position, task)| {
                        matches!(task, StatementWork::CloseScope(found) if *found == scope)
                            .then_some(position)
                    })
                    .collect::<Vec<_>>();
                let test_position = execution
                    .iter()
                    .position(|task| {
                        matches!(
                            task,
                            StatementWork::Bind(label)
                                if label.owner_span == statement.test.as_ref().expect("test").span()
                        )
                    })
                    .expect("test label");
                let rotate_position = execution
                    .iter()
                    .position(|task| {
                        matches!(
                            task,
                            StatementWork::Bind(label)
                                if label.owner_span
                                    == statement.update.as_ref().expect("update").span()
                        )
                    })
                    .expect("rotation label");
                let update_position = execution
                    .iter()
                    .position(|task| {
                        matches!(
                            task,
                            StatementWork::Expression(expression)
                                if expression.span()
                                    == statement.update.as_ref().expect("update").span()
                        )
                    })
                    .expect("update expression");
                let control = execution
                    .iter()
                    .find_map(|task| match task {
                        StatementWork::PushLoop(control) => Some(control),
                        _ => None,
                    })
                    .expect("loop control");

                assert_eq!(close_positions.len(), 2);
                assert!(close_positions[0] < test_position);
                assert!(rotate_position < close_positions[1]);
                assert!(close_positions[1] < update_position);
                assert_eq!(
                    control.continue_target.owner_span,
                    statement.update.as_ref().expect("update").span()
                );
                assert_eq!(control.scope_depth, 2);
                assert_eq!(
                    execution
                        .iter()
                        .filter(|task| {
                            matches!(task, StatementWork::PopScope(found) if *found == scope)
                        })
                        .count(),
                    1
                );
            },
        )
        .expect("front-end acceptance");
    }

    fn scheduled_statement_label(work: &[StatementWork<'_, '_>], span: Span) -> CompilerLabel {
        work.iter()
            .find_map(|task| match task {
                StatementWork::Bind(label) if label.owner_span == span => Some(label.clone()),
                _ => None,
            })
            .expect("scheduled label")
    }

    fn verify_capture_fixture(
        flow: PlannedControlFlow,
        local_count: u32,
        capture_layout: CompilerCaptureLayout,
        variable_reference_count: u32,
    ) -> (Vec<SourceInstruction>, VerifiedControlFlow) {
        let header =
            UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                false,
                0,
                variable_reference_count,
            );
        flow.finish()
            .expect("assembly")
            .verify_with_capture_layout(
                FunctionIndexDomains::new(0, 0, 0, local_count, 0),
                header,
                capture_layout,
                VerificationLimits::default(),
            )
            .expect("verified capture fixture")
    }

    fn emit_return_undefined(flow: &mut PlannedControlFlow, span: Span, expectation: &str) {
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            span,
        ))
        .expect(expectation);
    }

    const CAPTURED_FOR_CONTINUE_SOURCE: &str = "function outer(){ for(let i=0;i<2;i++){ \
        const capture=function(){return i;}; continue; } }";

    fn captured_for_continue_fixture() -> (Vec<SourceInstruction>, VerifiedControlFlow) {
        with_parsed_program(
            CAPTURED_FOR_CONTINUE_SOURCE,
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let executable = context
                    .executables()
                    .find(|candidate| candidate.metadata().name() == Some("outer"))
                    .expect("outer executable");
                let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                    panic!("function declaration");
                };
                let body = function.body.as_ref().expect("function body");
                let Statement::ForStatement(statement) = &body.statements[0] else {
                    panic!("classic for");
                };
                let Statement::BlockStatement(loop_body) = &statement.body else {
                    panic!("loop body");
                };
                let Statement::ContinueStatement(continue_statement) = &loop_body.body[1] else {
                    panic!("continue statement");
                };
                let scope = statement.scope_id.get().expect("for scope");
                let layout = FrameLayout::new(context.storage_plan(), executable.id())
                    .expect("frame layout");
                let tree_layout = context
                    .function_tree_layout()
                    .expect("function tree layout");
                let capture_layout = context
                    .compiler_capture_layout(
                        executable.id(),
                        function.scope_id.get().expect("function scope"),
                        &layout,
                        &tree_layout,
                    )
                    .expect("capture layout");

                let mut flow = PlannedControlFlow::new(VerificationLimits::default());
                let mut work = Vec::new();
                CompilationContext::schedule_for_statement(
                    statement, scope, &mut flow, &mut work, 1,
                )
                .expect("for schedule");
                let control = work
                    .iter()
                    .find_map(|task| match task {
                        StatementWork::PushLoop(control) => Some(control.clone()),
                        _ => None,
                    })
                    .expect("loop control");
                let test =
                    scheduled_statement_label(&work, statement.test.as_ref().expect("test").span());
                let done = scheduled_statement_label(&work, statement.span);

                flow.branch(
                    BranchKind::Goto,
                    &control.continue_target,
                    continue_statement.span,
                )
                .expect("continue branch");
                flow.bind(&test).expect("test label");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Nop,
                    Operands::None,
                    statement.test.as_ref().expect("test").span(),
                ))
                .expect("unreachable test");
                flow.branch(
                    BranchKind::Goto,
                    &control.continue_target,
                    statement.test.as_ref().expect("test").span(),
                )
                .expect("test-to-rotation branch");
                flow.bind(&control.continue_target).expect("rotation label");
                context
                    .plan_scope_exit(executable.id(), scope, &layout, &mut flow)
                    .expect("loop-head rotation");
                let update = statement.update.as_ref().expect("update");
                let constants = tree_layout
                    .constant_pool(executable.id())
                    .expect("constant pool");
                context
                    .plan_expression(update, &layout, &tree_layout, constants, &mut flow)
                    .expect("update");
                flow.emit(PlannedInstruction::new(
                    FinalOpcode::Drop,
                    Operands::None,
                    update.span(),
                ))
                .expect("drop update");
                emit_return_undefined(&mut flow, statement.span, "rotation terminal");
                flow.bind(&done).expect("done label");
                emit_return_undefined(&mut flow, statement.span, "done terminal");
                verify_capture_fixture(flow, layout.local_count, capture_layout, 1)
            },
        )
        .expect("front-end acceptance")
    }

    #[test]
    fn captured_classic_for_continue_targets_close_loc_before_update() {
        let source = CAPTURED_FOR_CONTINUE_SOURCE;
        let (source_instructions, verified) = captured_for_continue_fixture();
        let continue_instruction = verified
            .instructions()
            .iter()
            .find(|instruction| {
                let pc = instruction.decoded().pc();
                exact_source_span(&source_instructions, pc).is_some_and(|span| {
                    &source[span.start as usize..span.end as usize] == "continue;"
                })
            })
            .expect("continue instruction");
        let target = continue_instruction
            .successors()
            .jump_target()
            .and_then(|target| verified.instruction(target))
            .expect("continue target");
        assert_eq!(
            target.decoded().instruction().opcode(),
            FinalOpcode::CloseLoc
        );
        let target_position = verified
            .instructions()
            .iter()
            .position(|instruction| instruction.decoded().pc() == target.decoded().pc())
            .expect("target position");
        let update = &verified.instructions()[target_position + 1];
        assert_eq!(
            update.decoded().instruction().opcode(),
            FinalOpcode::GetLocCheck
        );
        let update_span =
            exact_source_span(&source_instructions, update.decoded().pc()).expect("update span");
        assert_eq!(
            &source[update_span.start as usize..update_span.end as usize],
            "i"
        );
    }

    #[test]
    fn compiler_labels_retain_owner_spans_for_bind_and_finish_failures() {
        let owner = Span::new(10, 20);

        let mut duplicate = PlannedControlFlow::new(VerificationLimits::default());
        let label = duplicate.new_label(owner).expect("label");
        duplicate.bind(&label).expect("first bind");
        let error = duplicate.bind(&label).expect_err("duplicate bind");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source: AssemblerError::DuplicateLabel { .. },
            } if span == owner
        ));

        let mut unbound = PlannedControlFlow::new(VerificationLimits::default());
        let _unbound_label = unbound.new_label(owner).expect("unbound label");
        let distractor_owner = Span::new(1, 2);
        let distractor = unbound
            .new_label(distractor_owner)
            .expect("distractor label");
        unbound.bind(&distractor).expect("distractor binding");
        unbound
            .emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                Span::new(30, 31),
            ))
            .expect("terminal");
        let error = unbound.finish().expect_err("unbound label");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source: AssemblerError::UnboundLabel { .. },
            } if span == owner
        ));

        let mut end_target = PlannedControlFlow::new(VerificationLimits::default());
        let label = end_target.new_label(owner).expect("label");
        let distractor_owner = Span::new(2, 3);
        let distractor = end_target
            .new_label(distractor_owner)
            .expect("distractor label");
        end_target.bind(&distractor).expect("distractor binding");
        end_target
            .branch(BranchKind::Goto, &label, Span::new(40, 41))
            .expect("branch");
        end_target.bind(&label).expect("bind at end");
        let error = end_target.finish().expect_err("end target");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source: AssemblerError::TargetAtEnd { .. },
            } if span == owner
        ));
    }

    #[test]
    fn reachable_statement_anchors_require_an_empty_stack() {
        let anchor_span = Span::new(20, 30);
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(10, 11),
        ))
        .expect("push");
        let anchor = flow
            .new_statement_label(anchor_span)
            .expect("statement label");
        flow.branch(BranchKind::Goto, &anchor, anchor_span)
            .expect("widened branch");
        for _ in 0..130 {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                Span::new(12, 13),
            ))
            .expect("padding");
        }
        flow.bind(&anchor).expect("bind");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Return,
            Operands::None,
            Span::new(30, 31),
        ))
        .expect("return");

        let finished = flow.finish().expect("assembly");
        let error = finished
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("depth-one statement anchor");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeStackInvariant {
                span,
                pc,
                expected: 0,
                actual: 1,
            } if span == anchor_span && pc == BytecodePc::new(134)
        ));
    }

    #[test]
    fn unreachable_statement_anchors_have_no_required_entry_depth() {
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let live_exit = flow.new_label(Span::new(40, 41)).expect("live exit");
        flow.branch(BranchKind::Goto, &live_exit, Span::new(0, 1))
            .expect("skip unreachable region");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(10, 11),
        ))
        .expect("unreachable push");
        let anchor = flow
            .new_statement_label(Span::new(20, 21))
            .expect("unreachable statement anchor");
        flow.bind(&anchor).expect("unreachable anchor binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(21, 22),
        ))
        .expect("unreachable terminal");
        flow.bind(&live_exit).expect("live exit binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(40, 41),
        ))
        .expect("live terminal");

        let (source_instructions, control_flow) = flow
            .finish()
            .expect("assembly")
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect("unreachable anchor is accepted");
        let anchor_source = source_instructions
            .iter()
            .find(|instruction| instruction.span() == Span::new(21, 22))
            .expect("unreachable target source");
        let anchor_index = control_flow
            .instruction_index_at(anchor_source.pc())
            .expect("verified unreachable target");
        assert_eq!(
            control_flow
                .instruction(anchor_index)
                .expect("unreachable target instruction")
                .entry_stack_depth(),
            None
        );
    }

    #[test]
    fn inconsistent_join_maps_incoming_and_target_source_spans() {
        let incoming_span = Span::new(20, 21);
        let target_span = Span::new(30, 31);
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let join = flow.new_label(Span::new(10, 11)).expect("join");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(0, 1),
        ))
        .expect("condition");
        flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
            .expect("conditional branch");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(2, 3),
        ))
        .expect("unbalanced value");
        flow.branch(BranchKind::Goto, &join, incoming_span)
            .expect("incoming edge");
        flow.bind(&join).expect("join binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            target_span,
        ))
        .expect("join target");

        let error = flow
            .finish()
            .expect("assembly")
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("inconsistent stack join");

        assert!(matches!(
            error,
            LeafCompilationError::BytecodeVerification {
                span: Some(span),
                related_span: Some(related_span),
                source,
            } if span == incoming_span
                && related_span == target_span
                && matches!(
                    source.kind(),
                    VerificationErrorKind::InconsistentStackAtJoin { .. }
                )
        ));
    }

    #[test]
    fn missing_primary_verifier_source_mapping_fails_as_a_compiler_invariant() {
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(0, 1),
        ))
        .expect("first push");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(1, 2),
        ))
        .expect("second push");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Return,
            Operands::None,
            Span::new(2, 3),
        ))
        .expect("return");
        let mut finished = flow.finish().expect("assembly");
        finished
            .source_instructions
            .retain(|instruction| instruction.pc() != BytecodePc::new(1));

        let error = finished
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::new(100, 10, 0, 0, 100, 1),
            )
            .expect_err("missing exact primary provenance");
        assert!(matches!(
            error,
            LeafCompilationError::SemanticInvariant {
                invariant: "verifier instruction PC resolves to an exact source instruction",
                span: None,
            }
        ));
    }

    #[test]
    fn missing_join_target_source_mapping_fails_as_a_compiler_invariant() {
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let join = flow.new_label(Span::new(10, 11)).expect("join");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(0, 1),
        ))
        .expect("condition");
        flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
            .expect("conditional branch");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Push1,
            Operands::NoneInt,
            Span::new(2, 3),
        ))
        .expect("unbalanced value");
        flow.branch(BranchKind::Goto, &join, Span::new(3, 4))
            .expect("incoming edge");
        flow.bind(&join).expect("join binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(4, 5),
        ))
        .expect("join target");
        let mut finished = flow.finish().expect("assembly");
        finished
            .source_instructions
            .retain(|instruction| instruction.span() != Span::new(4, 5));

        let error = finished
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("missing exact related provenance");
        assert!(matches!(
            error,
            LeafCompilationError::SemanticInvariant {
                invariant: "verifier join target resolves to an exact source instruction",
                span: None,
            }
        ));
    }

    #[test]
    fn widened_branch_verifier_failures_use_the_relocated_target_span() {
        let target_span = Span::new(30, 31);
        let mut flow = PlannedControlFlow::new(VerificationLimits::default());
        let target = flow.new_label(Span::new(20, 21)).expect("target");
        flow.branch(BranchKind::Goto, &target, Span::new(0, 1))
            .expect("widened branch");
        for _ in 0..130 {
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                Span::new(10, 11),
            ))
            .expect("padding");
        }
        flow.bind(&target).expect("target binding");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Drop,
            Operands::None,
            target_span,
        ))
        .expect("underflowing target");
        flow.emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(31, 32),
        ))
        .expect("terminal");

        let error = flow
            .finish()
            .expect("assembly")
            .verify(
                FunctionIndexDomains::new(0, 0, 0, 0, 0),
                UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
                VerificationLimits::default(),
            )
            .expect_err("reachable drop underflows");
        assert!(matches!(
            error,
            LeafCompilationError::BytecodeVerification {
                span: Some(span),
                related_span: None,
                source,
            } if span == target_span
                && source.pc() == Some(BytecodePc::new(133))
                && matches!(
                    source.kind(),
                    VerificationErrorKind::StackUnderflow {
                        required: 1,
                        available: 0,
                    }
                )
        ));
    }
}
