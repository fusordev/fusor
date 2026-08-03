use oxc_ast::ast::{Function, FunctionBody, Program};
use quickjs_bytecode::VerificationLimits;
use quickjs_frontend::Span;

use super::{
    CompilationContext, CompiledConstantPool, FrameLayout, FunctionTreeLayout,
    LeafCompilationError, LocalSlot, PlannedControlFlow, StatementCompletion,
    StatementControlStack, StatementPlanningState, StatementWork,
};
use crate::storage::ExecutableId;

#[derive(Clone, Copy)]
pub(in crate::lowering) struct FunctionPlanningContext<'layout> {
    pub(in crate::lowering) executable: ExecutableId,
    pub(in crate::lowering) layout: &'layout FrameLayout,
    pub(in crate::lowering) tree_layout: &'layout FunctionTreeLayout,
    pub(in crate::lowering) constants: &'layout CompiledConstantPool,
}

impl FunctionPlanningContext<'_> {
    fn validate_owner(self) -> Result<(), LeafCompilationError> {
        if self.layout.executable != self.executable {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function lowering session owns exactly one executable frame",
                span: None,
            });
        }
        Ok(())
    }
}

enum FunctionTerminal {
    Ordinary,
    Script(LocalSlot),
}

/// Mutable lowering state for exactly one selected executable.
pub(in crate::lowering) struct FunctionLoweringSession<
    'compiler,
    'statement,
    'unit,
    'arena,
    'scope,
    'layout,
> {
    compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
    planning: FunctionPlanningContext<'layout>,
    body_span: Span,
    state: StatementPlanningState<'statement, 'arena>,
    flow: PlannedControlFlow,
    terminal: FunctionTerminal,
}

impl<'compiler, 'statement, 'unit, 'arena, 'scope, 'layout>
    FunctionLoweringSession<'compiler, 'statement, 'unit, 'arena, 'scope, 'layout>
{
    pub(in crate::lowering) fn for_function(
        compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
        function: &'statement Function<'arena>,
        body: &'statement FunctionBody<'arena>,
        planning: FunctionPlanningContext<'layout>,
        limits: VerificationLimits,
    ) -> Result<Self, LeafCompilationError> {
        planning.validate_owner()?;
        let function_scope = compiler.created_scope(
            function.scope_id.get(),
            function.node_id.get(),
            function.span,
        )?;
        Ok(Self {
            compiler,
            planning,
            body_span: body.span,
            state: StatementPlanningState {
                work: vec![
                    StatementWork::PopScope(function_scope),
                    StatementWork::VisitList {
                        statements: &body.statements,
                        next: 0,
                    },
                    StatementWork::PushScope {
                        scope: function_scope,
                        creator: function.node_id.get(),
                        span: function.span,
                    },
                ],
                active_scopes: Vec::new(),
                controls: StatementControlStack::default(),
                abrupt_markers: Vec::new(),
                completion: StatementCompletion::Discard,
            },
            flow: PlannedControlFlow::new(limits),
            terminal: FunctionTerminal::Ordinary,
        })
    }

    pub(in crate::lowering) fn for_program(
        compiler: &'compiler CompilationContext<'unit, 'arena, 'scope>,
        program: &'statement Program<'arena>,
        completion: LocalSlot,
        planning: FunctionPlanningContext<'layout>,
        limits: VerificationLimits,
    ) -> Result<Self, LeafCompilationError> {
        planning.validate_owner()?;
        let program_scope =
            compiler.created_scope(program.scope_id.get(), program.node_id.get(), program.span)?;
        Ok(Self {
            compiler,
            planning,
            body_span: program.span,
            state: StatementPlanningState {
                work: vec![
                    StatementWork::PopScope(program_scope),
                    StatementWork::VisitList {
                        statements: &program.body,
                        next: 0,
                    },
                    StatementWork::PushScope {
                        scope: program_scope,
                        creator: program.node_id.get(),
                        span: program.span,
                    },
                ],
                active_scopes: Vec::new(),
                controls: StatementControlStack::default(),
                abrupt_markers: Vec::new(),
                completion: StatementCompletion::Script(completion),
            },
            flow: PlannedControlFlow::new(limits),
            terminal: FunctionTerminal::Script(completion),
        })
    }

    pub(in crate::lowering) fn lower(mut self) -> Result<PlannedControlFlow, LeafCompilationError> {
        while let Some(task) = self.state.work.pop() {
            self.compiler.process_statement_work(
                task,
                self.body_span,
                &self.planning,
                &mut self.flow,
                &mut self.state,
            )?;
        }
        if !self.state.active_scopes.is_empty()
            || !self.state.controls.is_empty()
            || !self.state.abrupt_markers.is_empty()
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: match self.terminal {
                    FunctionTerminal::Ordinary => {
                        "statement planning closes every scope and control region"
                    }
                    FunctionTerminal::Script(_) => {
                        "Program planning closes every scope and control region"
                    }
                },
                span: Some(self.body_span),
            });
        }
        match self.terminal {
            FunctionTerminal::Ordinary => self.flow.ensure_terminal(self.body_span)?,
            FunctionTerminal::Script(completion) => self
                .flow
                .ensure_script_terminal(completion, self.body_span)?,
        }
        Ok(self.flow)
    }
}

#[cfg(test)]
mod tests {
    use quickjs_frontend::{
        CompilationGoal, FrontendOptions, GlobalScriptGoal, with_parsed_program,
    };

    use crate::lowering::{
        CompilationContext, FrameLayout, FrameLayoutInput, FunctionPlanningContext,
        LeafCompilationError,
    };

    #[test]
    fn planning_context_rejects_a_frame_from_another_executable() {
        with_parsed_program(
            "function outer(){ function child(){} }",
            FrontendOptions::for_goal(CompilationGoal::GlobalScript(GlobalScriptGoal::new())),
            |unit| {
                let context = CompilationContext::new(unit).expect("storage plan");
                let plan = context.storage_plan();
                let executable = |name| {
                    plan.executables()
                        .iter()
                        .find(|executable| executable.name() == Some(name))
                        .expect("named executable")
                        .id()
                };
                let outer = executable("outer");
                let child = executable("child");
                let layout = FrameLayout::new(FrameLayoutInput::new(plan, outer))
                    .expect("outer frame layout");
                let tree_layout = context
                    .function_tree_layout()
                    .expect("function tree layout");
                let planning = FunctionPlanningContext {
                    executable: child,
                    layout: &layout,
                    tree_layout: &tree_layout,
                    constants: tree_layout.constant_pool(child).expect("child constants"),
                };

                assert!(matches!(
                    planning.validate_owner(),
                    Err(LeafCompilationError::SemanticInvariant {
                        invariant: "function lowering session owns exactly one executable frame",
                        span: None,
                    })
                ));
            },
        )
        .expect("front-end acceptance");
    }
}
