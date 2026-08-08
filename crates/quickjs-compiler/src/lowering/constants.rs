use std::sync::Arc;

use quickjs_bytecode::{
    AtomPoolIndex, Binary64Constant, CompilerAtom, CompilerBigInt, CompilerConstantValue,
    CompilerString, CompilerTemplateElement, CompilerTemplateObject, FinalOpcode, Operands,
};
use quickjs_frontend::Span;

use crate::storage::ExecutableId;

use super::atoms::{
    CompiledAtomCandidate, CompiledMetadataAtomCandidate, CompiledMetadataAtomKey,
    CompiledPropertyAtomKey, freeze_atom_candidates, freeze_metadata_atom_candidates,
};
use super::{
    ArrayExpression, ArrayExpressionElement, AssignmentTargetProperty, AstKind, BindingPattern,
    CompilationContext, CompiledConstant, CompiledFunctionConstant, Executable, Expression,
    ExpressionPlanner, FunctionTreeLayoutSeed, GetSpan, LeafCompilationError, NodeId,
    OxcPropertyKey, ParsedUnit, PlannedInstruction, PropertyKind, RegExpLiteral, StoragePlacement,
    UnaryOperator, checked_function_entry_count, compiled_static_property_key,
    compiler_identifier_string, decode_compiler_string, exact_i32, exact_negated_i32,
    record_property_candidate, record_property_candidate_for, record_string_candidate,
};

impl<'arena> CompilationContext<'_, 'arena, '_> {
    pub(in crate::lowering) fn compiled_constant_pools(
        &self,
        tree_layout: &FunctionTreeLayoutSeed,
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
        for (index, ((candidates, atoms), metadata_atoms)) in candidates
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
            pools.push(CompiledConstantPool::new(CompiledConstantPoolInput {
                children: tree_layout.children(executable.id())?,
                constant_candidates: candidates,
                atom_candidates: atoms,
                metadata_atom_candidates: metadata_atoms,
            })?);
        }
        Ok(pools.into_boxed_slice())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the single executable walk keeps every metadata atom owner and key auditable"
    )]
    fn record_metadata_atom_candidates(
        &self,
        tree_layout: &FunctionTreeLayoutSeed,
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
                && crate::is_supported_script_compilation_goal(self.unit.goal())
            {
                owner.push(CompiledMetadataAtomCandidate {
                    key: CompiledMetadataAtomKey::ScriptCompletion,
                    value: compiler_identifier_string("_ret_", executable.span())?,
                    span: executable.span(),
                });
            }
            self.record_raw_parameter_metadata_candidates(executable, owner)?;
            for binding in self.planned.plan.bindings_for(executable.id()).ok_or(
                LeafCompilationError::InvalidExecutable {
                    executable: executable.id(),
                },
            )? {
                if !matches!(
                    binding.placement(),
                    StoragePlacement::Argument { .. }
                        | StoragePlacement::Local
                        | StoragePlacement::GlobalObject
                        | StoragePlacement::GlobalLexical
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

    fn record_raw_parameter_metadata_candidates(
        &self,
        executable: &Executable,
        owner: &mut Vec<CompiledMetadataAtomCandidate>,
    ) -> Result<(), LeafCompilationError> {
        if executable.has_simple_parameter_list() {
            return Ok(());
        }
        let node = self
            .planned
            .identities
            .node_by_executable
            .get(executable.id().index())
            .copied()
            .ok_or(LeafCompilationError::InvalidExecutable {
                executable: executable.id(),
            })?;
        let parameters = match self.unit.semantic().nodes().kind(node) {
            AstKind::Function(function) => Some(function.params.as_ref()),
            AstKind::ArrowFunctionExpression(arrow) => Some(arrow.params.as_ref()),
            AstKind::Program(_) => None,
            _ => {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "non-simple parameter metadata belongs to a function",
                    span: Some(executable.span()),
                });
            }
        };
        let Some(parameters) = parameters else {
            return Ok(());
        };
        for (index, parameter) in parameters.items.iter().enumerate() {
            if !executable.has_parameter_expressions()
                && matches!(parameter.pattern, BindingPattern::BindingIdentifier(_))
            {
                continue;
            }
            let index =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "raw parameter metadata",
                })?;
            let name = format!("_arg_{index}_");
            owner.push(CompiledMetadataAtomCandidate {
                key: CompiledMetadataAtomKey::RawParameter(index),
                value: compiler_identifier_string(&name, parameter.span)?,
                span: parameter.span,
            });
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
                let owner = self
                    .instance_field_initializer_owner(node_id)?
                    .unwrap_or(owner);
                self.record_node_literal_candidate(node_id, owner, candidates, atom_candidates)?;
            }
        }
        Ok(())
    }

    fn instance_field_initializer_owner(
        &self,
        node_id: NodeId,
    ) -> Result<Option<ExecutableId>, LeafCompilationError> {
        let nodes = self.unit.semantic().nodes();
        let node_span = nodes.kind(node_id).span();
        for ancestor in nodes.ancestor_ids(node_id) {
            match nodes.kind(ancestor) {
                AstKind::Function(_) | AstKind::ArrowFunctionExpression(_) => return Ok(None),
                AstKind::PropertyDefinition(field)
                    if !field.r#static
                        && field.value.as_ref().is_some_and(|value| {
                            let value_span = value.span();
                            value_span.start <= node_span.start && node_span.end <= value_span.end
                        }) =>
                {
                    let AstKind::ClassBody(body) = nodes.parent_kind(field.node_id.get()) else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "instance field belongs to a class body",
                            span: Some(field.span),
                        });
                    };
                    let AstKind::Class(class) = nodes.parent_kind(body.node_id.get()) else {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "instance field class body belongs to a class",
                            span: Some(body.span),
                        });
                    };
                    return self
                        .instance_field_constructor_owner(class.node_id.get(), class)
                        .map(Some);
                }
                _ => {}
            }
        }
        Ok(None)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the literal/property candidate walk stays one exhaustive per-node audit"
    )]
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
            AstKind::BigIntLiteral(literal)
                if !is_noncomputed_static_property_key_node(self.unit, node_id)
                    && literal.value.parse::<i32>().is_err() =>
            {
                let decimal = compiler_identifier_string(literal.value.as_str(), literal.span)?;
                let value = CompilerBigInt::try_from_decimal(decimal).map_err(|source| {
                    LeafCompilationError::CompilerBigInt {
                        span: literal.span,
                        source,
                    }
                })?;
                candidates
                    .get_mut(owner.index())
                    .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
                    .push(CompiledConstantCandidate::BigInt {
                        value,
                        span: literal.span,
                    });
            }
            AstKind::StringLiteral(literal)
                if (!matches!(nodes.parent_kind(node_id), AstKind::Directive(_))
                    || (owner.index() == 0
                        && crate::is_supported_script_compilation_goal(self.unit.goal())))
                    && !is_noncomputed_static_property_key_node(self.unit, node_id) =>
            {
                let value = decode_compiler_string(
                    literal.value.as_str(),
                    literal.lone_surrogates,
                    literal.span,
                )?;
                record_string_candidate(owner, value, literal.span, candidates, atom_candidates)?;
            }
            AstKind::PrivateIdentifier(identifier)
                if matches!(
                    nodes.parent_kind(node_id),
                    AstKind::PropertyDefinition(_) | AstKind::MethodDefinition(_)
                ) =>
            {
                record_property_candidate(
                    owner,
                    compiler_identifier_string(
                        &format!("#{}", identifier.name.as_str()),
                        identifier.span,
                    )?,
                    identifier.span,
                    atom_candidates,
                )?;
            }
            AstKind::TaggedTemplateExpression(tagged) => {
                let template = &tagged.quasi;
                if template.quasis.len() != template.expressions.len().saturating_add(1) {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "tagged template has one more quasi than substitutions",
                        span: Some(template.span),
                    });
                }
                let mut elements = Vec::with_capacity(template.quasis.len());
                for (index, quasi) in template.quasis.iter().enumerate() {
                    if quasi.tail != (index + 1 == template.quasis.len()) {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "tagged template marks only its final quasi as tail",
                            span: Some(quasi.span),
                        });
                    }
                    let cooked = quasi
                        .value
                        .cooked
                        .as_ref()
                        .map(|value| {
                            decode_compiler_string(
                                value.as_str(),
                                quasi.lone_surrogates,
                                quasi.span,
                            )
                        })
                        .transpose()?;
                    let raw = compiler_identifier_string(quasi.value.raw.as_str(), quasi.span)?;
                    elements.push(CompilerTemplateElement::new(cooked, raw));
                }
                let value = CompilerTemplateObject::try_from_elements(elements.into()).map_err(
                    |source| LeafCompilationError::CompilerTemplateObject {
                        span: tagged.span,
                        source,
                    },
                )?;
                candidates
                    .get_mut(owner.index())
                    .ok_or(LeafCompilationError::InvalidExecutable { executable: owner })?
                    .push(CompiledConstantCandidate::TemplateObject {
                        value,
                        span: tagged.span,
                    });
            }
            AstKind::TemplateLiteral(template)
                if !matches!(
                    nodes.parent_kind(node_id),
                    AstKind::TaggedTemplateExpression(_)
                ) =>
            {
                if template.quasis.len() != template.expressions.len().saturating_add(1) {
                    return Err(LeafCompilationError::SemanticInvariant {
                        invariant: "untagged template has one more quasi than substitutions",
                        span: Some(template.span),
                    });
                }
                for (index, quasi) in template.quasis.iter().enumerate() {
                    if quasi.tail != (index + 1 == template.quasis.len()) {
                        return Err(LeafCompilationError::SemanticInvariant {
                            invariant: "untagged template marks only its final quasi as tail",
                            span: Some(quasi.span),
                        });
                    }
                    let cooked = quasi.value.cooked.as_ref().ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "untagged template quasi has a cooked value",
                            span: Some(quasi.span),
                        },
                    )?;
                    if cooked.is_empty() {
                        continue;
                    }
                    let value =
                        decode_compiler_string(cooked.as_str(), quasi.lone_surrogates, quasi.span)?;
                    record_string_candidate(owner, value, quasi.span, candidates, atom_candidates)?;
                }
            }
            AstKind::RegExpLiteral(literal) => {
                Self::record_regexp_literal_candidate(owner, literal, candidates, atom_candidates)?;
            }
            AstKind::ObjectProperty(property) => {
                if !property.computed
                    && let Some(key) = compiled_static_property_key(&property.key)?
                {
                    record_property_candidate(owner, key.value, key.span, atom_candidates)?;
                }
            }
            AstKind::Class(class) => {
                let (name, name_span) = self.class_definition_name(node_id, class)?;
                record_property_candidate_for(
                    owner,
                    name,
                    name_span,
                    CompiledPropertyAtomKey::Source(class.span),
                    atom_candidates,
                )?;
                if class.super_class.is_some() {
                    record_property_candidate_for(
                        owner,
                        compiler_identifier_string("prototype", class.span)?,
                        class.span,
                        CompiledPropertyAtomKey::ClassHeritagePrototype { class: class.span },
                        atom_candidates,
                    )?;
                }
                for element in &class.body.body {
                    match element {
                        super::ClassElement::MethodDefinition(method)
                            if !method.computed
                                && method.kind != super::MethodDefinitionKind::Constructor =>
                        {
                            if let Some(key) = compiled_static_property_key(&method.key)? {
                                record_property_candidate(
                                    owner,
                                    key.value,
                                    key.span,
                                    atom_candidates,
                                )?;
                            }
                        }
                        super::ClassElement::PropertyDefinition(field)
                            if field.r#static && !field.computed =>
                        {
                            if let Some(key) = compiled_static_property_key(&field.key)? {
                                record_property_candidate(
                                    owner,
                                    key.value,
                                    key.span,
                                    atom_candidates,
                                )?;
                            }
                        }
                        super::ClassElement::PropertyDefinition(field)
                            if !field.r#static
                                && !field.computed
                                && field.decorators.is_empty() =>
                        {
                            let field_owner =
                                self.instance_field_constructor_owner(node_id, class)?;
                            if let Some(key) = compiled_static_property_key(&field.key)? {
                                record_property_candidate(
                                    field_owner,
                                    key.value,
                                    key.span,
                                    atom_candidates,
                                )?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            AstKind::ObjectPattern(pattern) => {
                for property in &pattern.properties {
                    if !property.computed
                        && let Some(key) = compiled_static_property_key(&property.key)?
                    {
                        record_property_candidate(owner, key.value, key.span, atom_candidates)?;
                    }
                }
            }
            AstKind::ObjectAssignmentTarget(target) => {
                for property in &target.properties {
                    match property {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(
                            identifier,
                        ) => {
                            let key = compiler_identifier_string(
                                identifier.binding.name.as_str(),
                                identifier.binding.span,
                            )?;
                            record_property_candidate(
                                owner,
                                key,
                                identifier.binding.span,
                                atom_candidates,
                            )?;
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
                            if !property.computed
                                && let Some(key) = compiled_static_property_key(&property.name)?
                            {
                                record_property_candidate(
                                    owner,
                                    key.value,
                                    key.span,
                                    atom_candidates,
                                )?;
                            }
                        }
                    }
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
            AstKind::ArrayExpression(array) => {
                Self::record_array_property_candidates(owner, array, atom_candidates)?;
            }
            AstKind::YieldExpression(expression) if expression.delegate => {
                for (value, property_key) in [
                    (
                        "done",
                        CompiledPropertyAtomKey::YieldStarDone {
                            expression: expression.span,
                        },
                    ),
                    (
                        "value",
                        CompiledPropertyAtomKey::YieldStarValue {
                            expression: expression.span,
                        },
                    ),
                ] {
                    record_property_candidate_for(
                        owner,
                        compiler_identifier_string(value, expression.span)?,
                        expression.span,
                        property_key,
                        atom_candidates,
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn instance_field_constructor_owner(
        &self,
        class_node: NodeId,
        class: &super::Class<'arena>,
    ) -> Result<ExecutableId, LeafCompilationError> {
        for element in &class.body.body {
            let super::ClassElement::MethodDefinition(method) = element else {
                continue;
            };
            if method.kind != super::MethodDefinitionKind::Constructor {
                continue;
            }
            return self
                .planned
                .identities
                .executable_by_node
                .get(method.value.node_id.get().index())
                .copied()
                .flatten()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "class constructor owns its public field atoms",
                    span: Some(method.span),
                });
        }
        self.planned
            .identities
            .default_class_constructors
            .get(&class_node)
            .copied()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "class without a source constructor owns a synthesized field template",
                span: Some(class.span),
            })
    }

    fn class_definition_name(
        &self,
        node_id: NodeId,
        class: &super::Class<'arena>,
    ) -> Result<(CompilerString, Span), LeafCompilationError> {
        if let Some(identifier) = class.id.as_ref() {
            return Ok((
                compiler_identifier_string(identifier.name.as_str(), identifier.span)?,
                identifier.span,
            ));
        }
        let nodes = self.unit.semantic().nodes();
        let mut parent = nodes.parent_id(node_id);
        let declarator = loop {
            match nodes.kind(parent) {
                AstKind::ParenthesizedExpression(_) => parent = nodes.parent_id(parent),
                AstKind::VariableDeclarator(declarator) => break declarator,
                AstKind::PropertyDefinition(field) => {
                    return Self::class_property_definition_name(class, field);
                }
                AstKind::ObjectProperty(property) => {
                    return Self::class_object_property_name(class, property);
                }
                AstKind::AssignmentExpression(assignment)
                    if matches!(
                        assignment.operator,
                        super::AssignmentOperator::Assign
                            | super::AssignmentOperator::LogicalOr
                            | super::AssignmentOperator::LogicalAnd
                            | super::AssignmentOperator::LogicalNullish
                    ) =>
                {
                    return Self::direct_class_assignment_name(node_id, class, assignment);
                }
                AstKind::AssignmentPattern(assignment) => {
                    return Self::direct_class_binding_default_name(node_id, class, assignment);
                }
                AstKind::AssignmentTargetWithDefault(assignment) => {
                    return Self::direct_class_assignment_default_name(node_id, class, assignment);
                }
                _ => {
                    // The remaining expression contexts do not perform
                    // NamedEvaluation. Their anonymous class value retains
                    // the empty default name through `define_class`.
                    return Ok((compiler_identifier_string("", class.span)?, class.span));
                }
            }
        };
        let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return super::unsupported(
                super::UnsupportedLeafFeature::InferredFunctionName,
                class.span,
            );
        };
        let Some(mut initializer) = declarator.init.as_ref() else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class variable initializer remains a class expression",
                span: Some(class.span),
            });
        };
        while let Expression::ParenthesizedExpression(parenthesized) = initializer {
            initializer = &parenthesized.expression;
        }
        let Expression::ClassExpression(initializer) = initializer else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class variable initializer remains a class expression",
                span: Some(class.span),
            });
        };
        if initializer.node_id() != node_id {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class name is inferred from its direct initializer binding",
                span: Some(class.span),
            });
        }
        Ok((
            compiler_identifier_string(identifier.name.as_str(), identifier.span)?,
            identifier.span,
        ))
    }

    fn class_property_definition_name(
        class: &super::Class<'arena>,
        field: &super::PropertyDefinition<'arena>,
    ) -> Result<(CompilerString, Span), LeafCompilationError> {
        if field.computed {
            return Ok((
                compiler_identifier_string("<computed-class>", class.span)?,
                class.span,
            ));
        }
        let key =
            compiled_static_property_key(&field.key)?.ok_or(LeafCompilationError::Unsupported {
                feature: super::UnsupportedLeafFeature::InferredFunctionName,
                span: field.key.span(),
            })?;
        Ok((key.value, key.span))
    }

    fn class_object_property_name(
        class: &super::Class<'arena>,
        property: &super::ObjectProperty<'arena>,
    ) -> Result<(CompilerString, Span), LeafCompilationError> {
        if property.computed {
            return Ok((
                compiler_identifier_string("<computed-class>", class.span)?,
                class.span,
            ));
        }
        if property.shorthand || property.kind != PropertyKind::Init {
            return super::unsupported(
                super::UnsupportedLeafFeature::InferredFunctionName,
                class.span,
            );
        }
        let key = compiled_static_property_key(&property.key)?.ok_or(
            LeafCompilationError::Unsupported {
                feature: super::UnsupportedLeafFeature::InferredFunctionName,
                span: property.key.span(),
            },
        )?;
        Ok((key.value, key.span))
    }

    fn direct_class_assignment_name(
        node_id: NodeId,
        class: &super::Class<'arena>,
        assignment: &super::AssignmentExpression<'arena>,
    ) -> Result<(CompilerString, Span), LeafCompilationError> {
        let mut initializer = &assignment.right;
        while let Expression::ParenthesizedExpression(parenthesized) = initializer {
            initializer = &parenthesized.expression;
        }
        let Expression::ClassExpression(initializer) = initializer else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class assignment remains its direct right-hand expression",
                span: Some(class.span),
            });
        };
        if initializer.node_id() != node_id {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class name is inferred from its direct assignment target",
                span: Some(class.span),
            });
        }
        match &assignment.left {
            super::AssignmentTarget::AssignmentTargetIdentifier(identifier) => Ok((
                compiler_identifier_string(identifier.name.as_str(), identifier.span)?,
                identifier.span,
            )),
            // Assignment NamedEvaluation applies only to identifier references.
            // A static member assignment still creates the class through the
            // typed class-definition path, but its default name is the empty
            // string (rather than the member property name).
            super::AssignmentTarget::StaticMemberExpression(member) if !member.optional => {
                Ok((compiler_identifier_string("", class.span)?, class.span))
            }
            super::AssignmentTarget::ComputedMemberExpression(member) if !member.optional => {
                Ok((compiler_identifier_string("", class.span)?, class.span))
            }
            _ => super::unsupported(
                super::UnsupportedLeafFeature::InferredFunctionName,
                class.span,
            ),
        }
    }

    fn direct_class_binding_default_name(
        node_id: NodeId,
        class: &super::Class<'arena>,
        assignment: &super::AssignmentPattern<'arena>,
    ) -> Result<(CompilerString, Span), LeafCompilationError> {
        let BindingPattern::BindingIdentifier(identifier) = &assignment.left else {
            return super::unsupported(
                super::UnsupportedLeafFeature::InferredFunctionName,
                class.span,
            );
        };
        let mut initializer = &assignment.right;
        while let Expression::ParenthesizedExpression(parenthesized) = initializer {
            initializer = &parenthesized.expression;
        }
        let Expression::ClassExpression(initializer) = initializer else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class binding default remains its direct right-hand expression",
                span: Some(class.span),
            });
        };
        if initializer.node_id() != node_id {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class name is inferred from its direct binding default",
                span: Some(class.span),
            });
        }
        Ok((
            compiler_identifier_string(identifier.name.as_str(), identifier.span)?,
            identifier.span,
        ))
    }

    fn direct_class_assignment_default_name(
        node_id: NodeId,
        class: &super::Class<'arena>,
        assignment: &super::AssignmentTargetWithDefault<'arena>,
    ) -> Result<(CompilerString, Span), LeafCompilationError> {
        let super::AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.binding
        else {
            return super::unsupported(
                super::UnsupportedLeafFeature::InferredFunctionName,
                class.span,
            );
        };
        let mut initializer = &assignment.init;
        while let Expression::ParenthesizedExpression(parenthesized) = initializer {
            initializer = &parenthesized.expression;
        }
        let Expression::ClassExpression(initializer) = initializer else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class assignment default remains its direct right-hand expression",
                span: Some(class.span),
            });
        };
        if initializer.node_id() != node_id {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "anonymous class name is inferred from its direct assignment default",
                span: Some(class.span),
            });
        }
        Ok((
            compiler_identifier_string(identifier.name.as_str(), identifier.span)?,
            identifier.span,
        ))
    }

    fn record_regexp_literal_candidate(
        owner: ExecutableId,
        literal: &RegExpLiteral<'_>,
        candidates: &mut [Vec<CompiledConstantCandidate>],
        atom_candidates: &mut [Vec<CompiledAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        let pattern = literal.regex.pattern.text.as_str();
        let flags = literal.regex.flags.to_string();
        quickjs_regexp::CompiledRegExp::compile(
            pattern,
            &flags,
            quickjs_regexp::CompileLimits::default(),
        )
        .map_err(|source| LeafCompilationError::RegExp {
            span: literal.span,
            source,
        })?;
        let (pattern_span, flags_span) = regexp_component_spans(literal, flags.len())?;
        for (value, span) in [(pattern, pattern_span), (flags.as_str(), flags_span)] {
            record_string_candidate(
                owner,
                compiler_identifier_string(value, span)?,
                span,
                candidates,
                atom_candidates,
            )?;
        }
        Ok(())
    }

    fn record_array_property_candidates(
        owner: ExecutableId,
        array: &ArrayExpression<'arena>,
        atom_candidates: &mut [Vec<CompiledAtomCandidate>],
    ) -> Result<(), LeafCompilationError> {
        let first_spread = array
            .elements
            .iter()
            .position(ArrayExpressionElement::is_spread);
        let first_static_index = if first_spread.is_some() {
            ExpressionPlanner::spread_array_dense_prefix_len(array)
        } else {
            let Some(first_elision) = array
                .elements
                .iter()
                .position(ArrayExpressionElement::is_elision)
            else {
                return Ok(());
            };
            first_elision
        };
        let static_end = first_spread.unwrap_or(array.elements.len());
        for (index, element) in array
            .elements
            .iter()
            .enumerate()
            .skip(first_static_index)
            .take(static_end - first_static_index)
        {
            if element.is_elision() {
                continue;
            }
            let expression =
                element
                    .as_expression()
                    .ok_or(LeafCompilationError::SemanticInvariant {
                        invariant: "non-spread array element is an expression or elision",
                        span: Some(element.span()),
                    })?;
            let index =
                u32::try_from(index).map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "array literal element indices",
                })?;
            let span = expression.span();
            record_property_candidate_for(
                owner,
                compiler_identifier_string(&index.to_string(), span)?,
                span,
                CompiledPropertyAtomKey::ArrayIndex {
                    array: array.span,
                    index,
                },
                atom_candidates,
            )?;
        }
        let final_length_span = match first_spread {
            Some(_) => ExpressionPlanner::spread_array_final_length_span(array),
            None => array
                .elements
                .last()
                .filter(|element| element.is_elision())
                .map(GetSpan::span),
        };
        if let Some(final_length_span) = final_length_span {
            record_property_candidate_for(
                owner,
                compiler_identifier_string("length", final_length_span)?,
                final_length_span,
                CompiledPropertyAtomKey::ArrayLength { array: array.span },
                atom_candidates,
            )?;
        }
        Ok(())
    }
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

pub(in crate::lowering) struct CompiledConstantPool {
    atoms: Arc<[CompilerAtom]>,
    entries: Arc<[CompiledConstant]>,
    function_indices: Box<[(ExecutableId, u32)]>,
    number_indices: Box<[(Span, u32)]>,
    bigint_indices: Box<[(Span, u32)]>,
    template_object_indices: Box<[(Span, u32)]>,
    string_indices: Box<[(Span, CompiledStringLocation)]>,
    property_atom_indices: Box<[(CompiledPropertyAtomKey, u32)]>,
    metadata_atom_indices: Box<[(CompiledMetadataAtomKey, u32)]>,
}

pub(in crate::lowering) enum CompiledConstantCandidate {
    Number {
        value: Binary64Constant,
        span: Span,
    },
    BigInt {
        value: CompilerBigInt,
        span: Span,
    },
    TemplateObject {
        value: CompilerTemplateObject,
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
            Self::BigInt { span, .. } => (span.start, span.end, 1),
            Self::TemplateObject { span, .. } => (span.start, span.end, 2),
            Self::String { span, .. } => (span.start, span.end, 3),
            Self::Function { span, .. } => (span.start, span.end, 4),
        }
    }
}

pub(in crate::lowering) struct CompiledConstantPoolInput<'tree> {
    pub(in crate::lowering) children: &'tree [ExecutableId],
    pub(in crate::lowering) constant_candidates: Vec<CompiledConstantCandidate>,
    pub(in crate::lowering) atom_candidates: Vec<CompiledAtomCandidate>,
    pub(in crate::lowering) metadata_atom_candidates: Vec<CompiledMetadataAtomCandidate>,
}

#[derive(Clone, Copy)]
pub(in crate::lowering) enum CompiledStringLocation {
    Constant(u32),
    Atom(u32),
}

struct FrozenConstantCandidates {
    entries: Vec<CompiledConstant>,
    function_indices: Vec<(ExecutableId, u32)>,
    number_indices: Vec<(Span, u32)>,
    bigint_indices: Vec<(Span, u32)>,
    template_object_indices: Vec<(Span, u32)>,
    string_indices: Vec<(Span, CompiledStringLocation)>,
    property_atom_indices: Vec<(CompiledPropertyAtomKey, u32)>,
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
        bigint_indices: Vec::new(),
        template_object_indices: Vec::new(),
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
            CompiledConstantCandidate::BigInt { value, span } => {
                frozen
                    .entries
                    .push(CompiledConstant::Value(CompilerConstantValue::BigInt(
                        value,
                    )));
                frozen.bigint_indices.push((span, index));
            }
            CompiledConstantCandidate::TemplateObject { value, span } => {
                frozen.entries.push(CompiledConstant::Value(
                    CompilerConstantValue::TemplateObject(value),
                ));
                frozen.template_object_indices.push((span, index));
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
        .sort_unstable_by_key(|(key, _)| key.order_key());
    if let Some(key) = frozen
        .property_atom_indices
        .windows(2)
        .find_map(|pair| (pair[0].0 == pair[1].0).then_some(pair[0].0))
    {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "static property lookup keys are unique within a function",
            span: Some(key.span()),
        });
    }
    Ok(())
}

impl CompiledConstantPool {
    pub(in crate::lowering) fn new(
        input: CompiledConstantPoolInput<'_>,
    ) -> Result<Self, LeafCompilationError> {
        let CompiledConstantPoolInput {
            children,
            mut constant_candidates,
            mut atom_candidates,
            mut metadata_atom_candidates,
        } = input;
        constant_candidates.sort_unstable_by_key(CompiledConstantCandidate::order_key);
        atom_candidates.sort_unstable_by_key(CompiledAtomCandidate::order_key);
        metadata_atom_candidates.sort_unstable_by_key(CompiledMetadataAtomCandidate::order_key);
        let candidates = constant_candidates;
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
            bigint_indices: frozen.bigint_indices.into_boxed_slice(),
            template_object_indices: frozen.template_object_indices.into_boxed_slice(),
            string_indices: frozen.string_indices.into_boxed_slice(),
            property_atom_indices: frozen.property_atom_indices.into_boxed_slice(),
            metadata_atom_indices: metadata_atom_indices.into_boxed_slice(),
        })
    }

    pub(in crate::lowering) fn atoms(&self) -> &Arc<[CompilerAtom]> {
        &self.atoms
    }

    pub(in crate::lowering) fn entries(&self) -> &Arc<[CompiledConstant]> {
        &self.entries
    }

    pub(in crate::lowering) fn metadata_atom_index(
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

    pub(in crate::lowering) fn plan_number(
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

    pub(in crate::lowering) fn plan_bigint(
        &self,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let position = self
            .bigint_indices
            .binary_search_by_key(&(span.start, span.end), |(candidate, _)| {
                (candidate.start, candidate.end)
            })
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "large BigInt literal has one constant-pool entry",
                span: Some(span),
            })?;
        let index = self.bigint_indices[position].1;
        let Some(CompiledConstant::Value(CompilerConstantValue::BigInt(_))) =
            usize::try_from(index)
                .ok()
                .and_then(|index| self.entries.get(index))
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "BigInt constant index resolves to its exact decimal payload",
                span: Some(span),
            });
        };
        let (opcode, operands) = match u8::try_from(index) {
            Ok(index) => (FinalOpcode::PushConst8, Operands::Const8(index)),
            Err(_) => (FinalOpcode::PushConst, Operands::Const(index)),
        };
        Ok(PlannedInstruction::new(opcode, operands, span))
    }

    pub(in crate::lowering) fn plan_template_object(
        &self,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
        let position = self
            .template_object_indices
            .binary_search_by_key(&(span.start, span.end), |(candidate, _)| {
                (candidate.start, candidate.end)
            })
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "tagged template has one site-object constant",
                span: Some(span),
            })?;
        let index = self.template_object_indices[position].1;
        let Some(CompiledConstant::Value(CompilerConstantValue::TemplateObject(_))) =
            usize::try_from(index)
                .ok()
                .and_then(|index| self.entries.get(index))
        else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "tagged-template constant index resolves to its exact site payload",
                span: Some(span),
            });
        };
        let (opcode, operands) = match u8::try_from(index) {
            Ok(index) => (FinalOpcode::PushConst8, Operands::Const8(index)),
            Err(_) => (FinalOpcode::PushConst, Operands::Const(index)),
        };
        Ok(PlannedInstruction::new(opcode, operands, span))
    }

    pub(in crate::lowering) fn plan_string(
        &self,
        span: Span,
    ) -> Result<PlannedInstruction, LeafCompilationError> {
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

    pub(in crate::lowering) fn plan_regexp_literal(
        &self,
        literal: &RegExpLiteral<'_>,
    ) -> Result<[PlannedInstruction; 3], LeafCompilationError> {
        let flags = literal.regex.flags.to_string();
        let (pattern_span, flags_span) = regexp_component_spans(literal, flags.len())?;
        let pattern = if literal.regex.pattern.text.is_empty() {
            PlannedInstruction::new(FinalOpcode::PushEmptyString, Operands::None, pattern_span)
        } else {
            self.plan_string(pattern_span)?
        };
        let flags = if flags.is_empty() {
            PlannedInstruction::new(FinalOpcode::PushEmptyString, Operands::None, flags_span)
        } else {
            self.plan_string(flags_span)?
        };
        Ok([
            pattern,
            flags,
            PlannedInstruction::new(FinalOpcode::RegExp, Operands::None, literal.span),
        ])
    }

    pub(in crate::lowering) fn property_atom_index(
        &self,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(CompiledPropertyAtomKey::Source(span), span)
    }

    pub(in crate::lowering) fn array_index_atom_index(
        &self,
        array: Span,
        index: u32,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(CompiledPropertyAtomKey::ArrayIndex { array, index }, span)
    }

    pub(in crate::lowering) fn array_length_atom_index(
        &self,
        array: Span,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(CompiledPropertyAtomKey::ArrayLength { array }, span)
    }

    pub(in crate::lowering) fn class_heritage_prototype_atom_index(
        &self,
        class: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(
            CompiledPropertyAtomKey::ClassHeritagePrototype { class },
            class,
        )
    }

    pub(in crate::lowering) fn yield_star_done_atom_index(
        &self,
        expression: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(
            CompiledPropertyAtomKey::YieldStarDone { expression },
            expression,
        )
    }

    pub(in crate::lowering) fn yield_star_value_atom_index(
        &self,
        expression: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        self.property_atom_index_for(
            CompiledPropertyAtomKey::YieldStarValue { expression },
            expression,
        )
    }

    fn property_atom_index_for(
        &self,
        key: CompiledPropertyAtomKey,
        span: Span,
    ) -> Result<AtomPoolIndex, LeafCompilationError> {
        let position = self
            .property_atom_indices
            .binary_search_by_key(&key.order_key(), |(candidate, _)| candidate.order_key())
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "static property has one function-local atom",
                span: Some(span),
            })?;
        Ok(AtomPoolIndex::new(self.property_atom_indices[position].1))
    }

    pub(in crate::lowering) fn function_index(
        &self,
        executable: ExecutableId,
    ) -> Result<u32, LeafCompilationError> {
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

fn regexp_component_spans(
    literal: &RegExpLiteral<'_>,
    flags_len: usize,
) -> Result<(Span, Span), LeafCompilationError> {
    let pattern_len = u32::try_from(literal.regex.pattern.text.len()).map_err(|_| {
        LeafCompilationError::CapacityExceeded {
            domain: "RegExp literal source span",
        }
    })?;
    let flags_len =
        u32::try_from(flags_len).map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "RegExp literal flags span",
        })?;
    let pattern_start =
        literal
            .span
            .start
            .checked_add(1)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "RegExp literal source span",
            })?;
    let pattern_end =
        pattern_start
            .checked_add(pattern_len)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "RegExp literal source span",
            })?;
    let flags_start = pattern_end
        .checked_add(1)
        .ok_or(LeafCompilationError::CapacityExceeded {
            domain: "RegExp literal flags span",
        })?;
    let flags_end =
        flags_start
            .checked_add(flags_len)
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "RegExp literal flags span",
            })?;
    if flags_end != literal.span.end {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "RegExp literal body and flags partition its source span",
            span: Some(literal.span),
        });
    }
    Ok((
        Span::new(pattern_start, pattern_end),
        Span::new(flags_start, flags_end),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quickjs_bytecode::{Binary64Constant, CompilerConstantValue, CompilerString};
    use quickjs_frontend::{
        CompilationGoal, FrontendOptions, GlobalScriptGoal, Span, with_parsed_program,
    };

    use crate::lowering::atoms::{CompiledAtomCandidate, CompiledAtomPurpose};
    use crate::lowering::{CompilationContext, CompiledConstant, LeafCompilationError};

    use super::{CompiledConstantCandidate, CompiledConstantPool, CompiledConstantPoolInput};

    fn string(code_units: &[u16]) -> CompilerString {
        CompilerString::try_from_code_units(Arc::from(code_units)).expect("compiler string")
    }

    #[test]
    fn constructor_freezes_source_order_exact_number_bits_and_utf16_atoms() {
        let tagged = string(&[b'1'.into(), b'2'.into(), b'3'.into()]);
        let wide = string(&[0xd800, b'a'.into()]);
        let pool = CompiledConstantPool::new(CompiledConstantPoolInput {
            children: &[],
            constant_candidates: vec![
                CompiledConstantCandidate::Number {
                    value: Binary64Constant::from_bits(0x8000_0000_0000_0000),
                    span: Span::new(20, 22),
                },
                CompiledConstantCandidate::String {
                    value: tagged.clone(),
                    span: Span::new(2, 5),
                },
            ],
            atom_candidates: vec![CompiledAtomCandidate {
                value: wide.clone(),
                span: Span::new(10, 14),
                purpose: CompiledAtomPurpose::RuntimeString,
                property_key: None,
            }],
            metadata_atom_candidates: Vec::new(),
        })
        .expect("frozen pool");

        assert_eq!(
            pool.entries().as_ref(),
            [
                CompiledConstant::Value(CompilerConstantValue::String(tagged)),
                CompiledConstant::Value(CompilerConstantValue::Number(
                    Binary64Constant::from_bits(0x8000_0000_0000_0000),
                )),
            ]
        );
        assert_eq!(
            pool.atoms()[0].string().code_units().collect::<Vec<_>>(),
            [0xd800, u16::from(b'a')]
        );
    }

    #[test]
    fn constructor_rejects_a_function_candidate_outside_the_child_domain() {
        with_parsed_program(
            "function child(){}",
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let child = context
                    .storage_plan()
                    .executables()
                    .iter()
                    .find(|executable| executable.name() == Some("child"))
                    .expect("child executable")
                    .id();
                let result = CompiledConstantPool::new(CompiledConstantPoolInput {
                    children: &[],
                    constant_candidates: vec![CompiledConstantCandidate::Function {
                        executable: child,
                        span: Span::new(4, 8),
                    }],
                    atom_candidates: Vec::new(),
                    metadata_atom_candidates: Vec::new(),
                });
                let Err(error) = result else {
                    panic!("foreign function candidate must be rejected");
                };
                assert!(matches!(
                    error,
                    LeafCompilationError::SemanticInvariant {
                        invariant: "constant-pool function is a direct child",
                        span: Some(span),
                    } if span == Span::new(4, 8)
                ));
            },
        )
        .expect("front-end acceptance");
    }
}
