use super::super::{
    BindingId, CompilationContext, DeclarationKind, FrameSlot, InitializationPolicy,
    LeafCompilationError, Span, UnsupportedLeafFeature, VariableDeclarationKind, WritePolicy,
    unsupported,
};

impl CompilationContext<'_, '_, '_> {
    pub(in crate::lowering) fn validate_realm_global_var_declaration(
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
        if !crate::is_synchronous_dynamic_function_goal(self.unit.goal())
            || declaration_kind != VariableDeclarationKind::Var
            || !merged_global_policy
            || storage.policy().writes() != WritePolicy::Mutable
            || storage.policy().has_temporal_dead_zone()
        {
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
                matches!(
                    storage.policy().kind(),
                    DeclarationKind::Var | DeclarationKind::Parameter | DeclarationKind::Function
                ) && !storage.policy().has_temporal_dead_zone()
            }
            VariableDeclarationKind::Using | VariableDeclarationKind::AwaitUsing => false,
        };
        if !valid {
            return unsupported(UnsupportedLeafFeature::UnsupportedBinding, span);
        }
        Ok(())
    }
}
