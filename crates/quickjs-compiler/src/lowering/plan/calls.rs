use oxc_ast::ast::{ComputedMemberExpression, StaticMemberExpression};

#[derive(Clone, Copy)]
pub(in crate::lowering) enum MemberCallee<'expression, 'arena> {
    Static(&'expression StaticMemberExpression<'arena>),
    Computed(&'expression ComputedMemberExpression<'arena>),
}
