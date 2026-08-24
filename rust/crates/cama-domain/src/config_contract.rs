//! Environment-independent configuration defaults and pinned balance values.
//!
//! This module never reads or mutates process environment state. Runtime code
//! may inject string values through [`ConfigContract::from_lookup`] or
//! [`ConfigContract::from_map`]. Missing and invalid numeric values fall back
//! to the same in-code defaults as `config.py`.

use std::{borrow::Cow, collections::HashMap};

/// Default time between match creation and the Dota betting lock, in seconds.
pub const DEFAULT_BET_LOCK_SECONDS: i64 = 20 * 60;
/// Default probability of choosing the OpenSkill shuffler for a shuffle.
pub const DEFAULT_OPENSKILL_SHUFFLE_CHANCE: f64 = 0.05;
/// Default total number of automatic spectator bets.
pub const DEFAULT_AUTO_SPECTATOR_BET_COUNT: i64 = 10;
/// Default number of richest spectators placed in the higher-percentage tier.
pub const DEFAULT_AUTO_SPECTATOR_BET_TOP_COUNT: i64 = 5;
/// Default fraction wagered for a normal automatic spectator bet.
pub const DEFAULT_AUTO_SPECTATOR_BET_PERCENTAGE: f64 = 0.01;
/// Default fraction wagered for a top-tier automatic spectator bet.
pub const DEFAULT_AUTO_SPECTATOR_BET_TOP_PERCENTAGE: f64 = 0.02;

/// Default expected-value target for the regular gamba wheel.
pub const DEFAULT_WHEEL_TARGET_EV: f64 = -27.5;
/// Default number of wins needed to clear the bankruptcy penalty.
pub const DEFAULT_BANKRUPTCY_PENALTY_GAMES: i64 = 3;
/// Default fraction of winnings retained while the bankruptcy penalty applies.
pub const DEFAULT_BANKRUPTCY_PENALTY_RATE: f64 = 0.75;

/// Price ladder for successive paid digs during one day.
pub const PAID_DIG_COSTS_PER_DAY: [i64; 8] = [3, 5, 10, 20, 40, 60, 80, 100];
/// Resolution vote threshold shared with match recording.
pub const PREDICTION_MIN_RESOLUTION_VOTES: i64 = 3;

/// Default model when a Groq credential is available at startup.
pub const DEFAULT_GROQ_MODEL: &str = "groq/qwen/qwen3.6-27b";
/// Default model when no Groq credential is available at startup.
pub const DEFAULT_CEREBRAS_MODEL: &str = "cerebras/gemma-4-31b";

/// Select an LLM model and only the credential belonging to that provider.
///
/// An explicit model wins over provider availability. Unknown provider
/// prefixes deliberately receive no credential, matching Python's protection
/// against sending one provider's secret to another endpoint.
#[must_use]
pub fn select_llm_config<'value>(
    groq_key: Option<&'value str>,
    cerebras_key: Option<&'value str>,
    model_override: Option<&'value str>,
) -> (Cow<'value, str>, Option<&'value str>) {
    let model = model_override.map_or_else(
        || {
            Cow::Borrowed(if groq_key.is_some() {
                DEFAULT_GROQ_MODEL
            } else {
                DEFAULT_CEREBRAS_MODEL
            })
        },
        Cow::Borrowed,
    );
    let api_key = if model.starts_with("groq/") {
        groq_key
    } else if model.starts_with("cerebras/") {
        cerebras_key
    } else {
        None
    };

    (model, api_key)
}

/// The environment-overridable configuration values covered by this contract.
///
/// Signed integers are intentional: Python's configuration parser accepts
/// negative overrides and leaves validation to the consuming feature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfigContract {
    /// Seconds until Dota bets lock.
    pub bet_lock_seconds: i64,
    /// Probability of using OpenSkill for a shuffle.
    pub openskill_shuffle_chance: f64,
    /// Total cap for automatic spectator bets, including the top tier.
    pub auto_spectator_bet_count: i64,
    /// Number of automatic bettors selected for the richest tier.
    pub auto_spectator_bet_top_count: i64,
    /// Fraction wagered by ordinary automatic spectator bettors.
    pub auto_spectator_bet_percentage: f64,
    /// Fraction wagered by top-tier automatic spectator bettors.
    pub auto_spectator_bet_top_percentage: f64,
    /// Expected-value target for the regular gamba wheel.
    pub wheel_target_ev: f64,
    /// Wins needed to clear the bankruptcy penalty.
    pub bankruptcy_penalty_games: i64,
    /// Fraction of winnings retained during the bankruptcy penalty.
    pub bankruptcy_penalty_rate: f64,
}

impl Default for ConfigContract {
    fn default() -> Self {
        Self {
            bet_lock_seconds: DEFAULT_BET_LOCK_SECONDS,
            openskill_shuffle_chance: DEFAULT_OPENSKILL_SHUFFLE_CHANCE,
            auto_spectator_bet_count: DEFAULT_AUTO_SPECTATOR_BET_COUNT,
            auto_spectator_bet_top_count: DEFAULT_AUTO_SPECTATOR_BET_TOP_COUNT,
            auto_spectator_bet_percentage: DEFAULT_AUTO_SPECTATOR_BET_PERCENTAGE,
            auto_spectator_bet_top_percentage: DEFAULT_AUTO_SPECTATOR_BET_TOP_PERCENTAGE,
            wheel_target_ev: DEFAULT_WHEEL_TARGET_EV,
            bankruptcy_penalty_games: DEFAULT_BANKRUPTCY_PENALTY_GAMES,
            bankruptcy_penalty_rate: DEFAULT_BANKRUPTCY_PENALTY_RATE,
        }
    }
}

impl ConfigContract {
    /// Build the contract from an injected string lookup.
    ///
    /// The lookup returns `None` for an unset value. As in Python's `_parse_int`
    /// and `_parse_float`, malformed values fall back to that field's default.
    /// No range clamping or relationship between spectator counts is imposed.
    #[must_use]
    pub fn from_lookup<'value>(mut lookup: impl FnMut(&str) -> Option<&'value str>) -> Self {
        Self {
            bet_lock_seconds: parse_i64(lookup("BET_LOCK_SECONDS"), DEFAULT_BET_LOCK_SECONDS),
            openskill_shuffle_chance: parse_f64(
                lookup("OPENSKILL_SHUFFLE_CHANCE"),
                DEFAULT_OPENSKILL_SHUFFLE_CHANCE,
            ),
            auto_spectator_bet_count: parse_i64(
                lookup("AUTO_SPECTATOR_BET_COUNT"),
                DEFAULT_AUTO_SPECTATOR_BET_COUNT,
            ),
            auto_spectator_bet_top_count: parse_i64(
                lookup("AUTO_SPECTATOR_BET_TOP_COUNT"),
                DEFAULT_AUTO_SPECTATOR_BET_TOP_COUNT,
            ),
            auto_spectator_bet_percentage: parse_f64(
                lookup("AUTO_SPECTATOR_BET_PERCENTAGE"),
                DEFAULT_AUTO_SPECTATOR_BET_PERCENTAGE,
            ),
            auto_spectator_bet_top_percentage: parse_f64(
                lookup("AUTO_SPECTATOR_BET_TOP_PERCENTAGE"),
                DEFAULT_AUTO_SPECTATOR_BET_TOP_PERCENTAGE,
            ),
            wheel_target_ev: parse_f64(lookup("WHEEL_TARGET_EV"), DEFAULT_WHEEL_TARGET_EV),
            bankruptcy_penalty_games: parse_i64(
                lookup("BANKRUPTCY_PENALTY_GAMES"),
                DEFAULT_BANKRUPTCY_PENALTY_GAMES,
            ),
            bankruptcy_penalty_rate: parse_f64(
                lookup("BANKRUPTCY_PENALTY_RATE"),
                DEFAULT_BANKRUPTCY_PENALTY_RATE,
            ),
        }
    }

    /// Build the contract from a borrowed map of environment-style strings.
    #[must_use]
    pub fn from_map(values: &HashMap<String, String>) -> Self {
        Self::from_lookup(|name| values.get(name).map(String::as_str))
    }
}

fn parse_i64(raw: Option<&str>, default: i64) -> i64 {
    raw.and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

fn parse_f64(raw: Option<&str>, default: f64) -> f64 {
    raw.and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        ConfigContract, PAID_DIG_COSTS_PER_DAY, PREDICTION_MIN_RESOLUTION_VOTES, select_llm_config,
    };

    #[test]
    fn test_dota_betting_window_defaults_to_twenty_minutes() {
        let config = ConfigContract::default();

        assert_eq!(config.bet_lock_seconds, 20 * 60);
    }

    #[test]
    fn test_openskill_shuffle_chance_defaults_to_five_percent() {
        let config = ConfigContract::default();

        assert_eq!(config.openskill_shuffle_chance, 0.05);
    }

    #[test]
    fn test_spectator_autobet_defaults_to_two_five_player_tiers() {
        let config = ConfigContract::default();

        assert_eq!(config.auto_spectator_bet_count, 10);
        assert_eq!(config.auto_spectator_bet_top_count, 5);
        assert_eq!(config.auto_spectator_bet_top_percentage, 0.02);
        assert_eq!(config.auto_spectator_bet_percentage, 0.01);
    }

    #[test]
    fn test_spectator_autobet_count_override_remains_the_total_cap() {
        let values = HashMap::from([
            ("AUTO_SPECTATOR_BET_COUNT".to_owned(), "7".to_owned()),
            ("AUTO_SPECTATOR_BET_TOP_COUNT".to_owned(), "3".to_owned()),
        ]);
        let config = ConfigContract::from_map(&values);

        assert_eq!(config.auto_spectator_bet_count, 7);
        assert_eq!(config.auto_spectator_bet_top_count, 3);
    }

    #[test]
    fn test_paid_dig_cost_ladder_pinned() {
        assert_eq!(PAID_DIG_COSTS_PER_DAY, [3, 5, 10, 20, 40, 60, 80, 100]);
    }

    #[test]
    fn test_gamba_wheel_target_ev_pinned() {
        let config = ConfigContract::default();

        assert_eq!(config.wheel_target_ev, -27.5);
    }

    #[test]
    fn test_bankruptcy_penalty_pinned() {
        let config = ConfigContract::default();

        assert_eq!(config.bankruptcy_penalty_games, 3);
        assert_eq!(config.bankruptcy_penalty_rate, 0.75);
    }

    #[test]
    fn test_prediction_resolution_threshold_pinned() {
        assert_eq!(PREDICTION_MIN_RESOLUTION_VOTES, 3);
    }

    #[test]
    fn invalid_numeric_overrides_fall_back_independently() {
        let values = HashMap::from([
            ("BET_LOCK_SECONDS".to_owned(), "not-an-int".to_owned()),
            (
                "AUTO_SPECTATOR_BET_COUNT".to_owned(),
                "still-not-an-int".to_owned(),
            ),
            ("AUTO_SPECTATOR_BET_TOP_COUNT".to_owned(), " 3 ".to_owned()),
            (
                "AUTO_SPECTATOR_BET_PERCENTAGE".to_owned(),
                "bad-float".to_owned(),
            ),
        ]);
        let config = ConfigContract::from_map(&values);

        assert_eq!(config.bet_lock_seconds, 20 * 60);
        assert_eq!(config.auto_spectator_bet_count, 10);
        assert_eq!(config.auto_spectator_bet_top_count, 3);
        assert_eq!(config.auto_spectator_bet_percentage, 0.01);
    }

    #[test]
    fn lookup_injection_applies_all_supported_overrides() {
        let values = HashMap::from([
            ("BET_LOCK_SECONDS".to_owned(), "900".to_owned()),
            ("OPENSKILL_SHUFFLE_CHANCE".to_owned(), "0.5".to_owned()),
            ("AUTO_SPECTATOR_BET_COUNT".to_owned(), "7".to_owned()),
            ("AUTO_SPECTATOR_BET_TOP_COUNT".to_owned(), "3".to_owned()),
            (
                "AUTO_SPECTATOR_BET_PERCENTAGE".to_owned(),
                "0.03".to_owned(),
            ),
            (
                "AUTO_SPECTATOR_BET_TOP_PERCENTAGE".to_owned(),
                "0.04".to_owned(),
            ),
            ("WHEEL_TARGET_EV".to_owned(), "-10.5".to_owned()),
            ("BANKRUPTCY_PENALTY_GAMES".to_owned(), "4".to_owned()),
            ("BANKRUPTCY_PENALTY_RATE".to_owned(), "0.5".to_owned()),
        ]);
        let config = ConfigContract::from_lookup(|name| values.get(name).map(String::as_str));

        assert_eq!(
            config,
            ConfigContract {
                bet_lock_seconds: 900,
                openskill_shuffle_chance: 0.5,
                auto_spectator_bet_count: 7,
                auto_spectator_bet_top_count: 3,
                auto_spectator_bet_percentage: 0.03,
                auto_spectator_bet_top_percentage: 0.04,
                wheel_target_ev: -10.5,
                bankruptcy_penalty_games: 4,
                bankruptcy_penalty_rate: 0.5,
            }
        );
    }

    #[test]
    fn test_groq_is_primary_when_both_keys_exist() {
        assert_eq!(
            select_llm_config(Some("groq-key"), Some("cerebras-key"), None),
            ("groq/qwen/qwen3.6-27b".into(), Some("groq-key"))
        );
    }

    #[test]
    fn test_cerebras_only_defaults_to_gemma_4() {
        assert_eq!(
            select_llm_config(None, Some("cerebras-key"), None),
            ("cerebras/gemma-4-31b".into(), Some("cerebras-key"))
        );
    }

    #[test]
    fn test_model_override_uses_matching_provider_key() {
        assert_eq!(
            select_llm_config(
                Some("groq-key"),
                Some("cerebras-key"),
                Some("cerebras/gemma-4-31b"),
            ),
            ("cerebras/gemma-4-31b".into(), Some("cerebras-key"))
        );
    }

    #[test]
    fn test_unsupported_provider_does_not_reuse_another_provider_key() {
        assert_eq!(
            select_llm_config(Some("groq-key"), Some("cerebras-key"), Some("other/model"),),
            ("other/model".into(), None)
        );
    }
}
