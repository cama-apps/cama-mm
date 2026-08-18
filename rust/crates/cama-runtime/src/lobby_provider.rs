//! Production `/lobby` command, recovery, and raw-reaction composition.
//!
//! This module is the lift-and-shift boundary: it composes existing-schema
//! SQLite repositories with the application lobby/readycheck policies and a
//! transport-neutral Discord port. Construction hydrates durable `lobby_state`
//! before any gateway connection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::curfew_service::CurfewService;
use cama_app::dedicated_lobby_channel::{
    ChannelId as AppChannelId, GuildId as AppGuildId, LobbyMessageIds, LobbyScope,
    MessageId as AppMessageId, UserId as AppUserId,
};
use cama_app::draft::DraftStateManager;
use cama_app::embeds::{LobbyKind, LobbyPlayer};
use cama_app::lobby_runtime::{
    AMBIGUOUS_LOBBY_MESSAGE, AllowedMentions, LobbyCommandRuntime, LobbyRuntimeTransport,
    ResetLobbyOutcome, ResetLobbyRequest, ResetPendingMatchPort, ScopedPendingMatch,
    format_lobby_labels,
};
use cama_app::lobby_service::{
    DOTA_BET_SEED_AMOUNT, FirstGamePoolPreviewPort, FirstGamePoolPreviews, JoinContext,
    JoinFailure, LobbyCreationError, LobbyModerationPort, LobbyPersistencePort, LobbyPlayerPort,
    LobbyService, LobbySnapshot, PendingMatchPort, PendingMatchState, SystemLobbyClock,
};
use cama_app::moderation::{ModerationService, RecordKick};
use cama_app::readycheck::{
    CommitPublicationResult, ExistingMessageState, PublicationMode, ReadinessGroup, ReadyLobby,
    ReadycheckCommandFailure, ReadycheckCommandPlan, ReadycheckCommandRequest,
    ReadycheckPlayerData, ReadycheckService, ReadycheckTransportOperation, ready_announcement,
    readycheck_description,
};
use cama_db::core_repositories::{CoreRepositoryError, NewPlayer, PlayerRepository};
use cama_db::curfew::CurfewRepository;
use cama_db::dota_bet_seed_repository::DotaBetSeedRepository;
use cama_db::moderation::{
    LobbySuspension, ModerationRepository, ModerationSource, SuspensionCompletion, SuspensionScope,
};
use cama_db::pending_lobby::PendingLobbyRepository;
use cama_db::readycheck_repository::{PersistedLobbyState, ReadycheckRepository};
use cama_domain::embed_safety::EmbedModel;
use cama_domain::formatting::{JOPACOIN_EMOJI_ID, JOPACOIN_EMOTE};
use thiserror::Error;
use tracing::{debug, warn};

use crate::admin_provider::{
    AdminFakeLobbyRequest, AdminFakeLobbyResult, AdminLobbyControl, AdminLobbyEjectionRequest,
    AdminLobbyEjectionResult, AdminLobbyScope,
};
use crate::application_config::ApplicationConfig;
use crate::curfew_sweep_worker::CurfewLobbyDisplayPort;
use crate::discord_transport::{
    DiscordAllowedMentions, DiscordEmoji, DiscordMessage, DiscordPresence, DiscordTransport,
};
use crate::first_game_pool_worker::FirstGamePoolDisplayPort;
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::raw_reactions::{RawReactionEvent, RawReactionKind, RawReactionObserver};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

const LEGACY_FROGLING_EMOJI_ID: u64 = 1_463_270_458_848_842_003;
const SWORD_EMOJI: &str = "⚔️";
const READY_EMOJI: &str = "✅";
const BELL_EMOJI: &str = "🔔";
const CLIPBOARD_EMOJI: &str = "📋";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const RECONCILE_ATTEMPTS: usize = 3;
const RAW_REJECTION_TTL: Duration = Duration::from_secs(10);
const RAW_LONG_REJECTION_TTL: Duration = Duration::from_secs(15);

pub type LiveLobbyService =
    LobbyService<SqliteLobbyPlayers, SqlitePendingMatches, SystemLobbyClock>;
type LiveLobbyRuntime =
    LobbyCommandRuntime<SqliteLobbyPlayers, SqlitePendingMatches, SystemLobbyClock>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyRuntimeConfig {
    pub lobby_channel_id: Option<u64>,
    pub low_skill_lobby_channel_id: Option<u64>,
    pub admin_user_ids: BTreeSet<u64>,
    pub ready_threshold: usize,
    pub max_players: usize,
    pub first_game_pool_daily_amount: i64,
}

/// Durable-join projection delivered after the lobby display and ordered
/// thread activity have been published.  Consumers can claim one-shot watches
/// against `joined_at_ns` without reaching into lobby-runtime internals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmedLobbyJoin {
    pub guild_id: u64,
    pub player_id: u64,
    pub player_name: String,
    pub player_display_name: String,
    pub lobby_kind: LobbyKind,
    pub joined_at_ns: i64,
    pub player_ids: BTreeSet<u64>,
    pub ready_threshold: usize,
    pub lobby_channel_id: Option<u64>,
    pub lobby_message_id: Option<u64>,
    pub origin_channel_id: Option<u64>,
    pub thread_id: Option<u64>,
}

/// Projection of the auxiliary jopacoin reaction. The registration provider
/// owns the shared Neon service, so this keeps the lobby provider from
/// constructing a second cooldown/event ledger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LobbyGambaSpectator {
    pub guild_id: u64,
    pub player_id: u64,
    pub player_display_name: String,
    pub channel_id: u64,
}

#[async_trait]
pub trait LobbyJoinObserver: Send + Sync {
    async fn confirmed_lobby_join(&self, event: ConfirmedLobbyJoin) -> Result<(), String>;

    /// Python invokes this only after the successful `/join` follow-up. Lobby
    /// creation auto-join and raw sword joins deliberately never call it.
    async fn explicit_lobby_join_neon(&self, _event: ConfirmedLobbyJoin) -> Result<(), String> {
        Ok(())
    }

    async fn gamba_spectator(&self, _event: LobbyGambaSpectator) -> Result<(), String> {
        Ok(())
    }

    /// Reset any generation-scoped join side effects after `/resetlobby` has
    /// durably cleared the lobby. Python clears both rally and ready cooldowns
    /// at this boundary so a newly-created generation is never suppressed by
    /// the lobby it replaced.
    async fn lobby_reset(&self, _guild_id: u64, _lobby_kind: LobbyKind) -> Result<(), String> {
        Ok(())
    }
}

impl LobbyRuntimeConfig {
    pub fn from_application_config(
        config: &ApplicationConfig,
    ) -> Result<Self, LobbyProviderBuildError> {
        Ok(Self {
            lobby_channel_id: optional_snowflake(config.channels.lobby, "LOBBY_CHANNEL_ID")?,
            low_skill_lobby_channel_id: optional_snowflake(
                config.channels.low_skill_lobby,
                "LOWSKILL_LOBBY_CHANNEL_ID",
            )?,
            admin_user_ids: config
                .identities
                .admin_user_ids
                .iter()
                .copied()
                .map(|value| snowflake(value, "ADMIN_USER_IDS"))
                .collect::<Result<_, _>>()?,
            ready_threshold: positive_usize(
                config.values.lobby_ready_threshold,
                "LOBBY_READY_THRESHOLD",
            )?,
            max_players: positive_usize(config.values.lobby_max_players, "LOBBY_MAX_PLAYERS")?,
            first_game_pool_daily_amount: config.values.first_game_pool_daily_amount,
        })
    }
}

fn positive_usize(value: i64, name: &'static str) -> Result<usize, LobbyProviderBuildError> {
    usize::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(LobbyProviderBuildError::InvalidConfig(name))
}

fn optional_snowflake(
    value: Option<i64>,
    name: &'static str,
) -> Result<Option<u64>, LobbyProviderBuildError> {
    value.map(|value| snowflake(value, name)).transpose()
}

fn snowflake(value: i64, name: &'static str) -> Result<u64, LobbyProviderBuildError> {
    u64::try_from(value).map_err(|_| LobbyProviderBuildError::InvalidConfig(name))
}

#[derive(Debug, Error)]
pub enum LobbyProviderBuildError {
    #[error("invalid positive/snowflake value for {0}")]
    InvalidConfig(&'static str),
    #[error("could not hydrate persisted lobby state: {0}")]
    Persistence(String),
}

#[derive(Clone, Debug)]
struct SqliteLobbyPersistence {
    repository: ReadycheckRepository,
}

impl SqliteLobbyPersistence {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            repository: ReadycheckRepository::new(path),
        }
    }
}

impl LobbyPersistencePort for SqliteLobbyPersistence {
    fn load_all_lobbies(&self) -> Result<Vec<PersistedLobbyState>, String> {
        self.repository
            .load_all()
            .map_err(|error| error.to_string())
    }

    fn save_lobby(&self, lobby: &PersistedLobbyState) -> Result<(), String> {
        self.repository
            .save(lobby)
            .map_err(|error| error.to_string())
    }

    fn clear_lobby(&self, scope: LobbyScope) -> Result<bool, String> {
        self.repository
            .clear(scope.lobby_id(), Some(scope.guild_id.0))
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct SqliteLobbyPlayers {
    repository: PlayerRepository,
}

impl SqliteLobbyPlayers {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            repository: PlayerRepository::new(path),
        }
    }

    fn player(
        &self,
        player_id: AppUserId,
        guild_id: AppGuildId,
    ) -> Result<Option<cama_domain::player::Player>, String> {
        self.repository
            .get_by_id(player_id.0, Some(guild_id.0))
            .map_err(|error| error.to_string())
    }

    fn fallback_name(&self, player_id: AppUserId, guild_id: AppGuildId) -> String {
        self.player(player_id, guild_id)
            .ok()
            .flatten()
            .map_or_else(|| format!("User {}", player_id.0), |player| player.name)
    }
}

impl LobbyPlayerPort for SqliteLobbyPlayers {
    fn glicko_rating(&self, player_id: AppUserId, guild_id: AppGuildId) -> Option<f64> {
        self.player(player_id, guild_id)
            .ok()
            .flatten()
            .and_then(|player| player.glicko_rating)
    }

    fn get_by_ids(&self, player_ids: &[AppUserId], guild_id: AppGuildId) -> Vec<LobbyPlayer> {
        let ids = player_ids
            .iter()
            .map(|player_id| player_id.0)
            .collect::<Vec<_>>();
        self.repository
            .get_by_ids(&ids, Some(guild_id.0))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|player| {
                let discord_id = player.discord_id?;
                Some(LobbyPlayer {
                    discord_id,
                    name: player.name,
                    glicko_rating: player.glicko_rating,
                    mmr: player.mmr.and_then(|mmr| i32::try_from(mmr).ok()),
                    preferred_roles: player
                        .preferred_roles
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|role| role.parse::<u8>().ok())
                        .collect(),
                    guild_id: player.guild_id,
                    preferred_region: player.preferred_region,
                    inferred_region: player.inferred_region,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct SqlitePendingMatches {
    repository: PendingLobbyRepository,
}

#[derive(Clone, Debug)]
struct SqliteLobbyModeration {
    repository: ModerationRepository,
}

impl SqliteLobbyModeration {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            repository: ModerationRepository::new(path),
        }
    }
}

impl LobbyModerationPort for SqliteLobbyModeration {
    fn active_suspension(
        &self,
        player_id: AppUserId,
        scope: LobbyScope,
        now: i64,
    ) -> Result<Option<LobbySuspension>, String> {
        let kind = match scope.kind {
            LobbyKind::Open => SuspensionScope::Open,
            LobbyKind::LowSkill => SuspensionScope::Lowskill,
        };
        ModerationService::new(self.repository.clone())
            .get_active_suspension(player_id.0, Some(scope.guild_id.0), Some(kind), now)
            .map_err(|error| format!("{error:?}"))
    }
}

impl SqlitePendingMatches {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            repository: PendingLobbyRepository::new(path),
        }
    }
}

impl PendingMatchPort for SqlitePendingMatches {
    fn pending_match_for_player(
        &self,
        guild_id: AppGuildId,
        player_id: AppUserId,
    ) -> Option<PendingMatchState> {
        self.try_pending_match_for_player(guild_id, player_id)
            .ok()
            .flatten()
    }

    fn try_pending_match_for_player(
        &self,
        guild_id: AppGuildId,
        player_id: AppUserId,
    ) -> Result<Option<PendingMatchState>, String> {
        self.repository
            .for_player(guild_id.0, player_id.0)
            .map(|pending| {
                pending.map(|pending| PendingMatchState {
                    pending_match_id: pending.pending_match_id,
                    shuffle_message_jump_url: pending.shuffle_message_jump_url,
                })
            })
            .map_err(|error| error.to_string())
    }
}

impl ResetPendingMatchPort for SqlitePendingMatches {
    fn pending_matches(&self, guild_id: AppGuildId) -> Result<Vec<ScopedPendingMatch>, String> {
        self.repository
            .all_for_guild(guild_id.0)
            .map(|pending| {
                pending
                    .into_iter()
                    .map(|pending| ScopedPendingMatch {
                        lobby_kind: pending.lobby_kind.as_deref().and_then(parse_lobby_kind),
                        shuffle_message_jump_url: pending.shuffle_message_jump_url,
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }
}

fn parse_lobby_kind(value: &str) -> Option<LobbyKind> {
    match value {
        "open" => Some(LobbyKind::Open),
        "lowskill" => Some(LobbyKind::LowSkill),
        _ => None,
    }
}

fn lobby_kind_value(kind: LobbyKind) -> &'static str {
    match kind {
        LobbyKind::Open => "open",
        LobbyKind::LowSkill => "lowskill",
    }
}

fn private_suspension_message(suspension: &LobbySuspension) -> String {
    let scope = match suspension.scope {
        SuspensionScope::All => "all matchmaking lobbies",
        SuspensionScope::Open => LobbyKind::Open.label(),
        SuspensionScope::Lowskill => LobbyKind::LowSkill.label(),
    };
    let mut terms = Vec::new();
    if let Some(expires_at) = suspension.expires_at {
        terms.push(format!("until <t:{expires_at}:F> (<t:{expires_at}:R>)"));
    }
    if let Some(matches_remaining) = suspension.matches_remaining {
        let noun = if matches_remaining == 1 {
            "match"
        } else {
            "matches"
        };
        terms.push(format!("{matches_remaining} completed {noun} remaining"));
    }
    let terms = if terms.is_empty() {
        "temporarily".to_owned()
    } else {
        terms.join(if suspension.completion == SuspensionCompletion::Either {
            " or "
        } else {
            " and "
        })
    };
    format!(
        "You are suspended from {scope} {terms}. Reason: {}",
        suspension.reason
    )
}

fn to_ready_lobby(lobby: &LobbySnapshot) -> ReadyLobby {
    ReadyLobby {
        scope: lobby.scope,
        created_by: lobby.created_by.unwrap_or(AppUserId(0)),
        created_at: chrono::DateTime::from_timestamp_nanos(lobby.created_at_ns).to_rfc3339(),
        players: lobby.players.clone(),
        legacy_conditional_players: BTreeSet::new(),
        player_join_times: lobby.player_join_times.clone(),
        status: cama_app::readycheck::LobbyStatus::Open,
        thread_id: lobby.message_ids.thread_id,
    }
}

fn interaction_embed(embed: EmbedModel) -> InteractionEmbed {
    InteractionEmbed {
        title: embed.title,
        description: embed.description,
        footer: embed.footer,
        fields: embed
            .fields
            .into_iter()
            .map(|field| crate::registration::InteractionEmbedField {
                name: field.name,
                value: field.value,
                inline: field.inline,
            })
            .collect(),
        ..InteractionEmbed::default()
    }
}

struct LobbyRuntimeState {
    players: SqliteLobbyPlayers,
    pending_matches: SqlitePendingMatches,
    service: Arc<LiveLobbyService>,
    commands: Arc<LiveLobbyRuntime>,
    readychecks: ReadycheckService,
    drafts: Arc<DraftStateManager>,
    transport: Arc<dyn DiscordTransport>,
    config: LobbyRuntimeConfig,
    first_game_previews: RuntimeFirstGamePoolPreviews,
    moderation: ModerationRepository,
    curfew: CurfewService,
    join_observer: RwLock<Option<Arc<dyn LobbyJoinObserver>>>,
}

#[derive(Clone, Debug)]
struct RuntimeFirstGamePoolPreviews {
    repository: DotaBetSeedRepository,
    daily_amount: i64,
}

impl RuntimeFirstGamePoolPreviews {
    fn load(
        &self,
        guild_id: AppGuildId,
        regular_seed_amount: i64,
        game_date: &str,
    ) -> Result<FirstGamePoolPreviews, String> {
        self.repository
            .first_game_pool_previews(
                Some(guild_id.0),
                regular_seed_amount,
                Some(game_date),
                self.daily_amount,
            )
            .map(|balances| FirstGamePoolPreviews {
                open: balances.open,
                low_skill: balances.low_skill,
            })
            .map_err(|error| error.to_string())
    }
}

impl FirstGamePoolPreviewPort for RuntimeFirstGamePoolPreviews {
    fn first_game_pool_previews(
        &self,
        guild_id: AppGuildId,
        regular_seed_amount: i64,
    ) -> FirstGamePoolPreviews {
        self.load(
            guild_id,
            regular_seed_amount,
            &cama_domain::game_date::get_game_date(),
        )
        .unwrap_or_else(|error| {
            warn!(%error, guild_id = guild_id.0, "first-game pool preview read failed");
            FirstGamePoolPreviews {
                open: 0,
                low_skill: 0,
            }
        })
    }
}

struct LoadedFirstGamePoolPreviews(FirstGamePoolPreviews);

impl FirstGamePoolPreviewPort for LoadedFirstGamePoolPreviews {
    fn first_game_pool_previews(
        &self,
        _guild_id: AppGuildId,
        _regular_seed_amount: i64,
    ) -> FirstGamePoolPreviews {
        self.0
    }
}

impl LobbyRuntimeState {
    fn sync_ready_lobby(&self, scope: LobbyScope) {
        if let Some(lobby) = self.service.get_lobby(scope) {
            self.readychecks.put_lobby(to_ready_lobby(&lobby));
        } else {
            self.readychecks.reset_lobby(scope);
        }
    }

    fn scope_for_lobby_message(
        &self,
        guild_id: Option<u64>,
        message_id: u64,
    ) -> Option<LobbyScope> {
        let guild_id = i64::try_from(guild_id?).ok()?;
        let message_id = i64::try_from(message_id).ok()?;
        self.service.open_lobbies().into_iter().find_map(|lobby| {
            (lobby.scope.guild_id == AppGuildId(guild_id)
                && lobby.message_ids.message_id == Some(AppMessageId(message_id)))
            .then_some(lobby.scope)
        })
    }

    fn scope_for_readycheck_message(
        &self,
        guild_id: Option<u64>,
        message_id: u64,
    ) -> Option<LobbyScope> {
        let guild_id = i64::try_from(guild_id?).ok()?;
        let message_id = i64::try_from(message_id).ok()?;
        [LobbyKind::Open, LobbyKind::LowSkill]
            .into_iter()
            .map(|kind| LobbyScope::new(AppGuildId(guild_id), kind))
            .find(|scope| {
                self.readychecks
                    .readycheck_generation(*scope)
                    .is_some_and(|generation| generation.message_id == AppMessageId(message_id))
            })
    }

    async fn sync_lobby_display(&self, scope: LobbyScope) -> Result<(), String> {
        let lobby = self
            .service
            .get_lobby(scope)
            .ok_or_else(|| "lobby no longer exists".to_owned())?;
        let channel_id = lobby
            .message_ids
            .channel_id
            .ok_or_else(|| "lobby has no channel id".to_owned())?;
        let message_id = lobby
            .message_ids
            .message_id
            .ok_or_else(|| "lobby has no message id".to_owned())?;
        let service = Arc::clone(&self.service);
        let previews = self.first_game_previews.clone();
        let embed = tokio::task::spawn_blocking(move || {
            interaction_embed(service.build_lobby_embed(&lobby, Some(&previews)))
        })
        .await
        .map_err(|error| format!("lobby display render task failed: {error}"))?;
        self.transport
            .edit_message(
                to_u64(channel_id.0)?,
                to_u64(message_id.0)?,
                DiscordMessage::silent(InteractionResponse::message("").embed(embed))
                    .preserving_content(),
            )
            .await
    }

    async fn remove_lobby_reaction(
        &self,
        scope: LobbyScope,
        player_id: AppUserId,
    ) -> Result<(), String> {
        let Some(lobby) = self.service.get_lobby(scope) else {
            return Ok(());
        };
        let (Some(channel_id), Some(message_id)) =
            (lobby.message_ids.channel_id, lobby.message_ids.message_id)
        else {
            return Ok(());
        };
        self.transport
            .remove_reaction(
                to_u64(channel_id.0)?,
                to_u64(message_id.0)?,
                &DiscordEmoji::unicode(SWORD_EMOJI),
                to_u64(player_id.0)?,
            )
            .await
    }
}

fn to_u64(value: i64) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("invalid persisted Discord snowflake {value}"))
}

#[derive(Clone)]
pub struct LobbyRegistrationProvider {
    handler: Arc<LobbyInteractionHandler>,
    gateway_observer: Arc<LobbyGatewayObserver>,
    reaction_observer: Arc<LobbyRawReactionObserver>,
}

/// Read-only draft facts needed by `/shuffle` preflight policy.
///
/// The match extension deliberately receives this projection from the live
/// lobby provider instead of a second [`DraftStateManager`]. That keeps the
/// captain/admin restart hint tied to the same process-local draft generation
/// used by `/lobby` and `/draft`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchActiveDraft {
    pub lobby_kind: LobbyKind,
    pub captain_ids: BTreeSet<i64>,
}

/// Stable snapshot consumed while one live lobby operation lock is held.
///
/// `confirmed_player_ids` comes from [`ReadycheckService`], not the legacy
/// readycheck fields retained in `LobbyService`. This distinction matters for
/// the ten-confirmation conditional-roster behavior of Python `/shuffle`.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchLobbySnapshot {
    pub guild_id: i64,
    pub lobby_kind: LobbyKind,
    pub created_by: Option<i64>,
    pub player_ids: Vec<i64>,
    pub player_join_times: BTreeMap<i64, f64>,
    pub confirmed_player_ids: Option<BTreeSet<i64>>,
    pub ready_threshold: usize,
    pub lobby_channel_id: Option<u64>,
    pub lobby_message_id: Option<u64>,
    pub origin_channel_id: Option<u64>,
    pub thread_id: Option<u64>,
}

/// Cloneable bridge from `/match` to the single production lobby runtime.
///
/// Constructing another `LobbyService` would create independent in-flight
/// reservations and operation locks over the same durable lobby rows. This
/// handle intentionally retains the existing `Arc<LobbyRuntimeState>` so
/// shuffle, join, leave, readycheck, and reset serialize on exactly one state
/// graph.
#[derive(Clone)]
pub struct MatchLobbyPort {
    state: Arc<LobbyRuntimeState>,
}

#[derive(Clone)]
struct LobbyAdminControl {
    state: Arc<LobbyRuntimeState>,
}

struct FakeLobbyMutation {
    result: AdminFakeLobbyResult,
    refresh_display: bool,
}

#[async_trait]
impl AdminLobbyControl for LobbyAdminControl {
    async fn add_fake_users(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String> {
        self.mutate_fake_lobby(request.guild_id, Some(request.count))
            .await
    }

    async fn fill_lobby(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String> {
        self.mutate_fake_lobby(request.guild_id, None).await
    }

    async fn eject_suspended_player(
        &self,
        request: AdminLobbyEjectionRequest,
    ) -> Result<AdminLobbyEjectionResult, String> {
        let guild_id = AppGuildId(request.guild_id);
        let player_id = AppUserId(request.player_id);
        let kinds = self
            .state
            .service
            .get_lobby_kinds_for_player(player_id, guild_id)
            .into_iter()
            .filter(|kind| match request.scope {
                AdminLobbyScope::All => true,
                AdminLobbyScope::Open => *kind == LobbyKind::Open,
                AdminLobbyScope::Lowskill => *kind == LobbyKind::LowSkill,
            })
            .collect::<Vec<_>>();
        let mut result = AdminLobbyEjectionResult {
            applicable_lobbies: kinds.iter().map(|kind| kind.label().to_owned()).collect(),
            evicted_lobbies: Vec::new(),
        };
        for kind in kinds {
            let scope = LobbyScope::new(guild_id, kind);
            let operation_lock = self.state.commands.scope_operation_lock(scope);
            let _guard = operation_lock.lock().await;
            let state = Arc::clone(&self.state);
            let removed = tokio::task::spawn_blocking(move || {
                state.service.try_leave_lobby(player_id, scope)
            })
            .await
            .map_err(|error| format!("suspension lobby task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            if !removed {
                continue;
            }
            result.evicted_lobbies.push(kind.label().to_owned());
            self.state.sync_ready_lobby(scope);
            self.best_effort_display_refresh(scope, "suspension").await;
            if let Err(error) = self.state.remove_lobby_reaction(scope, player_id).await {
                debug!(%error, ?scope, "suspension lobby reaction cleanup failed");
            }
            if let Some(thread_id) = self
                .state
                .service
                .get_lobby(scope)
                .and_then(|lobby| lobby.message_ids.thread_id)
                .and_then(|thread_id| to_u64(thread_id.0).ok())
                && let Err(error) = self
                    .state
                    .transport
                    .send_message(
                        thread_id,
                        DiscordMessage::silent(InteractionResponse::message(format!(
                            "⛔ **{}** was temporarily suspended from matchmaking by an admin.",
                            request.player_display_name
                        ))),
                    )
                    .await
            {
                warn!(%error, ?scope, "suspension lobby activity publication failed");
            }
        }
        Ok(result)
    }
}

impl LobbyAdminControl {
    async fn mutate_fake_lobby(
        &self,
        guild_id: i64,
        requested_count: Option<usize>,
    ) -> Result<AdminFakeLobbyResult, String> {
        let scope = LobbyScope::new(AppGuildId(guild_id), LobbyKind::Open);
        let operation_lock = self.state.commands.scope_operation_lock(scope);
        let _guard = operation_lock.lock().await;
        let state = Arc::clone(&self.state);
        let mutation = tokio::task::spawn_blocking(move || {
            mutate_fake_lobby_blocking(&state, scope, requested_count)
        })
        .await
        .map_err(|error| format!("fake-lobby task failed: {error}"))??;
        if mutation.refresh_display {
            self.state.sync_ready_lobby(scope);
            self.best_effort_display_refresh(scope, "fake-user").await;
        }
        Ok(mutation.result)
    }

    async fn best_effort_display_refresh(&self, scope: LobbyScope, operation: &'static str) {
        let has_display = self.state.service.get_lobby(scope).is_some_and(|lobby| {
            lobby.message_ids.channel_id.is_some() && lobby.message_ids.message_id.is_some()
        });
        if has_display && let Err(error) = self.state.sync_lobby_display(scope).await {
            warn!(%error, ?scope, operation, "admin lobby display refresh failed");
        }
    }
}

fn mutate_fake_lobby_blocking(
    state: &LobbyRuntimeState,
    scope: LobbyScope,
    requested_count: Option<usize>,
) -> Result<FakeLobbyMutation, String> {
    let lobby = state
        .service
        .get_or_create_lobby(None, scope)
        .map_err(|error| error.to_string())?;
    let current = lobby.players.len();
    let count = match requested_count {
        Some(count) => {
            if current.saturating_add(count) > state.config.max_players {
                return Err(format!(
                    "Adding {count} users would exceed {} players. Currently {current}/{}.",
                    state.config.max_players, state.config.max_players
                ));
            }
            count
        }
        None if current >= state.config.ready_threshold => {
            return Ok(FakeLobbyMutation {
                result: AdminFakeLobbyResult {
                    users_added: 0,
                    user_names: Vec::new(),
                    already_at_threshold: Some((current, state.config.ready_threshold)),
                },
                refresh_display: false,
            });
        }
        None => state.config.ready_threshold.saturating_sub(current).min(10),
    };
    let next_index = lobby
        .players
        .iter()
        .filter(|player_id| player_id.0 < 0)
        .filter_map(|player_id| player_id.0.checked_neg())
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut user_names = Vec::with_capacity(count);
    let mut backdated = BTreeMap::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
        .as_secs_f64();
    const BACKDATE_OFFSETS: [f64; 10] = [
        60.0, 300.0, 900.0, 1_800.0, 3_600.0, 7_200.0, 14_400.0, 28_800.0, 43_200.0, 86_400.0,
    ];
    for offset in 0..count {
        let offset = i64::try_from(offset).map_err(|_| "fake-user index overflow")?;
        let index = next_index
            .checked_add(offset)
            .ok_or_else(|| "fake-user index overflow".to_owned())?;
        let fake_id = index
            .checked_neg()
            .ok_or_else(|| "fake-user id overflow".to_owned())?;
        let fake_name = format!("FakeUser{index}");
        if state
            .players
            .repository
            .get_by_id(fake_id, Some(scope.guild_id.0))
            .map_err(|error| error.to_string())?
            .is_none()
        {
            let mut roles = ["1", "2", "3", "4", "5"];
            fastrand::shuffle(&mut roles);
            let role_count = 1 + fastrand::usize(..roles.len());
            let mut player = NewPlayer::new(fake_id, &fake_name, Some(scope.guild_id.0));
            player.glicko_rating = Some(f64::from(fastrand::u16(1_000..2_001)));
            player.glicko_rd = Some(50.0 + fastrand::f64() * 300.0);
            player.glicko_volatility = Some(0.06);
            player.preferred_roles = Some(
                roles[..role_count]
                    .iter()
                    .map(|role| (*role).to_owned())
                    .collect(),
            );
            match state.players.repository.add(&player) {
                Ok(()) | Err(CoreRepositoryError::DuplicatePlayer { .. }) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        if state.service.join_lobby(AppUserId(fake_id), scope).success {
            if requested_count.is_none() {
                let seconds = BACKDATE_OFFSETS[user_names.len() % BACKDATE_OFFSETS.len()];
                backdated.insert(AppUserId(fake_id), now - seconds);
            }
            user_names.push(fake_name);
        }
    }
    if !backdated.is_empty() {
        let updated = state
            .service
            .try_set_player_join_times(scope, &backdated)
            .map_err(|error| error.to_string())?;
        if updated != backdated.len() {
            return Err("lobby membership changed while backdating fake users".to_owned());
        }
    }
    Ok(FakeLobbyMutation {
        result: AdminFakeLobbyResult {
            users_added: user_names.len(),
            user_names,
            already_at_threshold: None,
        },
        refresh_display: true,
    })
}

impl MatchLobbyPort {
    #[must_use]
    pub fn lobby_kinds_for_player(&self, guild_id: i64, player_id: i64) -> Vec<LobbyKind> {
        self.state
            .service
            .get_lobby_kinds_for_player(AppUserId(player_id), AppGuildId(guild_id))
    }

    /// Return the same per-scope mutex used by every live lobby mutation.
    #[must_use]
    pub fn operation_lock(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Arc<tokio::sync::Mutex<()>> {
        self.state
            .commands
            .scope_operation_lock(LobbyScope::new(AppGuildId(guild_id), lobby_kind))
    }

    #[must_use]
    pub fn snapshot(&self, guild_id: i64, lobby_kind: LobbyKind) -> Option<MatchLobbySnapshot> {
        let scope = LobbyScope::new(AppGuildId(guild_id), lobby_kind);
        let lobby = self.state.service.get_lobby(scope)?;
        let confirmed_player_ids =
            self.state
                .readychecks
                .readycheck_generation(scope)
                .map(|generation| {
                    generation
                        .reacted
                        .into_keys()
                        .map(|player_id| player_id.0)
                        .collect()
                });
        Some(MatchLobbySnapshot {
            guild_id,
            lobby_kind,
            created_by: lobby.created_by.map(|player_id| player_id.0),
            player_ids: lobby
                .players
                .into_iter()
                .map(|player_id| player_id.0)
                .collect(),
            player_join_times: lobby
                .player_join_times
                .into_iter()
                .map(|(player_id, joined_at)| (player_id.0, joined_at))
                .collect(),
            confirmed_player_ids,
            ready_threshold: self.state.config.ready_threshold,
            lobby_channel_id: lobby
                .message_ids
                .channel_id
                .and_then(|channel_id| u64::try_from(channel_id.0).ok()),
            lobby_message_id: lobby
                .message_ids
                .message_id
                .and_then(|message_id| u64::try_from(message_id.0).ok()),
            origin_channel_id: lobby
                .message_ids
                .origin_channel_id
                .and_then(|channel_id| u64::try_from(channel_id.0).ok()),
            thread_id: lobby
                .message_ids
                .thread_id
                .and_then(|thread_id| u64::try_from(thread_id.0).ok()),
        })
    }

    #[must_use]
    pub fn active_draft(&self, guild_id: i64) -> Option<MatchActiveDraft> {
        let handle = self.state.drafts.get_state(Some(guild_id))?;
        let draft = handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lobby_kind = match draft.lobby_kind {
            cama_app::draft::LobbyKind::Open => LobbyKind::Open,
            cama_app::draft::LobbyKind::LowSkill => LobbyKind::LowSkill,
        };
        Some(MatchActiveDraft {
            lobby_kind,
            captain_ids: [draft.captain1_id, draft.captain2_id]
                .into_iter()
                .flatten()
                .collect(),
        })
    }

    #[must_use]
    pub fn reserve_players(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
        player_ids: &BTreeSet<i64>,
    ) -> bool {
        let player_ids = player_ids.iter().copied().map(AppUserId).collect();
        self.state.service.reserve_lobby_players(
            &player_ids,
            LobbyScope::new(AppGuildId(guild_id), lobby_kind),
        )
    }

    pub fn release_players(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
        player_ids: &BTreeSet<i64>,
    ) {
        let player_ids = player_ids.iter().copied().map(AppUserId).collect();
        self.state.service.release_lobby_players(
            &player_ids,
            LobbyScope::new(AppGuildId(guild_id), lobby_kind),
        );
    }

    /// Remove only the ten matched players from the sibling queue.
    ///
    /// Callers invoke this while the source reservation is still held. The
    /// sibling display is best effort, matching Python's rule that a durable
    /// match is never unwound by later lobby presentation failure.
    pub async fn evict_from_other_lobby(
        &self,
        guild_id: i64,
        source_kind: LobbyKind,
        matched_player_ids: &BTreeSet<i64>,
    ) -> Result<BTreeSet<i64>, String> {
        let other_kind = match source_kind {
            LobbyKind::Open => LobbyKind::LowSkill,
            LobbyKind::LowSkill => LobbyKind::Open,
        };
        let scope = LobbyScope::new(AppGuildId(guild_id), other_kind);
        let player_ids = matched_player_ids.iter().copied().map(AppUserId).collect();
        let state = Arc::clone(&self.state);
        let removed = tokio::task::spawn_blocking(move || {
            state
                .service
                .try_remove_players_from_lobby(&player_ids, scope)
        })
        .await
        .map_err(|error| format!("sibling lobby eviction task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if removed.is_empty() {
            return Ok(BTreeSet::new());
        }
        self.state.sync_ready_lobby(scope);
        if let Err(error) = self.state.sync_lobby_display(scope).await {
            warn!(%error, ?scope, "sibling lobby display sync after shuffle failed");
        }
        Ok(removed.into_iter().map(|player_id| player_id.0).collect())
    }

    pub async fn unpin_source_message(&self, snapshot: &MatchLobbySnapshot) -> Result<(), String> {
        let (Some(channel_id), Some(message_id)) =
            (snapshot.lobby_channel_id, snapshot.lobby_message_id)
        else {
            return Ok(());
        };
        self.state
            .transport
            .unpin_message(channel_id, message_id)
            .await
    }

    /// Durably clear only the shuffled source generation and notify the shared
    /// registration observer so generation-scoped rally/ready cooldowns reset.
    pub async fn reset_after_shuffle(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<bool, String> {
        let scope = LobbyScope::new(AppGuildId(guild_id), lobby_kind);
        let state = Arc::clone(&self.state);
        let reset = tokio::task::spawn_blocking(move || state.service.try_reset_lobby(scope))
            .await
            .map_err(|error| format!("lobby reset task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if !reset {
            return Ok(false);
        }
        self.state.readychecks.reset_lobby(scope);
        let observer = self
            .state
            .join_observer
            .read()
            .map_err(|_| "lobby join observer lock was poisoned".to_owned())?
            .clone();
        if let Some(observer) = observer {
            observer
                .lobby_reset(
                    u64::try_from(guild_id)
                        .map_err(|_| format!("invalid persisted guild snowflake {guild_id}"))?,
                    lobby_kind,
                )
                .await?;
        }
        Ok(true)
    }

    /// Refresh the shared lobby embeds after a first-game pool claim.
    pub async fn refresh_first_game_pool_lobbies(&self, guild_id: i64) -> Result<(), String> {
        FirstGamePoolLobbyDisplay {
            state: Arc::clone(&self.state),
        }
        .refresh_first_game_pool_lobbies(guild_id)
        .await
    }
}

/// Cloneable production surface used by the first-game funding worker. It
/// discovers the current in-memory lobby generations, builds embeds (including
/// the newly funded preview) on the blocking pool, and performs Discord edits
/// without exposing command-runtime internals.
#[derive(Clone)]
struct FirstGamePoolLobbyDisplay {
    state: Arc<LobbyRuntimeState>,
}

#[async_trait]
impl FirstGamePoolDisplayPort for FirstGamePoolLobbyDisplay {
    async fn refresh_first_game_pool_lobbies(&self, guild_id: i64) -> Result<(), String> {
        let guild_id = AppGuildId(guild_id);
        let mut failures = Vec::new();
        for kind in [LobbyKind::Open, LobbyKind::LowSkill] {
            let scope = LobbyScope::new(guild_id, kind);
            let operation_lock = self.state.commands.scope_operation_lock(scope);
            let _guard = operation_lock.lock().await;
            let state = Arc::clone(&self.state);
            let prepared = tokio::task::spawn_blocking(move || {
                let Some(lobby) = state.service.get_lobby(scope) else {
                    return Ok(None);
                };
                let (Some(channel_id), Some(message_id)) =
                    (lobby.message_ids.channel_id, lobby.message_ids.message_id)
                else {
                    return Ok(None);
                };
                let previews = LoadedFirstGamePoolPreviews(state.first_game_previews.load(
                    guild_id,
                    DOTA_BET_SEED_AMOUNT,
                    &cama_domain::game_date::get_game_date(),
                )?);
                let embed =
                    interaction_embed(state.service.build_lobby_embed(&lobby, Some(&previews)));
                Ok::<_, String>(Some((
                    to_u64(channel_id.0)?,
                    to_u64(message_id.0)?,
                    DiscordMessage::silent(InteractionResponse::message("").embed(embed))
                        .preserving_content(),
                )))
            })
            .await
            .map_err(|error| format!("first-game lobby render task failed: {error}"))??;
            let Some((channel_id, message_id, message)) = prepared else {
                continue;
            };
            if let Err(error) = self
                .state
                .transport
                .edit_message(channel_id, message_id, message)
                .await
            {
                failures.push(format!("{}: {error}", lobby_kind_value(kind)));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// Production surface used by the curfew sweep worker to make a removal
/// actually visible: re-syncs readycheck eligibility and re-renders the
/// lobby's live Discord embed, matching Python's
/// `_deliver_curfew_kick` -> `_sync_lobby_displays` pair.
#[derive(Clone)]
struct CurfewLobbyDisplay {
    state: Arc<LobbyRuntimeState>,
}

#[async_trait]
impl CurfewLobbyDisplayPort for CurfewLobbyDisplay {
    async fn refresh_curfew_lobby(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
    ) -> Result<(), String> {
        let scope = LobbyScope::new(AppGuildId(guild_id), lobby_kind);
        self.state.sync_ready_lobby(scope);
        self.state.sync_lobby_display(scope).await
    }

    async fn remove_curfew_lobby_reaction(
        &self,
        guild_id: i64,
        lobby_kind: LobbyKind,
        discord_id: i64,
    ) -> Result<(), String> {
        let scope = LobbyScope::new(AppGuildId(guild_id), lobby_kind);
        self.state
            .remove_lobby_reaction(scope, AppUserId(discord_id))
            .await
    }
}

impl LobbyRegistrationProvider {
    pub fn new(
        database_path: impl AsRef<Path>,
        config: LobbyRuntimeConfig,
        drafts: Arc<DraftStateManager>,
        transport: Arc<dyn DiscordTransport>,
    ) -> Result<Self, LobbyProviderBuildError> {
        let database_path = database_path.as_ref();
        if config.ready_threshold == 0 || config.max_players == 0 {
            return Err(LobbyProviderBuildError::InvalidConfig("lobby sizes"));
        }
        let players = SqliteLobbyPlayers::new(database_path);
        let pending_matches = SqlitePendingMatches::new(database_path);
        let persistence: Arc<dyn LobbyPersistencePort> =
            Arc::new(SqliteLobbyPersistence::new(database_path));
        let service = LobbyService::new(
            players.clone(),
            Some(pending_matches.clone()),
            SystemLobbyClock,
            config.ready_threshold,
            config.max_players,
        )
        .with_moderation(Arc::new(SqliteLobbyModeration::new(database_path)))
        .with_persistence(persistence)
        .map_err(|error| LobbyProviderBuildError::Persistence(error.to_string()))?;
        let service = Arc::new(service);
        let commands = Arc::new(LobbyCommandRuntime::new(Arc::clone(&service)));
        let readychecks = ReadycheckService::new();
        for lobby in service.open_lobbies() {
            readychecks.put_lobby(to_ready_lobby(&lobby));
        }
        let first_game_previews = RuntimeFirstGamePoolPreviews {
            repository: DotaBetSeedRepository::new(database_path),
            daily_amount: config.first_game_pool_daily_amount,
        };
        let state = Arc::new(LobbyRuntimeState {
            players,
            pending_matches,
            service,
            commands,
            readychecks,
            drafts,
            transport,
            config,
            first_game_previews,
            moderation: ModerationRepository::new(database_path),
            curfew: CurfewService::new(CurfewRepository::new(database_path)),
            join_observer: RwLock::new(None),
        });
        Ok(Self {
            handler: Arc::new(LobbyInteractionHandler {
                state: Arc::clone(&state),
            }),
            gateway_observer: Arc::new(LobbyGatewayObserver {
                state: Arc::clone(&state),
            }),
            reaction_observer: Arc::new(LobbyRawReactionObserver { state }),
        })
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        self.gateway_observer.clone()
    }

    #[must_use]
    pub fn raw_reaction_observer(&self) -> Arc<dyn RawReactionObserver> {
        self.reaction_observer.clone()
    }

    #[must_use]
    pub fn first_game_pool_display(&self) -> Arc<dyn FirstGamePoolDisplayPort> {
        Arc::new(FirstGamePoolLobbyDisplay {
            state: Arc::clone(&self.handler.state),
        })
    }

    /// Share the live lobby state and operation locks with `/shuffle`.
    #[must_use]
    pub fn match_lobby_port(&self) -> MatchLobbyPort {
        MatchLobbyPort {
            state: Arc::clone(&self.handler.state),
        }
    }

    /// Share the live in-memory lobby service with the curfew sweep worker,
    /// so it can read current membership and remove players in real time.
    #[must_use]
    pub fn live_lobby_service(&self) -> Arc<LiveLobbyService> {
        Arc::clone(&self.handler.state.service)
    }

    /// Share the SQLite-backed curfew service with the curfew sweep worker.
    #[must_use]
    pub fn curfew_service(&self) -> CurfewService {
        self.handler.state.curfew.clone()
    }

    /// Let the curfew sweep worker refresh a lobby's Discord embed after
    /// removing members from it.
    #[must_use]
    pub fn curfew_lobby_display(&self) -> Arc<dyn CurfewLobbyDisplayPort> {
        Arc::new(CurfewLobbyDisplay {
            state: Arc::clone(&self.handler.state),
        })
    }

    /// Share the production lobby service, operation locks, persistence, and
    /// Discord display transport with `/admin` maintenance commands.
    #[must_use]
    pub fn admin_control(&self) -> Arc<dyn AdminLobbyControl> {
        Arc::new(LobbyAdminControl {
            state: Arc::clone(&self.handler.state),
        })
    }

    /// Attach the registration extension's lobby-notification side effects.
    /// Calling this after provider construction also affects recovered lobbies
    /// and raw-reaction joins because every path shares the same runtime state.
    pub fn set_join_observer(&self, observer: Arc<dyn LobbyJoinObserver>) -> Result<(), String> {
        *self
            .handler
            .state
            .join_observer
            .write()
            .map_err(|_| "lobby join observer lock was poisoned".to_owned())? = Some(observer);
        Ok(())
    }
}

impl RegistrationProvider for LobbyRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        let lobby_option = || lobby_choice_option("Lobby to create, join, reset, or check");
        registry.command(CommandSpec {
            name: "lobby".to_owned(),
            description: "Create or view a matchmaking lobby".to_owned(),
            options: vec![lobby_option()],
            handler: self.handler.clone(),
        })?;
        let mut player = CommandOptionSpec::new(
            "player",
            "The player to kick from every lobby they are in",
            CommandOptionKind::User,
        );
        player.required = true;
        let mut reason = CommandOptionSpec::new(
            "reason",
            "Optional reason shown privately to the player and admins",
            CommandOptionKind::String,
        );
        reason.min_length = Some(3);
        reason.max_length = Some(300);
        registry.command(CommandSpec {
            name: "kick".to_owned(),
            description: "Kick a player from every lobby they're in (Admin or lobby creator only)"
                .to_owned(),
            options: vec![player, reason],
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "join".to_owned(),
            description: "Join a matchmaking lobby".to_owned(),
            options: vec![lobby_option()],
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "leave".to_owned(),
            description: "Leave every matchmaking lobby you are queued in".to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "resetlobby".to_owned(),
            description: "Reset the current lobby (Admin or lobby creator only)".to_owned(),
            options: vec![lobby_option()],
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "readycheck".to_owned(),
            description: "Check lobby players' online status and ping those who are away"
                .to_owned(),
            options: vec![lobby_option()],
            handler: self.handler.clone(),
        })
    }
}

fn lobby_choice_option(description: &str) -> CommandOptionSpec {
    let mut option = CommandOptionSpec::new("lobby", description, CommandOptionKind::String);
    option.choices = vec![
        CommandOptionChoice::String {
            name: "🍽️ All You Can Feed".to_owned(),
            value: "open".to_owned(),
        },
        CommandOptionChoice::String {
            name: "🧀 Whine & Cheese".to_owned(),
            value: "lowskill".to_owned(),
        },
    ];
    option
}

struct LobbyInteractionHandler {
    state: Arc<LobbyRuntimeState>,
}

#[derive(Clone)]
struct CommandContext {
    name: String,
    user_id: AppUserId,
    user_display_name: String,
    guild_id: Option<AppGuildId>,
    channel_id: Option<AppChannelId>,
    member_permissions: Option<u64>,
    options: Vec<InteractionOption>,
}

impl TryFrom<InteractionRequest> for CommandContext {
    type Error = InteractionHandlerError;

    fn try_from(request: InteractionRequest) -> Result<Self, Self::Error> {
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
            return Err("lobby handler received a component interaction".into());
        };
        Ok(Self {
            name,
            user_id: AppUserId(i64::try_from(user_id).map_err(|_| {
                InteractionHandlerError::Transformer {
                    value: user_id.to_string(),
                }
            })?),
            user_display_name,
            guild_id: guild_id
                .map(|guild_id| i64::try_from(guild_id).map(AppGuildId))
                .transpose()
                .map_err(|_| InteractionHandlerError::Transformer {
                    value: "guild id".to_owned(),
                })?,
            channel_id: channel_id
                .map(|channel_id| i64::try_from(channel_id).map(AppChannelId))
                .transpose()
                .map_err(|_| InteractionHandlerError::Transformer {
                    value: "channel id".to_owned(),
                })?,
            member_permissions,
            options,
        })
    }
}

#[async_trait]
impl InteractionHandler for LobbyInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let command = CommandContext::try_from(request)?;
        match command.name.as_str() {
            "lobby" => self.handle_lobby(command, responder).await,
            "kick" => self.handle_kick(command, responder).await,
            "join" => self.handle_join(command, responder).await,
            "leave" => self.handle_leave(command, responder).await,
            "resetlobby" => self.handle_reset(command, responder).await,
            "readycheck" => self.handle_readycheck(command, responder).await,
            name => Err(format!("unexpected lobby command {name}").into()),
        }
    }
}

impl LobbyInteractionHandler {
    async fn handle_readycheck(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let Some((guild_id, channel_id)) = guild_channel(&command) else {
            return followup_ephemeral(&responder, "This command can only be used in a server.")
                .await;
        };
        let kind = match self.state.commands.select_readycheck_lobby(
            command.user_id,
            guild_id,
            selected_lobby_kind(&command.options),
        ) {
            Ok(kind) => kind,
            Err(cama_app::lobby_runtime::LobbySelectionError::Ambiguous) => {
                return followup_ephemeral(&responder, AMBIGUOUS_LOBBY_MESSAGE).await;
            }
            Err(cama_app::lobby_runtime::LobbySelectionError::NoLobby { message }) => {
                return followup_ephemeral(&responder, &message).await;
            }
        };
        match self
            .run_readycheck(
                LobbyScope::new(guild_id, kind),
                command.user_id,
                channel_id,
                true,
                false,
            )
            .await
        {
            Ok(result) => {
                if !result.mention_ids.is_empty() && channel_id != result.readycheck_channel_id {
                    let mention_ids = result
                        .mention_ids
                        .iter()
                        .filter_map(|player_id| u64::try_from(player_id.0).ok())
                        .collect::<Vec<_>>();
                    let tags = mention_ids
                        .iter()
                        .map(|user_id| format!("<@{user_id}>"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    return responder
                        .followup(
                            InteractionResponse::message(format!(
                                "⚔️ **{} ready check!** {tags}\n[React in the lobby thread]({})",
                                kind.label(),
                                result.jump_url
                            ))
                            .with_user_mentions(mention_ids),
                        )
                        .await
                        .map_err(|error| error.to_string().into());
                }
                let verb = if result.is_refresh {
                    "refreshed"
                } else {
                    "posted"
                };
                let mut content = format!(
                    "✅ {} ready check {verb}! [View]({})",
                    kind.label(),
                    result.jump_url
                );
                if result.pruned_count != 0 {
                    content.push_str(&format!(
                        " Removed {} away player(s); they can rejoin with `/join`.",
                        result.pruned_count
                    ));
                }
                followup_ephemeral(&responder, &content).await
            }
            Err(ReadycheckRunError::Policy(failure)) => {
                followup_ephemeral(&responder, readycheck_failure_message(failure)).await
            }
            Err(ReadycheckRunError::Transport(message)) => Err(message.into()),
        }
    }

    async fn run_readycheck(
        &self,
        scope: LobbyScope,
        invoker_id: AppUserId,
        invocation_channel_id: AppChannelId,
        confirm_invoker: bool,
        mirror_invocation: bool,
    ) -> Result<ReadycheckRunResult, ReadycheckRunError> {
        let operation_lock = self.state.commands.scope_operation_lock(scope);
        let _guard = operation_lock.lock().await;
        self.state.sync_ready_lobby(scope);
        let lobby = self
            .state
            .service
            .get_lobby(scope)
            .ok_or(ReadycheckRunError::Policy(
                ReadycheckCommandFailure::NoLobby,
            ))?;
        let (player_data, mentionable) = self.classify_readycheck_players(&lobby).await;
        let existing_generation = self.state.readychecks.readycheck_generation(scope);
        let existing_message = if let Some(generation) = &existing_generation {
            match self
                .state
                .transport
                .fetch_message(
                    to_u64(generation.channel_id.0).map_err(ReadycheckRunError::Transport)?,
                    to_u64(generation.message_id.0).map_err(ReadycheckRunError::Transport)?,
                )
                .await
            {
                Ok(Some(_)) => ExistingMessageState::Available,
                Ok(None) => ExistingMessageState::Missing,
                Err(error) => {
                    warn!(%error, ?scope, "readycheck predecessor availability unknown");
                    ExistingMessageState::Unknown
                }
            }
        } else {
            ExistingMessageState::Missing
        };
        let reserved_players = lobby
            .players
            .iter()
            .copied()
            .filter(|player_id| {
                self.state
                    .service
                    .get_in_flight_lobby_kind_for_player(*player_id, scope.guild_id)
                    == Some(scope.kind)
            })
            .collect();
        let plan = self
            .state
            .readychecks
            .prepare_command(
                ReadycheckCommandRequest {
                    guild_id: Some(scope.guild_id),
                    kind: scope.kind,
                    invoker_id,
                    invocation_channel_id,
                    now: unix_time_now(),
                    confirm_invoker,
                    existing_message,
                    reserved_players,
                },
                player_data.clone(),
            )
            .map_err(ReadycheckRunError::Policy)?;
        if !plan.pruned_players.is_empty() {
            let state = Arc::clone(&self.state);
            let pruned = plan.pruned_players.clone();
            let persisted = tokio::task::spawn_blocking(move || {
                state.service.try_remove_players_from_lobby(&pruned, scope)
            })
            .await
            .map_err(|error| ReadycheckRunError::Transport(error.to_string()))?
            .map_err(|error| ReadycheckRunError::Transport(error.to_string()))?;
            if persisted != plan.pruned_players {
                if let Some(permit) = &plan.permit {
                    self.state.readychecks.abort_publication(permit);
                }
                self.state.sync_ready_lobby(scope);
                return Err(ReadycheckRunError::Transport(
                    "readycheck stale-prune persistence raced with lobby membership".to_owned(),
                ));
            }
            self.state.sync_ready_lobby(scope);
        }
        self.execute_readycheck_plan(
            plan,
            player_data,
            mentionable,
            invocation_channel_id,
            mirror_invocation,
        )
        .await
    }

    async fn classify_readycheck_players(
        &self,
        lobby: &LobbySnapshot,
    ) -> (
        BTreeMap<AppUserId, ReadycheckPlayerData>,
        BTreeSet<AppUserId>,
    ) {
        let now = unix_time_now();
        let mut data = BTreeMap::new();
        let mut mentionable = BTreeSet::new();
        for player_id in &lobby.players {
            let member = match (to_u64(lobby.scope.guild_id.0), to_u64(player_id.0)) {
                (Ok(guild_id), Ok(player_id)) => self
                    .state
                    .transport
                    .guild_member(guild_id, player_id)
                    .await
                    .ok()
                    .flatten(),
                _ => None,
            };
            let recent = lobby
                .player_join_times
                .get(player_id)
                .is_some_and(|joined_at| now - joined_at < 10.0 * 60.0);
            let (name, group) = member.map_or_else(
                || {
                    (
                        self.state
                            .players
                            .fallback_name(*player_id, lobby.scope.guild_id),
                        if recent {
                            ReadinessGroup::Recent
                        } else {
                            ReadinessGroup::Afk
                        },
                    )
                },
                |member| {
                    mentionable.insert(*player_id);
                    let playing_dota = member
                        .activities
                        .iter()
                        .any(|activity| activity.to_lowercase().contains("dota"));
                    let group = if recent {
                        ReadinessGroup::Recent
                    } else if playing_dota {
                        ReadinessGroup::PlayingDota
                    } else if member.in_voice
                        || matches!(
                            member.presence,
                            DiscordPresence::Online | DiscordPresence::DoNotDisturb
                        )
                    {
                        ReadinessGroup::Ready
                    } else {
                        ReadinessGroup::Afk
                    };
                    (member.display_name, group)
                },
            );
            data.insert(*player_id, ReadycheckPlayerData { name, group });
        }
        (data, mentionable)
    }

    /// Mirror Python's `sync_readycheck_with_lobby`: an active generation is
    /// reconciled against the post-mutation roster, recent signups are
    /// state-auto-confirmed unless they explicitly withdrew, stale physical
    /// reactions are removed best effort, and the generation is repainted.
    async fn sync_readycheck_with_lobby(&self, scope: LobbyScope) {
        self.state.sync_ready_lobby(scope);
        let Some(previous) = self.state.readychecks.readycheck_generation(scope) else {
            return;
        };
        let Some(lobby) = self.state.service.get_lobby(scope) else {
            return;
        };
        let (player_data, _) = self.classify_readycheck_players(&lobby).await;
        let lobby_ids = player_data.keys().copied().collect::<BTreeSet<_>>();
        let declined = previous.declined;
        let auto_confirm = player_data
            .iter()
            .filter_map(|(player_id, data)| {
                (data.group == ReadinessGroup::Recent && !declined.contains(player_id))
                    .then_some(*player_id)
            })
            .collect::<BTreeSet<_>>();
        let departed_confirmations = previous
            .reacted
            .keys()
            .copied()
            .filter(|player_id| !lobby_ids.contains(player_id));
        let new_unconfirmed = lobby_ids
            .difference(&previous.lobby_ids)
            .copied()
            .filter(|player_id| !previous.reacted.contains_key(player_id))
            .filter(|player_id| !auto_confirm.contains(player_id));
        for player_id in departed_confirmations.chain(new_unconfirmed) {
            let (Ok(channel_id), Ok(message_id), Ok(user_id)) = (
                to_u64(previous.channel_id.0),
                to_u64(previous.message_id.0),
                to_u64(player_id.0),
            ) else {
                continue;
            };
            if let Err(error) = self
                .state
                .transport
                .remove_reaction(
                    channel_id,
                    message_id,
                    &DiscordEmoji::unicode(READY_EMOJI),
                    user_id,
                )
                .await
            {
                debug!(%error, ?scope, user_id, "readycheck roster reaction cleanup failed");
            }
        }
        if self
            .state
            .readychecks
            .update_readycheck_data(scope, lobby_ids, player_data, previous.message_id)
            .is_none()
        {
            return;
        }
        for player_id in auto_confirm {
            self.state.readychecks.add_readycheck_reaction(
                scope,
                player_id,
                format!("<@{}>", player_id.0),
                Some(previous.message_id),
            );
        }
        let Some(generation) = self.state.readychecks.readycheck_generation(scope) else {
            return;
        };
        let embed = readycheck_embed(
            scope.kind,
            &generation.player_data,
            &generation.reacted,
            self.state.config.ready_threshold,
        );
        match (
            to_u64(generation.channel_id.0),
            to_u64(generation.message_id.0),
        ) {
            (Ok(channel_id), Ok(message_id)) => {
                if let Err(error) = self
                    .state
                    .transport
                    .edit_message(
                        channel_id,
                        message_id,
                        DiscordMessage::silent(InteractionResponse::message("").embed(embed))
                            .preserving_content(),
                    )
                    .await
                {
                    warn!(%error, ?scope, "readycheck roster repaint failed");
                }
            }
            _ => warn!(?scope, "readycheck roster repaint has invalid Discord IDs"),
        }
        self.notify_readycheck_completion(scope, generation.message_id)
            .await;
    }

    async fn execute_readycheck_plan(
        &self,
        plan: ReadycheckCommandPlan,
        player_data: BTreeMap<AppUserId, ReadycheckPlayerData>,
        mentionable: BTreeSet<AppUserId>,
        invocation_channel_id: AppChannelId,
        mirror_invocation: bool,
    ) -> Result<ReadycheckRunResult, ReadycheckRunError> {
        for operation in &plan.transport.operations {
            match operation {
                ReadycheckTransportOperation::DeleteReadycheck {
                    channel_id,
                    message_id,
                    ..
                } => {
                    let _ = self
                        .state
                        .transport
                        .delete_message(
                            to_u64(channel_id.0).map_err(ReadycheckRunError::Transport)?,
                            to_u64(message_id.0).map_err(ReadycheckRunError::Transport)?,
                        )
                        .await;
                }
                ReadycheckTransportOperation::RemoveDepartedReaction {
                    channel_id,
                    message_id,
                    player_id,
                    emoji,
                    ..
                } => {
                    let _ = self
                        .state
                        .transport
                        .remove_reaction(
                            to_u64(channel_id.0).map_err(ReadycheckRunError::Transport)?,
                            to_u64(message_id.0).map_err(ReadycheckRunError::Transport)?,
                            &DiscordEmoji::unicode(*emoji),
                            to_u64(player_id.0).map_err(ReadycheckRunError::Transport)?,
                        )
                        .await;
                }
                ReadycheckTransportOperation::SyncLobbyDisplays { scope, .. } => {
                    self.state
                        .sync_lobby_display(*scope)
                        .await
                        .map_err(ReadycheckRunError::Transport)?;
                }
                _ => {}
            }
        }

        if plan.mode == PublicationMode::Refresh {
            let generation = self
                .state
                .readychecks
                .readycheck_generation(plan.scope)
                .ok_or_else(|| {
                    ReadycheckRunError::Transport("readycheck generation disappeared".to_owned())
                })?;
            let embed = readycheck_embed(
                plan.scope.kind,
                &generation.player_data,
                &generation.reacted,
                self.state.config.ready_threshold,
            );
            self.state
                .transport
                .edit_message(
                    to_u64(generation.channel_id.0).map_err(ReadycheckRunError::Transport)?,
                    to_u64(generation.message_id.0).map_err(ReadycheckRunError::Transport)?,
                    DiscordMessage::silent(InteractionResponse::message("").embed(embed))
                        .preserving_content(),
                )
                .await
                .map_err(ReadycheckRunError::Transport)?;
            self.publish_readycheck_ping(
                plan.scope.kind,
                generation.channel_id,
                &plan.mention_ids,
                &mentionable,
                None,
            )
            .await?;
            let jump_url = discord_jump_url(
                plan.scope.guild_id,
                generation.channel_id,
                generation.message_id,
            )?;
            if mirror_invocation
                && invocation_channel_id != generation.channel_id
                && !plan.mention_ids.is_empty()
            {
                self.publish_readycheck_ping(
                    plan.scope.kind,
                    invocation_channel_id,
                    &plan.mention_ids,
                    &mentionable,
                    Some(&jump_url),
                )
                .await?;
            }
            self.notify_readycheck_completion(plan.scope, generation.message_id)
                .await;
            return Ok(ReadycheckRunResult {
                jump_url,
                is_refresh: true,
                pruned_count: 0,
                mention_ids: plan
                    .mention_ids
                    .intersection(&mentionable)
                    .copied()
                    .collect(),
                readycheck_channel_id: generation.channel_id,
            });
        }

        let permit = plan.permit.clone().ok_or_else(|| {
            ReadycheckRunError::Transport("new readycheck plan has no permit".to_owned())
        })?;
        let lobby = self.state.service.get_lobby(plan.scope).ok_or_else(|| {
            ReadycheckRunError::Transport("lobby disappeared during readycheck".to_owned())
        })?;
        let thread_id = lobby
            .message_ids
            .thread_id
            .ok_or(ReadycheckRunError::Policy(
                ReadycheckCommandFailure::NoThread,
            ))?;
        let mut initial_reacted = BTreeMap::new();
        for (player_id, data) in &player_data {
            if data.group == ReadinessGroup::Recent
                || (permit.invoker_id == *player_id && permit.confirm_invoker)
            {
                initial_reacted.insert(*player_id, format!("<@{}>", player_id.0));
            }
        }
        let embed = readycheck_embed(
            plan.scope.kind,
            &player_data,
            &initial_reacted,
            self.state.config.ready_threshold,
        );
        let receipt = match self
            .state
            .transport
            .send_message(
                to_u64(thread_id.0).map_err(ReadycheckRunError::Transport)?,
                DiscordMessage::silent(InteractionResponse::message("").embed(embed)),
            )
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                self.state.readychecks.abort_publication(&permit);
                return Err(ReadycheckRunError::Transport(error));
            }
        };
        let message_id = AppMessageId(i64::try_from(receipt.message_id).map_err(|_| {
            ReadycheckRunError::Transport("readycheck message id overflow".to_owned())
        })?);
        let channel_id = AppChannelId(i64::try_from(receipt.channel_id).map_err(|_| {
            ReadycheckRunError::Transport("readycheck channel id overflow".to_owned())
        })?);
        if self.state.readychecks.commit_publication(
            permit,
            message_id,
            channel_id,
            player_data,
            unix_time_now(),
        ) != CommitPublicationResult::Applied
        {
            let _ = self
                .state
                .transport
                .delete_message(receipt.channel_id, receipt.message_id)
                .await;
            return Err(ReadycheckRunError::Transport(
                "readycheck publication was superseded".to_owned(),
            ));
        }
        let _ = self
            .state
            .transport
            .add_reaction(
                receipt.channel_id,
                receipt.message_id,
                &DiscordEmoji::unicode(READY_EMOJI),
            )
            .await;
        self.publish_readycheck_ping(
            plan.scope.kind,
            channel_id,
            &plan.mention_ids,
            &mentionable,
            None,
        )
        .await?;
        if mirror_invocation && invocation_channel_id != channel_id && !plan.mention_ids.is_empty()
        {
            self.publish_readycheck_ping(
                plan.scope.kind,
                invocation_channel_id,
                &plan.mention_ids,
                &mentionable,
                Some(&receipt.jump_url),
            )
            .await?;
        }
        self.notify_readycheck_completion(plan.scope, message_id)
            .await;
        Ok(ReadycheckRunResult {
            jump_url: receipt.jump_url,
            is_refresh: false,
            pruned_count: plan.pruned_players.len(),
            mention_ids: plan
                .mention_ids
                .intersection(&mentionable)
                .copied()
                .collect(),
            readycheck_channel_id: channel_id,
        })
    }

    async fn publish_readycheck_ping(
        &self,
        kind: LobbyKind,
        channel_id: AppChannelId,
        mention_ids: &BTreeSet<AppUserId>,
        mentionable: &BTreeSet<AppUserId>,
        jump_url: Option<&str>,
    ) -> Result<(), ReadycheckRunError> {
        let users = mention_ids
            .intersection(mentionable)
            .filter_map(|player_id| u64::try_from(player_id.0).ok())
            .collect::<BTreeSet<_>>();
        if users.is_empty() {
            return Ok(());
        }
        let tags = users
            .iter()
            .map(|user_id| format!("<@{user_id}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut content = format!("⚔️ **{} ready check!** {tags}", kind.label());
        if let Some(jump_url) = jump_url {
            content.push_str(&format!("\n[React in the lobby thread]({jump_url})"));
        }
        self.state
            .transport
            .send_message(
                to_u64(channel_id.0).map_err(ReadycheckRunError::Transport)?,
                DiscordMessage::mentioning(
                    InteractionResponse::message(content)
                        .with_user_mentions(users.iter().copied().collect()),
                    users,
                ),
            )
            .await
            .map(|_| ())
            .map_err(ReadycheckRunError::Transport)
    }

    async fn notify_readycheck_completion(&self, scope: LobbyScope, message_id: AppMessageId) {
        if !self
            .state
            .readychecks
            .take_completion_notification_at_threshold(
                scope,
                message_id,
                self.state.config.ready_threshold,
            )
        {
            return;
        }
        let Some(generation) = self.state.readychecks.readycheck_generation(scope) else {
            return;
        };
        let target_channel_id = self
            .state
            .service
            .get_lobby(scope)
            .and_then(|lobby| lobby.message_ids.origin_channel_id)
            .unwrap_or(generation.channel_id);
        let Ok(channel_id) = to_u64(target_channel_id.0) else {
            warn!(
                ?scope,
                "readycheck completion has invalid origin/fallback channel id"
            );
            self.state
                .readychecks
                .release_completion_notification(scope, message_id);
            return;
        };
        let result = self
            .state
            .transport
            .send_message(
                channel_id,
                DiscordMessage::silent(InteractionResponse::message(ready_announcement(
                    scope.kind,
                    self.state.config.ready_threshold,
                ))),
            )
            .await;
        if let Err(error) = result {
            warn!(%error, ?scope, "readycheck completion notification failed");
            self.state
                .readychecks
                .release_completion_notification(scope, message_id);
        }
    }

    async fn handle_kick(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let Some((guild_id, _channel_id)) = guild_channel(&command) else {
            return followup_ephemeral(&responder, "This command can only be used in a server.")
                .await;
        };
        let Some((target_id, target_name)) = selected_user(&command.options, "player")? else {
            return Err(InteractionHandlerError::Transformer {
                value: "missing player".to_owned(),
            });
        };
        let memberships = self
            .state
            .service
            .get_lobby_kinds_for_player(target_id, guild_id);
        if memberships.is_empty() {
            return followup_ephemeral(
                &responder,
                &format!("⚠️ <@{}> is not in a lobby.", target_id.0),
            )
            .await;
        }
        let is_admin = self.is_admin(&command);
        let kickable = memberships
            .into_iter()
            .filter(|kind| {
                if is_admin {
                    return true;
                }
                self.state
                    .service
                    .get_lobby(LobbyScope::new(guild_id, *kind))
                    .is_some_and(|lobby| {
                        lobby.created_by == Some(command.user_id)
                            && lobby.players.contains(&command.user_id)
                    })
            })
            .collect::<Vec<_>>();
        if kickable.is_empty() {
            return followup_ephemeral(
                &responder,
                "❌ Permission denied. Admin or lobby creator only.",
            )
            .await;
        }
        if target_id == command.user_id {
            return followup_ephemeral(
                &responder,
                "❌ You can't kick yourself. Use the Leave button in the lobby thread.",
            )
            .await;
        }
        let mut kicked = Vec::new();
        for kind in kickable {
            let scope = LobbyScope::new(guild_id, kind);
            let operation_lock = self.state.commands.scope_operation_lock(scope);
            let _guard = operation_lock.lock().await;
            let state = Arc::clone(&self.state);
            let removed = tokio::task::spawn_blocking(move || {
                state.service.try_leave_lobby(target_id, scope)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
            if !removed {
                continue;
            }
            kicked.push(kind);
            self.sync_readycheck_with_lobby(scope).await;
            if let Err(error) = self.state.sync_lobby_display(scope).await {
                warn!(%error, ?scope, "lobby display sync after kick failed");
            }
            let _ = self.state.remove_lobby_reaction(scope, target_id).await;
            if let Some(thread_id) = self
                .state
                .service
                .get_lobby(scope)
                .and_then(|lobby| lobby.message_ids.thread_id)
                .and_then(|thread_id| to_u64(thread_id.0).ok())
            {
                let _ = self
                    .state
                    .transport
                    .send_message(
                        thread_id,
                        DiscordMessage::silent(InteractionResponse::message(format!(
                            "👢 **{target_name}** was kicked by {}.",
                            command.user_display_name
                        ))),
                    )
                    .await;
            }
        }
        if kicked.is_empty() {
            return followup_ephemeral(
                &responder,
                &format!("❌ Failed to kick <@{}>.", target_id.0),
            )
            .await;
        }
        let reason = selected_string(&command.options, "reason").map(str::to_owned);
        let mut dm = format!(
            "You were kicked from {} by <@{}>.",
            format_lobby_labels(&kicked),
            command.user_id.0
        );
        if let Some(reason) = reason.as_deref() {
            dm.push_str(&format!("\nReason: {reason}"));
        }
        if let Err(error) = self
            .state
            .transport
            .send_direct_message(
                to_u64(target_id.0)?,
                DiscordMessage::silent(InteractionResponse::message(dm)),
            )
            .await
        {
            debug!(%error, target_id = target_id.0, "failed to DM kicked lobby player");
        }
        let followup = followup_ephemeral(
            &responder,
            &format!(
                "✅ Kicked <@{}> from {}.",
                target_id.0,
                format_lobby_labels(&kicked)
            ),
        )
        .await;
        for kind in &kicked {
            let moderation = self.state.moderation.clone();
            let audit_reason = reason.clone();
            let target_id = target_id.0;
            let guild_id = guild_id.0;
            let actor_id = command.user_id.0;
            let lobby_kind = match kind {
                LobbyKind::Open => SuspensionScope::Open,
                LobbyKind::LowSkill => SuspensionScope::Lowskill,
            };
            let source = if is_admin {
                ModerationSource::Admin
            } else {
                ModerationSource::Creator
            };
            match tokio::task::spawn_blocking(move || {
                ModerationService::new(moderation).record_kick(RecordKick {
                    discord_id: target_id,
                    guild_id: Some(guild_id),
                    actor_id,
                    lobby_kind,
                    reason: audit_reason.as_deref(),
                    source,
                    now: unix_time_now() as i64,
                })
            })
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => warn!(?error, "failed to audit lobby kick"),
                Err(error) => warn!(%error, "lobby kick audit task failed"),
            }
        }
        followup
    }

    async fn handle_reset(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let Some((guild_id, channel_id)) = guild_channel(&command) else {
            return followup_ephemeral(&responder, "This command can only be used in a server.")
                .await;
        };
        let request = ResetLobbyRequest {
            guild_id,
            user_id: command.user_id,
            is_admin: self.is_admin(&command),
            explicit_kind: selected_lobby_kind(&command.options),
            interaction_channel_id: channel_id,
        };
        let transport = InteractionLobbyTransport {
            state: Arc::clone(&self.state),
            responder: Arc::clone(&responder),
        };
        let outcome = self
            .state
            .commands
            .reset_lobby(
                &request,
                &self.state.pending_matches,
                self.state.drafts.as_ref(),
                &transport,
            )
            .await;
        if let ResetLobbyOutcome::Reset { scope, .. } = outcome {
            self.state.readychecks.reset_lobby(scope);
            let observer = self
                .state
                .join_observer
                .read()
                .ok()
                .and_then(|observer| observer.clone());
            if let Some(observer) = observer {
                match u64::try_from(scope.guild_id.0) {
                    Ok(guild_id) => {
                        if let Err(error) = observer.lobby_reset(guild_id, scope.kind).await {
                            warn!(%error, guild_id, ?scope, "lobby reset observer failed");
                        }
                    }
                    Err(_) => warn!(?scope, "lobby reset observer has invalid guild id"),
                }
            }
        }
        Ok(())
    }

    fn is_admin(&self, command: &CommandContext) -> bool {
        self.state
            .config
            .admin_user_ids
            .contains(&u64::try_from(command.user_id.0).unwrap_or_default())
            || command.member_permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
    }

    async fn handle_lobby(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let Some((guild_id, channel_id)) = guild_channel(&command) else {
            return followup_ephemeral(&responder, "This command can only be used in a server.")
                .await;
        };
        let player = self.load_player(command.user_id, guild_id).await?;
        let Some(player) = player else {
            return followup_ephemeral(
                &responder,
                "❌ You're not registered! Use `/player register` first.",
            )
            .await;
        };
        let kind = selected_lobby_kind(&command.options).unwrap_or(LobbyKind::Open);
        let scope = LobbyScope::new(guild_id, kind);
        let operation_lock = self.state.commands.scope_operation_lock(scope);
        let _guard = operation_lock.lock().await;
        // Opening a lobby is participation too, so curfew blocks it. Viewing
        // one that already exists stays allowed, matching the get-or-create
        // contract that keeps existing lobbies visible to suspended users; the
        // join itself is refused downstream either way. The operation lock is
        // already held, so no concurrent creator can slip in between.
        if self.state.service.get_lobby(scope).is_none()
            && let Some(window_description) =
                self.active_curfew_window(scope, command.user_id).await?
        {
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ You're inside your {window_description} curfew window, so you can't open a lobby. Use `/player curfew remove` if you'd rather queue through it."
                ),
            )
            .await;
        }
        let state = Arc::clone(&self.state);
        let lobby = tokio::task::spawn_blocking(move || {
            state
                .service
                .get_or_create_lobby(Some(command.user_id), scope)
        })
        .await
        .map_err(|error| error.to_string())?;
        let lobby = match lobby {
            Ok(lobby) => lobby,
            Err(LobbyCreationError::RatingTooHigh) => {
                return followup_ephemeral(
                    &responder,
                    "❌ Only players below 1400 Glicko can create Whine & Cheese.",
                )
                .await;
            }
            Err(LobbyCreationError::Suspended(suspension)) => {
                return followup_ephemeral(
                    &responder,
                    &format!("❌ {}", private_suspension_message(&suspension)),
                )
                .await;
            }
            Err(LobbyCreationError::ModerationGuard(error)) => {
                return Err(error.into());
            }
            Err(LobbyCreationError::Persistence(error)) => {
                return Err(error.to_string().into());
            }
        };

        if let (Some(existing_channel), Some(existing_message), Some(_thread_id)) = (
            lobby.message_ids.channel_id,
            lobby.message_ids.message_id,
            lobby.message_ids.thread_id,
        ) && let Ok(Some(message)) = self
            .state
            .transport
            .fetch_message(to_u64(existing_channel.0)?, to_u64(existing_message.0)?)
            .await
        {
            let join = self
                .join_registered_player(scope, command.user_id, &player)
                .await?;
            if join.joined {
                self.best_effort_join_publication(
                    scope,
                    command.user_id,
                    &player.name,
                    &command.user_display_name,
                    JoinThreadActivity::Command,
                    join.joined_at_ns.expect("successful join has commit time"),
                )
                .await;
            } else if let Err(error) = self.state.sync_lobby_display(scope).await {
                warn!(%error, ?scope, "failed to refresh existing lobby display");
            }
            let warning = lobby_command_auto_join_warning(kind, &join);
            let content = if join.joined {
                format!(
                    "✅ Joined {}! [View {}]({})",
                    kind.label(),
                    kind.display_name(),
                    message.receipt.jump_url
                )
            } else if let Some(warning) = warning {
                format!(
                    "{warning} [View {}]({})",
                    kind.label(),
                    message.receipt.jump_url
                )
            } else {
                format!("[View {}]({})", kind.label(), message.receipt.jump_url)
            };
            return followup_ephemeral(&responder, &content).await;
        }

        let state = Arc::clone(&self.state);
        let lobby_for_embed = lobby.clone();
        let embed = tokio::task::spawn_blocking(move || {
            interaction_embed(
                state
                    .service
                    .build_lobby_embed(&lobby_for_embed, Some(&state.first_game_previews)),
            )
        })
        .await
        .map_err(|error| error.to_string())?;
        let configured_channel = match kind {
            LobbyKind::LowSkill => self
                .state
                .config
                .low_skill_lobby_channel_id
                .or(self.state.config.lobby_channel_id),
            LobbyKind::Open => self.state.config.lobby_channel_id,
        };
        let primary_channel = configured_channel.unwrap_or(to_u64(channel_id.0)?);
        let message = DiscordMessage::silent(InteractionResponse::message("").embed(embed));
        let receipt = match self
            .state
            .transport
            .send_message(primary_channel, message.clone())
            .await
        {
            Ok(receipt) => receipt,
            Err(primary_error) if primary_channel != to_u64(channel_id.0)? => {
                warn!(%primary_error, primary_channel, "configured lobby channel unavailable; using interaction channel");
                self.state
                    .transport
                    .send_message(to_u64(channel_id.0)?, message)
                    .await
                    .map_err(InteractionHandlerError::from)?
            }
            Err(error) => return Err(error.into()),
        };
        let thread_id = match self
            .state
            .transport
            .create_public_thread(receipt.channel_id, receipt.message_id, kind.label())
            .await
        {
            Ok(thread_id) => thread_id,
            Err(error) => {
                let _ = self
                    .state
                    .transport
                    .delete_message(receipt.channel_id, receipt.message_id)
                    .await;
                warn!(%error, ?scope, "could not create required lobby thread");
                return followup_ephemeral(
                    &responder,
                    "❌ Bot needs Create Public Threads permission to create lobbies.",
                )
                .await;
            }
        };
        let ids = LobbyMessageIds {
            message_id: Some(AppMessageId(i64::try_from(receipt.message_id).map_err(
                |_| InteractionHandlerError::Transformer {
                    value: receipt.message_id.to_string(),
                },
            )?)),
            channel_id: Some(AppChannelId(i64::try_from(receipt.channel_id).map_err(
                |_| InteractionHandlerError::Transformer {
                    value: receipt.channel_id.to_string(),
                },
            )?)),
            thread_id: Some(AppChannelId(i64::try_from(thread_id).map_err(|_| {
                InteractionHandlerError::Transformer {
                    value: thread_id.to_string(),
                }
            })?)),
            embed_message_id: Some(AppMessageId(i64::try_from(receipt.message_id).map_err(
                |_| InteractionHandlerError::Transformer {
                    value: receipt.message_id.to_string(),
                },
            )?)),
            origin_channel_id: Some(channel_id),
        };
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || state.service.try_set_lobby_message_ids(scope, ids))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        self.state.sync_ready_lobby(scope);

        for result in [
            self.state
                .transport
                .pin_message(receipt.channel_id, receipt.message_id)
                .await,
            self.state
                .transport
                .add_reaction(
                    receipt.channel_id,
                    receipt.message_id,
                    &DiscordEmoji::unicode(SWORD_EMOJI),
                )
                .await,
            self.state
                .transport
                .add_reaction(
                    receipt.channel_id,
                    receipt.message_id,
                    &DiscordEmoji {
                        name: "jopacoin".to_owned(),
                        id: Some(JOPACOIN_EMOJI_ID),
                    },
                )
                .await,
            self.state
                .transport
                .add_reaction(
                    receipt.channel_id,
                    receipt.message_id,
                    &DiscordEmoji::unicode(CLIPBOARD_EMOJI),
                )
                .await,
            self.state
                .transport
                .add_reaction(
                    receipt.channel_id,
                    receipt.message_id,
                    &DiscordEmoji::unicode(BELL_EMOJI),
                )
                .await,
        ] {
            if let Err(error) = result {
                debug!(%error, ?scope, "best-effort lobby decoration failed");
            }
        }

        let join = self
            .join_registered_player(scope, command.user_id, &player)
            .await?;
        if join.joined {
            self.best_effort_join_publication(
                scope,
                command.user_id,
                &player.name,
                &command.user_display_name,
                JoinThreadActivity::Command,
                join.joined_at_ns.expect("successful join has commit time"),
            )
            .await;
        }
        let warning = lobby_command_auto_join_warning(kind, &join);
        let content = if join.joined {
            format!(
                "✅ {} created and joined! [View]({})",
                kind.label(),
                receipt.jump_url
            )
        } else if let Some(warning) = warning {
            format!(
                "✅ {} created! {warning} [View]({})",
                kind.label(),
                receipt.jump_url
            )
        } else {
            format!("✅ {} created! [View]({})", kind.label(), receipt.jump_url)
        };
        followup_ephemeral(&responder, &content).await
    }

    async fn handle_join(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let Some((guild_id, _channel_id)) = guild_channel(&command) else {
            return followup_ephemeral(&responder, "This command can only be used in a server.")
                .await;
        };
        let Some(player) = self.load_player(command.user_id, guild_id).await? else {
            return followup_ephemeral(
                &responder,
                "❌ You're not registered! Use `/player register` first.",
            )
            .await;
        };
        if player.preferred_roles.as_ref().is_none_or(Vec::is_empty) {
            return followup_ephemeral(
                &responder,
                "❌ Set your preferred roles first! Use `/player roles`.",
            )
            .await;
        }
        let kind = selected_lobby_kind(&command.options).unwrap_or(LobbyKind::Open);
        let scope = LobbyScope::new(guild_id, kind);
        if self.state.service.get_lobby(scope).is_none() {
            return followup_ephemeral(
                &responder,
                &format!(
                    "⚠️ No active {} lobby. Use `/lobby` to create one.",
                    kind.label()
                ),
            )
            .await;
        }
        let operation_lock = self.state.commands.scope_operation_lock(scope);
        let _guard = operation_lock.lock().await;
        let result = self
            .join_registered_player(scope, command.user_id, &player)
            .await?;
        if !result.joined {
            return followup_ephemeral(
                &responder,
                result
                    .warning
                    .as_deref()
                    .unwrap_or("❌ Could not join lobby."),
            )
            .await;
        }
        let event = self
            .best_effort_join_publication(
                scope,
                command.user_id,
                &player.name,
                &command.user_display_name,
                JoinThreadActivity::Silent,
                result
                    .joined_at_ns
                    .expect("successful join has commit time"),
            )
            .await;
        followup_ephemeral(&responder, &format!("✅ Joined {}!", kind.label())).await?;
        if let Some(event) = event {
            self.best_effort_explicit_lobby_join_neon(event).await;
        }
        Ok(())
    }

    async fn handle_leave(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let Some((guild_id, _channel_id)) = guild_channel(&command) else {
            return followup_ephemeral(&responder, "This command can only be used in a server.")
                .await;
        };
        let memberships = self
            .state
            .service
            .get_lobby_kinds_for_player(command.user_id, guild_id);
        if memberships.is_empty() {
            return followup_ephemeral(&responder, "⚠️ You're not in a lobby.").await;
        }
        let mut left = Vec::new();
        let mut pinned = Vec::new();
        for kind in memberships {
            let scope = LobbyScope::new(guild_id, kind);
            let operation_lock = self.state.commands.scope_operation_lock(scope);
            let _guard = operation_lock.lock().await;
            let state = Arc::clone(&self.state);
            let removed = tokio::task::spawn_blocking(move || {
                state.service.try_leave_lobby(command.user_id, scope)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
            if removed {
                left.push(kind);
                self.best_effort_leave_publication(
                    scope,
                    command.user_id,
                    &command.user_display_name,
                    false,
                )
                .await;
            } else if self
                .state
                .service
                .get_in_flight_lobby_kind_for_player(command.user_id, guild_id)
                == Some(kind)
            {
                pinned.push(kind);
            }
        }
        let content = if left.is_empty() {
            if pinned.is_empty() {
                "⚠️ You’re no longer in the lobby.".to_owned()
            } else {
                format!(
                    "❌ You can’t leave {} while its shuffle or draft is in progress.",
                    format_lobby_labels(&pinned)
                )
            }
        } else {
            let mut content = format!("✅ Left {}.", format_lobby_labels(&left));
            if !pinned.is_empty() {
                content.push_str(&format!(
                    "\n⚠️ Still in {} — its shuffle or draft is in progress.",
                    format_lobby_labels(&pinned)
                ));
            }
            content
        };
        followup_ephemeral(&responder, &content).await
    }

    async fn load_player(
        &self,
        player_id: AppUserId,
        guild_id: AppGuildId,
    ) -> Result<Option<cama_domain::player::Player>, InteractionHandlerError> {
        let players = self.state.players.clone();
        tokio::task::spawn_blocking(move || players.player(player_id, guild_id))
            .await
            .map_err(|error| error.to_string())?
            .map_err(InteractionHandlerError::from)
    }

    /// Format the player's currently-active curfew window, if any, for
    /// display. Shared by every join surface (`/lobby` auto-join, `/lobby
    /// join`, and the sword-reaction join) so none of them can slip a player
    /// into a lobby during their own curfew.
    async fn active_curfew_window(
        &self,
        scope: LobbyScope,
        player_id: AppUserId,
    ) -> Result<Option<String>, InteractionHandlerError> {
        let curfew = self.state.curfew.clone();
        let signed_guild_id = scope.guild_id.0;
        let signed_user_id = player_id.0;
        // Both reads are blocking SQLite and this runs on every join surface,
        // including the raw-reaction handler that has no interaction deferral
        // behind it — keep the whole lookup inside the one blocking task.
        let resolved = tokio::task::spawn_blocking(move || {
            let active_window =
                curfew.active_window(signed_user_id, signed_guild_id, chrono::Utc::now())?;
            let general_timezone = match active_window {
                Some(_) => curfew
                    .general_timezone(signed_user_id, signed_guild_id)
                    .unwrap_or(None),
                None => None,
            };
            Ok::<_, cama_app::curfew_service::CurfewServiceError>((active_window, general_timezone))
        })
        .await
        .map_err(|error| format!("curfew lookup task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let (active_window, general_timezone) = resolved;
        let Some(window) = active_window else {
            return Ok(None);
        };
        Ok(Some(cama_domain::curfew::format_window(
            &window,
            general_timezone.as_deref(),
        )))
    }

    async fn join_registered_player(
        &self,
        scope: LobbyScope,
        player_id: AppUserId,
        player: &cama_domain::player::Player,
    ) -> Result<JoinPresentation, InteractionHandlerError> {
        if player.preferred_roles.as_ref().is_none_or(Vec::is_empty) {
            return Ok(JoinPresentation {
                joined: false,
                warning: Some(
                    "⚠️ Set your preferred roles with `/player roles` to auto-join.".to_owned(),
                ),
                rejection: None,
                joined_at_ns: None,
            });
        }
        if let Some(window_description) = self.active_curfew_window(scope, player_id).await? {
            return Ok(JoinPresentation {
                joined: false,
                warning: Some(format!(
                    "❌ You're inside your {window_description} curfew window. Use `/player curfew remove` if you'd rather queue through it."
                )),
                rejection: Some(JoinRejection::Curfew(window_description)),
                joined_at_ns: None,
            });
        }
        if self.state.service.get_lobby(scope).is_none() {
            return Ok(JoinPresentation {
                joined: false,
                warning: Some(format!("⚠️ No active {} lobby.", scope.kind.label())),
                rejection: None,
                joined_at_ns: None,
            });
        }
        let state = Arc::clone(&self.state);
        let outcome =
            tokio::task::spawn_blocking(move || state.service.join_lobby(player_id, scope))
                .await
                .map_err(|error| error.to_string())?;
        if outcome.success {
            let joined_at_ns = outcome
                .join_commit()
                .expect("successful lobby join has a durable commit")
                .joined_at_ns;
            self.state.sync_ready_lobby(scope);
            return Ok(JoinPresentation {
                joined: true,
                warning: None,
                rejection: None,
                joined_at_ns: Some(joined_at_ns),
            });
        }
        let (warning, rejection) = match (outcome.failure, outcome.context) {
            (Some(JoinFailure::InPendingMatch), Some(JoinContext::PendingMatch(pending))) => {
                let mut message = format!(
                    "❌ You're already in a pending match (Match #{})!",
                    pending.pending_match_id
                );
                if let Some(jump_url) = &pending.shuffle_message_jump_url {
                    message.push_str(&format!(
                        " [View your match]({jump_url}) and use `/record` to complete it first."
                    ));
                } else {
                    message.push_str(" Use `/record` to complete it first.");
                }
                (message, JoinRejection::PendingMatch(pending))
            }
            (Some(JoinFailure::LobbyFull), _) => (
                format!("❌ {} is full.", scope.kind.label()),
                JoinRejection::LobbyFull,
            ),
            (Some(JoinFailure::RatingTooHigh), _) => (
                "❌ Whine & Cheese is limited to players below 1400 Glicko.".to_owned(),
                JoinRejection::RatingTooHigh,
            ),
            (Some(JoinFailure::InFlight), _) => (
                "❌ You can’t switch lobbies while your current shuffle or draft is in progress."
                    .to_owned(),
                JoinRejection::InFlight,
            ),
            (Some(JoinFailure::Suspended), Some(JoinContext::Suspension(suspension))) => (
                format!("❌ {}", private_suspension_message(&suspension)),
                JoinRejection::Suspended(*suspension),
            ),
            (
                Some(
                    JoinFailure::Persistence
                    | JoinFailure::PendingMatchGuard
                    | JoinFailure::ModerationGuard,
                ),
                _,
            ) => (
                "❌ Could not verify or persist lobby membership. Please try again.".to_owned(),
                JoinRejection::Storage,
            ),
            _ => (
                format!(
                    "❌ Already in {}, or that lobby is closed.",
                    scope.kind.label()
                ),
                JoinRejection::AlreadyJoined,
            ),
        };
        Ok(JoinPresentation {
            joined: false,
            warning: Some(warning),
            rejection: Some(rejection),
            joined_at_ns: None,
        })
    }

    async fn best_effort_join_publication(
        &self,
        scope: LobbyScope,
        player_id: AppUserId,
        player_name: &str,
        display_name: &str,
        thread_activity: JoinThreadActivity,
        joined_at_ns: i64,
    ) -> Option<ConfirmedLobbyJoin> {
        if let Err(error) = self.state.sync_lobby_display(scope).await {
            warn!(%error, ?scope, "lobby display sync after join failed");
        }
        self.sync_readycheck_with_lobby(scope).await;
        let lobby = self.state.service.get_lobby(scope)?;
        if let Some(thread_id) = lobby.message_ids.thread_id
            && let Some(content) = match thread_activity {
                JoinThreadActivity::Command => {
                    Some(format!("✅ <@{}> joined the lobby!", player_id.0))
                }
                JoinThreadActivity::RawReaction => Some(format!(
                    "✅ <@{}> joined {}!",
                    player_id.0,
                    scope.kind.label()
                )),
                JoinThreadActivity::Silent => None,
            }
        {
            let thread_id = to_u64(thread_id.0).ok();
            let player_snowflake = u64::try_from(player_id.0).ok();
            if let (Some(thread_id), Some(player_snowflake)) = (thread_id, player_snowflake) {
                if let Err(error) = self
                    .state
                    .transport
                    .send_message(
                        thread_id,
                        DiscordMessage::mentioning(
                            InteractionResponse::message(content)
                                .with_user_mentions(vec![player_snowflake]),
                            BTreeSet::from([player_snowflake]),
                        ),
                    )
                    .await
                {
                    warn!(%error, ?scope, "lobby thread join publication failed");
                }
            } else {
                warn!(
                    ?scope,
                    "lobby join publication has invalid persisted Discord IDs"
                );
            }
        } else if matches!(thread_activity, JoinThreadActivity::Silent)
            && let Some(thread_id) = lobby.message_ids.thread_id
            && let (Ok(thread_id), Ok(player_snowflake)) =
                (to_u64(thread_id.0), u64::try_from(player_id.0))
            && let Err(error) = self
                .state
                .transport
                .add_thread_member(thread_id, player_snowflake)
                .await
        {
            debug!(%error, ?scope, "could not subscribe slash-command joiner to lobby thread");
        }

        let (Ok(guild_id), Ok(player_snowflake)) =
            (u64::try_from(scope.guild_id.0), u64::try_from(player_id.0))
        else {
            warn!(?scope, "lobby observer projection has invalid Discord IDs");
            return None;
        };
        let event = ConfirmedLobbyJoin {
            guild_id,
            player_id: player_snowflake,
            player_name: player_name.to_owned(),
            player_display_name: display_name.to_owned(),
            lobby_kind: scope.kind,
            joined_at_ns,
            player_ids: lobby
                .players
                .iter()
                .filter_map(|player| u64::try_from(player.0).ok())
                .collect(),
            ready_threshold: self.state.config.ready_threshold,
            lobby_channel_id: lobby
                .message_ids
                .channel_id
                .and_then(|id| u64::try_from(id.0).ok()),
            lobby_message_id: lobby
                .message_ids
                .message_id
                .and_then(|id| u64::try_from(id.0).ok()),
            origin_channel_id: lobby
                .message_ids
                .origin_channel_id
                .and_then(|id| u64::try_from(id.0).ok()),
            thread_id: lobby
                .message_ids
                .thread_id
                .and_then(|id| u64::try_from(id.0).ok()),
        };
        if let Some(observer) = self
            .state
            .join_observer
            .read()
            .ok()
            .and_then(|observer| observer.clone())
            && let Err(error) = observer.confirmed_lobby_join(event.clone()).await
        {
            warn!(%error, ?scope, "lobby join observer failed");
        }
        Some(event)
    }

    async fn best_effort_explicit_lobby_join_neon(&self, event: ConfirmedLobbyJoin) {
        let observer = self
            .state
            .join_observer
            .read()
            .ok()
            .and_then(|observer| observer.clone());
        if let Some(observer) = observer
            && let Err(error) = observer.explicit_lobby_join_neon(event).await
        {
            debug!(%error, "explicit lobby-join Neon hook failed");
        }
    }

    async fn best_effort_leave_publication(
        &self,
        scope: LobbyScope,
        player_id: AppUserId,
        display_name: &str,
        raw_reaction: bool,
    ) {
        if let Err(error) = self.state.sync_lobby_display(scope).await {
            warn!(%error, ?scope, "lobby display sync after leave failed");
        }
        self.sync_readycheck_with_lobby(scope).await;
        if let Err(error) = self.state.remove_lobby_reaction(scope, player_id).await {
            debug!(%error, ?scope, "lobby reaction cleanup after leave failed");
        }
        let Some(thread_id) = self
            .state
            .service
            .get_lobby(scope)
            .and_then(|lobby| lobby.message_ids.thread_id)
        else {
            return;
        };
        if let Ok(thread_id) = to_u64(thread_id.0) {
            let content = if raw_reaction {
                format!("🚪 {display_name} left {}.", scope.kind.label())
            } else {
                format!("🚪 **{display_name}** left the lobby.")
            };
            let _ = self
                .state
                .transport
                .send_message(
                    thread_id,
                    DiscordMessage::silent(InteractionResponse::message(content)),
                )
                .await;
        }
    }
}

struct JoinPresentation {
    joined: bool,
    warning: Option<String>,
    rejection: Option<JoinRejection>,
    joined_at_ns: Option<i64>,
}

#[derive(Clone, Copy)]
enum JoinThreadActivity {
    Command,
    RawReaction,
    Silent,
}

enum JoinRejection {
    PendingMatch(PendingMatchState),
    LobbyFull,
    AlreadyJoined,
    RatingTooHigh,
    InFlight,
    Suspended(LobbySuspension),
    Curfew(String),
    Storage,
}

fn lobby_command_auto_join_warning(
    kind: LobbyKind,
    presentation: &JoinPresentation,
) -> Option<String> {
    match presentation.rejection.as_ref() {
        Some(JoinRejection::RatingTooHigh) => Some(format!(
            "ℹ️ You can view {}, but only players below 1400 Glicko can join.",
            kind.label()
        )),
        Some(JoinRejection::InFlight) => Some(
            "ℹ️ You can’t switch lobbies while your current shuffle or draft is in progress."
                .to_owned(),
        ),
        Some(JoinRejection::Suspended(_) | JoinRejection::Curfew(_)) => {
            presentation.warning.clone()
        }
        None => presentation.warning.clone(),
        Some(
            JoinRejection::PendingMatch(_)
            | JoinRejection::LobbyFull
            | JoinRejection::AlreadyJoined
            | JoinRejection::Storage,
        ) => None,
    }
}

struct ReadycheckRunResult {
    jump_url: String,
    is_refresh: bool,
    pruned_count: usize,
    mention_ids: BTreeSet<AppUserId>,
    readycheck_channel_id: AppChannelId,
}

enum ReadycheckRunError {
    Policy(ReadycheckCommandFailure),
    Transport(String),
}

fn readycheck_failure_message(failure: ReadycheckCommandFailure) -> &'static str {
    match failure {
        ReadycheckCommandFailure::NoGuild => "❌ Ready checks can only run in a server.",
        ReadycheckCommandFailure::NoLobby => {
            "❌ Ready check failed — make sure a lobby exists, then try again."
        }
        ReadycheckCommandFailure::NotMember => "❌ Join that lobby before running a ready check.",
        ReadycheckCommandFailure::NoThread => {
            "❌ Lobby thread not found. Use `/lobby` to repair or recreate the lobby."
        }
        ReadycheckCommandFailure::Cooldown { .. } => {
            "⏳ A ready check was just sent. Please wait before trying again."
        }
        ReadycheckCommandFailure::PublicationInFlight => {
            "⏳ A ready check is already being published. Please try again in a moment."
        }
    }
}

fn readycheck_embed(
    kind: LobbyKind,
    player_data: &BTreeMap<AppUserId, ReadycheckPlayerData>,
    reacted: &BTreeMap<AppUserId, String>,
    ready_threshold: usize,
) -> InteractionEmbed {
    let mut active = Vec::new();
    let mut afk = Vec::new();
    for (player_id, player) in player_data {
        if reacted.contains_key(player_id) {
            continue;
        }
        match player.group {
            ReadinessGroup::Ready => active.push(format!("{} 🟢", player.name)),
            ReadinessGroup::PlayingDota => active.push(format!("{} 🎮", player.name)),
            ReadinessGroup::Recent => active.push(format!("{} 🆕", player.name)),
            ReadinessGroup::Afk => afk.push(format!("<@{}> 🔴", player_id.0)),
        }
    }
    let mut embed = InteractionEmbed::titled(format!("{} Ready Check", kind.label()))
        .description(readycheck_description(player_data.len(), ready_threshold))
        .color(0x34_98_db)
        .footer("React with ✅ to confirm you are ready");
    if !active.is_empty() {
        embed = embed.field(
            format!("✅ Likely Active ({})", active.len()),
            active.join("\n"),
            false,
        );
    }
    if !afk.is_empty() {
        embed = embed.field(
            format!("⚠️ Possibly AFK ({})", afk.len()),
            afk.join("\n"),
            false,
        );
    }
    if !reacted.is_empty() {
        embed = embed.field(
            format!("✅ Reacted to Ready Check ({})", reacted.len()),
            reacted.values().cloned().collect::<Vec<_>>().join("\n"),
            false,
        );
    }
    embed
}

fn discord_jump_url(
    guild_id: AppGuildId,
    channel_id: AppChannelId,
    message_id: AppMessageId,
) -> Result<String, ReadycheckRunError> {
    Ok(format!(
        "https://discord.com/channels/{}/{}/{}",
        to_u64(guild_id.0).map_err(ReadycheckRunError::Transport)?,
        to_u64(channel_id.0).map_err(ReadycheckRunError::Transport)?,
        to_u64(message_id.0).map_err(ReadycheckRunError::Transport)?
    ))
}

fn unix_time_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

fn guild_channel(command: &CommandContext) -> Option<(AppGuildId, AppChannelId)> {
    Some((command.guild_id?, command.channel_id?))
}

fn selected_lobby_kind(options: &[InteractionOption]) -> Option<LobbyKind> {
    options.iter().find_map(|option| {
        (option.name == "lobby")
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::String(value) => parse_lobby_kind(value),
                _ => None,
            })
    })
}

fn selected_string<'a>(options: &'a [InteractionOption], name: &str) -> Option<&'a str> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::String(value) => Some(value.as_str()),
                _ => None,
            })
    })
}

fn selected_user(
    options: &[InteractionOption],
    name: &str,
) -> Result<Option<(AppUserId, String)>, InteractionHandlerError> {
    options
        .iter()
        .find_map(|option| (option.name == name).then_some(&option.value))
        .map(|value| match value {
            InteractionValue::User {
                id, display_name, ..
            } => Ok((
                AppUserId(i64::try_from(*id).map_err(|_| {
                    InteractionHandlerError::Transformer {
                        value: id.to_string(),
                    }
                })?),
                display_name.clone().unwrap_or_else(|| format!("User {id}")),
            )),
            _ => Err(InteractionHandlerError::Transformer {
                value: format!("{value:?}"),
            }),
        })
        .transpose()
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), InteractionHandlerError> {
    responder
        .followup(
            InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string().into())
}

struct InteractionLobbyTransport {
    state: Arc<LobbyRuntimeState>,
    responder: Arc<dyn InteractionResponder>,
}

#[async_trait]
impl LobbyRuntimeTransport for InteractionLobbyTransport {
    async fn sync_lobby_display(&self, scope: LobbyScope) -> Result<(), String> {
        self.state.sync_lobby_display(scope).await
    }

    async fn remove_lobby_reaction(
        &self,
        scope: LobbyScope,
        player_id: AppUserId,
    ) -> Result<(), String> {
        self.state.remove_lobby_reaction(scope, player_id).await
    }

    async fn send_thread_message(
        &self,
        thread_id: AppChannelId,
        content: &str,
        allowed_mentions: AllowedMentions,
    ) -> Result<(), String> {
        let message = match allowed_mentions {
            AllowedMentions::None => DiscordMessage::silent(InteractionResponse::message(content)),
            AllowedMentions::Default => DiscordMessage {
                response: InteractionResponse::message(content),
                allowed_mentions: DiscordAllowedMentions::Users(BTreeSet::new()),
                content_mode: crate::discord_transport::DiscordMessageContentMode::Replace,
            },
        };
        self.state
            .transport
            .send_message(to_u64(thread_id.0)?, message)
            .await
            .map(|_| ())
    }

    async fn close_lobby_message(
        &self,
        scope: LobbyScope,
        message_ids: &LobbyMessageIds,
        reason: &str,
    ) -> Result<(), String> {
        let (Some(channel_id), Some(message_id)) = (message_ids.channel_id, message_ids.message_id)
        else {
            return Ok(());
        };
        self.state
            .transport
            .edit_message(
                to_u64(channel_id.0)?,
                to_u64(message_id.0)?,
                DiscordMessage::silent(
                    InteractionResponse::message("").embed(
                        InteractionEmbed::titled(format!("{} — {reason}", scope.kind.label()))
                            .description("This lobby is closed.")
                            .color(0xe7_4c_3c),
                    ),
                )
                .preserving_content(),
            )
            .await
    }

    async fn archive_lobby_thread(
        &self,
        scope: LobbyScope,
        thread_id: AppChannelId,
        reason: &str,
    ) -> Result<(), String> {
        self.state
            .transport
            .archive_thread(
                to_u64(thread_id.0)?,
                &format!("{} — {reason}", scope.kind.label()),
                true,
            )
            .await
    }

    async fn unpin_lobby_message(
        &self,
        _scope: LobbyScope,
        channel_id: Option<AppChannelId>,
        message_id: Option<AppMessageId>,
        fallback_channel_id: AppChannelId,
    ) -> Result<(), String> {
        let Some(message_id) = message_id else {
            return Ok(());
        };
        self.state
            .transport
            .unpin_message(
                to_u64(channel_id.unwrap_or(fallback_channel_id).0)?,
                to_u64(message_id.0)?,
            )
            .await
    }

    async fn respond_ephemerally(
        &self,
        content: &str,
        allowed_mentions: AllowedMentions,
    ) -> Result<(), String> {
        let response = match allowed_mentions {
            AllowedMentions::None => InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
            AllowedMentions::Default => InteractionResponse::message(content).ephemeral(),
        };
        self.responder
            .followup(response)
            .await
            .map_err(|error| error.to_string())
    }
}

struct LobbyGatewayObserver {
    state: Arc<LobbyRuntimeState>,
}

#[async_trait]
impl GatewayEventObserver for LobbyGatewayObserver {
    fn name(&self) -> &'static str {
        "lobby-message-reconciliation"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let guilds = context.guild_ids().iter().copied().collect::<BTreeSet<_>>();
        let mut pending = self
            .state
            .service
            .open_lobbies()
            .into_iter()
            .filter(|lobby| {
                u64::try_from(lobby.scope.guild_id.0)
                    .ok()
                    .is_some_and(|guild_id| guilds.contains(&guild_id))
                    && lobby.message_ids.channel_id.is_some()
                    && lobby.message_ids.message_id.is_some()
            })
            .collect::<Vec<_>>();
        let attempted_guilds = pending
            .iter()
            .filter_map(|lobby| u64::try_from(lobby.scope.guild_id.0).ok())
            .collect::<BTreeSet<_>>();
        let mut refreshed_guilds = BTreeSet::new();
        let mut final_errors = BTreeMap::new();
        for attempt in 0..RECONCILE_ATTEMPTS {
            let mut failed = Vec::new();
            for lobby in pending {
                let channel_id = lobby.message_ids.channel_id.expect("filtered channel id");
                let message_id = lobby.message_ids.message_id.expect("filtered message id");
                let result = async {
                    let message = self
                        .state
                        .transport
                        .fetch_message(to_u64(channel_id.0)?, to_u64(message_id.0)?)
                        .await?
                        .ok_or_else(|| "persisted lobby message was not found".to_owned())?;
                    self.state.sync_lobby_display(lobby.scope).await?;
                    if message
                        .reactions
                        .iter()
                        .any(|emoji| emoji.id == Some(LEGACY_FROGLING_EMOJI_ID))
                    {
                        self.state
                            .transport
                            .clear_reaction(
                                to_u64(channel_id.0)?,
                                to_u64(message_id.0)?,
                                &DiscordEmoji {
                                    name: "frogling".to_owned(),
                                    id: Some(LEGACY_FROGLING_EMOJI_ID),
                                },
                            )
                            .await?;
                    }
                    Ok::<(), String>(())
                }
                .await;
                match result {
                    Ok(()) => {
                        if let Ok(guild_id) = u64::try_from(lobby.scope.guild_id.0) {
                            refreshed_guilds.insert(guild_id);
                        }
                        final_errors.remove(&lobby.scope);
                    }
                    Err(error) => {
                        final_errors.insert(lobby.scope, error);
                        failed.push(lobby);
                    }
                }
            }
            if failed.is_empty() {
                break;
            }
            pending = failed;
            if attempt + 1 < RECONCILE_ATTEMPTS {
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
            }
        }
        let failures = final_errors
            .into_iter()
            .map(|(scope, message)| ReadyRecoveryFailure {
                guild_id: u64::try_from(scope.guild_id.0).unwrap_or_default(),
                message: format!("{}: {message}", lobby_kind_value(scope.kind)),
            })
            .collect();
        ReadyRecoveryReport {
            observer: self.name(),
            guilds_attempted: attempted_guilds.len(),
            guilds_refreshed: refreshed_guilds.len(),
            guilds_superseded: 0,
            members_refreshed: 0,
            failures,
        }
    }
}

struct LobbyRawReactionObserver {
    state: Arc<LobbyRuntimeState>,
}

#[async_trait]
impl RawReactionObserver for LobbyRawReactionObserver {
    fn name(&self) -> &'static str {
        "lobby-reactions"
    }

    async fn observe(&self, event: RawReactionEvent) -> Result<(), String> {
        let handler = LobbyInteractionHandler {
            state: Arc::clone(&self.state),
        };
        if event.emoji.name == READY_EMOJI {
            let Some(scope) = self
                .state
                .scope_for_readycheck_message(event.guild_id, event.message_id)
            else {
                return Ok(());
            };
            let player_id = AppUserId(
                i64::try_from(event.user_id)
                    .map_err(|_| "raw reaction user id overflow".to_owned())?,
            );
            let message_id = AppMessageId(
                i64::try_from(event.message_id)
                    .map_err(|_| "raw reaction message id overflow".to_owned())?,
            );
            let changed = match event.kind {
                RawReactionKind::Add => self.state.readychecks.add_readycheck_reaction(
                    scope,
                    player_id,
                    format!("<@{}>", player_id.0),
                    Some(message_id),
                ),
                RawReactionKind::Remove => self.state.readychecks.remove_readycheck_reaction(
                    scope,
                    player_id,
                    Some(message_id),
                ),
            };
            if changed && let Some(generation) = self.state.readychecks.readycheck_generation(scope)
            {
                let embed = readycheck_embed(
                    scope.kind,
                    &generation.player_data,
                    &generation.reacted,
                    self.state.config.ready_threshold,
                );
                if let Err(error) = self
                    .state
                    .transport
                    .edit_message(
                        event.channel_id,
                        event.message_id,
                        DiscordMessage::silent(InteractionResponse::message("").embed(embed))
                            .preserving_content(),
                    )
                    .await
                {
                    warn!(%error, ?scope, "readycheck reaction display refresh failed");
                }
                handler
                    .notify_readycheck_completion(scope, message_id)
                    .await;
            }
            return Ok(());
        }

        let Some(scope) = self
            .state
            .scope_for_lobby_message(event.guild_id, event.message_id)
        else {
            return Ok(());
        };
        if event.emoji.id == Some(LEGACY_FROGLING_EMOJI_ID) {
            if event.kind == RawReactionKind::Add {
                let _ = self
                    .state
                    .transport
                    .remove_reaction(
                        event.channel_id,
                        event.message_id,
                        &DiscordEmoji {
                            name: event.emoji.name,
                            id: event.emoji.id,
                        },
                        event.user_id,
                    )
                    .await;
            }
            return Ok(());
        }
        if event.emoji.name == BELL_EMOJI && event.kind == RawReactionKind::Add {
            let user_id = AppUserId(
                i64::try_from(event.user_id)
                    .map_err(|_| "raw reaction user id overflow".to_owned())?,
            );
            let channel_id = self
                .state
                .service
                .get_lobby(scope)
                .and_then(|lobby| lobby.message_ids.origin_channel_id)
                .unwrap_or(AppChannelId(
                    i64::try_from(event.channel_id)
                        .map_err(|_| "raw reaction channel id overflow".to_owned())?,
                ));
            if handler
                .run_readycheck(scope, user_id, channel_id, false, true)
                .await
                .is_err()
            {
                let _ = self
                    .state
                    .transport
                    .remove_reaction(
                        event.channel_id,
                        event.message_id,
                        &DiscordEmoji::unicode(BELL_EMOJI),
                        event.user_id,
                    )
                    .await;
            }
            return Ok(());
        }
        if (event.emoji.id == Some(JOPACOIN_EMOJI_ID) || event.emoji.name == "jopacoin")
            && event.kind == RawReactionKind::Add
        {
            let user_id = AppUserId(
                i64::try_from(event.user_id)
                    .map_err(|_| "raw reaction user id overflow".to_owned())?,
            );
            let Some(lobby) = self.state.service.get_lobby(scope) else {
                return Ok(());
            };
            if lobby.players.contains(&user_id) {
                return Ok(());
            }
            if let Some(thread_id) = lobby.message_ids.thread_id
                && let Err(error) = self
                    .state
                    .transport
                    .send_message(
                        to_u64(thread_id.0)?,
                        DiscordMessage::mentioning(
                            InteractionResponse::message(format!(
                                "{JOPACOIN_EMOTE} <@{}> is here for the gamba!",
                                user_id.0
                            )),
                            BTreeSet::from([event.user_id]),
                        ),
                    )
                    .await
            {
                warn!(%error, ?scope, "gamba thread subscription failed");
            }
            let observer = self
                .state
                .join_observer
                .read()
                .ok()
                .and_then(|observer| observer.clone());
            if let Some(observer) = observer {
                let display_name = self
                    .resolve_actor_display_name(&event)
                    .await
                    .unwrap_or_else(|| "Lobby spectator".to_owned());
                if let (Ok(guild_id), Ok(player_id)) =
                    (u64::try_from(scope.guild_id.0), u64::try_from(user_id.0))
                    && let Err(error) = observer
                        .gamba_spectator(LobbyGambaSpectator {
                            guild_id,
                            player_id,
                            player_display_name: display_name,
                            channel_id: event.channel_id,
                        })
                        .await
                {
                    debug!(%error, "gamba spectator Neon hook failed");
                }
            }
            return Ok(());
        }
        if event.emoji.name != SWORD_EMOJI {
            return Ok(());
        }
        let user_id = AppUserId(
            i64::try_from(event.user_id).map_err(|_| "raw reaction user id overflow".to_owned())?,
        );
        let resolved_display_name = self.resolve_actor_display_name(&event).await;
        let operation_lock = self.state.commands.scope_operation_lock(scope);
        let _guard = operation_lock.lock().await;
        match event.kind {
            RawReactionKind::Add => {
                let Some(player) = handler
                    .load_player(user_id, scope.guild_id)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    self.reject_sword_reaction(
                        &event,
                        "You're not registered! Use `/player register` first to join the lobby.",
                        RAW_REJECTION_TTL,
                    )
                    .await;
                    return Ok(());
                };
                if player.preferred_roles.as_ref().is_none_or(Vec::is_empty) {
                    self.reject_sword_reaction(
                        &event,
                        "Set your preferred roles first! Use `/player roles` (e.g., `/player roles 123`).",
                        RAW_REJECTION_TTL,
                    )
                    .await;
                    return Ok(());
                }
                let outcome = handler
                    .join_registered_player(scope, user_id, &player)
                    .await
                    .map_err(|error| error.to_string())?;
                if !outcome.joined {
                    if let Some(JoinRejection::Suspended(suspension)) = &outcome.rejection {
                        self.reject_suspended_sword_reaction(&event, suspension)
                            .await;
                        return Ok(());
                    }
                    if let Some(JoinRejection::Curfew(window_description)) = &outcome.rejection {
                        self.reject_curfew_sword_reaction(&event, window_description)
                            .await;
                        return Ok(());
                    }
                    let rejection = outcome.rejection.as_ref().map_or_else(
                        || "Could not join lobby.".to_owned(),
                        |rejection| raw_rejection_message(scope.kind, rejection),
                    );
                    let ttl = if matches!(outcome.rejection, Some(JoinRejection::PendingMatch(_))) {
                        RAW_LONG_REJECTION_TTL
                    } else {
                        RAW_REJECTION_TTL
                    };
                    self.reject_sword_reaction(&event, &rejection, ttl).await;
                    return Ok(());
                }
                handler
                    .best_effort_join_publication(
                        scope,
                        user_id,
                        &player.name,
                        resolved_display_name.as_deref().unwrap_or(&player.name),
                        JoinThreadActivity::RawReaction,
                        outcome
                            .joined_at_ns
                            .expect("successful join has commit time"),
                    )
                    .await;
            }
            RawReactionKind::Remove => {
                let state = Arc::clone(&self.state);
                let left = tokio::task::spawn_blocking(move || {
                    state.service.try_leave_lobby(user_id, scope)
                })
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
                if left {
                    let display_name = resolved_display_name.unwrap_or_else(|| {
                        self.state.players.fallback_name(user_id, scope.guild_id)
                    });
                    handler
                        .best_effort_leave_publication(scope, user_id, &display_name, true)
                        .await;
                }
            }
        }
        Ok(())
    }
}

impl LobbyRawReactionObserver {
    async fn resolve_actor_display_name(&self, event: &RawReactionEvent) -> Option<String> {
        if let Some(display_name) = &event.actor_display_name {
            return Some(display_name.clone());
        }
        let guild_id = event.guild_id?;
        if let Some(member) = self
            .state
            .transport
            .guild_member(guild_id, event.user_id)
            .await
            .ok()
            .flatten()
        {
            return Some(member.display_name);
        }
        self.state
            .transport
            .user(event.user_id)
            .await
            .ok()
            .flatten()
            .map(|user| user.display_name)
    }

    async fn reject_sword_reaction(&self, event: &RawReactionEvent, reason: &str, ttl: Duration) {
        let _ = self
            .state
            .transport
            .remove_reaction(
                event.channel_id,
                event.message_id,
                &DiscordEmoji::unicode(SWORD_EMOJI),
                event.user_id,
            )
            .await;
        let users = BTreeSet::from([event.user_id]);
        let receipt = self
            .state
            .transport
            .send_message(
                event.channel_id,
                DiscordMessage::mentioning(
                    InteractionResponse::message(format!("<@{}> ❌ {reason}", event.user_id)),
                    users,
                ),
            )
            .await;
        if let Ok(receipt) = receipt {
            schedule_message_delete(Arc::clone(&self.state.transport), receipt, ttl);
        }
    }

    async fn reject_suspended_sword_reaction(
        &self,
        event: &RawReactionEvent,
        suspension: &LobbySuspension,
    ) {
        self.reject_sword_reaction(
            event,
            "You are temporarily restricted from this matchmaking lobby. Check your DMs or use `/player lobby status`.",
            RAW_LONG_REJECTION_TTL,
        )
        .await;
        let private_reason = &suspension.reason;
        let _ = self
            .state
            .transport
            .send_direct_message(
                event.user_id,
                DiscordMessage::silent(
                    InteractionResponse::message(format!(
                        "You are temporarily suspended from this matchmaking lobby.\nReason: {private_reason}\nUse `/player lobby status` in the server for the exact remaining term."
                    ))
                    .without_mentions(),
                ),
            )
            .await;
    }

    /// Curfew window names, times, and timezones are private. Reject publicly
    /// in generic terms and send the specifics by DM.
    async fn reject_curfew_sword_reaction(
        &self,
        event: &RawReactionEvent,
        window_description: &str,
    ) {
        self.reject_sword_reaction(
            event,
            &raw_rejection_message(
                LobbyKind::Open,
                &JoinRejection::Curfew(window_description.to_owned()),
            ),
            RAW_REJECTION_TTL,
        )
        .await;
        let _ = self
            .state
            .transport
            .send_direct_message(
                event.user_id,
                DiscordMessage::silent(
                    InteractionResponse::message(format!(
                        "You're inside your {window_description} curfew window, so you weren't added to the lobby.\nUse `/player curfew remove` if you'd rather queue through it."
                    ))
                    .without_mentions(),
                ),
            )
            .await;
    }
}

fn raw_rejection_message(kind: LobbyKind, rejection: &JoinRejection) -> String {
    match rejection {
        JoinRejection::PendingMatch(pending) => {
            let mut message = format!(
                "You're in a pending match (Match #{})!",
                pending.pending_match_id
            );
            if let Some(jump_url) = &pending.shuffle_message_jump_url {
                message.push_str(&format!(
                    " [View your match]({jump_url}) and use `/record` to complete it first."
                ));
            } else {
                message.push_str(" Use `/record` to complete it first.");
            }
            message
        }
        JoinRejection::LobbyFull => "Lobby is full.".to_owned(),
        JoinRejection::AlreadyJoined => "Already in lobby.".to_owned(),
        JoinRejection::RatingTooHigh => format!(
            "{} is limited to glicko below 1400.",
            LobbyKind::LowSkill.label()
        ),
        JoinRejection::InFlight => {
            "You can't switch lobbies while your current shuffle or draft is in progress."
                .to_owned()
        }
        JoinRejection::Suspended(_) => {
            "You are temporarily restricted from this matchmaking lobby.".to_owned()
        }
        // A window's name, exact times, and timezone are private to the
        // player. This message is posted publicly in the channel, so it stays
        // generic and the details go out by DM — the same split the
        // suspension path uses.
        JoinRejection::Curfew(_) => {
            "You're inside one of your curfew windows. Check your DMs, or use `/player curfew list`."
                .to_owned()
        }
        JoinRejection::Storage => {
            let _ = kind;
            "Could not join lobby.".to_owned()
        }
    }
}

fn schedule_message_delete(
    transport: Arc<dyn DiscordTransport>,
    receipt: crate::discord_transport::DiscordMessageReceipt,
    delay: Duration,
) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if let Err(error) = transport
            .delete_message(receipt.channel_id, receipt.message_id)
            .await
        {
            debug!(%error, "temporary lobby message cleanup failed");
        }
    });
}

#[cfg(test)]
#[path = "lobby_provider/tests.rs"]
mod tests;
