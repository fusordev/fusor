use std::{borrow::Cow, collections::HashMap};

use oxc_allocator::Allocator;
use oxc_regular_expression::{
    LiteralParser, Options,
    ast::{
        self, Alternative, BoundaryAssertionKind, CharacterClassContents,
        CharacterClassContentsKind, CharacterClassEscapeKind, Disjunction, LookAroundAssertionKind,
        Modifier, Pattern, Term,
    },
};
use oxc_span::GetSpan;

use crate::{
    CompileError, CompileLimits, emoji,
    flags::{MatchMode, RegExpFlags},
    program::{
        BoundaryKind, CharacterClass, CharacterClassItem, CharacterClassKind, Direction,
        Instruction, LookaroundKind, Program,
    },
    properties::UnicodeProperty,
};

pub(crate) fn compile(
    pattern_source: &str,
    flag_source: &str,
    limits: CompileLimits,
) -> Result<(Program, String), CompileError> {
    if pattern_source.len() > limits.max_pattern_bytes {
        return Err(CompileError::ResourceLimit("source length"));
    }
    let flags = RegExpFlags::parse(flag_source)?;
    let canonical_flags = flags.canonical_source();
    let annex_b_source = annex_b_pattern_source(
        pattern_source,
        flags.unicode_mode(),
        limits.max_pattern_bytes,
    )?;
    validate_unicode_control_escapes(annex_b_source.as_ref(), flags.unicode_mode())?;
    let pattern_source = normalize_group_name_source(annex_b_source.as_ref())?;
    let allocator = Allocator::default();
    let pattern = LiteralParser::new(
        &allocator,
        &pattern_source,
        Some(&canonical_flags),
        Options::default(),
    )
    .parse()
    .map_err(|error| CompileError::Syntax(error.to_string()))?;
    let mut compiler = Compiler::new(flags, limits);
    compiler.collect_captures(&pattern)?;
    let program = compiler.build(&pattern)?;
    Ok((program, canonical_flags))
}

pub(crate) fn validate_literal(
    pattern_source: &str,
    flag_source: &str,
    max_pattern_bytes: usize,
) -> Result<(), CompileError> {
    if pattern_source.len() > max_pattern_bytes {
        return Err(CompileError::ResourceLimit("source length"));
    }
    let flags = RegExpFlags::parse(flag_source)?;
    let canonical_flags = flags.canonical_source();
    let annex_b_source =
        annex_b_pattern_source(pattern_source, flags.unicode_mode(), max_pattern_bytes)?;
    validate_unicode_control_escapes(annex_b_source.as_ref(), flags.unicode_mode())?;
    let pattern_source = normalize_group_name_source(annex_b_source.as_ref())?;
    let allocator = Allocator::default();
    LiteralParser::new(
        &allocator,
        &pattern_source,
        Some(&canonical_flags),
        Options::default(),
    )
    .parse()
    .map_err(|error| CompileError::Syntax(error.to_string()))?;
    Ok(())
}

/// Normalizes the two Annex B productions that the published Oxc regexp AST
/// currently loses information for. The rewrite is compiler-internal: the
/// embedding retains the original pattern for `RegExp.prototype.source`.
///
/// In non-Unicode mode, `\c` followed by a decimal digit or `_` denotes the
/// code unit modulo 32 only inside a character class. A three-digit legacy
/// octal escape beginning with 4--7 consumes two octal digits and leaves the
/// third digit as a following `PatternCharacter`. Rewriting both forms to an
/// equivalent hexadecimal escape preserves those ordered grammar semantics
/// without patching or vendoring the parser dependency.
fn annex_b_pattern_source(
    source: &str,
    unicode_mode: bool,
    max_pattern_bytes: usize,
) -> Result<Cow<'_, str>, CompileError> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    if unicode_mode {
        return Ok(Cow::Borrowed(source));
    }

    let bytes = source.as_bytes();
    let mut replacements = Vec::new();
    let mut in_class = false;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if in_class
                && bytes.get(index + 1) == Some(&b'c')
                && let Some(&control) = bytes.get(index + 2)
                && (control.is_ascii_digit() || control == b'_')
            {
                replacements.push((index, index + 3, control % 32));
                index += 3;
                continue;
            }

            if let (Some(&first @ b'4'..=b'7'), Some(&second @ b'0'..=b'7'), Some(b'0'..=b'7')) = (
                bytes.get(index + 1),
                bytes.get(index + 2),
                bytes.get(index + 3),
            ) {
                replacements.push((index, index + 3, (first - b'0') * 8 + (second - b'0')));
                index += 3;
                continue;
            }

            index = index.saturating_add(2);
            continue;
        }

        match bytes[index] {
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            _ => {}
        }
        index += 1;
    }

    if replacements.is_empty() {
        return Ok(Cow::Borrowed(source));
    }

    let normalized_len = source
        .len()
        .checked_add(replacements.len())
        .ok_or(CompileError::ResourceLimit("source length"))?;
    if normalized_len > max_pattern_bytes {
        return Err(CompileError::ResourceLimit("source length"));
    }
    let mut normalized = String::new();
    normalized
        .try_reserve_exact(normalized_len)
        .map_err(|_| CompileError::ResourceLimit("source allocation"))?;
    let mut copied = 0;
    for (start, end, value) in replacements {
        normalized.push_str(&source[copied..start]);
        normalized.push('\\');
        normalized.push('x');
        normalized.push(char::from(HEX[usize::from(value >> 4)]));
        normalized.push(char::from(HEX[usize::from(value & 0x0f)]));
        copied = end;
    }
    normalized.push_str(&source[copied..]);
    debug_assert_eq!(normalized.len(), normalized_len);
    Ok(Cow::Owned(normalized))
}

/// Enforces the Unicode grammar's `c AsciiLetter` boundary before Oxc's class
/// parser can recover a bare `\c` as a legacy identity escape.
fn validate_unicode_control_escapes(source: &str, unicode_mode: bool) -> Result<(), CompileError> {
    if !unicode_mode {
        return Ok(());
    }
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        let Some(&escaped) = bytes.get(index + 1) else {
            break;
        };
        if escaped == b'c' && !bytes.get(index + 2).is_some_and(u8::is_ascii_alphabetic) {
            return Err(CompileError::Syntax(
                "Unicode control escape requires an ASCII letter".to_owned(),
            ));
        }
        index = index.saturating_add(if escaped == b'c' { 3 } else { 2 });
    }
    Ok(())
}

/// Cooks the `RegExpIdentifierName` inside named captures and backreferences.
///
/// Oxc deliberately retains the source spelling in its AST. That spelling is
/// not the ECMAScript `StringValue`: `A`, `\u0041`, and `\u{41}` name the same
/// capture, and the UTF-16 constructor transport writes a supplementary name
/// as two surrogate escapes in non-Unicode mode. Rewriting only the grammar's
/// name positions before parsing gives Oxc the canonical identifier for its
/// duplicate validation and gives this compiler one key for capture metadata
/// and backreference lookup. The runtime still retains the original pattern
/// source separately for `RegExp.prototype.source`.
fn normalize_group_name_source(source: &str) -> Result<Cow<'_, str>, CompileError> {
    let bytes = source.as_bytes();
    let mut replacements = Vec::new();
    let mut in_class = false;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if !in_class && bytes.get(index + 1..index + 3) == Some(b"k<") {
                let name_start = index + 3;
                let Some(relative_end) = bytes[name_start..].iter().position(|byte| *byte == b'>')
                else {
                    break;
                };
                let name_end = name_start + relative_end;
                collect_group_name_replacement(source, name_start, name_end, &mut replacements)?;
                index = name_end + 1;
                continue;
            }
            index = index.saturating_add(2);
            continue;
        }

        if !in_class
            && bytes[index] == b'('
            && bytes.get(index + 1..index + 3) == Some(b"?<")
            && !matches!(bytes.get(index + 3), Some(b'=' | b'!'))
        {
            let name_start = index + 3;
            let Some(relative_end) = bytes[name_start..].iter().position(|byte| *byte == b'>')
            else {
                break;
            };
            let name_end = name_start + relative_end;
            collect_group_name_replacement(source, name_start, name_end, &mut replacements)?;
            index = name_end + 1;
            continue;
        }

        match bytes[index] {
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            _ => {}
        }
        index += 1;
    }

    if replacements.is_empty() {
        return Ok(Cow::Borrowed(source));
    }
    let mut normalized = String::new();
    normalized
        .try_reserve_exact(source.len())
        .map_err(|_| CompileError::ResourceLimit("capture name allocation"))?;
    let mut copied = 0;
    for (start, end, name) in replacements {
        normalized.push_str(&source[copied..start]);
        normalized.push_str(&name);
        copied = end;
    }
    normalized.push_str(&source[copied..]);
    debug_assert!(normalized.len() <= source.len());
    Ok(Cow::Owned(normalized))
}

fn collect_group_name_replacement(
    source: &str,
    start: usize,
    end: usize,
    replacements: &mut Vec<(usize, usize, String)>,
) -> Result<(), CompileError> {
    let raw = &source[start..end];
    let cooked = cook_group_name(raw)?;
    if cooked != raw {
        replacements
            .try_reserve(1)
            .map_err(|_| CompileError::ResourceLimit("capture name replacements"))?;
        replacements.push((start, end, cooked));
    }
    Ok(())
}

fn cook_group_name(raw: &str) -> Result<String, CompileError> {
    let syntax = || CompileError::Syntax("invalid named capture identifier".to_owned());
    let mut cooked = String::new();
    cooked
        .try_reserve_exact(raw.len())
        .map_err(|_| CompileError::ResourceLimit("capture name allocation"))?;
    let mut characters = raw.chars().peekable();
    let mut pending_high_surrogate = None;
    while let Some(character) = characters.next() {
        if character != '\\' {
            if pending_high_surrogate.is_some() {
                return Err(syntax());
            }
            cooked.push(character);
            continue;
        }
        if characters.next() != Some('u') {
            return Err(syntax());
        }
        let first = characters.next().ok_or_else(&syntax)?;
        if first == '{' {
            if pending_high_surrogate.is_some() {
                return Err(syntax());
            }
            let mut scalar = 0_u32;
            let mut digits = 0_u8;
            loop {
                let digit = characters.next().ok_or_else(&syntax)?;
                if digit == '}' {
                    break;
                }
                scalar = scalar
                    .checked_mul(16)
                    .and_then(|value| {
                        digit
                            .to_digit(16)
                            .and_then(|digit| value.checked_add(digit))
                    })
                    .ok_or_else(&syntax)?;
                digits = digits.checked_add(1).ok_or_else(&syntax)?;
            }
            if digits == 0 {
                return Err(syntax());
            }
            cooked.push(char::from_u32(scalar).ok_or_else(&syntax)?);
            continue;
        }

        let mut unit = first.to_digit(16).ok_or_else(&syntax)?;
        for _ in 0..3 {
            unit = unit
                .checked_mul(16)
                .and_then(|value| {
                    characters
                        .next()
                        .and_then(|digit| digit.to_digit(16))
                        .and_then(|digit| value.checked_add(digit))
                })
                .ok_or_else(&syntax)?;
        }
        push_group_name_code_unit(
            &mut cooked,
            &mut pending_high_surrogate,
            u16::try_from(unit).map_err(|_| syntax())?,
        )?;
    }
    if pending_high_surrogate.is_some() {
        return Err(syntax());
    }
    Ok(cooked)
}

fn push_group_name_code_unit(
    cooked: &mut String,
    pending_high_surrogate: &mut Option<u16>,
    unit: u16,
) -> Result<(), CompileError> {
    if let Some(high) = pending_high_surrogate.take() {
        if !(0xdc00..=0xdfff).contains(&unit) {
            return Err(CompileError::Syntax(
                "invalid named capture surrogate pair".to_owned(),
            ));
        }
        let scalar = 0x1_0000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00);
        cooked.push(char::from_u32(scalar).expect("a validated surrogate pair is a scalar"));
    } else if (0xd800..=0xdbff).contains(&unit) {
        *pending_high_surrogate = Some(unit);
    } else if (0xdc00..=0xdfff).contains(&unit) {
        return Err(CompileError::Syntax(
            "invalid named capture surrogate pair".to_owned(),
        ));
    } else {
        cooked.push(char::from_u32(u32::from(unit)).expect("a non-surrogate u16 is a scalar"));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct CaptureMetadata {
    by_start: HashMap<u32, usize>,
    spans: Vec<(u32, u32, usize)>,
    by_name: HashMap<String, Vec<usize>>,
    names: Vec<Option<String>>,
}

impl CaptureMetadata {
    fn new() -> Self {
        Self {
            by_start: HashMap::new(),
            spans: Vec::new(),
            by_name: HashMap::new(),
            names: Vec::new(),
        }
    }

    fn within(&self, start: u32, end: u32) -> (usize, usize) {
        let mut lower = usize::MAX;
        let mut upper = 0;
        for &(capture_start, capture_end, index) in &self.spans {
            if capture_start >= start && capture_end <= end {
                lower = lower.min(index);
                upper = upper.max(index + 1);
            }
        }
        if lower == usize::MAX {
            (0, 0)
        } else {
            (lower, upper)
        }
    }
}

struct Compiler {
    flags: RegExpFlags,
    limits: CompileLimits,
    captures: CaptureMetadata,
    assembly: Vec<Assembly>,
    instruction_count: usize,
    next_label: usize,
    repeat_count: usize,
}

impl Compiler {
    fn new(flags: RegExpFlags, limits: CompileLimits) -> Self {
        Self {
            flags,
            limits,
            captures: CaptureMetadata::new(),
            assembly: Vec::new(),
            instruction_count: 0,
            next_label: 0,
            repeat_count: 0,
        }
    }

    fn collect_captures(&mut self, pattern: &Pattern<'_>) -> Result<(), CompileError> {
        let mut stack = vec![Visit::Disjunction(&pattern.body, 0)];
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Disjunction(disjunction, depth) => {
                    if depth > self.limits.max_nesting {
                        return Err(CompileError::ResourceLimit("nesting"));
                    }
                    for alternative in disjunction.body.iter().rev() {
                        for term in alternative.body.iter().rev() {
                            stack.push(Visit::Term(term, depth));
                        }
                    }
                }
                Visit::Term(term, depth) => match term {
                    Term::CapturingGroup(group) => {
                        let index = self.captures.spans.len() + 1;
                        if index > self.limits.max_captures {
                            return Err(CompileError::ResourceLimit("capture count"));
                        }
                        self.captures.by_start.insert(group.span.start, index);
                        self.captures
                            .spans
                            .push((group.span.start, group.span.end, index));
                        let name = group.name.as_ref().map(ToString::to_string);
                        if let Some(name) = &name {
                            self.captures
                                .by_name
                                .entry(name.clone())
                                .or_default()
                                .push(index);
                        }
                        self.captures.names.push(name);
                        stack.push(Visit::Disjunction(&group.body, depth + 1));
                    }
                    Term::IgnoreGroup(group) => {
                        stack.push(Visit::Disjunction(&group.body, depth + 1));
                    }
                    Term::LookAroundAssertion(assertion) => {
                        stack.push(Visit::Disjunction(&assertion.body, depth + 1));
                    }
                    Term::Quantifier(quantifier) => {
                        stack.push(Visit::Term(&quantifier.body, depth + 1));
                    }
                    _ => {}
                },
            }
        }
        Ok(())
    }

    fn build<'pattern, 'allocator>(
        mut self,
        pattern: &'pattern Pattern<'allocator>,
    ) -> Result<Program, CompileError> {
        let mut actions: Vec<Action<'pattern, 'allocator>> = vec![
            Action::Emit(UnresolvedInstruction::Match),
            Action::Emit(UnresolvedInstruction::SaveEnd(0)),
            Action::Disjunction(&pattern.body, self.flags.initial_mode(), Direction::Forward),
            Action::Emit(UnresolvedInstruction::SaveStart(0)),
        ];
        while let Some(action) = actions.pop() {
            match action {
                Action::Disjunction(disjunction, mode, direction) => {
                    self.schedule_disjunction(&mut actions, disjunction, mode, direction);
                }
                Action::Alternative(alternative, mode, direction) => match direction {
                    Direction::Forward => {
                        for term in alternative.body.iter().rev() {
                            actions.push(Action::Term(term, mode, direction));
                        }
                    }
                    Direction::Backward => {
                        for term in &alternative.body {
                            actions.push(Action::Term(term, mode, direction));
                        }
                    }
                },
                Action::Term(term, mode, direction) => {
                    self.schedule_term(&mut actions, term, mode, direction)?;
                }
                Action::Emit(instruction) => self.emit(instruction)?,
                Action::Mark(label) => self.assembly.push(Assembly::Label(label)),
            }
        }
        let flags = self.flags;
        let capture_count = self.captures.spans.len() + 1;
        let mut capture_names = std::mem::take(&mut self.captures.names);
        capture_names.insert(0, None);
        let repeat_count = self.repeat_count;
        let instructions = self.resolve()?;
        Ok(Program {
            instructions,
            capture_count,
            capture_names,
            repeat_count,
            flags,
        })
    }

    fn schedule_disjunction<'pattern, 'allocator>(
        &mut self,
        actions: &mut Vec<Action<'pattern, 'allocator>>,
        disjunction: &'pattern Disjunction<'allocator>,
        mode: MatchMode,
        direction: Direction,
    ) {
        if disjunction.body.len() == 1 {
            actions.push(Action::Alternative(&disjunction.body[0], mode, direction));
            return;
        }

        let end = self.label();
        let mut sequence = Vec::new();
        for (index, alternative) in disjunction.body.iter().enumerate() {
            if index + 1 < disjunction.body.len() {
                let branch = self.label();
                let next = self.label();
                sequence.push(Action::Emit(UnresolvedInstruction::Split {
                    first: branch,
                    second: next,
                }));
                sequence.push(Action::Mark(branch));
                sequence.push(Action::Alternative(alternative, mode, direction));
                sequence.push(Action::Emit(UnresolvedInstruction::Jump(end)));
                sequence.push(Action::Mark(next));
            } else {
                sequence.push(Action::Alternative(alternative, mode, direction));
            }
        }
        sequence.push(Action::Mark(end));
        actions.extend(sequence.into_iter().rev());
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive AST-term lowering match keeps feature coverage auditable"
    )]
    fn schedule_term<'pattern, 'allocator>(
        &mut self,
        actions: &mut Vec<Action<'pattern, 'allocator>>,
        term: &'pattern Term<'allocator>,
        mode: MatchMode,
        direction: Direction,
    ) -> Result<(), CompileError> {
        match term {
            Term::BoundaryAssertion(assertion) => {
                let kind = match assertion.kind {
                    BoundaryAssertionKind::Start => BoundaryKind::Start,
                    BoundaryAssertionKind::End => BoundaryKind::End,
                    BoundaryAssertionKind::Boundary => BoundaryKind::Word,
                    BoundaryAssertionKind::NegativeBoundary => BoundaryKind::NotWord,
                };
                actions.push(Action::Emit(UnresolvedInstruction::Boundary { kind, mode }));
            }
            Term::LookAroundAssertion(assertion) => {
                let kind = match assertion.kind {
                    LookAroundAssertionKind::Lookahead => LookaroundKind::Ahead,
                    LookAroundAssertionKind::NegativeLookahead => LookaroundKind::NegativeAhead,
                    LookAroundAssertionKind::Lookbehind => LookaroundKind::Behind,
                    LookAroundAssertionKind::NegativeLookbehind => LookaroundKind::NegativeBehind,
                };
                let body = self.label();
                let exit = self.label();
                let sequence = [
                    Action::Emit(UnresolvedInstruction::Lookaround { kind, body, exit }),
                    Action::Mark(body),
                    Action::Disjunction(&assertion.body, mode, kind.direction()),
                    Action::Emit(UnresolvedInstruction::LookaroundAccept(kind)),
                    Action::Mark(exit),
                ];
                actions.extend(sequence.into_iter().rev());
            }
            Term::Quantifier(quantifier) => {
                let slot = self.repeat_count;
                self.repeat_count = self
                    .repeat_count
                    .checked_add(1)
                    .ok_or(CompileError::ResourceLimit("repeat count"))?;
                let head = self.label();
                let body = self.label();
                let exit = self.label();
                let (capture_start, capture_end) = self
                    .captures
                    .within(quantifier.body.span().start, quantifier.body.span().end);
                let sequence = [
                    Action::Mark(head),
                    Action::Emit(UnresolvedInstruction::Repeat {
                        slot,
                        min: quantifier.min,
                        max: quantifier.max,
                        greedy: quantifier.greedy,
                        body,
                        exit,
                        capture_start,
                        capture_end,
                    }),
                    Action::Mark(body),
                    Action::Term(&quantifier.body, mode, direction),
                    Action::Emit(UnresolvedInstruction::RepeatEnd { slot, head }),
                    Action::Mark(exit),
                ];
                actions.extend(sequence.into_iter().rev());
            }
            Term::Character(character) => {
                actions.push(Action::Emit(UnresolvedInstruction::Character {
                    value: character.value,
                    mode,
                }));
            }
            Term::Dot(_) => actions.push(Action::Emit(UnresolvedInstruction::Dot(mode))),
            Term::CharacterClassEscape(escape) => {
                actions.push(Action::Emit(UnresolvedInstruction::CharacterClass {
                    class: CharacterClass {
                        negative: false,
                        kind: CharacterClassKind::Union,
                        items: vec![class_escape(escape.kind)],
                        strings: Vec::new(),
                        has_string_properties: false,
                    },
                    mode,
                }));
            }
            Term::UnicodePropertyEscape(property) => {
                actions.push(Action::Emit(UnresolvedInstruction::CharacterClass {
                    class: CharacterClass {
                        negative: false,
                        kind: CharacterClassKind::Union,
                        items: vec![CharacterClassItem::UnicodeProperty(lower_unicode_property(
                            property,
                        )?)],
                        strings: Vec::new(),
                        has_string_properties: property.strings,
                    },
                    mode,
                }));
            }
            Term::CharacterClass(class) => {
                actions.push(Action::Emit(UnresolvedInstruction::CharacterClass {
                    class: lower_character_class(class, 0, self.limits.max_nesting)?,
                    mode,
                }));
            }
            Term::CapturingGroup(group) => {
                let index = *self
                    .captures
                    .by_start
                    .get(&group.span.start)
                    .ok_or(CompileError::ResourceLimit("capture metadata"))?;
                let sequence = match direction {
                    Direction::Forward => [
                        Action::Emit(UnresolvedInstruction::SaveStart(index)),
                        Action::Disjunction(&group.body, mode, direction),
                        Action::Emit(UnresolvedInstruction::SaveEnd(index)),
                    ],
                    Direction::Backward => [
                        Action::Emit(UnresolvedInstruction::SaveEnd(index)),
                        Action::Disjunction(&group.body, mode, direction),
                        Action::Emit(UnresolvedInstruction::SaveStart(index)),
                    ],
                };
                actions.extend(sequence.into_iter().rev());
            }
            Term::IgnoreGroup(group) => {
                let mut nested_mode = mode;
                if let Some(modifiers) = &group.modifiers {
                    apply_modifier(
                        &mut nested_mode.ignore_case,
                        modifiers.enabling,
                        modifiers.disabling,
                        Modifier::I,
                    );
                    apply_modifier(
                        &mut nested_mode.multiline,
                        modifiers.enabling,
                        modifiers.disabling,
                        Modifier::M,
                    );
                    apply_modifier(
                        &mut nested_mode.dot_all,
                        modifiers.enabling,
                        modifiers.disabling,
                        Modifier::S,
                    );
                }
                actions.push(Action::Disjunction(&group.body, nested_mode, direction));
            }
            Term::IndexedReference(reference) => {
                actions.push(Action::Emit(UnresolvedInstruction::BackReference {
                    captures: vec![reference.index as usize],
                    mode,
                }));
            }
            Term::NamedReference(reference) => {
                let captures = self
                    .captures
                    .by_name
                    .get(reference.name.as_str())
                    .cloned()
                    .ok_or(CompileError::Syntax("unknown named capture".to_owned()))?;
                actions.push(Action::Emit(UnresolvedInstruction::BackReference {
                    captures,
                    mode,
                }));
            }
        }
        Ok(())
    }

    fn label(&mut self) -> usize {
        let label = self.next_label;
        self.next_label += 1;
        label
    }

    fn emit(&mut self, instruction: UnresolvedInstruction) -> Result<(), CompileError> {
        self.instruction_count += 1;
        if self.instruction_count > self.limits.max_instructions {
            return Err(CompileError::ResourceLimit("instruction count"));
        }
        self.assembly.push(Assembly::Instruction(instruction));
        Ok(())
    }

    fn resolve(self) -> Result<Vec<Instruction>, CompileError> {
        let mut labels = vec![None; self.next_label];
        let mut pc = 0;
        for entry in &self.assembly {
            match entry {
                Assembly::Label(label) => labels[*label] = Some(pc),
                Assembly::Instruction(_) => pc += 1,
            }
        }
        let target = |label: usize| {
            labels[label].ok_or(CompileError::ResourceLimit("unresolved instruction label"))
        };
        let mut instructions = Vec::with_capacity(self.instruction_count);
        for entry in self.assembly {
            let Assembly::Instruction(instruction) = entry else {
                continue;
            };
            instructions.push(match instruction {
                UnresolvedInstruction::Character { value, mode } => {
                    Instruction::Character { value, mode }
                }
                UnresolvedInstruction::Dot(mode) => Instruction::Dot(mode),
                UnresolvedInstruction::CharacterClass { class, mode } => {
                    Instruction::CharacterClass { class, mode }
                }
                UnresolvedInstruction::Boundary { kind, mode } => {
                    Instruction::Boundary { kind, mode }
                }
                UnresolvedInstruction::SaveStart(index) => Instruction::SaveStart(index),
                UnresolvedInstruction::SaveEnd(index) => Instruction::SaveEnd(index),
                UnresolvedInstruction::BackReference { captures, mode } => {
                    Instruction::BackReference { captures, mode }
                }
                UnresolvedInstruction::Lookaround { kind, body, exit } => Instruction::Lookaround {
                    kind,
                    body: target(body)?,
                    exit: target(exit)?,
                },
                UnresolvedInstruction::LookaroundAccept(kind) => {
                    Instruction::LookaroundAccept(kind)
                }
                UnresolvedInstruction::Split { first, second } => Instruction::Split {
                    first: target(first)?,
                    second: target(second)?,
                },
                UnresolvedInstruction::Jump(label) => Instruction::Jump(target(label)?),
                UnresolvedInstruction::Repeat {
                    slot,
                    min,
                    max,
                    greedy,
                    body,
                    exit,
                    capture_start,
                    capture_end,
                } => Instruction::Repeat {
                    slot,
                    min,
                    max,
                    greedy,
                    body: target(body)?,
                    exit: target(exit)?,
                    capture_start,
                    capture_end,
                },
                UnresolvedInstruction::RepeatEnd { slot, head } => Instruction::RepeatEnd {
                    slot,
                    head: target(head)?,
                },
                UnresolvedInstruction::Match => Instruction::Match,
            });
        }
        Ok(instructions)
    }
}

fn apply_modifier(value: &mut bool, enabling: Modifier, disabling: Modifier, flag: Modifier) {
    if enabling.contains(flag) {
        *value = true;
    }
    if disabling.contains(flag) {
        *value = false;
    }
}

fn class_escape(kind: CharacterClassEscapeKind) -> CharacterClassItem {
    match kind {
        CharacterClassEscapeKind::D => CharacterClassItem::Digit(false),
        CharacterClassEscapeKind::NegativeD => CharacterClassItem::Digit(true),
        CharacterClassEscapeKind::S => CharacterClassItem::Space(false),
        CharacterClassEscapeKind::NegativeS => CharacterClassItem::Space(true),
        CharacterClassEscapeKind::W => CharacterClassItem::Word(false),
        CharacterClassEscapeKind::NegativeW => CharacterClassItem::Word(true),
    }
}

fn lower_character_class(
    class: &ast::CharacterClass<'_>,
    depth: usize,
    max_depth: usize,
) -> Result<CharacterClass, CompileError> {
    if depth > max_depth {
        return Err(CompileError::ResourceLimit("nesting"));
    }
    let mut items = Vec::with_capacity(class.body.len());
    let mut strings = Vec::new();
    let mut has_string_properties = false;
    for content in &class.body {
        match content {
            CharacterClassContents::CharacterClassRange(range) => {
                items.push(CharacterClassItem::Range(range.min.value, range.max.value));
            }
            CharacterClassContents::CharacterClassEscape(escape) => {
                items.push(class_escape(escape.kind));
            }
            CharacterClassContents::Character(character) => {
                items.push(CharacterClassItem::Range(character.value, character.value));
            }
            CharacterClassContents::UnicodePropertyEscape(property) => {
                let property = lower_unicode_property(property)?;
                has_string_properties |= property.strings;
                items.push(CharacterClassItem::UnicodeProperty(property));
            }
            CharacterClassContents::NestedCharacterClass(nested) => {
                let nested = lower_character_class(nested, depth + 1, max_depth)?;
                strings.extend(nested.strings.iter().cloned());
                has_string_properties |= nested.has_string_properties;
                items.push(CharacterClassItem::Nested(Box::new(nested)));
            }
            CharacterClassContents::ClassStringDisjunction(disjunction) => {
                for string in &disjunction.body {
                    let value = string
                        .body
                        .iter()
                        .map(|character| character.value)
                        .collect::<Vec<_>>();
                    strings.push(value.clone());
                    items.push(CharacterClassItem::String(value));
                }
            }
        }
    }
    strings.sort_by_key(|string| std::cmp::Reverse(string.len()));
    strings.dedup();
    Ok(CharacterClass {
        negative: class.negative,
        kind: match class.kind {
            CharacterClassContentsKind::Union => CharacterClassKind::Union,
            CharacterClassContentsKind::Intersection => CharacterClassKind::Intersection,
            CharacterClassContentsKind::Subtraction => CharacterClassKind::Subtraction,
        },
        items,
        strings,
        has_string_properties,
    })
}

fn lower_unicode_property(
    property: &ast::UnicodePropertyEscape<'_>,
) -> Result<UnicodeProperty, CompileError> {
    if property.strings && !emoji::supports(property.name.as_str()) {
        return Err(CompileError::Unsupported(
            "RGI emoji ZWJ properties of strings",
        ));
    }
    Ok(UnicodeProperty::compile(
        property.negative,
        property.strings,
        property.name.as_str(),
        property.value.as_deref(),
    ))
}

enum Visit<'pattern, 'allocator> {
    Disjunction(&'pattern Disjunction<'allocator>, usize),
    Term(&'pattern Term<'allocator>, usize),
}

enum Action<'pattern, 'allocator> {
    Disjunction(&'pattern Disjunction<'allocator>, MatchMode, Direction),
    Alternative(&'pattern Alternative<'allocator>, MatchMode, Direction),
    Term(&'pattern Term<'allocator>, MatchMode, Direction),
    Emit(UnresolvedInstruction),
    Mark(usize),
}

enum Assembly {
    Label(usize),
    Instruction(UnresolvedInstruction),
}

enum UnresolvedInstruction {
    Character {
        value: u32,
        mode: MatchMode,
    },
    Dot(MatchMode),
    CharacterClass {
        class: CharacterClass,
        mode: MatchMode,
    },
    Boundary {
        kind: BoundaryKind,
        mode: MatchMode,
    },
    SaveStart(usize),
    SaveEnd(usize),
    BackReference {
        captures: Vec<usize>,
        mode: MatchMode,
    },
    Lookaround {
        kind: LookaroundKind,
        body: usize,
        exit: usize,
    },
    LookaroundAccept(LookaroundKind),
    Split {
        first: usize,
        second: usize,
    },
    Jump(usize),
    Repeat {
        slot: usize,
        min: u64,
        max: Option<u64>,
        greedy: bool,
        body: usize,
        exit: usize,
        capture_start: usize,
        capture_end: usize,
    },
    RepeatEnd {
        slot: usize,
        head: usize,
    },
    Match,
}
