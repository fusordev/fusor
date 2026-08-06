pub(in crate::lowering) mod abrupt;
pub(in crate::lowering) mod bindings;
pub(in crate::lowering) mod calls;
pub(in crate::lowering) mod control;
pub(in crate::lowering) mod destructuring;
pub(in crate::lowering) mod expressions;
pub(in crate::lowering) mod parameters;
pub(in crate::lowering) mod statements;

pub(in crate::lowering) use bindings::{
    LoweredReference, ScopeEntryInitialization, compact_get_argument, compact_get_local,
    compact_put_local, plan_put_slot,
};
pub(in crate::lowering) use control::StatementControlStack;
pub(in crate::lowering) use destructuring::DestructuringBindingInitialization;
pub(in crate::lowering) use expressions::{
    ExpressionPlanner, ExpressionWork, anonymous_class_expression_span,
    anonymous_named_evaluation_span, anonymous_ordinary_function_span, binary_opcode, exact_i32,
    exact_negated_i32, plan_push_integer,
};
pub(in crate::lowering) use parameters::LogicalCompilerScope;
pub(in crate::lowering) use statements::{
    StatementCompletion, StatementPlanningState, StatementWork,
};
