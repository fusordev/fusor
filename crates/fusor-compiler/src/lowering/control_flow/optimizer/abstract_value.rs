use fusor_bytecode::{CompilerConstantValue, FinalOpcode, Instruction, Operands};

use super::{super::LeafCompilationError, ConstantInputs};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AbstractValue {
    Overdefined,
    Undefined,
    Null,
    Boolean(bool),
    NumberI32(i32),
    BigIntI32(i32),
    EmptyString,
    KnownTruthy,
    KnownFalsy,
}

impl AbstractValue {
    const fn truthiness(self) -> Option<bool> {
        match self {
            Self::Overdefined => None,
            Self::Undefined
            | Self::Null
            | Self::Boolean(false)
            | Self::NumberI32(0)
            | Self::BigIntI32(0)
            | Self::EmptyString
            | Self::KnownFalsy => Some(false),
            Self::Boolean(true) | Self::NumberI32(_) | Self::BigIntI32(_) | Self::KnownTruthy => {
                Some(true)
            }
        }
    }

    pub(super) fn join(self, incoming: Self) -> Self {
        if self == incoming {
            return self;
        }
        match (self.truthiness(), incoming.truthiness()) {
            (Some(true), Some(true)) => Self::KnownTruthy,
            (Some(false), Some(false)) => Self::KnownFalsy,
            _ => Self::Overdefined,
        }
    }

    const fn is_undefined(self) -> Option<bool> {
        match self {
            Self::Undefined => Some(true),
            Self::Null
            | Self::Boolean(_)
            | Self::NumberI32(_)
            | Self::BigIntI32(_)
            | Self::EmptyString
            | Self::KnownTruthy => Some(false),
            Self::Overdefined | Self::KnownFalsy => None,
        }
    }

    const fn is_null(self) -> Option<bool> {
        match self {
            Self::Null => Some(true),
            Self::Undefined
            | Self::Boolean(_)
            | Self::NumberI32(_)
            | Self::BigIntI32(_)
            | Self::EmptyString
            | Self::KnownTruthy => Some(false),
            Self::Overdefined | Self::KnownFalsy => None,
        }
    }

    const fn is_nullish(self) -> Option<bool> {
        match self {
            Self::Undefined | Self::Null => Some(true),
            Self::Boolean(_)
            | Self::NumberI32(_)
            | Self::BigIntI32(_)
            | Self::EmptyString
            | Self::KnownTruthy => Some(false),
            Self::Overdefined | Self::KnownFalsy => None,
        }
    }

    const fn strict_equal(self, right: Self) -> Option<bool> {
        match (self, right) {
            (Self::Overdefined | Self::KnownTruthy | Self::KnownFalsy, _)
            | (_, Self::Overdefined | Self::KnownTruthy | Self::KnownFalsy) => None,
            (Self::Undefined, Self::Undefined)
            | (Self::Null, Self::Null)
            | (Self::EmptyString, Self::EmptyString) => Some(true),
            (Self::Boolean(left), Self::Boolean(right)) => Some(left == right),
            (Self::NumberI32(left), Self::NumberI32(right))
            | (Self::BigIntI32(left), Self::BigIntI32(right)) => Some(left == right),
            _ => Some(false),
        }
    }
}

pub(super) fn constant_branch_outcome(
    opcode: FinalOpcode,
    stack: &[AbstractValue],
) -> Option<bool> {
    let truthy = stack.last().copied()?.truthiness()?;
    match opcode {
        FinalOpcode::IfTrue | FinalOpcode::IfTrue8 => Some(truthy),
        FinalOpcode::IfFalse | FinalOpcode::IfFalse8 => Some(!truthy),
        _ => None,
    }
}

pub(super) fn is_truthiness_branch(opcode: FinalOpcode) -> bool {
    matches!(
        opcode,
        FinalOpcode::IfFalse | FinalOpcode::IfTrue | FinalOpcode::IfFalse8 | FinalOpcode::IfTrue8
    )
}

pub(super) fn transfer_stack(
    stack: &mut Vec<AbstractValue>,
    instruction: Instruction,
    inputs: &ConstantInputs<'_>,
) -> Result<(), LeafCompilationError> {
    let opcode = instruction.opcode();
    match opcode {
        FinalOpcode::Undefined => push_value(stack, AbstractValue::Undefined),
        FinalOpcode::Null => push_value(stack, AbstractValue::Null),
        FinalOpcode::PushFalse => push_value(stack, AbstractValue::Boolean(false)),
        FinalOpcode::PushTrue => push_value(stack, AbstractValue::Boolean(true)),
        FinalOpcode::PushMinus1 => push_value(stack, AbstractValue::NumberI32(-1)),
        FinalOpcode::Push0 => push_value(stack, AbstractValue::NumberI32(0)),
        FinalOpcode::Push1 => push_value(stack, AbstractValue::NumberI32(1)),
        FinalOpcode::Push2 => push_value(stack, AbstractValue::NumberI32(2)),
        FinalOpcode::Push3 => push_value(stack, AbstractValue::NumberI32(3)),
        FinalOpcode::Push4 => push_value(stack, AbstractValue::NumberI32(4)),
        FinalOpcode::Push5 => push_value(stack, AbstractValue::NumberI32(5)),
        FinalOpcode::Push6 => push_value(stack, AbstractValue::NumberI32(6)),
        FinalOpcode::Push7 => push_value(stack, AbstractValue::NumberI32(7)),
        FinalOpcode::PushI8 => push_i8(stack, instruction.operands()),
        FinalOpcode::PushI16 => push_i16(stack, instruction.operands()),
        FinalOpcode::PushI32 => push_i32(stack, instruction.operands(), false),
        FinalOpcode::PushBigIntI32 => push_i32(stack, instruction.operands(), true),
        FinalOpcode::PushConst | FinalOpcode::PushConst8 => {
            push_constant(stack, instruction.operands(), inputs)
        }
        FinalOpcode::PushAtomValue => push_atom(stack, instruction.operands(), inputs),
        FinalOpcode::PushEmptyString => push_value(stack, AbstractValue::EmptyString),
        FinalOpcode::Object
        | FinalOpcode::FClosure
        | FinalOpcode::FClosure8
        | FinalOpcode::PrivateSymbol => push_value(stack, AbstractValue::KnownTruthy),
        FinalOpcode::Lnot => map_unary(stack, |value| {
            value
                .truthiness()
                .map_or(AbstractValue::Overdefined, |truthy| {
                    AbstractValue::Boolean(!truthy)
                })
        }),
        FinalOpcode::IsUndefined | FinalOpcode::TypeofIsUndefined => {
            map_predicate(stack, AbstractValue::is_undefined)
        }
        FinalOpcode::IsNull => map_predicate(stack, AbstractValue::is_null),
        FinalOpcode::IsUndefinedOrNull => map_predicate(stack, AbstractValue::is_nullish),
        FinalOpcode::StrictEq | FinalOpcode::StrictNeq => strict_equality(stack, opcode),
        FinalOpcode::Dup => duplicate_top(stack),
        FinalOpcode::Dup1 => duplicate_below_top(stack),
        FinalOpcode::Dup2 => duplicate_tail(stack, 2),
        FinalOpcode::Dup3 => duplicate_tail(stack, 3),
        FinalOpcode::Insert2 => insert_top_copy(stack, 2),
        FinalOpcode::Insert3 => insert_top_copy(stack, 3),
        FinalOpcode::Insert4 => insert_top_copy(stack, 4),
        FinalOpcode::Nip => nip(stack),
        FinalOpcode::Swap => permute_tail(stack, 2, |values| values.swap(0, 1)),
        FinalOpcode::Perm3 => permute_tail(stack, 3, |values| values.swap(0, 1)),
        FinalOpcode::Perm4 => permute_tail(stack, 4, |values| values[..3].rotate_right(1)),
        FinalOpcode::Perm5 => permute_tail(stack, 5, |values| values[..4].rotate_right(1)),
        FinalOpcode::Rot3l => permute_tail(stack, 3, |values| values.rotate_left(1)),
        FinalOpcode::Rot3r => permute_tail(stack, 3, |values| values.rotate_right(1)),
        FinalOpcode::Rot4l => permute_tail(stack, 4, |values| values.rotate_left(1)),
        FinalOpcode::Rot5l => permute_tail(stack, 5, |values| values.rotate_left(1)),
        _ => apply_unknown_effect(stack, instruction),
    }
}

fn push_constant(
    stack: &mut Vec<AbstractValue>,
    operands: Operands,
    inputs: &ConstantInputs<'_>,
) -> Result<(), LeafCompilationError> {
    let index = match operands {
        Operands::Const(index) => index,
        Operands::Const8(index) => u32::from(index),
        _ => return invalid_verified_operand(),
    };
    let value = inputs
        .constant(index)
        .and_then(crate::lowering::CompiledConstant::value)
        .map_or(AbstractValue::Overdefined, constant_truthiness);
    push_value(stack, value)
}

fn constant_truthiness(value: &CompilerConstantValue) -> AbstractValue {
    let truthy = match value {
        CompilerConstantValue::Number(number) => {
            let number = number.to_f64();
            number != 0.0 && !number.is_nan()
        }
        CompilerConstantValue::String(string) => !string.is_empty(),
        CompilerConstantValue::BigInt(bigint) => {
            let decimal = bigint.decimal();
            decimal.len() != 1 || decimal.code_units().next() != Some(u16::from(b'0'))
        }
        CompilerConstantValue::TemplateObject(_) => true,
    };
    if truthy {
        AbstractValue::KnownTruthy
    } else {
        AbstractValue::KnownFalsy
    }
}

fn push_atom(
    stack: &mut Vec<AbstractValue>,
    operands: Operands,
    inputs: &ConstantInputs<'_>,
) -> Result<(), LeafCompilationError> {
    let Operands::Atom(index) = operands else {
        return invalid_verified_operand();
    };
    let value = inputs
        .atom(index.get())
        .map_or(AbstractValue::Overdefined, |atom| {
            if atom.string().is_empty() {
                AbstractValue::KnownFalsy
            } else {
                AbstractValue::KnownTruthy
            }
        });
    push_value(stack, value)
}

fn push_i8(stack: &mut Vec<AbstractValue>, operands: Operands) -> Result<(), LeafCompilationError> {
    let Operands::I8(value) = operands else {
        return invalid_verified_operand();
    };
    push_value(stack, AbstractValue::NumberI32(i32::from(value)))
}

fn push_i16(
    stack: &mut Vec<AbstractValue>,
    operands: Operands,
) -> Result<(), LeafCompilationError> {
    let Operands::I16(value) = operands else {
        return invalid_verified_operand();
    };
    push_value(stack, AbstractValue::NumberI32(i32::from(value)))
}

fn push_i32(
    stack: &mut Vec<AbstractValue>,
    operands: Operands,
    bigint: bool,
) -> Result<(), LeafCompilationError> {
    let Operands::I32(value) = operands else {
        return invalid_verified_operand();
    };
    let value = if bigint {
        AbstractValue::BigIntI32(value)
    } else {
        AbstractValue::NumberI32(value)
    };
    push_value(stack, value)
}

fn map_unary(
    stack: &mut Vec<AbstractValue>,
    predicate: impl FnOnce(AbstractValue) -> AbstractValue,
) -> Result<(), LeafCompilationError> {
    let value = pop_value(stack)?;
    push_value(stack, predicate(value))
}

fn map_predicate(
    stack: &mut Vec<AbstractValue>,
    predicate: impl FnOnce(AbstractValue) -> Option<bool>,
) -> Result<(), LeafCompilationError> {
    map_unary(stack, |value| {
        predicate(value).map_or(AbstractValue::Overdefined, AbstractValue::Boolean)
    })
}

fn strict_equality(
    stack: &mut Vec<AbstractValue>,
    opcode: FinalOpcode,
) -> Result<(), LeafCompilationError> {
    let right = pop_value(stack)?;
    let left = pop_value(stack)?;
    let result = left.strict_equal(right);
    let result = if opcode == FinalOpcode::StrictNeq {
        result.map(|equal| !equal)
    } else {
        result
    };
    push_value(
        stack,
        result.map_or(AbstractValue::Overdefined, AbstractValue::Boolean),
    )
}

fn duplicate_top(stack: &mut Vec<AbstractValue>) -> Result<(), LeafCompilationError> {
    let value = stack
        .last()
        .copied()
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "verified dup has an optimizer input",
            span: None,
        })?;
    push_value(stack, value)
}

fn duplicate_below_top(stack: &mut Vec<AbstractValue>) -> Result<(), LeafCompilationError> {
    let top = pop_value(stack)?;
    let value = stack
        .last()
        .copied()
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "verified dup1 has two optimizer inputs",
            span: None,
        })?;
    push_value(stack, value)?;
    push_value(stack, top)
}

fn nip(stack: &mut Vec<AbstractValue>) -> Result<(), LeafCompilationError> {
    let top = pop_value(stack)?;
    pop_value(stack)?;
    push_value(stack, top)
}

fn apply_unknown_effect(
    stack: &mut Vec<AbstractValue>,
    instruction: Instruction,
) -> Result<(), LeafCompilationError> {
    let effect =
        instruction
            .stack_effect()
            .map_err(|_| LeafCompilationError::SemanticInvariant {
                invariant: "verified instruction retains a valid optimizer stack effect",
                span: None,
            })?;
    let pops =
        usize::try_from(effect.pops()).map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "optimizer pop count",
        })?;
    if pops > stack.len() {
        return Err(LeafCompilationError::SemanticInvariant {
            invariant: "verified optimizer transfer does not underflow",
            span: None,
        });
    }
    stack.truncate(stack.len() - pops);
    let pushes =
        usize::try_from(effect.pushes()).map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "optimizer push count",
        })?;
    stack
        .try_reserve(pushes)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "CFG constant transfer stack",
        })?;
    stack.extend(std::iter::repeat_n(AbstractValue::Overdefined, pushes));
    Ok(())
}

fn duplicate_tail(
    stack: &mut Vec<AbstractValue>,
    count: usize,
) -> Result<(), LeafCompilationError> {
    let start = stack
        .len()
        .checked_sub(count)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "verified duplicate has optimizer inputs",
            span: None,
        })?;
    let original_len = stack.len();
    stack
        .try_reserve(count)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "CFG constant transfer stack",
        })?;
    for index in start..original_len {
        let value = stack[index];
        stack.push(value);
    }
    Ok(())
}

fn insert_top_copy(
    stack: &mut Vec<AbstractValue>,
    count: usize,
) -> Result<(), LeafCompilationError> {
    let start = stack
        .len()
        .checked_sub(count)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "verified insert has optimizer inputs",
            span: None,
        })?;
    let top = *stack
        .last()
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "verified insert has an optimizer top value",
            span: None,
        })?;
    stack
        .try_reserve(1)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "CFG constant transfer stack",
        })?;
    stack.insert(start, top);
    Ok(())
}

fn permute_tail(
    stack: &mut [AbstractValue],
    count: usize,
    permutation: impl FnOnce(&mut [AbstractValue]),
) -> Result<(), LeafCompilationError> {
    let start = stack
        .len()
        .checked_sub(count)
        .ok_or(LeafCompilationError::SemanticInvariant {
            invariant: "verified permutation has optimizer inputs",
            span: None,
        })?;
    permutation(&mut stack[start..]);
    Ok(())
}

fn push_value(
    stack: &mut Vec<AbstractValue>,
    value: AbstractValue,
) -> Result<(), LeafCompilationError> {
    stack
        .try_reserve(1)
        .map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "CFG constant transfer stack",
        })?;
    stack.push(value);
    Ok(())
}

fn pop_value(stack: &mut Vec<AbstractValue>) -> Result<AbstractValue, LeafCompilationError> {
    stack.pop().ok_or(LeafCompilationError::SemanticInvariant {
        invariant: "verified optimizer transfer has its stack input",
        span: None,
    })
}

fn invalid_verified_operand<T>() -> Result<T, LeafCompilationError> {
    Err(LeafCompilationError::SemanticInvariant {
        invariant: "verified optimizer opcode retains its operand format",
        span: None,
    })
}
