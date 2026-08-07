use crate::flags::{MatchMode, RegExpFlags};

#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub instructions: Vec<Instruction>,
    pub capture_count: usize,
    pub capture_names: Vec<Option<String>>,
    pub repeat_count: usize,
    pub flags: RegExpFlags,
}

#[derive(Clone, Debug)]
pub(crate) enum Instruction {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoundaryKind {
    Start,
    End,
    Word,
    NotWord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LookaroundKind {
    Ahead,
    NegativeAhead,
    Behind,
    NegativeBehind,
}

impl LookaroundKind {
    pub const fn negative(self) -> bool {
        matches!(self, Self::NegativeAhead | Self::NegativeBehind)
    }

    pub const fn direction(self) -> Direction {
        match self {
            Self::Ahead | Self::NegativeAhead => Direction::Forward,
            Self::Behind | Self::NegativeBehind => Direction::Backward,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Forward,
    Backward,
}

#[derive(Clone, Debug)]
pub(crate) struct CharacterClass {
    pub negative: bool,
    pub kind: CharacterClassKind,
    pub items: Vec<CharacterClassItem>,
    pub strings: Vec<Vec<u32>>,
    pub has_string_properties: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CharacterClassKind {
    Union,
    Intersection,
    Subtraction,
}

#[derive(Clone, Debug)]
pub(crate) enum CharacterClassItem {
    Range(u32, u32),
    Digit(bool),
    Space(bool),
    Word(bool),
    UnicodeProperty(UnicodeProperty),
    Nested(Box<CharacterClass>),
    String(Vec<u32>),
}

#[derive(Clone, Debug)]
pub(crate) struct UnicodeProperty {
    pub negative: bool,
    pub strings: bool,
    pub name: String,
    pub value: Option<String>,
}
