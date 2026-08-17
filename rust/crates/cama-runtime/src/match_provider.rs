//! Production `/shuffle` and `/record` provider.
//!
//! The provider is intentionally composed over the one live lobby handle. A
//! second lobby service would split reservations and scope locks even though
//! both instances point at the same SQLite file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::{
    AIService, FlavorEvent, FlavorTextService, GuildAiConfigPort, PlayerContext, PlayerContextPort,
};
use cama_app::betting_reminder_messaging::{
    AllowedMentions as ReminderAllowedMentions, BettingFlavorPort,
    BettingMode as ReminderBettingMode, MentionUsers, PendingBettingState,
    PotTotals as ReminderPotTotals, ReminderDestination, ReminderRequest,
    ReminderType as MessageReminderType, ShuffleMessageInfo, TopBettor, build_reminder,
};
use cama_app::betting_service::{
    AutomaticSpectatorConfig, WealthSnapshot, plan_automatic_spectator_bets,
};
use cama_app::dota_streak::{dota_streak_bonus, previous_game_date};
use cama_app::economy_event_service::EconomyEventConfig;
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::embeds::LobbyKind;
use cama_app::loan::{LoanPolicy, LoanService};
use cama_app::lobby_runtime::AMBIGUOUS_LOBBY_MESSAGE;
use cama_app::manashop_rework::{
    BuffService, HostileLossRequest as AppHostileLossRequest,
    HostileLossSettlement as AppHostileLossSettlement, ProtectionGateway,
};
use cama_app::match_recording::{GamesMilestone, RivalryRecord, WinStreakRecord};
use cama_app::match_voting_service::{MatchVotingService, MatchVotingServiceError};
use cama_app::neon_degen::{RatingHistorySnapshot, choose_notable_winner};
use cama_app::reminders::LobbyKind as ReminderLobbyKind;
use cama_app::service_container::PersistentVanityTaxService;
use cama_db::autobet_investments::AutobetInvestmentRepository;
use cama_db::betting_service_repository::{
    AutomaticBasisPointBetRequest, AutomaticBetOutcome, BetSettlementAdjustments,
    BettingServiceRepository, BlindBetCandidate, BlindBetOutcome, BlindBetPolicy,
    ConfiguredInvestmentRequest, SettlementResult,
};
use cama_db::core_repositories::{
    CoreGlickoUpdate, CoreMatchRecord, CoreOpenSkillUpdate, MatchPredictionSnapshot, MatchRecord,
    MatchRepository, NewCoreRatingHistory, NewPlayer, OrderedMap, ParticipantStatsUpdate,
    PlayerRepository, ShuffleInputs, StreakInsertDefaults,
};
use cama_db::dota_bet_seed_repository::{
    BettingMode as SeedBettingMode, BettingTeam, DotaBetSeedRepository, FirstGameLobby,
};
use cama_db::dota_streak_repository::{DotaStreakCredit, DotaStreakRepository};
use cama_db::gambling_stats_repository::{
    GamblingStatsPort, GamblingStatsRepository, GamblingStatsService,
};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::loan_repository::LoanRepository;
use cama_db::low_priority_repository::LowPriorityRepository;
use cama_db::manashop_rework_repository::ManashopRepository;
use cama_db::match_correction_repository::MatchCorrectionRepository;
use cama_db::match_recording_repository::{
    IncomeAwardCompensation, IncomeAwardReceipt, IncomeAwardRequest, MatchRecordingRepository,
};
use cama_db::match_runtime::{PendingMatchRecord, PendingMatchRepository, PendingMatchState};
use cama_db::match_voting::{MatchVotingRepository, PendingVoteKey};
use cama_db::moderation::{ModerationEventType, ModerationRepository};
use cama_db::package_deal_repository::PackageDealRepository;
use cama_db::pairings_repository::PairingsRepository;
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::rating_history_repository::RatingHistoryRepository;
use cama_db::shop_runtime::{
    HostileDestination, HostileLossRequest as DbHostileLossRequest, ShopRuntimeRepository,
    ShopRuntimeRepositoryError,
};
use cama_db::soft_avoid_repository::SoftAvoidRepository;
use cama_domain::discord_content::chunk_default_discord_content;
use cama_domain::economy_scaling::scale_minigame_jc_delta;
use cama_domain::formatting::{
    BettingDisplayOptions, JOPACOIN_EMOTE, ROLE_EMOJIS, ROLE_NAMES, format_betting_display,
};
use cama_domain::guild_config::GuildConfigStore;
use cama_domain::openskill::{CamaOpenSkillSystem, OpenSkillError, Rating as OpenSkillRating};
use cama_domain::openskill::{Player as OpenSkillPlayer, WinningTeam as OpenSkillWinningTeam};
use cama_domain::pet_evolution::PetActivity;
use cama_domain::player::Player;
use cama_domain::rate_limiter::RateLimiter;
use cama_domain::rating::{
    CamaRatingSystem, MatchUpdateOptions, RatingConfig, StreakAdjustment,
    TeamPlayer as GlickoTeamPlayer,
};
use cama_domain::region::{
    PlayerRegionInput, region_split_mismatches, resolve_region, summarize_region,
};
use cama_domain::shuffler::{
    BalancedShuffler, PackageDeal, PoolOptions, ShuffleConstraints, SoftAvoid,
};
use cama_domain::team::Team;
use cama_domain::team_balancing::TeamBalancingService;
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, NaiveDate, NaiveDateTime, Timelike, Utc,
    Weekday,
};
use rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::admin_provider::{
    AdminExtendBettingRequest, AdminExtendBettingResult, AdminMatchControl,
    AdminRoleBackfillResult, AdminSeedHeroGridRequest, AdminSeedHeroGridResult,
    CorrectionWinRewardControl, CorrectionWinRewardRequest, CorrectionWinRewardResult,
};
use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::enrichment_provider::{RecordedMatchDiscovery, RecordedMatchDiscoveryOutcome};
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::lobby_provider::{MatchLobbyPort, MatchLobbySnapshot};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};
use crate::reminder_provider::{ReminderDeliveryReport, ReminderHooks};

const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const SHUFFLE_RATE_LIMIT: usize = 2;
const RECORD_RATE_LIMIT: usize = 3;
const RATE_LIMIT_WINDOW: u64 = 30;
const SHUFFLE_LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const DISCORD_BLUE: u32 = 0x34_98_db;
const DISCORD_ORANGE: u32 = 0xe6_7e_22;
const PENDING_MATCH_EASTER_STREAKS_KEY: &str = "_rust_match_easter_streaks";

#[derive(Debug, Error)]
pub enum MatchProviderBuildError {
    #[error("invalid OpenSkill runtime configuration: {0}")]
    OpenSkill(#[from] OpenSkillError),
    #[error("invalid match runtime setting {0}")]
    InvalidConfig(&'static str),
}

/// Durable facts selected after Match commits its rating history. The
/// composition root adapts this request to the live JOPA-T/Neon delivery
/// service; Match owns the database read and Python-compatible winner policy.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchPostMatchDebriefRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub channel_id: Option<u64>,
    pub winner_id: Option<i64>,
    pub loser_id: Option<i64>,
    pub payout: i64,
    pub loss: i64,
    pub leverage: i64,
    pub rating_change: Option<f64>,
    pub expected_win_prob: Option<f64>,
}

/// Committed Match-side Neon candidates. Settlement owns the degen/balance
/// hooks; this adapter receives the remaining candidates so it can preserve
/// Python's games-milestone, streak-record, rivalry, then fallback ordering
/// and claim/replay the whole event atomically from the delivery boundary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatchEasterEggRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub channel_id: Option<u64>,
    pub games_milestones: Vec<GamesMilestone>,
    pub win_streak_records: Vec<WinStreakRecord>,
    pub rivalries_detected: Vec<RivalryRecord>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct PersistedWinStreakRecord {
    discord_id: i64,
    current_streak: i64,
    previous_best: i64,
}

fn persist_easter_streaks_in_state(state: &mut PendingMatchState, records: &[WinStreakRecord]) {
    let records = records
        .iter()
        .map(|record| PersistedWinStreakRecord {
            discord_id: record.discord_id,
            current_streak: record.current_streak,
            previous_best: record.previous_best,
        })
        .collect::<Vec<_>>();
    state.extra.insert(
        PENDING_MATCH_EASTER_STREAKS_KEY.to_owned(),
        json!({ "win_streak_records": records }),
    );
}

fn persisted_easter_streaks(
    state: &PendingMatchState,
) -> Result<Option<Vec<WinStreakRecord>>, String> {
    let Some(value) = state.extra.get(PENDING_MATCH_EASTER_STREAKS_KEY) else {
        return Ok(None);
    };
    let records = value
        .get("win_streak_records")
        .cloned()
        .ok_or_else(|| "persisted match Easter streak payload is missing records".to_owned())?;
    let records = serde_json::from_value::<Vec<PersistedWinStreakRecord>>(records)
        .map_err(|error| format!("invalid persisted match Easter streak payload: {error}"))?
        .into_iter()
        .map(|record| WinStreakRecord {
            discord_id: record.discord_id,
            current_streak: record.current_streak,
            previous_best: record.previous_best,
        })
        .collect();
    Ok(Some(records))
}

/// Settlement facts handed to Betting's Neon observer after Match has
/// committed the immutable bet settlement rows.  Match owns the source of
/// truth and this DTO deliberately contains no provider-local state, making
/// it safe to replay from READY recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchBetSettlementParticipant {
    pub discord_id: i64,
    pub amount: i64,
    pub leverage: i64,
    pub balance_after: i64,
    pub payout: i64,
    pub won: bool,
    pub refunded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchBetSettlementRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub channel_id: Option<u64>,
    pub participants: Vec<MatchBetSettlementParticipant>,
}

/// Narrow live seam for post-match JOPA-T/Neon presentation. Implementors
/// must make delivery idempotent by `match_id`; READY recovery may replay a
/// committed Match whose Discord presentation was interrupted.
#[async_trait]
pub trait MatchPostMatchDebriefPort: Send + Sync {
    async fn on_bet_settlement(&self, _request: MatchBetSettlementRequest) -> Result<(), String> {
        Ok(())
    }

    async fn on_match_easter_eggs(&self, _request: MatchEasterEggRequest) -> Result<(), String> {
        Ok(())
    }

    async fn on_post_match_debrief(
        &self,
        request: MatchPostMatchDebriefRequest,
    ) -> Result<(), String>;
}

/// Result returned by Match's persisted shuffle-message renderer after a
/// successful wager. Betting translates this into its own refresh-port type.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MatchWagerRefreshReport {
    pub attempted: usize,
    pub refreshed: usize,
    pub failures: Vec<String>,
}

#[derive(Clone)]
pub struct MatchRegistrationProvider {
    handler: Arc<MatchHandler>,
    correction_rewards: Arc<ProductionMatchRewardControl>,
    gateway_observer: Arc<MatchGatewayObserver>,
}

#[async_trait]
trait MatchReminderPort: Send + Sync {
    async fn notify_betting(
        &self,
        guild_id: i64,
        bet_lock_until: i64,
        pending_match_id: i64,
        lobby_kind: ReminderLobbyKind,
    ) -> Result<ReminderDeliveryReport, String>;
}

#[async_trait]
impl MatchReminderPort for ReminderHooks {
    async fn notify_betting(
        &self,
        guild_id: i64,
        bet_lock_until: i64,
        pending_match_id: i64,
        lobby_kind: ReminderLobbyKind,
    ) -> Result<ReminderDeliveryReport, String> {
        self.notify_betting(
            guild_id,
            bet_lock_until,
            Some(pending_match_id),
            Some(lobby_kind),
        )
        .await
    }
}

impl MatchRegistrationProvider {
    /// Compose the complete match extension over the admitted SQLite file,
    /// shared enrichment discovery handle, and existing live lobby runtime.
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        lobbies: MatchLobbyPort,
        recorded_match_discovery: Arc<dyn RecordedMatchDiscovery>,
        discord: Arc<dyn DiscordTransport>,
    ) -> Result<Self, MatchProviderBuildError> {
        let openskill = CamaOpenSkillSystem::with_config(config.migration.openskill.clone())?;
        openskill.calibrate_win_probability(0.5, None)?;
        let rating = CamaRatingSystem::with_config(
            config.values.initial_glicko_rd,
            0.06,
            RatingConfig {
                calibration_rd_threshold: config.values.calibration_rd_threshold,
                initial_rd: config.values.initial_glicko_rd,
                max_rating_swing_per_game: config.values.max_rating_swing_per_game,
                base_rating_delta_multiplier: config.values.base_rating_delta_multiplier,
                max_rd_contraction_per_game: config.values.max_rd_contraction_per_game,
                new_player_mmr_discount: config.migration.new_player_mmr_discount,
                rd_decay_constant: config.values.rd_decay_constant,
                rd_decay_grace_period_days: checked_i32(
                    config.values.rd_decay_grace_period_days,
                    "RD_DECAY_GRACE_PERIOD_DAYS",
                )?,
                streak_threshold: config.values.streak_threshold,
                streak_multiplier_per_game: config.values.streak_multiplier_per_game,
            },
        );
        if config.values.bet_lock_seconds < 0 {
            return Err(MatchProviderBuildError::InvalidConfig("BET_LOCK_SECONDS"));
        }
        let path = database_path.as_ref().to_path_buf();
        let economy = Arc::new(SqliteEconomyEventService::new(
            &path,
            match_economy_config(config),
        ));
        let correction_rewards = Arc::new(ProductionMatchRewardControl {
            database_path: path.clone(),
            garnishment_rate: config.values.garnishment_percentage,
            bankruptcy_kept_rate: config.values.bankruptcy_penalty_rate,
            vanity_tax_rate: config.values.vanity_tax_rate,
            minigame_scale: config.values.minigame_jc_delta_scale,
            vanity_tax: Arc::new(PersistentMatchVanityTax(vanity_tax)),
            economy: Arc::clone(&economy),
        });
        let handler = Arc::new(MatchHandler {
            pending: PendingMatchRepository::new(&path),
            players: PlayerRepository::new(&path),
            avoids: SoftAvoidRepository::new(&path),
            deals: PackageDealRepository::new(&path),
            low_priority: LowPriorityRepository::new(&path),
            bets: BettingServiceRepository::new(&path),
            rating_history: RatingHistoryRepository::new(&path),
            seeds: DotaBetSeedRepository::new(&path),
            rewards: Arc::clone(&correction_rewards),
            economy,
            database_path: path.clone(),
            config: MatchConfig::from_application(config),
            rating,
            openskill,
            lobbies,
            recorded_match_discovery,
            discord,
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new())),
            betting_tasks: Arc::new(Mutex::new(BTreeMap::new())),
            betting_reminders: Arc::new(OnceLock::new()),
            betting_flavor: Arc::new(RwLock::new(Arc::new(StaticBettingFlavor))),
            post_match_debrief: Arc::new(OnceLock::new()),
            finalizing_matches: Arc::new(Mutex::new(BTreeSet::new())),
            voting: MatchVotingService::new(MatchVotingRepository::new(&path)),
        });
        Ok(Self {
            gateway_observer: Arc::new(MatchGatewayObserver {
                handler: Arc::clone(&handler),
            }),
            handler,
            correction_rewards,
        })
    }

    /// Handle consumed by `/admin extendbetting` and `/admin seedherogrid`.
    #[must_use]
    pub fn admin_control(&self) -> Arc<dyn AdminMatchControl> {
        self.handler.clone()
    }

    /// Exact production win-reward policy shared by ordinary `/record` and
    /// durable `/admin correctmatch` recovery.
    #[must_use]
    pub fn correction_reward_control(&self) -> Arc<dyn CorrectionWinRewardControl> {
        self.correction_rewards.clone()
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        self.gateway_observer.clone()
    }

    /// Attach the live JOPA-T/Neon presentation adapter after all providers
    /// have been constructed. Keeping this a typed observer avoids a second
    /// Neon service and leaves Match's durable record path independent from
    /// Discord/AI availability.
    pub fn attach_post_match_debrief(
        &self,
        port: Arc<dyn MatchPostMatchDebriefPort>,
    ) -> Result<(), String> {
        self.handler
            .post_match_debrief
            .set(port)
            .map_err(|_| "match post-match debrief port was already attached".to_owned())
    }

    /// Refresh every persisted copy of a shuffle wager message. The embed and
    /// target de-duplication remain Match-owned; Betting only supplies the
    /// guild/pending-match identity after its atomic wager commits.
    pub async fn refresh_wager_message(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<MatchWagerRefreshReport, String> {
        self.handler
            .refresh_wager_message(guild_id, pending_match_id)
            .await
    }

    pub fn attach_reminder_hooks(&self, hooks: ReminderHooks) -> Result<(), String> {
        self.handler
            .betting_reminders
            .set(Arc::new(hooks))
            .map_err(|_| "match reminder hooks were already attached".to_owned())
    }

    /// Attach Betting's AI/persona adapter for final-warning and last-call
    /// reminder copy. Match always starts with the static fallback port, so a
    /// missing or disabled AI service still emits Python's visible flavor
    /// line; an attached adapter only changes how that line is produced.
    pub fn attach_betting_flavor(&self, flavor: Arc<dyn BettingFlavorPort>) -> Result<(), String> {
        *self
            .handler
            .betting_flavor
            .write()
            .map_err(|_| "match betting flavor lock was poisoned".to_owned())? = flavor;
        Ok(())
    }

    /// Reuse the live match provider's generation-owned betting reminder
    /// registry from `/draft` completion. The hook deliberately delegates to
    /// the same cancellation/replacement implementation used by `/shuffle`,
    /// `/record`, READY recovery, and `/admin extendbetting`.
    pub fn schedule_draft_betting_reminders(
        &self,
        pending: PendingMatchRecord,
    ) -> Result<(), String> {
        self.handler.schedule_betting_reminders(&pending, true);
        Ok(())
    }

    /// Rebuild the generation-owned reminder tasks after Draft publication
    /// recovery without replaying the immediate subscriber notification. The
    /// durable Draft job records the completion marker only after this
    /// synchronous cancel/replace operation returns.
    pub fn schedule_draft_betting_reminders_recovery(
        &self,
        pending: PendingMatchRecord,
    ) -> Result<(), String> {
        self.handler.schedule_betting_reminders(&pending, false);
        Ok(())
    }
}

/// Build Match's live betting-flavor adapter over the same SQLite player,
/// gambling-stat, loan, and guild-AI repositories used by Shop.  The returned
/// port is never absent: without an AI service it emits the static Python
/// fallback, while an attached AI service adds the persona/angle/context
/// payload and retains the same fallback on disabled/error/empty output.
pub fn production_betting_flavor(
    database_path: impl AsRef<Path>,
    config: &ApplicationConfig,
    ai_service: Option<Arc<AIService>>,
) -> Arc<dyn BettingFlavorPort> {
    let Some(ai_service) = ai_service else {
        return Arc::new(StaticBettingFlavor);
    };
    let path = database_path.as_ref().to_path_buf();
    let player_context: Arc<dyn PlayerContextPort> =
        Arc::new(ProductionBettingPlayerContext::new(&path));
    let guild_config: Arc<dyn GuildAiConfigPort> = Arc::new(ProductionBettingGuildAiConfig {
        repository: GuildConfigRepository::new(&path, config.values.ai_features_enabled),
    });
    Arc::new(FlavorTextService::new(ai_service, player_context).with_config(guild_config))
}

impl RegistrationProvider for MatchRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "shuffle".to_owned(),
            description: "Create balanced teams from lobby".to_owned(),
            options: shuffle_options(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "record".to_owned(),
            description: "Record a match result or abort the match".to_owned(),
            options: record_options(),
            handler: self.handler.clone(),
        })
    }
}

fn shuffle_options() -> Vec<CommandOptionSpec> {
    vec![
        choices(
            CommandOptionSpec::new("mode", "Team-shuffle mode", CommandOptionKind::String),
            &[
                ("Balanced (default)", "balanced"),
                ("Region Split (US West vs US East)", "region"),
            ],
        ),
        choices(
            CommandOptionSpec::new(
                "rating_system",
                "Rating system for team balancing (experimental)",
                CommandOptionKind::String,
            ),
            &[
                ("Glicko-2 (default)", "glicko"),
                ("OpenSkill (experimental)", "openskill"),
                ("Jopacoin Balance", "jopacoin"),
            ],
        ),
        choices(
            CommandOptionSpec::new(
                "lobby",
                "Lobby to shuffle (defaults to the lobby you're in)",
                CommandOptionKind::String,
            ),
            &[
                ("🍽️ All You Can Feed", "open"),
                ("🧀 Whine & Cheese", "lowskill"),
            ],
        ),
    ]
}

fn record_options() -> Vec<CommandOptionSpec> {
    vec![
        choices(
            CommandOptionSpec::new(
                "result",
                "Match result: Radiant won, Dire won, or Abort",
                CommandOptionKind::String,
            )
            .required(true),
            &[
                ("Radiant Won", "radiant"),
                ("Dire Won", "dire"),
                ("Abort Match", "abort"),
            ],
        ),
        CommandOptionSpec::new(
            "dotabuff_match_id",
            "Dotabuff match ID (optional)",
            CommandOptionKind::String,
        ),
    ]
}

fn choices(mut option: CommandOptionSpec, values: &[(&str, &str)]) -> CommandOptionSpec {
    option.choices = values
        .iter()
        .map(|(name, value)| CommandOptionChoice::String {
            name: (*name).to_owned(),
            value: (*value).to_owned(),
        })
        .collect();
    option
}

#[derive(Clone)]
struct ProductionMatchRewardControl {
    database_path: PathBuf,
    garnishment_rate: f64,
    bankruptcy_kept_rate: f64,
    vanity_tax_rate: f64,
    minigame_scale: f64,
    vanity_tax: Arc<dyn MatchVanityTaxSource>,
    economy: Arc<SqliteEconomyEventService>,
}

trait MatchVanityTaxSource: Send + Sync {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64>;
}

struct PersistentMatchVanityTax(Arc<PersistentVanityTaxService>);

impl MatchVanityTaxSource for PersistentMatchVanityTax {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64> {
        self.0.taxable_ids(guild_id)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MatchWinRewardOutcome {
    receipt: IncomeAwardReceipt,
    communion_bonus: i64,
    blood_pact_skim: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct GeneratedRewardOutcome {
    receipt: IncomeAwardReceipt,
    blood_pact_skim: i64,
}

impl GeneratedRewardOutcome {
    const fn net(self) -> i64 {
        self.receipt.net.saturating_sub(self.blood_pact_skim)
    }

    const fn balance_delta(self) -> i64 {
        self.receipt
            .balance_delta
            .saturating_sub(self.blood_pact_skim)
    }

    const fn compensatable_net(self) -> i64 {
        if self.receipt.applied { self.net() } else { 0 }
    }
}

#[derive(Clone, Copy)]
struct GeneratedRewardBatch<'a> {
    guild_id: i64,
    player_ids: &'a [i64],
    gross: i64,
    apply_bankruptcy_penalty: bool,
    /// Python parity: only `_award_with_penalties` batches (win, exclusion,
    /// streaming) carry vanity-tax rates; `award_participation` (including
    /// the bomb-pot bonus) never does.
    apply_vanity_tax: bool,
    source: &'a str,
    related_type: &'a str,
    related_id: i64,
    reason: &'a str,
    event_nonce_tag: u64,
}

impl MatchWinRewardOutcome {
    const fn snapshot_balance_delta(self) -> i64 {
        self.receipt
            .balance_delta
            .saturating_add(self.communion_bonus)
            .saturating_sub(self.blood_pact_skim)
    }

    const fn compensatable_net(self) -> i64 {
        if self.receipt.applied {
            self.receipt
                .net
                .saturating_add(self.communion_bonus)
                .saturating_sub(self.blood_pact_skim)
        } else {
            0
        }
    }
}

impl ProductionMatchRewardControl {
    fn apply_bet_blood_pacts(
        &self,
        guild_id: i64,
        match_id: i64,
        settlement: &SettlementResult,
    ) -> Result<BTreeMap<i64, i64>, String> {
        let repository = BettingServiceRepository::new(&self.database_path);
        let recovered = repository
            .match_blood_pact_skims(match_id, Some(guild_id))
            .map_err(|error| error.to_string())?;
        if !recovered.is_empty() {
            return Ok(recovered);
        }
        let mut profits = BTreeMap::<i64, i64>::new();
        for winner in &settlement.winners {
            profits
                .entry(winner.discord_id)
                .and_modify(|profit| {
                    *profit = profit
                        .saturating_add(winner.payout)
                        .saturating_sub(winner.effective_amount);
                })
                .or_insert_with(|| winner.payout.saturating_sub(winner.effective_amount));
        }
        for loser in &settlement.losers {
            if let Some(profit) = profits.get_mut(&loser.discord_id) {
                *profit = profit.saturating_sub(loser.effective_amount);
            }
        }
        let now = unix_seconds();
        let target_ids = profits.keys().copied().collect::<Vec<_>>();
        let targets = BuffService::new(ManashopRepository::new(&self.database_path), now)
            .blood_pact_targets(&target_ids, guild_id)
            .unwrap_or_default();
        let mut skims = BTreeMap::new();
        for (discord_id, profit) in profits {
            let net_profit = profit
                .saturating_sub(
                    settlement
                        .bankruptcy_penalties
                        .get(&discord_id)
                        .copied()
                        .unwrap_or_default(),
                )
                .saturating_sub(
                    settlement
                        .vanity_taxes
                        .get(&discord_id)
                        .copied()
                        .unwrap_or_default(),
                );
            if net_profit <= 0 || !targets.contains(&discord_id) {
                continue;
            }
            let skim = self
                .apply_bet_blood_pact(discord_id, guild_id, net_profit, match_id, now)
                .unwrap_or_default();
            if skim > 0 {
                skims.insert(discord_id, skim);
            }
        }
        Ok(skims)
    }

    fn award_generated_batch(
        &self,
        batch: GeneratedRewardBatch<'_>,
    ) -> Result<BTreeMap<i64, GeneratedRewardOutcome>, String> {
        if batch.player_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let now = unix_seconds();
        let buff_service = BuffService::new(ManashopRepository::new(&self.database_path), now)
            .with_minigame_scale(self.minigame_scale);
        let pact_targets = buff_service
            .blood_pact_targets(batch.player_ids, batch.guild_id)
            .map_err(|error| error.to_string())?;
        let vanity_taxable_ids = self.vanity_tax.taxable_ids(batch.guild_id);
        let requests = batch
            .player_ids
            .iter()
            .map(|discord_id| IncomeAwardRequest {
                discord_id: *discord_id,
                gross: batch.gross,
                garnishment_rate: self.garnishment_rate,
                apply_bankruptcy_penalty: batch.apply_bankruptcy_penalty,
                bankruptcy_kept_rate: self.bankruptcy_kept_rate,
                vanity_tax_rate: if batch.apply_vanity_tax
                    && vanity_taxable_ids.contains(discord_id)
                {
                    self.vanity_tax_rate
                } else {
                    0.0
                },
                decrement_bankruptcy_penalty: false,
                source: batch.source,
                related_type: Some(batch.related_type),
                related_id: Some(batch.related_id),
                reason: batch.reason,
                exact_once: true,
            })
            .collect::<Vec<_>>();
        let recorder = MatchRecordingRepository::new(&self.database_path);
        let mut outcomes = BTreeMap::new();
        if pact_targets.is_empty() {
            for receipt in recorder
                .credit_income_awards_atomic(Some(batch.guild_id), &requests)
                .map_err(|error| error.to_string())?
            {
                outcomes.insert(
                    receipt.discord_id,
                    GeneratedRewardOutcome {
                        receipt,
                        blood_pact_skim: 0,
                    },
                );
            }
            return Ok(outcomes);
        }

        // Python evaluates these one at a time when a pact can transfer into
        // another batch participant and affect that participant's subsequent
        // live debt/garnishment check.
        for request in requests {
            let receipt = recorder
                .credit_income_awards_atomic(Some(batch.guild_id), &[request])
                .map_err(|error| error.to_string())?
                .into_iter()
                .next()
                .ok_or_else(|| "generated award returned no receipt".to_owned())?;
            let skim = if pact_targets.contains(&request.discord_id) {
                self.apply_attributed_blood_pact(
                    request.discord_id,
                    batch.guild_id,
                    request.gross,
                    format!(
                        "match-reward-blood-pact:{}:{}:{}:{}:{}",
                        batch.related_type,
                        batch.related_id,
                        batch.source,
                        batch.event_nonce_tag,
                        request.discord_id
                    ),
                    now,
                )
                .unwrap_or_default()
            } else {
                0
            };
            outcomes.insert(
                request.discord_id,
                GeneratedRewardOutcome {
                    receipt,
                    blood_pact_skim: skim,
                },
            );
        }
        Ok(outcomes)
    }

    fn apply_attributed_blood_pact(
        &self,
        discord_id: i64,
        guild_id: i64,
        earning: i64,
        event_key: String,
        now: i64,
    ) -> Result<i64, String> {
        if earning <= 0 {
            return Ok(0);
        }
        if let Some(existing) = ShopRuntimeRepository::new(&self.database_path)
            .hostile_loss_event(guild_id, discord_id, &event_key)
            .map_err(|error| error.to_string())?
        {
            return Ok(existing.applied);
        }
        let service = BuffService::new(ManashopRepository::new(&self.database_path), now)
            .with_minigame_scale(self.minigame_scale);
        let Some(claim) = service
            .claim_blood_pact_skim(discord_id, guild_id, earning)
            .map_err(|error| error.to_string())?
        else {
            return Ok(0);
        };
        let mut amount = scale_minigame_jc_delta(claim.amount as f64, self.minigame_scale);
        if amount > claim.amount {
            let extra = service
                .repository()
                .claim_blood_pact_extra_atomic(claim.buff_id, amount - claim.amount)
                .map_err(|error| error.to_string())?;
            amount = claim.amount.saturating_add(extra);
        }
        let mut gateway = MatchProtectionGateway {
            repository: ShopRuntimeRepository::new(&self.database_path),
            now,
            mana_date: pacific_mana_day(now)?,
        };
        let settlement = gateway.apply_hostile_loss(AppHostileLossRequest {
            target_id: discord_id,
            guild_id,
            attempted_loss: amount,
            kind: "blood_pact",
            actor_id: claim.skimmer_id,
            event_key,
            destination: "player",
            recipient_id: claim.skimmer_id,
            clamp_to_balance: false,
        });
        let Ok(settlement) = settlement else {
            service
                .repository()
                .revert_blood_pact_skim(claim.buff_id, amount)
                .map_err(|error| error.to_string())?;
            return Ok(0);
        };
        if settlement.applied_loss < amount {
            service
                .repository()
                .revert_blood_pact_skim(claim.buff_id, amount - settlement.applied_loss)
                .map_err(|error| error.to_string())?;
        }
        Ok(settlement.applied_loss)
    }

    fn apply_bet_blood_pact(
        &self,
        discord_id: i64,
        guild_id: i64,
        earning: i64,
        match_id: i64,
        now: i64,
    ) -> Result<i64, String> {
        self.apply_attributed_blood_pact(
            discord_id,
            guild_id,
            earning,
            format!("dota-bet-blood-pact:{match_id}:{discord_id}"),
            now,
        )
    }

    fn award_match_win(
        &self,
        request: CorrectionWinRewardRequest,
    ) -> Result<MatchWinRewardOutcome, String> {
        let vanity_tax_rate = if self
            .vanity_tax
            .taxable_ids(request.guild_id)
            .contains(&request.discord_id)
        {
            self.vanity_tax_rate
        } else {
            0.0
        };
        let award = IncomeAwardRequest {
            discord_id: request.discord_id,
            gross: request.gross_reward,
            garnishment_rate: self.garnishment_rate,
            apply_bankruptcy_penalty: true,
            bankruptcy_kept_rate: self.bankruptcy_kept_rate,
            vanity_tax_rate,
            decrement_bankruptcy_penalty: true,
            source: "match_win_bonus",
            related_type: Some("match_win_bonus"),
            related_id: Some(request.match_id),
            reason: "match win bonus",
            exact_once: true,
        };
        let recorder = MatchRecordingRepository::new(&self.database_path);
        let receipt = recorder
            .credit_income_awards_atomic(Some(request.guild_id), &[award])
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "match win award returned no receipt".to_owned())?;

        // Sanctuary's match-win income bonus is retired in the live Python
        // service and therefore intentionally contributes zero here.
        let recovered_communion = if receipt.applied {
            0
        } else {
            recorder
                .match_win_communion_bonus(
                    Some(request.guild_id),
                    request.match_id,
                    request.discord_id,
                )
                .unwrap_or_default()
        };
        let now = unix_seconds();
        let manashop = ManashopRepository::new(&self.database_path);
        let communion = manashop
            .active_for(
                request.discord_id,
                Some(request.guild_id),
                "communion_blessing",
                now,
            )
            .unwrap_or_default()
            .into_iter()
            .next();
        let credited_communion = if let Some(blessing) = communion {
            let raw = ((request.gross_reward as f64 * 0.10) as i64).max(1);
            let scaled = scale_minigame_jc_delta(raw as f64, self.minigame_scale);
            let adjusted = self
                .economy
                .adjust_reward_at(request.guild_id, scaled, now)
                .unwrap_or(scaled);
            if BettingServiceRepository::new(&self.database_path)
                .consume_and_credit_match_win_atomic(
                    blessing.id,
                    request.discord_id,
                    Some(request.guild_id),
                    adjusted,
                    request.match_id,
                )
                .unwrap_or(false)
            {
                adjusted
            } else {
                0
            }
        } else {
            0
        };
        let communion_bonus = recovered_communion.saturating_add(credited_communion);

        let earning = receipt.net.saturating_add(communion_bonus);
        let blood_pact_skim = recorder
            .match_win_blood_pact_skim(Some(request.guild_id), request.match_id, request.discord_id)
            .unwrap_or_default()
            .unwrap_or_else(|| {
                self.apply_attributed_blood_pact(
                    request.discord_id,
                    request.guild_id,
                    earning,
                    format!(
                        "match-win-blood-pact:{}:{}",
                        request.match_id, request.discord_id
                    ),
                    now,
                )
                .unwrap_or_default()
            });
        Ok(MatchWinRewardOutcome {
            receipt,
            communion_bonus,
            blood_pact_skim,
        })
    }
}

impl CorrectionWinRewardControl for ProductionMatchRewardControl {
    fn award_match_win_bonus(
        &self,
        request: CorrectionWinRewardRequest,
    ) -> Result<CorrectionWinRewardResult, String> {
        let outcome = self.award_match_win(request)?;
        Ok(CorrectionWinRewardResult {
            snapshot_balance_delta: outcome.snapshot_balance_delta(),
        })
    }
}

struct MatchProtectionGateway {
    repository: ShopRuntimeRepository,
    now: i64,
    mana_date: String,
}

impl ProtectionGateway for MatchProtectionGateway {
    type Error = ShopRuntimeRepositoryError;

    fn apply_hostile_loss(
        &mut self,
        request: AppHostileLossRequest,
    ) -> Result<AppHostileLossSettlement, Self::Error> {
        let destination = match request.destination {
            "burn" => HostileDestination::Burn,
            "reserve" => HostileDestination::Reserve,
            _ => HostileDestination::Player,
        };
        let settlement = self.repository.apply_hostile_loss(&DbHostileLossRequest {
            victim_id: request.target_id,
            guild_id: request.guild_id,
            requested: request.attempted_loss,
            kind: request.kind.to_owned(),
            actor_id: Some(request.actor_id),
            event_key: request.event_key,
            destination,
            recipient_id: (destination == HostileDestination::Player)
                .then_some(request.recipient_id),
            clamp_to_balance: request.clamp_to_balance,
            min_balance: None,
            protect_self: false,
            metadata: json!({
                "kind": request.kind,
                "attempted_loss": request.attempted_loss,
                "destination": request.destination,
                "recipient_id": request.recipient_id,
            }),
            occurred_at: self.now,
            mana_date: self.mana_date.clone(),
        })?;
        Ok(AppHostileLossSettlement {
            attempted_loss: settlement.attempted,
            absorbed_amount: settlement.absorbed,
            applied_loss: settlement.applied,
        })
    }
}

fn match_economy_config(config: &ApplicationConfig) -> EconomyEventConfig {
    let values = &config.values;
    EconomyEventConfig {
        enabled: values.economy_events_enabled,
        normal_annual_rate: values.economy_normal_annual_rate,
        inflation_ceiling: values.economy_inflation_ceiling,
        lookback_days: values.economy_event_lookback_days,
        max_reserve_burn_pct: values.economy_event_max_reserve_burn_pct,
        max_wallet_burn_pct: values.economy_event_max_wallet_burn_pct,
        trigger_hour_local: u8::try_from(values.economy_event_trigger_hour_local.clamp(0, 23))
            .unwrap_or(10),
    }
    .normalized()
}

fn pacific_mana_day(timestamp: i64) -> Result<String, String> {
    let utc = DateTime::<Utc>::from_timestamp(timestamp, 0)
        .ok_or_else(|| format!("invalid Unix timestamp {timestamp}"))?;
    let year = utc.year();
    let nth_weekday = |month: u32, weekday: Weekday, occurrence: u32| -> NaiveDate {
        let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid fixed month");
        let delta = (7 + i64::from(weekday.num_days_from_sunday())
            - i64::from(first.weekday().num_days_from_sunday()))
            % 7;
        first + ChronoDuration::days(delta + 7 * i64::from(occurrence.saturating_sub(1)))
    };
    let start = nth_weekday(3, Weekday::Sun, 2)
        .and_hms_opt(10, 0, 0)
        .expect("valid DST transition")
        .and_utc()
        .timestamp();
    let end = nth_weekday(11, Weekday::Sun, 1)
        .and_hms_opt(9, 0, 0)
        .expect("valid DST transition")
        .and_utc()
        .timestamp();
    let offset = if (start..end).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    };
    let local = DateTime::<Utc>::from_timestamp(timestamp.saturating_add(offset), 0)
        .ok_or_else(|| format!("invalid Pacific timestamp {timestamp}"))?
        .naive_utc();
    let effective = if local.hour() < 4 {
        local - ChronoDuration::days(1)
    } else {
        local
    };
    Ok(effective.format("%Y-%m-%d").to_string())
}

#[derive(Clone, Debug)]
struct MatchConfig {
    admin_user_ids: BTreeSet<i64>,
    bet_lock_seconds: i64,
    bomb_pot_chance: f64,
    bomb_pot_ante: i64,
    bomb_pot_blind_percentage: f64,
    bomb_pot_participation_bonus: i64,
    auto_blind_enabled: bool,
    auto_blind_threshold: i64,
    auto_blind_percentage: f64,
    auto_spectator_bet_enabled: bool,
    auto_spectator_bet_count: usize,
    auto_spectator_bet_percentage: f64,
    auto_spectator_bet_top_count: usize,
    auto_spectator_bet_top_percentage: f64,
    max_debt: i64,
    first_game_pool_daily_amount: i64,
    dota_bet_seed_amount: i64,
    openskill_shuffle_chance: f64,
    bankruptcy_kept_rate: f64,
    house_payout_multiplier: f64,
    vanity_tax_rate: f64,
    loan_cooldown_seconds: i64,
    loan_max_amount: i64,
    loan_fee_rate: f64,
    streaming_bonus: i64,
    jopacoin_min_bet: i64,
    jopacoin_win_reward: i64,
    jopacoin_per_game: i64,
    jopacoin_exclusion_reward: i64,
    off_role_multiplier: f64,
    off_role_flat_value_penalty: f64,
    off_role_flat_penalty: f64,
    role_matchup_delta_weight: f64,
    exclusion_penalty_weight: f64,
    rd_priority_weight: f64,
    recent_match_penalty_weight: f64,
    soft_avoid_penalty: f64,
    package_deal_penalty: f64,
    package_deal_split_penalty: f64,
    rating_spread_divisor: f64,
    region_split_penalty: f64,
    bet_reminder_offsets: Vec<i64>,
    bet_last_call_offset: i64,
}

impl MatchConfig {
    fn from_application(config: &ApplicationConfig) -> Self {
        Self {
            admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            bet_lock_seconds: config.values.bet_lock_seconds,
            bomb_pot_chance: config.values.bomb_pot_chance,
            bomb_pot_ante: config.values.bomb_pot_ante,
            bomb_pot_blind_percentage: config.values.bomb_pot_blind_percentage,
            bomb_pot_participation_bonus: config.values.bomb_pot_participation_bonus,
            auto_blind_enabled: config.values.auto_blind_enabled,
            auto_blind_threshold: config.values.auto_blind_threshold,
            auto_blind_percentage: config.values.auto_blind_percentage,
            auto_spectator_bet_enabled: config.values.auto_spectator_bet_enabled,
            auto_spectator_bet_count: usize::try_from(config.values.auto_spectator_bet_count)
                .unwrap_or_default(),
            auto_spectator_bet_percentage: config.values.auto_spectator_bet_percentage,
            auto_spectator_bet_top_count: usize::try_from(
                config.values.auto_spectator_bet_top_count,
            )
            .unwrap_or_default(),
            auto_spectator_bet_top_percentage: config.values.auto_spectator_bet_top_percentage,
            max_debt: config.values.max_debt,
            first_game_pool_daily_amount: config.values.first_game_pool_daily_amount,
            dota_bet_seed_amount: config.values.dota_bet_seed_amount,
            openskill_shuffle_chance: config.values.openskill_shuffle_chance,
            bankruptcy_kept_rate: config.values.bankruptcy_penalty_rate,
            house_payout_multiplier: config.values.house_payout_multiplier,
            vanity_tax_rate: config.values.vanity_tax_rate,
            loan_cooldown_seconds: config.values.loan_cooldown_seconds,
            loan_max_amount: config.values.loan_max_amount,
            loan_fee_rate: config.values.loan_fee_rate,
            streaming_bonus: config.values.streaming_bonus,
            jopacoin_min_bet: config.values.jopacoin_min_bet,
            jopacoin_win_reward: config.values.jopacoin_win_reward,
            jopacoin_per_game: config.values.jopacoin_per_game,
            jopacoin_exclusion_reward: config.values.jopacoin_exclusion_reward,
            off_role_multiplier: config.values.off_role_multiplier,
            off_role_flat_value_penalty: config.values.off_role_flat_value_penalty,
            off_role_flat_penalty: config.values.off_role_flat_penalty,
            role_matchup_delta_weight: config.values.role_matchup_delta_weight,
            exclusion_penalty_weight: config.values.exclusion_penalty_weight,
            rd_priority_weight: config.values.rd_priority_weight,
            recent_match_penalty_weight: config.values.recent_match_penalty_weight,
            soft_avoid_penalty: config.values.soft_avoid_penalty,
            package_deal_penalty: config.values.package_deal_penalty,
            package_deal_split_penalty: config.values.package_deal_split_penalty,
            rating_spread_divisor: config.values.rating_spread_divisor,
            region_split_penalty: config.values.region_split_penalty,
            bet_reminder_offsets: config.values.bet_reminder_offsets.clone(),
            bet_last_call_offset: config.values.bet_last_call_offset,
        }
    }
}

#[derive(Clone)]
struct MatchHandler {
    database_path: PathBuf,
    pending: PendingMatchRepository,
    players: PlayerRepository,
    avoids: SoftAvoidRepository,
    deals: PackageDealRepository,
    low_priority: LowPriorityRepository,
    bets: BettingServiceRepository,
    rating_history: RatingHistoryRepository,
    seeds: DotaBetSeedRepository,
    rewards: Arc<ProductionMatchRewardControl>,
    economy: Arc<SqliteEconomyEventService>,
    config: MatchConfig,
    rating: CamaRatingSystem,
    openskill: CamaOpenSkillSystem,
    lobbies: MatchLobbyPort,
    recorded_match_discovery: Arc<dyn RecordedMatchDiscovery>,
    discord: Arc<dyn DiscordTransport>,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    betting_tasks: BettingTaskRegistry,
    betting_reminders: Arc<OnceLock<Arc<dyn MatchReminderPort>>>,
    betting_flavor: Arc<RwLock<Arc<dyn BettingFlavorPort>>>,
    post_match_debrief: Arc<OnceLock<Arc<dyn MatchPostMatchDebriefPort>>>,
    finalizing_matches: Arc<Mutex<BTreeSet<(i64, i64)>>>,
    voting: MatchVotingService<MatchVotingRepository>,
}

type BettingTaskRegistry = Arc<Mutex<BTreeMap<(i64, i64), Vec<JoinHandle<()>>>>>;

struct MatchGatewayObserver {
    handler: Arc<MatchHandler>,
}

#[async_trait]
impl GatewayEventObserver for MatchGatewayObserver {
    fn name(&self) -> &'static str {
        "match-pending-recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let mut report = ReadyRecoveryReport::empty(self.name(), context.guild_ids().len());
        for guild_id in context.guild_ids() {
            let Ok(guild_id_i64) = i64::try_from(*guild_id) else {
                report.failures.push(ReadyRecoveryFailure {
                    guild_id: *guild_id,
                    message: "guild ID exceeds SQLite INTEGER range".to_owned(),
                });
                continue;
            };
            let repository = self.handler.pending.clone();
            let pending =
                match tokio::task::spawn_blocking(move || repository.pending_matches(guild_id_i64))
                    .await
                {
                    Ok(Ok(pending)) => pending,
                    Ok(Err(error)) => {
                        report.failures.push(ReadyRecoveryFailure {
                            guild_id: *guild_id,
                            message: error.to_string(),
                        });
                        continue;
                    }
                    Err(error) => {
                        report.failures.push(ReadyRecoveryFailure {
                            guild_id: *guild_id,
                            message: format!("pending-match recovery task failed: {error}"),
                        });
                        continue;
                    }
                };
            let mut failed = false;
            for match_state in pending {
                match self.handler.recover_pending_match(match_state).await {
                    Ok(()) => {}
                    Err(message) => {
                        failed = true;
                        report.failures.push(ReadyRecoveryFailure {
                            guild_id: *guild_id,
                            message,
                        });
                    }
                }
            }
            if !failed {
                report.guilds_refreshed += 1;
            }
        }
        report
    }
}

#[derive(Clone, Debug)]
struct MatchCommandContext {
    user_id: i64,
    guild_id: i64,
    channel_id: Option<u64>,
    member_permissions: Option<u64>,
    options: Vec<InteractionOption>,
}

#[derive(Clone, Debug)]
struct PrepareShuffleRequest {
    guild_id: i64,
    lobby_kind: LobbyKind,
    player_ids: Vec<i64>,
    excluded_conditional_ids: Vec<i64>,
    lobby_wait_minutes: HashMap<i64, i64>,
    rating_system: String,
    shuffle_mode: String,
    shuffle_timestamp: i64,
    is_bomb_pot: bool,
}

#[derive(Clone, Debug)]
struct PreparedShuffle {
    pending: PendingMatchRecord,
}

#[derive(Clone, Debug, PartialEq)]
struct RecordedMatch {
    match_id: i64,
    jc_lines: Vec<String>,
    bet_settlement: Option<MatchBetSettlementRequest>,
    easter_eggs: MatchEasterEggRequest,
    debrief: Option<MatchPostMatchDebriefRequest>,
}

impl PreparedShuffle {
    fn matched_ids(&self) -> BTreeSet<i64> {
        self.pending.state.participant_ids()
    }
}

#[async_trait]
impl InteractionHandler for MatchHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            name,
            user_id,
            guild_id,
            channel_id,
            member_permissions,
            options,
            ..
        } = request
        else {
            return Err("match extension has no component, modal, or autocomplete routes".into());
        };
        let context = MatchCommandContext {
            user_id: signed_id(user_id, "user")?,
            guild_id: guild_id
                .map(|id| signed_id(id, "guild"))
                .transpose()?
                .unwrap_or_default(),
            channel_id,
            member_permissions,
            options,
        };
        let (limit, scope) = match name.as_str() {
            "shuffle" => (SHUFFLE_RATE_LIMIT, "shuffle"),
            "record" => (RECORD_RATE_LIMIT, "record"),
            _ => return Err(format!("match handler received command {name:?}").into()),
        };
        if let Some(retry_after) = self.rate_limit(scope, &context, limit)? {
            responder
                .respond(
                    InteractionResponse::message(format!(
                        "⏳ Please wait {retry_after}s before using `/{scope}` again."
                    ))
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        match name.as_str() {
            "shuffle" => self.handle_shuffle(context, responder).await,
            "record" => self.handle_record(context, responder).await,
            _ => unreachable!(),
        }
    }
}

impl MatchHandler {
    async fn recover_pending_match(&self, pending: PendingMatchRecord) -> Result<(), String> {
        let recorded = MatchRepository::new(&self.database_path)
            .match_id_for_pending_match(pending.guild_id, pending.pending_match_id)
            .map_err(|error| error.to_string())?;
        if let Some(match_id) = recorded {
            // A committed core row means `/record` crossed the durable point.
            // Re-enter the idempotent core/money path to finish any stranded
            // bet, reward, streak, or loan phase, then remove the obsolete
            // pending row.  Public presentation is intentionally not replayed:
            // Python also treats it as best-effort after the durable clear.
            let winner = MatchRepository::new(&self.database_path)
                .get_match(match_id, Some(pending.guild_id))
                .map_err(|error| error.to_string())?
                .and_then(|summary| sql_i64(&summary.winning_team))
                .map_or(
                    "dire",
                    |winning_team| {
                        if winning_team == 1 { "radiant" } else { "dire" }
                    },
                );
            let worker = self.clone();
            let pending_for_task = pending.clone();
            let winner = winner.to_owned();
            let recorded = tokio::task::spawn_blocking(move || {
                worker.record_match_blocking(&pending_for_task, &winner, None)
            })
            .await
            .map_err(|error| format!("record recovery task failed: {error}"))??;
            self.emit_post_match_hooks(
                recorded.bet_settlement,
                Some(recorded.easter_eggs),
                recorded.debrief,
            )
            .await;
            self.cancel_betting_tasks(pending.guild_id, Some(pending.pending_match_id));
            let repository = self.pending.clone();
            tokio::task::spawn_blocking(move || {
                repository.delete_pending_match(pending.guild_id, pending.pending_match_id)
            })
            .await
            .map_err(|error| format!("record recovery cleanup task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            self.spawn_recorded_match_discovery(pending.guild_id, match_id, &pending);
            return Ok(());
        }

        // Uncommitted shuffles retain their durable betting deadline. Re-arm
        // every reminder from that absolute time; stale deadlines naturally
        // schedule nothing, and an admin extension replaces this task set.
        self.schedule_betting_reminders(&pending, false);
        Ok(())
    }

    fn rate_limit(
        &self,
        scope: &str,
        context: &MatchCommandContext,
        limit: usize,
    ) -> Result<Option<u64>, InteractionHandlerError> {
        let decision = self
            .rate_limiter
            .lock()
            .map_err(|_| "match rate-limit lock was poisoned")?
            .check_at(
                monotonic_seconds(),
                scope,
                context.guild_id,
                context.user_id,
                limit,
                RATE_LIMIT_WINDOW,
            );
        Ok((!decision.allowed).then_some(decision.retry_after_seconds))
    }

    fn is_admin(&self, context: &MatchCommandContext) -> bool {
        self.config.admin_user_ids.contains(&context.user_id)
            || context.member_permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
    }

    async fn handle_shuffle(
        &self,
        context: MatchCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let explicit_kind = string_option(&context.options, "lobby")
            .map(|value| parse_lobby_kind(&value))
            .transpose()?;
        let memberships = self
            .lobbies
            .lobby_kinds_for_player(context.guild_id, context.user_id);
        let lobby_kind = match explicit_kind {
            Some(kind) => kind,
            None if memberships.is_empty() => {
                return followup_ephemeral(&responder, "❌ Join a lobby before using `/shuffle`.")
                    .await;
            }
            None if memberships.len() > 1 => {
                return followup_ephemeral(&responder, AMBIGUOUS_LOBBY_MESSAGE).await;
            }
            None => memberships[0],
        };

        let operation_lock = self.lobbies.operation_lock(context.guild_id, lobby_kind);
        let Ok(operation_guard) =
            tokio::time::timeout(SHUFFLE_LOCK_TIMEOUT, operation_lock.lock()).await
        else {
            return followup_ephemeral(
                &responder,
                &format!(
                    "A {} shuffle is already in progress. Please wait for it to complete.",
                    lobby_kind.label()
                ),
            )
            .await;
        };

        let pending_repository = self.pending.clone();
        let guild_id = context.guild_id;
        let user_id = context.user_id;
        let participant_match = tokio::task::spawn_blocking(move || {
            pending_repository.pending_match_for_participant(guild_id, user_id)
        })
        .await
        .map_err(|error| format!("pending-match lookup task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if let Some(pending) = participant_match {
            let mut message = format!(
                "❌ You're already in a pending match (Match #{})!",
                pending.pending_match_id
            );
            if let Some(jump_url) = pending.state.shuffle_message_jump_url {
                message.push_str(&format!(
                    " [View your match]({jump_url}) and use `/record` to complete it first."
                ));
            } else {
                message.push_str(" Use `/record` to complete it first.");
            }
            return followup_ephemeral(&responder, &message).await;
        }

        if let Some(draft) = self.lobbies.active_draft(context.guild_id)
            && draft.lobby_kind == lobby_kind
        {
            let message = if draft.captain_ids.contains(&context.user_id) || self.is_admin(&context)
            {
                "❌ There's an active Immortal Draft in progress! Use `/draft restart` to restart it or complete the draft first."
            } else {
                "❌ There's an active Immortal Draft in progress! Please wait for it to complete."
            };
            return followup_ephemeral(&responder, message).await;
        }

        let Some(snapshot) = self.lobbies.snapshot(context.guild_id, lobby_kind) else {
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ No active {} lobby. Use `/lobby` to create one!",
                    lobby_kind.label()
                ),
            )
            .await;
        };
        if !snapshot.player_ids.contains(&context.user_id) {
            return followup_ephemeral(
                &responder,
                &format!("❌ Join {} before using `/shuffle`.", lobby_kind.label()),
            )
            .await;
        }
        if snapshot.player_ids.len() < 10 {
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ {} needs at least 10 players. Currently {}/10.",
                    lobby_kind.label(),
                    snapshot.player_ids.len()
                ),
            )
            .await;
        }

        let (player_ids, excluded_conditional_ids) = selected_roster(&snapshot);
        let pending_repository = self.pending.clone();
        let pending_ids = tokio::task::spawn_blocking(move || {
            pending_repository.all_pending_player_ids(guild_id)
        })
        .await
        .map_err(|error| format!("pending-player lookup task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let blocked = player_ids
            .iter()
            .copied()
            .filter(|player_id| pending_ids.contains(player_id))
            .collect::<Vec<_>>();
        if !blocked.is_empty() {
            let players = self.players.clone();
            let blocked_lookup = blocked.clone();
            let loaded = tokio::task::spawn_blocking(move || {
                players.get_by_ids(&blocked_lookup, Some(guild_id))
            })
            .await
            .map_err(|error| format!("blocked-player lookup task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            let names = loaded
                .iter()
                .take(5)
                .map(|player| player.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let remainder = if blocked.len() > 5 {
                format!(" and {} more", blocked.len() - 5)
            } else {
                String::new()
            };
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ Cannot shuffle: {} players are already in a pending match: {names}{remainder}\nThey must complete their current match with `/record` first.",
                    blocked.len()
                ),
            )
            .await;
        }

        let reserved_ids = player_ids.iter().copied().collect::<BTreeSet<_>>();
        if !self
            .lobbies
            .reserve_players(context.guild_id, lobby_kind, &reserved_ids)
        {
            return followup_ephemeral(
                &responder,
                "❌ The lobby changed while starting the shuffle. Please try again.",
            )
            .await;
        }

        let requested_rating_system = string_option(&context.options, "rating_system")
            .unwrap_or_else(|| {
                if fastrand::f64() < self.config.openskill_shuffle_chance {
                    "openskill".to_owned()
                } else {
                    "glicko".to_owned()
                }
            });
        let shuffle_timestamp = unix_seconds();
        let lobby_wait_minutes = lobby_wait_minutes(&snapshot, &player_ids, shuffle_timestamp);
        let request = PrepareShuffleRequest {
            guild_id: context.guild_id,
            lobby_kind,
            player_ids,
            excluded_conditional_ids,
            lobby_wait_minutes,
            rating_system: requested_rating_system,
            shuffle_mode: string_option(&context.options, "mode")
                .unwrap_or_else(|| "balanced".to_owned()),
            shuffle_timestamp,
            is_bomb_pot: fastrand::f64() < self.config.bomb_pot_chance,
        };
        let worker = self.clone();
        let prepared = tokio::task::spawn_blocking(move || worker.prepare_shuffle(request)).await;
        let prepared = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                self.lobbies
                    .release_players(context.guild_id, lobby_kind, &reserved_ids);
                warn!(%error, "shuffle validation/persistence failed");
                return followup_ephemeral(&responder, &format!("❌ {error}")).await;
            }
            Err(error) => {
                self.lobbies
                    .release_players(context.guild_id, lobby_kind, &reserved_ids);
                warn!(%error, "shuffle blocking task failed");
                return followup_ephemeral(
                    &responder,
                    "❌ Unexpected error while shuffling. Please try again.",
                )
                .await;
            }
        };

        if let Err(error) = self
            .lobbies
            .evict_from_other_lobby(context.guild_id, lobby_kind, &prepared.matched_ids())
            .await
        {
            warn!(%error, "matched-player sibling lobby eviction failed");
        }
        self.lobbies
            .release_players(context.guild_id, lobby_kind, &reserved_ids);

        let result = self
            .finalize_shuffle(context, responder, snapshot, prepared)
            .await;
        // The refresh traverses the same per-lobby operation locks. The
        // shuffle generation is already durably finalized/reset, so release
        // this scope before refreshing or the success path deadlocks itself.
        drop(operation_guard);
        if let Err(error) = self.lobbies.refresh_first_game_pool_lobbies(guild_id).await {
            warn!(%error, "post-shuffle first-game pool lobby refresh failed");
        }
        result
    }

    async fn handle_record(
        &self,
        context: MatchCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let result = string_option(&context.options, "result")
            .ok_or("required record result was not provided")?;
        if !matches!(result.as_str(), "radiant" | "dire" | "abort") {
            return followup_ephemeral(&responder, "❌ Invalid match result.").await;
        }
        let pending = self
            .select_pending_for_record(context.guild_id, context.user_id, &result)
            .await?;
        let Some(pending) = pending else {
            let all = self.pending_matches(context.guild_id).await?;
            if all.is_empty() {
                return followup_ephemeral(&responder, "❌ No pending match to record.").await;
            }
            let match_ids = all
                .iter()
                .map(|pending| format!("#{}", pending.pending_match_id))
                .collect::<Vec<_>>()
                .join(", ");
            let (context_text, target_text) = if result == "abort" {
                (
                    "you're not in any shuffled lobby",
                    "Only players in the shuffled lobby can vote to abort.",
                )
            } else {
                (
                    "you're not a participant in any of them",
                    "Only match participants can vote on the result.",
                )
            };
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ Multiple pending matches exist ({match_ids}) but {context_text}. {target_text}"
                ),
            )
            .await;
        };
        let is_admin = self.is_admin(&context);
        if result == "abort" {
            if is_admin {
                return self.finalize_abort(&pending, responder).await;
            }
            let voting = self.voting.clone();
            let key = PendingVoteKey {
                guild_id: Some(context.guild_id),
                pending_match_id: Some(pending.pending_match_id),
            };
            let user_id = context.user_id;
            let submission = tokio::task::spawn_blocking(move || {
                voting.add_abort_submission(key, user_id, false)
            })
            .await
            .map_err(|error| format!("abort-vote task failed: {error}"))?;
            let submission = match submission {
                Ok(submission) => submission,
                Err(error) => {
                    return followup_ephemeral(
                        &responder,
                        &format!("❌ {}", voting_error_message(error)),
                    )
                    .await;
                }
            };
            if !submission.is_ready {
                return followup_ephemeral(
                    &responder,
                    &format!(
                        "✅ Abort request recorded (Match #{}). Non-admin submissions: {}/4 (admins do not count toward the minimum).\nRequires 4 abort confirmations.",
                        pending.pending_match_id, submission.non_admin_count
                    ),
                )
                .await;
            }
            return self.finalize_abort(&pending, responder).await;
        }

        let voting = self.voting.clone();
        let key = PendingVoteKey {
            guild_id: Some(context.guild_id),
            pending_match_id: Some(pending.pending_match_id),
        };
        let user_id = context.user_id;
        let vote_result = result.clone();
        let submission = tokio::task::spawn_blocking(move || {
            voting.add_record_submission(key, user_id, &vote_result, is_admin)
        })
        .await
        .map_err(|error| format!("record-vote task failed: {error}"))?;
        let submission = match submission {
            Ok(submission) => submission,
            Err(error) => {
                return followup_ephemeral(
                    &responder,
                    &format!("❌ {}", voting_error_message(error)),
                )
                .await;
            }
        };
        if !submission.is_ready {
            let result_name = if result == "radiant" {
                "Radiant Won"
            } else {
                "Dire Won"
            };
            let admin_note = if is_admin {
                " (admins do not count toward the minimum)"
            } else {
                ""
            };
            return followup_ephemeral(
                &responder,
                &format!(
                    "✅ Result recorded for {result_name} (Match #{}).{admin_note}\n🟢 Radiant: {}/3 | 🔴 Dire: {}/3\nRequires 3 confirmations.",
                    pending.pending_match_id,
                    submission.vote_counts.radiant,
                    submission.vote_counts.dire,
                ),
            )
            .await;
        }
        let winning_result = submission
            .result
            .ok_or("ready record vote did not contain a winning result")?;
        let dotabuff_match_id = string_option(&context.options, "dotabuff_match_id");
        self.finalize_record(
            pending,
            context,
            responder,
            winning_result.name(),
            submission.non_admin_count,
            submission.vote_counts.radiant,
            submission.vote_counts.dire,
            dotabuff_match_id,
        )
        .await
    }

    async fn pending_matches(
        &self,
        guild_id: i64,
    ) -> Result<Vec<PendingMatchRecord>, InteractionHandlerError> {
        let repository = self.pending.clone();
        tokio::task::spawn_blocking(move || repository.pending_matches(guild_id))
            .await
            .map_err(|error| -> InteractionHandlerError {
                format!("pending-match list task failed: {error}").into()
            })?
            .map_err(|error| error.to_string().into())
    }

    async fn select_pending_for_record(
        &self,
        guild_id: i64,
        user_id: i64,
        result: &str,
    ) -> Result<Option<PendingMatchRecord>, InteractionHandlerError> {
        let all = self.pending_matches(guild_id).await?;
        match all.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(only.clone())),
            _ => {
                let repository = self.pending.clone();
                let abort = result == "abort";
                tokio::task::spawn_blocking(move || {
                    if abort {
                        repository.pending_match_for_full_lobby_player(guild_id, user_id)
                    } else {
                        repository.pending_match_for_participant(guild_id, user_id)
                    }
                })
                .await
                .map_err(|error| -> InteractionHandlerError {
                    format!("pending-match selection task failed: {error}").into()
                })?
                .map_err(|error| error.to_string().into())
            }
        }
    }

    async fn finalize_abort(
        &self,
        pending: &PendingMatchRecord,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let key = (pending.guild_id, pending.pending_match_id);
        let acquired = {
            let mut finalizing = self
                .finalizing_matches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            finalizing.insert(key)
        };
        if !acquired {
            return followup_ephemeral(&responder, "⏳ This match is already being aborted.").await;
        }
        let result = self.finalize_abort_owned(pending).await;
        self.finalizing_matches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        if let Err(error) = result {
            warn!(%error, pending_match_id = pending.pending_match_id, "match abort failed");
            return followup_ephemeral(
                &responder,
                "❌ Match abort failed while refunding bets. Please try again.",
            )
            .await;
        }

        let users = pending
            .state
            .full_lobby_player_ids()
            .into_iter()
            .filter(|player_id| *player_id > 0)
            .filter_map(|player_id| u64::try_from(player_id).ok())
            .collect::<Vec<_>>();
        let mentions = users
            .iter()
            .map(|player_id| format!("<@{player_id}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut content = format!(
            "✅ Match aborted (Match #{}). Bets have been refunded.",
            pending.pending_match_id
        );
        if !mentions.is_empty() {
            content.push('\n');
            content.push_str(&mentions);
        }
        responder
            .followup(InteractionResponse::message(content).with_user_mentions(users))
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn finalize_abort_owned(&self, pending: &PendingMatchRecord) -> Result<(), String> {
        let bets = self.bets.clone();
        let seeds = self.seeds.clone();
        let guild_id = pending.guild_id;
        let pending_match_id = pending.pending_match_id;
        let shuffle_timestamp = pending.state.shuffle_timestamp.unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            bets.refund_pending_bets_atomic(
                Some(guild_id),
                shuffle_timestamp,
                Some(pending_match_id),
            )
            .map_err(|error| error.to_string())?;
            seeds
                .abort_seed_atomic(Some(guild_id), pending_match_id)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(())
        })
        .await
        .map_err(|error| format!("abort refund task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        self.cancel_betting_tasks(guild_id, Some(pending_match_id));

        if let Some(thread_id) = pending
            .state
            .thread_shuffle_thread_id
            .and_then(|thread_id| u64::try_from(thread_id).ok())
        {
            let lobby = parse_persisted_lobby_kind(pending.state.lobby_kind.as_deref());
            if let Err(error) = self
                .discord
                .send_message(
                    thread_id,
                    DiscordMessage::silent(InteractionResponse::message(format!(
                        "🚫 **{} Match Aborted** - All bets have been refunded.",
                        lobby.label()
                    ))),
                )
                .await
            {
                debug!(%error, thread_id, "abort thread announcement failed");
            }
            if let Err(error) = self
                .discord
                .edit_thread(
                    thread_id,
                    &format!("🚫 {} Match Aborted", lobby.label()),
                    true,
                    true,
                )
                .await
            {
                debug!(%error, thread_id, "abort thread archive failed");
            }
        }

        let repository = self.pending.clone();
        tokio::task::spawn_blocking(move || {
            repository.delete_pending_match(guild_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("abort cleanup task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn finalize_record(
        &self,
        pending: PendingMatchRecord,
        context: MatchCommandContext,
        responder: Arc<dyn InteractionResponder>,
        winning_result: &str,
        non_admin_count: usize,
        radiant_votes: usize,
        dire_votes: usize,
        dotabuff_match_id: Option<String>,
    ) -> Result<(), InteractionHandlerError> {
        let key = (pending.guild_id, pending.pending_match_id);
        let acquired = {
            let mut finalizing = self
                .finalizing_matches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            finalizing.insert(key)
        };
        if !acquired {
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ Match recording already in progress (Match #{}).",
                    pending.pending_match_id
                ),
            )
            .await;
        }
        let worker = self.clone();
        let pending_for_task = pending.clone();
        let winning_for_task = winning_result.to_owned();
        let recorded = tokio::task::spawn_blocking(move || {
            worker.record_match_blocking(
                &pending_for_task,
                &winning_for_task,
                dotabuff_match_id.as_deref(),
            )
        })
        .await;
        self.finalizing_matches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        let recorded = match recorded {
            Ok(Ok(recorded)) => recorded,
            Ok(Err(error)) => {
                self.cancel_betting_tasks(pending.guild_id, Some(pending.pending_match_id));
                warn!(%error, pending_match_id = pending.pending_match_id, "match record failed");
                return followup_ephemeral(&responder, &format!("❌ {error}")).await;
            }
            Err(error) => {
                self.cancel_betting_tasks(pending.guild_id, Some(pending.pending_match_id));
                warn!(%error, pending_match_id = pending.pending_match_id, "match record task failed");
                return followup_ephemeral(
                    &responder,
                    "❌ Unexpected error recording match. Please try again.",
                )
                .await;
            }
        };
        self.cancel_betting_tasks(pending.guild_id, Some(pending.pending_match_id));

        // The core and every retryable money phase are durable at this point.
        // Clear the pending token before any Discord work so an announcement
        // failure cannot leave a completed match selectable for `/record`.
        let repository = self.pending.clone();
        let guild_id = pending.guild_id;
        let pending_match_id = pending.pending_match_id;
        tokio::task::spawn_blocking(move || {
            repository.delete_pending_match(guild_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("record cleanup task failed: {error}"))?
        .map_err(|error| error.to_string())?;

        self.record_pet_match_activity(&pending, recorded.match_id)
            .await;
        self.emit_post_match_hooks(
            recorded.bet_settlement.clone(),
            Some(recorded.easter_eggs.clone()),
            recorded.debrief.clone(),
        )
        .await;

        if let Some(thread_id) = pending
            .state
            .thread_shuffle_thread_id
            .and_then(|thread_id| u64::try_from(thread_id).ok())
        {
            let discord = Arc::clone(&self.discord);
            let lobby = parse_persisted_lobby_kind(pending.state.lobby_kind.as_deref());
            let winner = if winning_result == "radiant" {
                "Radiant"
            } else {
                "Dire"
            };
            tokio::spawn(Self::finalize_record_thread(
                discord,
                thread_id,
                lobby.label().to_owned(),
                winner.to_owned(),
                Duration::from_secs(15),
            ));
        }

        let streaming = if self.config.streaming_bonus > 0 {
            let participant_ids = pending
                .state
                .participant_ids()
                .into_iter()
                .filter(|player_id| *player_id > 0)
                .filter_map(|player_id| u64::try_from(player_id).ok())
                .collect::<Vec<_>>();
            self.discord
                .streaming_member_ids(
                    u64::try_from(pending.guild_id).unwrap_or_default(),
                    &participant_ids,
                )
                .await
                .unwrap_or_default()
        } else {
            BTreeSet::new()
        };
        if !streaming.is_empty() {
            let guild_id = pending.guild_id;
            let amount = self.config.streaming_bonus;
            let match_id = recorded.match_id;
            let player_ids = streaming
                .iter()
                .filter_map(|discord_id| i64::try_from(*discord_id).ok())
                .collect::<Vec<_>>();
            let rewards = Arc::clone(&self.rewards);
            match tokio::task::spawn_blocking(move || {
                rewards.award_generated_batch(GeneratedRewardBatch {
                    guild_id,
                    player_ids: &player_ids,
                    gross: amount,
                    apply_bankruptcy_penalty: true,
                    apply_vanity_tax: true,
                    source: "match_streaming_bonus",
                    related_type: "match",
                    related_id: match_id,
                    reason: "match streaming bonus",
                    event_nonce_tag: 6,
                })
            })
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => warn!(%error, "record streaming bonus failed"),
                Err(error) => warn!(%error, "record streaming bonus task failed"),
            }
        }

        let admin_override = self.is_admin(&context) && non_admin_count < 3;
        let winner = if winning_result == "radiant" {
            "Radiant Won"
        } else {
            "Dire Won"
        };
        let confirmations = if admin_override {
            String::new()
        } else {
            format!(" (🟢 {radiant_votes} vs 🔴 {dire_votes})")
        };
        let mut message = format!(
            "✅ Match recorded — {winner}{confirmations}.\n\n🪙 **JC Changes:**\n{}",
            recorded.jc_lines.join("\n")
        );
        if !streaming.is_empty() {
            let mentions = streaming
                .iter()
                .map(|discord_id| format!("<@{discord_id}>"))
                .collect::<Vec<_>>()
                .join(", ");
            message.push_str(&format!(
                "\n📺 Streaming bonus (+{} {JOPACOIN_EMOTE}): {mentions}",
                self.config.streaming_bonus
            ));
        }
        for chunk in chunk_default_discord_content(&message) {
            responder
                .followup(InteractionResponse::message(chunk))
                .await
                .map_err(|error| error.to_string())?;
        }

        self.spawn_moderation_completion_notifications(pending.guild_id, recorded.match_id);
        self.spawn_recorded_match_discovery(pending.guild_id, recorded.match_id, &pending);
        debug!(
            match_id = recorded.match_id,
            "match committed and announced"
        );
        Ok(())
    }

    async fn record_pet_match_activity(&self, pending: &PendingMatchRecord, match_id: i64) {
        let participant_ids = pending
            .state
            .participant_ids()
            .into_iter()
            .filter(|discord_id| *discord_id > 0)
            .collect::<Vec<_>>();
        let guild_id = pending.guild_id;
        let source_key = format!("match:{match_id}");
        let occurred_at = unix_seconds();
        let database_path = self.database_path.clone();
        if let Err(error) = tokio::task::spawn_blocking(move || {
            let repository = PetEvolutionRepository::new(database_path);
            for discord_id in participant_ids {
                // Match recording is already durable. Pet upbringing is a
                // best-effort side effect and must never roll the match back.
                let _ = repository.record_activity(
                    discord_id,
                    Some(guild_id),
                    PetActivity::MatchRecorded,
                    &source_key,
                    occurred_at,
                );
            }
        })
        .await
        {
            warn!(%error, match_id, "pet match activity task failed");
        }
    }

    async fn finalize_record_thread(
        discord: Arc<dyn DiscordTransport>,
        thread_id: u64,
        lobby_label: String,
        winner: String,
        archive_delay: Duration,
    ) {
        if let Err(error) = discord
            .send_message(
                thread_id,
                DiscordMessage::silent(InteractionResponse::message(format!(
                    "🏆 **{lobby_label} Match Complete - {winner} Victory!**"
                ))),
            )
            .await
        {
            debug!(%error, thread_id, "record thread result send failed");
        }
        tokio::time::sleep(archive_delay).await;
        if let Err(error) = discord
            .edit_thread(
                thread_id,
                &format!("✅ {lobby_label} Match Complete - {winner} Won"),
                true,
                true,
            )
            .await
        {
            debug!(%error, thread_id, "record thread archive failed");
        }
    }

    fn spawn_moderation_completion_notifications(&self, guild_id: i64, match_id: i64) {
        let handler = self.clone();
        tokio::spawn(async move {
            handler
                .notify_moderation_completions(guild_id, match_id)
                .await;
        });
    }

    async fn notify_moderation_completions(&self, guild_id: i64, match_id: i64) {
        let repository = ModerationRepository::new(&self.database_path);
        let events = match tokio::task::spawn_blocking(move || {
            repository.completion_events_for_match(Some(guild_id), match_id)
        })
        .await
        {
            Ok(Ok(events)) => events,
            Ok(Err(error)) => {
                warn!(%error, match_id, "failed to load moderation completions");
                return;
            }
            Err(error) => {
                warn!(%error, match_id, "moderation completion query task failed");
                return;
            }
        };

        for event in events {
            if event.event_type == ModerationEventType::LowprioComplete {
                continue;
            }
            let Ok(discord_id) = u64::try_from(event.discord_id) else {
                continue;
            };
            let message = match event.event_type {
                ModerationEventType::Complete => format!(
                    "Your matchmaking lobby suspension was completed by match #{match_id}. \
                     You may create and join eligible lobbies again."
                ),
                _ => continue,
            };
            if let Err(error) = self
                .discord
                .send_direct_message(
                    discord_id,
                    DiscordMessage::silent(InteractionResponse::message(message)),
                )
                .await
            {
                debug!(%error, discord_id, match_id, "moderation completion DM failed");
            }
        }
    }

    fn spawn_recorded_match_discovery(
        &self,
        guild_id: i64,
        match_id: i64,
        pending: &PendingMatchRecord,
    ) {
        let discovery = Arc::clone(&self.recorded_match_discovery);
        let discord = Arc::clone(&self.discord);
        let channel_id = pending
            .state
            .origin_channel_id
            .or(pending.state.cmd_shuffle_channel_id)
            .or(pending.state.shuffle_channel_id)
            .and_then(|channel_id| u64::try_from(channel_id).ok());
        tokio::spawn(Self::run_recorded_match_discovery(
            discovery, discord, channel_id, guild_id, match_id,
        ));
    }

    async fn run_recorded_match_discovery(
        discovery: Arc<dyn RecordedMatchDiscovery>,
        discord: Arc<dyn DiscordTransport>,
        channel_id: Option<u64>,
        guild_id: i64,
        match_id: i64,
    ) {
        match discovery.discover_recorded_match(guild_id, match_id).await {
            Ok(RecordedMatchDiscoveryOutcome::Discovered { result, response }) => {
                if let Some(channel_id) = channel_id
                    && let Err(error) = discord
                        .send_message(channel_id, DiscordMessage::silent(response))
                        .await
                {
                    warn!(%error, channel_id, match_id, "auto-enrichment publication failed");
                }
                debug!(
                    match_id,
                    valve_match_id = ?result.valve_match_id.map(|id| id.0),
                    confidence = ?result.confidence,
                    "match auto-enrichment completed"
                );
            }
            Ok(RecordedMatchDiscoveryOutcome::Disabled) => {
                debug!(match_id, guild_id, "match auto-enrichment disabled");
            }
            Ok(RecordedMatchDiscoveryOutcome::Exhausted { last_result }) => {
                warn!(
                    match_id,
                    guild_id,
                    status = ?last_result.map(|result| result.status),
                    "match auto-enrichment retries exhausted"
                );
            }
            Ok(RecordedMatchDiscoveryOutcome::Stopped(result)) => {
                debug!(
                    match_id,
                    guild_id,
                    status = ?result.status,
                    "match auto-enrichment stopped without retry"
                );
            }
            Err(error) => {
                warn!(%error, match_id, guild_id, "match auto-enrichment failed");
            }
        }
    }

    async fn emit_post_match_hooks(
        &self,
        settlement: Option<MatchBetSettlementRequest>,
        easter_eggs: Option<MatchEasterEggRequest>,
        debrief: Option<MatchPostMatchDebriefRequest>,
    ) {
        let Some(port) = self.post_match_debrief.get().cloned() else {
            return;
        };
        if let Some(request) = settlement {
            let match_id = request.match_id;
            if let Err(error) = port.on_bet_settlement(request).await {
                warn!(%error, match_id, "post-settlement Neon delivery failed");
            }
        }
        if let Some(request) = easter_eggs {
            let match_id = request.match_id;
            if let Err(error) = port.on_match_easter_eggs(request).await {
                warn!(%error, match_id, "match Easter-egg Neon delivery failed");
            }
        }
        if let Some(request) = debrief {
            let match_id = request.match_id;
            if let Err(error) = port.on_post_match_debrief(request).await {
                warn!(%error, match_id, "post-match debrief delivery failed");
            }
        }
    }

    #[cfg(test)]
    async fn emit_post_match_debrief(&self, request: Option<MatchPostMatchDebriefRequest>) {
        self.emit_post_match_hooks(None, None, request).await;
    }

    fn build_bet_settlement_request(
        &self,
        guild_id: i64,
        match_id: i64,
        channel_id: Option<u64>,
        settlement: &SettlementResult,
        leverages: &BTreeMap<i64, i64>,
    ) -> Result<MatchBetSettlementRequest, String> {
        let mut participants = Vec::with_capacity(
            settlement
                .winners
                .len()
                .saturating_add(settlement.losers.len()),
        );
        for bet in settlement.winners.iter().chain(settlement.losers.iter()) {
            participants.push(MatchBetSettlementParticipant {
                discord_id: bet.discord_id,
                amount: bet.effective_amount,
                leverage: leverages.get(&bet.bet_id).copied().unwrap_or(1),
                balance_after: bet.balance_after,
                payout: bet.payout,
                won: settlement
                    .winners
                    .iter()
                    .any(|winner| winner.bet_id == bet.bet_id),
                refunded: false,
            });
        }
        Ok(MatchBetSettlementRequest {
            guild_id,
            match_id,
            channel_id,
            participants,
        })
    }

    fn build_post_match_debrief(
        &self,
        match_id: i64,
        guild_id: i64,
        winning_ids: &[i64],
        settlement: &SettlementResult,
        channel_id: Option<u64>,
    ) -> Option<MatchPostMatchDebriefRequest> {
        if winning_ids.is_empty() {
            return None;
        }
        let history = match self
            .rating_history
            .get_match_rating_history(match_id, Some(guild_id))
        {
            Ok(history) => history,
            Err(error) => {
                warn!(%error, match_id, guild_id, "post-match rating history read failed");
                Vec::new()
            }
        };
        let snapshots = history
            .iter()
            .map(|entry| RatingHistorySnapshot {
                discord_id: entry.discord_id,
                rating_before: entry.rating_before,
                rating_after: entry.rating_after,
                openskill_mu_before: entry.openskill_mu_before,
                openskill_mu_after: entry.openskill_mu_after,
                expected_team_win_prob: entry.expected_team_win_prob,
            })
            .collect::<Vec<_>>();
        let selected = choose_notable_winner(&snapshots, winning_ids, &self.openskill);
        let (winner_id, rating_change, expected_win_prob) = selected.map_or_else(
            || (winning_ids[0], None, None),
            |(winner_id, details)| (winner_id, details.rating_change, details.expected_win_prob),
        );
        let loser = settlement.losers.iter().max_by_key(|loser| {
            (
                loser.effective_amount,
                loser.payout,
                std::cmp::Reverse(loser.discord_id),
            )
        });
        let payout = settlement
            .winners
            .iter()
            .map(|winner| winner.payout)
            .max()
            .unwrap_or_default();
        Some(MatchPostMatchDebriefRequest {
            guild_id,
            match_id,
            channel_id,
            winner_id: Some(winner_id),
            loser_id: loser.map(|loser| loser.discord_id),
            payout,
            loss: loser
                .map(|loser| loser.effective_amount)
                .unwrap_or_default(),
            leverage: 1,
            rating_change,
            expected_win_prob,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_match_easter_eggs(
        &self,
        participant_ids: &[i64],
        radiant_team_ids: &[i64],
        dire_team_ids: &[i64],
        winning_ids: &[i64],
        streak_data: &BTreeMap<i64, StreakAdjustment>,
        persisted_streak_records: Option<&[WinStreakRecord]>,
        guild_id: i64,
        match_id: i64,
        channel_id: Option<u64>,
    ) -> Result<MatchEasterEggRequest, String> {
        let games_milestones = self
            .players
            .get_by_ids(participant_ids, Some(guild_id))
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(|player| {
                let discord_id = player.discord_id?;
                let total_games = player.wins.saturating_add(player.losses);
                matches!(total_games, 10 | 50 | 100 | 200 | 500).then_some(GamesMilestone {
                    discord_id,
                    games_played: total_games,
                })
            })
            .collect::<Vec<_>>();

        let win_streak_records = if let Some(records) = persisted_streak_records {
            records.to_vec()
        } else {
            let mut streak_candidates = OrderedMap::new();
            for discord_id in winning_ids {
                let Some(adjustment) = streak_data.get(discord_id) else {
                    continue;
                };
                if adjustment.streak_length >= 5 {
                    streak_candidates.insert(
                        *discord_id,
                        i64::try_from(adjustment.streak_length).unwrap_or(i64::MAX),
                    );
                }
            }
            let mut records = Vec::new();
            for (discord_id, current_streak) in streak_candidates.iter() {
                let previous_best = self
                    .players
                    .get_personal_best_win_streak(*discord_id, Some(guild_id))
                    .map_err(|error| error.to_string())?;
                if current_streak > &previous_best {
                    records.push(WinStreakRecord {
                        discord_id: *discord_id,
                        current_streak: *current_streak,
                        previous_best,
                    });
                }
            }
            records
        };

        let mut rivalries_detected = Vec::new();
        match PairingsRepository::new(&self.database_path)
            .get_pairings_for_players(participant_ids, Some(guild_id))
        {
            Ok(pairings) => {
                for radiant_id in radiant_team_ids {
                    for dire_id in dire_team_ids {
                        let key = PairingsRepository::canonical_pair(*radiant_id, *dire_id);
                        let Some(pairing) = pairings.get(&key) else {
                            continue;
                        };
                        if pairing.games_against < 10 {
                            continue;
                        }
                        let radiant_wins = pairing.queried_player_wins_against(*radiant_id);
                        let winrate_vs = 100.0 * radiant_wins as f64 / pairing.games_against as f64;
                        if !(30.0 < winrate_vs && winrate_vs < 70.0) {
                            rivalries_detected.push(RivalryRecord {
                                player1_id: *radiant_id,
                                player2_id: *dire_id,
                                games_against: pairing.games_against,
                                winrate_vs,
                            });
                        }
                    }
                }
            }
            Err(error) => {
                warn!(
                    %error,
                    guild_id,
                    "match rivalry lookup failed; preserving non-rivalry Easter eggs"
                );
            }
        }

        Ok(MatchEasterEggRequest {
            guild_id,
            match_id,
            channel_id,
            games_milestones,
            win_streak_records,
            rivalries_detected,
        })
    }

    fn apply_easter_streak_records(
        &self,
        guild_id: i64,
        records: &[WinStreakRecord],
    ) -> Result<(), String> {
        let mut updates = OrderedMap::new();
        for record in records {
            updates.insert(record.discord_id, record.current_streak);
        }
        self.players
            .update_personal_best_win_streaks(&updates, Some(guild_id))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn record_match_blocking(
        &self,
        pending: &PendingMatchRecord,
        winning_result: &str,
        dotabuff_match_id: Option<&str>,
    ) -> Result<RecordedMatch, String> {
        let winning_team = match winning_result {
            "radiant" => 1,
            "dire" => 2,
            _ => return Err("winning_team must be 'radiant' or 'dire'.".to_owned()),
        };
        let persisted_streak_records = persisted_easter_streaks(&pending.state)?;
        let participant_ids = pending
            .state
            .radiant_team_ids
            .iter()
            .chain(&pending.state.dire_team_ids)
            .copied()
            .collect::<Vec<_>>();
        if participant_ids.len() != 10
            || participant_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != 10
        {
            return Err("A recorded match requires ten distinct participants.".to_owned());
        }
        if pending
            .state
            .excluded_player_ids
            .iter()
            .any(|player_id| participant_ids.contains(player_id))
        {
            return Err("Excluded players detected in match teams.".to_owned());
        }

        let (inputs, expected_openskill_revision) = self
            .players
            .get_match_rating_inputs_with_openskill_revision(
                &participant_ids,
                Some(pending.guild_id),
            )
            .map_err(|error| error.to_string())?;
        if inputs.len() != participant_ids.len() {
            return Err("Could not load every participant's rating state.".to_owned());
        }
        let matches = MatchRepository::new(&self.database_path);
        let recent = matches
            .get_player_recent_outcomes_bulk(&participant_ids, Some(pending.guild_id), 20)
            .map_err(|error| error.to_string())?;
        let low_priority = self
            .low_priority
            .get_active_ids(&participant_ids, Some(pending.guild_id))
            .map_err(|error| error.to_string())?;

        let mut glicko_before = BTreeMap::new();
        let mut openskill_before = BTreeMap::new();
        for player_id in &participant_ids {
            let input = inputs
                .get(player_id)
                .ok_or_else(|| format!("missing rating input for {player_id}"))?;
            let mut glicko = sql_f64(&input.glicko_rating).map_or_else(
                || {
                    self.rating.create_player_from_mmr(
                        sql_i64(&input.current_mmr).and_then(|value| i32::try_from(value).ok()),
                    )
                },
                |rating| {
                    self.rating.create_player_from_rating(
                        rating,
                        sql_f64(&input.glicko_rd).unwrap_or(self.rating.initial_rd),
                        sql_f64(&input.glicko_volatility).unwrap_or(self.rating.initial_volatility),
                    )
                },
            );
            glicko.rd = decayed_glicko_rd(
                &self.rating,
                glicko.rd,
                sql_datetime(&input.last_match_date),
                sql_datetime(&input.created_at),
                Utc::now(),
            );
            let mut os_mu = sql_f64(&input.os_mu).unwrap_or_else(|| {
                let mmr = sql_i64(&input.current_mmr)
                    .and_then(|value| i32::try_from(value).ok())
                    .unwrap_or(4000);
                self.openskill
                    .mmr_to_os_mu(self.rating.new_player_seed_mmr(mmr))
            });
            if !os_mu.is_finite() {
                os_mu = CamaOpenSkillSystem::DEFAULT_MU;
            }
            let mut os_sigma = sql_f64(&input.os_sigma)
                .filter(|sigma| sigma.is_finite())
                .unwrap_or(CamaOpenSkillSystem::DEFAULT_SIGMA);
            let reference =
                sql_datetime(&input.last_match_date).or_else(|| sql_datetime(&input.created_at));
            if let Some(reference) = reference {
                os_sigma = self
                    .openskill
                    .apply_sigma_decay(os_sigma, (Utc::now() - reference).num_days().max(0));
            }
            glicko_before.insert(*player_id, glicko);
            openskill_before.insert(*player_id, OpenSkillRating::new(os_mu, os_sigma));
        }

        let winners = if winning_team == 1 {
            &pending.state.radiant_team_ids
        } else {
            &pending.state.dire_team_ids
        };
        let losers = if winning_team == 1 {
            &pending.state.dire_team_ids
        } else {
            &pending.state.radiant_team_ids
        };
        let mut streak_multipliers = HashMap::new();
        let mut streak_data = BTreeMap::new();
        let mut gain_multipliers = HashMap::new();
        for player_id in &participant_ids {
            let won = winners.contains(player_id);
            let adjustment = self.rating.calculate_streak_multiplier(
                recent.get(player_id).map_or(&[], Vec::as_slice),
                won,
                None,
                None,
            );
            streak_multipliers.insert(*player_id, adjustment.multiplier);
            streak_data.insert(*player_id, adjustment);
            if low_priority.contains(player_id) {
                gain_multipliers.insert(
                    *player_id,
                    self.openskill.config().low_priority_gain_multiplier,
                );
            }
        }

        let radiant_glicko = pending
            .state
            .radiant_team_ids
            .iter()
            .map(|player_id| {
                Ok(GlickoTeamPlayer::new(
                    *glicko_before
                        .get(player_id)
                        .ok_or_else(|| format!("missing Glicko state for {player_id}"))?,
                    *player_id,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let dire_glicko = pending
            .state
            .dire_team_ids
            .iter()
            .map(|player_id| {
                Ok(GlickoTeamPlayer::new(
                    *glicko_before
                        .get(player_id)
                        .ok_or_else(|| format!("missing Glicko state for {player_id}"))?,
                    *player_id,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let radiant_stats = self.rating.aggregate_team_stats(
            &radiant_glicko
                .iter()
                .map(|player| player.player)
                .collect::<Vec<_>>(),
        );
        let dire_stats = self.rating.aggregate_team_stats(
            &dire_glicko
                .iter()
                .map(|player| player.player)
                .collect::<Vec<_>>(),
        );
        let expected_radiant = CamaRatingSystem::predict_win_probability(
            radiant_stats.rating,
            radiant_stats.rd,
            dire_stats.rating,
            dire_stats.rd,
        );
        let glicko_updates = self
            .rating
            .update_ratings_after_match_with_options(
                &radiant_glicko,
                &dire_glicko,
                winning_team,
                &MatchUpdateOptions {
                    streak_multipliers: streak_multipliers.clone(),
                    base_rating_delta_multiplier: None,
                    gain_multipliers: gain_multipliers.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        let glicko_updates = glicko_updates
            .team1
            .into_iter()
            .chain(glicko_updates.team2)
            .map(|update| {
                (
                    update.id,
                    CoreGlickoUpdate {
                        discord_id: update.id,
                        rating: update.rating,
                        rd: update.rd,
                        volatility: update.volatility,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        let synthetic_ids = participant_ids
            .iter()
            .enumerate()
            .map(|(index, player_id)| (*player_id, u64::try_from(index + 1).unwrap_or_default()))
            .collect::<BTreeMap<_, _>>();
        let os_team = |ids: &[i64]| {
            ids.iter()
                .map(|player_id| {
                    let rating = openskill_before
                        .get(player_id)
                        .ok_or_else(|| format!("missing OpenSkill state for {player_id}"))?;
                    Ok(OpenSkillPlayer::new(
                        synthetic_ids[player_id],
                        Some(rating.mu),
                        Some(rating.sigma),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()
        };
        let radiant_os = os_team(&pending.state.radiant_team_ids)?;
        let dire_os = os_team(&pending.state.dire_team_ids)?;
        let os_streaks = participant_ids
            .iter()
            .map(|player_id| {
                (
                    synthetic_ids[player_id],
                    streak_multipliers.get(player_id).copied().unwrap_or(1.0),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let os_gains = participant_ids
            .iter()
            .filter_map(|player_id| {
                gain_multipliers
                    .get(player_id)
                    .map(|gain| (synthetic_ids[player_id], *gain))
            })
            .collect::<BTreeMap<_, _>>();
        let raw_openskill = self.openskill.os_predict_win_probability(
            &pending
                .state
                .radiant_team_ids
                .iter()
                .map(|player_id| openskill_before[player_id])
                .collect::<Vec<_>>(),
            &pending
                .state
                .dire_team_ids
                .iter()
                .map(|player_id| openskill_before[player_id])
                .collect::<Vec<_>>(),
        );
        let calibrated_openskill = self
            .openskill
            .calibrate_win_probability(raw_openskill, None)
            .map_err(|error| error.to_string())?;
        let os_result = self
            .openskill
            .update_ratings_equal_weight(
                &radiant_os,
                &dire_os,
                if winning_team == 1 {
                    OpenSkillWinningTeam::Team1
                } else {
                    OpenSkillWinningTeam::Team2
                },
                &os_streaks,
                &os_gains,
            )
            .map_err(|error| error.to_string())?;
        let os_updates = participant_ids
            .iter()
            .map(|player_id| {
                let updated = os_result
                    .get(&synthetic_ids[player_id])
                    .ok_or_else(|| format!("missing OpenSkill update for {player_id}"))?;
                Ok((
                    *player_id,
                    CoreOpenSkillUpdate {
                        discord_id: *player_id,
                        mu: updated.mu,
                        sigma: updated.sigma,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;

        let mut history = Vec::with_capacity(participant_ids.len());
        let mut first_calibration_ids = Vec::new();
        for player_id in &participant_ids {
            let glicko_before = glicko_before[player_id];
            let glicko_after = glicko_updates[player_id];
            let os_before = openskill_before[player_id];
            let os_after = os_updates[player_id];
            let team_number = if pending.state.radiant_team_ids.contains(player_id) {
                1
            } else {
                2
            };
            let input = inputs
                .get(player_id)
                .ok_or_else(|| format!("missing rating input for {player_id}"))?;
            if glicko_after.rd <= self.rating.config.calibration_rd_threshold
                && matches!(input.first_calibrated_at, SqlValue::Null)
            {
                first_calibration_ids.push(*player_id);
            }
            let streak = streak_data[player_id];
            history.push(NewCoreRatingHistory {
                discord_id: *player_id,
                rating: Some(glicko_after.rating),
                rating_before: Some(glicko_before.rating),
                rd_before: Some(glicko_before.rd),
                rd_after: Some(glicko_after.rd),
                volatility_before: Some(glicko_before.volatility),
                volatility_after: Some(glicko_after.volatility),
                expected_team_win_prob: Some(if team_number == 1 {
                    expected_radiant
                } else {
                    1.0 - expected_radiant
                }),
                team_number: Some(team_number),
                won: Some(winners.contains(player_id)),
                os_mu_before: Some(os_before.mu),
                os_mu_after: Some(os_after.mu),
                os_sigma_before: Some(os_before.sigma),
                os_sigma_after: Some(os_after.sigma),
                streak_length: Some(i64::try_from(streak.streak_length).unwrap_or(i64::MAX)),
                streak_multiplier: Some(streak.multiplier),
                streak_multiplier_per_game: Some(self.rating.streak_multiplier_per_game()),
                streak_threshold: Some(self.rating.streak_threshold()),
                base_rating_delta_multiplier: Some(self.rating.base_rating_delta_multiplier()),
                low_priority_gain_multiplier: Some(
                    gain_multipliers.get(player_id).copied().unwrap_or(1.0),
                ),
                ..NewCoreRatingHistory::default()
            });
        }

        let now = Utc::now();
        let mut match_record = MatchRecord::new(
            pending.state.radiant_team_ids.clone(),
            pending.state.dire_team_ids.clone(),
            winning_team.into(),
            Some(pending.guild_id),
        );
        match_record.dotabuff_match_id = dotabuff_match_id.map(str::to_owned);
        match_record.lobby_type = if pending.state.is_draft {
            "draft".to_owned()
        } else {
            "shuffle".to_owned()
        };
        match_record.lobby_kind = pending.state.lobby_kind.clone();
        match_record.balancing_rating_system = pending.state.balancing_rating_system.clone();
        match_record.betting_mode = pending.state.betting_mode.clone();
        // The shuffler's role assignment lives only in the pending payload,
        // which is deleted immediately after this record commits. Copy it into
        // the permanent match row or it is lost for good.
        let match_record = match_record.with_roles(
            pending.state.radiant_roles.clone(),
            pending.state.dire_roles.clone(),
        );
        let mut core = CoreMatchRecord::new(match_record, now.to_rfc3339());
        core.pending_match_id = Some(pending.pending_match_id);
        core.winning_ids = winners.clone();
        core.losing_ids = losers.clone();
        core.rating_history_rows = history;
        core.match_prediction = MatchPredictionSnapshot {
            radiant_rating: Some(radiant_stats.rating),
            dire_rating: Some(dire_stats.rating),
            radiant_rd: Some(radiant_stats.rd),
            dire_rd: Some(dire_stats.rd),
            expected_radiant_win_prob: Some(expected_radiant),
            openskill_radiant_win_prob: Some(calibrated_openskill),
            openskill_raw_radiant_win_prob: Some(raw_openskill),
        };
        core.streak_defaults = StreakInsertDefaults {
            multiplier_per_game: self.rating.streak_multiplier_per_game(),
            threshold: self.rating.streak_threshold(),
        };
        core.glicko_updates = glicko_updates.values().copied().collect();
        core.openskill_updates = os_updates.values().copied().collect();
        core.first_calibration_ids = first_calibration_ids;
        core.first_calibration_unix = unix_seconds();
        if pending.state.exclusion_updates_deferred {
            core.exclusion_decay_ids = participant_ids.clone();
            core.full_exclusion_increment_ids = pending.state.full_exclusion_increment_ids.clone();
            core.half_exclusion_increment_ids = pending.state.half_exclusion_increment_ids.clone();
        }
        if !pending.state.is_draft {
            core.effective_avoid_ids = pending.state.effective_avoid_ids.clone();
            core.effective_deal_ids = pending.state.effective_deal_ids.clone();
        }
        core.expected_openskill_revision = Some(expected_openskill_revision);
        core.expected_low_priority_ids = Some(low_priority.clone());
        core.win_reward_jc = Some(self.config.jopacoin_win_reward);
        core.settle_referrals_at = Some(unix_seconds());
        let match_id = matches
            .record_match_core_atomic(&core)
            .map_err(|error| error.to_string())?;

        let winning_bet_team = if winning_team == 1 {
            BettingTeam::Radiant
        } else {
            BettingTeam::Dire
        };
        let betting_mode = if pending.state.betting_mode == "house" {
            SeedBettingMode::House
        } else {
            SeedBettingMode::Pool
        };
        let leverage_by_bet = self
            .bets
            .get_pending_bets(
                Some(pending.guild_id),
                None,
                0,
                Some(pending.pending_match_id),
            )
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|bet| (bet.bet_id, bet.leverage.max(1)))
            .collect::<BTreeMap<_, _>>();
        let settled = self
            .bets
            .settle_pending_bets_with_adjustments_atomic(
                match_id,
                Some(pending.guild_id),
                pending.state.shuffle_timestamp.unwrap_or_default(),
                Some(pending.pending_match_id),
                winning_bet_team,
                betting_mode,
                &BetSettlementAdjustments {
                    bankruptcy_kept_rate: Some(self.config.bankruptcy_kept_rate),
                    vanity_tax_rate: Some(self.config.vanity_tax_rate),
                    vanity_taxable_ids: self.rewards.vanity_tax.taxable_ids(pending.guild_id),
                    house_payout_multiplier: self.config.house_payout_multiplier,
                    payout_multiplier: self
                        .economy
                        .effects_at(pending.guild_id, unix_seconds())
                        .map_or(1.0, |effects| effects.bet_payout_multiplier),
                    consume_seed: true,
                    ..BetSettlementAdjustments::default()
                },
            )
            .map_err(|error| error.to_string())?;

        // Bet settlement commits before Match's aggregate reward snapshot.
        // Persist the betting-owned component immediately so a later snapshot
        // read/write failure leaves a durable retry anchor. On an idempotent
        // Match-core retry, there are no pending rows left; reconstruct the
        // original distribution from the tagged bets and immutable ledger
        // instead of treating the retry as a no-bet match.
        let settlement = if settled.winners.is_empty() && settled.losers.is_empty() {
            self.bets
                .recover_settlement_for_match(match_id, Some(pending.guild_id))
                .map_err(|error| error.to_string())?
        } else {
            settled
        };
        let leverage_by_bet = if leverage_by_bet.is_empty() {
            self.bets
                .get_match_bet_leverages(match_id, Some(pending.guild_id))
                .map_err(|error| error.to_string())?
        } else {
            leverage_by_bet
        };
        self.bets
            .persist_settlement_bet_jc_changes(match_id, Some(pending.guild_id), &settlement)
            .map_err(|error| error.to_string())?;

        let debrief_channel_id = pending
            .state
            .origin_channel_id
            .or(pending.state.cmd_shuffle_channel_id)
            .or(pending.state.shuffle_channel_id)
            .and_then(|channel_id| u64::try_from(channel_id).ok());
        let bet_settlement = self.build_bet_settlement_request(
            pending.guild_id,
            match_id,
            debrief_channel_id,
            &settlement,
            &leverage_by_bet,
        )?;
        let debrief = self.build_post_match_debrief(
            match_id,
            pending.guild_id,
            winners,
            &settlement,
            debrief_channel_id,
        );

        let jc_lines =
            self.settle_match_rewards(match_id, pending, winners, losers, &settlement)?;
        LoanService::new(
            LoanRepository::new(&self.database_path),
            LoanPolicy {
                cooldown_seconds: self.config.loan_cooldown_seconds,
                max_amount: self.config.loan_max_amount,
                fee_rate: self.config.loan_fee_rate,
                max_debt: self.config.max_debt,
            },
        )
        .repay_outstanding_match_loans(&participant_ids, Some(pending.guild_id))
        .map_err(|error| error.to_string())?;
        let easter_eggs = self.collect_match_easter_eggs(
            &participant_ids,
            &pending.state.radiant_team_ids,
            &pending.state.dire_team_ids,
            winners,
            &streak_data,
            persisted_streak_records.as_deref(),
            pending.guild_id,
            match_id,
            debrief_channel_id,
        )?;
        let streak_records = easter_eggs.win_streak_records.clone();
        self.pending
            .mutate_pending_match(pending.guild_id, pending.pending_match_id, |state| {
                persist_easter_streaks_in_state(state, &streak_records)
            })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "pending match {} disappeared before Easter payload persistence",
                    pending.pending_match_id
                )
            })?;
        self.apply_easter_streak_records(pending.guild_id, &streak_records)?;
        Ok(RecordedMatch {
            match_id,
            jc_lines,
            bet_settlement: (!bet_settlement.participants.is_empty()).then_some(bet_settlement),
            easter_eggs,
            debrief,
        })
    }

    fn settle_match_rewards(
        &self,
        match_id: i64,
        pending: &PendingMatchRecord,
        winners: &[i64],
        losers: &[i64],
        settlement: &SettlementResult,
    ) -> Result<Vec<String>, String> {
        let matches = MatchRepository::new(&self.database_path);
        let mut jc_changes = matches
            .get_match(match_id, Some(pending.guild_id))
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("recorded match {match_id} disappeared"))?
            .jc_changes;
        let bet_skims =
            self.rewards
                .apply_bet_blood_pacts(pending.guild_id, match_id, settlement)?;
        let has_distribution = !settlement.winners.is_empty() || !settlement.losers.is_empty();
        if has_distribution {
            for (discord_id, delta) in bet_jc_deltas(settlement, &bet_skims) {
                jc_changes
                    .entry(discord_id)
                    .or_default()
                    .insert("bet".to_owned(), delta);
            }
        }
        for (discord_id, skimmed) in bet_skims {
            let components = jc_changes.entry(discord_id).or_default();
            if !components.contains_key("bet_blood_pact_skim") {
                if !has_distribution {
                    components
                        .entry("bet".to_owned())
                        .and_modify(|delta| *delta = delta.saturating_sub(skimmed))
                        .or_insert(-skimmed);
                }
                components.insert("bet_blood_pact_skim".to_owned(), skimmed);
            }
        }
        matches
            .update_match_jc_changes(match_id, Some(pending.guild_id), &jc_changes)
            .map_err(|error| error.to_string())?;

        let claimed = matches
            .claim_match_bonuses_paid(match_id, Some(pending.guild_id))
            .map_err(|error| error.to_string())?;

        let mut bonus_net = BTreeMap::<i64, i64>::new();
        let mut compensatable_net = BTreeMap::<i64, i64>::new();
        let mut compensatable_by_source = BTreeMap::<(i64, String), i64>::new();
        let mut bonus_components = BTreeMap::<i64, BTreeMap<String, i64>>::new();
        let mut win_bonus_by_player = BTreeMap::new();
        // Python compensates a claimed bonus block when a win award fails
        // after participation has already committed.  Later failures (for
        // example, an unavailable exclusion recipient) retain the durable
        // exact-once phases so the existing post-core recovery path remains
        // resumable without replaying bets or bonuses.
        let mut compensate_partial = false;
        let payout = (|| -> Result<(), String> {
            let participation = self.rewards.award_generated_batch(GeneratedRewardBatch {
                guild_id: pending.guild_id,
                player_ids: losers,
                gross: self
                    .config
                    .jopacoin_per_game
                    .saturating_add(if pending.state.is_bomb_pot {
                        self.config.bomb_pot_participation_bonus
                    } else {
                        0
                    }),
                apply_bankruptcy_penalty: false,
                apply_vanity_tax: false,
                source: "match_participation",
                related_type: "match",
                related_id: match_id,
                reason: "match participation bonus",
                event_nonce_tag: 1,
            })?;
            accumulate_generated_rewards(
                &participation,
                "payout",
                &mut bonus_net,
                &mut bonus_components,
            );
            accumulate_generated_compensation(
                &participation,
                "match_participation",
                &mut compensatable_net,
                &mut compensatable_by_source,
            );
            compensate_partial = true;

            if pending.state.is_bomb_pot {
                let bomb = self.rewards.award_generated_batch(GeneratedRewardBatch {
                    guild_id: pending.guild_id,
                    player_ids: winners,
                    gross: self.config.bomb_pot_participation_bonus,
                    apply_bankruptcy_penalty: false,
                    apply_vanity_tax: false,
                    source: "match_participation",
                    related_type: "match",
                    related_id: match_id,
                    reason: "bomb-pot participation bonus",
                    event_nonce_tag: 2,
                })?;
                accumulate_generated_rewards(
                    &bomb,
                    "payout",
                    &mut bonus_net,
                    &mut bonus_components,
                );
                accumulate_generated_compensation(
                    &bomb,
                    "match_participation",
                    &mut compensatable_net,
                    &mut compensatable_by_source,
                );
            }

            for player_id in winners {
                let outcome = self.rewards.award_match_win(CorrectionWinRewardRequest {
                    guild_id: pending.guild_id,
                    match_id,
                    discord_id: *player_id,
                    gross_reward: self.config.jopacoin_win_reward,
                })?;
                let net = outcome
                    .receipt
                    .net
                    .saturating_add(outcome.communion_bonus)
                    .saturating_sub(outcome.blood_pact_skim);
                accumulate_reward_component(
                    *player_id,
                    "payout",
                    net,
                    outcome.snapshot_balance_delta(),
                    &mut bonus_net,
                    &mut bonus_components,
                );
                win_bonus_by_player.insert(*player_id, outcome.snapshot_balance_delta());
                accumulate_compensatable_net(
                    *player_id,
                    outcome.compensatable_net(),
                    &mut compensatable_net,
                );
                accumulate_compensatable_source(
                    *player_id,
                    "match_win_bonus",
                    outcome.compensatable_net(),
                    &mut compensatable_by_source,
                );
            }
            compensate_partial = false;

            let exclusions = self.rewards.award_generated_batch(GeneratedRewardBatch {
                guild_id: pending.guild_id,
                player_ids: &pending.state.excluded_player_ids,
                gross: self.config.jopacoin_exclusion_reward,
                apply_bankruptcy_penalty: true,
                apply_vanity_tax: true,
                source: "match_exclusion",
                related_type: "match",
                related_id: match_id,
                reason: "match exclusion bonus",
                event_nonce_tag: 3,
            })?;
            accumulate_generated_rewards(
                &exclusions,
                "payout",
                &mut bonus_net,
                &mut bonus_components,
            );
            accumulate_generated_compensation(
                &exclusions,
                "match_exclusion",
                &mut compensatable_net,
                &mut compensatable_by_source,
            );

            self.award_match_streaks(
                match_id,
                pending.guild_id,
                &winners.iter().chain(losers).copied().collect::<Vec<_>>(),
                &mut bonus_net,
                &mut bonus_components,
                &mut compensatable_net,
            )?;
            for (discord_id, components) in &bonus_components {
                let target = jc_changes.entry(*discord_id).or_default();
                for (component, amount) in components {
                    target.insert(component.clone(), *amount);
                }
            }
            matches
                .update_match_jc_changes(match_id, Some(pending.guild_id), &jc_changes)
                .map_err(|error| error.to_string())?;
            MatchCorrectionRepository::new(&self.database_path)
                .update_participant_bonus_jc(
                    match_id,
                    Some(pending.guild_id),
                    &bonus_net,
                    &win_bonus_by_player,
                )
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if let Err(error) = payout {
            if compensate_partial {
                let compensations = compensatable_by_source
                    .iter()
                    .map(|((discord_id, source), amount)| IncomeAwardCompensation {
                        discord_id: *discord_id,
                        source,
                        related_id: match_id,
                        amount: *amount,
                    })
                    .collect::<Vec<_>>();
                MatchRecordingRepository::new(&self.database_path)
                    .compensate_income_awards_atomic(
                        Some(pending.guild_id),
                        &compensations,
                    )
                    .map_err(|compensation_error| {
                        format!(
                            "match bonus payout failed: {error}; compensation failed: {compensation_error}"
                        )
                    })?;
            }
            // Every phase above owns an immutable event key. A win-phase
            // failure was compensated above, so its source-scoped retry
            // markers are reopened. Later-phase failures retain successful
            // phases and re-enter the saga without double-paying them.
            matches
                .release_match_bonuses_claim(match_id, Some(pending.guild_id))
                .map_err(|release_error| {
                    format!(
                        "match bonus payout failed: {error}; releasing the bonus claim failed: {release_error}"
                    )
                })?;
            let durable_partial = compensatable_net
                .values()
                .copied()
                .fold(0_i64, i64::saturating_add);
            warn!(
                match_id,
                claimed,
                durable_partial,
                %error,
                "match bonus payout paused for exact-once recovery"
            );
            return Err(format!("match bonus payout failed: {error}"));
        }
        Ok(render_jc_lines(
            winners,
            losers,
            &pending.state.excluded_player_ids,
            &jc_changes,
            settlement,
        ))
    }

    fn award_match_streaks(
        &self,
        match_id: i64,
        guild_id: i64,
        participant_ids: &[i64],
        bonus_net: &mut BTreeMap<i64, i64>,
        bonus_components: &mut BTreeMap<i64, BTreeMap<String, i64>>,
        compensatable_net: &mut BTreeMap<i64, i64>,
    ) -> Result<(), String> {
        let today = cama_domain::game_date::get_game_date();
        let yesterday = previous_game_date(&today).map_err(|error| error.to_string())?;
        let repository = DotaStreakRepository::new(&self.database_path);
        let streaks = repository
            .advance_bulk_atomic(participant_ids, Some(guild_id), &today, &yesterday)
            .map_err(|error| error.to_string())?;
        let credits = participant_ids
            .iter()
            .copied()
            .zip(streaks)
            .filter_map(|(discord_id, streak_days)| {
                let bonus = dota_streak_bonus(streak_days);
                (bonus > 0).then_some(DotaStreakCredit {
                    discord_id,
                    streak_days,
                    bonus,
                })
            })
            .collect::<Vec<_>>();
        let now = unix_seconds();
        let targets = BuffService::new(ManashopRepository::new(&self.database_path), now)
            .blood_pact_targets(
                &credits
                    .iter()
                    .map(|credit| credit.discord_id)
                    .collect::<Vec<_>>(),
                guild_id,
            )
            .unwrap_or_default();
        if targets.is_empty() {
            let outcomes = repository
                .credit_match_awards_ordered_atomic(&credits, Some(guild_id), match_id)
                .map_err(|error| error.to_string())?;
            for (credit, outcome) in credits.into_iter().zip(outcomes) {
                accumulate_reward_component(
                    credit.discord_id,
                    "streak",
                    credit.bonus,
                    credit.bonus,
                    bonus_net,
                    bonus_components,
                );
                if outcome.applied {
                    accumulate_compensatable_net(
                        credit.discord_id,
                        credit.bonus,
                        compensatable_net,
                    );
                }
            }
            return Ok(());
        }
        for credit in credits {
            let credited = repository
                .credit_match_awards_ordered_atomic(&[credit], Some(guild_id), match_id)
                .map_err(|error| error.to_string())?;
            let skim = if targets.contains(&credit.discord_id) {
                self.rewards
                    .apply_attributed_blood_pact(
                        credit.discord_id,
                        guild_id,
                        credit.bonus,
                        format!("match-streak-blood-pact:{match_id}:{}", credit.discord_id),
                        now,
                    )
                    .unwrap_or_default()
            } else {
                0
            };
            let net = credit.bonus.saturating_sub(skim);
            accumulate_reward_component(
                credit.discord_id,
                "streak",
                net,
                net,
                bonus_net,
                bonus_components,
            );
            if credited.first().is_some_and(|outcome| outcome.applied) {
                accumulate_compensatable_net(credit.discord_id, net, compensatable_net);
            }
        }
        Ok(())
    }

    fn prepare_shuffle(&self, request: PrepareShuffleRequest) -> Result<PreparedShuffle, String> {
        if !matches!(
            request.rating_system.as_str(),
            "glicko" | "openskill" | "jopacoin"
        ) {
            return Err("rating_system must be 'glicko', 'openskill', or 'jopacoin'".to_owned());
        }
        if !matches!(request.shuffle_mode.as_str(), "balanced" | "region") {
            return Err("shuffle_mode must be 'balanced' or 'region'".to_owned());
        }
        let ShuffleInputs {
            mut players,
            last_match_dates,
            exclusion_counts,
        } = self
            .players
            .get_shuffle_inputs(&request.player_ids, Some(request.guild_id))
            .map_err(|error| error.to_string())?;
        if players.len() != request.player_ids.len() {
            return Err(format!(
                "Could not load all players: expected {}, got {}",
                request.player_ids.len(),
                players.len()
            ));
        }
        if players.len() < 10 {
            return Err("Need at least 10 players to shuffle.".to_owned());
        }

        let now = Utc::now();
        for player in &mut players {
            let Some(player_id) = player.discord_id else {
                continue;
            };
            let Some(last_match) = last_match_dates.get(&player_id).and_then(sql_datetime) else {
                continue;
            };
            let days_since = (now - last_match).num_days().max(0);
            if let Some(rd) = player.glicko_rd {
                player.glicko_rd = Some(
                    self.rating
                        .apply_rd_decay(rd, i32::try_from(days_since).unwrap_or(i32::MAX)),
                );
            }
            if let Some(sigma) = player.os_sigma {
                player.os_sigma = Some(self.openskill.apply_sigma_decay(sigma, days_since));
            }
        }

        let mut rating_system = request.rating_system.clone();
        if rating_system == "openskill" && players.iter().any(|player| player.os_mu.is_none()) {
            rating_system = "glicko".to_owned();
        }
        let use_openskill = rating_system == "openskill";
        let use_jopacoin = rating_system == "jopacoin";
        let avoids = self
            .avoids
            .get_active_avoids_for_players(Some(request.guild_id), &request.player_ids)
            .map_err(|error| error.to_string())?;
        let deals = self
            .deals
            .get_active_deals_for_players(Some(request.guild_id), &request.player_ids)
            .map_err(|error| error.to_string())?;
        let low_priority_ids = self
            .low_priority
            .get_active_ids(&request.player_ids, Some(request.guild_id))
            .map_err(|error| error.to_string())?;
        let domain_avoids = avoids
            .iter()
            .map(|avoid| SoftAvoid {
                avoider_discord_id: avoid.avoider_discord_id,
                avoided_discord_id: avoid.avoided_discord_id,
            })
            .collect::<Vec<_>>();
        let domain_deals = deals
            .iter()
            .map(|deal| PackageDeal {
                buyer_discord_id: deal.buyer_discord_id,
                partner_discord_id: deal.partner_discord_id,
            })
            .collect::<Vec<_>>();
        let constraints = ShuffleConstraints {
            avoids: Some(&domain_avoids),
            deals: Some(&domain_deals),
            low_priority_ids: Some(&low_priority_ids),
        };

        let mut shuffler = BalancedShuffler::default();
        shuffler.use_glicko = true;
        shuffler.use_openskill = use_openskill;
        shuffler.use_jopacoin = use_jopacoin;
        shuffler.off_role_multiplier = self.config.off_role_multiplier;
        shuffler.off_role_flat_value_penalty = self.config.off_role_flat_value_penalty;
        shuffler.off_role_flat_penalty = self.config.off_role_flat_penalty;
        shuffler.role_matchup_delta_weight = self.config.role_matchup_delta_weight;
        shuffler.exclusion_penalty_weight = self.config.exclusion_penalty_weight;
        shuffler.rd_priority_weight = self.config.rd_priority_weight;
        shuffler.recent_match_penalty_weight = self.config.recent_match_penalty_weight;
        shuffler.soft_avoid_penalty = self.config.soft_avoid_penalty;
        shuffler.package_deal_penalty = self.config.package_deal_penalty;
        shuffler.package_deal_split_penalty = self.config.package_deal_split_penalty;
        shuffler.rating_spread_divisor = self.config.rating_spread_divisor;
        shuffler.region_split = request.shuffle_mode == "region";
        shuffler.region_split_penalty = self.config.region_split_penalty;

        let recent_match_ids = last_match_participant_ids(&self.database_path, request.guild_id)?;
        let recent_match_names = players
            .iter()
            .filter(|player| {
                player
                    .discord_id
                    .is_some_and(|id| recent_match_ids.contains(&id))
            })
            .map(|player| player.name.clone())
            .collect::<HashSet<_>>();
        let exclusion_counts = players
            .iter()
            .filter_map(|player| {
                player.discord_id.map(|player_id| {
                    (
                        player.name.clone(),
                        usize::try_from(
                            exclusion_counts
                                .get(&player_id)
                                .copied()
                                .unwrap_or_default()
                                .max(0),
                        )
                        .unwrap_or(usize::MAX),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let result = shuffler
            .shuffle_from_pool(
                &players,
                PoolOptions {
                    exclusion_counts: Some(&exclusion_counts),
                    recent_match_names: Some(&recent_match_names),
                    constraints,
                    lobby_wait_minutes: Some(&request.lobby_wait_minutes),
                    sampling_seed: fastrand::u64(..),
                },
            )
            .map_err(|error| error.to_string())?;
        let (radiant_team, dire_team) = if fastrand::bool() {
            (result.team1, result.team2)
        } else {
            (result.team2, result.team1)
        };
        let excluded_players = result.excluded;
        let radiant_ids = team_player_ids(&radiant_team)?;
        let dire_ids = team_player_ids(&dire_team)?;
        let excluded_ids = player_ids(&excluded_players);
        let radiant_roles = radiant_team
            .role_assignments
            .clone()
            .ok_or_else(|| "Radiant role assignments were not produced".to_owned())?;
        let dire_roles = dire_team
            .role_assignments
            .clone()
            .ok_or_else(|| "Dire role assignments were not produced".to_owned())?;
        let radiant_value = radiant_team
            .get_team_value_with_off_role_value_penalty(
                true,
                use_openskill,
                use_jopacoin,
                shuffler.off_role_flat_value_penalty,
            )
            .map_err(|error| error.to_string())?;
        let dire_value = dire_team
            .get_team_value_with_off_role_value_penalty(
                true,
                use_openskill,
                use_jopacoin,
                shuffler.off_role_flat_value_penalty,
            )
            .map_err(|error| error.to_string())?;
        let value_diff = (radiant_value - dire_value).abs();

        let balancing = TeamBalancingService::new_with_off_role_value_penalty(
            true,
            shuffler.off_role_multiplier,
            shuffler.off_role_flat_value_penalty,
            shuffler.off_role_flat_penalty,
            shuffler.role_matchup_delta_weight,
        );
        let off_role_penalty = (radiant_team
            .get_off_role_count()
            .map_err(|error| error.to_string())?
            + dire_team
                .get_off_role_count()
                .map_err(|error| error.to_string())?) as f64
            * shuffler.off_role_flat_penalty;
        let weighted_parity_delta = (balancing
            .calculate_role_matchup_delta(&radiant_team, &dire_team, use_openskill, use_jopacoin)
            .map_err(|error| error.to_string())?
            + balancing
                .calculate_role_parity_delta(&radiant_team, &dire_team, use_openskill, use_jopacoin)
                .map_err(|error| error.to_string())?)
            * shuffler.role_matchup_delta_weight;
        let radiant_set = radiant_ids.iter().copied().collect::<HashSet<_>>();
        let dire_set = dire_ids.iter().copied().collect::<HashSet<_>>();
        let selected_set = radiant_set
            .union(&dire_set)
            .copied()
            .collect::<HashSet<_>>();
        let excluded_set = excluded_ids.iter().copied().collect::<HashSet<_>>();
        let excluded_penalty = excluded_players
            .iter()
            .map(|player| {
                exclusion_counts
                    .get(&player.name)
                    .copied()
                    .unwrap_or_default()
            })
            .sum::<usize>() as f64
            * shuffler.exclusion_penalty_weight;
        let recent_match_penalty = radiant_team
            .players
            .iter()
            .chain(&dire_team.players)
            .filter(|player| recent_match_names.contains(&player.name))
            .count() as f64
            * shuffler.recent_match_penalty_weight;
        let soft_avoid_penalty = shuffler.calculate_soft_avoid_penalty(
            &radiant_set,
            &dire_set,
            Some(&domain_avoids),
            Some(&low_priority_ids),
        );
        let package_deal_penalty = shuffler.calculate_package_deal_penalty(
            &radiant_set,
            &dire_set,
            Some(&domain_deals),
            Some(&low_priority_ids),
        );
        let deal_split_penalty = shuffler.calculate_package_deal_split_penalty(
            &selected_set,
            &excluded_set,
            Some(&domain_deals),
            Some(&low_priority_ids),
        );
        let low_priority_penalty = BalancedShuffler::calculate_low_priority_penalty(
            &selected_set,
            Some(&low_priority_ids),
        );
        let low_priority_team_adjustment = BalancedShuffler::calculate_low_priority_team_adjustment(
            &radiant_set,
            &dire_set,
            Some(&low_priority_ids),
        );
        let region_split_penalty = if request.shuffle_mode == "region" {
            region_split_mismatches(
                &region_inputs(&radiant_team.players),
                &region_inputs(&dire_team.players),
            ) as f64
                * shuffler.region_split_penalty
        } else {
            0.0
        };
        let selected_players = radiant_team
            .players
            .iter()
            .chain(&dire_team.players)
            .collect::<Vec<_>>();
        let selected_values = selected_players
            .iter()
            .map(|player| player.get_value(true, use_openskill, use_jopacoin))
            .collect::<Vec<_>>();
        let goodness_score = value_diff
            + off_role_penalty
            + weighted_parity_delta
            + excluded_penalty
            + recent_match_penalty
            + soft_avoid_penalty
            + package_deal_penalty
            + deal_split_penalty
            + low_priority_penalty
            + low_priority_team_adjustment
            + region_split_penalty
            + shuffler.calculate_rating_spread_penalty(&selected_values)
            - BalancedShuffler::calculate_lobby_rating_bonus(&selected_values)
            - BalancedShuffler::calculate_lobby_wait_bonus(
                &selected_players,
                Some(&request.lobby_wait_minutes),
            )
            - shuffler.calculate_rd_priority(&selected_players);

        let glicko_radiant_win_prob =
            glicko_probability(&self.rating, &radiant_team.players, &dire_team.players);
        let openskill_radiant_win_prob =
            openskill_probability(&self.openskill, &radiant_team.players, &dire_team.players)?;
        let included_ids = selected_set;
        let effective_avoid_ids = avoids
            .iter()
            .filter(|avoid| {
                included_ids.contains(&avoid.avoider_discord_id)
                    && included_ids.contains(&avoid.avoided_discord_id)
                    && ((radiant_set.contains(&avoid.avoider_discord_id)
                        && dire_set.contains(&avoid.avoided_discord_id))
                        || (dire_set.contains(&avoid.avoider_discord_id)
                            && radiant_set.contains(&avoid.avoided_discord_id)))
            })
            .map(|avoid| avoid.id)
            .collect();
        let effective_deal_ids = deals
            .iter()
            .filter(|deal| {
                (radiant_set.contains(&deal.buyer_discord_id)
                    && radiant_set.contains(&deal.partner_discord_id))
                    || (dire_set.contains(&deal.buyer_discord_id)
                        && dire_set.contains(&deal.partner_discord_id))
            })
            .map(|deal| deal.id)
            .collect();

        let mut state = PendingMatchState {
            radiant_team_ids: radiant_ids.clone(),
            dire_team_ids: dire_ids.clone(),
            excluded_player_ids: excluded_ids,
            excluded_conditional_player_ids: request.excluded_conditional_ids.clone(),
            radiant_roles,
            dire_roles,
            radiant_value,
            dire_value,
            value_diff,
            first_pick_team: Some(first_pick_team(fastrand::bool()).to_owned()),
            shuffle_timestamp: Some(request.shuffle_timestamp),
            bet_lock_until: Some(
                request
                    .shuffle_timestamp
                    .checked_add(self.config.bet_lock_seconds)
                    .ok_or_else(|| "betting lock timestamp overflow".to_owned())?,
            ),
            betting_mode: "pool".to_owned(),
            is_bomb_pot: request.is_bomb_pot,
            is_openskill_shuffle: use_openskill,
            balancing_rating_system: rating_system,
            lobby_kind: Some(lobby_kind_value(request.lobby_kind).to_owned()),
            effective_avoid_ids,
            effective_deal_ids,
            exclusion_updates_deferred: true,
            full_exclusion_increment_ids: excluded_set.into_iter().collect(),
            half_exclusion_increment_ids: request.excluded_conditional_ids,
            ..PendingMatchState::default()
        };
        state
            .extra
            .insert("shuffle_mode".to_owned(), json!(request.shuffle_mode));
        state
            .extra
            .insert("goodness_score".to_owned(), json!(goodness_score));
        state.extra.insert(
            "glicko_radiant_win_prob".to_owned(),
            json!(glicko_radiant_win_prob),
        );
        state.extra.insert(
            "openskill_radiant_win_prob".to_owned(),
            json!(openskill_radiant_win_prob),
        );
        let mut pending = self
            .pending
            .create_pending_match(request.guild_id, &state)
            .map_err(|error| error.to_string())?;

        let mut first_game_reserved = 0;
        if self.config.first_game_pool_daily_amount > 0 {
            let game_date = cama_domain::game_date::get_game_date();
            self.seeds
                .fund_first_game_pools(
                    Some(request.guild_id),
                    &game_date,
                    self.config.first_game_pool_daily_amount,
                )
                .map_err(|error| error.to_string())?;
            first_game_reserved = self
                .seeds
                .claim_first_game_pool(
                    Some(request.guild_id),
                    match request.lobby_kind {
                        LobbyKind::Open => FirstGameLobby::Open,
                        LobbyKind::LowSkill => FirstGameLobby::LowSkill,
                    },
                    &game_date,
                    pending.pending_match_id,
                )
                .map_err(|error| error.to_string())?;
        }
        self.seeds
            .reserve_seed_atomic(
                Some(request.guild_id),
                pending.pending_match_id,
                self.config.dota_bet_seed_amount,
                first_game_reserved,
                SeedBettingMode::Pool,
            )
            .map_err(|error| error.to_string())?;
        pending = self
            .pending
            .pending_match(request.guild_id, pending.pending_match_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "pending match disappeared after seed reservation".to_owned())?;

        // Python snapshots configured-investment eligibility before the blind
        // batch debits wallets, then sizes each position from the post-blind
        // balance. Keep that two-snapshot contract across the DB hook rather
        // than letting the blind transaction change the 50-JC gate.
        let investment_starting_balances = {
            let target_ids = radiant_ids
                .iter()
                .chain(dire_ids.iter())
                .copied()
                .collect::<Vec<_>>();
            let investments = AutobetInvestmentRepository::new(&self.database_path);
            match investments.for_targets(Some(request.guild_id), &target_ids) {
                Ok(positions) => {
                    let investor_ids = positions
                        .iter()
                        .map(|position| position.investor_id)
                        .collect::<Vec<_>>();
                    match self
                        .players
                        .get_balances_bulk(&investor_ids, Some(request.guild_id))
                    {
                        Ok(balances) => balances.into_iter().collect::<BTreeMap<_, _>>(),
                        Err(error) => {
                            warn!(
                                %error,
                                pending_match_id = pending.pending_match_id,
                                "configured-investment starting-balance snapshot failed"
                            );
                            BTreeMap::new()
                        }
                    }
                }
                Err(error) => {
                    warn!(
                        %error,
                        pending_match_id = pending.pending_match_id,
                        "configured-investment position snapshot failed"
                    );
                    BTreeMap::new()
                }
            }
        };

        let blind_bets = if self.config.auto_blind_enabled {
            let candidates = radiant_ids
                .iter()
                .copied()
                .map(|discord_id| BlindBetCandidate {
                    discord_id,
                    team: BettingTeam::Radiant,
                    ante_override: None,
                })
                .chain(
                    dire_ids
                        .iter()
                        .copied()
                        .map(|discord_id| BlindBetCandidate {
                            discord_id,
                            team: BettingTeam::Dire,
                            ante_override: None,
                        }),
                )
                .collect::<Vec<_>>();
            match self.bets.create_auto_blind_bets_atomic(
                Some(request.guild_id),
                Some(pending.pending_match_id),
                request.shuffle_timestamp,
                &candidates,
                BlindBetPolicy {
                    is_bomb_pot: request.is_bomb_pot,
                    normal_threshold: self.config.auto_blind_threshold,
                    normal_percentage: percentage_points(self.config.auto_blind_percentage),
                    bomb_percentage: percentage_points(self.config.bomb_pot_blind_percentage),
                    bomb_ante: self.config.bomb_pot_ante,
                    max_debt: self.config.max_debt,
                },
            ) {
                Ok(outcome) => Some(outcome),
                Err(error) => {
                    warn!(
                        %error,
                        pending_match_id = pending.pending_match_id,
                        "automatic blind bets failed"
                    );
                    None
                }
            }
        } else {
            None
        };

        // Python's `create_match_automatic_bets` runs the three automatic
        // liquidity policies in this order: blind bets, configured
        // long/short investments, then spectator balancing.  Each hook is
        // best-effort after the durable pending row exists; one stale wallet
        // or candidate must not roll back the published match.  The DB hooks
        // use their own immediate transaction and are idempotent for this
        // pending-match identity, so READY recovery can safely retry them.
        let mut automatic_summary = blind_bets
            .as_ref()
            .map(|outcome| blind_outcome_json(outcome, self.config.clone(), request.is_bomb_pot))
            .unwrap_or_else(|| {
                json!({
                    "created": 0,
                    "total_radiant": 0,
                    "total_dire": 0,
                    "percentage": if request.is_bomb_pot {
                        self.config.bomb_pot_blind_percentage
                    } else {
                        self.config.auto_blind_percentage
                    },
                    "is_bomb_pot": request.is_bomb_pot,
                    "bets": [],
                    "skipped": [],
                })
            });

        match self
            .bets
            .create_configured_investment_bets_for_shuffle(ConfiguredInvestmentRequest {
                guild_id: Some(request.guild_id),
                pending_match_id: Some(pending.pending_match_id),
                bet_time: request.shuffle_timestamp,
                radiant_ids: radiant_ids.clone(),
                dire_ids: dire_ids.clone(),
                max_debt: self.config.max_debt,
                starting_balances: investment_starting_balances,
            }) {
            Ok(outcome) => {
                let target_ids = outcome
                    .created
                    .iter()
                    .filter_map(|bet| bet.investment_target_id)
                    .collect::<BTreeSet<_>>();
                let investment_metadata = AutobetInvestmentRepository::new(&self.database_path)
                    .for_targets(
                        Some(request.guild_id),
                        &target_ids.iter().copied().collect::<Vec<_>>(),
                    )
                    .map(|positions| {
                        positions
                            .into_iter()
                            .map(|position| {
                                (
                                    (position.investor_id, position.target_id, position.direction),
                                    position.percentage,
                                )
                            })
                            .collect::<BTreeMap<_, _>>()
                    })
                    .unwrap_or_default();
                automatic_summary["investment_bets"] =
                    automatic_outcome_json(&outcome, &BTreeMap::new(), &investment_metadata);
            }
            Err(error) => warn!(
                %error,
                pending_match_id = pending.pending_match_id,
                "configured-investment hook failed"
            ),
        }

        if self.config.auto_spectator_bet_enabled {
            let candidates = match self.players.get_all(Some(request.guild_id)) {
                Ok(players) => players
                    .into_iter()
                    .filter_map(|player| {
                        player.discord_id.map(|discord_id| WealthSnapshot {
                            discord_id,
                            balance: player.jopacoin_balance,
                        })
                    })
                    .collect::<Vec<_>>(),
                Err(error) => {
                    warn!(
                        %error,
                        pending_match_id = pending.pending_match_id,
                        "automatic spectator candidate lookup failed"
                    );
                    Vec::new()
                }
            };
            let participant_ids = radiant_ids
                .iter()
                .chain(dire_ids.iter())
                .copied()
                .collect::<BTreeSet<_>>();
            let plan = plan_automatic_spectator_bets(
                &candidates,
                &participant_ids,
                AutomaticSpectatorConfig {
                    enabled: true,
                    total_count: self.config.auto_spectator_bet_count,
                    top_count: self.config.auto_spectator_bet_top_count,
                    base_basis_points: percentage_basis_points(
                        self.config.auto_spectator_bet_percentage,
                    ),
                    top_basis_points: percentage_basis_points(
                        self.config.auto_spectator_bet_top_percentage,
                    ),
                },
            );
            let requests = plan
                .bets
                .iter()
                .map(|bet| AutomaticBasisPointBetRequest {
                    discord_id: bet.discord_id,
                    team: bet.team,
                    basis_points: bet.basis_points,
                })
                .collect::<Vec<_>>();
            match self.bets.place_automatic_basis_point_bets_atomic(
                Some(request.guild_id),
                Some(pending.pending_match_id),
                request.shuffle_timestamp,
                &requests,
                self.config.max_debt,
            ) {
                Ok(outcome) => {
                    automatic_summary["spectator_bets"] = automatic_outcome_json(
                        &outcome,
                        &plan
                            .bets
                            .iter()
                            .map(|bet| (bet.discord_id, (bet.networth, bet.basis_points)))
                            .collect::<BTreeMap<_, _>>(),
                        &BTreeMap::new(),
                    );
                }
                Err(error) => warn!(
                    %error,
                    pending_match_id = pending.pending_match_id,
                    "spectator auto-wager hook failed"
                ),
            }
        }

        let has_automatic_bets = automatic_summary
            .get("created")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default()
            > 0
            || automatic_summary
                .get("investment_bets")
                .and_then(|value| value.get("created"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                > 0
            || automatic_summary
                .get("spectator_bets")
                .and_then(|value| value.get("created"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                > 0;
        if has_automatic_bets {
            pending = self
                .pending
                .mutate_pending_match(request.guild_id, pending.pending_match_id, move |state| {
                    state.blind_bets_result = Some(automatic_summary)
                })
                .map_err(|error| error.to_string())?
                .map(|(record, ())| record)
                .ok_or_else(|| "pending match disappeared after automatic bets".to_owned())?;
        }

        Ok(PreparedShuffle { pending })
    }

    async fn finalize_shuffle(
        &self,
        context: MatchCommandContext,
        responder: Arc<dyn InteractionResponder>,
        snapshot: MatchLobbySnapshot,
        mut prepared: PreparedShuffle,
    ) -> Result<(), InteractionHandlerError> {
        let guild_id =
            u64::try_from(context.guild_id).map_err(|_| "guild ID is outside Discord's range")?;
        let guild_id_i64 = context.guild_id;
        let full_lobby_ids = prepared.pending.state.full_lobby_player_ids();
        let real_ids = full_lobby_ids
            .iter()
            .copied()
            .filter(|player_id| *player_id > 0)
            .filter_map(|player_id| u64::try_from(player_id).ok())
            .collect::<Vec<_>>();
        let streaming_ids = if self.config.streaming_bonus > 0 {
            self.discord
                .streaming_member_ids(guild_id, &real_ids)
                .await
                .unwrap_or_default()
        } else {
            BTreeSet::new()
        };
        if !streaming_ids.is_empty() {
            let player_ids = streaming_ids
                .iter()
                .filter_map(|player_id| i64::try_from(*player_id).ok())
                .collect::<Vec<_>>();
            if let Err(error) = self.rewards.award_generated_batch(GeneratedRewardBatch {
                guild_id: context.guild_id,
                player_ids: &player_ids,
                gross: self.config.streaming_bonus,
                apply_bankruptcy_penalty: true,
                apply_vanity_tax: true,
                source: "shuffle_streaming_bonus",
                related_type: "pending_match",
                related_id: prepared.pending.pending_match_id,
                reason: "shuffle streaming bonus",
                event_nonce_tag: 5,
            }) {
                warn!(%error, "shuffle streaming bonus failed");
            }
            let persisted_streamers = streaming_ids
                .iter()
                .filter_map(|player_id| i64::try_from(*player_id).ok())
                .map(serde_json::Value::from)
                .collect::<Vec<_>>();
            let pending_repository = self.pending.clone();
            let pending_match_id = prepared.pending.pending_match_id;
            prepared.pending = tokio::task::spawn_blocking(move || {
                pending_repository.mutate_pending_match(
                    guild_id_i64,
                    pending_match_id,
                    move |state| {
                        state.extra.insert(
                            "shuffle_streaming_bonus_ids".to_owned(),
                            serde_json::Value::Array(persisted_streamers),
                        );
                    },
                )
            })
            .await
            .map_err(|error| format!("streaming metadata task failed: {error}"))?
            .map_err(|error| error.to_string())?
            .map(|(pending, ())| pending)
            .ok_or("pending match disappeared before publication")?;
        }

        let embed = self.render_shuffle_embed(&prepared.pending).await?;
        let public_response = InteractionResponse::message("").embed(embed.clone());
        let lobby_send = async {
            let channel_id = snapshot.lobby_channel_id?;
            match self
                .discord
                .send_message(channel_id, DiscordMessage::silent(public_response.clone()))
                .await
            {
                Ok(receipt) => Some(receipt),
                Err(error) => {
                    warn!(%error, channel_id, "shuffle lobby-channel publication failed");
                    None
                }
            }
        };
        let command_send = async {
            let channel_id = context.channel_id?;
            if Some(channel_id) == snapshot.lobby_channel_id {
                return None;
            }
            match self
                .discord
                .send_message(channel_id, DiscordMessage::silent(public_response.clone()))
                .await
            {
                Ok(receipt) => Some(receipt),
                Err(error) => {
                    warn!(%error, channel_id, "shuffle command-channel publication failed");
                    None
                }
            }
        };
        let confirmation = responder.followup(
            InteractionResponse::message(format!(
                "✅ {} teams shuffled!",
                snapshot.lobby_kind.label()
            ))
            .ephemeral(),
        );
        let (lobby_receipt, command_receipt, confirmation_result) =
            tokio::join!(lobby_send, command_send, confirmation);
        confirmation_result.map_err(|error| error.to_string())?;

        let origin_channel_id = snapshot.origin_channel_id;
        let pending_repository = self.pending.clone();
        let pending_match_id = prepared.pending.pending_match_id;
        let lobby_receipt_for_state = lobby_receipt.clone();
        let command_receipt_for_state = command_receipt.clone();
        prepared.pending = tokio::task::spawn_blocking(move || {
            pending_repository.mutate_pending_match(guild_id_i64, pending_match_id, move |state| {
                if let Some(receipt) = lobby_receipt_for_state {
                    state.shuffle_channel_id = i64::try_from(receipt.channel_id).ok();
                    state.shuffle_message_id = i64::try_from(receipt.message_id).ok();
                    state.shuffle_message_jump_url = Some(receipt.jump_url);
                }
                if let Some(receipt) = command_receipt_for_state {
                    state.cmd_shuffle_channel_id = i64::try_from(receipt.channel_id).ok();
                    state.cmd_shuffle_message_id = i64::try_from(receipt.message_id).ok();
                }
                state.origin_channel_id = origin_channel_id.and_then(|id| i64::try_from(id).ok());
            })
        })
        .await
        .map_err(|error| format!("publication metadata task failed: {error}"))?
        .map_err(|error| error.to_string())?
        .map(|(pending, ())| pending)
        .ok_or("pending match disappeared during publication")?;

        self.schedule_betting_reminders(&prepared.pending, true);
        let thread_publish = self.publish_shuffle_thread(&snapshot, &prepared.pending, embed);
        let unpin = self.lobbies.unpin_source_message(&snapshot);
        let (thread_result, unpin_result) = tokio::join!(thread_publish, unpin);
        if let Err(error) = thread_result {
            warn!(%error, "shuffle thread publication failed");
        }
        if let Err(error) = unpin_result {
            warn!(%error, "shuffle source unpin failed");
        }
        self.lobbies
            .reset_after_shuffle(context.guild_id, snapshot.lobby_kind)
            .await
            .map_err(Into::into)
            .map(|_| ())
    }

    async fn render_shuffle_embed(
        &self,
        pending: &PendingMatchRecord,
    ) -> Result<InteractionEmbed, InteractionHandlerError> {
        let participant_ids = pending
            .state
            .radiant_team_ids
            .iter()
            .chain(&pending.state.dire_team_ids)
            .copied()
            .collect::<Vec<_>>();
        let players_repository = self.players.clone();
        let guild_id = pending.guild_id;
        let mut player_ids = participant_ids.clone();
        player_ids.extend(&pending.state.excluded_player_ids);
        player_ids.sort_unstable();
        player_ids.dedup();
        let player_ids_for_query = player_ids.clone();
        let players = tokio::task::spawn_blocking(move || {
            players_repository.get_by_ids(&player_ids_for_query, Some(guild_id))
        })
        .await
        .map_err(|error| format!("shuffle embed player task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let players_by_id = players
            .iter()
            .filter_map(|player| player.discord_id.map(|id| (id, player)))
            .collect::<BTreeMap<_, _>>();
        let mut display_names = BTreeMap::new();
        if let Some(guild_id) = u64::try_from(pending.guild_id)
            .ok()
            .filter(|guild_id| *guild_id != 0)
        {
            for player_id in player_ids {
                let Ok(user_id) = u64::try_from(player_id) else {
                    continue;
                };
                if user_id == 0 {
                    continue;
                }
                match self
                    .discord
                    .cached_guild_member_display_name(guild_id, user_id)
                {
                    Ok(Some(display_name)) => {
                        display_names.insert(player_id, display_name);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        debug!(%error, guild_id, player_id, "shuffle player name lookup failed");
                    }
                }
            }
        }
        let line = |player_id: i64, role: &str| {
            let player = players_by_id.get(&player_id);
            let name = display_names.get(&player_id).cloned().unwrap_or_else(|| {
                player.map_or_else(
                    || format!("Unknown({player_id})"),
                    |player| player.name.clone(),
                )
            });
            let on_role = player
                .and_then(|player| player.preferred_roles.as_ref())
                .is_some_and(|roles| roles.iter().any(|preferred| preferred == role));
            let emoji = role_value(&ROLE_EMOJIS, role);
            let role_name = role_value(&ROLE_NAMES, role);
            let rating = player.map_or_else(
                || "N/A".to_owned(),
                |player| {
                    balanced_player_rating_label(
                        player,
                        pending.state.balancing_rating_system.as_str(),
                        &self.rating,
                        &self.openskill,
                    )
                },
            );
            let name = if on_role { format!("**{name}**") } else { name };
            let warning = if on_role { "" } else { " ⚠️" };
            if pending.state.balancing_rating_system == "jopacoin" {
                let balance = player.map_or(0, |player| player.jopacoin_balance);
                format!("{emoji} {name} ({role_name}) [{rating}] 💰 {balance}{warning}")
            } else {
                format!("{emoji} {name} ({role_name}) [{rating}]{warning}")
            }
        };
        let mut radiant = pending
            .state
            .radiant_team_ids
            .iter()
            .copied()
            .zip(&pending.state.radiant_roles)
            .map(|(player_id, role)| (role.clone(), line(player_id, role)))
            .collect::<Vec<_>>();
        let mut dire = pending
            .state
            .dire_team_ids
            .iter()
            .copied()
            .zip(&pending.state.dire_roles)
            .map(|(player_id, role)| (role.clone(), line(player_id, role)))
            .collect::<Vec<_>>();
        radiant.sort_by_key(|(role, _)| role.parse::<u8>().unwrap_or(u8::MAX));
        dire.sort_by_key(|(role, _)| role.parse::<u8>().unwrap_or(u8::MAX));
        let head_to_head = radiant
            .iter()
            .zip(&dire)
            .map(|((_, radiant), (_, dire))| format!("{radiant}  |  {dire}"))
            .collect::<Vec<_>>()
            .join("\n");
        let radiant_sum = pending
            .state
            .radiant_team_ids
            .iter()
            .filter_map(|id| players_by_id.get(id))
            .map(|player| {
                player.get_value(
                    true,
                    pending.state.balancing_rating_system == "openskill",
                    pending.state.balancing_rating_system == "jopacoin",
                )
            })
            .sum::<f64>();
        let dire_sum = pending
            .state
            .dire_team_ids
            .iter()
            .filter_map(|id| players_by_id.get(id))
            .map(|player| {
                player.get_value(
                    true,
                    pending.state.balancing_rating_system == "openskill",
                    pending.state.balancing_rating_system == "jopacoin",
                )
            })
            .sum::<f64>();
        let first_pick = pending
            .state
            .first_pick_team
            .as_deref()
            .unwrap_or("Radiant");
        let first_pick_emoji = if first_pick == "Radiant" {
            "🟢"
        } else {
            "🔴"
        };
        let lobby_kind = parse_persisted_lobby_kind(pending.state.lobby_kind.as_deref());
        let shuffle_mode = extra_str(&pending.state, "shuffle_mode").unwrap_or("balanced");
        let title_mode = if shuffle_mode == "region" {
            "Region Team Shuffle"
        } else {
            "Balanced Team Shuffle"
        };
        let title = if pending.state.is_bomb_pot {
            format!(
                "💣 BOMB POT 💣 {} — Match #{} — {title_mode}",
                lobby_kind.label(),
                pending.pending_match_id
            )
        } else {
            format!(
                "{} — Match #{} — {title_mode}",
                lobby_kind.label(),
                pending.pending_match_id
            )
        };
        let regions = participant_ids
            .iter()
            .filter_map(|player_id| players_by_id.get(player_id).copied())
            .map(|player| PlayerRegionInput {
                preferred_region: player.preferred_region.as_deref(),
                inferred_region: player.inferred_region.as_deref(),
            })
            .collect::<Vec<_>>();
        let mut embed = InteractionEmbed::titled(title)
            .color(if pending.state.is_bomb_pot {
                DISCORD_ORANGE
            } else {
                DISCORD_BLUE
            })
            .field(
                format!("🟢 Radiant ({radiant_sum:.0})  |  🔴 Dire ({dire_sum:.0})"),
                format!("{first_pick_emoji} **First Pick: {first_pick}**\n\n{head_to_head}"),
                false,
            )
            .field("🌎 Recommended Server", summarize_region(&regions), false);
        if shuffle_mode == "region" {
            let radiant_regions = pending
                .state
                .radiant_team_ids
                .iter()
                .filter_map(|id| players_by_id.get(id).copied())
                .collect::<Vec<_>>();
            let dire_regions = pending
                .state
                .dire_team_ids
                .iter()
                .filter_map(|id| players_by_id.get(id).copied())
                .collect::<Vec<_>>();
            embed = embed.field(
                "🌎 Region Split",
                format!(
                    "{}\n{}",
                    format_region_mix("Radiant", &radiant_regions),
                    format_region_mix("Dire", &dire_regions)
                ),
                false,
            );
        }
        let rating_display = match pending.state.balancing_rating_system.as_str() {
            "jopacoin" => "💰 Jopacoin Balance",
            "openskill" => "⚗️ OpenSkill (experimental)",
            _ => "📊 Glicko-2",
        };
        let radiant_off = off_role_count(
            &pending.state.radiant_team_ids,
            &pending.state.radiant_roles,
            &players_by_id,
        );
        let dire_off = off_role_count(
            &pending.state.dire_team_ids,
            &pending.state.dire_roles,
            &players_by_id,
        );
        let mut balance_info = format!(
            "**Shuffle mode:** {}\n**Balanced with:** {rating_display}\n**Goodness score:** {:.1} (lower = better)\n**Value diff:** {:.0}\n**Off-role players:** Radiant: {radiant_off}, Dire: {dire_off} (Total: {})",
            if shuffle_mode == "region" {
                "Region Split"
            } else {
                "Balanced"
            },
            extra_f64(&pending.state, "goodness_score").unwrap_or_default(),
            pending.state.value_diff,
            radiant_off + dire_off,
        );
        if !pending.state.excluded_player_ids.is_empty() {
            let names = pending
                .state
                .excluded_player_ids
                .iter()
                .map(|player_id| {
                    display_names.get(player_id).cloned().unwrap_or_else(|| {
                        players_by_id.get(player_id).map_or_else(
                            || format!("Unknown({player_id})"),
                            |player| player.name.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join(", ");
            balance_info.push_str(&format!("\n**Excluded:** {names}"));
        }
        embed = embed
            .field("📊 Balance", balance_info, false)
            .field(
                "📝 How to Bet",
                format!(
                    "`/bet <Radiant/Dire> <amount>` (min {} {JOPACOIN_EMOTE}). Pool betting: odds determined by bet distribution.",
                    self.config.jopacoin_min_bet
                ),
                false,
            );
        if let Some(blind) = pending.state.blind_bets_result.as_ref()
            && blind
                .get("created")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default()
                > 0
        {
            embed = embed.field(
                if pending.state.is_bomb_pot {
                    "💣 Bomb Pot Stakes"
                } else {
                    "🎲 Blind Bets"
                },
                blind_bet_display(blind, pending.state.is_bomb_pot),
                false,
            );
        }
        let bets = self.bets.clone();
        let shuffle_timestamp = pending.state.shuffle_timestamp.unwrap_or_default();
        let pending_match_id = pending.pending_match_id;
        let totals = tokio::task::spawn_blocking(move || {
            bets.get_pending_totals(Some(guild_id), shuffle_timestamp, Some(pending_match_id))
        })
        .await
        .map_err(|error| format!("bet totals task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let (wager_name, wager_value) = format_betting_display(
            totals.radiant,
            totals.dire,
            &pending.state.betting_mode,
            BettingDisplayOptions {
                lock_until: pending.state.bet_lock_until,
                locked: pending
                    .state
                    .bet_lock_until
                    .is_some_and(|lock| lock <= unix_seconds()),
                seed_radiant: pending.state.bet_seed_radiant,
                seed_dire: pending.state.bet_seed_dire,
                seed_bonus: pending.state.bet_seed_bonus,
            },
        );
        Ok(embed.field(wager_name, wager_value, false).footer(format!(
            "Match #{} | {:.2} {:.2}",
            pending.pending_match_id,
            extra_f64(&pending.state, "glicko_radiant_win_prob").unwrap_or(0.5),
            extra_f64(&pending.state, "openskill_radiant_win_prob").unwrap_or(0.5),
        )))
    }

    async fn publish_shuffle_thread(
        &self,
        snapshot: &MatchLobbySnapshot,
        pending: &PendingMatchRecord,
        embed: InteractionEmbed,
    ) -> Result<(), String> {
        let Some(thread_id) = snapshot.thread_id else {
            return Ok(());
        };
        let name = format!(
            "🔒 {} Shuffled - Awaiting Results",
            snapshot.lobby_kind.label()
        );
        let rename = self.discord.edit_thread(thread_id, &name, false, false);
        let publish = async {
            let receipt = self
                .discord
                .send_message(
                    thread_id,
                    DiscordMessage::silent(InteractionResponse::message("").embed(embed)),
                )
                .await?;
            let real_player_ids = pending
                .state
                .participant_ids()
                .into_iter()
                .filter(|player_id| *player_id > 0)
                .filter_map(|player_id| u64::try_from(player_id).ok())
                .collect::<BTreeSet<_>>();
            if !real_player_ids.is_empty() {
                let mentions = real_player_ids
                    .iter()
                    .map(|player_id| format!("<@{player_id}>"))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.discord
                    .send_message(
                        thread_id,
                        DiscordMessage::mentioning(
                            InteractionResponse::message(format!(
                                "{mentions}\nPlayers, take your starting positions"
                            )),
                            real_player_ids,
                        ),
                    )
                    .await?;
            }
            Ok::<_, String>(receipt)
        };
        let (rename_result, publish_result) = tokio::join!(rename, publish);
        if let Err(error) = rename_result {
            debug!(%error, thread_id, "shuffle thread rename failed");
        }
        let receipt = publish_result?;
        self.discord
            .edit_thread(thread_id, &name, false, true)
            .await?;
        let repository = self.pending.clone();
        let guild_id = pending.guild_id;
        let pending_match_id = pending.pending_match_id;
        tokio::task::spawn_blocking(move || {
            repository.mutate_pending_match(guild_id, pending_match_id, move |state| {
                state.thread_shuffle_thread_id = i64::try_from(thread_id).ok();
                state.thread_shuffle_message_id = i64::try_from(receipt.message_id).ok();
            })
        })
        .await
        .map_err(|error| format!("thread metadata task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn schedule_betting_reminders(&self, pending: &PendingMatchRecord, notify_subscribers: bool) {
        self.cancel_betting_tasks(pending.guild_id, Some(pending.pending_match_id));
        let Some(lock_until) = pending.state.bet_lock_until else {
            return;
        };
        let seconds_until_close = lock_until.saturating_sub(unix_seconds());
        if seconds_until_close <= 0 {
            return;
        }
        if notify_subscribers && let Some(reminders) = self.betting_reminders.get().cloned() {
            let guild_id = pending.guild_id;
            let pending_match_id = pending.pending_match_id;
            let lobby_kind = match parse_persisted_lobby_kind(pending.state.lobby_kind.as_deref()) {
                LobbyKind::Open => ReminderLobbyKind::Open,
                LobbyKind::LowSkill => ReminderLobbyKind::LowSkill,
            };
            tokio::spawn(async move {
                if let Err(error) = reminders
                    .notify_betting(guild_id, lock_until, pending_match_id, lobby_kind)
                    .await
                {
                    warn!(%error, guild_id, pending_match_id, "betting subscriber delivery failed");
                }
            });
        }
        let plan = betting_reminder_plan(
            seconds_until_close,
            &self.config.bet_reminder_offsets,
            self.config.bet_last_call_offset,
        );
        let mut tasks = Vec::new();
        for reminder in plan {
            let handler = self.clone();
            let guild_id = pending.guild_id;
            let pending_match_id = pending.pending_match_id;
            tasks.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(
                    u64::try_from(reminder.delay_seconds).unwrap_or_default(),
                ))
                .await;
                handler
                    .run_betting_reminder(
                        guild_id,
                        pending_match_id,
                        lock_until,
                        reminder.kind,
                        reminder.is_final_warning,
                    )
                    .await;
            }));
        }
        self.betting_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((pending.guild_id, pending.pending_match_id), tasks);
    }

    fn cancel_betting_tasks(&self, guild_id: i64, pending_match_id: Option<i64>) {
        let mut tasks = self
            .betting_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let keys = tasks
            .keys()
            .copied()
            .filter(|(task_guild, task_match)| {
                *task_guild == guild_id
                    && pending_match_id.is_none_or(|match_id| match_id == *task_match)
            })
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(handles) = tasks.remove(&key) {
                for handle in handles {
                    handle.abort();
                }
            }
        }
    }

    async fn run_betting_reminder(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        expected_lock_until: i64,
        kind: BettingReminderType,
        is_final_warning: bool,
    ) {
        let repository = self.pending.clone();
        let pending = tokio::task::spawn_blocking(move || {
            repository.pending_match(guild_id, pending_match_id)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        let Some(pending) = pending else {
            return;
        };
        if pending.state.bet_lock_until != Some(expected_lock_until) {
            return;
        }
        let totals_repository = self.bets.clone();
        let since = pending.state.shuffle_timestamp.unwrap_or_default();
        let betting_data = tokio::task::spawn_blocking(move || {
            let totals = totals_repository
                .get_pending_totals(Some(guild_id), since, Some(pending_match_id))
                .unwrap_or_default();
            let bets = totals_repository
                .get_pending_bets(Some(guild_id), None, since, Some(pending_match_id))
                .unwrap_or_default();
            let top_bettor = bets
                .into_iter()
                .filter(|bet| !bet.is_blind)
                .max_by_key(|bet| (bet.amount, std::cmp::Reverse(bet.bet_id)))
                .map(|bet| TopBettor {
                    discord_id: bet.discord_id,
                    team_bet_on: Some(
                        match bet.team {
                            BettingTeam::Radiant => "radiant",
                            BettingTeam::Dire => "dire",
                        }
                        .to_owned(),
                    ),
                    amount: bet.amount,
                });
            (totals, top_bettor)
        })
        .await
        .ok()
        .unwrap_or_default();
        let (totals, top_bettor) = betting_data;
        let pending_state = reminder_pending_state(&pending);
        let message_info = reminder_message_info(&pending);
        let flavor = self
            .betting_flavor
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let plan = tokio::task::spawn_blocking(move || {
            build_reminder(
                ReminderRequest {
                    guild_id: Some(guild_id),
                    reminder_type: reminder_type(kind),
                    lock_until: Some(expected_lock_until),
                    pending_match_id: Some(pending_match_id),
                    is_final_warning,
                    now: unix_seconds(),
                    pending_state: &pending_state,
                    message_info: &message_info,
                    totals: ReminderPotTotals {
                        radiant: totals.radiant,
                        dire: totals.dire,
                    },
                    top_bettor: top_bettor.as_ref(),
                },
                Some(flavor.as_ref()),
            )
        })
        .await
        .ok()
        .flatten();
        let Some(plan) = plan else {
            return;
        };
        for delivery in plan.deliveries {
            let message = reminder_discord_message(delivery.content, delivery.allowed_mentions);
            let result = match delivery.destination {
                ReminderDestination::Channel { channel_id } => {
                    let Ok(channel_id) = u64::try_from(channel_id) else {
                        continue;
                    };
                    self.discord.send_message(channel_id, message).await
                }
                ReminderDestination::ThreadReply {
                    thread_id,
                    parent_message_id,
                    use_partial_message,
                } => {
                    let (Ok(thread_id), Ok(parent_message_id)) =
                        (u64::try_from(thread_id), u64::try_from(parent_message_id))
                    else {
                        continue;
                    };
                    if use_partial_message {
                        self.discord
                            .send_thread_reply(thread_id, parent_message_id, message)
                            .await
                    } else {
                        self.discord.send_message(thread_id, message).await
                    }
                }
            };
            if let Err(error) = result {
                warn!(%error, "betting reminder delivery failed");
            }
        }
        if plan.locked_wager_refresh.is_some() {
            let _ = self.refresh_shuffle_messages(&pending, true).await;
        }
    }

    async fn refresh_shuffle_messages(
        &self,
        pending: &PendingMatchRecord,
        _locked: bool,
    ) -> (usize, Vec<String>) {
        let embed = match self.render_shuffle_embed(pending).await {
            Ok(embed) => embed,
            Err(error) => return (0, vec![error.to_string()]),
        };
        let response = DiscordMessage::silent(InteractionResponse::message("").embed(embed))
            .preserving_content();
        let mut failures = Vec::new();
        let mut refreshed = 0;
        let mut seen = BTreeSet::new();
        for (channel_id, message_id) in [
            (
                pending.state.shuffle_channel_id,
                pending.state.shuffle_message_id,
            ),
            (
                pending.state.cmd_shuffle_channel_id,
                pending.state.cmd_shuffle_message_id,
            ),
            (
                pending.state.thread_shuffle_thread_id,
                pending.state.thread_shuffle_message_id,
            ),
        ] {
            let (Some(channel_id), Some(message_id)) = (channel_id, message_id) else {
                continue;
            };
            let (Ok(channel_id), Ok(message_id)) =
                (u64::try_from(channel_id), u64::try_from(message_id))
            else {
                failures.push(format!("invalid Discord target {channel_id}/{message_id}"));
                continue;
            };
            if !seen.insert((channel_id, message_id)) {
                continue;
            }
            if let Err(error) = self
                .discord
                .edit_message(channel_id, message_id, response.clone())
                .await
            {
                failures.push(format!("{channel_id}/{message_id}: {error}"));
            } else {
                refreshed += 1;
            }
        }
        (refreshed, failures)
    }

    async fn refresh_wager_message(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<MatchWagerRefreshReport, String> {
        let repository = self.pending.clone();
        let pending = tokio::task::spawn_blocking(move || {
            repository.pending_match(guild_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("wager refresh lookup task failed: {error}"))?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "pending match {pending_match_id} not found in guild {guild_id} for wager refresh"
            )
        })?;
        let (refreshed, failures) = self.refresh_shuffle_messages(&pending, false).await;
        Ok(MatchWagerRefreshReport {
            attempted: refreshed.saturating_add(failures.len()),
            refreshed,
            failures,
        })
    }

    fn seed_hero_grid_blocking(&self, guild_id: i64) -> Result<AdminSeedHeroGridResult, String> {
        const HERO_POOL: [i64; 20] = [
            1, 2, 5, 7, 8, 11, 14, 18, 19, 22, 25, 29, 35, 39, 41, 44, 48, 49, 74, 86,
        ];
        const NUM_PLAYERS: usize = 20;
        const NUM_MATCHES: usize = 30;
        const BASE_ID: i64 = -1001;

        let player_ids = (0..NUM_PLAYERS)
            .map(|index| BASE_ID - i64::try_from(index).unwrap_or_default())
            .collect::<Vec<_>>();
        let mut player_hero_pools = Vec::with_capacity(NUM_PLAYERS);
        for _ in 0..NUM_PLAYERS {
            let mut pool = HERO_POOL.to_vec();
            fastrand::shuffle(&mut pool);
            pool.truncate(fastrand::usize(4..=6));
            player_hero_pools.push(pool);
        }

        for (index, player_id) in player_ids.iter().copied().enumerate() {
            if self
                .players
                .get_by_id(player_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                .is_some()
            {
                continue;
            }
            let mut roles = ["1", "2", "3", "4", "5"];
            fastrand::shuffle(&mut roles);
            let role_count = fastrand::usize(1..=3);
            let mut player =
                NewPlayer::new(player_id, format!("GridTest{}", index + 1), Some(guild_id));
            player.glicko_rating = Some(fastrand::i64(1000..=2000) as f64);
            player.glicko_rd = Some(fastrand::f64() * 150.0 + 50.0);
            player.glicko_volatility = Some(0.06);
            player.preferred_roles = Some(
                roles[..role_count]
                    .iter()
                    .map(|role| (*role).to_owned())
                    .collect(),
            );
            self.players
                .add(&player)
                .map_err(|error| error.to_string())?;
        }

        let matches = MatchRepository::new(&self.database_path);
        for _ in 0..NUM_MATCHES {
            let mut shuffled = player_ids.clone();
            fastrand::shuffle(&mut shuffled);
            shuffled.truncate(10);
            let (radiant, dire) = shuffled.split_at(5);
            let winning_team = if fastrand::bool() { 1 } else { 2 };
            let match_id = matches
                .record_match(&MatchRecord::new(
                    radiant.to_vec(),
                    dire.to_vec(),
                    winning_team,
                    Some(guild_id),
                ))
                .map_err(|error| error.to_string())?;
            for player_id in shuffled {
                let index = usize::try_from(BASE_ID - player_id)
                    .map_err(|_| format!("invalid seeded player ID {player_id}"))?;
                let hero_id = if fastrand::f64() < 0.7 {
                    let pool = &player_hero_pools[index];
                    pool[fastrand::usize(..pool.len())]
                } else {
                    HERO_POOL[fastrand::usize(..HERO_POOL.len())]
                };
                matches
                    .update_participant_stats(
                        match_id,
                        player_id,
                        &ParticipantStatsUpdate {
                            hero_id,
                            kills: fastrand::i64(0..=25),
                            deaths: fastrand::i64(0..=15),
                            assists: fastrand::i64(0..=30),
                            gpm: fastrand::i64(200..=800),
                            xpm: fastrand::i64(200..=700),
                            hero_damage: fastrand::i64(5_000..=50_000),
                            tower_damage: fastrand::i64(500..=15_000),
                            last_hits: fastrand::i64(20..=400),
                            denies: fastrand::i64(0..=40),
                            net_worth: fastrand::i64(5_000..=40_000),
                            hero_healing: 0,
                            lane_role: None,
                            lane_efficiency: None,
                            gold_at_10: None,
                        },
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(AdminSeedHeroGridResult {
            players_seeded: NUM_PLAYERS,
            matches_created: NUM_MATCHES,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BettingReminderType {
    Warning,
    LastCall,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BettingReminderPlan {
    delay_seconds: i64,
    kind: BettingReminderType,
    is_final_warning: bool,
}

fn betting_reminder_plan(
    seconds_until_close: i64,
    warning_offsets: &[i64],
    last_call_offset: i64,
) -> Vec<BettingReminderPlan> {
    if seconds_until_close <= 0 {
        return Vec::new();
    }
    let mut offset_types = BTreeMap::<i64, BettingReminderType>::new();
    for offset in warning_offsets {
        if *offset > 0 && *offset < seconds_until_close {
            offset_types
                .entry(*offset)
                .or_insert(BettingReminderType::Warning);
        }
    }
    if last_call_offset > 0 && last_call_offset < seconds_until_close {
        offset_types.insert(last_call_offset, BettingReminderType::LastCall);
    }
    let final_warning = offset_types
        .iter()
        .filter_map(|(offset, kind)| (*kind == BettingReminderType::Warning).then_some(*offset))
        .min();
    let mut plan = offset_types
        .into_iter()
        .rev()
        .map(|(offset, kind)| BettingReminderPlan {
            delay_seconds: seconds_until_close.saturating_sub(offset),
            kind,
            is_final_warning: kind == BettingReminderType::Warning && final_warning == Some(offset),
        })
        .collect::<Vec<_>>();
    plan.push(BettingReminderPlan {
        delay_seconds: seconds_until_close,
        kind: BettingReminderType::Closed,
        is_final_warning: false,
    });
    plan
}

#[async_trait]
impl AdminMatchControl for MatchHandler {
    async fn extend_betting(
        &self,
        request: AdminExtendBettingRequest,
    ) -> Result<AdminExtendBettingResult, String> {
        let extension_seconds = request
            .minutes
            .checked_mul(60)
            .ok_or_else(|| "betting extension overflow".to_owned())?;
        let repository = self.pending.clone();
        let extension = tokio::task::spawn_blocking(move || {
            repository.extend_betting_atomic(
                request.guild_id,
                request.pending_match_id,
                unix_seconds(),
                extension_seconds,
            )
        })
        .await
        .map_err(|error| format!("betting extension task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        self.cancel_betting_tasks(request.guild_id, Some(request.pending_match_id));
        self.schedule_betting_reminders(&extension.pending_match, true);
        let (refreshed_routes, refresh_failures) = self
            .refresh_shuffle_messages(&extension.pending_match, false)
            .await;
        Ok(AdminExtendBettingResult {
            pending_match_id: request.pending_match_id,
            old_bet_lock_until: extension.old_lock_until,
            new_bet_lock_until: extension.new_lock_until,
            lobby_label: parse_persisted_lobby_kind(
                extension.pending_match.state.lobby_kind.as_deref(),
            )
            .label()
            .to_owned(),
            jump_url: extension.pending_match.state.shuffle_message_jump_url,
            refreshed_routes,
            refresh_failures,
        })
    }

    async fn seed_hero_grid(
        &self,
        request: AdminSeedHeroGridRequest,
    ) -> Result<AdminSeedHeroGridResult, String> {
        let worker = self.clone();
        tokio::task::spawn_blocking(move || worker.seed_hero_grid_blocking(request.guild_id))
            .await
            .map_err(|error| format!("hero-grid seed task failed: {error}"))?
    }

    async fn backfill_derived_roles(
        &self,
        guild_id: i64,
    ) -> Result<AdminRoleBackfillResult, String> {
        let database_path = self.database_path.clone();
        tokio::task::spawn_blocking(move || {
            let matches = MatchRepository::new(&database_path);
            let report = matches
                .backfill_derived_roles(Some(guild_id))
                .map_err(|error| error.to_string())?;
            // Read coverage back from the database rather than reporting only
            // the run's own counters, so a no-op run is distinguishable from a
            // run whose writes did not land.
            let coverage = matches
                .role_coverage(Some(guild_id))
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(AdminRoleBackfillResult {
                matches_scanned: report.matches_scanned,
                teams_derived: report.teams_derived,
                gold_samples_written: report.gold_samples_written,
                unreadable_payloads: report.unreadable_payloads,
                unparsed_replays: report.unparsed_replays,
                ambiguous_lanes: report.ambiguous_lanes,
                incomplete_teams: report.incomplete_teams,
                tied_farm_priority: report.tied_farm_priority,
                participants: coverage.participants,
                with_derived_role: coverage.with_derived_role,
                with_gold_at_10: coverage.with_gold_at_10,
                player_roles_above_minimum_sample: coverage.player_roles_above_minimum_sample,
            })
        })
        .await
        .map_err(|error| format!("role backfill task failed: {error}"))?
    }
}

fn signed_id(id: u64, kind: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(id).map_err(|_| format!("{kind} id exceeds SQLite INTEGER range").into())
}

fn checked_i32(value: i64, name: &'static str) -> Result<i32, MatchProviderBuildError> {
    i32::try_from(value).map_err(|_| MatchProviderBuildError::InvalidConfig(name))
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn reminder_type(kind: BettingReminderType) -> MessageReminderType {
    match kind {
        BettingReminderType::Warning => MessageReminderType::Warning,
        BettingReminderType::LastCall => MessageReminderType::LastCall,
        BettingReminderType::Closed => MessageReminderType::Closed,
    }
}

/// Match's always-present no-AI flavor adapter. Betting can replace this with
/// the production `FlavorTextService` adapter after startup composition, but a
/// disabled/missing provider still preserves Python's visible fallback line.
#[derive(Default)]
struct StaticBettingFlavor;

impl BettingFlavorPort for StaticBettingFlavor {
    fn generate(
        &self,
        _request: &cama_app::betting_reminder_messaging::FlavorRequest,
    ) -> Result<Option<String>, String> {
        let examples = FlavorEvent::BetLastCall.examples();
        let index = fastrand::usize(0..examples.len());
        Ok(examples.get(index).map(|line| (*line).to_owned()))
    }
}

struct ProductionBettingGuildAiConfig {
    repository: GuildConfigRepository,
}

impl GuildAiConfigPort for ProductionBettingGuildAiConfig {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String> {
        self.repository
            .get_ai_enabled(guild_id)
            .map_err(|error| error.to_string())
    }
}

struct ProductionBettingPlayerContext {
    shop: ShopRuntimeRepository,
    gambling: GamblingStatsRepository,
    loans: LoanRepository,
}

impl ProductionBettingPlayerContext {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            shop: ShopRuntimeRepository::new(path.as_ref()),
            gambling: GamblingStatsRepository::new(path.as_ref()),
            loans: LoanRepository::new(path.as_ref()),
        }
    }
}

impl PlayerContextPort for ProductionBettingPlayerContext {
    fn player_context(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PlayerContext>, String> {
        let guild_id = guild_id.unwrap_or_default();
        let Some(player) = self
            .shop
            .player(discord_id, guild_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let games = player.games_played();
        let stats = GamblingStatsService::new(self.gambling.clone())
            .get_player_stats(discord_id, Some(guild_id))
            .ok()
            .flatten();
        let bankruptcies = self
            .gambling
            .player_bankruptcy_count(discord_id, Some(guild_id))
            .unwrap_or_default();
        let loan = self
            .loans
            .get_state(discord_id, Some(guild_id))
            .ok()
            .flatten();
        Ok(Some(PlayerContext {
            username: player.name,
            balance: player.balance,
            debt_amount: (player.balance < 0).then_some(player.balance.saturating_abs()),
            win_rate: (games > 0).then_some(player.wins as f64 / games as f64 * 100.0),
            degen_score: stats.as_ref().map(|stats| stats.degen_score.total),
            bankruptcy_count: bankruptcies,
            total_bets: stats.as_ref().map_or(0, |stats| stats.total_bets),
            total_loans: loan.map_or(0, |state| state.total_loans_taken),
            negative_loans: loan.map_or(0, |state| state.negative_loans_taken),
            total_fees_paid: loan.map_or(0, |state| state.total_fees_paid),
            biggest_win: stats.as_ref().map(|stats| stats.biggest_win),
            biggest_loss: stats
                .as_ref()
                .map(|stats| stats.biggest_loss.saturating_abs()),
            bet_win_rate: stats.as_ref().map(|stats| stats.win_rate * 100.0),
            times_in_debt: bankruptcies,
            lowest_balance: player.lowest_balance,
        }))
    }
}

fn reminder_pending_state(pending: &PendingMatchRecord) -> PendingBettingState {
    PendingBettingState {
        betting_mode: if pending.state.betting_mode.eq_ignore_ascii_case("house") {
            ReminderBettingMode::House
        } else {
            ReminderBettingMode::Pool
        },
        bet_lock_until: pending.state.bet_lock_until,
        pending_match_id: Some(pending.pending_match_id),
        lobby_kind: parse_persisted_lobby_kind(pending.state.lobby_kind.as_deref()),
        bet_seed_radiant: pending.state.bet_seed_radiant,
        bet_seed_dire: pending.state.bet_seed_dire,
        bet_seed_bonus: pending.state.bet_seed_bonus,
        shuffle_channel_id: pending.state.shuffle_channel_id,
        shuffle_message_id: pending.state.shuffle_message_id,
        command_shuffle_channel_id: pending.state.cmd_shuffle_channel_id,
        command_shuffle_message_id: pending.state.cmd_shuffle_message_id,
        thread_shuffle_thread_id: pending.state.thread_shuffle_thread_id,
        thread_shuffle_message_id: pending.state.thread_shuffle_message_id,
        radiant_team_ids: pending.state.radiant_team_ids.clone(),
        dire_team_ids: pending.state.dire_team_ids.clone(),
    }
}

fn reminder_message_info(pending: &PendingMatchRecord) -> ShuffleMessageInfo {
    ShuffleMessageInfo {
        channel_id: pending.state.shuffle_channel_id,
        thread_message_id: pending.state.thread_shuffle_message_id,
        thread_id: pending.state.thread_shuffle_thread_id,
        origin_channel_id: pending.state.origin_channel_id,
    }
}

fn reminder_discord_message(
    content: String,
    allowed_mentions: ReminderAllowedMentions,
) -> DiscordMessage {
    let response = InteractionResponse::message(content);
    match allowed_mentions.users {
        MentionUsers::Scoped(ids) => DiscordMessage::mentioning(
            response,
            ids.into_iter()
                .filter(|id| *id > 0)
                .filter_map(|id| u64::try_from(id).ok())
                .collect(),
        ),
        MentionUsers::Disabled => DiscordMessage::silent(response),
    }
}

fn monotonic_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), InteractionHandlerError> {
    responder
        .followup(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string().into())
}

fn string_option(options: &[InteractionOption], name: &str) -> Option<String> {
    options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match &option.value {
            InteractionValue::String(value) => Some(value.clone()),
            _ => None,
        })
}

fn parse_lobby_kind(value: &str) -> Result<LobbyKind, InteractionHandlerError> {
    match value {
        "open" => Ok(LobbyKind::Open),
        "lowskill" => Ok(LobbyKind::LowSkill),
        value => Err(format!("unknown lobby kind {value:?}").into()),
    }
}

fn parse_persisted_lobby_kind(value: Option<&str>) -> LobbyKind {
    match value {
        Some("lowskill") => LobbyKind::LowSkill,
        _ => LobbyKind::Open,
    }
}

const fn first_pick_team(radiant: bool) -> &'static str {
    if radiant { "Radiant" } else { "Dire" }
}

fn balanced_player_rating_label(
    player: &Player,
    balancing_system: &str,
    rating_system: &CamaRatingSystem,
    openskill_system: &CamaOpenSkillSystem,
) -> String {
    if balancing_system == "openskill" {
        return player.os_mu.map_or_else(
            || "N/A".to_owned(),
            |mu| {
                let display = openskill_system.mu_to_display(mu);
                let certainty = openskill_system.certainty_percentage(
                    player
                        .os_sigma
                        .unwrap_or(CamaOpenSkillSystem::DEFAULT_SIGMA),
                );
                format!("{display} | {certainty:.0}%")
            },
        );
    }
    player.glicko_rating.map_or_else(
        || "N/A".to_owned(),
        |rating| rating_system.rating_to_display(rating).to_string(),
    )
}

fn role_value(configuration: &[(&'static str, &'static str)], role: &str) -> &'static str {
    configuration
        .iter()
        .find_map(|(identifier, value)| (*identifier == role).then_some(*value))
        .unwrap_or("")
}

fn extra_str<'a>(state: &'a PendingMatchState, key: &str) -> Option<&'a str> {
    state.extra.get(key).and_then(serde_json::Value::as_str)
}

fn extra_f64(state: &PendingMatchState, key: &str) -> Option<f64> {
    state.extra.get(key).and_then(serde_json::Value::as_f64)
}

fn format_region_mix(label: &str, players: &[&Player]) -> String {
    let mut west = 0;
    let mut east = 0;
    let mut unset = 0;
    for player in players {
        match resolve_region(&PlayerRegionInput {
            preferred_region: player.preferred_region.as_deref(),
            inferred_region: player.inferred_region.as_deref(),
        }) {
            Some("USW") => west += 1,
            Some("USE") => east += 1,
            _ => unset += 1,
        }
    }
    let mut parts = Vec::new();
    if west > 0 {
        parts.push(format!("US West: {west}"));
    }
    if east > 0 {
        parts.push(format!("US East: {east}"));
    }
    if unset > 0 {
        parts.push(format!("Unset: {unset}"));
    }
    format!(
        "**{label}:** {}",
        if parts.is_empty() {
            "US West".to_owned()
        } else {
            parts.join(", ")
        }
    )
}

fn off_role_count(player_ids: &[i64], roles: &[String], players: &BTreeMap<i64, &Player>) -> usize {
    player_ids
        .iter()
        .zip(roles)
        .filter(|(player_id, role)| {
            !players
                .get(player_id)
                .and_then(|player| player.preferred_roles.as_ref())
                .is_some_and(|preferred| preferred.iter().any(|candidate| candidate == *role))
        })
        .count()
}

fn blind_bet_display(blind: &serde_json::Value, is_bomb_pot: bool) -> String {
    let created = blind
        .get("created")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let radiant = blind
        .get("total_radiant")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    let dire = blind
        .get("total_dire")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    if is_bomb_pot {
        let percentage = blind
            .get("percentage")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_default()
            * 100.0;
        format!(
            "💣 **BOMB POT:** All 10 players ante'd in! ({percentage}% + 10 {JOPACOIN_EMOTE} ante)\n🟢 Radiant: {radiant} {JOPACOIN_EMOTE} | 🔴 Dire: {dire} {JOPACOIN_EMOTE}\n_+1 bonus {JOPACOIN_EMOTE} for ALL players this match!_"
        )
    } else {
        format!(
            "**Auto-liquidity:** {created} players contributed blind bets\n🟢 Radiant: {radiant} {JOPACOIN_EMOTE} | 🔴 Dire: {dire} {JOPACOIN_EMOTE}"
        )
    }
}

fn selected_roster(snapshot: &MatchLobbySnapshot) -> (Vec<i64>, Vec<i64>) {
    let Some(confirmed) = snapshot.confirmed_player_ids.as_ref() else {
        return (snapshot.player_ids.clone(), Vec::new());
    };
    let confirmed_count = snapshot
        .player_ids
        .iter()
        .filter(|player_id| confirmed.contains(player_id))
        .count();
    if confirmed_count < snapshot.ready_threshold {
        return (snapshot.player_ids.clone(), Vec::new());
    }
    snapshot
        .player_ids
        .iter()
        .copied()
        .partition(|player_id| confirmed.contains(player_id))
}

fn lobby_wait_minutes(
    snapshot: &MatchLobbySnapshot,
    player_ids: &[i64],
    shuffle_timestamp: i64,
) -> HashMap<i64, i64> {
    player_ids
        .iter()
        .filter_map(|player_id| {
            snapshot.player_join_times.get(player_id).map(|joined_at| {
                (
                    *player_id,
                    ((shuffle_timestamp as f64 - joined_at) / 60.0).max(0.0) as i64,
                )
            })
        })
        .collect()
}

fn lobby_kind_value(kind: LobbyKind) -> &'static str {
    match kind {
        LobbyKind::Open => "open",
        LobbyKind::LowSkill => "lowskill",
    }
}

fn sql_datetime(value: &SqlValue) -> Option<DateTime<Utc>> {
    let SqlValue::Text(value) = value else {
        return None;
    };
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|date| date.and_utc())
        })
}

fn decayed_glicko_rd(
    system: &CamaRatingSystem,
    rd: f64,
    last_match_at: Option<DateTime<Utc>>,
    created_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> f64 {
    let Some(reference) = last_match_at.or(created_at) else {
        return rd;
    };
    let days = (now - reference).num_days().max(0);
    system.apply_rd_decay(rd, i32::try_from(days).unwrap_or(i32::MAX))
}

fn sql_i64(value: &SqlValue) -> Option<i64> {
    match value {
        SqlValue::Integer(value) => Some(*value),
        SqlValue::Real(value) if value.is_finite() => Some(*value as i64),
        SqlValue::Text(value) => value.parse().ok(),
        SqlValue::Null | SqlValue::Blob(_) | SqlValue::Real(_) => None,
    }
}

fn sql_f64(value: &SqlValue) -> Option<f64> {
    match value {
        SqlValue::Integer(value) => Some(*value as f64),
        SqlValue::Real(value) if value.is_finite() => Some(*value),
        SqlValue::Text(value) => value.parse().ok(),
        SqlValue::Null | SqlValue::Blob(_) | SqlValue::Real(_) => None,
    }
}

fn voting_error_message<E: std::fmt::Display>(error: MatchVotingServiceError<E>) -> String {
    match error {
        MatchVotingServiceError::Policy(error) => error.to_string(),
        MatchVotingServiceError::Port(error) => format!("Could not update match votes: {error}"),
    }
}

fn last_match_participant_ids(path: &Path, guild_id: i64) -> Result<HashSet<i64>, String> {
    MatchRepository::new(path)
        .get_last_match_participant_ids(Some(guild_id))
        .map_err(|error| error.to_string())
}

fn team_player_ids(team: &Team) -> Result<Vec<i64>, String> {
    team.players
        .iter()
        .map(|player| {
            player
                .discord_id
                .ok_or_else(|| format!("shuffled player {:?} has no Discord ID", player.name))
        })
        .collect()
}

fn player_ids(players: &[Player]) -> Vec<i64> {
    players
        .iter()
        .filter_map(|player| player.discord_id)
        .collect()
}

fn region_inputs(players: &[Player]) -> Vec<PlayerRegionInput<'_>> {
    players
        .iter()
        .map(|player| PlayerRegionInput {
            preferred_region: player.preferred_region.as_deref(),
            inferred_region: player.inferred_region.as_deref(),
        })
        .collect()
}

fn glicko_probability(system: &CamaRatingSystem, radiant: &[Player], dire: &[Player]) -> f64 {
    let prediction = |player: &Player| {
        player.glicko_rating.map_or_else(
            || system.create_player_from_mmr(player.mmr.and_then(|mmr| i32::try_from(mmr).ok())),
            |rating| {
                system.create_player_from_rating(
                    rating,
                    player.glicko_rd.unwrap_or(system.initial_rd),
                    player
                        .glicko_volatility
                        .unwrap_or(system.initial_volatility),
                )
            },
        )
    };
    let radiant = radiant.iter().map(prediction).collect::<Vec<_>>();
    let dire = dire.iter().map(prediction).collect::<Vec<_>>();
    let radiant = system.aggregate_team_stats(&radiant);
    let dire = system.aggregate_team_stats(&dire);
    CamaRatingSystem::predict_win_probability(radiant.rating, radiant.rd, dire.rating, dire.rd)
}

fn openskill_probability(
    system: &CamaOpenSkillSystem,
    radiant: &[Player],
    dire: &[Player],
) -> Result<f64, String> {
    let rating = |player: &Player| {
        OpenSkillRating::new(
            player
                .os_mu
                .filter(|value| *value != 0.0)
                .unwrap_or(CamaOpenSkillSystem::DEFAULT_MU),
            player
                .os_sigma
                .filter(|value| *value != 0.0)
                .unwrap_or(CamaOpenSkillSystem::DEFAULT_SIGMA),
        )
    };
    let radiant = radiant.iter().map(rating).collect::<Vec<_>>();
    let dire = dire.iter().map(rating).collect::<Vec<_>>();
    system
        .calibrate_win_probability(system.os_predict_win_probability(&radiant, &dire), None)
        .map_err(|error| error.to_string())
}

fn percentage_points(value: f64) -> i64 {
    (value * 100.0).round_ties_even() as i64
}

fn percentage_basis_points(value: f64) -> i64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value * 10_000.0).round() as i64
    }
}

fn bet_jc_deltas(
    settlement: &SettlementResult,
    blood_pact_skims: &BTreeMap<i64, i64>,
) -> BTreeMap<i64, i64> {
    let mut deltas = BTreeMap::new();
    for bet in settlement.winners.iter().chain(&settlement.losers) {
        deltas
            .entry(bet.discord_id)
            .and_modify(|delta: &mut i64| *delta = delta.saturating_sub(bet.effective_amount))
            .or_insert(-bet.effective_amount);
    }
    for bet in &settlement.winners {
        deltas
            .entry(bet.discord_id)
            .and_modify(|delta| *delta = delta.saturating_add(bet.payout))
            .or_insert(bet.payout);
    }
    for deductions in [
        &settlement.bankruptcy_penalties,
        &settlement.vanity_taxes,
        blood_pact_skims,
    ] {
        for (discord_id, amount) in deductions {
            deltas
                .entry(*discord_id)
                .and_modify(|delta| *delta = delta.saturating_sub(*amount))
                .or_insert(-*amount);
        }
    }
    deltas
}

fn accumulate_generated_rewards(
    rewards: &BTreeMap<i64, GeneratedRewardOutcome>,
    component: &str,
    bonus_net: &mut BTreeMap<i64, i64>,
    bonus_components: &mut BTreeMap<i64, BTreeMap<String, i64>>,
) {
    for (discord_id, reward) in rewards {
        accumulate_reward_component(
            *discord_id,
            component,
            reward.net(),
            reward.balance_delta(),
            bonus_net,
            bonus_components,
        );
    }
}

fn accumulate_generated_compensation(
    rewards: &BTreeMap<i64, GeneratedRewardOutcome>,
    source: &str,
    compensatable_net: &mut BTreeMap<i64, i64>,
    compensatable_by_source: &mut BTreeMap<(i64, String), i64>,
) {
    for (discord_id, reward) in rewards {
        accumulate_compensatable_net(*discord_id, reward.compensatable_net(), compensatable_net);
        accumulate_compensatable_source(
            *discord_id,
            source,
            reward.compensatable_net(),
            compensatable_by_source,
        );
    }
}

fn accumulate_compensatable_source(
    discord_id: i64,
    source: &str,
    amount: i64,
    compensatable_by_source: &mut BTreeMap<(i64, String), i64>,
) {
    if amount == 0 {
        return;
    }
    compensatable_by_source
        .entry((discord_id, source.to_owned()))
        .and_modify(|current| *current = current.saturating_add(amount))
        .or_insert(amount);
}

fn accumulate_compensatable_net(
    discord_id: i64,
    amount: i64,
    compensatable_net: &mut BTreeMap<i64, i64>,
) {
    if amount == 0 {
        return;
    }
    compensatable_net
        .entry(discord_id)
        .and_modify(|current| *current = current.saturating_add(amount))
        .or_insert(amount);
}

fn accumulate_reward_component(
    discord_id: i64,
    component: &str,
    net: i64,
    balance_delta: i64,
    bonus_net: &mut BTreeMap<i64, i64>,
    bonus_components: &mut BTreeMap<i64, BTreeMap<String, i64>>,
) {
    bonus_net
        .entry(discord_id)
        .and_modify(|current| *current = current.saturating_add(net))
        .or_insert(net);
    bonus_components
        .entry(discord_id)
        .or_default()
        .entry(component.to_owned())
        .and_modify(|current| *current = current.saturating_add(balance_delta))
        .or_insert(balance_delta);
}

fn render_jc_lines(
    winners: &[i64],
    losers: &[i64],
    excluded: &[i64],
    changes: &BTreeMap<i64, BTreeMap<String, i64>>,
    settlement: &SettlementResult,
) -> Vec<String> {
    let winning_ids = winners.iter().copied().collect::<BTreeSet<_>>();
    let losing_ids = losers.iter().copied().collect::<BTreeSet<_>>();
    let excluded_ids = excluded.iter().copied().collect::<BTreeSet<_>>();
    let participant_ids = winning_ids
        .union(&losing_ids)
        .copied()
        .chain(excluded_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut stakes = BTreeMap::<i64, i64>::new();
    for bet in settlement.winners.iter().chain(&settlement.losers) {
        stakes
            .entry(bet.discord_id)
            .and_modify(|stake| *stake = stake.saturating_add(bet.effective_amount))
            .or_insert(bet.effective_amount);
    }

    let mut ordered_ids = winners
        .iter()
        .chain(losers)
        .chain(excluded)
        .copied()
        .collect::<Vec<_>>();
    ordered_ids.extend(changes.keys().copied());
    let mut seen = BTreeSet::new();
    ordered_ids.retain(|discord_id| seen.insert(*discord_id));

    let signed = |amount: i64| match amount.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{amount}"),
        std::cmp::Ordering::Less => format!("−{}", amount.saturating_abs()),
        std::cmp::Ordering::Equal => "0".to_owned(),
    };
    let mut entries = ordered_ids
        .into_iter()
        .filter_map(|discord_id| {
            let components = changes.get(&discord_id)?;
            let mut details = Vec::new();
            if let Some(payout) = components.get("payout") {
                let label = if winning_ids.contains(&discord_id) {
                    "win"
                } else if losing_ids.contains(&discord_id) {
                    "play"
                } else if excluded_ids.contains(&discord_id) {
                    "excluded"
                } else {
                    "payout"
                };
                details.push(format!("{label} {}", signed(*payout)));
            }
            if let Some(bet) = components.get("bet") {
                let mut detail = format!("bet {}", signed(*bet));
                if let Some(stake) = stakes.get(&discord_id).filter(|stake| **stake != 0) {
                    detail.push_str(&format!(" · stake {stake}"));
                }
                details.push(detail);
            }
            for (component, label) in [("streak", "streak"), ("referral", "referral")] {
                if let Some(amount) = components.get(component) {
                    details.push(format!("{label} {}", signed(*amount)));
                }
            }
            let total = ["payout", "bet", "streak", "referral"]
                .into_iter()
                .filter_map(|component| components.get(component))
                .copied()
                .fold(0_i64, i64::saturating_add);
            let detail = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };
            Some((
                discord_id,
                total,
                format!(
                    "<@{discord_id}>: **{}** {JOPACOIN_EMOTE}{detail}",
                    signed(total)
                ),
            ))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| right.1.cmp(&left.1));

    let mut lines = Vec::new();
    if let Some(first) = settlement.winners.first() {
        let winner_stake = settlement
            .winners
            .iter()
            .map(|bet| bet.effective_amount)
            .fold(0_i64, i64::saturating_add);
        let winner_payout = settlement
            .winners
            .iter()
            .map(|bet| bet.payout)
            .fold(0_i64, i64::saturating_add);
        if winner_stake > 0 {
            let team = match first.team {
                BettingTeam::Radiant => "🟢 Radiant",
                BettingTeam::Dire => "🔴 Dire",
            };
            lines.push(format!(
                "**Final odds:** {team} {:.2}x",
                winner_payout as f64 / winner_stake as f64
            ));
        }
    }
    for (heading, predicate) in [
        ("**Match Players:**", 0_u8),
        ("**Bettors:**", 1_u8),
        ("**Other Rewards:**", 2_u8),
    ] {
        let group = entries
            .iter()
            .filter(|(discord_id, _, _)| match predicate {
                0 => participant_ids.contains(discord_id),
                1 => {
                    !participant_ids.contains(discord_id) && changes[discord_id].contains_key("bet")
                }
                _ => {
                    !participant_ids.contains(discord_id)
                        && !changes[discord_id].contains_key("bet")
                }
            })
            .map(|(_, _, line)| line.clone())
            .collect::<Vec<_>>();
        if !group.is_empty() {
            lines.push(heading.to_owned());
            lines.extend(group);
        }
    }
    lines
}

fn blind_outcome_json(
    outcome: &BlindBetOutcome,
    config: MatchConfig,
    is_bomb_pot: bool,
) -> serde_json::Value {
    let bets = outcome
        .created
        .iter()
        .map(|bet| {
            json!({
                "discord_id": bet.discord_id,
                "team": match bet.team {
                    BettingTeam::Radiant => "radiant",
                    BettingTeam::Dire => "dire",
                },
                "amount": bet.amount,
            })
        })
        .collect::<Vec<_>>();
    let skipped = outcome
        .skipped
        .iter()
        .map(|skip| json!({"discord_id": skip.discord_id, "reason": skip.reason}))
        .collect::<Vec<_>>();
    json!({
        "created": outcome.created.len(),
        "total_radiant": outcome.total_radiant,
        "total_dire": outcome.total_dire,
        "percentage": if is_bomb_pot {
            config.bomb_pot_blind_percentage
        } else {
            config.auto_blind_percentage
        },
        "bets": bets,
        "skipped": skipped,
        "is_bomb_pot": is_bomb_pot,
    })
}

fn automatic_outcome_json(
    outcome: &AutomaticBetOutcome,
    spectator_metadata: &BTreeMap<i64, (i64, i64)>,
    investment_metadata: &BTreeMap<(i64, i64, String), i64>,
) -> serde_json::Value {
    let bets = outcome
        .created
        .iter()
        .map(|bet| {
            let mut value = json!({
                "discord_id": bet.discord_id,
                "team": match bet.team {
                    BettingTeam::Radiant => "radiant",
                    BettingTeam::Dire => "dire",
                },
                "amount": bet.amount,
            });
            if let Some(target_id) = bet.investment_target_id {
                value["investor_id"] = json!(bet.discord_id);
                value["target_id"] = json!(target_id);
                if let Some(direction) = bet.investment_direction.as_deref() {
                    value["direction"] = json!(direction);
                    if let Some(percentage) =
                        investment_metadata.get(&(bet.discord_id, target_id, direction.to_owned()))
                    {
                        value["percentage"] = json!(percentage);
                    }
                }
            }
            if let Some((networth, basis_points)) = spectator_metadata.get(&bet.discord_id) {
                value["networth"] = json!(networth);
                value["basis_points"] = json!(basis_points);
                value["percentage"] = json!((*basis_points as f64) / 10_000.0);
            }
            value
        })
        .collect::<Vec<_>>();
    let total_radiant = outcome
        .created
        .iter()
        .filter(|bet| bet.team == BettingTeam::Radiant)
        .map(|bet| bet.amount)
        .sum::<i64>();
    let total_dire = outcome
        .created
        .iter()
        .filter(|bet| bet.team == BettingTeam::Dire)
        .map(|bet| bet.amount)
        .sum::<i64>();
    let skipped = outcome
        .skipped
        .iter()
        .map(|discord_id| json!({"discord_id": discord_id, "reason": "candidate skipped"}))
        .collect::<Vec<_>>();
    json!({
        "created": outcome.created.len(),
        "total_radiant": total_radiant,
        "total_dire": total_dire,
        "bets": bets,
        "skipped": skipped,
    })
}

#[cfg(test)]
#[path = "match_provider/tests.rs"]
mod tests;
