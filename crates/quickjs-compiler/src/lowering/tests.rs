use super::*;
use quickjs_frontend::{CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program};

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
                root_pool.entries.as_ref(),
                [
                    CompiledConstant::Value(CompilerConstantValue::Number(
                        Binary64Constant::from_f64(1.5),
                    )),
                    CompiledConstant::Function(CompiledFunctionConstant { executable: child }),
                ]
            );
            let child_pool = layout.constant_pool(child).expect("child constant pool");
            assert_eq!(
                child_pool.entries.as_ref(),
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
            let layout =
                FrameLayout::new(context.storage_plan(), executable.id()).expect("frame layout");
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
            let layout =
                FrameLayout::new(context.storage_plan(), executable.id()).expect("layout");
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
            let layout =
                FrameLayout::new(context.storage_plan(), executable.id()).expect("frame layout");
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
                completion: StatementCompletion::Discard,
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
                            if label.owner_span == statement.test.as_ref().expect("test").span()
                    )
                })
                .expect("test label");
            let rotate_position = execution
                .iter()
                .position(|task| {
                    matches!(
                        task,
                        StatementWork::Bind(label)
                            if label.owner_span
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
                    .owner_span,
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
            StatementWork::Bind(label) if label.owner_span == span => Some(label.clone()),
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
            let layout =
                FrameLayout::new(context.storage_plan(), executable.id()).expect("frame layout");
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
fn compiler_labels_retain_owner_spans_for_bind_and_finish_failures() {
    let owner = Span::new(10, 20);

    let mut duplicate = PlannedControlFlow::new(VerificationLimits::default());
    let label = duplicate.new_label(owner).expect("label");
    duplicate.bind(&label).expect("first bind");
    let error = duplicate.bind(&label).expect_err("duplicate bind");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source: AssemblerError::DuplicateLabel { .. },
        } if span == owner
    ));

    let mut unbound = PlannedControlFlow::new(VerificationLimits::default());
    let _unbound_label = unbound.new_label(owner).expect("unbound label");
    let distractor_owner = Span::new(1, 2);
    let distractor = unbound
        .new_label(distractor_owner)
        .expect("distractor label");
    unbound.bind(&distractor).expect("distractor binding");
    unbound
        .emit(PlannedInstruction::new(
            FinalOpcode::ReturnUndef,
            Operands::None,
            Span::new(30, 31),
        ))
        .expect("terminal");
    let error = unbound.finish().expect_err("unbound label");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source: AssemblerError::UnboundLabel { .. },
        } if span == owner
    ));

    let mut end_target = PlannedControlFlow::new(VerificationLimits::default());
    let label = end_target.new_label(owner).expect("label");
    let distractor_owner = Span::new(2, 3);
    let distractor = end_target
        .new_label(distractor_owner)
        .expect("distractor label");
    end_target.bind(&distractor).expect("distractor binding");
    end_target
        .branch(BranchKind::Goto, &label, Span::new(40, 41))
        .expect("branch");
    end_target.bind(&label).expect("bind at end");
    let error = end_target.finish().expect_err("end target");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeAssembly {
            span: Some(span),
            source: AssemblerError::TargetAtEnd { .. },
        } if span == owner
    ));
}

#[test]
fn reachable_statement_anchors_require_an_empty_stack() {
    let anchor_span = Span::new(20, 30);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(10, 11),
    ))
    .expect("push");
    let anchor = flow
        .new_statement_label(anchor_span)
        .expect("statement label");
    flow.branch(BranchKind::Goto, &anchor, anchor_span)
        .expect("widened branch");
    for _ in 0..130 {
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Nop,
            Operands::None,
            Span::new(12, 13),
        ))
        .expect("padding");
    }
    flow.bind(&anchor).expect("bind");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Return,
        Operands::None,
        Span::new(30, 31),
    ))
    .expect("return");

    let finished = flow.finish().expect("assembly");
    let error = finished
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("depth-one statement anchor");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeStackInvariant {
            span,
            pc,
            expected: 0,
            actual: 1,
        } if span == anchor_span && pc == BytecodePc::new(134)
    ));
}

#[test]
fn unreachable_statement_anchors_have_no_required_entry_depth() {
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let live_exit = flow.new_label(Span::new(40, 41)).expect("live exit");
    flow.branch(BranchKind::Goto, &live_exit, Span::new(0, 1))
        .expect("skip unreachable region");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(10, 11),
    ))
    .expect("unreachable push");
    let anchor = flow
        .new_statement_label(Span::new(20, 21))
        .expect("unreachable statement anchor");
    flow.bind(&anchor).expect("unreachable anchor binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(21, 22),
    ))
    .expect("unreachable terminal");
    flow.bind(&live_exit).expect("live exit binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(40, 41),
    ))
    .expect("live terminal");

    let (source_instructions, control_flow) = flow
        .finish()
        .expect("assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect("unreachable anchor is accepted");
    let anchor_source = source_instructions
        .iter()
        .find(|instruction| instruction.span() == Span::new(21, 22))
        .expect("unreachable target source");
    let anchor_index = control_flow
        .instruction_index_at(anchor_source.pc())
        .expect("verified unreachable target");
    assert_eq!(
        control_flow
            .instruction(anchor_index)
            .expect("unreachable target instruction")
            .entry_stack_depth(),
        None
    );
}

#[test]
fn inconsistent_join_maps_incoming_and_target_source_spans() {
    let incoming_span = Span::new(20, 21);
    let target_span = Span::new(30, 31);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let join = flow.new_label(Span::new(10, 11)).expect("join");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(0, 1),
    ))
    .expect("condition");
    flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
        .expect("conditional branch");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(2, 3),
    ))
    .expect("unbalanced value");
    flow.branch(BranchKind::Goto, &join, incoming_span)
        .expect("incoming edge");
    flow.bind(&join).expect("join binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        target_span,
    ))
    .expect("join target");

    let error = flow
        .finish()
        .expect("assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("inconsistent stack join");

    assert!(matches!(
        error,
        LeafCompilationError::BytecodeVerification {
            span: Some(span),
            related_span: Some(related_span),
            source,
        } if span == incoming_span
            && related_span == target_span
            && matches!(
                source.kind(),
                VerificationErrorKind::InconsistentStackAtJoin { .. }
            )
    ));
}

#[test]
fn missing_primary_verifier_source_mapping_fails_as_a_compiler_invariant() {
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(0, 1),
    ))
    .expect("first push");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(1, 2),
    ))
    .expect("second push");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Return,
        Operands::None,
        Span::new(2, 3),
    ))
    .expect("return");
    let mut finished = flow.finish().expect("assembly");
    finished
        .source_instructions
        .retain(|instruction| instruction.pc() != BytecodePc::new(1));

    let error = finished
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::new(100, 10, 0, 0, 100, 1),
        )
        .expect_err("missing exact primary provenance");
    assert!(matches!(
        error,
        LeafCompilationError::SemanticInvariant {
            invariant: "verifier instruction PC resolves to an exact source instruction",
            span: None,
        }
    ));
}

#[test]
fn missing_join_target_source_mapping_fails_as_a_compiler_invariant() {
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let join = flow.new_label(Span::new(10, 11)).expect("join");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(0, 1),
    ))
    .expect("condition");
    flow.branch(BranchKind::IfFalse, &join, Span::new(1, 2))
        .expect("conditional branch");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Push1,
        Operands::NoneInt,
        Span::new(2, 3),
    ))
    .expect("unbalanced value");
    flow.branch(BranchKind::Goto, &join, Span::new(3, 4))
        .expect("incoming edge");
    flow.bind(&join).expect("join binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(4, 5),
    ))
    .expect("join target");
    let mut finished = flow.finish().expect("assembly");
    finished
        .source_instructions
        .retain(|instruction| instruction.span() != Span::new(4, 5));

    let error = finished
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("missing exact related provenance");
    assert!(matches!(
        error,
        LeafCompilationError::SemanticInvariant {
            invariant: "verifier join target resolves to an exact source instruction",
            span: None,
        }
    ));
}

#[test]
fn widened_branch_verifier_failures_use_the_relocated_target_span() {
    let target_span = Span::new(30, 31);
    let mut flow = PlannedControlFlow::new(VerificationLimits::default());
    let target = flow.new_label(Span::new(20, 21)).expect("target");
    flow.branch(BranchKind::Goto, &target, Span::new(0, 1))
        .expect("widened branch");
    for _ in 0..130 {
        flow.emit(PlannedInstruction::new(
            FinalOpcode::Nop,
            Operands::None,
            Span::new(10, 11),
        ))
        .expect("padding");
    }
    flow.bind(&target).expect("target binding");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::Drop,
        Operands::None,
        target_span,
    ))
    .expect("underflowing target");
    flow.emit(PlannedInstruction::new(
        FinalOpcode::ReturnUndef,
        Operands::None,
        Span::new(31, 32),
    ))
    .expect("terminal");

    let error = flow
        .finish()
        .expect("assembly")
        .verify(
            FunctionIndexDomains::new(0, 0, 0, 0, 0),
            UnverifiedFunctionHeader::stripped_ordinary_source_function(false, 0),
            VerificationLimits::default(),
        )
        .expect_err("reachable drop underflows");
    assert!(matches!(
        error,
        LeafCompilationError::BytecodeVerification {
            span: Some(span),
            related_span: None,
            source,
        } if span == target_span
            && source.pc() == Some(BytecodePc::new(133))
            && matches!(
                source.kind(),
                VerificationErrorKind::StackUnderflow {
                    required: 1,
                    available: 0,
                }
            )
    ));
}
