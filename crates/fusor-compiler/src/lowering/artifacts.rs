use std::sync::Arc;

use fusor_bytecode::{
    AtomPoolIndex, BytecodePc, CompilerAtom, CompilerBindingPolicy, CompilerClosureBinding,
    CompilerConstantKind, CompilerConstantValue, FunctionTemplateId, UnverifiedFunctionMetadata,
    VerifiedBytecode, VerifiedCompilerFunctionGraph, VerifiedControlFlow,
};
use fusor_frontend::Span;

use crate::storage::{BindingId, CaptureSlot, DeclarationPolicy, ExecutableId, StoragePlan};

/// A validated function-local slot number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LocalSlot(pub(super) u16);

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
    pub(super) binding: BindingId,
    pub(super) slot: LocalSlot,
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
    pub(super) pc: BytecodePc,
    pub(super) span: Span,
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
    pub(super) executable: ExecutableId,
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
pub struct RealmGlobalId(pub(super) u32);

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
    /// A direct-eval Script root imports one live caller binding.
    DirectEvalBinding {
        /// Zero-based entry in the caller-environment snapshot.
        index: u32,
        /// Exact caller-environment shape bound into the authority.
        environment_size: u32,
    },
    /// A sloppy direct-eval declaration creates this caller function binding.
    DirectEvalVariable {
        /// Dense entry appended after the caller snapshot.
        index: u32,
        /// Exact combined caller and created-variable environment size.
        environment_size: u32,
    },
    /// A child forwards the same realm-owned handle from its parent.
    ParentClosure(u16),
}

/// One dense constructor-realm global descriptor for a compiled function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledRealmGlobal {
    pub(super) id: RealmGlobalId,
    pub(super) name: Arc<str>,
    pub(super) atom: AtomPoolIndex,
    pub(super) slot: u16,
    pub(super) source: CompiledRealmGlobalSource,
    pub(super) binding: CompilerClosureBinding,
    pub(super) policy: CompilerBindingPolicy,
    pub(super) deletable_eval_variable: bool,
    pub(super) function_initializer: Option<u32>,
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

    /// Returns whether this slot is a captured caller cell or a Realm-global
    /// handle.
    #[must_use]
    pub const fn binding(&self) -> CompilerClosureBinding {
        self.binding
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

/// Dense compiler identity of one module-environment binding.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ModuleBindingId(pub(super) u32);

impl ModuleBindingId {
    /// Returns the dense zero-based module-binding index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Source of one module-environment closure-domain slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompiledModuleBindingSource {
    /// The Module root owns this cell in its module environment.
    Module {
        /// Zero-based cell index in the module environment.
        index: u32,
    },
    /// A child forwards the same module cell from its parent.
    ParentClosure(u16),
}

/// One module-environment binding descriptor for a compiled function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledModuleBinding {
    pub(super) id: ModuleBindingId,
    pub(super) name: Arc<str>,
    pub(super) atom: AtomPoolIndex,
    pub(super) slot: u16,
    pub(super) source: CompiledModuleBindingSource,
    pub(super) policy: CompilerBindingPolicy,
    pub(super) origin: fusor_bytecode::ModuleBindingOrigin,
    pub(super) import: Option<fusor_bytecode::ModuleImportName>,
    pub(super) function_initializer: Option<u32>,
}

#[allow(dead_code)]
impl CompiledModuleBinding {
    /// Returns the compilation-unit module-binding identity.
    #[must_use]
    pub const fn id(&self) -> ModuleBindingId {
        self.id
    }

    /// Returns the exact binding identifier text.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the function-local atom naming this binding.
    #[must_use]
    pub const fn atom(&self) -> AtomPoolIndex {
        self.atom
    }

    /// Returns the dense function-local closure-domain slot.
    #[must_use]
    pub const fn slot(&self) -> u16 {
        self.slot
    }

    /// Returns where this function originates or forwards the module cell.
    #[must_use]
    pub const fn source(&self) -> CompiledModuleBindingSource {
        self.source
    }

    /// Returns the verified declaration policy.
    #[must_use]
    pub const fn policy(&self) -> CompilerBindingPolicy {
        self.policy
    }

    /// Returns the module binding origin category.
    #[must_use]
    pub const fn origin(&self) -> fusor_bytecode::ModuleBindingOrigin {
        self.origin
    }

    /// Returns the import-side name for an imported binding.
    #[must_use]
    pub fn import(&self) -> Option<&fusor_bytecode::ModuleImportName> {
        self.import.as_ref()
    }

    /// Returns the root-only function-template initializer for a hoisted
    /// module-level function declaration.
    #[must_use]
    pub const fn function_initializer(&self) -> Option<u32> {
        self.function_initializer
    }
}

/// One dense imported-closure descriptor for a compiled function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompiledClosureVariable {
    pub(super) binding: BindingId,
    pub(super) slot: CaptureSlot,
    pub(super) source: CompiledClosureSource,
    pub(super) policy: DeclarationPolicy,
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
    pub(super) executable: ExecutableId,
    pub(super) storage_plan: Arc<StoragePlan>,
    pub(super) source_text: Arc<str>,
    pub(super) locals: Arc<[LoweredLocal]>,
    pub(super) atoms: Arc<[CompilerAtom]>,
    pub(super) constants: Arc<[CompiledConstant]>,
    pub(super) closure_variables: Arc<[CompiledClosureVariable]>,
    pub(super) realm_globals: Arc<[CompiledRealmGlobal]>,
    pub(super) module_bindings: Arc<[CompiledModuleBinding]>,
    pub(super) source_instructions: Arc<[SourceInstruction]>,
    pub(super) control_flow: Arc<VerifiedControlFlow>,
    pub(super) eval_reference_call_instructions: Arc<[u32]>,
    pub(super) parameter_initialization_end: Option<u32>,
    pub(super) function_initializer_prefix_start: u32,
    pub(super) metadata: UnverifiedFunctionMetadata,
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

    /// Returns the `eval` instruction indices whose callee was obtained as a
    /// reference carrying an ordinary-call receiver.
    #[must_use]
    pub fn eval_reference_call_instructions(&self) -> &[u32] {
        &self.eval_reference_call_instructions
    }

    /// Returns the first body instruction after parameter-expression evaluation.
    #[must_use]
    pub const fn parameter_initialization_end(&self) -> Option<u32> {
        self.parameter_initialization_end
    }

    /// Returns the first instruction in the isolated lexical/function
    /// instantiation prefix after parameter and `arguments` initialization.
    #[must_use]
    pub const fn function_initializer_prefix_start(&self) -> u32 {
        self.function_initializer_prefix_start
    }
}

/// Backward-compatible name for the nested-function-free
/// [`super::CompilationContext::compile_leaf`] result.
pub type CompiledLeafFunction = CompiledFunction;

/// Failure-atomic output for one complete compiled executable subtree.
///
/// Functions are stored in stable executable preorder. Every constant edge
/// names one member of this same tree. A selected nested root is accepted only
/// when it imports no cells from its omitted external parent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledFunctionTree {
    pub(super) root: ExecutableId,
    pub(super) storage_plan: Arc<StoragePlan>,
    pub(super) source_text: Arc<str>,
    pub(super) functions: Arc<[CompiledFunction]>,
    pub(super) function_graph: Arc<VerifiedCompilerFunctionGraph>,
    pub(super) verified_bytecode: Arc<VerifiedBytecode>,
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
