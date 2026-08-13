use std::collections::HashMap;

use crate::function::VerifiedFunctionHeader;

use super::{
    CompilerCaptureLayout, CompilerCapturedBinding, CompilerCapturedBindingIdentity,
    CompilerConstantLayout, FunctionIndexDomains, VerificationError, VerificationErrorKind,
    VerificationResource, usize_to_u64,
};

pub(super) fn validate_compiler_constant_layout(
    layout: CompilerConstantLayout,
    domains: FunctionIndexDomains,
) -> Result<CompilerConstantLayout, VerificationError> {
    let entries = usize_to_u64(layout.kinds.len());
    if entries != u64::from(domains.constant_pool_len) {
        return Err(VerificationError::root(
            VerificationErrorKind::CompilerConstantCountMismatch {
                declared: domains.constant_pool_len,
                entries,
            },
        ));
    }
    Ok(layout)
}

#[derive(Debug)]
pub(super) struct ValidatedCompilerCaptureLayout {
    pub(super) layout: CompilerCaptureLayout,
    bindings_by_identity: HashMap<CompilerCapturedBindingIdentity, CompilerCapturedBinding>,
}

impl ValidatedCompilerCaptureLayout {
    pub(super) fn is_scoped_local(&self, local: u32) -> bool {
        matches!(
            self.bindings_by_identity
                .get(&CompilerCapturedBindingIdentity::Local(local)),
            Some(CompilerCapturedBinding::ScopedLocal(_))
        )
    }
}

pub(super) fn validate_compiler_capture_layout(
    layout: CompilerCaptureLayout,
    domains: FunctionIndexDomains,
    function_header: &VerifiedFunctionHeader,
) -> Result<ValidatedCompilerCaptureLayout, VerificationError> {
    let capture_count = usize_to_u64(layout.bindings.len());
    if capture_count != u64::from(function_header.variable_reference_count()) {
        return Err(VerificationError::root(
            VerificationErrorKind::CompilerCaptureCountMismatch {
                variable_references: function_header.variable_reference_count(),
                captures: capture_count,
            },
        ));
    }

    for &binding in layout.bindings.iter() {
        let (index, len) = match binding {
            CompilerCapturedBinding::Argument(index) => (index, domains.argument_count),
            CompilerCapturedBinding::FunctionLocal(index)
            | CompilerCapturedBinding::ScopedLocal(index) => (index, domains.local_count),
        };
        if index >= len {
            return Err(VerificationError::root(
                VerificationErrorKind::CompilerCaptureIndexOutOfBounds { binding, len },
            ));
        }
    }

    if let Some(mapped_arguments) = &layout.mapped_arguments {
        let mut previous = None;
        for &index in mapped_arguments.iter() {
            if index >= domains.argument_count {
                return Err(VerificationError::root(
                    VerificationErrorKind::CompilerMappedArgumentIndexOutOfBounds {
                        index,
                        len: domains.argument_count,
                    },
                ));
            }
            if let Some(previous) = previous
                && index <= previous
            {
                return Err(VerificationError::root(
                    VerificationErrorKind::CompilerMappedArgumentsNotAscending { previous, index },
                ));
            }
            previous = Some(index);
        }
    }

    let mut bindings_by_identity = HashMap::new();
    bindings_by_identity
        .try_reserve(layout.bindings.len())
        .map_err(|_| {
            VerificationError::root(VerificationErrorKind::AllocationFailed {
                resource: VerificationResource::CompilerCaptures,
                requested: capture_count,
            })
        })?;
    for &binding in layout.bindings.iter() {
        if bindings_by_identity
            .insert(binding.identity(), binding)
            .is_some()
        {
            return Err(VerificationError::root(
                VerificationErrorKind::DuplicateCompilerCapture { binding },
            ));
        }
    }

    Ok(ValidatedCompilerCaptureLayout {
        layout,
        bindings_by_identity,
    })
}
