use quickjs_compiler::{
    CaptureSource, CompilationUnitKind, CompilerError, DeclarationKind, ExecutableKind,
    InitializationPolicy, StoragePlacement, UnsupportedFeature, WritePolicy, build_storage_plan,
};
use quickjs_frontend::{
    Allocator, CompilationGoal, FrontendOptions, GlobalScriptGoal, IndirectEvalGoal, ParseMode,
    parse, with_parsed_program,
};

fn script(source: &str) -> quickjs_compiler::StoragePlan {
    script_with_goal(source, GlobalScriptGoal::new())
}

fn script_with_goal(source: &str, goal: GlobalScriptGoal) -> quickjs_compiler::StoragePlan {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(goal)),
        build_storage_plan,
    )
    .expect("front-end acceptance")
    .expect("storage plan")
}

fn module(source: &str) -> quickjs_compiler::StoragePlan {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::Module),
        build_storage_plan,
    )
    .expect("front-end acceptance")
    .expect("storage plan")
}

fn indirect_eval(source: &str) -> quickjs_compiler::StoragePlan {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::IndirectEval(IndirectEvalGoal::new())),
        build_storage_plan,
    )
    .expect("front-end acceptance")
    .expect("indirect eval storage plan")
}

#[test]
fn plan_escapes_the_frontend_arena_with_dense_executable_ids() {
    let plan =
        script("var global = 1; function outer(arg) { let local; const arrow = (item) => item; }");

    assert_eq!(plan.kind(), CompilationUnitKind::Script);
    assert_eq!(plan.executables().len(), 3);
    for (index, executable) in plan.executables().iter().enumerate() {
        assert_eq!(executable.id().index(), index);
    }
    assert_eq!(
        plan.executables()[0].kind(),
        ExecutableKind::Script {
            asynchronous: false
        }
    );
    assert_eq!(plan.executables()[0].parent(), None);
    assert_eq!(
        plan.executables()[1].parent(),
        Some(plan.executables()[0].id())
    );
    assert_eq!(plan.executables()[1].name(), Some("outer"));
    assert_eq!(
        plan.executables()[1].name_span(),
        Some(quickjs_frontend::Span::new(25, 30))
    );
    assert_eq!(plan.executables()[1].parameter_count(), 1);
    assert_eq!(
        plan.executables()[2].parent(),
        Some(plan.executables()[1].id())
    );
    assert_eq!(
        plan.executables()[2].kind(),
        ExecutableKind::Arrow {
            asynchronous: false
        }
    );
}

#[test]
fn ordinary_script_root_retains_synchronous_sloppy_metadata_after_arena_drop() {
    let plan = script_with_goal("0;", GlobalScriptGoal::new());
    let root = &plan.executables()[0];

    assert_eq!(
        root.kind(),
        ExecutableKind::Script {
            asynchronous: false
        }
    );
    assert!(!root.is_strict());
}

#[test]
fn global_class_declaration_uses_a_mutable_lexical_binding() {
    let plan = script("class Counter {}");
    let binding = plan
        .bindings()
        .iter()
        .find(|binding| {
            binding.name() == "Counter" && binding.placement() == StoragePlacement::GlobalLexical
        })
        .expect("global Counter binding");

    assert_eq!(binding.policy().kind(), DeclarationKind::Class);
    assert_eq!(
        binding.policy().initialization(),
        InitializationPolicy::AtDeclaration
    );
    assert_eq!(binding.policy().writes(), WritePolicy::Mutable);
    assert!(binding.policy().has_temporal_dead_zone());
}

#[test]
fn async_script_root_retains_asynchronous_sloppy_metadata_after_arena_drop() {
    let plan = script_with_goal(
        "await 0;",
        GlobalScriptGoal::new().with_top_level_await(true),
    );
    let root = &plan.executables()[0];

    assert_eq!(root.kind(), ExecutableKind::Script { asynchronous: true });
    assert!(!root.is_strict());
}

#[test]
fn forced_strict_async_script_root_retains_both_flags_after_arena_drop() {
    let plan = script_with_goal(
        "await 0;",
        GlobalScriptGoal::new()
            .with_forced_strict(true)
            .with_top_level_await(true),
    );
    let root = &plan.executables()[0];

    assert_eq!(root.kind(), ExecutableKind::Script { asynchronous: true });
    assert!(root.is_strict());
}

#[test]
fn script_storage_distinguishes_root_globals_from_nested_blocks() {
    let plan = script(
        "var object; let lexical; const fixed = 1; { var hoisted; let nested; const inner = 2; }",
    );
    let root = plan.executables()[0].id();
    let bindings = plan.bindings_for(root).unwrap();
    let lookup = |name: &str| {
        bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap()
    };

    assert_eq!(lookup("object").placement(), StoragePlacement::GlobalObject);
    assert_eq!(
        lookup("hoisted").placement(),
        StoragePlacement::GlobalObject
    );
    assert_eq!(
        lookup("lexical").placement(),
        StoragePlacement::GlobalLexical
    );
    assert_eq!(lookup("fixed").placement(), StoragePlacement::GlobalLexical);
    assert_eq!(lookup("nested").placement(), StoragePlacement::Local);
    assert_eq!(lookup("inner").placement(), StoragePlacement::Local);
    assert_eq!(lookup("object").policy().kind(), DeclarationKind::Var);
    assert_eq!(
        lookup("object").policy().initialization(),
        InitializationPolicy::UndefinedAtInstantiation
    );
    assert!(!lookup("object").policy().has_temporal_dead_zone());
    assert_eq!(lookup("fixed").policy().writes(), WritePolicy::Immutable);
    assert!(lookup("fixed").policy().has_temporal_dead_zone());
}

#[test]
fn sloppy_indirect_eval_keeps_lexicals_local_and_vars_global() {
    let plan =
        indirect_eval("var shared; function declared() {} let local; const fixed = 1; class C {}");
    let lookup = |name: &str| {
        plan.bindings()
            .iter()
            .find(|binding| binding.name() == name)
            .expect("binding")
    };

    assert_eq!(lookup("shared").placement(), StoragePlacement::GlobalObject);
    assert_eq!(
        lookup("declared").placement(),
        StoragePlacement::GlobalObject
    );
    for name in ["local", "fixed", "C"] {
        assert_eq!(lookup(name).placement(), StoragePlacement::Local, "{name}");
    }
}

#[test]
fn strict_indirect_eval_keeps_every_declaration_local() {
    let plan = indirect_eval(
        "\"use strict\"; var localVar; function localFunction() {} let localLet; const localConst = 1; class LocalClass {}",
    );

    for name in [
        "localVar",
        "localFunction",
        "localLet",
        "localConst",
        "LocalClass",
    ] {
        let binding = plan
            .bindings()
            .iter()
            .find(|binding| binding.name() == name)
            .expect("binding");
        assert_eq!(binding.placement(), StoragePlacement::Local, "{name}");
    }
}

#[test]
fn function_parameters_locals_and_named_expression_bindings_are_explicit() {
    let plan = script(
        "const holder = function self(left, right) { var local; let lexical; try {} catch (error) {} };",
    );
    let function = &plan.executables()[1];
    let bindings = plan.bindings_for(function.id()).unwrap();
    let lookup = |name: &str| {
        bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap()
    };

    assert_eq!(
        lookup("left").placement(),
        StoragePlacement::Argument { parameter_index: 0 }
    );
    assert_eq!(
        lookup("right").placement(),
        StoragePlacement::Argument { parameter_index: 1 }
    );
    assert_eq!(lookup("local").placement(), StoragePlacement::Local);
    assert_eq!(lookup("lexical").placement(), StoragePlacement::Local);
    assert_eq!(lookup("error").policy().kind(), DeclarationKind::Catch);
    assert_eq!(
        lookup("self").policy().kind(),
        DeclarationKind::FunctionName
    );
    assert_eq!(
        lookup("self").policy().writes(),
        WritePolicy::ImmutableInStrictCode
    );
}

#[test]
fn annex_b_catch_var_redeclaration_splits_variable_and_catch_environments() {
    let plan = script("try{throw 1}catch(foo){var foo=2;foo;}foo;");
    let foo = plan
        .bindings()
        .iter()
        .filter(|binding| binding.name() == "foo")
        .collect::<Vec<_>>();
    assert_eq!(foo.len(), 2);

    let catch = foo
        .iter()
        .copied()
        .find(|binding| binding.policy().kind() == DeclarationKind::Catch)
        .expect("catch-parameter binding");
    let variable = foo
        .iter()
        .copied()
        .find(|binding| binding.policy().kind() == DeclarationKind::Var)
        .expect("variable-environment binding");
    assert_eq!(catch.placement(), StoragePlacement::Local);
    assert_eq!(variable.placement(), StoragePlacement::GlobalObject);
    assert_eq!(catch.declaration_spans().len(), 1);
    assert_eq!(variable.declaration_spans().len(), 1);

    let catch_references = plan
        .resolved_references()
        .iter()
        .filter(|reference| reference.binding() == catch.id())
        .count();
    let variable_references = plan
        .resolved_references()
        .iter()
        .filter(|reference| reference.binding() == variable.id())
        .count();
    assert_eq!(catch_references, 1, "the in-catch read uses the parameter");
    assert_eq!(
        variable_references, 1,
        "the post-catch read uses the hoisted var binding"
    );
}

#[test]
fn module_storage_distinguishes_namespace_imports_and_synthesizes_default_cell() {
    let plan = module(
        "import ordinary, { named } from './dep.js'; import * as namespace from './ns.js'; \
         export const local = 1; export default 42;",
    );
    let bindings = plan.bindings_for(plan.executables()[0].id()).unwrap();
    let lookup = |name: &str| {
        bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap()
    };

    assert_eq!(plan.kind(), CompilationUnitKind::Module);
    assert_eq!(
        lookup("ordinary").placement(),
        StoragePlacement::ModuleImport
    );
    assert_eq!(lookup("named").placement(), StoragePlacement::ModuleImport);
    assert_eq!(
        lookup("namespace").placement(),
        StoragePlacement::ModuleLocal
    );
    assert_eq!(
        lookup("namespace").policy().kind(),
        DeclarationKind::NamespaceImport
    );
    assert_eq!(
        lookup("namespace").policy().initialization(),
        InitializationPolicy::ModuleNamespace
    );
    assert_eq!(lookup("local").placement(), StoragePlacement::ModuleLocal);
    assert_eq!(
        lookup("*default*").policy().kind(),
        DeclarationKind::SyntheticDefault
    );
    assert_eq!(
        lookup("*default*").policy().initialization(),
        InitializationPolicy::AtDeclaration
    );
    assert!(lookup("*default*").policy().has_temporal_dead_zone());
}

#[test]
fn unresolved_globals_keep_exact_owner_span_and_access() {
    let plan = script("missing; assigned = 1; function local() { nested; }");
    let root = plan.executables()[0].id();
    let function = plan.executables()[1].id();

    let root_globals = plan.unresolved_globals_for(root).unwrap();
    assert_eq!(root_globals.len(), 2);
    assert_eq!(root_globals[0].name(), "missing");
    assert!(root_globals[0].access().reads());
    assert!(!root_globals[0].access().writes());
    assert_eq!(root_globals[1].name(), "assigned");
    assert!(!root_globals[1].access().reads());
    assert!(root_globals[1].access().writes());

    let nested = plan.unresolved_globals_for(function).unwrap();
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].name(), "nested");
}

#[test]
fn ordinary_namespace_and_default_module_bindings_have_no_oxc_identity_surface() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<quickjs_compiler::StoragePlan>();

    let plan = module("export default function () {}");
    assert_eq!(plan.executables().len(), 2);
    let default = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "*default*")
        .unwrap();
    assert_eq!(
        default.policy().initialization(),
        InitializationPolicy::FunctionAtInstantiation
    );
    assert!(!default.policy().has_temporal_dead_zone());
}

#[test]
fn synthetic_default_atom_cannot_collide_with_a_source_identifier() {
    let plan = module("const _default_ = 1; export default 2;");
    let source_binding = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "_default_")
        .unwrap();
    let synthetic = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "*default*")
        .unwrap();

    assert_eq!(source_binding.policy().kind(), DeclarationKind::Const);
    assert_eq!(synthetic.policy().kind(), DeclarationKind::SyntheticDefault);
}

#[test]
fn strict_named_function_expression_binding_is_read_only() {
    let plan = script("\"use strict\"; const holder = function self() { self = 1; };");
    let function = plan.executables()[1].id();
    let binding = plan
        .bindings_for(function)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "self")
        .unwrap();

    assert_eq!(binding.policy().kind(), DeclarationKind::FunctionName);
    assert_eq!(binding.policy().writes(), WritePolicy::Immutable);
}

#[test]
fn legal_redeclarations_merge_exact_declaration_spans_deterministically() {
    let source = "var merged; function merged() {}";
    let plan = script(source);
    let merged = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "merged")
        .unwrap();

    assert_eq!(merged.policy().kind(), DeclarationKind::Function);
    assert_eq!(merged.placement(), StoragePlacement::GlobalObject);
    assert_eq!(merged.declaration_spans().len(), 2);
    assert_eq!(
        merged
            .declaration_spans()
            .iter()
            .map(|span| &source[span.start as usize..span.end as usize])
            .collect::<Vec<_>>(),
        ["merged", "merged"]
    );
    assert!(
        merged
            .declaration_spans()
            .windows(2)
            .all(|pair| pair[0].start < pair[1].start)
    );
}

#[test]
fn resolved_references_keep_same_name_sibling_block_bindings_distinct() {
    let source = "function f() { { let x = 1; x; } { let x = 2; x; } }";
    let plan = script(source);
    let function = plan.executables()[1].id();
    let bindings = plan
        .bindings_for(function)
        .unwrap()
        .iter()
        .filter(|binding| binding.name() == "x")
        .collect::<Vec<_>>();
    let references = plan.resolved_references_for(function).unwrap();

    assert_eq!(bindings.len(), 2);
    assert_eq!(references.len(), 2);
    assert!(
        plan.resolved_references_for(plan.executables()[0].id())
            .unwrap()
            .is_empty()
    );
    for (index, reference) in references.iter().enumerate() {
        assert_eq!(reference.id().index(), index);
        assert_eq!(reference.executable(), function);
        assert_eq!(reference.binding(), bindings[index].id());
        assert_eq!(
            &source[reference.span().start as usize..reference.span().end as usize],
            "x"
        );
        assert!(reference.access().reads());
        assert!(!reference.access().writes());
    }
}

#[test]
fn resolved_reference_ids_are_checked_at_plan_boundaries() {
    let one = script("let first; first;");
    let two = script("let first; first; let second; second;");
    let out_of_range = two.resolved_references()[1].id();

    assert!(one.resolved_reference(out_of_range).is_none());
    assert_eq!(
        one.resolved_reference(one.resolved_references()[0].id()),
        one.resolved_references().first()
    );
}

#[test]
fn nested_frame_captures_are_forwarded_through_intermediate_executables() {
    let plan = script(
        "function outer(argument) { \
             let local = 1; \
             function middle() { \
                 return function inner() { return argument + local; }; \
             } \
         }",
    );
    let outer = plan.executables()[1].id();
    let middle = plan.executables()[2].id();
    let inner = plan.executables()[3].id();
    let outer_bindings = plan.bindings_for(outer).unwrap();
    let argument = outer_bindings
        .iter()
        .find(|binding| binding.name() == "argument")
        .unwrap();
    let local = outer_bindings
        .iter()
        .find(|binding| binding.name() == "local")
        .unwrap();

    assert!(argument.is_frame_captured());
    assert!(local.is_frame_captured());
    assert!(
        outer_bindings
            .iter()
            .find(|binding| binding.name() == "middle")
            .is_some_and(|binding| !binding.is_frame_captured())
    );
    assert!(plan.frame_captures_for(outer).unwrap().is_empty());

    let middle_captures = plan.frame_captures_for(middle).unwrap();
    assert_eq!(middle_captures.len(), 2);
    assert_eq!(
        middle_captures
            .iter()
            .map(|capture| (capture.binding(), capture.slot().index(), capture.source()))
            .collect::<Vec<_>>(),
        [
            (
                argument.id(),
                0,
                CaptureSource::ParentBinding(argument.id())
            ),
            (local.id(), 1, CaptureSource::ParentBinding(local.id())),
        ]
    );

    let inner_captures = plan.frame_captures_for(inner).unwrap();
    assert_eq!(inner_captures.len(), 2);
    assert_eq!(
        inner_captures
            .iter()
            .map(|capture| (capture.binding(), capture.slot().index(), capture.source()))
            .collect::<Vec<_>>(),
        [
            (
                argument.id(),
                0,
                CaptureSource::ParentCapture(middle_captures[0].slot())
            ),
            (
                local.id(),
                1,
                CaptureSource::ParentCapture(middle_captures[1].slot())
            ),
        ]
    );
    assert_eq!(plan.frame_captures().len(), 4);
}

#[test]
fn direct_eval_captures_visible_outer_bindings_without_source_references() {
    let plan = script(
        "function outer(value) { \
             return function inner() { return eval('value'); }; \
         }",
    );
    let outer = plan.executables()[1].id();
    let inner = plan.executables()[2].id();
    let value = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "value")
        .unwrap();

    assert!(value.is_frame_captured());
    let captures = plan.frame_captures_for(inner).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].binding(), value.id());
    assert_eq!(
        captures[0].source(),
        CaptureSource::ParentBinding(value.id())
    );
}

#[test]
fn direct_eval_capture_visibility_respects_lexical_shadowing() {
    let plan = script(
        "function outer(value) { \
             { let value = 2; return function inner() { return eval('value'); }; } \
         }",
    );
    let outer = plan.executables()[1].id();
    let inner = plan.executables()[2].id();
    let outer_values = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .filter(|binding| binding.name() == "value")
        .collect::<Vec<_>>();
    let parameter = outer_values
        .iter()
        .copied()
        .find(|binding| binding.policy().kind() == DeclarationKind::Parameter)
        .unwrap();
    let lexical = outer_values
        .iter()
        .copied()
        .find(|binding| binding.policy().kind() == DeclarationKind::Let)
        .unwrap();

    assert!(!parameter.is_frame_captured());
    assert!(lexical.is_frame_captured());
    let captures = plan.frame_captures_for(inner).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].binding(), lexical.id());
}

#[test]
fn sibling_captures_are_deduplicated_per_executable() {
    let plan = script(
        "function outer(value) { \
             function middle() { \
                 const left = () => value + value; \
                 const right = () => value; \
             } \
         }",
    );
    let outer = plan.executables()[1].id();
    let middle = plan.executables()[2].id();
    let left = plan.executables()[3].id();
    let right = plan.executables()[4].id();
    let value = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "value")
        .unwrap();

    assert!(value.is_frame_captured());
    assert!(plan.frame_captures_for(outer).unwrap().is_empty());
    let middle_captures = plan.frame_captures_for(middle).unwrap();
    assert_eq!(middle_captures.len(), 1);
    assert_eq!(middle_captures[0].binding(), value.id());
    assert_eq!(middle_captures[0].slot().index(), 0);
    assert_eq!(
        middle_captures[0].source(),
        CaptureSource::ParentBinding(value.id())
    );
    for executable in [left, right] {
        let captures = plan.frame_captures_for(executable).unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].executable(), executable);
        assert_eq!(captures[0].binding(), value.id());
        assert_eq!(captures[0].slot().index(), 0);
        assert_eq!(
            captures[0].source(),
            CaptureSource::ParentCapture(middle_captures[0].slot())
        );
    }
    assert_eq!(plan.frame_captures().len(), 3);
}

#[test]
fn global_and_module_cells_do_not_become_frame_captures() {
    let script_plan = script(
        "var object_cell = 1; \
         let lexical_cell = 2; \
         function read() { return object_cell + lexical_cell; }",
    );
    let script_function = script_plan.executables()[1].id();
    assert!(
        script_plan
            .frame_captures_for(script_function)
            .unwrap()
            .is_empty()
    );
    for name in ["object_cell", "lexical_cell"] {
        assert!(
            script_plan
                .bindings()
                .iter()
                .find(|binding| binding.name() == name)
                .is_some_and(|binding| !binding.is_frame_captured())
        );
    }
    assert_eq!(
        script_plan
            .resolved_references_for(script_function)
            .unwrap()
            .len(),
        2
    );

    let module_plan = module(
        "import { imported } from './dep.js'; \
         const local = 1; \
         export function read() { return imported + local; }",
    );
    let module_function = module_plan.executables()[1].id();
    assert!(
        module_plan
            .frame_captures_for(module_function)
            .unwrap()
            .is_empty()
    );
    for name in ["imported", "local"] {
        assert!(
            module_plan
                .bindings()
                .iter()
                .find(|binding| binding.name() == name)
                .is_some_and(|binding| !binding.is_frame_captured())
        );
    }
    assert_eq!(
        module_plan
            .resolved_references_for(module_function)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn root_block_local_is_captured_but_root_global_cells_are_not() {
    let plan = script(
        "var object_cell = 1; \
         let lexical_cell = 2; \
         { \
             let block_local = 3; \
             const read = () => object_cell + lexical_cell + block_local; \
         }",
    );
    let root = plan.executables()[0].id();
    let arrow = plan.executables()[1].id();
    let root_bindings = plan.bindings_for(root).unwrap();
    let binding = |name: &str| {
        root_bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap()
    };

    assert!(!binding("object_cell").is_frame_captured());
    assert!(!binding("lexical_cell").is_frame_captured());
    assert!(binding("block_local").is_frame_captured());

    let captures = plan.frame_captures_for(arrow).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].binding(), binding("block_local").id());
    assert_eq!(
        captures[0].source(),
        CaptureSource::ParentBinding(binding("block_local").id())
    );
    assert_eq!(plan.resolved_references_for(arrow).unwrap().len(), 3);
}

#[test]
fn one_executable_can_mix_parent_owned_and_forwarded_captures() {
    let plan = script(
        "function outer(outer_value) { \
             function middle(middle_value) { \
                 return () => outer_value + middle_value; \
             } \
         }",
    );
    let outer = plan.executables()[1].id();
    let middle = plan.executables()[2].id();
    let arrow = plan.executables()[3].id();
    let outer_value = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "outer_value")
        .unwrap();
    let middle_value = plan
        .bindings_for(middle)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "middle_value")
        .unwrap();

    let middle_captures = plan.frame_captures_for(middle).unwrap();
    assert_eq!(middle_captures.len(), 1);
    assert_eq!(middle_captures[0].binding(), outer_value.id());
    assert_eq!(
        middle_captures[0].source(),
        CaptureSource::ParentBinding(outer_value.id())
    );

    let arrow_captures = plan.frame_captures_for(arrow).unwrap();
    assert_eq!(arrow_captures.len(), 2);
    let forwarded = arrow_captures
        .iter()
        .find(|capture| capture.binding() == outer_value.id())
        .unwrap();
    assert_eq!(
        forwarded.source(),
        CaptureSource::ParentCapture(middle_captures[0].slot())
    );
    let parent_owned = arrow_captures
        .iter()
        .find(|capture| capture.binding() == middle_value.id())
        .unwrap();
    assert_eq!(
        parent_owned.source(),
        CaptureSource::ParentBinding(middle_value.id())
    );
}

#[test]
fn write_only_reference_still_captures_its_frame_binding() {
    let plan = script(
        "function outer(value) { \
             return function inner() { value = 1; }; \
         }",
    );
    let outer = plan.executables()[1].id();
    let inner = plan.executables()[2].id();
    let value = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "value")
        .unwrap();
    let references = plan.resolved_references_for(inner).unwrap();

    assert_eq!(references.len(), 1);
    assert!(!references[0].access().reads());
    assert!(references[0].access().writes());
    assert!(value.is_frame_captured());
    let captures = plan.frame_captures_for(inner).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].binding(), value.id());
    assert_eq!(
        captures[0].source(),
        CaptureSource::ParentBinding(value.id())
    );
}

#[test]
fn root_arguments_is_an_unresolved_global_and_out_of_range_ids_are_checked() {
    let root_plan = script("arguments;");
    let root = root_plan.executables()[0].id();
    let unresolved = root_plan.unresolved_globals_for(root).unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].name(), "arguments");

    let nested_plan = script("function nested() {}");
    let foreign_nested = nested_plan.executables()[1].id();
    assert!(root_plan.executable(foreign_nested).is_none());
    assert!(root_plan.bindings_for(foreign_nested).is_none());
    assert!(root_plan.resolved_references_for(foreign_nested).is_none());
    assert!(root_plan.unresolved_globals_for(foreign_nested).is_none());
    assert!(root_plan.frame_captures_for(foreign_nested).is_none());
}

#[test]
fn top_level_arrow_arguments_remains_an_unresolved_global() {
    let plan = script("const top = () => arguments;");
    let arrow = plan.executables()[1].id();
    let unresolved = plan.unresolved_globals_for(arrow).unwrap();

    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].name(), "arguments");
}

#[test]
fn parameter_initializers_resolve_past_body_only_bindings() {
    let outer_plan = script("let outer=10;const f=(p=()=>outer)=>{let outer=20;};");
    let parameter_closure = outer_plan.executables()[2].id();
    let outer = outer_plan
        .bindings_for(outer_plan.executables()[0].id())
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "outer")
        .expect("outer lexical binding");
    assert_eq!(
        outer_plan
            .resolved_references_for(parameter_closure)
            .unwrap()[0]
            .binding(),
        outer.id()
    );

    for (source, name) in [
        (
            "const f=(p=eval('var arguments=1'),q=()=>arguments)=>{let arguments=20;};",
            "arguments",
        ),
        (
            "const f=(p=eval('var value=1'),q=()=>value)=>{var value=20;};",
            "value",
        ),
    ] {
        let plan = script(source);
        let parameter_closure = plan.executables()[2].id();
        assert!(
            plan.resolved_references_for(parameter_closure)
                .unwrap()
                .is_empty(),
            "{source}"
        );
        let unresolved = plan.unresolved_globals_for(parameter_closure).unwrap();
        assert_eq!(unresolved.len(), 1, "{source}");
        assert_eq!(unresolved[0].name(), name, "{source}");
    }
}

#[test]
fn sloppy_arguments_is_synthesized_and_captured_by_an_arrow() {
    let plan = script("function outer() { arguments; return () => arguments; }");
    let outer = plan.executables()[1].id();
    let arrow = plan.executables()[2].id();
    let binding = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .find(|binding| binding.is_arguments_object())
        .expect("arguments binding");

    assert!(binding.is_frame_captured());
    assert!(plan.unresolved_globals_for(outer).unwrap().is_empty());
    assert!(plan.unresolved_globals_for(arrow).unwrap().is_empty());
    assert_eq!(plan.resolved_references_for(outer).unwrap().len(), 1);
    assert_eq!(plan.resolved_references_for(arrow).unwrap().len(), 1);
    let captures = plan.frame_captures_for(arrow).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].binding(), binding.id());
    assert_eq!(
        captures[0].source(),
        CaptureSource::ParentBinding(binding.id())
    );
}

#[test]
fn strict_arguments_is_synthesized_once_and_captured_by_an_arrow() {
    let plan = script("function outer() {'use strict'; arguments; return () => arguments;}");
    let outer = plan.executables()[1].id();
    let arrow = plan.executables()[2].id();
    let binding = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .find(|binding| binding.is_arguments_object())
        .expect("arguments binding");

    assert_eq!(binding.name(), "arguments");
    assert!(binding.is_frame_captured());
    assert!(plan.unresolved_globals_for(outer).unwrap().is_empty());
    assert!(plan.unresolved_globals_for(arrow).unwrap().is_empty());
    assert_eq!(plan.resolved_references_for(outer).unwrap().len(), 1);
    assert_eq!(plan.resolved_references_for(arrow).unwrap().len(), 1);
    let captures = plan.frame_captures_for(arrow).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].binding(), binding.id());
    assert_eq!(
        captures[0].source(),
        CaptureSource::ParentBinding(binding.id())
    );
}

#[test]
fn oxc_resolved_arguments_collisions_route_to_the_specification_binding() {
    let var_plan = script("function f() { var arguments; return arguments; }");
    let function = var_plan.executables()[1].id();
    let arguments = var_plan
        .bindings_for(function)
        .unwrap()
        .iter()
        .find(|binding| binding.is_arguments_object())
        .expect("the var binding is reused as the arguments object");
    assert_eq!(arguments.policy().kind(), DeclarationKind::Var);
    assert_eq!(
        var_plan.resolved_references_for(function).unwrap()[0].binding(),
        arguments.id()
    );

    let named_plan = script("const f = function arguments() { return arguments; };");
    let function = named_plan.executables()[1].id();
    let named_bindings = named_plan
        .bindings_for(function)
        .unwrap()
        .iter()
        .filter(|binding| binding.name() == "arguments")
        .collect::<Vec<_>>();
    assert_eq!(named_bindings.len(), 2);
    let arguments = named_bindings
        .iter()
        .copied()
        .find(|binding| binding.is_arguments_object())
        .expect("named expression gains an inner arguments object");
    assert!(named_bindings.iter().any(|binding| {
        binding.policy().kind() == DeclarationKind::FunctionName && !binding.is_arguments_object()
    }));
    assert_eq!(
        named_plan.resolved_references_for(function).unwrap()[0].binding(),
        arguments.id()
    );
}

#[test]
fn implicit_arguments_shadow_outer_bindings_but_preserve_inner_explicit_ones() {
    let outer_plan = script("let arguments=0; function f(){ return arguments; }");
    let function = outer_plan.executables()[1].id();
    let arguments = outer_plan
        .bindings_for(function)
        .unwrap()
        .iter()
        .find(|binding| binding.is_arguments_object())
        .expect("ordinary function arguments shadow the outer lexical");
    assert_eq!(
        outer_plan.resolved_references_for(function).unwrap()[0].binding(),
        arguments.id()
    );

    let arrow_plan = script("function f(){ return (arguments) => arguments; }");
    let function = arrow_plan.executables()[1].id();
    let arrow = arrow_plan.executables()[2].id();
    assert!(
        arrow_plan
            .bindings_for(function)
            .unwrap()
            .iter()
            .all(|binding| !binding.is_arguments_object())
    );
    let parameter = arrow_plan
        .bindings_for(arrow)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "arguments")
        .expect("arrow parameter binding");
    assert_eq!(
        arrow_plan.resolved_references_for(arrow).unwrap()[0].binding(),
        parameter.id()
    );

    let arrow_var_plan =
        script("function f(){ return () => { var arguments=2; return arguments; }; }");
    let function = arrow_var_plan.executables()[1].id();
    let arrow = arrow_var_plan.executables()[2].id();
    assert!(
        arrow_var_plan
            .bindings_for(function)
            .unwrap()
            .iter()
            .all(|binding| !binding.is_arguments_object())
    );
    let binding = arrow_var_plan
        .bindings_for(arrow)
        .unwrap()
        .iter()
        .find(|binding| binding.name() == "arguments")
        .expect("arrow var binding");
    assert_eq!(
        arrow_var_plan.resolved_references_for(arrow).unwrap()[0].binding(),
        binding.id()
    );

    let block_plan = script(
        "function f(){ let result; { let arguments=3; result=arguments; } return arguments; }",
    );
    let function = block_plan.executables()[1].id();
    let bindings = block_plan.bindings_for(function).unwrap();
    let implicit = bindings
        .iter()
        .find(|binding| binding.is_arguments_object())
        .expect("outer function arguments binding");
    let lexical = bindings
        .iter()
        .find(|binding| {
            binding.name() == "arguments" && binding.policy().kind() == DeclarationKind::Let
        })
        .expect("inner block lexical binding");
    let targets = block_plan
        .resolved_references_for(function)
        .unwrap()
        .iter()
        .map(quickjs_compiler::ResolvedReference::binding)
        .collect::<Vec<_>>();
    assert!(targets.contains(&implicit.id()));
    assert!(targets.contains(&lexical.id()));
}

#[test]
fn explicit_arguments_bindings_with_ordinary_semantics_remain_supported() {
    for source in [
        "function parameter(arguments) { var arguments; return arguments; }",
        "function lexical() { let arguments = 1; return arguments; }",
        "function outer() { function arguments() {} return arguments; }",
    ] {
        let plan = script(source);
        assert!(
            plan.bindings_for(plan.executables()[1].id())
                .unwrap()
                .iter()
                .all(|binding| !binding.is_arguments_object()),
            "{source}"
        );
    }
}

#[test]
fn expression_free_destructured_parameters_use_raw_positions_and_local_bindings() {
    let plan = script(
        "function f(keep,{value,...objectRest},[head,,...tail]){\
            return keep+value+objectRest+head+tail+arguments.length;}",
    );
    let function = plan.executables()[1].id();
    let executable = &plan.executables()[1];
    assert_eq!(executable.parameter_count(), 3);
    assert!(!executable.has_simple_parameter_list());
    assert!(executable.parameter_binding_indices().is_empty());
    assert!(executable.mapped_parameter_indices().is_empty());

    let bindings = plan.bindings_for(function).unwrap();
    let keep = bindings
        .iter()
        .find(|binding| binding.name() == "keep")
        .expect("identifier formal binding");
    assert_eq!(
        keep.placement(),
        StoragePlacement::Argument { parameter_index: 0 }
    );
    for name in ["value", "objectRest", "head", "tail"] {
        let binding = bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap_or_else(|| panic!("missing destructured parameter {name}"));
        assert_eq!(binding.placement(), StoragePlacement::Local, "{name}");
        assert_eq!(
            binding.policy().kind(),
            DeclarationKind::Parameter,
            "{name}"
        );
    }
    assert!(
        bindings
            .iter()
            .any(quickjs_compiler::BindingStorage::is_arguments_object)
    );
}

#[test]
fn formal_rest_parameters_start_after_fixed_arguments_and_bind_locally() {
    let plan = script(
        "function f(keep,...[head,{value},...tail]){\
            return keep+head+value+tail.length+arguments.length;}",
    );
    let function = plan.executables()[1].id();
    let executable = &plan.executables()[1];
    assert_eq!(executable.parameter_count(), 1);
    assert!(!executable.has_simple_parameter_list());
    assert!(executable.parameter_binding_indices().is_empty());
    assert!(executable.mapped_parameter_indices().is_empty());

    let bindings = plan.bindings_for(function).unwrap();
    let keep = bindings
        .iter()
        .find(|binding| binding.name() == "keep")
        .expect("fixed parameter binding");
    assert_eq!(
        keep.placement(),
        StoragePlacement::Argument { parameter_index: 0 }
    );
    for name in ["head", "value", "tail"] {
        let binding = bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap_or_else(|| panic!("missing rest parameter binding {name}"));
        assert_eq!(binding.placement(), StoragePlacement::Local, "{name}");
        assert_eq!(
            binding.policy().kind(),
            DeclarationKind::Parameter,
            "{name}"
        );
        assert_eq!(
            binding.policy().initialization(),
            InitializationPolicy::Argument,
            "{name}"
        );
    }
    assert!(
        bindings
            .iter()
            .any(quickjs_compiler::BindingStorage::is_arguments_object)
    );
}

#[test]
fn parameter_expressions_use_tdz_locals_and_stop_observable_length() {
    let plan = script(
        "function f(first,{[first]: value = 2} = {},last = value,...[tail = last]){\
            return first+value+last+tail+arguments.length;}",
    );
    let executable = &plan.executables()[1];
    assert_eq!(executable.parameter_count(), 3);
    assert_eq!(executable.defined_parameter_count(), 1);
    assert!(!executable.has_simple_parameter_list());
    assert!(executable.has_parameter_expressions());

    let bindings = plan.bindings_for(executable.id()).unwrap();
    for name in ["first", "value", "last", "tail"] {
        let binding = bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap_or_else(|| panic!("missing parameter-expression binding {name}"));
        assert_eq!(binding.placement(), StoragePlacement::Local, "{name}");
        assert_eq!(
            binding.policy().kind(),
            DeclarationKind::Parameter,
            "{name}"
        );
        assert_eq!(
            binding.policy().initialization(),
            InitializationPolicy::Argument,
            "{name}"
        );
        assert!(binding.policy().has_temporal_dead_zone(), "{name}");
    }
    assert!(
        bindings
            .iter()
            .any(quickjs_compiler::BindingStorage::is_arguments_object)
    );
}

#[test]
fn parameter_expression_collisions_split_parameter_and_body_bindings() {
    let plan = script(
        "function f(a=1,reader=function inner(){return a}){var a;function reader(){}\
            var arguments;return a+reader+arguments.length;}",
    );
    let executable = plan.executables()[1].id();
    let bindings = plan.bindings_for(executable).unwrap();

    let a = bindings
        .iter()
        .filter(|binding| binding.name() == "a")
        .collect::<Vec<_>>();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].policy().kind(), DeclarationKind::Parameter);
    assert!(a[0].policy().has_temporal_dead_zone());
    assert_eq!(a[1].policy().kind(), DeclarationKind::Var);
    assert!(!a[1].policy().has_temporal_dead_zone());

    let reader = bindings
        .iter()
        .filter(|binding| binding.name() == "reader")
        .collect::<Vec<_>>();
    assert_eq!(reader.len(), 2);
    assert_eq!(reader[0].policy().kind(), DeclarationKind::Parameter);
    assert_eq!(reader[1].policy().kind(), DeclarationKind::Function);
    assert_eq!(
        reader[1].policy().initialization(),
        InitializationPolicy::FunctionAtScopeEntry
    );

    let arguments = bindings
        .iter()
        .filter(|binding| binding.name() == "arguments")
        .collect::<Vec<_>>();
    assert_eq!(arguments.len(), 2);
    assert!(
        arguments
            .iter()
            .any(|binding| binding.is_arguments_object())
    );
    assert!(arguments.iter().any(|binding| {
        !binding.is_arguments_object() && binding.policy().kind() == DeclarationKind::Var
    }));
}

#[test]
fn non_simple_body_functions_activate_after_parameter_initialization() {
    let plan = script(
        "function f({value}){function value(){return 1;}function other(){return 2;}\
            return value()+other();}",
    );
    let bindings = plan.bindings_for(plan.executables()[1].id()).unwrap();
    for name in ["value", "other"] {
        let binding = bindings
            .iter()
            .find(|binding| binding.name() == name)
            .unwrap_or_else(|| panic!("missing body function {name}"));
        assert_eq!(binding.placement(), StoragePlacement::Local, "{name}");
        assert_eq!(binding.policy().kind(), DeclarationKind::Function, "{name}");
        assert_eq!(
            binding.policy().initialization(),
            InitializationPolicy::FunctionAtScopeEntry,
            "{name}"
        );
    }
}

#[test]
fn duplicate_parameters_share_the_last_formal_argument_slot() {
    let plan = script("function f(a, a) { return a + arguments[0] + arguments[1]; }");
    let function = plan.executables()[1].id();
    let parameters = plan
        .bindings_for(function)
        .unwrap()
        .iter()
        .filter(|binding| binding.name() == "a")
        .collect::<Vec<_>>();

    assert_eq!(plan.executables()[1].parameter_count(), 2);
    assert_eq!(plan.executables()[1].parameter_binding_indices(), [1, 1]);
    assert_eq!(plan.executables()[1].mapped_parameter_indices(), [1]);
    assert_eq!(parameters.len(), 1);
    assert_eq!(
        parameters[0].placement(),
        StoragePlacement::Argument { parameter_index: 1 }
    );
    assert!(
        plan.bindings_for(function)
            .unwrap()
            .iter()
            .any(quickjs_compiler::BindingStorage::is_arguments_object)
    );
}

#[test]
fn module_root_and_nested_declarations_have_distinct_storage() {
    let plan = module("var top; { let nested; }");
    let top = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "top")
        .unwrap();
    let nested = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "nested")
        .unwrap();

    assert_eq!(top.placement(), StoragePlacement::ModuleLocal);
    assert_eq!(nested.placement(), StoragePlacement::Local);
}

fn unsupported(source: &str, mode: ParseMode) -> (UnsupportedFeature, quickjs_frontend::Span) {
    let allocator = Allocator::new();
    let unit = parse(&allocator, source, FrontendOptions::new(mode)).expect("front-end acceptance");
    match build_storage_plan(&unit).expect_err("compiler must fail closed") {
        CompilerError::Unsupported { feature, span } => (feature, span),
        other => panic!("unexpected compiler error: {other:?}"),
    }
}

#[test]
fn with_object_environment_is_a_hidden_scoped_capture() {
    let plan = script("function outer(object) { with (object) { return () => value; } }");
    let outer = plan.executables()[1].id();
    let arrow = plan.executables()[2].id();
    let binding = plan
        .bindings_for(outer)
        .unwrap()
        .iter()
        .find(|binding| binding.policy().kind() == DeclarationKind::WithObject)
        .expect("hidden with-object binding");

    assert_eq!(binding.placement(), StoragePlacement::Local);
    assert_eq!(
        binding.policy().initialization(),
        InitializationPolicy::AtDeclaration
    );
    assert_eq!(binding.policy().writes(), WritePolicy::Immutable);
    assert!(binding.policy().has_temporal_dead_zone());
    assert!(binding.is_frame_captured());
    let captures = plan.frame_captures_for(arrow).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].binding(), binding.id());
    assert_eq!(
        captures[0].source(),
        CaptureSource::ParentBinding(binding.id())
    );
}

#[test]
fn direct_eval_inside_with_retains_the_hidden_object_environment() {
    let plan = script("function invoke(object) { with (object) return eval('value'); }");
    let invoke = plan.executables()[1].id();
    let binding = plan
        .bindings_for(invoke)
        .expect("invoke bindings")
        .iter()
        .find(|binding| binding.policy().kind() == DeclarationKind::WithObject)
        .expect("hidden with-object binding");

    assert_eq!(binding.placement(), StoragePlacement::Local);
    assert!(!binding.is_frame_captured());
    assert!(plan.executables()[1].has_direct_eval());
}

#[test]
fn lexical_bindings_inside_with_shadow_the_object_environment() {
    let plan = script("with (object) { let value = () => 1; value = () => 2; value(); }");
    let value = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "value")
        .expect("inner lexical binding");
    assert_eq!(value.policy().kind(), DeclarationKind::Let);
}

#[test]
fn sloppy_block_functions_fail_closed_for_annex_b_dual_bindings() {
    let (feature, span) = unsupported("{ function legacy() {} }", ParseMode::Script);
    assert_eq!(feature, UnsupportedFeature::AnnexBBlockFunction);
    assert_eq!(
        &"{ function legacy() {} }"[span.start as usize..span.end as usize],
        "function legacy() {}"
    );
}

#[test]
fn sloppy_labelled_function_fails_closed_for_annex_b_binding_rules() {
    let source = "legacy: function declared() {}";
    let (feature, span) = unsupported(source, ParseMode::Script);
    assert_eq!(feature, UnsupportedFeature::AnnexBBlockFunction);
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        "function declared() {}"
    );
}

#[test]
fn strict_block_function_is_a_single_local_binding() {
    let plan = script("\"use strict\"; { function local() {} }");
    let binding = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "local")
        .unwrap();
    assert_eq!(binding.placement(), StoragePlacement::Local);
    assert_eq!(
        binding.policy().initialization(),
        InitializationPolicy::FunctionAtScopeEntry
    );
}

#[test]
fn host_forced_strict_block_function_is_a_single_local_binding() {
    let plan = script_with_goal(
        "{ function local() {} }",
        GlobalScriptGoal::new().with_forced_strict(true),
    );
    let binding = plan
        .bindings()
        .iter()
        .find(|binding| binding.name() == "local")
        .unwrap();
    assert_eq!(binding.placement(), StoragePlacement::Local);
    assert_eq!(
        binding.policy().initialization(),
        InitializationPolicy::FunctionAtScopeEntry
    );
}

#[test]
fn shadowed_bare_eval_retains_ordinary_resolved_storage() {
    let plan = script("function f(eval) { eval('code'); }");
    let function = plan
        .executables()
        .iter()
        .find(|executable| executable.name() == Some("f"))
        .expect("function executable");
    assert!(
        plan.resolved_references_for(function.id())
            .expect("function references")
            .iter()
            .any(|reference| {
                plan.binding(reference.binding())
                    .is_some_and(|binding| binding.name() == "eval")
            })
    );
}
