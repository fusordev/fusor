use std::sync::Arc;

use quickjs_bytecode::{
    BytecodeGraphVerificationLimits, FunctionGraphVerificationLimits, VerificationLimits,
};
use quickjs_frontend::ParsedUnit;

use crate::storage::{
    CompilerError, Executable, ExecutableId, PlannedStorage, StoragePlan, build_planned_storage,
};

use super::{
    CompiledFunctionTree, CompiledLeafFunction, LeafCompilationError, UnsupportedLeafFeature,
    unsupported,
};

#[derive(Debug)]
pub(super) struct ContextIdentity;

/// An owned executable selection issued by one [`CompilationContext`].
///
/// Its private context identity prevents a numerically equal
/// [`ExecutableId`] from another storage plan from selecting the wrong body.
#[derive(Clone, Debug)]
pub struct CompilationExecutable {
    pub(super) context_identity: Arc<ContextIdentity>,
    pub(super) executable: Executable,
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
    pub(super) unit: &'unit ParsedUnit<'arena, 'scope>,
    pub(super) planned: PlannedStorage,
    pub(super) source_text: Arc<str>,
    pub(super) source_name: Arc<str>,
    pub(super) identity: Arc<ContextIdentity>,
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
    /// `if`/`else`, `while`, `do`/`while`, classic `for`, `for-in`, ordinary
    /// synchronous `for-of`, `switch`, labeled and unlabeled `break`/`continue`,
    /// exact-span no-op `debugger` statements, expression statements, and
    /// explicit or implicit returns. A leaf may own ordinary value constants
    /// and may read or write frame cells captured from an ancestor. The entire
    /// function is converted to typed symbolic instructions before branch
    /// relaxation emits any bytes.
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

    /// Lowers the complete exact wrapper Script for a supported synchronous
    /// dynamic-function constructor invocation.
    ///
    /// The Program root and every nested function template are compiled and
    /// final-verified as one indivisible authority. No API on this context can
    /// extract the synthetic named wrapper function from a dynamic source unit.
    ///
    /// # Errors
    ///
    /// Rejects every non-dynamic source unit, asynchronous dynamic-function
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

    /// Lowers a complete synchronous dynamic-function Script with every staged
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
        if !crate::is_synchronous_dynamic_function_goal(self.unit.goal()) {
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
}
