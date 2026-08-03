use quickjs_frontend::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::lowering) enum OrdinaryFunctionForm {
    Function,
    ObjectMethod { property_span: Span },
}
