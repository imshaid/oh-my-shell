//! Whether the AI layer is available, from the command line and the `[ai]` section together
//! (blueprint §13.1: off until a provider is set up; `--no-ai` switches it off for a session).

use crate::schema::{Ai, Provider};

/// What the AI layer may do in this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiState {
    /// `--no-ai` was given. It wins over everything in the file.
    OffByFlag,
    /// `[ai] enabled = false`.
    OffByConfig,
    /// On, but no provider and model chosen yet: `omsh ai setup` is the way forward.
    NotSetUp,
    /// A provider and a model are chosen.
    Ready {
        provider: Provider,
        model: String,
        /// The address from `[ai] base_url`, if the user set one.
        base_url: Option<String>,
    },
}

impl AiState {
    /// Combines the `[ai]` section with the `--no-ai` switch.
    #[must_use]
    pub fn new(ai: &Ai, no_ai: bool) -> Self {
        if no_ai {
            return Self::OffByFlag;
        }
        if !ai.enabled {
            return Self::OffByConfig;
        }
        match (ai.provider, &ai.model) {
            (Some(provider), Some(model)) => Self::Ready {
                provider,
                model: model.clone(),
                base_url: ai.base_url.clone(),
            },
            _ => Self::NotSetUp,
        }
    }

    /// One line for `omsh ai status` and for the message when a request cannot be made.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        match self {
            Self::OffByFlag => "off for this session (--no-ai)",
            Self::OffByConfig => "off ([ai] enabled = false in config.toml)",
            Self::NotSetUp => "not set up yet (run: omsh ai setup)",
            Self::Ready { .. } => "ready",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_up() -> Ai {
        Ai {
            provider: Some(Provider::Anthropic),
            model: Some("m".to_owned()),
            ..Ai::default()
        }
    }

    #[test]
    fn nothing_chosen_means_not_set_up() {
        assert_eq!(AiState::new(&Ai::default(), false), AiState::NotSetUp);
    }

    #[test]
    fn a_provider_without_a_model_is_not_set_up() {
        let ai = Ai {
            provider: Some(Provider::Ollama),
            ..Ai::default()
        };
        assert_eq!(AiState::new(&ai, false), AiState::NotSetUp);
    }

    #[test]
    fn a_provider_and_a_model_are_ready() {
        let mut ai = set_up();
        ai.base_url = Some("https://example.org/v1".to_owned());
        assert_eq!(
            AiState::new(&ai, false),
            AiState::Ready {
                provider: Provider::Anthropic,
                model: "m".to_owned(),
                base_url: Some("https://example.org/v1".to_owned()),
            }
        );
    }

    #[test]
    fn the_config_switch_turns_it_off() {
        let ai = Ai {
            enabled: false,
            ..set_up()
        };
        assert_eq!(AiState::new(&ai, false), AiState::OffByConfig);
    }

    #[test]
    fn the_flag_wins_over_a_complete_setup_and_over_the_config_switch() {
        assert_eq!(AiState::new(&set_up(), true), AiState::OffByFlag);
        let off = Ai {
            enabled: false,
            ..set_up()
        };
        assert_eq!(AiState::new(&off, true), AiState::OffByFlag);
    }
}
