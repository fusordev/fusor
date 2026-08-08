use std::ops::Range;

use icu::casemap::CaseMapperBorrowed;

use crate::{
    ExecError, ExecLimits, Match,
    flags::MatchMode,
    program::{
        BoundaryKind, CharacterClass, CharacterClassItem, CharacterClassKind, Direction,
        Instruction, LookaroundKind, Program,
    },
    properties,
};

pub(crate) fn execute(
    program: &Program,
    input: &[u16],
    start_index: usize,
    limits: ExecLimits,
    steps: &mut u64,
) -> Result<Option<Match>, ExecError> {
    if start_index > input.len() {
        return Ok(None);
    }
    let mut candidate = start_index;
    loop {
        consume_step(steps, limits.max_steps)?;
        if let Some(result) = run_candidate(program, input, candidate, limits, steps)? {
            return Ok(Some(result));
        }
        if program.flags.sticky || candidate == input.len() {
            return Ok(None);
        }
        candidate = advance_index(input, candidate, program.flags.unicode_mode());
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive instruction dispatcher keeps backtracking state transitions auditable"
)]
fn run_candidate(
    program: &Program,
    input: &[u16],
    candidate: usize,
    limits: ExecLimits,
    steps: &mut u64,
) -> Result<Option<Match>, ExecError> {
    let mut state = State {
        pc: 0,
        position: candidate,
        captures: vec![CaptureSlot::default(); program.capture_count],
        repeats: vec![RepeatState::default(); program.repeat_count],
        terminal_repeat: None,
        direction: Direction::Forward,
    };
    let mut backtrack = Vec::new();

    loop {
        consume_step(steps, limits.max_steps)?;
        let Some(instruction) = program.instructions.get(state.pc) else {
            return Ok(None);
        };
        match instruction {
            Instruction::Character { value, mode } => {
                let Some((actual, next)) =
                    decode_in_direction(input, state.position, mode.unicode, state.direction)
                else {
                    if !recover_or_restore_terminal_repeat(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                    continue;
                };
                if canonicalize(actual, *mode) != canonicalize(*value, *mode) {
                    if !recover_or_restore_terminal_repeat(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                    continue;
                }
                state.position = next;
                state.pc += 1;
            }
            Instruction::Dot(mode) => {
                let Some((actual, next)) =
                    decode_in_direction(input, state.position, mode.unicode, state.direction)
                else {
                    if !recover_or_restore_terminal_repeat(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                    continue;
                };
                if !mode.dot_all && is_line_terminator(actual) {
                    if !recover_or_restore_terminal_repeat(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                    continue;
                }
                state.position = next;
                state.pc += 1;
            }
            Instruction::CharacterClass { class, mode } => {
                let positions =
                    class_match_positions(class, input, state.position, *mode, state.direction);
                let Some((&next, alternatives)) = positions.split_first() else {
                    if !recover_or_restore_terminal_repeat(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                    continue;
                };
                let next_pc = state.pc + 1;
                for &alternative_position in alternatives.iter().rev() {
                    push_alternative(
                        &mut backtrack,
                        State {
                            pc: next_pc,
                            position: alternative_position,
                            ..state.clone()
                        },
                        limits.max_backtrack_states,
                    )?;
                }
                state.position = next;
                state.pc = next_pc;
            }
            Instruction::Boundary { kind, mode } => {
                if !boundary_matches(*kind, input, state.position, *mode) {
                    if !restore(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                    continue;
                }
                state.pc += 1;
            }
            Instruction::SaveStart(index) => {
                let capture = &mut state.captures[*index];
                capture.start = Some(state.position);
                if state.direction == Direction::Forward {
                    capture.end = None;
                }
                state.pc += 1;
            }
            Instruction::SaveEnd(index) => {
                let capture = &mut state.captures[*index];
                if state.direction == Direction::Backward {
                    capture.start = None;
                } else {
                    capture.start.get_or_insert(state.position);
                }
                capture.end = Some(state.position);
                state.pc += 1;
            }
            Instruction::BackReference { captures, mode } => {
                let capture = captures.iter().find_map(|index| {
                    state
                        .captures
                        .get(*index)
                        .copied()
                        .and_then(CaptureSlot::range)
                });
                let Some(range) = capture else {
                    state.pc += 1;
                    continue;
                };
                let Some(next) =
                    match_back_reference(input, state.position, range, *mode, state.direction)
                else {
                    if !restore(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                    continue;
                };
                state.position = next;
                state.pc += 1;
            }
            Instruction::Split { first, second } => {
                push_alternative(
                    &mut backtrack,
                    State {
                        pc: *second,
                        ..state.clone()
                    },
                    limits.max_backtrack_states,
                )?;
                state.pc = *first;
            }
            Instruction::Lookaround { kind, body, exit } => {
                let mut outer = state.clone();
                outer.pc = *exit;
                push_lookaround(&mut backtrack, outer, *kind, limits.max_backtrack_states)?;
                state.pc = *body;
                state.direction = kind.direction();
            }
            Instruction::LookaroundAccept(kind) => {
                let Some(outer) = pop_lookaround(&mut backtrack) else {
                    return Ok(None);
                };
                if kind.negative() {
                    if !restore(&mut state, &mut backtrack) {
                        return Ok(None);
                    }
                } else {
                    let captures = state.captures;
                    state = outer;
                    state.captures = captures;
                }
            }
            Instruction::Jump(target) => state.pc = *target,
            Instruction::Repeat {
                slot,
                min,
                max,
                greedy,
                body,
                exit,
                capture_start,
                capture_end,
            } => {
                if !state.repeats[*slot].active {
                    state.repeats[*slot] = RepeatState {
                        active: true,
                        count: 0,
                        iteration_start: None,
                        optional_attempted: false,
                    };
                }
                let repeated = state.repeats[*slot];
                let made_no_progress =
                    repeated.count > 0 && repeated.iteration_start == Some(state.position);
                let reached_max = max.is_some_and(|maximum| repeated.count >= maximum);
                if reached_max {
                    state.repeats[*slot].active = false;
                    clear_terminal_repeat(&mut state, *slot);
                    state.pc = *exit;
                    continue;
                }
                if repeated.count >= *min && made_no_progress {
                    if repeated.optional_attempted {
                        if !restore(&mut state, &mut backtrack) {
                            return Ok(None);
                        }
                    } else {
                        state.repeats[*slot].active = false;
                        clear_terminal_repeat(&mut state, *slot);
                        state.pc = *exit;
                    }
                    continue;
                }
                if repeated.count < *min {
                    start_iteration(&mut state, *slot, *body, *capture_start, *capture_end);
                    continue;
                }

                let mut body_state = state.clone();
                body_state.repeats[*slot].optional_attempted = true;
                start_iteration(&mut body_state, *slot, *body, *capture_start, *capture_end);
                let mut exit_state = state.clone();
                exit_state.repeats[*slot].active = false;
                exit_state.pc = *exit;
                if *greedy {
                    if let Some(terminal) =
                        terminal_repeat(program, *slot, *body, *exit, *capture_start, *capture_end)
                    {
                        body_state.terminal_repeat = Some(terminal);
                    } else {
                        push_alternative(&mut backtrack, exit_state, limits.max_backtrack_states)?;
                    }
                    state = body_state;
                } else {
                    push_alternative(&mut backtrack, body_state, limits.max_backtrack_states)?;
                    state = exit_state;
                }
            }
            Instruction::RepeatEnd { slot, head } => {
                state.repeats[*slot].count = state.repeats[*slot].count.saturating_add(1);
                state.pc = *head;
            }
            Instruction::Match => {
                let captures = state.captures.into_iter().map(CaptureSlot::range).collect();
                return Ok(Some(Match { captures }));
            }
        }
    }
}

fn consume_step(steps: &mut u64, limit: u64) -> Result<(), ExecError> {
    if *steps >= limit {
        return Err(ExecError::StepLimit);
    }
    *steps += 1;
    Ok(())
}

fn push_alternative(
    backtrack: &mut Vec<BacktrackEntry>,
    state: State,
    limit: usize,
) -> Result<(), ExecError> {
    if backtrack.len() >= limit {
        return Err(ExecError::BacktrackLimit);
    }
    backtrack.push(BacktrackEntry::Alternative(state));
    Ok(())
}

fn push_lookaround(
    backtrack: &mut Vec<BacktrackEntry>,
    outer: State,
    kind: LookaroundKind,
    limit: usize,
) -> Result<(), ExecError> {
    if backtrack.len() >= limit {
        return Err(ExecError::BacktrackLimit);
    }
    backtrack.push(BacktrackEntry::Lookaround { outer, kind });
    Ok(())
}

fn restore(state: &mut State, backtrack: &mut Vec<BacktrackEntry>) -> bool {
    while let Some(entry) = backtrack.pop() {
        match entry {
            BacktrackEntry::Alternative(alternative) => {
                *state = alternative;
                return true;
            }
            BacktrackEntry::Lookaround { outer, kind } => {
                if kind.negative() {
                    *state = outer;
                    return true;
                }
            }
        }
    }
    false
}

/// Recovers from a failed token in a committed terminal repeat, or restores a
/// regular backtracking alternative.
fn recover_or_restore_terminal_repeat(
    state: &mut State,
    backtrack: &mut Vec<BacktrackEntry>,
) -> bool {
    if let Some(terminal) = state.terminal_repeat
        && state.pc == terminal.body
        && let Some(repeat) = state.repeats.get_mut(terminal.slot)
    {
        repeat.active = false;
        state.terminal_repeat = None;
        state.pc = terminal.exit;
        return true;
    }
    restore(state, backtrack)
}

/// Clears a terminal-repeat commitment when the repeat exits normally.
fn clear_terminal_repeat(state: &mut State, slot: usize) {
    if state
        .terminal_repeat
        .is_some_and(|terminal| terminal.slot == slot)
    {
        state.terminal_repeat = None;
    }
}

/// Recognizes a greedy, single-token repeat whose only continuation is a
/// non-multiline end anchor followed by the implicit whole-match save and
/// match. No shorter repetition can satisfy that continuation after the token
/// first fails, so per-iteration exit alternatives are unnecessary.
fn terminal_repeat(
    program: &Program,
    slot: usize,
    body: usize,
    exit: usize,
    capture_start: usize,
    capture_end: usize,
) -> Option<TerminalRepeat> {
    if capture_start != capture_end
        || !matches!(
            program.instructions.get(body),
            Some(
                Instruction::Character { .. }
                    | Instruction::Dot(_)
                    | Instruction::CharacterClass { .. }
            )
        )
        || !matches!(
            program.instructions.get(body.saturating_add(1)),
            Some(Instruction::RepeatEnd { .. })
        )
    {
        return None;
    }
    let Instruction::Boundary {
        kind: BoundaryKind::End,
        mode,
    } = program.instructions.get(exit)?
    else {
        return None;
    };
    if mode.multiline
        || !matches!(
            program.instructions.get(exit.saturating_add(1)),
            Some(Instruction::SaveEnd(0))
        )
        || !matches!(
            program.instructions.get(exit.saturating_add(2)),
            Some(Instruction::Match)
        )
    {
        return None;
    }
    Some(TerminalRepeat { slot, body, exit })
}

fn pop_lookaround(backtrack: &mut Vec<BacktrackEntry>) -> Option<State> {
    while let Some(entry) = backtrack.pop() {
        if let BacktrackEntry::Lookaround { outer, .. } = entry {
            return Some(outer);
        }
    }
    None
}

fn start_iteration(
    state: &mut State,
    slot: usize,
    body: usize,
    capture_start: usize,
    capture_end: usize,
) {
    if capture_start < capture_end {
        state.captures[capture_start..capture_end].fill(CaptureSlot::default());
    }
    state.repeats[slot].iteration_start = Some(state.position);
    state.pc = body;
}

fn match_back_reference(
    input: &[u16],
    mut position: usize,
    range: Range<usize>,
    mode: MatchMode,
    direction: Direction,
) -> Option<usize> {
    if direction == Direction::Backward {
        let mut capture_position = range.end;
        while capture_position > range.start {
            let (expected, next_capture) = decode_before(input, capture_position, mode.unicode)?;
            if next_capture < range.start {
                return None;
            }
            let (actual, next_position) = decode_before(input, position, mode.unicode)?;
            if canonicalize(expected, mode) != canonicalize(actual, mode) {
                return None;
            }
            capture_position = next_capture;
            position = next_position;
        }
        return Some(position);
    }
    let mut capture_position = range.start;
    while capture_position < range.end {
        let (expected, next_capture) = decode_at(input, capture_position, mode.unicode)?;
        if next_capture > range.end {
            return None;
        }
        let (actual, next_position) = decode_at(input, position, mode.unicode)?;
        if canonicalize(expected, mode) != canonicalize(actual, mode) {
            return None;
        }
        capture_position = next_capture;
        position = next_position;
    }
    Some(position)
}

fn boundary_matches(kind: BoundaryKind, input: &[u16], position: usize, mode: MatchMode) -> bool {
    match kind {
        BoundaryKind::Start => {
            position == 0
                || (mode.multiline
                    && decode_before(input, position, mode.unicode)
                        .is_some_and(|(value, _)| is_line_terminator(value)))
        }
        BoundaryKind::End => {
            position == input.len()
                || (mode.multiline
                    && decode_at(input, position, mode.unicode)
                        .is_some_and(|(value, _)| is_line_terminator(value)))
        }
        BoundaryKind::Word | BoundaryKind::NotWord => {
            let previous = decode_before(input, position, mode.unicode)
                .is_some_and(|(value, _)| is_word(value, mode));
            let next = decode_at(input, position, mode.unicode)
                .is_some_and(|(value, _)| is_word(value, mode));
            let boundary = previous != next;
            if kind == BoundaryKind::Word {
                boundary
            } else {
                !boundary
            }
        }
    }
}

fn class_matches(class: &CharacterClass, value: u32, mode: MatchMode) -> bool {
    class_contains_sequence(class, &[value], mode)
}

fn class_match_positions(
    class: &CharacterClass,
    input: &[u16],
    position: usize,
    mode: MatchMode,
    direction: Direction,
) -> Vec<usize> {
    let mut positions = Vec::new();
    for string in &class.strings {
        if string.len() == 1 || !class_contains_sequence(class, string, mode) {
            continue;
        }
        if let Some(next) = match_class_string(input, position, string, mode, direction)
            && !positions.contains(&next)
        {
            positions.push(next);
        }
    }
    if class.has_string_properties {
        let dynamic = directional_prefixes(input, position, mode.unicode, direction, 16);
        for (string, next) in dynamic.iter().rev() {
            if string.len() > 1
                && class_contains_sequence(class, string, mode)
                && !positions.contains(next)
            {
                positions.push(*next);
            }
        }
    }
    if let Some((value, next)) = decode_in_direction(input, position, mode.unicode, direction)
        && class_matches(class, value, mode)
        && !positions.contains(&next)
    {
        positions.push(next);
    }
    positions
}

fn class_contains_sequence(class: &CharacterClass, value: &[u32], mode: MatchMode) -> bool {
    let matched = match class.kind {
        CharacterClassKind::Union => class
            .items
            .iter()
            .any(|item| class_item_matches(item, value, mode)),
        CharacterClassKind::Intersection => class
            .items
            .iter()
            .all(|item| class_item_matches(item, value, mode)),
        CharacterClassKind::Subtraction => {
            class.items.split_first().is_some_and(|(first, rest)| {
                class_item_matches(first, value, mode)
                    && !rest
                        .iter()
                        .any(|item| class_item_matches(item, value, mode))
            })
        }
    };
    matched != class.negative
}

fn class_item_matches(item: &CharacterClassItem, value: &[u32], mode: MatchMode) -> bool {
    match item {
        CharacterClassItem::Range(start, end) => {
            let [value] = value else {
                return false;
            };
            let canonical = canonicalize(*value, mode);
            let start = canonicalize(*start, mode);
            let end = canonicalize(*end, mode);
            canonical >= start.min(end) && canonical <= start.max(end)
        }
        CharacterClassItem::Digit(negative) => {
            let [value] = value else {
                return false;
            };
            is_digit(*value) != *negative
        }
        CharacterClassItem::Space(negative) => {
            let [value] = value else {
                return false;
            };
            is_space(*value) != *negative
        }
        CharacterClassItem::Word(negative) => {
            let [value] = value else {
                return false;
            };
            is_word(*value, mode) != *negative
        }
        CharacterClassItem::UnicodeProperty(property) => {
            properties::matches_sequence(property, value, mode)
        }
        CharacterClassItem::Nested(class) => class_contains_sequence(class, value, mode),
        CharacterClassItem::String(expected) => sequences_equal(expected, value, mode),
    }
}

fn directional_prefixes(
    input: &[u16],
    position: usize,
    unicode: bool,
    direction: Direction,
    maximum: usize,
) -> Vec<(Vec<u32>, usize)> {
    let mut prefixes = Vec::new();
    let mut value = Vec::new();
    let mut cursor = position;
    for _ in 0..maximum {
        let Some((code_point, next)) = decode_in_direction(input, cursor, unicode, direction)
        else {
            break;
        };
        match direction {
            Direction::Forward => value.push(code_point),
            Direction::Backward => value.insert(0, code_point),
        }
        cursor = next;
        prefixes.push((value.clone(), cursor));
    }
    prefixes
}

fn sequences_equal(left: &[u32], right: &[u32], mode: MatchMode) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(&left, &right)| canonicalize(left, mode) == canonicalize(right, mode))
}

fn match_class_string(
    input: &[u16],
    mut position: usize,
    value: &[u32],
    mode: MatchMode,
    direction: Direction,
) -> Option<usize> {
    match direction {
        Direction::Forward => {
            for &expected in value {
                let (actual, next) = decode_at(input, position, mode.unicode)?;
                if canonicalize(expected, mode) != canonicalize(actual, mode) {
                    return None;
                }
                position = next;
            }
        }
        Direction::Backward => {
            for &expected in value.iter().rev() {
                let (actual, next) = decode_before(input, position, mode.unicode)?;
                if canonicalize(expected, mode) != canonicalize(actual, mode) {
                    return None;
                }
                position = next;
            }
        }
    }
    Some(position)
}

fn canonicalize(value: u32, mode: MatchMode) -> u32 {
    if !mode.ignore_case {
        return value;
    }
    let Some(character) = char::from_u32(value) else {
        return value;
    };
    let mapper = CaseMapperBorrowed::new();
    if mode.unicode {
        u32::from(mapper.simple_fold(character))
    } else {
        let upper = u32::from(mapper.simple_uppercase(character));
        if value >= 0x80 && upper < 0x80 {
            value
        } else {
            upper
        }
    }
}

fn is_word(value: u32, mode: MatchMode) -> bool {
    let canonical = canonicalize(value, mode);
    is_digit(canonical) || matches!(canonical, 0x41..=0x5a | 0x5f | 0x61..=0x7a)
}

const fn is_digit(value: u32) -> bool {
    matches!(value, 0x30..=0x39)
}

const fn is_space(value: u32) -> bool {
    matches!(
        value,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

const fn is_line_terminator(value: u32) -> bool {
    matches!(value, 0x000a | 0x000d | 0x2028 | 0x2029)
}

fn advance_index(input: &[u16], position: usize, unicode: bool) -> usize {
    decode_at(input, position, unicode).map_or(input.len(), |(_, next)| next)
}

fn decode_at(input: &[u16], position: usize, unicode: bool) -> Option<(u32, usize)> {
    let first = *input.get(position)?;
    if unicode
        && (0xd800..=0xdbff).contains(&first)
        && let Some(&second) = input.get(position + 1)
        && (0xdc00..=0xdfff).contains(&second)
    {
        let high = u32::from(first) - 0xd800;
        let low = u32::from(second) - 0xdc00;
        return Some((0x1_0000 + (high << 10) + low, position + 2));
    }
    Some((u32::from(first), position + 1))
}

fn decode_before(input: &[u16], position: usize, unicode: bool) -> Option<(u32, usize)> {
    let last_position = position.checked_sub(1)?;
    let last = input[last_position];
    if unicode && (0xdc00..=0xdfff).contains(&last) && last_position > 0 {
        let first = input[last_position - 1];
        if (0xd800..=0xdbff).contains(&first) {
            let high = u32::from(first) - 0xd800;
            let low = u32::from(last) - 0xdc00;
            return Some((0x1_0000 + (high << 10) + low, last_position - 1));
        }
    }
    Some((u32::from(last), last_position))
}

fn decode_in_direction(
    input: &[u16],
    position: usize,
    unicode: bool,
    direction: Direction,
) -> Option<(u32, usize)> {
    match direction {
        Direction::Forward => decode_at(input, position, unicode),
        Direction::Backward => decode_before(input, position, unicode),
    }
}

#[derive(Clone, Debug)]
struct State {
    pc: usize,
    position: usize,
    captures: Vec<CaptureSlot>,
    repeats: Vec<RepeatState>,
    terminal_repeat: Option<TerminalRepeat>,
    direction: Direction,
}

/// A greedy repeat whose only continuation is an ordinary end anchor.
///
/// Such a repeat can discard its per-iteration exit alternatives: if its
/// one-token body stops matching, the only viable continuation is its final
/// end-anchor path. Retaining that state here avoids linear backtracking
/// storage for Test262's full-Unicode property tests.
#[derive(Clone, Copy, Debug)]
struct TerminalRepeat {
    slot: usize,
    body: usize,
    exit: usize,
}

#[derive(Clone, Debug)]
enum BacktrackEntry {
    Alternative(State),
    Lookaround { outer: State, kind: LookaroundKind },
}

#[derive(Clone, Copy, Debug, Default)]
struct CaptureSlot {
    start: Option<usize>,
    end: Option<usize>,
}

impl CaptureSlot {
    fn range(self) -> Option<Range<usize>> {
        Some(self.start?..self.end?)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RepeatState {
    active: bool,
    count: u64,
    iteration_start: Option<usize>,
    optional_attempted: bool,
}
