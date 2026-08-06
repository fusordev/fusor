use quickjs_frontend::Span;

use super::super::{
    ArrowFunctionExpression, AstKind, CompilationContext, CompilationUnitKind, Executable,
    ExecutableId, ExecutableKind, Expression, Function, FunctionType, LeafCompilationError, NodeId,
    ParsedUnit, Program, PropertyKind, UnsupportedLeafFeature, unsupported,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum OrdinaryFunctionForm {
    Function,
    ObjectMethod { property_span: Span },
}

impl<'arena> CompilationContext<'_, 'arena, '_> {
    pub(in crate::lowering) fn selected_arrow(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &ArrowFunctionExpression<'arena>), LeafCompilationError> {
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
                invariant: "arrow executable has an Oxc node identity",
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
                invariant: "Oxc arrow and executable identities are bijective",
                span: Some(executable.span()),
            });
        }
        let AstKind::ArrowFunctionExpression(arrow) = self.unit.semantic().nodes().kind(node_id)
        else {
            return unsupported(
                UnsupportedLeafFeature::NonOrdinaryFunction,
                executable.span(),
            );
        };
        if arrow.r#async {
            return unsupported(UnsupportedLeafFeature::NonOrdinaryFunction, arrow.span);
        }
        if executable.kind()
            != (ExecutableKind::Arrow {
                asynchronous: false,
            })
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc arrow has matching executable metadata",
                span: Some(arrow.span),
            });
        }
        if self.planned.plan.kind() != CompilationUnitKind::Script {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                arrow.span,
            );
        }
        if !crate::is_supported_script_root_goal(self.unit.goal())
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
        Ok((executable, arrow))
    }

    pub(in crate::lowering) fn selected_function(
        &self,
        executable_id: ExecutableId,
    ) -> Result<
        (
            &Executable,
            &Function<'arena>,
            OrdinaryFunctionForm,
            bool,
            bool,
        ),
        LeafCompilationError,
    > {
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
        if executable.kind()
            != (ExecutableKind::Function {
                asynchronous: function.r#async,
                generator: function.generator,
            })
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc function has matching executable metadata",
                span: Some(function.span),
            });
        }
        if self.planned.plan.kind() != CompilationUnitKind::Script {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                function.span,
            );
        }
        if !crate::is_supported_script_root_goal(self.unit.goal())
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
        Ok((
            executable,
            function,
            form,
            function.generator,
            function.r#async,
        ))
    }

    pub(in crate::lowering) fn selected_dynamic_function_script(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Program<'arena>), LeafCompilationError> {
        if !crate::is_supported_dynamic_function_goal(self.unit.goal()) {
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
                invariant: "dynamic function has one synchronous sloppy Script root",
                span: Some(program.span),
            });
        }
        Ok((executable, program))
    }

    pub(in crate::lowering) fn selected_global_script(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Program<'arena>), LeafCompilationError> {
        if !crate::is_supported_global_script_goal(self.unit.goal()) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
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
                invariant: "Global Script executable has an Oxc node identity",
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
                invariant: "Global Script Oxc node and executable identities are bijective",
                span: Some(executable.span()),
            });
        }
        let AstKind::Program(program) = self.unit.semantic().nodes().kind(node_id) else {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                executable.span(),
            );
        };
        if executable_id.index() != 0
            || executable.parent().is_some()
            || executable.parameter_count() != 0
            || !matches!(
                executable.kind(),
                ExecutableKind::Script {
                    asynchronous: false
                }
            )
            || self.planned.plan.kind() != CompilationUnitKind::Script
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Global Script has one synchronous zero-argument Program root",
                span: Some(program.span),
            });
        }
        Ok((executable, program))
    }
}

pub(in crate::lowering) fn object_method_or_accessor_span(
    unit: &ParsedUnit<'_, '_>,
    node_id: NodeId,
) -> Option<Span> {
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
