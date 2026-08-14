//! Arena-independent ECMAScript module syntax used by later `QuickJS` lowering.
//!
//! Oxc's semantic model remains the source of truth for scopes, symbols, and
//! references. This module copies only the static module-request and
//! import/export entry data that must outlive Oxc's allocator.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use oxc_ast::ast::{
    ExportDefaultDeclarationKind, ImportAttributeKey as OxcImportAttributeKey, ImportPhase,
    Program, Statement, StringLiteral, WithClause,
};
use oxc_span::Span;
use oxc_syntax::module_record::{
    ExportEntry as OxcExportEntry, ExportExportName as OxcExportExportName,
    ExportImportName as OxcExportImportName, ExportLocalName as OxcExportLocalName,
    ImportEntry as OxcImportEntry, ImportImportName as OxcImportImportName,
    ModuleRecord as OxcModuleRecord, NameSpan as OxcNameSpan,
};

use crate::decode_oxc_cooked_string;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModuleSyntaxLoweringError {
    DuplicateRequestSpan {
        span: Span,
    },
    MissingRequest {
        span: Span,
    },
    MalformedOxcString {
        span: Span,
        encoded_offset: usize,
    },
    ExportRoleMismatch {
        span: Span,
        role: ModuleExportEntryRole,
        has_request: bool,
    },
    MissingExportStatement {
        span: Span,
    },
    UnexpectedDefaultLocalName {
        span: Span,
    },
}

impl ModuleSyntaxLoweringError {
    pub(crate) const fn span(&self) -> Span {
        match self {
            Self::DuplicateRequestSpan { span }
            | Self::MissingRequest { span }
            | Self::MalformedOxcString { span, .. }
            | Self::ExportRoleMismatch { span, .. }
            | Self::MissingExportStatement { span }
            | Self::UnexpectedDefaultLocalName { span } => *span,
        }
    }
}

impl fmt::Display for ModuleSyntaxLoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateRequestSpan { span } => write!(
                formatter,
                "Oxc produced duplicate static module-literal span {}..{}",
                span.start, span.end
            ),
            Self::MissingRequest { span } => write!(
                formatter,
                "Oxc module entry has no static request at module-literal span {}..{}",
                span.start, span.end
            ),
            Self::MalformedOxcString {
                span,
                encoded_offset,
            } => write!(
                formatter,
                "Oxc produced malformed lone-surrogate marker encoding at offset {encoded_offset} for string span {}..{}",
                span.start, span.end
            ),
            Self::ExportRoleMismatch {
                span,
                role,
                has_request,
            } => write!(
                formatter,
                "Oxc export entry at {}..{} has role {role:?} with module-request presence {has_request}",
                span.start, span.end
            ),
            Self::MissingExportStatement { span } => write!(
                formatter,
                "Oxc export entry span {}..{} is not contained in a top-level export declaration",
                span.start, span.end
            ),
            Self::UnexpectedDefaultLocalName { span } => write!(
                formatter,
                "Oxc produced a default local name outside a default-export expression at {}..{}",
                span.start, span.end
            ),
        }
    }
}

/// Index of one static module-request occurrence in a [`ModuleSyntaxRecord`].
///
/// Repeated module specifier strings have distinct indices because `QuickJS`
/// retains a request entry, including attributes, for every source occurrence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleRequestIndex(usize);

impl ModuleRequestIndex {
    /// Returns the zero-based request occurrence index.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// Owned text and its original half-open UTF-8 byte span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleNameSpan {
    code_units: Arc<[u16]>,
    span: Span,
}

impl ModuleNameSpan {
    fn from_well_formed_text(name: &str, span: Span) -> Self {
        Self {
            code_units: name.encode_utf16().collect::<Vec<_>>().into(),
            span,
        }
    }

    fn from_oxc(name: &OxcNameSpan<'_>) -> Self {
        // Oxc rejects lone surrogates in ModuleExportName before this lowering
        // runs; identifiers are necessarily well-formed Unicode.
        Self::from_well_formed_text(name.name.as_str(), name.span)
    }

    fn from_literal(literal: &StringLiteral<'_>) -> Result<Self, ModuleSyntaxLoweringError> {
        Ok(Self {
            code_units: decode_oxc_cooked_string(literal.value.as_str(), literal.lone_surrogates)
                .map_err(|source| ModuleSyntaxLoweringError::MalformedOxcString {
                span: literal.span,
                encoded_offset: source.encoded_offset(),
            })?,
            span: literal.span,
        })
    }

    /// Returns the exact ECMAScript UTF-16 code units.
    ///
    /// Unlike Rust UTF-8 strings, this representation preserves lone
    /// surrogates accepted by `QuickJS`.
    #[must_use]
    pub fn code_units(&self) -> &[u16] {
        &self.code_units
    }

    /// Returns whether these code units equal a well-formed Rust UTF-8 string.
    #[must_use]
    pub fn equals_utf8(&self, value: &str) -> bool {
        self.code_units.iter().copied().eq(value.encode_utf16())
    }

    /// Returns the original UTF-8 byte span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }
}

/// How one static module request participates in linking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModuleRequestKind {
    /// An `import` declaration, including a side-effect-only import.
    Import,
    /// A named `export { ... } from` declaration.
    NamedReExport,
    /// An `export * from` declaration.
    StarReExport,
    /// An `export * as name from` declaration.
    NamespaceReExport,
}

/// Keyword that introduced an import-attribute clause.
///
/// The `QuickJS` compatibility profile rejects legacy `assert` syntax before
/// this owned record is constructed. The variant remains explicit so this
/// syntax representation does not conflate the two Oxc AST forms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ImportAttributeKeyword {
    /// Standard `with { ... }` import attributes.
    With,
    /// Legacy `assert { ... }` syntax.
    Assert,
}

/// One owned import-attribute key/value pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportAttribute {
    span: Span,
    key: ModuleNameSpan,
    value: ModuleNameSpan,
}

impl ImportAttribute {
    /// Returns the complete key/value entry span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Returns the decoded key and its source span.
    #[must_use]
    pub const fn key(&self) -> &ModuleNameSpan {
        &self.key
    }

    /// Returns the decoded string value and its source span.
    #[must_use]
    pub const fn value(&self) -> &ModuleNameSpan {
        &self.value
    }
}

/// A syntactically present import-attribute clause.
///
/// An empty `with {}` is represented by `Some` with an empty entry slice,
/// preserving its distinction from an absent clause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportAttributes {
    span: Span,
    keyword: ImportAttributeKeyword,
    entries: Arc<[ImportAttribute]>,
}

impl ImportAttributes {
    fn from_oxc(clause: &WithClause<'_>) -> Result<Self, ModuleSyntaxLoweringError> {
        let keyword = match clause.keyword {
            oxc_ast::ast::WithClauseKeyword::With => ImportAttributeKeyword::With,
            oxc_ast::ast::WithClauseKeyword::Assert => ImportAttributeKeyword::Assert,
        };
        let entries = clause
            .with_entries
            .iter()
            .map(|entry| -> Result<_, ModuleSyntaxLoweringError> {
                let key = match &entry.key {
                    OxcImportAttributeKey::Identifier(identifier) => {
                        ModuleNameSpan::from_well_formed_text(
                            identifier.name.as_str(),
                            identifier.span,
                        )
                    }
                    OxcImportAttributeKey::StringLiteral(literal) => {
                        ModuleNameSpan::from_literal(literal)?
                    }
                };
                Ok(ImportAttribute {
                    span: entry.span,
                    key,
                    value: ModuleNameSpan::from_literal(&entry.value)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .into();
        Ok(Self {
            span: clause.span,
            keyword,
            entries,
        })
    }

    /// Returns the complete attribute-clause span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Returns the clause keyword.
    #[must_use]
    pub const fn keyword(&self) -> ImportAttributeKeyword {
        self.keyword
    }

    /// Returns attributes in source order.
    #[must_use]
    pub fn entries(&self) -> &[ImportAttribute] {
        &self.entries
    }
}

/// One source occurrence of a static module request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticModuleRequest {
    kind: ModuleRequestKind,
    statement_span: Span,
    specifier: ModuleNameSpan,
    attributes: Option<ImportAttributes>,
    deferred: bool,
}

impl StaticModuleRequest {
    /// Returns the request's import/re-export role.
    #[must_use]
    pub const fn kind(&self) -> ModuleRequestKind {
        self.kind
    }

    /// Returns whether the request was introduced by an `import defer`
    /// declaration (ECMA-262 `ModuleRequest` [[Phase]] ~defer~).
    #[must_use]
    pub const fn is_deferred(&self) -> bool {
        self.deferred
    }

    /// Returns the complete import/export declaration span.
    #[must_use]
    pub const fn statement_span(&self) -> Span {
        self.statement_span
    }

    /// Returns the decoded module specifier and its per-occurrence span.
    #[must_use]
    pub const fn specifier(&self) -> &ModuleNameSpan {
        &self.specifier
    }

    /// Returns the syntactically present attribute clause, if any.
    #[must_use]
    pub const fn attributes(&self) -> Option<&ImportAttributes> {
        self.attributes.as_ref()
    }
}

/// Imported name used by an import entry.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModuleImportName {
    /// A named import.
    Name(ModuleNameSpan),
    /// The default export, carrying the `default` source span.
    Default(Span),
    /// The requested module namespace object.
    Namespace,
    /// A deferred namespace object (`import defer * as ns`).
    DeferredNamespace,
}

/// One import entry needed during module linking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleImportEntry {
    statement_span: Span,
    request: ModuleRequestIndex,
    import_name: ModuleImportName,
    local_name: ModuleNameSpan,
}

impl ModuleImportEntry {
    /// Returns the complete import declaration span.
    #[must_use]
    pub const fn statement_span(&self) -> Span {
        self.statement_span
    }

    /// Returns the referenced static request occurrence.
    #[must_use]
    pub const fn request(&self) -> ModuleRequestIndex {
        self.request
    }

    /// Returns the requested export name or namespace role.
    #[must_use]
    pub const fn import_name(&self) -> &ModuleImportName {
        &self.import_name
    }

    /// Returns the local binding name and span.
    #[must_use]
    pub const fn local_name(&self) -> &ModuleNameSpan {
        &self.local_name
    }
}

/// Linking category of an export entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModuleExportEntryRole {
    /// An export backed by a binding in this module.
    Local,
    /// A named or namespace re-export backed by another module.
    Indirect,
    /// An `export *` entry, excluding `default`.
    Star,
}

/// Imported-side name used by an export entry.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModuleExportImportName {
    /// A named binding from the requested module.
    Name(ModuleNameSpan),
    /// The requested module's default export.
    Default(Span),
    /// The full namespace used by `export * as name`.
    All,
    /// Every exported name except `default`, used by `export *`.
    AllButDefault,
    /// No imported-side name for a local export.
    Null,
}

/// Public name exposed by an export entry.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModuleExportName {
    /// A named export.
    Name(ModuleNameSpan),
    /// The default export, carrying its source span.
    Default(Span),
    /// No direct name for an `export *` entry.
    Null,
}

/// Module-local name backing an export entry.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModuleExportLocalName {
    /// An ordinary local binding.
    Name(ModuleNameSpan),
    /// `QuickJS`'s internal default cell for an anonymous declaration or
    /// default-export expression.
    SyntheticDefault,
    /// No local binding for an indirect or star export.
    Null,
}

/// One source-text module export entry needed during linking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleExportEntry {
    role: ModuleExportEntryRole,
    statement_span: Span,
    span: Span,
    request: Option<ModuleRequestIndex>,
    import_name: ModuleExportImportName,
    export_name: ModuleExportName,
    local_name: ModuleExportLocalName,
}

impl ModuleExportEntry {
    /// Returns whether this entry is local, indirect, or star.
    #[must_use]
    pub const fn role(&self) -> ModuleExportEntryRole {
        self.role
    }

    /// Returns the complete export declaration span.
    #[must_use]
    pub const fn statement_span(&self) -> Span {
        self.statement_span
    }

    /// Returns the individual export-entry span.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Returns the referenced static request for indirect/star exports.
    #[must_use]
    pub const fn request(&self) -> Option<ModuleRequestIndex> {
        self.request
    }

    /// Returns the imported-side role or name.
    #[must_use]
    pub const fn import_name(&self) -> &ModuleExportImportName {
        &self.import_name
    }

    /// Returns the public export name.
    #[must_use]
    pub const fn export_name(&self) -> &ModuleExportName {
        &self.export_name
    }

    /// Returns the module-local binding name.
    #[must_use]
    pub const fn local_name(&self) -> &ModuleExportLocalName {
        &self.local_name
    }
}

/// QuickJS-owned, arena-independent static module syntax.
///
/// Cloning this value shares immutable request and entry storage through
/// [`Arc`]. No lock is required. Scope, symbol, reference, class, and
/// private-name data deliberately remain in Oxc's authoritative semantic
/// model and are not duplicated here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleSyntaxRecord {
    has_module_syntax: bool,
    requests: Arc<[StaticModuleRequest]>,
    import_entries: Arc<[ModuleImportEntry]>,
    export_entries: Arc<[ModuleExportEntry]>,
}

impl ModuleSyntaxRecord {
    pub(crate) fn from_oxc(
        program: &Program<'_>,
        module_record: &OxcModuleRecord<'_>,
    ) -> Result<Self, ModuleSyntaxLoweringError> {
        let (requests, request_by_specifier) = lower_requests(program)?;
        let export_statements = export_statements(program);
        let deferred_spans: HashSet<(u32, u32)> = program
            .body
            .iter()
            .filter_map(|statement| {
                let Statement::ImportDeclaration(declaration) = statement else {
                    return None;
                };
                (declaration.phase == Some(ImportPhase::Defer))
                    .then_some((declaration.span.start, declaration.span.end))
            })
            .collect();
        let import_entries = module_record
            .import_entries
            .iter()
            .map(|entry| lower_import_entry(entry, &request_by_specifier, &deferred_spans))
            .collect::<Result<Vec<_>, _>>()?;

        let mut export_entries = Vec::with_capacity(
            module_record.local_export_entries.len()
                + module_record.indirect_export_entries.len()
                + module_record.star_export_entries.len(),
        );
        export_entries.extend(
            module_record
                .local_export_entries
                .iter()
                .map(|entry| {
                    lower_export_entry(
                        entry,
                        ModuleExportEntryRole::Local,
                        &request_by_specifier,
                        &export_statements,
                        &module_record.import_entries,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        export_entries.extend(
            module_record
                .indirect_export_entries
                .iter()
                .map(|entry| {
                    lower_export_entry(
                        entry,
                        ModuleExportEntryRole::Indirect,
                        &request_by_specifier,
                        &export_statements,
                        &module_record.import_entries,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        export_entries.extend(
            module_record
                .star_export_entries
                .iter()
                .map(|entry| {
                    lower_export_entry(
                        entry,
                        ModuleExportEntryRole::Star,
                        &request_by_specifier,
                        &export_statements,
                        &module_record.import_entries,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        // ECMA-262 ParseModule step 10.i.ii: a local export entry whose local
        // name is an imported binding is really an *indirect* export of the
        // import's module request and import name (a namespace import maps to
        // ~all~). Oxc keeps such entries local; rewrite them so linking
        // resolves through the exporting module, making two re-exports of the
        // same namespace object or binding unambiguous.
        for entry in &mut export_entries {
            if entry.role != ModuleExportEntryRole::Local {
                continue;
            }
            let ModuleExportLocalName::Name(local_name) = &entry.local_name else {
                continue;
            };
            let Some(import) = import_entries
                .iter()
                .find(|import| import.local_name.code_units() == local_name.code_units())
            else {
                continue;
            };
            *entry = ModuleExportEntry {
                role: ModuleExportEntryRole::Indirect,
                statement_span: entry.statement_span,
                span: entry.span,
                request: Some(import.request),
                import_name: match &import.import_name {
                    ModuleImportName::Name(name) => ModuleExportImportName::Name(name.clone()),
                    ModuleImportName::Default(span) => ModuleExportImportName::Default(*span),
                    ModuleImportName::Namespace => ModuleExportImportName::All,
                    // A deferred namespace import keeps its local entry: the
                    // binding cell holds the module's deferred namespace object
                    // (identity-cached per module), and re-exporting it must
                    // forward that exact object, not a star-resolution view.
                    ModuleImportName::DeferredNamespace => continue,
                },
                export_name: entry.export_name.clone(),
                local_name: ModuleExportLocalName::Null,
            };
        }
        export_entries.sort_by_key(|entry| {
            (
                entry.span.start,
                entry.span.end,
                export_role_order(entry.role),
            )
        });

        Ok(Self {
            has_module_syntax: module_record.has_module_syntax,
            requests: requests.into(),
            import_entries: import_entries.into(),
            export_entries: export_entries.into(),
        })
    }

    /// Returns whether Oxc observed module syntax.
    ///
    /// This can be true for `import.meta` even when the static request and
    /// entry slices are empty.
    #[must_use]
    pub const fn has_module_syntax(&self) -> bool {
        self.has_module_syntax
    }

    /// Returns static module request occurrences in source order.
    #[must_use]
    pub fn requests(&self) -> &[StaticModuleRequest] {
        &self.requests
    }

    /// Resolves a typed request occurrence index.
    #[must_use]
    pub fn request(&self, index: ModuleRequestIndex) -> Option<&StaticModuleRequest> {
        self.requests.get(index.0)
    }

    /// Returns import entries in source order.
    #[must_use]
    pub fn import_entries(&self) -> &[ModuleImportEntry] {
        &self.import_entries
    }

    /// Returns local, indirect, and star export entries in source order.
    #[must_use]
    pub fn export_entries(&self) -> &[ModuleExportEntry] {
        &self.export_entries
    }
}

type RequestBySpecifier = HashMap<(u32, u32), ModuleRequestIndex>;

fn lower_attributes(
    clause: Option<&WithClause<'_>>,
) -> Result<Option<ImportAttributes>, ModuleSyntaxLoweringError> {
    clause.map(ImportAttributes::from_oxc).transpose()
}

fn lower_requests(
    program: &Program<'_>,
) -> Result<(Vec<StaticModuleRequest>, RequestBySpecifier), ModuleSyntaxLoweringError> {
    let mut requests = Vec::new();
    let mut request_by_specifier = HashMap::new();

    for statement in &program.body {
        let request = match statement {
            Statement::ImportDeclaration(declaration) => Some(StaticModuleRequest {
                kind: ModuleRequestKind::Import,
                statement_span: declaration.span,
                specifier: ModuleNameSpan::from_literal(&declaration.source)?,
                attributes: lower_attributes(declaration.with_clause.as_deref())?,
                deferred: declaration.phase == Some(ImportPhase::Defer),
            }),
            Statement::ExportNamedDeclaration(declaration) => declaration
                .source
                .as_ref()
                .map(|source| -> Result<_, ModuleSyntaxLoweringError> {
                    Ok(StaticModuleRequest {
                        kind: ModuleRequestKind::NamedReExport,
                        statement_span: declaration.span,
                        specifier: ModuleNameSpan::from_literal(source)?,
                        attributes: lower_attributes(declaration.with_clause.as_deref())?,
                        deferred: false,
                    })
                })
                .transpose()?,
            Statement::ExportAllDeclaration(declaration) => Some(StaticModuleRequest {
                kind: if declaration.exported.is_some() {
                    ModuleRequestKind::NamespaceReExport
                } else {
                    ModuleRequestKind::StarReExport
                },
                statement_span: declaration.span,
                specifier: ModuleNameSpan::from_literal(&declaration.source)?,
                attributes: lower_attributes(declaration.with_clause.as_deref())?,
                deferred: false,
            }),
            _ => None,
        };

        if let Some(request) = request {
            let index = ModuleRequestIndex(requests.len());
            if request_by_specifier
                .insert(span_key(request.specifier.span), index)
                .is_some()
            {
                return Err(ModuleSyntaxLoweringError::DuplicateRequestSpan {
                    span: request.specifier.span,
                });
            }
            requests.push(request);
        }
    }

    Ok((requests, request_by_specifier))
}

#[derive(Clone, Copy)]
struct ExportStatementInfo {
    span: Span,
    synthetic_default: bool,
}

fn export_statements(program: &Program<'_>) -> Vec<ExportStatementInfo> {
    program
        .body
        .iter()
        .filter_map(|statement| match statement {
            Statement::ExportAllDeclaration(declaration) => Some(ExportStatementInfo {
                span: declaration.span,
                synthetic_default: false,
            }),
            Statement::ExportDefaultDeclaration(declaration) => Some(ExportStatementInfo {
                span: declaration.span,
                synthetic_default: match &declaration.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
                        function.id.is_none()
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(class) => class.id.is_none(),
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => false,
                    _ => true,
                },
            }),
            Statement::ExportNamedDeclaration(declaration) => Some(ExportStatementInfo {
                span: declaration.span,
                synthetic_default: false,
            }),
            _ => None,
        })
        .collect()
}

fn containing_export_statement(
    export_statements: &[ExportStatementInfo],
    entry_span: Span,
) -> Option<ExportStatementInfo> {
    let insertion =
        export_statements.partition_point(|statement| statement.span.start <= entry_span.start);
    let candidate = *export_statements.get(insertion.checked_sub(1)?)?;
    (candidate.span.start <= entry_span.start && entry_span.end <= candidate.span.end)
        .then_some(candidate)
}

fn lower_import_entry(
    entry: &OxcImportEntry<'_>,
    request_by_specifier: &RequestBySpecifier,
    deferred_spans: &HashSet<(u32, u32)>,
) -> Result<ModuleImportEntry, ModuleSyntaxLoweringError> {
    let deferred = deferred_spans.contains(&(entry.statement_span.start, entry.statement_span.end));
    Ok(ModuleImportEntry {
        statement_span: entry.statement_span,
        request: request_index(request_by_specifier, entry.module_request.span)?,
        import_name: match &entry.import_name {
            OxcImportImportName::Name(name) => {
                ModuleImportName::Name(ModuleNameSpan::from_oxc(name))
            }
            OxcImportImportName::NamespaceObject if deferred => ModuleImportName::DeferredNamespace,
            OxcImportImportName::NamespaceObject => ModuleImportName::Namespace,
            OxcImportImportName::Default(span) => ModuleImportName::Default(*span),
        },
        local_name: ModuleNameSpan::from_oxc(&entry.local_name),
    })
}

fn lower_export_entry(
    entry: &OxcExportEntry<'_>,
    role: ModuleExportEntryRole,
    request_by_specifier: &RequestBySpecifier,
    export_statements: &[ExportStatementInfo],
    import_entries: &[OxcImportEntry<'_>],
) -> Result<ModuleExportEntry, ModuleSyntaxLoweringError> {
    let request = entry
        .module_request
        .as_ref()
        .map(|request| request_index(request_by_specifier, request.span))
        .transpose()?;
    let expected_request = role != ModuleExportEntryRole::Local;
    if request.is_some() != expected_request {
        return Err(ModuleSyntaxLoweringError::ExportRoleMismatch {
            span: entry.span,
            role,
            has_request: request.is_some(),
        });
    }
    let statement = containing_export_statement(export_statements, entry.span)
        .ok_or(ModuleSyntaxLoweringError::MissingExportStatement { span: entry.span })?;
    let statement_span = statement.span;
    let request_span = entry.module_request.as_ref().map(|request| request.span);
    let local_name = if statement.synthetic_default
        && matches!(entry.export_name, OxcExportExportName::Default(_))
    {
        ModuleExportLocalName::SyntheticDefault
    } else {
        match &entry.local_name {
            OxcExportLocalName::Name(name) => {
                ModuleExportLocalName::Name(ModuleNameSpan::from_oxc(name))
            }
            OxcExportLocalName::Default(_) => {
                return Err(ModuleSyntaxLoweringError::UnexpectedDefaultLocalName {
                    span: entry.span,
                });
            }
            OxcExportLocalName::Null => ModuleExportLocalName::Null,
        }
    };

    Ok(ModuleExportEntry {
        role,
        statement_span,
        span: entry.span,
        request,
        import_name: match &entry.import_name {
            OxcExportImportName::Name(name) => import_entries
                .iter()
                .find_map(|import| {
                    let OxcImportImportName::Default(default_span) = &import.import_name else {
                        return None;
                    };
                    (import.statement_span == entry.statement_span
                        && Some(import.module_request.span) == request_span
                        && import.local_name.span == name.span)
                        .then_some(ModuleExportImportName::Default(*default_span))
                })
                .unwrap_or_else(|| ModuleExportImportName::Name(ModuleNameSpan::from_oxc(name))),
            OxcExportImportName::All => ModuleExportImportName::All,
            OxcExportImportName::AllButDefault => ModuleExportImportName::AllButDefault,
            OxcExportImportName::Null => ModuleExportImportName::Null,
        },
        export_name: match &entry.export_name {
            OxcExportExportName::Name(name) => {
                ModuleExportName::Name(ModuleNameSpan::from_oxc(name))
            }
            OxcExportExportName::Default(span) => ModuleExportName::Default(*span),
            OxcExportExportName::Null => ModuleExportName::Null,
        },
        local_name,
    })
}

fn request_index(
    request_by_specifier: &RequestBySpecifier,
    specifier_span: Span,
) -> Result<ModuleRequestIndex, ModuleSyntaxLoweringError> {
    request_by_specifier
        .get(&span_key(specifier_span))
        .copied()
        .ok_or(ModuleSyntaxLoweringError::MissingRequest {
            span: specifier_span,
        })
}

const fn span_key(span: Span) -> (u32, u32) {
    (span.start, span.end)
}

const fn export_role_order(role: ModuleExportEntryRole) -> u8 {
    match role {
        ModuleExportEntryRole::Local => 0,
        ModuleExportEntryRole::Indirect => 1,
        ModuleExportEntryRole::Star => 2,
    }
}
