use oxc_ast::ast::VariableDeclarationKind;

#[derive(Clone, Copy)]
pub(in crate::lowering) enum DestructuringBindingInitialization {
    Declaration(VariableDeclarationKind),
    Parameter,
}
