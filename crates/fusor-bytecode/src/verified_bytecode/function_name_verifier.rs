/// Certifies the compiler-owned named-evaluation primitives. `set_name` and
/// `set_name_computed` may rename only a fresh anonymous ordinary closure, or
/// a fresh base class immediately after a converted computed key and its typed
/// definition. Every form has exactly one effective incoming edge. This
/// prevents arbitrary function objects, methods, named templates, or
/// control-flow joins from acquiring the intrinsic mutation authority.
fn verify_inferred_function_names(
    graph: &VerifiedCompilerFunctionGraph,
    metadata: &[VerifiedFunctionMetadata],
) -> Result<(), BytecodeVerificationError> {
    for (parent_index, parent) in graph.functions().iter().enumerate() {
        let parent_id = function_id(parent_index)?;
        let instructions = parent.control_flow().instructions();
        let internal_stack = &metadata[parent_index].internal_stack;
        let mut predecessor_counts = try_filled_vec(
            parent_id,
            instructions.len(),
            0_u32,
            BytecodeGraphResource::SourceMappings,
        )?;
        for index in 0..instructions.len() {
            for edge in internal_stack.effective_successors(instructions, index) {
                let successor = edge.target;
                predecessor_counts[successor.get() as usize] =
                    predecessor_counts[successor.get() as usize].saturating_add(1);
            }
        }

        for (index, verified) in instructions.iter().enumerate() {
            let decoded = verified.decoded();
            let opcode = decoded.instruction().opcode();
            if matches!(opcode, FinalOpcode::SetName | FinalOpcode::SetNameComputed)
                && inferred_function_name_pair(
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
                && inferred_computed_class_name_pair(
                    graph,
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
                && inferred_captured_computed_class_name_pair(
                    graph,
                    parent,
                    &metadata[parent_index],
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
                && private_method_name_pair(
                    parent,
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
                .is_none()
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::SetNameTemplateMismatch { pc: decoded.pc() },
                ));
            }
            if opcode == FinalOpcode::SetHomeObject
                && !private_method_home_object_pair(
                    parent,
                    &metadata[parent_index],
                    metadata,
                    instructions,
                    &predecessor_counts,
                    internal_stack,
                    index,
                )
            {
                return Err(BytecodeVerificationError::function(
                    parent_id,
                    BytecodeVerificationErrorKind::UnsupportedCompilerOpcode {
                        pc: decoded.pc(),
                        opcode,
                    },
                ));
            }
        }
    }
    Ok(())
}

/// Certifies one private instance method closure created during class
/// definition evaluation. The method name is set only on a fresh anonymous
/// method template, after the surrounding class prototype has become its home
/// object, and the function is immediately retained in a class-local cell.
fn private_instance_method_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetName, Operands::Atom(_))
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let closure_index = set_name_index.checked_sub(4)?;
    if !matches!(
        instructions
            .get(closure_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let expected = [
        (closure_index.checked_add(1)?, FinalOpcode::Swap),
        (closure_index.checked_add(2)?, FinalOpcode::SetHomeObject),
        (closure_index.checked_add(3)?, FinalOpcode::Swap),
    ];
    for (index, opcode) in expected {
        let instruction = instructions.get(index)?.decoded().instruction();
        if instruction.opcode() != opcode {
            return None;
        }
    }
    let store_index = set_name_index.checked_add(1)?;
    if !matches!(
        instructions
            .get(store_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    ) {
        return None;
    }
    for (from, to) in [
        (closure_index, closure_index.checked_add(1)?),
        (closure_index.checked_add(1)?, closure_index.checked_add(2)?),
        (closure_index.checked_add(2)?, closure_index.checked_add(3)?),
        (closure_index.checked_add(3)?, set_name_index),
        (set_name_index, store_index),
    ] {
        if !internal_stack.has_effective_successor(instructions, from, usize_to_u32(to)) {
            return None;
        }
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) && child_metadata.function_name.is_none())
    .then_some(*child)
}

/// Certifies one private static method closure created during class definition
/// evaluation. Its home object is the fresh constructor, and the closure is
/// retained in its class-local cell before that same constructor receives the
/// private method element.
fn private_static_method_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetName, Operands::Atom(_))
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let closure_index = set_name_index.checked_sub(10)?;
    if !matches!(
        instructions
            .get(closure_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let expected = [
        (closure_index.checked_add(1)?, FinalOpcode::Swap),
        (closure_index.checked_add(2)?, FinalOpcode::Perm3),
        (closure_index.checked_add(3)?, FinalOpcode::Swap),
        (closure_index.checked_add(4)?, FinalOpcode::Perm3),
        (closure_index.checked_add(5)?, FinalOpcode::SetHomeObject),
        (closure_index.checked_add(6)?, FinalOpcode::Perm3),
        (closure_index.checked_add(7)?, FinalOpcode::Swap),
        (closure_index.checked_add(8)?, FinalOpcode::Perm3),
        (closure_index.checked_add(9)?, FinalOpcode::Swap),
    ];
    for (index, opcode) in expected {
        if instructions.get(index)?.decoded().instruction().opcode() != opcode {
            return None;
        }
    }
    let store_index = set_name_index.checked_add(1)?;
    if !matches!(
        instructions
            .get(store_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    ) {
        return None;
    }
    for from in closure_index..store_index {
        if !internal_stack.has_effective_successor(
            instructions,
            from,
            usize_to_u32(from.checked_add(1)?),
        ) {
            return None;
        }
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) && child_metadata.function_name.is_none())
    .then_some(*child)
}

fn private_method_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    private_instance_method_name_pair(
        parent,
        metadata,
        instructions,
        predecessor_counts,
        internal_stack,
        set_name_index,
    )
    .or_else(|| {
        private_static_method_name_pair(
            parent,
            metadata,
            instructions,
            predecessor_counts,
            internal_stack,
            set_name_index,
        )
    })
}

/// Certifies the hidden instance-element initializer closure created
/// immediately after its class definition. The fresh class prototype becomes
/// the method's home object, and the closure is then published only into the
/// compiler-owned immutable initializer cell.
fn class_instance_initializer_pair(
    parent: &VerifiedCompilerFunction,
    parent_metadata: &VerifiedFunctionMetadata,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    closure_index: usize,
) -> Option<FunctionTemplateId> {
    let class_index = closure_index.checked_sub(1)?;
    let class_instruction = instructions.get(class_index)?.decoded().instruction();
    if !matches!(
        (class_instruction.opcode(), class_instruction.operands()),
        (FinalOpcode::DefineClass, Operands::AtomU8 { value, .. }) if value & 2 != 0
    ) {
        return None;
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    if !matches!(
        closure.opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let swap_home_index = closure_index.checked_add(1)?;
    let home_index = closure_index.checked_add(2)?;
    let swap_store_index = closure_index.checked_add(3)?;
    let store_index = closure_index.checked_add(4)?;
    for (index, opcode) in [
        (swap_home_index, FinalOpcode::Swap),
        (home_index, FinalOpcode::SetHomeObject),
        (swap_store_index, FinalOpcode::Swap),
    ] {
        if instructions.get(index)?.decoded().instruction().opcode() != opcode
            || predecessor_counts.get(index) != Some(&1)
        {
            return None;
        }
    }
    let store = instructions.get(store_index)?.decoded().instruction();
    if !is_unchecked_local_put(store.opcode()) {
        return None;
    }
    let local = local_operand(store.opcode(), store.operands())?;
    let arguments = parent.control_flow().domains().argument_count() as usize;
    if parent_metadata
        .variables
        .get(arguments.checked_add(local as usize)?)?
        .policy()
        .kind()
        != CompilerBindingKind::ClassInstanceInitializer
        || predecessor_counts.get(store_index) != Some(&1)
    {
        return None;
    }
    for from in class_index..store_index {
        if !internal_stack.has_effective_successor(
            instructions,
            from,
            usize_to_u32(from.checked_add(1)?),
        ) {
            return None;
        }
    }
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (child_metadata.executable_kind == CompilerExecutableKind::ClassInstanceInitializer
        && child_metadata.function_name.is_none())
    .then_some(*child)
}

fn private_method_home_object_pair(
    parent: &VerifiedCompilerFunction,
    parent_metadata: &VerifiedFunctionMetadata,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    home_object_index: usize,
) -> bool {
    let instance = home_object_index
        .checked_add(2)
        .and_then(|set_name_index| {
            private_instance_method_name_pair(
                parent,
                metadata,
                instructions,
                predecessor_counts,
                internal_stack,
                set_name_index,
            )
        })
        .is_some();
    let r#static = home_object_index
        .checked_add(5)
        .and_then(|set_name_index| {
            private_static_method_name_pair(
                parent,
                metadata,
                instructions,
                predecessor_counts,
                internal_stack,
                set_name_index,
            )
        })
        .is_some();
    let initializer = home_object_index
        .checked_sub(2)
        .is_some_and(|closure_index| {
            class_instance_initializer_pair(
                parent,
                parent_metadata,
                metadata,
                instructions,
                predecessor_counts,
                internal_stack,
                closure_index,
            )
            .is_some()
        });
    instance || r#static || initializer
}

fn inferred_function_name_pair(
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?;
    let set_name_instruction = set_name.decoded().instruction();
    if !matches!(
        (
            set_name_instruction.opcode(),
            set_name_instruction.operands(),
        ),
        (FinalOpcode::SetName, Operands::Atom(_)) | (FinalOpcode::SetNameComputed, Operands::None)
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    if set_name_instruction.opcode() == FinalOpcode::SetNameComputed {
        let definition_index = set_name_index.checked_add(1)?;
        if instructions
            .get(definition_index)?
            .decoded()
            .instruction()
            .opcode()
            != FinalOpcode::DefineArrayEl
            || !internal_stack.has_effective_successor(
                instructions,
                set_name_index,
                usize_to_u32(definition_index),
            )
        {
            return None;
        }
    }
    let closure_index = set_name_index.checked_sub(1)?;
    if !internal_stack.has_effective_successor(
        instructions,
        closure_index,
        usize_to_u32(set_name_index),
    ) {
        return None;
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_metadata = usize::try_from(child.get())
        .ok()
        .and_then(|index| metadata.get(index))?;
    (matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryFunction
            | CompilerExecutableKind::OrdinaryArrow
            | CompilerExecutableKind::AsyncArrow
            | CompilerExecutableKind::GeneratorFunction
            | CompilerExecutableKind::AsyncFunction
            | CompilerExecutableKind::AsyncGeneratorFunction
    ) && child_metadata.function_name.is_none())
    .then_some(*child)
}

fn inferred_computed_class_name_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetNameComputed, Operands::None)
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let key_permutation_index = set_name_index.checked_sub(1)?;
    let constructor_swap_index = key_permutation_index.checked_sub(1)?;
    let definition_class_index = constructor_swap_index.checked_sub(1)?;
    let child = class_definition_pair(
        graph,
        parent,
        metadata,
        instructions,
        predecessor_counts,
        internal_stack,
        definition_class_index,
    )?;
    let closure_index = definition_class_index.checked_sub(1)?;
    let undefined_index = closure_index.checked_sub(1)?;
    let key_conversion_index = undefined_index.checked_sub(1)?;
    let expected_opcodes = [
        (key_conversion_index, FinalOpcode::ToPropKey),
        (undefined_index, FinalOpcode::Undefined),
        (definition_class_index, FinalOpcode::DefineClass),
        (constructor_swap_index, FinalOpcode::Swap),
        (key_permutation_index, FinalOpcode::Perm3),
    ];
    for (index, expected) in expected_opcodes {
        let actual = instructions.get(index)?.decoded().instruction().opcode();
        if actual != expected {
            return None;
        }
    }
    if !matches!(
        instructions
            .get(closure_index)?
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::FClosure | FinalOpcode::FClosure8
    ) {
        return None;
    }
    let sequence = [
        key_conversion_index,
        undefined_index,
        closure_index,
        definition_class_index,
        constructor_swap_index,
        key_permutation_index,
        set_name_index,
    ];
    for pair in sequence.windows(2) {
        if !internal_stack.has_effective_successor(instructions, pair[0], usize_to_u32(pair[1])) {
            return None;
        }
    }
    Some(child)
}

/// Certifies `NamedEvaluation` for an anonymous class in a computed public
/// field initializer. The key is evaluated once during
/// `ClassDefinitionEvaluation` and retained in a compiler-created immutable
/// `ClassFieldKey` cell: locally for a static field or through the constructor
/// capture for an instance field.
#[allow(
    clippy::too_many_arguments,
    reason = "the certificate validates one complete cross-function class-name sequence"
)]
fn inferred_captured_computed_class_name_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    parent_metadata: &VerifiedFunctionMetadata,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    set_name_index: usize,
) -> Option<FunctionTemplateId> {
    let set_name = instructions.get(set_name_index)?.decoded().instruction();
    if !matches!(
        (set_name.opcode(), set_name.operands()),
        (FinalOpcode::SetNameComputed, Operands::None)
    ) || predecessor_counts.get(set_name_index) != Some(&1)
    {
        return None;
    }
    let key_permutation_index = set_name_index.checked_sub(1)?;
    let constructor_swap_index = key_permutation_index.checked_sub(1)?;
    let definition_class_index = constructor_swap_index.checked_sub(1)?;
    let closure_index = definition_class_index.checked_sub(1)?;
    let undefined_index = closure_index.checked_sub(1)?;
    let key_read_index = undefined_index.checked_sub(1)?;
    let child = class_definition_pair(
        graph,
        parent,
        metadata,
        instructions,
        predecessor_counts,
        internal_stack,
        definition_class_index,
    )?;
    let key_read = instructions.get(key_read_index)?.decoded().instruction();
    let retained_key = if key_read.opcode() == FinalOpcode::GetVarRefCheck {
        closure_operand(key_read.opcode(), key_read.operands()).is_some_and(|slot| {
            parent_metadata
                .closures()
                .get(slot as usize)
                .is_some_and(|definition| {
                    definition.policy().kind() == CompilerBindingKind::ClassFieldKey
                })
        })
    } else if key_read.opcode() == FinalOpcode::GetLocCheck {
        local_operand(key_read.opcode(), key_read.operands()).is_some_and(|slot| {
            let arguments = parent.control_flow().domains().argument_count() as usize;
            parent_metadata
                .variables
                .get(arguments.saturating_add(slot as usize))
                .is_some_and(|definition| {
                    definition.policy().kind() == CompilerBindingKind::ClassFieldKey
                })
        })
    } else {
        false
    };
    if !retained_key {
        return None;
    }
    let expected_opcodes = [
        (undefined_index, FinalOpcode::Undefined),
        (closure_index, FinalOpcode::FClosure),
        (definition_class_index, FinalOpcode::DefineClass),
        (constructor_swap_index, FinalOpcode::Swap),
        (key_permutation_index, FinalOpcode::Perm3),
    ];
    for (index, expected) in expected_opcodes {
        let actual = instructions.get(index)?.decoded().instruction().opcode();
        if actual != expected
            && !(expected == FinalOpcode::FClosure
                && matches!(actual, FinalOpcode::FClosure | FinalOpcode::FClosure8))
        {
            return None;
        }
    }
    let sequence = [
        key_read_index,
        undefined_index,
        closure_index,
        definition_class_index,
        constructor_swap_index,
        key_permutation_index,
        set_name_index,
    ];
    for pair in sequence.windows(2) {
        if !internal_stack.has_effective_successor(instructions, pair[0], usize_to_u32(pair[1])) {
            return None;
        }
    }
    Some(child)
}

fn method_definition_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    definition_index: usize,
) -> Option<(FunctionTemplateId, u8)> {
    let definition = instructions.get(definition_index)?;
    let definition_instruction = definition.decoded().instruction();
    let ((FinalOpcode::DefineMethod, Operands::AtomU8 { value: flags, .. })
    | (FinalOpcode::DefineMethodComputed, Operands::U8(flags))) = (
        definition_instruction.opcode(),
        definition_instruction.operands(),
    )
    else {
        return None;
    };
    if !matches!(flags, 0..=2 | 4..=6) || predecessor_counts.get(definition_index) != Some(&1) {
        return None;
    }
    let closure_index = definition_index.checked_sub(1)?;
    let closure = instructions.get(closure_index)?;
    if !internal_stack.has_effective_successor(
        instructions,
        closure_index,
        usize_to_u32(definition_index),
    ) {
        return None;
    }
    let closure_instruction = closure.decoded().instruction();
    let constant = closure_constant(closure_instruction.opcode(), closure_instruction.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_index = usize::try_from(child.get()).ok()?;
    let child_metadata = metadata.get(child_index)?;
    if !matches!(
        child_metadata.executable_kind,
        CompilerExecutableKind::OrdinaryMethod
            | CompilerExecutableKind::GeneratorMethod
            | CompilerExecutableKind::AsyncMethod
            | CompilerExecutableKind::AsyncGeneratorMethod
    ) {
        return None;
    }
    // Accessor grammar constrains the complete formal-parameter list, not
    // the observable `length`. A setter with one defaulted parameter has one
    // argument slot while its ExpectedArgumentCount is zero.
    let arguments = graph
        .function(*child)?
        .control_flow()
        .domains()
        .argument_count();
    let kind = flags & 0b11;
    if (kind == 1 && arguments != 0) || (kind == 2 && arguments != 1) {
        return None;
    }
    Some((*child, flags))
}

const fn is_method_definition_opcode(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::DefineMethod | FinalOpcode::DefineMethodComputed
    )
}

const fn is_class_definition_opcode(opcode: FinalOpcode) -> bool {
    matches!(opcode, FinalOpcode::DefineClass)
}

fn class_definition_pair(
    graph: &VerifiedCompilerFunctionGraph,
    parent: &VerifiedCompilerFunction,
    metadata: &[VerifiedFunctionMetadata],
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    definition_index: usize,
) -> Option<FunctionTemplateId> {
    let definition = instructions.get(definition_index)?;
    let definition_instruction = definition.decoded().instruction();
    let (FinalOpcode::DefineClass, Operands::AtomU8 { value: flags, .. }) = (
        definition_instruction.opcode(),
        definition_instruction.operands(),
    ) else {
        return None;
    };
    if flags > 3 || predecessor_counts.get(definition_index) != Some(&1) {
        return None;
    }
    let heritage = flags & 1;
    let closure_index = definition_index.checked_sub(1)?;
    if !internal_stack.has_effective_successor(
        instructions,
        closure_index,
        usize_to_u32(definition_index),
    ) {
        return None;
    }
    let closure = instructions.get(closure_index)?.decoded().instruction();
    let constant = closure_constant(closure.opcode(), closure.operands())?;
    let crate::CompilerConstant::Function(child) = parent.constants().get(constant as usize)?
    else {
        return None;
    };
    let child_index = usize::try_from(child.get()).ok()?;
    let child_metadata = metadata.get(child_index)?;
    if child_metadata.executable_kind != CompilerExecutableKind::ClassConstructor {
        return None;
    }
    let child_function = graph.function(*child)?;
    let derived = child_function
        .control_flow()
        .function_header()
        .flags()
        .is_derived_class_constructor();
    if derived != (heritage == 1) {
        return None;
    }
    if heritage == 1
        && !derived_class_heritage_pair(
            parent,
            instructions,
            predecessor_counts,
            internal_stack,
            definition_index,
        )
    {
        return None;
    }
    Some(*child)
}

/// Proves that the derived `define_class` received the pair produced by
/// `ClassDefinitionEvaluation`: the one evaluated superclass (or `null`) and
/// the exactly-once observed `superclass.prototype` value.  The shape also
/// makes `check_ctor` admissible only at this semantic site.
fn derived_class_heritage_pair(
    parent: &VerifiedCompilerFunction,
    instructions: &[VerifiedInstruction],
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
    definition_index: usize,
) -> bool {
    let Some(closure_index) = definition_index.checked_sub(1) else {
        return false;
    };
    // A compile-time `extends null` heritage emits the null-prototype pair
    // directly — `[Null, Null, FClosure, DefineClass]` — with no runtime
    // dispatch. The evaluated superclass is `null` and the prototype parent
    // is the null-prototype marker, exactly the pair the runtime dispatch
    // would have produced.
    if let Some(superclass_index) = closure_index.checked_sub(1)
        && let Some(prototype_index) = superclass_index.checked_sub(1)
        && instructions
            .get(superclass_index)
            .is_some_and(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::Null
            })
        && instructions
            .get(prototype_index)
            .is_some_and(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::Null
            })
        && predecessor_counts.get(superclass_index) == Some(&1)
        && predecessor_counts.get(prototype_index) == Some(&1)
        && internal_stack.has_effective_successor(
            instructions,
            prototype_index,
            usize_to_u32(superclass_index),
        )
        && internal_stack.has_effective_successor(
            instructions,
            superclass_index,
            usize_to_u32(closure_index),
        )
    {
        return true;
    }
    let Some(null_index) = closure_index.checked_sub(1) else {
        return false;
    };
    let Some(goto_index) = null_index.checked_sub(1) else {
        return false;
    };
    let Some(get_prototype_index) = goto_index.checked_sub(1) else {
        return false;
    };
    let Some(duplicate_constructor_index) = get_prototype_index.checked_sub(1) else {
        return false;
    };
    let Some(check_constructor_index) = duplicate_constructor_index.checked_sub(1) else {
        return false;
    };
    let Some(if_null_index) = check_constructor_index.checked_sub(1) else {
        return false;
    };
    let Some(null_test_index) = if_null_index.checked_sub(1) else {
        return false;
    };
    let Some(duplicate_heritage_index) = null_test_index.checked_sub(1) else {
        return false;
    };

    let is_prototype_read = instructions
        .get(get_prototype_index)
        .map(|instruction| instruction.decoded().instruction())
        .is_some_and(
            |instruction| match (instruction.opcode(), instruction.operands()) {
                (FinalOpcode::GetField, Operands::Atom(atom)) => usize::try_from(atom.get())
                    .ok()
                    .and_then(|index| parent.atoms().get(index))
                    .is_some_and(|candidate| {
                        candidate.string().latin1_units() == Some(b"prototype")
                    }),
                _ => false,
            },
        );
    if !is_prototype_read {
        return false;
    }

    let expected_opcodes = [
        (duplicate_heritage_index, FinalOpcode::Dup),
        (null_test_index, FinalOpcode::IsNull),
        (check_constructor_index, FinalOpcode::CheckCtor),
        (duplicate_constructor_index, FinalOpcode::Dup),
        (null_index, FinalOpcode::Null),
    ];
    if expected_opcodes.into_iter().any(|(index, expected)| {
        instructions
            .get(index)
            .is_none_or(|instruction| instruction.decoded().instruction().opcode() != expected)
    }) {
        return false;
    }
    if !matches!(
        instructions
            .get(if_null_index)
            .map(|instruction| instruction.decoded().instruction().opcode()),
        Some(FinalOpcode::IfTrue | FinalOpcode::IfTrue8)
    ) || !matches!(
        instructions
            .get(goto_index)
            .map(|instruction| instruction.decoded().instruction().opcode()),
        Some(FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16)
    ) {
        return false;
    }
    let sequence = [
        (duplicate_heritage_index, null_test_index),
        (null_test_index, if_null_index),
        (if_null_index, check_constructor_index),
        (if_null_index, null_index),
        (check_constructor_index, duplicate_constructor_index),
        (duplicate_constructor_index, get_prototype_index),
        (get_prototype_index, goto_index),
        (goto_index, closure_index),
        (null_index, closure_index),
        (closure_index, definition_index),
    ];
    if sequence.into_iter().any(|(from, to)| {
        !internal_stack.has_effective_successor(instructions, from, usize_to_u32(to))
    }) {
        return false;
    }
    predecessor_counts.get(null_index) == Some(&1)
        && predecessor_counts.get(check_constructor_index) == Some(&1)
        && predecessor_counts.get(closure_index) == Some(&2)
}

/// Certifies the complete source-less derived-constructor body. Instance
/// elements are delegated to one compiler-owned hidden method; they are never
/// inlined into the constructor or duplicated at an arrow `super()` site.
fn derived_default_constructor_pair(
    function: &VerifiedCompilerFunction,
    metadata: &VerifiedFunctionMetadata,
    predecessor_counts: &[u32],
    internal_stack: &InternalStackCertificate,
) -> bool {
    let instructions = function.control_flow().instructions();
    let has_opcodes = |expected: &[FinalOpcode]| {
        instructions.len() == expected.len()
            && instructions
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.decoded().instruction().opcode() == *expected)
    };
    let no_initializer = has_opcodes(&[
        FinalOpcode::CheckCtor,
        FinalOpcode::InitCtor,
        FinalOpcode::Drop,
        FinalOpcode::ReturnUndef,
    ]);
    let initializer = has_opcodes(&[
        FinalOpcode::CheckCtor,
        FinalOpcode::InitCtor,
        FinalOpcode::GetVarRefCheck,
        FinalOpcode::PushThis,
        FinalOpcode::Swap,
        FinalOpcode::CallMethod,
        FinalOpcode::Drop,
        FinalOpcode::Drop,
        FinalOpcode::ReturnUndef,
    ]) && matches!(
        instructions
            .get(5)
            .map(|instruction| instruction.decoded().instruction().operands()),
        Some(Operands::NPop { argument_count: 0 })
    ) && instructions.get(2).is_some_and(|instruction| {
        let instruction = instruction.decoded().instruction();
        closure_operand(instruction.opcode(), instruction.operands()).is_some_and(|slot| {
            metadata
                .closures()
                .get(slot as usize)
                .is_some_and(|definition| {
                    definition.policy().kind() == CompilerBindingKind::ClassInstanceInitializer
                })
        })
    });
    if !no_initializer && !initializer {
        return false;
    }
    predecessor_counts.len() == instructions.len()
        && predecessor_counts.first() == Some(&0)
        && predecessor_counts.iter().skip(1).all(|count| *count == 1)
        && (0..instructions.len().saturating_sub(1)).all(|source| {
            has_only_effective_successor(
                internal_stack,
                instructions,
                source,
                usize_to_u32(source.saturating_add(1)),
            )
        })
}

fn has_only_effective_successor(
    internal_stack: &InternalStackCertificate,
    instructions: &[VerifiedInstruction],
    source: usize,
    target: u32,
) -> bool {
    let mut successors = internal_stack.effective_successors(instructions, source);
    successors
        .next()
        .is_some_and(|edge| edge.target.get() == target)
        && successors.next().is_none()
}

struct ParentClosureDefinition<'metadata> {
    name: Option<AtomPoolIndex>,
    binding: CompilerClosureBinding,
    arguments_object: bool,
    deletable_eval_variable: bool,
    atoms: &'metadata [crate::CompilerAtom],
}

fn parent_definition_for_reference<'metadata>(
    parent: &'metadata VerifiedCompilerFunction,
    metadata: &'metadata VerifiedFunctionMetadata,
    reference: u32,
) -> Option<ParentClosureDefinition<'metadata>> {
    let binding = parent
        .control_flow()
        .compiler_capture_layout()?
        .binding_for_variable_reference(reference)?;
    let arguments = parent.control_flow().domains().argument_count() as usize;
    let index = match binding {
        CompilerCapturedBinding::Argument(index) => usize::try_from(index).ok()?,
        CompilerCapturedBinding::FunctionLocal(index)
        | CompilerCapturedBinding::ScopedLocal(index) => {
            arguments.checked_add(usize::try_from(index).ok()?)?
        }
    };
    let definition = metadata.variables.get(index)?;
    (definition.variable_reference == Some(reference)).then_some(ParentClosureDefinition {
        name: definition.name,
        binding: CompilerClosureBinding::Captured(definition.policy),
        arguments_object: definition.arguments_object,
        deletable_eval_variable: false,
        atoms: parent.atoms(),
    })
}

fn atom_contents(
    atom: Option<AtomPoolIndex>,
    atoms: &[crate::CompilerAtom],
) -> Option<&crate::CompilerString> {
    let index = usize::try_from(atom?.get()).ok()?;
    atoms.get(index).map(crate::CompilerAtom::string)
}

pub(super) fn contextual_instance_initializer_sequence(
    flow: &VerifiedControlFlow,
    check_index: usize,
) -> bool {
    let instructions = flow.instructions();
    let expected = [
        (FinalOpcode::CheckCtorReturn, Operands::None),
        (FinalOpcode::SpecialObject, Operands::U8(6)),
        (FinalOpcode::PushThis, Operands::None),
        (FinalOpcode::Swap, Operands::None),
        (
            FinalOpcode::CallMethod,
            Operands::NPop { argument_count: 0 },
        ),
        (FinalOpcode::Drop, Operands::None),
        (FinalOpcode::Drop, Operands::None),
    ];
    for (offset, (opcode, operands)) in expected.into_iter().enumerate() {
        let Some(index) = check_index.checked_add(offset) else {
            return false;
        };
        let Some(instruction) = instructions.get(index) else {
            return false;
        };
        let instruction = instruction.decoded().instruction();
        if instruction.opcode() != opcode || instruction.operands() != operands {
            return false;
        }
        if offset == 0 {
            continue;
        }
        let predecessor_count = instructions
            .iter()
            .filter(|candidate| {
                let successors = candidate.successors();
                successors.fallthrough().map(InstructionIndex::get) == Some(usize_to_u32(index))
                    || successors.branch_target().map(InstructionIndex::get)
                        == Some(usize_to_u32(index))
                    || successors.jump_target().map(InstructionIndex::get)
                        == Some(usize_to_u32(index))
            })
            .count();
        let Some(previous) = instructions.get(index - 1) else {
            return false;
        };
        if predecessor_count != 1
            || previous.successors().kind() != crate::VerifiedSuccessorKind::Fallthrough
            || previous
                .successors()
                .fallthrough()
                .map(InstructionIndex::get)
                != Some(usize_to_u32(index))
        {
            return false;
        }
    }
    true
}
