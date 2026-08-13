use crate::function::{
    FunctionBitField, FunctionHeaderFlag, FunctionHeaderFlags, FunctionKind,
    FunctionKindRequirement, FunctionMode, JS_MODE_MASK, SERIALIZED_FUNCTION_FLAGS_MASK,
    UnverifiedFunctionHeader, VerifiedFunctionHeader,
};

use super::{
    FunctionCountDomain, FunctionIndexDomains, MAX_FUNCTION_INDEX_ENTRIES, MAX_OPERAND_STACK_DEPTH,
    VerificationError, VerificationErrorKind, VerificationLimits, VerificationResource,
    usize_to_u64,
};

pub(super) fn validate_limits_and_counts(
    bytecode_len: usize,
    domains: FunctionIndexDomains,
    function_header: UnverifiedFunctionHeader,
    expected_stack_size: Option<u32>,
    limits: VerificationLimits,
) -> Result<(), VerificationError> {
    if limits.max_stack_depth > MAX_OPERAND_STACK_DEPTH {
        return Err(VerificationError::root(
            VerificationErrorKind::InvalidStackLimit {
                value: limits.max_stack_depth,
                maximum: MAX_OPERAND_STACK_DEPTH,
            },
        ));
    }

    check_limit(
        VerificationResource::BytecodeBytes,
        usize_to_u64(bytecode_len),
        u64::from(limits.max_bytecode_bytes_per_function),
    )?;
    check_limit(
        VerificationResource::Constants,
        u64::from(domains.constant_pool_len),
        u64::from(limits.max_constants_per_function),
    )?;
    check_limit(
        VerificationResource::AtomPoolEntries,
        u64::from(domains.atom_pool_len),
        u64::from(limits.max_atom_pool_entries),
    )?;

    check_structural_count(FunctionCountDomain::Arguments, domains.argument_count)?;
    check_structural_count(FunctionCountDomain::Locals, domains.local_count)?;
    check_structural_count(
        FunctionCountDomain::VariableReferences,
        function_header.variable_reference_count(),
    )?;
    check_structural_count(
        FunctionCountDomain::ClosureVariables,
        domains.closure_var_count,
    )?;
    if let Some(expected_stack_size) = expected_stack_size {
        check_structural_count(FunctionCountDomain::ExpectedStackSize, expected_stack_size)?;
        check_limit(
            VerificationResource::StackDepth,
            u64::from(expected_stack_size),
            u64::from(limits.max_stack_depth),
        )?;
    }
    Ok(())
}

pub(super) fn validate_function_header(
    header: UnverifiedFunctionHeader,
    domains: FunctionIndexDomains,
) -> Result<VerifiedFunctionHeader, VerificationError> {
    let serialized_flags = header.serialized_flags();
    let unknown_flags = serialized_flags & !SERIALIZED_FUNCTION_FLAGS_MASK;
    if unknown_flags != 0 {
        return Err(VerificationError::root(
            VerificationErrorKind::DisallowedFunctionBits {
                field: FunctionBitField::SerializedFlags,
                value: serialized_flags,
                allowed_mask: SERIALIZED_FUNCTION_FLAGS_MASK,
                disallowed_bits: unknown_flags,
            },
        ));
    }

    let js_mode = header.js_mode();
    let unknown_mode = js_mode & !JS_MODE_MASK;
    if unknown_mode != 0 {
        return Err(VerificationError::root(
            VerificationErrorKind::DisallowedFunctionBits {
                field: FunctionBitField::JsMode,
                value: u16::from(js_mode),
                allowed_mask: u16::from(JS_MODE_MASK),
                disallowed_bits: u16::from(unknown_mode),
            },
        ));
    }

    let defined_argument_count = header.defined_argument_count();
    if defined_argument_count > domains.argument_count {
        return Err(VerificationError::root(
            VerificationErrorKind::DefinedArgumentCountOutOfRange {
                defined: defined_argument_count,
                argument_count: domains.argument_count,
            },
        ));
    }

    let variable_reference_count = header.variable_reference_count();
    let available_bindings = u64::from(domains.argument_count) + u64::from(domains.local_count);
    if u64::from(variable_reference_count) > available_bindings {
        return Err(VerificationError::root(
            VerificationErrorKind::VariableReferenceCountOutOfRange {
                variable_references: variable_reference_count,
                argument_count: domains.argument_count,
                local_count: domains.local_count,
            },
        ));
    }

    let flags = FunctionHeaderFlags::from_validated_bits(serialized_flags);
    let kind = FunctionKind::from_serialized_flags(serialized_flags);
    if flags.has_prototype() && flags.is_derived_class_constructor() {
        return Err(VerificationError::root(
            VerificationErrorKind::ConflictingFunctionFlags {
                first: FunctionHeaderFlag::HasPrototype,
                second: FunctionHeaderFlag::DerivedClassConstructor,
            },
        ));
    }
    if !matches!(kind, FunctionKind::Normal) {
        let invalid_flag = if flags.has_prototype() {
            Some(FunctionHeaderFlag::HasPrototype)
        } else if flags.is_derived_class_constructor() {
            Some(FunctionHeaderFlag::DerivedClassConstructor)
        } else {
            None
        };
        if let Some(flag) = invalid_flag {
            return Err(VerificationError::root(
                VerificationErrorKind::FunctionFlagNotAllowedForKind {
                    flag,
                    kind,
                    requirement: FunctionKindRequirement::Normal,
                },
            ));
        }
    }

    Ok(VerifiedFunctionHeader::new(
        flags,
        FunctionMode::from_validated_bits(js_mode),
        kind,
        defined_argument_count,
        variable_reference_count,
    ))
}

fn check_structural_count(
    domain: FunctionCountDomain,
    value: u32,
) -> Result<(), VerificationError> {
    if value > MAX_FUNCTION_INDEX_ENTRIES {
        return Err(VerificationError::root(
            VerificationErrorKind::MetadataCountOutOfRange {
                domain,
                value,
                maximum: MAX_FUNCTION_INDEX_ENTRIES,
            },
        ));
    }
    Ok(())
}

fn check_limit(
    resource: VerificationResource,
    observed: u64,
    limit: u64,
) -> Result<(), VerificationError> {
    if observed > limit {
        return Err(VerificationError::root(
            VerificationErrorKind::LimitExceeded {
                resource,
                limit,
                observed,
            },
        ));
    }
    Ok(())
}
