use oxc_syntax::{identifier::is_white_space, line_terminator::is_line_terminator};
use quickjs_frontend::Span;

use super::super::{
    ArrowFunctionExpression, AstKind, Class, CompilationContext, CompilationUnitKind, Executable,
    ExecutableId, ExecutableKind, Expression, Function, FunctionType, LeafCompilationError,
    MethodDefinition, MethodDefinitionKind, NodeId, ParsedUnit, Program, PropertyKind,
    UnsupportedLeafFeature, unsupported,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum OrdinaryFunctionForm {
    Function,
    ObjectMethod { property_span: Span },
    ClassConstructor { class_span: Span, derived: bool },
    ClassMethod { property_span: Span },
}

impl<'arena> CompilationContext<'_, 'arena, '_> {
    pub(in crate::lowering) fn selected_class_instance_initializer(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Class<'arena>), LeafCompilationError> {
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
                invariant: "class instance initializer has an Oxc class identity",
                span: Some(executable.span()),
            })?;
        let AstKind::Class(class) = self.unit.semantic().nodes().kind(node_id) else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "class instance initializer belongs to a class node",
                span: Some(executable.span()),
            });
        };
        if self
            .planned
            .identities
            .class_instance_initializers
            .get(&node_id)
            .copied()
            != Some(executable_id)
            || executable.kind() != ExecutableKind::ClassInstanceInitializer
            || executable.parameter_count() != 0
            || executable.defined_parameter_count() != 0
            || !executable.has_simple_parameter_list()
            || executable.has_parameter_expressions()
            || !executable.is_strict()
            || !class.decorators.is_empty()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "hidden instance initializer has strict method metadata",
                span: Some(class.span),
            });
        }
        // Mirrors selected_function: scripts and modules both link closures
        // against root cells, and the strict metadata required above holds for
        // module units as well (modules are always strict).
        if !matches!(
            self.planned.plan.kind(),
            CompilationUnitKind::Script | CompilationUnitKind::Module
        ) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                class.span,
            );
        }
        Ok((executable, class))
    }

    pub(in crate::lowering) fn selected_default_class_constructor(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Class<'arena>), LeafCompilationError> {
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
                invariant: "default class constructor has an Oxc class identity",
                span: Some(executable.span()),
            })?;
        let AstKind::Class(class) = self.unit.semantic().nodes().kind(node_id) else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "default class constructor belongs to a class node",
                span: Some(executable.span()),
            });
        };
        if self
            .planned
            .identities
            .default_class_constructors
            .get(&node_id)
            .copied()
            != Some(executable_id)
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "class has its selected synthesized default constructor",
                span: Some(class.span),
            });
        }
        if executable.kind() != ExecutableKind::ClassDefaultConstructor
            || executable.parameter_count() != 0
            || executable.defined_parameter_count() != 0
            || !executable.has_simple_parameter_list()
            || executable.has_parameter_expressions()
            || !executable.is_strict()
            || !class.decorators.is_empty()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "synthesized default constructor has base-class metadata",
                span: Some(class.span),
            });
        }
        // Mirrors selected_function: scripts and modules both link closures
        // against root cells, and the strict metadata required above holds for
        // module units as well (modules are always strict).
        if !matches!(
            self.planned.plan.kind(),
            CompilationUnitKind::Script | CompilationUnitKind::Module
        ) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                class.span,
            );
        }
        Ok((executable, class))
    }

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
        if executable.kind()
            != (ExecutableKind::Arrow {
                asynchronous: arrow.r#async,
            })
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc arrow has matching executable metadata",
                span: Some(arrow.span),
            });
        }
        // Mirrors selected_function: scripts and modules both link closures
        // against root cells. Arrow lowering is unit-kind agnostic: `this` and
        // `new.target` resolve lexically at runtime (module top-level `this` is
        // undefined, and new.target at module top level is a SyntaxError caught
        // by the parser). Module units are strict, which only flows through as
        // executable.is_strict() metadata below.
        if !matches!(
            self.planned.plan.kind(),
            CompilationUnitKind::Script | CompilationUnitKind::Module
        ) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                arrow.span,
            );
        }
        if !crate::is_supported_script_compilation_goal(self.unit.goal())
            && !crate::is_supported_module_goal(self.unit.goal())
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
        let form = class_method_form(self.unit, node_id)?.unwrap_or_else(|| {
            object_method_or_accessor_span(self.unit, node_id)
                .map_or(OrdinaryFunctionForm::Function, |property_span| {
                    OrdinaryFunctionForm::ObjectMethod { property_span }
                })
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
        if !matches!(
            self.planned.plan.kind(),
            CompilationUnitKind::Script | CompilationUnitKind::Module
        ) {
            return unsupported(
                UnsupportedLeafFeature::UnsupportedCompilationUnit,
                function.span,
            );
        }
        if !crate::is_supported_script_compilation_goal(self.unit.goal())
            && !crate::is_supported_module_goal(self.unit.goal())
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

    pub(in crate::lowering) fn selected_module(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Program<'arena>), LeafCompilationError> {
        if !crate::is_supported_module_goal(self.unit.goal()) {
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
                invariant: "Module executable has an Oxc node identity",
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
                invariant: "Module Oxc node and executable identities are bijective",
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
            || !matches!(executable.kind(), ExecutableKind::Module)
            || self.planned.plan.kind() != CompilationUnitKind::Module
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Module has one zero-argument Program root",
                span: Some(program.span),
            });
        }
        Ok((executable, program))
    }

    pub(in crate::lowering) fn selected_eval_script(
        &self,
        executable_id: ExecutableId,
    ) -> Result<(&Executable, &Program<'arena>), LeafCompilationError> {
        if !crate::is_supported_indirect_eval_goal(self.unit.goal())
            && !crate::is_supported_direct_eval_goal(self.unit.goal())
        {
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
                invariant: "eval executable has an Oxc node identity",
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
                invariant: "eval Oxc node and executable identities are bijective",
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
                invariant: "eval has one synchronous zero-argument Program root",
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

fn class_method_form(
    unit: &ParsedUnit<'_, '_>,
    node_id: NodeId,
) -> Result<Option<OrdinaryFunctionForm>, LeafCompilationError> {
    let AstKind::MethodDefinition(method) = unit.semantic().nodes().parent_kind(node_id) else {
        return Ok(None);
    };
    if method.value.node_id.get() != node_id {
        return Ok(None);
    }
    let AstKind::ClassBody(body) = unit.semantic().nodes().parent_kind(method.node_id.get()) else {
        return Ok(None);
    };
    let AstKind::Class(class) = unit.semantic().nodes().parent_kind(body.node_id.get()) else {
        return Ok(None);
    };
    let form = match method.kind {
        MethodDefinitionKind::Constructor => OrdinaryFunctionForm::ClassConstructor {
            class_span: class.span,
            derived: class.super_class.is_some(),
        },
        MethodDefinitionKind::Method | MethodDefinitionKind::Get | MethodDefinitionKind::Set => {
            OrdinaryFunctionForm::ClassMethod {
                property_span: class_method_definition_span(unit, method)?,
            }
        }
    };
    Ok(Some(form))
}

fn class_method_definition_span(
    unit: &ParsedUnit<'_, '_>,
    method: &MethodDefinition<'_>,
) -> Result<Span, LeafCompilationError> {
    if !method.r#static {
        return Ok(method.span);
    }

    let invalid_source = || LeafCompilationError::SemanticInvariant {
        invariant: "a static class element retains its nested MethodDefinition source",
        span: Some(method.span),
    };
    let source = unit.program().source_text;
    let start = method.span.start as usize;
    let end = method.span.end as usize;
    let element_source = source.get(start..end).ok_or_else(invalid_source)?;
    let after_static = element_source
        .strip_prefix("static")
        .ok_or_else(invalid_source)?;
    let trivia_bytes = leading_ecmascript_trivia_bytes(after_static).ok_or_else(invalid_source)?;
    let relative_start = "static"
        .len()
        .checked_add(trivia_bytes)
        .ok_or_else(invalid_source)?;
    if relative_start >= element_source.len() {
        return Err(invalid_source());
    }
    let relative_start =
        u32::try_from(relative_start).map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "class method source span",
        })?;
    let nested_start = method
        .span
        .start
        .checked_add(relative_start)
        .ok_or_else(invalid_source)?;
    Ok(Span::new(nested_start, method.span.end))
}

fn leading_ecmascript_trivia_bytes(source: &str) -> Option<usize> {
    let mut offset = 0_usize;
    loop {
        let remaining = source.get(offset..)?;
        if let Some(comment) = remaining.strip_prefix("//") {
            let comment_bytes = comment
                .char_indices()
                .find_map(|(index, character)| is_line_terminator(character).then_some(index))
                .unwrap_or(comment.len());
            offset = offset.checked_add(2)?.checked_add(comment_bytes)?;
            continue;
        }
        if let Some(comment) = remaining.strip_prefix("/*") {
            let comment_bytes = comment.find("*/")?;
            offset = offset
                .checked_add(2)?
                .checked_add(comment_bytes)?
                .checked_add(2)?;
            continue;
        }
        let Some(character) = remaining.chars().next() else {
            return Some(offset);
        };
        if !is_white_space(character) && !is_line_terminator(character) {
            return Some(offset);
        }
        offset = offset.checked_add(character.len_utf8())?;
    }
}
