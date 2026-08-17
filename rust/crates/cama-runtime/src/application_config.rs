//! Typed lift-and-shift of every setting declared by Python's `config.py`.
//!
//! Ordinary application settings deliberately retain Python's fail-soft parsing:
//! malformed integers, floats, and lists fall back to their declared defaults;
//! any non-affirmative boolean string is false. Only process-critical gateway
//! settings are validated separately by `RuntimeConfig`.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::sync::Arc;

use cama_app::opendota_http::{OpenDotaHttpBuildError, OpenDotaRuntimeServices};
use cama_app::service_container::ServiceContainerOptions;
use cama_app::shop_commands::SHOP_SOFT_AVOID_COST;
use cama_db::schema_manager::MigrationSettings;

use crate::{DiscordToken, RuntimeConfig};

pub const MAFIA_CHANNEL_ID: i64 = 1_514_997_325_385_306_132;
const DEFAULT_GROQ_MODEL: &str = "groq/qwen/qwen3.6-27b";
const DEFAULT_CEREBRAS_MODEL: &str = "cerebras/gemma-4-31b";

#[derive(Clone, Eq, PartialEq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityConfig {
    pub admin_user_ids: Vec<i64>,
    pub tax_man_user_ids: Vec<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelConfig {
    pub gamba: Option<i64>,
    pub lobby: Option<i64>,
    pub low_skill_lobby: Option<i64>,
    pub dig: Option<i64>,
    pub duel: Option<i64>,
    pub mafia: i64,
    pub pet: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmConfig {
    pub groq_api_key: Option<Secret>,
    pub cerebras_api_key: Option<Secret>,
    pub model: String,
    pub selected_api_key: Option<Secret>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub admin_rating_adjustment_max_games: i64,
    pub admin_rating_max: f64,
    pub ai_features_enabled: bool,
    pub ai_max_tokens: i64,
    pub ai_rate_limit_requests: i64,
    pub ai_rate_limit_window: i64,
    pub ai_timeout_seconds: f64,
    pub auto_blind_enabled: bool,
    pub auto_blind_percentage: f64,
    pub auto_blind_threshold: i64,
    pub auto_spectator_bet_count: i64,
    pub auto_spectator_bet_enabled: bool,
    pub auto_spectator_bet_percentage: f64,
    pub auto_spectator_bet_top_count: i64,
    pub auto_spectator_bet_top_percentage: f64,
    pub bankruptcy_cooldown_seconds: i64,
    pub bankruptcy_fresh_start_balance: i64,
    pub bankruptcy_penalty_games: i64,
    pub bankruptcy_penalty_rate: f64,
    pub base_rating_delta_multiplier: f64,
    pub bet_last_call_offset: i64,
    pub bet_lock_seconds: i64,
    pub bet_reminder_offsets: Vec<i64>,
    pub bet_underdog_ping_ratio: f64,
    pub bomb_pot_ante: i64,
    pub bomb_pot_blind_percentage: f64,
    pub bomb_pot_chance: f64,
    pub bomb_pot_participation_bonus: i64,
    pub calibration_rd_threshold: f64,
    pub dig_llm_enabled: bool,
    pub disburse_min_fund: i64,
    pub disburse_quorum_percentage: f64,
    pub dota_bet_seed_amount: i64,
    pub double_or_nothing_cooldown_seconds: i64,
    pub economy_events_enabled: bool,
    pub economy_event_lookback_days: i64,
    pub economy_event_max_reserve_burn_pct: f64,
    pub economy_event_max_wallet_burn_pct: f64,
    pub economy_event_trigger_hour_local: i64,
    pub economy_event_wake_seconds: i64,
    pub economy_inflation_ceiling: f64,
    pub economy_normal_annual_rate: f64,
    pub enrichment_discovery_time_window: i64,
    pub enrichment_min_player_match: i64,
    pub enrichment_retry_delays: Vec<i64>,
    pub exclusion_penalty_weight: f64,
    pub first_game_pool_daily_amount: i64,
    /// Explicit dev-only wheel fixture support. When enabled, negative
    /// Discord IDs in the database are treated as synthetic guild members.
    pub gamba_synthetic_members_enabled: bool,
    pub garnishment_percentage: f64,
    pub hostile_loss_min_balance: i64,
    pub house_payout_multiplier: f64,
    pub initial_glicko_rd: f64,
    pub jopacoin_exclusion_reward: i64,
    pub jopacoin_min_bet: i64,
    pub jopacoin_per_game: i64,
    pub jopacoin_win_reward: i64,
    pub leverage_tiers: Vec<i64>,
    pub lightning_bolt_min_tax: i64,
    pub lightning_bolt_pct_max: f64,
    pub lightning_bolt_pct_min: f64,
    pub loan_cooldown_seconds: i64,
    pub loan_fee_rate: f64,
    pub loan_max_amount: i64,
    pub lobby_max_players: i64,
    pub lobby_rally_cooldown_seconds: i64,
    pub lobby_ready_cooldown_seconds: i64,
    pub lobby_ready_threshold: i64,
    pub lottery_activity_days: i64,
    pub max_debt: i64,
    pub max_rating_swing_per_game: f64,
    pub max_rd_contraction_per_game: f64,
    pub minigame_jc_delta_scale: f64,
    pub mmr_modal_retry_limit: i64,
    pub mmr_modal_timeout_minutes: i64,
    pub neon_bigwin_floor: f64,
    pub neon_bigwin_full_payout: i64,
    pub neon_bigwin_min_payout: i64,
    pub neon_cooldown_seconds: i64,
    pub neon_degen_enabled: bool,
    pub neon_dig_chance: f64,
    pub neon_layer1_chance: f64,
    pub neon_llm_chance: f64,
    pub neon_mvp_chance: f64,
    pub new_player_exclusion_boost: i64,
    pub new_player_mmr_discount: i64,
    pub off_role_flat_value_penalty: f64,
    pub off_role_flat_penalty: f64,
    pub off_role_multiplier: f64,
    pub openskill_calibration_sigma_threshold: f64,
    pub openskill_performance_strength: f64,
    pub openskill_shuffle_chance: f64,
    pub openskill_sigma_decay_grace_period_days: i64,
    pub openskill_sigma_decay_per_week: f64,
    pub openskill_win_probability_temperature: f64,
    pub package_deal_games_duration: i64,
    pub package_deal_penalty: f64,
    pub package_deal_split_penalty: f64,
    pub pet_hunger_decay_per_day: i64,
    pub pingedash_cooldown_seconds: i64,
    pub pingedash_cost: i64,
    pub pingedash_target_user_id: Option<i64>,
    pub pingedkevin_target_user_id: Option<i64>,
    pub player_trivia_answer_timeout_seconds: i64,
    pub player_trivia_cooldown_seconds: i64,
    pub player_trivia_include_spicy: bool,
    pub player_trivia_question_count: i64,
    pub player_trivia_recent_days: i64,
    pub player_trivia_reward_per_correct: i64,
    pub prediction_contract_value: i64,
    pub prediction_digest_hour_utc: i64,
    pub prediction_drift_max: i64,
    pub prediction_drift_min: i64,
    pub prediction_fade_ticks: i64,
    pub prediction_initial_fair_default: i64,
    pub prediction_levels_per_side: i64,
    pub prediction_max_contracts_per_trade: i64,
    pub prediction_outer_level_sizes: Vec<i64>,
    pub prediction_price_high: i64,
    pub prediction_price_low: i64,
    pub prediction_recent_trades_shown: i64,
    pub prediction_refresh_levels_per_side: i64,
    pub prediction_refresh_outer_level_sizes: Vec<i64>,
    pub prediction_refresh_seconds: i64,
    pub prediction_refresh_size_per_level: i64,
    pub prediction_refresh_spread_ticks: i64,
    pub prediction_refresh_wake_seconds: i64,
    pub prediction_size_per_level: i64,
    pub prediction_spread_ticks: i64,
    pub prediction_tick_size: i64,
    pub rating_spread_divisor: f64,
    pub rd_decay_constant: f64,
    pub rd_decay_grace_period_days: i64,
    pub rd_priority_weight: f64,
    pub recalibration_cooldown_seconds: i64,
    pub recalibration_initial_rd: f64,
    pub recalibration_initial_volatility: f64,
    pub region_split_penalty: f64,
    pub role_matchup_delta_weight: f64,
    pub shop_announce_cost: i64,
    pub shop_announce_target_cost: i64,
    pub shop_double_or_nothing_cost: i64,
    pub shop_jopa_coin_cost: i64,
    pub shop_new_mystery_gift_cost: i64,
    pub shop_package_deal_base_cost: i64,
    pub shop_package_deal_rating_divisor: f64,
    pub shop_recalibrate_cost: i64,
    pub shop_soft_avoid_cost: i64,
    pub soft_avoid_penalty: f64,
    pub streak_multiplier_per_game: f64,
    pub streak_threshold: i64,
    pub streaming_bonus: i64,
    pub tax_fine_cooldown_seconds: i64,
    pub tip_fee_rate: f64,
    pub trivia_answer_timeout_seconds: i64,
    pub trivia_bankrupt_multiplier: f64,
    pub trivia_cooldown_seconds: i64,
    pub trivia_reward_per_question: i64,
    pub use_glicko: bool,
    pub vanity_tax_rate: f64,
    pub wheel_banana_peel_est_ev: f64,
    pub wheel_banana_peel_loss_max: i64,
    pub wheel_banana_peel_loss_min: i64,
    pub wheel_bankrupt_target_ev: f64,
    pub wheel_blue_shell_est_ev: f64,
    pub wheel_bomb_omb_est_ev: f64,
    pub wheel_bomb_omb_victim_count: i64,
    pub wheel_bomb_omb_victim_loss_max: i64,
    pub wheel_bomb_omb_victim_loss_min: i64,
    pub wheel_chain_reaction_est_ev: f64,
    pub wheel_comeback_est_ev: f64,
    pub wheel_commune_est_ev: f64,
    pub wheel_cooldown_seconds: i64,
    pub wheel_discover_est_ev: f64,
    pub wheel_emergency_est_ev: f64,
    pub wheel_extend_1_est_ev: f64,
    pub wheel_extend_2_est_ev: f64,
    pub wheel_golden_compound_est_ev: f64,
    pub wheel_golden_dividend_est_ev: f64,
    pub wheel_golden_heist_est_ev: f64,
    pub wheel_golden_hostile_takeover_est_ev: f64,
    pub wheel_golden_market_crash_est_ev: f64,
    pub wheel_golden_recession_est_ev: f64,
    pub wheel_golden_recession_mid_pct: f64,
    pub wheel_golden_recession_mid_rank_end: i64,
    pub wheel_golden_recession_rest_pct: f64,
    pub wheel_golden_recession_top_pct: f64,
    pub wheel_golden_target_ev: f64,
    pub wheel_golden_top_n: i64,
    pub wheel_golden_trickle_down_est_ev: f64,
    pub wheel_golden_trickle_down_pct_max: f64,
    pub wheel_golden_trickle_down_pct_min: f64,
    pub wheel_green_shell_est_ev: f64,
    pub wheel_green_shell_steal_max: i64,
    pub wheel_green_shell_steal_min: i64,
    pub wheel_jailbreak_est_ev: f64,
    pub wheel_lightning_bolt_est_ev: f64,
    pub wheel_lose_penalty_cooldown: i64,
    pub wheel_red_shell_est_ev: f64,
    pub wheel_target_ev: f64,
    pub wheel_town_trial_est_ev: f64,
    pub white_bankrupt_stipend: i64,
    pub wrapped_min_bets: i64,
    pub wrapped_min_games: i64,
    /// Hard-coded shuffler value retained from Python's SHUFFLER_SETTINGS.
    pub recent_match_penalty_weight: f64,
    /// Python compatibility alias; always follows `pingedash_cost`.
    pub pingedkevin_cost: i64,
    /// Python compatibility alias; always follows `pingedash_cooldown_seconds`.
    pub pingedkevin_cooldown_seconds: i64,
}

#[derive(Clone, PartialEq)]
pub struct ApplicationConfig {
    pub runtime: RuntimeConfig,
    pub identities: IdentityConfig,
    pub channels: ChannelConfig,
    pub values: Values,
    /// Exact schema-backfill settings parsed from the same environment snapshot.
    pub migration: MigrationSettings,
    pub llm: LlmConfig,
    pub opendota_api_key: Option<Secret>,
}

impl fmt::Debug for ApplicationConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationConfig")
            .field("runtime", &self.runtime)
            .field("identities", &self.identities)
            .field("channels", &self.channels)
            .field("values", &self.values)
            .field("migration", &self.migration)
            .field("llm", &self.llm)
            .field("opendota_api_key", &self.opendota_api_key)
            .finish()
    }
}

impl ApplicationConfig {
    pub fn from_env() -> Result<Self, crate::ConfigError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    /// The env-configured Glicko system shared by every rating-mutating path.
    ///
    /// Python's module-level constants make its default-constructed
    /// `CamaRatingSystem` env-configured; the Rust default bakes compile-time
    /// constants, so production paths must construct through here or silently
    /// ignore deployment overrides such as `MAX_RATING_SWING_PER_GAME`.
    pub fn glicko_rating_system(
        &self,
    ) -> Result<cama_domain::rating::CamaRatingSystem, &'static str> {
        use cama_domain::rating::{CamaRatingSystem, RatingConfig};
        Ok(CamaRatingSystem::with_config(
            self.values.initial_glicko_rd,
            0.06,
            RatingConfig {
                calibration_rd_threshold: self.values.calibration_rd_threshold,
                initial_rd: self.values.initial_glicko_rd,
                max_rating_swing_per_game: self.values.max_rating_swing_per_game,
                base_rating_delta_multiplier: self.values.base_rating_delta_multiplier,
                max_rd_contraction_per_game: self.values.max_rd_contraction_per_game,
                new_player_mmr_discount: self.migration.new_player_mmr_discount,
                rd_decay_constant: self.values.rd_decay_constant,
                rd_decay_grace_period_days: i32::try_from(self.values.rd_decay_grace_period_days)
                    .map_err(|_| "RD_DECAY_GRACE_PERIOD_DAYS")?,
                streak_threshold: self.values.streak_threshold,
                streak_multiplier_per_game: self.values.streak_multiplier_per_game,
            },
        ))
    }

    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, crate::ConfigError> {
        let captured = all_runtime_env_keys()
            .into_iter()
            .filter_map(|name| lookup(name).map(|value| (name, value)))
            .collect::<BTreeMap<_, _>>();
        Self::from_captured(captured)
    }

    fn from_captured(captured: BTreeMap<&'static str, String>) -> Result<Self, crate::ConfigError> {
        let runtime = RuntimeConfig::from_lookup(|name| captured.get(name).cloned())?;
        let p = Parser(&captured);
        let migration = MigrationSettings::from_lookup(|name| captured.get(name).cloned());
        let admin_user_ids = p.admin_ids();
        let tax_man_user_ids = p.tax_man_ids();
        let groq_api_key = p.secret("GROQ_API_KEY");
        let cerebras_api_key = p.secret("CEREBRAS_API_KEY");
        let model_override = captured
            .get("AI_MODEL")
            .map(String::as_str)
            .filter(|value| !value.is_empty());
        let model = model_override.map_or_else(
            || {
                if groq_api_key
                    .as_ref()
                    .is_some_and(|key| !key.expose().is_empty())
                {
                    DEFAULT_GROQ_MODEL
                } else {
                    DEFAULT_CEREBRAS_MODEL
                }
                .to_owned()
            },
            str::to_owned,
        );
        let selected_api_key = if model.starts_with("groq/") {
            groq_api_key.clone()
        } else if model.starts_with("cerebras/") {
            cerebras_api_key.clone()
        } else {
            None
        };

        Ok(Self {
            runtime,
            identities: IdentityConfig {
                admin_user_ids,
                tax_man_user_ids,
            },
            channels: ChannelConfig {
                gamba: p.optional_channel("GAMBA_CHANNEL_ID"),
                lobby: p.optional_channel("LOBBY_CHANNEL_ID"),
                low_skill_lobby: p.optional_channel("LOWSKILL_LOBBY_CHANNEL_ID"),
                dig: p.optional_channel("DIG_CHANNEL_ID"),
                duel: p.optional_channel("DUEL_CHANNEL_ID"),
                mafia: MAFIA_CHANNEL_ID,
                pet: p.optional_channel("PET_CHANNEL_ID"),
            },
            values: Values {
                admin_rating_adjustment_max_games: p.i64("ADMIN_RATING_ADJUSTMENT_MAX_GAMES", 50),
                admin_rating_max: p.f64("ADMIN_RATING_MAX", 6000.0),
                ai_features_enabled: p.bool("AI_FEATURES_ENABLED", false),
                ai_max_tokens: p.i64("AI_MAX_TOKENS", 500),
                ai_rate_limit_requests: p.i64("AI_RATE_LIMIT_REQUESTS", 10),
                ai_rate_limit_window: p.i64("AI_RATE_LIMIT_WINDOW", 60),
                ai_timeout_seconds: p.f64("AI_TIMEOUT_SECONDS", 15.0),
                auto_blind_enabled: p.bool("AUTO_BLIND_ENABLED", true),
                auto_blind_percentage: p.f64("AUTO_BLIND_PERCENTAGE", 0.1),
                auto_blind_threshold: p.i64("AUTO_BLIND_THRESHOLD", 50),
                auto_spectator_bet_count: p.i64("AUTO_SPECTATOR_BET_COUNT", 10),
                auto_spectator_bet_enabled: p.bool("AUTO_SPECTATOR_BET_ENABLED", true),
                auto_spectator_bet_percentage: p.f64("AUTO_SPECTATOR_BET_PERCENTAGE", 0.01),
                auto_spectator_bet_top_count: p.i64("AUTO_SPECTATOR_BET_TOP_COUNT", 5),
                auto_spectator_bet_top_percentage: p.f64("AUTO_SPECTATOR_BET_TOP_PERCENTAGE", 0.02),
                bankruptcy_cooldown_seconds: p.i64("BANKRUPTCY_COOLDOWN_SECONDS", 604800),
                bankruptcy_fresh_start_balance: p.i64("BANKRUPTCY_FRESH_START_BALANCE", 3),
                bankruptcy_penalty_games: p.i64("BANKRUPTCY_PENALTY_GAMES", 3),
                bankruptcy_penalty_rate: p.f64("BANKRUPTCY_PENALTY_RATE", 0.75),
                base_rating_delta_multiplier: p.f64("BASE_RATING_DELTA_MULTIPLIER", 0.98),
                bet_last_call_offset: p.i64("BET_LAST_CALL_OFFSET", 60),
                bet_lock_seconds: p.i64("BET_LOCK_SECONDS", 1200),
                bet_reminder_offsets: p.i64_list("BET_REMINDER_OFFSETS", &[600, 300]),
                bet_underdog_ping_ratio: p.f64("BET_UNDERDOG_PING_RATIO", 4.0),
                bomb_pot_ante: p.i64("BOMB_POT_ANTE", 10),
                bomb_pot_blind_percentage: p.f64("BOMB_POT_BLIND_PERCENTAGE", 0.15),
                bomb_pot_chance: p.f64("BOMB_POT_CHANCE", 0.2),
                bomb_pot_participation_bonus: p.i64("BOMB_POT_PARTICIPATION_BONUS", 5),
                calibration_rd_threshold: p.f64("CALIBRATION_RD_THRESHOLD", 100.0),
                dig_llm_enabled: p.bool("DIG_LLM_ENABLED", true),
                disburse_min_fund: p.i64("DISBURSE_MIN_FUND", 250),
                disburse_quorum_percentage: p.f64("DISBURSE_QUORUM_PERCENTAGE", 0.4),
                dota_bet_seed_amount: p.i64("DOTA_BET_SEED_AMOUNT", 50),
                double_or_nothing_cooldown_seconds: p
                    .i64("DOUBLE_OR_NOTHING_COOLDOWN_SECONDS", 2592000),
                economy_events_enabled: p.bool("ECONOMY_EVENTS_ENABLED", true),
                economy_event_lookback_days: p.i64("ECONOMY_EVENT_LOOKBACK_DAYS", 7),
                economy_event_max_reserve_burn_pct: p
                    .f64("ECONOMY_EVENT_MAX_RESERVE_BURN_PCT", 0.03),
                economy_event_max_wallet_burn_pct: p
                    .f64("ECONOMY_EVENT_MAX_WALLET_BURN_PCT", 0.0025),
                economy_event_trigger_hour_local: p.i64("ECONOMY_EVENT_TRIGGER_HOUR_LOCAL", 10),
                economy_event_wake_seconds: p.i64("ECONOMY_EVENT_WAKE_SECONDS", 3600),
                economy_inflation_ceiling: p.f64("ECONOMY_INFLATION_CEILING", 0.02),
                economy_normal_annual_rate: p.f64("ECONOMY_NORMAL_ANNUAL_RATE", 0.02),
                enrichment_discovery_time_window: p.i64("ENRICHMENT_DISCOVERY_TIME_WINDOW", 7200),
                enrichment_min_player_match: p.i64("ENRICHMENT_MIN_PLAYER_MATCH", 10),
                enrichment_retry_delays: p
                    .i64_list("ENRICHMENT_RETRY_DELAYS", &[1, 5, 20, 60, 180]),
                exclusion_penalty_weight: p.f64("EXCLUSION_PENALTY_WEIGHT", 70.0),
                first_game_pool_daily_amount: p.i64("FIRST_GAME_POOL_DAILY_AMOUNT", 100),
                gamba_synthetic_members_enabled: p.bool("GAMBA_SYNTHETIC_MEMBERS_ENABLED", false),
                garnishment_percentage: p.f64("GARNISHMENT_PERCENTAGE", 1.0),
                hostile_loss_min_balance: p.i64("HOSTILE_LOSS_MIN_BALANCE", 50),
                house_payout_multiplier: p.f64("HOUSE_PAYOUT_MULTIPLIER", 1.0),
                initial_glicko_rd: p.f64("INITIAL_GLICKO_RD", 350.0),
                jopacoin_exclusion_reward: p.i64("JOPACOIN_EXCLUSION_REWARD", 5),
                jopacoin_min_bet: p.i64("JOPACOIN_MIN_BET", 1),
                // Base Dota rewards are source-controlled so deployments cannot
                // silently drift from the 10/5 payout policy.
                jopacoin_per_game: 5,
                jopacoin_win_reward: 10,
                leverage_tiers: p.i64_list("LEVERAGE_TIERS", &[2, 3, 5]),
                lightning_bolt_min_tax: p.i64("LIGHTNING_BOLT_MIN_TAX", 1),
                lightning_bolt_pct_max: p.f64("LIGHTNING_BOLT_PCT_MAX", 0.0627),
                lightning_bolt_pct_min: p.f64("LIGHTNING_BOLT_PCT_MIN", 0.02),
                loan_cooldown_seconds: p.i64("LOAN_COOLDOWN_SECONDS", 259200),
                loan_fee_rate: p.f64("LOAN_FEE_RATE", 0.2),
                loan_max_amount: p.i64("LOAN_MAX_AMOUNT", 100),
                lobby_max_players: p.i64("LOBBY_MAX_PLAYERS", 20),
                lobby_rally_cooldown_seconds: p.i64("LOBBY_RALLY_COOLDOWN_SECONDS", 120),
                lobby_ready_cooldown_seconds: p.i64("LOBBY_READY_COOLDOWN_SECONDS", 60),
                lobby_ready_threshold: p.i64("LOBBY_READY_THRESHOLD", 10),
                lottery_activity_days: p.i64("LOTTERY_ACTIVITY_DAYS", 14),
                max_debt: p.i64("MAX_DEBT", 500),
                max_rating_swing_per_game: p.f64("MAX_RATING_SWING_PER_GAME", 400.0),
                max_rd_contraction_per_game: p.f64("MAX_RD_CONTRACTION_PER_GAME", 0.065),
                minigame_jc_delta_scale: p.f64("MINIGAME_JC_DELTA_SCALE", 1.0),
                mmr_modal_retry_limit: p.i64("MMR_MODAL_RETRY_LIMIT", 3),
                mmr_modal_timeout_minutes: p.i64("MMR_MODAL_TIMEOUT_MINUTES", 5),
                neon_bigwin_floor: p.f64("NEON_BIGWIN_FLOOR", 0.05),
                neon_bigwin_full_payout: p.i64("NEON_BIGWIN_FULL_PAYOUT", 5000),
                neon_bigwin_min_payout: p.i64("NEON_BIGWIN_MIN_PAYOUT", 500),
                neon_cooldown_seconds: p.i64("NEON_COOLDOWN_SECONDS", 60),
                neon_degen_enabled: p.bool("NEON_DEGEN_ENABLED", true),
                neon_dig_chance: p.f64("NEON_DIG_CHANCE", 0.12),
                neon_layer1_chance: p.f64("NEON_LAYER1_CHANCE", 0.35),
                neon_llm_chance: p.f64("NEON_LLM_CHANCE", 0.6),
                neon_mvp_chance: p.f64("NEON_MVP_CHANCE", 0.35),
                new_player_exclusion_boost: p.i64("NEW_PLAYER_EXCLUSION_BOOST", 5),
                new_player_mmr_discount: i64::from(migration.new_player_mmr_discount),
                off_role_flat_value_penalty: p.f64("OFF_ROLE_FLAT_VALUE_PENALTY", 100.0),
                off_role_flat_penalty: p.f64("OFF_ROLE_FLAT_PENALTY", 500.0),
                off_role_multiplier: p.f64("OFF_ROLE_MULTIPLIER", 0.95),
                openskill_calibration_sigma_threshold: migration.openskill.calibration_threshold,
                openskill_performance_strength: migration.openskill.performance_strength,
                openskill_shuffle_chance: p.f64("OPENSKILL_SHUFFLE_CHANCE", 0.02),
                openskill_sigma_decay_grace_period_days: migration
                    .openskill
                    .sigma_decay_grace_period_days,
                openskill_sigma_decay_per_week: migration.openskill.sigma_decay_per_week,
                openskill_win_probability_temperature: migration
                    .openskill
                    .win_probability_temperature,
                package_deal_games_duration: p.i64("PACKAGE_DEAL_GAMES_DURATION", 10),
                package_deal_penalty: p.f64("PACKAGE_DEAL_PENALTY", 90.0),
                package_deal_split_penalty: p.f64("PACKAGE_DEAL_SPLIT_PENALTY", 90.0),
                pet_hunger_decay_per_day: p.i64("PET_HUNGER_DECAY_PER_DAY", 20),
                pingedash_cooldown_seconds: p.i64("PINGEDASH_COOLDOWN_SECONDS", 86400),
                pingedash_cost: p.i64("PINGEDASH_COST", 10),
                pingedash_target_user_id: p.optional_i64("PINGEDASH_TARGET_USER_ID"),
                pingedkevin_target_user_id: p.optional_i64("PINGEDKEVIN_TARGET_USER_ID"),
                player_trivia_answer_timeout_seconds: p
                    .i64("PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS", 20),
                player_trivia_cooldown_seconds: p.i64("PLAYER_TRIVIA_COOLDOWN_SECONDS", 86400),
                player_trivia_include_spicy: p.bool("PLAYER_TRIVIA_INCLUDE_SPICY", false),
                player_trivia_question_count: p.i64("PLAYER_TRIVIA_QUESTION_COUNT", 10),
                player_trivia_recent_days: p.i64("PLAYER_TRIVIA_RECENT_DAYS", 30),
                player_trivia_reward_per_correct: p.i64("PLAYER_TRIVIA_REWARD_PER_CORRECT", 1),
                prediction_contract_value: p.i64("PREDICTION_CONTRACT_VALUE", 10),
                prediction_digest_hour_utc: p.i64("PREDICTION_DIGEST_HOUR_UTC", 12),
                prediction_drift_max: p.i64("PREDICTION_DRIFT_MAX", 3),
                prediction_drift_min: p.i64("PREDICTION_DRIFT_MIN", -3),
                prediction_fade_ticks: p.i64("PREDICTION_FADE_TICKS", 5),
                prediction_initial_fair_default: p.i64("PREDICTION_INITIAL_FAIR_DEFAULT", 50),
                prediction_levels_per_side: p.i64("PREDICTION_LEVELS_PER_SIDE", 3),
                prediction_max_contracts_per_trade: p
                    .i64("PREDICTION_MAX_CONTRACTS_PER_TRADE", 1000),
                prediction_outer_level_sizes: p
                    .i64_list("PREDICTION_OUTER_LEVEL_SIZES", &[20, 15, 10, 5]),
                prediction_price_high: p.i64("PREDICTION_PRICE_HIGH", 95),
                prediction_price_low: p.i64("PREDICTION_PRICE_LOW", 5),
                prediction_recent_trades_shown: p.i64("PREDICTION_RECENT_TRADES_SHOWN", 5),
                prediction_refresh_levels_per_side: p.i64("PREDICTION_REFRESH_LEVELS_PER_SIDE", 3),
                prediction_refresh_outer_level_sizes: p
                    .i64_list("PREDICTION_REFRESH_OUTER_LEVEL_SIZES", &[8, 6, 4, 2]),
                prediction_refresh_seconds: p.i64("PREDICTION_REFRESH_SECONDS", 86400),
                prediction_refresh_size_per_level: p.i64("PREDICTION_REFRESH_SIZE_PER_LEVEL", 10),
                prediction_refresh_spread_ticks: p.i64("PREDICTION_REFRESH_SPREAD_TICKS", 4),
                prediction_refresh_wake_seconds: p.i64("PREDICTION_REFRESH_WAKE_SECONDS", 3600),
                prediction_size_per_level: p.i64("PREDICTION_SIZE_PER_LEVEL", 50),
                prediction_spread_ticks: p.i64("PREDICTION_SPREAD_TICKS", 2),
                prediction_tick_size: p.i64("PREDICTION_TICK_SIZE", 1),
                rating_spread_divisor: p.f64("RATING_SPREAD_DIVISOR", 10.0),
                rd_decay_constant: p.f64("RD_DECAY_CONSTANT", 100.0),
                rd_decay_grace_period_days: p.i64("RD_DECAY_GRACE_PERIOD_DAYS", 7),
                rd_priority_weight: p.f64("RD_PRIORITY_WEIGHT", 0.2),
                recalibration_cooldown_seconds: p.i64("RECALIBRATION_COOLDOWN_SECONDS", 7776000),
                recalibration_initial_rd: p.f64("RECALIBRATION_INITIAL_RD", 350.0),
                recalibration_initial_volatility: p.f64("RECALIBRATION_INITIAL_VOLATILITY", 0.06),
                region_split_penalty: p.f64("REGION_SPLIT_PENALTY", 500.0),
                role_matchup_delta_weight: p.f64("ROLE_MATCHUP_DELTA_WEIGHT", 0.17),
                shop_announce_cost: p.i64("SHOP_ANNOUNCE_COST", 10),
                shop_announce_target_cost: p.i64("SHOP_ANNOUNCE_TARGET_COST", 100),
                shop_double_or_nothing_cost: p.i64("SHOP_DOUBLE_OR_NOTHING_COST", 50),
                shop_jopa_coin_cost: p.i64("SHOP_JOPA_COIN_COST", 10000),
                shop_new_mystery_gift_cost: p.i64("SHOP_NEW_MYSTERY_GIFT_COST", 20000),
                shop_package_deal_base_cost: p.i64("SHOP_PACKAGE_DEAL_BASE_COST", 500),
                shop_package_deal_rating_divisor: p.f64("SHOP_PACKAGE_DEAL_RATING_DIVISOR", 10.0),
                shop_recalibrate_cost: p.i64("SHOP_RECALIBRATE_COST", 300),
                shop_soft_avoid_cost: p.i64("SHOP_SOFT_AVOID_COST", SHOP_SOFT_AVOID_COST),
                soft_avoid_penalty: p.f64("SOFT_AVOID_PENALTY", 160.0),
                streak_multiplier_per_game: migration.openskill.streak_multiplier_per_game,
                streak_threshold: migration.streak_threshold,
                streaming_bonus: p.i64("STREAMING_BONUS", 1),
                tax_fine_cooldown_seconds: p.i64("TAX_FINE_COOLDOWN_SECONDS", 2592000),
                tip_fee_rate: p.clamped_rate("TIP_FEE_RATE", 0.01),
                trivia_answer_timeout_seconds: p.i64("TRIVIA_ANSWER_TIMEOUT_SECONDS", 15),
                trivia_bankrupt_multiplier: p.f64("TRIVIA_BANKRUPT_MULTIPLIER", 2.0),
                trivia_cooldown_seconds: p.i64("TRIVIA_COOLDOWN_SECONDS", 21600),
                trivia_reward_per_question: p.i64("TRIVIA_REWARD_PER_QUESTION", 1),
                use_glicko: p.bool("USE_GLICKO", true),
                vanity_tax_rate: p.clamped_rate("VANITY_TAX_RATE", 0.1),
                wheel_banana_peel_est_ev: p.f64("WHEEL_BANANA_PEEL_EST_EV", -23.5),
                wheel_banana_peel_loss_max: p.i64("WHEEL_BANANA_PEEL_LOSS_MAX", 32),
                wheel_banana_peel_loss_min: p.i64("WHEEL_BANANA_PEEL_LOSS_MIN", 15),
                wheel_bankrupt_target_ev: p.f64("WHEEL_BANKRUPT_TARGET_EV", 12.0),
                wheel_blue_shell_est_ev: p.f64("WHEEL_BLUE_SHELL_EST_EV", -4.0),
                wheel_bomb_omb_est_ev: p.f64("WHEEL_BOMB_OMB_EST_EV", -52.5),
                wheel_bomb_omb_victim_count: p.i64("WHEEL_BOMB_OMB_VICTIM_COUNT", 3),
                wheel_bomb_omb_victim_loss_max: p.i64("WHEEL_BOMB_OMB_VICTIM_LOSS_MAX", 25),
                wheel_bomb_omb_victim_loss_min: p.i64("WHEEL_BOMB_OMB_VICTIM_LOSS_MIN", 10),
                wheel_chain_reaction_est_ev: p.f64("WHEEL_CHAIN_REACTION_EST_EV", -25.0),
                wheel_comeback_est_ev: p.f64("WHEEL_COMEBACK_EST_EV", 15.0),
                wheel_commune_est_ev: p.f64("WHEEL_COMMUNE_EST_EV", 8.0),
                wheel_cooldown_seconds: p.i64("WHEEL_COOLDOWN_SECONDS", 86400),
                wheel_discover_est_ev: p.f64("WHEEL_DISCOVER_EST_EV", 5.0),
                wheel_emergency_est_ev: p.f64("WHEEL_EMERGENCY_EST_EV", -25.0),
                wheel_extend_1_est_ev: p.f64("WHEEL_EXTEND_1_EST_EV", -10.0),
                wheel_extend_2_est_ev: p.f64("WHEEL_EXTEND_2_EST_EV", -20.0),
                wheel_golden_compound_est_ev: p.f64("WHEEL_GOLDEN_COMPOUND_EST_EV", 100.0),
                wheel_golden_dividend_est_ev: p.f64("WHEEL_GOLDEN_DIVIDEND_EST_EV", 10.0),
                wheel_golden_heist_est_ev: p.f64("WHEEL_GOLDEN_HEIST_EST_EV", 33.0),
                wheel_golden_hostile_takeover_est_ev: p
                    .f64("WHEEL_GOLDEN_HOSTILE_TAKEOVER_EST_EV", 35.0),
                wheel_golden_market_crash_est_ev: p.f64("WHEEL_GOLDEN_MARKET_CRASH_EST_EV", 35.0),
                wheel_golden_recession_est_ev: p.f64("WHEEL_GOLDEN_RECESSION_EST_EV", -200.0),
                wheel_golden_recession_mid_pct: p.f64("WHEEL_GOLDEN_RECESSION_MID_PCT", 0.035),
                wheel_golden_recession_mid_rank_end: p
                    .i64("WHEEL_GOLDEN_RECESSION_MID_RANK_END", 10),
                wheel_golden_recession_rest_pct: p.f64("WHEEL_GOLDEN_RECESSION_REST_PCT", 0.02),
                wheel_golden_recession_top_pct: p.f64("WHEEL_GOLDEN_RECESSION_TOP_PCT", 0.06),
                wheel_golden_target_ev: p.f64("WHEEL_GOLDEN_TARGET_EV", -82.5),
                wheel_golden_top_n: p.i64("WHEEL_GOLDEN_TOP_N", 3),
                wheel_golden_trickle_down_est_ev: p.f64("WHEEL_GOLDEN_TRICKLE_DOWN_EST_EV", 65.0),
                wheel_golden_trickle_down_pct_max: p.f64("WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MAX", 0.05),
                wheel_golden_trickle_down_pct_min: p.f64("WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MIN", 0.02),
                wheel_green_shell_est_ev: p.f64("WHEEL_GREEN_SHELL_EST_EV", 0.0),
                wheel_green_shell_steal_max: p.i64("WHEEL_GREEN_SHELL_STEAL_MAX", 33),
                wheel_green_shell_steal_min: p.i64("WHEEL_GREEN_SHELL_STEAL_MIN", 15),
                wheel_jailbreak_est_ev: p.f64("WHEEL_JAILBREAK_EST_EV", 10.0),
                wheel_lightning_bolt_est_ev: p.f64("WHEEL_LIGHTNING_BOLT_EST_EV", -65.0),
                wheel_lose_penalty_cooldown: p.i64("WHEEL_LOSE_PENALTY_COOLDOWN", 432000),
                wheel_red_shell_est_ev: p.f64("WHEEL_RED_SHELL_EST_EV", 0.0),
                wheel_target_ev: p.f64("WHEEL_TARGET_EV", -27.5),
                wheel_town_trial_est_ev: p.f64("WHEEL_TOWN_TRIAL_EST_EV", 0.0),
                white_bankrupt_stipend: p.i64("WHITE_BANKRUPT_STIPEND", 5),
                wrapped_min_bets: p.i64("WRAPPED_MIN_BETS", 3),
                wrapped_min_games: p.i64("WRAPPED_MIN_GAMES", 3),
                recent_match_penalty_weight: 230.0,
                pingedkevin_cost: p.i64("PINGEDASH_COST", 10),
                pingedkevin_cooldown_seconds: p.i64("PINGEDASH_COOLDOWN_SECONDS", 86_400),
            },
            migration,
            llm: LlmConfig {
                groq_api_key,
                cerebras_api_key,
                model,
                selected_api_key,
            },
            opendota_api_key: p.secret("OPENDOTA_API_KEY"),
        })
    }

    #[must_use]
    pub fn migration_settings(&self) -> MigrationSettings {
        self.migration.clone()
    }

    #[must_use]
    pub fn discord_token(&self) -> &DiscordToken {
        &self.runtime.token
    }

    /// Construct the shared OpenDota client exclusively from this immutable
    /// startup snapshot; the adapter never rereads process environment state.
    pub fn opendota_services(&self) -> Result<OpenDotaRuntimeServices, OpenDotaHttpBuildError> {
        OpenDotaRuntimeServices::from_production_settings(
            self.opendota_api_key
                .as_ref()
                .map(|secret| secret.expose().to_owned()),
            &self.values.enrichment_retry_delays,
        )
    }

    /// Map the Python composition-root inputs into Rust's concrete service
    /// graph without raw string retention or any additional environment read.
    #[must_use]
    pub fn service_container_options(
        &self,
        opendota: Option<Arc<OpenDotaRuntimeServices>>,
    ) -> ServiceContainerOptions {
        ServiceContainerOptions {
            admin_user_ids: self.identities.admin_user_ids.clone(),
            lobby_ready_threshold: self.values.lobby_ready_threshold,
            lobby_max_players: self.values.lobby_max_players,
            use_glicko: self.values.use_glicko,
            max_debt: self.values.max_debt,
            leverage_tiers: self.values.leverage_tiers.clone(),
            garnishment_percentage: self.values.garnishment_percentage,
            economy_events_enabled: self.values.economy_events_enabled,
            llm_api_key: self
                .llm
                .selected_api_key
                .as_ref()
                .filter(|secret| !secret.expose().is_empty())
                .map(|secret| secret.expose().to_owned()),
            ai_model: self.llm.model.clone(),
            ai_timeout_seconds: self.values.ai_timeout_seconds,
            ai_max_tokens: self.values.ai_max_tokens,
            production_ai_service: None,
            dig_llm_enabled: self.values.dig_llm_enabled,
            pet_enabled: self.channels.pet.is_some(),
            vanity_tax_rate: self.values.vanity_tax_rate,
            opendota,
        }
    }
}

struct Parser<'values>(&'values BTreeMap<&'static str, String>);

impl Parser<'_> {
    fn raw(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    fn i64(&self, name: &str, default: i64) -> i64 {
        self.raw(name)
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or(default)
    }

    fn optional_i64(&self, name: &str) -> Option<i64> {
        self.raw(name).and_then(|raw| raw.trim().parse().ok())
    }

    fn optional_channel(&self, name: &str) -> Option<i64> {
        self.raw(name)
            .filter(|raw| !raw.is_empty())
            .and_then(|raw| raw.trim().parse().ok())
    }

    fn f64(&self, name: &str, default: f64) -> f64 {
        self.raw(name)
            .and_then(|raw| raw.trim().parse().ok())
            .unwrap_or(default)
    }

    fn bool(&self, name: &str, default: bool) -> bool {
        self.raw(name).map_or(default, |raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    }

    fn clamped_rate(&self, name: &str, default: f64) -> f64 {
        let value = self.f64(name, default);
        // Python evaluates max(0.0, min(0.5, NaN)) as 0.5 because NaN never
        // replaces the first min() argument. `f64::clamp` would retain NaN.
        if value.is_nan() {
            0.5
        } else {
            value.clamp(0.0, 0.5)
        }
    }

    fn i64_list(&self, name: &str, default: &[i64]) -> Vec<i64> {
        let Some(raw) = self.raw(name) else {
            return default.to_vec();
        };
        raw.split(',')
            .filter(|part| !part.trim().is_empty())
            .map(|part| part.trim().parse())
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|_| default.to_vec())
    }

    fn admin_ids(&self) -> Vec<i64> {
        match self.raw("ADMIN_USER_IDS") {
            None | Some("") => Vec::new(),
            Some(raw) => self.parse_id_list(raw).unwrap_or_default(),
        }
    }

    fn tax_man_ids(&self) -> Vec<i64> {
        self.raw("TAX_MAN_USER_IDS")
            .or_else(|| self.raw("TAX_MEN_USER_IDS"))
            .and_then(|raw| self.parse_id_list(raw))
            .unwrap_or_default()
    }

    fn parse_id_list(&self, raw: &str) -> Option<Vec<i64>> {
        raw.split(',')
            .filter(|part| !part.trim().is_empty())
            .map(|part| part.trim().parse())
            .collect::<Result<Vec<_>, _>>()
            .ok()
    }

    fn secret(&self, name: &str) -> Option<Secret> {
        self.0.get(name).cloned().map(Secret::new)
    }
}

/// Maintained catalog of every environment key declared by Python's `config.py`.
///
/// Production uses this to take one immutable environment snapshot. A test
/// parses `config.py` and fails on any catalog drift.
pub fn config_py_env_keys() -> impl Iterator<Item = &'static str> {
    include_str!("config_env_catalog.txt")
        .lines()
        .filter(|line| !line.is_empty())
}

fn all_runtime_env_keys() -> Vec<&'static str> {
    let mut keys = config_py_env_keys().collect::<Vec<_>>();
    keys.extend([
        "CAMA_GATEWAY_RECONNECT_INITIAL_SECONDS",
        "CAMA_GATEWAY_RECONNECT_MAX_SECONDS",
        "GAMBA_SYNTHETIC_MEMBERS_ENABLED",
        "OPENDOTA_API_KEY",
        "RUST_CUTOVER_CANDIDATE",
    ]);
    keys
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::process::Command;

    use super::*;

    fn parse(values: &[(&str, &str)]) -> ApplicationConfig {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>();
        ApplicationConfig::from_lookup(|key| values.get(key).cloned()).expect("valid config")
    }

    #[test]
    fn defaults_and_derived_aliases_match_python() {
        let config = parse(&[("DISCORD_BOT_TOKEN", "token")]);
        assert_eq!(config.values.bet_lock_seconds, 1_200);
        assert_eq!(config.values.openskill_shuffle_chance, 0.02);
        assert_eq!(config.values.auto_spectator_bet_count, 10);
        assert_eq!(config.values.auto_spectator_bet_top_count, 5);
        assert_eq!(config.values.auto_spectator_bet_percentage, 0.01);
        assert_eq!(config.values.auto_spectator_bet_top_percentage, 0.02);
        assert_eq!(config.values.pingedash_cost, 10);
        assert_eq!(config.values.recent_match_penalty_weight, 230.0);
        assert!(!config.values.gamba_synthetic_members_enabled);
        assert_eq!(config.channels.gamba, None);
        assert_eq!(config.channels.mafia, MAFIA_CHANNEL_ID);
        assert_eq!(config.llm.model, DEFAULT_CEREBRAS_MODEL);
    }

    #[test]
    fn soft_avoid_defaults_to_five_hundred() {
        let config = parse(&[("DISCORD_BOT_TOKEN", "token")]);
        assert_eq!(config.values.shop_soft_avoid_cost, 500);
    }

    #[test]
    fn dota_match_rewards_ignore_environment_overrides() {
        let config = parse(&[
            ("DISCORD_BOT_TOKEN", "token"),
            ("JOPACOIN_WIN_REWARD", "3"),
            ("JOPACOIN_PER_GAME", "2"),
        ]);
        assert_eq!(config.values.jopacoin_win_reward, 10);
        assert_eq!(config.values.jopacoin_per_game, 5);
    }

    #[test]
    fn synthetic_gamba_members_are_explicitly_opt_in() {
        let config = parse(&[
            ("DISCORD_BOT_TOKEN", "token"),
            ("GAMBA_SYNTHETIC_MEMBERS_ENABLED", "true"),
        ]);
        assert!(config.values.gamba_synthetic_members_enabled);
    }

    #[test]
    fn test_new_player_exclusion_boost_defaults_to_five() {
        let config = parse(&[("DISCORD_BOT_TOKEN", "token")]);
        assert_eq!(config.values.new_player_exclusion_boost, 5);
        assert_eq!(config.values.off_role_flat_value_penalty, 100.0);
        assert_eq!(config.values.off_role_flat_penalty, 500.0);
        assert_eq!(config.values.soft_avoid_penalty, 160.0);
        assert_eq!(config.values.package_deal_penalty, 90.0);
        assert_eq!(config.values.package_deal_split_penalty, 90.0);
    }

    #[test]
    fn neon_dig_chance_defaults_and_overrides_are_preserved() {
        let default_config = parse(&[("DISCORD_BOT_TOKEN", "token")]);
        assert_eq!(default_config.values.neon_dig_chance, 0.12);

        let override_config = parse(&[("DISCORD_BOT_TOKEN", "token"), ("NEON_DIG_CHANCE", "0.27")]);
        assert_eq!(override_config.values.neon_dig_chance, 0.27);

        let raw_config = parse(&[("DISCORD_BOT_TOKEN", "token"), ("NEON_DIG_CHANCE", "nan")]);
        assert!(raw_config.values.neon_dig_chance.is_nan());
    }

    #[test]
    fn python_fail_soft_parsing_and_clamps_are_exact() {
        let config = parse(&[
            ("DISCORD_BOT_TOKEN", "discord-secret"),
            ("BET_LOCK_SECONDS", "bad"),
            ("AUTO_BLIND_ENABLED", "definitely"),
            ("LEVERAGE_TIERS", "2,bad,5"),
            ("ENRICHMENT_RETRY_DELAYS", ""),
            ("TIP_FEE_RATE", "9"),
            ("VANITY_TAX_RATE", "-1"),
            ("TIP_FEE_RATE", "nan"),
            ("LOBBY_CHANNEL_ID", " 42 "),
            ("GAMBA_CHANNEL_ID", " 84 "),
        ]);
        assert_eq!(config.values.bet_lock_seconds, 1_200);
        assert!(!config.values.auto_blind_enabled);
        assert_eq!(config.values.leverage_tiers, [2, 3, 5]);
        assert!(config.values.enrichment_retry_delays.is_empty());
        assert_eq!(config.values.tip_fee_rate, 0.5);
        assert_eq!(config.values.vanity_tax_rate, 0.0);
        assert_eq!(config.channels.lobby, Some(42));
        assert_eq!(config.channels.gamba, Some(84));
    }

    #[test]
    fn tax_alias_precedence_and_atomic_id_fallback_match_python() {
        let legacy = parse(&[("DISCORD_BOT_TOKEN", "token"), ("TAX_MEN_USER_IDS", "1, 2")]);
        assert_eq!(legacy.identities.tax_man_user_ids, [1, 2]);
        let canonical = parse(&[
            ("DISCORD_BOT_TOKEN", "token"),
            ("TAX_MAN_USER_IDS", ""),
            ("TAX_MEN_USER_IDS", "1, 2"),
            ("ADMIN_USER_IDS", "3,wat,4"),
        ]);
        assert!(canonical.identities.tax_man_user_ids.is_empty());
        assert!(canonical.identities.admin_user_ids.is_empty());
    }

    #[test]
    fn selected_llm_credential_is_provider_bound_and_redacted() {
        let config = parse(&[
            ("DISCORD_BOT_TOKEN", "discord-secret"),
            ("GROQ_API_KEY", "groq-secret"),
            ("CEREBRAS_API_KEY", "cerebras-secret"),
            ("AI_MODEL", "cerebras/gemma-4-31b"),
            ("OPENDOTA_API_KEY", "dota-secret"),
        ]);
        assert_eq!(
            config.llm.selected_api_key.as_ref().map(Secret::expose),
            Some("cerebras-secret")
        );
        let debugged = format!("{config:?}");
        for secret in [
            "discord-secret",
            "groq-secret",
            "cerebras-secret",
            "dota-secret",
        ] {
            assert!(!debugged.contains(secret));
        }
    }

    #[test]
    fn migration_settings_are_derived_from_the_same_snapshot() {
        let config = parse(&[
            ("DISCORD_BOT_TOKEN", "token"),
            ("STREAK_THRESHOLD", "9"),
            ("NEW_PLAYER_MMR_DISCOUNT", "700"),
            ("OPENSKILL_PERFORMANCE_STRENGTH", "0.25"),
        ]);
        let settings = config.migration_settings();
        assert_eq!(settings.streak_threshold, 9);
        assert_eq!(settings.new_player_mmr_discount, 700);
        assert_eq!(settings.openskill.performance_strength, 0.25);
    }

    #[test]
    fn concrete_service_options_consume_the_typed_snapshot() {
        let config = parse(&[
            ("DISCORD_BOT_TOKEN", "token"),
            ("ADMIN_USER_IDS", "10,20"),
            ("LOBBY_READY_THRESHOLD", "8"),
            ("LOBBY_MAX_PLAYERS", "18"),
            ("USE_GLICKO", "off"),
            ("MAX_DEBT", "700"),
            ("LEVERAGE_TIERS", "3,4"),
            ("GARNISHMENT_PERCENTAGE", "0.75"),
            ("ECONOMY_EVENTS_ENABLED", "false"),
            ("GROQ_API_KEY", "provider-secret"),
            ("AI_TIMEOUT_SECONDS", "19.5"),
            ("AI_MAX_TOKENS", "777"),
            ("DIG_LLM_ENABLED", "no"),
            ("PET_CHANNEL_ID", "42"),
            ("VANITY_TAX_RATE", "0.123456"),
        ]);
        let options = config.service_container_options(None);
        assert_eq!(options.admin_user_ids, [10, 20]);
        assert_eq!(options.lobby_ready_threshold, 8);
        assert_eq!(options.lobby_max_players, 18);
        assert!(!options.use_glicko);
        assert_eq!(options.max_debt, 700);
        assert_eq!(options.leverage_tiers, [3, 4]);
        assert_eq!(options.garnishment_percentage, 0.75);
        assert!(!options.economy_events_enabled);
        assert_eq!(options.llm_api_key.as_deref(), Some("provider-secret"));
        assert_eq!(options.ai_model, DEFAULT_GROQ_MODEL);
        assert_eq!(options.ai_timeout_seconds, 19.5);
        assert_eq!(options.ai_max_tokens, 777);
        assert!(!options.dig_llm_enabled);
        assert!(options.pet_enabled);
        assert_eq!(options.vanity_tax_rate, 0.123456);
        assert!(!format!("{options:?}").contains("provider-secret"));
    }

    #[test]
    fn test_default_garnishment_rate_uses_config() {
        let config = parse(&[("DISCORD_BOT_TOKEN", "token")]);
        assert_eq!(config.values.garnishment_percentage, 1.0);
        assert_eq!(
            config
                .service_container_options(None)
                .garnishment_percentage,
            1.0
        );
    }

    #[test]
    fn double_or_nothing_default_cost_matches_python() {
        let config = parse(&[("DISCORD_BOT_TOKEN", "token")]);
        assert_eq!(config.values.shop_double_or_nothing_cost, 50);
    }

    #[test]
    fn double_or_nothing_default_cooldown_matches_python() {
        let config = parse(&[("DISCORD_BOT_TOKEN", "token")]);
        assert_eq!(
            config.values.double_or_nothing_cooldown_seconds,
            30 * 86_400
        );
    }

    #[test]
    fn bot_composition_passes_only_the_selected_unified_llm_key() {
        let config = parse(&[
            ("DISCORD_BOT_TOKEN", "discord-secret"),
            ("GROQ_API_KEY", "groq-secret"),
            ("CEREBRAS_API_KEY", "cerebras-secret"),
            ("AI_MODEL", "groq/qwen/qwen3.6-27b"),
        ]);
        let options = config.service_container_options(None);
        assert_eq!(options.llm_api_key.as_deref(), Some("groq-secret"));
        assert!(!format!("{options:?}").contains("groq-secret"));
        assert!(!format!("{options:?}").contains("cerebras-secret"));
    }

    #[test]
    fn catalog_has_exactly_one_entry_for_every_config_py_environment_key() {
        let catalog = config_py_env_keys().collect::<BTreeSet<_>>();
        assert_eq!(catalog.len(), 214, "catalog entries must remain unique");
        let config_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config.py");
        let script = r#"
import ast, pathlib, sys
tree = ast.parse(pathlib.Path(sys.argv[1]).read_text())
parsers = {'_parse_int', '_parse_optional_int', '_parse_float', '_parse_bool', '_parse_int_list'}
keys = set()
for node in ast.walk(tree):
    if not isinstance(node, ast.Call) or not node.args:
        continue
    is_getenv = isinstance(node.func, ast.Attribute) and node.func.attr == 'getenv'
    is_parser = isinstance(node.func, ast.Name) and node.func.id in parsers
    if (is_getenv or is_parser) and isinstance(node.args[0], ast.Constant):
        keys.add(node.args[0].value)
print('\n'.join(sorted(keys)))
"#;
        let output = Command::new("python3")
            .args(["-c", script, config_path.to_str().expect("UTF-8 path")])
            .output()
            .expect("Python is available to the Rust test suite");
        assert!(output.status.success());
        let discovered = String::from_utf8(output.stdout)
            .expect("UTF-8 Python output")
            .lines()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            discovered,
            catalog.into_iter().map(str::to_owned).collect(),
            "config.py environment declarations and Rust catalog drifted"
        );
    }

    #[test]
    fn every_catalog_key_is_consumed_by_a_typed_configuration_field() {
        let config_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config.py");
        let script = r#"
import ast, pathlib, sys
tree = ast.parse(pathlib.Path(sys.argv[1]).read_text())
parsers = {'_parse_int', '_parse_optional_int', '_parse_float', '_parse_bool', '_parse_int_list'}
overrides = {}
for node in ast.walk(tree):
    if not isinstance(node, ast.Call) or not node.args:
        continue
    if not isinstance(node.func, ast.Name) or node.func.id not in parsers:
        continue
    if not isinstance(node.args[0], ast.Constant):
        continue
    key, parser = node.args[0].value, node.func.id
    if parser == '_parse_bool':
        default = ast.literal_eval(node.args[1])
        overrides[key] = 'false' if default else 'true'
    elif parser == '_parse_int':
        default = eval(compile(ast.Expression(node.args[1]), '<config>', 'eval'), {'__builtins__': {}}, {})
        overrides[key] = str(default + 12345)
    elif parser == '_parse_float':
        default = ast.literal_eval(node.args[1])
        overrides[key] = repr(default + 0.123456789)
    elif parser in {'_parse_optional_int', '_parse_int_list'}:
        overrides[key] = '987654321'
overrides.update({
    'DB_PATH': '/typed/config.db',
    'DISCORD_BOT_TOKEN': 'different-discord-secret',
    'ADMIN_USER_IDS': '987654321',
    'TAX_MAN_USER_IDS': '987654321',
    'TAX_MEN_USER_IDS': '987654321',
    'LOBBY_CHANNEL_ID': '987654321',
    'GAMBA_CHANNEL_ID': '987654321',
    'LOWSKILL_LOBBY_CHANNEL_ID': '987654321',
    'DIG_CHANNEL_ID': '987654321',
    'DUEL_CHANNEL_ID': '987654321',
    'PET_CHANNEL_ID': '987654321',
    'GROQ_API_KEY': 'different-groq-secret',
    'CEREBRAS_API_KEY': 'different-cerebras-secret',
    'AI_MODEL': 'other/provider-model',
})
print('\n'.join(f'{key}\t{value}' for key, value in sorted(overrides.items())))
"#;
        let output = Command::new("python3")
            .args(["-c", script, config_path.to_str().expect("UTF-8 path")])
            .output()
            .expect("Python is available to the Rust test suite");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output_text = String::from_utf8(output.stdout).expect("UTF-8 Python output");
        let overrides = output_text
            .lines()
            .map(|line| line.split_once('\t').expect("key and override"))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            overrides.keys().copied().collect::<BTreeSet<_>>(),
            config_py_env_keys().collect::<BTreeSet<_>>()
        );

        let baseline = parse(&[("DISCORD_BOT_TOKEN", "baseline-discord-secret")]);
        for (key, override_value) in overrides {
            let mut values = BTreeMap::from([(
                "DISCORD_BOT_TOKEN".to_owned(),
                "baseline-discord-secret".to_owned(),
            )]);
            values.insert(key.to_owned(), override_value.to_owned());
            let overridden = ApplicationConfig::from_lookup(|name| values.get(name).cloned())
                .expect("generated override remains valid");
            assert_ne!(
                overridden, baseline,
                "{key} was captured but did not reach a typed configuration field"
            );
        }
    }
}
