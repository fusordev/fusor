//! The pinned `QuickJS` grammar production ledger.
//!
//! Productions are enumerated from the pinned `QuickJS` 2026-06-04 parser's own
//! dispatch structure rather than from an outside grammar summary, so the
//! ledger's accepted-grammar vocabulary tracks the parser it describes. Each
//! entry names the `quickjs.c` function or `case` that parses it.
//!
//! The vocabulary is closed in both directions: a fixture may only declare a
//! production listed here, and every production must be exercised by at least
//! one fixture the pinned oracle accepts. Rejection alone is not coverage; a
//! production is covered only when both engines accept the construct.

/// One grammar production the pinned parser recognizes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PinnedProduction {
    /// Stable ledger identifier used by `manifest.json`.
    pub(crate) id: &'static str,
    /// The ECMAScript productions this entry covers.
    pub(crate) grammar: &'static str,
    /// `quickjs.c` anchors that parse it.
    pub(crate) sites: &'static [&'static str],
    /// Parser goals under which the production can appear.
    pub(crate) goals: ProductionGoals,
}

/// Which parse goals admit a production.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductionGoals {
    /// Any goal admits it.
    Any,
    /// Only the Module goal admits it.
    ModuleOnly,
    /// Only sloppy (non-strict) goals admit it.
    SloppyOnly,
    /// Only goals whose top level allows `await` admit it.
    AwaitCapable,
}

/// Every grammar production the pinned parser recognizes.
pub(crate) const PINNED_PRODUCTIONS: [PinnedProduction; 72] = [
    // Source text and lexical grammar.
    PinnedProduction {
        id: "lexical.hashbang",
        grammar: "HashbangComment",
        sites: &["quickjs.c:37013"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.comments",
        grammar: "SingleLineComment, MultiLineComment",
        sites: &["quickjs.c:22770"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.identifier-escapes",
        grammar: "IdentifierName with UnicodeEscapeSequence, non-ASCII identifiers",
        sites: &["quickjs.c:22645"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.numeric-literal",
        grammar: "DecimalLiteral, BinaryIntegerLiteral, OctalIntegerLiteral, \
                  HexIntegerLiteral, NumericLiteralSeparator",
        sites: &["quickjs.c:22928"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.bigint-literal",
        grammar: "NumericLiteral with BigIntLiteralSuffix",
        sites: &["quickjs.c:22905"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.string-literal",
        grammar: "StringLiteral with escape sequences",
        sites: &["quickjs.c:22344"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.template-literal",
        grammar: "Template, TemplateSubstitutionTail",
        sites: &["quickjs.c:22281", "quickjs.c:24374"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.regexp-literal",
        grammar: "RegularExpressionLiteral boundary and flags",
        sites: &["quickjs.c:22486"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.private-name",
        grammar: "PrivateIdentifier",
        sites: &["quickjs.c:22878"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "lexical.asi",
        grammar: "Automatic semicolon insertion",
        sites: &["quickjs.c:22261"],
        goals: ProductionGoals::Any,
    },
    // Bindings.
    PinnedProduction {
        id: "binding.var",
        grammar: "VariableStatement",
        sites: &["quickjs.c:28398"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "binding.lexical",
        grammar: "LexicalDeclaration for `let` and `const`",
        sites: &["quickjs.c:28398"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "binding.array-pattern",
        grammar: "ArrayBindingPattern with elisions, defaults, and BindingRestElement",
        sites: &["quickjs.c:26500"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "binding.object-pattern",
        grammar: "ObjectBindingPattern with computed keys, defaults, and BindingRestProperty",
        sites: &["quickjs.c:26240"],
        goals: ProductionGoals::Any,
    },
    // Functions.
    PinnedProduction {
        id: "function.declaration",
        grammar: "FunctionDeclaration",
        sites: &["quickjs.c:36951"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "function.expression",
        grammar: "FunctionExpression, named function expression",
        sites: &["quickjs.c:36383"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "function.arrow",
        grammar: "ArrowFunction, AsyncArrowFunction",
        sites: &["quickjs.c:36566"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "function.generator",
        grammar: "GeneratorDeclaration, GeneratorExpression, YieldExpression",
        sites: &["quickjs.c:27882"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "function.async",
        grammar: "AsyncFunctionDeclaration, AsyncFunctionExpression, AwaitExpression",
        sites: &["quickjs.c:27476"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "function.async-generator",
        grammar: "AsyncGeneratorDeclaration, AsyncGeneratorExpression",
        sites: &["quickjs.c:36383"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "function.parameters",
        grammar: "FormalParameters with defaults, patterns, and FunctionRestParameter",
        sites: &["quickjs.c:36600"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "function.directives",
        grammar: "Directive Prologue, `use strict`",
        sites: &["quickjs.c:36210"],
        goals: ProductionGoals::Any,
    },
    // Expressions.
    PinnedProduction {
        id: "expression.primary",
        grammar: "this, IdentifierReference, Literal, ParenthesizedExpression",
        sites: &["quickjs.c:26715"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.array-literal",
        grammar: "ArrayLiteral with Elision and SpreadElement",
        sites: &["quickjs.c:25669"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.object-literal",
        grammar: "ObjectLiteral with shorthand, computed keys, methods, and accessors",
        sites: &["quickjs.c:24850"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.member",
        grammar: "MemberExpression, computed member, private member access",
        sites: &["quickjs.c:27200"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.call",
        grammar: "CallExpression, Arguments with SpreadElement",
        sites: &["quickjs.c:27143"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.new",
        grammar: "NewExpression",
        sites: &["quickjs.c:26880"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.new-target",
        grammar: "NewTarget",
        sites: &["quickjs.c:26893"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.optional-chain",
        grammar: "OptionalExpression, OptionalChain",
        sites: &["quickjs.c:26985"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.tagged-template",
        grammar: "TaggedTemplate",
        sites: &["quickjs.c:24374"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.unary",
        grammar: "UnaryExpression with delete, void, typeof, and sign operators",
        sites: &["quickjs.c:27476"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.update",
        grammar: "UpdateExpression, prefix and postfix",
        sites: &["quickjs.c:27476"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.binary",
        grammar: "ExponentiationExpression through RelationalExpression, `in`, `instanceof`",
        sites: &["quickjs.c:27616"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.logical",
        grammar: "LogicalANDExpression, LogicalORExpression",
        sites: &["quickjs.c:27783"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.coalesce",
        grammar: "CoalesceExpression",
        sites: &["quickjs.c:27825"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.conditional",
        grammar: "ConditionalExpression",
        sites: &["quickjs.c:27853"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.assignment",
        grammar: "AssignmentExpression with compound and logical assignment operators",
        sites: &["quickjs.c:27882"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.destructuring-assignment",
        grammar: "ArrayAssignmentPattern, ObjectAssignmentPattern",
        sites: &["quickjs.c:26221"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.sequence",
        grammar: "Expression with the comma operator",
        sites: &["quickjs.c:28172"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.super",
        grammar: "SuperCall, SuperProperty",
        sites: &["quickjs.c:26919"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.import-call",
        grammar: "ImportCall",
        sites: &["quickjs.c:26946"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "expression.import-meta",
        grammar: "ImportMeta",
        sites: &["quickjs.c:26936"],
        goals: ProductionGoals::ModuleOnly,
    },
    // Classes.
    PinnedProduction {
        id: "class.declaration",
        grammar: "ClassDeclaration",
        sites: &["quickjs.c:25157"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "class.expression",
        grammar: "ClassExpression",
        sites: &["quickjs.c:25157"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "class.heritage",
        grammar: "ClassHeritage, derived constructor",
        sites: &["quickjs.c:25200"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "class.methods",
        grammar: "MethodDefinition with accessors, generators, and async methods",
        sites: &["quickjs.c:25322"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "class.fields",
        grammar: "FieldDefinition with Initializer",
        sites: &["quickjs.c:25396"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "class.private",
        grammar: "Private class elements and RelationalExpression `#x in obj`",
        sites: &["quickjs.c:25508"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "class.static",
        grammar: "static MethodDefinition, static FieldDefinition, ClassStaticBlock",
        sites: &["quickjs.c:28848"],
        goals: ProductionGoals::Any,
    },
    // Statements.
    PinnedProduction {
        id: "statement.block",
        grammar: "Block, StatementList, EmptyStatement",
        sites: &["quickjs.c:28378"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.expression",
        grammar: "ExpressionStatement",
        sites: &["quickjs.c:28784"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.if",
        grammar: "IfStatement with an else clause",
        sites: &["quickjs.c:28900"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.iteration",
        grammar: "DoWhileStatement, WhileStatement, ForStatement",
        sites: &["quickjs.c:28960"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.for-in-of",
        grammar: "ForInStatement, ForOfStatement",
        sites: &["quickjs.c:28548"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.for-await-of",
        grammar: "ForAwaitOfStatement",
        sites: &["quickjs.c:29022"],
        goals: ProductionGoals::AwaitCapable,
    },
    PinnedProduction {
        id: "statement.continue-break",
        grammar: "ContinueStatement, BreakStatement, with and without labels",
        sites: &["quickjs.c:28200"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.return",
        grammar: "ReturnStatement",
        sites: &["quickjs.c:28840"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.labelled",
        grammar: "LabelledStatement",
        sites: &["quickjs.c:28790"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.switch",
        grammar: "SwitchStatement, CaseClause, DefaultClause",
        sites: &["quickjs.c:29180"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.throw-try",
        grammar: "ThrowStatement, TryStatement, Catch with and without a binding, Finally",
        sites: &["quickjs.c:28870", "quickjs.c:29280"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.debugger",
        grammar: "DebuggerStatement",
        sites: &["quickjs.c:29440"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "statement.with",
        grammar: "WithStatement",
        sites: &["quickjs.c:29400"],
        goals: ProductionGoals::SloppyOnly,
    },
    // Annex B.
    PinnedProduction {
        id: "annex-b.html-comment",
        grammar: "HTMLOpenComment, SingleLineHTMLCloseComment",
        sites: &["quickjs.c:23050"],
        goals: ProductionGoals::Any,
    },
    PinnedProduction {
        id: "annex-b.block-function",
        grammar: "Block-level and if-clause FunctionDeclaration in sloppy code",
        sites: &["quickjs.c:28370"],
        goals: ProductionGoals::SloppyOnly,
    },
    PinnedProduction {
        id: "annex-b.legacy-octal",
        grammar: "LegacyOctalIntegerLiteral, LegacyOctalEscapeSequence",
        sites: &["quickjs.c:22419", "quickjs.c:22905"],
        goals: ProductionGoals::SloppyOnly,
    },
    // Modules.
    PinnedProduction {
        id: "module.import-declaration",
        grammar: "ImportDeclaration with default, namespace, and named bindings",
        sites: &["quickjs.c:31782"],
        goals: ProductionGoals::ModuleOnly,
    },
    PinnedProduction {
        id: "module.export-declaration",
        grammar: "ExportDeclaration with named, default, and `export *` forms",
        sites: &["quickjs.c:31582"],
        goals: ProductionGoals::ModuleOnly,
    },
    PinnedProduction {
        id: "module.re-export",
        grammar: "ExportFromClause, FromClause",
        sites: &["quickjs.c:31548"],
        goals: ProductionGoals::ModuleOnly,
    },
    PinnedProduction {
        id: "module.string-names",
        grammar: "ModuleExportName as a StringLiteral",
        sites: &["quickjs.c:31613"],
        goals: ProductionGoals::ModuleOnly,
    },
    PinnedProduction {
        id: "module.import-attributes",
        grammar: "WithClause, WithEntries",
        sites: &["quickjs.c:31477"],
        goals: ProductionGoals::ModuleOnly,
    },
    PinnedProduction {
        id: "module.top-level-await",
        grammar: "Module-level AwaitExpression",
        sites: &["quickjs.c:36543"],
        goals: ProductionGoals::AwaitCapable,
    },
];
