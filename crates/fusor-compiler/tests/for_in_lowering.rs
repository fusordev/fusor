use fusor_bytecode::{
    AssemblerError, AssemblerResource, BytecodeGraphResource, BytecodeGraphVerificationLimits,
    BytecodePc, BytecodeVerificationErrorKind, FinalOpcode, FunctionGraphVerificationLimits,
    Operands, VerificationErrorKind, VerificationLimits,
};
use fusor_compiler::{
    CompilationContext, CompiledFunctionTree, CompiledLeafFunction, LeafCompilationError,
};
use fusor_frontend::{
    CompilationGoal, FrontendDiagnosticCode, FrontendOptions, GlobalScriptGoal, with_parsed_program,
};

fn compile(source: &str, name: &str) -> CompiledLeafFunction {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_leaf(&executable, VerificationLimits::default())
                .expect("for-in lowering")
        },
    )
    .expect("front-end acceptance")
}

fn compile_tree(source: &str, name: &str) -> CompiledLeafFunction {
    compile_verified_tree(source, name).root().clone()
}

fn compile_verified_tree(source: &str, name: &str) -> CompiledFunctionTree {
    with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some(name))
                .expect("named function");
            context
                .compile_tree(&executable, VerificationLimits::default())
                .expect("for-in tree lowering")
        },
    )
    .expect("front-end acceptance")
}

fn decoded(compiled: &CompiledLeafFunction) -> Vec<(BytecodePc, FinalOpcode, Operands)> {
    compiled
        .control_flow()
        .instructions()
        .iter()
        .map(|instruction| {
            let decoded = instruction.decoded();
            (
                decoded.pc(),
                decoded.instruction().opcode(),
                decoded.instruction().operands(),
            )
        })
        .collect()
}

fn opcodes(compiled: &CompiledLeafFunction) -> Vec<FinalOpcode> {
    decoded(compiled)
        .into_iter()
        .map(|(_, opcode, _)| opcode)
        .collect()
}

fn source_slice_at<'source>(
    compiled: &CompiledLeafFunction,
    source: &'source str,
    pc: BytecodePc,
) -> &'source str {
    let span = compiled
        .source_instructions()
        .iter()
        .find(|mapping| mapping.pc() == pc)
        .expect("source mapping at instruction PC")
        .span();
    &source[span.start as usize..span.end as usize]
}

#[test]
fn declaration_and_identifier_targets_use_the_typed_for_in_protocol() {
    let declared = compile(
        "function declared(object){ let result=\"\"; for (const key in object) result=result+key; return result; }",
        "declared",
    );
    let assigned = compile(
        "function assigned(object){ let key; for (key in object) break; return key; }",
        "assigned",
    );

    for compiled in [&declared, &assigned] {
        let opcodes = opcodes(compiled);
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&opcode| opcode == FinalOpcode::ForInStart)
                .count(),
            1
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&opcode| opcode == FinalOpcode::ForInNext)
                .count(),
            1
        );
        assert_eq!(compiled.control_flow().computed_stack_size(), 3);
    }
}

#[test]
fn member_targets_preserve_javascript_reference_evaluation_order() {
    let static_source = "function staticTarget(object, target){ for (target.key in object) break; return target.key; }";
    let static_target = compile(static_source, "staticTarget");
    let computed_source = "function computedTarget(object, target, property){ for (target[property] in object) break; return target[property]; }";
    let computed_target = compile(computed_source, "computedTarget");

    let static_instructions = static_target.control_flow().instructions();
    for opcode in [FinalOpcode::Swap, FinalOpcode::PutField] {
        let instruction = static_instructions
            .iter()
            .find(|instruction| instruction.decoded().instruction().opcode() == opcode)
            .expect("static member head opcode");
        assert_eq!(instruction.entry_stack_depth(), Some(3));
        assert_eq!(
            source_slice_at(&static_target, static_source, instruction.decoded().pc()),
            "target.key"
        );
    }

    let computed_instructions = computed_target.control_flow().instructions();
    for opcode in [FinalOpcode::Rot3l, FinalOpcode::PutArrayEl] {
        let instruction = computed_instructions
            .iter()
            .find(|instruction| instruction.decoded().instruction().opcode() == opcode)
            .expect("computed member head opcode");
        assert_eq!(instruction.entry_stack_depth(), Some(4));
        assert_eq!(
            source_slice_at(
                &computed_target,
                computed_source,
                instruction.decoded().pc()
            ),
            "target[property]"
        );
    }
}

#[test]
fn abrupt_exits_remove_exact_crossed_iterator_markers() {
    let compiled = compile(
        "function f(outer, inner, stop){ first: for (const a in outer) { for (const b in inner) { if (stop) return a+b; if (b) continue first; break; } break first; } return \"done\"; }",
        "f",
    );
    let opcodes = opcodes(&compiled);

    assert!(
        opcodes
            .windows(3)
            .any(|window| window == [FinalOpcode::Nip, FinalOpcode::Nip, FinalOpcode::Return]),
        "return value must survive cleanup of both active iterator markers"
    );
    assert!(
        opcodes.windows(2).any(|window| {
            window[0] == FinalOpcode::Drop
                && matches!(
                    window[1],
                    FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
                )
        }),
        "continue to the outer loop must drop the crossed inner iterator marker"
    );
}

#[test]
fn captured_lexical_binding_is_closed_before_the_next_iteration() {
    let compiled = compile_tree(
        "function f(object){ let saved; for (let key in object) { saved=function capture(){return key;}; } return saved; }",
        "f",
    );
    let opcodes = opcodes(&compiled);
    let next = opcodes
        .iter()
        .position(|&opcode| opcode == FinalOpcode::ForInNext)
        .expect("for-in next");
    let close = opcodes
        .iter()
        .rposition(|&opcode| opcode == FinalOpcode::CloseLoc)
        .expect("per-iteration lexical close");

    assert!(close > next);
    assert!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == FinalOpcode::CloseLoc)
            .count()
            >= 2
    );
}

#[test]
fn final_authority_accepts_every_supported_head_family() {
    for (name, source) in [
        (
            "varHead",
            "function varHead(object){ for (var key in object) break; return key; }",
        ),
        (
            "letHead",
            "function letHead(object){ for (let key in object) break; }",
        ),
        (
            "constHead",
            "function constHead(object){ for (const key in object) {} }",
        ),
        (
            "parameterHead",
            "function parameterHead(object,key){ for (key in object) break; return key; }",
        ),
        (
            "localHead",
            "function localHead(object){ let key; for (key in object) break; return key; }",
        ),
        (
            "staticHead",
            "function staticHead(object,target){ for (target.key in object) break; }",
        ),
        (
            "computedHead",
            "function computedHead(object,target,name){ for (target[name] in object) break; }",
        ),
    ] {
        let tree = compile_verified_tree(source, name);
        let root = tree
            .verified_bytecode()
            .root()
            .function()
            .control_flow()
            .instructions();
        assert!(
            root.iter().any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::ForInStart
            }),
            "{name}"
        );
        assert!(
            root.iter().any(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::ForInNext
            }),
            "{name}"
        );
    }
}

#[test]
fn protocol_instructions_retain_exact_spans_and_stack_anchors() {
    let source = "function spans(object){ for (let key in object) { if (key) continue; break; } }";
    let loop_source = "for (let key in object) { if (key) continue; break; }";
    let compiled = compile_tree(source, "spans");
    let instructions = compiled.control_flow().instructions();

    let start = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::ForInStart
        })
        .expect("for-in start");
    let next = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::ForInNext
        })
        .expect("for-in next");
    assert_eq!(instructions[start].entry_stack_depth(), Some(1));
    assert_eq!(instructions[next].entry_stack_depth(), Some(1));
    assert_eq!(
        source_slice_at(&compiled, source, instructions[start].decoded().pc()),
        "object"
    );
    assert_eq!(
        source_slice_at(&compiled, source, instructions[next].decoded().pc()),
        loop_source
    );

    let key_write = instructions
        .iter()
        .find(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::PutLoc
                    | FinalOpcode::PutLoc8
                    | FinalOpcode::PutLoc0
                    | FinalOpcode::PutLoc1
                    | FinalOpcode::PutLoc2
                    | FinalOpcode::PutLoc3
            ) && instruction.entry_stack_depth() == Some(2)
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "key"
        })
        .expect("iteration key initialization");
    assert_eq!(key_write.entry_stack_depth(), Some(2));

    let loop_drops = instructions
        .iter()
        .filter(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::Drop
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == loop_source
        })
        .map(|instruction| instruction.entry_stack_depth())
        .collect::<Vec<_>>();
    assert!(
        loop_drops.contains(&Some(2)),
        "done value is removed above the iterator"
    );
    assert!(
        loop_drops.contains(&Some(1)),
        "iterator is removed at shared cleanup"
    );

    let continue_jump = instructions
        .iter()
        .find(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
            ) && source_slice_at(&compiled, source, instruction.decoded().pc()) == "continue;"
        })
        .expect("source-owned continue jump");
    assert_eq!(continue_jump.entry_stack_depth(), Some(1));
    assert!(
        compiled.source_instructions().iter().all(|mapping| {
            let span = mapping.span();
            &source[span.start as usize..span.end as usize] != "break;"
        }),
        "the threaded unreachable break trampoline is excised"
    );
}

#[test]
fn nested_labeled_abrupt_cleanup_has_exact_marker_depths_and_spans() {
    let source = "function nested(outer,inner,stop){ outerLoop: for (const a in outer) { \
        for (const b in inner) { \
            if (stop) return a+b; \
            if (b) continue outerLoop; \
            throw b; \
        } \
    } }";
    let compiled = compile_tree(source, "nested");
    let instructions = compiled.control_flow().instructions();

    let return_index = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::Return
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "return a+b;"
        })
        .expect("nested return");
    assert!(return_index >= 2);
    assert_eq!(
        instructions[return_index - 2]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Nip
    );
    assert_eq!(
        instructions[return_index - 1]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Nip
    );
    assert_eq!(instructions[return_index - 2].entry_stack_depth(), Some(3));
    assert_eq!(instructions[return_index - 1].entry_stack_depth(), Some(2));

    let continue_index = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
            ) && source_slice_at(&compiled, source, instruction.decoded().pc())
                == "continue outerLoop;"
        })
        .expect("outer continue");
    assert!(continue_index > 0);
    assert_eq!(
        instructions[continue_index - 1]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Drop
    );
    assert_eq!(
        instructions[continue_index - 1].entry_stack_depth(),
        Some(2)
    );
    assert_eq!(instructions[continue_index].entry_stack_depth(), Some(1));

    let throw_index = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::Throw
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "throw b;"
        })
        .expect("nested throw");
    assert!(throw_index >= 2);
    assert_eq!(
        instructions[throw_index - 2]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Nip
    );
    assert_eq!(
        instructions[throw_index - 1]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Nip
    );
}

#[test]
fn labeled_break_across_nested_for_in_loops_drops_every_crossed_marker() {
    let source = "function nestedBreak(outer,inner){ exit: { for (const a in outer) { \
        for (const b in inner) break exit; \
    } } return 1; }";
    let compiled = compile_tree(source, "nestedBreak");
    let instructions = compiled.control_flow().instructions();
    let break_index = instructions
        .iter()
        .position(|instruction| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::Goto | FinalOpcode::Goto8 | FinalOpcode::Goto16
            ) && source_slice_at(&compiled, source, instruction.decoded().pc()) == "break exit;"
        })
        .expect("labeled break");

    assert!(break_index >= 2);
    assert_eq!(
        instructions[break_index - 2]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Drop
    );
    assert_eq!(
        instructions[break_index - 1]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Drop
    );
    assert_eq!(instructions[break_index - 2].entry_stack_depth(), Some(2));
    assert_eq!(instructions[break_index - 1].entry_stack_depth(), Some(1));
    assert_eq!(instructions[break_index].entry_stack_depth(), Some(0));
}

#[test]
fn loop_body_completion_is_discarded_without_disturbing_the_iterator_marker() {
    let source = "function completion(object){ for (const key in object) 17; return 23; }";
    let compiled = compile_tree(source, "completion");
    let instructions = compiled.control_flow().instructions();
    let body_value = instructions
        .iter()
        .position(|instruction| {
            source_slice_at(&compiled, source, instruction.decoded().pc()) == "17"
                && instruction
                    .decoded()
                    .instruction()
                    .stack_effect()
                    .is_ok_and(|effect| effect.pops() == 0 && effect.pushes() == 1)
        })
        .expect("body expression value");
    assert_eq!(instructions[body_value].entry_stack_depth(), Some(1));
    assert_eq!(
        instructions[body_value + 1]
            .decoded()
            .instruction()
            .opcode(),
        FinalOpcode::Drop
    );
    assert_eq!(instructions[body_value + 1].entry_stack_depth(), Some(2));

    let returned = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::Return
                && source_slice_at(&compiled, source, instruction.decoded().pc()) == "return 23;"
        })
        .expect("explicit return");
    assert_eq!(instructions[returned].entry_stack_depth(), Some(1));
}

#[test]
fn sloppy_var_initializer_precedes_the_single_rhs_evaluation() {
    let source = "function legacy(make,object){ for (var key=7 in make(object)) {} return key; }";
    let compiled = compile_tree(source, "legacy");
    let instructions = compiled.control_flow().instructions();

    let initializer = instructions
        .iter()
        .position(|instruction| {
            source_slice_at(&compiled, source, instruction.decoded().pc()) == "7"
                && instruction
                    .decoded()
                    .instruction()
                    .stack_effect()
                    .is_ok_and(|effect| effect.pops() == 0 && effect.pushes() == 1)
        })
        .expect("legacy initializer");
    let rhs_calls = instructions
        .iter()
        .enumerate()
        .filter(|(_, instruction)| {
            matches!(
                instruction.decoded().instruction().opcode(),
                FinalOpcode::Call
                    | FinalOpcode::Call0
                    | FinalOpcode::Call1
                    | FinalOpcode::Call2
                    | FinalOpcode::Call3
            ) && source_slice_at(&compiled, source, instruction.decoded().pc()) == "make(object)"
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let start = instructions
        .iter()
        .position(|instruction| {
            instruction.decoded().instruction().opcode() == FinalOpcode::ForInStart
        })
        .expect("for-in start");
    assert_eq!(rhs_calls.len(), 1);
    assert!(initializer < rhs_calls[0]);
    assert!(rhs_calls[0] < start);
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| {
                instruction.decoded().instruction().opcode() == FinalOpcode::ForInStart
            })
            .count(),
        1
    );
}

#[test]
fn strict_legacy_var_initializer_is_rejected_by_the_published_oxc_frontend() {
    let source = "function strictHead(object){ \"use strict\"; for (var key=0 in object) {} }";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |_| (),
    )
    .expect_err("strict for-in declaration initializer is an early error");
    assert!(
        error.diagnostics().iter().any(|diagnostic| {
            matches!(
                diagnostic.code,
                FrontendDiagnosticCode::OxcParser | FrontendDiagnosticCode::OxcSemantic
            )
        }),
        "{:?}",
        error.diagnostics()
    );
    assert!(error.diagnostics().iter().any(|diagnostic| {
        diagnostic.labels.iter().any(|label| {
            let text = &source[label.span.start as usize..label.span.end as usize];
            text.contains("key") || text.contains("var")
        })
    }));
}

#[test]
fn declaration_and_assignment_destructuring_heads_use_the_iteration_value() {
    for (name, source, checked_initializations, scope_activations) in [
        (
            "declarationPattern",
            "function declarationPattern(object){for(const {length,[length-1]:tail} in object){return tail;}}",
            2,
            4,
        ),
        (
            "assignmentPattern",
            "function assignmentPattern(object,key){for([key] in object){return key;}}",
            0,
            0,
        ),
    ] {
        let compiled = compile_tree(source, name);
        let opcodes = opcodes(&compiled);
        assert!(opcodes.contains(&FinalOpcode::ForInNext), "{name}");
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&opcode| opcode == FinalOpcode::PutLocCheckInit)
                .count(),
            checked_initializations,
            "{name}"
        );
        assert_eq!(
            opcodes
                .iter()
                .filter(|&&opcode| opcode == FinalOpcode::SetLocUninitialized)
                .count(),
            scope_activations,
            "{name}"
        );
    }
}

#[test]
fn for_in_capacity_failures_are_structured_and_source_owned() {
    let source = "function bounded(object){ for (var key in object) {} }";
    let instruction_error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("bounded"))
                .expect("named function");
            context
                .compile_leaf(
                    &executable,
                    VerificationLimits::new(1_000, 1, 0, 0, 100, 10),
                )
                .expect_err("for-in scaffold exceeds one instruction")
        },
    )
    .expect("front-end acceptance");
    let LeafCompilationError::BytecodeAssembly {
        span: Some(span),
        source:
            AssemblerError::LimitExceeded {
                resource: AssemblerResource::Instructions,
                instruction_index: 1,
                limit: 1,
                observed: 2,
            },
    } = instruction_error
    else {
        panic!("expected exact instruction-capacity failure: {instruction_error:?}");
    };
    assert_eq!(&source[span.start as usize..span.end as usize], "object");

    let stack_error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("bounded"))
                .expect("named function");
            context
                .compile_leaf(
                    &executable,
                    VerificationLimits::new(1_000, 100, 100, 100, 1_000, 2),
                )
                .expect_err("ForInNext requires three operand-stack values")
        },
    )
    .expect("front-end acceptance");
    let LeafCompilationError::BytecodeVerification {
        span: Some(span),
        source: verification,
        ..
    } = stack_error
    else {
        panic!("expected exact stack-capacity failure: {stack_error:?}");
    };
    assert_eq!(
        verification.kind(),
        &VerificationErrorKind::StackLimitExceeded { depth: 3, limit: 2 }
    );
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        "for (var key in object) {}"
    );
}

#[test]
fn final_typed_iterator_analysis_honors_its_frame_state_budget() {
    let source = "function bounded(object){ for (const key in object) {} }";
    let error = with_parsed_program(
        source,
        FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
        |unit| {
            let context = CompilationContext::new(unit).expect("storage planning");
            let executable = context
                .executables()
                .find(|candidate| candidate.metadata().name() == Some("bounded"))
                .expect("named function");
            context
                .compile_tree_with_all_limits(
                    &executable,
                    VerificationLimits::default(),
                    FunctionGraphVerificationLimits::default(),
                    BytecodeGraphVerificationLimits::default().with_max_frame_state_entries(0),
                )
                .expect_err("typed iterator analysis must honor the zero state budget")
        },
    )
    .expect("front-end acceptance");
    let LeafCompilationError::BytecodeGraphVerification { source, .. } = error else {
        panic!("expected final-verifier resource failure: {error:?}");
    };
    assert!(matches!(
        source.kind(),
        BytecodeVerificationErrorKind::LimitExceeded {
            resource: BytecodeGraphResource::FrameStateEntries,
            limit: 0,
            observed: _
        }
    ));
}
