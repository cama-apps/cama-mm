//! Production registration for Python's commands.betting extension.
//!
//! Durable money and wager decisions stay in the existing-schema repositories.
//! The runtime layer owns only interaction translation, rate limiting, and
//! Discord response composition.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::dig_neon::{BigWinFlavor, BigWinSource};
use cama_app::dota_bet_seed::{BetTotals, bets_overview};
use cama_app::economy_actions::apply_gamba_event_multiplier;
use cama_app::economy_event_service::EconomyEventConfig;
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::golden_wheel::{LiveGoldenEconomy, compute_live_golden_wedges};
use cama_app::jopat_post_match::{JopatPostMatchContext, NumericFact};
use cama_app::mana_service::mana_day_label;
use cama_app::manashop_rework::{
    BuffService, HostileLossRequest as AppHostileLossRequest,
    HostileLossSettlement as AppHostileLossSettlement, ProtectionGateway,
};
use cama_app::neon_degen::{EventKey, NeonDegenService, NeonEventPort, NeonResult, StoreError};
use cama_app::wheel::{
    CooldownOutcome, LossRoll, WHEEL_BANANA_PEEL_LOSS_MAX, WHEEL_BANANA_PEEL_LOSS_MIN,
    WHEEL_BOMB_OMB_VICTIM_COUNT, WHEEL_BOMB_OMB_VICTIM_LOSS_MAX, WHEEL_BOMB_OMB_VICTIM_LOSS_MIN,
    WHEEL_GREEN_SHELL_STEAL_MAX, WHEEL_GREEN_SHELL_STEAL_MIN, WheelEconomyPolicy, WheelKind,
    WheelMechanic, WheelValue, canonical_outcome_code, cooldown_after_resolution, get_wheel_wedges,
    requested_percentage_or_flat, resolve_explosion, select_wheel_kind,
};
use cama_db::autobet_investments::AutobetInvestmentRepository;
use cama_db::bankruptcy_repository::{
    BankruptcyRepository, DeclarationOutcome, DeclarationRequest,
};
use cama_db::betting_service_repository::{
    BetCounterReceipt, BettingServiceRepository, PendingBetRecord, PlaceBetRequest,
};
use cama_db::core_repositories::{MatchRepository, PlayerRepository, WheelSpinClaim};
use cama_db::disbursement::{
    DISBURSE_METHODS, DisbursementProposal, DisbursementRepository, DisbursementVote,
};
use cama_db::dota_bet_seed_repository::{BettingMode, BettingTeam, SeedSplit};
use cama_db::gambling_stats_repository::{GamblingStatsRepository, GamblingStatsService};
use cama_db::golden_wheel_repository::GoldenWheelRepository;
use cama_db::loan_repository::{DebtPayment, LedgerContext, LoanRepository};
use cama_db::mana_service_repository::BankruptBuff;
use cama_db::mana_service_repository::ManaRepository;
use cama_db::manashop_rework_repository::ManashopRepository;
use cama_db::match_recording_repository::{
    IncomeAwardReceipt, IncomeAwardRequest, MatchRecordingRepository,
};
use cama_db::match_runtime::{PendingMatchRecord, PendingMatchRepository};
use cama_db::neon_events::NeonEventRepository;
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::shop_runtime::{
    HostileDestination as DbHostileDestination, HostileLossRequest as DbHostileLossRequest,
    ShopRuntimeRepository,
};
use cama_db::tip_repository::TipRepository;
use cama_db::wheel_spin_repository::{NewWheelSpin, WheelSpinRepository};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::mana::{ManaEffects, format_mana_badge};
use cama_domain::pet_evolution::PetActivity;
use cama_domain::player::Player;
use chrono::{DateTime, FixedOffset, Utc};
use gif::{Encoder, Frame, Repeat};
use rusqlite::types::Value as SqlValue;
use serde_json::json;

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::gateway_events::{GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryReport};
use crate::match_provider::{
    MatchBetSettlementRequest, MatchPostMatchDebriefPort, MatchPostMatchDebriefRequest,
    MatchRegistrationProvider,
};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionAttachment, InteractionButton, InteractionButtonStyle,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};
use crate::worker::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

/// Narrow dependency seam for the existing vanity-tax cache.  The betting
/// provider only needs the current taxable-id snapshot at the point a wheel
/// reward is credited; tax eligibility remains owned by the shared service.
pub trait BettingVanityTaxPort: Send + Sync {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64>;
}

impl BettingVanityTaxPort for cama_app::service_container::PersistentVanityTaxService {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64> {
        Self::taxable_ids(self, guild_id)
    }
}

#[derive(Debug, Default)]
struct NoVanityTax;

impl BettingVanityTaxPort for NoVanityTax {
    fn taxable_ids(&self, _guild_id: i64) -> BTreeSet<i64> {
        BTreeSet::new()
    }
}

/// Match-owned Discord presentation hook for Python's
/// `update_shuffle_message_wagers` helper.  Betting owns when a successful
/// wager must refresh; Match owns the shuffle embed body and its three
/// persisted message copies.  Keeping that ownership split prevents betting
/// from reconstructing (and eventually drifting from) Match's player/odds
/// embed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BettingWagerRefreshReport {
    pub attempted: usize,
    pub refreshed: usize,
    pub failures: Vec<String>,
}

#[async_trait]
pub trait BettingWagerRefreshPort: Send + Sync {
    async fn refresh_wager_message(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<BettingWagerRefreshReport, String>;
}

/// Build the production Match-owned wager-message adapter. The betting
/// provider triggers this port after its atomic wager commit, while Match
/// remains the sole owner of shuffle embeds and persisted Discord targets.
#[must_use]
pub fn match_wager_refresh_port(
    match_provider: MatchRegistrationProvider,
) -> Arc<dyn BettingWagerRefreshPort> {
    Arc::new(MatchBettingWagerRefreshPort { match_provider })
}

/// Build the production Match-owned JOPA-T/Neon debrief adapter. Match reads
/// and selects rating-history facts; Betting owns the shared Neon policy and
/// Discord attachment/fallback delivery. The adapter is intentionally narrow
/// so Match settlement never reaches into betting's process-local state.
#[must_use]
pub fn match_post_match_debrief_port(
    betting_provider: BettingRegistrationProvider,
) -> Arc<dyn MatchPostMatchDebriefPort> {
    Arc::new(MatchBettingPostMatchDebriefPort {
        handler: Arc::clone(&betting_provider.handler),
    })
}

#[derive(Clone)]
struct MatchBettingWagerRefreshPort {
    match_provider: MatchRegistrationProvider,
}

struct MatchBettingPostMatchDebriefPort {
    handler: Arc<BettingInteractionHandler>,
}

#[async_trait]
impl BettingWagerRefreshPort for MatchBettingWagerRefreshPort {
    async fn refresh_wager_message(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<BettingWagerRefreshReport, String> {
        let report = self
            .match_provider
            .refresh_wager_message(guild_id, pending_match_id)
            .await?;
        Ok(BettingWagerRefreshReport {
            attempted: report.attempted,
            refreshed: report.refreshed,
            failures: report.failures,
        })
    }
}

#[async_trait]
impl MatchPostMatchDebriefPort for MatchBettingPostMatchDebriefPort {
    async fn on_bet_settlement(&self, request: MatchBetSettlementRequest) -> Result<(), String> {
        let Some(channel_id) = request.channel_id else {
            return Ok(());
        };
        let event_type = format!("match_settlement:{}", request.match_id);
        let repository = NeonEventRepository::new(&self.handler.database_path);
        let guild_id = request.guild_id;
        let claimed = sqlite("claim match settlement Neon event", {
            let repository = repository.clone();
            let event_type = event_type.clone();
            move || {
                repository
                    .claim_one_time_event(
                        0,
                        guild_id,
                        &event_type,
                        2,
                        unix_seconds().unwrap_or_default(),
                    )
                    .map_err(|error| error.to_string())
            }
        })
        .await?;
        if !claimed {
            return Ok(());
        }

        let mut selected = None;
        {
            let mut neon = self
                .handler
                .neon
                .lock()
                .map_err(|_| "betting Neon lock poisoned".to_owned())?;
            if let Some(winner) = request
                .participants
                .iter()
                .filter(|participant| participant.won && !participant.refunded)
                .max_by_key(|participant| {
                    (
                        participant.payout,
                        std::cmp::Reverse(participant.discord_id),
                    )
                })
            {
                selected = neon.on_big_win(
                    u64::try_from(winner.discord_id).unwrap_or_default(),
                    Some(u64::try_from(guild_id).unwrap_or_default()),
                    BigWinSource::Match,
                    winner.payout,
                    BigWinFlavor::BigWin,
                );
            }
            if selected.is_none() {
                for loser in request
                    .participants
                    .iter()
                    .filter(|participant| !participant.won && !participant.refunded)
                {
                    let discord_id = u64::try_from(loser.discord_id).unwrap_or_default();
                    let guild = Some(u64::try_from(guild_id).unwrap_or_default());
                    if loser.leverage >= 5 && loser.balance_after < 0 {
                        selected = neon.on_leverage_loss(
                            discord_id,
                            guild,
                            loser.amount,
                            loser.leverage,
                            loser.balance_after,
                        );
                    }
                    if selected.is_none()
                        && (loser.balance_after <= -self.handler.config.max_debt
                            || (loser.balance_after == 0 && !loser.won))
                    {
                        selected =
                            neon.on_bet_settled(discord_id, guild, loser.won, loser.balance_after);
                    }
                    if selected.is_some() {
                        break;
                    }
                }
            }
        }
        let Some(result) = selected else {
            let repository = NeonEventRepository::new(&self.handler.database_path);
            let event_type = event_type.clone();
            let _ = sqlite("release untriggered settlement Neon event", move || {
                repository
                    .release_one_time_event(0, guild_id, &event_type)
                    .map_err(|error| error.to_string())
            })
            .await;
            return Ok(());
        };
        if !self
            .handler
            .emit_neon_result_checked(channel_id, result)
            .await
        {
            let repository = NeonEventRepository::new(&self.handler.database_path);
            let _ = sqlite("release failed settlement Neon event", move || {
                repository
                    .release_one_time_event(0, guild_id, &event_type)
                    .map_err(|error| error.to_string())
            })
            .await;
        }
        Ok(())
    }

    async fn on_post_match_debrief(
        &self,
        request: MatchPostMatchDebriefRequest,
    ) -> Result<(), String> {
        let Some(channel_id) = request.channel_id else {
            return Ok(());
        };
        let path = self.handler.database_path.clone();
        let guild_id = request.guild_id;
        let winner_id = request.winner_id;
        let loser_id = request.loser_id;
        let (winner_name, loser_name) = sqlite("match Neon player names", move || {
            let players = PlayerRepository::new(path);
            let winner_name = if let Some(id) = winner_id {
                players
                    .get_by_id(id, Some(guild_id))
                    .map_err(|error| error.to_string())?
                    .map(|player| player.name)
            } else {
                None
            };
            let loser_name = if let Some(id) = loser_id {
                players
                    .get_by_id(id, Some(guild_id))
                    .map_err(|error| error.to_string())?
                    .map(|player| player.name)
            } else {
                None
            };
            Ok::<_, String>((winner_name, loser_name))
        })
        .await
        .unwrap_or((None, None));
        let repository = NeonEventRepository::new(&self.handler.database_path);
        let event_type = format!("match_debrief:{}", request.match_id);
        let claimed = sqlite("claim match debrief Neon event", {
            let repository = repository.clone();
            let event_type = event_type.clone();
            move || {
                repository
                    .claim_one_time_event(
                        0,
                        guild_id,
                        &event_type,
                        2,
                        unix_seconds().unwrap_or_default(),
                    )
                    .map_err(|error| error.to_string())
            }
        })
        .await?;
        if !claimed {
            return Ok(());
        }
        let context = JopatPostMatchContext {
            winner_name,
            loser_name,
            rating_change: request
                .rating_change
                .filter(|value| value.is_finite())
                .map(NumericFact::from),
            expected_win_prob: request
                .expected_win_prob
                .filter(|value| value.is_finite())
                .map(NumericFact::from),
            payout: request.payout.max(0),
            loss: request.loss.max(0),
            leverage: request.leverage.max(1),
            ..JopatPostMatchContext::default()
        };
        let result = self
            .handler
            .neon
            .lock()
            .map_err(|_| "betting Neon lock poisoned".to_owned())?
            .on_post_match_debrief(&context);
        if let Some(result) = result {
            if !self
                .handler
                .emit_neon_result_checked(channel_id, result)
                .await
            {
                let repository = NeonEventRepository::new(&self.handler.database_path);
                let _ = sqlite("release failed match debrief Neon event", move || {
                    repository
                        .release_one_time_event(0, guild_id, &event_type)
                        .map_err(|error| error.to_string())
                })
                .await;
            }
        } else {
            let repository = NeonEventRepository::new(&self.handler.database_path);
            let _ = sqlite("release untriggered match debrief Neon event", move || {
                repository
                    .release_one_time_event(0, guild_id, &event_type)
                    .map_err(|error| error.to_string())
            })
            .await;
        }
        Ok(())
    }
}

/// Existing-schema Neon event storage for the betting provider.  Cooldowns
/// remain process-local just like Python's service, while one-time milestone
/// events survive a restart through the shared `neon_events` table.
#[derive(Clone, Debug)]
struct BettingSqliteNeonEventPort {
    repository: NeonEventRepository,
}

impl NeonEventPort for BettingSqliteNeonEventPort {
    fn load_one_time_events(&self) -> Result<Vec<EventKey>, StoreError> {
        self.repository
            .load_one_time_events()
            .map_err(|error| StoreError(error.to_string()))?
            .into_iter()
            .map(|record| {
                let discord_id = u64::try_from(record.discord_id)
                    .map_err(|_| StoreError("Neon discord id exceeds unsigned range".to_owned()))?;
                let guild_id = u64::try_from(record.guild_id)
                    .map_err(|_| StoreError("Neon guild id exceeds unsigned range".to_owned()))?;
                Ok(EventKey::new(discord_id, Some(guild_id), record.event_type))
            })
            .collect()
    }

    fn check_one_time_event(&self, key: &EventKey) -> Result<bool, StoreError> {
        let discord_id = i64::try_from(key.discord_id)
            .map_err(|_| StoreError("Neon discord id exceeds SQLite range".to_owned()))?;
        let guild_id = i64::try_from(key.guild_id)
            .map_err(|_| StoreError("Neon guild id exceeds SQLite range".to_owned()))?;
        self.repository
            .check_one_time_event(discord_id, guild_id, &key.event_type)
            .map_err(|error| StoreError(error.to_string()))
    }

    fn persist_one_time_event(&self, key: EventKey, layer: u8) -> Result<(), StoreError> {
        let discord_id = i64::try_from(key.discord_id)
            .map_err(|_| StoreError("Neon discord id exceeds SQLite range".to_owned()))?;
        let guild_id = i64::try_from(key.guild_id)
            .map_err(|_| StoreError("Neon guild id exceeds SQLite range".to_owned()))?;
        self.repository
            .persist_one_time_event(
                discord_id,
                guild_id,
                &key.event_type,
                i64::from(layer),
                unix_seconds().unwrap_or_default(),
            )
            .map_err(|error| StoreError(error.to_string()))
    }
}

const COMPONENT_PREFIX: &str = "betting:";
const DISBURSE_COMPONENT_PREFIX: &str = "disburse:";
const TOWN_TRIAL_COMPONENT_PREFIX: &str = "tt_";
const DISCOVER_COMPONENT_PREFIX: &str = "disc_";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const DEFAULT_RATE_WINDOW: Duration = Duration::from_secs(10);
const DISBURSE_VOTES_PAGE_SIZE: usize = 15;
const DISBURSE_VOTES_TTL: Duration = Duration::from_secs(300);
const BETTING_NEON_DELETE_DELAY: Duration = Duration::from_secs(60);
const BETTING_EXPLOSION_DISPLAY_DELAY: Duration = Duration::from_secs(8);

#[derive(Clone, Debug)]
pub struct BettingRuntimeConfig {
    pub min_bet: i64,
    pub max_debt: i64,
    pub bankruptcy_cooldown_seconds: i64,
    pub bankruptcy_fresh_start_balance: i64,
    pub bankruptcy_penalty_games: i64,
    pub bankruptcy_penalty_rate: f64,
    pub garnishment_rate: f64,
    /// Kept in the runtime snapshot for the coordinated VanityTaxPort hook.
    /// The provider intentionally does not tax every spinner until that port
    /// supplies the taxable-id set (see `credit_wheel_income`).
    #[allow(dead_code)]
    pub vanity_tax_rate: f64,
    pub wheel_target_ev: f64,
    pub wheel_bankrupt_target_ev: f64,
    pub wheel_golden_target_ev: f64,
    pub loan_cooldown_seconds: i64,
    pub loan_fee_rate: f64,
    pub loan_max_amount: i64,
    pub tip_fee_rate: f64,
    pub minigame_jc_delta_scale: f64,
    pub disburse_min_fund: i64,
    pub disburse_quorum_percentage: f64,
    /// Daily economy-event policy is read from the same existing-schema
    /// controller used by match/shop settlement.  Keeping the configuration
    /// here makes `/gamba` use the active event without a second event model.
    pub economy_events_enabled: bool,
    pub economy_normal_annual_rate: f64,
    pub economy_inflation_ceiling: f64,
    pub economy_event_lookback_days: i64,
    pub economy_event_max_reserve_burn_pct: f64,
    pub economy_event_max_wallet_burn_pct: f64,
    pub economy_event_trigger_hour_local: u8,
    pub neon_degen_enabled: bool,
    pub neon_cooldown_seconds: i64,
    pub neon_bigwin_floor: f64,
    pub neon_bigwin_full_payout: i64,
    pub neon_bigwin_min_payout: i64,
    pub admin_user_ids: BTreeSet<i64>,
    pub tax_man_user_ids: BTreeSet<i64>,
}

impl BettingRuntimeConfig {
    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        let values = &config.values;
        Self {
            min_bet: values.jopacoin_min_bet.max(1),
            max_debt: values.max_debt,
            bankruptcy_cooldown_seconds: values.bankruptcy_cooldown_seconds,
            bankruptcy_fresh_start_balance: values.bankruptcy_fresh_start_balance,
            bankruptcy_penalty_games: values.bankruptcy_penalty_games,
            bankruptcy_penalty_rate: values.bankruptcy_penalty_rate,
            garnishment_rate: values.garnishment_percentage,
            vanity_tax_rate: values.vanity_tax_rate,
            wheel_target_ev: values.wheel_target_ev,
            wheel_bankrupt_target_ev: values.wheel_bankrupt_target_ev,
            wheel_golden_target_ev: values.wheel_golden_target_ev,
            loan_cooldown_seconds: values.loan_cooldown_seconds,
            loan_fee_rate: values.loan_fee_rate,
            loan_max_amount: values.loan_max_amount,
            tip_fee_rate: values.tip_fee_rate,
            minigame_jc_delta_scale: values.minigame_jc_delta_scale,
            disburse_min_fund: values.disburse_min_fund,
            disburse_quorum_percentage: values.disburse_quorum_percentage,
            economy_events_enabled: values.economy_events_enabled,
            economy_normal_annual_rate: values.economy_normal_annual_rate,
            economy_inflation_ceiling: values.economy_inflation_ceiling,
            economy_event_lookback_days: values.economy_event_lookback_days,
            economy_event_max_reserve_burn_pct: values.economy_event_max_reserve_burn_pct,
            economy_event_max_wallet_burn_pct: values.economy_event_max_wallet_burn_pct,
            economy_event_trigger_hour_local: u8::try_from(
                values.economy_event_trigger_hour_local.clamp(0, 23),
            )
            .unwrap_or(10),
            neon_degen_enabled: values.neon_degen_enabled,
            neon_cooldown_seconds: values.neon_cooldown_seconds,
            neon_bigwin_floor: values.neon_bigwin_floor,
            neon_bigwin_full_payout: values.neon_bigwin_full_payout,
            neon_bigwin_min_payout: values.neon_bigwin_min_payout,
            admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            tax_man_user_ids: config.identities.tax_man_user_ids.iter().copied().collect(),
        }
    }
}

fn economy_event_config(config: &BettingRuntimeConfig) -> EconomyEventConfig {
    EconomyEventConfig {
        enabled: config.economy_events_enabled,
        normal_annual_rate: config.economy_normal_annual_rate,
        inflation_ceiling: config.economy_inflation_ceiling,
        lookback_days: config.economy_event_lookback_days,
        max_reserve_burn_pct: config.economy_event_max_reserve_burn_pct,
        max_wallet_burn_pct: config.economy_event_max_wallet_burn_pct,
        trigger_hour_local: config.economy_event_trigger_hour_local,
    }
    .normalized()
}

#[derive(Clone)]
pub struct BettingRegistrationProvider {
    handler: Arc<BettingInteractionHandler>,
}

pub const BETTING_VIEW_TIMEOUT_WORKER_NAME: &str = "betting-view-timeouts";
pub const BETTING_VIEW_TIMEOUT_WAKE_INTERVAL: Duration = Duration::from_secs(1);

impl BettingRegistrationProvider {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        Self::with_runtime_config(
            database_path.as_ref().to_path_buf(),
            BettingRuntimeConfig::from_application_config(config),
            discord,
        )
    }

    #[must_use]
    pub fn with_runtime_config(
        database_path: PathBuf,
        config: BettingRuntimeConfig,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        Self::with_runtime_config_and_vanity_tax(
            database_path,
            config,
            discord,
            Arc::new(NoVanityTax),
        )
    }

    #[must_use]
    pub fn with_runtime_config_and_vanity_tax(
        database_path: PathBuf,
        config: BettingRuntimeConfig,
        discord: Arc<dyn DiscordTransport>,
        vanity_tax: Arc<dyn BettingVanityTaxPort>,
    ) -> Self {
        let neon_port: Arc<dyn NeonEventPort> = Arc::new(BettingSqliteNeonEventPort {
            repository: NeonEventRepository::new(&database_path),
        });
        let mut neon = NeonDegenService::with_event_port(fastrand::u64(..), neon_port);
        neon.set_enabled(config.neon_degen_enabled);
        neon.set_cooldown_seconds(
            u64::try_from(config.neon_cooldown_seconds.max(0)).unwrap_or(u64::MAX),
        );
        neon.set_max_debt(config.max_debt);
        neon.set_bigwin_policy(
            config.neon_bigwin_floor,
            config.neon_bigwin_full_payout,
            config.neon_bigwin_min_payout,
        );
        Self {
            handler: Arc::new(BettingInteractionHandler {
                database_path,
                config,
                discord,
                vanity_tax,
                neon: Mutex::new(neon),
                wager_refresh: Mutex::new(None),
                rates: Mutex::new(BTreeMap::new()),
                wheel_interactions: Mutex::new(BTreeMap::new()),
                wheel_in_flight: Mutex::new(BTreeSet::new()),
                disburse_vote_pages: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Install the coordinated Match-owned wager-message renderer after both
    /// providers have been constructed.  The port is optional so isolated
    /// betting deployments retain the same durable command behavior while a
    /// full composition can refresh all persisted shuffle copies.
    pub fn set_wager_refresh_port(&self, port: Arc<dyn BettingWagerRefreshPort>) {
        if let Ok(mut slot) = self.handler.wager_refresh.lock() {
            *slot = Some(port);
        }
    }

    /// Builder form for composition roots that prefer immutable setup.
    #[must_use]
    pub fn with_wager_refresh_port(self, port: Arc<dyn BettingWagerRefreshPort>) -> Self {
        self.set_wager_refresh_port(port);
        self
    }

    /// Gateway observation hook for the process-local Discord views owned by
    /// this provider. Money, cooldown claims, and Neon one-time events are
    /// durable in the existing schema. Views intentionally survive READY,
    /// RESUME, and guild-create notifications; a true process restart starts
    /// with an empty in-memory map, while the supervised timeout worker owns
    /// expiry.
    ///
    /// The composition root may register this observer with the shared
    /// Serenity gateway fan-out.  Keeping it here avoids introducing a second
    /// persistence model for Python's process-local `discord.ui.View` state.
    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(BettingGatewayObserver {
            handler: Arc::clone(&self.handler),
        })
    }

    /// Resolve any wheel views whose Python-equivalent timeout has elapsed.
    ///
    /// This is also the one-shot operation used by the supervised timeout
    /// worker; exposing it keeps deterministic recovery tests independent of
    /// the runtime supervisor. The wallet settlement remains the same
    /// exact-once SQLite operation used by a button click. The process-local
    /// entry is claimed while SQLite is in flight and retained on failure so
    /// a transient error can be retried without permitting a double settle.
    pub async fn expire_pending_views(&self) -> Result<usize, String> {
        self.handler.expire_pending_views().await
    }

    /// Build the supervised timeout worker for the process-local wheel views.
    ///
    /// A pending key is claimed before SQLite and removed only after success;
    /// failed work releases the claim for a later retry. The worker is
    /// intentionally returned as a normal `BackgroundWorkerSpec` so the
    /// runtime supervisor owns cancellation and bounded restart policy.
    #[must_use]
    pub fn timeout_worker(&self) -> BackgroundWorkerSpec {
        BackgroundWorkerSpec::new(
            BETTING_VIEW_TIMEOUT_WORKER_NAME,
            Arc::new(BettingViewTimeoutWorker {
                handler: Arc::clone(&self.handler),
            }),
        )
    }

    /// Trigger the same free wheel spin used by the Dig bonus dispatcher.
    ///
    /// Dig owns the bonus selection and presentation; this narrow hook keeps
    /// wheel admission, settlement, media, and exact-once logging in the
    /// betting provider without making Dig duplicate `/gamba` policy.
    pub async fn trigger_bonus_spin(
        &self,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<u64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        self.handler
            .gamba(user_id, Some(guild_id), channel_id, 0, &responder, true)
            .await
    }
}

struct BettingViewTimeoutWorker {
    handler: Arc<BettingInteractionHandler>,
}

#[async_trait]
impl BackgroundWorker for BettingViewTimeoutWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        while context.sleep(BETTING_VIEW_TIMEOUT_WAKE_INTERVAL).await {
            self.handler.expire_pending_views().await?;
        }
        Ok(())
    }
}

struct BettingGatewayObserver {
    handler: Arc<BettingInteractionHandler>,
}

#[async_trait]
impl GatewayEventObserver for BettingGatewayObserver {
    fn name(&self) -> &'static str {
        "betting-view-recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        // Serenity dispatches this hook for READY, RESUME, and guild_create.
        // Python's in-memory views survive a RESUME, while a process restart
        // constructs a fresh provider with no handles.  Keep the callback
        // observational; the supervised timeout worker owns expiry and
        // settlement of live components.
        let _ = &self.handler;
        ReadyRecoveryReport::empty(self.name(), context.guild_ids().len())
    }
}

impl RegistrationProvider for BettingRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "bet".to_owned(),
            description: "Place a jopacoin bet on a match (check balance with /balance)".to_owned(),
            options: bet_options(self.handler.config.min_bet),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "mybets".to_owned(),
            description: "Show your active bets".to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "bets".to_owned(),
            description: "Show all bets in the current pool".to_owned(),
            options: vec![match_option(
                "Match to view bets for (auto-selects if only one match exists)",
            )],
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "balance".to_owned(),
            description: "Check your jopacoin balance".to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "gamba".to_owned(),
            description: "Spin the Wheel of Fortune! (once per day)".to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "economy".to_owned(),
            description: "Jopacoin transfers, debt, and Reserve commands".to_owned(),
            options: economy_options(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: DISBURSE_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })?;
        // Python's TownTrial and Discover views use these short, process-local
        // IDs.  Register them explicitly so a Rust restart acknowledges an
        // old message with the same expiry copy instead of silently dropping
        // the interaction.  New Rust wheel messages use the namespaced route.
        registry.component(ComponentRoute {
            custom_id_prefix: TOWN_TRIAL_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: DISCOVER_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

fn choices_string(mut option: CommandOptionSpec, values: &[(&str, &str)]) -> CommandOptionSpec {
    option.choices = values
        .iter()
        .map(|(name, value)| CommandOptionChoice::String {
            name: (*name).to_owned(),
            value: (*value).to_owned(),
        })
        .collect();
    option
}

fn choices_integer(mut option: CommandOptionSpec, values: &[(&str, i64)]) -> CommandOptionSpec {
    option.choices = values
        .iter()
        .map(|(name, value)| CommandOptionChoice::Integer {
            name: (*name).to_owned(),
            value: *value,
        })
        .collect();
    option
}

fn match_option(description: &str) -> CommandOptionSpec {
    CommandOptionSpec::new("match", description, CommandOptionKind::Integer).autocomplete()
}

fn bet_options(min_bet: i64) -> Vec<CommandOptionSpec> {
    let mut amount = CommandOptionSpec::new(
        "amount",
        "Amount of jopacoin to wager (view balance with /balance)",
        CommandOptionKind::Integer,
    )
    .required(true);
    amount.min_integer = Some(min_bet);
    vec![
        choices_string(
            CommandOptionSpec::new("team", "Radiant or Dire", CommandOptionKind::String)
                .required(true),
            &[("Radiant", "radiant"), ("Dire", "dire")],
        ),
        amount,
        choices_integer(
            CommandOptionSpec::new(
                "leverage",
                "Leverage multiplier (2x, 3x, 5x) - can cause debt!",
                CommandOptionKind::Integer,
            ),
            &[
                ("None (1x)", 1),
                ("2x", 2),
                ("3x", 3),
                ("5x", 5),
                ("10x", 10),
            ],
        ),
        match_option(
            "Match to bet on (optional - auto-selects if you're a participant or only one match exists)",
        ),
    ]
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

fn economy_options() -> Vec<CommandOptionSpec> {
    let mut percentage = CommandOptionSpec::new(
        "percentage",
        "Whole percentage of your balance (1-10)",
        CommandOptionKind::Integer,
    );
    percentage.min_integer = Some(1);
    percentage.max_integer = Some(10);
    let mut position = CommandOptionSpec::new(
        "position",
        "Number shown by List (for removing a position)",
        CommandOptionKind::Integer,
    );
    position.min_integer = Some(1);
    position.max_integer = Some(50);
    let mut loan_amount = CommandOptionSpec::new(
        "amount",
        "Amount to borrow (max 100)",
        CommandOptionKind::Integer,
    );
    loan_amount.required = true;
    vec![
        subcommand(
            "tip",
            "Give jopacoin to another player",
            vec![
                CommandOptionSpec::new("player", "Player to tip", CommandOptionKind::User)
                    .required(true),
                CommandOptionSpec::new(
                    "amount",
                    "Amount of jopacoin to give",
                    CommandOptionKind::Integer,
                )
                .required(true),
            ],
        ),
        subcommand(
            "invest",
            "Configure free automatic long or short match investments",
            vec![
                choices_string(
                    CommandOptionSpec::new(
                        "action",
                        "Set, remove, or list investments",
                        CommandOptionKind::String,
                    )
                    .required(true),
                    &[("Set", "set"), ("Remove", "remove"), ("List", "list")],
                ),
                CommandOptionSpec::new(
                    "player",
                    "Player whose team you want to back or fade",
                    CommandOptionKind::User,
                ),
                choices_string(
                    CommandOptionSpec::new(
                        "direction",
                        "Long backs the player; short backs their opponent",
                        CommandOptionKind::String,
                    ),
                    &[("Long", "long"), ("Short", "short")],
                ),
                percentage,
                position,
            ],
        ),
        subcommand(
            "paydebt",
            "Help another player pay off their debt",
            vec![
                CommandOptionSpec::new(
                    "player",
                    "Player whose debt to pay",
                    CommandOptionKind::User,
                )
                .required(true),
                CommandOptionSpec::new(
                    "amount",
                    "Amount of jopacoin to pay toward their debt",
                    CommandOptionKind::Integer,
                )
                .required(true),
            ],
        ),
        subcommand(
            "bankruptcy",
            "Declare bankruptcy to clear your debt (once per week, with penalties)",
            Vec::new(),
        ),
        subcommand("loan", "Borrow jopacoin (with a fee)", vec![loan_amount]),
        subcommand("reserve", "View the Jopacoin Reserve", Vec::new()),
        subcommand(
            "disburse",
            "Propose or manage Jopacoin Reserve allocation",
            vec![choices_string(
                CommandOptionSpec::new("action", "Action to perform", CommandOptionKind::String),
                &[
                    ("propose", "propose"),
                    ("status", "status"),
                    ("reset", "reset"),
                    ("votes", "votes"),
                    ("execute", "execute"),
                ],
            )],
        ),
    ]
}

type RateKey = (String, i64, i64);

#[derive(Clone, Debug)]
struct DisbursementExecution {
    method: String,
    total_disbursed: i64,
    distributions: Vec<(i64, i64)>,
    cancelled: bool,
    message: Option<String>,
}

#[derive(Clone, Debug)]
struct WheelOption {
    label: String,
    value: WheelValue,
    color: &'static str,
}

const WHEEL_MEDIA_SIZE: u16 = 500;
/// Keep the production GIF contract in one place.  Python's wheel renderer
/// uses 70 frames: four acceleration/deceleration bands, a creep phase, then
/// two terminal hold frames.  GIF stores delays in centiseconds, so the
/// millisecond values below are converted at the encoder boundary.
const WHEEL_MEDIA_FRAME_COUNT: usize = 70;
const EXPLOSION_MEDIA_FRAME_COUNT: usize = 56;
const WHEEL_MEDIA_UPLOAD_LIMIT: usize = 4 * 1024 * 1024;
#[cfg(test)]
const EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET: usize = 500_000;
const MAX_WHEEL_LABEL_SPRITES: usize = 128;

fn wheel_frame_delay_ms(frame_index: usize) -> u64 {
    match frame_index {
        0..=13 => 30,
        14..=27 => 45,
        28..=41 => 70,
        42..=57 => 110,
        58 => 180,
        59..=67 => 180 + (frame_index as u64 - 58) * 15,
        _ => 60_000,
    }
}

fn explosion_frame_delay_ms(frame_index: usize) -> u64 {
    match frame_index {
        0..=13 => 50,
        14..=23 => 60 + (frame_index as u64 - 14) * 10,
        24..=27 => 60,
        28..=41 => 80,
        42..=54 => 100,
        _ => 60_000,
    }
}

fn gif_delay_centiseconds(milliseconds: u64) -> u16 {
    // GIF stores centiseconds. Pillow (the Python implementation) truncates
    // sub-centisecond durations when it writes the control extension, so use
    // the same floor conversion rather than rounding 45ms up to 50ms.
    u16::try_from((milliseconds / 10).min(u64::from(u16::MAX))).unwrap_or(u16::MAX)
}

/// Render a compact, deterministic wheel animation for the same attachment
/// lifecycle as Python's `create_wheel_gif`.  The policy layer chooses the
/// target wedge; this renderer only paints the already-resolved presentation,
/// so it cannot influence money or outcome selection.
fn render_wheel_attachment(
    wedges: &[cama_app::wheel::WheelWedge],
    target_index: usize,
    golden: bool,
) -> Result<InteractionAttachment, String> {
    if wedges.is_empty() {
        return Err("cannot render an empty wheel".to_owned());
    }
    let mut palette = vec![
        [7, 9, 14],                                      // background
        [241, 196, 15],                                  // gold hub/final highlight
        [231, 76, 60],                                   // pointer
        if golden { [58, 40, 0] } else { [44, 62, 80] }, // hub
        [0, 0, 0],                                       // label shadow
        [255, 255, 255],                                 // label text
    ];
    let mut wedge_indices = Vec::with_capacity(wedges.len());
    for wedge in wedges {
        let rgb = parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]);
        let index = palette
            .iter()
            .position(|candidate| candidate == &rgb)
            .unwrap_or_else(|| {
                palette.push(rgb);
                palette.len() - 1
            });
        wedge_indices.push(u8::try_from(index).map_err(|_| "wheel palette overflow".to_owned())?);
    }
    let palette_bytes = palette
        .iter()
        .flat_map(|rgb| rgb.iter().copied())
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(
            &mut bytes,
            WHEEL_MEDIA_SIZE,
            WHEEL_MEDIA_SIZE,
            &palette_bytes,
        )
        .map_err(|error| format!("wheel GIF encoder: {error}"))?;
        encoder
            .set_repeat(Repeat::Finite(1))
            .map_err(|error| format!("wheel GIF repeat: {error}"))?;
        let mut sprite_cache = WheelLabelSpriteCache::default();
        let label_atlas = build_wheel_label_atlas(wedges, &mut sprite_cache);
        for frame_index in 0..WHEEL_MEDIA_FRAME_COUNT {
            let mut pixels =
                vec![0_u8; usize::from(WHEEL_MEDIA_SIZE) * usize::from(WHEEL_MEDIA_SIZE)];
            draw_wheel_frame(
                &mut pixels,
                &wedge_indices,
                &label_atlas,
                target_index,
                frame_index,
                golden,
            );
            let mut frame = Frame {
                width: WHEEL_MEDIA_SIZE,
                height: WHEEL_MEDIA_SIZE,
                delay: gif_delay_centiseconds(wheel_frame_delay_ms(frame_index)),
                buffer: Cow::Owned(pixels),
                ..Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder
                .write_frame(&frame)
                .map_err(|error| format!("wheel GIF frame: {error}"))?;
        }
    }
    if bytes.len() >= WHEEL_MEDIA_UPLOAD_LIMIT {
        return Err("wheel GIF exceeded Discord's 4 MiB upload limit".to_owned());
    }
    Ok(InteractionAttachment::bytes("wheel.gif", bytes))
}

/// Render the explosion branch as a real GIF attachment.  The short radial
/// animation is intentionally self-contained: Discord upload failures can
/// fall back to the text response without touching the settled wallet state.
fn render_explosion_attachment() -> Result<InteractionAttachment, String> {
    let palette: &[[u8; 3]] = &[
        [5, 5, 10],
        [70, 8, 8],
        [180, 20, 10],
        [240, 90, 10],
        [255, 210, 25],
        [255, 255, 235],
    ];
    let palette_bytes = palette
        .iter()
        .flat_map(|rgb| rgb.iter().copied())
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(
            &mut bytes,
            WHEEL_MEDIA_SIZE,
            WHEEL_MEDIA_SIZE,
            &palette_bytes,
        )
        .map_err(|error| format!("explosion GIF encoder: {error}"))?;
        encoder
            .set_repeat(Repeat::Finite(1))
            .map_err(|error| format!("explosion GIF repeat: {error}"))?;
        for frame_index in 0..EXPLOSION_MEDIA_FRAME_COUNT {
            let mut pixels =
                vec![0_u8; usize::from(WHEEL_MEDIA_SIZE) * usize::from(WHEEL_MEDIA_SIZE)];
            draw_explosion_frame(&mut pixels, frame_index);
            let mut frame = Frame {
                width: WHEEL_MEDIA_SIZE,
                height: WHEEL_MEDIA_SIZE,
                delay: gif_delay_centiseconds(explosion_frame_delay_ms(frame_index)),
                buffer: Cow::Owned(pixels),
                ..Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder
                .write_frame(&frame)
                .map_err(|error| format!("explosion GIF frame: {error}"))?;
        }
    }
    if bytes.len() >= WHEEL_MEDIA_UPLOAD_LIMIT {
        return Err("explosion GIF exceeded Discord's 4 MiB upload limit".to_owned());
    }
    Ok(InteractionAttachment::bytes("explosion.gif", bytes))
}

/// Return the bounded contact-sheet dimensions used by the Python renderer
/// when it builds one representative palette for all explosion phases.  The
/// Rust renderer's fixed six-color palette is already shared by every frame,
/// but retaining this policy seam makes the memory bound explicit and keeps a
/// future richer palette implementation from accidentally materializing a
/// 500x500x56 RGB sheet.
#[cfg(test)]
fn explosion_palette_sample_size(frame_size: (usize, usize), frame_count: usize) -> (usize, usize) {
    let source_pixels = frame_size
        .0
        .saturating_mul(frame_size.1)
        .saturating_mul(frame_count.max(1));
    let scale =
        ((source_pixels as f64 / EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET as f64).sqrt()).max(1.0);
    (
        ((frame_size.0 as f64 / scale) as usize).max(1),
        ((frame_size.1 as f64 / scale) as usize).max(1),
    )
}

fn parse_hex_rgb(color: &str) -> Option<[u8; 3]> {
    let value = color.strip_prefix('#').unwrap_or(color);
    if value.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

#[derive(Clone, Debug)]
struct WheelLabelSprite {
    width: i32,
    height: i32,
    mask: Vec<u8>,
}

#[derive(Clone, Debug)]
struct WheelLabelAtlas {
    sprites: Vec<WheelLabelSprite>,
    wedge_sprite_indices: Vec<usize>,
}

/// Bounded process-local sprite cache used while composing wheel media.
///
/// Pillow's implementation uses an LRU cache keyed by the measured label
/// geometry. Rust keeps the same policy with compact 5x7 masks instead of
/// font-backed RGBA images. The cache is supplied by the caller so one
/// animation can reuse sprites across all frames without global mutable
/// state; callers can also reuse it across renders for warm-cache behavior.
#[derive(Debug, Default)]
struct WheelLabelSpriteCache {
    entries: BTreeMap<(String, i32), WheelLabelSprite>,
    order: VecDeque<(String, i32)>,
    hits: usize,
    misses: usize,
}

impl WheelLabelSpriteCache {
    fn get_or_insert(&mut self, text: &str, scale: i32) -> WheelLabelSprite {
        let key = (text.to_owned(), scale);
        if let Some(sprite) = self.entries.get(&key) {
            self.hits = self.hits.saturating_add(1);
            let sprite = sprite.clone();
            if let Some(position) = self.order.iter().position(|entry| entry == &key) {
                self.order.remove(position);
            }
            self.order.push_back(key);
            return sprite;
        }
        self.misses = self.misses.saturating_add(1);
        let width = bitmap_text_width(text, scale).max(1);
        let height = 7 * scale;
        let sprite = WheelLabelSprite {
            width,
            height,
            mask: render_bitmap_mask(text, scale, width, height),
        };
        if self.entries.len() >= MAX_WHEEL_LABEL_SPRITES
            && let Some(evicted) = self.order.pop_front()
        {
            self.entries.remove(&evicted);
        }
        self.entries.insert(key.clone(), sprite.clone());
        self.order.push_back(key);
        sprite
    }
}

fn build_wheel_label_atlas(
    wedges: &[cama_app::wheel::WheelWedge],
    sprite_cache: &mut WheelLabelSpriteCache,
) -> WheelLabelAtlas {
    let wedge_count = wedges.len().max(1);
    let slice = std::f64::consts::TAU / wedge_count as f64;
    let radius = i32::from(WHEEL_MEDIA_SIZE) / 2 - 30;
    let mut atlas_indices = BTreeMap::<(String, i32), usize>::new();
    let mut sprites = Vec::new();
    let mut wedge_sprite_indices = Vec::with_capacity(wedges.len());
    for wedge in wedges {
        let text = match wedge.value {
            WheelValue::Numeric(value) if value > 0 => value.to_string(),
            _ => wedge.label.clone(),
        };
        let scale = label_scale(&text, slice, radius);
        let key = (text.clone(), scale);
        let sprite_index = if let Some(index) = atlas_indices.get(&key).copied() {
            index
        } else {
            let sprite = sprite_cache.get_or_insert(&text, scale);
            let index = sprites.len();
            atlas_indices.insert(key, index);
            sprites.push(sprite);
            index
        };
        wedge_sprite_indices.push(sprite_index);
    }
    WheelLabelAtlas {
        sprites,
        wedge_sprite_indices,
    }
}

fn draw_wheel_frame(
    pixels: &mut [u8],
    wedge_indices: &[u8],
    label_atlas: &WheelLabelAtlas,
    target_index: usize,
    frame_index: usize,
    golden: bool,
) {
    let size = i32::from(WHEEL_MEDIA_SIZE);
    let center = size / 2;
    let radius = center - 30;
    let inner_radius = radius / 3;
    let wedge_count = wedge_indices.len();
    let progress = frame_index as f64 / (WHEEL_MEDIA_FRAME_COUNT.saturating_sub(1)) as f64;
    let full_spins = std::f64::consts::TAU * 4.0 * (1.0 - progress);
    let target_rotation = std::f64::consts::TAU * target_index as f64 / wedge_count as f64;
    let rotation = full_spins + target_rotation;
    let slice = std::f64::consts::TAU / wedge_count as f64;
    for y in 0..size {
        for x in 0..size {
            let dx = x - center;
            let dy = y - center;
            let distance = ((dx * dx + dy * dy) as f64).sqrt();
            let index = usize::try_from(y * size + x).unwrap_or_default();
            if distance > f64::from(radius) {
                continue;
            }
            if distance <= f64::from(inner_radius) {
                pixels[index] = 3;
                continue;
            }
            if distance >= f64::from(radius - 6) {
                pixels[index] = 1;
                continue;
            }
            let angle = (f64::from(dx).atan2(f64::from(-dy)) + std::f64::consts::TAU)
                .rem_euclid(std::f64::consts::TAU);
            let wedge = ((angle + rotation).rem_euclid(std::f64::consts::TAU) / slice).floor()
                as usize
                % wedge_count;
            pixels[index] = wedge_indices[wedge];
        }
    }
    draw_wheel_labels(pixels, label_atlas, rotation);
    // Draw the red pointer over the wheel face so its target remains obvious.
    let pointer_top = center - radius - 22;
    let pointer_tip = center - radius + 30;
    for y in pointer_top..=pointer_tip {
        let progress = (y - pointer_top) as f64 / (pointer_tip - pointer_top).max(1) as f64;
        let half_width = (18.0 - 14.0 * progress).max(2.0) as i32;
        for x in (center - half_width)..=(center + half_width) {
            if (0..size).contains(&x) && (0..size).contains(&y) {
                pixels[usize::try_from(y * size + x).unwrap_or_default()] = 2;
            }
        }
    }
    // A gold hub distinguishes Golden Wheel without changing the policy face.
    if golden {
        for y in (center - inner_radius)..=(center + inner_radius) {
            for x in (center - inner_radius)..=(center + inner_radius) {
                let dx = x - center;
                let dy = y - center;
                if dx * dx + dy * dy <= inner_radius * inner_radius
                    && (0..size).contains(&x)
                    && (0..size).contains(&y)
                {
                    pixels[usize::try_from(y * size + x).unwrap_or_default()] = 1;
                }
            }
        }
    }
}

/// Draw compact, font-independent wedge labels.  The Python renderer uses a
/// bounded Pillow sprite cache; this adapter uses a fixed 5x7 glyph atlas so
/// every frame reuses the same measured text geometry and never loads a font
/// or allocates a per-frame layout.  Labels are intentionally horizontal after
/// wheel rotation, matching the Python visual contract.
fn draw_wheel_labels(pixels: &mut [u8], atlas: &WheelLabelAtlas, rotation: f64) {
    if atlas.wedge_sprite_indices.is_empty() {
        return;
    }
    let size = i32::from(WHEEL_MEDIA_SIZE);
    let center = size / 2;
    let radius = center - 30;
    let text_radius = radius * 68 / 100;
    let wedge_count = atlas.wedge_sprite_indices.len();
    let slice = std::f64::consts::TAU / wedge_count as f64;
    for (index, sprite_index) in atlas.wedge_sprite_indices.iter().copied().enumerate() {
        let Some(sprite) = atlas.sprites.get(sprite_index) else {
            continue;
        };
        let scale = sprite.height / 7;
        let width = sprite.width;
        let angle = index as f64 * slice + rotation + slice / 2.0;
        let x = f64::from(center) + f64::from(text_radius) * angle.sin();
        let y = f64::from(center) - f64::from(text_radius) * angle.cos();
        let left = x.round() as i32 - width / 2;
        let top = y.round() as i32 - (7 * scale) / 2;
        blit_bitmap_mask(pixels, sprite, left + 1, top + 1, 4);
        blit_bitmap_mask(pixels, sprite, left, top, 5);
    }
}

fn label_scale(text: &str, slice: f64, radius: i32) -> i32 {
    let arc_width =
        2.0 * std::f64::consts::PI * f64::from(radius) * 0.68 * slice / std::f64::consts::TAU;
    let max_width = (arc_width * 0.85).max(5.0);
    (2..=3)
        .rev()
        .find(|scale| f64::from(bitmap_text_width(text, *scale)) <= max_width)
        .unwrap_or(1)
}

fn bitmap_text_width(text: &str, scale: i32) -> i32 {
    text.chars().count() as i32 * 6 * scale - scale
}

fn render_bitmap_mask(text: &str, scale: i32, width: i32, height: i32) -> Vec<u8> {
    let mut mask = vec![0_u8; usize::try_from(width.saturating_mul(height)).unwrap_or_default()];
    for (char_index, character) in text.chars().enumerate() {
        let glyph = bitmap_glyph(character);
        let origin_x = char_index as i32 * 6 * scale;
        for (row, bits) in glyph.iter().copied().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = origin_x + column * scale + dx;
                        let y = row as i32 * scale + dy;
                        if (0..width).contains(&x) && (0..height).contains(&y) {
                            mask[usize::try_from(y * width + x).unwrap_or_default()] = 1;
                        }
                    }
                }
            }
        }
    }
    mask
}

fn blit_bitmap_mask(pixels: &mut [u8], sprite: &WheelLabelSprite, left: i32, top: i32, color: u8) {
    let size = i32::from(WHEEL_MEDIA_SIZE);
    for y in 0..sprite.height {
        for x in 0..sprite.width {
            let source = sprite.mask[usize::try_from(y * sprite.width + x).unwrap_or_default()];
            if source == 0 {
                continue;
            }
            let target_x = left + x;
            let target_y = top + y;
            if (0..size).contains(&target_x) && (0..size).contains(&target_y) {
                pixels[usize::try_from(target_y * size + target_x).unwrap_or_default()] = color;
            }
        }
    }
}

fn bitmap_glyph(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0f, 0x10, 0x10, 0x10, 0x10, 0x10, 0x0f],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0f, 0x10, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1f],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1b, 0x11],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x10, 0x1e, 0x01, 0x01, 0x1e],
        '6' => [0x0e, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x01, 0x0e],
        '+' => [0x00, 0x04, 0x04, 0x1f, 0x04, 0x04, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f],
        ' ' => [0; 7],
        _ => [0x1f, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x1f],
    }
}

fn draw_explosion_frame(pixels: &mut [u8], frame_index: usize) {
    let size = i32::from(WHEEL_MEDIA_SIZE);
    let center = size / 2;
    let progress = frame_index as f64 / EXPLOSION_MEDIA_FRAME_COUNT as f64;
    let radius = (progress * f64::from(center + 20)) as i32;
    for y in 0..size {
        for x in 0..size {
            let dx = x - center;
            let dy = y - center;
            let distance = ((dx * dx + dy * dy) as f64).sqrt() as i32;
            let index = usize::try_from(y * size + x).unwrap_or_default();
            if distance <= radius {
                pixels[index] = if distance < radius / 3 {
                    5
                } else if distance < radius / 2 {
                    4
                } else if distance < radius * 2 / 3 {
                    3
                } else {
                    2
                };
            } else if distance <= radius.saturating_add(5) {
                pixels[index] = 1;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WheelInteractionKind {
    TownTrial,
    Discover,
    Scrying,
    Reroll,
}

#[derive(Clone)]
struct PendingWheelInteraction {
    kind: WheelInteractionKind,
    user_id: i64,
    guild_id: i64,
    created_at: Instant,
    options: Vec<WheelOption>,
    /// Keep the complete wheel presentation so a Red-mana reroll can replace
    /// the uploaded GIF with the newly selected wedge.  The economic options
    /// above intentionally remain the smaller policy surface.
    wheel_wedges: Vec<cama_app::wheel::WheelWedge>,
    golden: bool,
    /// The exact bytes sent with the initial prompt.  Python edits the
    /// message without an `attachments=` argument when settling, which keeps
    /// that GIF visible; carrying the bytes through gives the typed responder
    /// the same result without changing explicit-clear semantics elsewhere.
    initial_attachment: Option<InteractionAttachment>,
    votes: BTreeMap<i64, usize>,
    spin_kind: WheelKind,
    balance_before: i64,
    effects: ManaEffects,
    event_id: String,
    config: BettingRuntimeConfig,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    bonus_spin: bool,
    last_regular_spin: Option<i64>,
    /// The original command responder lets a later button click replace the
    /// public wheel prompt with the settled result.  This is intentionally
    /// process-local, matching Python's in-memory Discord view lifetime.
    original_responder: Option<Arc<dyn InteractionResponder>>,
}

fn wheel_interaction_ttl(kind: WheelInteractionKind) -> Duration {
    match kind {
        WheelInteractionKind::TownTrial => Duration::from_secs(300),
        WheelInteractionKind::Discover => Duration::from_secs(60),
        WheelInteractionKind::Scrying | WheelInteractionKind::Reroll => Duration::from_secs(30),
    }
}

fn timeout_wheel_option(pending: &PendingWheelInteraction) -> WheelOption {
    match pending.kind {
        WheelInteractionKind::TownTrial => {
            let counts = pending.votes.values().fold(
                BTreeMap::<usize, usize>::new(),
                |mut counts, index| {
                    *counts.entry(*index).or_default() += 1;
                    counts
                },
            );
            let (_, winners) = counts.into_iter().fold(
                (0_usize, Vec::new()),
                |(best_count, mut winners), (index, count)| {
                    if count > best_count {
                        winners.clear();
                        winners.push(index);
                        (count, winners)
                    } else if count == best_count {
                        winners.push(index);
                        (best_count, winners)
                    } else {
                        (best_count, winners)
                    }
                },
            );
            winners
                .get(fastrand::usize(..winners.len().max(1)))
                .and_then(|index| pending.options.get(*index))
                .cloned()
                .unwrap_or(WheelOption {
                    label: "LOSE".to_owned(),
                    value: WheelValue::Numeric(0),
                    color: "#4a4a4a",
                })
        }
        WheelInteractionKind::Discover => pending
            .options
            .iter()
            .min_by(|left, right| {
                timeout_option_score(left)
                    .total_cmp(&timeout_option_score(right))
                    .then_with(|| left.label.cmp(&right.label))
            })
            .cloned()
            .unwrap_or(WheelOption {
                label: "LOSE".to_owned(),
                value: WheelValue::Numeric(0),
                color: "#4a4a4a",
            }),
        WheelInteractionKind::Scrying => pending
            .options
            .get(fastrand::usize(..pending.options.len().max(1)))
            .cloned()
            .unwrap_or(WheelOption {
                label: "LOSE".to_owned(),
                value: WheelValue::Numeric(0),
                color: "#4a4a4a",
            }),
        WheelInteractionKind::Reroll => pending.options.first().cloned().unwrap_or(WheelOption {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#4a4a4a",
        }),
    }
}

fn timeout_option_score(option: &WheelOption) -> f64 {
    match option.value {
        WheelValue::Numeric(value) => value as f64,
        WheelValue::Mechanic(mechanic) => match mechanic {
            WheelMechanic::LightningBolt => -65.0,
            WheelMechanic::BombOmb => -52.5,
            WheelMechanic::BananaPeel => -23.5,
            WheelMechanic::RedShell | WheelMechanic::BlueShell => -4.0,
            WheelMechanic::GreenShell
            | WheelMechanic::Comeback
            | WheelMechanic::Jailbreak
            | WheelMechanic::ExtendOne
            | WheelMechanic::ExtendTwo
            | WheelMechanic::Commune
            | WheelMechanic::Discover
            | WheelMechanic::TownTrial => 0.0,
            WheelMechanic::Emergency | WheelMechanic::Decay | WheelMechanic::Recession => -20.0,
            WheelMechanic::Heist
            | WheelMechanic::MarketCrash
            | WheelMechanic::CompoundInterest
            | WheelMechanic::TrickleDown
            | WheelMechanic::Dividend
            | WheelMechanic::HostileTakeover
            | WheelMechanic::Eruption
            | WheelMechanic::Overgrowth
            | WheelMechanic::ChainReaction => 10.0,
        },
    }
}

struct SpinResult {
    message: String,
    completion_message: Option<String>,
    completion_embed: Option<InteractionEmbed>,
    completion_delay: Duration,
    pending: Option<(String, PendingWheelInteraction)>,
    attachment: Option<InteractionAttachment>,
    neon_wheel_result: Option<i64>,
    neon_wheel_balance_before: Option<i64>,
    neon_wheel_balance: Option<i64>,
    neon_lightning: Option<(i64, usize)>,
    golden_announcement: Option<InteractionResponse>,
}

/// The durable settlement result returned by an interactive wheel choice.
///
/// Interactive views are process-local, but the economic resolution is still
/// committed through the SQLite closure.  Keep the Neon inputs alongside the
/// response text so a delayed button click follows the same event priority as
/// a direct `/gamba` spin after the response has been delivered.
#[derive(Debug)]
struct PendingWheelResolution {
    message: String,
    completion_embed: InteractionEmbed,
    neon_wheel_result: Option<i64>,
    neon_wheel_balance_before: Option<i64>,
    neon_wheel_balance: Option<i64>,
    neon_lightning: Option<(i64, usize)>,
}

#[derive(Clone, Debug)]
enum WheelComponentAction {
    Vote { key: Option<String>, index: usize },
    Choose { key: Option<String>, index: usize },
    Reroll { key: Option<String>, reroll: bool },
}

enum WheelComponentDecision {
    Reply(String),
    Resolve(String, PendingWheelInteraction, WheelComponentAction),
    ResolveTimeout(String, PendingWheelInteraction, WheelOption),
}

impl WheelComponentAction {
    fn key(&self) -> Option<&str> {
        match self {
            Self::Vote { key, .. } | Self::Choose { key, .. } | Self::Reroll { key, .. } => {
                key.as_deref()
            }
        }
    }

    fn index(&self) -> Option<usize> {
        match self {
            Self::Vote { index, .. } | Self::Choose { index, .. } => Some(*index),
            Self::Reroll { reroll, .. } => Some(usize::from(*reroll)),
        }
    }

    fn matches_kind(&self, kind: WheelInteractionKind) -> bool {
        matches!(
            (self, kind),
            (Self::Vote { .. }, WheelInteractionKind::TownTrial)
                | (Self::Choose { .. }, WheelInteractionKind::Discover)
                | (Self::Choose { .. }, WheelInteractionKind::Scrying)
                | (Self::Reroll { .. }, WheelInteractionKind::Reroll)
        )
    }
}

#[derive(Clone, Debug)]
struct PendingDisburseVotes {
    guild_id: i64,
    requester_id: i64,
    proposal: DisbursementProposal,
    votes: Vec<DisbursementVote>,
    page: usize,
    created_at: Instant,
}

struct BettingInteractionHandler {
    database_path: PathBuf,
    config: BettingRuntimeConfig,
    discord: Arc<dyn DiscordTransport>,
    vanity_tax: Arc<dyn BettingVanityTaxPort>,
    neon: Mutex<NeonDegenService>,
    wager_refresh: Mutex<Option<Arc<dyn BettingWagerRefreshPort>>>,
    rates: Mutex<BTreeMap<RateKey, Vec<Instant>>>,
    wheel_interactions: Mutex<BTreeMap<String, PendingWheelInteraction>>,
    /// Generation/claim keys for component and timeout resolution. Keeping a
    /// pending entry while SQLite is in flight allows a transient database
    /// failure to retry without permitting a concurrent double settlement.
    wheel_in_flight: Mutex<BTreeSet<String>>,
    disburse_vote_pages: Mutex<BTreeMap<String, PendingDisburseVotes>>,
}

#[async_trait]
impl InteractionHandler for BettingInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let result = match request {
            InteractionRequest::Autocomplete { .. } => self.autocomplete(request, responder).await,
            InteractionRequest::Component { .. } => self.component(request, responder).await,
            InteractionRequest::Command { .. } => self.command(request, responder).await,
            InteractionRequest::Modal { .. } => {
                Err("betting extension received an unsupported modal".to_owned())
            }
        };
        result.map_err(Into::into)
    }
}

impl BettingInteractionHandler {
    async fn expire_pending_views(&self) -> Result<usize, String> {
        let expired = {
            let interactions = self
                .wheel_interactions
                .lock()
                .map_err(|_| "wheel interaction state is poisoned".to_owned())?;
            let mut in_flight = self
                .wheel_in_flight
                .lock()
                .map_err(|_| "wheel interaction claim state is poisoned".to_owned())?;
            let keys = interactions
                .iter()
                .filter(|(key, pending)| {
                    !in_flight.contains(*key)
                        && pending.created_at.elapsed() > wheel_interaction_ttl(pending.kind)
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| {
                    let pending = interactions.get(&key).cloned()?;
                    in_flight.insert(key.clone());
                    Some((key, pending))
                })
                .collect::<Vec<_>>()
        };
        let mut settled = 0;
        let mut first_error = None;
        for (key, pending) in expired {
            let path = self.database_path.clone();
            let config = pending.config.clone();
            let event_id = pending.event_id.clone();
            let effects = pending.effects.clone();
            let original_responder = pending.original_responder.clone();
            let vanity_taxable_ids = self.vanity_tax.taxable_ids(pending.guild_id);
            let option = timeout_wheel_option(&pending);
            let result = sqlite("expired wheel interaction sweep", move || {
                resolve_pending_wheel(
                    &path,
                    pending.guild_id,
                    pending.user_id,
                    pending.balance_before,
                    pending.spin_kind,
                    &effects,
                    &config,
                    &event_id,
                    &option,
                    unix_seconds()?,
                    false,
                    pending.bonus_spin,
                    pending.last_regular_spin,
                    pending.event_win_multiplier,
                    pending.event_loss_multiplier,
                    &vanity_taxable_ids,
                )
            })
            .await;
            match result {
                Ok(result) => {
                    if let Ok(mut interactions) = self.wheel_interactions.lock() {
                        interactions.remove(&key);
                    }
                    if let Ok(mut in_flight) = self.wheel_in_flight.lock() {
                        in_flight.remove(&key);
                    }
                    if let Some(original_responder) = original_responder {
                        let _ = original_responder
                            .edit_original(wheel_completion_response(
                                format!("This Wheel interaction timed out. {}", result.message),
                                Some(result.completion_embed.clone()),
                                pending.initial_attachment.clone(),
                            ))
                            .await;
                    }
                    settled += 1;
                }
                Err(error) => {
                    if let Ok(mut in_flight) = self.wheel_in_flight.lock() {
                        in_flight.remove(&key);
                    }
                    first_error.get_or_insert(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(settled)
    }

    async fn command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            name,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            member_permissions,
            options,
            ..
        } = request
        else {
            return Err("invalid betting command payload".to_owned());
        };
        let user_id = signed_id(user_id, "user")?;
        let guild_id = guild_id
            .map(|value| signed_id(value, "guild"))
            .transpose()?;
        let qualified = qualified_name(&name, &options);
        match qualified.as_str() {
            "bet" => {
                self.bet(user_id, guild_id, channel_id, &options, &responder)
                    .await
            }
            "mybets" => self.mybets(user_id, guild_id, &responder).await,
            "bets" => {
                self.bets(
                    user_id,
                    guild_id,
                    member_permissions.unwrap_or_default(),
                    &options,
                    &responder,
                )
                .await
            }
            "balance" => {
                self.balance(user_id, user_display_name, guild_id, channel_id, &responder)
                    .await
            }
            "gamba" => {
                self.gamba(
                    user_id,
                    guild_id,
                    channel_id,
                    member_permissions.unwrap_or_default(),
                    &responder,
                    false,
                )
                .await
            }
            "economy tip" => {
                self.tip(
                    user_id,
                    user_display_name,
                    guild_id,
                    channel_id,
                    &options,
                    &responder,
                )
                .await
            }
            "economy invest" => self.invest(user_id, guild_id, &options, &responder).await,
            "economy paydebt" => self.paydebt(user_id, guild_id, &options, &responder).await,
            "economy bankruptcy" => {
                self.bankruptcy(user_id, guild_id, channel_id, &responder)
                    .await
            }
            "economy loan" => {
                self.loan(user_id, guild_id, channel_id, &options, &responder)
                    .await
            }
            "economy reserve" => self.reserve(guild_id, &responder).await,
            "economy disburse" => {
                self.disburse(
                    user_id,
                    guild_id,
                    channel_id,
                    member_permissions.unwrap_or_default(),
                    &options,
                    &responder,
                )
                .await
            }
            _ => Err(format!("betting handler received command {qualified:?}")),
        }
    }

    async fn autocomplete(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Autocomplete {
            name,
            guild_id,
            focused_value,
            ..
        } = request
        else {
            return Err("invalid betting autocomplete payload".to_owned());
        };
        if name != "bet" && name != "bets" {
            return responder
                .autocomplete(Vec::new())
                .await
                .map_err(|error| error.to_string());
        }
        let Some(guild_id) = guild_id else {
            return responder
                .autocomplete(Vec::new())
                .await
                .map_err(|error| error.to_string());
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let path = self.database_path.clone();
        let mut choices = sqlite("betting match autocomplete", move || {
            PendingMatchRepository::new(path)
                .pending_matches(guild_id)
                .map_err(|error| error.to_string())
        })
        .await?
        .into_iter()
        .map(|pending| CommandOptionChoice::Integer {
            name: format!("Match #{}", pending.pending_match_id),
            value: pending.pending_match_id,
        })
        .filter(|choice| match choice {
            CommandOptionChoice::Integer { name, .. } => {
                focused_value.is_empty()
                    || name
                        .to_ascii_lowercase()
                        .contains(&focused_value.to_ascii_lowercase())
            }
            _ => false,
        })
        .collect::<Vec<_>>();
        choices.truncate(25);
        responder
            .autocomplete(choices)
            .await
            .map_err(|error| error.to_string())
    }

    async fn component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            custom_id,
            user_id,
            guild_id,
            channel_id,
            ..
        } = request
        else {
            return Err("invalid betting component payload".to_owned());
        };
        if let Some((page_key, page_action)) = parse_disbursement_page_component(&custom_id) {
            let guild_id = guild_id
                .map(|value| signed_id(value, "guild"))
                .transpose()?
                .ok_or_else(|| GUILD_ONLY_MESSAGE.to_owned())?;
            let user_id = signed_id(user_id, "user")?;
            return self
                .disburse_votes_page(&page_key, page_action, guild_id, user_id, &responder)
                .await;
        }
        if let Some(method) = custom_id.strip_prefix(DISBURSE_COMPONENT_PREFIX) {
            let guild_id = guild_id
                .map(|value| signed_id(value, "guild"))
                .transpose()?
                .ok_or_else(|| GUILD_ONLY_MESSAGE.to_owned())?;
            let user_id = signed_id(user_id, "user")?;
            let method = method.trim().to_owned();
            if !DISBURSE_METHODS.contains(&method.as_str()) {
                return respond_ephemeral(&responder, "That Reserve vote is not available.").await;
            }
            if self.reserve_voting_restricted(guild_id).await? {
                return respond_ephemeral(
                    &responder,
                    "Reserve voting is paused by the active economy event.",
                )
                .await;
            }
            let path = self.database_path.clone();
            let result = sqlite("disbursement vote", move || {
                let players = PlayerRepository::new(&path);
                if !players
                    .exists(user_id, Some(guild_id))
                    .map_err(|error| error.to_string())?
                {
                    return Err(
                        "You must be registered to vote. Use `/player register` first.".to_owned(),
                    );
                }
                let proposal = DisbursementRepository::new(&path)
                    .add_vote(Some(guild_id), user_id, &method)
                    .map_err(|error| error.to_string())?;
                Ok((proposal, method.to_owned()))
            })
            .await;
            let (proposal, method) = match result {
                Ok(value) => value,
                Err(error) => return respond_ephemeral(&responder, &error).await,
            };
            if proposal.quorum_reached() {
                let result = self.execute_disbursement(guild_id, &proposal).await?;
                self.update_persistent_disbursement_message(
                    &proposal,
                    disbursement_proposal_response_with_disabled_buttons(&proposal),
                )
                .await;
                return responder
                    .respond(disbursement_response(&result, false))
                    .await
                    .map_err(|error| error.to_string());
            }
            self.update_persistent_disbursement_message(
                &proposal,
                disbursement_proposal_response(&proposal),
            )
            .await;
            return respond_ephemeral(
                &responder,
                &format!(
                    "Your vote for **{}** has been recorded! ({}/{})",
                    disburse_method_label(&method),
                    proposal.total_votes(),
                    proposal.quorum_required
                ),
            )
            .await;
        }
        if let Some(action) = parse_wheel_component(&custom_id) {
            let guild_id = guild_id
                .map(|value| signed_id(value, "guild"))
                .transpose()?
                .ok_or_else(|| GUILD_ONLY_MESSAGE.to_owned())?;
            let user_id = signed_id(user_id, "user")?;
            return self
                .wheel_component(action, user_id, guild_id, channel_id, &responder)
                .await;
        }
        Err(format!("unknown betting component {custom_id:?}"))
    }

    async fn wheel_component(
        &self,
        action: WheelComponentAction,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<u64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let decision: Result<WheelComponentDecision, String> = 'lookup: {
            let mut interactions = self
                .wheel_interactions
                .lock()
                .map_err(|_| "wheel interaction state is poisoned".to_owned())?;
            let mut in_flight = self
                .wheel_in_flight
                .lock()
                .map_err(|_| "wheel interaction claim state is poisoned".to_owned())?;
            let key = action.key().and_then(|candidate| {
                interactions
                    .get(candidate)
                    .filter(|pending| pending.guild_id == guild_id)
                    .map(|_| candidate.to_owned())
            });
            let key = key.or_else(|| {
                interactions
                    .iter()
                    .find(|(_, pending)| {
                        pending.guild_id == guild_id
                            && action.matches_kind(pending.kind)
                            && action
                                .index()
                                .is_some_and(|index| index < pending.options.len())
                    })
                    .map(|(key, _)| key.clone())
            });
            let Some(key) = key else {
                break 'lookup Ok(WheelComponentDecision::Reply(
                    "This Wheel interaction expired. Run /gamba again to start a new spin."
                        .to_owned(),
                ));
            };
            let pending = interactions.get(&key).ok_or_else(|| {
                "This Wheel interaction expired. Run /gamba again to start a new spin.".to_owned()
            })?;
            if pending.guild_id != guild_id {
                break 'lookup Ok(WheelComponentDecision::Reply(
                    "This Wheel interaction expired. Run /gamba again to start a new spin."
                        .to_owned(),
                ));
            }
            if in_flight.contains(&key) {
                break 'lookup Ok(WheelComponentDecision::Reply(
                    "This Wheel interaction is already being resolved. Please wait.".to_owned(),
                ));
            }
            if pending.created_at.elapsed() > wheel_interaction_ttl(pending.kind) {
                let pending = interactions.get(&key).cloned().ok_or_else(|| {
                    "This Wheel interaction expired. Run /gamba again to start a new spin."
                        .to_owned()
                })?;
                in_flight.insert(key.clone());
                break 'lookup Ok(WheelComponentDecision::ResolveTimeout(
                    key,
                    pending.clone(),
                    timeout_wheel_option(&pending),
                ));
            }
            if let WheelComponentAction::Vote { index, .. } = &action {
                if *index >= pending.options.len() {
                    break 'lookup Ok(WheelComponentDecision::Reply(
                        "That Wheel option is no longer available.".to_owned(),
                    ));
                }
                let pending = interactions.get_mut(&key).ok_or_else(|| {
                    "This Wheel interaction expired. Run /gamba again to start a new spin."
                        .to_owned()
                })?;
                pending.votes.insert(user_id, *index);
                let label = pending
                    .options
                    .get(*index)
                    .map(|option| option.label.as_str())
                    .unwrap_or("that option");
                break 'lookup Ok(WheelComponentDecision::Reply(format!(
                    "Voted for **{label}**!"
                )));
            }
            if pending.user_id != user_id {
                let message = match pending.kind {
                    WheelInteractionKind::Scrying => "This isn't your scrying!",
                    WheelInteractionKind::Reroll => "Not your spin!",
                    WheelInteractionKind::Discover => "This choice isn't yours to make.",
                    WheelInteractionKind::TownTrial => "Voters may participate in the Town Trial.",
                };
                break 'lookup Ok(WheelComponentDecision::Reply(message.to_owned()));
            }
            let pending = interactions.get(&key).cloned().ok_or_else(|| {
                "This Wheel interaction expired. Run /gamba again to start a new spin.".to_owned()
            })?;
            in_flight.insert(key.clone());
            Ok(WheelComponentDecision::Resolve(key, pending, action))
        };
        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => return respond_ephemeral(responder, &error).await,
        };
        let pending = match decision {
            WheelComponentDecision::Reply(message) => {
                return respond_ephemeral(responder, &message).await;
            }
            WheelComponentDecision::ResolveTimeout(key, pending, option) => {
                let original_responder = pending.original_responder.clone();
                let path = self.database_path.clone();
                let config = pending.config.clone();
                let event_id = pending.event_id.clone();
                let effects = pending.effects.clone();
                let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
                let result = sqlite("expired wheel interaction resolution", move || {
                    resolve_pending_wheel(
                        &path,
                        guild_id,
                        pending.user_id,
                        pending.balance_before,
                        pending.spin_kind,
                        &effects,
                        &config,
                        &event_id,
                        &option,
                        unix_seconds()?,
                        false,
                        pending.bonus_spin,
                        pending.last_regular_spin,
                        pending.event_win_multiplier,
                        pending.event_loss_multiplier,
                        &vanity_taxable_ids,
                    )
                })
                .await;
                let result = match result {
                    Ok(result) => result,
                    Err(error) => {
                        if let Ok(mut in_flight) = self.wheel_in_flight.lock() {
                            in_flight.remove(&key);
                        }
                        return Err(error);
                    }
                };
                if let Ok(mut interactions) = self.wheel_interactions.lock() {
                    interactions.remove(&key);
                }
                if let Ok(mut in_flight) = self.wheel_in_flight.lock() {
                    in_flight.remove(&key);
                }
                let timeout_message =
                    format!("This Wheel interaction timed out. {}", result.message);
                let response = responder
                    .respond(
                        InteractionResponse::message(&timeout_message)
                            .ephemeral()
                            .embed(result.completion_embed.clone()),
                    )
                    .await
                    .map_err(|error| error.to_string());
                if response.is_ok() {
                    if let Some(original_responder) = original_responder {
                        let _ = original_responder
                            .edit_original(wheel_completion_response(
                                timeout_message,
                                Some(result.completion_embed.clone()),
                                pending.initial_attachment.clone(),
                            ))
                            .await;
                    }
                    self.emit_pending_neon(channel_id, user_id, guild_id, &result)
                        .await;
                }
                return response;
            }
            WheelComponentDecision::Resolve(key, pending, action) => {
                let use_reroll = match &action {
                    WheelComponentAction::Reroll { reroll, .. } => *reroll,
                    _ => false,
                };
                let index = action
                    .index()
                    .ok_or_else(|| "That Wheel option is no longer available.".to_owned())?;
                let option = pending
                    .options
                    .get(index)
                    .cloned()
                    .ok_or_else(|| "That Wheel option is no longer available.".to_owned())?;
                (key, pending, option, use_reroll)
            }
        };
        let (key, pending, option, use_reroll) = pending;
        let original_responder = pending.original_responder.clone();
        let final_attachment = if use_reroll {
            wheel_attachment_for_option(&pending, &option)
                .or_else(|| pending.initial_attachment.clone())
        } else {
            pending.initial_attachment.clone()
        };
        let path = self.database_path.clone();
        let config = pending.config.clone();
        let event_id = pending.event_id.clone();
        let event_win_multiplier = pending.event_win_multiplier;
        let event_loss_multiplier = pending.event_loss_multiplier;
        let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
        let result = sqlite("wheel interaction resolution", move || {
            resolve_pending_wheel(
                &path,
                guild_id,
                pending.user_id,
                pending.balance_before,
                pending.spin_kind,
                &pending.effects,
                &config,
                &event_id,
                &option,
                unix_seconds()?,
                use_reroll,
                pending.bonus_spin,
                pending.last_regular_spin,
                event_win_multiplier,
                event_loss_multiplier,
                &vanity_taxable_ids,
            )
        })
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if let Ok(mut in_flight) = self.wheel_in_flight.lock() {
                    in_flight.remove(&key);
                }
                return Err(error);
            }
        };
        if let Ok(mut interactions) = self.wheel_interactions.lock() {
            interactions.remove(&key);
        }
        if let Ok(mut in_flight) = self.wheel_in_flight.lock() {
            in_flight.remove(&key);
        }
        let response = responder
            .respond(
                InteractionResponse::message(&result.message)
                    .ephemeral()
                    .embed(result.completion_embed.clone()),
            )
            .await
            .map_err(|error| error.to_string());
        if response.is_ok() {
            if let Some(original_responder) = original_responder {
                let _ = original_responder
                    .edit_original(wheel_completion_response(
                        result.message.clone(),
                        Some(result.completion_embed.clone()),
                        final_attachment,
                    ))
                    .await;
            }
            self.emit_pending_neon(channel_id, user_id, guild_id, &result)
                .await;
        }
        response
    }

    async fn bet(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        options: &[InteractionOption],
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        if !self.take_rate("bet", guild_id, user_id, 5, Duration::from_secs(20))? {
            return respond_ephemeral(responder, "⏳ Please wait before using `/bet` again.").await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let team = match parse_string(options, "team") {
            Some(team) => team,
            None => return followup_ephemeral(responder, "Choose Radiant or Dire.").await,
        };
        let team = match parse_team(&team) {
            Ok(team) => team,
            Err(error) => return followup_ephemeral(responder, &error).await,
        };
        let amount = match parse_i64(options, "amount") {
            Some(amount) => amount,
            None => return followup_ephemeral(responder, "Amount must be positive.").await,
        };
        if amount < self.config.min_bet {
            return followup_ephemeral(
                responder,
                &format!("Minimum bet is {} {}.", self.config.min_bet, JOPACOIN_EMOTE),
            )
            .await;
        }
        let leverage = parse_i64(options, "leverage").unwrap_or(1);
        if !is_valid_leverage_tier(leverage) {
            return followup_ephemeral(responder, "Invalid leverage selection.").await;
        }
        let requested_match = parse_i64(options, "match");
        let path = self.database_path.clone();
        let selected = sqlite("betting pending match selection", move || {
            select_pending(&path, guild_id, user_id, requested_match)
        })
        .await?;
        let Some(pending) = selected else {
            let message = requested_match.map_or_else(
                || "❌ No active match to bet on.".to_owned(),
                |match_id| format!("❌ Match #{match_id} not found or already completed."),
            );
            return followup_ephemeral(responder, &message).await;
        };
        let effects = {
            let path = self.database_path.clone();
            sqlite("bet mana effects", move || {
                active_mana_effects(&path, user_id, Some(guild_id), unix_seconds()?)
            })
            .await
            .unwrap_or_default()
        };
        if leverage == 10 && !effects.red_10x_leverage {
            return followup_ephemeral(
                responder,
                "❌ 10x leverage is exclusive to **Red mana (Mountain)** players!",
            )
            .await;
        }
        let pending_match_id = pending.pending_match_id;
        let betting_mode = pending.state.betting_mode.clone();
        let now = unix_seconds()?;
        let max_debt = self.config.max_debt;
        let path = self.database_path.clone();
        let result = sqlite("betting place wager", move || {
            let repo = BettingServiceRepository::new(path);
            let totals = repo
                .get_pending_totals(Some(guild_id), 0, Some(pending_match_id))
                .ok();
            let odds_at_placement = totals.and_then(|totals| {
                let total = totals.total() as f64;
                let side = match team {
                    BettingTeam::Radiant => totals.radiant,
                    BettingTeam::Dire => totals.dire,
                } as f64;
                (side > 0.0).then_some(total / side)
            });
            repo.place_bet_atomic(PlaceBetRequest {
                guild_id: Some(guild_id),
                pending_match_id,
                discord_id: user_id,
                team,
                amount,
                bet_time: now,
                leverage,
                max_debt,
                is_blind: false,
                odds_at_placement,
            })
            .map_err(|error| error.to_string())
        })
        .await;
        match result {
            Ok(record) => {
                let effective = record.effective_amount();
                let activity_path = self.database_path.clone();
                let activity_source = format!("bet:{}", record.bet_id);
                let activity_now = now;
                let _ = sqlite("bet pet activity", move || {
                    record_betting_pet_activity(
                        &activity_path,
                        user_id,
                        guild_id,
                        PetActivity::BetPlaced,
                        &activity_source,
                        activity_now,
                    );
                    Ok::<_, String>(())
                })
                .await;
                if effects.match_bet_steady_bonus > 0 {
                    let base_bonus = cama_domain::economy_scaling::scale_minigame_jc_delta(
                        effects.match_bet_steady_bonus as f64,
                        self.config.minigame_jc_delta_scale,
                    );
                    let event_config = economy_event_config(&self.config);
                    let event_path = self.database_path.clone();
                    let bonus = sqlite("bet economy-event reward", move || {
                        SqliteEconomyEventService::new(event_path, event_config)
                            .adjust_reward_at(guild_id, base_bonus, unix_seconds()?)
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .unwrap_or(base_bonus);
                    if bonus > 0 {
                        let path = self.database_path.clone();
                        let metadata = json!({
                            "base_reward": effects.match_bet_steady_bonus,
                            "adjusted_reward": bonus,
                        });
                        let _ = sqlite("bet Green mana bonus", move || {
                            ShopRuntimeRepository::new(path)
                                .adjust_balance(
                                    user_id,
                                    guild_id,
                                    bonus,
                                    Some("mana_reward"),
                                    Some(user_id),
                                    Some("match_bet_steady_bonus"),
                                    Some(&pending_match_id.to_string()),
                                    Some("scaled Green mana bet-placement reward"),
                                    Some(&metadata),
                                )
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        })
                        .await;
                    }
                }
                let badge = format_mana_badge(Some(&effects));
                let prefix = if badge.is_empty() {
                    String::new()
                } else {
                    format!("{badge} ")
                };
                let match_note = format!(" (Match #{pending_match_id})");
                let leverage_note = if leverage > 1 {
                    format!(
                        " at {leverage}x leverage (effective: {effective} {})",
                        JOPACOIN_EMOTE
                    )
                } else {
                    String::new()
                };
                let pool_warning = if betting_mode == "pool" {
                    "\n⚠️ Pool mode: odds may shift as more bets come in. Use `/mybets` to check current EV."
                } else {
                    ""
                };
                let response = followup_ephemeral(
                    responder,
                    &format!(
                        "{prefix}Bet placed{match_note}: {amount} {} on {}{leverage_note}.{pool_warning}",
                        JOPACOIN_EMOTE,
                        team_name(team)
                    ),
                )
                .await;
                if response.is_ok() {
                    // Python increments total_bets_placed only after a
                    // successful response.  Keep that ordering, but make
                    // the update durable and return one snapshot for every
                    // rare Neon candidate rather than re-reading a mutable
                    // Player value for each trigger.
                    let counter_path = self.database_path.clone();
                    let counter = sqlite("betting counter update", move || {
                        BettingServiceRepository::new(counter_path)
                            .record_successful_bet_counter(user_id, Some(guild_id), leverage)
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .ok();
                    let lock_until = pending.state.lock_time.or(pending.state.bet_lock_until);
                    let seconds_remaining = lock_until
                        .map(|lock| lock.saturating_sub(now).max(0))
                        .unwrap_or_default();
                    self.emit_bet_neon(
                        channel_id,
                        user_id,
                        guild_id,
                        amount,
                        leverage,
                        team,
                        counter,
                        seconds_remaining,
                    )
                    .await;
                    self.refresh_wager_message(guild_id, pending_match_id).await;
                }
                response
            }
            Err(error) => followup_ephemeral(responder, &format!("❌ {error}")).await,
        }
    }

    async fn refresh_wager_message(&self, guild_id: i64, pending_match_id: i64) {
        let port = self
            .wager_refresh
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().cloned());
        let Some(port) = port else {
            return;
        };
        if let Err(error) = port.refresh_wager_message(guild_id, pending_match_id).await {
            tracing::warn!(
                %error,
                guild_id,
                pending_match_id,
                "betting wager-message refresh failed"
            );
        }
    }

    async fn mybets(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        if !self.take_rate("mybets", guild_id, user_id, 5, DEFAULT_RATE_WINDOW)? {
            return respond_ephemeral(responder, "Please wait before using /mybets again.").await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.database_path.clone();
        let bets = sqlite("betting active wagers", move || {
            BettingServiceRepository::new(path)
                .get_pending_bets(Some(guild_id), Some(user_id), 0, None)
                .map_err(|error| error.to_string())
        })
        .await?;
        if bets.is_empty() {
            return followup_ephemeral(responder, "You have no active bets.").await;
        }
        let path = self.database_path.clone();
        let pending = sqlite("betting active match context", move || {
            PendingMatchRepository::new(path)
                .pending_matches(guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        let pending_by_id = pending
            .into_iter()
            .map(|match_state| (Some(match_state.pending_match_id), match_state))
            .collect::<BTreeMap<_, _>>();
        let mut by_match = BTreeMap::<Option<i64>, Vec<PendingBetRecord>>::new();
        for bet in bets {
            by_match.entry(bet.pending_match_id).or_default().push(bet);
        }
        let multiple_matches = by_match.len() > 1;
        let mut sections = Vec::new();
        for (pending_match_id, match_bets) in by_match {
            let pending_state = pending_match_id.and_then(|id| pending_by_id.get(&Some(id)));
            let pool_total =
                if pending_state.is_some_and(|pending| pending.state.betting_mode == "pool") {
                    let path = self.database_path.clone();
                    Some(
                        sqlite("betting mybets pool total", move || {
                            BettingServiceRepository::new(path)
                                .get_pending_totals(Some(guild_id), 0, pending_match_id)
                                .map(|totals| totals.total())
                                .map_err(|error| error.to_string())
                        })
                        .await?,
                    )
                } else {
                    None
                };
            let mut team_sections = Vec::new();
            for team in [BettingTeam::Radiant, BettingTeam::Dire] {
                let team_bets = match_bets
                    .iter()
                    .filter(|bet| bet.team == team)
                    .collect::<Vec<_>>();
                if team_bets.is_empty() {
                    continue;
                }
                let total_amount = team_bets.iter().map(|bet| bet.amount).sum::<i64>();
                let total_effective = team_bets
                    .iter()
                    .map(|bet| bet.effective_amount())
                    .sum::<i64>();
                let mut lines = team_bets
                    .iter()
                    .enumerate()
                    .map(|(index, bet)| {
                        let leverage = if bet.leverage > 1 {
                            format!(
                                " at {}x (effective: {} {})",
                                bet.leverage,
                                bet.effective_amount(),
                                JOPACOIN_EMOTE
                            )
                        } else {
                            String::new()
                        };
                        let auto_tag = if bet.is_blind { " (auto)" } else { "" };
                        format!(
                            "{}. {} {}{leverage}{auto_tag} — <t:{}:t>",
                            index + 1,
                            bet.amount,
                            JOPACOIN_EMOTE,
                            bet.bet_time
                        )
                    })
                    .collect::<Vec<_>>();
                if team_bets.len() > 1 {
                    if total_amount != total_effective {
                        lines.push(format!(
                            "\n**Total:** {total_amount} {JOPACOIN_EMOTE} (effective: {total_effective} {JOPACOIN_EMOTE})"
                        ));
                    } else {
                        lines.push(format!("\n**Total:** {total_amount} {JOPACOIN_EMOTE}"));
                    }
                }
                let header = if team_bets.len() == 1 {
                    format!("**{}:**", team_name(team))
                } else {
                    format!("**{}** ({} bets):", team_name(team), team_bets.len())
                };
                let mut section = format!("{header}\n{}", lines.join("\n"));
                if let Some(pending) = pending_state {
                    let path = self.database_path.clone();
                    let totals = sqlite("betting mybets odds", move || {
                        BettingServiceRepository::new(path)
                            .get_pending_totals(Some(guild_id), 0, pending_match_id)
                            .map_err(|error| error.to_string())
                    })
                    .await?;
                    let my_team_total = match team {
                        BettingTeam::Radiant => totals.radiant,
                        BettingTeam::Dire => totals.dire,
                    };
                    if pending.state.betting_mode == "pool" {
                        if my_team_total > 0 && totals.total() > 0 {
                            let seed_against = match team {
                                BettingTeam::Radiant => pending.state.bet_seed_dire,
                                BettingTeam::Dire => pending.state.bet_seed_radiant,
                            };
                            let seeded_pool = totals.total().saturating_add(seed_against);
                            let potential = (seeded_pool as f64 * total_effective as f64
                                / my_team_total as f64)
                                as i64;
                            let odds = seeded_pool as f64 / my_team_total as f64 - 1.0;
                            section.push_str(&format!(
                                "\n📊 If {} wins: ~{potential} {JOPACOIN_EMOTE} ({odds:.2}:1)",
                                team_name(team)
                            ));
                        }
                    } else {
                        let mut potential = total_effective.saturating_mul(2);
                        let mut bonus = 0;
                        if pending.state.bet_seed_bonus > 0 && my_team_total > 0 {
                            bonus = pending.state.bet_seed_bonus.saturating_mul(total_effective)
                                / my_team_total;
                            potential = potential.saturating_add(bonus);
                        }
                        let bonus_text = if bonus > 0 {
                            format!(" + ~{bonus} bonus")
                        } else {
                            String::new()
                        };
                        section.push_str(&format!(
                            "\nIf {} wins: {potential} {JOPACOIN_EMOTE} (1:1{bonus_text})",
                            team_name(team)
                        ));
                    }
                }
                team_sections.push(section);
            }
            if team_sections.len() > 1 {
                team_sections.push(
                    "_You have bets on both teams; only one side wins — payouts are not additive._"
                        .to_owned(),
                );
                if let Some(pool_total) = pool_total {
                    team_sections.push(format!(
                        "📊 **Pool total:** {pool_total} {JOPACOIN_EMOTE} (odds may change)"
                    ));
                }
            }
            let label = if multiple_matches {
                pending_match_id
                    .map(|id| format!(" (Match #{id})"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            sections.push(format!("{}{}", team_sections.join("\n\n"), label));
        }
        followup_ephemeral(responder, &sections.join("\n\n---\n\n")).await
    }

    async fn bets(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        permissions: u64,
        options: &[InteractionOption],
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        let is_admin = permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            || self.config.admin_user_ids.contains(&user_id);
        if !is_admin && !self.take_rate("bets", guild_id, user_id, 1, Duration::from_secs(60))? {
            return respond_ephemeral(responder, "Please wait before using /bets again.").await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let requested_match = parse_i64(options, "match");
        let path = self.database_path.clone();
        let pending = sqlite("betting pool selection", move || {
            select_pending(&path, guild_id, user_id, requested_match)
        })
        .await?;
        let Some(pending) = pending else {
            return followup_ephemeral(responder, "No active match to show bets for.").await;
        };
        let pending_match_id = pending.pending_match_id;
        let path = self.database_path.clone();
        let bets = sqlite("betting pool rows", move || {
            BettingServiceRepository::new(path)
                .get_pending_bets(Some(guild_id), None, 0, Some(pending_match_id))
                .map_err(|error| error.to_string())
        })
        .await?;
        if bets.is_empty() {
            return followup_ephemeral(responder, "No bets placed yet.").await;
        }
        let path = self.database_path.clone();
        let totals = sqlite("betting pool totals", move || {
            BettingServiceRepository::new(path)
                .get_pending_totals(Some(guild_id), 0, Some(pending_match_id))
                .map_err(|error| error.to_string())
        })
        .await?;
        let lock_until = pending.state.bet_lock_until.or(pending.state.lock_time);
        let betting_open = lock_until.is_some_and(|lock| lock > unix_seconds().unwrap_or_default());
        let total = totals.total();
        let mode = if pending.state.betting_mode == "house" {
            BettingMode::House
        } else {
            BettingMode::Pool
        };
        let seed = SeedSplit {
            radiant: pending.state.bet_seed_radiant,
            dire: pending.state.bet_seed_dire,
            bonus: pending.state.bet_seed_bonus,
        };
        let overview = bets_overview(
            pending_match_id,
            bets.len(),
            mode,
            BetTotals {
                radiant: totals.radiant,
                dire: totals.dire,
            },
            seed,
        );
        let mut odds_text = overview.current_odds;
        if let Some(lock) = lock_until {
            odds_text.push_str(&format!("\nBetting closes <t:{lock}:R>"));
        }
        let mut embed = InteractionEmbed::titled(overview.title)
            .color(0xF1_C4_0F)
            .field("Current Odds", odds_text, false);
        for (team, label) in [
            (BettingTeam::Radiant, "🟢 Radiant"),
            (BettingTeam::Dire, "🔴 Dire"),
        ] {
            let rows = bets
                .iter()
                .filter(|bet| bet.team == team)
                .collect::<Vec<_>>();
            if rows.is_empty() {
                continue;
            }
            let mut lines = rows
                .iter()
                .take(15)
                .enumerate()
                .map(|(index, bet)| {
                    let bettor = if is_admin || !betting_open {
                        format!("<@{}>", bet.discord_id)
                    } else {
                        format!("Bettor #{}", index + 1)
                    };
                    let auto = if bet.is_blind { " (auto)" } else { "" };
                    let leverage = if bet.leverage > 1 {
                        format!(" at {}x → {} eff", bet.leverage, bet.effective_amount())
                    } else {
                        String::new()
                    };
                    let odds_at_placement = bet
                        .odds_at_placement
                        .filter(|odds| odds.is_finite() && *odds > 0.0)
                        .map_or_else(String::new, |odds| format!(" • {odds:.2}x"));
                    format!(
                        "{bettor} • {} {}{leverage}{auto}{odds_at_placement}",
                        bet.amount, JOPACOIN_EMOTE,
                    )
                })
                .collect::<Vec<_>>();
            if rows.len() > 15 {
                lines.push(format!("... +{} more", rows.len() - 15));
            }
            embed = embed.field(
                format!("{label} Bets ({})", rows.len()),
                lines.join("\n"),
                false,
            );
        }
        let summary_name = if mode == BettingMode::Pool {
            "Pool Summary"
        } else {
            "House Summary"
        };
        let summary_total_label = if mode == BettingMode::Pool {
            "Total"
        } else {
            "Total staked"
        };
        embed = embed.field(
            summary_name,
            format!(
                "**{summary_total_label}:** {total} {} effective\nRadiant: {} ({:.0}%) | Dire: {} ({:.0}%)",
                JOPACOIN_EMOTE,
                totals.radiant,
                pct(totals.radiant, total),
                totals.dire,
                pct(totals.dire, total)
            ),
            false,
        );
        responder
            .followup(InteractionResponse::message("").embed(embed).ephemeral())
            .await
            .map_err(|error| error.to_string())
    }

    async fn balance(
        &self,
        user_id: i64,
        _display_name: String,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.take_rate(
            "balance",
            guild_id.unwrap_or_default(),
            user_id,
            5,
            DEFAULT_RATE_WINDOW,
        )? {
            return respond_ephemeral(responder, "Please wait before using `/balance` again.")
                .await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.database_path.clone();
        let details = sqlite("betting balance read", move || {
            let players = PlayerRepository::new(&path);
            let player = players
                .get_by_id(user_id, guild_id)
                .map_err(|error| error.to_string())?;
            let bankruptcy = BankruptcyRepository::new(&path)
                .get_state(user_id, guild_id)
                .map_err(|error| error.to_string())?;
            let loan = LoanRepository::new(&path)
                .get_state(user_id, guild_id)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((player, bankruptcy, loan))
        })
        .await?;
        let Some(player) = details.0 else {
            return followup_ephemeral(
                responder,
                "You need to /player register before checking your balance.",
            )
            .await;
        };
        let balance = player.jopacoin_balance;
        let penalty = details
            .1
            .filter(|state| state.penalty_games_remaining > 0)
            .map(|state| {
                format!(
                    "\n**Bankruptcy penalty:** {:.0}% win bonus for {} more win(s)",
                    self.config.bankruptcy_penalty_rate * 100.0,
                    state.penalty_games_remaining
                )
            })
            .unwrap_or_default();
        let loan = details.2.map_or_else(String::new, |state| {
            let mut content = String::new();
            if state.outstanding_principal > 0 {
                content.push_str(&format!(
                    "\n⚠️ **Outstanding loan:** {} {} (repaid after next match)",
                    state.outstanding_total().unwrap_or_default(),
                    JOPACOIN_EMOTE
                ));
            }
            if state.total_loans_taken > 0 {
                content.push_str(&format!(
                    "\n**Loans taken:** {} (fees paid: {})",
                    state.total_loans_taken, state.total_fees_paid
                ));
            }
            if let Some(last_loan_at) = state.last_loan_at {
                let remaining = self.config.loan_cooldown_seconds.saturating_sub(
                    unix_seconds()
                        .unwrap_or_default()
                        .saturating_sub(last_loan_at),
                );
                if remaining > 0 {
                    content.push_str(&format!(
                        "\n**Loan cooldown:** {}h {}m remaining",
                        remaining / 3_600,
                        (remaining % 3_600) / 60
                    ));
                }
            }
            content
        });
        let effects = {
            let path = self.database_path.clone();
            active_mana_effects(&path, user_id, guild_id, unix_seconds().unwrap_or_default())
                .unwrap_or_default()
        };
        let badge = format_mana_badge(Some(&effects));
        let prefix = if badge.is_empty() {
            String::new()
        } else {
            format!("{badge} ")
        };
        let content = if balance >= 0 {
            format!(
                "{prefix}<@{user_id}> has {balance} {}.{penalty}{loan}",
                JOPACOIN_EMOTE
            )
        } else {
            format!(
                "{prefix}<@{user_id}> has **{balance}** {} (in debt)\n\
                 Garnishment: {:.0}% of winnings go to debt repayment{penalty}{loan}\n\n\
                 Use /economy bankruptcy to clear your debt (with penalties).\n\
                 Use /economy loan to borrow more jopacoin (with a fee).",
                JOPACOIN_EMOTE,
                self.config.garnishment_rate * 100.0,
            )
        };
        let response = followup_ephemeral(responder, &content).await;
        if response.is_ok()
            && let Some(guild_id) = guild_id
        {
            self.emit_balance_neon(channel_id, user_id, guild_id, balance)
                .await;
        }
        response
    }

    async fn emit_neon_result(&self, channel_id: u64, result: NeonResult) {
        let _ = self.emit_neon_result_checked(channel_id, result).await;
    }

    async fn emit_neon_result_checked(&self, channel_id: u64, result: NeonResult) -> bool {
        let mut response = InteractionResponse::message(
            result.text_block.or(result.footer_text).unwrap_or_default(),
        )
        .without_mentions();
        if let Some(gif) = result.gif_file {
            response = response.attachment(InteractionAttachment::bytes(
                "jopat_terminal.gif",
                gif.bytes,
            ));
        }
        let has_attachment = !response.attachments.is_empty();
        let first = self
            .discord
            .send_message(channel_id, DiscordMessage::silent(response.clone()))
            .await;
        let receipt = match first {
            Ok(receipt) => Some(receipt),
            Err(_) if has_attachment => {
                let mut text_only = response;
                text_only.attachments.clear();
                self.discord
                    .send_message(channel_id, DiscordMessage::silent(text_only))
                    .await
                    .ok()
            }
            Err(_) => None,
        };
        let delivered = receipt.is_some();
        if let Some(receipt) = receipt {
            let discord = Arc::clone(&self.discord);
            tokio::spawn(async move {
                tokio::time::sleep(BETTING_NEON_DELETE_DELAY).await;
                let _ = discord
                    .delete_message(receipt.channel_id, receipt.message_id)
                    .await;
            });
        }
        delivered
    }

    async fn emit_gamba_neon(
        &self,
        channel_id: u64,
        user_id: i64,
        guild_id: i64,
        value: i64,
        prior_balance: i64,
        new_balance: i64,
    ) -> bool {
        let Ok(discord_id) = u64::try_from(user_id) else {
            return false;
        };
        let Ok(guild_id) = u64::try_from(guild_id) else {
            return false;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            (value < 0).then(|| {
                neon.on_wheel_result_with_prior_balance(
                    discord_id,
                    Some(guild_id),
                    value,
                    Some(prior_balance),
                    new_balance,
                )
            })?
        });
        let fired = result.is_some();
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
        fired
    }

    async fn emit_gamba_lightning(
        &self,
        channel_id: u64,
        user_id: i64,
        guild_id: i64,
        total_taxed: i64,
        players_hit: usize,
    ) -> bool {
        let (Ok(discord_id), Ok(guild_id)) = (u64::try_from(user_id), u64::try_from(guild_id))
        else {
            return false;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            neon.on_lightning_bolt(discord_id, Some(guild_id), total_taxed, players_hit)
        });
        let fired = result.is_some();
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
        fired
    }

    async fn emit_pending_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        guild_id: i64,
        result: &PendingWheelResolution,
    ) {
        let Some(channel_id) = channel_id else {
            return;
        };
        let wheel_fired =
            result
                .neon_wheel_result
                .filter(|value| *value < 0)
                .map(|value| async move {
                    self.emit_gamba_neon(
                        channel_id,
                        user_id,
                        guild_id,
                        value,
                        result.neon_wheel_balance_before.unwrap_or_default(),
                        result.neon_wheel_balance.unwrap_or_default(),
                    )
                    .await
                });
        let wheel_fired = match wheel_fired {
            Some(future) => future.await,
            None => false,
        };
        if wheel_fired {
            return;
        }
        let lightning_fired = if let Some((total, players)) = result.neon_lightning {
            self.emit_gamba_lightning(channel_id, user_id, guild_id, total, players)
                .await
        } else {
            false
        };
        if !lightning_fired {
            self.emit_gamba_milestone(channel_id, user_id, guild_id)
                .await;
        }
    }

    async fn emit_gamba_milestone(&self, channel_id: u64, user_id: i64, guild_id: i64) -> bool {
        let (Ok(discord_id), Ok(guild_id)) = (u64::try_from(user_id), u64::try_from(guild_id))
        else {
            return false;
        };
        let path = self.database_path.clone();
        let score = sqlite("gamba degen score", move || {
            GamblingStatsService::new(GamblingStatsRepository::new(path))
                .calculate_degen_score(user_id, Some(guild_id as i64))
                .map(|score| u8::try_from(score.total.clamp(0, 100)).unwrap_or(0))
                .map_err(|error| error.to_string())
        })
        .await
        .ok();
        let Some(score) = score else {
            return false;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            neon.on_degen_milestone(discord_id, Some(guild_id), score)
        });
        let fired = result.is_some();
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
        fired
    }

    #[allow(clippy::too_many_arguments)]
    async fn emit_bet_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        guild_id: i64,
        amount: i64,
        leverage: i64,
        team: BettingTeam,
        counter: Option<BetCounterReceipt>,
        seconds_remaining: i64,
    ) {
        let Some(channel_id) = channel_id else {
            return;
        };
        let (Ok(discord_id), Ok(guild_id), Ok(leverage)) = (
            u64::try_from(user_id),
            u64::try_from(guild_id),
            u8::try_from(leverage),
        ) else {
            return;
        };
        let database_guild_id = i64::try_from(guild_id).unwrap_or_default();
        let now = unix_seconds().unwrap_or_default();
        let (result, first_leverage_fired) = self
            .neon
            .lock()
            .ok()
            .map(|mut neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                let mut first_leverage_fired = false;
                let result = counter.and_then(|receipt| {
                    if receipt.first_leverage_candidate
                        && let Some(result) =
                            neon.on_first_leverage_bet(discord_id, Some(guild_id), leverage)
                    {
                        first_leverage_fired = true;
                        return Some(result);
                    }
                    if receipt.total_bets_placed == 100
                        && let Some(result) = neon.on_100_bets_milestone(
                            discord_id,
                            Some(guild_id),
                            receipt.total_bets_placed,
                        )
                    {
                        return Some(result);
                    }
                    let balance_before = receipt.balance_after.saturating_add(amount);
                    if balance_before > 0
                        && let Some(result) =
                            neon.on_all_in_bet(discord_id, Some(guild_id), amount, balance_before)
                    {
                        return Some(result);
                    }
                    if seconds_remaining > 0
                        && seconds_remaining <= 60
                        && let Some(result) =
                            neon.on_last_second_bet(discord_id, Some(guild_id), seconds_remaining)
                    {
                        return Some(result);
                    }
                    None
                });
                let result = result.or_else(|| {
                    neon.on_bet_placed(
                        discord_id,
                        Some(guild_id),
                        amount,
                        leverage,
                        team_name(team),
                    )
                });
                (result, first_leverage_fired)
            })
            .unwrap_or((None, false));
        if first_leverage_fired {
            let path = self.database_path.clone();
            let _ = sqlite("bet first leverage marker", move || {
                BettingServiceRepository::new(path)
                    .mark_first_leverage_used(user_id, Some(database_guild_id))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .await;
        }
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
    }

    async fn emit_balance_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        guild_id: i64,
        balance: i64,
    ) {
        let Some(channel_id) = channel_id else {
            return;
        };
        let (Ok(discord_id), Ok(guild_id)) = (u64::try_from(user_id), u64::try_from(guild_id))
        else {
            return;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            neon.on_balance_check(discord_id, Some(guild_id), balance)
        });
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
    }

    async fn emit_cooldown_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        guild_id: Option<i64>,
        scope: &str,
    ) -> bool {
        let Some(channel_id) = channel_id else {
            return false;
        };
        let (Ok(discord_id), Ok(guild_id)) = (
            u64::try_from(user_id),
            guild_id.map_or(Ok(0), u64::try_from),
        ) else {
            return false;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            neon.on_cooldown_hit(discord_id, Some(guild_id), scope)
        });
        let fired = result.is_some();
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
        fired
    }

    #[allow(clippy::too_many_arguments)]
    async fn emit_tip_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        guild_id: i64,
        sender_name: &str,
        recipient_name: &str,
        amount: i64,
        fee: i64,
    ) {
        let Some(channel_id) = channel_id else {
            return;
        };
        let (Ok(discord_id), Ok(guild_id)) = (u64::try_from(user_id), u64::try_from(guild_id))
        else {
            return;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            neon.on_tip(
                discord_id,
                Some(guild_id),
                sender_name,
                recipient_name,
                amount,
                fee,
            )
        });
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
    }

    async fn emit_loan_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        total_owed: i64,
        is_negative: bool,
    ) {
        let Some(channel_id) = channel_id else {
            return;
        };
        let (Ok(discord_id), Ok(guild_id)) = (
            u64::try_from(user_id),
            guild_id.map_or(Ok(0), u64::try_from),
        ) else {
            return;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            neon.on_loan(discord_id, Some(guild_id), amount, total_owed, is_negative)
        });
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
    }

    async fn emit_bankruptcy_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        guild_id: Option<i64>,
        debt_cleared: i64,
        filing_number: i64,
    ) -> bool {
        let Some(channel_id) = channel_id else {
            return false;
        };
        let (Ok(discord_id), Ok(guild_id), Ok(filing_number)) = (
            u64::try_from(user_id),
            guild_id.map_or(Ok(0), u64::try_from),
            u32::try_from(filing_number),
        ) else {
            return false;
        };
        let now = unix_seconds().unwrap_or_default();
        let result = self.neon.lock().ok().and_then(|mut neon| {
            neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
            neon.on_bankruptcy(discord_id, Some(guild_id), debt_cleared, filing_number)
        });
        let fired = result.is_some();
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
        fired
    }

    async fn gamba(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        member_permissions: u64,
        responder: &Arc<dyn InteractionResponder>,
        bonus_spin: bool,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        if !bonus_spin {
            let Some(channel_id) = channel_id else {
                let _ = charge_wrong_channel(&self.database_path, user_id, guild_id).await;
                return respond_ephemeral(
                    responder,
                    "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.",
                )
                .await;
            };
            let guild_raw =
                u64::try_from(guild_id).map_err(|_| "guild id is negative".to_owned())?;
            let gamba_channel = self.discord.mafia_gamba_channel_id(guild_raw).await?;
            let direct_match = gamba_channel == Some(channel_id);
            let parent_match = if direct_match {
                true
            } else if let Some(parent_id) = self
                .discord
                .channel_parent_id(guild_raw, channel_id)
                .await?
            {
                gamba_channel == Some(parent_id)
            } else {
                false
            };
            if !parent_match {
                let _ = charge_wrong_channel(&self.database_path, user_id, guild_id).await;
                return respond_ephemeral(
                    responder,
                    "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.",
                )
                .await;
            }
        }
        if !bonus_spin && !self.take_rate("gamba", guild_id, user_id, 1, Duration::from_secs(2))? {
            let response = responder
                .respond(
                    InteractionResponse::message("The wheel is still spinning. Try again shortly.")
                        .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string());
            if response.is_ok()
                && let (Ok(discord_id), Ok(guild_id)) =
                    (u64::try_from(user_id), u64::try_from(guild_id))
            {
                let now = unix_seconds().unwrap_or_default();
                let result = self.neon.lock().ok().and_then(|mut neon| {
                    neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                    neon.on_cooldown_hit(discord_id, Some(guild_id), "gamba")
                });
                if let Some(result) = result
                    && let Some(channel_id) = channel_id
                {
                    self.emit_neon_result(channel_id, result).await;
                }
            }
            return response;
        }
        let now = unix_seconds()?;
        let member_snapshot = self.guild_member_snapshot(guild_id).await;
        let admin_bypass = self.config.admin_user_ids.contains(&user_id)
            || member_permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0;
        let config = self.config.clone();
        let path = self.database_path.clone();
        let event_config = economy_event_config(&config);
        let event_path = self.database_path.clone();
        let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
        let (event_win_multiplier, event_loss_multiplier) =
            sqlite("gamba economy-event effects", move || {
                let service = SqliteEconomyEventService::new(event_path, event_config);
                service
                    .effects_at(guild_id, now)
                    .map(|effects| (effects.gamba_win_multiplier, effects.gamba_loss_multiplier))
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or((1.0, 1.0));
        let outcome = sqlite("gamba spin", move || {
            spin_once(
                &path,
                guild_id,
                user_id,
                now,
                &config,
                event_win_multiplier,
                event_loss_multiplier,
                &vanity_taxable_ids,
                admin_bypass,
                bonus_spin,
                member_snapshot.as_ref(),
            )
        })
        .await?;
        let completion_message = outcome.completion_message.clone();
        let completion_embed = outcome.completion_embed.clone();
        let completion_delay = outcome.completion_delay;
        let initial_attachment = outcome.attachment.clone();
        let neon_wheel_result = outcome.neon_wheel_result;
        let neon_wheel_balance_before = outcome.neon_wheel_balance_before;
        let neon_wheel_balance = outcome.neon_wheel_balance;
        let neon_lightning = outcome.neon_lightning;
        if let (Some(announcement), Some(channel_id)) =
            (outcome.golden_announcement.clone(), channel_id)
            && let Err(error) = self
                .discord
                .send_message(channel_id, DiscordMessage::default_mentions(announcement))
                .await
        {
            tracing::warn!(%error, user_id, guild_id, "golden wheel announcement failed");
        }
        let neon_channel_id = channel_id.unwrap_or_default();
        // Python sends the wheel reveal publicly in the configured gamba
        // channel. Errors (wrong channel/cooldown/registration) remain
        // ephemeral above; only the successful reveal is public.
        let pending_key = outcome.pending.as_ref().map(|(key, _)| key.clone());
        let mut response = InteractionResponse::message(outcome.message);
        if let Some(attachment) = outcome.attachment {
            response = response.attachment(attachment);
        }
        if let Some((key, pending)) = outcome.pending {
            let rows = wheel_interaction_rows(&key, &pending);
            self.wheel_interactions
                .lock()
                .map_err(|_| "wheel interaction state is poisoned".to_owned())?
                .insert(key, pending);
            response = response.action_rows(rows);
        }
        let response_has_attachment = !response.attachments.is_empty();
        let mut attachment_delivered = false;
        let delivery = match responder.respond(response.clone()).await {
            Ok(()) => {
                attachment_delivered = response_has_attachment;
                Ok(())
            }
            Err(error) if !response.attachments.is_empty() => {
                // Discord can reject a generated media upload (size, transient
                // CDN failure, or an adapter without attachment support). Keep
                // the settled result visible with a text-only retry; the money
                // transaction already committed before this delivery boundary.
                let mut text_only = response;
                text_only.attachments.clear();
                responder.respond(text_only).await.map_err(|retry_error| {
                    format!("gamba response failed ({error}); text fallback failed ({retry_error})")
                })
            }
            Err(error) => Err(error.to_string()),
        };
        if delivery.is_ok()
            && let Some(pending_key) = pending_key
            && let Ok(mut interactions) = self.wheel_interactions.lock()
            && let Some(pending) = interactions.get_mut(&pending_key)
        {
            pending.original_responder = Some(Arc::clone(responder));
            if !attachment_delivered {
                pending.initial_attachment = None;
            }
        }
        if delivery.is_ok()
            && let Some(completion_message) = completion_message
        {
            // Python keeps the explosion attachment visible for the full
            // animation before replacing the reveal copy.  The settlement
            // already committed before this delivery boundary; an edit
            // failure therefore degrades to the initial attachment message.
            tokio::time::sleep(completion_delay).await;
            let completion = wheel_completion_response(
                completion_message,
                completion_embed,
                attachment_delivered.then_some(initial_attachment).flatten(),
            );
            let _ = responder.edit_original(completion).await;
        }
        if delivery.is_ok() {
            let wheel_fired =
                neon_wheel_result
                    .filter(|value| *value < 0)
                    .map(|value| async move {
                        self.emit_gamba_neon(
                            neon_channel_id,
                            user_id,
                            guild_id,
                            value,
                            neon_wheel_balance_before.unwrap_or_default(),
                            neon_wheel_balance.unwrap_or_default(),
                        )
                        .await
                    });
            let wheel_fired = match wheel_fired {
                Some(future) => future.await,
                None => false,
            };
            if !wheel_fired {
                let lightning_fired = if let Some((total, players)) = neon_lightning {
                    self.emit_gamba_lightning(neon_channel_id, user_id, guild_id, total, players)
                        .await
                } else {
                    false
                };
                if !lightning_fired {
                    self.emit_gamba_milestone(neon_channel_id, user_id, guild_id)
                        .await;
                }
            }
        }
        delivery
    }

    /// Resolve one immutable member snapshot before the wheel's SQLite
    /// transaction.  `None` means the transport/cache could not provide a
    /// snapshot, so the Python helper's compatibility behavior preserves raw
    /// repository ordering. `Some(empty)` is a real, authoritative empty
    /// guild and therefore removes every departed player from rank mechanics.
    async fn guild_member_snapshot(&self, guild_id: i64) -> Option<BTreeSet<i64>> {
        let guild = u64::try_from(guild_id).ok()?;
        let path = self.database_path.clone();
        let ids = sqlite("gamba member snapshot candidates", move || {
            PlayerRepository::new(path)
                .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
                .map(|players| {
                    players
                        .into_iter()
                        .filter_map(|player| player.discord_id)
                        .collect::<Vec<_>>()
                })
                .map_err(|error| error.to_string())
        })
        .await
        .ok()?;
        let mut visible = BTreeSet::new();
        for id in ids {
            let Ok(user_id) = u64::try_from(id) else {
                continue;
            };
            match self.discord.guild_member(guild, user_id).await {
                Ok(Some(_)) => {
                    visible.insert(id);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%error, guild_id, user_id, "gamba member snapshot unavailable");
                    return None;
                }
            }
        }
        Some(visible)
    }

    async fn tip(
        &self,
        user_id: i64,
        user_display_name: String,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        options: &[InteractionOption],
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.take_rate(
            "tip",
            guild_id.unwrap_or_default(),
            user_id,
            5,
            DEFAULT_RATE_WINDOW,
        )? {
            return respond_ephemeral(responder, "Please wait before using `/economy tip` again.")
                .await;
        }
        let target = parse_nested_user(options, "player")
            .ok_or_else(|| "Choose a player to tip.".to_owned())?;
        let amount = parse_nested_i64(options, "amount")
            .ok_or_else(|| "Amount must be positive.".to_owned())?;
        if amount <= 0 {
            return respond_ephemeral(responder, "Amount must be positive.").await;
        }
        if target == user_id {
            return respond_ephemeral(responder, "You cannot tip yourself.").await;
        }
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let effects = {
            let path = self.database_path.clone();
            sqlite("tip mana effects", move || {
                active_mana_effects(&path, user_id, Some(guild_id), unix_seconds()?)
            })
            .await
            .unwrap_or_default()
        };
        let fee =
            if effects.color.as_deref() == Some("White") && effects.plains_tip_fee_rate.is_some() {
                0
            } else {
                ((amount as f64 * self.config.tip_fee_rate).ceil() as i64).max(1)
            };
        let tithe = if effects.plains_tithe_rate > 0.0 {
            ((amount as f64 * effects.plains_tithe_rate) as i64).max(1)
        } else {
            0
        };
        let path = self.database_path.clone();
        let result = sqlite("economy tip", move || {
            let players = PlayerRepository::new(&path);
            let Some(sender) = players
                .get_by_id(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?
            else {
                return Err("You need to `/player register` before you can tip.".to_owned());
            };
            let total_cost = amount
                .checked_add(fee)
                .and_then(|value| value.checked_add(tithe))
                .ok_or_else(|| "Tip amount is too large.".to_owned())?;
            if sender.jopacoin_balance < total_cost {
                let mut breakdown = format!("{amount} tip + {fee} fee");
                if tithe > 0 {
                    breakdown.push_str(&format!(" + {tithe} tithe"));
                }
                return Err(format!(
                    "Insufficient balance. You need {total_cost} {JOPACOIN_EMOTE} ({breakdown}). You have {} {JOPACOIN_EMOTE}.",
                    sender.jopacoin_balance
                ));
            }
            if !players
                .exists(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?
            {
                return Err("You need to /player register before you can tip.".to_owned());
            }
            if !players
                .exists(target, Some(guild_id))
                .map_err(|error| error.to_string())?
            {
                return Err(format!("<@{target}> is not registered."));
            }
            let loan = LoanRepository::new(&path)
                .get_state(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            if let Some(state) = loan.filter(|state| state.outstanding_principal > 0) {
                return Err(format!(
                    "You cannot tip while you have an outstanding loan. Play a match to repay your loan ({} {}).",
                    state.outstanding_total().unwrap_or_default(),
                    JOPACOIN_EMOTE,
                ));
            }
            TipRepository::new(&path)
                .tip_atomic(Some(guild_id), user_id, target, amount, fee, tithe)
                .map_err(|error| error.to_string())
        })
        .await;
        match result {
            Ok(transfer) => {
                // Python keeps the wallet transfer and its human-facing tip
                // history as separate operations: history failure must not
                // roll back a successful tip.  Preserve that boundary while
                // using the persisted row id as the pet activity key.
                let history_path = self.database_path.clone();
                let tip_id = sqlite("tip history", move || {
                    TipRepository::new(history_path)
                        .log_tip(
                            user_id,
                            target,
                            transfer.amount,
                            transfer.fee,
                            Some(guild_id),
                        )
                        .map_err(|error| error.to_string())
                })
                .await
                .ok();
                if let Some(tip_id) = tip_id {
                    let activity_path = self.database_path.clone();
                    let activity_source = format!("tip:{tip_id}");
                    let activity_now = unix_seconds().unwrap_or_default();
                    let _ = sqlite("tip pet activity", move || {
                        record_betting_pet_activity(
                            &activity_path,
                            user_id,
                            guild_id,
                            PetActivity::TipSent,
                            &activity_source,
                            activity_now,
                        );
                        Ok::<_, String>(())
                    })
                    .await;
                }
                let mut mana_notes = Vec::new();
                if effects.green_steady_bonus > 0 {
                    let base_bonus = cama_domain::economy_scaling::scale_minigame_jc_delta(
                        effects.green_steady_bonus as f64,
                        self.config.minigame_jc_delta_scale,
                    );
                    let event_config = economy_event_config(&self.config);
                    let event_path = self.database_path.clone();
                    let bonus = sqlite("tip economy-event reward", move || {
                        SqliteEconomyEventService::new(event_path, event_config)
                            .adjust_reward_at(guild_id, base_bonus, unix_seconds()?)
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .unwrap_or(base_bonus);
                    if bonus > 0 {
                        let path = self.database_path.clone();
                        let metadata = json!({
                            "base_reward": effects.green_steady_bonus,
                            "adjusted_reward": bonus,
                        });
                        if sqlite("tip Green mana bonus", move || {
                            ShopRuntimeRepository::new(path)
                                .adjust_balance(
                                    target,
                                    guild_id,
                                    bonus,
                                    Some("mana_reward"),
                                    Some(user_id),
                                    Some("tip_steady_bonus"),
                                    Some(&user_id.to_string()),
                                    Some("scaled Green mana tip reward"),
                                    Some(&metadata),
                                )
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .is_ok()
                        {
                            mana_notes.push(format!("🌲 +{bonus} bonus to recipient"));
                        }
                    }
                }
                if effects.swamp_self_tax > 0 {
                    let path = self.database_path.clone();
                    let tax = effects.swamp_self_tax;
                    if sqlite("tip Swamp self tax", move || {
                        ShopRuntimeRepository::new(path)
                            .adjust_balance(
                                user_id,
                                guild_id,
                                -tax,
                                Some("mana"),
                                Some(user_id),
                                Some("swamp_self_tax"),
                                Some(&user_id.to_string()),
                                Some("Swamp mana tip tax"),
                                None,
                            )
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .is_ok()
                    {
                        mana_notes.push(format!("🌿 Swamp tax: -{tax}"));
                    }
                }
                if effects.swamp_siphon {
                    let path = self.database_path.clone();
                    let event_id = format!("tip:{user_id}:{target}:{}", unix_seconds()?);
                    let scale = self.config.minigame_jc_delta_scale;
                    if let Ok(Some(siphoned)) = sqlite("tip Swamp siphon", move || {
                        apply_swamp_siphon(&path, guild_id, user_id, scale, &event_id)
                    })
                    .await
                    {
                        mana_notes.push(format!("🌿 Siphon: +{siphoned}"));
                    }
                }
                if tithe > 0 {
                    mana_notes.push(format!("🌾 Tithe: -{tithe}"));
                }
                let suffix = if mana_notes.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", mana_notes.join(" | "))
                };
                let badge = format_mana_badge(Some(&effects));
                let prefix = if badge.is_empty() {
                    String::new()
                } else {
                    format!("{badge} ")
                };
                let response = responder
                    .followup(InteractionResponse::message(format!(
                        "{prefix}<@{user_id}> tipped {amount} {JOPACOIN_EMOTE} to <@{target}>! \
                         ({fee} {JOPACOIN_EMOTE} fee to Jopacoin Reserve){suffix}"
                    )))
                    .await
                    .map_err(|error| error.to_string());
                if response.is_ok() {
                    let target_name = {
                        let path = self.database_path.clone();
                        sqlite("tip Neon recipient lookup", move || {
                            Ok::<_, String>(
                                PlayerRepository::new(path)
                                    .get_by_id(target, Some(guild_id))
                                    .map_err(|error| error.to_string())?
                                    .map(|player| player.name)
                                    .unwrap_or_else(|| format!("<@{target}>")),
                            )
                        })
                        .await
                        .unwrap_or_else(|_| format!("<@{target}>"))
                    };
                    self.emit_tip_neon(
                        channel_id,
                        user_id,
                        guild_id,
                        &user_display_name,
                        &target_name,
                        amount,
                        fee,
                    )
                    .await;
                }
                response
            }
            Err(error) => followup_ephemeral(responder, &error).await,
        }
    }

    async fn paydebt(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        options: &[InteractionOption],
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.take_rate(
            "paydebt",
            guild_id.unwrap_or_default(),
            user_id,
            5,
            DEFAULT_RATE_WINDOW,
        )? {
            return respond_ephemeral(
                responder,
                "Please wait before using `/economy paydebt` again.",
            )
            .await;
        }
        let target = parse_nested_user(options, "player")
            .ok_or_else(|| "Choose a player whose debt to pay.".to_owned())?;
        let amount = parse_nested_i64(options, "amount")
            .ok_or_else(|| "Amount must be positive.".to_owned())?;
        if amount <= 0 {
            return respond_ephemeral(responder, "Amount must be positive.").await;
        }
        if target == user_id {
            return respond_ephemeral(responder, "You cannot pay your own debt.").await;
        }
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.database_path.clone();
        let result: Result<DebtPayment, String> = sqlite("economy debt payment", move || {
            LoanRepository::new(path)
                .pay_debt_atomic(user_id, target, Some(guild_id), amount)
                .map_err(|error| error.to_string())
        })
        .await;
        match result {
            Ok(payment) => responder
                .followup(InteractionResponse::message(format!(
                    "<@{user_id}> paid {} {} toward <@{target}>'s debt!",
                    payment.amount_paid, JOPACOIN_EMOTE
                )))
                .await
                .map_err(|error| error.to_string()),
            Err(error) => followup_ephemeral(responder, &error).await,
        }
    }

    async fn bankruptcy(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.take_rate(
            "bankruptcy",
            guild_id.unwrap_or_default(),
            user_id,
            2,
            Duration::from_secs(30),
        )? {
            return respond_ephemeral(
                responder,
                "The bankruptcy court requires you to wait before filing again.",
            )
            .await;
        }
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let path = self.database_path.clone();
        let effects_path = self.database_path.clone();
        let effects = sqlite("bankruptcy mana effects", move || {
            active_mana_effects(&effects_path, user_id, guild_id, unix_seconds()?)
        })
        .await
        .unwrap_or_default();
        let request = DeclarationRequest {
            discord_id: user_id,
            guild_id,
            fresh_start_balance: self.config.bankruptcy_fresh_start_balance,
            cooldown_seconds: self.config.bankruptcy_cooldown_seconds,
            penalty_games: self.config.bankruptcy_penalty_games,
            now: unix_seconds()?,
        };
        let result = sqlite("economy bankruptcy", move || {
            BankruptcyRepository::new(path)
                .declare_atomic(request)
                .map_err(|error| error.to_string())
        })
        .await?;
        let mut neon_data = None;
        let bankruptcy_cooldown_hit = matches!(&result, DeclarationOutcome::Cooldown { .. });
        let content = match result {
            DeclarationOutcome::Applied {
                debt_cleared,
                mut penalty_games,
                bankruptcy_count,
                ..
            } => {
                neon_data = Some((debt_cleared, bankruptcy_count));
                if effects.color.as_deref() == Some("Black")
                    && effects.swamp_bankruptcy_games < penalty_games
                {
                    let reduction = penalty_games - effects.swamp_bankruptcy_games;
                    let path = self.database_path.clone();
                    if sqlite("bankruptcy Swamp penalty reduction", move || {
                        BankruptcyRepository::new(path)
                            .adjust_penalty_games_atomic(user_id, guild_id, -reduction)
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .is_ok()
                    {
                        penalty_games = effects.swamp_bankruptcy_games;
                    }
                }
                format!(
                    "**<@{user_id}> HAS DECLARED BANKRUPTCY**\n\n\
                     **Details:**\n\
                     Debt cleared: {debt_cleared} {JOPACOIN_EMOTE}\n\
                     Penalty: {:.0}% win bonus until you **WIN** {penalty_games} games\n\
                     New balance: 0 {JOPACOIN_EMOTE}",
                    self.config.bankruptcy_penalty_rate * 100.0
                )
            }
            DeclarationOutcome::PlayerNotFound => {
                "You need to /player register before you can declare bankruptcy.".to_owned()
            }
            DeclarationOutcome::NotInDebt { balance } => format!(
                "<@{user_id}> tried to declare bankruptcy, but has no debt.\n\
                 Their balance: {balance} {JOPACOIN_EMOTE}"
            ),
            DeclarationOutcome::Cooldown { remaining_seconds } => format!(
                "<@{user_id}> cannot file again yet. Try again <t:{}:R>.",
                unix_seconds()?.saturating_add(remaining_seconds)
            ),
        };
        let response = responder
            .followup(InteractionResponse::message(content))
            .await
            .map_err(|error| error.to_string());
        if response.is_ok()
            && let Some((debt_cleared, filing_number)) = neon_data
        {
            let fired = self
                .emit_bankruptcy_neon(channel_id, user_id, guild_id, debt_cleared, filing_number)
                .await;
            if !fired && let (Some(channel_id), Some(guild_id)) = (channel_id, guild_id) {
                self.emit_gamba_milestone(channel_id, user_id, guild_id)
                    .await;
            }
        } else if response.is_ok() && bankruptcy_cooldown_hit {
            self.emit_cooldown_neon(channel_id, user_id, guild_id, "bankruptcy")
                .await;
        }
        response
    }

    async fn loan(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        options: &[InteractionOption],
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let amount = parse_nested_i64(options, "amount")
            .ok_or_else(|| "Amount must be positive.".to_owned())?;
        let registration_path = self.database_path.clone();
        let registered = sqlite("loan registration", move || {
            PlayerRepository::new(registration_path)
                .exists(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        if !registered {
            return respond_ephemeral(
                responder,
                "You need to /player register before taking loans.",
            )
            .await;
        }
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let effects = {
            let path = self.database_path.clone();
            sqlite("loan mana effects", move || {
                active_mana_effects(&path, user_id, guild_id, unix_seconds()?)
            })
            .await
            .unwrap_or_default()
        };
        let modifiers = cama_domain::mana::apply_loan_modifiers(
            &effects,
            self.config.loan_fee_rate,
            self.config.loan_max_amount,
        );
        let fee = (amount as f64 * modifiers.fee_rate) as i64;
        let cooldown = self.config.loan_cooldown_seconds;
        let max_amount = modifiers.limit;
        let path = self.database_path.clone();
        let result = sqlite("economy loan", move || {
            LoanRepository::new(path)
                .execute_loan_atomic(user_id, guild_id, amount, fee, cooldown, max_amount)
                .map_err(|error| error.to_string())
        })
        .await;
        match result {
            Ok(loan) => {
                let mut mana_notes = Vec::new();
                if effects.swamp_self_tax > 0 {
                    let tax = effects.swamp_self_tax;
                    let path = self.database_path.clone();
                    if sqlite("loan Swamp self tax", move || {
                        ShopRuntimeRepository::new(path)
                            .adjust_balance(
                                user_id,
                                guild_id.unwrap_or_default(),
                                -tax,
                                Some("mana"),
                                Some(user_id),
                                Some("swamp_self_tax"),
                                Some(&user_id.to_string()),
                                Some("Swamp mana loan tax"),
                                None,
                            )
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .is_ok()
                    {
                        mana_notes.push(format!("🌿 Swamp tax: -{tax}"));
                    }
                }
                if effects.swamp_siphon {
                    let path = self.database_path.clone();
                    let event_id = format!("loan:{user_id}:{}", unix_seconds()?);
                    let scale = self.config.minigame_jc_delta_scale;
                    if let Ok(Some(siphoned)) = sqlite("loan Swamp siphon", move || {
                        apply_swamp_siphon(
                            &path,
                            guild_id.unwrap_or_default(),
                            user_id,
                            scale,
                            &event_id,
                        )
                    })
                    .await
                    {
                        mana_notes.push(format!("🌿 Siphon: +{siphoned}"));
                    }
                }
                let suffix = if mana_notes.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", mana_notes.join(" | "))
                };
                let badge = format_mana_badge(Some(&effects));
                let title_prefix = if badge.is_empty() {
                    String::new()
                } else {
                    format!("{badge} ")
                };
                let (title, description, color) = if loan.was_negative_loan {
                    (
                        format!("{title_prefix}🎪 LEGENDARY DEGEN MOVE 🎪"),
                        "You... you took out a loan while already in debt. The money went straight to your creditors. You're now even MORE in debt. Congratulations, you absolute degenerate."
                            .to_owned(),
                        0x9B_59_B6,
                    )
                } else {
                    (
                        format!("{title_prefix}🏦 Loan Approved"),
                        format!(
                            "The bank approves your request. {} {} deposited. You now owe {}. Good luck.",
                            loan.amount, JOPACOIN_EMOTE, loan.total_owed
                        ),
                        0x2E_CC_71,
                    )
                };
                let fee_pct = self.config.loan_fee_rate * 100.0;
                let mut embed = InteractionEmbed::titled(title)
                    .description(description)
                    .color(color)
                    .field(
                        "Details",
                        format!(
                            "Borrowed: **{}** {JOPACOIN_EMOTE}\nFee ({fee_pct:.0}%): **{}** {JOPACOIN_EMOTE}\nTotal Owed: **{}** {JOPACOIN_EMOTE}\nNew Balance: **{}** {JOPACOIN_EMOTE}",
                            loan.amount, loan.fee, loan.total_owed, loan.new_balance
                        ),
                        false,
                    )
                    .field(
                        "📅 Repayment",
                        "You will repay the full amount **after your next match**.",
                        false,
                    )
                    .footer(format!(
                        "Loan #{} | {}",
                        loan.total_loans_taken,
                        if loan.was_negative_loan {
                            "Go bet it all, you beautiful degen"
                        } else {
                            "Fee deposited to Jopacoin Reserve"
                        }
                    ));
                if !suffix.is_empty() {
                    embed = embed.field("Mana Effects", suffix.trim(), false);
                }
                let response = responder
                    .followup(InteractionResponse::message("").embed(embed))
                    .await
                    .map_err(|error| error.to_string());
                if response.is_ok() {
                    self.emit_loan_neon(
                        channel_id,
                        user_id,
                        guild_id,
                        loan.amount,
                        loan.total_owed,
                        loan.was_negative_loan,
                    )
                    .await;
                }
                response
            }
            Err(error) => {
                let response = followup_ephemeral(responder, &error).await;
                if response.is_ok() && error.starts_with("Loan cooldown active") {
                    self.emit_cooldown_neon(channel_id, user_id, guild_id, "loan")
                        .await;
                }
                response
            }
        }
    }

    async fn reserve(
        &self,
        guild_id: Option<i64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let path = self.database_path.clone();
        let snapshot = sqlite("economy reserve", move || {
            let total = LoanRepository::new(&path)
                .get_nonprofit_fund(guild_id)
                .map_err(|error| error.to_string())?;
            let disbursement = DisbursementRepository::new(&path);
            let active = disbursement
                .get_active_proposal(guild_id)
                .map_err(|error| error.to_string())?;
            let last = disbursement
                .get_last_disbursement(guild_id)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((total, active, last))
        })
        .await?;
        let (total, active, last) = snapshot;
        let reserved = active.as_ref().map_or(0, |proposal| proposal.fund_amount);
        let effective_total = total.saturating_add(reserved);
        let status = if effective_total >= self.config.disburse_min_fund {
            if reserved > 0 {
                format!("Proposal active ({reserved} reserved)")
            } else {
                format!(
                    "Ready for allocation! (min: {})",
                    self.config.disburse_min_fund
                )
            }
        } else {
            format!(
                "Collecting... ({effective_total}/{}) needed",
                self.config.disburse_min_fund
            )
        };
        let mut embed = InteractionEmbed::titled("🏛️ Jopacoin Reserve")
            .description(
                "The reserve is the server operations budget. Fees, fines, and economy sinks collect here before Tax Men allocate the budget.\n\n*A public balance sheet for server business.*",
            )
            .color(0xE9_1E_63)
            .field("Available Budget", format!("**{total}** {JOPACOIN_EMOTE}"), reserved == 0)
            .field("Status", status, true);
        if reserved > 0 {
            embed = embed
                .field(
                    "Reserved Budget",
                    format!("**{reserved}** {JOPACOIN_EMOTE}"),
                    true,
                )
                .field(
                    "Total",
                    format!("**{effective_total}** {JOPACOIN_EMOTE}"),
                    true,
                );
        }
        if let Some(last) = last {
            let recipients = if last.recipients.is_empty() {
                "No recipients".to_owned()
            } else {
                let mut lines = last
                    .recipients
                    .iter()
                    .take(3)
                    .map(|(discord_id, amount)| format!("<@{discord_id}>: +{amount}"))
                    .collect::<Vec<_>>();
                if last.recipients.len() > 3 {
                    lines.push(format!("+{} more", last.recipients.len() - 3));
                }
                lines.join("\n")
            };
            embed = embed.field(
                "Last Disbursement",
                format!(
                    "**{}** {} via {}\n<t:{}:R>\n{recipients}",
                    last.total_amount,
                    JOPACOIN_EMOTE,
                    disburse_method_label(&last.method),
                    last.disbursed_at
                ),
                false,
            );
        }
        responder
            .respond(InteractionResponse::message("").embed(embed))
            .await
            .map_err(|error| error.to_string())
    }

    async fn invest(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        options: &[InteractionOption],
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        if !self.take_rate("invest", guild_id, user_id, 5, DEFAULT_RATE_WINDOW)? {
            let retry_after = self
                .rate_retry_after("invest", guild_id, user_id, DEFAULT_RATE_WINDOW)
                .unwrap_or(DEFAULT_RATE_WINDOW.as_secs());
            return respond_ephemeral(
                responder,
                &format!("Slow down! Try configuring investments again in {retry_after}s."),
            )
            .await;
        }
        let action = parse_nested_string(options, "action").unwrap_or_else(|| "list".to_owned());
        match action.as_str() {
            "list" => {
                let path = self.database_path.clone();
                let positions = sqlite("economy investment list", move || {
                    AutobetInvestmentRepository::new(path)
                        .list(Some(guild_id), user_id)
                        .map_err(|error| error.to_string())
                })
                .await?;
                if positions.is_empty() {
                    return respond_ephemeral(responder, "You have no configured investments.")
                        .await;
                }
                let total = positions
                    .iter()
                    .map(|position| position.percentage)
                    .sum::<i64>();
                let mut content = String::from("**Configured investments**\n");
                for (index, position) in positions.iter().enumerate() {
                    content.push_str(&format!(
                        "{}. {} <@{}> — {}%\n",
                        index + 1,
                        position.direction.to_ascii_uppercase(),
                        position.target_id,
                        position.percentage
                    ));
                }
                content.push_str(&format!("Total: **{total}% / 50%**"));
                respond_ephemeral(responder, &content).await
            }
            "remove" => {
                let target = parse_nested_user(options, "player");
                let position = parse_nested_i64(options, "position");
                if target.is_some() && position.is_some() {
                    return respond_ephemeral(
                        responder,
                        "Choose either a target player or a listed position number, not both.",
                    )
                    .await;
                }
                let target_id = if let Some(position) = position {
                    let path = self.database_path.clone();
                    let positions = sqlite("economy investment positions", move || {
                        AutobetInvestmentRepository::new(path)
                            .list(Some(guild_id), user_id)
                            .map_err(|error| error.to_string())
                    })
                    .await?;
                    let Some(index) = position
                        .checked_sub(1)
                        .and_then(|value| usize::try_from(value).ok())
                        .and_then(|value| positions.get(value).map(|_| value))
                    else {
                        return respond_ephemeral(
                            responder,
                            &format!(
                                "Investment position {position} does not exist. Run `/economy invest list` to see the current numbers."
                            ),
                        )
                        .await;
                    };
                    positions[index].target_id
                } else if let Some(target) = target {
                    target
                } else {
                    return respond_ephemeral(
                        responder,
                        "Choose a target player or a listed position number to remove.",
                    )
                    .await;
                };
                let path = self.database_path.clone();
                let removed = sqlite("economy investment removal", move || {
                    AutobetInvestmentRepository::new(path)
                        .remove(Some(guild_id), user_id, target_id)
                        .map_err(|error| error.to_string())
                })
                .await?;
                let content = if removed {
                    format!("Removed your investment in <@{target_id}>.")
                } else {
                    format!("You do not have an investment in <@{target_id}>.")
                };
                respond_ephemeral(responder, &content).await
            }
            "set" => {
                let Some(target_id) = parse_nested_user(options, "player") else {
                    return respond_ephemeral(
                        responder,
                        "Choose a target player when setting an investment.",
                    )
                    .await;
                };
                let Some(direction) = parse_nested_string(options, "direction") else {
                    return respond_ephemeral(
                        responder,
                        "Choose a direction and percentage when setting an investment.",
                    )
                    .await;
                };
                let Some(percentage) = parse_nested_i64(options, "percentage") else {
                    return respond_ephemeral(
                        responder,
                        "Choose a direction and percentage when setting an investment.",
                    )
                    .await;
                };
                let path = self.database_path.clone();
                let direction_for_validation = direction.clone();
                let direction_for_storage = direction.clone();
                let result = sqlite("economy investment set", move || {
                    let players = PlayerRepository::new(&path);
                    let investor_registered = players
                        .exists(user_id, Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    let target_registered = players
                        .exists(target_id, Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    validate_investment_target(
                        user_id,
                        target_id,
                        &direction_for_validation,
                        investor_registered,
                        target_registered,
                    )?;
                    AutobetInvestmentRepository::new(path)
                        .set(
                            Some(guild_id),
                            user_id,
                            target_id,
                            &direction_for_storage,
                            percentage,
                        )
                        .map_err(|error| error.to_string())
                })
                .await;
                match result {
                    Ok(total) => respond_ephemeral(
                        responder,
                        &format!(
                            "Set a **{direction}** investment in <@{target_id}> at **{percentage}%**. Configured total: **{total}% / 50%**."
                        ),
                    )
                    .await,
                    Err(error) => respond_ephemeral(responder, &format!("❌ {error}")).await,
                }
            }
            _ => respond_ephemeral(responder, "Invalid investment action.").await,
        }
    }

    async fn reserve_voting_restricted(&self, guild_id: i64) -> Result<bool, String> {
        let path = self.database_path.clone();
        let event_config = economy_event_config(&self.config);
        Ok(sqlite("reserve voting restriction", move || {
            SqliteEconomyEventService::new(path, event_config)
                .effects_at(guild_id, unix_seconds()?)
                .map(|effects| effects.reserve_voting_disabled)
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap_or(false))
    }

    async fn disburse(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        permissions: u64,
        options: &[InteractionOption],
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        let action = parse_nested_string(options, "action").unwrap_or_else(|| "status".to_owned());
        let is_tax_man = permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            || self.config.tax_man_user_ids.contains(&user_id);
        if matches!(action.as_str(), "reset" | "votes" | "execute") && !is_tax_man {
            return respond_ephemeral(responder, "Only Tax Men can manage disbursement proposals.")
                .await;
        }
        match action.as_str() {
            "propose" => {
                if self.reserve_voting_restricted(guild_id).await? {
                    return respond_ephemeral(
                        responder,
                        "Reserve voting is paused by the active economy event.",
                    )
                    .await;
                }
                let path = self.database_path.clone();
                let quorum_percentage = self.config.disburse_quorum_percentage;
                let minimum = self.config.disburse_min_fund;
                let result = sqlite("disbursement proposal", move || {
                    let players = PlayerRepository::new(&path);
                    let registered = players
                        .get_all(Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    if !registered.iter().any(|player| player.jopacoin_balance < 0) {
                        return Err("No players with negative balance to receive funds.".to_owned());
                    }
                    let player_count = i64::try_from(registered.len())
                        .map_err(|_| "registered player count exceeds SQLite range".to_owned())?;
                    let quorum = ((player_count as f64 * quorum_percentage).ceil() as i64).max(1);
                    DisbursementRepository::new(&path)
                        .create_proposal_atomic(
                            Some(guild_id),
                            unix_seconds().map_err(|error| error.to_string())?,
                            minimum,
                            quorum,
                        )
                        .map_err(|error| error.to_string())
                })
                .await;
                match result {
                    Ok(proposal) => {
                        self.send_persistent_disbursement_message(
                            guild_id,
                            channel_id,
                            disbursement_proposal_response(&proposal),
                            responder,
                        )
                        .await
                    }
                    Err(error) => respond_ephemeral(responder, &error).await,
                }
            }
            "status" => {
                let path = self.database_path.clone();
                let proposal = sqlite("disbursement status", move || {
                    DisbursementRepository::new(path)
                        .get_active_proposal(Some(guild_id))
                        .map_err(|error| error.to_string())
                })
                .await?;
                let Some(proposal) = proposal else {
                    let path = self.database_path.clone();
                    let reserve = sqlite("disbursement reserve", move || {
                        LoanRepository::new(path)
                            .get_nonprofit_fund(Some(guild_id))
                            .map_err(|error| error.to_string())
                    })
                    .await?;
                    return respond_ephemeral(
                        responder,
                        &format!(
                            "No active disbursement proposal. Reserve currently holds **{reserve}** {}.",
                            JOPACOIN_EMOTE
                        ),
                    )
                    .await;
                };
                if let (Some(message_id), Some(previous_channel)) =
                    (proposal.message_id, proposal.channel_id)
                    && let (Ok(previous_channel), Ok(message_id)) =
                        (u64::try_from(previous_channel), u64::try_from(message_id))
                {
                    let _ = self
                        .discord
                        .delete_message(previous_channel, message_id)
                        .await;
                }
                self.send_persistent_disbursement_message(
                    guild_id,
                    channel_id,
                    disbursement_proposal_response(&proposal),
                    responder,
                )
                .await
            }
            "reset" => {
                let path = self.database_path.clone();
                let reset = sqlite("disbursement reset", move || {
                    DisbursementRepository::new(path)
                        .reset_and_return_fund_atomic(Some(guild_id), "reset")
                        .map_err(|error| error.to_string())
                })
                .await?;
                if reset {
                    responder
                        .respond(InteractionResponse::message(
                            "Disbursement proposal has been reset.",
                        ))
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    respond_ephemeral(responder, "No active proposal to reset.").await
                }
            }
            "votes" => {
                let path = self.database_path.clone();
                let proposal = sqlite("disbursement votes proposal", move || {
                    DisbursementRepository::new(path)
                        .get_active_proposal(Some(guild_id))
                        .map_err(|error| error.to_string())
                })
                .await?;
                let Some(proposal) = proposal else {
                    return respond_ephemeral(
                        responder,
                        "No active disbursement proposal. Use `/economy disburse` and choose status to check.",
                    )
                    .await;
                };
                let path = self.database_path.clone();
                let votes = sqlite("disbursement votes", move || {
                    DisbursementRepository::new(path)
                        .individual_votes(Some(guild_id))
                        .map_err(|error| error.to_string())
                })
                .await?;
                if votes.is_empty() {
                    return respond_ephemeral(
                        responder,
                        "No active disbursement proposal. Use `/economy disburse` and choose status to check.",
                    )
                    .await;
                }
                let key = format!("{guild_id}:{user_id}:{}", proposal.proposal_id);
                let state = PendingDisburseVotes {
                    guild_id,
                    requester_id: user_id,
                    proposal,
                    votes,
                    page: 0,
                    created_at: Instant::now(),
                };
                let response = disbursement_votes_response(&state).ephemeral();
                self.disburse_vote_pages
                    .lock()
                    .map_err(|_| "disbursement vote page state is poisoned".to_owned())?
                    .insert(key, state);
                responder
                    .respond(response)
                    .await
                    .map_err(|error| error.to_string())
            }
            "execute" => {
                if self.reserve_voting_restricted(guild_id).await? {
                    return respond_ephemeral(
                        responder,
                        "Reserve voting is paused by the active economy event.",
                    )
                    .await;
                }
                let path = self.database_path.clone();
                let proposal = sqlite("disbursement execution lookup", move || {
                    DisbursementRepository::new(path)
                        .get_active_proposal(Some(guild_id))
                        .map_err(|error| error.to_string())
                })
                .await?;
                let Some(proposal) = proposal else {
                    return respond_ephemeral(responder, "No active disbursement proposal.").await;
                };
                if proposal.total_votes() == 0 {
                    return respond_ephemeral(
                        responder,
                        "Cannot execute: No votes have been cast yet.",
                    )
                    .await;
                }
                let result = self.execute_disbursement(guild_id, &proposal).await;
                match result {
                    Ok(result) => {
                        self.update_persistent_disbursement_message(
                            &proposal,
                            disbursement_proposal_response_with_disabled_buttons(&proposal),
                        )
                        .await;
                        responder
                            .respond(disbursement_response(&result, true))
                            .await
                            .map_err(|error| error.to_string())
                    }
                    Err(error) => {
                        respond_ephemeral(responder, &format!("Cannot execute: {error}")).await
                    }
                }
            }
            _ => respond_ephemeral(responder, "Invalid disbursement action.").await,
        }
    }

    async fn send_persistent_disbursement_message(
        &self,
        guild_id: i64,
        _channel_id: Option<u64>,
        response: InteractionResponse,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| error.to_string())?;
        let Some(receipt) = receipt else {
            return Ok(());
        };
        let message_id = signed_id(receipt.message_id, "disbursement message")?;
        let channel_id = signed_id(receipt.channel_id, "disbursement channel")?;
        let path = self.database_path.clone();
        sqlite("disbursement message persistence", move || {
            DisbursementRepository::new(path)
                .set_proposal_message(Some(guild_id), message_id, channel_id)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn update_persistent_disbursement_message(
        &self,
        proposal: &DisbursementProposal,
        response: InteractionResponse,
    ) {
        let (Some(channel_id), Some(message_id)) = (proposal.channel_id, proposal.message_id)
        else {
            return;
        };
        let (Ok(channel_id), Ok(message_id)) =
            (u64::try_from(channel_id), u64::try_from(message_id))
        else {
            return;
        };
        let _ = self
            .discord
            .edit_message(
                channel_id,
                message_id,
                DiscordMessage::default_mentions(response).preserving_content(),
            )
            .await;
    }

    async fn execute_disbursement(
        &self,
        guild_id: i64,
        proposal: &DisbursementProposal,
    ) -> Result<DisbursementExecution, String> {
        let method = winning_disbursement_method(&proposal.votes)
            .ok_or_else(|| "No votes have been cast yet".to_owned())?
            .to_owned();
        let path = self.database_path.clone();
        let fund = proposal.fund_amount;
        sqlite("disbursement execution", move || {
            let repository = DisbursementRepository::new(&path);
            match method.as_str() {
                "cancel" => {
                    if !repository
                        .reset_and_return_fund_atomic(Some(guild_id), "cancelled")
                        .map_err(|error| error.to_string())?
                    {
                        return Err("No active proposal".to_owned());
                    }
                    Ok(DisbursementExecution {
                        method: method.to_owned(),
                        total_disbursed: 0,
                        distributions: Vec::new(),
                        cancelled: true,
                        message: Some(
                            "Proposal cancelled by vote. Funds returned to nonprofit.".to_owned(),
                        ),
                    })
                }
                "burn" | "next_match_pot" => {
                    let amount = repository
                        .complete_reserve_allocation_atomic(Some(guild_id), &method)
                        .map_err(|error| error.to_string())?;
                    let message = if method == "burn" {
                        format!("{amount} Jopacoin removed from circulation.")
                    } else {
                        format!("{amount} Jopacoin queued for the next match betting pot.")
                    };
                    Ok(DisbursementExecution {
                        method: method.to_owned(),
                        total_disbursed: amount,
                        distributions: Vec::new(),
                        cancelled: false,
                        message: Some(message),
                    })
                }
                _ => {
                    let players = PlayerRepository::new(&path)
                        .get_all(Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    let distributions = calculate_distributions(&method, fund, &players);
                    let total = repository
                        .complete_and_disburse_atomic(Some(guild_id), &method, &distributions)
                        .map_err(|error| error.to_string())?;
                    let message = if distributions.is_empty() {
                        Some("No eligible players for this distribution method.".to_owned())
                    } else {
                        None
                    };
                    Ok(DisbursementExecution {
                        method: method.to_owned(),
                        total_disbursed: total,
                        distributions,
                        cancelled: false,
                        message,
                    })
                }
            }
        })
        .await
    }

    async fn disburse_votes_page(
        &self,
        page_key: &str,
        action: DisburseVotesPageAction,
        guild_id: i64,
        user_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let response = {
            let mut pages = self
                .disburse_vote_pages
                .lock()
                .map_err(|_| "disbursement vote page state is poisoned".to_owned())?;
            pages.retain(|_, page| page.created_at.elapsed() <= DISBURSE_VOTES_TTL);
            match pages.get_mut(page_key) {
                None => None,
                Some(page) => {
                    if page.guild_id != guild_id || page.requester_id != user_id {
                        Some(Err("This vote audit view belongs to another Tax Man."))
                    } else {
                        match action {
                            DisburseVotesPageAction::Previous => {
                                page.page = page.page.saturating_sub(1);
                            }
                            DisburseVotesPageAction::Next => {
                                let total_pages =
                                    page_count(page.votes.len(), DISBURSE_VOTES_PAGE_SIZE);
                                page.page = page
                                    .page
                                    .saturating_add(1)
                                    .min(total_pages.saturating_sub(1));
                            }
                        }
                        Some(Ok(disbursement_votes_response(page)))
                    }
                }
            }
        };
        match response {
            Some(Ok(response)) => responder
                .update(response)
                .await
                .map_err(|error| error.to_string()),
            Some(Err(message)) => respond_ephemeral(responder, message).await,
            None => {
                respond_ephemeral(
                    responder,
                    "This vote audit view expired. Run `/economy disburse` and choose votes again.",
                )
                .await
            }
        }
    }

    fn take_rate(
        &self,
        scope: &str,
        guild_id: i64,
        user_id: i64,
        limit: usize,
        window: Duration,
    ) -> Result<bool, String> {
        let now = Instant::now();
        let mut rates = self
            .rates
            .lock()
            .map_err(|_| "betting rate limiter lock was poisoned".to_owned())?;
        let claims = rates
            .entry((scope.to_owned(), guild_id, user_id))
            .or_default();
        claims.retain(|claimed| now.saturating_duration_since(*claimed) < window);
        if claims.len() >= limit {
            return Ok(false);
        }
        claims.push(now);
        Ok(true)
    }

    fn rate_retry_after(
        &self,
        scope: &str,
        guild_id: i64,
        user_id: i64,
        window: Duration,
    ) -> Option<u64> {
        let now = Instant::now();
        let rates = self.rates.lock().ok()?;
        let claims = rates.get(&(scope.to_owned(), guild_id, user_id))?;
        let oldest = claims.first()?;
        Some(retry_after_seconds(
            window,
            now.saturating_duration_since(*oldest),
        ))
    }
}

/// Match Python's `int(retry_after)` response formatting: durations are
/// truncated toward zero rather than rounded up to the next second.
fn retry_after_seconds(window: Duration, elapsed: Duration) -> u64 {
    window.saturating_sub(elapsed).as_secs()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisburseVotesPageAction {
    Previous,
    Next,
}

fn parse_disbursement_page_component(custom_id: &str) -> Option<(String, DisburseVotesPageAction)> {
    let rest = custom_id.strip_prefix("disburse:votes:")?;
    let (key, action) = rest.rsplit_once(':')?;
    let action = match action {
        "prev" => DisburseVotesPageAction::Previous,
        "next" => DisburseVotesPageAction::Next,
        _ => return None,
    };
    (!key.is_empty()).then(|| (key.to_owned(), action))
}

fn page_count(item_count: usize, page_size: usize) -> usize {
    item_count
        .saturating_add(page_size.saturating_sub(1))
        .checked_div(page_size.max(1))
        .unwrap_or(1)
        .max(1)
}

fn disbursement_votes_response(state: &PendingDisburseVotes) -> InteractionResponse {
    let total_votes = state.proposal.total_votes();
    let quorum = state.proposal.quorum_required;
    let total_pages = page_count(state.votes.len(), DISBURSE_VOTES_PAGE_SIZE);
    let page = state.page.min(total_pages.saturating_sub(1));
    let start = page.saturating_mul(DISBURSE_VOTES_PAGE_SIZE);
    let end = (start + DISBURSE_VOTES_PAGE_SIZE).min(state.votes.len());
    let progress = if quorum > 0 {
        (total_votes as f64 / quorum as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let breakdown = DISBURSE_METHODS
        .iter()
        .map(|method| {
            let count = state.proposal.votes.get(*method).copied().unwrap_or(0);
            let percentage = if total_votes > 0 {
                count as f64 / total_votes as f64 * 100.0
            } else {
                0.0
            };
            format!(
                "**{}:** {count} ({percentage:.0}%)",
                disburse_method_label(method)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let voter_lines = if start < end {
        state.votes[start..end]
            .iter()
            .map(|vote| {
                format!(
                    "• <@{}> → {}",
                    vote.discord_id,
                    disburse_method_label(&vote.vote_method)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        "*No votes yet*".to_owned()
    };
    let voter_range = if state.votes.is_empty() {
        "Individual Votes".to_owned()
    } else {
        format!(
            "Individual Votes ({}-{} of {})",
            start + 1,
            end,
            state.votes.len()
        )
    };
    let rows = if total_pages > 1 {
        vec![InteractionActionRow::buttons(vec![
            InteractionButton::new(
                format!("disburse:votes:{}:prev", state_key(state)),
                "Previous",
            )
            .style(InteractionButtonStyle::Secondary)
            .disabled(page == 0),
            InteractionButton::new(format!("disburse:votes:{}:next", state_key(state)), "Next")
                .style(InteractionButtonStyle::Secondary)
                .disabled(page + 1 >= total_pages),
        ])]
    } else {
        Vec::new()
    };
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled("🔍 Disbursement Vote Details (Tax Man)")
                .description(format!(
                    "Reserve Budget: **{}** {JOPACOIN_EMOTE}",
                    state.proposal.fund_amount
                ))
                .color(0x9C_27_B0)
                .field(
                    "📋 Proposal Status",
                    format!(
                        "**Quorum:** {total_votes}/{quorum} ({progress:.0}%)\n**Status:** {}",
                        if state.proposal.quorum_reached() {
                            "✅ Ready"
                        } else {
                            "⏳ Voting"
                        }
                    ),
                    false,
                )
                .field("📊 Vote Breakdown", breakdown, false)
                .field(format!("👥 {voter_range}"), voter_lines, false)
                .footer(format!(
                    "Tax Man audit only | Page {}/{}",
                    page + 1,
                    total_pages
                )),
        )
        .action_rows(rows)
}

/// The key is stable for the process-local view and is independently encoded
/// in each button custom ID.  It deliberately mirrors the state map key used
/// by the command branch (`guild:user:proposal`), without inventing durable
/// schema for an ephemeral audit page.
fn state_key(state: &PendingDisburseVotes) -> String {
    format!(
        "{}:{}:{}",
        state.guild_id, state.requester_id, state.proposal.proposal_id
    )
}

fn disburse_method_label(method: &str) -> &str {
    match method {
        "even" => "Even Split",
        "proportional" => "Proportional",
        "neediest" => "Neediest First",
        "stimulus" => "Stimulus",
        "lottery" => "Lottery",
        "social_security" => "Social Security",
        "richest" => "Richest",
        "burn" => "Burn",
        "next_match_pot" => "Next Match Pot",
        "cancel" => "Cancel",
        _ => method,
    }
}

fn winning_disbursement_method(votes: &BTreeMap<String, i64>) -> Option<&str> {
    let max = votes.values().copied().max().unwrap_or(0);
    (max > 0).then(|| {
        DISBURSE_METHODS
            .into_iter()
            .find(|method| votes.get(*method).copied().unwrap_or(0) == max)
            .unwrap_or("even")
    })
}

fn disbursement_proposal_response(proposal: &DisbursementProposal) -> InteractionResponse {
    disbursement_proposal_response_with_disabled_buttons_impl(proposal, false)
}

fn disbursement_proposal_response_with_disabled_buttons(
    proposal: &DisbursementProposal,
) -> InteractionResponse {
    disbursement_proposal_response_with_disabled_buttons_impl(proposal, true)
}

fn disbursement_proposal_response_with_disabled_buttons_impl(
    proposal: &DisbursementProposal,
    disabled: bool,
) -> InteractionResponse {
    let vote_lines = DISBURSE_METHODS
        .iter()
        .map(|method| {
            format!(
                "{}: {}",
                disburse_method_label(method),
                proposal.votes.get(*method).copied().unwrap_or(0)
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    let rows = DISBURSE_METHODS
        .chunks(5)
        .map(|methods| {
            InteractionActionRow::buttons(
                methods
                    .iter()
                    .map(|method| {
                        let style = if *method == "cancel" || *method == "burn" {
                            InteractionButtonStyle::Danger
                        } else if *method == "next_match_pot" {
                            InteractionButtonStyle::Success
                        } else {
                            InteractionButtonStyle::Primary
                        };
                        InteractionButton::new(
                            format!("disburse:{method}"),
                            disburse_method_label(method),
                        )
                        .style(style)
                        .disabled(disabled)
                    })
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled("💝 Jopacoin Reserve Allocation")
                .description(format!(
                    "**{}** {} is locked for this allocation vote.\n\nVotes: **{}/{}**\n{}",
                    proposal.fund_amount,
                    JOPACOIN_EMOTE,
                    proposal.total_votes(),
                    proposal.quorum_required,
                    vote_lines
                ))
                .color(0xE9_1E_63),
        )
        .action_rows(rows)
}

fn disbursement_response(execution: &DisbursementExecution, forced: bool) -> InteractionResponse {
    let title = if execution.cancelled {
        "❌ Proposal Cancelled"
    } else if forced {
        "💝 Disbursement Complete (Tax Man)"
    } else {
        "💝 Disbursement Complete!"
    };
    let description = if let Some(message) = &execution.message {
        message.clone()
    } else {
        let mut lines = execution
            .distributions
            .iter()
            .take(10)
            .map(|(discord_id, amount)| format!("<@{discord_id}>: +{amount}"))
            .collect::<Vec<_>>();
        if execution.distributions.len() > 10 {
            lines.push(format!(
                "...and {} more",
                execution.distributions.len() - 10
            ));
        }
        format!(
            "**{}** {} distributed via **{}** to **{}** player(s):\n{}",
            execution.total_disbursed,
            JOPACOIN_EMOTE,
            disburse_method_label(&execution.method),
            execution.distributions.len(),
            lines.join("\n")
        )
    };
    InteractionResponse::message("").embed(
        InteractionEmbed::titled(title)
            .description(description)
            .color(if execution.cancelled {
                0xFF_6B_6B
            } else {
                0x00_FF_00
            }),
    )
}

fn calculate_distributions(method: &str, fund: i64, players: &[Player]) -> Vec<(i64, i64)> {
    if fund <= 0 {
        return Vec::new();
    }
    let mut debtors = players
        .iter()
        .filter_map(|player| {
            player
                .discord_id
                .filter(|_| player.jopacoin_balance < 0)
                .map(|discord_id| (discord_id, player.jopacoin_balance))
        })
        .collect::<Vec<_>>();
    match method {
        "even" => even_distributions(fund, &debtors),
        "proportional" => proportional_distributions(fund, &mut debtors),
        "neediest" => debtors
            .iter()
            .min_by_key(|(_, balance)| *balance)
            .map(|(discord_id, balance)| (*discord_id, fund.min(balance.saturating_abs())))
            .filter(|(_, amount)| *amount > 0)
            .into_iter()
            .collect(),
        "stimulus" => {
            let mut eligible = players
                .iter()
                .filter(|player| player.jopacoin_balance >= 0)
                .filter_map(|player| player.discord_id.map(|id| (id, player.jopacoin_balance)))
                .collect::<Vec<_>>();
            if eligible.len() < 4 {
                return Vec::new();
            }
            eligible.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
            let eligible = &eligible[3..];
            even_unlimited_distributions(fund, eligible)
        }
        "lottery" => players
            .iter()
            .filter_map(|player| player.discord_id)
            .collect::<Vec<_>>()
            .get(
                usize::try_from(unix_seconds().unwrap_or_default().unsigned_abs())
                    .unwrap_or_default()
                    % players
                        .iter()
                        .filter(|player| player.discord_id.is_some())
                        .count()
                        .max(1),
            )
            .copied()
            .map(|discord_id| vec![(discord_id, fund)])
            .unwrap_or_default(),
        "social_security" => {
            let mut eligible = players
                .iter()
                .filter_map(|player| {
                    player
                        .discord_id
                        .map(|discord_id| (discord_id, player.wins.saturating_add(player.losses)))
                })
                .filter(|(_, games)| *games > 0)
                .collect::<Vec<_>>();
            if eligible.is_empty() {
                return Vec::new();
            }
            eligible.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
            let total_games = eligible.iter().map(|(_, games)| *games).sum::<i64>();
            let mut remaining = fund;
            eligible
                .iter()
                .enumerate()
                .filter_map(|(index, (discord_id, games))| {
                    let amount = if index + 1 == eligible.len() {
                        remaining
                    } else {
                        fund.saturating_mul(*games) / total_games
                    };
                    remaining = remaining.saturating_sub(amount);
                    (amount > 0).then_some((*discord_id, amount))
                })
                .collect()
        }
        "richest" => players
            .iter()
            .filter_map(|player| player.discord_id.map(|id| (id, player.jopacoin_balance)))
            .max_by(|left, right| right.1.cmp(&left.1).then(right.0.cmp(&left.0)))
            .map(|(discord_id, _)| vec![(discord_id, fund)])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn even_distributions(fund: i64, debtors: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut remaining = fund;
    let mut received = vec![0_i64; debtors.len()];
    let mut open = (0..debtors.len()).collect::<Vec<_>>();
    while remaining > 0 && !open.is_empty() {
        let share = (remaining / i64::try_from(open.len()).unwrap_or(1)).max(1);
        let mut distributed = 0;
        let mut next = Vec::new();
        for index in open {
            let need = debtors[index]
                .1
                .saturating_abs()
                .saturating_sub(received[index]);
            let amount = share.min(need).min(remaining.saturating_sub(distributed));
            if amount > 0 {
                received[index] += amount;
                distributed += amount;
            }
            if received[index] < debtors[index].1.saturating_abs() {
                next.push(index);
            }
        }
        if distributed == 0 {
            break;
        }
        remaining -= distributed;
        open = next;
    }
    debtors
        .iter()
        .enumerate()
        .filter_map(|(index, (discord_id, _))| {
            (received[index] > 0).then_some((*discord_id, received[index]))
        })
        .collect()
}

fn proportional_distributions(fund: i64, debtors: &mut [(i64, i64)]) -> Vec<(i64, i64)> {
    debtors.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    let total_debt = debtors
        .iter()
        .map(|(_, balance)| balance.saturating_abs())
        .sum::<i64>();
    if total_debt <= 0 {
        return even_distributions(fund, debtors);
    }
    let mut remaining = fund;
    debtors
        .iter()
        .enumerate()
        .filter_map(|(index, (discord_id, balance))| {
            let debt = balance.saturating_abs();
            let amount = if index + 1 == debtors.len() {
                remaining.min(debt)
            } else {
                ((fund as f64 * debt as f64 / total_debt as f64) as i64)
                    .min(debt)
                    .min(remaining)
            };
            remaining = remaining.saturating_sub(amount);
            (amount > 0).then_some((*discord_id, amount))
        })
        .collect()
}

fn even_unlimited_distributions(fund: i64, players: &[(i64, i64)]) -> Vec<(i64, i64)> {
    if players.is_empty() {
        return Vec::new();
    }
    let count = i64::try_from(players.len()).unwrap_or(1);
    let base = fund / count;
    let remainder = fund % count;
    players
        .iter()
        .enumerate()
        .map(|(index, (discord_id, _))| {
            (
                *discord_id,
                base + i64::from(i64::try_from(index).unwrap_or(i64::MAX) < remainder),
            )
        })
        .filter(|(_, amount)| *amount > 0)
        .collect()
}

fn parse_team(value: &str) -> Result<BettingTeam, String> {
    match value.to_ascii_lowercase().as_str() {
        "radiant" => Ok(BettingTeam::Radiant),
        "dire" => Ok(BettingTeam::Dire),
        _ => Err("Choose Radiant or Dire.".to_owned()),
    }
}

fn team_name(team: BettingTeam) -> &'static str {
    match team {
        BettingTeam::Radiant => "Radiant",
        BettingTeam::Dire => "Dire",
    }
}

fn parse_user(options: &[InteractionOption], name: &str) -> Option<i64> {
    options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match option.value {
            InteractionValue::User { id, .. } => i64::try_from(id).ok(),
            _ => None,
        })
}

fn parse_string(options: &[InteractionOption], name: &str) -> Option<String> {
    options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match &option.value {
            InteractionValue::String(value) => Some(value.clone()),
            InteractionValue::Integer(value) => Some(value.to_string()),
            _ => None,
        })
}

fn parse_i64(options: &[InteractionOption], name: &str) -> Option<i64> {
    options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| match &option.value {
            InteractionValue::Integer(value) => Some(*value),
            InteractionValue::String(value) => value.parse().ok(),
            _ => None,
        })
}

fn parse_nested_options(options: &[InteractionOption]) -> &[InteractionOption] {
    options
        .iter()
        .find_map(|option| match &option.value {
            InteractionValue::Subcommand(children)
            | InteractionValue::SubcommandGroup(children) => Some(children.as_slice()),
            _ => None,
        })
        .unwrap_or(options)
}

fn parse_nested_string(options: &[InteractionOption], name: &str) -> Option<String> {
    parse_string(parse_nested_options(options), name).or_else(|| parse_string(options, name))
}

fn parse_nested_i64(options: &[InteractionOption], name: &str) -> Option<i64> {
    parse_i64(parse_nested_options(options), name).or_else(|| parse_i64(options, name))
}

fn parse_nested_user(options: &[InteractionOption], name: &str) -> Option<i64> {
    parse_user(parse_nested_options(options), name).or_else(|| parse_user(options, name))
}

fn qualified_name(name: &str, options: &[InteractionOption]) -> String {
    let mut parts = vec![name.to_owned()];
    let mut current = options;
    loop {
        let Some(option) = current.iter().find(|option| {
            matches!(
                option.value,
                InteractionValue::Subcommand(_) | InteractionValue::SubcommandGroup(_)
            )
        }) else {
            break;
        };
        parts.push(option.name.clone());
        current = match &option.value {
            InteractionValue::Subcommand(children)
            | InteractionValue::SubcommandGroup(children) => children,
            _ => unreachable!(),
        };
    }
    parts.join(" ")
}

fn pct(value: i64, total: i64) -> f64 {
    if total > 0 {
        value as f64 / total as f64 * 100.0
    } else {
        0.0
    }
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), String> {
    responder
        .respond(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string())
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), String> {
    responder
        .followup(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string())
}

async fn sqlite<T, F>(label: &'static str, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("{label} task failed: {error}"))?
}

/// Apply Python's wrong-channel penance through the same immediate transaction
/// as every other reserve transfer.  A missing player is deliberately ignored
/// by the caller: the Discord-facing rejection is still the canonical copy,
/// while registered players get the exact one-JC debit and reserve credit.
async fn charge_wrong_channel(path: &Path, user_id: i64, guild_id: i64) -> Result<(), String> {
    let path = path.to_path_buf();
    sqlite("gamba wrong-channel penance", move || {
        LoanRepository::new(path)
            .transfer_balance_to_nonprofit_atomic(
                user_id,
                Some(guild_id),
                1,
                &LedgerContext {
                    source: Some("gamba".to_owned()),
                    actor_id: Some(user_id),
                    related_type: Some("wrong_channel_penalty".to_owned()),
                    related_id: Some(user_id.to_string()),
                    reason: Some("gamba wrong-channel penance".to_owned()),
                    metadata: Some(r#"{"amount":1}"#.to_owned()),
                },
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
}

fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} id exceeds SQLite signed range"))
}

/// Stable positive relation key for the ledger's integer exact-once marker.
/// Wheel event IDs intentionally retain human-readable components (`time`,
/// actor, random nonce), while the existing schema stores only an integer
/// relation. FNV-1a keeps retries and restart recovery deterministic.
fn event_related_id(event_id: &str) -> i64 {
    let hash = event_id
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
        });
    i64::try_from(hash & 0x7fff_ffff_ffff_ffff).unwrap_or(i64::MAX)
}

fn unix_seconds() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before Unix epoch".to_owned())
        .and_then(|duration| {
            i64::try_from(duration.as_secs())
                .map_err(|_| "system timestamp exceeds SQLite range".to_owned())
        })
}

/// Mirror Python's best-effort pet activity side effect without coupling
/// betting settlement to pet evolution.  The activity repository owns the
/// existing-schema eligibility, daily cap, and exact-once source-key rules;
/// a missing/disabled pet therefore remains a quiet no-op.
fn record_betting_pet_activity(
    path: &Path,
    user_id: i64,
    guild_id: i64,
    activity: PetActivity,
    source_key: &str,
    occurred_at: i64,
) {
    let _ = PetEvolutionRepository::new(path).record_activity(
        user_id,
        Some(guild_id),
        activity,
        source_key,
        occurred_at,
    );
}

fn active_mana_effects(
    path: &Path,
    user_id: i64,
    guild_id: Option<i64>,
    now: i64,
) -> Result<ManaEffects, String> {
    let Some(row) = ManaRepository::new(path)
        .get_mana(user_id, guild_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(ManaEffects::default());
    };
    let offset = pacific_offset_seconds(now)?;
    let local = DateTime::<Utc>::from_timestamp(now, 0)
        .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?
        .with_timezone(
            &FixedOffset::east_opt(offset)
                .ok_or_else(|| "invalid Pacific time-zone offset".to_owned())?,
        );
    if row.assigned_date != mana_day_label(local) || row.consumed_today {
        return Ok(ManaEffects::default());
    }
    let color = match row.current_land.as_str() {
        "Island" => "Blue",
        "Mountain" => "Red",
        "Forest" => "Green",
        "Plains" => "White",
        "Swamp" => "Black",
        _ => return Ok(ManaEffects::default()),
    };
    Ok(ManaEffects::for_color(Some(color), Some(&row.current_land)))
}

/// Resolve the same Pacific 4AM day boundary used by the mana provider.
fn pacific_offset_seconds(timestamp: i64) -> Result<i32, String> {
    use chrono::{Datelike, NaiveDate, TimeZone, Weekday};

    let utc = DateTime::<Utc>::from_timestamp(timestamp, 0)
        .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?;
    let year = utc.year();
    let march =
        NaiveDate::from_ymd_opt(year, 3, 1).ok_or_else(|| "invalid March date".to_owned())?;
    let november =
        NaiveDate::from_ymd_opt(year, 11, 1).ok_or_else(|| "invalid November date".to_owned())?;
    let to_sunday = |date: NaiveDate| -> i64 {
        match date.weekday() {
            Weekday::Sun => 0,
            Weekday::Mon => 6,
            Weekday::Tue => 5,
            Weekday::Wed => 4,
            Weekday::Thu => 3,
            Weekday::Fri => 2,
            Weekday::Sat => 1,
        }
    };
    let second_sunday_march = march + chrono::Duration::days(to_sunday(march) + 7);
    let first_sunday_november = november + chrono::Duration::days(to_sunday(november));
    let standard = FixedOffset::west_opt(8 * 3_600)
        .ok_or_else(|| "invalid Pacific standard offset".to_owned())?;
    let daylight = FixedOffset::west_opt(7 * 3_600)
        .ok_or_else(|| "invalid Pacific daylight offset".to_owned())?;
    let starts = standard
        .from_local_datetime(
            &second_sunday_march
                .and_hms_opt(2, 0, 0)
                .ok_or_else(|| "invalid DST start time".to_owned())?,
        )
        .single()
        .ok_or_else(|| "ambiguous DST start".to_owned())?
        .timestamp();
    let ends = daylight
        .from_local_datetime(
            &first_sunday_november
                .and_hms_opt(2, 0, 0)
                .ok_or_else(|| "invalid DST end time".to_owned())?,
        )
        .single()
        .ok_or_else(|| "ambiguous DST end".to_owned())?
        .timestamp();
    Ok(if (starts..ends).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    })
}

fn select_pending(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    requested_match: Option<i64>,
) -> Result<Option<PendingMatchRecord>, String> {
    let repository = PendingMatchRepository::new(path);
    if let Some(match_id) = requested_match {
        return repository
            .pending_match(guild_id, match_id)
            .map_err(|error| error.to_string());
    }
    let pending = repository
        .pending_matches(guild_id)
        .map_err(|error| error.to_string())?;
    if pending.len() == 1 {
        return Ok(pending.into_iter().next());
    }
    if pending.len() > 1 {
        if let Some(match_for_player) = pending
            .iter()
            .find(|pending| pending.state.contains_participant(user_id))
        {
            return Ok(Some(match_for_player.clone()));
        }
        let match_list = pending
            .iter()
            .map(|pending| format!("Match #{}", pending.pending_match_id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "❌ Multiple matches in progress ({match_list}). Please specify which match to bet on using the `match` parameter."
        ));
    }
    Ok(None)
}

fn apply_mana_wheel_modifiers(
    wedges: &mut [cama_app::wheel::WheelWedge],
    effects: &ManaEffects,
    kind: WheelKind,
) {
    // Python applies wheel mana variance/caps only to the ordinary wheel.
    // Bankrupt and Golden catalogs retain their authored negative/hostile
    // mechanics; their live calibration is handled by the shared policy.
    if kind != WheelKind::Regular {
        return;
    }
    if effects.color.as_deref() == Some("Green") {
        for wedge in wedges.iter_mut() {
            if let WheelValue::Numeric(value) = wedge.value {
                let adjusted = value
                    .max(effects.green_bankrupt_penalty)
                    .min(effects.green_max_wheel_win);
                wedge.value = WheelValue::Numeric(adjusted);
                wedge.label = adjusted.to_string();
            }
        }
    }
    if effects.color.as_deref() == Some("White")
        && let Some(cap) = effects.plains_max_wheel_win
    {
        for wedge in wedges.iter_mut() {
            if let WheelValue::Numeric(value) = wedge.value
                && value > cap
            {
                wedge.value = WheelValue::Numeric(cap);
                wedge.label = cap.to_string();
            }
        }
    }
    {
        let replacement = match effects.color.as_deref() {
            Some("Red") => Some(("ERUPTION", WheelMechanic::Eruption, "#ff4500")),
            Some("Green") => Some(("GROWTH", WheelMechanic::Overgrowth, "#228b22")),
            Some("Black") => Some(("DECAY", WheelMechanic::Decay, "#4b0082")),
            _ => None,
        };
        if let Some((label, mechanic, color)) = replacement
            && let Some(wedge) = wedges.iter_mut().find(|wedge| {
                matches!(wedge.value, WheelValue::Numeric(value) if (15..=25).contains(&value))
            })
        {
            wedge.label = label.to_owned();
            wedge.value = WheelValue::Mechanic(mechanic);
            wedge.color = color;
        }
    }
}

fn wheel_value_json(value: WheelValue) -> serde_json::Value {
    match value {
        WheelValue::Numeric(value) => json!(value),
        WheelValue::Mechanic(mechanic) => json!(mechanic.code()),
    }
}

fn wheel_metadata_json(value: serde_json::Value) -> cama_db::wheel_spin_repository::JsonValue {
    match value {
        serde_json::Value::Null => cama_db::wheel_spin_repository::JsonValue::Null,
        serde_json::Value::Bool(value) => cama_db::wheel_spin_repository::JsonValue::Bool(value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(cama_db::wheel_spin_repository::JsonValue::Integer)
            .or_else(|| {
                value
                    .as_f64()
                    .map(cama_db::wheel_spin_repository::JsonValue::Float)
            })
            .unwrap_or(cama_db::wheel_spin_repository::JsonValue::Null),
        serde_json::Value::String(value) => {
            cama_db::wheel_spin_repository::JsonValue::String(value)
        }
        serde_json::Value::Array(values) => cama_db::wheel_spin_repository::JsonValue::Array(
            values.into_iter().map(wheel_metadata_json).collect(),
        ),
        serde_json::Value::Object(values) => cama_db::wheel_spin_repository::JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, wheel_metadata_json(value)))
                .collect(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn log_wheel_spin(
    path: &Path,
    user_id: i64,
    guild_id: i64,
    result: i64,
    now: i64,
    kind: WheelKind,
    is_bonus: bool,
    outcome_code: &str,
    event_id: &str,
    metadata: serde_json::Value,
) -> Result<(), String> {
    WheelSpinRepository::new(path)
        .log_wheel_spin(&NewWheelSpin {
            discord_id: user_id,
            guild_id: Some(guild_id),
            result,
            spin_time: now,
            is_bankrupt: kind == WheelKind::Bankrupt,
            is_golden: kind == WheelKind::Golden,
            outcome_code: Some(outcome_code.to_owned()),
            is_bonus,
            event_id: Some(event_id.to_owned()),
            outcome_metadata: Some(wheel_metadata_json(metadata)),
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn credit_wheel_income(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    _balance_before: i64,
    gross: i64,
    config: &BettingRuntimeConfig,
    event_id: &str,
    reason: &str,
    vanity_taxable_ids: &BTreeSet<i64>,
) -> Result<cama_db::match_recording_repository::IncomeAwardReceipt, String> {
    if gross <= 0 {
        let balance = PlayerRepository::new(path)
            .get_by_id(user_id, Some(guild_id))
            .map_err(|error| error.to_string())?
            .map(|player| player.jopacoin_balance)
            .unwrap_or_default();
        return Ok(IncomeAwardReceipt {
            discord_id: user_id,
            gross,
            balance_after: balance,
            net: gross,
            ..IncomeAwardReceipt::default()
        });
    }
    MatchRecordingRepository::new(path)
        .credit_income_awards_atomic(
            Some(guild_id),
            &[IncomeAwardRequest {
                discord_id: user_id,
                gross,
                garnishment_rate: config.garnishment_rate,
                apply_bankruptcy_penalty: true,
                bankruptcy_kept_rate: config.bankruptcy_penalty_rate,
                vanity_tax_rate: if vanity_taxable_ids.contains(&user_id) {
                    config.vanity_tax_rate
                } else {
                    0.0
                },
                decrement_bankruptcy_penalty: false,
                source: "gamba",
                related_type: Some("wheel_spin"),
                related_id: Some(event_related_id(event_id)),
                reason,
                exact_once: true,
            }],
        )
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "wheel income credit returned no receipt".to_owned())
}

/// Route wheel Blood Pact payouts through the shared protection policy.  The
/// hostile-loss repository owns the durable event key, protection pool, and
/// recipient transfer; this adapter only translates the app-level port into
/// the existing-schema request used by the rest of the economy.
struct BettingProtectionGateway {
    repository: ShopRuntimeRepository,
    now: i64,
    mana_date: String,
}

impl ProtectionGateway for BettingProtectionGateway {
    type Error = cama_db::shop_runtime::ShopRuntimeRepositoryError;

    fn apply_hostile_loss(
        &mut self,
        request: AppHostileLossRequest,
    ) -> Result<AppHostileLossSettlement, Self::Error> {
        let destination = match request.destination {
            "burn" => DbHostileDestination::Burn,
            "reserve" => DbHostileDestination::Reserve,
            _ => DbHostileDestination::Player,
        };
        let settlement = self.repository.apply_hostile_loss(&DbHostileLossRequest {
            victim_id: request.target_id,
            guild_id: request.guild_id,
            requested: request.attempted_loss,
            kind: request.kind.to_owned(),
            actor_id: Some(request.actor_id),
            event_key: request.event_key,
            destination,
            recipient_id: (destination == DbHostileDestination::Player)
                .then_some(request.recipient_id),
            clamp_to_balance: request.clamp_to_balance,
            min_balance: None,
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

/// Apply the post-outcome Blood Pact step from Python's wheel processor.
///
/// The wheel can settle several independent transfers before it reaches this
/// boundary, so the earning base is the actual positive balance delta rather
/// than the landed wedge value.  The hostile-loss event key makes a retry
/// after a process stop return the already-settled transfer without consuming
/// another portion of the pact cap.
fn apply_wheel_blood_pact_skim(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    balance_before: i64,
    event_id: &str,
    now: i64,
    minigame_scale: f64,
) -> Result<(i64, i64), String> {
    let players = PlayerRepository::new(path);
    let current = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .map(|player| player.jopacoin_balance)
        .unwrap_or(balance_before);
    let earning = current.saturating_sub(balance_before);
    if earning <= 0 {
        return Ok((0, current));
    }
    let event_key = format!("gamba-blood-pact:{event_id}:{user_id}");
    let shop = ShopRuntimeRepository::new(path);
    if let Some(existing) = shop
        .hostile_loss_event(guild_id, user_id, &event_key)
        .map_err(|error| error.to_string())?
    {
        return Ok((existing.applied, current));
    }
    let service =
        BuffService::new(ManashopRepository::new(path), now).with_minigame_scale(minigame_scale);
    let mut gateway = BettingProtectionGateway {
        repository: shop,
        now,
        mana_date: {
            let offset = pacific_offset_seconds(now)?;
            let local = DateTime::<Utc>::from_timestamp(now, 0)
                .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?
                .with_timezone(
                    &FixedOffset::east_opt(offset)
                        .ok_or_else(|| "invalid Pacific time-zone offset".to_owned())?,
                );
            mana_day_label(local)
        },
    };
    let skimmed = service
        .apply_blood_pact_with_protection_event_key(
            user_id,
            guild_id,
            earning,
            event_key,
            &mut gateway,
        )
        .map_err(|error| error.to_string())?;
    let balance_after = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .map(|player| player.jopacoin_balance)
        .unwrap_or(current);
    Ok((skimmed, balance_after))
}

#[allow(clippy::too_many_arguments)]
fn apply_hostile_wheel_loss(
    path: &Path,
    guild_id: i64,
    victim_id: i64,
    actor_id: i64,
    requested: i64,
    kind: &str,
    event_id: &str,
    destination: DbHostileDestination,
    recipient_id: Option<i64>,
    min_balance: Option<i64>,
    clamp_to_balance: bool,
    now: i64,
) -> Result<cama_db::shop_runtime::HostileLossSettlement, String> {
    if requested <= 0 {
        return Err("wheel hostile amount must be positive".to_owned());
    }
    let offset = pacific_offset_seconds(now)?;
    let local = DateTime::<Utc>::from_timestamp(now, 0)
        .ok_or_else(|| "system timestamp is outside chrono's range".to_owned())?
        .with_timezone(
            &FixedOffset::east_opt(offset)
                .ok_or_else(|| "invalid Pacific time-zone offset".to_owned())?,
        );
    ShopRuntimeRepository::new(path)
        .apply_hostile_loss(&DbHostileLossRequest {
            victim_id,
            guild_id,
            requested,
            kind: kind.to_owned(),
            actor_id: Some(actor_id),
            event_key: event_id.to_owned(),
            destination,
            recipient_id,
            clamp_to_balance,
            min_balance,
            metadata: json!({"outcome": kind}),
            occurred_at: now,
            mana_date: mana_day_label(local),
        })
        .map_err(|error| error.to_string())
}

fn apply_swamp_siphon(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    scale: f64,
    event_id: &str,
) -> Result<Option<i64>, String> {
    let players = PlayerRepository::new(path);
    let leaderboard = players
        .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
        .map_err(|error| error.to_string())?;
    let eligible = leaderboard
        .iter()
        .filter(|player| player.discord_id != Some(user_id) && player.jopacoin_balance >= 50)
        .collect::<Vec<_>>();
    let Some(victim) = eligible.get(fastrand::usize(..eligible.len().max(1))) else {
        return Ok(None);
    };
    let Some(victim_id) = victim.discord_id else {
        return Ok(None);
    };
    let requested =
        cama_domain::economy_scaling::scale_minigame_jc_delta(fastrand::i64(1..4) as f64, scale)
            .min(victim.jopacoin_balance)
            .max(0);
    if requested <= 0 {
        return Ok(None);
    }
    let settled = apply_hostile_wheel_loss(
        path,
        guild_id,
        victim_id,
        user_id,
        requested,
        "swamp_siphon",
        event_id,
        DbHostileDestination::Player,
        Some(user_id),
        Some(50),
        true,
        unix_seconds()?.max(0),
    )?;
    Ok((settled.applied > 0).then_some(settled.applied))
}

fn overgrowth_reward(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    effects: &ManaEffects,
    minigame_scale: f64,
) -> Result<i64, String> {
    let since = Utc::now() - chrono::Duration::days(7);
    let matches = match MatchRepository::new(path).get_player_matches(user_id, Some(guild_id), 100)
    {
        Ok(matches) => matches,
        Err(_) => {
            return Ok(cama_domain::economy_scaling::scale_minigame_jc_delta(
                10.0,
                minigame_scale,
            ));
        }
    };
    let games_this_week = matches
        .iter()
        .filter(|player_match| match &player_match.match_date {
            SqlValue::Text(value) => {
                parse_match_timestamp(value.as_str()).is_some_and(|timestamp| timestamp >= since)
            }
            _ => false,
        })
        .count() as i64;
    let base = games_this_week.saturating_mul(10).max(10);
    let capped = effects
        .green_gain_cap
        .map_or(base, |cap| base.min(cap.max(0)));
    Ok(cama_domain::economy_scaling::scale_minigame_jc_delta(
        capped as f64,
        minigame_scale,
    ))
}

/// Apply the daily gamba event exactly once to a newly minted wheel reward.
///
/// Numeric wedges are adjusted at admission, while mechanic rewards reach
/// this helper after their central minigame scaling. Keeping the two stages in
/// one small policy function prevents Eruption/Chain/Dividend and hostile
/// fallback credits from accidentally applying the event twice.
fn scale_minted_gamba_reward(
    base: i64,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    bonus_spin: bool,
) -> i64 {
    let adjusted = apply_gamba_event_multiplier(base, event_win_multiplier, event_loss_multiplier);
    if bonus_spin {
        cama_app::wheel::scale_positive_dig_jc(adjusted)
    } else {
        adjusted
    }
}

fn parse_match_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .map(|timestamp| DateTime::<Utc>::from_naive_utc_and_offset(timestamp, Utc))
        })
        .ok()
}

#[derive(Clone, Debug)]
struct WheelResolution {
    resolved: WheelValue,
    logged: i64,
    cooldown_outcome: CooldownOutcome,
    extra_note: String,
    income: Option<cama_db::match_recording_repository::IncomeAwardReceipt>,
    /// Lightning is a server-wide outcome whose Neon hook needs both the
    /// aggregate tax and the number of accounts actually charged.  Keeping
    /// that side effect beside the policy result avoids reconstructing it
    /// from a later, potentially changed leaderboard.
    neon_lightning: Option<(i64, usize)>,
}

impl WheelResolution {
    fn new(
        resolved: WheelValue,
        logged: i64,
        cooldown_outcome: CooldownOutcome,
        extra_note: String,
    ) -> Self {
        Self {
            resolved,
            logged,
            cooldown_outcome,
            extra_note,
            income: None,
            neon_lightning: None,
        }
    }

    fn with_lightning(mut self, total: i64, players: usize) -> Self {
        self.neon_lightning = Some((total, players));
        self
    }

    fn with_income(
        mut self,
        income: cama_db::match_recording_repository::IncomeAwardReceipt,
    ) -> Self {
        self.income = Some(income);
        self
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_wheel_value(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    balance_before: i64,
    landed: &cama_app::wheel::WheelWedge,
    kind: WheelKind,
    effects: &ManaEffects,
    config: &BettingRuntimeConfig,
    event_id: &str,
    vanity_taxable_ids: &BTreeSet<i64>,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    bonus_spin: bool,
    visible_positive_balance: Option<i64>,
) -> Result<WheelResolution, String> {
    let mechanic = match landed.value {
        WheelValue::Numeric(value) => {
            // Both direct and interactive selections arrive here before the
            // daily-event adjustment. Applying it at this single boundary
            // avoids double-scaling numeric wedges while letting minted
            // mechanic rewards use the same event snapshot.
            let adjusted =
                apply_gamba_event_multiplier(value, event_win_multiplier, event_loss_multiplier);
            if adjusted > 0 {
                let mut adjusted = adjusted;
                if bonus_spin {
                    adjusted = cama_app::wheel::scale_positive_dig_jc(adjusted);
                }
                if effects.blue_gamba_reduction > 0.0 {
                    adjusted = adjusted
                        .saturating_sub((adjusted as f64 * effects.blue_gamba_reduction) as i64);
                }
                if effects.green_gain_cap.is_some_and(|cap| adjusted > cap) {
                    adjusted = effects.green_gain_cap.unwrap_or(adjusted);
                }
                let receipt = credit_wheel_income(
                    path,
                    guild_id,
                    user_id,
                    balance_before,
                    adjusted,
                    config,
                    event_id,
                    "gamba wheel payout",
                    vanity_taxable_ids,
                )?;
                return Ok(WheelResolution::new(
                    WheelValue::Numeric(adjusted),
                    adjusted,
                    CooldownOutcome::Other,
                    wheel_income_note(&receipt),
                )
                .with_income(receipt));
            }
            if adjusted < 0
                && kind == WheelKind::Bankrupt
                && PlayerRepository::new(path)
                    .consume_wheel_pardon_atomic(user_id, Some(guild_id))
                    .map_err(|error| error.to_string())?
            {
                return Ok(WheelResolution::new(
                    WheelValue::Numeric(0),
                    0,
                    CooldownOutcome::Other,
                    "🛡️ Comeback pardon consumed: the bankrupt loss was forgiven.".to_owned(),
                ));
            }
            if adjusted < 0 {
                LoanRepository::new(path)
                    .transfer_balance_to_nonprofit_atomic(
                        user_id,
                        Some(guild_id),
                        adjusted.saturating_abs(),
                        &LedgerContext {
                            source: Some("gamba".to_owned()),
                            actor_id: Some(user_id),
                            related_type: Some("wheel_spin".to_owned()),
                            related_id: Some(event_id.to_owned()),
                            reason: Some("gamba wheel loss reserve credit".to_owned()),
                            metadata: Some(json!({"wedge": landed.label}).to_string()),
                        },
                    )
                    .map_err(|error| error.to_string())?;
            }
            return Ok(WheelResolution::new(
                WheelValue::Numeric(adjusted),
                adjusted,
                if adjusted == 0 {
                    CooldownOutcome::LoseTurn
                } else {
                    CooldownOutcome::Other
                },
                String::new(),
            ));
        }
        WheelValue::Mechanic(mechanic) => mechanic,
    };
    let minted_reward = |base: i64| {
        scale_minted_gamba_reward(
            base,
            event_win_multiplier,
            event_loss_multiplier,
            bonus_spin,
        )
    };
    match mechanic {
        WheelMechanic::ExtendOne | WheelMechanic::ExtendTwo => {
            let delta = if mechanic == WheelMechanic::ExtendOne {
                1
            } else {
                2
            };
            let bankruptcy = BankruptcyRepository::new(path);
            // Python only extends an existing bankruptcy penalty. A player
            // can still land an EXTEND wedge after recovering from debt; in
            // that case the result is deliberately a no-op rather than a new
            // penalty state.
            if bankruptcy
                .get_penalty_games(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                > 0
            {
                bankruptcy
                    .adjust_penalty_games_atomic(user_id, Some(guild_id), delta)
                    .map_err(|error| error.to_string())?;
            }
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                String::new(),
            ))
        }
        WheelMechanic::Jailbreak => {
            BankruptcyRepository::new(path)
                .adjust_penalty_games_atomic(user_id, Some(guild_id), -1)
                .map_err(|error| error.to_string())?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                String::new(),
            ))
        }
        WheelMechanic::Eruption => {
            let last_spin = WheelSpinRepository::new(path)
                .get_last_normal_wheel_spin(Some(guild_id))
                .map_err(|error| error.to_string())?;
            let base_reward = last_spin
                .map(|spin| spin.result.saturating_abs().saturating_mul(2))
                .filter(|reward| *reward > 0)
                .unwrap_or_else(|| {
                    cama_domain::economy_scaling::scale_minigame_jc_delta(
                        50.0,
                        config.minigame_jc_delta_scale,
                    )
                });
            let reward = minted_reward(base_reward);
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba eruption fallback credit",
                vanity_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::Overgrowth => {
            let reward = minted_reward(overgrowth_reward(
                path,
                guild_id,
                user_id,
                effects,
                config.minigame_jc_delta_scale,
            )?);
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba overgrowth reward",
                vanity_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::CompoundInterest => {
            let reward = minted_reward(100);
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba compound interest credit",
                vanity_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                reward,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::Dividend => {
            let total = visible_positive_balance.unwrap_or(
                GoldenWheelRepository::new(path)
                    .get_total_positive_balance(Some(guild_id))
                    .map_err(|error| error.to_string())?,
            );
            let reward = minted_reward(((total as f64 * 0.005) as i64).max(10));
            let receipt = credit_wheel_income(
                path,
                guild_id,
                user_id,
                balance_before,
                reward,
                config,
                event_id,
                "gamba dividend credit",
                vanity_taxable_ids,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                reward,
                CooldownOutcome::Other,
                wheel_income_note(&receipt),
            )
            .with_income(receipt))
        }
        WheelMechanic::Recession => {
            let recession = resolve_recession(
                path,
                guild_id,
                user_id,
                event_id,
                config.minigame_jc_delta_scale,
            )?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                -recession.self_loss,
                CooldownOutcome::Other,
                format!(
                    "**{}** player(s) lost a slice of their balance. You lost **{}** {}. Total drained from the server: **{}** {} (sent to the Jopacoin Reserve).",
                    recession.count,
                    recession.self_loss,
                    JOPACOIN_EMOTE,
                    recession.total,
                    JOPACOIN_EMOTE,
                ),
            ))
        }
        WheelMechanic::RedShell
        | WheelMechanic::BlueShell
        | WheelMechanic::BananaPeel
        | WheelMechanic::GreenShell
        | WheelMechanic::BombOmb
        | WheelMechanic::LightningBolt
        | WheelMechanic::Commune
        | WheelMechanic::TrickleDown
        | WheelMechanic::Heist
        | WheelMechanic::MarketCrash
        | WheelMechanic::HostileTakeover
        | WheelMechanic::Emergency
        | WheelMechanic::Decay => {
            let hostile = resolve_hostile_mechanic(
                path,
                guild_id,
                user_id,
                balance_before,
                mechanic,
                event_id,
                config,
                vanity_taxable_ids,
                event_win_multiplier,
            )?;
            let resolution = WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                hostile.log_result,
                CooldownOutcome::Other,
                hostile_result_note(mechanic, &hostile, config),
            );
            if hostile.lightning_total > 0 {
                Ok(resolution.with_lightning(hostile.lightning_total, hostile.lightning_count))
            } else {
                Ok(resolution)
            }
        }
        WheelMechanic::Comeback => {
            PlayerRepository::new(path)
                .set_wheel_pardon(user_id, Some(guild_id), true)
                .map_err(|error| error.to_string())?;
            Ok(WheelResolution::new(
                WheelValue::Mechanic(mechanic),
                0,
                CooldownOutcome::Other,
                "🛡️ Comeback pardon armed: your next bankrupt loss is forgiven.".to_owned(),
            ))
        }
        WheelMechanic::ChainReaction => {
            let previous = WheelSpinRepository::new(path)
                .get_last_normal_wheel_spin(Some(guild_id))
                .map_err(|error| error.to_string())?;
            let Some(previous) = previous else {
                return Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    "No previous ordinary spin is available to copy.".to_owned(),
                ));
            };
            if previous.result > 0 {
                let reward = minted_reward(previous.result);
                let receipt = credit_wheel_income(
                    path,
                    guild_id,
                    user_id,
                    balance_before,
                    reward,
                    config,
                    event_id,
                    "gamba chain reaction credit",
                    vanity_taxable_ids,
                )?;
                Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    format!(
                        "Copied <@{}>'s previous **{}** {} outcome.",
                        previous.discord_id, previous.result, JOPACOIN_EMOTE
                    ),
                )
                .with_income(receipt))
            } else if previous.result < 0 {
                LoanRepository::new(path)
                    .transfer_balance_to_nonprofit_atomic(
                        user_id,
                        Some(guild_id),
                        previous.result.saturating_abs(),
                        &LedgerContext {
                            source: Some("gamba".to_owned()),
                            actor_id: Some(user_id),
                            related_type: Some("wheel_spin".to_owned()),
                            related_id: Some(event_id.to_owned()),
                            reason: Some("gamba chain reaction loss reserve credit".to_owned()),
                            metadata: Some(
                                json!({"copied_spin_user_id": previous.discord_id}).to_string(),
                            ),
                        },
                    )
                    .map_err(|error| error.to_string())?;
                Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    format!(
                        "Copied <@{}>'s previous **{}** {} outcome.",
                        previous.discord_id, previous.result, JOPACOIN_EMOTE
                    ),
                ))
            } else {
                Ok(WheelResolution::new(
                    WheelValue::Mechanic(mechanic),
                    0,
                    CooldownOutcome::Other,
                    "The previous spin had no numeric outcome to copy.".to_owned(),
                ))
            }
        }
        WheelMechanic::TownTrial | WheelMechanic::Discover => Ok(WheelResolution::new(
            WheelValue::Mechanic(mechanic),
            0,
            CooldownOutcome::Other,
            "Interactive wheel outcome requires an active Discord view.".to_owned(),
        )),
    }
}

fn wheel_income_note(receipt: &cama_db::match_recording_repository::IncomeAwardReceipt) -> String {
    let mut notes = Vec::new();
    if receipt.garnished > 0 {
        notes.push(format!(
            "{} {} garnished",
            receipt.garnished, JOPACOIN_EMOTE
        ));
    }
    if receipt.bankruptcy_penalty > 0 {
        notes.push(format!(
            "{} {} bankruptcy penalty",
            receipt.bankruptcy_penalty, JOPACOIN_EMOTE
        ));
    }
    if receipt.vanity_tax > 0 {
        notes.push(format!(
            "{} {} vanity tax",
            receipt.vanity_tax, JOPACOIN_EMOTE
        ));
    }
    if notes.is_empty() {
        String::new()
    } else {
        format!("\n{}", notes.join(" | "))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RecessionResolution {
    total: i64,
    count: usize,
    self_loss: i64,
}

fn resolve_recession(
    path: &Path,
    guild_id: i64,
    spinner_id: i64,
    event_id: &str,
    scale: f64,
) -> Result<RecessionResolution, String> {
    let leaderboard = PlayerRepository::new(path)
        .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
        .map_err(|error| error.to_string())?;
    let now = unix_seconds()?.max(0);
    let mut total = 0_i64;
    let mut count = 0_usize;
    let mut spinner_loss = 0_i64;
    for (rank, player) in leaderboard.iter().enumerate() {
        if player.jopacoin_balance < cama_app::wheel::HOSTILE_LOSS_MIN_BALANCE {
            continue;
        }
        let (percentage, minimum) = if rank < 3 {
            (0.06, 50_i64)
        } else if rank < 10 {
            (0.035, 10_i64)
        } else {
            (0.02, 1_i64)
        };
        let requested = cama_domain::economy_scaling::scale_minigame_jc_delta(
            (minimum.max((player.jopacoin_balance as f64 * percentage) as i64)) as f64,
            scale,
        )
        .min(player.jopacoin_balance)
        .max(0);
        let Some(victim_id) = player.discord_id else {
            continue;
        };
        if requested <= 0 {
            continue;
        }
        let settled = apply_hostile_wheel_loss(
            path,
            guild_id,
            victim_id,
            spinner_id,
            requested,
            WheelMechanic::Recession.code(),
            &format!("{event_id}:{victim_id}"),
            DbHostileDestination::Reserve,
            None,
            Some(cama_app::wheel::HOSTILE_LOSS_MIN_BALANCE),
            true,
            now,
        )?;
        total = total.saturating_add(settled.applied);
        if settled.applied > 0 {
            count = count.saturating_add(1);
        }
        if victim_id == spinner_id {
            spinner_loss = spinner_loss.saturating_add(settled.applied);
        }
    }
    Ok(RecessionResolution {
        total,
        count,
        self_loss: spinner_loss,
    })
}

#[derive(Clone, Debug, Default)]
struct HostileResolution {
    /// Numeric value persisted to wheel history.  This intentionally differs
    /// from `total` for effects whose Python history contract records zero
    /// (for example Lightning, Emergency, and a missed Takeover).
    log_result: i64,
    lightning_total: i64,
    lightning_count: usize,
    /// Presentation facts retained from the same settlement pass.  Python's
    /// result embed reports these values; recomputing them after the atomic
    /// hostile-loss pass would race another economy event, so keep the live
    /// receipt snapshot beside the durable result.
    total: i64,
    count: usize,
    missed: bool,
    self_hit: bool,
    victim_name: Option<String>,
    victim_new_balance: Option<i64>,
    victims: Vec<(String, i64)>,
    shield_absorbed_total: i64,
    shielded_count: usize,
}

#[allow(clippy::too_many_arguments)]
fn resolve_hostile_mechanic(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    balance_before: i64,
    mechanic: WheelMechanic,
    event_id: &str,
    config: &BettingRuntimeConfig,
    vanity_taxable_ids: &BTreeSet<i64>,
    event_win_multiplier: f64,
) -> Result<HostileResolution, String> {
    let players = PlayerRepository::new(path);
    let leaderboard = players
        .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
        .map_err(|error| error.to_string())?;
    let now = unix_seconds()?;
    let eligible = leaderboard
        .iter()
        .filter(|row| row.discord_id != Some(user_id) && row.jopacoin_balance >= 50)
        .collect::<Vec<_>>();
    let total = Cell::new(0_i64);
    let mut log_result = 0_i64;
    let lightning_count = Cell::new(0_usize);
    let applied_count = Cell::new(0_usize);
    let shield_absorbed_total = Cell::new(0_i64);
    let shielded_count = Cell::new(0_usize);
    let settle = |victim_id: i64,
                  requested: i64,
                  destination: DbHostileDestination,
                  recipient_id: Option<i64>,
                  clamp: bool|
     -> Result<(), String> {
        let result = apply_hostile_wheel_loss(
            path,
            guild_id,
            victim_id,
            user_id,
            requested,
            mechanic.code(),
            &format!("{event_id}:{victim_id}"),
            destination,
            recipient_id,
            Some(50),
            clamp,
            now,
        )?;
        total.set(total.get().saturating_add(result.applied));
        if result.applied > 0 {
            applied_count.set(applied_count.get().saturating_add(1));
        }
        if result.absorbed > 0 {
            shield_absorbed_total.set(shield_absorbed_total.get().saturating_add(result.absorbed));
            shielded_count.set(shielded_count.get().saturating_add(1));
        }
        if mechanic == WheelMechanic::LightningBolt && result.applied > 0 {
            lightning_count.set(lightning_count.get().saturating_add(1));
        }
        Ok(())
    };
    let mut victim_name = None;
    let mut victim_new_balance = None;
    let mut victims = Vec::new();
    let mut self_hit = false;
    let mut missed = false;
    match mechanic {
        WheelMechanic::RedShell => {
            if let Some(victim) = players
                .get_player_above(user_id, Some(guild_id), Some(50))
                .map_err(|error| error.to_string())?
                && let Some(victim_id) = victim.discord_id
            {
                let before = total.get();
                settle(
                    victim_id,
                    requested_percentage_or_flat(
                        victim.jopacoin_balance,
                        LossRoll {
                            percentage: 0.02 + fastrand::f64() * (0.0924 - 0.02),
                            flat: fastrand::i64(2..14),
                        },
                        config.minigame_jc_delta_scale,
                    ),
                    DbHostileDestination::Player,
                    Some(user_id),
                    false,
                )?;
                log_result = total.get().saturating_sub(before);
                if log_result > 0 {
                    victim_name = Some(victim.name.clone());
                    victim_new_balance = players
                        .get_by_id(victim_id, Some(guild_id))
                        .map_err(|error| error.to_string())?
                        .map(|player| player.jopacoin_balance);
                }
            }
        }
        WheelMechanic::BlueShell => {
            if let Some(victim) = leaderboard.first()
                && let Some(victim_id) = victim.discord_id
            {
                let before = total.get();
                settle(
                    victim_id,
                    requested_percentage_or_flat(
                        victim.jopacoin_balance,
                        LossRoll {
                            percentage: if victim_id == user_id {
                                0.02 + fastrand::f64() * (0.07 - 0.02)
                            } else {
                                0.02 + fastrand::f64() * (0.1016 - 0.02)
                            },
                            flat: if victim_id == user_id {
                                fastrand::i64(4..21)
                            } else {
                                fastrand::i64(4..30)
                            },
                        },
                        config.minigame_jc_delta_scale,
                    ),
                    if victim_id == user_id {
                        DbHostileDestination::Reserve
                    } else {
                        DbHostileDestination::Player
                    },
                    (victim_id != user_id).then_some(user_id),
                    false,
                )?;
                let applied = total.get().saturating_sub(before);
                self_hit = victim_id == user_id;
                log_result = if victim_id == user_id {
                    -applied
                } else {
                    applied
                };
                if applied > 0 && !self_hit {
                    victim_name = Some(victim.name.clone());
                    victim_new_balance = players
                        .get_by_id(victim_id, Some(guild_id))
                        .map_err(|error| error.to_string())?
                        .map(|player| player.jopacoin_balance);
                }
            }
        }
        WheelMechanic::BananaPeel => {
            if let Some(victim) = players
                .get_player_below(user_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                && let Some(victim_id) = victim.discord_id
            {
                settle(
                    victim_id,
                    fastrand::i64(WHEEL_BANANA_PEEL_LOSS_MIN..WHEEL_BANANA_PEEL_LOSS_MAX + 1),
                    DbHostileDestination::Burn,
                    None,
                    true,
                )?;
                let applied = total.get();
                if applied > 0 {
                    victim_name = Some(victim.name.clone());
                }
            }
        }
        WheelMechanic::GreenShell => {
            if let Some(victim) = eligible.get(fastrand::usize(..eligible.len().max(1)))
                && let Some(victim_id) = victim.discord_id
            {
                let before = total.get();
                settle(
                    victim_id,
                    fastrand::i64(WHEEL_GREEN_SHELL_STEAL_MIN..WHEEL_GREEN_SHELL_STEAL_MAX + 1),
                    DbHostileDestination::Player,
                    Some(user_id),
                    false,
                )?;
                log_result = total.get().saturating_sub(before);
                if log_result > 0 {
                    victim_name = Some(victim.name.clone());
                }
            }
        }
        WheelMechanic::BombOmb => {
            let mut sampled = eligible.clone();
            fastrand::shuffle(&mut sampled);
            for victim in sampled.iter().take(WHEEL_BOMB_OMB_VICTIM_COUNT) {
                if let Some(victim_id) = victim.discord_id {
                    let before = total.get();
                    settle(
                        victim_id,
                        fastrand::i64(
                            WHEEL_BOMB_OMB_VICTIM_LOSS_MIN..WHEEL_BOMB_OMB_VICTIM_LOSS_MAX + 1,
                        ),
                        DbHostileDestination::Burn,
                        None,
                        true,
                    )?;
                    let applied = total.get().saturating_sub(before);
                    if applied > 0 {
                        victims.push((victim.name.clone(), applied));
                    }
                }
            }
        }
        WheelMechanic::LightningBolt => {
            let tax_rate = 0.02 + fastrand::f64() * (0.0627 - 0.02);
            // Lightning is the one server-wide tax that includes the
            // spinner. Python's leaderboard pass intentionally charges every
            // account at/above the hostile floor, including self.
            for victim in leaderboard
                .iter()
                .filter(|player| player.jopacoin_balance >= 50)
            {
                if let Some(victim_id) = victim.discord_id {
                    let before = total.get();
                    settle(
                        victim_id,
                        cama_domain::economy_scaling::scale_minigame_jc_delta(
                            (1_i64.max((victim.jopacoin_balance as f64 * tax_rate) as i64)) as f64,
                            config.minigame_jc_delta_scale,
                        ),
                        DbHostileDestination::Reserve,
                        None,
                        true,
                    )?;
                    let applied = total.get().saturating_sub(before);
                    if applied > 0 && victims.len() < 3 {
                        victims.push((victim.name.clone(), applied));
                    }
                }
            }
        }
        WheelMechanic::Commune | WheelMechanic::TrickleDown => {
            let tax_rate = 0.02 + fastrand::f64() * (0.05 - 0.02);
            for victim in &eligible {
                if let Some(victim_id) = victim.discord_id {
                    let before = total.get();
                    settle(
                        victim_id,
                        if mechanic == WheelMechanic::Commune {
                            1
                        } else {
                            cama_domain::economy_scaling::scale_minigame_jc_delta(
                                (1_i64.max((victim.jopacoin_balance as f64 * tax_rate) as i64))
                                    as f64,
                                config.minigame_jc_delta_scale,
                            )
                        },
                        DbHostileDestination::Player,
                        Some(user_id),
                        true,
                    )?;
                    let applied = total.get().saturating_sub(before);
                    if applied > 0 && victims.len() < 3 {
                        victims.push((victim.name.clone(), applied));
                    }
                }
            }
            log_result = total.get();
        }
        WheelMechanic::Heist => {
            for victim in eligible.iter().rev().take(30) {
                if let Some(victim_id) = victim.discord_id {
                    let before = total.get();
                    settle(
                        victim_id,
                        cama_domain::economy_scaling::scale_minigame_jc_delta(
                            (victim.jopacoin_balance as f64
                                * (0.05 + fastrand::f64() * (0.12 - 0.05)))
                                .max(1.0),
                            config.minigame_jc_delta_scale,
                        ),
                        DbHostileDestination::Player,
                        Some(user_id),
                        false,
                    )?;
                    let applied = total.get().saturating_sub(before);
                    if applied > 0 && victims.len() < 3 {
                        victims.push((victim.name.clone(), applied));
                    }
                }
            }
            if total.get() == 0 {
                let reward = apply_gamba_event_multiplier(
                    cama_domain::economy_scaling::scale_minigame_jc_delta(
                        20.0,
                        config.minigame_jc_delta_scale,
                    ),
                    event_win_multiplier,
                    1.0,
                );
                total.set(
                    credit_wheel_income(
                        path,
                        guild_id,
                        user_id,
                        balance_before,
                        reward,
                        config,
                        event_id,
                        "gamba heist fallback credit",
                        vanity_taxable_ids,
                    )?
                    .net,
                );
            }
            log_result = total.get();
        }
        WheelMechanic::MarketCrash => {
            for victim in leaderboard.iter().take(3) {
                if let Some(victim_id) = victim.discord_id
                    && victim_id != user_id
                    && victim.jopacoin_balance >= 50
                {
                    let before = total.get();
                    settle(
                        victim_id,
                        cama_domain::economy_scaling::scale_minigame_jc_delta(
                            (victim.jopacoin_balance as f64
                                * (0.08 + fastrand::f64() * (0.15 - 0.08)))
                                .max(1.0),
                            config.minigame_jc_delta_scale,
                        ),
                        DbHostileDestination::Player,
                        Some(user_id),
                        false,
                    )?;
                    let applied = total.get().saturating_sub(before);
                    if applied > 0 && victims.len() < 3 {
                        victims.push((victim.name.clone(), applied));
                    }
                }
            }
            if total.get() == 0 {
                let reward = apply_gamba_event_multiplier(
                    cama_domain::economy_scaling::scale_minigame_jc_delta(
                        25.0,
                        config.minigame_jc_delta_scale,
                    ),
                    event_win_multiplier,
                    1.0,
                );
                total.set(
                    credit_wheel_income(
                        path,
                        guild_id,
                        user_id,
                        balance_before,
                        reward,
                        config,
                        event_id,
                        "gamba market crash fallback credit",
                        vanity_taxable_ids,
                    )?
                    .net,
                );
            }
            log_result = total.get();
        }
        WheelMechanic::HostileTakeover => {
            if let Some(victim) = leaderboard.get(3)
                && let Some(victim_id) = victim.discord_id
                && victim.jopacoin_balance >= 50
            {
                let before = total.get();
                settle(
                    victim_id,
                    cama_domain::economy_scaling::scale_minigame_jc_delta(
                        (victim.jopacoin_balance as f64 * (0.08 + fastrand::f64() * (0.15 - 0.08)))
                            .max(1.0),
                        config.minigame_jc_delta_scale,
                    ),
                    DbHostileDestination::Player,
                    Some(user_id),
                    false,
                )?;
                let applied = total.get().saturating_sub(before);
                if applied > 0 {
                    victim_name = Some(victim.name.clone());
                    victim_new_balance = players
                        .get_by_id(victim_id, Some(guild_id))
                        .map_err(|error| error.to_string())?
                        .map(|player| player.jopacoin_balance);
                }
            }
            let takeover_missed = total.get() == 0;
            missed = takeover_missed;
            if takeover_missed {
                let reward = apply_gamba_event_multiplier(
                    cama_domain::economy_scaling::scale_minigame_jc_delta(
                        40.0,
                        config.minigame_jc_delta_scale,
                    ),
                    event_win_multiplier,
                    1.0,
                );
                total.set(
                    credit_wheel_income(
                        path,
                        guild_id,
                        user_id,
                        balance_before,
                        reward,
                        config,
                        event_id,
                        "gamba hostile takeover fallback credit",
                        vanity_taxable_ids,
                    )?
                    .net,
                );
            }
            // Python records a missed Takeover as zero even though the
            // fallback consolation credit is still applied to the wallet.
            log_result = if takeover_missed { 0 } else { total.get() };
        }
        WheelMechanic::Emergency => {
            for victim in &leaderboard {
                if let Some(victim_id) = victim.discord_id
                    && victim.jopacoin_balance >= 50
                {
                    settle(
                        victim_id,
                        cama_domain::economy_scaling::scale_minigame_jc_delta(
                            20.0,
                            config.minigame_jc_delta_scale,
                        ),
                        DbHostileDestination::Burn,
                        None,
                        true,
                    )?;
                }
            }
        }
        WheelMechanic::Decay => {
            for (rank, victim) in leaderboard.iter().take(4).enumerate() {
                if let Some(victim_id) = victim.discord_id
                    && victim_id != user_id
                    && victim.jopacoin_balance >= 50
                {
                    settle(
                        victim_id,
                        cama_domain::economy_scaling::scale_minigame_jc_delta(
                            if rank == 3 { 80.0 } else { 60.0 },
                            config.minigame_jc_delta_scale,
                        ),
                        DbHostileDestination::Player,
                        Some(user_id),
                        true,
                    )?;
                }
            }
        }
        _ => {}
    }
    let total_value = total.get();
    if matches!(
        mechanic,
        WheelMechanic::RedShell
            | WheelMechanic::BlueShell
            | WheelMechanic::BananaPeel
            | WheelMechanic::GreenShell
            | WheelMechanic::BombOmb
    ) && total_value == 0
    {
        missed = true;
    }
    victims.sort_by(|left, right| right.1.cmp(&left.1));
    Ok(HostileResolution {
        log_result,
        lightning_total: if mechanic == WheelMechanic::LightningBolt {
            total_value
        } else {
            0
        },
        lightning_count: lightning_count.get(),
        total: total_value,
        count: applied_count.get(),
        missed,
        self_hit,
        victim_name,
        victim_new_balance,
        victims,
        shield_absorbed_total: shield_absorbed_total.get(),
        shielded_count: shielded_count.get(),
    })
}

fn hostile_result_note(
    mechanic: WheelMechanic,
    outcome: &HostileResolution,
    config: &BettingRuntimeConfig,
) -> String {
    let note = match mechanic {
        WheelMechanic::RedShell if outcome.missed => {
            "The Red Shell circles the track but finds no eligible target!".to_owned()
        }
        WheelMechanic::RedShell => format!(
            "💥 Red Shell locked onto **{}**! You stole **{}** {}.\n\n*Victim's new balance: **{}** {}*",
            outcome.victim_name.as_deref().unwrap_or("the player above"),
            outcome.total,
            JOPACOIN_EMOTE,
            outcome.victim_new_balance.unwrap_or_default(),
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::BlueShell if outcome.missed => {
            "The Blue Shell circles the track but finds no target!".to_owned()
        }
        WheelMechanic::BlueShell if outcome.self_hit => format!(
            "💥 The Blue Shell targets the leader — **that's you**! You lost **{}** {}.",
            outcome.total, JOPACOIN_EMOTE,
        ),
        WheelMechanic::BlueShell => format!(
            "💥 Blue Shell targets the leader: **{}**! You stole **{}** {}.\n\n*Victim's new balance: **{}** {}*",
            outcome
                .victim_name
                .as_deref()
                .unwrap_or("the richest player"),
            outcome.total,
            JOPACOIN_EMOTE,
            outcome.victim_new_balance.unwrap_or_default(),
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::LightningBolt => {
            let victims = outcome
                .victims
                .iter()
                .map(|(name, amount)| format!("⚡ **{name}** lost **{amount}** JC\n"))
                .collect::<String>();
            format!(
                "Lightning struck **{}** player(s) for a total of **{}** {}. All funds went to the Jopacoin Reserve.\n{}",
                outcome.lightning_count, outcome.lightning_total, JOPACOIN_EMOTE, victims,
            )
        }
        WheelMechanic::Emergency => format!(
            "Economic crisis: **{}** player(s) each lost up to **{}** {}. Total drained: **{}** {} (vanished).",
            outcome.count,
            cama_domain::economy_scaling::scale_minigame_jc_delta(
                20.0,
                config.minigame_jc_delta_scale,
            ),
            JOPACOIN_EMOTE,
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::Commune => format!(
            "**{}** player(s) donated 1 JC. You received **+{}** {} from the collective.",
            outcome.count, outcome.total, JOPACOIN_EMOTE,
        ),
        WheelMechanic::Heist => {
            if outcome.count == 0 {
                format!(
                    "No eligible players were found. Consolation prize: **+{}** {}.",
                    outcome.total, JOPACOIN_EMOTE
                )
            } else {
                format!(
                    "Robbed **{}** players at the bottom of the ladder for **{}** {} total.",
                    outcome.count, outcome.total, JOPACOIN_EMOTE
                )
            }
        }
        WheelMechanic::MarketCrash => {
            if outcome.count == 0 {
                format!(
                    "No eligible whales were found. Consolation prize: **+{}** {}.",
                    outcome.total, JOPACOIN_EMOTE
                )
            } else {
                format!(
                    "Taxed **{}** fellow top-3 players for **{}** {} total.",
                    outcome.count, outcome.total, JOPACOIN_EMOTE
                )
            }
        }
        WheelMechanic::TrickleDown => {
            if outcome.count == 0 {
                "There was no one else to tax.".to_owned()
            } else {
                format!(
                    "Taxed **{}** players for **{}** {} total.",
                    outcome.count, outcome.total, JOPACOIN_EMOTE
                )
            }
        }
        WheelMechanic::HostileTakeover if outcome.missed => format!(
            "Rank #4 was broke or absent. Consolation prize: **+{}** {}.",
            outcome.total, JOPACOIN_EMOTE,
        ),
        WheelMechanic::HostileTakeover => format!(
            "Corporate raid on **{}** (rank #4)! Seized **{}** {}.",
            outcome.victim_name.as_deref().unwrap_or("rank #4"),
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::BananaPeel if outcome.missed => {
            "You dropped a banana peel, but no one was riding your bumper.".to_owned()
        }
        WheelMechanic::BananaPeel => format!(
            "**{}** (ranked just below you) slipped on your peel and lost **{}** {} — burned into the void.",
            outcome
                .victim_name
                .as_deref()
                .unwrap_or("the player behind you"),
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::GreenShell if outcome.missed => {
            "The shell skittered off the track — no one was around to clip.".to_owned()
        }
        WheelMechanic::GreenShell => format!(
            "You launched a green shell at **{}** and stole **{}** {}.",
            outcome.victim_name.as_deref().unwrap_or("someone"),
            outcome.total,
            JOPACOIN_EMOTE,
        ),
        WheelMechanic::BombOmb if outcome.missed => {
            "The bomb-omb rolled into an empty room and fizzled.".to_owned()
        }
        WheelMechanic::BombOmb => {
            let victims = outcome
                .victims
                .iter()
                .map(|(name, amount)| {
                    format!("💥 **{name}** lost **{amount}** {}\n", JOPACOIN_EMOTE)
                })
                .collect::<String>();
            format!(
                "KABOOM!\n{}Total burned: **{}** {} (vanished, not banked).",
                victims, outcome.total, JOPACOIN_EMOTE
            )
        }
        WheelMechanic::Decay => format!(
            "The top 3 were targeted for {} {} each and rank #4 for {} {}. The Swamp claims what it is owed.",
            cama_domain::economy_scaling::scale_minigame_jc_delta(
                60.0,
                config.minigame_jc_delta_scale,
            ),
            JOPACOIN_EMOTE,
            cama_domain::economy_scaling::scale_minigame_jc_delta(
                80.0,
                config.minigame_jc_delta_scale,
            ),
            JOPACOIN_EMOTE,
        ),
        _ => String::new(),
    };
    if outcome.shield_absorbed_total > 0 {
        format!(
            "{note}\n\n🌾 White Mana Shields: **{}** shield activation(s) absorbed **{}** {}.",
            outcome.shielded_count, outcome.shield_absorbed_total, JOPACOIN_EMOTE,
        )
    } else {
        note
    }
}

#[allow(clippy::too_many_arguments)]
fn spin_once(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    now: i64,
    config: &BettingRuntimeConfig,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    vanity_taxable_ids: &BTreeSet<i64>,
    admin_bypass: bool,
    bonus_spin: bool,
    visible_member_ids: Option<&BTreeSet<i64>>,
) -> Result<SpinResult, String> {
    let players = PlayerRepository::new(path);
    let Some(player) = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
    else {
        return Ok(spin_result(
            "You need to /player register before you can spin the wheel.",
        ));
    };
    let last_regular_spin = bonus_spin
        .then(|| players.get_last_wheel_spin(user_id, Some(guild_id)))
        .transpose()
        .map_err(|error| error.to_string())?
        .flatten();
    if bonus_spin {
        // Dig bonuses are free and deliberately leave the persisted regular
        // cooldown untouched.  Keep the prior timestamp for the final copy.
    } else if admin_bypass {
        players
            .set_last_wheel_spin(user_id, Some(guild_id), now)
            .map_err(|error| error.to_string())?;
    } else {
        match players
            .try_claim_wheel_spin(
                user_id,
                Some(guild_id),
                now,
                cama_app::wheel::WHEEL_COOLDOWN_SECONDS,
            )
            .map_err(|error| error.to_string())?
        {
            WheelSpinClaim::MissingPlayer => {
                return Ok(spin_result(
                    "You need to /player register before you can spin the wheel.",
                ));
            }
            WheelSpinClaim::Cooldown { last_spin } => {
                return Ok(spin_result(format!(
                    "You already spun the wheel today! Try again <t:{}:R>.",
                    last_spin.saturating_add(cama_app::wheel::WHEEL_COOLDOWN_SECONDS)
                )));
            }
            WheelSpinClaim::Claimed { .. } => {}
        }
    }

    let raw_leaderboard = players
        .get_leaderboard(Some(guild_id), i64::from(i32::MAX), 0)
        .map_err(|error| error.to_string())?;
    let leaderboard = filter_visible_wheel_leaderboard(raw_leaderboard.clone(), visible_member_ids);
    let visible_positive_balance =
        visible_wheel_positive_balance(&raw_leaderboard, visible_member_ids);
    let top_ids = leaderboard
        .iter()
        .take(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
        .filter_map(|row| row.discord_id)
        .collect::<Vec<_>>();
    let kind = select_wheel_kind(user_id, player.jopacoin_balance, bonus_spin, &top_ids);
    let golden_announcement = (kind == WheelKind::Golden && !bonus_spin)
        .then(|| golden_wheel_announcement(user_id, &leaderboard));
    let policy = WheelEconomyPolicy {
        minigame_scale: config.minigame_jc_delta_scale,
        regular_target_ev: config.wheel_target_ev,
        bankrupt_target_ev: config.wheel_bankrupt_target_ev,
        golden_target_ev: config.wheel_golden_target_ev,
    };
    let effects = active_mana_effects(path, user_id, Some(guild_id), now)?;
    let mut wedges = if kind == WheelKind::Golden {
        let top_balances = leaderboard
            .iter()
            .take(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
            .filter_map(|row| {
                (row.discord_id != Some(user_id) && row.jopacoin_balance > 0)
                    .then_some(row.jopacoin_balance)
            })
            .collect::<Vec<_>>();
        let rank_next = leaderboard
            .get(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
            .filter(|row| row.jopacoin_balance > 0)
            .map(|row| row.jopacoin_balance);
        let total_positive = GoldenWheelRepository::new(path)
            .get_total_positive_balance(Some(guild_id))
            .map_err(|error| error.to_string())?;
        let bottom = GoldenWheelRepository::new(path)
            .get_leaderboard_bottom(Some(guild_id), 30, 1)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|row| row.discord_id != user_id)
            .filter(|row| {
                visible_member_ids.is_none_or(|member_ids| member_ids.contains(&row.discord_id))
            })
            .map(|row| row.balance)
            .collect::<Vec<_>>();
        compute_live_golden_wedges(
            LiveGoldenEconomy {
                spinner_balance: player.jopacoin_balance,
                other_top_balances: &top_balances,
                rank_next_balance: rank_next,
                total_positive_balance: total_positive,
                bottom_player_balances: &bottom,
            },
            policy,
        )
    } else {
        get_wheel_wedges(kind == WheelKind::Bankrupt, false, policy)
    };
    apply_mana_wheel_modifiers(&mut wedges, &effects, kind);
    if wedges.is_empty() {
        return Err("wheel has no wedges".to_owned());
    }
    // `fastrand` keeps the adapter dependency-free while preserving Python's
    // uniform random wedge selection. The persisted claim is the idempotency
    // boundary; a process restart cannot replay a claimed spin.
    let index = fastrand::usize(..wedges.len());
    let mut landed = wedges
        .get(index)
        .cloned()
        .ok_or_else(|| "wheel has no wedges".to_owned())?;
    let event_id = format!("{now}:{user_id}:{:016x}", fastrand::u64(..));

    if fastrand::f64() < cama_app::wheel::WHEEL_EXPLOSION_CHANCE {
        let explosion = resolve_explosion(
            bonus_spin,
            player.jopacoin_balance,
            event_win_multiplier,
            config.minigame_jc_delta_scale,
        );
        let mut receipt = credit_wheel_income(
            path,
            guild_id,
            user_id,
            player.jopacoin_balance,
            explosion.credited_reward,
            config,
            &event_id,
            "EXPLOSION",
            vanity_taxable_ids,
        )?;
        let (blood_pact_skim, balance_after) = match apply_wheel_blood_pact_skim(
            path,
            guild_id,
            user_id,
            player.jopacoin_balance,
            &event_id,
            now,
            config.minigame_jc_delta_scale,
        ) {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(%error, user_id, guild_id, "failed to apply wheel Blood Pact skim");
                (0, receipt.balance_after)
            }
        };
        receipt.balance_after = balance_after;
        receipt.net = receipt.net.saturating_sub(blood_pact_skim);
        receipt.balance_delta = receipt
            .balance_after
            .saturating_sub(player.jopacoin_balance);
        log_wheel_spin(
            path,
            user_id,
            guild_id,
            explosion.log_result,
            now,
            kind,
            bonus_spin,
            "EXPLOSION",
            &event_id,
            json!({
                "gross_reward": explosion.gross_reward,
                "credited_reward": explosion.credited_reward,
                "garnished_amount": receipt.garnished,
                "bankruptcy_penalty": receipt.bankruptcy_penalty,
                "vanity_tax": receipt.vanity_tax,
                "blood_pact_skim": blood_pact_skim,
                "event_win_multiplier": event_win_multiplier,
                "event_loss_multiplier": event_loss_multiplier,
            }),
        )?;
        record_betting_pet_activity(
            path,
            user_id,
            guild_id,
            PetActivity::WheelSpun,
            &format!("wheel:{event_id}"),
            now,
        );
        let blood_pact_note = if blood_pact_skim > 0 {
            format!("\n🩸 Blood Pact skimmed **{blood_pact_skim}** {JOPACOIN_EMOTE}.")
        } else {
            String::new()
        };
        let final_message = format!(
            "💥 **THE WHEEL EXPLODES!**\nYou received **{}** {}. Balance: **{}** {}.{blood_pact_note}",
            receipt.net, JOPACOIN_EMOTE, receipt.balance_after, JOPACOIN_EMOTE
        );
        let mut result = spin_result_with_attachment(
            "💥 **THE WHEEL EXPLODES!** The wheel is resolving...",
            render_explosion_attachment().ok(),
        );
        result.completion_message = Some(final_message);
        let next_spin_at =
            cooldown_after_resolution(now, bonus_spin, last_regular_spin, CooldownOutcome::Other)
                .next_spin_at;
        let mut explosion_embed = wheel_explosion_embed(
            receipt.balance_after,
            receipt
                .gross
                .saturating_sub(receipt.bankruptcy_penalty)
                .saturating_sub(receipt.vanity_tax),
            next_spin_at,
            receipt.garnished,
            receipt.bankruptcy_penalty,
            receipt.vanity_tax,
            blood_pact_skim,
        );
        if bonus_spin {
            explosion_embed = explosion_embed.field(
                "⛏️ Unearthed in the Mine",
                "Your pickaxe struck a buried wheel mechanism, waking a free normal wheel spin from the rock. Regular `/gamba` cooldown unchanged.",
                false,
            );
        }
        result.completion_embed = Some(explosion_embed);
        result.completion_delay = BETTING_EXPLOSION_DISPLAY_DELAY;
        return Ok(result);
    }

    let insurance_activated = if kind == WheelKind::Bankrupt
        && effects.color.as_deref() == Some("Green")
        && matches!(landed.value, WheelValue::Numeric(value) if value < 0)
    {
        ManaRepository::new(path)
            .claim_bankrupt_buff_atomic(user_id, Some(guild_id), BankruptBuff::Insurance)
            .map_err(|error| error.to_string())?
    } else {
        false
    };
    if insurance_activated {
        landed = cama_app::wheel::WheelWedge {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#4a4a4a",
        };
    }

    // Python sends a small Blue-mana preview for the bankrupt wheel before
    // the animation.  Sampling here consumes randomness only after the
    // landed wedge is fixed, so it cannot change the settlement outcome.
    let blue_bankrupt_preview = (kind == WheelKind::Bankrupt
        && effects.color.as_deref() == Some("Blue"))
    .then(|| wheel_bankrupt_preview(&wedges));

    if let Some((key, pending)) = pending_wheel_interaction(
        &wedges,
        &landed,
        kind,
        user_id,
        guild_id,
        player.jopacoin_balance,
        effects.clone(),
        event_id.clone(),
        config.clone(),
        event_win_multiplier,
        event_loss_multiplier,
        if kind == WheelKind::Bankrupt
            && effects.color.as_deref() == Some("Red")
            && matches!(
                landed.value,
                WheelValue::Numeric(0)
                    | WheelValue::Mechanic(WheelMechanic::ExtendOne | WheelMechanic::ExtendTwo)
            )
        {
            !ManaRepository::new(path)
                .is_bankrupt_buff_used(user_id, Some(guild_id), BankruptBuff::Reroll)
                .map_err(|error| error.to_string())?
        } else {
            false
        },
        bonus_spin,
        last_regular_spin,
    ) {
        let (key, mut pending) = (key, pending);
        let prompt = match pending.kind {
            WheelInteractionKind::TownTrial => {
                "⚖️ **TOWN TRIAL** — the town has five minutes to decide your fate."
            }
            WheelInteractionKind::Discover => {
                "🃏 **DISCOVER** — choose your fate before this view expires."
            }
            WheelInteractionKind::Scrying => {
                "🏝️ **MANA SCRYING** — Blue mana reveals two possible outcomes."
            }
            WheelInteractionKind::Reroll => {
                "🔴 **RE-ROLL AVAILABLE** — Red mana lets you choose whether to spin again."
            }
        };
        let prompt = blue_bankrupt_preview.as_deref().map_or_else(
            || prompt.to_owned(),
            |preview| format!("{prompt}\n\n{preview}"),
        );
        let attachment = render_wheel_attachment(&wedges, index, kind == WheelKind::Golden).ok();
        pending.initial_attachment = attachment.clone();
        return Ok(SpinResult {
            message: prompt,
            completion_message: None,
            completion_embed: None,
            completion_delay: Duration::ZERO,
            pending: Some((key, pending)),
            attachment,
            neon_wheel_result: None,
            neon_wheel_balance_before: None,
            neon_wheel_balance: None,
            neon_lightning: None,
            golden_announcement: None,
        });
    }

    let WheelResolution {
        resolved,
        logged,
        cooldown_outcome,
        extra_note,
        income,
        neon_lightning,
    } = resolve_wheel_value(
        path,
        guild_id,
        user_id,
        player.jopacoin_balance,
        &landed,
        kind,
        &effects,
        config,
        &event_id,
        vanity_taxable_ids,
        event_win_multiplier,
        event_loss_multiplier,
        bonus_spin,
        visible_positive_balance,
    )?;
    let next = cooldown_after_resolution(now, bonus_spin, last_regular_spin, cooldown_outcome);
    if let Some(replacement) = next.replacement_last_spin {
        players
            .set_last_wheel_spin(user_id, Some(guild_id), replacement)
            .map_err(|error| error.to_string())?;
    }
    let balance = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .map(|row| row.jopacoin_balance)
        .unwrap_or(player.jopacoin_balance);
    let (blood_pact_skim, balance) = match apply_wheel_blood_pact_skim(
        path,
        guild_id,
        user_id,
        player.jopacoin_balance,
        &event_id,
        now,
        config.minigame_jc_delta_scale,
    ) {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(%error, user_id, guild_id, "failed to apply wheel Blood Pact skim");
            (0, balance)
        }
    };
    let outcome_code = canonical_outcome_code(&landed, resolved, kind);
    log_wheel_spin(
        path,
        user_id,
        guild_id,
        logged,
        now,
        kind,
        bonus_spin,
        &outcome_code,
        &event_id,
        json!({
            "wedge_label": landed.label,
            "resolved_value": wheel_value_json(resolved),
            "next_spin_at": next.next_spin_at,
            "mana": effects.color,
            "mana_insurance_activated": insurance_activated,
            "blood_pact_skim": blood_pact_skim,
            "event_win_multiplier": event_win_multiplier,
            "event_loss_multiplier": event_loss_multiplier,
        }),
    )?;
    record_betting_pet_activity(
        path,
        user_id,
        guild_id,
        PetActivity::WheelSpun,
        &format!("wheel:{event_id}"),
        now,
    );
    let result_label = match resolved {
        WheelValue::Numeric(value) => value.to_string(),
        WheelValue::Mechanic(mechanic) => mechanic.code().to_owned(),
    };
    let loss_note = if cooldown_outcome == CooldownOutcome::LoseTurn {
        "\n⏳ Lose Turn: your next spin is delayed for five days."
    } else {
        ""
    };
    let blood_pact_note = if blood_pact_skim > 0 {
        format!("\n🩸 Blood Pact skimmed **{blood_pact_skim}** {JOPACOIN_EMOTE} from this payout.")
    } else {
        String::new()
    };
    let presentation_note = format!("{extra_note}{loss_note}{blood_pact_note}");
    let final_message = format!(
        "🎡 **{}**\nYou spun **{}** {}. Balance: **{}** {}.{extra_note}{loss_note}{blood_pact_note}",
        landed.label, result_label, JOPACOIN_EMOTE, balance, JOPACOIN_EMOTE
    );
    let spinning_message = blue_bankrupt_preview.as_deref().map_or_else(
        || "🎡 The wheel is spinning...".to_owned(),
        |preview| format!("🎡 The wheel is spinning...\n\n{preview}"),
    );
    let mut result = spin_result_with_attachment(
        spinning_message,
        render_wheel_attachment(&wedges, index, kind == WheelKind::Golden).ok(),
    );
    result.completion_message = Some(final_message);
    let mut result_embed = wheel_result_embed(
        &landed.label,
        resolved,
        kind,
        balance,
        next.next_spin_at,
        &presentation_note,
        income,
    );
    if bonus_spin {
        result_embed = result_embed.field(
            "⛏️ Unearthed in the Mine",
            "Your pickaxe struck a buried wheel mechanism, waking a free normal wheel spin from the rock. Regular `/gamba` cooldown unchanged.",
            false,
        );
    }
    result.completion_embed = Some(result_embed);
    result.completion_delay = Duration::from_secs(15);
    // Python evaluates Neon against the post-policy wedge.  This avoids
    // firing the bankrupt hook after a Comeback pardon changed a negative
    // wedge into LOSE/zero, and preserves the event-adjusted loss amount.
    if let WheelValue::Numeric(value) = resolved
        && value < 0
    {
        result.neon_wheel_result = Some(value);
        result.neon_wheel_balance_before = Some(player.jopacoin_balance);
        result.neon_wheel_balance = Some(balance);
    }
    result.neon_lightning = neon_lightning;
    result.golden_announcement = golden_announcement;
    Ok(result)
}

fn spin_result(message: impl Into<String>) -> SpinResult {
    spin_result_with_attachment(message, None)
}

fn filter_visible_wheel_leaderboard(
    players: Vec<Player>,
    visible_member_ids: Option<&BTreeSet<i64>>,
) -> Vec<Player> {
    visible_member_ids.map_or(players.clone(), |member_ids| {
        players
            .iter()
            .filter(|row| row.discord_id.is_some_and(|id| member_ids.contains(&id)))
            .cloned()
            .collect()
    })
}

fn visible_wheel_positive_balance(
    players: &[Player],
    visible_member_ids: Option<&BTreeSet<i64>>,
) -> Option<i64> {
    visible_member_ids.map(|member_ids| {
        players
            .iter()
            .filter(|row| {
                row.discord_id.is_some_and(|id| member_ids.contains(&id))
                    && row.jopacoin_balance > 0
            })
            .map(|row| row.jopacoin_balance)
            .sum()
    })
}

fn golden_wheel_announcement(user_id: i64, leaderboard: &[Player]) -> InteractionResponse {
    let top_lines = leaderboard
        .iter()
        .take(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
        .enumerate()
        .filter_map(|(index, row)| {
            row.discord_id.map(|discord_id| {
                format!(
                    "**#{}** <@{discord_id}> — {} {JOPACOIN_EMOTE}",
                    index + 1,
                    row.jopacoin_balance
                )
            })
        })
        .collect::<Vec<_>>();
    let mut mention_ids = leaderboard
        .iter()
        .take(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
        .filter_map(|row| row.discord_id)
        .filter_map(|id| u64::try_from(id).ok())
        .collect::<BTreeSet<_>>();
    if let Ok(user_id) = u64::try_from(user_id) {
        mention_ids.insert(user_id);
    }
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled("👑 GOLDEN WHEEL INCOMING 👑")
                .description(format!(
                    "👑 <@{user_id}> is spinning the GOLDEN WHEEL!\nThey are among the top {} wealthiest players in the server...\n\n**Current top-{}:**\n{}",
                    cama_app::wheel::WHEEL_GOLDEN_TOP_N,
                    cama_app::wheel::WHEEL_GOLDEN_TOP_N,
                    top_lines.join("\n")
                ))
                .color(0xffd700),
        )
        .with_user_mentions(mention_ids.into_iter().collect())
}

fn spin_result_with_attachment(
    message: impl Into<String>,
    attachment: Option<InteractionAttachment>,
) -> SpinResult {
    SpinResult {
        message: message.into(),
        completion_message: None,
        completion_embed: None,
        completion_delay: Duration::ZERO,
        pending: None,
        attachment,
        neon_wheel_result: None,
        neon_wheel_balance_before: None,
        neon_wheel_balance: None,
        neon_lightning: None,
        golden_announcement: None,
    }
}

fn wheel_completion_response(
    content: impl Into<String>,
    embed: Option<InteractionEmbed>,
    attachment: Option<InteractionAttachment>,
) -> InteractionResponse {
    let mut response = InteractionResponse::message(content);
    if let Some(embed) = embed {
        response = response.embed(embed);
    }
    if let Some(attachment) = attachment {
        response = response.attachment(attachment);
    }
    response
}

fn wheel_attachment_for_option(
    pending: &PendingWheelInteraction,
    option: &WheelOption,
) -> Option<InteractionAttachment> {
    let index = pending.wheel_wedges.iter().position(|wedge| {
        wedge.label == option.label && wedge.value == option.value && wedge.color == option.color
    })?;
    render_wheel_attachment(&pending.wheel_wedges, index, pending.golden).ok()
}

fn wheel_result_embed(
    landed_label: &str,
    resolved: WheelValue,
    kind: WheelKind,
    balance: i64,
    next_spin_at: i64,
    extra_note: &str,
    income: Option<cama_db::match_recording_repository::IncomeAwardReceipt>,
) -> InteractionEmbed {
    let (title, color, mut description) = match resolved {
        WheelValue::Numeric(value) if value > 0 => {
            if kind == WheelKind::Bankrupt && value == 1 {
                (
                    "🪙 One Coin. One.".to_owned(),
                    0x3a3a1a,
                    "**1**\n\nThe wheel took pity on you. One coin.\n\n*It's still technically a win.*".to_owned(),
                )
            } else if kind == WheelKind::Bankrupt && value == 2 {
                (
                    "🪙 Two Coins.".to_owned(),
                    0x3a3500,
                    "**2**\n\nEven charity has standards. Here's 2.\n\n*Don't spend it all in one place.*".to_owned(),
                )
            } else if kind == WheelKind::Golden && landed_label == "CROWN" {
                (
                    "👑 CROWN JEWEL! 👑".to_owned(),
                    0xffd700,
                    format!(
                        "**CROWN**\n\n✨ **+{value} {JOPACOIN_EMOTE} JACKPOT!** ✨\n\nThe golden wheel's ultimate prize.\nThe crown jewel of Jopacoin fortune.\n\n*The server weeps. You reign.*"
                    ),
                )
            } else if value == 100 {
                (
                    "🌟 JACKPOT! 🌟".to_owned(),
                    0xffd700,
                    format!("**{landed_label}**\n\nYou won **{value} {JOPACOIN_EMOTE}!**"),
                )
            } else if kind == WheelKind::Golden {
                (
                    "👑 Golden Win!".to_owned(),
                    0xdaa520,
                    format!(
                        "**+{value} JC**\n\nYou won **{value} {JOPACOIN_EMOTE} from the Golden Wheel!**"
                    ),
                )
            } else {
                (
                    "🎉 Winner!".to_owned(),
                    0x2ecc71,
                    format!("**+{value} JC**\n\nYou won **{value} {JOPACOIN_EMOTE}!**"),
                )
            }
        }
        WheelValue::Numeric(value) if value < 0 => {
            if kind == WheelKind::Golden {
                (
                    "📉 OVEREXTENDED! 📉".to_owned(),
                    0x4a3000,
                    format!(
                        "**OVEREXTENDED**\n\nYou flew too close to the sun.\n\nLost **{} {}**.\n\n*Pride goes before the fall.*",
                        value.saturating_abs(),
                        JOPACOIN_EMOTE
                    ),
                )
            } else {
                (
                    "💀 BANKRUPT! 💀".to_owned(),
                    0xed4245,
                    format!(
                        "**{landed_label}**\n\nYou lost **{} {}**!\n\n*The wheel shows no mercy...*",
                        value.saturating_abs(),
                        JOPACOIN_EMOTE
                    ),
                )
            }
        }
        WheelValue::Numeric(0) => (
            "🚫 LOSE A TURN 🚫".to_owned(),
            0x555555,
            format!(
                "**{landed_label}**\n\nNo jopacoin lost... but you just got **5-day timeout'd** from the wheel.\n\n*Imagine being this unlucky. Go outside. Touch grass. Reflect on your gambling addiction.*"
            ),
        ),
        WheelValue::Numeric(_) => unreachable!("wheel numeric value was not classified"),
        WheelValue::Mechanic(mechanic) => wheel_mechanic_embed_copy(mechanic, landed_label),
    };
    if !extra_note.trim().is_empty() {
        description.push_str("\n\n");
        description.push_str(extra_note.trim());
    }
    let mut embed = InteractionEmbed::titled(title)
        .description(description)
        .color(color)
        .field(
            "New Balance",
            format!("**{balance}** {JOPACOIN_EMOTE}"),
            false,
        )
        .field("Next Spin", format!("<t:{next_spin_at}:R>"), false);
    if let Some(receipt) = income {
        if receipt.bankruptcy_penalty > 0 {
            embed = embed.field(
                "Bankruptcy Penalty",
                format!(
                    "−{} {} withheld while bankrupt",
                    receipt.bankruptcy_penalty, JOPACOIN_EMOTE
                ),
                false,
            );
        }
        if receipt.vanity_tax > 0 {
            embed = embed.field(
                "Vanity Tax",
                format!("−{} {} vanity tax", receipt.vanity_tax, JOPACOIN_EMOTE),
                false,
            );
        }
    }
    embed
}

fn wheel_explosion_embed(
    balance: i64,
    reward: i64,
    next_spin_at: i64,
    garnished: i64,
    bankruptcy_penalty: i64,
    vanity_tax: i64,
    blood_pact_skim: i64,
) -> InteractionEmbed {
    let mut description = format!(
        "**KABOOM!**\n\nThe wheel has exploded! Fortunately, no one was hurt.\n\nWe sincerely apologize for the inconvenience. As compensation, you've been awarded **{reward} {JOPACOIN_EMOTE}**."
    );
    if garnished > 0 {
        description.push_str(&format!(
            "\n\n*{garnished} {JOPACOIN_EMOTE} went to debt repayment.*"
        ));
    }
    let mut embed = InteractionEmbed::titled("💥 THE WHEEL EXPLODED! 💥")
        .description(description)
        .color(0xffa500);
    if bankruptcy_penalty > 0 {
        embed = embed.field(
            "Bankruptcy Penalty",
            format!("−{bankruptcy_penalty} {JOPACOIN_EMOTE} withheld while bankrupt"),
            false,
        );
    }
    if vanity_tax > 0 {
        embed = embed.field(
            "Vanity Tax",
            format!("−{vanity_tax} {JOPACOIN_EMOTE} vanity tax"),
            false,
        );
    }
    if blood_pact_skim > 0 {
        embed = embed.field(
            "Blood Pact",
            format!("−{blood_pact_skim} {JOPACOIN_EMOTE} skimmed by Blood Pact"),
            false,
        );
    }
    embed
        .field(
            "New Balance",
            format!("**{balance}** {JOPACOIN_EMOTE}"),
            false,
        )
        .field("Next Spin", format!("<t:{next_spin_at}:R>"), false)
        .footer("Our engineers are working on a replacement wheel.")
}

fn wheel_mechanic_embed_copy(mechanic: WheelMechanic, landed_label: &str) -> (String, u32, String) {
    match mechanic {
        WheelMechanic::Jailbreak => (
            "🔓 JAILBREAK! 🔓".to_owned(),
            0x0a2a0a,
            "**JAIL**\n\nYou found a crack in the cell wall.\n\n**−1 penalty game** removed!\n\n*Don't celebrate yet. You're still in here.*".to_owned(),
        ),
        WheelMechanic::ChainReaction => (
            "⛓️ CHAIN REACTION! ⛓️".to_owned(),
            0x1a1a3a,
            "**CHAIN**\n\n⛓️ The chain reaches back... and copies the previous ordinary spin.\n\n*Their luck became yours.*".to_owned(),
        ),
        WheelMechanic::Emergency => (
            "🚨 EMERGENCY! 🚨".to_owned(),
            0x2a1a00,
            format!("**SOS**\n\n🚨 Economic crisis triggered!\n\n**{landed_label}** drains the wealthy into the Reserve.\n\n*No one is safe. Not even you.*"),
        ),
        WheelMechanic::Commune => (
            "🫳 SEIZE THE MEANS! 🫳".to_owned(),
            0x1a2a1a,
            "**SEIZE**\n\nPlayers donate to the collective and you receive the proceeds.\n\n*From each according to their balance, to you.*".to_owned(),
        ),
        WheelMechanic::Comeback => (
            "🃏 CLUTCH SAVE! 🃏".to_owned(),
            0x0a1a2a,
            "**CLUTCH**\n\nFortune smiles — once. Your next BANKRUPT will be converted to a LOSE instead. *Don't waste it.*".to_owned(),
        ),
        WheelMechanic::ExtendOne | WheelMechanic::ExtendTwo => (
            "⛓️ PENALTY EXTENDED! ⛓️".to_owned(),
            0x8b0000,
            format!("**{landed_label}**\n\nYour bankruptcy penalty has been extended.\n\n*The wheel remembers your sins... keep winning to escape!*"),
        ),
        WheelMechanic::RedShell => (
            "🔴 RED SHELL HIT! 🔴".to_owned(),
            0xed4245,
            "**RED**\n\n💥 Red Shell locked onto a player ahead.\n\n*The track has a new target.*".to_owned(),
        ),
        WheelMechanic::BlueShell => (
            "🔵 BLUE SHELL STRIKE! 🔵".to_owned(),
            0x3498db,
            "**BLUE**\n\n💥 Blue Shell targets the leader.\n\n*The price of being on top.*".to_owned(),
        ),
        WheelMechanic::LightningBolt => (
            "⚡ LIGHTNING BOLT! ⚡".to_owned(),
            0xf39c12,
            "**BOLT**\n\nLightning strikes the entire server and sends the proceeds to the Jopacoin Reserve.\n\n*No one is safe.*".to_owned(),
        ),
        WheelMechanic::BananaPeel => (
            "🍌 BANANA PEEL! 🍌".to_owned(),
            0xffe14a,
            "**BANANA**\n\n🍌 A player behind you slipped on your peel.\n\n*Stay behind a thrower at your own peril.*".to_owned(),
        ),
        WheelMechanic::GreenShell => (
            "🟢 GREEN SHELL! 🟢".to_owned(),
            0x228b22,
            "**GREEN**\n\n🐢 You launched a green shell down the track.\n\n*Direct hit. Pocket the coins.*".to_owned(),
        ),
        WheelMechanic::BombOmb => (
            "💣 BOMB-OMB! 💣".to_owned(),
            0x0d0d0d,
            "**BOMB**\n\n💣 KABOOM! The bomb-omb detonates into the crowd.\n\n*You walked away unscathed. Lucky.*".to_owned(),
        ),
        WheelMechanic::Heist => (
            "🥇 HEIST! 🥇".to_owned(),
            0x7a5c00,
            "**HEIST**\n\n💰 You robbed the bottom of the ladder.\n\n*Crime pays — when you're already on top.*".to_owned(),
        ),
        WheelMechanic::MarketCrash => (
            "📉 MARKET CRASH! 📉".to_owned(),
            0x8a4000,
            "**CRASH**\n\n📉 You taxed fellow top players.\n\n*The rich get richer. That's just economics.*".to_owned(),
        ),
        WheelMechanic::CompoundInterest => (
            "📈 BONUS! 📈".to_owned(),
            0x6b8c00,
            "**COMPOUND**\n\n📈 A flat bonus goes straight to your balance.\n\n*The most boring way to get rich — and the most reliable.*".to_owned(),
        ),
        WheelMechanic::TrickleDown => (
            "💧 TRICKLE DOWN! 💧".to_owned(),
            0x5c7a00,
            "**TRICKLE**\n\n💧 You taxed the wealthy.\n\n*It trickled up, actually.*".to_owned(),
        ),
        WheelMechanic::Dividend => (
            "💎 DIVIDEND! 💎".to_owned(),
            0x4a7000,
            "**DIVIDEND**\n\n💎 The server's collective wealth pays out.\n\n*Being rich in a rich server pays dividends. Literally.*".to_owned(),
        ),
        WheelMechanic::HostileTakeover => (
            "🏴 HOSTILE TAKEOVER! 🏴".to_owned(),
            0x6a2a80,
            "**TAKEOVER**\n\n🏴 A corporate raid targets rank #4.\n\n*So close to the top — and yet, so far.*".to_owned(),
        ),
        WheelMechanic::Recession => (
            "🩸 RECESSION! 🩸".to_owned(),
            0x3a0a0a,
            "**RECESSION**\n\n📉 The economy contracts and wealth bleeds into the Jopacoin Reserve.\n\n*Pride goes before the fall — and it drags the rest down with it.*".to_owned(),
        ),
        WheelMechanic::Eruption => (
            "⛰️🔥 ERUPTION!".to_owned(),
            0xff4500,
            "**ERUPTION**\n\nThe Mountain erupts! You gain **2x** the last spinner's result.\n\n*Red mana burns bright.*".to_owned(),
        ),
        WheelMechanic::Overgrowth => (
            "🌲🌿 OVERGROWTH!".to_owned(),
            0x228b22,
            "**OVERGROWTH**\n\nThe Forest rewards consistency. You earn 10 JC per game played this week.\n\n*Slow and steady wins the race.*".to_owned(),
        ),
        WheelMechanic::Decay => (
            "🌿💀 DECAY!".to_owned(),
            0x4b0082,
            "**DECAY**\n\nRot spreads to the wealthy. The Swamp claims what it is owed.\n\n*Nothing stays green forever.*".to_owned(),
        ),
        WheelMechanic::TownTrial | WheelMechanic::Discover => (
            "⚖️ INTERACTIVE WHEEL".to_owned(),
            0x2a1a1a,
            format!("**{landed_label}**\n\nThe interactive wheel outcome has been resolved."),
        ),
    }
}

fn wheel_bankrupt_preview(wedges: &[cama_app::wheel::WheelWedge]) -> String {
    if wedges.is_empty() {
        return "🔮 Blue mana reveals: no wedges available".to_owned();
    }
    let sample_count = wedges.len().min(3);
    let mut indices = Vec::with_capacity(sample_count);
    while indices.len() < sample_count {
        let index = fastrand::usize(..wedges.len());
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    let preview = indices
        .into_iter()
        .filter_map(|index| wedges.get(index))
        .map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value > 0 => format!("{value} JC"),
            _ => wedge.label.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("🔮 Blue mana reveals: {preview}")
}

fn wheel_option(wedge: &cama_app::wheel::WheelWedge) -> WheelOption {
    WheelOption {
        label: wedge.label.clone(),
        value: wedge.value,
        color: wedge.color,
    }
}

fn wheel_display_label(option: &WheelOption) -> String {
    match option.value {
        WheelValue::Numeric(value) if value > 0 => format!("{} JC", option.label),
        _ => option.label.clone(),
    }
}

#[cfg(test)]
fn apply_event_multiplier_to_wedge(
    wedge: &mut cama_app::wheel::WheelWedge,
    win_multiplier: f64,
    loss_multiplier: f64,
) {
    let WheelValue::Numeric(value) = wedge.value else {
        return;
    };
    if value == 0 {
        return;
    }
    let adjusted = apply_gamba_event_multiplier(value, win_multiplier, loss_multiplier);
    wedge.value = WheelValue::Numeric(adjusted);
    wedge.label = adjusted.to_string();
}

#[allow(clippy::too_many_arguments)]
fn pending_wheel_interaction(
    wedges: &[cama_app::wheel::WheelWedge],
    landed: &cama_app::wheel::WheelWedge,
    kind: WheelKind,
    user_id: i64,
    guild_id: i64,
    balance_before: i64,
    effects: ManaEffects,
    event_id: String,
    config: BettingRuntimeConfig,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    reroll_available: bool,
    bonus_spin: bool,
    last_regular_spin: Option<i64>,
) -> Option<(String, PendingWheelInteraction)> {
    let mut options = Vec::new();
    let interaction_kind = if kind == WheelKind::Regular
        && effects.color.as_deref() == Some("Blue")
        && effects.blue_gamba_scrying
    {
        let first = fastrand::usize(..wedges.len());
        let mut second = fastrand::usize(..wedges.len());
        while second == first && wedges.len() > 1 {
            second = fastrand::usize(..wedges.len());
        }
        options.push(wheel_option(&wedges[first]));
        options.push(wheel_option(&wedges[second]));
        Some(WheelInteractionKind::Scrying)
    } else if kind == WheelKind::Bankrupt
        && matches!(landed.value, WheelValue::Mechanic(WheelMechanic::TownTrial))
    {
        options = wedges
            .iter()
            .filter(|wedge| {
                !matches!(
                    wedge.value,
                    WheelValue::Mechanic(
                        WheelMechanic::TownTrial
                            | WheelMechanic::Discover
                            | WheelMechanic::ChainReaction
                    )
                )
            })
            .map(wheel_option)
            .collect();
        fastrand::shuffle(&mut options);
        options.truncate(3);
        Some(WheelInteractionKind::TownTrial)
    } else if kind == WheelKind::Bankrupt
        && matches!(landed.value, WheelValue::Mechanic(WheelMechanic::Discover))
    {
        options = wedges
            .iter()
            .filter(|wedge| {
                !matches!(
                    wedge.value,
                    WheelValue::Mechanic(
                        WheelMechanic::TownTrial
                            | WheelMechanic::Discover
                            | WheelMechanic::ChainReaction
                    )
                )
            })
            .map(wheel_option)
            .collect();
        fastrand::shuffle(&mut options);
        options.truncate(3);
        Some(WheelInteractionKind::Discover)
    } else if kind == WheelKind::Bankrupt
        && reroll_available
        && effects.color.as_deref() == Some("Red")
        && matches!(
            landed.value,
            WheelValue::Numeric(0)
                | WheelValue::Mechanic(WheelMechanic::ExtendOne | WheelMechanic::ExtendTwo)
        )
    {
        options.push(wheel_option(landed));
        let candidates = wedges
            .iter()
            .filter(|wedge| wedge.value != landed.value)
            .collect::<Vec<_>>();
        let replacement = candidates
            .get(fastrand::usize(..candidates.len().max(1)))
            .copied()
            .unwrap_or(landed);
        options.push(wheel_option(replacement));
        Some(WheelInteractionKind::Reroll)
    } else {
        None
    }?;
    let valid = match interaction_kind {
        WheelInteractionKind::Scrying | WheelInteractionKind::Reroll => options.len() == 2,
        WheelInteractionKind::TownTrial | WheelInteractionKind::Discover => !options.is_empty(),
    };
    if !valid {
        return None;
    }
    let key = match interaction_kind {
        WheelInteractionKind::TownTrial => format!("tt_{event_id}"),
        WheelInteractionKind::Discover => format!("disc_{event_id}"),
        WheelInteractionKind::Scrying => format!("betting:scry:{event_id}"),
        WheelInteractionKind::Reroll => format!("betting:reroll:{event_id}"),
    };
    Some((
        key,
        PendingWheelInteraction {
            kind: interaction_kind,
            user_id,
            guild_id,
            created_at: Instant::now(),
            options,
            wheel_wedges: wedges.to_vec(),
            golden: kind == WheelKind::Golden,
            initial_attachment: None,
            votes: BTreeMap::new(),
            spin_kind: kind,
            balance_before,
            effects,
            event_id,
            config,
            event_win_multiplier,
            event_loss_multiplier,
            bonus_spin,
            last_regular_spin,
            original_responder: None,
        },
    ))
}

fn parse_wheel_component(custom_id: &str) -> Option<WheelComponentAction> {
    if let Some(rest) = custom_id.strip_prefix("betting:scry:") {
        let (token, choice) = rest.rsplit_once(':')?;
        let index = match choice {
            "a" | "A" => 0,
            "b" | "B" => 1,
            _ => return None,
        };
        return Some(WheelComponentAction::Choose {
            key: Some(format!("betting:scry:{token}")),
            index,
        });
    }
    if let Some(rest) = custom_id.strip_prefix("betting:reroll:") {
        let (token, choice) = rest.rsplit_once(':')?;
        let reroll = match choice {
            "yes" => true,
            "no" => false,
            _ => return None,
        };
        return Some(WheelComponentAction::Reroll {
            key: Some(format!("betting:reroll:{token}")),
            reroll,
        });
    }
    if let Some(rest) = custom_id.strip_prefix(TOWN_TRIAL_COMPONENT_PREFIX) {
        let (token, index) = rest
            .rsplit_once(':')
            .map_or((None, rest), |(token, index)| (Some(token), index));
        return Some(WheelComponentAction::Vote {
            key: token.map(|token| format!("tt_{token}")),
            index: index.parse().ok()?,
        });
    }
    if let Some(rest) = custom_id.strip_prefix(DISCOVER_COMPONENT_PREFIX) {
        let (token, index) = rest
            .rsplit_once(':')
            .map_or((None, rest), |(token, index)| (Some(token), index));
        return Some(WheelComponentAction::Choose {
            key: token.map(|token| format!("disc_{token}")),
            index: index.parse().ok()?,
        });
    }
    None
}

fn wheel_interaction_rows(
    key: &str,
    pending: &PendingWheelInteraction,
) -> Vec<InteractionActionRow> {
    let buttons = match pending.kind {
        WheelInteractionKind::TownTrial | WheelInteractionKind::Discover => pending
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let custom_id = format!("{key}:{index}");
                InteractionButton::new(custom_id, wheel_display_label(option))
                    .style(InteractionButtonStyle::Secondary)
            })
            .collect::<Vec<_>>(),
        WheelInteractionKind::Scrying => pending
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let choice = if index == 0 { "a" } else { "b" };
                let custom_id = format!("{key}:{choice}");
                InteractionButton::new(
                    custom_id,
                    format!(
                        "{}: {}",
                        choice.to_ascii_uppercase(),
                        wheel_display_label(option)
                    ),
                )
                .style(InteractionButtonStyle::Primary)
            })
            .collect::<Vec<_>>(),
        WheelInteractionKind::Reroll => vec![
            InteractionButton::new(format!("{key}:no"), "Keep result")
                .style(InteractionButtonStyle::Secondary),
            InteractionButton::new(format!("{key}:yes"), "Re-roll")
                .style(InteractionButtonStyle::Danger),
        ],
    };
    vec![InteractionActionRow::buttons(buttons)]
}

#[allow(clippy::too_many_arguments)]
fn resolve_pending_wheel(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    balance_before: i64,
    kind: WheelKind,
    effects: &ManaEffects,
    config: &BettingRuntimeConfig,
    event_id: &str,
    option: &WheelOption,
    now: i64,
    use_reroll: bool,
    bonus_spin: bool,
    last_regular_spin: Option<i64>,
    event_win_multiplier: f64,
    event_loss_multiplier: f64,
    vanity_taxable_ids: &BTreeSet<i64>,
) -> Result<PendingWheelResolution, String> {
    if use_reroll
        && !ManaRepository::new(path)
            .claim_bankrupt_buff_atomic(user_id, Some(guild_id), BankruptBuff::Reroll)
            .map_err(|error| error.to_string())?
    {
        return Err("Your Red mana re-roll has already been used today.".to_owned());
    }
    let landed = cama_app::wheel::WheelWedge {
        label: option.label.clone(),
        value: option.value,
        color: option.color,
    };
    let players = PlayerRepository::new(path);
    let outcome_balance_before = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .map(|row| row.jopacoin_balance)
        .unwrap_or(balance_before);
    let WheelResolution {
        resolved,
        logged,
        cooldown_outcome,
        extra_note,
        income,
        neon_lightning,
    } = resolve_wheel_value(
        path,
        guild_id,
        user_id,
        outcome_balance_before,
        &landed,
        kind,
        effects,
        config,
        event_id,
        vanity_taxable_ids,
        event_win_multiplier,
        event_loss_multiplier,
        bonus_spin,
        None,
    )?;
    let next = cooldown_after_resolution(now, bonus_spin, last_regular_spin, cooldown_outcome);
    if let Some(replacement) = next.replacement_last_spin {
        players
            .set_last_wheel_spin(user_id, Some(guild_id), replacement)
            .map_err(|error| error.to_string())?;
    }
    let balance = players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .map(|row| row.jopacoin_balance)
        .unwrap_or(outcome_balance_before);
    let (blood_pact_skim, balance) = match apply_wheel_blood_pact_skim(
        path,
        guild_id,
        user_id,
        outcome_balance_before,
        event_id,
        now,
        config.minigame_jc_delta_scale,
    ) {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(%error, user_id, guild_id, "failed to apply interactive wheel Blood Pact skim");
            (0, balance)
        }
    };
    let outcome_code = canonical_outcome_code(&landed, resolved, kind);
    log_wheel_spin(
        path,
        user_id,
        guild_id,
        logged,
        now,
        kind,
        bonus_spin,
        &outcome_code,
        event_id,
        json!({
            "interactive": true,
            "wedge_label": landed.label,
            "resolved_value": wheel_value_json(resolved),
            "next_spin_at": next.next_spin_at,
            "mana": effects.color,
            "event_win_multiplier": event_win_multiplier,
            "event_loss_multiplier": event_loss_multiplier,
            "blood_pact_skim": blood_pact_skim,
        }),
    )?;
    record_betting_pet_activity(
        path,
        user_id,
        guild_id,
        PetActivity::WheelSpun,
        &format!("wheel:{event_id}"),
        now,
    );
    let result_label = match resolved {
        WheelValue::Numeric(value) => value.to_string(),
        WheelValue::Mechanic(mechanic) => mechanic.code().to_owned(),
    };
    let loss_note = if cooldown_outcome == CooldownOutcome::LoseTurn {
        "\n⏳ Lose Turn: your next spin is delayed for five days."
    } else {
        ""
    };
    let blood_pact_note = if blood_pact_skim > 0 {
        format!("\n🩸 Blood Pact skimmed **{blood_pact_skim}** {JOPACOIN_EMOTE} from this payout.")
    } else {
        String::new()
    };
    let presentation_note = format!("{extra_note}{loss_note}{blood_pact_note}");
    let mut completion_embed = wheel_result_embed(
        &landed.label,
        resolved,
        kind,
        balance,
        next.next_spin_at,
        &presentation_note,
        income,
    );
    if bonus_spin {
        completion_embed = completion_embed.field(
            "⛏️ Unearthed in the Mine",
            "Your pickaxe struck a buried wheel mechanism, waking a free normal wheel spin from the rock. Regular `/gamba` cooldown unchanged.",
            false,
        );
    }
    Ok(PendingWheelResolution {
        message: format!(
            "🎡 **{}**\nYou spun **{}** {}. Balance: **{}** {}.{extra_note}{loss_note}{blood_pact_note}",
            landed.label, result_label, JOPACOIN_EMOTE, balance, JOPACOIN_EMOTE
        ),
        completion_embed,
        neon_wheel_result: match resolved {
            WheelValue::Numeric(value) if value < 0 => Some(value),
            _ => None,
        },
        neon_wheel_balance_before: match resolved {
            WheelValue::Numeric(value) if value < 0 => Some(outcome_balance_before),
            _ => None,
        },
        neon_wheel_balance: match resolved {
            WheelValue::Numeric(value) if value < 0 => Some(balance),
            _ => None,
        },
        neon_lightning,
    })
}

const fn is_valid_leverage_tier(leverage: i64) -> bool {
    matches!(leverage, 1 | 2 | 3 | 5 | 10)
}

fn validate_investment_target(
    investor_id: i64,
    target_id: i64,
    direction: &str,
    investor_registered: bool,
    target_registered: bool,
) -> Result<(), String> {
    if !investor_registered {
        return Err("You need to /player register before configuring investments.".to_owned());
    }
    if !target_registered {
        return Err(format!("<@{target_id}> is not registered."));
    }
    if investor_id == target_id && direction == "short" {
        return Err("You cannot short yourself.".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cama_db::core_repositories::NewPlayer;
    use cama_db::match_runtime::{PendingMatchRepository, PendingMatchState};
    use cama_db::schema_manager::initialize_or_migrate;
    use rusqlite::Connection;
    use std::io::Cursor;
    use tempfile::NamedTempFile;

    #[derive(Default)]
    struct CompositionResponder;

    #[derive(Clone, Default)]
    struct WagerRefreshProbe {
        calls: Arc<Mutex<Vec<(i64, i64)>>>,
    }

    #[test]
    fn match_wager_refresh_port_is_exposed_for_composition() {
        let constructor: fn(MatchRegistrationProvider) -> Arc<dyn BettingWagerRefreshPort> =
            match_wager_refresh_port;
        let _ = constructor;
    }

    #[test]
    fn match_post_match_debrief_port_is_exposed_for_composition() {
        let constructor: fn(BettingRegistrationProvider) -> Arc<dyn MatchPostMatchDebriefPort> =
            match_post_match_debrief_port;
        let _ = constructor;
    }

    #[test]
    fn invalid_leverage_tier_is_rejected_before_repository_work() {
        assert!(is_valid_leverage_tier(1));
        assert!(is_valid_leverage_tier(10));
        assert!(!is_valid_leverage_tier(4));
        assert!(!is_valid_leverage_tier(0));
    }

    #[test]
    fn investment_target_validation_rejects_self_short_and_unregistered_targets() {
        assert_eq!(
            validate_investment_target(1, 1, "short", true, true),
            Err("You cannot short yourself.".to_owned())
        );
        assert_eq!(
            validate_investment_target(1, 999, "long", true, false),
            Err("<@999> is not registered.".to_owned())
        );
    }

    fn invest_subcommand(action: &str, children: Vec<InteractionOption>) -> Vec<InteractionOption> {
        vec![InteractionOption {
            name: "invest".to_owned(),
            value: InteractionValue::Subcommand(
                std::iter::once(InteractionOption {
                    name: "action".to_owned(),
                    value: InteractionValue::String(action.to_owned()),
                })
                .chain(children)
                .collect(),
            ),
        }]
    }

    #[tokio::test]
    async fn economy_invest_command_sets_lists_and_removes_positions() {
        let database = NamedTempFile::new().expect("investment command database");
        initialize_or_migrate(database.path()).expect("investment command schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(1, "investor", Some(42)))
            .expect("investor");
        players
            .add(&NewPlayer::new(2, "target", Some(42)))
            .expect("target");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("investment command configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let recording = RecordingResponder::default();
        let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
        provider
            .handler
            .invest(
                1,
                Some(42),
                &invest_subcommand(
                    "set",
                    vec![
                        InteractionOption {
                            name: "player".to_owned(),
                            value: InteractionValue::User {
                                id: 2,
                                display_name: Some("target".to_owned()),
                                is_bot: Some(false),
                            },
                        },
                        InteractionOption {
                            name: "direction".to_owned(),
                            value: InteractionValue::String("long".to_owned()),
                        },
                        InteractionOption {
                            name: "percentage".to_owned(),
                            value: InteractionValue::Integer(7),
                        },
                    ],
                ),
                &responder,
            )
            .await
            .expect("set investment");
        assert!(
            recording.responses.lock().unwrap()[0]
                .content
                .contains("Configured total: **7% / 50%**")
        );
        recording.responses.lock().unwrap().clear();

        provider
            .handler
            .invest(
                1,
                Some(42),
                &invest_subcommand("list", Vec::new()),
                &responder,
            )
            .await
            .expect("list investment");
        assert!(
            recording.responses.lock().unwrap()[0]
                .content
                .contains("1. LONG <@2> — 7%")
        );
        recording.responses.lock().unwrap().clear();

        provider
            .handler
            .invest(
                1,
                Some(42),
                &invest_subcommand(
                    "remove",
                    vec![InteractionOption {
                        name: "player".to_owned(),
                        value: InteractionValue::User {
                            id: 2,
                            display_name: None,
                            is_bot: None,
                        },
                    }],
                ),
                &responder,
            )
            .await
            .expect("remove investment");
        assert!(
            recording.responses.lock().unwrap()[0]
                .content
                .contains("Removed your investment in <@2>.")
        );
        assert!(
            AutobetInvestmentRepository::new(database.path())
                .list(Some(42), 1)
                .expect("investment list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn economy_invest_position_removal_survives_departed_target() {
        let database = NamedTempFile::new().expect("departed investment database");
        initialize_or_migrate(database.path()).expect("departed investment schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(1, "investor", Some(42)))
            .expect("departed investor");
        players
            .add(&NewPlayer::new(2, "departed", Some(42)))
            .expect("departed target");
        AutobetInvestmentRepository::new(database.path())
            .set(Some(42), 1, 2, "long", 7)
            .expect("departed target position");
        // A departed Discord member remains a durable player row; only the
        // gateway member snapshot is absent. Removing the row would violate
        // legacy SQLite foreign-key shapes, and is not Python's behavior.
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("departed configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let recording = RecordingResponder::default();
        let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
        provider
            .handler
            .invest(
                1,
                Some(42),
                &invest_subcommand(
                    "remove",
                    vec![InteractionOption {
                        name: "position".to_owned(),
                        value: InteractionValue::Integer(1),
                    }],
                ),
                &responder,
            )
            .await
            .expect("remove departed position");
        assert!(
            recording.responses.lock().unwrap()[0]
                .content
                .contains("Removed your investment in <@2>.")
        );
        assert!(
            AutobetInvestmentRepository::new(database.path())
                .list(Some(42), 1)
                .expect("departed position list")
                .is_empty()
        );
    }

    #[test]
    fn economy_invest_rate_limit_is_five_calls_per_ten_seconds() {
        let database = NamedTempFile::new().expect("investment rate database");
        initialize_or_migrate(database.path()).expect("investment rate schema");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("investment rate configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        for _ in 0..5 {
            assert!(
                provider
                    .handler
                    .take_rate("invest", 42, 1, 5, DEFAULT_RATE_WINDOW)
                    .expect("rate check")
            );
        }
        assert!(
            !provider
                .handler
                .take_rate("invest", 42, 1, 5, DEFAULT_RATE_WINDOW)
                .expect("rate rejection")
        );
        assert!(
            provider
                .handler
                .take_rate("invest", 43, 1, 5, DEFAULT_RATE_WINDOW)
                .expect("guild-scoped rate check")
        );
    }

    #[tokio::test]
    async fn economy_invest_rate_limit_returns_ephemeral_retry_copy() {
        let database = NamedTempFile::new().expect("investment retry database");
        initialize_or_migrate(database.path()).expect("investment retry schema");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("investment retry configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let recording = RecordingResponder::default();
        let responder: Arc<dyn InteractionResponder> = Arc::new(recording.clone());
        for _ in 0..5 {
            provider
                .handler
                .invest(
                    1,
                    Some(42),
                    &invest_subcommand("list", Vec::new()),
                    &responder,
                )
                .await
                .expect("rate-limited invest response");
        }
        {
            let mut rates = provider.handler.rates.lock().expect("investment rate lock");
            rates.insert(
                ("invest".to_owned(), 42, 1),
                vec![Instant::now() - Duration::from_millis(5_800); 5],
            );
        }
        provider
            .handler
            .invest(
                1,
                Some(42),
                &invest_subcommand("list", Vec::new()),
                &responder,
            )
            .await
            .expect("rate-limited invest response");
        let responses = recording.responses.lock().unwrap();
        let last = responses.last().expect("retry response");
        assert!(last.ephemeral);
        assert_eq!(
            last.content,
            "Slow down! Try configuring investments again in 4s."
        );
    }

    #[test]
    fn economy_invest_retry_after_truncates_four_point_two_to_four_seconds() {
        assert_eq!(
            retry_after_seconds(DEFAULT_RATE_WINDOW, Duration::from_millis(5_800)),
            4
        );
    }

    #[test]
    fn golden_wheel_member_snapshot_removes_departed_wealth_before_rank_selection() {
        let mut departed = Player::new("departed");
        departed.discord_id = Some(99);
        departed.jopacoin_balance = 10_000;
        let mut first = Player::new("first");
        first.discord_id = Some(1);
        first.jopacoin_balance = 900;
        let mut second = Player::new("second");
        second.discord_id = Some(2);
        second.jopacoin_balance = 800;
        let mut third = Player::new("third");
        third.discord_id = Some(3);
        third.jopacoin_balance = 700;
        let raw = vec![departed, first, second, third];
        let visible_ids = BTreeSet::from([1, 2, 3]);
        let visible = filter_visible_wheel_leaderboard(raw, Some(&visible_ids));
        assert_eq!(
            visible
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn golden_wheel_member_snapshot_preserves_raw_order_when_unavailable() {
        let mut departed = Player::new("departed");
        departed.discord_id = Some(99);
        departed.jopacoin_balance = 10_000;
        let mut visible = Player::new("visible");
        visible.discord_id = Some(1);
        visible.jopacoin_balance = 900;
        let raw = vec![departed, visible];
        let result = filter_visible_wheel_leaderboard(raw, None);
        assert_eq!(result[0].discord_id, Some(99));
    }

    #[test]
    fn golden_wheel_dividend_uses_visible_positive_wealth_snapshot() {
        let mut departed = Player::new("departed");
        departed.discord_id = Some(99);
        departed.jopacoin_balance = 100_000;
        let mut first = Player::new("first");
        first.discord_id = Some(1);
        first.jopacoin_balance = 2_500;
        let mut second = Player::new("second");
        second.discord_id = Some(2);
        second.jopacoin_balance = 1_500;
        let raw = vec![departed, first, second];
        let visible_ids = BTreeSet::from([1, 2]);
        assert_eq!(
            visible_wheel_positive_balance(&raw, Some(&visible_ids)),
            Some(4_000)
        );
    }

    #[test]
    fn match_settlement_neon_claim_is_restart_safe_and_releasable() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("migrate Neon database");
        let repository = NeonEventRepository::new(database.path());
        assert!(
            repository
                .claim_one_time_event(0, 42, "match_settlement:7", 2, 100)
                .expect("first claim")
        );
        assert!(
            !repository
                .claim_one_time_event(0, 42, "match_settlement:7", 2, 101)
                .expect("duplicate claim")
        );
        repository
            .release_one_time_event(0, 42, "match_settlement:7")
            .expect("release failed delivery");
        assert!(
            repository
                .claim_one_time_event(0, 42, "match_settlement:7", 2, 102)
                .expect("retry claim")
        );
    }

    #[async_trait]
    impl BettingWagerRefreshPort for WagerRefreshProbe {
        async fn refresh_wager_message(
            &self,
            guild_id: i64,
            pending_match_id: i64,
        ) -> Result<BettingWagerRefreshReport, String> {
            self.calls
                .lock()
                .expect("wager refresh calls")
                .push((guild_id, pending_match_id));
            Ok(BettingWagerRefreshReport {
                attempted: 3,
                refreshed: 3,
                failures: Vec::new(),
            })
        }
    }

    #[async_trait]
    impl InteractionResponder for CompositionResponder {
        async fn respond(
            &self,
            _response: InteractionResponse,
        ) -> Result<(), crate::registration::InteractionResponseError> {
            Ok(())
        }

        async fn defer(
            &self,
            _ephemeral: bool,
        ) -> Result<(), crate::registration::InteractionResponseError> {
            Ok(())
        }

        async fn followup(
            &self,
            _response: InteractionResponse,
        ) -> Result<(), crate::registration::InteractionResponseError> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingResponder {
        responses: Arc<Mutex<Vec<InteractionResponse>>>,
        defers: Arc<Mutex<Vec<bool>>>,
        edits: Arc<Mutex<Vec<InteractionResponse>>>,
    }

    #[async_trait]
    impl InteractionResponder for RecordingResponder {
        async fn respond(
            &self,
            response: InteractionResponse,
        ) -> Result<(), crate::registration::InteractionResponseError> {
            self.responses
                .lock()
                .expect("recording responder responses lock")
                .push(response);
            Ok(())
        }

        async fn defer(
            &self,
            ephemeral: bool,
        ) -> Result<(), crate::registration::InteractionResponseError> {
            self.defers
                .lock()
                .expect("recording responder defers lock")
                .push(ephemeral);
            Ok(())
        }

        async fn followup(
            &self,
            response: InteractionResponse,
        ) -> Result<(), crate::registration::InteractionResponseError> {
            self.responses
                .lock()
                .expect("recording responder responses lock")
                .push(response);
            Ok(())
        }

        async fn edit_original(
            &self,
            response: InteractionResponse,
        ) -> Result<(), crate::registration::InteractionResponseError> {
            self.edits
                .lock()
                .expect("recording responder edits lock")
                .push(response);
            Ok(())
        }
    }

    struct EmptyMemberSource;

    #[async_trait]
    impl crate::gateway_events::GuildMemberPageSource for EmptyMemberSource {
        async fn fetch_page(
            &self,
            _guild_id: u64,
            _after: Option<u64>,
            _limit: u64,
        ) -> Result<Vec<crate::gateway_events::GatewayMember>, String> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn provider_composes_live_surface_and_dispatches_a_command() {
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("betting composition configuration");
        let provider = BettingRegistrationProvider::new(
            "/tmp/cama-mm-betting-provider-test.sqlite",
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let mut builder = RegistryBuilder::default();
        builder
            .add_provider(&provider)
            .expect("register betting provider");
        let registry = builder.build();

        let command_names = registry
            .commands()
            .map(|command| command.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            command_names,
            BTreeSet::from(["bet", "mybets", "bets", "balance", "gamba", "economy"])
        );
        assert_eq!(registry.component_routes().len(), 4);
        assert!(registry.command_handler("bet").is_some());
        assert!(registry.component_handler("betting:wheel:0").is_some());
        assert!(registry.component_handler("tt_0").is_some());
        assert!(registry.component_handler("disc_0").is_some());
        assert!(registry.component_handler("disburse:approve").is_some());

        provider
            .handler
            .handle(
                InteractionRequest::Command {
                    interaction_id: 1,
                    name: "bet".to_owned(),
                    user_id: 7,
                    user_display_name: "bettor".to_owned(),
                    guild_id: None,
                    channel_id: None,
                    member_permissions: None,
                    options: Vec::new(),
                },
                Arc::new(CompositionResponder),
            )
            .await
            .expect("dispatch betting command");
        assert_eq!(
            provider.expire_pending_views().await.expect("view sweep"),
            0
        );
    }

    #[tokio::test]
    async fn gateway_ready_recovery_preserves_process_local_betting_views() {
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("betting composition configuration");
        let provider = BettingRegistrationProvider::new(
            "/tmp/cama-mm-betting-provider-recovery-test.sqlite",
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let observer = provider.gateway_observer();
        assert_eq!(observer.name(), "betting-view-recovery");
        let report = observer
            .ready_recovery(ReadyRecoveryContext::new(
                Arc::<[u64]>::from(vec![42_u64]),
                Arc::new(EmptyMemberSource),
            ))
            .await;
        assert_eq!(report.guilds_attempted, 1);
        assert_eq!(report.guilds_refreshed, 0);
        assert_eq!(report.guilds_superseded, 0);
        assert!(report.failures.is_empty());
    }

    #[tokio::test]
    async fn timeout_worker_is_supervised_and_stops_on_runtime_shutdown() {
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("betting composition configuration");
        let provider = BettingRegistrationProvider::new(
            "/tmp/cama-mm-betting-provider-timeout-worker.sqlite",
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let _spec = provider.timeout_worker();
        let (_sender, receiver) = tokio::sync::watch::channel(true);
        BettingViewTimeoutWorker {
            handler: Arc::clone(&provider.handler),
        }
        .run(WorkerContext::new(receiver))
        .await
        .expect("worker exits cleanly on shutdown");
        assert_eq!(BETTING_VIEW_TIMEOUT_WORKER_NAME, "betting-view-timeouts");
        assert_eq!(BETTING_VIEW_TIMEOUT_WAKE_INTERVAL, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn expired_wheel_sqlite_failure_keeps_claim_for_retry() {
        let database = NamedTempFile::new().expect("temporary retry database");
        initialize_or_migrate(database.path()).expect("retry schema");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "retry-player", Some(42)))
            .expect("retry player");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("retry configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("retry interaction lock")
            .insert(
                "disc_retry".to_owned(),
                PendingWheelInteraction {
                    kind: WheelInteractionKind::Discover,
                    user_id: 7,
                    guild_id: 42,
                    created_at: Instant::now() - Duration::from_secs(120),
                    options: vec![WheelOption {
                        label: "LOSE".to_owned(),
                        value: WheelValue::Numeric(0),
                        color: "#000000",
                    }],
                    wheel_wedges: Vec::new(),
                    golden: false,
                    initial_attachment: None,
                    votes: BTreeMap::new(),
                    spin_kind: WheelKind::Regular,
                    balance_before: 0,
                    effects: ManaEffects::default(),
                    event_id: "retry-event".to_owned(),
                    config: BettingRuntimeConfig::from_application_config(&config),
                    event_win_multiplier: 1.0,
                    event_loss_multiplier: 1.0,
                    bonus_spin: false,
                    last_regular_spin: None,
                    original_responder: None,
                },
            );
        Connection::open(database.path())
            .expect("open retry database")
            .execute_batch(
                "CREATE TRIGGER fail_first_wheel_log
                 BEFORE INSERT ON wheel_spins
                 BEGIN SELECT RAISE(ABORT, 'transient wheel failure'); END;",
            )
            .expect("install transient wheel failure");

        let first = provider.expire_pending_views().await;
        assert!(first.is_err(), "first timeout attempt should fail");
        assert!(
            provider
                .handler
                .wheel_interactions
                .lock()
                .expect("retained interaction lock")
                .contains_key("disc_retry")
        );
        assert!(
            provider
                .handler
                .wheel_in_flight
                .lock()
                .expect("released claim lock")
                .is_empty()
        );

        Connection::open(database.path())
            .expect("reopen retry database")
            .execute("DROP TRIGGER fail_first_wheel_log", [])
            .expect("remove transient failure");
        assert_eq!(
            provider
                .expire_pending_views()
                .await
                .expect("retry timeout attempt"),
            1
        );
        let spin_count: i64 = Connection::open(database.path())
            .expect("open retry result")
            .query_row(
                "SELECT COUNT(*) FROM wheel_spins WHERE event_id='retry-event'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("retry spin count");
        assert_eq!(spin_count, 1);
    }

    #[tokio::test]
    async fn wheel_click_and_timeout_race_settles_exactly_once() {
        let database = NamedTempFile::new().expect("temporary race database");
        initialize_or_migrate(database.path()).expect("race schema");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "race-player", Some(42)))
            .expect("race player");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("race configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("race interaction lock")
            .insert(
                "disc_race".to_owned(),
                PendingWheelInteraction {
                    kind: WheelInteractionKind::Discover,
                    user_id: 7,
                    guild_id: 42,
                    created_at: Instant::now() - Duration::from_secs(120),
                    options: vec![WheelOption {
                        label: "LOSE".to_owned(),
                        value: WheelValue::Numeric(0),
                        color: "#000000",
                    }],
                    wheel_wedges: Vec::new(),
                    golden: false,
                    initial_attachment: None,
                    votes: BTreeMap::new(),
                    spin_kind: WheelKind::Regular,
                    balance_before: 0,
                    effects: ManaEffects::default(),
                    event_id: "race-event".to_owned(),
                    config: BettingRuntimeConfig::from_application_config(&config),
                    event_win_multiplier: 1.0,
                    event_loss_multiplier: 1.0,
                    bonus_spin: false,
                    last_regular_spin: None,
                    original_responder: None,
                },
            );
        let responder = Arc::new(RecordingResponder::default());
        let component = InteractionRequest::Component {
            interaction_id: 99,
            custom_id: "disc_race:0".to_owned(),
            user_id: 7,
            user_display_name: "race-player".to_owned(),
            guild_id: Some(42),
            channel_id: Some(99),
            member_permissions: None,
            values: Vec::new(),
        };
        let (timeout, click) = tokio::join!(
            provider.expire_pending_views(),
            provider.handler.handle(component, responder),
        );
        assert!(timeout.is_ok() || click.is_ok());
        assert!(
            provider
                .handler
                .wheel_interactions
                .lock()
                .expect("race interaction cleanup lock")
                .is_empty()
        );
        assert!(
            provider
                .handler
                .wheel_in_flight
                .lock()
                .expect("race claim cleanup lock")
                .is_empty()
        );
        let spin_count: i64 = Connection::open(database.path())
            .expect("open race result")
            .query_row(
                "SELECT COUNT(*) FROM wheel_spins WHERE event_id='race-event'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("race spin count");
        assert_eq!(spin_count, 1);
    }

    #[tokio::test]
    async fn bet_command_uses_existing_schema_and_red_green_policy() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(7, "bettor", Some(42)))
            .expect("player");
        players.update_balance(7, Some(42), 100).expect("balance");
        let now = unix_seconds().expect("timestamp");
        let pending = PendingMatchRepository::new(database.path())
            .create_pending_match(
                42,
                &PendingMatchState {
                    shuffle_timestamp: Some(now.saturating_sub(10)),
                    bet_lock_until: Some(now.saturating_add(600)),
                    ..PendingMatchState::default()
                },
            )
            .expect("pending match");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("betting composition configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let wager_refresh = WagerRefreshProbe::default();
        provider.set_wager_refresh_port(Arc::new(wager_refresh.clone()));
        let responder = RecordingResponder::default();
        provider
            .handler
            .handle(
                InteractionRequest::Command {
                    interaction_id: 2,
                    name: "bet".to_owned(),
                    user_id: 7,
                    user_display_name: "bettor".to_owned(),
                    guild_id: Some(42),
                    channel_id: Some(99),
                    member_permissions: None,
                    options: vec![
                        InteractionOption {
                            name: "team".to_owned(),
                            value: InteractionValue::String("radiant".to_owned()),
                        },
                        InteractionOption {
                            name: "amount".to_owned(),
                            value: InteractionValue::Integer(10),
                        },
                        InteractionOption {
                            name: "match".to_owned(),
                            value: InteractionValue::Integer(pending.pending_match_id),
                        },
                    ],
                },
                Arc::new(responder.clone()),
            )
            .await
            .expect("dispatch bet command");
        assert_eq!(responder.defers.lock().unwrap().as_slice(), &[true]);
        let responses = responder.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        assert!(responses[0].ephemeral);
        assert!(responses[0].content.contains("Bet placed (Match #"));
        assert_eq!(
            players
                .get_by_id(7, Some(42))
                .expect("player lookup")
                .expect("player")
                .jopacoin_balance,
            90
        );
        assert_eq!(
            BettingServiceRepository::new(database.path())
                .get_pending_bets(Some(42), Some(7), 0, Some(pending.pending_match_id))
                .expect("bet lookup")
                .len(),
            1
        );
        assert_eq!(
            wager_refresh
                .calls
                .lock()
                .expect("wager refresh calls")
                .as_slice(),
            &[(42, pending.pending_match_id)]
        );
    }

    #[test]
    fn command_tree_contains_betting_and_economy_surface() {
        assert_eq!(bet_options(1).len(), 4);
        assert_eq!(economy_options().len(), 7);
        assert_eq!(parse_team("radiant").unwrap(), BettingTeam::Radiant);
        assert_eq!(parse_team("dire").unwrap(), BettingTeam::Dire);
    }

    #[test]
    fn golden_wheel_announcement_matches_public_python_copy_and_mentions_top_players() {
        let mut first = Player::new("first");
        first.discord_id = Some(7);
        first.jopacoin_balance = 900;
        let mut second = Player::new("second");
        second.discord_id = Some(8);
        second.jopacoin_balance = 700;
        let announcement = golden_wheel_announcement(7, &[first, second]);
        assert!(!announcement.ephemeral);
        assert_eq!(
            announcement.allowed_mentions,
            crate::registration::InteractionAllowedMentions::Users(vec![7, 8])
        );
        assert_eq!(
            announcement.embeds[0].title.as_deref(),
            Some("👑 GOLDEN WHEEL INCOMING 👑")
        );
        let description = announcement.embeds[0]
            .description
            .as_deref()
            .unwrap_or_default();
        assert!(description.contains("<@7> is spinning the GOLDEN WHEEL!"));
        assert!(description.contains("**#1** <@7> — 900"));
        assert!(description.contains("**#2** <@8> — 700"));
    }

    #[test]
    fn restart_resilience_pending_match_is_restored_from_migrated_sqlite() {
        let database = NamedTempFile::new().expect("restart database");
        initialize_or_migrate(database.path()).expect("migrate restart database");
        let pending = PendingMatchRepository::new(database.path())
            .create_pending_match(
                42,
                &PendingMatchState {
                    radiant_team_ids: vec![1, 2, 3, 4, 5],
                    dire_team_ids: vec![6, 7, 8, 9, 10],
                    shuffle_timestamp: Some(1_700_000_000),
                    bet_lock_until: Some(1_700_000_600),
                    ..PendingMatchState::default()
                },
            )
            .expect("persist pending match");
        let restarted = PendingMatchRepository::new(database.path())
            .pending_match(42, pending.pending_match_id)
            .expect("reload pending match")
            .expect("pending match survives restart");
        assert_eq!(restarted.state.radiant_team_ids, vec![1, 2, 3, 4, 5]);
        assert_eq!(restarted.state.dire_team_ids, vec![6, 7, 8, 9, 10]);
        assert_eq!(restarted.state.bet_lock_until, Some(1_700_000_600));
    }

    #[test]
    fn restart_resilience_bets_survive_and_settle_after_repository_recreation() {
        let database = NamedTempFile::new().expect("restart betting database");
        initialize_or_migrate(database.path()).expect("migrate restart betting database");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(9_999, "spectator", Some(42)))
            .expect("insert spectator");
        PlayerRepository::new(database.path())
            .update_balance(9_999, Some(42), 50)
            .expect("fund spectator");
        let pending = PendingMatchRepository::new(database.path())
            .create_pending_match(
                42,
                &PendingMatchState {
                    radiant_team_ids: vec![1, 2, 3, 4, 5],
                    dire_team_ids: vec![6, 7, 8, 9, 10],
                    shuffle_timestamp: Some(1_700_000_000),
                    bet_lock_until: Some(1_700_000_600),
                    betting_mode: "house".to_owned(),
                    ..PendingMatchState::default()
                },
            )
            .expect("persist pending match");
        BettingServiceRepository::new(database.path())
            .place_bet_atomic(PlaceBetRequest {
                guild_id: Some(42),
                pending_match_id: pending.pending_match_id,
                discord_id: 9_999,
                team: BettingTeam::Radiant,
                amount: 20,
                bet_time: 1_700_000_010,
                leverage: 1,
                max_debt: 500,
                is_blind: false,
                odds_at_placement: None,
            })
            .expect("place pre-restart bet");
        let restarted = BettingServiceRepository::new(database.path());
        assert_eq!(
            restarted
                .get_pending_bets(
                    Some(42),
                    Some(9_999),
                    1_700_000_000,
                    Some(pending.pending_match_id)
                )
                .expect("reload pre-restart bet")
                .len(),
            1
        );
        restarted
            .settle_pending_bets_atomic(
                7_001,
                Some(42),
                1_700_000_000,
                Some(pending.pending_match_id),
                BettingTeam::Radiant,
                BettingMode::House,
            )
            .expect("settle after restart");
        assert_eq!(
            PlayerRepository::new(database.path())
                .get_by_id(9_999, Some(42))
                .expect("reload spectator")
                .expect("spectator")
                .jopacoin_balance,
            70
        );
    }

    #[test]
    fn restart_resilience_full_betting_workflow_settles_both_sides_after_recreation() {
        let database = NamedTempFile::new().expect("full restart database");
        initialize_or_migrate(database.path()).expect("migrate full restart database");
        let players = PlayerRepository::new(database.path());
        for (discord_id, balance) in [(8_001, 100), (8_002, 100)] {
            players
                .add(&NewPlayer::new(discord_id, "spectator", Some(42)))
                .expect("insert spectator");
            players
                .update_balance(discord_id, Some(42), balance)
                .expect("fund spectator");
        }
        let pending = PendingMatchRepository::new(database.path())
            .create_pending_match(
                42,
                &PendingMatchState {
                    radiant_team_ids: vec![1, 2, 3, 4, 5],
                    dire_team_ids: vec![6, 7, 8, 9, 10],
                    shuffle_timestamp: Some(1_700_000_000),
                    bet_lock_until: Some(1_700_000_600),
                    betting_mode: "house".to_owned(),
                    ..PendingMatchState::default()
                },
            )
            .expect("persist full workflow match");
        let place = |discord_id, team, amount| {
            BettingServiceRepository::new(database.path())
                .place_bet_atomic(PlaceBetRequest {
                    guild_id: Some(42),
                    pending_match_id: pending.pending_match_id,
                    discord_id,
                    team,
                    amount,
                    bet_time: 1_700_000_010,
                    leverage: 1,
                    max_debt: 500,
                    is_blind: false,
                    odds_at_placement: None,
                })
                .expect("place workflow bet");
        };
        place(8_001, BettingTeam::Radiant, 30);
        place(8_002, BettingTeam::Dire, 25);

        let restarted_pending = PendingMatchRepository::new(database.path())
            .pending_match(42, pending.pending_match_id)
            .expect("reload full workflow match")
            .expect("full workflow match survives restart");
        assert_eq!(restarted_pending.pending_match_id, pending.pending_match_id);
        let restarted = BettingServiceRepository::new(database.path());
        restarted
            .settle_pending_bets_atomic(
                7_002,
                Some(42),
                1_700_000_000,
                Some(restarted_pending.pending_match_id),
                BettingTeam::Dire,
                BettingMode::House,
            )
            .expect("settle full workflow after restart");
        assert_eq!(
            players
                .get_by_id(8_001, Some(42))
                .expect("radiant balance")
                .expect("radiant")
                .jopacoin_balance,
            70
        );
        assert_eq!(
            players
                .get_by_id(8_002, Some(42))
                .expect("dire balance")
                .expect("dire")
                .jopacoin_balance,
            125
        );
        assert!(
            restarted
                .get_pending_bets(
                    Some(42),
                    None,
                    1_700_000_000,
                    Some(pending.pending_match_id)
                )
                .expect("pending bets after full workflow")
                .is_empty()
        );
    }

    #[test]
    fn provider_exposes_dig_bonus_wheel_seam() {
        let _ = BettingRegistrationProvider::trigger_bonus_spin;
        let _ = BettingRegistrationProvider::with_wager_refresh_port;
        let explosion = resolve_explosion(true, 0, 1.0, 1.0);
        assert_eq!(explosion.credited_reward, 44);
        assert!(explosion.is_bonus);
        let next = cooldown_after_resolution(1_000, true, Some(900), CooldownOutcome::Other);
        assert_eq!(next.next_spin_at, 87_300);
        assert!(!next.schedule_reminder);
    }

    #[test]
    fn wheel_and_explosion_media_are_valid_multi_frame_gifs() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        let wedges = get_wheel_wedges(false, false, policy);
        let wheel = render_wheel_attachment(&wedges, 3, false).expect("wheel gif");
        let explosion = render_explosion_attachment().expect("explosion gif");
        let expected_wheel_delays = (0..WHEEL_MEDIA_FRAME_COUNT)
            .map(wheel_frame_delay_ms)
            .collect::<Vec<_>>();
        let expected_explosion_delays = (0..EXPLOSION_MEDIA_FRAME_COUNT)
            .map(explosion_frame_delay_ms)
            .collect::<Vec<_>>();
        for (attachment, expected_frames, expected_delays) in [
            (
                wheel,
                WHEEL_MEDIA_FRAME_COUNT,
                expected_wheel_delays.as_slice(),
            ),
            (
                explosion,
                EXPLOSION_MEDIA_FRAME_COUNT,
                expected_explosion_delays.as_slice(),
            ),
        ] {
            assert!(attachment.bytes.starts_with(b"GIF89a"));
            assert!(attachment.bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
            let mut decoder = gif::DecodeOptions::new()
                .read_info(Cursor::new(&attachment.bytes))
                .expect("decode gif header");
            assert_eq!(decoder.width(), WHEEL_MEDIA_SIZE);
            assert_eq!(decoder.height(), WHEEL_MEDIA_SIZE);
            let mut frames = 0;
            let mut delays = Vec::with_capacity(expected_frames);
            while decoder
                .read_next_frame()
                .expect("decode gif frame")
                .is_some_and(|frame| {
                    delays.push(u64::from(frame.delay) * 10);
                    frames += 1;
                    true
                })
            {}
            let encoded_delays = expected_delays
                .iter()
                .map(|milliseconds| milliseconds / 10 * 10)
                .collect::<Vec<_>>();
            assert_eq!(delays, encoded_delays);
            assert_eq!(delays.first().copied(), Some(expected_delays[0]));
            assert_eq!(delays.last().copied(), Some(60_000));
            assert_eq!(frames, expected_frames);
        }
        assert_eq!(explosion_palette_sample_size((500, 500), 56), (94, 94));
        let palette_budget = std::hint::black_box(EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET);
        assert!(94_usize * 94 * EXPLOSION_MEDIA_FRAME_COUNT <= palette_budget);
    }

    #[test]
    fn wheel_media_labels_cover_python_themes_and_fractional_phases() {
        let policies = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        let cases = [
            (get_wheel_wedges(false, false, policies), false, 0.0_f64),
            (get_wheel_wedges(true, false, policies), false, 47.25_f64),
            (get_wheel_wedges(false, true, policies), true, 271.875_f64),
            (get_wheel_wedges(false, false, policies), false, 13.37_f64),
            (get_wheel_wedges(true, false, policies), false, 222.615_f64),
            (get_wheel_wedges(true, false, policies), false, 358.905_f64),
        ];
        for (wedges, golden, rotation) in cases {
            let colors = wedges
                .iter()
                .map(|wedge| parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]))
                .collect::<Vec<_>>();
            let mut palette = vec![
                [7, 9, 14],
                [241, 196, 15],
                [231, 76, 60],
                if golden { [58, 40, 0] } else { [44, 62, 80] },
                [0, 0, 0],
                [255, 255, 255],
            ];
            let indices = colors
                .iter()
                .map(|color| {
                    palette
                        .iter()
                        .position(|candidate| candidate == color)
                        .unwrap_or_else(|| {
                            palette.push(*color);
                            palette.len() - 1
                        }) as u8
                })
                .collect::<Vec<_>>();
            let mut pixels = vec![0_u8; usize::from(WHEEL_MEDIA_SIZE).pow(2)];
            let mut sprite_cache = WheelLabelSpriteCache::default();
            let label_atlas = build_wheel_label_atlas(&wedges, &mut sprite_cache);
            draw_wheel_frame(
                &mut pixels,
                &indices,
                &label_atlas,
                0,
                ((rotation / 11.75).round() as usize).min(WHEEL_MEDIA_FRAME_COUNT - 1),
                golden,
            );
            assert!(pixels.contains(&4));
            assert!(pixels.contains(&5));
            assert!(
                pixels
                    .iter()
                    .all(|pixel| usize::from(*pixel) < palette.len())
            );
        }
    }

    fn assert_cached_label_visual_case(
        wedges: &[cama_app::wheel::WheelWedge],
        golden: bool,
        frame_index: usize,
    ) {
        let colors = wedges
            .iter()
            .map(|wedge| parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]))
            .collect::<Vec<_>>();
        let palette = [
            [7, 9, 14],
            [241, 196, 15],
            [231, 76, 60],
            if golden { [58, 40, 0] } else { [44, 62, 80] },
            [0, 0, 0],
            [255, 255, 255],
        ];
        let indices = colors
            .iter()
            .map(|color| {
                palette
                    .iter()
                    .position(|candidate| candidate == color)
                    .unwrap_or(0) as u8
            })
            .collect::<Vec<_>>();
        let mut pixels = vec![0_u8; usize::from(WHEEL_MEDIA_SIZE).pow(2)];
        let mut sprite_cache = WheelLabelSpriteCache::default();
        let atlas = build_wheel_label_atlas(wedges, &mut sprite_cache);
        draw_wheel_frame(
            &mut pixels,
            &indices,
            &atlas,
            0,
            frame_index.min(WHEEL_MEDIA_FRAME_COUNT - 1),
            golden,
        );
        assert!(pixels.contains(&4));
        assert!(pixels.contains(&5));
        assert!(pixels.iter().all(|pixel| usize::from(*pixel) < 256));
    }

    #[test]
    fn cached_wedge_label_sprites_match_normal_visual_bound() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 0);
    }

    #[test]
    fn cached_wedge_label_sprites_match_bankrupt_visual_bound() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        assert_cached_label_visual_case(&get_wheel_wedges(true, false, policy), false, 4);
    }

    #[test]
    fn cached_wedge_label_sprites_match_golden_visual_bound() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        assert_cached_label_visual_case(&get_wheel_wedges(false, true, policy), true, 23);
    }

    #[test]
    fn cached_wedge_label_sprites_match_fractional_phase_a() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 1);
    }

    #[test]
    fn cached_wedge_label_sprites_match_fractional_phase_b() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        assert_cached_label_visual_case(&get_wheel_wedges(false, false, policy), false, 19);
    }

    #[test]
    fn cached_wedge_label_sprites_match_fractional_phase_c() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        assert_cached_label_visual_case(&get_wheel_wedges(true, false, policy), false, 30);
    }

    #[test]
    fn wheel_label_layout_is_measured_once_and_reused_across_frames() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        let wedges = get_wheel_wedges(false, false, policy);
        let mut cache = WheelLabelSpriteCache::default();
        let first = build_wheel_label_atlas(&wedges, &mut cache);
        let misses_after_first = cache.misses;
        let hits_after_first = cache.hits;
        let second = build_wheel_label_atlas(&wedges, &mut cache);
        assert_eq!(first.wedge_sprite_indices, second.wedge_sprite_indices);
        assert_eq!(cache.misses, misses_after_first);
        assert_eq!(cache.hits - hits_after_first, first.sprites.len());
        assert!(cache.entries.len() <= MAX_WHEEL_LABEL_SPRITES);
    }

    #[test]
    fn wheel_label_sprite_cache_is_custom_safe_and_bounded() {
        let wedges = (0..(MAX_WHEEL_LABEL_SPRITES + 24))
            .map(|index| cama_app::wheel::WheelWedge {
                label: format!("CUSTOM_{index}"),
                value: WheelValue::Numeric(0),
                color: "#123456",
            })
            .collect::<Vec<_>>();
        let mut cache = WheelLabelSpriteCache::default();
        let atlas = build_wheel_label_atlas(&wedges, &mut cache);
        assert_eq!(atlas.wedge_sprite_indices.len(), wedges.len());
        assert!(cache.entries.len() <= MAX_WHEEL_LABEL_SPRITES);
        assert_eq!(cache.misses, wedges.len());
        assert!(atlas.sprites.len() > MAX_WHEEL_LABEL_SPRITES);
    }

    #[test]
    fn wheel_label_sprite_cache_distinguishes_subpixel_geometry_keys() {
        let wedges = vec![cama_app::wheel::WheelWedge {
            label: "PHASE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#123456",
        }];
        let mut cache = WheelLabelSpriteCache::default();
        let _ = cache.get_or_insert("PHASE", 1);
        let _ = cache.get_or_insert("PHASE", 2);
        let _ = cache.get_or_insert("PHASE", 1);
        assert_eq!(cache.misses, 2);
        assert_eq!(cache.hits, 1);
        let atlas = build_wheel_label_atlas(&wedges, &mut cache);
        assert_eq!(atlas.sprites.len(), 1);
        let expected_scale = label_scale("PHASE", std::f64::consts::TAU, 220);
        assert_eq!(
            atlas.sprites[0].width,
            bitmap_text_width("PHASE", expected_scale)
        );
    }

    #[test]
    fn wheel_production_media_is_500px_and_under_discord_limit() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        let wedges = get_wheel_wedges(false, false, policy);
        let attachment = render_wheel_attachment(&wedges, 7, false).expect("production wheel");
        assert!(attachment.bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
        let decoder = gif::DecodeOptions::new()
            .read_info(Cursor::new(attachment.bytes))
            .expect("wheel header");
        assert_eq!(decoder.width(), WHEEL_MEDIA_SIZE);
        assert_eq!(decoder.height(), WHEEL_MEDIA_SIZE);
    }

    #[test]
    fn explosion_production_media_uses_shared_palette_budget_and_upload_bound() {
        let attachment = render_explosion_attachment().expect("production explosion");
        assert!(attachment.bytes.len() < WHEEL_MEDIA_UPLOAD_LIMIT);
        assert_eq!(explosion_palette_sample_size((500, 500), 56), (94, 94));
        let palette_budget = std::hint::black_box(EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET);
        assert!(94_usize * 94 * EXPLOSION_MEDIA_FRAME_COUNT <= palette_budget);
    }

    #[test]
    fn wheel_gif_reuses_palette_seed_without_changing_timing() {
        let policy = WheelEconomyPolicy {
            minigame_scale: 1.0,
            regular_target_ev: 10.0,
            bankrupt_target_ev: 10.0,
            golden_target_ev: 10.0,
        };
        let attachment = render_wheel_attachment(&get_wheel_wedges(false, false, policy), 0, false)
            .expect("seeded wheel GIF");
        let mut decoder = gif::DecodeOptions::new()
            .read_info(Cursor::new(attachment.bytes))
            .expect("wheel GIF header");
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame().expect("wheel frame") {
            delays.push(u64::from(frame.delay) * 10);
        }
        assert_eq!(delays.len(), WHEEL_MEDIA_FRAME_COUNT);
        assert_eq!(delays[..14], [30_u64; 14]);
        assert_eq!(delays.last().copied(), Some(60_000));
    }

    #[test]
    fn explosion_gif_builds_one_shared_palette_without_changing_timing() {
        let attachment = render_explosion_attachment().expect("shared-palette explosion GIF");
        let mut decoder = gif::DecodeOptions::new()
            .read_info(Cursor::new(attachment.bytes))
            .expect("explosion GIF header");
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame().expect("explosion frame") {
            assert!(frame.buffer.iter().all(|index| *index < 6));
            delays.push(u64::from(frame.delay) * 10);
        }
        assert_eq!(delays.len(), EXPLOSION_MEDIA_FRAME_COUNT);
        assert_eq!(delays[0..14], [50_u64; 14]);
        assert_eq!(delays.last().copied(), Some(60_000));
    }

    #[test]
    fn media_delay_contract_matches_python_frame_tables() {
        assert_eq!(
            (0..WHEEL_MEDIA_FRAME_COUNT)
                .map(wheel_frame_delay_ms)
                .collect::<Vec<_>>(),
            [
                [30; 14].as_slice(),
                [45; 14].as_slice(),
                [70; 14].as_slice(),
                [110; 16].as_slice(),
                [180].as_slice(),
                &[195, 210, 225, 240, 255, 270, 285, 300, 315],
                &[60_000, 60_000],
            ]
            .concat()
        );
        assert_eq!(
            (0..EXPLOSION_MEDIA_FRAME_COUNT)
                .map(explosion_frame_delay_ms)
                .collect::<Vec<_>>(),
            [
                [50; 14].as_slice(),
                &[60, 70, 80, 90, 100, 110, 120, 130, 140, 150],
                [60; 4].as_slice(),
                [80; 14].as_slice(),
                [100; 13].as_slice(),
                &[60_000],
            ]
            .concat()
        );
        assert_eq!(gif_delay_centiseconds(60_000), 6_000);
    }

    #[test]
    fn neon_one_time_events_survive_provider_reconstruction() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let port = BettingSqliteNeonEventPort {
            repository: NeonEventRepository::new(database.path()),
        };
        let key = EventKey::new(7, Some(42), "degen_90");
        assert!(
            !port
                .check_one_time_event(&key)
                .expect("initial Neon lookup")
        );
        port.persist_one_time_event(key.clone(), 3)
            .expect("persist Neon event");
        let rebuilt = BettingSqliteNeonEventPort {
            repository: NeonEventRepository::new(database.path()),
        };
        assert!(
            rebuilt
                .check_one_time_event(&key)
                .expect("rebuilt Neon lookup")
        );
        assert_eq!(
            rebuilt.load_one_time_events().expect("load Neon events"),
            vec![key]
        );
    }

    #[test]
    fn nested_option_parser_reads_discord_subcommand_payload() {
        let options = vec![InteractionOption {
            name: "tip".to_owned(),
            value: InteractionValue::Subcommand(vec![InteractionOption {
                name: "amount".to_owned(),
                value: InteractionValue::Integer(9),
            }]),
        }];
        assert_eq!(
            parse_nested_string(&options, "amount"),
            Some("9".to_owned())
        );
    }

    #[test]
    fn event_multiplier_changes_numeric_wedges_but_not_special_values() {
        let mut positive = cama_app::wheel::WheelWedge {
            label: "10".to_owned(),
            value: WheelValue::Numeric(10),
            color: "#fff",
        };
        apply_event_multiplier_to_wedge(&mut positive, 1.5, 0.5);
        assert_eq!(positive.value, WheelValue::Numeric(15));
        assert_eq!(positive.label, "15");

        let mut zero = cama_app::wheel::WheelWedge {
            label: "LOSE".to_owned(),
            value: WheelValue::Numeric(0),
            color: "#000",
        };
        apply_event_multiplier_to_wedge(&mut zero, 10.0, 0.0);
        assert_eq!(zero.value, WheelValue::Numeric(0));
        assert_eq!(zero.label, "LOSE");

        let mut mechanic = cama_app::wheel::WheelWedge {
            label: "DISCOVER".to_owned(),
            value: WheelValue::Mechanic(WheelMechanic::Discover),
            color: "#000",
        };
        apply_event_multiplier_to_wedge(&mut mechanic, 2.0, 2.0);
        assert_eq!(
            mechanic.value,
            WheelValue::Mechanic(WheelMechanic::Discover)
        );
        assert_eq!(mechanic.label, "DISCOVER");
    }

    #[test]
    fn gamba_special_minted_rewards_apply_event_after_central_scaling() {
        assert_eq!(scale_minted_gamba_reward(50, 0.5, 1.0, false), 25);
        assert_eq!(scale_minted_gamba_reward(100, 0.5, 1.0, false), 50);
    }

    #[test]
    fn gamba_chain_reaction_scales_only_the_newly_minted_copy() {
        assert_eq!(scale_minted_gamba_reward(30, 0.5, 1.0, false), 15);
    }

    #[test]
    fn gamba_dynamic_minted_rewards_apply_event_and_report_adjusted_amount() {
        assert_eq!(scale_minted_gamba_reward(20, 0.5, 1.0, false), 10);
        assert_eq!(scale_minted_gamba_reward(50, 0.5, 1.0, false), 25);
    }

    #[test]
    fn gamba_heist_fallback_is_event_scaled() {
        assert_eq!(scale_minted_gamba_reward(20, 0.5, 1.0, false), 10);
    }

    #[test]
    fn gamba_market_crash_fallback_is_event_scaled() {
        assert_eq!(scale_minted_gamba_reward(25, 0.5, 1.0, false), 12);
    }

    #[test]
    fn gamba_hostile_takeover_fallback_is_event_scaled() {
        assert_eq!(scale_minted_gamba_reward(40, 0.5, 1.0, false), 20);
    }

    #[test]
    fn gamba_numeric_and_player_transfer_rewards_are_not_double_scaled() {
        assert_eq!(apply_gamba_event_multiplier(20, 1.0, 1.0), 20);
        assert_eq!(apply_gamba_event_multiplier(12, 1.0, 1.0), 12);
    }

    #[test]
    fn extend_wedge_only_changes_an_existing_bankruptcy_penalty() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "player", Some(42)))
            .expect("player");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let config = BettingRuntimeConfig::from_application_config(&config);
        let wedge = cama_app::wheel::WheelWedge {
            label: "+1".to_owned(),
            value: WheelValue::Mechanic(WheelMechanic::ExtendOne),
            color: "#8b0000",
        };
        let effects = ManaEffects::default();
        let taxable = BTreeSet::new();
        resolve_wheel_value(
            database.path(),
            42,
            7,
            0,
            &wedge,
            WheelKind::Bankrupt,
            &effects,
            &config,
            "extend-no-penalty",
            &taxable,
            1.0,
            1.0,
            false,
            None,
        )
        .expect("resolve no-op extension");
        assert_eq!(
            BankruptcyRepository::new(database.path())
                .get_penalty_games(7, Some(42))
                .expect("read absent penalty"),
            0
        );

        BankruptcyRepository::new(database.path())
            .adjust_penalty_games_atomic(7, Some(42), 2)
            .expect("seed active penalty");
        resolve_wheel_value(
            database.path(),
            42,
            7,
            0,
            &wedge,
            WheelKind::Bankrupt,
            &effects,
            &config,
            "extend-active-penalty",
            &taxable,
            1.0,
            1.0,
            false,
            None,
        )
        .expect("resolve active extension");
        assert_eq!(
            BankruptcyRepository::new(database.path())
                .get_penalty_games(7, Some(42))
                .expect("read extended penalty"),
            3
        );
    }

    #[test]
    fn wheel_interaction_timeout_policy_matches_python_views() {
        assert_eq!(
            wheel_interaction_ttl(WheelInteractionKind::TownTrial),
            Duration::from_secs(300)
        );
        assert_eq!(
            wheel_interaction_ttl(WheelInteractionKind::Discover),
            Duration::from_secs(60)
        );
        assert_eq!(
            wheel_interaction_ttl(WheelInteractionKind::Scrying),
            Duration::from_secs(30)
        );
        assert_eq!(
            wheel_interaction_ttl(WheelInteractionKind::Reroll),
            Duration::from_secs(30)
        );
        let options = vec![
            WheelOption {
                label: "GOOD".to_owned(),
                value: WheelValue::Numeric(100),
                color: "#fff000",
            },
            WheelOption {
                label: "BAD".to_owned(),
                value: WheelValue::Numeric(-100),
                color: "#000000",
            },
        ];
        let base = || PendingWheelInteraction {
            kind: WheelInteractionKind::Discover,
            user_id: 7,
            guild_id: 42,
            created_at: Instant::now(),
            options: options.clone(),
            wheel_wedges: Vec::new(),
            golden: false,
            initial_attachment: None,
            votes: BTreeMap::new(),
            spin_kind: WheelKind::Bankrupt,
            balance_before: -100,
            effects: ManaEffects::default(),
            event_id: "timeout".to_owned(),
            config: BettingRuntimeConfig {
                min_bet: 1,
                max_debt: 500,
                bankruptcy_cooldown_seconds: 0,
                bankruptcy_fresh_start_balance: 0,
                bankruptcy_penalty_games: 0,
                bankruptcy_penalty_rate: 0.0,
                garnishment_rate: 0.0,
                vanity_tax_rate: 0.0,
                wheel_target_ev: 0.0,
                wheel_bankrupt_target_ev: 0.0,
                wheel_golden_target_ev: 0.0,
                loan_cooldown_seconds: 0,
                loan_fee_rate: 0.0,
                loan_max_amount: 0,
                tip_fee_rate: 0.0,
                minigame_jc_delta_scale: 1.0,
                disburse_min_fund: 0,
                disburse_quorum_percentage: 0.0,
                economy_events_enabled: false,
                economy_normal_annual_rate: 0.0,
                economy_inflation_ceiling: 0.0,
                economy_event_lookback_days: 0,
                economy_event_max_reserve_burn_pct: 0.0,
                economy_event_max_wallet_burn_pct: 0.0,
                economy_event_trigger_hour_local: 0,
                neon_degen_enabled: false,
                neon_cooldown_seconds: 0,
                neon_bigwin_floor: 0.05,
                neon_bigwin_full_payout: 5_000,
                neon_bigwin_min_payout: 500,
                admin_user_ids: BTreeSet::new(),
                tax_man_user_ids: BTreeSet::new(),
            },
            event_win_multiplier: 1.0,
            event_loss_multiplier: 1.0,
            bonus_spin: false,
            last_regular_spin: None,
            original_responder: None,
        };
        assert_eq!(timeout_wheel_option(&base()).label, "BAD");
        let mut trial = base();
        trial.kind = WheelInteractionKind::TownTrial;
        trial.votes.insert(1, 0);
        trial.votes.insert(2, 1);
        trial.votes.insert(3, 1);
        assert_eq!(timeout_wheel_option(&trial).label, "BAD");
        let mut reroll = base();
        reroll.kind = WheelInteractionKind::Reroll;
        assert_eq!(timeout_wheel_option(&reroll).label, "GOOD");
    }

    #[test]
    fn disbursement_vote_audit_matches_python_pagination_contract() {
        let proposal = DisbursementProposal {
            guild_id: 42,
            proposal_id: 900,
            message_id: None,
            channel_id: None,
            fund_amount: 250,
            quorum_required: 20,
            status: "active".to_owned(),
            created_at: None,
            votes: {
                let mut votes = DISBURSE_METHODS
                    .iter()
                    .map(|method| ((*method).to_owned(), 0))
                    .collect::<BTreeMap<_, _>>();
                votes.insert("even".to_owned(), 16);
                votes
            },
        };
        let votes = (0..16)
            .map(|index| DisbursementVote {
                discord_id: 1_000 + index,
                vote_method: "even".to_owned(),
                voted_at: 10_000 + index,
            })
            .collect::<Vec<_>>();
        let state = PendingDisburseVotes {
            guild_id: 42,
            requester_id: 7,
            proposal,
            votes,
            page: 0,
            created_at: Instant::now(),
        };
        let response = disbursement_votes_response(&state);
        assert_eq!(
            response.embeds[0].title.as_deref(),
            Some("🔍 Disbursement Vote Details (Tax Man)")
        );
        assert!(response.embeds[0].fields[2].name.contains("1-15 of 16"));
        assert_eq!(response.components.len(), 1);
        assert!(response.components[0].buttons[0].disabled);
        assert!(!response.components[0].buttons[1].disabled);
        assert_eq!(
            parse_disbursement_page_component("disburse:votes:42:7:900:next"),
            Some(("42:7:900".to_owned(), DisburseVotesPageAction::Next))
        );
    }

    #[tokio::test]
    async fn wrong_channel_penalty_debits_one_and_credits_reserve() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(7, "bettor", Some(42)))
            .expect("player");
        players.update_balance(7, Some(42), 2).expect("balance");
        charge_wrong_channel(database.path(), 7, 42)
            .await
            .expect("penance");
        assert_eq!(
            players
                .get_by_id(7, Some(42))
                .expect("player lookup")
                .expect("player")
                .jopacoin_balance,
            1
        );
        assert_eq!(
            LoanRepository::new(database.path())
                .get_nonprofit_fund(Some(42))
                .expect("reserve"),
            1
        );
    }

    #[test]
    fn wheel_income_applies_vanity_tax_snapshot_and_stable_exact_once_key() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(7, "taxable", Some(42)))
            .expect("player");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let runtime = BettingRuntimeConfig::from_application_config(&config);
        let taxable_ids = BTreeSet::from([7_i64]);
        let first = credit_wheel_income(
            database.path(),
            42,
            7,
            0,
            100,
            &runtime,
            "123:7:abc",
            "test wheel reward",
            &taxable_ids,
        )
        .expect("credit taxable wheel reward");
        assert_eq!(first.gross, 100);
        assert_eq!(first.vanity_tax, 10);
        assert_eq!(first.net, 90);
        assert_eq!(
            players
                .get_by_id(7, Some(42))
                .expect("player lookup")
                .expect("player")
                .jopacoin_balance,
            93
        );
        assert_eq!(event_related_id("123:7:abc"), event_related_id("123:7:abc"));
        assert_ne!(event_related_id("123:7:abc"), event_related_id("124:7:abc"));
    }

    #[test]
    fn wheel_positive_delta_applies_blood_pact_once_through_protection_gateway() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(7, "target", Some(42)))
            .expect("target");
        players
            .add(&NewPlayer::new(9, "skimmer", Some(42)))
            .expect("skimmer");
        let skimmer_balance_before = players
            .get_by_id(9, Some(42))
            .expect("skimmer lookup")
            .expect("skimmer")
            .jopacoin_balance;
        let blood_pact = BuffService::new(ManashopRepository::new(database.path()), 1_700_000_000);
        blood_pact
            .grant_blood_pact(9, 42, 7)
            .expect("grant Blood Pact");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let runtime = BettingRuntimeConfig::from_application_config(&config);
        let credit = credit_wheel_income(
            database.path(),
            42,
            7,
            0,
            100,
            &runtime,
            "wheel-blood-pact",
            "test wheel reward",
            &BTreeSet::new(),
        )
        .expect("credit wheel reward");

        let first = apply_wheel_blood_pact_skim(
            database.path(),
            42,
            7,
            0,
            "wheel-blood-pact",
            1_700_000_001,
            runtime.minigame_jc_delta_scale,
        )
        .expect("first Blood Pact skim");
        let expected_skim = cama_domain::economy_scaling::scale_minigame_jc_delta(
            25.0,
            runtime.minigame_jc_delta_scale,
        );
        assert_eq!(first, (expected_skim, credit.balance_after - expected_skim));
        assert_eq!(
            players
                .get_by_id(9, Some(42))
                .expect("skimmer lookup")
                .expect("skimmer")
                .jopacoin_balance,
            skimmer_balance_before + expected_skim
        );

        let retry = apply_wheel_blood_pact_skim(
            database.path(),
            42,
            7,
            0,
            "wheel-blood-pact",
            1_700_000_002,
            runtime.minigame_jc_delta_scale,
        )
        .expect("retry Blood Pact skim");
        assert_eq!(retry, first);
        assert_eq!(
            blood_pact
                .blood_pact_skimmer(7, 42)
                .expect("Blood Pact lookup")
                .expect("active Blood Pact")
                .data
                .skimmed_total,
            Some(expected_skim)
        );
    }

    #[tokio::test]
    async fn wheel_component_after_restart_returns_expiry_copy() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let responder = RecordingResponder::default();
        provider
            .handler
            .handle(
                InteractionRequest::Component {
                    interaction_id: 44,
                    custom_id: "disc_expired:0".to_owned(),
                    user_id: 7,
                    user_display_name: "player".to_owned(),
                    guild_id: Some(42),
                    channel_id: Some(99),
                    member_permissions: None,
                    values: Vec::new(),
                },
                Arc::new(responder.clone()),
            )
            .await
            .expect("expired component response");
        let responses = responder.responses.lock().expect("responses lock");
        assert_eq!(responses.len(), 1);
        assert!(responses[0].ephemeral);
        assert!(responses[0].content.contains("interaction expired"));
        assert!(responses[0].content.contains("/gamba"));
    }

    #[tokio::test]
    async fn wheel_choice_rejects_non_owner_but_town_vote_is_public() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let pending = PendingWheelInteraction {
            kind: WheelInteractionKind::Discover,
            user_id: 7,
            guild_id: 42,
            created_at: Instant::now(),
            options: vec![WheelOption {
                label: "LOSE".to_owned(),
                value: WheelValue::Numeric(0),
                color: "#000000",
            }],
            wheel_wedges: Vec::new(),
            golden: false,
            initial_attachment: None,
            votes: BTreeMap::new(),
            spin_kind: WheelKind::Bankrupt,
            balance_before: -10,
            effects: ManaEffects::default(),
            event_id: "test".to_owned(),
            config: BettingRuntimeConfig::from_application_config(&config),
            event_win_multiplier: 1.0,
            event_loss_multiplier: 1.0,
            bonus_spin: false,
            last_regular_spin: None,
            original_responder: None,
        };
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("wheel state lock")
            .insert("disc_test".to_owned(), pending);
        let responder = RecordingResponder::default();
        provider
            .handler
            .handle(
                InteractionRequest::Component {
                    interaction_id: 45,
                    custom_id: "disc_test:0".to_owned(),
                    user_id: 8,
                    user_display_name: "spectator".to_owned(),
                    guild_id: Some(42),
                    channel_id: Some(99),
                    member_permissions: None,
                    values: Vec::new(),
                },
                Arc::new(responder.clone()),
            )
            .await
            .expect("owner guard response");
        let responses = responder.responses.lock().expect("responses lock");
        assert_eq!(responses[0].content, "This choice isn't yours to make.");
    }

    #[tokio::test]
    async fn wheel_component_edits_public_prompt_after_resolution() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        let players = PlayerRepository::new(database.path());
        players
            .add(&NewPlayer::new(7, "player", Some(42)))
            .expect("player");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        let responder = RecordingResponder::default();
        let initial_attachment = InteractionAttachment::bytes("wheel.gif", vec![1, 2, 3, 4]);
        let pending = PendingWheelInteraction {
            kind: WheelInteractionKind::Discover,
            user_id: 7,
            guild_id: 42,
            created_at: Instant::now(),
            options: vec![WheelOption {
                label: "LOSE".to_owned(),
                value: WheelValue::Numeric(0),
                color: "#000000",
            }],
            wheel_wedges: Vec::new(),
            golden: false,
            initial_attachment: Some(initial_attachment.clone()),
            votes: BTreeMap::new(),
            spin_kind: WheelKind::Regular,
            balance_before: 0,
            effects: ManaEffects::default(),
            event_id: "public-edit".to_owned(),
            config: BettingRuntimeConfig::from_application_config(&config),
            event_win_multiplier: 1.0,
            event_loss_multiplier: 1.0,
            bonus_spin: false,
            last_regular_spin: None,
            original_responder: Some(Arc::new(responder.clone())),
        };
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("wheel state lock")
            .insert("disc_public-edit".to_owned(), pending);

        provider
            .handler
            .handle(
                InteractionRequest::Component {
                    interaction_id: 46,
                    custom_id: "disc_public-edit:0".to_owned(),
                    user_id: 7,
                    user_display_name: "player".to_owned(),
                    guild_id: Some(42),
                    channel_id: Some(99),
                    member_permissions: None,
                    values: Vec::new(),
                },
                Arc::new(responder.clone()),
            )
            .await
            .expect("resolve wheel component");

        let responses = responder.responses.lock().expect("responses lock");
        assert_eq!(responses.len(), 1);
        assert!(responses[0].ephemeral);
        drop(responses);
        let edits = responder.edits.lock().expect("edits lock");
        assert_eq!(edits.len(), 1);
        assert!(!edits[0].ephemeral);
        assert!(edits[0].content.contains("You spun"));
        assert_eq!(
            edits[0].embeds[0].title.as_deref(),
            Some("🚫 LOSE A TURN 🚫")
        );
        assert_eq!(edits[0].embeds.len(), 1);
        assert_eq!(edits[0].attachments, vec![initial_attachment]);
    }

    #[tokio::test]
    async fn wheel_component_reroll_replaces_public_gif_attachment() {
        let database = NamedTempFile::new().expect("temporary database");
        initialize_or_migrate(database.path()).expect("schema");
        PlayerRepository::new(database.path())
            .add(&NewPlayer::new(7, "player", Some(42)))
            .expect("player");
        ManaRepository::new(database.path())
            .set_mana(7, Some(42), "Mountain", "2025-01-01")
            .expect("red mana");
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let mut effects = ManaEffects::default();
        effects.color = Some("Red".to_owned());
        let wedges = vec![
            cama_app::wheel::WheelWedge {
                label: "LOSE".to_owned(),
                value: WheelValue::Numeric(0),
                color: "#000000",
            },
            cama_app::wheel::WheelWedge {
                label: "GOOD".to_owned(),
                value: WheelValue::Numeric(5),
                color: "#ffffff",
            },
        ];
        let initial_attachment =
            render_wheel_attachment(&wedges, 0, false).expect("initial wheel GIF");
        let responder = RecordingResponder::default();
        let pending = PendingWheelInteraction {
            kind: WheelInteractionKind::Reroll,
            user_id: 7,
            guild_id: 42,
            created_at: Instant::now(),
            options: wedges.iter().map(wheel_option).collect(),
            wheel_wedges: wedges.clone(),
            golden: false,
            initial_attachment: Some(initial_attachment.clone()),
            votes: BTreeMap::new(),
            spin_kind: WheelKind::Bankrupt,
            balance_before: 0,
            effects,
            event_id: "reroll-media".to_owned(),
            config: BettingRuntimeConfig::from_application_config(&config),
            event_win_multiplier: 1.0,
            event_loss_multiplier: 1.0,
            bonus_spin: false,
            last_regular_spin: None,
            original_responder: Some(Arc::new(responder.clone())),
        };
        let provider = BettingRegistrationProvider::new(
            database.path(),
            &config,
            Arc::new(crate::serenity_transport::SerenityDiscordTransport::new()),
        );
        provider
            .handler
            .wheel_interactions
            .lock()
            .expect("wheel state lock")
            .insert("betting:reroll:reroll-media".to_owned(), pending);

        provider
            .handler
            .handle(
                InteractionRequest::Component {
                    interaction_id: 47,
                    custom_id: "betting:reroll:reroll-media:yes".to_owned(),
                    user_id: 7,
                    user_display_name: "player".to_owned(),
                    guild_id: Some(42),
                    channel_id: Some(99),
                    member_permissions: None,
                    values: Vec::new(),
                },
                Arc::new(responder.clone()),
            )
            .await
            .expect("resolve reroll component");

        let responses = responder.responses.lock().expect("responses lock");
        assert_eq!(responses.len(), 1);
        assert!(responses[0].ephemeral);
        assert!(responses[0].attachments.is_empty());
        drop(responses);
        let edits = responder.edits.lock().expect("edits lock");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].attachments.len(), 1);
        assert_eq!(edits[0].attachments[0].filename, "wheel.gif");
        assert_ne!(edits[0].attachments[0].bytes, initial_attachment.bytes);
        assert!(
            ManaRepository::new(database.path())
                .is_bankrupt_buff_used(7, Some(42), BankruptBuff::Reroll)
                .expect("reroll claim")
        );
    }

    #[test]
    fn hostile_result_notes_preserve_python_detail_fields() {
        let config = ApplicationConfig::from_lookup(|name| {
            (name == "DISCORD_BOT_TOKEN").then_some("test-token".to_owned())
        })
        .expect("configuration");
        let config = BettingRuntimeConfig::from_application_config(&config);
        let lightning = HostileResolution {
            lightning_total: 27,
            lightning_count: 2,
            total: 27,
            count: 2,
            victims: vec![("alice".to_owned(), 19), ("bob".to_owned(), 8)],
            shield_absorbed_total: 3,
            shielded_count: 1,
            ..HostileResolution::default()
        };
        let note = hostile_result_note(WheelMechanic::LightningBolt, &lightning, &config);
        assert!(note.contains("**2** player(s)"));
        assert!(note.contains("27"));
        assert!(note.contains("alice"));
        assert!(note.contains("White Mana Shields"));

        let shell = HostileResolution {
            total: 12,
            victim_name: Some("rival".to_owned()),
            victim_new_balance: Some(88),
            ..HostileResolution::default()
        };
        let note = hostile_result_note(WheelMechanic::RedShell, &shell, &config);
        assert!(note.contains("rival"));
        assert!(note.contains("Victim's new balance: **88**"));
    }
}
