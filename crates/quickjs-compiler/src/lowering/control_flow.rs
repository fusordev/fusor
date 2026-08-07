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
            label_spans: Vec::new(),
            stack_anchors: Vec::new(),
            last_instruction_can_fall_through: None,
            label_bound_after_last_instruction: false,
            statement_stack_base: 0,
        }
    }

    pub(in crate::lowering) fn emit(
        &mut self,
        instruction: PlannedInstruction,
    ) -> Result<(), LeafCompilationError> {
        self.assembler
            .push(instruction.opcode, instruction.operands)
            .map_err(|source| LeafCompilationError::BytecodeAssembly {
                span: Some(instruction.span),
                source,
            })?;
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
            stack_anchors: resolved_stack_anchors,
        })
    }
}

impl FinishedControlFlow {
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
        }
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
