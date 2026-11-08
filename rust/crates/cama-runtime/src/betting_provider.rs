//! Production registration for Python's commands.betting extension.
//!
//! Durable money and wager decisions stay in the existing-schema repositories.
//! The runtime layer owns only interaction translation, rate limiting, and
//! Discord response composition.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::dig_neon::{BigWinFlavor, BigWinSource};
use cama_app::dota_bet_seed::{BetTotals, bets_overview, betting_seed_without_lobby_bonus};
use cama_app::economy_event_service::EconomyEventConfig;
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::jopat_post_match::{JopatPostMatchContext, NumericFact};
use cama_app::loan::{LoanPolicy, LoanService};
use cama_app::neon_degen::{EventKey, NeonDegenService, NeonEventPort, NeonResult, StoreError};
use cama_app::wheel::{WheelKind, WheelMechanic, WheelValue, wheel_spin_is_available};
pub(crate) use cama_app::wheel_settlement::active_mana_effects;
use cama_app::wheel_settlement::{
    NoWheelReminders, PendingWheelPlan, PendingWheelRequest, SettledWheelSpin, WheelAttemptMemo,
    WheelInteractionKind, WheelOption, WheelPlayerNameSource, WheelReminderPort,
    WheelSettlementConfig, WheelSpinOutcome, WheelSpinRequest, apply_hostile_wheel_loss,
    pin_wheel_selection, record_betting_pet_activity, spin_wheel, wheel_cooldown_message,
};
use cama_db::autobet_investments::AutobetInvestmentRepository;
use cama_db::bankruptcy_repository::{
    BankruptcyRepository, DeclarationOutcome, DeclarationRequest,
};
use cama_db::betting_service_repository::{
    BetCounterReceipt, BettingServiceRepository, PendingBetRecord, PlaceBetRequest,
};
use cama_db::core_repositories::{MatchRepository, PlayerRepository};
use cama_db::disbursement::{
    DISBURSE_METHODS, DisbursementProposal, DisbursementRepository, DisbursementVote,
};
use cama_db::dota_bet_seed_repository::{BettingMode, BettingTeam, SeedSplit};
use cama_db::gambling_stats_repository::{GamblingStatsRepository, GamblingStatsService};
use cama_db::loan_repository::{DebtPayment, LoanRepository};
use cama_db::low_priority_repository::LowPriorityRepository;
use cama_db::match_runtime::{PendingMatchRecord, PendingMatchRepository};
use cama_db::neon_events::NeonEventRepository;
use cama_db::shop_runtime::{HostileDestination as DbHostileDestination, ShopRuntimeRepository};
use cama_db::tax_repository::{TaxLedgerEntry, TaxPlayerSnapshot, TaxRepository};
use cama_db::tip_repository::{TipRepository, TipRepositoryError};
use cama_domain::discord_content::{UNKNOWN_PLAYER, readable_player_name};
use cama_domain::embed_safety::{FIELD_VALUE_LIMIT, MAX_FIELDS, TOTAL_LIMIT, truncate_field};
use cama_domain::formatting::{JOPACOIN_EMOTE, format_grouped};
use cama_domain::game_date::next_game_day_start_ts;
use cama_domain::mana::{ManaEffects, format_mana_badge};
use cama_domain::pet_evolution::PetActivity;
use cama_domain::tip_service::tip_fee_and_tithe;
use chrono::Utc;
use serde_json::json;
use tracing::warn;

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{
    DiscordMessage, DiscordTransport, GuildPlayerNameDirectory, cached_guild_player_names,
    resolve_guild_player_names,
};
use crate::gateway_events::{GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryReport};
use crate::ids::{blocking as sqlite, signed_id};
use crate::match_provider::MatchRegistrationProvider;
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionAttachment, InteractionButton, InteractionButtonStyle,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};
use crate::reminder_provider::ReminderHooks;
use crate::runtime_ports::{
    MatchBetSettlementParticipant, MatchBetSettlementRequest, MatchEasterEggRequest,
    MatchPostMatchDebriefPort, MatchPostMatchDebriefRequest,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatchBigWinSelection {
    discord_id: i64,
    payout: i64,
    flavor: BigWinFlavor,
}

fn select_match_big_win(
    participants: &[MatchBetSettlementParticipant],
) -> Option<MatchBigWinSelection> {
    // Keep Python's ordered `max(winners, key=payout)` semantics: an equal
    // payout keeps the first winner, and a refunded top winner blocks the
    // headline rather than falling through to a lower-payout winner.
    let top = participants
        .iter()
        .filter(|participant| participant.won)
        .fold(None, |best, participant| match best {
            None => Some(participant),
            Some(best) if participant.payout > best.payout => Some(participant),
            Some(best) => Some(best),
        })?;
    if top.payout == 0 || top.refunded {
        return None;
    }
    let win_stake = participants
        .iter()
        .filter(|participant| participant.won)
        .fold(0_i64, |total, participant| {
            total.saturating_add(participant.amount)
        });
    let lose_stake = participants
        .iter()
        .filter(|participant| !participant.won)
        .fold(0_i64, |total, participant| {
            total.saturating_add(participant.amount)
        });
    Some(MatchBigWinSelection {
        discord_id: top.discord_id,
        payout: top.payout,
        flavor: if win_stake != 0 && lose_stake > win_stake {
            BigWinFlavor::Underdog
        } else {
            BigWinFlavor::TopDog
        },
    })
}

fn active_settlement_loser_ids(participants: &[MatchBetSettlementParticipant]) -> Vec<i64> {
    let mut loser_ids = Vec::new();
    for participant in participants
        .iter()
        .filter(|participant| !participant.won && !participant.refunded)
    {
        if !loser_ids.contains(&participant.discord_id) {
            loser_ids.push(participant.discord_id);
        }
    }
    loser_ids
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

        let participants = request.participants.clone();
        let max_debt = self.handler.config.max_debt;
        let mut selected = self
            .handler
            .run_neon(move |neon| {
                let mut selected = None;
                if let Some(winner) = select_match_big_win(&participants) {
                    selected = neon.on_big_win(
                        u64::try_from(winner.discord_id).unwrap_or_default(),
                        Some(u64::try_from(guild_id).unwrap_or_default()),
                        BigWinSource::Match,
                        winner.payout,
                        winner.flavor,
                    );
                }
                if selected.is_none() {
                    for loser in participants
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
                            && (loser.balance_after <= -max_debt
                                || (loser.balance_after == 0 && !loser.won))
                        {
                            selected = neon.on_bet_settled(
                                discord_id,
                                guild,
                                loser.won,
                                loser.balance_after,
                            );
                        }
                        if selected.is_some() {
                            break;
                        }
                    }
                }
                selected
            })
            .await?;
        if selected.is_none() {
            let loser_ids = active_settlement_loser_ids(&request.participants);
            let mut degen_scores = Vec::new();
            for discord_id in loser_ids {
                let path = self.handler.database_path.clone();
                let score = sqlite("match degen milestone score", move || {
                    GamblingStatsService::new(GamblingStatsRepository::new(path))
                        .calculate_degen_score(discord_id, Some(guild_id))
                        .map(|score| u8::try_from(score.total.clamp(0, 100)).unwrap_or_default())
                        .map_err(|error| error.to_string())
                })
                .await
                .ok();
                if let Some(score) = score.filter(|score| *score >= 90) {
                    degen_scores.push((discord_id, score));
                }
            }
            selected = self
                .handler
                .run_neon(move |neon| {
                    for (discord_id, score) in degen_scores {
                        let selected = neon.on_degen_milestone(
                            u64::try_from(discord_id).unwrap_or_default(),
                            Some(u64::try_from(guild_id).unwrap_or_default()),
                            score,
                        );
                        if selected.is_some() {
                            return selected;
                        }
                    }
                    None
                })
                .await?;
        }
        let Some(result) = selected else {
            warn!(
                match_id = request.match_id,
                guild_id, "match settlement Neon claim declined; terminal marker retained"
            );
            return Ok(());
        };
        let fired_event_type = format!("match_settlement:fired:{}", request.match_id);
        let repository = NeonEventRepository::new(&self.handler.database_path);
        sqlite("mark match settlement Neon decision", move || {
            repository
                .persist_one_time_event(
                    0,
                    guild_id,
                    &fired_event_type,
                    2,
                    unix_seconds().unwrap_or_default(),
                )
                .map_err(|error| error.to_string())
        })
        .await?;
        if !self
            .handler
            .emit_neon_result_checked(channel_id, result)
            .await
        {
            warn!(
                match_id = request.match_id,
                guild_id, "match settlement Neon delivery failed; terminal marker retained"
            );
        }
        Ok(())
    }

    async fn on_match_easter_eggs(&self, request: MatchEasterEggRequest) -> Result<(), String> {
        let Some(channel_id) = request.channel_id else {
            return Ok(());
        };
        if request.games_milestones.is_empty()
            && request.win_streak_records.is_empty()
            && request.rivalries_detected.is_empty()
        {
            return Ok(());
        }
        let guild_id = request.guild_id;
        let event_type = format!("match_easter_eggs:{}", request.match_id);
        let repository = NeonEventRepository::new(&self.handler.database_path);
        let settlement_fired_event_type = format!("match_settlement:fired:{}", request.match_id);
        if repository
            .check_one_time_event(0, guild_id, &settlement_fired_event_type)
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        let claimed = sqlite("claim match Easter-egg Neon event", {
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

        let now = unix_seconds().unwrap_or_default();
        let hook_request = request.clone();
        let selected = self
            .handler
            .run_neon(move |neon| {
                let request = hook_request;
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                let mut selected = None;
                for milestone in &request.games_milestones {
                    if selected.is_some() {
                        break;
                    }
                    let Ok(discord_id) = u64::try_from(milestone.discord_id) else {
                        continue;
                    };
                    selected = neon.on_games_milestone(
                        discord_id,
                        u64::try_from(guild_id).ok(),
                        milestone.games_played,
                    );
                    if selected.is_some() {
                        break;
                    }
                }
                if selected.is_none() {
                    for streak in &request.win_streak_records {
                        let Ok(discord_id) = u64::try_from(streak.discord_id) else {
                            continue;
                        };
                        selected = neon.on_win_streak_record(
                            discord_id,
                            u64::try_from(guild_id).ok(),
                            streak.current_streak,
                            streak.previous_best,
                        );
                        if selected.is_some() {
                            break;
                        }
                    }
                }
                if selected.is_none() {
                    for rivalry in &request.rivalries_detected {
                        let Ok(player1_id) = u64::try_from(rivalry.player1_id) else {
                            continue;
                        };
                        let Ok(player2_id) = u64::try_from(rivalry.player2_id) else {
                            continue;
                        };
                        selected = neon.on_rivalry_detected(
                            u64::try_from(guild_id).ok(),
                            player1_id,
                            player2_id,
                            rivalry.games_against,
                            rivalry.winrate_vs,
                        );
                        if selected.is_some() {
                            break;
                        }
                    }
                }
                selected
            })
            .await?;

        let Some(result) = selected else {
            warn!(
                match_id = request.match_id,
                guild_id, "match Easter-egg Neon claim declined; terminal marker retained"
            );
            return Ok(());
        };
        let fired_event_type = format!("match_easter_eggs:fired:{}", request.match_id);
        let repository = NeonEventRepository::new(&self.handler.database_path);
        sqlite("mark match Easter-egg Neon decision", move || {
            repository
                .persist_one_time_event(
                    0,
                    guild_id,
                    &fired_event_type,
                    2,
                    unix_seconds().unwrap_or_default(),
                )
                .map_err(|error| error.to_string())
        })
        .await?;
        if !self
            .handler
            .emit_neon_result_checked(channel_id, result)
            .await
        {
            warn!(
                match_id = request.match_id,
                guild_id, "match Easter-egg Neon delivery failed; terminal marker retained"
            );
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
        let guild_id = request.guild_id;
        let winner_id = request.winner_id;
        let loser_id = request.loser_id;
        let player_ids = [winner_id, loser_id]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let player_names = self
            .handler
            .render_player_names(&player_ids, Some(guild_id))
            .await;
        let winner_name = winner_id.and_then(|id| player_names.get(&id).cloned());
        let loser_name = loser_id.and_then(|id| player_names.get(&id).cloned());
        let repository = NeonEventRepository::new(&self.handler.database_path);
        let event_type = format!("match_debrief:{}", request.match_id);
        for fired_event_type in [
            format!("match_settlement:fired:{}", request.match_id),
            format!("match_easter_eggs:fired:{}", request.match_id),
        ] {
            if repository
                .check_one_time_event(0, guild_id, &fired_event_type)
                .map_err(|error| error.to_string())?
            {
                return Ok(());
            }
        }
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
            .run_neon(move |neon| neon.on_post_match_debrief(&context))
            .await?;
        if let Some(result) = result {
            if !self
                .handler
                .emit_neon_result_checked(channel_id, result)
                .await
            {
                warn!(
                    match_id = request.match_id,
                    guild_id, "match debrief Neon delivery failed; terminal marker retained"
                );
            }
        } else {
            warn!(
                match_id = request.match_id,
                guild_id, "match debrief Neon claim declined; terminal marker retained"
            );
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
const BALANCE_COMPONENT_PREFIX: &str = "betting:balance:";
const DISBURSE_COMPONENT_PREFIX: &str = "disburse:";
const TOWN_TRIAL_COMPONENT_PREFIX: &str = "tt_";
const DISCOVER_COMPONENT_PREFIX: &str = "disc_";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const DEFAULT_RATE_WINDOW: Duration = Duration::from_secs(10);
const DISBURSE_VOTES_PAGE_SIZE: usize = 15;
const DISBURSE_VOTES_TTL: Duration = Duration::from_secs(300);
const BALANCE_MARKET_PAGE_SIZE: usize = 4;
const BALANCE_LEDGER_PAGE_SIZE: usize = 6;
const BALANCE_VIEW_TTL: Duration = Duration::from_secs(300);
const BETTING_NEON_DELETE_DELAY: Duration = Duration::from_secs(60);
// Python waits for the animation and then gives Discord half a second before
// adding the result embed to the attachment-only message.
const BETTING_WHEEL_DISPLAY_DELAY: Duration = Duration::from_millis(15_500);
const BETTING_EXPLOSION_DISPLAY_DELAY: Duration = Duration::from_millis(8_500);

#[derive(Clone, Debug)]
pub struct BettingRuntimeConfig {
    pub min_bet: i64,
    pub max_debt: i64,
    pub bankruptcy_cooldown_seconds: i64,
    pub bankruptcy_fresh_start_balance: i64,
    pub bankruptcy_penalty_games: i64,
    pub bankruptcy_penalty_rate_per_game: f64,
    pub garnishment_rate: f64,
    /// Profit tax withheld from wheel income for players in the vanity-tax
    /// taxable-id set (see `credit_wheel_income`).
    pub vanity_tax_rate: f64,
    /// Profit tax withheld from wheel income while a player serves active low
    /// priority. The taxable set is read from SQLite inside the spin's
    /// blocking closure (see `credit_wheel_income`).
    pub low_priority_tax_rate: f64,
    pub wheel_target_ev: f64,
    pub wheel_bankrupt_target_ev: f64,
    pub wheel_golden_target_ev: f64,
    pub loan_cooldown_seconds: i64,
    pub loan_fee_rate: f64,
    pub loan_max_amount: i64,
    pub tip_fee_rate: f64,
    pub minigame_jc_delta_scale: f64,
    pub prediction_contract_value: i64,
    pub prediction_initial_fair_default: i64,
    /// Admit negative-ID database fixtures to wheel rank mechanics. Real
    /// Discord IDs remain checked against the live guild member snapshot.
    pub synthetic_members_enabled: bool,
    pub disburse_min_fund: i64,
    /// Recency window that admits a player to the `lottery` disbursement draw.
    pub lottery_activity_days: i64,
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
    /// The settings `/gamba` settlement reads.
    #[must_use]
    pub const fn wheel_settlement(&self) -> WheelSettlementConfig {
        WheelSettlementConfig {
            garnishment_rate: self.garnishment_rate,
            bankruptcy_penalty_rate_per_game: self.bankruptcy_penalty_rate_per_game,
            vanity_tax_rate: self.vanity_tax_rate,
            low_priority_tax_rate: self.low_priority_tax_rate,
            minigame_jc_delta_scale: self.minigame_jc_delta_scale,
            wheel_target_ev: self.wheel_target_ev,
            wheel_bankrupt_target_ev: self.wheel_bankrupt_target_ev,
            wheel_golden_target_ev: self.wheel_golden_target_ev,
        }
    }

    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        let values = &config.values;
        Self {
            min_bet: values.jopacoin_min_bet.max(1),
            max_debt: values.max_debt,
            bankruptcy_cooldown_seconds: values.bankruptcy_cooldown_seconds,
            bankruptcy_fresh_start_balance: values.bankruptcy_fresh_start_balance,
            bankruptcy_penalty_games: values.bankruptcy_penalty_games,
            bankruptcy_penalty_rate_per_game: values.bankruptcy_penalty_rate_per_game,
            garnishment_rate: values.garnishment_percentage,
            vanity_tax_rate: values.vanity_tax_rate,
            low_priority_tax_rate: values.low_priority_profit_tax_rate,
            wheel_target_ev: values.wheel_target_ev,
            wheel_bankrupt_target_ev: values.wheel_bankrupt_target_ev,
            wheel_golden_target_ev: values.wheel_golden_target_ev,
            loan_cooldown_seconds: values.loan_cooldown_seconds,
            loan_fee_rate: values.loan_fee_rate,
            loan_max_amount: values.loan_max_amount,
            tip_fee_rate: values.tip_fee_rate,
            minigame_jc_delta_scale: values.minigame_jc_delta_scale,
            prediction_contract_value: values.prediction_contract_value,
            prediction_initial_fair_default: values.prediction_initial_fair_default,
            synthetic_members_enabled: values.gamba_synthetic_members_enabled,
            disburse_min_fund: values.disburse_min_fund,
            lottery_activity_days: values.lottery_activity_days,
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
        let tax_repository = TaxRepository::new(
            &database_path,
            config.prediction_contract_value,
            config.prediction_initial_fair_default,
        );
        let mut neon = NeonDegenService::with_event_port(fastrand::u64(..), neon_port);
        neon.set_player_names(Arc::new(crate::discord_transport::GuildCacheNeonNames(
            Arc::new(
                crate::discord_transport::DiscordGuildPlayerNameResolver::new(Arc::clone(&discord)),
            ),
        )));
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
                tax_repository,
                config,
                discord,
                vanity_tax,
                neon: Arc::new(Mutex::new(neon)),
                wheel_reminders: OnceLock::new(),
                wager_refresh: Mutex::new(None),
                rates: Mutex::new(BTreeMap::new()),
                wheel_interactions: Mutex::new(BTreeMap::new()),
                wheel_in_flight: Mutex::new(BTreeSet::new()),
                disburse_vote_pages: Mutex::new(BTreeMap::new()),
                balance_views: Mutex::new(BTreeMap::new()),
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

    /// Schedule each spin's wheel-ready reminder through the reminder runtime.
    pub fn attach_reminder_hooks(&self, hooks: ReminderHooks) -> Result<(), String> {
        self.handler
            .wheel_reminders
            .set(Arc::new(hooks))
            .map_err(|_| "betting reminder hooks were already attached".to_owned())
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

    #[must_use]
    pub fn disbursement_worker(&self) -> BackgroundWorkerSpec {
        BackgroundWorkerSpec::new(
            "disbursement-quorum",
            Arc::new(DisbursementQuorumWorker(self.handler.clone())),
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

struct DisbursementQuorumWorker(Arc<BettingInteractionHandler>);

#[async_trait]
impl BackgroundWorker for DisbursementQuorumWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        while !context.shutdown_requested() {
            let now = unix_seconds()?;
            let reset = next_game_day_start_ts(now).map_err(|error| error.to_string())?;
            if !context
                .sleep(Duration::from_secs(reset.abs_diff(now)))
                .await
            {
                break;
            }
            self.0.reconcile_disbursements().await?;
        }
        Ok(())
    }
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
        self.register_commands(registry, None)
    }
}

impl BettingRegistrationProvider {
    pub fn register_with_deadlock(
        &self,
        registry: &mut RegistryBuilder,
        deadlock: Arc<dyn InteractionHandler>,
    ) -> Result<(), RegistrationError> {
        self.register_commands(registry, Some(deadlock))
    }

    fn register_commands(
        &self,
        registry: &mut RegistryBuilder,
        deadlock: Option<Arc<dyn InteractionHandler>>,
    ) -> Result<(), RegistrationError> {
        let mut options = bet_options(self.handler.config.min_bet);
        let handler: Arc<dyn InteractionHandler> = if let Some(deadlock) = deadlock {
            options.push(choices_string(
                CommandOptionSpec::new(
                    "game",
                    "Game to bet on (default Dota)",
                    CommandOptionKind::String,
                ),
                &[("Dota", "dota"), ("Deadlock", "deadlock")],
            ));
            if let Some(team) = options.iter_mut().find(|option| option.name == "team") {
                team.description = "Team shown on the selected match".to_owned();
                for (name, value) in [("Deadlock Team 1", "team1"), ("Deadlock Team 2", "team2")] {
                    team.choices.push(CommandOptionChoice::String {
                        name: name.to_owned(),
                        value: value.to_owned(),
                    });
                }
            }
            Arc::new(crate::deadlock_provider::DeadlockCommandRouter::new(
                self.handler.clone(),
                deadlock,
                "game",
            ))
        } else {
            self.handler.clone()
        };
        registry.command(CommandSpec {
            name: "bet".to_owned(),
            description: "Place a jopacoin bet on a match (check balance with /balance)".to_owned(),
            options,
            handler,
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
            description: "View your Jopacoin portfolio, exposure, and activity".to_owned(),
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
        CommandOptionSpec::subcommand(
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
        CommandOptionSpec::subcommand(
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
        CommandOptionSpec::subcommand(
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
        CommandOptionSpec::subcommand(
            "bankruptcy",
            "Declare bankruptcy to clear your debt (once per week, with penalties)",
            Vec::new(),
        ),
        CommandOptionSpec::subcommand("loan", "Borrow jopacoin (with a fee)", vec![loan_amount]),
        CommandOptionSpec::subcommand("reserve", "View the Jopacoin Reserve", Vec::new()),
        CommandOptionSpec::subcommand(
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

/// Render the wheel animation as the reveal attachment. Drawing lives in
/// [`cama_app::wheel_media`]; callers run this on a blocking worker.
fn render_wheel_attachment(
    wedges: &[cama_app::wheel::WheelWedge],
    target_index: usize,
    golden: bool,
    display_name: Option<&str>,
) -> Result<InteractionAttachment, String> {
    let started = Instant::now();
    let rendered =
        cama_app::wheel_media::render_wheel_gif(wedges, target_index, golden, display_name)?;
    tracing::debug!(
        cache_hit = rendered.cache_hit,
        elapsed_ms = started.elapsed().as_millis(),
        bytes = rendered.bytes.len(),
        "gamba wheel media ready"
    );
    Ok(InteractionAttachment::bytes("wheel.gif", rendered.bytes))
}

/// Render the explosion branch as a real GIF attachment. Discord upload
/// failures can fall back to the text response without touching the settled
/// wallet state.
fn render_explosion_attachment() -> Result<InteractionAttachment, String> {
    cama_app::wheel_media::render_explosion_gif()
        .map(|bytes| InteractionAttachment::bytes("explosion.gif", bytes))
}

/// Emit the exact wheel attachment used by the production provider for the
/// cross-language visual-equivalence runner.  Keeping this façade adjacent to
/// the provider's renderer prevents the harness from reimplementing
/// presentation or accidentally bypassing the upload-limit checks.
#[doc(hidden)]
pub fn render_wheel_attachment_for_visual_equivalence(
    wedges: &[cama_app::wheel::WheelWedge],
    target_index: usize,
    golden: bool,
    display_name: Option<&str>,
) -> Result<InteractionAttachment, String> {
    render_wheel_attachment(wedges, target_index, golden, display_name)
}

/// Emit the exact explosion attachment used by the production provider for
/// the cross-language visual-equivalence runner.
#[doc(hidden)]
pub fn render_explosion_attachment_for_visual_equivalence() -> Result<InteractionAttachment, String>
{
    render_explosion_attachment()
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
    /// Python chooses the final wedge before rendering, so retain the name
    /// needed to render the selected animation after a button click.
    display_name: Option<String>,
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
    /// Settlement progress shared by every clone of this interaction, so a
    /// retried click or timeout sweep resumes the first attempt.
    attempt: Arc<Mutex<WheelAttemptMemo>>,
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
    embeds: Vec<InteractionEmbed>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GambaResponseRoute {
    Initial,
    DeferredOriginal,
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
    ResolveTimeout(String, PendingWheelInteraction, WheelOption, bool),
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
    /// Voter names, resolved once when the audit opens.
    voter_names: GuildPlayerNameDirectory,
    page: usize,
    created_at: Instant,
}

#[derive(Clone, Debug)]
struct PendingBalanceView {
    user_id: i64,
    guild_id: i64,
    display_name: String,
    current_page: usize,
    created_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BalancePageDirection {
    Previous,
    Next,
}

impl BalancePageDirection {
    fn apply(self, current_page: usize) -> usize {
        match self {
            Self::Previous => current_page.saturating_sub(1).max(1),
            Self::Next => current_page.saturating_add(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BalancePageSection {
    Overview,
    Markets { page: usize, total_pages: usize },
    Activity { page: usize, total_pages: usize },
}

#[derive(Clone, Debug)]
struct BalanceStatement {
    snapshot: TaxPlayerSnapshot,
    page: usize,
    total_pages: usize,
    section: BalancePageSection,
}

struct BettingInteractionHandler {
    database_path: PathBuf,
    tax_repository: TaxRepository,
    config: BettingRuntimeConfig,
    discord: Arc<dyn DiscordTransport>,
    vanity_tax: Arc<dyn BettingVanityTaxPort>,
    neon: Arc<Mutex<NeonDegenService>>,
    /// Schedules the wheel-ready reminder after every settled spin.
    wheel_reminders: OnceLock<Arc<dyn WheelReminderPort>>,
    wager_refresh: Mutex<Option<Arc<dyn BettingWagerRefreshPort>>>,
    rates: Mutex<BTreeMap<RateKey, Vec<Instant>>>,
    wheel_interactions: Mutex<BTreeMap<String, PendingWheelInteraction>>,
    /// Generation/claim keys for component and timeout resolution. Keeping a
    /// pending entry while SQLite is in flight allows a transient database
    /// failure to retry without permitting a concurrent double settlement.
    wheel_in_flight: Mutex<BTreeSet<String>>,
    disburse_vote_pages: Mutex<BTreeMap<String, PendingDisburseVotes>>,
    balance_views: Mutex<BTreeMap<u64, PendingBalanceView>>,
}

struct CachedDiscordWheelPlayerNames {
    discord: Arc<dyn DiscordTransport>,
    guild_id: i64,
}

const WHEEL_MEMBER_LOOKUP_CONCURRENCY: usize = 8;

async fn resolve_wheel_member_snapshot<F, Fut>(
    ids: Vec<i64>,
    cached_members: Option<BTreeSet<u64>>,
    synthetic_members_enabled: bool,
    lookup: F,
) -> Result<BTreeSet<i64>, String>
where
    F: Fn(u64) -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>> + Send + 'static,
{
    let mut visible = BTreeSet::new();
    let mut lookups = tokio::task::JoinSet::new();
    for id in ids {
        if is_enabled_synthetic_wheel_member(id, synthetic_members_enabled) {
            visible.insert(id);
            continue;
        }
        let Ok(user_id) = u64::try_from(id) else {
            continue;
        };
        if let Some(members) = &cached_members {
            if members.contains(&user_id) {
                visible.insert(id);
            }
            continue;
        }
        if lookups.len() == WHEEL_MEMBER_LOOKUP_CONCURRENCY {
            let (id, present) = lookups
                .join_next()
                .await
                .expect("member lookup batch is nonempty")
                .map_err(|error| error.to_string())?;
            if present? {
                visible.insert(id);
            }
        }
        let lookup = lookup(user_id);
        lookups.spawn(async move { (id, lookup.await) });
    }
    while let Some(result) = lookups.join_next().await {
        let (id, present) = result.map_err(|error| error.to_string())?;
        if present? {
            visible.insert(id);
        }
    }
    Ok(visible)
}

impl CachedDiscordWheelPlayerNames {
    fn new(discord: Arc<dyn DiscordTransport>, guild_id: i64) -> Self {
        Self { discord, guild_id }
    }
}

impl WheelPlayerNameSource for CachedDiscordWheelPlayerNames {
    fn names_for(&self, player_ids: &[i64]) -> BTreeMap<i64, String> {
        let discord_ids = player_ids
            .iter()
            .filter_map(|player_id| u64::try_from(*player_id).ok())
            .collect::<Vec<_>>();
        let names = u64::try_from(self.guild_id).map_or_else(
            |_| GuildPlayerNameDirectory::default(),
            |guild_id| cached_guild_player_names(self.discord.as_ref(), guild_id, &discord_ids),
        );
        player_ids
            .iter()
            .map(|player_id| (*player_id, names.resolve(*player_id)))
            .collect()
    }
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
    async fn render_player_name(&self, user_id: i64, guild_id: Option<i64>) -> String {
        self.render_player_names(&[user_id], guild_id)
            .await
            .remove(&user_id)
            .unwrap_or_else(|| UNKNOWN_PLAYER.to_owned())
    }

    async fn render_player_names(
        &self,
        user_ids: &[i64],
        guild_id: Option<i64>,
    ) -> BTreeMap<i64, String> {
        let names = resolve_guild_player_names(
            self.discord.as_ref(),
            guild_id.and_then(|guild_id| u64::try_from(guild_id).ok()),
            user_ids,
        )
        .await;
        user_ids
            .iter()
            .map(|user_id| (*user_id, names.resolve(*user_id)))
            .collect()
    }

    /// Cache and last-seen names for replies sent without a deferral, which
    /// cannot wait on a member fetch.
    fn cached_player_names(
        &self,
        guild_id: i64,
        user_ids: impl IntoIterator<Item = i64>,
    ) -> GuildPlayerNameDirectory {
        let user_ids = user_ids
            .into_iter()
            .filter_map(|user_id| u64::try_from(user_id).ok())
            .collect::<Vec<_>>();
        u64::try_from(guild_id).map_or_else(
            |_| GuildPlayerNameDirectory::default(),
            |guild_id| cached_guild_player_names(self.discord.as_ref(), guild_id, &user_ids),
        )
    }

    async fn expire_pending_views(&self) -> Result<usize, String> {
        self.balance_views
            .lock()
            .map_err(|_| "balance view state is poisoned".to_owned())?
            .retain(|_, view| view.created_at.elapsed() <= BALANCE_VIEW_TTL);
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
            let (option, use_reroll) =
                pin_wheel_selection(&pending.attempt, timeout_wheel_option(&pending), false);
            let option_for_settlement = option.clone();
            let attempt = Arc::clone(&pending.attempt);
            let member_snapshot = self.guild_member_snapshot(pending.guild_id).await;
            let player_names =
                CachedDiscordWheelPlayerNames::new(Arc::clone(&self.discord), pending.guild_id);
            let reminders = self.wheel_reminders();
            let result = sqlite("expired wheel interaction sweep", move || {
                // Low-priority eligibility is a SQLite read, so it resolves on
                // the blocking side with the rest of the settlement.
                let low_priority_taxable_ids = LowPriorityRepository::new(&path)
                    .active_taxable_ids(Some(pending.guild_id))
                    .map_err(|error| error.to_string())?;
                resolve_pending_wheel(
                    &path,
                    pending.guild_id,
                    pending.user_id,
                    pending.balance_before,
                    pending.spin_kind,
                    &effects,
                    &config,
                    &event_id,
                    &option_for_settlement,
                    unix_seconds()?,
                    use_reroll,
                    pending.bonus_spin,
                    pending.last_regular_spin,
                    pending.event_win_multiplier,
                    pending.event_loss_multiplier,
                    &vanity_taxable_ids,
                    &low_priority_taxable_ids,
                    member_snapshot.as_ref(),
                    &player_names,
                    &attempt,
                    reminders.as_ref(),
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
                        let attachment = wheel_attachment_for_option(&pending, &option)
                            .await
                            .or_else(|| pending.initial_attachment.clone());
                        let _ = original_responder
                            .edit_original(wheel_timeout_response(
                                &pending, &option, &result, attachment,
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
            interaction_id,
            name,
            user_id,
            user_display_name: _,
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
                self.balance(interaction_id, user_id, guild_id, channel_id, &responder)
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
                self.tip(user_id, guild_id, channel_id, &options, &responder)
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
        if custom_id.starts_with(BALANCE_COMPONENT_PREFIX) {
            let (session_id, direction) = parse_balance_component(&custom_id)?;
            let guild_id = guild_id
                .map(|value| signed_id(value, "guild"))
                .transpose()?
                .ok_or_else(|| GUILD_ONLY_MESSAGE.to_owned())?;
            let user_id = signed_id(user_id, "user")?;
            return self
                .balance_component(session_id, direction, user_id, guild_id, &responder)
                .await;
        }
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
        if let Some(ballot) = custom_id.strip_prefix(DISBURSE_COMPONENT_PREFIX) {
            let Some((proposal_id, method)) = ballot
                .split_once(':')
                .and_then(|(id, method)| id.parse::<i64>().ok().map(|id| (id, method)))
            else {
                return respond_ephemeral(&responder, "This Reserve proposal is no longer valid.")
                    .await;
            };
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
                    .add_vote(Some(guild_id), proposal_id, user_id, &method)
                    .map_err(|error| error.to_string())?;
                Ok((proposal, method.to_owned()))
            })
            .await;
            let (proposal, method) = match result {
                Ok(value) => value,
                Err(error) => return respond_ephemeral(&responder, &error).await,
            };
            if proposal.quorum_reached() {
                let result = match self.execute_disbursement(guild_id, &proposal, true).await {
                    Ok(result) => result,
                    Err(error) => return respond_ephemeral(&responder, &error).await,
                };
                self.update_persistent_disbursement_message(
                    &proposal,
                    disbursement_proposal_response_with_disabled_buttons(&proposal),
                )
                .await;
                let names = self.cached_player_names(
                    guild_id,
                    result
                        .distributions
                        .iter()
                        .map(|(discord_id, _)| *discord_id),
                );
                return responder
                    .respond(disbursement_response(&result, false, &names))
                    .await
                    .map_err(|error| error.to_string());
            }
            self.publish_open_disbursement(&proposal).await?;
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
                let (option, use_reroll) =
                    pin_wheel_selection(&pending.attempt, timeout_wheel_option(&pending), false);
                break 'lookup Ok(WheelComponentDecision::ResolveTimeout(
                    key,
                    pending.clone(),
                    option,
                    use_reroll,
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
            WheelComponentDecision::ResolveTimeout(key, pending, option, use_reroll) => {
                // Discord invalidates component tokens unless the initial
                // acknowledgement arrives within three seconds. Rendering the
                // selected wheel GIF is CPU-heavy, so acknowledge before any
                // media work and render outside the async executor.
                responder
                    .defer(false)
                    .await
                    .map_err(|error| error.to_string())?;
                let original_responder = pending.original_responder.clone();
                let final_attachment = wheel_attachment_for_option(&pending, &option)
                    .await
                    .or_else(|| pending.initial_attachment.clone());
                // A late click still receives only a component acknowledgement.
                // Keep the timeout result on the public wheel message, matching
                // the normal choice path instead of creating a private duplicate.
                let path = self.database_path.clone();
                let config = pending.config.clone();
                let event_id = pending.event_id.clone();
                let effects = pending.effects.clone();
                let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
                let option_for_settlement = option.clone();
                let attempt = Arc::clone(&pending.attempt);
                let member_snapshot = self.guild_member_snapshot(guild_id).await;
                let player_names =
                    CachedDiscordWheelPlayerNames::new(Arc::clone(&self.discord), guild_id);
                let reminders = self.wheel_reminders();
                let result = sqlite("expired wheel interaction resolution", move || {
                    // Low-priority eligibility is a SQLite read, so it resolves
                    // on the blocking side with the rest of the settlement.
                    let low_priority_taxable_ids = LowPriorityRepository::new(&path)
                        .active_taxable_ids(Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    resolve_pending_wheel(
                        &path,
                        guild_id,
                        pending.user_id,
                        pending.balance_before,
                        pending.spin_kind,
                        &effects,
                        &config,
                        &event_id,
                        &option_for_settlement,
                        unix_seconds()?,
                        use_reroll,
                        pending.bonus_spin,
                        pending.last_regular_spin,
                        pending.event_win_multiplier,
                        pending.event_loss_multiplier,
                        &vanity_taxable_ids,
                        &low_priority_taxable_ids,
                        member_snapshot.as_ref(),
                        &player_names,
                        &attempt,
                        reminders.as_ref(),
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
                if let Some(original_responder) = original_responder {
                    let animation_delivered = if let Some(attachment) = final_attachment.clone() {
                        original_responder
                            .edit_original(wheel_completion_response("", None, Some(attachment)))
                            .await
                            .is_ok()
                    } else {
                        false
                    };
                    if animation_delivered {
                        tokio::time::sleep(BETTING_WHEEL_DISPLAY_DELAY).await;
                    }
                    let timeout = if animation_delivered {
                        wheel_timeout_response(&pending, &option, &result, None)
                            .preserve_attachments()
                    } else {
                        wheel_timeout_response(&pending, &option, &result, final_attachment)
                    };
                    original_responder
                        .edit_original(timeout)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                self.emit_pending_neon(channel_id, user_id, guild_id, &result)
                    .await;
                return Ok(());
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
                let (option, use_reroll) =
                    pin_wheel_selection(&pending.attempt, option, use_reroll);
                (key, pending, option, use_reroll)
            }
        };
        let (key, pending, option, use_reroll) = pending;
        // Python's button callback only acknowledges the component. The
        // selected result is revealed on the public wheel message after its
        // animation, never as an immediate private duplicate. This must happen
        // before selected-wedge GIF rendering to preserve Discord's three-second
        // interaction token window.
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let original_responder = pending.original_responder.clone();
        let final_attachment = wheel_attachment_for_option(&pending, &option)
            .await
            .or_else(|| pending.initial_attachment.clone());
        let path = self.database_path.clone();
        let config = pending.config.clone();
        let event_id = pending.event_id.clone();
        let event_win_multiplier = pending.event_win_multiplier;
        let event_loss_multiplier = pending.event_loss_multiplier;
        let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
        let member_snapshot = self.guild_member_snapshot(guild_id).await;
        let player_names = CachedDiscordWheelPlayerNames::new(Arc::clone(&self.discord), guild_id);
        let reminders = self.wheel_reminders();
        let result = sqlite("wheel interaction resolution", move || {
            // Low-priority eligibility is a SQLite read, so it resolves on the
            // blocking side with the rest of the settlement.
            let low_priority_taxable_ids = LowPriorityRepository::new(&path)
                .active_taxable_ids(Some(guild_id))
                .map_err(|error| error.to_string())?;
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
                &low_priority_taxable_ids,
                member_snapshot.as_ref(),
                &player_names,
                &pending.attempt,
                reminders.as_ref(),
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
        if let Some(original_responder) = original_responder {
            let animation_delivered = if let Some(attachment) = final_attachment.clone() {
                original_responder
                    .edit_original(wheel_completion_response("", None, Some(attachment)))
                    .await
                    .is_ok()
            } else {
                false
            };
            if animation_delivered {
                tokio::time::sleep(BETTING_WHEEL_DISPLAY_DELAY).await;
            }
            let completion = if animation_delivered {
                wheel_completion_response("", Some(result.completion_embed.clone()), None)
                    .preserve_attachments()
            } else {
                wheel_completion_response(
                    "",
                    Some(result.completion_embed.clone()),
                    final_attachment,
                )
            };
            original_responder
                .edit_original(completion)
                .await
                .map_err(|error| error.to_string())?;
        }
        self.emit_pending_neon(channel_id, user_id, guild_id, &result)
            .await;
        Ok(())
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
                    let seed = betting_seed_without_lobby_bonus(
                        SeedSplit {
                            radiant: pending.state.bet_seed_radiant,
                            dire: pending.state.bet_seed_dire,
                            bonus: pending.state.bet_seed_bonus,
                        },
                        pending.state.first_game_pool_reserved,
                        if pending.state.betting_mode == "pool" {
                            BettingMode::Pool
                        } else {
                            BettingMode::House
                        },
                    );
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
                                BettingTeam::Radiant => seed.dire,
                                BettingTeam::Dire => seed.radiant,
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
                        if seed.bonus > 0 && my_team_total > 0 {
                            bonus = seed.bonus.saturating_mul(total_effective) / my_team_total;
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
        let (totals, recorded) = sqlite("betting pool totals", move || {
            let totals = BettingServiceRepository::new(&path)
                .get_pending_totals(Some(guild_id), 0, Some(pending_match_id))
                .map_err(|error| error.to_string())?;
            let recorded = MatchRepository::new(path)
                .match_id_for_pending_match(guild_id, pending_match_id)
                .map_err(|error| error.to_string())?
                .is_some();
            Ok((totals, recorded))
        })
        .await?;
        let lock_until = pending.state.bet_lock_until.or(pending.state.lock_time);
        let betting_open = !recorded
            && pending
                .state
                .betting_open(unix_seconds().unwrap_or_default());
        let total = totals.total();
        let mode = if pending.state.betting_mode == "house" {
            BettingMode::House
        } else {
            BettingMode::Pool
        };
        let lobby_bonus = pending.state.first_game_pool_reserved;
        let seed = betting_seed_without_lobby_bonus(
            SeedSplit {
                radiant: pending.state.bet_seed_radiant,
                dire: pending.state.bet_seed_dire,
                bonus: pending.state.bet_seed_bonus,
            },
            lobby_bonus,
            mode,
        );
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
        if !betting_open {
            odds_text.push_str("\nBetting is closed.");
        } else if let Some(lock) = lock_until {
            odds_text.push_str(&format!("\nBetting closes <t:{lock}:R>"));
        }
        let mut fields = vec![("Current Odds".to_owned(), odds_text)];
        if lobby_bonus > 0 {
            fields.push((
                "Lobby Bonus".to_owned(),
                format!("{lobby_bonus} {JOPACOIN_EMOTE} shared equally among match participants."),
            ));
        }
        let bettor_names = if is_admin || !betting_open {
            self.render_player_names(
                &bets.iter().map(|bet| bet.discord_id).collect::<Vec<_>>(),
                Some(guild_id),
            )
            .await
        } else {
            BTreeMap::new()
        };
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
            let lines = rows
                .iter()
                .enumerate()
                .map(|(index, bet)| {
                    let bettor = if is_admin || !betting_open {
                        bettor_names
                            .get(&bet.discord_id)
                            .cloned()
                            .unwrap_or_else(|| UNKNOWN_PLAYER.to_owned())
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
            fields.extend(bounded_bet_team_fields(label, rows.len(), &lines));
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
        fields.push((
            summary_name.to_owned(),
            format!(
                "**{summary_total_label}:** {total} {} effective\nRadiant: {} ({:.0}%) | Dire: {} ({:.0}%)",
                JOPACOIN_EMOTE,
                totals.radiant,
                pct(totals.radiant, total),
                totals.dire,
                pct(totals.dire, total)
            ),
        ));
        for embed in paginated_bet_embeds(&overview.title, fields) {
            responder
                .followup(InteractionResponse::message("").embed(embed).ephemeral())
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn balance(
        &self,
        interaction_id: u64,
        user_id: i64,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(responder, GUILD_ONLY_MESSAGE).await;
        };
        if !self.take_rate("balance", guild_id, user_id, 5, DEFAULT_RATE_WINDOW)? {
            return respond_ephemeral(responder, "Please wait before using `/balance` again.")
                .await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let display_name = self.render_player_name(user_id, Some(guild_id)).await;
        let tax = self.tax_repository.clone();
        let path = self.database_path.clone();
        let loaded = sqlite("betting balance portfolio read", move || {
            let statement = load_balance_statement(&tax, user_id, guild_id, 1)?;
            let effects = active_mana_effects(&path, user_id, Some(guild_id), unix_seconds()?)
                .unwrap_or_default();
            Ok::<_, String>((statement, effects))
        })
        .await;
        let (statement, effects) = match loaded {
            Ok(loaded) => loaded,
            Err(_) => {
                return followup_ephemeral(
                    responder,
                    "Couldn't load your Jopacoin portfolio right now.",
                )
                .await;
            }
        };
        let Some(statement) = statement else {
            return followup_ephemeral(
                responder,
                "You need to /player register before checking your balance.",
            )
            .await;
        };
        let badge = format_mana_badge(Some(&effects));
        let response = balance_statement_response(
            interaction_id,
            &display_name,
            &statement,
            badge,
            self.config.bankruptcy_penalty_rate_per_game,
            self.config.garnishment_rate,
        );
        let result = responder
            .followup(response)
            .await
            .map_err(|error| error.to_string());
        if result.is_ok() {
            self.balance_views
                .lock()
                .map_err(|_| "balance view state is poisoned".to_owned())?
                .insert(
                    interaction_id,
                    PendingBalanceView {
                        user_id,
                        guild_id,
                        display_name,
                        current_page: statement.page,
                        created_at: Instant::now(),
                    },
                );
            self.emit_balance_neon(channel_id, user_id, guild_id, statement.snapshot.balance)
                .await;
        }
        result
    }

    async fn balance_component(
        &self,
        session_id: u64,
        direction: BalancePageDirection,
        user_id: i64,
        guild_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let view = self
            .balance_views
            .lock()
            .map_err(|_| "balance view state is poisoned".to_owned())?
            .get(&session_id)
            .cloned();
        let Some(view) = view else {
            return respond_ephemeral(
                responder,
                "This balance statement expired. Run `/balance` again.",
            )
            .await;
        };
        if view.created_at.elapsed() > BALANCE_VIEW_TTL {
            self.balance_views
                .lock()
                .map_err(|_| "balance view state is poisoned".to_owned())?
                .remove(&session_id);
            return respond_ephemeral(
                responder,
                "This balance statement expired. Run `/balance` again.",
            )
            .await;
        }
        if view.user_id != user_id || view.guild_id != guild_id {
            return respond_ephemeral(
                responder,
                "This private balance statement belongs to another player.",
            )
            .await;
        }
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let requested_page = direction.apply(view.current_page);
        let tax = self.tax_repository.clone();
        let path = self.database_path.clone();
        let loaded = sqlite("betting balance page read", move || {
            let statement = load_balance_statement(&tax, user_id, guild_id, requested_page)?;
            let effects = active_mana_effects(&path, user_id, Some(guild_id), unix_seconds()?)
                .unwrap_or_default();
            Ok::<_, String>((statement, effects))
        })
        .await;
        let (statement, effects) = match loaded {
            Ok(loaded) => loaded,
            Err(_) => {
                return followup_ephemeral(
                    responder,
                    "Couldn't refresh your Jopacoin portfolio right now.",
                )
                .await;
            }
        };
        let Some(statement) = statement else {
            return followup_ephemeral(
                responder,
                "Your player registration is no longer available in this server.",
            )
            .await;
        };
        let badge = format_mana_badge(Some(&effects));
        responder
            .edit_original(balance_statement_response(
                session_id,
                &view.display_name,
                &statement,
                badge,
                self.config.bankruptcy_penalty_rate_per_game,
                self.config.garnishment_rate,
            ))
            .await
            .map_err(|error| error.to_string())?;
        if let Some(current) = self
            .balance_views
            .lock()
            .map_err(|_| "balance view state is poisoned".to_owned())?
            .get_mut(&session_id)
        {
            current.current_page = statement.page;
            current.created_at = Instant::now();
        }
        Ok(())
    }

    /// Run one Neon hook on the blocking pool. Hooks read and write SQLite
    /// through the event port and can render a multi-frame GIF while holding
    /// the service lock, none of which may stall a Tokio worker.
    async fn run_neon<T, F>(&self, hook: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut NeonDegenService) -> T + Send + 'static,
    {
        let neon = Arc::clone(&self.neon);
        sqlite("betting Neon hook", move || {
            let mut neon = neon
                .lock()
                .map_err(|_| "betting Neon lock poisoned".to_owned())?;
            Ok(hook(&mut neon))
        })
        .await
    }

    /// The reminder port a spin settlement schedules through.
    fn wheel_reminders(&self) -> Arc<dyn WheelReminderPort> {
        self.wheel_reminders
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(NoWheelReminders))
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
        let result = self
            .run_neon(move |neon| {
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
            })
            .await
            .ok()
            .flatten();
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
        let result = self
            .run_neon(move |neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                neon.on_lightning_bolt(discord_id, Some(guild_id), total_taxed, players_hit)
            })
            .await
            .ok()
            .flatten();
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
        let result = self
            .run_neon(move |neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                neon.on_degen_milestone(discord_id, Some(guild_id), score)
            })
            .await
            .ok()
            .flatten();
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
            .run_neon(move |neon| {
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
            .await
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
        let result = self
            .run_neon(move |neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                neon.on_balance_check(discord_id, Some(guild_id), balance)
            })
            .await
            .ok()
            .flatten();
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
        let scope = scope.to_owned();
        let now = unix_seconds().unwrap_or_default();
        let result = self
            .run_neon(move |neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                neon.on_cooldown_hit(discord_id, Some(guild_id), &scope)
            })
            .await
            .ok()
            .flatten();
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
        let (sender_name, recipient_name) = (sender_name.to_owned(), recipient_name.to_owned());
        let now = unix_seconds().unwrap_or_default();
        let result = self
            .run_neon(move |neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                neon.on_tip(
                    discord_id,
                    Some(guild_id),
                    &sender_name,
                    &recipient_name,
                    amount,
                    fee,
                )
            })
            .await
            .ok()
            .flatten();
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
        let result = self
            .run_neon(move |neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                neon.on_loan(discord_id, Some(guild_id), amount, total_owed, is_negative)
            })
            .await
            .ok()
            .flatten();
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
    }

    async fn emit_bankruptcy_neon(
        &self,
        channel_id: Option<u64>,
        user_id: i64,
        user_display_name: &str,
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
        let user_display_name = user_display_name.to_owned();
        let now = unix_seconds().unwrap_or_default();
        let result = self
            .run_neon(move |neon| {
                neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                neon.on_bankruptcy_named(
                    discord_id,
                    Some(guild_id),
                    &user_display_name,
                    debt_cleared,
                    filing_number,
                )
            })
            .await
            .ok()
            .flatten();
        let fired = result.is_some();
        if let Some(result) = result {
            self.emit_neon_result(channel_id, result).await;
        }
        fired
    }

    #[allow(clippy::too_many_arguments)]
    async fn gamba(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        channel_id: Option<u64>,
        member_permissions: u64,
        responder: &Arc<dyn InteractionResponder>,
        bonus_spin: bool,
    ) -> Result<(), String> {
        let started = Instant::now();
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
            if !self.discord.channel_is_gamba(guild_raw, channel_id).await? {
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
                let result = self
                    .run_neon(move |neon| {
                        neon.set_now_seconds(u64::try_from(now.max(0)).unwrap_or_default());
                        neon.on_cooldown_hit(discord_id, Some(guild_id), "gamba")
                    })
                    .await
                    .ok()
                    .flatten();
                if let Some(result) = result
                    && let Some(channel_id) = channel_id
                {
                    self.emit_neon_result(channel_id, result).await;
                }
            }
            return response;
        }
        let admin_bypass = self.config.admin_user_ids.contains(&user_id)
            || member_permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0;
        let now = unix_seconds()?;
        if !bonus_spin {
            let path = self.database_path.clone();
            let preflight = sqlite("gamba interaction preflight", move || {
                gamba_interaction_preflight(&path, guild_id, user_id, now, admin_bypass)
            })
            .await?;
            if let Some(message) = preflight {
                return respond_ephemeral(responder, &message).await;
            }
        }
        let response_route = begin_gamba_response(responder, bonus_spin).await?;
        let preflight_ms = started.elapsed().as_millis() as u64;
        let preparation_started = Instant::now();
        let user_display_name = self.render_player_name(user_id, Some(guild_id)).await;
        let config = self.config.clone();
        let path = self.database_path.clone();
        let event_config = economy_event_config(&config);
        let event_path = self.database_path.clone();
        let vanity_taxable_ids = self.vanity_tax.taxable_ids(guild_id);
        let wheel_display_name = user_display_name.clone();
        // Membership may need Discord I/O; daily effects are independent SQLite
        // work and can finish while those requests are in flight.
        let player_names = CachedDiscordWheelPlayerNames::new(Arc::clone(&self.discord), guild_id);
        let (member_snapshot, event_multipliers) = tokio::join!(
            self.guild_member_snapshot(guild_id),
            sqlite("gamba economy-event effects", move || {
                let service = SqliteEconomyEventService::new(event_path, event_config);
                service
                    .effects_at(guild_id, now)
                    .map(|effects| (effects.gamba_win_multiplier, effects.gamba_loss_multiplier))
                    .map_err(|error| error.to_string())
            })
        );
        let (event_win_multiplier, event_loss_multiplier) = event_multipliers.unwrap_or((1.0, 1.0));
        let preparation_ms = preparation_started.elapsed().as_millis() as u64;
        let spin_started = Instant::now();
        let reminders = self.wheel_reminders();
        let outcome = sqlite("gamba spin", move || {
            // Low-priority eligibility is a SQLite read, so it resolves on the
            // blocking side with the rest of the spin.
            let low_priority_taxable_ids = LowPriorityRepository::new(&path)
                .active_taxable_ids(Some(guild_id))
                .map_err(|error| error.to_string())?;
            spin_once(
                &path,
                guild_id,
                user_id,
                now,
                &config,
                event_win_multiplier,
                event_loss_multiplier,
                &vanity_taxable_ids,
                &low_priority_taxable_ids,
                admin_bypass,
                bonus_spin,
                member_snapshot.as_ref(),
                Some(&wheel_display_name),
                &player_names,
                reminders.as_ref(),
            )
        })
        .await?;
        let spin_ms = spin_started.elapsed().as_millis() as u64;
        let delivery_started = Instant::now();
        let completion_message = outcome.completion_message.clone();
        let completion_embed = outcome.completion_embed.clone();
        let completion_delay = outcome.completion_delay;
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
        for embed in outcome.embeds {
            response = response.embed(embed);
        }
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
        let delivery = deliver_gamba_response(
            responder,
            response,
            completion_embed.clone(),
            response_route,
        )
        .await;
        let attachment_delivered = delivery.as_ref().copied().unwrap_or(false);
        tracing::info!(
            user_id,
            guild_id,
            bonus_spin,
            preflight_ms,
            preparation_ms,
            spin_ms,
            delivery_ms = delivery_started.elapsed().as_millis() as u64,
            total_ms = started.elapsed().as_millis() as u64,
            delivered = delivery.is_ok(),
            attachment_delivered,
            "gamba initial delivery completed"
        );
        if delivery.is_ok()
            && let Some(pending_key) = pending_key
            && let Ok(mut interactions) = self.wheel_interactions.lock()
            && let Some(pending) = interactions.get_mut(&pending_key)
        {
            pending.original_responder = Some(Arc::clone(responder));
            if response_has_attachment && !attachment_delivered {
                pending.initial_attachment = None;
            }
        }
        if delivery.is_ok()
            && let Some(completion_message) = completion_message
        {
            // Python edits the attachment-only post with an embed and does
            // not send replacement content or re-upload the GIF.  The
            // settlement already committed before this delivery boundary; an
            // edit failure therefore degrades to the initial attachment.
            tokio::time::sleep(completion_delay).await;
            let completion = wheel_completion_response(completion_message, completion_embed, None)
                .preserve_attachments();
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
        delivery.map(drop)
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
        let cached_members = self.discord.cached_guild_member_ids(guild).ok().flatten();
        let result = resolve_wheel_member_snapshot(
            ids,
            cached_members,
            self.config.synthetic_members_enabled,
            |user_id| {
                let discord = Arc::clone(&self.discord);
                async move {
                    discord
                        .guild_member(guild, user_id)
                        .await
                        .map(|member| member.is_some())
                }
            },
        )
        .await;
        match result {
            Ok(visible) => Some(visible),
            Err(error) => {
                tracing::debug!(%error, guild_id, "gamba member snapshot unavailable");
                None
            }
        }
    }

    async fn tip(
        &self,
        user_id: i64,
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
        let (fee, tithe) = tip_fee_and_tithe(amount, self.config.tip_fee_rate, &effects);
        let rendered_names = self
            .render_player_names(&[user_id, target], Some(guild_id))
            .await;
        let user_display_name = rendered_names
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| UNKNOWN_PLAYER.to_owned());
        let target_name = rendered_names
            .get(&target)
            .cloned()
            .unwrap_or_else(|| UNKNOWN_PLAYER.to_owned());
        let unregistered_target_name = target_name.clone();
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
                return Err(format!("{unregistered_target_name} is not registered."));
            }
            TipRepository::new(&path)
                .tip_atomic(Some(guild_id), user_id, target, amount, fee, tithe)
                .map_err(|error| match error {
                    TipRepositoryError::OutstandingLoan { outstanding_total } => format!(
                        "You cannot tip while you have an outstanding loan. Play a match to repay your loan ({outstanding_total} {JOPACOIN_EMOTE})."
                    ),
                    error => error.to_string(),
                })
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
                        "{prefix}{user_display_name} tipped {amount} {JOPACOIN_EMOTE} to {target_name}! \
                         ({fee} {JOPACOIN_EMOTE} fee to Jopacoin Reserve){suffix}"
                    )).without_mentions())
                    .await
                    .map_err(|error| error.to_string());
                if response.is_ok() {
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
            Ok(payment) => {
                let names = self
                    .render_player_names(&[user_id, target], Some(guild_id))
                    .await;
                responder
                    .followup(
                        InteractionResponse::message(format!(
                            "{} paid {} {} toward {}'s debt!",
                            names[&user_id], payment.amount_paid, JOPACOIN_EMOTE, names[&target]
                        ))
                        .without_mentions(),
                    )
                    .await
                    .map_err(|error| error.to_string())
            }
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
        let user_display_name = self.render_player_name(user_id, guild_id).await;
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
            penalty_games: bankruptcy_penalty_games(self.config.bankruptcy_penalty_games, &effects),
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
                new_balance,
                penalty_games,
                bankruptcy_count,
            } => {
                neon_data = Some((debt_cleared, bankruptcy_count));
                let per_game = self.config.bankruptcy_penalty_rate_per_game;
                let withheld_now =
                    cama_domain::bankruptcy::withheld_rate(per_game, penalty_games) * 100.0;
                let per_game = per_game * 100.0;
                format!(
                    "**{user_display_name} HAS DECLARED BANKRUPTCY**\n\n\
                     **Details:**\n\
                     Debt cleared: {debt_cleared} {JOPACOIN_EMOTE}\n\
                     Penalty: {withheld_now:.0}% of profit withheld \
                     ({per_game:.0}% per game left) \
                     until you **WIN** {penalty_games} games\n\
                     New balance: {new_balance} {JOPACOIN_EMOTE}"
                )
            }
            DeclarationOutcome::PlayerNotFound => {
                "You need to /player register before you can declare bankruptcy.".to_owned()
            }
            DeclarationOutcome::NotInDebt { balance } => format!(
                "{user_display_name} tried to declare bankruptcy, but has no debt.\n\
                 Their balance: {balance} {JOPACOIN_EMOTE}"
            ),
            DeclarationOutcome::Cooldown { remaining_seconds } => format!(
                "{user_display_name} cannot file again yet. Try again <t:{}:R>.",
                unix_seconds()?.saturating_add(remaining_seconds)
            ),
        };
        let response = responder
            .followup(InteractionResponse::message(content).without_mentions())
            .await
            .map_err(|error| error.to_string());
        if response.is_ok()
            && let Some((debt_cleared, filing_number)) = neon_data
        {
            let fired = self
                .emit_bankruptcy_neon(
                    channel_id,
                    user_id,
                    &user_display_name,
                    guild_id,
                    debt_cleared,
                    filing_number,
                )
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
        // Defer before the registration lookup: slash commands get no
        // automatic acknowledgement.
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let registration_path = self.database_path.clone();
        let registered = sqlite("loan registration", move || {
            PlayerRepository::new(registration_path)
                .exists(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        if !registered {
            return followup_ephemeral(
                responder,
                "You need to /player register before taking loans.",
            )
            .await;
        }
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
        let policy = LoanPolicy {
            cooldown_seconds: self.config.loan_cooldown_seconds,
            max_amount: self.config.loan_max_amount,
            fee_rate: self.config.loan_fee_rate,
            max_debt: self.config.max_debt,
        };
        let path = self.database_path.clone();
        let result = sqlite("economy loan", move || {
            LoanService::new(LoanRepository::new(path), policy)
                .execute_loan(
                    user_id,
                    amount,
                    guild_id,
                    Some(modifiers.fee_rate),
                    Some(modifiers.limit),
                )
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
                let names = self.cached_player_names(
                    guild_id.unwrap_or_default(),
                    last.recipients
                        .iter()
                        .take(3)
                        .map(|(discord_id, _)| *discord_id),
                );
                let mut lines = last
                    .recipients
                    .iter()
                    .take(3)
                    .map(|(discord_id, amount)| {
                        format!("{}: +{amount}", names.resolve(*discord_id))
                    })
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
                // This reply is not deferred, so names come from the cache and
                // last-seen archive rather than a member fetch.
                let names = u64::try_from(guild_id).map_or_else(
                    |_| GuildPlayerNameDirectory::default(),
                    |guild_id| {
                        cached_guild_player_names(
                            self.discord.as_ref(),
                            guild_id,
                            &positions
                                .iter()
                                .filter_map(|position| u64::try_from(position.target_id).ok())
                                .collect::<Vec<_>>(),
                        )
                    },
                );
                let mut content = String::from("**Configured investments**\n");
                for (index, position) in positions.iter().enumerate() {
                    content.push_str(&format!(
                        "{}. {} {} — {}%\n",
                        index + 1,
                        position.direction.to_ascii_uppercase(),
                        names.resolve(position.target_id),
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
                let target_name = self
                    .cached_player_names(guild_id, [target_id])
                    .resolve(target_id);
                let content = if removed {
                    format!("Removed your investment in {target_name}.")
                } else {
                    format!("You do not have an investment in {target_name}.")
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
                let target_name = self
                    .cached_player_names(guild_id, [target_id])
                    .resolve(target_id);
                let unregistered_target_name = target_name.clone();
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
                        &unregistered_target_name,
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
                            "Set a **{direction}** investment in {target_name} at **{percentage}%**. Configured total: **{total}% / 50%**."
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
        sqlite("reserve voting restriction", move || {
            SqliteEconomyEventService::new(path, event_config)
                .effects_at(guild_id, unix_seconds()?)
                .map(|effects| effects.reserve_voting_disabled)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn disburse(
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
                let minimum = self.config.disburse_min_fund;
                let result = sqlite("disbursement proposal", move || {
                    let players = PlayerRepository::new(&path);
                    let registered = players
                        .get_all(Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    if !registered.iter().any(|player| player.jopacoin_balance < 0) {
                        return Err("No players with negative balance to receive funds.".to_owned());
                    }
                    DisbursementRepository::new(&path)
                        .create_proposal_atomic(
                            Some(guild_id),
                            unix_seconds().map_err(|error| error.to_string())?,
                            minimum,
                        )
                        .map_err(|error| error.to_string())
                })
                .await;
                match result {
                    Ok(proposal) => {
                        self.send_persistent_disbursement_message(proposal, responder)
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
                if let (Some(channel_id), Some(message_id)) =
                    (proposal.channel_id, proposal.message_id)
                {
                    let message = format!(
                        "Reserve ballot: {}/{} votes. [Open the ballot](https://discord.com/channels/{guild_id}/{channel_id}/{message_id})",
                        proposal.total_votes(),
                        proposal.quorum_required,
                    );
                    return respond_ephemeral(responder, &message).await;
                }
                // A proposal whose original publication failed can still accept votes.
                responder
                    .respond(disbursement_proposal_response(&proposal).ephemeral())
                    .await
                    .map_err(|error| error.to_string())
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
                let voter_names =
                    self.cached_player_names(guild_id, votes.iter().map(|vote| vote.discord_id));
                let state = PendingDisburseVotes {
                    guild_id,
                    requester_id: user_id,
                    proposal,
                    votes,
                    voter_names,
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
                let result = self.execute_disbursement(guild_id, &proposal, false).await;
                match result {
                    Ok(result) => {
                        self.update_persistent_disbursement_message(
                            &proposal,
                            disbursement_proposal_response_with_disabled_buttons(&proposal),
                        )
                        .await;
                        let names = self.cached_player_names(
                            guild_id,
                            result
                                .distributions
                                .iter()
                                .map(|(discord_id, _)| *discord_id),
                        );
                        responder
                            .respond(disbursement_response(&result, true, &names))
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
        proposal: DisbursementProposal,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = responder
            .followup_with_receipt(disbursement_proposal_response(&proposal))
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
                .set_proposal_message(&proposal, message_id, channel_id)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn update_persistent_disbursement_message(
        &self,
        proposal: &DisbursementProposal,
        response: InteractionResponse,
    ) -> bool {
        let (Some(channel_id), Some(message_id)) = (proposal.channel_id, proposal.message_id)
        else {
            return true;
        };
        let (Ok(channel_id), Ok(message_id)) =
            (u64::try_from(channel_id), u64::try_from(message_id))
        else {
            return false;
        };
        if let Err(error) = self
            .discord
            .edit_message(
                channel_id,
                message_id,
                DiscordMessage::default_mentions(response).preserving_content(),
            )
            .await
        {
            warn!(%error, channel_id, message_id, "could not update disbursement ballot");
            return false;
        }
        true
    }

    async fn reconcile_disbursements(&self) -> Result<(), String> {
        let repository = DisbursementRepository::new(&self.database_path);
        let proposals = sqlite("open disbursement quorums", move || {
            repository
                .proposals_needing_refresh()
                .map_err(|error| error.to_string())
        })
        .await?;
        for proposal in proposals {
            if let Err(error) = self.reconcile_disbursement(&proposal).await {
                warn!(%error, guild_id = proposal.guild_id, proposal_id = proposal.proposal_id, "could not reconcile disbursement");
            }
        }
        Ok(())
    }

    async fn reconcile_disbursement(&self, proposal: &DisbursementProposal) -> Result<(), String> {
        if proposal.quorum_reached() {
            if self.reserve_voting_restricted(proposal.guild_id).await? {
                return Ok(());
            }
            let result = self
                .execute_disbursement(proposal.guild_id, proposal, true)
                .await?;
            let recipient_ids = result
                .distributions
                .iter()
                .map(|(discord_id, _)| *discord_id)
                .collect::<Vec<_>>();
            let names = resolve_guild_player_names(
                self.discord.as_ref(),
                u64::try_from(proposal.guild_id).ok(),
                &recipient_ids,
            )
            .await;
            let mut response = disbursement_proposal_response_with_disabled_buttons(proposal);
            response
                .embeds
                .extend(disbursement_response(&result, false, &names).embeds);
            self.update_persistent_disbursement_message(proposal, response)
                .await;
            return Ok(());
        }
        self.publish_open_disbursement(proposal).await
    }

    async fn publish_open_disbursement(
        &self,
        proposal: &DisbursementProposal,
    ) -> Result<(), String> {
        if self
            .update_persistent_disbursement_message(
                proposal,
                disbursement_proposal_response(proposal),
            )
            .await
        {
            let repository = DisbursementRepository::new(&self.database_path);
            let proposal = proposal.clone();
            sqlite("publish disbursement quorum", move || {
                repository
                    .record_published_quorum(&proposal)
                    .map_err(|error| error.to_string())
            })
            .await?;
        }
        Ok(())
    }

    async fn execute_disbursement(
        &self,
        guild_id: i64,
        proposal: &DisbursementProposal,
        require_quorum: bool,
    ) -> Result<DisbursementExecution, String> {
        let method = winning_disbursement_method(&proposal.votes)
            .ok_or_else(|| "No votes have been cast yet".to_owned())?
            .to_owned();
        let path = self.database_path.clone();
        let proposal_id = proposal.proposal_id;
        let days = self.config.lottery_activity_days;
        sqlite("disbursement execution", move || {
            let repository = DisbursementRepository::new(&path);
            match method.as_str() {
                "cancel" => {
                    if !repository
                        .cancel_voted_proposal_atomic(Some(guild_id), proposal_id, require_quorum)
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
                        .complete_reserve_allocation_atomic(
                            Some(guild_id),
                            proposal_id,
                            &method,
                            require_quorum,
                        )
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
                    let payout = repository
                        .complete_and_disburse_atomic(
                            Some(guild_id),
                            proposal_id,
                            &method,
                            require_quorum,
                            lottery_activity_cutoff(days),
                            |len| fastrand::usize(..len),
                        )
                        .map_err(|error| error.to_string())?;
                    let distributions = payout.distributions;
                    let message = if distributions.is_empty() {
                        Some("No eligible players for this distribution method.".to_owned())
                    } else {
                        None
                    };
                    Ok(DisbursementExecution {
                        method: method.to_owned(),
                        total_disbursed: payout.total,
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

fn bounded_bet_team_fields(
    label: &str,
    total_rows: usize,
    lines: &[String],
) -> Vec<(String, String)> {
    let mut values = Vec::new();
    let mut current = String::new();
    for line in lines {
        let line = truncate_field(line, FIELD_VALUE_LIMIT);
        let separator_chars = usize::from(!current.is_empty());
        if !current.is_empty()
            && current.chars().count() + separator_chars + line.chars().count() > FIELD_VALUE_LIMIT
        {
            values.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&line);
    }
    if !current.is_empty() {
        values.push(current);
    }
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let name = if index == 0 {
                format!("{label} Bets ({total_rows})")
            } else {
                format!("{label} Bets (cont.)")
            };
            (name, value)
        })
        .collect()
}

fn paginated_bet_embeds(title: &str, fields: Vec<(String, String)>) -> Vec<InteractionEmbed> {
    let title_chars = title.chars().count();
    let mut embeds = Vec::new();
    let mut embed = InteractionEmbed::titled(title).color(0xF1_C4_0F);
    let mut total_chars = title_chars;

    for (name, value) in fields {
        let field_chars = name.chars().count() + value.chars().count();
        if !embed.fields.is_empty()
            && (embed.fields.len() == MAX_FIELDS || total_chars + field_chars > TOTAL_LIMIT)
        {
            embeds.push(embed);
            embed = InteractionEmbed::titled(title).color(0xF1_C4_0F);
            total_chars = title_chars;
        }
        total_chars += field_chars;
        embed = embed.field(name, value, false);
    }

    if !embed.fields.is_empty() {
        embeds.push(embed);
    }
    embeds
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
                    "• {} → {}",
                    state.voter_names.resolve(vote.discord_id),
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

fn disburse_method_details(method: &str) -> (&str, &str, &str) {
    match method {
        "even" => (
            "📊",
            "Even Split",
            "Repays every debtor evenly, capped at each player's debt.",
        ),
        "proportional" => (
            "📈",
            "Proportional",
            "Repays debtors in proportion to how much each one owes.",
        ),
        "neediest" => (
            "🎯",
            "Neediest First",
            "Sends relief to the player deepest in debt, capped at their debt.",
        ),
        "stimulus" => (
            "💸",
            "Stimulus",
            "Splits the reserve among non-debtors outside the three richest.",
        ),
        "lottery" => (
            "🎲",
            "Lottery",
            "One randomly selected registered player wins the entire reserve.",
        ),
        "social_security" => (
            "👴",
            "Social Security",
            "Rewards registered players in proportion to games played.",
        ),
        "richest" => (
            "💎",
            "Richest",
            "Gives the entire reserve to the player with the highest balance.",
        ),
        "burn" => ("🔥", "Burn", "Removes the entire reserve from circulation."),
        "next_match_pot" => (
            "🎰",
            "Next Match Pot",
            "Moves the entire reserve into the next match's betting pot.",
        ),
        "cancel" => (
            "↩️",
            "Cancel",
            "Ends the vote and returns all locked funds to the reserve.",
        ),
        _ => ("🗳️", method, "Custom reserve allocation option."),
    }
}

fn disburse_method_label(method: &str) -> &str {
    disburse_method_details(method).1
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
    const QUORUM_BAR_WIDTH: i64 = 12;
    let total_votes = proposal.total_votes().max(0);
    let quorum = proposal.quorum_required.max(0);
    let progress_votes = if quorum == 0 {
        quorum
    } else {
        total_votes.min(quorum)
    };
    let filled = if quorum == 0 {
        QUORUM_BAR_WIDTH
    } else {
        progress_votes.saturating_mul(QUORUM_BAR_WIDTH) / quorum
    };
    let percentage = if quorum == 0 {
        100
    } else {
        progress_votes.saturating_mul(100) / quorum
    };
    let filled = usize::try_from(filled).unwrap_or_default();
    let empty = usize::try_from(QUORUM_BAR_WIDTH)
        .unwrap_or_default()
        .saturating_sub(filled);
    let remaining = quorum.saturating_sub(total_votes);
    let progress_note = if disabled {
        "✅ Voting closed — this petition has been finalized.".to_owned()
    } else if remaining == 0 {
        "✅ Quorum reached — allocation begins with this ballot.".to_owned()
    } else {
        format!(
            "{} more {} needed to decide the reserve's destination.",
            remaining,
            if remaining == 1 { "vote" } else { "votes" }
        )
    };
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
                            format!("disburse:{}:{method}", proposal.proposal_id),
                            disburse_method_label(method),
                        )
                        .style(style)
                        .disabled(disabled)
                    })
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let mut embed = InteractionEmbed::titled("🏛️ Jopacoin Reserve Allocation Vote")
        .description(format!(
            "**{}** {} is locked from the reserve. Choose what happens to it — every option is explained below.",
            proposal.fund_amount, JOPACOIN_EMOTE
        ))
        .color(0xE9_1E_63);
    for method in DISBURSE_METHODS {
        let (icon, label, explanation) = disburse_method_details(method);
        let votes = proposal.votes.get(method).copied().unwrap_or(0);
        embed = embed.field(
            format!("{icon} {label}"),
            format!(
                "{explanation}\n**{votes} {}**",
                if votes == 1 { "vote" } else { "votes" }
            ),
            true,
        );
    }
    embed = embed
        .field(
            "🗳️ Petition Progress",
            format!(
                "`{}{}` **{total_votes}/{quorum}** • **{percentage}%**\n{progress_note}\nQuorum is 36% of players who played here in the last 30 days, rounded up (minimum 1).",
                "█".repeat(filled),
                "░".repeat(empty),
            ),
            false,
        )
        .footer(if disabled {
            "Voting closed • Ties favor Even Split"
        } else {
            "Tap a button to vote • Your latest ballot replaces the previous one • Ties favor Even Split"
        });
    InteractionResponse::message("")
        .embed(embed)
        .action_rows(rows)
}

fn disbursement_response(
    execution: &DisbursementExecution,
    forced: bool,
    names: &GuildPlayerNameDirectory,
) -> InteractionResponse {
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
            .map(|(discord_id, amount)| format!("{}: +{amount}", names.resolve(*discord_id)))
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

/// Unix timestamp admitting players whose last match is within `days`.
///
/// A non-positive window would admit nobody, so it is clamped to zero, which
/// keeps only players who played at or after "now".
fn lottery_activity_cutoff(days: i64) -> i64 {
    Utc::now()
        .timestamp()
        .saturating_sub(days.max(0).saturating_mul(86_400))
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

fn parse_balance_component(custom_id: &str) -> Result<(u64, BalancePageDirection), String> {
    let mut parts = custom_id.split(':');
    if parts.next() != Some("betting") || parts.next() != Some("balance") {
        return Err("invalid balance component prefix".to_owned());
    }
    let session_id = parts
        .next()
        .ok_or_else(|| "missing balance component session".to_owned())?
        .parse::<u64>()
        .map_err(|_| "invalid balance component session".to_owned())?;
    let direction = match parts.next() {
        Some("previous") => BalancePageDirection::Previous,
        Some("next") => BalancePageDirection::Next,
        _ => return Err("invalid balance component direction".to_owned()),
    };
    if parts.next().is_some() {
        return Err("invalid balance component suffix".to_owned());
    }
    Ok((session_id, direction))
}

fn load_balance_statement(
    repository: &TaxRepository,
    user_id: i64,
    guild_id: i64,
    requested_page: usize,
) -> Result<Option<BalanceStatement>, String> {
    let Some(mut snapshot) = repository
        .player_snapshot(
            user_id,
            guild_id,
            BALANCE_LEDGER_PAGE_SIZE,
            0,
            unix_seconds()?,
        )
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let market_pages = snapshot
        .prediction_exposure
        .positions
        .len()
        .div_ceil(BALANCE_MARKET_PAGE_SIZE);
    let ledger_entries = usize::try_from(snapshot.recent_ledger_total.max(0)).unwrap_or(usize::MAX);
    let ledger_pages = ledger_entries.div_ceil(BALANCE_LEDGER_PAGE_SIZE).max(1);
    let total_pages = 1_usize
        .saturating_add(market_pages)
        .saturating_add(ledger_pages);
    let page = requested_page.clamp(1, total_pages);
    let section = if page == 1 {
        BalancePageSection::Overview
    } else if page <= market_pages.saturating_add(1) {
        BalancePageSection::Markets {
            page: page - 1,
            total_pages: market_pages,
        }
    } else {
        BalancePageSection::Activity {
            page: page - market_pages - 1,
            total_pages: ledger_pages,
        }
    };
    if let BalancePageSection::Activity {
        page: activity_page,
        ..
    } = section
    {
        let offset = (activity_page - 1).saturating_mul(BALANCE_LEDGER_PAGE_SIZE);
        if offset > 0 {
            snapshot.recent_ledger = repository
                .recent_ledger(guild_id, BALANCE_LEDGER_PAGE_SIZE, offset, Some(user_id))
                .map_err(|error| error.to_string())?;
            snapshot.recent_ledger_offset = offset;
        }
    }
    Ok(Some(BalanceStatement {
        snapshot,
        page,
        total_pages,
        section,
    }))
}

fn balance_statement_response(
    session_id: u64,
    display_name: &str,
    statement: &BalanceStatement,
    mana_badge: &str,
    bankruptcy_penalty_rate_per_game: f64,
    garnishment_rate: f64,
) -> InteractionResponse {
    InteractionResponse::message("")
        .embed(balance_statement_embed(
            display_name,
            statement,
            mana_badge,
            bankruptcy_penalty_rate_per_game,
            garnishment_rate,
        ))
        .ephemeral()
        .without_mentions()
        .action_row(balance_statement_buttons(session_id, statement))
}

fn balance_statement_embed(
    display_name: &str,
    statement: &BalanceStatement,
    mana_badge: &str,
    bankruptcy_penalty_rate_per_game: f64,
    garnishment_rate: f64,
) -> InteractionEmbed {
    let snapshot = &statement.snapshot;
    let title = format!("💰 Jopacoin Portfolio — {display_name}");
    let section_label = match statement.section {
        BalancePageSection::Overview => "Overview".to_owned(),
        BalancePageSection::Markets { page, total_pages } => {
            format!("Markets {page}/{total_pages}")
        }
        BalancePageSection::Activity { page, total_pages } => {
            format!("Activity {page}/{total_pages}")
        }
    };
    let footer = format!(
        "Private statement • Page {}/{} • {section_label}",
        statement.page, statement.total_pages
    );
    match statement.section {
        BalancePageSection::Overview => balance_overview_embed(
            title,
            footer,
            snapshot,
            mana_badge,
            bankruptcy_penalty_rate_per_game,
            garnishment_rate,
        ),
        BalancePageSection::Markets { page, total_pages } => {
            balance_markets_embed(title, footer, snapshot, page, total_pages)
        }
        BalancePageSection::Activity { page, total_pages } => {
            balance_activity_embed(title, footer, snapshot, page, total_pages)
        }
    }
}

fn balance_overview_embed(
    title: String,
    footer: String,
    snapshot: &TaxPlayerSnapshot,
    mana_badge: &str,
    bankruptcy_penalty_rate_per_game: f64,
    garnishment_rate: f64,
) -> InteractionEmbed {
    let summary = &snapshot.prediction_exposure.summary;
    let market_contracts = summary.yes_contracts.saturating_add(summary.no_contracts);
    let marked_portfolio = snapshot.balance.saturating_add(summary.expected_payout);
    let best_case_portfolio = snapshot.balance.saturating_add(summary.max_payout);
    let cash_asset = snapshot.balance.max(0);
    let market_asset = summary.expected_payout.max(0);
    let total_assets = cash_asset.saturating_add(market_asset);
    let cash_share = balance_asset_share(cash_asset, total_assets);
    let market_share = balance_asset_share(market_asset, total_assets);
    let badge = if mana_badge.is_empty() {
        String::new()
    } else {
        format!("{mana_badge}\n")
    };
    let mut status = format!(
        "Bankruptcies: **{}** • Penalty games: **{}**\nLoans taken: **{}** • Fees paid: {}",
        format_grouped(snapshot.bankruptcy_count),
        format_grouped(snapshot.penalty_games_remaining),
        format_grouped(snapshot.total_loans_taken),
        portfolio_jc(snapshot.total_fees_paid),
    );
    if snapshot.penalty_games_remaining > 0 {
        let withheld = cama_domain::bankruptcy::withheld_rate(
            bankruptcy_penalty_rate_per_game,
            snapshot.penalty_games_remaining,
        );
        status.push_str(&format!(
            "\nBankruptcy modifier: **{:.0}% of profit withheld** for {} more win(s)",
            withheld * 100.0,
            format_grouped(snapshot.penalty_games_remaining),
        ));
    }
    if snapshot.balance < 0 {
        status.push_str(&format!(
            "\nGarnishment: **{:.0}% of winnings** goes to debt repayment.",
            garnishment_rate * 100.0,
        ));
    }
    InteractionEmbed::titled(title)
        .description(format!(
            "{badge}A live, private view of your wallet, obligations, market positions, and ledger activity."
        ))
        .color(0xF1C40F)
        .field(
            "Estimated Portfolio",
            format!(
                "**{}**\nWallet + current market value\nBest case: {}",
                portfolio_jc(marked_portfolio),
                portfolio_jc(best_case_portfolio),
            ),
            true,
        )
        .field(
            "Wallet",
            format!(
                "Cash: {}\nVisible debt: {}",
                portfolio_jc(snapshot.balance),
                portfolio_jc(snapshot.visible_debt),
            ),
            true,
        )
        .field(
            "Market Exposure",
            format!(
                "{} market(s) • {} contracts\nBasis: {}\nValue: {} • P/L: {}",
                format_grouped(summary.markets),
                format_grouped(market_contracts),
                portfolio_jc(summary.cost_basis),
                portfolio_jc(summary.expected_payout),
                signed_portfolio_jc(summary.ev),
            ),
            true,
        )
        .field(
            "Asset Breakdown",
            format!(
                "`Wallet  {}` **{:>3}%**  {}\n`Markets {}` **{:>3}%**  {}\n**Tracked assets:** {}",
                allocation_bar(cash_share),
                cash_share,
                portfolio_jc(cash_asset),
                allocation_bar(market_share),
                market_share,
                portfolio_jc(market_asset),
                portfolio_jc(total_assets),
            ),
            false,
        )
        .field(
            "Match Bets at Risk",
            format!(
                "Dota: {} · Deadlock: {}\n{} unsettled wagers. Payouts depend on the final pools and results; stakes are not included as portfolio assets.",
                portfolio_jc(snapshot.pending_dota_stake),
                portfolio_jc(snapshot.pending_deadlock_stake),
                format_grouped(snapshot.pending_match_bet_count),
            ),
            false,
        )
        .field(
            "Liabilities & Capital at Risk",
            format!(
                "Wallet debt: {} • Loans: {}\nDark Bargains: {} across {} active\nMarket basis at risk: {}\n**Total tax-ledger exposure:** {}",
                portfolio_jc(snapshot.visible_debt),
                portfolio_jc(snapshot.loan_total),
                portfolio_jc(snapshot.dark_bargain_due),
                format_grouped(snapshot.dark_bargain_count),
                portfolio_jc(summary.cost_basis),
                portfolio_jc(snapshot.effective_obligations),
            ),
            false,
        )
        .field("Account Status", status, false)
        .footer(footer)
}

fn balance_markets_embed(
    title: String,
    footer: String,
    snapshot: &TaxPlayerSnapshot,
    page: usize,
    total_pages: usize,
) -> InteractionEmbed {
    let summary = &snapshot.prediction_exposure.summary;
    let mut embed = InteractionEmbed::titled(format!("{title} • Open Markets"))
        .description(format!(
            "**{} positions** • Basis {} • Current value {} • P/L {} • Max payout {}",
            format_grouped(summary.markets),
            portfolio_jc(summary.cost_basis),
            portfolio_jc(summary.expected_payout),
            signed_portfolio_jc(summary.ev),
            portfolio_jc(summary.max_payout),
        ))
        .color(0x3498DB)
        .footer(footer);
    let start = (page - 1).saturating_mul(BALANCE_MARKET_PAGE_SIZE);
    let end = snapshot
        .prediction_exposure
        .positions
        .len()
        .min(start.saturating_add(BALANCE_MARKET_PAGE_SIZE));
    for position in &snapshot.prediction_exposure.positions[start..end] {
        let contracts = position.yes_contracts.saturating_add(position.no_contracts);
        let position_status = title_case_token(&position.status);
        embed = embed.field(
            format!(
                "#{} • {}",
                position.prediction_id,
                short_balance_text(&position.question, 82),
            ),
            format!(
                "`{position_status}` • YES **{}¢** / NO **{}¢**\nYES: **{}** contracts • basis {}\nNO: **{}** contracts • basis {}\n**{} contracts** • basis {} → value {} ({}) • max {}",
                position.current_price,
                100_i64.saturating_sub(position.current_price),
                format_grouped(position.yes_contracts),
                portfolio_jc(position.yes_cost_basis),
                format_grouped(position.no_contracts),
                portfolio_jc(position.no_cost_basis),
                format_grouped(contracts),
                portfolio_jc(position.cost_basis),
                portfolio_jc(position.expected_payout),
                signed_portfolio_jc(position.ev),
                portfolio_jc(position.max_payout),
            ),
            false,
        );
    }
    debug_assert!(page <= total_pages);
    embed
}

fn balance_activity_embed(
    title: String,
    footer: String,
    snapshot: &TaxPlayerSnapshot,
    page: usize,
    total_pages: usize,
) -> InteractionEmbed {
    let total_entries = snapshot.recent_ledger_total.max(0);
    let mut embed = InteractionEmbed::titled(format!("{title} • Recent Activity"))
        .description(format!(
            "Every wallet movement in the central economy ledger • **{} entries**",
            format_grouped(total_entries),
        ))
        .color(0x5865F2)
        .footer(footer);
    if snapshot.recent_ledger.is_empty() {
        embed = embed.field("No activity yet", "Your ledger is clean and quiet.", false);
    } else {
        for row in &snapshot.recent_ledger {
            let direction = if row.delta > 0 {
                "↗"
            } else if row.delta < 0 {
                "↘"
            } else {
                "•"
            };
            embed = embed.field(
                format!(
                    "{direction} {} • <t:{}:R>",
                    signed_portfolio_jc(row.delta),
                    row.created_at,
                ),
                format!(
                    "{}\nBalance {} → {}",
                    balance_ledger_detail(row),
                    portfolio_jc(row.balance_before),
                    portfolio_jc(row.balance_after),
                ),
                false,
            );
        }
    }
    debug_assert!(page <= total_pages);
    embed
}

fn balance_statement_buttons(
    session_id: u64,
    statement: &BalanceStatement,
) -> InteractionActionRow {
    InteractionActionRow::buttons(vec![
        InteractionButton::new(format!("betting:balance:{session_id}:previous"), "Previous")
            .emoji("◀️")
            .style(InteractionButtonStyle::Secondary)
            .disabled(statement.page <= 1),
        InteractionButton::new(
            format!("betting:balance:{session_id}:page"),
            format!("Page {} / {}", statement.page, statement.total_pages),
        )
        .style(InteractionButtonStyle::Secondary)
        .disabled(true),
        InteractionButton::new(format!("betting:balance:{session_id}:next"), "Next")
            .emoji("▶️")
            .style(InteractionButtonStyle::Primary)
            .disabled(statement.page >= statement.total_pages),
    ])
}

fn balance_ledger_detail(row: &TaxLedgerEntry) -> String {
    if let Some(reason) = row
        .reason
        .as_deref()
        .map(normalize_balance_text)
        .filter(|reason| !reason.is_empty())
    {
        return short_balance_text(&reason, 180);
    }
    let label = match row.source.as_str() {
        "balance_update" => "Balance adjustment".to_owned(),
        "player_insert" => "Registration starting balance".to_owned(),
        "ledger_backfill" => "Opening balance".to_owned(),
        "dig" => "Dig balance change".to_owned(),
        "gamba" => "Wheel balance change".to_owned(),
        source => title_case_token(&source.replace('_', " ")),
    };
    let related_type = row
        .related_type
        .as_deref()
        .map(normalize_balance_text)
        .unwrap_or_default();
    let related_id = row
        .related_id
        .as_deref()
        .map(normalize_balance_text)
        .unwrap_or_default();
    match (related_type.is_empty(), related_id.is_empty()) {
        (false, false) => format!("{label} • {related_type} #{related_id}"),
        (false, true) => format!("{label} • {related_type}"),
        _ => label,
    }
}

fn balance_asset_share(value: i64, total: i64) -> i64 {
    if total <= 0 {
        0
    } else {
        value
            .saturating_mul(100)
            .saturating_add(total / 2)
            .checked_div(total)
            .unwrap_or_default()
            .clamp(0, 100)
    }
}

fn allocation_bar(percentage: i64) -> String {
    let filled = usize::try_from((percentage.clamp(0, 100) + 5) / 10).unwrap_or_default();
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

fn portfolio_jc(amount: i64) -> String {
    format!("{} {JOPACOIN_EMOTE}", format_grouped(amount))
}

fn signed_portfolio_jc(amount: i64) -> String {
    if amount > 0 {
        format!("+{} {JOPACOIN_EMOTE}", format_grouped(amount))
    } else {
        portfolio_jc(amount)
    }
}

fn normalize_balance_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn short_balance_text(value: &str, limit: usize) -> String {
    let value = normalize_balance_text(value);
    if value.chars().count() <= limit {
        value
    } else {
        let mut result = value
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>();
        result.push('…');
        result
    }
}

fn title_case_token(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
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

fn gamba_interaction_preflight(
    path: &Path,
    guild_id: i64,
    user_id: i64,
    now: i64,
    admin_bypass: bool,
) -> Result<Option<String>, String> {
    let players = PlayerRepository::new(path);
    if players
        .get_by_id(user_id, Some(guild_id))
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Ok(Some(
            "You need to /player register before you can spin the wheel.".to_owned(),
        ));
    }
    if !admin_bypass
        && let Some(last_spin) = players
            .get_last_wheel_spin(user_id, Some(guild_id))
            .map_err(|error| error.to_string())?
        && !wheel_spin_is_available(now, Some(last_spin))
    {
        return Ok(Some(wheel_cooldown_message(now, last_spin)));
    }
    Ok(None)
}

async fn begin_gamba_response(
    responder: &Arc<dyn InteractionResponder>,
    bonus_spin: bool,
) -> Result<GambaResponseRoute, String> {
    if bonus_spin {
        return Ok(GambaResponseRoute::Initial);
    }
    responder
        .defer(false)
        .await
        .map_err(|error| format!("gamba defer failed: {error}"))?;
    Ok(GambaResponseRoute::DeferredOriginal)
}

async fn send_gamba_response(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
    route: GambaResponseRoute,
) -> Result<(), String> {
    match route {
        GambaResponseRoute::Initial => responder.respond(response).await,
        GambaResponseRoute::DeferredOriginal => responder.edit_original(response).await,
    }
    .map_err(|error| error.to_string())
}

async fn deliver_gamba_response(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
    completion_embed: Option<InteractionEmbed>,
    route: GambaResponseRoute,
) -> Result<bool, String> {
    let response_has_attachment = !response.attachments.is_empty();
    match send_gamba_response(responder, response.clone(), route).await {
        Ok(()) => Ok(response_has_attachment),
        Err(error) if response_has_attachment => {
            // A media rejection happens after the wheel transaction commits.
            // Retry through the same acknowledged delivery route; calling the
            // initial-response endpoint again would itself be invalid after a
            // defer and recreates Discord's `Unknown interaction` failure.
            let mut text_only = response;
            text_only.attachments.clear();
            if text_only.content.is_empty() {
                if let Some(embed) = completion_embed {
                    text_only.embeds.push(embed);
                } else {
                    text_only.content = "The wheel animation could not be uploaded.".to_owned();
                }
            }
            send_gamba_response(responder, text_only, route)
                .await
                .map(|()| false)
                .map_err(|retry_error| {
                    format!("gamba response failed ({error}); text fallback failed ({retry_error})")
                })
        }
        Err(error) => Err(error),
    }
}

/// Apply Python's wrong-channel penance through the same immediate transaction
/// as every other reserve transfer, shared with every gamba-channel command.
/// A missing player is a silent no-op: the Discord-facing rejection is still
/// the canonical copy, while registered players get the exact one-JC debit and
/// reserve credit.
async fn charge_wrong_channel(path: &Path, user_id: i64, guild_id: i64) -> Result<(), String> {
    let path = path.to_path_buf();
    sqlite("gamba wrong-channel penance", move || {
        LoanRepository::new(path)
            .charge_gamba_channel_penalty_atomic(user_id, Some(guild_id))
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
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

/// Penalty games a new bankruptcy filing records: Black mana's Swamp effect
/// caps the configured count, so the declaration writes it in one step.
fn bankruptcy_penalty_games(configured: i64, effects: &ManaEffects) -> i64 {
    if effects.color.as_deref() == Some("Black") {
        configured.min(effects.swamp_bankruptcy_games.max(0))
    } else {
        configured
    }
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
        .filter(|player| {
            player.discord_id != Some(user_id)
                && player.jopacoin_balance >= cama_app::wheel::HOSTILE_LOSS_MIN_BALANCE
        })
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
        false,
        unix_seconds()?.max(0),
    )?;
    Ok((settled.applied > 0).then_some(settled.applied))
}

/// Log the non-fatal problems a wheel settlement reported.
fn log_wheel_warnings(warnings: &[String], user_id: i64, guild_id: i64) {
    for warning in warnings {
        tracing::warn!(user_id, guild_id, %warning, "wheel settlement warning");
    }
}

/// Run one direct `/gamba` spin through the application settlement and
/// render its Discord presentation. Runs on a blocking worker.
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
    low_priority_taxable_ids: &BTreeSet<i64>,
    admin_bypass: bool,
    bonus_spin: bool,
    visible_member_ids: Option<&BTreeSet<i64>>,
    display_name: Option<&str>,
    player_names: &dyn WheelPlayerNameSource,
    reminders: &dyn WheelReminderPort,
) -> Result<SpinResult, String> {
    let settlement_config = config.wheel_settlement();
    let outcome = spin_wheel(&WheelSpinRequest {
        path,
        guild_id,
        user_id,
        now,
        config: &settlement_config,
        event_win_multiplier,
        event_loss_multiplier,
        vanity_taxable_ids,
        low_priority_taxable_ids,
        admin_bypass,
        bonus_spin,
        visible_member_ids,
        player_names,
        reminders,
    })?;
    match outcome {
        WheelSpinOutcome::Rejected(message) => Ok(spin_result(message)),
        WheelSpinOutcome::Exploded(explosion) => {
            log_wheel_warnings(&explosion.warnings, user_id, guild_id);
            let receipt = explosion.receipt;
            let mut result = wheel_animation_result(render_explosion_attachment().ok());
            let mut explosion_embed = wheel_explosion_embed(
                receipt.balance_after,
                receipt
                    .gross
                    .saturating_sub(receipt.bankruptcy_penalty)
                    .saturating_sub(receipt.vanity_tax)
                    .saturating_sub(receipt.low_priority_tax),
                explosion.next_spin_at,
                receipt.garnished,
                receipt.bankruptcy_penalty,
                receipt.vanity_tax,
                receipt.low_priority_tax,
                explosion.blood_pact_skim,
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
            Ok(result)
        }
        WheelSpinOutcome::Pending(plan) => {
            let blue_preview = plan.blue_preview.clone();
            let (key, pending) = pending_wheel_interaction(
                *plan,
                user_id,
                guild_id,
                config.clone(),
                display_name.map(str::to_owned),
            );
            let prompt_embed = wheel_interaction_prompt_embed(&pending);
            let mut prompt_embeds = Vec::with_capacity(2);
            if let Some(preview) = blue_preview.as_deref() {
                prompt_embeds.push(wheel_bankrupt_preview_embed(preview));
            }
            prompt_embeds.push(prompt_embed);
            Ok(SpinResult {
                message: String::new(),
                embeds: prompt_embeds,
                completion_message: None,
                completion_embed: None,
                completion_delay: Duration::ZERO,
                pending: Some((key, pending)),
                // Python presents only the choice embed/buttons at this stage
                // and renders the selected wedge after the button resolves.
                attachment: None,
                neon_wheel_result: None,
                neon_wheel_balance_before: None,
                neon_wheel_balance: None,
                neon_lightning: None,
                golden_announcement: None,
            })
        }
        WheelSpinOutcome::Settled(settled) => {
            let SettledWheelSpin {
                wedges,
                landed_index,
                outcome,
                blue_preview,
                golden_leaderboard,
            } = *settled;
            log_wheel_warnings(&outcome.warnings, user_id, guild_id);
            let golden_announcement = golden_leaderboard
                .map(|leaderboard| golden_wheel_announcement(user_id, &leaderboard, player_names));
            // Blue mana's preview is additional information in both runtimes.
            // The ordinary Python wheel post itself has no content: just the
            // GIF, then the result embed after the animation.
            let mut result = wheel_animation_result(
                render_wheel_attachment(
                    &wedges,
                    landed_index,
                    outcome.kind == WheelKind::Golden,
                    display_name,
                )
                .ok(),
            );
            if let Some(preview) = blue_preview {
                result.embeds.push(wheel_bankrupt_preview_embed(&preview));
            }
            let mut result_embed = wheel_result_embed(
                &outcome.landed.label,
                outcome.resolved,
                outcome.kind,
                outcome.balance,
                outcome.next_spin_at,
                &outcome.presentation_note(),
                outcome.direct_loss_settled,
                outcome.income,
            );
            if bonus_spin {
                result_embed = result_embed.field(
                    "⛏️ Unearthed in the Mine",
                    "Your pickaxe struck a buried wheel mechanism, waking a free normal wheel spin from the rock. Regular `/gamba` cooldown unchanged.",
                    false,
                );
            }
            result.completion_embed = Some(result_embed);
            result.completion_delay = BETTING_WHEEL_DISPLAY_DELAY;
            // Python evaluates Neon against the post-policy wedge. This avoids
            // firing the bankrupt hook after a Comeback pardon changed a
            // negative wedge into LOSE/zero, and preserves the event-adjusted
            // loss amount.
            if let Some(value) = outcome.neon_wheel_loss() {
                result.neon_wheel_result = Some(value);
                result.neon_wheel_balance_before = Some(outcome.balance_before);
                result.neon_wheel_balance = Some(outcome.balance);
            }
            result.neon_lightning = outcome.neon_lightning;
            result.golden_announcement = golden_announcement;
            Ok(result)
        }
    }
}

fn spin_result(message: impl Into<String>) -> SpinResult {
    spin_result_with_attachment(message, None)
}

/// Match Python's successful `/gamba` message contract: initially the post
/// contains only the animation attachment, then the same post receives the
/// result embed.  `completion_message` remains the scheduling marker used by
/// the handler, but its empty value intentionally clears/omits message text.
fn wheel_animation_result(attachment: Option<InteractionAttachment>) -> SpinResult {
    let mut result = spin_result_with_attachment("", attachment);
    result.completion_message = Some(String::new());
    result
}

fn is_enabled_synthetic_wheel_member(discord_id: i64, enabled: bool) -> bool {
    enabled && discord_id < 0
}

/// `leaderboard` holds the visible top balances as `(discord_id, balance)`.
fn golden_wheel_announcement(
    user_id: i64,
    leaderboard: &[(i64, i64)],
    player_names: &dyn WheelPlayerNameSource,
) -> InteractionResponse {
    let named_ids = leaderboard
        .iter()
        .take(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
        .map(|(discord_id, _)| *discord_id)
        .chain([user_id])
        .collect::<Vec<_>>();
    let names = player_names.names_for(&named_ids);
    let name = |discord_id: i64| {
        names
            .get(&discord_id)
            .map_or(UNKNOWN_PLAYER, String::as_str)
            .to_owned()
    };
    let top_lines = leaderboard
        .iter()
        .take(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
        .enumerate()
        .map(|(index, (discord_id, balance))| {
            format!(
                "**#{}** {} — {balance} {JOPACOIN_EMOTE}",
                index + 1,
                name(*discord_id),
            )
        })
        .collect::<Vec<_>>();
    let mut mention_ids = leaderboard
        .iter()
        .take(cama_app::wheel::WHEEL_GOLDEN_TOP_N)
        .filter_map(|(discord_id, _)| u64::try_from(*discord_id).ok())
        .collect::<BTreeSet<_>>();
    if let Ok(user_id) = u64::try_from(user_id) {
        mention_ids.insert(user_id);
    }
    InteractionResponse::message("")
        .embed(
            InteractionEmbed::titled("👑 GOLDEN WHEEL INCOMING 👑")
                .description(format!(
                    "👑 {} is spinning the GOLDEN WHEEL!\nThey are among the top {} wealthiest players in the server...\n\n**Current top-{}:**\n{}",
                    name(user_id),
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
        embeds: Vec::new(),
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

fn wheel_timeout_response(
    pending: &PendingWheelInteraction,
    option: &WheelOption,
    result: &PendingWheelResolution,
    attachment: Option<InteractionAttachment>,
) -> InteractionResponse {
    let mut response = InteractionResponse::message(format!(
        "This Wheel interaction timed out. {}",
        result.message
    ));
    let terminal = match pending.kind {
        WheelInteractionKind::TownTrial => Some(
            InteractionEmbed::titled("⚖️ THE TOWN HAS SPOKEN")
                .description(format!(
                    "The town decided: **{}** for {}!",
                    option.label,
                    pending_wheel_player_name(pending)
                ))
                .color(0xED_42_45),
        ),
        WheelInteractionKind::Discover => Some(
            InteractionEmbed::titled("🃏 DISCOVER — TIMEOUT")
                .description(format!(
                    "{} didn't choose in time. The worst fate applies: **{}**!",
                    pending_wheel_player_name(pending),
                    option.label
                ))
                .color(0xED_42_45),
        ),
        WheelInteractionKind::Scrying | WheelInteractionKind::Reroll => None,
    };
    if let Some(terminal) = terminal {
        response = response.embed(terminal);
    }
    response = response.embed(result.completion_embed.clone());
    if let Some(attachment) = attachment {
        response = response.attachment(attachment);
    }
    response
}

async fn wheel_attachment_for_option(
    pending: &PendingWheelInteraction,
    option: &WheelOption,
) -> Option<InteractionAttachment> {
    let wheel_wedges = pending.wheel_wedges.clone();
    let golden = pending.golden;
    let display_name = pending.display_name.clone();
    let option = option.clone();
    match tokio::task::spawn_blocking(move || {
        let index = wheel_wedges.iter().position(|wedge| {
            wedge.label == option.label
                && wedge.value == option.value
                && wedge.color == option.color
        })?;
        render_wheel_attachment(&wheel_wedges, index, golden, display_name.as_deref()).ok()
    })
    .await
    {
        Ok(attachment) => attachment,
        Err(error) => {
            tracing::warn!(%error, "selected wheel GIF render task failed");
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn wheel_result_embed(
    landed_label: &str,
    resolved: WheelValue,
    kind: WheelKind,
    balance: i64,
    next_spin_at: i64,
    extra_note: &str,
    direct_loss_settled: bool,
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
        WheelValue::Numeric(0) if direct_loss_settled && kind == WheelKind::Golden => (
            "📉 OVEREXTENDED! 📉".to_owned(),
            0x4a3000,
            format!(
                "**OVEREXTENDED**\n\nYou flew too close to the sun.\n\nLost **0** {}.\n\n*Pride goes before the fall.*",
                JOPACOIN_EMOTE
            ),
        ),
        WheelValue::Numeric(0) if direct_loss_settled => (
            "💀 BANKRUPT! 💀".to_owned(),
            0xed4245,
            format!(
                "**{landed_label}**\n\nYou lost **0** {}!\n\n*The wheel shows no mercy...*",
                JOPACOIN_EMOTE
            ),
        ),
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
    if matches!(resolved, WheelValue::Mechanic(WheelMechanic::Heist))
        && !extra_note.trim().is_empty()
    {
        // HEIST's Python embed is entirely outcome-dependent (including the
        // zero-victim consolation copy), so the resolved note replaces the
        // generic mechanic placeholder instead of being appended to it.
        description = extra_note.trim().to_owned();
    } else if !extra_note.trim().is_empty() {
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
        if receipt.low_priority_tax > 0 {
            embed = embed.field(
                "Low Priority Tax",
                format!(
                    "−{} {} low priority tax",
                    receipt.low_priority_tax, JOPACOIN_EMOTE
                ),
                false,
            );
        }
    }
    embed
}

#[allow(clippy::too_many_arguments)]
fn wheel_explosion_embed(
    balance: i64,
    reward: i64,
    next_spin_at: i64,
    garnished: i64,
    bankruptcy_penalty: i64,
    vanity_tax: i64,
    low_priority_tax: i64,
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
    if low_priority_tax > 0 {
        embed = embed.field(
            "Low Priority Tax",
            format!("−{low_priority_tax} {JOPACOIN_EMOTE} low priority tax"),
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

fn scrying_display_label(option: &WheelOption) -> String {
    match option.value {
        WheelValue::Numeric(0) => "LOSE (0 JC)".to_owned(),
        WheelValue::Numeric(value) if value > 0 => format!("+{value} JC"),
        WheelValue::Numeric(value) => format!("{value} JC"),
        WheelValue::Mechanic(_) => option.label.clone(),
    }
}

fn wheel_bankrupt_preview_embed(preview: &str) -> InteractionEmbed {
    InteractionEmbed::titled("Wheel preview")
        .description(preview)
        .color(0x34_98_DB)
}

/// The spinner's readable name for wheel embeds, which never resolve mention
/// markup into a name.
fn pending_wheel_player_name(pending: &PendingWheelInteraction) -> &str {
    pending
        .display_name
        .as_deref()
        .and_then(readable_player_name)
        .unwrap_or(UNKNOWN_PLAYER)
}

fn wheel_interaction_prompt_embed(pending: &PendingWheelInteraction) -> InteractionEmbed {
    let name = pending_wheel_player_name(pending);
    match pending.kind {
        WheelInteractionKind::TownTrial => InteractionEmbed::titled("⚖️ TOWN TRIAL")
            .description(format!(
                "⚖️ **TOWN TRIAL** — The town has **5 minutes** to decide {name}'s fate!\n\nVote for a result:"
            ))
            .color(0x2A_1A_1A),
        WheelInteractionKind::Discover => InteractionEmbed::titled("🃏 DISCOVER")
            .description(format!(
                "🃏 **DISCOVER** — {name} must choose their fate!\n\nYou have **60 seconds** to choose:"
            ))
            .color(0x1A_2A_2A),
        WheelInteractionKind::Scrying => {
            let first = pending
                .options
                .first()
                .map_or_else(|| "LOSE (0 JC)".to_owned(), scrying_display_label);
            let second = pending
                .options
                .get(1)
                .map_or_else(|| "LOSE (0 JC)".to_owned(), scrying_display_label);
            InteractionEmbed::titled("🏝️ MANA SCRYING")
                .description(format!(
                    "🔮 {name}, the Island reveals two fates:\n\n**A:** {first}\n**B:** {second}\n\nChoose wisely. *(Blue mana: winnings reduced by 25%)*"
                ))
                .color(0x34_98_DB)
        }
        WheelInteractionKind::Reroll => InteractionEmbed::titled("Re-roll available")
            .description(format!(
                "{name} — Red mana lets you re-roll once. 30 seconds to decide."
            ))
            .color(0xED_42_45),
    }
}

fn wheel_display_label(option: &WheelOption) -> String {
    match option.value {
        WheelValue::Numeric(value) if value > 0 => format!("{} JC", option.label),
        _ => option.label.clone(),
    }
}

/// Turn the settlement's interactive plan into the process-local view state.
fn pending_wheel_interaction(
    plan: PendingWheelPlan,
    user_id: i64,
    guild_id: i64,
    config: BettingRuntimeConfig,
    display_name: Option<String>,
) -> (String, PendingWheelInteraction) {
    (
        plan.key,
        PendingWheelInteraction {
            kind: plan.interaction,
            user_id,
            guild_id,
            created_at: Instant::now(),
            options: plan.options,
            golden: plan.spin_kind == WheelKind::Golden,
            wheel_wedges: plan.wedges,
            display_name,
            initial_attachment: None,
            votes: BTreeMap::new(),
            spin_kind: plan.spin_kind,
            balance_before: plan.balance_before,
            effects: plan.effects,
            event_id: plan.event_id,
            config,
            event_win_multiplier: plan.event_win_multiplier,
            event_loss_multiplier: plan.event_loss_multiplier,
            bonus_spin: plan.bonus_spin,
            last_regular_spin: plan.last_regular_spin,
            original_responder: None,
            attempt: Arc::default(),
        },
    )
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
                        scrying_display_label(option)
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

/// Settle a pending interactive wheel through the application settlement and
/// render its completion. Runs on a blocking worker.
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
    low_priority_taxable_ids: &BTreeSet<i64>,
    visible_member_ids: Option<&BTreeSet<i64>>,
    player_names: &dyn WheelPlayerNameSource,
    attempt: &Mutex<WheelAttemptMemo>,
    reminders: &dyn WheelReminderPort,
) -> Result<PendingWheelResolution, String> {
    let settlement_config = config.wheel_settlement();
    let outcome = cama_app::wheel_settlement::resolve_pending_wheel(
        &PendingWheelRequest {
            path,
            guild_id,
            user_id,
            balance_before,
            kind,
            effects,
            config: &settlement_config,
            event_id,
            option,
            now,
            use_reroll,
            bonus_spin,
            last_regular_spin,
            event_win_multiplier,
            event_loss_multiplier,
            vanity_taxable_ids,
            low_priority_taxable_ids,
            visible_member_ids,
            player_names,
            reminders,
        },
        attempt,
    )?;
    log_wheel_warnings(&outcome.warnings, user_id, guild_id);
    let result_label = match outcome.resolved {
        WheelValue::Numeric(value) => value.to_string(),
        WheelValue::Mechanic(mechanic) => mechanic.code().to_owned(),
    };
    let presentation_note = outcome.presentation_note();
    let mut completion_embed = wheel_result_embed(
        &outcome.landed.label,
        outcome.resolved,
        kind,
        outcome.balance,
        outcome.next_spin_at,
        &presentation_note,
        outcome.direct_loss_settled,
        outcome.income,
    );
    if bonus_spin {
        completion_embed = completion_embed.field(
            "⛏️ Unearthed in the Mine",
            "Your pickaxe struck a buried wheel mechanism, waking a free normal wheel spin from the rock. Regular `/gamba` cooldown unchanged.",
            false,
        );
    }
    let neon_loss = outcome.neon_wheel_loss();
    Ok(PendingWheelResolution {
        message: format!(
            "🎡 **{}**\nYou spun **{}** {}. Balance: **{}** {}.{presentation_note}",
            outcome.landed.label, result_label, JOPACOIN_EMOTE, outcome.balance, JOPACOIN_EMOTE
        ),
        completion_embed,
        neon_wheel_result: neon_loss,
        neon_wheel_balance_before: neon_loss.map(|_| outcome.balance_before),
        neon_wheel_balance: neon_loss.map(|_| outcome.balance),
        neon_lightning: outcome.neon_lightning,
    })
}

const fn is_valid_leverage_tier(leverage: i64) -> bool {
    matches!(leverage, 1 | 2 | 3 | 5 | 10)
}

fn validate_investment_target(
    investor_id: i64,
    target_id: i64,
    target_name: &str,
    direction: &str,
    investor_registered: bool,
    target_registered: bool,
) -> Result<(), String> {
    if !investor_registered {
        return Err("You need to /player register before configuring investments.".to_owned());
    }
    if !target_registered {
        return Err(format!("{target_name} is not registered."));
    }
    if investor_id == target_id && direction == "short" {
        return Err("You cannot short yourself.".to_owned());
    }
    Ok(())
}

#[cfg(all(test, feature = "runtime-test-match"))]
#[path = "betting_provider/tests.rs"]
mod tests;
