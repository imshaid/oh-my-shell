//! Risk levels (§12.1).

/// How risky a command is. The order matters: a later level is more risky.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Runs without a word.
    Safe,
    /// Runs, with a one-line note.
    Caution,
    /// Asks first, and shows what it would touch.
    Dangerous,
    /// Asks first, and wants the target typed.
    Critical,
}

impl Level {
    /// The lowercase name, as shown to the person and written in `config.toml`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Caution => "caution",
            Self::Dangerous => "dangerous",
            Self::Critical => "critical",
        }
    }

    /// One step up. `Safe` stays `Safe` (`sudo` only raises what is already flagged) and
    /// `Critical` is the top.
    #[must_use]
    pub const fn raise(self) -> Self {
        match self {
            Self::Safe => Self::Safe,
            Self::Caution => Self::Dangerous,
            Self::Dangerous | Self::Critical => Self::Critical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn levels_are_ordered_from_safe_to_critical() {
        assert!(Level::Safe < Level::Caution);
        assert!(Level::Caution < Level::Dangerous);
        assert!(Level::Dangerous < Level::Critical);
    }

    #[test]
    fn raise_goes_one_step_and_stops_at_the_ends() {
        assert_eq!(Level::Safe.raise(), Level::Safe);
        assert_eq!(Level::Caution.raise(), Level::Dangerous);
        assert_eq!(Level::Dangerous.raise(), Level::Critical);
        assert_eq!(Level::Critical.raise(), Level::Critical);
    }

    #[test]
    fn names_are_lowercase_words() {
        assert_eq!(Level::Safe.name(), "safe");
        assert_eq!(Level::Critical.name(), "critical");
    }
}
