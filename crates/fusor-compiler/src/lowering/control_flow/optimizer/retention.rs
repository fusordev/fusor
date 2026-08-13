use std::collections::VecDeque;

use fusor_bytecode::{FinalOpcode, VerifiedControlFlow, VerifiedInstruction};

use super::super::LeafCompilationError;

pub(super) fn retained_instructions(
    control_flow: &VerifiedControlFlow,
    executable: &[bool],
    branch_outcomes: &[Option<bool>],
) -> Result<Vec<bool>, LeafCompilationError> {
    let mut retained = clone_bools(executable, "CFG retained instructions")?;
    retain_binding_policy_anchors(control_flow, &mut retained);
    retain_function_initializer_pairs(control_flow, &mut retained);
    retain_exception_components(control_flow, executable, &mut retained)?;
    retain_loop_activation_certificates(control_flow, branch_outcomes, &mut retained)?;
    Ok(retained)
}

fn retain_binding_policy_anchors(control_flow: &VerifiedControlFlow, retained: &mut [bool]) {
    for (position, verified) in control_flow.instructions().iter().copied().enumerate() {
        if verified.decoded().instruction().opcode() == FinalOpcode::SetLocUninitialized {
            retained[position] = true;
        }
    }
}

fn retain_function_initializer_pairs(control_flow: &VerifiedControlFlow, retained: &mut [bool]) {
    let instructions = control_flow.instructions();
    for position in 0..instructions.len().saturating_sub(1) {
        if matches!(
            instructions[position].decoded().instruction().opcode(),
            FinalOpcode::FClosure | FinalOpcode::FClosure8
        ) && is_initializer_put(instructions[position + 1].decoded().instruction().opcode())
        {
            retained[position] = true;
            retained[position + 1] = true;
        }
    }
}

const fn is_initializer_put(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::PutArg
            | FinalOpcode::PutArg0
            | FinalOpcode::PutArg1
            | FinalOpcode::PutArg2
            | FinalOpcode::PutArg3
            | FinalOpcode::PutLoc
            | FinalOpcode::PutLoc8
            | FinalOpcode::PutLoc0
            | FinalOpcode::PutLoc1
            | FinalOpcode::PutLoc2
            | FinalOpcode::PutLoc3
    )
}

fn retain_exception_components(
    control_flow: &VerifiedControlFlow,
    executable: &[bool],
    retained: &mut [bool],
) -> Result<(), LeafCompilationError> {
    let instruction_count = control_flow.instructions().len();
    let mut visited = reserved_vec(instruction_count, "CFG structural exception components")?;
    visited.resize(instruction_count, false);
    let mut worklist = VecDeque::new();

    for (position, verified) in control_flow.instructions().iter().copied().enumerate() {
        if !executable[position] && verified.decoded().instruction().opcode() == FinalOpcode::Catch
        {
            reserve_worklist(&mut worklist)?;
            worklist.push_back(position);
        }
    }

    while let Some(position) = worklist.pop_front() {
        let Some(seen) = visited.get_mut(position) else {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "structural exception work item resolves to an instruction",
                span: None,
            });
        };
        if *seen {
            continue;
        }
        *seen = true;
        retained[position] = true;
        let verified = control_flow.instructions()[position];
        for successor in [
            verified.successors().fallthrough(),
            verified.successors().branch_target(),
            verified.successors().jump_target(),
        ]
        .into_iter()
        .flatten()
        {
            let successor = usize_from_u32(successor.get(), "structural exception successor")?;
            if !visited.get(successor).copied().unwrap_or(false) {
                reserve_worklist(&mut worklist)?;
                worklist.push_back(successor);
            }
        }
    }
    Ok(())
}

fn retain_loop_activation_certificates(
    control_flow: &VerifiedControlFlow,
    branch_outcomes: &[Option<bool>],
    retained: &mut [bool],
) -> Result<(), LeafCompilationError> {
    let mut certified_targets = reserved_vec(retained.len(), "CFG loop activation targets")?;
    certified_targets.resize(retained.len(), false);

    for (position, verified) in control_flow.instructions().iter().copied().enumerate() {
        if !retained[position] || branch_outcomes[position] == Some(false) {
            continue;
        }
        let Some(target) = ordinary_backward_target(verified, position)? else {
            continue;
        };
        if is_retained_loop_activation_target(control_flow, retained, target)? {
            certified_targets[target] = true;
        }
    }

    for position in (0..control_flow.instructions().len()).rev() {
        if retained[position] {
            continue;
        }
        let verified = control_flow.instructions()[position];
        let Some(target) = ordinary_backward_target(verified, position)? else {
            continue;
        };
        if is_retained_loop_activation_target(control_flow, retained, target)?
            && !certified_targets[target]
        {
            retained[position] = true;
            certified_targets[target] = true;
        }
    }
    Ok(())
}

fn ordinary_backward_target(
    verified: VerifiedInstruction,
    position: usize,
) -> Result<Option<usize>, LeafCompilationError> {
    if !is_ordinary_branch(verified.decoded().instruction().opcode()) {
        return Ok(None);
    }
    let Some(target) = verified
        .successors()
        .jump_target()
        .or_else(|| verified.successors().branch_target())
    else {
        return Ok(None);
    };
    let target = usize_from_u32(target.get(), "optimizer structural back-edge target")?;
    Ok((target < position).then_some(target))
}

fn is_retained_loop_activation_target(
    control_flow: &VerifiedControlFlow,
    retained: &[bool],
    target: usize,
) -> Result<bool, LeafCompilationError> {
    if !retained.get(target).copied().unwrap_or(false) {
        return Ok(false);
    }
    let target_opcode = control_flow
        .instructions()
        .get(target)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "optimizer structural back edge resolves to an instruction",
            span: None,
        })?
        .decoded()
        .instruction()
        .opcode();
    Ok(target_opcode == FinalOpcode::SetLocUninitialized)
}

const fn is_ordinary_branch(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::IfFalse
            | FinalOpcode::IfFalse8
            | FinalOpcode::IfTrue
            | FinalOpcode::IfTrue8
            | FinalOpcode::Goto
            | FinalOpcode::Goto8
            | FinalOpcode::Goto16
    )
}

fn reserve_worklist(worklist: &mut VecDeque<usize>) -> Result<(), LeafCompilationError> {
    worklist
        .try_reserve(1)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "CFG structural retention worklist",
        })
}

fn clone_bools(values: &[bool], domain: &'static str) -> Result<Vec<bool>, LeafCompilationError> {
    let mut clone = reserved_vec(values.len(), domain)?;
    clone.extend_from_slice(values);
    Ok(clone)
}

fn reserved_vec<T>(capacity: usize, domain: &'static str) -> Result<Vec<T>, LeafCompilationError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| LeafCompilationError::CapacityExceeded { domain })?;
    Ok(values)
}

fn usize_from_u32(value: u32, domain: &'static str) -> Result<usize, LeafCompilationError> {
    usize::try_from(value).map_err(|_| LeafCompilationError::CapacityExceeded { domain })
}
