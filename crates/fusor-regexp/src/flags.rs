use crate::CompileError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "ECMAScript exposes eight independent RegExp flag bits"
)]
pub(crate) struct RegExpFlags {
    pub has_indices: bool,
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub unicode_sets: bool,
    pub sticky: bool,
}

impl RegExpFlags {
    pub fn parse(source: &str) -> Result<Self, CompileError> {
        let mut flags = Self::default();
        for flag in source.chars() {
            let destination = match flag {
                'd' => &mut flags.has_indices,
                'g' => &mut flags.global,
                'i' => &mut flags.ignore_case,
                'm' => &mut flags.multiline,
                's' => &mut flags.dot_all,
                'u' => &mut flags.unicode,
                'v' => &mut flags.unicode_sets,
                'y' => &mut flags.sticky,
                _ => return Err(CompileError::InvalidFlags),
            };
            if *destination {
                return Err(CompileError::InvalidFlags);
            }
            *destination = true;
        }
        if flags.unicode && flags.unicode_sets {
            return Err(CompileError::InvalidFlags);
        }
        Ok(flags)
    }

    pub fn canonical_source(self) -> String {
        let mut source = String::with_capacity(8);
        for (enabled, flag) in [
            (self.has_indices, 'd'),
            (self.global, 'g'),
            (self.ignore_case, 'i'),
            (self.multiline, 'm'),
            (self.dot_all, 's'),
            (self.unicode, 'u'),
            (self.unicode_sets, 'v'),
            (self.sticky, 'y'),
        ] {
            if enabled {
                source.push(flag);
            }
        }
        source
    }

    pub const fn unicode_mode(self) -> bool {
        self.unicode || self.unicode_sets
    }

    pub const fn initial_mode(self) -> MatchMode {
        MatchMode {
            ignore_case: self.ignore_case,
            multiline: self.multiline,
            dot_all: self.dot_all,
            unicode: self.unicode_mode(),
            unicode_sets: self.unicode_sets,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "inline modifiers independently select five executor modes"
)]
pub(crate) struct MatchMode {
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub unicode_sets: bool,
}
