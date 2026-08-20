//! Production Discord provider for the complete Python `/admin` surface.
//!
//! The provider deliberately keeps the two pieces of live in-memory state it
//! does not own behind narrow handles: lobby mutation/display refresh and
//! pending-match reminder/embed refresh. Durable player, moderation, economy,
//! rating, Steam, and correction operations use the existing SQLite schema.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::admin_low_priority::{REASON_DISPLAY_LIMIT, assignment_direct_message};
use cama_app::moderation::{
    CreateSuspension, ModerationService, ModerationServiceError, RecordLowPriorityEvent,
    parse_expiry,
};
use cama_app::opendota_http::OpenDotaRuntimeServices;
use cama_app::player_mmr_fallback::{RegisterPlayerError, RegisterPlayerInput, register_player};
use cama_db::admin::{AdminRepository, SetBothDisplayRatingsRequest};
use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::core_repositories::PlayerRepository;
use cama_db::loan_repository::{LoanRepository, LoanRepositoryError};
use cama_db::low_priority_repository::{
    LOW_PRIORITY_REQUIRED_WINS, LowPriorityRepository, PythonIntegerInput, SetLowPriorityInput,
};
use cama_db::moderation::{
    LobbySuspension, ModerationEventType, ModerationRepository, ModerationSource,
    SuspensionCompletion, SuspensionScope,
};
use cama_db::opendota_player::OpenDotaPlayerRepository;
use cama_db::rating_history_repository::{RatingHistoryRepository, RecalibrationRequest};
use cama_db::registration_repository::RegistrationRepository;
use cama_domain::openskill::CamaOpenSkillSystem;
use cama_domain::permissions::{DiscordPermissions, PermissionContext, has_admin_permission};
use cama_domain::rating::{CamaRatingSystem, MAX_GLICKO_RD, cap_glicko_rd};
use cama_domain::region::{
    CountsPayload, GameCountValue, RegionCountEntry, infer_region_from_counts,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value as JsonValue;
use tracing::{debug, error, warn};

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::global_hooks::{UsageMonitor, UsageSnapshot};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionHandler,
    InteractionHandlerError, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionValue, RegistrationError, RegistrationProvider,
    RegistryBuilder,
};

const ADMIN_DENIED: &str = "❌ Admin only! You need Administrator or Manage Server permissions.";
const GUILD_ONLY: &str = "This command can only be used in a server.";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const RECALIBRATION_MIN_GAMES: i64 = 5;

/// A request to extend one explicitly selected pending match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminExtendBettingRequest {
    pub guild_id: i64,
    pub actor_id: i64,
    pub minutes: i64,
    pub pending_match_id: i64,
}

/// The live match runtime owns persistence, task replacement, and all rendered
/// copies; this result contains only the copy needed by `/admin extendbetting`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminExtendBettingResult {
    pub pending_match_id: i64,
    pub old_bet_lock_until: i64,
    pub new_bet_lock_until: i64,
    pub lobby_label: String,
    pub jump_url: Option<String>,
    pub refreshed_routes: usize,
    pub refresh_failures: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminSeedHeroGridRequest {
    pub guild_id: i64,
    pub actor_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminSeedHeroGridResult {
    pub players_seeded: usize,
    pub matches_created: usize,
}

/// Additive control surface implemented by the production match provider.
#[async_trait]
pub trait AdminMatchControl: Send + Sync {
    async fn extend_betting(
        &self,
        request: AdminExtendBettingRequest,
    ) -> Result<AdminExtendBettingResult, String>;

    async fn seed_hero_grid(
        &self,
        request: AdminSeedHeroGridRequest,
    ) -> Result<AdminSeedHeroGridResult, String>;

    async fn backfill_derived_roles(
        &self,
        guild_id: i64,
    ) -> Result<AdminRoleBackfillResult, String>;
}

/// Result of a derived-role backfill, paired with the coverage that exists
/// afterwards so the run can be confirmed rather than taken on trust.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AdminRoleBackfillResult {
    pub matches_scanned: u64,
    pub teams_derived: u64,
    pub gold_samples_written: u64,
    pub last_hits_samples_written: u64,
    pub unreadable_payloads: u64,
    pub unparsed_teams: u64,
    pub ambiguous_lanes: u64,
    pub incomplete_teams: u64,
    pub tied_farm_priority: u64,
    pub tied_farm_and_ward_priority: u64,
    pub participants: i64,
    pub with_derived_role: i64,
    pub with_gold_at_10: i64,
    pub with_last_hits_at_10: i64,
    pub player_roles_above_minimum_sample: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminCorrectMatchSide {
    Radiant,
    Dire,
}

impl AdminCorrectMatchSide {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Radiant => "Radiant",
            Self::Dire => "Dire",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminCorrectMatchRequest {
    pub guild_id: i64,
    pub actor_id: i64,
    pub match_id: i64,
    pub correct_result: AdminCorrectMatchSide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminCorrectMatchBetResult {
    pub bets_affected: usize,
    pub old_winners_reversed: usize,
    pub new_winners_paid: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminCorrectMatchResult {
    pub old_winning_team: AdminCorrectMatchSide,
    pub new_winning_team: AdminCorrectMatchSide,
    pub correction_id: Option<i64>,
    pub players_affected: usize,
    pub ratings_updated: usize,
    pub openskill_matches_replayed: usize,
    pub bet_correction: Option<AdminCorrectMatchBetResult>,
    pub replay_recovered: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdminCorrectMatchError {
    InvalidRequest(String),
    Unexpected(String),
}

/// Durable recorded-match correction is intentionally separate from
/// [`AdminMatchControl`]: it owns no pending-match reminders or rendered
/// runtime state and must remain recoverable from SQLite after a restart.
#[async_trait]
pub trait AdminMatchCorrectionControl: Send + Sync {
    async fn correct_match(
        &self,
        request: AdminCorrectMatchRequest,
    ) -> Result<AdminCorrectMatchResult, AdminCorrectMatchError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorrectionWinRewardRequest {
    pub guild_id: i64,
    pub match_id: i64,
    pub discord_id: i64,
    pub gross_reward: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorrectionWinRewardResult {
    /// The exact participant snapshot used for a later result reversal:
    /// service `net + garnished`, matching Python's durable correction input.
    pub snapshot_balance_delta: i64,
}

/// Production reward policy invoked one corrected winner at a time. The
/// implementation atomically credits and writes the match-win ledger marker,
/// then applies the same garnishment, bankruptcy, vanity-tax, Sanctuary,
/// Communion, and Blood Pact ordering as ordinary match recording.
pub trait CorrectionWinRewardControl: Send + Sync {
    fn award_match_win_bonus(
        &self,
        request: CorrectionWinRewardRequest,
    ) -> Result<CorrectionWinRewardResult, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminFakeLobbyRequest {
    pub interaction_id: u64,
    pub guild_id: i64,
    pub channel_id: u64,
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminFakeLobbyResult {
    pub users_added: usize,
    pub user_names: Vec<String>,
    pub already_at_threshold: Option<(usize, usize)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AdminLobbyEjectionResult {
    pub applicable_lobbies: Vec<String>,
    pub evicted_lobbies: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminLobbyScope {
    All,
    Open,
    Lowskill,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminLobbyEjectionRequest {
    pub guild_id: i64,
    pub player_id: i64,
    pub player_display_name: String,
    pub scope: AdminLobbyScope,
}

/// Additive control surface implemented by the production lobby provider.
#[async_trait]
pub trait AdminLobbyControl: Send + Sync {
    async fn add_fake_users(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String>;

    async fn fill_lobby(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String>;

    /// Eject the player only from queued lobbies covered by `scope`. A lobby
    /// already starting a match must be left untouched.
    async fn eject_suspended_player(
        &self,
        request: AdminLobbyEjectionRequest,
    ) -> Result<AdminLobbyEjectionResult, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminDiscordHealth {
    pub guild_count: usize,
    pub latency_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdminCommandSyncResult {
    pub command_count: usize,
    pub guild_count: usize,
}

/// READY-attached Serenity cache/registry operations used by health and sync.
#[async_trait]
pub trait AdminDiscordControl: Send + Sync {
    async fn health(&self) -> AdminDiscordHealth;
    async fn sync_commands(&self) -> Result<AdminCommandSyncResult, String>;
}

#[derive(Clone)]
pub struct AdminRuntimePorts {
    pub discord: Arc<dyn DiscordTransport>,
    pub discord_control: Arc<dyn AdminDiscordControl>,
    pub lobby: Arc<dyn AdminLobbyControl>,
    pub matches: Arc<dyn AdminMatchControl>,
    pub match_corrections: Arc<dyn AdminMatchCorrectionControl>,
}

#[derive(Clone, Debug)]
struct AdminCommandConfig {
    admin_user_ids: Vec<u64>,
    admin_rating_adjustment_max_games: i64,
    admin_rating_max: f64,
    initial_glicko_rd: f64,
    recalibration_cooldown_seconds: i64,
    recalibration_initial_rd: f64,
    recalibration_initial_volatility: f64,
    new_player_exclusion_boost: i64,
    registration_rating: CamaRatingSystem,
    registration_openskill: CamaOpenSkillSystem,
}

#[derive(Clone)]
struct AdminMonitoring {
    database_path: PathBuf,
    usage: UsageMonitor,
    started_at: DateTime<Utc>,
    git_sha: String,
}

struct AdminHealthSnapshot {
    status: &'static str,
    reasons: Vec<String>,
    uptime: String,
    started_at: String,
    git_sha: String,
    db_ok: bool,
    db_latency_ms: Option<f64>,
    db_size: String,
    memory_rss: String,
    open_fds: Option<usize>,
    io_blocks_in: u64,
    io_blocks_out: u64,
    discord_latency_ms: Option<u64>,
    guild_count: usize,
    usage: UsageSnapshot,
}

impl AdminMonitoring {
    fn snapshot(&self, discord: AdminDiscordHealth) -> AdminHealthSnapshot {
        let started = Instant::now();
        let db_probe = rusqlite::Connection::open_with_flags(
            &self.database_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .and_then(|connection| {
            // Python treats an existing-but-empty players table as healthy:
            // `execute(...).fetchone()` returning `None` is not an error. Keep
            // the table-presence probe while guaranteeing one result row.
            connection.query_row("SELECT EXISTS(SELECT 1 FROM players LIMIT 1)", [], |_| {
                Ok(())
            })
        });
        let (db_ok, db_latency_ms, reasons) = match db_probe {
            Ok(()) => (
                true,
                Some(started.elapsed().as_secs_f64() * 1_000.0),
                Vec::new(),
            ),
            Err(error) => (false, None, vec![format!("DB probe failed: {error}")]),
        };
        let uptime = (Utc::now() - self.started_at).num_seconds().max(0);
        let resources = process_resources();
        AdminHealthSnapshot {
            status: if db_ok { "ok" } else { "degraded" },
            reasons,
            uptime: format_duration(uptime),
            started_at: self.started_at.to_rfc3339_opts(SecondsFormat::Secs, false),
            git_sha: self.git_sha.clone(),
            db_ok,
            db_latency_ms,
            db_size: fs::metadata(&self.database_path).ok().map_or_else(
                || "unavailable".to_owned(),
                |metadata| format_bytes(metadata.len()),
            ),
            memory_rss: resources
                .peak_rss_bytes
                .map_or_else(|| "unavailable".to_owned(), format_bytes),
            open_fds: open_fd_count(),
            io_blocks_in: resources.io_blocks_in,
            io_blocks_out: resources.io_blocks_out,
            discord_latency_ms: discord.latency_ms,
            guild_count: discord.guild_count,
            usage: self.usage.snapshot(),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ProcessResources {
    peak_rss_bytes: Option<u64>,
    io_blocks_in: u64,
    io_blocks_out: u64,
}

impl AdminCommandConfig {
    fn from_application(config: &ApplicationConfig) -> Self {
        Self {
            admin_user_ids: config
                .identities
                .admin_user_ids
                .iter()
                .filter_map(|id| u64::try_from(*id).ok())
                .collect(),
            admin_rating_adjustment_max_games: config.values.admin_rating_adjustment_max_games,
            admin_rating_max: config.values.admin_rating_max,
            initial_glicko_rd: config.values.initial_glicko_rd,
            recalibration_cooldown_seconds: config.values.recalibration_cooldown_seconds,
            recalibration_initial_rd: config.values.recalibration_initial_rd,
            recalibration_initial_volatility: config.values.recalibration_initial_volatility,
            new_player_exclusion_boost: config.values.new_player_exclusion_boost,
            // Fail-soft like the rest of the admin config values.
            registration_rating: config
                .glicko_rating_system()
                .unwrap_or_else(|_| CamaRatingSystem::default()),
            registration_openskill: CamaOpenSkillSystem::with_config(
                config.migration.openskill.clone(),
            )
            .unwrap_or_default(),
        }
    }
}

#[derive(Clone)]
pub struct AdminRegistrationProvider {
    handler: Arc<dyn InteractionHandler>,
    admin_rating_max: f64,
}

impl AdminRegistrationProvider {
    /// Compose the live provider over the already-admitted Python schema and
    /// the shared lobby/match/Discord runtime handles.
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        opendota: Arc<OpenDotaRuntimeServices>,
        ports: AdminRuntimePorts,
        usage: UsageMonitor,
    ) -> Self {
        let path = database_path.as_ref();
        let command_config = AdminCommandConfig::from_application(config);
        Self {
            admin_rating_max: command_config.admin_rating_max,
            handler: Arc::new(AdminHandler {
                players: PlayerRepository::new(path),
                admin: AdminRepository::new(path),
                low_priority: LowPriorityRepository::new(path),
                moderation: ModerationService::new(ModerationRepository::new(path)),
                loans: LoanRepository::new(path),
                bankruptcy: BankruptcyRepository::new(path),
                recalibration: RatingHistoryRepository::new(path),
                steam: OpenDotaPlayerRepository::new(path),
                registration: RegistrationRepository::new(path),
                opendota,
                ports,
                config: command_config,
                monitoring: AdminMonitoring {
                    database_path: path.to_path_buf(),
                    usage,
                    started_at: Utc::now(),
                    git_sha: short_git_sha(),
                },
                rate_limits: Mutex::new(BTreeMap::new()),
                processed_addfake: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Low-level constructor used by the production composition constructor
    /// and deterministic provider tests.
    #[must_use]
    pub fn from_handler(handler: Arc<dyn InteractionHandler>, admin_rating_max: f64) -> Self {
        Self {
            handler,
            admin_rating_max,
        }
    }
}

struct AdminHandler {
    players: PlayerRepository,
    admin: AdminRepository,
    low_priority: LowPriorityRepository,
    moderation: ModerationService<ModerationRepository>,
    loans: LoanRepository,
    bankruptcy: BankruptcyRepository,
    recalibration: RatingHistoryRepository,
    steam: OpenDotaPlayerRepository,
    registration: RegistrationRepository,
    opendota: Arc<OpenDotaRuntimeServices>,
    ports: AdminRuntimePorts,
    config: AdminCommandConfig,
    monitoring: AdminMonitoring,
    rate_limits: Mutex<BTreeMap<RateLimitKey, VecDeque<Instant>>>,
    processed_addfake: Mutex<BTreeMap<(u64, u64), Instant>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RateLimitKey {
    scope: &'static str,
    guild_id: i64,
    user_id: i64,
}

#[derive(Clone, Debug)]
struct AdminCommandContext {
    interaction_id: u64,
    actor_id: u64,
    _actor_display_name: String,
    guild_id: i64,
    channel_id: u64,
    member_permissions: Option<u64>,
    path: Vec<String>,
    options: Vec<InteractionOption>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AdminUserOption {
    id: u64,
    display_name: Option<String>,
    is_bot: Option<bool>,
}

enum SuspendPersistence {
    Saved {
        state: LobbySuspension,
        replaced: bool,
    },
    ActiveExists(LobbySuspension),
}

#[async_trait]
impl InteractionHandler for AdminHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            interaction_id,
            name,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            member_permissions,
            options,
        } = request
        else {
            return Err("admin provider accepts slash commands only".into());
        };
        if name != "admin" {
            return Err(format!("admin handler received command {name:?}").into());
        }
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(&responder, GUILD_ONLY).await;
        };
        let (path, options) = nested_command_path(&options)?;
        let context = AdminCommandContext {
            interaction_id,
            actor_id: user_id,
            _actor_display_name: user_display_name,
            guild_id: signed_id(guild_id, "guild")?,
            channel_id: channel_id.unwrap_or_default(),
            member_permissions,
            path,
            options,
        };
        self.handle_command(context, responder).await
    }
}

impl AdminHandler {
    async fn lowprio_add(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let reason = string(&context.options, "reason");
        let wins = integer(&context.options, "wins").unwrap_or(LOW_PRIORITY_REQUIRED_WINS);
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let players = self.players.clone();
        let exists = tokio::task::spawn_blocking(move || players.exists(target_id, Some(guild_id)))
            .await
            .map_err(|error| format!("low-priority player task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if !exists {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let low_priority = self.low_priority.clone();
        let moderation = self.moderation.clone();
        let actor_id = signed_id(context.actor_id, "actor")?;
        let reason_for_write = reason.clone();
        let state = tokio::task::spawn_blocking(move || {
            let previous = low_priority
                .get_state(target_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            let watermark = moderation
                .port()
                .pending_match_watermark(Some(guild_id))
                .map_err(|error| error.to_string())?;
            let mut input = SetLowPriorityInput::new(
                target_id,
                Some(guild_id),
                actor_id,
                reason_for_write.clone(),
            );
            input.wins_required = PythonIntegerInput::Integer(wins);
            input.start_pending_match_id = Some(PythonIntegerInput::Integer(watermark));
            // The option description this admin just read says the placed player
            // sees the reason, so this row's reason is theirs to read.
            input.reason_player_visible = true;
            let state = low_priority
                .set_low_priority(&input)
                .map_err(|error| error.to_string())?;
            let event_type = if previous.is_some_and(|state| state.active) {
                ModerationEventType::LowprioReplace
            } else {
                ModerationEventType::LowprioAssign
            };
            if let Err(error) = moderation.record_low_priority_event(RecordLowPriorityEvent {
                discord_id: target_id,
                guild_id: Some(guild_id),
                event_type,
                wins_required: state.wins_required,
                wins_remaining: state.wins_remaining,
                actor_id: Some(actor_id),
                reason: reason_for_write.as_deref(),
                match_id: state.start_pending_match_id,
                now: now_seconds(),
            }) {
                warn!(?error, "failed to audit low-priority assignment");
            }
            Ok::<_, String>(state)
        })
        .await
        .map_err(|error| format!("low-priority write task failed: {error}"))??;
        // The penalty is already committed, so the player is told either way:
        // acknowledge first, then notify, and only then surface an ack failure.
        let acknowledged = respond_ephemeral(
            &responder,
            format!(
                "✅ Updated matchmaking state for <@{}>. {} wins required.",
                user.id, state.wins_remaining
            ),
        )
        .await;
        let guild_name = match u64::try_from(guild_id) {
            Ok(guild_id) => self
                .ports
                .discord
                .guild_name(guild_id)
                .await
                .unwrap_or_default(),
            Err(_) => None,
        };
        self.best_effort_dm(
            user.id,
            assignment_direct_message(
                state.wins_required,
                state
                    .reason
                    .as_deref()
                    .filter(|_| state.reason_player_visible),
                guild_name.as_deref(),
            ),
        )
        .await;
        acknowledged
    }

    async fn lowprio_remove(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let reason = string(&context.options, "reason");
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let actor_id = signed_id(context.actor_id, "actor")?;
        let low_priority = self.low_priority.clone();
        let moderation = self.moderation.clone();
        let reason_for_write = reason.clone();
        let removed = tokio::task::spawn_blocking(move || {
            let removed = low_priority
                .clear_low_priority(
                    target_id,
                    Some(guild_id),
                    actor_id,
                    reason_for_write.as_deref(),
                )
                .map_err(|error| error.to_string())?;
            if removed {
                let prior = low_priority
                    .get_state(target_id, Some(guild_id))
                    .map_err(|error| error.to_string())?;
                if let Err(error) = moderation.record_low_priority_event(RecordLowPriorityEvent {
                    discord_id: target_id,
                    guild_id: Some(guild_id),
                    event_type: ModerationEventType::LowprioClear,
                    wins_required: prior
                        .as_ref()
                        .map_or(LOW_PRIORITY_REQUIRED_WINS, |state| state.wins_required),
                    wins_remaining: 0,
                    actor_id: Some(actor_id),
                    reason: reason_for_write.as_deref(),
                    match_id: prior.and_then(|state| state.start_pending_match_id),
                    now: now_seconds(),
                }) {
                    warn!(?error, "failed to audit low-priority removal");
                }
            }
            Ok::<_, String>(removed)
        })
        .await
        .map_err(|error| format!("low-priority clear task failed: {error}"))??;
        let content = if removed {
            format!("✅ Cleared matchmaking state for <@{}>.", user.id)
        } else {
            format!(
                "<@{}> does not have active restricted matchmaking state.",
                user.id
            )
        };
        respond_ephemeral(&responder, content).await?;
        Ok(())
    }

    async fn lowprio_status(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let repository = self.low_priority.clone();
        let state =
            tokio::task::spawn_blocking(move || repository.get_state(target_id, Some(guild_id)))
                .await
                .map_err(|error| format!("low-priority status task failed: {error}"))?
                .map_err(|error| error.to_string())?;
        let content = match state.filter(|state| state.active) {
            Some(state) => format!(
                "<@{}>: {}/{} wins completed ({} remaining).\nReason: {}\nIssued by: <@{}>\nApplied: {}",
                user.id,
                state.wins_required - state.wins_remaining,
                state.wins_required,
                state.wins_remaining,
                state.reason.as_deref().unwrap_or("Not provided"),
                state.set_by,
                state.created_at
            ),
            None => format!(
                "<@{}> does not have active restricted matchmaking state.",
                user.id
            ),
        };
        respond_ephemeral(&responder, content).await
    }

    async fn lowprio_list(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let repository = self.low_priority.clone();
        let guild_id = context.guild_id;
        let states = tokio::task::spawn_blocking(move || repository.list_active(Some(guild_id)))
            .await
            .map_err(|error| format!("low-priority list task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        let content = if states.is_empty() {
            "No active restricted matchmaking state.".to_owned()
        } else {
            states
                .iter()
                .map(|state| {
                    format!(
                        "<@{}> — {}/{} wins remaining; reason: {}; by <@{}>; assigned {}",
                        state.discord_id,
                        state.wins_remaining,
                        state.wins_required,
                        state.reason.as_deref().unwrap_or("not provided"),
                        state.set_by,
                        state.created_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        respond_ephemeral(&responder, truncate_discord(content)).await
    }

    async fn best_effort_dm(&self, user_id: u64, content: String) {
        if let Err(error) = self
            .ports
            .discord
            .send_direct_message(
                user_id,
                DiscordMessage::silent(InteractionResponse::message(content)),
            )
            .await
        {
            debug!(user_id, %error, "admin direct message failed");
        }
    }
}

impl AdminHandler {
    async fn handle_command(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let route = context.path.join(" ");
        match route.as_str() {
            "adjust rating" => self.adjust_rating(context, responder).await,
            "adjust rd" => self.adjust_rd(context, responder).await,
            "lowprio add" => self.lowprio_add(context, responder).await,
            "lowprio remove" => self.lowprio_remove(context, responder).await,
            "lowprio status" => self.lowprio_status(context, responder).await,
            "lowprio list" => self.lowprio_list(context, responder).await,
            "moderation suspend" => self.moderation_suspend(context, responder).await,
            "moderation lift" => self.moderation_lift(context, responder).await,
            "moderation status" => self.moderation_status(context, responder).await,
            "moderation list" => self.moderation_list(context, responder).await,
            "moderation history" => self.moderation_history(context, responder).await,
            "health" => self.health(context, responder).await,
            "addfake" => self.addfake(context, responder).await,
            "filllobbytest" => self.fill_lobby_test(context, responder).await,
            "resetuser" => self.reset_user(context, responder).await,
            "registeruser" => self.register_user(context, responder).await,
            "sync" => self.sync(context, responder).await,
            "backfillroles" => self.backfill_roles(context, responder).await,
            "givecoin" => self.give_coin(context, responder).await,
            "resetloancooldown" => self.reset_loan_cooldown(context, responder).await,
            "resetbankruptcycooldown" => self.reset_bankruptcy_cooldown(context, responder).await,
            "setrating" => self.set_initial_rating(context, responder).await,
            "recalibrate" => self.recalibrate(context, responder).await,
            "bumprd" => self.bump_rd(context, responder).await,
            "resetrecalibrationcooldown" => {
                self.reset_recalibration_cooldown(context, responder).await
            }
            "extendbetting" => self.extend_betting(context, responder).await,
            "correctmatch" => self.correct_match(context, responder).await,
            "addsteamid" => self.add_steam_id(context, responder).await,
            "removesteamid" => self.remove_steam_id(context, responder).await,
            "setprimarysteam" => self.set_primary_steam(context, responder).await,
            "seedherogrid" => self.seed_hero_grid(context, responder).await,
            _ => Err(format!("unknown /admin route {route:?}").into()),
        }
    }

    fn admin_allowed(&self, context: &AdminCommandContext) -> bool {
        let permissions = context
            .member_permissions
            .map(|permissions| DiscordPermissions {
                administrator: permissions & ADMINISTRATOR_PERMISSION != 0,
                manage_guild: permissions & MANAGE_GUILD_PERMISSION != 0,
            });
        let permission_context = permissions.map_or_else(
            || PermissionContext::new(context.actor_id),
            |permissions| {
                PermissionContext::new(context.actor_id).with_guild_member_permissions(permissions)
            },
        );
        has_admin_permission(&permission_context, &self.config.admin_user_ids)
    }

    async fn require_admin(
        &self,
        context: &AdminCommandContext,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, InteractionHandlerError> {
        if self.admin_allowed(context) {
            Ok(true)
        } else {
            send_response(
                responder,
                InteractionResponse::message(ADMIN_DENIED).ephemeral(),
            )
            .await?;
            Ok(false)
        }
    }

    fn rate_limit(
        &self,
        context: &AdminCommandContext,
        scope: &'static str,
        limit: usize,
        window: Duration,
    ) -> Option<u64> {
        let key = RateLimitKey {
            scope,
            guild_id: context.guild_id,
            user_id: i64::try_from(context.actor_id).unwrap_or(i64::MAX),
        };
        let now = Instant::now();
        let mut limits = self
            .rate_limits
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let attempts = limits.entry(key).or_default();
        while attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= window)
        {
            attempts.pop_front();
        }
        if attempts.len() >= limit {
            let elapsed = now.duration_since(*attempts.front().expect("limit is nonzero"));
            return Some(window.saturating_sub(elapsed).as_secs_f64().ceil() as u64);
        }
        attempts.push_back(now);
        None
    }

    fn first_addfake_delivery(&self, interaction_id: u64, actor_id: u64) -> bool {
        let mut processed = self
            .processed_addfake
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let key = (interaction_id, actor_id);
        if processed.contains_key(&key) {
            warn!(
                interaction_id,
                actor_id, "duplicate admin addfake delivery ignored"
            );
            return false;
        }
        let now = Instant::now();
        processed.insert(key, now);
        if processed.len() > 100 {
            processed.retain(|_, handled_at| {
                now.duration_since(*handled_at) <= Duration::from_secs(300)
            });
        }
        true
    }
}

impl AdminHandler {
    async fn health(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let can_respond = try_defer(&responder, true).await;
        let discord = self.ports.discord_control.health().await;
        let monitoring = self.monitoring.clone();
        let snapshot = tokio::task::spawn_blocking(move || monitoring.snapshot(discord))
            .await
            .map_err(|error| format!("health snapshot task failed: {error}"))?;
        if can_respond {
            respond_ephemeral(&responder, format_health_snapshot(&snapshot)).await
        } else {
            Ok(())
        }
    }

    async fn addfake(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = self.rate_limit(&context, "addfake", 2, Duration::from_secs(60)) {
            return respond_ephemeral(
                &responder,
                format!("⏳ Please wait {retry}s before using `/admin addfake` again."),
            )
            .await;
        }
        if !self.first_addfake_delivery(context.interaction_id, context.actor_id) {
            return Ok(());
        }
        let can_respond = try_defer(&responder, true).await;
        if !self.admin_allowed(&context) {
            return if can_respond {
                respond_ephemeral(&responder, ADMIN_DENIED).await
            } else {
                Ok(())
            };
        }
        let count = integer(&context.options, "count").unwrap_or(1);
        if !(1..=10).contains(&count) {
            return if can_respond {
                respond_ephemeral(&responder, "❌ Count must be between 1 and 10.").await
            } else {
                Ok(())
            };
        }
        let result = self
            .ports
            .lobby
            .add_fake_users(AdminFakeLobbyRequest {
                interaction_id: context.interaction_id,
                guild_id: context.guild_id,
                channel_id: context.channel_id,
                count: usize::try_from(count)
                    .map_err(|_| "validated fake-user count is negative")?,
            })
            .await;
        if !can_respond {
            return Ok(());
        }
        match result {
            Ok(result) => {
                respond_ephemeral(
                    &responder,
                    format!(
                        "✅ Added {} fake user(s): {}",
                        result.users_added,
                        result.user_names.join(", ")
                    ),
                )
                .await
            }
            Err(message) => respond_ephemeral(&responder, format!("❌ {message}")).await,
        }
    }

    async fn fill_lobby_test(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.admin_allowed(&context) {
            return respond_ephemeral(&responder, "❌ Admin only command.").await;
        }
        defer(&responder, true).await?;
        let result = self
            .ports
            .lobby
            .fill_lobby(AdminFakeLobbyRequest {
                interaction_id: context.interaction_id,
                guild_id: context.guild_id,
                channel_id: context.channel_id,
                count: 0,
            })
            .await;
        match result {
            Ok(AdminFakeLobbyResult {
                already_at_threshold: Some((current, threshold)),
                ..
            }) => {
                respond_ephemeral(
                    &responder,
                    format!("✅ Lobby already has {current}/{threshold} players."),
                )
                .await
            }
            Ok(result) => {
                respond_ephemeral(
                    &responder,
                    format!(
                        "✅ Added {} fake user(s) to fill lobby.",
                        result.users_added
                    ),
                )
                .await
            }
            Err(message) => respond_ephemeral(&responder, format!("❌ {message}")).await,
        }
    }

    async fn register_user(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = self.rate_limit(&context, "registeruser", 5, Duration::from_secs(60)) {
            return respond_ephemeral(
                &responder,
                format!("⏳ Please wait {retry}s before using `/registeruser` again."),
            )
            .await;
        }
        let _ = try_defer(&responder, true).await;
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let steam_id = required_integer(&context.options, "steam_id")?;
        let mmr = integer(&context.options, "mmr");
        if mmr.is_some_and(|mmr| !(0..=12_000).contains(&mmr)) {
            return respond_ephemeral(&responder, "❌ MMR must be between 0 and 12000.").await;
        }
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let username = user
            .display_name
            .clone()
            .unwrap_or_else(|| format!("User {}", user.id));
        let mut repository = self.registration.clone();
        let mut api = self.opendota.registration_api();
        let input = RegisterPlayerInput {
            discord_id: target_id,
            discord_username: username,
            guild_id,
            steam_id,
            mmr_override: mmr,
            exclusion_count: self.config.new_player_exclusion_boost,
            added_at: now_seconds(),
        };
        let rating_system = self.config.registration_rating;
        let openskill = self.config.registration_openskill.clone();
        let result = tokio::task::spawn_blocking(move || {
            register_player(&mut repository, &mut api, input, &rating_system, &openskill)
        })
        .await
        .map_err(|task_error| format!("registration task failed: {task_error}"))?;
        match result {
            Ok(result) => {
                if mmr.is_none() {
                    self.best_effort_registration_region(target_id, guild_id, steam_id)
                        .await;
                }
                respond_ephemeral(
                    &responder,
                    format!(
                        "✅ Registered <@{}>!\nCama Rating: {} ({:.0}% uncertainty)\nThey can use `/player roles` to set their preferred roles.",
                        user.id, result.cama_rating, result.uncertainty
                    ),
                )
                .await
            }
            Err(RegisterPlayerError::Repository(message)) => {
                error!(user_id = user.id, %message, "admin player registration failed");
                respond_ephemeral(
                    &responder,
                    "❌ Unexpected error registering user. Check logs.",
                )
                .await
            }
            Err(error) => respond_ephemeral(&responder, format!("❌ {error}")).await,
        }
    }

    async fn best_effort_registration_region(&self, discord_id: i64, guild_id: i64, steam_id: i64) {
        let Some(raw_counts) = self
            .opendota
            .shared_client()
            .get_player_counts(steam_id)
            .await
        else {
            return;
        };
        let counts = project_counts_payload(&raw_counts);
        let Some(region) = infer_region_from_counts(Some(&counts)) else {
            return;
        };
        let repository = self.registration.clone();
        let region = region.to_owned();
        match tokio::task::spawn_blocking(move || {
            repository.set_inferred_region(discord_id, Some(guild_id), &region)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => warn!(%error, "admin registration region inference write failed"),
            Err(error) => warn!(%error, "admin registration region inference task failed"),
        }
    }

    async fn backfill_roles(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = self.rate_limit(&context, "backfillroles", 1, Duration::from_secs(300))
        {
            return respond_ephemeral(
                &responder,
                format!("⏳ Please wait {retry}s before running `/admin backfillroles` again."),
            )
            .await;
        }
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let guild_id = context.guild_id;
        // Scans every enriched match, so acknowledge before any database work.
        defer(&responder, true).await?;
        match self.ports.matches.backfill_derived_roles(guild_id).await {
            Ok(result) => {
                // Every skip reason here is per team, so the total reconciles
                // against the teams actually examined.
                let skipped = result.unparsed_teams
                    + result.ambiguous_lanes
                    + result.incomplete_teams
                    + result.tied_farm_priority
                    + result.tied_farm_and_ward_priority;
                let coverage = if result.participants == 0 {
                    0.0
                } else {
                    result.with_derived_role as f64 * 100.0 / result.participants as f64
                };
                respond_ephemeral(
                    &responder,
                    format!(
                        "✅ Role backfill complete.\n\
                         **Scanned:** {} match(es), {} unreadable, wrote {} gold sample(s) and {} last-hits sample(s).\n\
                         **Teams:** {} derived, {skipped} skipped ({} unparsed, {} ambiguous lanes, {} incomplete, {} tied farm, {} tied farm and wards).\n\
                         **Coverage:** {}/{} participants have a role ({coverage:.1}%), {} have 10-minute last hits (the metric that drives derivation), {} have 10-minute gold.\n\
                         **Above minimum sample:** {} (player, role) pair(s) can move off the 0.95 floor.",
                        result.matches_scanned,
                        result.unreadable_payloads,
                        result.gold_samples_written,
                        result.last_hits_samples_written,
                        result.teams_derived,
                        result.unparsed_teams,
                        result.ambiguous_lanes,
                        result.incomplete_teams,
                        result.tied_farm_priority,
                        result.tied_farm_and_ward_priority,
                        result.with_derived_role,
                        result.participants,
                        result.with_last_hits_at_10,
                        result.with_gold_at_10,
                        result.player_roles_above_minimum_sample,
                    ),
                )
                .await
            }
            Err(error) => {
                respond_ephemeral(&responder, format!("❌ Role backfill failed: {error}")).await
            }
        }
    }

    async fn sync(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = self.rate_limit(&context, "sync", 1, Duration::from_secs(60)) {
            return respond_ephemeral(
                &responder,
                format!("⏳ Please wait {retry}s before using `/admin sync` again."),
            )
            .await;
        }
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        defer(&responder, true).await?;
        match self.ports.discord_control.sync_commands().await {
            Ok(result) => {
                respond_ephemeral(
                    &responder,
                    format!(
                        "✅ Synced {} command(s) to {} guild(s) and globally.",
                        result.command_count, result.guild_count
                    ),
                )
                .await
            }
            Err(message) => {
                error!(%message, "admin Discord command sync failed");
                respond_ephemeral(&responder, format!("❌ Error syncing commands: {message}")).await
            }
        }
    }

    async fn reset_user(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if let Some(retry) = self.rate_limit(&context, "resetuser", 2, Duration::from_secs(60)) {
            return respond_ephemeral(
                &responder,
                format!("⏳ Please wait {retry}s before using `/resetuser` again."),
            )
            .await;
        }
        let _ = try_defer(&responder, true).await;
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let players = self.players.clone();
        let exists = tokio::task::spawn_blocking(move || players.exists(target_id, Some(guild_id)))
            .await
            .map_err(|error| format!("reset-user lookup task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if !exists {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let players = self.players.clone();
        let deleted =
            tokio::task::spawn_blocking(move || players.delete(target_id, Some(guild_id)))
                .await
                .map_err(|error| format!("reset-user delete task failed: {error}"))?
                .map_err(|error| error.to_string())?;
        if !deleted {
            return respond_ephemeral(
                &responder,
                format!("❌ Failed to reset <@{}>'s account.", user.id),
            )
            .await;
        }
        respond_ephemeral(
            &responder,
            format!(
                "✅ Reset <@{}>'s account. They can register again.",
                user.id
            ),
        )
        .await?;
        self.best_effort_dm(
            user.id,
            format!(
                "Your account was reset by an administrator (<@{}>). You can register again with `/player register`.",
                context.actor_id
            ),
        )
        .await;
        Ok(())
    }

    async fn give_coin(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let amount = required_integer(&context.options, "amount")?;
        let user = user(&context.options, "user")?;
        let nonprofit = boolean(&context.options, "nonprofit").unwrap_or(false);
        if nonprofit && user.is_some() {
            return respond_ephemeral(
                &responder,
                "❌ Cannot specify both a user and the Jopacoin Reserve. Choose one target.",
            )
            .await;
        }
        if !nonprofit && user.is_none() {
            return respond_ephemeral(
                &responder,
                "❌ Must specify either a user or set nonprofit=True for the Jopacoin Reserve.",
            )
            .await;
        }
        let guild_id = context.guild_id;
        if nonprofit {
            let loans = self.loans.clone();
            let result = tokio::task::spawn_blocking(move || {
                let old = loans.get_nonprofit_fund(Some(guild_id))?;
                let new = if amount >= 0 {
                    loans.add_to_nonprofit_fund(Some(guild_id), amount, None)?
                } else {
                    loans.deduct_from_nonprofit_fund(
                        Some(guild_id),
                        amount
                            .checked_abs()
                            .ok_or(LoanRepositoryError::AmountOverflow)?,
                        None,
                    )?
                };
                Ok::<_, LoanRepositoryError>((old, new))
            })
            .await
            .map_err(|error| format!("reserve adjustment task failed: {error}"))?;
            return match result {
                Ok((old, new)) if amount >= 0 => {
                    respond_ephemeral(
                        &responder,
                        format!(
                            "✅ Added **{amount}** jopacoin to the Jopacoin Reserve\nReserve balance: {old} → {new}"
                        ),
                    )
                    .await
                }
                Ok((old, new)) => {
                    respond_ephemeral(
                        &responder,
                        format!(
                            "✅ Took **{}** jopacoin from the Jopacoin Reserve\nReserve balance: {old} → {new}",
                            amount.saturating_abs()
                        ),
                    )
                    .await
                }
                Err(LoanRepositoryError::InsufficientNonprofitFunds {
                    available,
                    requested,
                }) => {
                    respond_ephemeral(
                        &responder,
                        format!(
                            "❌ Jopacoin Reserve only has {available} jopacoin. Cannot take {requested}."
                        ),
                    )
                    .await
                }
                Err(error) => Err(error.to_string().into()),
            };
        }
        let user = user.expect("nonprofit false requires user");
        let target_id = signed_id(user.id, "user")?;
        let actor_id = signed_id(context.actor_id, "actor")?;
        let admin = self.admin.clone();
        let balances = tokio::task::spawn_blocking(move || {
            admin.adjust_player_balance(target_id, guild_id, amount, actor_id)
        })
        .await
        .map_err(|error| format!("player balance task failed: {error}"))?;
        let (old, new) = match balances {
            Ok(balances) => balances,
            Err(cama_db::admin::AdminRepositoryError::PlayerNotFound) => {
                return respond_ephemeral(
                    &responder,
                    format!("⚠️ <@{}> is not registered.", user.id),
                )
                .await;
            }
            Err(error) => return Err(error.to_string().into()),
        };
        let (verb, direction) = if amount >= 0 {
            ("Gave", "to")
        } else {
            ("Took", "from")
        };
        respond_ephemeral(
            &responder,
            format!(
                "✅ {verb} **{}** jopacoin {direction} <@{}>\nBalance: {old} → {new}",
                amount.saturating_abs(),
                user.id
            ),
        )
        .await
    }

    async fn reset_loan_cooldown(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let loans = self.loans.clone();
        let registered = tokio::task::spawn_blocking(move || {
            loans.get_player_balance(target_id, Some(guild_id))
        })
        .await
        .map_err(|error| format!("loan player task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if registered.is_none() {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let loans = self.loans.clone();
        tokio::task::spawn_blocking(move || loans.reset_cooldown(target_id, Some(guild_id)))
            .await
            .map_err(|error| format!("loan cooldown task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        respond_ephemeral(
            &responder,
            format!(
                "✅ Reset loan cooldown for <@{}>. They can now take a new loan.",
                user.id
            ),
        )
        .await
    }

    async fn reset_bankruptcy_cooldown(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let players = self.players.clone();
        let exists = tokio::task::spawn_blocking(move || players.exists(target_id, Some(guild_id)))
            .await
            .map_err(|error| format!("bankruptcy player task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if !exists {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let bankruptcy = self.bankruptcy.clone();
        let reset = tokio::task::spawn_blocking(move || {
            bankruptcy.reset_cooldown_only(target_id, Some(guild_id), 0, 0)
        })
        .await
        .map_err(|error| format!("bankruptcy reset task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if !reset {
            return respond_ephemeral(
                &responder,
                format!("ℹ️ <@{}> has no bankruptcy history to reset.", user.id),
            )
            .await;
        }
        respond_ephemeral(
            &responder,
            format!(
                "✅ Reset bankruptcy for <@{}>. Cooldown and penalty games cleared.",
                user.id
            ),
        )
        .await
    }

    async fn add_steam_id(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let steam_id = required_integer(&context.options, "steam_id")?;
        let set_primary = boolean(&context.options, "set_primary").unwrap_or(false);
        if steam_id <= 0 || steam_id > 1_i64 << 32 {
            return respond_ephemeral(&responder, "❌ Invalid Steam ID.").await;
        }
        if !self.registered(user.id, context.guild_id).await? {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let target_id = signed_id(user.id, "user")?;
        let steam = self.steam.clone();
        let result = tokio::task::spawn_blocking(move || {
            let current = steam.get_steam_ids(target_id)?;
            let primary = set_primary || current.is_empty();
            steam.add_steam_id(target_id, steam_id, primary, now_seconds())?;
            Ok::<_, cama_db::opendota_player::OpenDotaPlayerRepositoryError>(primary)
        })
        .await
        .map_err(|error| format!("Steam link task failed: {error}"))?;
        match result {
            Ok(primary) => {
                let note = if primary { " (set as primary)" } else { "" };
                respond_ephemeral(
                    &responder,
                    format!(
                        "✅ Added Steam ID `{steam_id}` to <@{}>'s account{note}.",
                        user.id
                    ),
                )
                .await
            }
            Err(error) => respond_ephemeral(&responder, format!("❌ {error}")).await,
        }
    }

    async fn remove_steam_id(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let steam_id = required_integer(&context.options, "steam_id")?;
        if !self.registered(user.id, context.guild_id).await? {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let target_id = signed_id(user.id, "user")?;
        let steam = self.steam.clone();
        let result = tokio::task::spawn_blocking(move || {
            let current = steam.get_steam_ids(target_id)?;
            if !current.contains(&steam_id) {
                return Ok((false, current));
            }
            let removed = steam.remove_steam_id(target_id, steam_id)?;
            let remaining = steam.get_steam_ids(target_id)?;
            Ok::<_, cama_db::opendota_player::OpenDotaPlayerRepositoryError>((removed, remaining))
        })
        .await
        .map_err(|error| format!("Steam unlink task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let (removed, remaining) = result;
        if !removed {
            return respond_ephemeral(
                &responder,
                format!("❌ Steam ID `{steam_id}` is not linked to <@{}>.", user.id),
            )
            .await;
        }
        let suffix = remaining.first().map_or_else(
            || "They no longer have any linked Steam accounts.".to_owned(),
            |primary| format!("Primary is now `{primary}`."),
        );
        respond_ephemeral(
            &responder,
            format!(
                "✅ Removed Steam ID `{steam_id}` from <@{}>'s account.\n{suffix}",
                user.id
            ),
        )
        .await
    }

    async fn set_primary_steam(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let steam_id = required_integer(&context.options, "steam_id")?;
        if !self.registered(user.id, context.guild_id).await? {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let target_id = signed_id(user.id, "user")?;
        let steam = self.steam.clone();
        let current = tokio::task::spawn_blocking(move || steam.get_steam_ids(target_id))
            .await
            .map_err(|error| format!("Steam list task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if !current.contains(&steam_id) {
            let linked = if current.is_empty() {
                "none".to_owned()
            } else {
                current
                    .iter()
                    .map(|id| format!("`{id}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return respond_ephemeral(
                &responder,
                format!(
                    "❌ Steam ID `{steam_id}` is not linked to <@{}>.\nLinked accounts: {linked}",
                    user.id
                ),
            )
            .await;
        }
        let steam = self.steam.clone();
        let set =
            tokio::task::spawn_blocking(move || steam.set_primary_steam_id(target_id, steam_id))
                .await
                .map_err(|error| format!("Steam primary task failed: {error}"))?
                .map_err(|error| error.to_string())?;
        if set {
            respond_ephemeral(
                &responder,
                format!(
                    "✅ Set `{steam_id}` as <@{}>'s primary Steam account.",
                    user.id
                ),
            )
            .await
        } else {
            respond_ephemeral(&responder, "❌ Failed to set primary Steam ID.").await
        }
    }

    async fn registered(
        &self,
        user_id: u64,
        guild_id: i64,
    ) -> Result<bool, InteractionHandlerError> {
        let target_id = signed_id(user_id, "user")?;
        let players = self.players.clone();
        tokio::task::spawn_blocking(move || players.exists(target_id, Some(guild_id)))
            .await
            .map_err(|error| format!("player lookup task failed: {error}"))?
            .map_err(|error| error.to_string().into())
    }
}

impl AdminHandler {
    async fn adjust_rating(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let rating = required_number(&context.options, "rating")?;
        if !(0.0..=self.config.admin_rating_max).contains(&rating) {
            return respond_ephemeral(
                &responder,
                format!(
                    "❌ Rating must be between 0 and {:.0}.",
                    self.config.admin_rating_max
                ),
            )
            .await;
        }
        let target_id = signed_id(user.id, "user")?;
        let repository = self.admin.clone();
        let guild_id = context.guild_id;
        let snapshot = tokio::task::spawn_blocking(move || repository.player(target_id, guild_id))
            .await
            .map_err(|error| format!("admin player task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        let Some(snapshot) = snapshot else {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        };
        let Some(old_rating) = snapshot.glicko_rating else {
            return respond_ephemeral(
                &responder,
                format!("❌ <@{}> has no Glicko rating.", user.id),
            )
            .await;
        };
        let Some(rd) = snapshot.glicko_rd else {
            return Err("rated player has no Glicko RD".into());
        };
        let rd = cap_glicko_rd(rd);
        let Some(volatility) = snapshot.glicko_volatility else {
            return Err("rated player has no Glicko volatility".into());
        };
        let repository = self.admin.clone();
        let actor_id = signed_id(context.actor_id, "actor")?;
        tokio::task::spawn_blocking(move || {
            repository.set_both_display_ratings(SetBothDisplayRatingsRequest {
                discord_id: target_id,
                guild_id,
                display_rating: rating,
                rd,
                volatility,
                reason: "admin_rating_override",
                actor_id,
            })
        })
        .await
        .map_err(|error| format!("admin rating task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        respond_ephemeral(
            &responder,
            format!(
                "✅ Set rating for <@{}>: {old_rating:.0} → **{rating:.0}** (RD kept at {rd:.1}; OpenSkill σ unchanged).",
                user.id
            ),
        )
        .await
    }

    async fn adjust_rd(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let rd = required_number(&context.options, "rd")?;
        if !(0.0..=MAX_GLICKO_RD).contains(&rd) {
            return respond_ephemeral(
                &responder,
                format!("❌ RD must be between 0 and {MAX_GLICKO_RD:.0}."),
            )
            .await;
        }
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let repository = self.admin.clone();
        let snapshot = tokio::task::spawn_blocking(move || repository.player(target_id, guild_id))
            .await
            .map_err(|error| format!("admin player task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        let Some(snapshot) = snapshot else {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        };
        let Some(rating) = snapshot.glicko_rating else {
            return respond_ephemeral(
                &responder,
                format!("❌ <@{}> has no Glicko rating.", user.id),
            )
            .await;
        };
        let Some(old_rd) = snapshot.glicko_rd else {
            return Err("rated player has no Glicko RD".into());
        };
        let Some(volatility) = snapshot.glicko_volatility else {
            return Err("rated player has no Glicko volatility".into());
        };
        let repository = self.admin.clone();
        tokio::task::spawn_blocking(move || {
            repository.set_glicko_uncertainty(target_id, guild_id, rating, rd, volatility)
        })
        .await
        .map_err(|error| format!("admin RD task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        respond_ephemeral(
            &responder,
            format!(
                "✅ Set RD for <@{}>: {old_rd:.1} → **{rd:.1}** (rating kept at {rating:.0}).",
                user.id
            ),
        )
        .await
    }

    async fn set_initial_rating(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let rating = required_number(&context.options, "rating")?;
        if !(0.0..=self.config.admin_rating_max).contains(&rating) {
            return respond_ephemeral(
                &responder,
                format!(
                    "❌ Rating must be between 0 and {:.0}.",
                    self.config.admin_rating_max
                ),
            )
            .await;
        }
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let repository = self.admin.clone();
        let snapshot = tokio::task::spawn_blocking(move || repository.player(target_id, guild_id))
            .await
            .map_err(|error| format!("admin player task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        let Some(snapshot) = snapshot else {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        };
        if snapshot.games_played() >= self.config.admin_rating_adjustment_max_games {
            return respond_ephemeral(
                &responder,
                "❌ Player has too many games for initial rating adjustment.",
            )
            .await;
        }
        let rd = cap_glicko_rd(snapshot.glicko_rd.unwrap_or(self.config.initial_glicko_rd));
        let volatility = snapshot.glicko_volatility.unwrap_or(0.06);
        let actor_id = signed_id(context.actor_id, "actor")?;
        let repository = self.admin.clone();
        tokio::task::spawn_blocking(move || {
            repository.set_both_display_ratings(SetBothDisplayRatingsRequest {
                discord_id: target_id,
                guild_id,
                display_rating: rating,
                rd,
                volatility,
                reason: "admin_initial_rating",
                actor_id,
            })
        })
        .await
        .map_err(|error| format!("initial rating task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        respond_ephemeral(
            &responder,
            format!(
                "✅ Set initial rating for <@{}> to {rating} (RD kept at {rd:.1}).",
                user.id
            ),
        )
        .await
    }

    async fn bump_rd(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let amount = integer(&context.options, "amount").unwrap_or(100);
        if !(1..=MAX_GLICKO_RD as i64).contains(&amount) {
            return respond_ephemeral(&responder, "❌ Amount must be between 1 and 250.").await;
        }
        defer(&responder, true).await?;
        let repository = self.admin.clone();
        let actor_id = signed_id(context.actor_id, "actor")?;
        let guild_id = context.guild_id;
        let result = tokio::task::spawn_blocking(move || {
            repository.bump_uncertainties(guild_id, amount as f64, actor_id)
        })
        .await
        .map_err(|error| format!("RD bump task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let Some(result) = result else {
            return respond_ephemeral(&responder, "❌ No rated players found.").await;
        };
        let mut content = format!(
            "✅ Bumped RD for **{}** players by **+{amount}** (capped at {MAX_GLICKO_RD:.0})\n• Avg RD: {:.0} → **{:.0}**",
            result.count, result.average_rd_before, result.average_rd_after
        );
        if let (Some(before), Some(after)) =
            (result.average_sigma_before, result.average_sigma_after)
        {
            content.push_str(&format!(
                "\n• Avg OpenSkill σ: {before:.2} → **{after:.2}**"
            ));
        }
        respond_ephemeral(&responder, content).await
    }

    async fn recalibrate(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let admin = self.admin.clone();
        let snapshot = tokio::task::spawn_blocking(move || admin.player(target_id, guild_id))
            .await
            .map_err(|error| format!("recalibration player task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        let Some(snapshot) = snapshot else {
            return respond_ephemeral(&responder, format!("❌ <@{}> is not registered.", user.id))
                .await;
        };
        let Some(rating) = snapshot.glicko_rating else {
            return respond_ephemeral(
                &responder,
                format!("❌ <@{}> has no Glicko rating.", user.id),
            )
            .await;
        };
        if snapshot.games_played() < RECALIBRATION_MIN_GAMES {
            return respond_ephemeral(
                &responder,
                format!(
                    "❌ <@{}> must play at least {RECALIBRATION_MIN_GAMES} games before recalibrating. Current: {} games.",
                    user.id,
                    snapshot.games_played()
                ),
            )
            .await;
        }
        let old_rd = snapshot.glicko_rd.ok_or("rated player has no Glicko RD")?;
        let new_rd = self
            .config
            .recalibration_initial_rd
            .max(old_rd)
            .min(MAX_GLICKO_RD);
        let old_os_sigma = snapshot.os_sigma;
        let new_os_sigma = old_os_sigma.map(|_| CamaOpenSkillSystem::DEFAULT_SIGMA);
        let now = now_seconds();
        let cooldown = self.config.recalibration_cooldown_seconds;
        let repository = self.recalibration.clone();
        let request = RecalibrationRequest {
            discord_id: target_id,
            guild_id: Some(guild_id),
            now,
            cooldown_seconds: cooldown,
            rating,
            new_rd,
            new_volatility: self.config.recalibration_initial_volatility,
            new_os_sigma,
        };
        let total = match tokio::task::spawn_blocking(move || {
            repository.execute_recalibration_atomic(request)
        })
        .await
        .map_err(|error| format!("recalibration task failed: {error}"))?
        {
            Ok(total) => total,
            Err(cama_db::rating_history_repository::RecalibrationRepositoryError::OnCooldown {
                remaining_seconds,
            }) => {
                return respond_ephemeral(
                    &responder,
                    format!(
                        "❌ <@{}> is on recalibration cooldown. Can recalibrate again <t:{}:R>.",
                        user.id,
                        now.saturating_add(remaining_seconds)
                    ),
                )
                .await;
            }
            Err(error) => return Err(error.to_string().into()),
        };
        let mut os_line = String::new();
        if let (Some(old), Some(new)) = (old_os_sigma, new_os_sigma) {
            os_line = format!("• OpenSkill σ: {old:.2} → **{new:.2}**\n");
        }
        respond_ephemeral(
            &responder,
            format!(
                "✅ Recalibrated <@{}>!\n• Rating: **{rating:.0}** (unchanged)\n• RD: {old_rd:.1} → **{new_rd:.0}** (high uncertainty)\n{os_line}• Total recalibrations: {total}\n• Next recalibration available: <t:{}:R>",
                user.id,
                now.saturating_add(cooldown)
            ),
        )
        .await
    }

    async fn reset_recalibration_cooldown(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let user = required_user(&context.options, "user")?;
        let target_id = signed_id(user.id, "user")?;
        let guild_id = context.guild_id;
        let repository = self.admin.clone();
        let snapshot = tokio::task::spawn_blocking(move || repository.player(target_id, guild_id))
            .await
            .map_err(|error| format!("admin player task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if snapshot.is_none() {
            return respond_ephemeral(&responder, format!("⚠️ <@{}> is not registered.", user.id))
                .await;
        }
        let repository = self.admin.clone();
        let reset = tokio::task::spawn_blocking(move || {
            repository.reset_recalibration_cooldown(target_id, guild_id)
        })
        .await
        .map_err(|error| format!("recalibration cooldown task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if !reset {
            return respond_ephemeral(
                &responder,
                format!("ℹ️ <@{}> has no recalibration history to reset.", user.id),
            )
            .await;
        }
        respond_ephemeral(
            &responder,
            format!(
                "✅ Reset recalibration cooldown for <@{}>. They can now recalibrate.",
                user.id
            ),
        )
        .await
    }
}

impl AdminHandler {
    async fn moderation_suspend(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let player = required_user(&context.options, "player")?;
        if player.is_bot == Some(true) {
            return respond_ephemeral(
                &responder,
                "❌ Bot accounts cannot be suspended from player lobbies.",
            )
            .await;
        }
        let reason = required_string(&context.options, "reason")?;
        let duration = string(&context.options, "duration");
        let matches = integer(&context.options, "matches");
        let completion_raw = string(&context.options, "completion");
        if duration.is_none() && matches.is_none() {
            return respond_ephemeral(
                &responder,
                "❌ Supply at least one term: `duration` and/or `matches`.",
            )
            .await;
        }
        if duration.is_some() && matches.is_some() && completion_raw.is_none() {
            return respond_ephemeral(
                &responder,
                "❌ Choose whether `either` term releases the suspension or `both` are required.",
            )
            .await;
        }
        if (duration.is_none() || matches.is_none()) && completion_raw.is_some() {
            return respond_ephemeral(
                &responder,
                "❌ `completion` is only used when both duration and matches are supplied.",
            )
            .await;
        }
        let now = now_seconds();
        let expires_at = match duration.as_deref() {
            Some(duration) => match parse_expiry(duration, now) {
                Ok(expires_at) => Some(expires_at),
                Err(error) => {
                    return respond_ephemeral(&responder, format!("❌ {error}")).await;
                }
            },
            None => None,
        };
        if !try_defer(&responder, true).await {
            return Ok(());
        }
        let completion = match completion_raw.as_deref() {
            Some("either") => SuspensionCompletion::Either,
            Some("both") => SuspensionCompletion::Both,
            None if duration.is_some() => SuspensionCompletion::Time,
            None => SuspensionCompletion::Matches,
            Some(value) => return Err(format!("invalid suspension completion {value:?}").into()),
        };
        let scope = match string(&context.options, "lobby").as_deref() {
            None | Some("all") => SuspensionScope::All,
            Some("open") => SuspensionScope::Open,
            Some("lowskill") => SuspensionScope::Lowskill,
            Some(value) => return Err(format!("invalid suspension lobby {value:?}").into()),
        };
        let replace = boolean(&context.options, "replace").unwrap_or(false);
        let target_id = signed_id(player.id, "player")?;
        let actor_id = signed_id(context.actor_id, "actor")?;
        let guild_id = context.guild_id;
        let moderation = self.moderation.clone();
        let reason_for_write = reason.clone();
        let persisted = tokio::task::spawn_blocking(move || {
            let existing = if replace {
                moderation
                    .get_active_suspension(target_id, Some(guild_id), None, now)
                    .map_err(moderation_error)?
            } else {
                None
            };
            let watermark = moderation
                .port()
                .pending_match_watermark(Some(guild_id))
                .map_err(|error| error.to_string())?;
            let state = moderation.create_suspension(CreateSuspension {
                discord_id: target_id,
                guild_id: Some(guild_id),
                actor_id,
                reason: &reason_for_write,
                scope,
                completion,
                expires_at,
                matches,
                source: ModerationSource::Admin,
                replace,
                pending_match_watermark: Some(watermark),
                now,
            });
            match state {
                Ok(state) => Ok(SuspendPersistence::Saved {
                    state,
                    replaced: existing.is_some(),
                }),
                Err(ModerationServiceError::Policy(
                    cama_app::moderation::ModerationPolicyError::ActiveSuspensionExists,
                )) => {
                    let active = moderation
                        .get_active_suspension(target_id, Some(guild_id), None, now)
                        .map_err(moderation_error)?;
                    Ok::<_, String>(SuspendPersistence::ActiveExists(
                        active.ok_or_else(|| "active suspension changed; try again".to_owned())?,
                    ))
                }
                Err(error) => Err(moderation_error(error)),
            }
        })
        .await
        .map_err(|error| format!("moderation suspend task failed: {error}"))??;
        let (state, replaced) = match persisted {
            SuspendPersistence::Saved { state, replaced } => (state, replaced),
            SuspendPersistence::ActiveExists(state) => {
                return respond_ephemeral(
                    &responder,
                    format!(
                        "❌ <@{}> already has an active suspension from **{}**: {} (reason: {}). Re-run with `replace: True` to overwrite it.",
                        player.id,
                        suspension_scope(&state),
                        suspension_terms(&state),
                        state.reason
                    ),
                )
                .await;
            }
        };
        self.best_effort_dm(
            player.id,
            format!(
                "You have been temporarily suspended from {}.\nTerm: {}\nReason: {}\nUse `/player lobby status` in the server for your current status.",
                suspension_scope(&state),
                suspension_terms(&state),
                reason
            ),
        )
        .await;
        let lobby_scope = match scope {
            SuspensionScope::All => AdminLobbyScope::All,
            SuspensionScope::Open => AdminLobbyScope::Open,
            SuspensionScope::Lowskill => AdminLobbyScope::Lowskill,
        };
        let ejection = self
            .ports
            .lobby
            .eject_suspended_player(AdminLobbyEjectionRequest {
                guild_id,
                player_id: target_id,
                player_display_name: player
                    .display_name
                    .clone()
                    .unwrap_or_else(|| format!("<@{}>", player.id)),
                scope: lobby_scope,
            })
            .await
            .map_err(|error| format!("suspension lobby ejection failed: {error}"))?;
        let action = if replaced {
            "Replaced the active suspension for"
        } else {
            "Suspended"
        };
        let mut outcome = if ejection.evicted_lobbies.is_empty() {
            String::new()
        } else {
            format!(
                " Removed from {}.",
                format_lobby_labels(&ejection.evicted_lobbies)
            )
        };
        if !ejection.applicable_lobbies.is_empty()
            && ejection.evicted_lobbies.len() < ejection.applicable_lobbies.len()
        {
            outcome.push_str(" Their already-starting match was left intact.");
        }
        respond_ephemeral(
            &responder,
            format!(
                "✅ {action} <@{}> from **{}**: {}.{outcome}\nReason: {reason}",
                player.id,
                suspension_scope(&state),
                suspension_terms(&state)
            ),
        )
        .await
    }

    async fn moderation_lift(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let player = required_user(&context.options, "player")?;
        let reason = required_string(&context.options, "reason")?;
        let target_id = signed_id(player.id, "player")?;
        let actor_id = signed_id(context.actor_id, "actor")?;
        let guild_id = context.guild_id;
        let moderation = self.moderation.clone();
        let reason_for_write = reason.clone();
        let lifted = tokio::task::spawn_blocking(move || {
            moderation
                .lift_suspension(
                    target_id,
                    Some(guild_id),
                    actor_id,
                    &reason_for_write,
                    now_seconds(),
                )
                .map_err(moderation_error)
        })
        .await
        .map_err(|error| format!("moderation lift task failed: {error}"))??;
        if lifted.is_none() {
            return respond_ephemeral(
                &responder,
                format!("<@{}> has no active lobby suspension.", player.id),
            )
            .await;
        }
        self.best_effort_dm(
            player.id,
            format!("Your matchmaking lobby suspension has been lifted.\nReason: {reason}"),
        )
        .await;
        respond_ephemeral(
            &responder,
            format!(
                "✅ Lifted <@{}>'s lobby suspension.\nReason: {reason}",
                player.id
            ),
        )
        .await
    }

    async fn moderation_status(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let player = required_user(&context.options, "player")?;
        let target_id = signed_id(player.id, "player")?;
        let guild_id = context.guild_id;
        let moderation = self.moderation.clone();
        let state = tokio::task::spawn_blocking(move || {
            moderation
                .get_active_suspension(target_id, Some(guild_id), None, now_seconds())
                .map_err(moderation_error)
        })
        .await
        .map_err(|error| format!("moderation status task failed: {error}"))??;
        let content = state.map_or_else(
            || format!("<@{}> has no active lobby suspension.", player.id),
            |state| {
                format!(
                    "**<@{}>** — {}\nTerm: {}\nReason: {}\nIssued by: <@{}>\nApplied: <t:{}:F>",
                    player.id,
                    suspension_scope(&state),
                    suspension_terms(&state),
                    state.reason,
                    state.actor_id,
                    state.created_at
                )
            },
        );
        respond_ephemeral(&responder, content).await
    }

    async fn moderation_list(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let guild_id = context.guild_id;
        let moderation = self.moderation.clone();
        let states = tokio::task::spawn_blocking(move || {
            moderation
                .list_active_suspensions(Some(guild_id), None, now_seconds())
                .map_err(moderation_error)
        })
        .await
        .map_err(|error| format!("moderation list task failed: {error}"))??;
        let content = if states.is_empty() {
            "No active lobby suspensions.".to_owned()
        } else {
            let mut lines = states
                .iter()
                .take(10)
                .map(|state| {
                    format!(
                        "<@{}> — {}; {}; reason: {}; by <@{}> at <t:{}:f>",
                        state.discord_id,
                        suspension_scope(state),
                        suspension_terms(state),
                        state.reason,
                        state.actor_id,
                        state.created_at
                    )
                })
                .collect::<Vec<_>>();
            if states.len() > 10 {
                lines.push(format!("…and {} more", states.len() - 10));
            }
            lines.join("\n")
        };
        respond_ephemeral(&responder, truncate_discord(content)).await
    }

    async fn moderation_history(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let player_id = user(&context.options, "player")?
            .map(|player| signed_id(player.id, "player"))
            .transpose()?;
        let guild_id = context.guild_id;
        let moderation = self.moderation.clone();
        let events = tokio::task::spawn_blocking(move || {
            moderation
                .port()
                .history(Some(guild_id), player_id, 20, 0)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("moderation history task failed: {error}"))??;
        let content = if events.is_empty() {
            "No moderation history found.".to_owned()
        } else {
            events
                .iter()
                .map(|event| {
                    let actor = event
                        .actor_id
                        .map_or_else(String::new, |actor| format!(" by <@{actor}>"));
                    let reason = event
                        .reason
                        .as_deref()
                        .map_or_else(String::new, |reason| format!(" — {reason}"));
                    let mut details = vec![format!("source={}", event.source.name())];
                    if let Some(scope) = event.scope {
                        details.push(format!("scope={}", scope.name()));
                    }
                    if let Some(required) = event.matches_required {
                        details.push(format!(
                            "matches={}/{} remaining",
                            event.matches_remaining.unwrap_or_default(),
                            required
                        ));
                    }
                    if let Some(required) = event.wins_required {
                        details.push(format!(
                            "wins={}/{} remaining",
                            event.wins_remaining.unwrap_or_default(),
                            required
                        ));
                    }
                    if let Some(match_id) = event.match_id {
                        details.push(format!("match=#{match_id}"));
                    }
                    format!(
                        "<t:{}:f> **{}** <@{}>{actor} ({}){reason}",
                        event.created_at,
                        event.event_type.name().replace('_', " "),
                        event.discord_id,
                        details.join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        respond_ephemeral(&responder, truncate_discord(content)).await
    }
}

impl AdminHandler {
    async fn extend_betting(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let minutes = required_integer(&context.options, "minutes")?;
        let pending_match_id = required_integer(&context.options, "pending_match_id")?;
        if !(1..=60).contains(&minutes) {
            return respond_ephemeral(&responder, "❌ Extension must be between 1 and 60 minutes.")
                .await;
        }
        let result = self
            .ports
            .matches
            .extend_betting(AdminExtendBettingRequest {
                guild_id: context.guild_id,
                actor_id: signed_id(context.actor_id, "actor")?,
                minutes,
                pending_match_id,
            })
            .await;
        let result = match result {
            Ok(result) => result,
            Err(message) => {
                return respond_ephemeral(&responder, format!("❌ {message}")).await;
            }
        };
        for failure in &result.refresh_failures {
            warn!(
                pending_match_id,
                failure, "match copy refresh failed after betting extension"
            );
        }
        let jump_link = result
            .jump_url
            .as_deref()
            .filter(|url| !url.is_empty())
            .map_or_else(String::new, |url| format!(" [View match]({url})"));
        send_response(
            &responder,
            InteractionResponse::message(format!(
                "⏰ **{} / Match #{} — betting window extended by {minutes} minute(s)!** Closes <t:{}:R>.{jump_link}",
                result.lobby_label, result.pending_match_id, result.new_bet_lock_until
            )),
        )
        .await
    }

    async fn seed_hero_grid(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.admin_allowed(&context) {
            return respond_ephemeral(&responder, "❌ Admin only command.").await;
        }
        defer(&responder, true).await?;
        let result = self
            .ports
            .matches
            .seed_hero_grid(AdminSeedHeroGridRequest {
                guild_id: context.guild_id,
                actor_id: signed_id(context.actor_id, "actor")?,
            })
            .await;
        match result {
            Ok(result) => {
                respond_ephemeral(
                    &responder,
                    format!(
                        "✅ Seeded {} players and {} enriched matches.\nRun `/herogrid source:All Players min_games:1` to see the grid.",
                        result.players_seeded, result.matches_created
                    ),
                )
                .await
            }
            Err(message) => respond_ephemeral(&responder, format!("❌ {message}")).await,
        }
    }

    async fn correct_match(
        &self,
        context: AdminCommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let _deferred = try_defer(&responder, true).await;
        let match_id = required_integer(&context.options, "match_id")?;
        let correct_result = match required_string(&context.options, "correct_result")?.as_str() {
            "radiant" => AdminCorrectMatchSide::Radiant,
            "dire" => AdminCorrectMatchSide::Dire,
            value => {
                return Err(format!("invalid correct_result option {value:?}").into());
            }
        };
        let request = AdminCorrectMatchRequest {
            guild_id: context.guild_id,
            actor_id: signed_id(context.actor_id, "actor")?,
            match_id,
            correct_result,
        };
        match self.ports.match_corrections.correct_match(request).await {
            Ok(result) => {
                respond_ephemeral(&responder, format_correct_match_result(match_id, result)).await
            }
            Err(AdminCorrectMatchError::InvalidRequest(message)) => {
                respond_ephemeral(&responder, format!("❌ {message}")).await
            }
            Err(AdminCorrectMatchError::Unexpected(message)) => {
                error!(match_id, %message, "admin match correction failed");
                respond_ephemeral(
                    &responder,
                    format!("❌ Unexpected error correcting match: {message}"),
                )
                .await
            }
        }
    }
}

fn nested_command_path(
    options: &[InteractionOption],
) -> Result<(Vec<String>, Vec<InteractionOption>), InteractionHandlerError> {
    let Some(option) = options.iter().find(|option| {
        matches!(
            option.value,
            InteractionValue::Subcommand(_) | InteractionValue::SubcommandGroup(_)
        )
    }) else {
        return Err("/admin interaction is missing a subcommand".into());
    };
    let mut path = vec![option.name.clone()];
    let mut children = match &option.value {
        InteractionValue::Subcommand(children) | InteractionValue::SubcommandGroup(children) => {
            children.clone()
        }
        _ => unreachable!("filtered to nested command option"),
    };
    if matches!(option.value, InteractionValue::SubcommandGroup(_)) {
        let (nested_path, nested_options) = nested_command_path(&children)?;
        path.extend(nested_path);
        children = nested_options;
    }
    Ok((path, children))
}

async fn send_response(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), InteractionHandlerError> {
    if responder.response_is_done() {
        responder.followup(response).await
    } else {
        responder.respond(response).await
    }
    .map_err(|error| error.to_string().into())
}

async fn defer(
    responder: &Arc<dyn InteractionResponder>,
    ephemeral: bool,
) -> Result<(), InteractionHandlerError> {
    responder
        .defer(ephemeral)
        .await
        .map_err(|error| error.to_string().into())
}

async fn try_defer(responder: &Arc<dyn InteractionResponder>, ephemeral: bool) -> bool {
    responder.defer(ephemeral).await.is_ok()
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), InteractionHandlerError> {
    send_response(
        responder,
        InteractionResponse::message(content.into()).ephemeral(),
    )
    .await
}

fn signed_id(id: u64, kind: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(id).map_err(|_| format!("{kind} id exceeds SQLite INTEGER range").into())
}

fn required_user(
    options: &[InteractionOption],
    name: &str,
) -> Result<AdminUserOption, InteractionHandlerError> {
    let Some(InteractionValue::User {
        id,
        display_name,
        is_bot,
    }) = options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
    else {
        return Err(format!("missing required user option {name:?}").into());
    };
    Ok(AdminUserOption {
        id: *id,
        display_name: display_name.clone(),
        is_bot: *is_bot,
    })
}

fn required_number(
    options: &[InteractionOption],
    name: &str,
) -> Result<f64, InteractionHandlerError> {
    match options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
    {
        Some(InteractionValue::Number(value)) => Ok(*value),
        _ => Err(format!("missing required number option {name:?}").into()),
    }
}

fn user(
    options: &[InteractionOption],
    name: &str,
) -> Result<Option<AdminUserOption>, InteractionHandlerError> {
    match options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
    {
        Some(InteractionValue::User {
            id,
            display_name,
            is_bot,
        }) => Ok(Some(AdminUserOption {
            id: *id,
            display_name: display_name.clone(),
            is_bot: *is_bot,
        })),
        None => Ok(None),
        Some(_) => Err(format!("option {name:?} is not a user").into()),
    }
}

fn integer(options: &[InteractionOption], name: &str) -> Option<i64> {
    match options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
    {
        Some(InteractionValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn required_integer(
    options: &[InteractionOption],
    name: &str,
) -> Result<i64, InteractionHandlerError> {
    integer(options, name).ok_or_else(|| format!("missing required integer option {name:?}").into())
}

fn string(options: &[InteractionOption], name: &str) -> Option<String> {
    match options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
    {
        Some(InteractionValue::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn required_string(
    options: &[InteractionOption],
    name: &str,
) -> Result<String, InteractionHandlerError> {
    string(options, name).ok_or_else(|| format!("missing required string option {name:?}").into())
}

fn boolean(options: &[InteractionOption], name: &str) -> Option<bool> {
    match options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
    {
        Some(InteractionValue::Boolean(value)) => Some(*value),
        _ => None,
    }
}

fn truncate_discord(content: String) -> String {
    content.chars().take(2_000).collect()
}

fn format_correct_match_result(match_id: i64, result: AdminCorrectMatchResult) -> String {
    if result.replay_recovered {
        return format!(
            "✅ Recovered the pending OpenSkill replay for match #{match_id} ({} matches replayed).",
            result.openskill_matches_replayed
        );
    }

    let mut lines = vec![
        format!("✅ **Match #{match_id} corrected!**"),
        format!(
            "• Result changed: {} → **{}**",
            result.old_winning_team.title(),
            result.new_winning_team.title()
        ),
        format!("• Players affected: {}", result.players_affected),
        format!("• Ratings updated: {}", result.ratings_updated),
    ];
    if let Some(bets) = result.bet_correction.filter(|bets| bets.bets_affected > 0) {
        lines.push(format!(
            "• Bets corrected: {} ({} reversed, {} paid)",
            bets.bets_affected, bets.old_winners_reversed, bets.new_winners_paid
        ));
    }
    lines.push(format!(
        "\n*Correction ID: {}*",
        result
            .correction_id
            .map_or_else(|| "N/A".to_owned(), |id| id.to_string())
    ));
    lines.join("\n")
}

fn moderation_error(
    error: ModerationServiceError<cama_db::moderation::ModerationRepositoryError>,
) -> String {
    match error {
        ModerationServiceError::Policy(error) => error.to_string(),
        ModerationServiceError::Port(error) => error.to_string(),
    }
}

fn suspension_scope(state: &LobbySuspension) -> &'static str {
    match state.scope {
        SuspensionScope::All => "all matchmaking lobbies",
        SuspensionScope::Open => "🍽️ All You Can Feed",
        SuspensionScope::Lowskill => "🧀 Whine & Cheese",
    }
}

fn suspension_terms(state: &LobbySuspension) -> String {
    let mut parts = Vec::new();
    if let Some(expires_at) = state.expires_at {
        parts.push(format!("until <t:{expires_at}:F> (<t:{expires_at}:R>)"));
    }
    if let Some(remaining) = state.matches_remaining {
        let noun = if remaining == 1 { "match" } else { "matches" };
        parts.push(format!("{remaining} completed {noun} remaining"));
    }
    match parts.as_slice() {
        [] => "no remaining term".to_owned(),
        [only] => only.clone(),
        _ => parts.join(if state.completion == SuspensionCompletion::Either {
            " or "
        } else {
            " and "
        }),
    }
}

fn format_lobby_labels(labels: &[String]) -> String {
    match labels {
        [] => String::new(),
        [only] => only.clone(),
        _ => format!(
            "{} and {}",
            labels[..labels.len() - 1].join(", "),
            labels.last().expect("nonempty labels")
        ),
    }
}

fn project_counts_payload(value: &JsonValue) -> CountsPayload {
    let region = value
        .get("region")
        .and_then(JsonValue::as_object)
        .map(|regions| {
            regions
                .iter()
                .map(|(key, entry)| {
                    let projected = if let Some(object) = entry.as_object() {
                        object
                            .get("games")
                            .map_or(RegionCountEntry::GamesMissing, |games| {
                                RegionCountEntry::Games(project_game_count(games))
                            })
                    } else {
                        RegionCountEntry::Bare(project_game_count(entry))
                    };
                    (key.clone(), projected)
                })
                .collect()
        });
    CountsPayload { region }
}

fn project_game_count(value: &JsonValue) -> GameCountValue {
    match value {
        JsonValue::Null => GameCountValue::Null,
        JsonValue::Bool(value) => GameCountValue::Boolean(*value),
        JsonValue::Number(value) => value.as_i64().map_or_else(
            || {
                value.as_u64().map_or_else(
                    || {
                        value
                            .as_f64()
                            .map_or(GameCountValue::Malformed, GameCountValue::Float)
                    },
                    GameCountValue::UnsignedInteger,
                )
            },
            GameCountValue::Integer,
        ),
        JsonValue::String(value) => GameCountValue::String(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => GameCountValue::Malformed,
    }
}

fn format_health_snapshot(snapshot: &AdminHealthSnapshot) -> String {
    let db_latency = snapshot
        .db_latency_ms
        .map_or_else(|| "failed".to_owned(), |latency| format!("{latency:.1} ms"));
    let discord_latency = snapshot.discord_latency_ms.map_or_else(
        || "unavailable".to_owned(),
        |latency| format!("{latency} ms"),
    );
    let open_fds = snapshot
        .open_fds
        .map_or_else(|| "unavailable".to_owned(), |count| count.to_string());
    let top_commands = if snapshot.usage.commands_ranked.is_empty() {
        "none".to_owned()
    } else {
        snapshot
            .usage
            .commands_ranked
            .iter()
            .map(|(name, count)| format!("{name}: {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let api_requests = if snapshot.usage.api_requests.is_empty() {
        "none".to_owned()
    } else {
        snapshot
            .usage
            .api_requests
            .iter()
            .map(|(name, count)| format!("{name}: {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let reasons = if snapshot.reasons.is_empty() {
        "- none".to_owned()
    } else {
        snapshot
            .reasons
            .iter()
            .map(|reason| format!("- {reason}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "**Status:** {}\n\
         **Uptime:** {}\n\
         **Started:** {}\n\
         **Git SHA:** `{}`\n\
         **Python:** not applicable (Rust runtime)\n\
         **Discord latency:** {}\n\
         **Guilds:** {}\n\
         **DB:** {} ({}), size {}\n\
         **Peak RSS:** {}\n\
         **Open FDs:** {}\n\
         **IO blocks:** in {}, out {}\n\
         **Commands:** {} total, {} failed\n\
         **Top commands:** {}\n\
         **API requests:** {}\n\
         **Degraded reasons:**\n{}",
        if snapshot.status == "ok" {
            "OK"
        } else {
            "DEGRADED"
        },
        snapshot.uptime,
        snapshot.started_at,
        snapshot.git_sha,
        discord_latency,
        snapshot.guild_count,
        if snapshot.db_ok { "ok" } else { "failed" },
        db_latency,
        snapshot.db_size,
        snapshot.memory_rss,
        open_fds,
        snapshot.io_blocks_in,
        snapshot.io_blocks_out,
        snapshot.usage.command_total,
        snapshot.usage.command_failures,
        top_commands,
        api_requests,
        reasons
    )
}

fn short_git_sha() -> String {
    short_git_sha_with(std::env::var("GIT_SHA").ok(), git_short_sha)
}

fn short_git_sha_with(
    environment: Option<String>,
    fallback: impl FnOnce() -> Option<String>,
) -> String {
    if let Some(value) = environment {
        let value = value.trim();
        if !value.is_empty() {
            return value.chars().take(12).collect();
        }
    }
    fallback()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn git_short_sha() -> Option<String> {
    let Ok(mut child) = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return None;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut output = String::new();
                if child
                    .stdout
                    .take()
                    .is_some_and(|mut stdout| stdout.read_to_string(&mut output).is_ok())
                {
                    return Some(output);
                }
                return None;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 || !parts.is_empty() {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 || !parts.is_empty() {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

fn format_bytes(bytes: u64) -> String {
    let mut size = bytes as f64;
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if size < 1_024.0 || unit == "GiB" {
            return if unit == "B" {
                format!("{bytes} B")
            } else {
                format!("{size:.1} {unit}")
            };
        }
        size /= 1_024.0;
    }
    unreachable!("byte unit loop always returns")
}

fn open_fd_count() -> Option<usize> {
    ["/proc/self/fd", "/dev/fd"]
        .into_iter()
        .find_map(|path| fs::read_dir(path).ok().map(Iterator::count))
}

fn process_resources() -> ProcessResources {
    let peak_rss_bytes = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
                    .and_then(|kilobytes| kilobytes.checked_mul(1_024))
            })
        });
    ProcessResources {
        peak_rss_bytes,
        io_blocks_in: 0,
        io_blocks_out: 0,
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

impl RegistrationProvider for AdminRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "admin".to_owned(),
            description: "Admin maintenance commands".to_owned(),
            options: admin_options(self.admin_rating_max),
            handler: Arc::clone(&self.handler),
        })
    }
}

fn admin_options(admin_rating_max: f64) -> Vec<CommandOptionSpec> {
    vec![
        group(
            "adjust",
            "Adjust player rating fields",
            vec![
                subcommand(
                    "rating",
                    "Set a player's displayed rating in both systems (Admin only)",
                    vec![
                        required("user", "Player to adjust", CommandOptionKind::User),
                        required(
                            "rating",
                            &format!("New rating (0-{admin_rating_max:.0})"),
                            CommandOptionKind::Number,
                        ),
                    ],
                ),
                subcommand(
                    "rd",
                    "Set a player's Glicko RD/uncertainty (Admin only)",
                    vec![
                        required("user", "Player to adjust", CommandOptionKind::User),
                        required(
                            "rd",
                            "New rating deviation / uncertainty (0-250)",
                            CommandOptionKind::Number,
                        ),
                    ],
                ),
            ],
        ),
        group(
            "lowprio",
            "Restricted matchmaking maintenance",
            low_priority_options(),
        ),
        group(
            "moderation",
            "Temporary matchmaking suspensions and audit history",
            moderation_options(),
        ),
        subcommand(
            "health",
            "Show bot health and since-startup usage (Admin only)",
            vec![],
        ),
        subcommand(
            "addfake",
            "Add fake users to lobby for testing (Admin only)",
            vec![option(
                "count",
                "Number of fake users to add (1-10)",
                CommandOptionKind::Integer,
            )],
        ),
        subcommand(
            "filllobbytest",
            "Fill remaining lobby spots with fake users for testing (Admin only)",
            vec![],
        ),
        subcommand(
            "resetuser",
            "Reset a specific user's account (Admin only)",
            vec![required(
                "user",
                "The user whose account to reset",
                CommandOptionKind::User,
            )],
        ),
        subcommand(
            "registeruser",
            "Register another user as a player (Admin only)",
            vec![
                required("user", "The user to register", CommandOptionKind::User),
                required(
                    "steam_id",
                    "Steam32 ID (found in Dotabuff URL)",
                    CommandOptionKind::Integer,
                ),
                option(
                    "mmr",
                    "Optional MMR override (0-12000)",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
        subcommand("sync", "Force sync commands (Admin only)", vec![]),
        subcommand(
            "backfillroles",
            "Derive played roles from stored OpenDota data and report coverage (Admin only)",
            vec![],
        ),
        subcommand(
            "givecoin",
            "Give jopacoin to a user or reserve (Admin only)",
            vec![
                required(
                    "amount",
                    "Amount to give (can be negative to take)",
                    CommandOptionKind::Integer,
                ),
                option(
                    "user",
                    "The user to give coins to (leave empty if targeting reserve)",
                    CommandOptionKind::User,
                ),
                option(
                    "nonprofit",
                    "Give to the Jopacoin Reserve instead of a user",
                    CommandOptionKind::Boolean,
                ),
            ],
        ),
        user_subcommand(
            "resetloancooldown",
            "Reset a user's loan cooldown (Admin only)",
            "The user whose loan cooldown to reset",
        ),
        user_subcommand(
            "resetbankruptcycooldown",
            "Reset a user's bankruptcy cooldown (Admin only)",
            "The user whose bankruptcy cooldown to reset",
        ),
        subcommand(
            "setrating",
            "Set initial rating for a player",
            vec![
                required(
                    "user",
                    "Player to adjust (must have few games)",
                    CommandOptionKind::User,
                ),
                required(
                    "rating",
                    &format!("Initial rating (0-{admin_rating_max:.0})"),
                    CommandOptionKind::Number,
                ),
            ],
        ),
        user_subcommand(
            "recalibrate",
            "Reset rating uncertainty for a player (Admin only)",
            "The player to recalibrate",
        ),
        subcommand(
            "bumprd",
            "Increase rating uncertainty after a major patch (Admin only)",
            vec![option(
                "amount",
                "RD increase amount (default 100, max 250)",
                CommandOptionKind::Integer,
            )],
        ),
        user_subcommand(
            "resetrecalibrationcooldown",
            "Reset a user's recalibration cooldown (Admin only)",
            "The user whose recalibration cooldown to reset",
        ),
        subcommand(
            "extendbetting",
            "Extend the betting window for a pending match (Admin only)",
            vec![
                required(
                    "minutes",
                    "Number of minutes to extend betting (1-60)",
                    CommandOptionKind::Integer,
                ),
                required(
                    "pending_match_id",
                    "Pending match ID to extend",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
        subcommand(
            "correctmatch",
            "Correct an incorrectly recorded match result (Admin only)",
            vec![
                required(
                    "match_id",
                    "The match ID to correct",
                    CommandOptionKind::Integer,
                ),
                choices(
                    required(
                        "correct_result",
                        "The correct winning team",
                        CommandOptionKind::String,
                    ),
                    &[("Radiant", "radiant"), ("Dire", "dire")],
                ),
            ],
        ),
        subcommand(
            "addsteamid",
            "Add a Steam ID to a player's account (Admin only)",
            vec![
                required(
                    "user",
                    "The user to add the Steam ID to",
                    CommandOptionKind::User,
                ),
                required("steam_id", "Steam32 ID to add", CommandOptionKind::Integer),
                option(
                    "set_primary",
                    "Set as primary account (default: False)",
                    CommandOptionKind::Boolean,
                ),
            ],
        ),
        subcommand(
            "removesteamid",
            "Remove a Steam ID from a player's account (Admin only)",
            vec![
                required(
                    "user",
                    "The user to remove the Steam ID from",
                    CommandOptionKind::User,
                ),
                required(
                    "steam_id",
                    "Steam32 ID to remove",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
        subcommand(
            "setprimarysteam",
            "Set a player's primary Steam ID (Admin only)",
            vec![
                required(
                    "user",
                    "The user to set primary Steam ID for",
                    CommandOptionKind::User,
                ),
                required(
                    "steam_id",
                    "Steam32 ID to set as primary (must already be linked)",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
        subcommand(
            "seedherogrid",
            "Seed fake players with enriched match data for /herogrid testing (Admin only)",
            vec![],
        ),
    ]
}

fn low_priority_options() -> Vec<CommandOptionSpec> {
    let mut wins = option(
        "wins",
        "Required wins to clear low priority (1-20)",
        CommandOptionKind::Integer,
    );
    wins.min_integer = Some(1);
    wins.max_integer = Some(20);
    // The assignment reason reaches the placed player, so it carries the same
    // bounds as the other player-facing moderation reason.
    let mut assignment_reason = option(
        "reason",
        "Reason shown privately to the player and admins — do not name who reported them",
        CommandOptionKind::String,
    );
    // Bounded above only. A lower bound would silently reject the short
    // shorthand reasons this command has always accepted.
    assignment_reason.max_length = Some(REASON_DISPLAY_LIMIT);
    vec![
        subcommand(
            "add",
            "Set restricted matchmaking state",
            vec![
                required("user", "Player to update", CommandOptionKind::User),
                assignment_reason,
                wins,
            ],
        ),
        subcommand(
            "remove",
            "Clear restricted matchmaking state",
            vec![
                required("user", "Player to update", CommandOptionKind::User),
                option(
                    "reason",
                    "Reason visible to admins and recorded in moderation audit history",
                    CommandOptionKind::String,
                ),
            ],
        ),
        subcommand(
            "status",
            "Show restricted matchmaking state",
            vec![required(
                "user",
                "Player to inspect",
                CommandOptionKind::User,
            )],
        ),
        subcommand("list", "List restricted matchmaking state", vec![]),
    ]
}

fn moderation_options() -> Vec<CommandOptionSpec> {
    let mut reason = required(
        "reason",
        "Reason shown privately to the player and admins",
        CommandOptionKind::String,
    );
    reason.min_length = Some(3);
    reason.max_length = Some(300);
    let mut matches = option(
        "matches",
        "Optional number of future completed matches (1-20)",
        CommandOptionKind::Integer,
    );
    matches.min_integer = Some(1);
    matches.max_integer = Some(20);
    vec![
        subcommand(
            "suspend",
            "Temporarily block a player from matchmaking lobbies",
            vec![
                required("player", "Player to suspend", CommandOptionKind::User),
                reason.clone(),
                option(
                    "duration",
                    "Optional duration such as 30m, 2h, 3d, or 1w",
                    CommandOptionKind::String,
                ),
                matches,
                choices(
                    option(
                        "completion",
                        "When both terms are set, release on either term or only after both",
                        CommandOptionKind::String,
                    ),
                    &[
                        ("Either term (first one releases)", "either"),
                        ("Both terms must be served", "both"),
                    ],
                ),
                choices(
                    option(
                        "lobby",
                        "Lobby scope; defaults to all lobbies",
                        CommandOptionKind::String,
                    ),
                    &[
                        ("All lobbies", "all"),
                        ("All You Can Feed", "open"),
                        ("Whine & Cheese", "lowskill"),
                    ],
                ),
                option(
                    "replace",
                    "Explicitly overwrite the player's existing active suspension",
                    CommandOptionKind::Boolean,
                ),
            ],
        ),
        subcommand(
            "lift",
            "Lift an active lobby suspension",
            vec![
                required(
                    "player",
                    "Player whose suspension should be lifted",
                    CommandOptionKind::User,
                ),
                reason,
            ],
        ),
        subcommand(
            "status",
            "Show one player's lobby suspension",
            vec![required(
                "player",
                "Player to inspect",
                CommandOptionKind::User,
            )],
        ),
        subcommand("list", "List active lobby suspensions", vec![]),
        subcommand(
            "history",
            "Show durable matchmaking moderation history",
            vec![option(
                "player",
                "Optional player to filter",
                CommandOptionKind::User,
            )],
        ),
    ]
}

fn option(name: &str, description: &str, kind: CommandOptionKind) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, kind)
}

fn required(name: &str, description: &str, kind: CommandOptionKind) -> CommandOptionSpec {
    option(name, description, kind).required(true)
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    option(name, description, CommandOptionKind::Subcommand).options(options)
}

fn group(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    option(name, description, CommandOptionKind::SubcommandGroup).options(options)
}

fn user_subcommand(name: &str, description: &str, user_description: &str) -> CommandOptionSpec {
    subcommand(
        name,
        description,
        vec![required("user", user_description, CommandOptionKind::User)],
    )
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

#[cfg(all(test, feature = "runtime-test-dig"))]
#[path = "admin_provider/tests.rs"]
mod tests;
