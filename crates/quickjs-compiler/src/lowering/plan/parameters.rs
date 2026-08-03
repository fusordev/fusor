use oxc_semantic::ScopeId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::lowering) enum LogicalCompilerScope {
    Function,
    Body,
    Oxc(ScopeId),
}
