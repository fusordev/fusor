use fusor_frontend::{
    Allocator, DiagnosticStage, FrontendDiagnosticCode, FrontendOptions, ModuleExportEntryRole,
    ModuleExportImportName, ModuleExportLocalName, ModuleExportName, ModuleImportName,
    ModuleRequestKind, ModuleSyntaxRecord, ParseMode, parse, with_parsed_program,
};

#[test]
fn lowers_source_ordered_static_requests_with_per_occurrence_attributes() {
    let source = r#"
        import "./same.js" with { type: "json" };
        import primary, { item as local } from "./same.js" with { mode: "strict" };
        export { remote as renamed } from "./remote.js" with { type: "json" };
        export * from "./star.js";
        export * as namespace from "./namespace.js" with {};
    "#;
    let allocator = Allocator::new();
    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Module)).expect("valid module");
    let syntax = unit.module_syntax();

    assert!(syntax.has_module_syntax());
    assert_eq!(
        syntax
            .requests()
            .iter()
            .map(|request| {
                String::from_utf16(request.specifier().code_units())
                    .expect("test specifiers are well-formed")
            })
            .collect::<Vec<_>>(),
        vec![
            "./same.js",
            "./same.js",
            "./remote.js",
            "./star.js",
            "./namespace.js",
        ]
    );
    assert_eq!(
        syntax
            .requests()
            .iter()
            .map(fusor_frontend::StaticModuleRequest::kind)
            .collect::<Vec<_>>(),
        vec![
            ModuleRequestKind::Import,
            ModuleRequestKind::Import,
            ModuleRequestKind::NamedReExport,
            ModuleRequestKind::StarReExport,
            ModuleRequestKind::NamespaceReExport,
        ]
    );

    for pair in syntax.requests().windows(2) {
        assert!(pair[0].statement_span().start < pair[1].statement_span().start);
    }
    assert_ne!(
        syntax.requests()[0].specifier().span(),
        syntax.requests()[1].specifier().span(),
        "repeated specifiers remain distinct source occurrences"
    );

    let first_attributes = syntax.requests()[0]
        .attributes()
        .expect("first import has attributes");
    assert_eq!(first_attributes.entries().len(), 1);
    assert!(first_attributes.entries()[0].key().equals_utf8("type"));
    assert!(first_attributes.entries()[0].value().equals_utf8("json"));
    assert!(
        first_attributes.span().start < first_attributes.entries()[0].span().start,
        "the clause and entry retain separate source ranges"
    );

    let second_attributes = syntax.requests()[1]
        .attributes()
        .expect("second import has attributes");
    assert!(second_attributes.entries()[0].key().equals_utf8("mode"));
    assert!(second_attributes.entries()[0].value().equals_utf8("strict"));
    assert!(syntax.requests()[3].attributes().is_none());
    assert!(
        syntax.requests()[4]
            .attributes()
            .expect("empty attribute clause remains syntactically present")
            .entries()
            .is_empty()
    );
}

#[test]
fn decodes_oxc_lone_surrogate_markers_without_conflating_replacement_characters() {
    let source = "import \"\\uD800\u{fffd}\" with { \"\\uD801\u{fffd}\": \"\\uD802\u{fffd}\" };";
    let allocator = Allocator::new();
    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Module)).expect("valid module");
    let request = &unit.module_syntax().requests()[0];

    assert_eq!(request.specifier().code_units(), &[0xd800, 0xfffd]);
    let attributes = request.attributes().expect("attributes");
    assert_eq!(
        attributes.entries()[0].key().code_units(),
        &[0xd801, 0xfffd]
    );
    assert_eq!(
        attributes.entries()[0].value().code_units(),
        &[0xd802, 0xfffd]
    );
}

#[test]
fn retains_quickjs_accepted_string_import_and_export_names() {
    let source = r#"
        import { "remote" as imported } from "./dep.js";
        const local = 1;
        export { local as "public" };
    "#;
    let allocator = Allocator::new();
    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Module)).expect("valid module");
    let syntax = unit.module_syntax();

    assert!(matches!(
        syntax.import_entries()[0].import_name(),
        ModuleImportName::Name(name) if name.equals_utf8("remote")
    ));
    assert!(syntax.export_entries().iter().any(|entry| {
        matches!(
            entry.export_name(),
            ModuleExportName::Name(name) if name.equals_utf8("public")
        )
    }));
}

#[test]
fn owns_import_and_export_linking_roles_after_the_oxc_arena_is_dropped() {
    let source = r#"
        import primary, { item as local } from "./imports.js";
        import * as namespace from "./namespace.js";
        const localValue = 1;
        export { localValue as exposed };
        export { remote as renamed } from "./remote.js";
        export * from "./star.js";
        export * as forwarded from "./forwarded.js";
        export default localValue;
    "#;

    let syntax = with_parsed_program(source, FrontendOptions::new(ParseMode::Module), |unit| {
        unit.module_syntax().clone()
    })
    .expect("valid module");

    assert_owned_import_entries(&syntax);

    assert_eq!(
        syntax
            .export_entries()
            .iter()
            .map(fusor_frontend::ModuleExportEntry::role)
            .collect::<Vec<_>>(),
        vec![
            ModuleExportEntryRole::Local,
            ModuleExportEntryRole::Indirect,
            ModuleExportEntryRole::Star,
            ModuleExportEntryRole::Indirect,
            ModuleExportEntryRole::Local,
        ]
    );

    let local = &syntax.export_entries()[0];
    assert!(matches!(
        local.local_name(),
        ModuleExportLocalName::Name(name) if name.equals_utf8("localValue")
    ));
    assert!(matches!(
        local.export_name(),
        ModuleExportName::Name(name) if name.equals_utf8("exposed")
    ));
    assert!(local.request().is_none());

    let indirect = &syntax.export_entries()[1];
    assert!(matches!(
        indirect.import_name(),
        ModuleExportImportName::Name(name) if name.equals_utf8("remote")
    ));
    assert!(matches!(
        indirect.export_name(),
        ModuleExportName::Name(name) if name.equals_utf8("renamed")
    ));
    assert!(
        syntax
            .request(indirect.request().expect("indirect request"))
            .expect("entry request belongs to this record")
            .specifier()
            .equals_utf8("./remote.js")
    );

    let star = &syntax.export_entries()[2];
    assert!(matches!(
        star.import_name(),
        ModuleExportImportName::AllButDefault
    ));
    assert!(matches!(star.export_name(), ModuleExportName::Null));
    assert!(matches!(star.local_name(), ModuleExportLocalName::Null));

    let namespace_reexport = &syntax.export_entries()[3];
    assert!(matches!(
        namespace_reexport.import_name(),
        ModuleExportImportName::All
    ));
    assert!(matches!(
        namespace_reexport.export_name(),
        ModuleExportName::Name(name) if name.equals_utf8("forwarded")
    ));

    let default_export = &syntax.export_entries()[4];
    assert!(matches!(
        default_export.export_name(),
        ModuleExportName::Default(_)
    ));
    assert!(matches!(
        default_export.local_name(),
        ModuleExportLocalName::SyntheticDefault
    ));
}

fn assert_owned_import_entries(syntax: &ModuleSyntaxRecord) {
    assert_eq!(syntax.import_entries().len(), 3);
    assert!(matches!(
        syntax.import_entries()[0].import_name(),
        ModuleImportName::Default(_)
    ));
    assert!(
        syntax.import_entries()[0]
            .local_name()
            .equals_utf8("primary")
    );
    assert!(matches!(
        syntax.import_entries()[1].import_name(),
        ModuleImportName::Name(name) if name.equals_utf8("item")
    ));
    assert!(syntax.import_entries()[1].local_name().equals_utf8("local"));
    assert!(matches!(
        syntax.import_entries()[2].import_name(),
        ModuleImportName::Namespace
    ));
    assert!(
        syntax
            .request(syntax.import_entries()[2].request())
            .expect("entry request belongs to this record")
            .specifier()
            .equals_utf8("./namespace.js")
    );
}

#[test]
fn default_exports_use_quickjs_synthetic_or_declared_local_cells() {
    for source in [
        "const value = 1; export default value;",
        "export default 1;",
        "export default function() {}",
        "export default class {}",
    ] {
        let allocator = Allocator::new();
        let unit = parse(&allocator, source, FrontendOptions::new(ParseMode::Module))
            .expect("valid default export");
        assert!(matches!(
            unit.module_syntax().export_entries()[0].local_name(),
            ModuleExportLocalName::SyntheticDefault
        ));
    }

    for (source, expected_name) in [
        ("export default function declared() {}", "declared"),
        ("export default class Declared {}", "Declared"),
    ] {
        let allocator = Allocator::new();
        let unit = parse(&allocator, source, FrontendOptions::new(ParseMode::Module))
            .expect("valid named default declaration");
        assert!(matches!(
            unit.module_syntax().export_entries()[0].local_name(),
            ModuleExportLocalName::Name(name) if name.equals_utf8(expected_name)
        ));
    }
}

#[test]
fn imported_binding_reexports_keep_the_import_request_and_export_source_order() {
    let source = r#"
        import { item } from "./dep.js";
        import dependencyDefault from "./default.js";
        export { item as forwarded };
        export { dependencyDefault as defaultForwarded };
        export const after = 1;
    "#;
    let allocator = Allocator::new();
    let unit =
        parse(&allocator, source, FrontendOptions::new(ParseMode::Module)).expect("valid module");
    let syntax = unit.module_syntax();

    assert_eq!(syntax.export_entries().len(), 3);
    let forwarded = &syntax.export_entries()[0];
    assert_eq!(forwarded.role(), ModuleExportEntryRole::Indirect);
    assert!(
        syntax
            .request(forwarded.request().expect("import request"))
            .expect("entry request belongs to this record")
            .specifier()
            .equals_utf8("./dep.js")
    );
    assert!(
        syntax.import_entries()[0].statement_span().end < forwarded.statement_span().start,
        "the linking request comes from the import while the export statement span stays at the re-export"
    );
    assert!(matches!(
        forwarded.import_name(),
        ModuleExportImportName::Name(name) if name.equals_utf8("item")
    ));

    let default_forwarded = &syntax.export_entries()[1];
    assert_eq!(default_forwarded.role(), ModuleExportEntryRole::Indirect);
    assert!(matches!(
        default_forwarded.import_name(),
        ModuleExportImportName::Default(_)
    ));
    assert!(
        syntax
            .request(default_forwarded.request().expect("default import request"))
            .expect("entry request belongs to this record")
            .specifier()
            .equals_utf8("./default.js")
    );
    assert!(matches!(
        default_forwarded.export_name(),
        ModuleExportName::Name(name) if name.equals_utf8("defaultForwarded")
    ));
    assert!(forwarded.span().start < default_forwarded.span().start);
    assert!(default_forwarded.span().start < syntax.export_entries()[2].span().start);
}

#[test]
fn import_meta_sets_module_syntax_without_creating_a_static_request() {
    let allocator = Allocator::new();
    let unit = parse(
        &allocator,
        "void import.meta;",
        FrontendOptions::new(ParseMode::Module),
    )
    .expect("valid module");
    let syntax = unit.module_syntax();

    assert!(syntax.has_module_syntax());
    assert!(syntax.requests().is_empty());
    assert!(syntax.import_entries().is_empty());
    assert!(syntax.export_entries().is_empty());
}

#[test]
fn cloned_syntax_records_share_immutable_arc_backing_and_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ModuleSyntaxRecord>();

    let source = "import value from './dep.js'; export { value };";
    let syntax = with_parsed_program(source, FrontendOptions::new(ParseMode::Module), |unit| {
        unit.module_syntax().clone()
    })
    .expect("valid module");
    let clone = syntax.clone();

    assert!(std::ptr::eq(syntax.requests(), clone.requests()));
    assert!(std::ptr::eq(
        syntax.import_entries(),
        clone.import_entries()
    ));
    assert!(std::ptr::eq(
        syntax.export_entries(),
        clone.export_entries()
    ));
}
