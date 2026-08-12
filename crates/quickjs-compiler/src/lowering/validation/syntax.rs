use super::super::{
    BindingId, CompilationContext, DeclarationKind, ExecutableId, FrameSlot, InitializationPolicy,
    LeafCompilationError, Span, StoragePlacement, UnsupportedLeafFeature, VariableDeclarationKind,
    WritePolicy, unsupported,
};
use oxc_ast::AstKind;
use oxc_span::GetSpan;

impl CompilationContext<'_, '_, '_> {
    pub(in crate::lowering) fn validate_realm_global_class_declaration(
        &self,
        storage: &crate::storage::BindingStorage,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let valid = storage.placement() == StoragePlacement::GlobalLexical
            && storage.policy().kind() == DeclarationKind::Class
            && storage.policy().initialization() == InitializationPolicy::AtDeclaration
            && storage.policy().writes() == WritePolicy::Mutable
            && storage.policy().has_temporal_dead_zone();
        if !crate::is_supported_script_root_goal(self.unit.goal()) || !valid {
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, span);
        }
        Ok(())
    }

    pub(in crate::lowering) fn validate_class_declaration_storage(
        &self,
        binding: BindingId,
        frame_slot: FrameSlot,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let storage =
            self.planned
                .plan
                .binding(binding)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "class declaration compiler binding exists",
                    span: Some(span),
                })?;
        let valid = storage.placement() == StoragePlacement::Local
            && storage.policy().kind() == DeclarationKind::Class
            && storage.policy().initialization() == InitializationPolicy::AtDeclaration
            && storage.policy().writes() == WritePolicy::Mutable
            && storage.policy().has_temporal_dead_zone()
            && matches!(frame_slot, FrameSlot::Local(_));
        if !valid {
            return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
        }
        Ok(())
    }

    pub(in crate::lowering) fn validate_realm_global_declaration(
        &self,
        declaration_kind: VariableDeclarationKind,
        storage: &crate::storage::BindingStorage,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let supported_goal = crate::is_supported_realm_global_binding_goal(self.unit.goal());
        let valid = match storage.placement() {
            StoragePlacement::GlobalObject => {
                declaration_kind == VariableDeclarationKind::Var
                    && matches!(
                        (storage.policy().kind(), storage.policy().initialization()),
                        (
                            DeclarationKind::Var,
                            InitializationPolicy::UndefinedAtInstantiation
                        ) | (
                            DeclarationKind::Function,
                            InitializationPolicy::FunctionAtInstantiation
                        )
                    )
                    && storage.policy().writes() == WritePolicy::Mutable
                    && !storage.policy().has_temporal_dead_zone()
            }
            StoragePlacement::GlobalLexical => {
                matches!(
                    (declaration_kind, storage.policy().kind()),
                    (VariableDeclarationKind::Let, DeclarationKind::Let)
                        | (VariableDeclarationKind::Const, DeclarationKind::Const)
                ) && storage.policy().initialization() == InitializationPolicy::AtDeclaration
                    && storage.policy().has_temporal_dead_zone()
                    && matches!(
                        (declaration_kind, storage.policy().writes()),
                        (VariableDeclarationKind::Let, WritePolicy::Mutable)
                            | (VariableDeclarationKind::Const, WritePolicy::Immutable)
                    )
            }
            StoragePlacement::Argument { .. }
            | StoragePlacement::Local
            | StoragePlacement::ModuleLocal
            | StoragePlacement::ModuleImport => false,
        };
        if !supported_goal || !valid {
            if crate::is_supported_direct_eval_goal(self.unit.goal()) {
                return unsupported(UnsupportedLeafFeature::DirectEvalVariableEnvironment, span);
            }
            return unsupported(UnsupportedLeafFeature::UnsupportedDeclaration, span);
        }
        Ok(())
    }

    pub(in crate::lowering) fn validate_declaration_storage(
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
                (matches!(
                    storage.policy().kind(),
                    DeclarationKind::Var | DeclarationKind::Parameter | DeclarationKind::Function
                ) && !storage.policy().has_temporal_dead_zone())
                    // Annex B.3.4 evaluates the initializer through the catch
                    // environment even though the `var` is instantiated in
                    // the surrounding variable environment.
                    || (storage.placement() == StoragePlacement::Local
                        && storage.policy().kind() == DeclarationKind::Catch
                        && storage.policy().initialization() == InitializationPolicy::Catch
                        && storage.policy().writes() == WritePolicy::Mutable
                        && storage.policy().has_temporal_dead_zone()
                        && matches!(frame_slot, FrameSlot::Local(_)))
            }
            VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing => false,
        };
        if !valid {
            return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
        }
        Ok(())
    }

    /// Returns the span of the first `await` expression or `for await` loop in
    /// the Module's own top level, if any. Awaits nested inside functions,
    /// arrows, or classes belong to those executables, not to the Module root.
    /// Class heritage is evaluated in the scope enclosing the class definition,
    /// so an `await` in the `extends` expression belongs to the outer
    /// executable and does not stop the walk.
    pub(in crate::lowering) fn module_top_level_await(
        &self,
        root: ExecutableId,
    ) -> Result<Option<Span>, LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        let root_program = self
            .planned
            .identities
            .node_by_executable
            .get(root.index())
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "Module root executable has an Oxc node identity",
                span: Some(self.unit.program().span),
            })?;
        for (node_id, node) in nodes.iter_enumerated() {
            let span = node.kind().span();
            let is_top_level_await_marker = match node.kind() {
                AstKind::AwaitExpression(_) => true,
                AstKind::ForOfStatement(statement) => statement.r#await,
                _ => false,
            };
            if !is_top_level_await_marker {
                continue;
            }
            // Walk parents to the nearest enclosing function, arrow, class, or
            // the Program root. If that boundary is the Module Program, the
            // node is at module top level.
            let mut current = nodes.parent_id(node_id);
            let mut child = node_id;
            let mut at_top_level = false;
            loop {
                match nodes.kind(current) {
                    AstKind::Program(_) => {
                        at_top_level = current == root_program;
                        break;
                    }
                    AstKind::Class(class) => {
                        let in_heritage = class
                            .super_class
                            .as_ref()
                            .is_some_and(|super_class| super_class.span() == nodes.kind(child).span());
                        if !in_heritage {
                            break;
                        }
                    }
                    AstKind::Function(_) | AstKind::ArrowFunctionExpression(_) => break,
                    _ => {}
                }
                let parent = nodes.parent_id(current);
                if parent.index() >= current.index() {
                    break;
                }
                child = current;
                current = parent;
            }
            if at_top_level {
                return Ok(Some(span));
            }
        }
        Ok(None)
    }
}
