fn verify_source(
    id: FunctionTemplateId,
    flow: &VerifiedControlFlow,
    metadata: &UnverifiedFunctionMetadata,
) -> Result<(), BytecodeVerificationError> {
    let source = &metadata.source;
    if source.display_name.is_empty() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::EmptySourceDisplayName,
        ));
    }
    validate_source_span(id, &source.text, source.function_span)?;
    if metadata.function_name.is_some() != source.name_span.is_some() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::FunctionNameSourceMismatch,
        ));
    }
    if let Some(name_span) = source.name_span {
        validate_source_span(id, &source.text, name_span)?;
        if !contains(source.function_span, name_span) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::FunctionNameOutsideFunction,
            ));
        }
    }
    if source.mappings.len() != flow.instructions().len() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::SourceMappingCountMismatch {
                instructions: usize_to_u64(flow.instructions().len()),
                mappings: usize_to_u64(source.mappings.len()),
            },
        ));
    }
    let strict_mode_pcs = source.strict_mode_pcs.as_deref().unwrap_or_default();
    if strict_mode_pcs.len() > flow.instructions().len() {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::StrictModeInstructionCountOutOfBounds {
                strict_instructions: usize_to_u64(strict_mode_pcs.len()),
                instructions: usize_to_u64(flow.instructions().len()),
            },
        ));
    }
    for (index, window) in strict_mode_pcs.windows(2).enumerate() {
        let [previous, current] = window else {
            unreachable!("two-entry strict-source window")
        };
        if previous >= current {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::StrictModePcNotIncreasing {
                    index: usize_to_u32(index + 1),
                    previous: *previous,
                    current: *current,
                },
            ));
        }
    }
    for (index, pc) in strict_mode_pcs.iter().copied().enumerate() {
        if !flow.is_instruction_start(pc) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::StrictModePcNotInstruction {
                    index: usize_to_u32(index),
                    pc,
                },
            ));
        }
    }
    for (index, (mapping, instruction)) in
        source.mappings.iter().zip(flow.instructions()).enumerate()
    {
        let actual = instruction.decoded().pc();
        if mapping.pc != actual {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::SourcePcMismatch {
                    mapping: usize_to_u32(index),
                    declared: mapping.pc,
                    actual,
                },
            ));
        }
        validate_source_span(id, &source.text, mapping.span)?;
        if !contains(source.function_span, mapping.span) {
            return Err(BytecodeVerificationError::function(
                id,
                BytecodeVerificationErrorKind::InstructionSourceOutsideFunction {
                    mapping: usize_to_u32(index),
                },
            ));
        }
    }
    Ok(())
}
fn validate_source_span(
    id: FunctionTemplateId,
    text: &str,
    span: SourceByteSpan,
) -> Result<(), BytecodeVerificationError> {
    let start = span.start as usize;
    let end = span.end as usize;
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return Err(BytecodeVerificationError::function(
            id,
            BytecodeVerificationErrorKind::InvalidSourceSpan { span },
        ));
    }
    Ok(())
}

const fn contains(outer: SourceByteSpan, inner: SourceByteSpan) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

#[allow(
    clippy::too_many_lines,
    reason = "parent-child variable, global initializer, and forwarded closure metadata are verified together"
)]
fn verify_closure_metadata(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
) -> Result<(), BytecodeVerificationError> {
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let parent_metadata = &metadata[parent_index];
        for (definition_index, definition) in parent_metadata.variables.iter().enumerate() {
            let Some(constant_index) = definition.function_initializer else {
                continue;
            };
            let Some(crate::CompilerConstant::Function(child_id)) =
                parent.constants().get(constant_index as usize)
            else {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: usize_to_u32(definition_index),
                        constant: Some(constant_index),
                    },
                ));
            };
            let child_index = usize::try_from(child_id.get()).ok();
            let child = child_index.and_then(|index| graph.functions().get(index));
            let child_metadata = child_index.and_then(|index| metadata.get(index));
            let names_match = child
                .zip(child_metadata)
                .is_some_and(|(child, child_metadata)| {
                    let closure_name = atom_contents(definition.name, parent.atoms());
                    let function_name = atom_contents(child_metadata.function_name, child.atoms());
                    closure_name == function_name
                        || (closure_name
                            .is_some_and(|name| name.code_units().eq("*default*".encode_utf16()))
                            && function_name
                                .is_some_and(|name| name.code_units().eq("default".encode_utf16())))
                });
            if !names_match {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::FunctionInitializerMetadataMismatch {
                        definition: usize_to_u32(definition_index),
                        constant: Some(constant_index),
                    },
                ));
            }
        }
        for (closure_index, definition) in parent_metadata.closures.iter().enumerate() {
            let Some(constant_index) = definition.function_initializer else {
                continue;
            };
            let Some(crate::CompilerConstant::Function(child_id)) =
                parent.constants().get(constant_index as usize)
            else {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                        closure: usize_to_u32(closure_index),
                        constant: Some(constant_index),
                    },
                ));
            };
            let child_index = usize::try_from(child_id.get()).ok();
            let child = child_index.and_then(|index| graph.functions().get(index));
            let child_metadata = child_index.and_then(|index| metadata.get(index));
            let names_match = child
                .zip(child_metadata)
                .is_some_and(|(child, child_metadata)| {
                    let closure_name = atom_contents(definition.name, parent.atoms());
                    let function_name = atom_contents(child_metadata.function_name, child.atoms());
                    closure_name == function_name
                        || (closure_name
                            .is_some_and(|name| name.code_units().eq("*default*".encode_utf16()))
                            && function_name
                                .is_some_and(|name| name.code_units().eq("default".encode_utf16())))
                });
            if !names_match {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::RealmGlobalFunctionInitializerMetadataMismatch {
                        closure: usize_to_u32(closure_index),
                        constant: Some(constant_index),
                    },
                ));
            }
        }
        for constant in parent.constants() {
            let crate::CompilerConstant::Function(child_id) = constant else {
                continue;
            };
            let child_index = usize::try_from(child_id.get()).map_err(|_| {
                BytecodeVerificationError::function(
                    *child_id,
                    BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                        child: *child_id,
                        closure: 0,
                    },
                )
            })?;
            let child = graph.function(*child_id).ok_or_else(|| {
                BytecodeVerificationError::function(
                    *child_id,
                    BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                        child: *child_id,
                        closure: 0,
                    },
                )
            })?;
            let child_metadata = &metadata[child_index];
            for (closure_index, (closure, source)) in child_metadata
                .closures
                .iter()
                .zip(child.closure_sources())
                .enumerate()
            {
                let expected = match *source {
                    CompilerClosureSource::ParentVariableReference(reference) => {
                        parent_definition_for_reference(parent, parent_metadata, reference)
                    }
                    CompilerClosureSource::ParentClosure(index) => usize::try_from(index)
                        .ok()
                        .and_then(|index| parent_metadata.closures.get(index))
                        .map(|definition| ParentClosureDefinition {
                            name: definition.name,
                            binding: definition.binding,
                            arguments_object: definition.arguments_object,
                            deletable_eval_variable: definition.deletable_eval_variable,
                            atoms: parent.atoms(),
                        }),
                    CompilerClosureSource::ConstructorRealmGlobal(_)
                    | CompilerClosureSource::DirectEvalBinding { .. }
                    | CompilerClosureSource::DirectEvalVariable { .. }
                    | CompilerClosureSource::Module { .. } => None,
                };
                let matches = expected.is_some_and(|expected| {
                    expected.binding == closure.binding
                        && expected.arguments_object == closure.arguments_object
                        && expected.deletable_eval_variable == closure.deletable_eval_variable
                        && atom_contents(expected.name, expected.atoms)
                            == atom_contents(closure.name, child.atoms())
                });
                if !matches {
                    return Err(BytecodeVerificationError::function(
                        *child_id,
                        BytecodeVerificationErrorKind::ClosureMetadataMismatch {
                            child: *child_id,
                            closure: usize_to_u32(closure_index),
                        },
                    ));
                }
            }
        }
    }
    Ok(())
}
