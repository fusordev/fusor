use std::sync::Arc;

use oxc_ast::AstKind;
use fusor_bytecode::{
    BytecodeGraphVerificationLimits, FunctionGraphVerificationLimits, VerificationLimits,
};
use fusor_frontend::{ParsedUnit, Span};

use crate::storage::{
    CompilerError, Executable, ExecutableId, PlannedStorage, StoragePlan, build_planned_storage,
};

use super::{
    CompiledFunctionTree, CompiledLeafFunction, LeafCompilationError, UnsupportedLeafFeature,
    unsupported,
};

#[derive(Debug)]
pub(super) struct ContextIdentity;

/// One lossless replacement applied before a UTF-16 runtime source is passed
/// to Oxc's UTF-8 parser boundary.
///
/// `transformed` addresses the parser-facing UTF-8 source. `original` retains
/// the exact ECMAScript UTF-16 code units that compiler-owned literal lowering
/// must restore. The compiler currently admits substitutions only inside a
/// `RegExp` literal body, where it can preserve both matcher and `.source`
/// semantics without guessing about another token kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceTextSubstitution {
    transformed: Span,
    original: Arc<[u16]>,
}

impl SourceTextSubstitution {
    /// Creates one parser-source substitution.
    #[must_use]
    pub const fn new(transformed: Span, original: Arc<[u16]>) -> Self {
        Self {
            transformed,
            original,
        }
    }

    /// Returns the parser-facing byte span replaced by this record.
    #[must_use]
    pub const fn transformed(&self) -> Span {
        self.transformed
    }

    /// Returns the exact original UTF-16 code units.
    #[must_use]
    pub fn original(&self) -> &[u16] {
        &self.original
    }
}

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
    pub(super) source_substitutions: Arc<[SourceTextSubstitution]>,
    strict_class_ranges: Arc<[Span]>,
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
        Self::new_with_source_name_and_substitutions(unit, source_name, Arc::from([]))
    }

    /// Builds compiler state with exact UTF-16 substitutions for a transformed
    /// runtime source.
    ///
    /// # Errors
    ///
    /// Returns the storage planner's typed failure, rejects an empty source
    /// identity, or rejects malformed/overlapping substitutions and any
    /// substitution outside a `RegExp` literal body.
    pub fn new_with_source_name_and_substitutions(
        unit: &'unit ParsedUnit<'arena, 'scope>,
        source_name: Arc<str>,
        source_substitutions: Arc<[SourceTextSubstitution]>,
    ) -> Result<Self, CompilerError> {
        if source_name.is_empty() {
            return Err(CompilerError::SemanticInvariant {
                invariant: "nonempty compiler source display name",
                span: None,
            });
        }
        let planned = build_planned_storage(unit)?;
        let source_text = Arc::from(unit.program().source_text);
        validate_source_substitutions(unit, &source_text, &source_substitutions)?;
        let strict_class_ranges = collect_strict_class_ranges(unit);
        Ok(Self {
            unit,
            planned,
            source_text,
            source_substitutions,
            strict_class_ranges,
            source_name,
            identity: Arc::new(ContextIdentity),
        })
    }

    /// Returns whether a lowered instruction span is wholly contained by
    /// class syntax, whose inline evaluation is strict even in a sloppy
    /// surrounding executable.
    pub(super) fn span_has_class_strict_context(&self, span: Span) -> bool {
        let insertion = self
            .strict_class_ranges
            .partition_point(|range| range.start <= span.start);
        insertion.checked_sub(1).is_some_and(|index| {
            let range = self.strict_class_ranges[index];
            span.start < range.end && span.end <= range.end
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

    /// Lowers one complete host-loaded Global Script and every nested
    /// function template as an indivisible verified authority.
    ///
    /// # Errors
    ///
    /// Rejects non-Global-Script goals, asynchronous Script roots,
    /// unsupported syntax, resource limits, and staged or final verification
    /// failures.
    pub fn compile_global_script(
        &self,
        limits: VerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.compile_global_script_with_all_limits(
            limits,
            FunctionGraphVerificationLimits::default(),
            BytecodeGraphVerificationLimits::default(),
        )
    }

    /// Lowers a complete Global Script with every staged and final graph
    /// limit explicit.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::compile_global_script`].
    pub fn compile_global_script_with_all_limits(
        &self,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        if !crate::is_supported_global_script_goal(self.unit.goal()) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                self.unit.program().span,
            );
        }
        let root = self
            .planned
            .plan
            .executables()
            .first()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Global Script storage plan has a Program root",
                span: Some(self.unit.program().span),
            })?
            .id();
        self.compile_subtree_with_all_limits(root, limits, graph_limits, bytecode_limits)
    }

    /// Lowers one complete indirect-eval Script and every nested function
    /// template as an indivisible verified authority.
    ///
    /// Program lexical declarations are eval-local. In sloppy eval, `var` and
    /// function declarations target the Realm global environment; in strict
    /// eval they are eval-local as well.
    ///
    /// # Errors
    ///
    /// Rejects non-indirect-eval goals, unsupported syntax, resource limits,
    /// and staged or final verification failures.
    pub fn compile_indirect_eval_script(
        &self,
        limits: VerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.compile_indirect_eval_script_with_all_limits(
            limits,
            FunctionGraphVerificationLimits::default(),
            BytecodeGraphVerificationLimits::default(),
        )
    }

    /// Lowers a complete indirect-eval Script with every staged and final
    /// graph limit explicit.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::compile_indirect_eval_script`].
    pub fn compile_indirect_eval_script_with_all_limits(
        &self,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        if !crate::is_supported_indirect_eval_goal(self.unit.goal()) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                self.unit.program().span,
            );
        }
        let root = self
            .planned
            .plan
            .executables()
            .first()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "indirect eval storage plan has a Program root",
                span: Some(self.unit.program().span),
            })?
            .id();
        self.compile_subtree_with_all_limits(root, limits, graph_limits, bytecode_limits)
    }

    /// Lowers a closed direct-eval Script and every nested function template
    /// as an indivisible verified authority.
    ///
    /// Caller strictness and source strict directives are honored. Eval-local
    /// lexical declarations and strict `var` declarations are supported;
    /// caller/global name resolution and sloppy variable-environment mutation
    /// remain fail closed until an external environment is supplied.
    ///
    /// # Errors
    ///
    /// Rejects non-direct-eval goals, caller/global references, sloppy `var`
    /// declarations, unsupported syntax, resource limits, and verification
    /// failures.
    pub fn compile_direct_eval_script(
        &self,
        limits: VerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.compile_direct_eval_script_with_all_limits(
            limits,
            FunctionGraphVerificationLimits::default(),
            BytecodeGraphVerificationLimits::default(),
        )
    }

    /// Lowers a closed direct-eval Script with every staged and final graph
    /// limit explicit.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::compile_direct_eval_script`].
    pub fn compile_direct_eval_script_with_all_limits(
        &self,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        if !crate::is_supported_direct_eval_goal(self.unit.goal()) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                self.unit.program().span,
            );
        }
        let root = self
            .planned
            .plan
            .executables()
            .first()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "direct eval storage plan has a Program root",
                span: Some(self.unit.program().span),
            })?
            .id();
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
        if !crate::is_supported_dynamic_function_goal(self.unit.goal()) {
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

    /// Lowers one complete ECMAScript Module and every nested function
    /// template as an indivisible verified authority.
    ///
    /// The module root owns a module environment of cells materialized by the
    /// runtime linker; imported bindings alias exporter cells and module-local
    /// declarations lower to captured root-frame cells. Top-level `await` and
    /// `import.meta` are rejected with dedicated errors in this step.
    ///
    /// # Errors
    ///
    /// Rejects non-Module goals, top-level `await`, unsupported syntax,
    /// resource limits, and staged or final verification failures.
    pub fn compile_module(
        &self,
        limits: VerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        self.compile_module_with_all_limits(
            limits,
            FunctionGraphVerificationLimits::default(),
            BytecodeGraphVerificationLimits::default(),
        )
    }

    /// Lowers a complete Module with every staged and final graph limit
    /// explicit.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::compile_module`].
    pub fn compile_module_with_all_limits(
        &self,
        limits: VerificationLimits,
        graph_limits: FunctionGraphVerificationLimits,
        bytecode_limits: BytecodeGraphVerificationLimits,
    ) -> Result<CompiledFunctionTree, LeafCompilationError> {
        if !crate::is_supported_module_goal(self.unit.goal()) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                self.unit.program().span,
            );
        }
        let root = self
            .planned
            .plan
            .executables()
            .first()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Module storage plan has a Program root",
                span: Some(self.unit.program().span),
            })?
            .id();
        self.compile_subtree_with_all_limits(root, limits, graph_limits, bytecode_limits)
    }
}

fn collect_strict_class_ranges(unit: &ParsedUnit<'_, '_>) -> Arc<[Span]> {
    let mut ranges = unit
        .semantic()
        .nodes()
        .iter()
        .filter_map(|node| match node.kind() {
            AstKind::Class(class) => Some(class.span),
            _ => None,
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|range| (range.start, range.end));

    let mut merged: Vec<Span> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged.into()
}

fn validate_source_substitutions(
    unit: &ParsedUnit<'_, '_>,
    source_text: &str,
    substitutions: &[SourceTextSubstitution],
) -> Result<(), CompilerError> {
    let mut previous_end = 0_u32;
    for substitution in substitutions {
        let span = substitution.transformed();
        let range = usize::try_from(span.start)
            .ok()
            .and_then(|start| usize::try_from(span.end).ok().map(|end| start..end));
        if span.start >= span.end
            || span.start < previous_end
            || substitution.original().is_empty()
            || range
                .as_ref()
                .and_then(|range| source_text.get(range.clone()))
                .is_none()
        {
            return Err(CompilerError::SemanticInvariant {
                invariant: "ordered nonempty source substitutions address UTF-8 boundaries",
                span: Some(span),
            });
        }
        let contained_by_regexp_pattern = unit.semantic().nodes().iter().any(|node| {
            let AstKind::RegExpLiteral(literal) = node.kind() else {
                return false;
            };
            let Ok(pattern_len) = u32::try_from(literal.regex.pattern.text.len()) else {
                return false;
            };
            let Some(pattern_start) = literal.span.start.checked_add(1) else {
                return false;
            };
            let Some(pattern_end) = pattern_start.checked_add(pattern_len) else {
                return false;
            };
            span.start >= pattern_start && span.end <= pattern_end
        });
        if !contained_by_regexp_pattern {
            return Err(CompilerError::SemanticInvariant {
                invariant: "source substitutions occur only within RegExp literal bodies",
                span: Some(span),
            });
        }
        previous_end = span.end;
    }
    Ok(())
}
