use std::{error::Error, fmt, sync::Arc};

use oxc_ast::{
    AstKind,
    ast::{
        BindingPattern, Expression, Function, FunctionType, PropertyKind, Statement,
        VariableDeclarationKind,
    },
};
use oxc_semantic::{ReferenceId, SymbolId};
use oxc_span::GetSpan;
use quickjs_bytecode::{
    BytecodeBuilder, BytecodePc, EncodeError, FinalOpcode, FunctionIndexDomains, Instruction,
    Operands, StackEffectError, UnverifiedFunctionBody, UnverifiedFunctionHeader,
    VerificationError, VerificationLimits, VerifiedControlFlow, verify_control_flow,
};
use quickjs_frontend::{ParsedUnit, Span};

use crate::storage::{
    BindingId, CompilationUnitKind, CompilerError, DeclarationKind, Executable, ExecutableId,
    ExecutableKind, NativeReferenceId, PlannedStorage, StoragePlacement, StoragePlan,
    build_planned_storage,
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

/// Owned output from the validated ordinary leaf-function lowering slice.
///
/// This artifact is deliberately not execution authority. Its control-flow
/// certificate still requires the future whole-function verifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledLeafFunction {
    executable: ExecutableId,
    storage_plan: Arc<StoragePlan>,
    source_text: Arc<str>,
    locals: Arc<[LoweredLocal]>,
    source_instructions: Arc<[SourceInstruction]>,
    control_flow: Arc<VerifiedControlFlow>,
}

impl CompiledLeafFunction {
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

    /// Returns the selected function's dense local layout.
    #[must_use]
    pub fn locals(&self) -> &[LoweredLocal] {
        &self.locals
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
        let planned = build_planned_storage(unit)?;
        let source_text = Arc::from(unit.program().source_text);
        Ok(Self {
            unit,
            planned,
            source_text,
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
    /// The accepted initial Script-only family has one lexical binding
    /// initialized from a resolved argument and returns that lexical binding.
    /// The entire function is validated before any instruction is emitted.
    ///
    /// # Errors
    ///
    /// Rejects foreign executable selections, unsupported source structure,
    /// inconsistent semantic identities, bytecode encoding failures, and
    /// verifier failures.
    pub fn compile_leaf(
        &self,
        selection: &CompilationExecutable,
        limits: VerificationLimits,
    ) -> Result<CompiledLeafFunction, LeafCompilationError> {
        let executable = self.resolve_selection(selection)?;
        let validated = self.validate_leaf(executable)?;
        let mut emitter = StraightLineEmitter::new(limits.max_bytecode_bytes_per_function());

        emitter.emit(
            FinalOpcode::SetLocUninitialized,
            Operands::Loc(validated.local_slot.index()),
            validated.local_span,
        )?;
        let (get_argument, get_argument_operands) = compact_get_argument(validated.argument_slot);
        emitter.emit(
            get_argument,
            get_argument_operands,
            validated.initializer_span,
        )?;
        let (put_local, put_local_operands) = compact_put_local(validated.local_slot);
        emitter.emit(put_local, put_local_operands, validated.local_span)?;
        emitter.emit(
            FinalOpcode::GetLocCheck,
            Operands::Loc(validated.local_slot.index()),
            validated.return_value_span,
        )?;
        emitter.emit(FinalOpcode::Return, Operands::None, validated.return_span)?;
        if emitter.depth != 0 {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "ordinary leaf returns with an empty operand stack",
                span: Some(validated.return_span),
            });
        }

        let (bytecode, source_instructions, expected_stack_size) = emitter.finish();
        let domains =
            FunctionIndexDomains::new(0, 0, validated.argument_count, validated.local_count, 0);
        let header = UnverifiedFunctionHeader::stripped_ordinary_source_function(
            validated.strict,
            validated.argument_count,
        );
        let control_flow = verify_control_flow(
            UnverifiedFunctionBody::new(bytecode, expected_stack_size, domains, header),
            limits,
        )
        .map_err(|source| LeafCompilationError::BytecodeVerification { source })?;

        Ok(CompiledLeafFunction {
            executable,
            storage_plan: Arc::clone(&self.planned.plan),
            source_text: Arc::clone(&self.source_text),
            locals: Arc::from([LoweredLocal {
                binding: validated.local_binding,
                slot: validated.local_slot,
            }]),
            source_instructions: source_instructions.into(),
            control_flow: Arc::new(control_flow),
        })
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

    fn validate_leaf(
        &self,
        executable_id: ExecutableId,
    ) -> Result<ValidatedLeaf, LeafCompilationError> {
        let (executable, function) = self.selected_ordinary_leaf(executable_id)?;
        let source = validate_source_shape(function)?;
        let layout = FrameLayout::new(&self.planned.plan, executable_id)?;

        let local_binding = self.binding_for_identifier(source.local_symbol, source.local_span)?;
        let local_slot = layout
            .local(local_binding)
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedBinding,
                span: source.local_span,
            })?;
        let local_storage = self.planned.plan.binding(local_binding).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "local compiler binding exists",
                span: Some(source.local_span),
            },
        )?;
        if !matches!(
            local_storage.policy().kind(),
            DeclarationKind::Let | DeclarationKind::Const
        ) || !local_storage.policy().has_temporal_dead_zone()
        {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedBinding,
                source.local_span,
            );
        }

        let initializer_binding =
            self.resolved_binding(source.initializer_reference, source.initializer_span)?;
        let argument_slot =
            layout
                .argument(initializer_binding)
                .ok_or(LeafCompilationError::Unsupported {
                    feature: UnsupportedLeafFeature::UnsupportedInitializer,
                    span: source.initializer_span,
                })?;
        let return_binding =
            self.resolved_binding(source.return_reference, source.return_value_span)?;
        if return_binding != local_binding {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedReturn,
                source.return_value_span,
            );
        }

        Ok(ValidatedLeaf {
            strict: executable.is_strict(),
            argument_count: executable.parameter_count(),
            local_count: u32::from(layout.local_count),
            local_binding,
            local_slot,
            argument_slot,
            local_span: source.local_span,
            initializer_span: source.initializer_span,
            return_value_span: source.return_value_span,
            return_span: source.return_span,
        })
    }

    fn selected_ordinary_leaf(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Function<'arena>), LeafCompilationError> {
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
        if is_expression && function.id.is_some() {
            return unsupported(
                UnsupportedLeafFeature::NamedFunctionExpression,
                function.span,
            );
        }
        if !is_declaration && !is_expression {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedFunctionForm,
                function.span,
            );
        }
        if is_object_method_or_accessor(self.unit, node_id) {
            return unsupported(
                UnsupportedLeafFeature::ObjectMethodOrAccessor,
                function.span,
            );
        }
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
        if let Some(child) = self
            .planned
            .plan
            .executables()
            .iter()
            .find(|candidate| candidate.parent() == Some(executable_id))
        {
            return unsupported(UnsupportedLeafFeature::NestedExecutable, child.span());
        }
        if let Some(reference) = self
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
        Ok((executable, function))
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

    fn resolved_binding(
        &self,
        reference_id: Option<ReferenceId>,
        span: Span,
    ) -> Result<BindingId, LeafCompilationError> {
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
        let resolved_id = match native {
            NativeReferenceId::Resolved(resolved_id) => resolved_id,
            NativeReferenceId::Unresolved(unresolved_id) => {
                let reference = self
                    .planned
                    .plan
                    .unresolved_globals()
                    .get(unresolved_id.index())
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
                return unsupported(UnsupportedLeafFeature::UnresolvedReference, span);
            }
        };
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
        if !reference.access().reads() || reference.access().writes() {
            return unsupported(UnsupportedLeafFeature::UnsupportedReference, span);
        }
        Ok(reference.binding())
    }
}

fn is_object_method_or_accessor(unit: &ParsedUnit<'_, '_>, node_id: oxc_semantic::NodeId) -> bool {
    let AstKind::ObjectProperty(property) = unit.semantic().nodes().parent_kind(node_id) else {
        return false;
    };
    let Expression::FunctionExpression(value) = &property.value else {
        return false;
    };
    value.node_id.get() == node_id
        && (property.method || !matches!(property.kind, PropertyKind::Init))
}

struct ValidatedSourceShape {
    local_symbol: Option<SymbolId>,
    initializer_reference: Option<ReferenceId>,
    return_reference: Option<ReferenceId>,
    local_span: Span,
    initializer_span: Span,
    return_value_span: Span,
    return_span: Span,
}

fn validate_source_shape(
    function: &Function<'_>,
) -> Result<ValidatedSourceShape, LeafCompilationError> {
    let body = function
        .body
        .as_ref()
        .ok_or(LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::UnsupportedBody,
            span: function.span,
        })?;
    let mut statements = body.statements.iter();
    let declaration_statement = statements.next().ok_or(LeafCompilationError::Unsupported {
        feature: UnsupportedLeafFeature::UnsupportedBody,
        span: body.span,
    })?;
    let Statement::VariableDeclaration(declaration) = declaration_statement else {
        return unsupported(
            UnsupportedLeafFeature::UnsupportedBody,
            declaration_statement.span(),
        );
    };
    if !matches!(
        declaration.kind,
        VariableDeclarationKind::Let | VariableDeclarationKind::Const
    ) || declaration.declarations.len() != 1
    {
        return unsupported(
            UnsupportedLeafFeature::UnsupportedDeclaration,
            declaration.span,
        );
    }
    let declarator = &declaration.declarations[0];
    let BindingPattern::BindingIdentifier(binding_identifier) = &declarator.id else {
        return unsupported(
            UnsupportedLeafFeature::UnsupportedDeclaration,
            declarator.span,
        );
    };
    let initializer = declarator
        .init
        .as_ref()
        .ok_or(LeafCompilationError::Unsupported {
            feature: UnsupportedLeafFeature::UnsupportedDeclaration,
            span: declarator.span,
        })?;
    let Expression::Identifier(initializer_identifier) = initializer else {
        return unsupported(
            UnsupportedLeafFeature::UnsupportedInitializer,
            initializer.span(),
        );
    };

    let return_source = statements.next().ok_or(LeafCompilationError::Unsupported {
        feature: UnsupportedLeafFeature::UnsupportedBody,
        span: body.span,
    })?;
    let Statement::ReturnStatement(return_statement) = return_source else {
        return unsupported(
            UnsupportedLeafFeature::UnsupportedBody,
            return_source.span(),
        );
    };
    let return_value =
        return_statement
            .argument
            .as_ref()
            .ok_or(LeafCompilationError::Unsupported {
                feature: UnsupportedLeafFeature::UnsupportedReturn,
                span: return_statement.span,
            })?;
    let Expression::Identifier(return_identifier) = return_value else {
        return unsupported(
            UnsupportedLeafFeature::UnsupportedReturn,
            return_value.span(),
        );
    };
    if let Some(extra) = statements.next() {
        return unsupported(UnsupportedLeafFeature::UnsupportedBody, extra.span());
    }

    Ok(ValidatedSourceShape {
        local_symbol: binding_identifier.symbol_id.get(),
        initializer_reference: initializer_identifier.reference_id.get(),
        return_reference: return_identifier.reference_id.get(),
        local_span: binding_identifier.span,
        initializer_span: initializer_identifier.span,
        return_value_span: return_identifier.span,
        return_span: return_statement.span,
    })
}

#[derive(Clone, Copy)]
struct ArgumentSlot(u16);

#[derive(Clone, Copy)]
enum FrameSlot {
    Argument(ArgumentSlot),
    Local(LocalSlot),
}

struct FrameLayout {
    slots: Vec<Option<FrameSlot>>,
    local_count: u16,
}

impl FrameLayout {
    fn new(plan: &StoragePlan, executable: ExecutableId) -> Result<Self, LeafCompilationError> {
        let mut slots = vec![None; plan.bindings().len()];
        let mut local_count = 0_u16;
        let bindings = plan
            .bindings_for(executable)
            .ok_or(LeafCompilationError::InvalidExecutable { executable })?;
        for binding in bindings {
            let slot = match binding.placement() {
                StoragePlacement::Argument { parameter_index } => {
                    let parameter_index = u16::try_from(parameter_index).map_err(|_| {
                        LeafCompilationError::CapacityExceeded {
                            domain: "function argument slots",
                        }
                    })?;
                    FrameSlot::Argument(ArgumentSlot(parameter_index))
                }
                StoragePlacement::Local => {
                    let slot = LocalSlot(local_count);
                    local_count = local_count.checked_add(1).ok_or(
                        LeafCompilationError::CapacityExceeded {
                            domain: "function local slots",
                        },
                    )?;
                    FrameSlot::Local(slot)
                }
                StoragePlacement::GlobalObject
                | StoragePlacement::GlobalLexical
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
            let target = slots.get_mut(binding.id().index()).ok_or(
                LeafCompilationError::SemanticInvariant {
                    invariant: "binding identity indexes frame layout",
                    span: binding.declaration_spans().first().copied(),
                },
            )?;
            if target.replace(slot).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "one frame slot per compiler binding",
                    span: binding.declaration_spans().first().copied(),
                });
            }
        }
        Ok(Self { slots, local_count })
    }

    fn argument(&self, binding: BindingId) -> Option<ArgumentSlot> {
        match self.slots.get(binding.index()).copied().flatten()? {
            FrameSlot::Argument(slot) => Some(slot),
            FrameSlot::Local(_) => None,
        }
    }

    fn local(&self, binding: BindingId) -> Option<LocalSlot> {
        match self.slots.get(binding.index()).copied().flatten()? {
            FrameSlot::Local(slot) => Some(slot),
            FrameSlot::Argument(_) => None,
        }
    }
}

struct ValidatedLeaf {
    strict: bool,
    argument_count: u32,
    local_count: u32,
    local_binding: BindingId,
    local_slot: LocalSlot,
    argument_slot: ArgumentSlot,
    local_span: Span,
    initializer_span: Span,
    return_value_span: Span,
    return_span: Span,
}

struct StraightLineEmitter {
    builder: BytecodeBuilder,
    source_instructions: Vec<SourceInstruction>,
    depth: u32,
    max_depth: u32,
}

impl StraightLineEmitter {
    fn new(byte_limit: u32) -> Self {
        Self {
            builder: BytecodeBuilder::with_byte_limit(byte_limit),
            source_instructions: Vec::new(),
            depth: 0,
            max_depth: 0,
        }
    }

    fn emit(
        &mut self,
        opcode: FinalOpcode,
        operands: Operands,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let pc = self.builder.next_pc();
        let instruction = Instruction::new(opcode, operands).map_err(|source| {
            LeafCompilationError::BytecodeEncoding {
                span,
                source: EncodeError::InvalidInstruction { pc, source },
            }
        })?;
        let effect = instruction
            .stack_effect()
            .map_err(|source| LeafCompilationError::BytecodeStackEffect { span, source })?;
        if self.depth < effect.pops() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiler-emitted straight-line stack does not underflow",
                span: Some(span),
            });
        }
        let output_depth = self
            .depth
            .checked_sub(effect.pops())
            .and_then(|depth| depth.checked_add(effect.pushes()))
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "operand stack depth",
            })?;
        let emitted_pc = self
            .builder
            .push_instruction(instruction)
            .map_err(|source| LeafCompilationError::BytecodeEncoding { span, source })?;
        if emitted_pc != pc {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "bytecode builder returns its prior next position",
                span: Some(span),
            });
        }
        self.depth = output_depth;
        self.max_depth = self.max_depth.max(output_depth);
        self.source_instructions
            .push(SourceInstruction { pc, span });
        Ok(())
    }

    fn finish(self) -> (Vec<u8>, Vec<SourceInstruction>, u32) {
        (
            self.builder.into_bytes(),
            self.source_instructions,
            self.max_depth,
        )
    }
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

fn unsupported<T>(feature: UnsupportedLeafFeature, span: Span) -> Result<T, LeafCompilationError> {
    Err(LeafCompilationError::Unsupported { feature, span })
}

/// Syntax or storage behavior outside the first ordinary leaf-function slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedLeafFeature {
    /// The selected executable is not a synchronous ordinary function.
    NonOrdinaryFunction,
    /// A named function expression requires its immutable self binding.
    NamedFunctionExpression,
    /// The Oxc function form is neither a declaration nor function expression.
    UnsupportedFunctionForm,
    /// Object methods and accessors need distinct prototype/home-object flags.
    ObjectMethodOrAccessor,
    /// The selected function contains another executable body.
    NestedExecutable,
    /// Module-owned storage is outside this Script-only lowering slice.
    UnsupportedCompilationUnit,
    /// A statement sequence is outside the validated two-statement family.
    UnsupportedBody,
    /// A declaration is not one simple lexical declarator.
    UnsupportedDeclaration,
    /// The initializer is not one resolved argument identifier.
    UnsupportedInitializer,
    /// The return is not the declared local identifier.
    UnsupportedReturn,
    /// A binding cannot be represented by this frame layout.
    UnsupportedBinding,
    /// A reference is not a pure read.
    UnsupportedReference,
    /// An identifier remained unresolved after Oxc semantics.
    UnresolvedReference,
}

/// Failure to lower or verify an ordinary leaf function.
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
    /// A typed final instruction could not be encoded.
    BytecodeEncoding {
        /// Source span responsible for the instruction.
        span: Span,
        /// Exact encoder failure.
        source: EncodeError,
    },
    /// Static opcode metadata could not produce a complete stack effect.
    BytecodeStackEffect {
        /// Source span responsible for the instruction.
        span: Span,
        /// Exact stack-effect failure.
        source: StackEffectError,
    },
    /// The emitted body failed staged control-flow verification.
    BytecodeVerification {
        /// Exact verifier failure.
        source: VerificationError,
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
            Self::BytecodeEncoding { span, source } => {
                write!(formatter, "bytecode encoding failed at {span:?}: {source}")
            }
            Self::BytecodeStackEffect { span, source } => {
                write!(
                    formatter,
                    "bytecode stack effect failed at {span:?}: {source}"
                )
            }
            Self::BytecodeVerification { source } => {
                write!(formatter, "bytecode verification failed: {source}")
            }
        }
    }
}

impl Error for LeafCompilationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BytecodeEncoding { source, .. } => Some(source),
            Self::BytecodeStackEffect { source, .. } => Some(source),
            Self::BytecodeVerification { source } => Some(source),
            Self::ForeignExecutable { .. }
            | Self::InvalidExecutable { .. }
            | Self::Unsupported { .. }
            | Self::SemanticInvariant { .. }
            | Self::CapacityExceeded { .. } => None,
        }
    }
}
