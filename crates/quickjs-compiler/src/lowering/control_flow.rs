use quickjs_bytecode::{
    AssemblerError, AssemblerLabel, AssemblerLimits, AssemblerResource, BranchKind,
    BytecodeAssembler, BytecodePc, CompilerCaptureLayout, CompilerConstantLayout, FinalOpcode,
    FunctionIndexDomains, Operands, UnverifiedCompilerFunctionBody, UnverifiedFunctionHeader,
    VerificationErrorKind, VerificationLimits, VerifiedControlFlow, verify_compiler_control_flow,
};
use quickjs_frontend::Span;

use super::{LeafCompilationError, LocalSlot, SourceInstruction, compact_get_local};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
struct StackAnchor {
    instruction_index: usize,
    span: Span,
    expected_depth: u32,
}

#[derive(Clone, Copy, Debug)]
struct ResolvedStackAnchor {
    pc: BytecodePc,
    span: Span,
    expected_depth: u32,
}

pub(in crate::lowering) struct PlannedControlFlow {
    assembler: BytecodeAssembler,
    max_instructions: u32,
    instruction_spans: Vec<Span>,
    eval_reference_call_instructions: Vec<u32>,
    parameter_initialization_end: Option<u32>,
    function_initializer_prefix_start: Option<u32>,
    label_spans: Vec<Span>,
    stack_anchors: Vec<StackAnchor>,
    last_instruction_can_fall_through: Option<bool>,
    label_bound_after_last_instruction: bool,
    statement_stack_base: u32,
}

#[derive(Debug)]
pub(in crate::lowering) struct FinishedControlFlow {
    bytecode: Vec<u8>,
    source_instructions: Vec<SourceInstruction>,
    eval_reference_call_instructions: Vec<u32>,
    parameter_initialization_end: Option<u32>,
    function_initializer_prefix_start: u32,
    stack_anchors: Vec<ResolvedStackAnchor>,
}

impl PlannedControlFlow {
    pub(in crate::lowering) fn new(limits: VerificationLimits) -> Self {
        let assembler_limits = AssemblerLimits::new(
            limits.max_bytecode_bytes_per_function(),
            limits.max_instructions_per_function(),
            limits.max_transfer_evaluations(),
        );
        Self {
            assembler: BytecodeAssembler::with_limits(assembler_limits),
            max_instructions: limits.max_instructions_per_function(),
            instruction_spans: Vec::new(),
            eval_reference_call_instructions: Vec::new(),
            parameter_initialization_end: None,
            function_initializer_prefix_start: None,
            label_spans: Vec::new(),
            stack_anchors: Vec::new(),
            last_instruction_can_fall_through: None,
            label_bound_after_last_instruction: false,
            statement_stack_base: 0,
        }
    }

    pub(in crate::lowering) fn mark_parameter_initialization_end(
        &mut self,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if self.parameter_initialization_end.is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "parameter initialization has one instruction boundary",
                span: Some(span),
            });
        }
        self.parameter_initialization_end =
            Some(u32::try_from(self.instruction_spans.len()).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "parameter initialization instruction boundary",
                }
            })?);
        Ok(())
    }

    pub(in crate::lowering) fn mark_function_initializer_prefix_start(
        &mut self,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if self.function_initializer_prefix_start.is_some() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "function initializers have one entry-prefix boundary",
                span: Some(span),
            });
        }
        self.function_initializer_prefix_start =
            Some(u32::try_from(self.instruction_spans.len()).map_err(|_| {
                LeafCompilationError::CapacityExceeded {
                    domain: "function initializer entry-prefix boundary",
                }
            })?);
        Ok(())
    }

    pub(in crate::lowering) fn emit(
        &mut self,
        instruction: PlannedInstruction,
    ) -> Result<(), LeafCompilationError> {
        if instruction.eval_reference_call
            && !matches!(
                (instruction.opcode, instruction.operands),
                (FinalOpcode::Eval, Operands::NPopU16 { .. })
                    | (FinalOpcode::ApplyEval, Operands::U16(_))
            )
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "eval reference-call metadata names an eval-family instruction",
                span: Some(instruction.span),
            });
        }
        self.assembler
            .push(instruction.opcode, instruction.operands)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(instruction.span),
                source,
            })?;
        if instruction.eval_reference_call {
            self.eval_reference_call_instructions.push(
                u32::try_from(self.instruction_spans.len()).map_err(|_| {
                    LeafCompilationError::CapacityExceeded {
                        domain: "eval reference-call instruction indices",
                    }
                })?,
            );
        }
        self.instruction_spans.push(instruction.span);
        self.last_instruction_can_fall_through = Some(!matches!(
            instruction.opcode,
            FinalOpcode::Ret
                | FinalOpcode::Return
                | FinalOpcode::ReturnUndef
                | FinalOpcode::ReturnAsync
                | FinalOpcode::Throw
        ));
        self.label_bound_after_last_instruction = false;
        Ok(())
    }

    pub(in crate::lowering) fn ensure_additional_instruction_capacity(
        &self,
        additional: u64,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let current = u64::try_from(self.instruction_spans.len()).map_err(|_| {
            LeafCompilationError::CapacityExceeded {
                domain: "planned instruction count",
            }
        })?;
        let required =
            current
                .checked_add(additional)
                .ok_or(LeafCompilationError::CapacityExceeded {
                    domain: "planned instruction count",
                })?;
        let limit = u64::from(self.max_instructions);
        if required > limit {
            return Err(LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source: AssemblerError::LimitExceeded {
                    resource: AssemblerResource::Instructions,
                    instruction_index: self.max_instructions,
                    limit,
                    observed: limit + 1,
                },
            });
        }
        Ok(())
    }

    pub(in crate::lowering) fn new_label(
        &mut self,
        span: Span,
    ) -> Result<CompilerLabel, LeafCompilationError> {
        self.new_label_with_expected_depth(span, None)
    }

    pub(in crate::lowering) fn new_statement_label(
        &mut self,
        span: Span,
    ) -> Result<CompilerLabel, LeafCompilationError> {
        self.new_label_with_expected_depth(span, Some(self.statement_stack_base))
    }

    pub(in crate::lowering) fn new_statement_label_with_offset(
        &mut self,
        span: Span,
        offset: u32,
    ) -> Result<CompilerLabel, LeafCompilationError> {
        let expected = self.statement_stack_base.checked_add(offset).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "statement operand-stack base",
            },
        )?;
        self.new_label_with_expected_depth(span, Some(expected))
    }

    pub(in crate::lowering) fn push_statement_stack_base(
        &mut self,
        _span: Span,
    ) -> Result<(), LeafCompilationError> {
        self.statement_stack_base = self.statement_stack_base.checked_add(1).ok_or(
            LeafCompilationError::CapacityExceeded {
                domain: "statement operand-stack base",
            },
        )?;
        Ok(())
    }

    pub(in crate::lowering) fn pop_statement_stack_base(
        &mut self,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        self.statement_stack_base = self.statement_stack_base.checked_sub(1).ok_or(
            LeafCompilationError::SemanticInvariant {
                invariant: "statement operand-stack base is nonempty on exit",
                span: Some(span),
            },
        )?;
        Ok(())
    }

    fn new_label_with_expected_depth(
        &mut self,
        span: Span,
        expected_stack_depth: Option<u32>,
    ) -> Result<CompilerLabel, LeafCompilationError> {
        self.label_spans
            .try_reserve(1)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "label source spans",
            })?;
        let assembler = self.assembler.new_label().map_err(|source| {
            LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source,
            }
        })?;
        self.label_spans.push(span);
        Ok(CompilerLabel {
            assembler,
            owner_span: span,
            expected_stack_depth,
        })
    }

    pub(in crate::lowering) fn branch(
        &mut self,
        kind: BranchKind,
        target: &CompilerLabel,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        self.assembler
            .branch(kind, &target.assembler)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source,
            })?;
        self.instruction_spans.push(span);
        self.last_instruction_can_fall_through = Some(kind != BranchKind::Goto);
        self.label_bound_after_last_instruction = false;
        Ok(())
    }

    pub(in crate::lowering) fn with_branch(
        &mut self,
        opcode: FinalOpcode,
        atom: quickjs_bytecode::AtomPoolIndex,
        value: u8,
        target: &CompilerLabel,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        self.assembler
            .with_branch(opcode, atom, value, &target.assembler)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(span),
                source,
            })?;
        self.instruction_spans.push(span);
        self.last_instruction_can_fall_through = Some(true);
        self.label_bound_after_last_instruction = false;
        Ok(())
    }

    pub(in crate::lowering) fn bind(
        &mut self,
        label: &CompilerLabel,
    ) -> Result<(), LeafCompilationError> {
        self.assembler.bind(&label.assembler).map_err(|source| {
            LeafCompilationError::BytecodeAssembly {
                span: Some(label.owner_span),
                source,
            }
        })?;
        if let Some(expected_depth) = label.expected_stack_depth {
            self.stack_anchors.push(StackAnchor {
                instruction_index: self.instruction_spans.len(),
                span: label.owner_span,
                expected_depth,
            });
        }
        self.label_bound_after_last_instruction = true;
        Ok(())
    }

    pub(in crate::lowering) fn ensure_terminal(
        &mut self,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if self.label_bound_after_last_instruction
            || self.last_instruction_can_fall_through.unwrap_or(true)
        {
            self.emit(PlannedInstruction::new(
                FinalOpcode::ReturnUndef,
                Operands::None,
                span,
            ))?;
        }
        Ok(())
    }

    pub(in crate::lowering) fn ensure_generator_terminal(
        &mut self,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if self.label_bound_after_last_instruction
            || self.last_instruction_can_fall_through.unwrap_or(true)
        {
            self.emit(PlannedInstruction::new(
                FinalOpcode::Undefined,
                Operands::None,
                span,
            ))?;
            self.emit(PlannedInstruction::new(
                FinalOpcode::ReturnAsync,
                Operands::None,
                span,
            ))?;
        }
        Ok(())
    }

    pub(in crate::lowering) fn ensure_script_terminal(
        &mut self,
        completion: LocalSlot,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        if self.label_bound_after_last_instruction
            || self.last_instruction_can_fall_through.unwrap_or(true)
        {
            let (opcode, operands) = compact_get_local(completion);
            self.emit(PlannedInstruction::new(opcode, operands, span))?;
            self.emit(PlannedInstruction::new(
                FinalOpcode::Return,
                Operands::None,
                span,
            ))?;
        }
        Ok(())
    }

    pub(in crate::lowering) fn finish(self) -> Result<FinishedControlFlow, LeafCompilationError> {
        if self.statement_stack_base != 0 {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "statement operand-stack base returns to zero",
                span: self.instruction_spans.last().copied(),
            });
        }
        let Self {
            assembler,
            max_instructions: _,
            instruction_spans: spans,
            eval_reference_call_instructions,
            parameter_initialization_end,
            function_initializer_prefix_start,
            label_spans,
            stack_anchors,
            last_instruction_can_fall_through: _,
            label_bound_after_last_instruction: _,
            statement_stack_base: _,
        } = self;
        let assembled = match assembler.finish() {
            Ok(assembled) => assembled,
            Err(AssemblerError::Encoding {
                instruction_index,
                source,
            }) => {
                let span = spans.get(instruction_index as usize).copied().ok_or(
                    LeafCompilationError::SemanticInvariant {
                        invariant: "assembler encoding failure indexes a planned source span",
                        span: None,
                    },
                )?;
                return Err(LeafCompilationError::BytecodeEncoding { span, source });
            }
            Err(source) => {
                let span = source
                    .instruction_index()
                    .and_then(|index| spans.get(index as usize).copied())
                    .or_else(|| {
                        source
                            .label_index()
                            .and_then(|index| label_spans.get(index as usize).copied())
                    });
                return Err(LeafCompilationError::BytecodeAssembly { span, source });
            }
        };
        let (bytecode, instruction_pcs) = assembled.into_parts();
        if instruction_pcs.len() != spans.len() {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "assembler returns one final PC per planned instruction",
                span: spans.last().copied(),
            });
        }
        let mut resolved_stack_anchors = Vec::with_capacity(stack_anchors.len());
        for anchor in stack_anchors {
            let Some(pc) = instruction_pcs.get(anchor.instruction_index).copied() else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "statement stack anchor resolves to a final instruction",
                    span: Some(anchor.span),
                });
            };
            resolved_stack_anchors.push(ResolvedStackAnchor {
                pc,
                span: anchor.span,
                expected_depth: anchor.expected_depth,
            });
        }
        let source_instructions = instruction_pcs
            .into_iter()
            .zip(spans)
            .map(|(pc, span)| SourceInstruction { pc, span })
            .collect();
        Ok(FinishedControlFlow {
            bytecode,
            source_instructions,
            eval_reference_call_instructions,
            parameter_initialization_end,
            function_initializer_prefix_start: function_initializer_prefix_start.unwrap_or(0),
            stack_anchors: resolved_stack_anchors,
        })
    }
}

impl FinishedControlFlow {
    pub(in crate::lowering) const fn parameter_initialization_end(&self) -> Option<u32> {
        self.parameter_initialization_end
    }

    pub(in crate::lowering) const fn function_initializer_prefix_start(&self) -> u32 {
        self.function_initializer_prefix_start
    }

    pub(in crate::lowering) fn eval_reference_call_instructions(&self) -> &[u32] {
        &self.eval_reference_call_instructions
    }

    #[cfg(test)]
    fn verify(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        self.verify_with_capture_layout(domains, header, CompilerCaptureLayout::default(), limits)
    }

    #[cfg(test)]
    pub(in crate::lowering) fn verify_with_capture_layout(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        capture_layout: CompilerCaptureLayout,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        self.verify_with_layouts(
            domains,
            header,
            capture_layout,
            CompilerConstantLayout::default(),
            limits,
        )
    }

    pub(in crate::lowering) fn verify_with_layouts(
        self,
        domains: FunctionIndexDomains,
        header: UnverifiedFunctionHeader,
        capture_layout: CompilerCaptureLayout,
        constant_layout: CompilerConstantLayout,
        limits: VerificationLimits,
    ) -> Result<(Vec<SourceInstruction>, VerifiedControlFlow), LeafCompilationError> {
        let Self {
            bytecode,
            source_instructions,
            eval_reference_call_instructions: _,
            parameter_initialization_end: _,
            function_initializer_prefix_start: _,
            stack_anchors,
        } = self;
        let control_flow = match verify_compiler_control_flow(
            UnverifiedCompilerFunctionBody::new(bytecode, domains, header)
                .with_capture_layout(capture_layout)
                .with_constant_layout(constant_layout),
            limits,
        ) {
            Ok(control_flow) => control_flow,
            Err(source) => {
                let span = match source.pc() {
                    Some(pc) => Some(exact_source_span(&source_instructions, pc).ok_or(
                        LeafCompilationError::SemanticInvariant {
                            invariant:
                                "verifier instruction PC resolves to an exact source instruction",
                            span: None,
                        },
                    )?),
                    None => None,
                };
                let related_span = match source.kind() {
                VerificationErrorKind::InconsistentStackAtJoin { target, .. } => {
                        Some(exact_source_span(&source_instructions, *target).ok_or(
                            LeafCompilationError::SemanticInvariant {
                                invariant:
                                    "verifier join target resolves to an exact source instruction",
                                span: None,
                            },
                        )?)
                }
                _ => None,
            };
                return Err(LeafCompilationError::BytecodeVerification {
                    span,
                    related_span,
                    source,
                });
            }
        };

        for anchor in stack_anchors {
            let Some(index) = control_flow.instruction_index_at(anchor.pc) else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "resolved statement stack anchor remains an instruction start",
                    span: Some(anchor.span),
                });
            };
            let Some(instruction) = control_flow.instruction(index) else {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "resolved statement stack anchor has a verified instruction",
                    span: Some(anchor.span),
                });
            };
            let Some(actual) = instruction.entry_stack_depth() else {
                continue;
            };
            if actual != anchor.expected_depth {
                return Err(LeafCompilationError::BytecodeStackInvariant {
                    span: anchor.span,
                    pc: anchor.pc,
                    expected: anchor.expected_depth,
                    actual,
                });
            }
        }

        Ok((source_instructions, control_flow))
    }
}

pub(in crate::lowering) fn exact_source_span(
    source_instructions: &[SourceInstruction],
    pc: BytecodePc,
) -> Option<Span> {
    source_instructions
        .binary_search_by_key(&pc, |instruction| instruction.pc())
        .ok()
        .map(|index| source_instructions[index].span())
}

#[derive(Clone, Copy)]
pub(in crate::lowering) struct PlannedInstruction {
    opcode: FinalOpcode,
    operands: Operands,
    span: Span,
    eval_reference_call: bool,
}

impl PlannedInstruction {
    pub(in crate::lowering) const fn new(
        opcode: FinalOpcode,
        operands: Operands,
        span: Span,
    ) -> Self {
        Self {
            opcode,
            operands,
            span,
            eval_reference_call: false,
        }
    }

    pub(in crate::lowering) const fn with_eval_reference_call(mut self) -> Self {
        self.eval_reference_call = true;
        self
    }
}

#[derive(Clone)]
pub(in crate::lowering) struct CompilerLabel {
    assembler: AssemblerLabel,
    owner_span: Span,
    expected_stack_depth: Option<u32>,
}

impl CompilerLabel {
    #[cfg(test)]
    pub(in crate::lowering) const fn owner_span(&self) -> Span {
        self.owner_span
    }
}
