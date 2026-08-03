use std::sync::Arc;

use quickjs_bytecode::{
    CompilerClosureSource as CompilerGraphClosureSource, CompilerConstant as CompilerGraphConstant,
    FunctionGraphVerificationLimits, FunctionTemplateId, UnverifiedCompilerFunction,
    UnverifiedCompilerFunctionGraph, VerifiedCompilerFunctionGraph, verify_compiler_function_graph,
};

use crate::storage::{Executable, ExecutableId};

use super::{
    CompiledClosureSource, CompiledConstant, CompiledFunction, CompiledRealmGlobalSource,
    LeafCompilationError,
};

pub(in crate::lowering) fn verify_compiled_function_graph(
    root: ExecutableId,
    functions: &[CompiledFunction],
    limits: FunctionGraphVerificationLimits,
) -> Result<VerifiedCompilerFunctionGraph, LeafCompilationError> {
    if functions.first().map(CompiledFunction::executable) != Some(root) {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "compiled subtree begins with its selected root",
            span: None,
        });
    }
    let mut identities = Vec::with_capacity(functions.len());
    for (index, function) in functions.iter().enumerate() {
        if identities
            .last()
            .is_some_and(|(previous, _)| *previous >= function.executable())
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled subtree executables are strictly ordered",
                span: None,
            });
        }
        identities.push((function.executable(), checked_function_template_id(index)?));
    }
    let root = resolve_function_template_id(&identities, root)?;
    let root_index =
        usize::try_from(root.get()).map_err(|_| LeafCompilationError::SemanticInvariant {
            invariant: "graph-local root identity fits usize",
            span: None,
        })?;

    let (records, parent_counts) = build_unverified_graph_records(functions, &identities)?;
    for (index, &actual) in parent_counts.iter().enumerate() {
        let expected = u32::from(index != root_index);
        if actual != expected {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "compiled function subtree has exactly one parent per child",
                span: None,
            });
        }
    }

    verify_compiler_function_graph(
        UnverifiedCompilerFunctionGraph::new(root, records.into()),
        limits,
    )
    .map_err(|source| {
        let span = source
            .function()
            .and_then(|template| usize::try_from(template.get()).ok())
            .and_then(|index| functions.get(index))
            .and_then(|function| {
                function
                    .storage_plan
                    .executable(function.executable)
                    .map(Executable::span)
            });
        LeafCompilationError::FunctionGraphVerification { span, source }
    })
}

fn build_unverified_graph_records(
    functions: &[CompiledFunction],
    identities: &[(ExecutableId, FunctionTemplateId)],
) -> Result<(Vec<UnverifiedCompilerFunction>, Vec<u32>), LeafCompilationError> {
    let mut records = Vec::with_capacity(functions.len());
    let mut parent_counts = vec![0_u32; functions.len()];
    for function in functions {
        let mut constants = Vec::with_capacity(function.constants.len());
        for constant in function.constants.iter() {
            match constant {
                CompiledConstant::Value(value) => {
                    constants.push(CompilerGraphConstant::Value(value.clone()));
                }
                CompiledConstant::Function(function_constant) => {
                    let template =
                        resolve_function_template_id(identities, function_constant.executable())?;
                    let template_index = usize::try_from(template.get()).map_err(|_| {
                        LeafCompilationError::SemanticInvariant {
                            invariant: "graph-local template identity fits usize",
                            span: None,
                        }
                    })?;
                    let count = parent_counts.get_mut(template_index).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant: "function constant has a graph parent-count slot",
                            span: None,
                        },
                    )?;
                    *count =
                        count
                            .checked_add(1)
                            .ok_or(LeafCompilationError::CapacityExceeded {
                                domain: "compiler function parent edges",
                            })?;
                    constants.push(CompilerGraphConstant::Function(template));
                }
            }
        }
        let closure_capacity = function
            .closure_variables
            .len()
            .checked_add(function.realm_globals.len())
            .ok_or(LeafCompilationError::CapacityExceeded {
                domain: "compiler graph closure sources",
            })?;
        let mut closure_sources = Vec::with_capacity(closure_capacity);
        for (index, closure) in function.closure_variables.iter().copied().enumerate() {
            if closure.slot().index() != index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled closure slots are dense and ordered",
                    span: None,
                });
            }
            closure_sources.push(match closure.source() {
                CompiledClosureSource::ParentVariableReference(index) => {
                    CompilerGraphClosureSource::ParentVariableReference(u32::from(index))
                }
                CompiledClosureSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            });
        }
        for (offset, global) in function.realm_globals.iter().enumerate() {
            let expected = function.closure_variables.len().checked_add(offset).ok_or(
                LeafCompilationError::CapacityExceeded {
                    domain: "compiler graph closure sources",
                },
            )?;
            if usize::from(global.slot()) != expected {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "compiled realm-global slots follow captured closure slots",
                    span: None,
                });
            }
            closure_sources.push(match global.source() {
                CompiledRealmGlobalSource::ConstructorRealm => {
                    CompilerGraphClosureSource::ConstructorRealmGlobal(global.atom())
                }
                CompiledRealmGlobalSource::ParentClosure(index) => {
                    CompilerGraphClosureSource::ParentClosure(u32::from(index))
                }
            });
        }
        records.push(
            UnverifiedCompilerFunction::new(
                Arc::clone(&function.control_flow),
                constants.into(),
                closure_sources.into(),
            )
            .with_atom_pool(Arc::clone(&function.atoms)),
        );
    }
    Ok((records, parent_counts))
}

fn checked_function_template_id(index: usize) -> Result<FunctionTemplateId, LeafCompilationError> {
    u32::try_from(index)
        .map(FunctionTemplateId::new)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "compiler function graph templates",
        })
}

fn resolve_function_template_id(
    identities: &[(ExecutableId, FunctionTemplateId)],
    executable: ExecutableId,
) -> Result<FunctionTemplateId, LeafCompilationError> {
    identities
        .binary_search_by_key(&executable, |(candidate, _)| *candidate)
        .ok()
        .and_then(|index| identities.get(index))
        .map(|(_, template)| *template)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "function constant belongs to the compiled subtree",
            span: None,
        })
}
