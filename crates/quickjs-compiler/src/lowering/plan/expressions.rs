use oxc_ast::ast::{Expression, LogicalExpression};
use quickjs_bytecode::BranchKind;
use quickjs_frontend::Span;

use crate::lowering::{CompilerLabel, PlannedInstruction};

pub(in crate::lowering) enum ExpressionWork<'expression, 'arena> {
    Visit(&'expression Expression<'arena>),
    Emit(PlannedInstruction),
    Branch {
        kind: BranchKind,
        target: CompilerLabel,
        span: Span,
    },
    Bind(CompilerLabel),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum ObjectMethodKind {
    Method,
    Getter,
    Setter,
}

impl ObjectMethodKind {
    const ENUMERABLE: u8 = 1 << 2;

    pub(in crate::lowering) const fn define_method_flags(self) -> u8 {
        Self::ENUMERABLE
            | match self {
                Self::Method => 0,
                Self::Getter => 1,
                Self::Setter => 2,
            }
    }
}

pub(in crate::lowering) fn same_operator_left_chain<'expression, 'arena>(
    logical: &'expression LogicalExpression<'arena>,
) -> Vec<&'expression Expression<'arena>> {
    let mut reversed = vec![&logical.right];
    let mut left = &logical.left;
    while let Expression::LogicalExpression(inner) = left
        && inner.operator == logical.operator
    {
        reversed.push(&inner.right);
        left = &inner.left;
    }
    reversed.push(left);
    reversed.reverse();
    reversed
}
