use std::collections::HashMap;

use quickjs_frontend::Span;

use crate::lowering::{CompilerLabel, LeafCompilationError};

use super::abrupt::IterationMarkerKind;

#[derive(Clone)]
pub(in crate::lowering) struct ControlRegion<'statement> {
    pub(in crate::lowering) labels: Vec<&'statement str>,
    pub(in crate::lowering) break_target: CompilerLabel,
    pub(in crate::lowering) continue_target: Option<CompilerLabel>,
    pub(in crate::lowering) accepts_unlabeled_break: bool,
    pub(in crate::lowering) scope_depth: usize,
    pub(in crate::lowering) owned_iteration_marker: Option<IterationMarkerKind>,
    pub(in crate::lowering) owned_marker_scope_depth: Option<usize>,
    pub(in crate::lowering) abrupt_marker_depth: Option<usize>,
}

impl<'statement> ControlRegion<'statement> {
    pub(in crate::lowering) fn iteration(
        labels: Vec<&'statement str>,
        break_target: CompilerLabel,
        continue_target: CompilerLabel,
        scope_depth: usize,
    ) -> Self {
        Self {
            labels,
            break_target,
            continue_target: Some(continue_target),
            accepts_unlabeled_break: true,
            scope_depth,
            owned_iteration_marker: None,
            owned_marker_scope_depth: None,
            abrupt_marker_depth: None,
        }
    }

    pub(in crate::lowering) fn for_in_iteration(
        labels: Vec<&'statement str>,
        break_target: CompilerLabel,
        continue_target: CompilerLabel,
        scope_depth: usize,
    ) -> Self {
        Self {
            labels,
            break_target,
            continue_target: Some(continue_target),
            accepts_unlabeled_break: true,
            scope_depth,
            owned_iteration_marker: Some(IterationMarkerKind::ForIn),
            owned_marker_scope_depth: Some(scope_depth),
            abrupt_marker_depth: None,
        }
    }

    pub(in crate::lowering) fn for_of_iteration(
        labels: Vec<&'statement str>,
        break_target: CompilerLabel,
        continue_target: CompilerLabel,
        scope_depth: usize,
    ) -> Self {
        Self {
            labels,
            break_target,
            continue_target: Some(continue_target),
            accepts_unlabeled_break: true,
            scope_depth,
            owned_iteration_marker: Some(IterationMarkerKind::ForOf),
            owned_marker_scope_depth: Some(scope_depth),
            abrupt_marker_depth: None,
        }
    }

    pub(in crate::lowering) fn breakable(
        labels: Vec<&'statement str>,
        break_target: CompilerLabel,
        accepts_unlabeled_break: bool,
        scope_depth: usize,
    ) -> Self {
        Self {
            labels,
            break_target,
            continue_target: None,
            accepts_unlabeled_break,
            scope_depth,
            owned_iteration_marker: None,
            owned_marker_scope_depth: None,
            abrupt_marker_depth: None,
        }
    }
}

#[derive(Default)]
pub(in crate::lowering) struct StatementControlStack<'statement> {
    regions: Vec<ControlRegion<'statement>>,
    labeled: HashMap<&'statement str, usize>,
    breakable: Vec<usize>,
    iterations: Vec<usize>,
}

impl<'statement> StatementControlStack<'statement> {
    #[cfg(test)]
    pub(in crate::lowering) fn with_control(
        mut control: ControlRegion<'statement>,
        span: Span,
    ) -> Result<Self, LeafCompilationError> {
        let mut controls = Self::default();
        control.abrupt_marker_depth = Some(usize::from(control.owned_iteration_marker.is_some()));
        controls.push(control, span)?;
        Ok(controls)
    }

    pub(in crate::lowering) fn push(
        &mut self,
        control: ControlRegion<'statement>,
        span: Span,
    ) -> Result<(), LeafCompilationError> {
        let index = self.regions.len();
        self.regions
            .try_reserve(1)
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement control regions",
            })?;
        self.labeled
            .try_reserve(control.labels.len())
            .map_err(|_| LeafCompilationError::CapacityExceeded {
                domain: "statement labeled control targets",
            })?;
        if control.accepts_unlabeled_break {
            self.breakable
                .try_reserve(1)
                .map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "statement break targets",
                })?;
        }
        if control.continue_target.is_some() {
            self.iterations
                .try_reserve(1)
                .map_err(|_| LeafCompilationError::CapacityExceeded {
                    domain: "statement continue targets",
                })?;
        }
        if control
            .labels
            .iter()
            .any(|label| self.labeled.contains_key(label))
        {
            return Err(LeafCompilationError::SemanticInvariant {
                invariant: "Oxc rejects duplicate active statement labels",
                span: Some(span),
            });
        }
        for label in &control.labels {
            if self.labeled.insert(label, index).is_some() {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "Oxc rejects duplicate labels in one statement chain",
                    span: Some(span),
                });
            }
        }
        if control.accepts_unlabeled_break {
            self.breakable.push(index);
        }
        if control.continue_target.is_some() {
            self.iterations.push(index);
        }
        self.regions.push(control);
        Ok(())
    }

    pub(in crate::lowering) fn pop(
        &mut self,
        span: Span,
    ) -> Result<ControlRegion<'statement>, LeafCompilationError> {
        let index =
            self.regions
                .len()
                .checked_sub(1)
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "statement control-region stack is nonempty on exit",
                    span: Some(span),
                })?;
        let control = self
            .regions
            .pop()
            .ok_or(LeafCompilationError::SemanticInvariant {
                invariant: "statement control-region index names a region",
                span: Some(span),
            })?;
        if control.accepts_unlabeled_break {
            let actual = self
                .breakable
                .pop()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "statement break target stack is nonempty on exit",
                    span: Some(span),
                })?;
            if actual != index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "statement break targets exit in last-in-first-out order",
                    span: Some(span),
                });
            }
        }
        if control.continue_target.is_some() {
            let actual = self
                .iterations
                .pop()
                .ok_or(LeafCompilationError::SemanticInvariant {
                    invariant: "statement continue target stack is nonempty on exit",
                    span: Some(span),
                })?;
            if actual != index {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "statement continue targets exit in last-in-first-out order",
                    span: Some(span),
                });
            }
        }
        for label in &control.labels {
            if self.labeled.remove(label) != Some(index) {
                return Err(LeafCompilationError::SemanticInvariant {
                    invariant: "statement label names its active control region",
                    span: Some(span),
                });
            }
        }
        Ok(control)
    }

    pub(in crate::lowering) fn resolve(
        &self,
        label: Option<&str>,
        jump: LoopJump,
    ) -> Option<(usize, &ControlRegion<'statement>)> {
        let index = match (label, jump) {
            (Some(label), _) => self.labeled.get(label),
            (None, LoopJump::Break) => self.breakable.last(),
            (None, LoopJump::Continue) => self.iterations.last(),
        }?;
        self.regions.get(*index).map(|control| (*index, control))
    }

    pub(in crate::lowering) fn is_empty(&self) -> bool {
        self.regions.is_empty()
            && self.labeled.is_empty()
            && self.breakable.is_empty()
            && self.iterations.is_empty()
    }
}

pub(in crate::lowering) struct SwitchControlLabels {
    pub(in crate::lowering) body: Vec<CompilerLabel>,
    pub(in crate::lowering) matched: Vec<CompilerLabel>,
    pub(in crate::lowering) fallback: CompilerLabel,
    pub(in crate::lowering) no_match: Option<CompilerLabel>,
}

pub(in crate::lowering) fn switch_scaffold_instruction_count(
    case_count: usize,
    tested_count: usize,
    has_default: bool,
) -> Result<u64, LeafCompilationError> {
    let cases = u64::try_from(case_count).map_err(|_| LeafCompilationError::CapacityExceeded {
        domain: "switch scaffold instructions",
    })?;
    let tested =
        u64::try_from(tested_count).map_err(|_| LeafCompilationError::CapacityExceeded {
            domain: "switch scaffold instructions",
        })?;
    tested
        .checked_mul(3)
        .and_then(|count| {
            cases
                .checked_mul(2)
                .and_then(|cases| count.checked_add(cases))
        })
        .and_then(|count| count.checked_add(1))
        .and_then(|count| count.checked_add(if has_default { 0 } else { 2 }))
        .ok_or(LeafCompilationError::CapacityExceeded {
            domain: "switch scaffold instructions",
        })
}

#[derive(Clone, Copy)]
pub(in crate::lowering) enum LoopJump {
    Break,
    Continue,
}

impl LoopJump {
    pub(in crate::lowering) const fn missing_region_invariant(self) -> &'static str {
        match self {
            Self::Break => "Oxc accepts unlabeled break only in a breakable region",
            Self::Continue => "Oxc accepts unlabeled continue only in an iteration",
        }
    }

    pub(in crate::lowering) const fn missing_label_invariant(self) -> &'static str {
        match self {
            Self::Break => "Oxc resolves labeled break to an enclosing statement",
            Self::Continue => "Oxc resolves labeled continue to an enclosing iteration",
        }
    }

    pub(in crate::lowering) const fn invalid_labeled_target_invariant(self) -> &'static str {
        match self {
            Self::Break => "every labeled control region has a break target",
            Self::Continue => "Oxc resolves labeled continue only to an iteration target",
        }
    }

    pub(in crate::lowering) const fn scope_invariant(self) -> &'static str {
        match self {
            Self::Break => "break target scope encloses the abrupt statement",
            Self::Continue => "continue target scope encloses the abrupt statement",
        }
    }

    pub(in crate::lowering) const fn target<'control>(
        self,
        control: &'control ControlRegion<'_>,
    ) -> Option<&'control CompilerLabel> {
        match self {
            Self::Break => Some(&control.break_target),
            Self::Continue => control.continue_target.as_ref(),
        }
    }
}
