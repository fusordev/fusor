use super::*;
use fusor_bytecode::{BytecodePc, CompilerConstantValue, FunctionKind, VerifiedControlFlow};
use fusor_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

fn test_frame_layout(
    context: &CompilationContext<'_, '_, '_>,
    executable: ExecutableId,
) -> FrameLayout {
    FrameLayout::new(FrameLayoutInput::new(context.storage_plan(), executable))
        .expect("frame layout")
}

#[test]
fn function_index_capacity_checks_count_and_index_boundaries() {
    assert_eq!(
        checked_function_entry_count(MAX_FUNCTION_INDEX_ENTRIES, "test count"),
        Ok(MAX_FUNCTION_INDEX_ENTRIES)
    );
    assert_eq!(
        checked_function_index(MAX_FUNCTION_INDEX_ENTRIES - 1, "test index"),
        Ok(u16::try_from(MAX_FUNCTION_INDEX_ENTRIES - 1).expect("u16 index"))
    );
    assert!(matches!(
        checked_function_entry_count(u64::from(MAX_FUNCTION_INDEX_ENTRIES) + 1, "test count"),
        Err(LeafCompilationError::CapacityExceeded {
            domain: "test count"
        })
    ));
    assert!(matches!(
        checked_function_index(MAX_FUNCTION_INDEX_ENTRIES, "test index"),
        Err(LeafCompilationError::CapacityExceeded {
            domain: "test index"
        })
    ));
}

#[test]
fn constant_pool_ownership_includes_the_program_and_nearest_function() {
    with_parsed_program(
        "1.5; function child(){ return 2.5; }",
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let root = context
                .executables()
                .find(|candidate| candidate.metadata().parent().is_none())
                .expect("program executable")
                .id();
            let child = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("child"))
                .expect("child executable")
                .id();
            let layout = context
                .function_tree_layout()
                .expect("function tree layout");

            let root_pool = layout.constant_pool(root).expect("program constant pool");
            assert_eq!(
                root_pool.entries().as_ref(),
                [
                    CompiledConstant::Value(CompilerConstantValue::Number(
                        Binary64Constant::from_f64(1.5),
                    )),
                    CompiledConstant::Function(CompiledFunctionConstant { executable: child }),
                ]
            );
            let child_pool = layout.constant_pool(child).expect("child constant pool");
            assert_eq!(
                child_pool.entries().as_ref(),
                [CompiledConstant::Value(CompilerConstantValue::Number(
                    Binary64Constant::from_f64(2.5),
                ))]
            );
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn own_capture_layout_distinguishes_argument_function_and_scoped_cells() {
    let source = "function outer(arg){ var functionLocal=1; { let scoped=2; \
                  const capture=function(){ return arg+functionLocal+scoped; }; } }";
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("outer"))
                .expect("outer executable");
            let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                panic!("function declaration");
            };
            let layout = test_frame_layout(&context, executable.id());
            let tree_layout = context
                .function_tree_layout()
                .expect("function tree layout");
            let capture_layout = context
                .compiler_capture_layout(
                    executable.id(),
                    function.scope_id.get().expect("function scope"),
                    &layout,
                    &tree_layout,
                )
                .expect("capture layout");

            assert_eq!(
                capture_layout.bindings(),
                [
                    CompilerCapturedBinding::Argument(0),
                    CompilerCapturedBinding::FunctionLocal(0),
                    CompilerCapturedBinding::ScopedLocal(1),
                ]
            );
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn captured_block_exit_closes_exact_scope_locals_in_reverse_slot_order() {
    let source = "function outer(){ { let first=1; let second=2; \
                  const capture=function(){ return first+second; }; } }";
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("outer"))
                .expect("outer executable");
            let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                panic!("function declaration");
            };
            let body = function.body.as_ref().expect("function body");
            let Statement::BlockStatement(block) = &body.statements[0] else {
                panic!("captured block");
            };
            let block_scope = context
                .created_scope(block.scope_id.get(), block.node_id.get(), block.span)
                .expect("block scope");
            let function_scope = context
                .created_scope(function.scope_id.get(), function.node_id.get(), function.span)
                .expect("function scope");
            let layout = test_frame_layout(&context, executable.id());
            let tree_layout = context.function_tree_layout().expect("function tree layout");
            let capture_layout = context
                .compiler_capture_layout(
                    executable.id(),
                    function_scope,
                    &layout,
                    &tree_layout,
                )
                .expect("capture layout");
            assert_eq!(
                capture_layout.bindings(),
                [
                    CompilerCapturedBinding::ScopedLocal(0),
                    CompilerCapturedBinding::ScopedLocal(1),
                ]
            );

            let mut flow = PlannedControlFlow::new(VerificationLimits::default());
            context
                .plan_scope_exit(executable.id(), block_scope, &layout, &mut flow)
                .expect("scope exit");
            flow.emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                block.span,
            ))
            .expect("terminal");
            let header =
                UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                    false,
                    0,
                    2,
                );
            let (_, verified) = flow
                .finish()
                .expect("assembly")
                .verify_with_capture_layout(
                    FunctionIndexDomains::new(0, 0, 0, layout.local_count, 0),
                    header,
                    capture_layout,
                    VerificationLimits::default(),
                )
                .expect("verified close_loc");
            assert_eq!(
                verified
                    .instructions()
                    .iter()
                    .map(|instruction| {
                        let instruction = instruction.decoded().instruction();
                        (instruction.opcode(), instruction.operands())
                    })
                    .collect::<Vec<_>>(),
                [
                    (FinalOpcode::CloseLoc, Operands::Loc(1)),
                    (FinalOpcode::CloseLoc, Operands::Loc(0)),
                    (FinalOpcode::ReturnUndef, Operands::None),
                ]
            );
        },
    )
    .expect("front-end acceptance");
}

#[allow(clippy::too_many_lines)]
fn abrupt_cleanup_fixture(
    source: &str,
) -> (
    Vec<SourceInstruction>,
    VerifiedControlFlow,
    Vec<CompilerCapturedBinding>,
    Span,
    Span,
) {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("outer"))
                .expect("outer executable");
            let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                panic!("function declaration");
            };
            let body = function.body.as_ref().expect("function body");
            let Statement::WhileStatement(loop_statement) = &body.statements[0] else {
                panic!("while statement");
            };
            let Statement::BlockStatement(loop_body) = &loop_statement.body else {
                panic!("loop body");
            };
            let Statement::BlockStatement(inner) = &loop_body.body[2] else {
                panic!("inner block");
            };
            let Statement::BreakStatement(break_statement) = &inner.body[2] else {
                panic!("break statement");
            };
            let function_scope = function.scope_id.get().expect("function scope");
            let loop_scope = loop_body.scope_id.get().expect("loop body scope");
            let inner_scope = inner.scope_id.get().expect("inner scope");
            let layout = test_frame_layout(&context, executable.id());
            let tree_layout = context.function_tree_layout().expect("function tree layout");
            let capture_layout = context
                .compiler_capture_layout(
                    executable.id(),
                    function_scope,
                    &layout,
                    &tree_layout,
                )
                .expect("capture layout");
            let captured_bindings = capture_layout.bindings().to_vec();

            let mut flow = PlannedControlFlow::new(VerificationLimits::default());
            let done = flow
                .new_statement_label(loop_statement.span)
                .expect("done label");
            let controls = StatementControlStack::with_control(
                ControlRegion::iteration(Vec::new(), done.clone(), done.clone(), 1),
                loop_statement.span,
            )
            .expect("loop control");
            let state = StatementPlanningState {
                work: Vec::new(),
                active_scopes: vec![function_scope, loop_scope, inner_scope],
                controls,
                abrupt_markers: Vec::new(),
                disconnected_abrupt_floors: Vec::new(),
                completion: StatementCompletion::Discard,
                next_script_finally_completion: 0,
                script_finally_completion_limit: 0,
            };
            context
                .plan_control_jump(
                    None,
                    break_statement.span,
                    LoopJump::Break,
                    &state,
                    &layout,
                    &mut flow,
                )
                .expect("abrupt cleanup");
            flow.bind(&done).expect("done binding");
            emit_return_undefined(&mut flow, loop_statement.span, "terminal");
            let header =
                UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
                    false,
                    0,
                    2,
                );
            let (source_instructions, verified) = flow
                .finish()
                .expect("assembly")
                .verify_with_capture_layout(
                    FunctionIndexDomains::new(0, 0, 0, layout.local_count, 0),
                    header,
                    capture_layout,
                    VerificationLimits::default(),
                )
                .expect("verified abrupt cleanup");
            let inner_binding_span = unit.semantic().scoping().symbol_span(
                unit.semantic()
                    .scoping()
                    .iter_bindings_in(inner_scope)
                    .next()
                    .expect("inner binding"),
            );
            (
                source_instructions,
                verified,
                captured_bindings,
                inner_binding_span,
                break_statement.span,
            )
        },
    )
    .expect("front-end acceptance")
}

#[test]
fn abrupt_loop_exit_closes_captured_scope_suffix_from_inner_to_outer() {
    let source = "function outer(){ while(true){ let outerValue=1; \
                  const outerCapture=function(){return outerValue;}; \
                  { let innerValue=2; \
                  const innerCapture=function(){return innerValue;}; break; } } }";
    let (source_instructions, verified, captured, inner_span, break_span) =
        abrupt_cleanup_fixture(source);

    assert_eq!(
        captured,
        [
            CompilerCapturedBinding::ScopedLocal(0),
            CompilerCapturedBinding::ScopedLocal(2),
        ]
    );
    assert_eq!(
        verified
            .instructions()
            .iter()
            .map(|instruction| {
                let instruction = instruction.decoded().instruction();
                (instruction.opcode(), instruction.operands())
            })
            .collect::<Vec<_>>(),
        [
            (FinalOpcode::CloseLoc, Operands::Loc(2)),
            (FinalOpcode::CloseLoc, Operands::Loc(0)),
            (FinalOpcode::Goto8, Operands::Label8(1)),
            (FinalOpcode::ReturnUndef, Operands::None),
        ]
    );
    assert_eq!(
        exact_source_span(&source_instructions, BytecodePc::new(0)),
        Some(inner_span)
    );
    assert_eq!(
        exact_source_span(&source_instructions, BytecodePc::new(6)),
        Some(break_span)
    );
}

#[test]
fn classic_for_schedule_places_rotation_before_test_update_and_final_exit() {
    let source = "function f(){ for(let i=0;i<2;i++){} }";
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                panic!("function declaration");
            };
            let body = function.body.as_ref().expect("function body");
            let Statement::ForStatement(statement) = &body.statements[0] else {
                panic!("classic for");
            };
            let scope = statement.scope_id.get().expect("for scope");
            let mut flow = PlannedControlFlow::new(VerificationLimits::default());
            let mut work = Vec::new();
            CompilationContext::schedule_for_statement(
                statement,
                scope,
                &mut flow,
                &mut work,
                1,
                Vec::new(),
            )
            .expect("iterative for schedule");
            let execution = work.iter().rev().collect::<Vec<_>>();
            let close_positions = execution
                .iter()
                .enumerate()
                .filter_map(|(position, task)| {
                    matches!(task, StatementWork::CloseScope(found) if *found == scope)
                        .then_some(position)
                })
                .collect::<Vec<_>>();
            let test_position = execution
                .iter()
                .position(|task| {
                    matches!(
                        task,
                        StatementWork::Bind(label)
                                if label.owner_span()
                                    == statement.test.as_ref().expect("test").span()
                    )
                })
                .expect("test label");
            let rotate_position = execution
                .iter()
                .position(|task| {
                    matches!(
                        task,
                        StatementWork::Bind(label)
                                if label.owner_span()
                                == statement.update.as_ref().expect("update").span()
                    )
                })
                .expect("rotation label");
            let update_position = execution
                .iter()
                .position(|task| {
                    matches!(
                        task,
                        StatementWork::Expression(expression)
                            if expression.span()
                                == statement.update.as_ref().expect("update").span()
                    )
                })
                .expect("update expression");
            let control = execution
                .iter()
                .find_map(|task| match task {
                    StatementWork::PushControl(control) => Some(control),
                    _ => None,
                })
                .expect("loop control");

            assert_eq!(close_positions.len(), 2);
            assert!(close_positions[0] < test_position);
            assert!(rotate_position < close_positions[1]);
            assert!(close_positions[1] < update_position);
            assert_eq!(
                control
                    .continue_target
                    .as_ref()
                    .expect("iteration continue target")
                    .owner_span(),
                statement.update.as_ref().expect("update").span()
            );
            assert_eq!(control.scope_depth, 2);
            assert_eq!(
                execution
                    .iter()
                    .filter(|task| {
                        matches!(task, StatementWork::PopScope(found) if *found == scope)
                    })
                    .count(),
                1
            );
        },
    )
    .expect("front-end acceptance");
}

fn scheduled_statement_label(work: &[StatementWork<'_, '_>], span: Span) -> CompilerLabel {
    work.iter()
        .find_map(|task| match task {
            StatementWork::Bind(label) if label.owner_span() == span => Some(label.clone()),
            _ => None,
        })
        .expect("scheduled label")
}

fn verify_capture_fixture(
    flow: PlannedControlFlow,
    local_count: u32,
    capture_layout: CompilerCaptureLayout,
    variable_reference_count: u32,
) -> (Vec<SourceInstruction>, VerifiedControlFlow) {
    let header =
        UnverifiedFunctionHeader::stripped_ordinary_source_function_with_variable_references(
            false,
            0,
            variable_reference_count,
        );
    flow.finish()
        .expect("assembly")
        .verify_with_capture_layout(
            FunctionIndexDomains::new(0, 0, 0, local_count, 0),
            header,
            capture_layout,
            VerificationLimits::default(),
        )
        .expect("verified capture fixture")
}

fn emit_return_undefined(flow: &mut PlannedControlFlow, span: Span, expectation: &str) {
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        span,
    ))
    .expect(expectation);
}

const CAPTURED_FOR_CONTINUE_SOURCE: &str = "function outer(){ for(let i=0;i<2;i++){ \
    const capture=function(){return i;}; continue; } }";

fn captured_for_continue_fixture() -> (Vec<SourceInstruction>, VerifiedControlFlow) {
    with_parsed_program(
        CAPTURED_FOR_CONTINUE_SOURCE,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage plan");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("outer"))
                .expect("outer executable");
            let Statement::FunctionDeclaration(function) = &unit.program().body[0] else {
                panic!("function declaration");
            };
            let body = function.body.as_ref().expect("function body");
            let Statement::ForStatement(statement) = &body.statements[0] else {
                panic!("classic for");
            };
            let Statement::BlockStatement(loop_body) = &statement.body else {
                panic!("loop body");
            };
            let Statement::ContinueStatement(continue_statement) = &loop_body.body[1] else {
                panic!("continue statement");
            };
            let scope = statement.scope_id.get().expect("for scope");
            let layout = test_frame_layout(&context, executable.id());
            let tree_layout = context
                .function_tree_layout()
                .expect("function tree layout");
            let capture_layout = context
                .compiler_capture_layout(
                    executable.id(),
                    function.scope_id.get().expect("function scope"),
                    &layout,
                    &tree_layout,
                )
                .expect("capture layout");

            let mut flow = PlannedControlFlow::new(VerificationLimits::default());
            let mut work = Vec::new();
            CompilationContext::schedule_for_statement(
                statement,
                scope,
                &mut flow,
                &mut work,
                1,
                Vec::new(),
            )
            .expect("for schedule");
            let control = work
                .iter()
                .find_map(|task| match task {
                    StatementWork::PushControl(control) => Some(control.clone()),
                    _ => None,
                })
                .expect("loop control");
            let continue_target = control
                .continue_target
                .as_ref()
                .expect("iteration continue target");
            let test =
                scheduled_statement_label(&work, statement.test.as_ref().expect("test").span());
            let done = scheduled_statement_label(&work, statement.span);

            flow.branch(BranchKind::Goto, continue_target, continue_statement.span)
                .expect("continue branch");
            flow.bind(&test).expect("test label");
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Nop,
                Operands::None,
                statement.test.as_ref().expect("test").span(),
            ))
            .expect("unreachable test");
            flow.branch(
                BranchKind::Goto,
                continue_target,
                statement.test.as_ref().expect("test").span(),
            )
            .expect("test-to-rotation branch");
            flow.bind(continue_target).expect("rotation label");
            context
                .plan_scope_exit(executable.id(), scope, &layout, &mut flow)
                .expect("loop-head rotation");
            let update = statement.update.as_ref().expect("update");
            let constants = tree_layout
                .constant_pool(executable.id())
                .expect("constant pool");
            context
                .plan_expression(update, &layout, &tree_layout, constants, &mut flow)
                .expect("update");
            flow.emit(PlannedInstruction::new(
                FinalOpcode::Drop,
                Operands::None,
                update.span(),
            ))
            .expect("drop update");
            emit_return_undefined(&mut flow, statement.span, "rotation terminal");
            flow.bind(&done).expect("done label");
            emit_return_undefined(&mut flow, statement.span, "done terminal");
            verify_capture_fixture(flow, layout.local_count, capture_layout, 1)
        },
    )
    .expect("front-end acceptance")
}

#[test]
fn captured_classic_for_continue_targets_close_loc_before_update() {
    let source = CAPTURED_FOR_CONTINUE_SOURCE;
    let (source_instructions, verified) = captured_for_continue_fixture();
    let continue_instruction = verified
        .instructions()
        .iter()
        .find(|instruction| {
            let pc = instruction.decoded().pc();
            exact_source_span(&source_instructions, pc)
                .is_some_and(|span| &source[span.start as usize..span.end as usize] == "continue;")
        })
        .expect("continue instruction");
    let target = continue_instruction
        .successors()
        .jump_target()
        .and_then(|target| verified.instruction(target))
        .expect("continue target");
    assert_eq!(
        target.decoded().instruction().opcode(),
        FinalOpcode::CloseLoc
    );
    let target_position = verified
        .instructions()
        .iter()
        .position(|instruction| instruction.decoded().pc() == target.decoded().pc())
        .expect("target position");
    let update = &verified.instructions()[target_position + 1];
    assert_eq!(
        update.decoded().instruction().opcode(),
        FinalOpcode::GetLocCheck
    );
    let update_span =
        exact_source_span(&source_instructions, update.decoded().pc()).expect("update span");
    assert_eq!(
        &source[update_span.start as usize..update_span.end as usize],
        "i"
    );
}

#[test]
fn module_compiles_to_a_module_root_with_verified_bytecode() {
    with_parsed_program(
        "const x = 1; export { x }; export default x + 1;",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("module compiles and verifies");
            let bytecode = tree.verified_bytecode();
            assert_eq!(
                bytecode.root().metadata().executable_kind(),
                fusor_bytecode::CompilerExecutableKind::Module
            );
            assert!(!bytecode.requirements().iter().any(|requirement| {
                *requirement == fusor_bytecode::ExecutionRequirement::RealmGlobalBindings
            }));
            assert!(bytecode.requirements().iter().any(|requirement| {
                *requirement == fusor_bytecode::ExecutionRequirement::ModuleBindings
            }));
            assert!(bytecode.module().is_some());
        },
    )
    .expect("front-end acceptance");
}

fn module_binding_names(tree: &CompiledFunctionTree) -> Vec<String> {
    let bytecode = tree.verified_bytecode();
    let root = bytecode.root();
    let atoms = root.function().atoms();
    bytecode
        .module()
        .expect("module record")
        .bindings()
        .iter()
        .map(|binding| {
            let atom = &atoms[binding.name().get() as usize];
            String::from_utf16(&atom.string().code_units().collect::<Vec<u16>>())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn module_import_bindings_are_immutable_temporal_dead_zone_cells() {
    use fusor_bytecode::{CompilerBindingKind, CompilerClosureBinding, CompilerWritePolicy};
    with_parsed_program(
        "import { a } from './m.js'; import * as ns from './ns.js'; export { a, ns };",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("module compiles");
            let bytecode = tree.verified_bytecode();
            let record = bytecode.module().expect("module record");
            let import_a = record
                .bindings()
                .iter()
                .find(|binding| binding.import().is_some() && !binding.import().unwrap().is_namespace())
                .expect("named import binding");
            assert_eq!(import_a.policy().kind(), CompilerBindingKind::Const);
            assert_eq!(import_a.policy().writes(), CompilerWritePolicy::Immutable);
            assert!(import_a.policy().has_temporal_dead_zone());
            assert_eq!(
                import_a.origin(),
                fusor_bytecode::ModuleBindingOrigin::Import
            );
            assert!(import_a.initializer().is_none());
            let namespace = record
                .bindings()
                .iter()
                .find(|binding| {
                    binding
                        .import()
                        .is_some_and(fusor_bytecode::ModuleImportName::is_namespace)
                })
                .expect("namespace import binding");
            assert_eq!(namespace.policy().kind(), CompilerBindingKind::Const);
            assert_eq!(
                namespace.origin(),
                fusor_bytecode::ModuleBindingOrigin::Namespace
            );
            // The cell reads lower to TDZ-checked captured-cell reads: the root
            // closure descriptor for the import is a Captured(immutable, TDZ)
            // binding.
            let root_closures = bytecode.root().metadata().closures();
            let import_closure = root_closures
                .iter()
                .find(|closure| matches!(closure.binding(), CompilerClosureBinding::Captured(policy) if policy.kind() == CompilerBindingKind::Const))
                .expect("import closure descriptor is a captured const cell");
            assert!(matches!(
                import_closure.binding(),
                CompilerClosureBinding::Captured(policy) if policy.has_temporal_dead_zone()
            ));
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn module_export_default_expression_stores_at_statement_position() {
    use fusor_bytecode::{FinalOpcode, Operands};
    with_parsed_program(
        "const x = 1; export default x + 1;",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("module compiles");
            let instructions = tree
                .verified_bytecode()
                .root()
                .function()
                .control_flow()
                .instructions();
            // The synthetic *default* cell receives the default expression value
            // via the TDZ-initializing PutVarRef at the export default statement.
            let put = instructions
                .iter()
                .find(|instruction| {
                    matches!(
                        (
                            instruction.decoded().instruction().opcode(),
                            instruction.decoded().instruction().operands(),
                        ),
                        (FinalOpcode::PutVarRefCheckInit, Operands::VarRef(_))
                    )
                })
                .expect("export default initializes the synthetic cell");
            // A checked read of the exported `x` precedes the store.
            assert!(instructions.iter().any(|instruction| {
                matches!(
                    instruction.decoded().instruction().opcode(),
                    FinalOpcode::GetVarRefCheck
                )
            }));
            let _ = put;
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn module_declaration_record_lists_expected_binding_policies() {
    use fusor_bytecode::{
        CompilerBindingKind, CompilerInitializationPolicy, ModuleBindingOrigin,
    };
    with_parsed_program(
        "var v; let l; const c = 1; function f(){} export default 0;",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("module compiles");
            let record = tree.verified_bytecode().module().expect("module record");
            let names = module_binding_names(&tree);
            let policy = |name: &str| {
                let index = names
                    .iter()
                    .position(|candidate| candidate == name)
                    .unwrap_or_else(|| panic!("missing module binding {name}: {names:?}"));
                record.bindings()[index].policy()
            };
            assert_eq!(policy("v").kind(), CompilerBindingKind::Var);
            assert_eq!(
                policy("v").initialization(),
                CompilerInitializationPolicy::UndefinedAtInstantiation
            );
            assert_eq!(policy("l").kind(), CompilerBindingKind::Let);
            assert_eq!(policy("c").kind(), CompilerBindingKind::Const);
            assert_eq!(policy("f").kind(), CompilerBindingKind::Function);
            assert_eq!(
                policy("f").initialization(),
                CompilerInitializationPolicy::FunctionAtInstantiation
            );
            // The synthetic *default* cell is a mutable TDZ local declaration.
            assert!(record.bindings().iter().any(|binding| {
                binding.origin() == ModuleBindingOrigin::Local
                    && binding.policy().kind() == CompilerBindingKind::Let
                    && binding.policy().initialization()
                        == CompilerInitializationPolicy::AtDeclaration
            }));
            // The hoisted function declaration carries a function initializer.
            let f_binding = record
                .bindings()
                .iter()
                .find(|binding| binding.initializer().is_some())
                .expect("a hoisted module function has an initializer");
            assert_eq!(f_binding.policy().kind(), CompilerBindingKind::Function);
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn module_arrows_and_class_constructors_compile_in_module_units() {
    with_parsed_program(
        "const f = () => 1;\n\
         const g = async () => 2;\n\
         class OnlyFields { value = 3; }\n\
         export { f, g, OnlyFields };",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("module with arrows and classes compiles");
            let kinds: Vec<_> = tree
                .storage_plan()
                .executables()
                .iter()
                .map(|executable| executable.kind())
                .collect();
            assert!(
                kinds.contains(&ExecutableKind::Arrow {
                    asynchronous: false
                }),
                "ordinary arrow compiles, got: {kinds:?}"
            );
            assert!(
                kinds.contains(&ExecutableKind::Arrow { asynchronous: true }),
                "async arrow compiles, got: {kinds:?}"
            );
            assert!(
                kinds.contains(&ExecutableKind::ClassDefaultConstructor),
                "default class constructor compiles, got: {kinds:?}"
            );
            assert!(
                kinds.contains(&ExecutableKind::ClassInstanceInitializer),
                "class instance initializer compiles, got: {kinds:?}"
            );
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn module_anonymous_default_class_exports_compile_in_module_units() {
    for source in [
        "export default class { marker = 9; }",
        "const Base = class { constructor() { this.tag = 4; } };\n\
         export default class extends Base {}",
    ] {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::Module),
            |unit| {
                let context = CompilationContext::new(unit).expect("module storage plan");
                context
                    .compile_module(VerificationLimits::default())
                    .expect("anonymous default class export compiles");
            },
        )
        .expect("front-end acceptance");
    }
}

#[test]
fn module_top_level_await_compiles_an_async_root() {
    with_parsed_program(
        "await Promise.resolve(1);",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("top-level await module compiles and verifies");
            let bytecode = tree.verified_bytecode();
            let flow = bytecode.root().function().control_flow();
            assert_eq!(
                flow.function_header().kind(),
                FunctionKind::Async,
                "top-level await module root compiles as an async function"
            );
            let opcodes: Vec<_> = flow
                .instructions()
                .iter()
                .map(|instruction| instruction.decoded().instruction().opcode())
                .collect();
            assert!(
                opcodes.contains(&FinalOpcode::Await),
                "module root emits await, got: {opcodes:?}"
            );
            assert_eq!(
                opcodes[opcodes.len() - 2..],
                [FinalOpcode::Undefined, FinalOpcode::ReturnAsync],
                "async module root falls through to undefined; return_async, got: {opcodes:?}"
            );
        },
    )
    .expect("front-end acceptance");

    with_parsed_program(
        "const value = 1;\nexport { value };",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("module compiles and verifies");
            let bytecode = tree.verified_bytecode();
            assert_eq!(
                bytecode
                    .root()
                    .function()
                    .control_flow()
                    .function_header()
                    .kind(),
                FunctionKind::Normal,
                "module without top-level await keeps a normal root"
            );
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn module_top_level_await_in_class_heritage_compiles_an_async_root() {
    for source in [
        "function fn() { return class {}; }\n\
         export class C extends fn(await Promise.resolve(1)) {}",
        "function fn() { return class {}; }\n\
         export default class extends fn(await Promise.resolve(1)) {}",
    ] {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::Module),
            |unit| {
                let context = CompilationContext::new(unit).expect("module storage plan");
                let tree = context
                    .compile_module(VerificationLimits::default())
                    .expect("class heritage top-level await module compiles and verifies");
                assert_eq!(
                    tree.verified_bytecode()
                        .root()
                        .function()
                        .control_flow()
                        .function_header()
                        .kind(),
                    FunctionKind::Async,
                    "heritage await compiles the module root as an async function"
                );
            },
        )
        .expect("front-end acceptance");
    }
}

#[test]
fn module_for_await_compiles_an_async_root() {
    with_parsed_program(
        "for await (const value of []) {}",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("for await module compiles and verifies");
            let flow = tree.verified_bytecode().root().function().control_flow();
            assert_eq!(
                flow.function_header().kind(),
                FunctionKind::Async,
                "for await compiles the module root as an async function"
            );
            let opcodes: Vec<_> = flow
                .instructions()
                .iter()
                .map(|instruction| instruction.decoded().instruction().opcode())
                .collect();
            assert!(
                opcodes.contains(&FinalOpcode::ForAwaitOfStart),
                "module root emits for-await iteration, got: {opcodes:?}"
            );
        },
    )
    .expect("front-end acceptance");
}

#[test]
fn module_iteration_heads_with_module_local_var_compile() {
    for source in [
        "var binding;\nfor (var binding of [1]) { break; }",
        "var binding;\nfor (var binding in { a: 1 }) { break; }",
        "for await (var binding of [1]) { break; }",
        "var binding;\nfor (binding of [1]) { break; }",
    ] {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::Module),
            |unit| {
                let context = CompilationContext::new(unit).expect("module storage plan");
                context
                    .compile_module(VerificationLimits::default())
                    .expect("module-local iteration head compiles and verifies");
            },
        )
        .expect("front-end acceptance");
    }
}

#[test]
fn module_destructuring_declarations_compile() {
    for source in [
        "var { y = 2 } = {};",
        "let { z = 3 } = {};",
        "const [w = 4] = [];",
        "export var name1 = await Promise.resolve(1);\n\
         export var { x = await Promise.resolve(2) } = {};",
    ] {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::Module),
            |unit| {
                let context = CompilationContext::new(unit).expect("module storage plan");
                context
                    .compile_module(VerificationLimits::default())
                    .expect("module destructuring declaration compiles and verifies");
            },
        )
        .expect("front-end acceptance");
    }
}

#[test]
fn unreachable_closure_templates_keep_their_definition_sites() {
    for source in [
        "if (false) { class C {} }",
        "if (false) { ({ m() {} }); }",
        "for (false; false; await { function() {} }) { break; }",
        "for (false; false;) { await { m() {} }; break; }",
    ] {
        with_parsed_program(
            source,
            FrontendOptions::for_goal(CompilationGoal::Module),
            |unit| {
                let context = CompilationContext::new(unit).expect("module storage plan");
                context
                    .compile_module(VerificationLimits::default())
                    .expect("unreachable closure templates compile and verify");
            },
        )
        .expect("front-end acceptance");
    }
}

#[test]
fn module_import_meta_compiles() {
    with_parsed_program(
        "const m = import.meta;",
        FrontendOptions::for_goal(CompilationGoal::Module),
        |unit| {
            let context = CompilationContext::new(unit).expect("module storage plan");
            let tree = context
                .compile_module(VerificationLimits::default())
                .expect("module compiles and verifies");
            let bytecode = tree.verified_bytecode();
            let opcodes: Vec<_> = bytecode
                .root()
                .function()
                .control_flow()
                .instructions()
                .iter()
                .map(|instruction| instruction.decoded().instruction().opcode())
                .collect();
            assert!(
                opcodes.contains(&FinalOpcode::ImportMeta),
                "module root emits import_meta, got: {opcodes:?}"
            );
        },
    )
    .expect("front-end acceptance");
}
