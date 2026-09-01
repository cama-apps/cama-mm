//! Live runtime adapter for the rare bonuses awarded after a successful Dig.
//!
//! The application crate owns the bonus policy and the session state machines
//! (`cama_app::dig_bonus_events`).  This module supplies the runtime pieces
//! that policy deliberately does not know about: a Discord member snapshot,
//! follow-up/edit delivery, the existing betting wheel, and the SQLite
//! repositories.  Package and trivia views are intentionally process-local,
//! matching `discord.py`'s non-persistent `discord.ui.View` behavior.  The
//! balance/deal writes remain durable before a message edit is attempted.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::dig_bonus_events::{
    DigBonus, DigTriviaSession, GuildMember, MessageUpdate, PackageBonusPresentation,
    PackageDealSession, TRIVIA_CORRECT_GROSS_JC, prepare_package_bonus, prepare_trivia_bonus,
};
use cama_app::dig_loot::{LootEntropy, SeededLootEntropy};
use cama_app::economy_event_service::EconomyEventConfig;
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::trivia_questions::{TriviaCatalog, TriviaRandom, generate_question};
use cama_db::dig_bonus_events_repository::DigBonusRepository;
use cama_db::package_deal_repository::PackageDealRepository;
use chrono::Utc;
use thiserror::Error;

use crate::application_config::ApplicationConfig;
use crate::betting_provider::BettingRegistrationProvider;
use crate::discord_transport::{DiscordIdPlayerNameResolver, GuildPlayerNameResolver};
use crate::economy_events_worker::EconomyEventsWorkerConfig;
use crate::registration::{
    ComponentRoute, InteractionActionRow, InteractionButton, InteractionButtonStyle,
    InteractionEmbed, InteractionEmbedField, InteractionHandler, InteractionHandlerError,
    InteractionMessageReceipt, InteractionRequest, InteractionResponder, InteractionResponse,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};
use crate::runtime_ports::DigBonusDispatchPort;

/// Component prefix owned by this module.  The composition root should add a
/// route for this prefix after constructing [`DigBonusRuntime`].
pub const DIG_BONUS_COMPONENT_PREFIX: &str = "dig_bonus:";
pub const DEFAULT_PACKAGE_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_TRIVIA_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_TRIVIA_GENERATION_RETRIES: usize = 15;

/// Configuration that belongs to the process-local bonus views.  Trivia's
/// answer timeout follows the existing application config; package offers use
/// Python's fixed one-minute view timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigBonusRuntimeConfig {
    pub package_timeout: Duration,
    pub trivia_timeout: Duration,
    pub trivia_generation_retries: usize,
    /// Server-side secret mixed into the package-bonus draw.
    ///
    /// The action id it would otherwise be seeded from is printed verbatim in
    /// the button the player clicks, so the draw would be computable offline.
    /// Zero keeps the legacy seed, which is what the tests pin.
    pub entropy_secret: u64,
}

impl Default for DigBonusRuntimeConfig {
    fn default() -> Self {
        Self {
            package_timeout: DEFAULT_PACKAGE_TIMEOUT,
            trivia_timeout: DEFAULT_TRIVIA_TIMEOUT,
            trivia_generation_retries: DEFAULT_TRIVIA_GENERATION_RETRIES,
            entropy_secret: 0,
        }
    }
}

impl DigBonusRuntimeConfig {
    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        Self {
            trivia_timeout: Duration::from_secs(
                u64::try_from(config.values.trivia_answer_timeout_seconds.max(0))
                    .unwrap_or(u64::MAX),
            ),
            // Production shares the dig runtime's per-process secret.
            entropy_secret: cama_app::dig_runtime::DigRuntimeConfig::production().entropy_secret,
            ..Self::default()
        }
    }
}

/// Discord operations required by a bonus view.  Keeping this separate from
/// `DigDiscordPort` lets the eventual Serenity composition expose only member
/// snapshots and message edits, while tests can model the exact interaction
/// receipt/owner boundary without a Discord client.
#[async_trait]
pub trait DigBonusDiscordPort: Send + Sync {
    async fn guild_members(&self, guild_id: i64) -> Result<Vec<GuildMember>, String>;

    async fn send_bonus(
        &self,
        responder: Arc<dyn InteractionResponder>,
        response: InteractionResponse,
    ) -> Result<InteractionMessageReceipt, DigBonusSendFailure>;

    async fn edit_bonus_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), String>;
}

/// Sending a bonus follow-up is the only presentation step that can race a
/// durable wheel/deal/trivia claim.  A definite rejection is safe for the
/// provider to retry; an ambiguous HTTP/network result is terminal for this
/// action and is reported to the user without releasing the action marker.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum DigBonusSendFailure {
    #[error("Discord definitely rejected the bonus message: {0}")]
    Rejected(String),
    #[error("Discord may have accepted the bonus message: {0}")]
    Ambiguous(String),
}

/// Adapter for transports whose responder already knows how to send and edit
/// follow-ups.  Production composition can wrap this with member snapshots
/// from Serenity's guild cache/HTTP API.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResponderDigBonusMessagePort;

#[async_trait]
impl DigBonusDiscordPort for ResponderDigBonusMessagePort {
    async fn guild_members(&self, _guild_id: i64) -> Result<Vec<GuildMember>, String> {
        Err("Dig bonus member snapshots are not configured".to_owned())
    }

    async fn send_bonus(
        &self,
        responder: Arc<dyn InteractionResponder>,
        response: InteractionResponse,
    ) -> Result<InteractionMessageReceipt, DigBonusSendFailure> {
        responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| DigBonusSendFailure::Ambiguous(error.to_string()))?
            .ok_or_else(|| {
                DigBonusSendFailure::Ambiguous(
                    "bonus follow-up did not return a message receipt".to_owned(),
                )
            })
    }

    async fn edit_bonus_message(
        &self,
        _receipt: InteractionMessageReceipt,
        _response: InteractionResponse,
    ) -> Result<(), String> {
        Err(
            "the responder-only Dig bonus port cannot edit a follow-up without a Discord transport"
                .to_owned(),
        )
    }
}

/// Narrow wheel boundary.  The blanket production adapter below calls the
/// existing `BettingRegistrationProvider::trigger_bonus_spin` method, so the
/// wheel continues to own gamba admission, settlement, media, and audit rows.
#[async_trait]
pub trait DigBonusWheelPort: Send + Sync {
    async fn trigger_bonus_spin(
        &self,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), DigBonusWheelFailure>;
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum DigBonusWheelFailure {
    #[error("wheel bonus was definitely rejected: {0}")]
    Rejected(String),
    #[error("wheel bonus may have settled before the transport failed: {0}")]
    Ambiguous(String),
}

#[derive(Clone)]
pub struct BettingDigBonusWheelPort {
    betting: Arc<BettingRegistrationProvider>,
}

impl BettingDigBonusWheelPort {
    #[must_use]
    pub fn new(betting: BettingRegistrationProvider) -> Self {
        Self {
            betting: Arc::new(betting),
        }
    }
}

#[async_trait]
impl DigBonusWheelPort for BettingDigBonusWheelPort {
    async fn trigger_bonus_spin(
        &self,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), DigBonusWheelFailure> {
        self.betting
            .trigger_bonus_spin(
                user_id,
                guild_id,
                Some(u64::try_from(channel_id).map_err(|_| {
                    DigBonusWheelFailure::Rejected(
                        "Dig bonus channel ID is outside Discord's unsigned range".to_owned(),
                    )
                })?),
                responder,
            )
            .await
            .map_err(DigBonusWheelFailure::Ambiguous)
    }
}

/// Source for the event-adjusted Dig reward used by trivia. Python applies the
/// daily economy event to the fixed 15-JC trivia gross at bonus presentation
/// time (not when the Dig itself committed). The runtime captures one clock
/// value per presentation and freezes the resulting amount in the session.
pub trait DigBonusRewardSource: Send + Sync {
    fn event_adjusted_gross_jc(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
    ) -> Result<i64, String>;

    /// Apply the event policy at the instant the bonus is presented.  The
    /// default keeps lightweight test sources/source-compatible adapters
    /// useful while SQLite overrides this to honor the supplied boundary.
    fn event_adjusted_gross_jc_at(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        _dispatch_timestamp: i64,
    ) -> Result<i64, String> {
        self.event_adjusted_gross_jc(action_id, user_id, guild_id)
    }
}

#[derive(Clone)]
pub struct SqliteDigBonusRewardSource {
    actions: DigBonusRepository,
    economy_events: Arc<SqliteEconomyEventService>,
}

impl SqliteDigBonusRewardSource {
    #[must_use]
    pub fn new(path: impl AsRef<Path>, config: EconomyEventConfig) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            economy_events: Arc::new(SqliteEconomyEventService::new(&path, config)),
            actions: DigBonusRepository::new(&path),
        }
    }

    #[must_use]
    pub fn from_application_config(path: impl AsRef<Path>, config: &ApplicationConfig) -> Self {
        Self::new(
            path,
            EconomyEventsWorkerConfig::from_application_config(config).event,
        )
    }
}

impl DigBonusRewardSource for SqliteDigBonusRewardSource {
    fn event_adjusted_gross_jc(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
    ) -> Result<i64, String> {
        self.event_adjusted_gross_jc_at(action_id, user_id, guild_id, Utc::now().timestamp())
    }

    fn event_adjusted_gross_jc_at(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        dispatch_timestamp: i64,
    ) -> Result<i64, String> {
        if !self
            .actions
            .dig_action_exists_for_actor(action_id, user_id, guild_id)
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "Dig action {action_id} was not found for this player"
            ));
        }
        self.economy_events
            .adjust_reward_at(
                guild_id,
                TRIVIA_CORRECT_GROSS_JC,
                dispatch_timestamp,
            )
            .map_err(|error| {
                format!(
                    "failed to apply the daily economy event to Dig trivia action {action_id}: {error}"
                )
            })
    }
}

#[derive(Debug, Error)]
pub enum DigBonusRuntimeError {
    #[error("Dig bonus interaction was malformed: {0}")]
    InvalidInteraction(String),
    #[error("Dig bonus runtime dependency failed: {0}")]
    Dependency(String),
    #[error("Dig bonus session is no longer active")]
    StaleSession,
}

#[derive(Clone, Debug)]
enum LiveBonusSession {
    Package {
        action_id: i64,
        owner_id: i64,
        guild_id: i64,
        receipt: InteractionMessageReceipt,
        view: PackageDealSession,
        expires_at: Instant,
    },
    Trivia {
        owner_id: i64,
        guild_id: i64,
        receipt: InteractionMessageReceipt,
        view: DigTriviaSession,
        expires_at: Instant,
    },
}

impl LiveBonusSession {
    fn owner_id(&self) -> i64 {
        match self {
            Self::Package { owner_id, .. } | Self::Trivia { owner_id, .. } => *owner_id,
        }
    }

    fn guild_id(&self) -> i64 {
        match self {
            Self::Package { guild_id, .. } | Self::Trivia { guild_id, .. } => *guild_id,
        }
    }

    fn receipt(&self) -> InteractionMessageReceipt {
        match self {
            Self::Package { receipt, .. } | Self::Trivia { receipt, .. } => *receipt,
        }
    }

    fn expired(&self, now: Instant) -> bool {
        match self {
            Self::Package { expires_at, .. } | Self::Trivia { expires_at, .. } => {
                *expires_at <= now
            }
        }
    }
}

struct DigBonusRuntimeState {
    economy: DigBonusRepository,
    packages: PackageDealRepository,
    catalog: Arc<TriviaCatalog>,
    wheel: Arc<dyn DigBonusWheelPort>,
    reward_source: Arc<dyn DigBonusRewardSource>,
    discord: Arc<dyn DigBonusDiscordPort>,
    player_names: Arc<dyn GuildPlayerNameResolver>,
    config: DigBonusRuntimeConfig,
    sessions: Mutex<BTreeMap<String, LiveBonusSession>>,
}

/// Runtime owner for all three rare Dig bonus components.
#[derive(Clone)]
pub struct DigBonusRuntime {
    state: Arc<DigBonusRuntimeState>,
}

impl DigBonusRuntime {
    /// Construct the production adapter around the existing betting provider
    /// and the already-composed economy-event policy.
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: DigBonusRuntimeConfig,
        economy_config: EconomyEventConfig,
        catalog: Arc<TriviaCatalog>,
        betting: BettingRegistrationProvider,
        discord: Arc<dyn DigBonusDiscordPort>,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        Self::with_ports(
            database_path.clone(),
            config,
            catalog,
            Arc::new(BettingDigBonusWheelPort::new(betting)),
            Arc::new(SqliteDigBonusRewardSource::new(
                &database_path,
                economy_config,
            )),
            discord,
        )
    }

    /// Named alias for callers that want to make the shared policy argument
    /// especially explicit at the composition site.
    #[must_use]
    pub fn new_with_economy_config(
        database_path: impl AsRef<Path>,
        config: DigBonusRuntimeConfig,
        economy_config: EconomyEventConfig,
        catalog: Arc<TriviaCatalog>,
        betting: BettingRegistrationProvider,
        discord: Arc<dyn DigBonusDiscordPort>,
    ) -> Self {
        Self::new(
            database_path,
            config,
            economy_config,
            catalog,
            betting,
            discord,
        )
    }

    /// Convenience constructor that takes the application config's trivia
    /// timeout while preserving the fixed Python package timeout.
    #[must_use]
    pub fn from_application_config(
        database_path: impl AsRef<Path>,
        app_config: &ApplicationConfig,
        catalog: Arc<TriviaCatalog>,
        betting: BettingRegistrationProvider,
        discord: Arc<dyn DigBonusDiscordPort>,
    ) -> Self {
        Self::new_with_economy_config(
            database_path,
            DigBonusRuntimeConfig::from_application_config(app_config),
            EconomyEventsWorkerConfig::from_application_config(app_config).event,
            catalog,
            betting,
            discord,
        )
    }

    /// Injection seam for focused runtime tests and alternate transports.
    #[must_use]
    pub fn with_ports(
        database_path: impl AsRef<Path>,
        config: DigBonusRuntimeConfig,
        catalog: Arc<TriviaCatalog>,
        wheel: Arc<dyn DigBonusWheelPort>,
        reward_source: Arc<dyn DigBonusRewardSource>,
        discord: Arc<dyn DigBonusDiscordPort>,
    ) -> Self {
        let database_path = database_path.as_ref().to_path_buf();
        Self {
            state: Arc::new(DigBonusRuntimeState {
                economy: DigBonusRepository::new(&database_path),
                packages: PackageDealRepository::new(&database_path),
                catalog,
                wheel,
                reward_source,
                discord,
                player_names: Arc::new(DiscordIdPlayerNameResolver),
                config,
                sessions: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    #[must_use]
    pub fn with_player_names(mut self, player_names: Arc<dyn GuildPlayerNameResolver>) -> Self {
        Arc::get_mut(&mut self.state)
            .expect("new Dig bonus runtime owns its state")
            .player_names = player_names;
        self
    }

    #[must_use]
    pub fn component_prefix(&self) -> &'static str {
        DIG_BONUS_COMPONENT_PREFIX
    }

    /// Return the number of currently live process-local views.
    #[must_use]
    pub fn active_session_count(&self) -> usize {
        self.state
            .sessions
            .lock()
            .map_or(0, |sessions| sessions.len())
    }

    /// Remove expired views and apply trivia's timeout penalty before editing
    /// the stale Discord message.  The Tokio timer calls this same operation;
    /// exposing it makes restart/recovery tests deterministic.
    pub async fn expire_sessions(&self) -> Result<usize, String> {
        self.expire_sessions_at(Instant::now()).await
    }

    async fn expire_sessions_at(&self, now: Instant) -> Result<usize, String> {
        let expired = {
            let mut sessions = self
                .state
                .sessions
                .lock()
                .map_err(|_| "Dig bonus session lock poisoned".to_owned())?;
            let keys = sessions
                .iter()
                .filter(|(_, session)| session.expired(now))
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| sessions.remove(&key).map(|session| (key, session)))
                .collect::<Vec<_>>()
        };
        let count = expired.len();
        for (_key, session) in expired {
            let receipt = session.receipt();
            let response = self.timeout_response(session).await?;
            // A Discord deletion/HTTP 404 is presentation-only after trivia's
            // durable -5 settlement; the caller may log the edit failure.
            self.state
                .discord
                .edit_bonus_message(receipt, response)
                .await?;
        }
        Ok(count)
    }

    async fn timeout_response(
        &self,
        session: LiveBonusSession,
    ) -> Result<InteractionResponse, String> {
        match session {
            LiveBonusSession::Package {
                action_id,
                mut view,
                ..
            } => {
                let update = view.on_timeout().unwrap_or_default();
                Ok(package_timeout_response(action_id, &view, update))
            }
            LiveBonusSession::Trivia { mut view, .. } => {
                let economy = self.state.economy.clone();
                let update = tokio::task::spawn_blocking(move || view.on_timeout(&economy))
                    .await
                    .map_err(|error| format!("Dig trivia timeout task failed: {error}"))?
                    .ok_or_else(|| "Dig trivia timeout session was already resolved".to_owned())?;
                Ok(message_update_response(update))
            }
        }
    }

    fn schedule_timeout(&self, key: String, duration: Duration) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let runtime = Self { state };
            let _ = runtime.expire_session_key(&key, Instant::now()).await;
        });
    }

    async fn expire_session_key(&self, key: &str, now: Instant) -> Result<(), String> {
        let session = {
            let mut sessions = self
                .state
                .sessions
                .lock()
                .map_err(|_| "Dig bonus session lock poisoned".to_owned())?;
            if sessions
                .get(key)
                .is_some_and(|session| session.expired(now))
            {
                sessions.remove(key)
            } else {
                None
            }
        };
        let Some(session) = session else {
            return Ok(());
        };
        let receipt = session.receipt();
        let response = self.timeout_response(session).await?;
        self.state
            .discord
            .edit_bonus_message(receipt, response)
            .await
    }

    /// Route a `dig_bonus:...` component interaction.  Ownership is checked
    /// before a session is removed or a durable trivia/deal operation runs.
    pub async fn handle_component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            custom_id,
            user_id,
            guild_id,
            values,
            ..
        } = request
        else {
            return Err(DigBonusRuntimeError::InvalidInteraction(
                "bonus route received a non-component interaction".to_owned(),
            )
            .to_string());
        };
        let actor_id = signed_id(user_id, "user")?;
        let guild_id = guild_id.map_or(Ok(0), |id| signed_id(id, "guild"))?;
        let parsed = parse_component(&custom_id, &values)?;
        let session = match self.take_session(&parsed.key, actor_id, guild_id)? {
            SessionLookup::Ready(session) => *session,
            SessionLookup::WrongOwner => {
                return respond_ephemeral(&responder, "This isn't your Dig bonus session.").await;
            }
            SessionLookup::Missing | SessionLookup::WrongGuild | SessionLookup::Expired => {
                return respond_ephemeral(&responder, "This bonus is no longer active.").await;
            }
        };
        let update = match (parsed.kind, session) {
            (
                BonusComponentKind::Package(partner_id),
                LiveBonusSession::Package { mut view, .. },
            ) => {
                let packages = self.state.packages.clone();
                tokio::task::spawn_blocking(move || view.select_partner(partner_id, &packages))
                    .await
                    .map_err(|error| format!("Dig package selection task failed: {error}"))?
            }
            (BonusComponentKind::Trivia(choice), LiveBonusSession::Trivia { mut view, .. }) => {
                let economy = self.state.economy.clone();
                tokio::task::spawn_blocking(move || view.answer(choice, &economy))
                    .await
                    .map_err(|error| format!("Dig trivia answer task failed: {error}"))?
            }
            (_, session) => {
                return Err(DigBonusRuntimeError::InvalidInteraction(format!(
                    "component does not match live {} session",
                    session_kind(&session)
                ))
                .to_string());
            }
        };
        // A component interaction must be acknowledged through its own
        // update callback. The stored receipt remains available only for the
        // timeout task, which runs without a live component responder.
        responder
            .update(message_update_response(update))
            .await
            .map_err(|error| error.to_string())
    }

    fn take_session(
        &self,
        key: &str,
        actor_id: i64,
        guild_id: i64,
    ) -> Result<SessionLookup, String> {
        let mut sessions = self
            .state
            .sessions
            .lock()
            .map_err(|_| "Dig bonus session lock poisoned".to_owned())?;
        let Some(session) = sessions.get(key) else {
            return Ok(SessionLookup::Missing);
        };
        if session.owner_id() != actor_id {
            return Ok(SessionLookup::WrongOwner);
        }
        if session.guild_id() != guild_id {
            return Ok(SessionLookup::WrongGuild);
        }
        if session.expired(Instant::now()) {
            sessions.remove(key);
            return Ok(SessionLookup::Expired);
        }
        let session = sessions.remove(key).expect("session existed above");
        Ok(SessionLookup::Ready(Box::new(session)))
    }
}

enum SessionLookup {
    Ready(Box<LiveBonusSession>),
    Missing,
    WrongOwner,
    WrongGuild,
    Expired,
}

#[async_trait]
impl DigBonusDispatchPort for DigBonusRuntime {
    async fn dispatch_bonus(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        channel_id: i64,
        bonus: DigBonus,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        match bonus {
            DigBonus::Wheel => match self
                .state
                .wheel
                .trigger_bonus_spin(user_id, guild_id, channel_id, Arc::clone(&responder))
                .await
            {
                Ok(()) => Ok(()),
                Err(DigBonusWheelFailure::Rejected(error)) => Err(error),
                Err(DigBonusWheelFailure::Ambiguous(error)) => {
                    self.report_ambiguous_delivery(&responder, &error).await;
                    Ok(())
                }
            },
            DigBonus::PackageDeal => {
                self.dispatch_package(action_id, user_id, guild_id, responder)
                    .await
            }
            DigBonus::Trivia => {
                self.dispatch_trivia(action_id, user_id, guild_id, responder)
                    .await
            }
        }
    }

    async fn report_partial_failure(
        &self,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .followup(
                InteractionResponse::message(cama_app::dig_bonus_events::PARTIAL_BONUS_FAILURE)
                    .ephemeral(),
            )
            .await
            .map_err(|error| error.to_string())
    }
}

impl DigBonusRuntime {
    async fn dispatch_package(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let mut members = self.state.discord.guild_members(guild_id).await?;
        let player_ids = members.iter().map(|member| member.id).collect::<Vec<_>>();
        let names = self
            .state
            .player_names
            .names_for_guild(guild_id, &player_ids)
            .unwrap_or_default();
        for member in &mut members {
            member.display_name = names.resolve(member.id);
        }
        let economy = self.state.economy.clone();
        let config = self.state.config;
        let presentation = tokio::task::spawn_blocking(move || {
            let mut entropy =
                SeededLootEntropy::new(action_id.unsigned_abs() ^ config.entropy_secret);
            prepare_package_bonus(&economy, guild_id, user_id, &members, &mut entropy)
        })
        .await
        .map_err(|error| format!("Dig package preparation task failed: {error}"))?
        .map_err(|error| error.to_string())?;

        let (response, session) = match presentation {
            PackageBonusPresentation::InsufficientCandidates(update) => {
                let mut response = message_update_response(update);
                response.ephemeral = true;
                let _ = self.send_definite_or_ambiguous(responder, response).await?;
                return Ok(());
            }
            PackageBonusPresentation::Offer { embed, session } => {
                let response = package_offer_response(action_id, embed, &session);
                (response, session)
            }
        };
        let Some(receipt) = self
            .send_definite_or_ambiguous(Arc::clone(&responder), response)
            .await?
        else {
            return Ok(());
        };
        let key = package_session_key(action_id);
        let session = LiveBonusSession::Package {
            action_id,
            owner_id: user_id,
            guild_id,
            receipt,
            view: session,
            expires_at: Instant::now() + config.package_timeout,
        };
        self.insert_session_or_report(&key, session, &responder)
            .await;
        self.schedule_timeout(key, config.package_timeout);
        Ok(())
    }

    async fn dispatch_trivia(
        &self,
        action_id: i64,
        user_id: i64,
        guild_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let reward_source = Arc::clone(&self.state.reward_source);
        let dispatch_timestamp = Utc::now().timestamp();
        let reward = tokio::task::spawn_blocking(move || {
            reward_source.event_adjusted_gross_jc_at(
                action_id,
                user_id,
                guild_id,
                dispatch_timestamp,
            )
        })
        .await
        .map_err(|error| format!("Dig trivia reward lookup task failed: {error}"))??;
        let catalog = Arc::clone(&self.state.catalog);
        let retries = self.state.config.trivia_generation_retries;
        let question = tokio::task::spawn_blocking(move || {
            let mut random = SeededTriviaRandom::new(action_id.unsigned_abs());
            generate_question(&catalog, 0, &[], retries, &mut random)
        })
        .await
        .map_err(|error| format!("Dig trivia question task failed: {error}"))?
        .ok_or_else(|| "the configured trivia catalog could not produce a question".to_owned())?;
        let (embed, session) = prepare_trivia_bonus(user_id, guild_id, question, reward);
        let response =
            trivia_offer_response(action_id, embed, &session, self.state.config.trivia_timeout);
        let Some(receipt) = self
            .send_definite_or_ambiguous(Arc::clone(&responder), response)
            .await?
        else {
            return Ok(());
        };
        let key = trivia_session_key(action_id);
        let session = LiveBonusSession::Trivia {
            owner_id: user_id,
            guild_id,
            receipt,
            view: session,
            expires_at: Instant::now() + self.state.config.trivia_timeout,
        };
        self.insert_session_or_report(&key, session, &responder)
            .await;
        self.schedule_timeout(key, self.state.config.trivia_timeout);
        Ok(())
    }

    async fn send_definite_or_ambiguous(
        &self,
        responder: Arc<dyn InteractionResponder>,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, String> {
        match self
            .state
            .discord
            .send_bonus(responder.clone(), response)
            .await
        {
            Ok(receipt) => Ok(Some(receipt)),
            Err(DigBonusSendFailure::Rejected(error)) => Err(error),
            Err(DigBonusSendFailure::Ambiguous(error)) => {
                self.report_ambiguous_delivery(&responder, &error).await;
                // No receipt exists that can safely own a component session.
                // Preserve the provider marker and report the partial result;
                // returning Err here would release it and permit a duplicate
                // wheel/deal/trivia settlement on the next retry.
                Ok(None)
            }
        }
    }

    async fn report_ambiguous_delivery(
        &self,
        responder: &Arc<dyn InteractionResponder>,
        error: &str,
    ) {
        let message = format!(
            "{}\n\n(Delivery detail: {error})",
            cama_app::dig_bonus_events::PARTIAL_BONUS_FAILURE
        );
        let _ = responder
            .followup(InteractionResponse::message(message).ephemeral())
            .await;
    }

    async fn insert_session_or_report(
        &self,
        key: &str,
        session: LiveBonusSession,
        responder: &Arc<dyn InteractionResponder>,
    ) {
        let inserted = self
            .state
            .sessions
            .lock()
            .map(|mut sessions| sessions.insert(key.to_owned(), session))
            .is_ok();
        if !inserted {
            // The Discord message is already accepted and the action marker
            // must remain claimed.  Make the partial state explicit rather
            // than returning Err (which would release the provider marker).
            self.report_ambiguous_delivery(responder, "session state was unavailable")
                .await;
        }
    }
}

#[async_trait]
impl InteractionHandler for DigBonusRuntime {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        self.handle_component(request, responder)
            .await
            .map_err(InteractionHandlerError::from)
    }
}

impl RegistrationProvider for DigBonusRuntime {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.component(ComponentRoute {
            custom_id_prefix: DIG_BONUS_COMPONENT_PREFIX.to_owned(),
            handler: Arc::new(self.clone()),
        })
    }
}

fn package_session_key(action_id: i64) -> String {
    format!("{DIG_BONUS_COMPONENT_PREFIX}package:{action_id}")
}

fn trivia_session_key(action_id: i64) -> String {
    format!("{DIG_BONUS_COMPONENT_PREFIX}trivia:{action_id}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BonusComponentKind {
    Package(i64),
    Trivia(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedBonusComponent {
    key: String,
    kind: BonusComponentKind,
}

fn parse_component(custom_id: &str, values: &[String]) -> Result<ParsedBonusComponent, String> {
    let mut parts = custom_id.split(':');
    let prefix = parts.next();
    let kind = parts.next();
    let action_id = parts
        .next()
        .ok_or_else(|| "bonus component omitted its action ID".to_owned())?
        .parse::<i64>()
        .map_err(|_| "bonus component had an invalid action ID".to_owned())?;
    if prefix != Some("dig_bonus") {
        return Err("bonus component had an unknown route".to_owned());
    }
    match kind {
        Some("package") => {
            let partner_text = parts
                .next()
                .or_else(|| values.first().map(String::as_str))
                .ok_or_else(|| "package component omitted its partner value".to_owned())?;
            if parts.next().is_some() {
                return Err("package component had too many route segments".to_owned());
            }
            let partner_id = partner_text
                .parse::<i64>()
                .map_err(|_| "package component had an invalid partner".to_owned())?;
            Ok(ParsedBonusComponent {
                key: package_session_key(action_id),
                kind: BonusComponentKind::Package(partner_id),
            })
        }
        Some("trivia") => {
            let choice_text = parts
                .next()
                .or_else(|| values.first().map(String::as_str))
                .ok_or_else(|| "trivia component omitted its answer value".to_owned())?;
            if parts.next().is_some() {
                return Err("trivia component had too many route segments".to_owned());
            }
            let choice = choice_text
                .parse::<usize>()
                .map_err(|_| "trivia component had an invalid answer".to_owned())?;
            Ok(ParsedBonusComponent {
                key: trivia_session_key(action_id),
                kind: BonusComponentKind::Trivia(choice),
            })
        }
        _ => Err("bonus component had an unknown bonus kind".to_owned()),
    }
}

fn session_kind(session: &LiveBonusSession) -> &'static str {
    match session {
        LiveBonusSession::Package { .. } => "package",
        LiveBonusSession::Trivia { .. } => "trivia",
    }
}

fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} ID is outside SQLite INTEGER range"))
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), String> {
    responder
        .followup(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string())
}

fn message_update_response(update: MessageUpdate) -> InteractionResponse {
    let mut response = InteractionResponse::message(update.content.unwrap_or_default());
    if let Some(embed) = update.embed {
        response = response.embed(interaction_embed(embed));
    }
    if update.clear_view {
        response.components.clear();
    }
    response
}

fn package_offer_response(
    action_id: i64,
    embed: cama_domain::embed_safety::EmbedModel,
    session: &PackageDealSession,
) -> InteractionResponse {
    let buttons = package_buttons(action_id, session, false);
    InteractionResponse::message("")
        .embed(interaction_embed_with_color(embed, Some(0xF1_C4_0F)))
        .action_row(InteractionActionRow::buttons(buttons))
}

fn package_timeout_response(
    action_id: i64,
    session: &PackageDealSession,
    update: MessageUpdate,
) -> InteractionResponse {
    message_update_response(update).action_row(InteractionActionRow::buttons(package_buttons(
        action_id, session, true,
    )))
}

fn package_buttons(
    action_id: i64,
    session: &PackageDealSession,
    disabled: bool,
) -> Vec<InteractionButton> {
    session
        .candidates
        .iter()
        .map(|candidate| {
            InteractionButton::new(
                format!(
                    "{DIG_BONUS_COMPONENT_PREFIX}package:{action_id}:{}",
                    candidate.id
                ),
                truncate_button_label(&candidate.display_name),
            )
            .style(InteractionButtonStyle::Primary)
            .disabled(disabled)
        })
        .collect()
}

fn trivia_offer_response(
    action_id: i64,
    embed: cama_domain::embed_safety::EmbedModel,
    session: &DigTriviaSession,
    timeout: Duration,
) -> InteractionResponse {
    let buttons = session
        .question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            InteractionButton::new(
                format!("{DIG_BONUS_COMPONENT_PREFIX}trivia:{action_id}:{index}"),
                truncate_button_label(&format!("{}. {}", ['A', 'B', 'C', 'D'][index], option)),
            )
            .style(InteractionButtonStyle::Primary)
        })
        .collect();
    let mut response = InteractionResponse::message("")
        .embed(interaction_embed_with_color(embed, Some(0x58_65_F2)))
        .action_row(InteractionActionRow::buttons(buttons));
    if let Some(image_url) = session.question.image_url.as_deref()
        && let Some(embed) = response.embeds.first_mut()
    {
        embed.thumbnail_url = Some(image_url.to_owned());
    }
    if let Some(embed) = response.embeds.first_mut()
        && let Some(footer) = embed.footer.as_mut()
    {
        footer.push_str(&format!(" • {}s", timeout.as_secs()));
    }
    response
}

fn interaction_embed(embed: cama_domain::embed_safety::EmbedModel) -> InteractionEmbed {
    let color = embed.title.as_deref().and_then(result_embed_color);
    interaction_embed_with_color(embed, color)
}

fn interaction_embed_with_color(
    embed: cama_domain::embed_safety::EmbedModel,
    color: Option<u32>,
) -> InteractionEmbed {
    InteractionEmbed {
        title: embed.title,
        description: embed.description,
        color,
        footer: embed.footer,
        fields: embed
            .fields
            .into_iter()
            .map(|field| InteractionEmbedField {
                name: field.name,
                value: field.value,
                inline: field.inline,
            })
            .collect(),
        ..InteractionEmbed::default()
    }
}

fn result_embed_color(title: &str) -> Option<u32> {
    if title.contains("Correct") {
        Some(0x43_A0_47)
    } else if title.contains("Wrong")
        || title.contains("Time's up")
        || title.contains("Settlement Error")
    {
        Some(0xE5_39_35)
    } else {
        None
    }
}

fn truncate_button_label(value: &str) -> String {
    value.chars().take(80).collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SeededTriviaRandom {
    entropy: SeededLootEntropy,
}

impl SeededTriviaRandom {
    #[must_use]
    const fn new(seed: u64) -> Self {
        Self {
            entropy: SeededLootEntropy::new(seed),
        }
    }
}

impl TriviaRandom for SeededTriviaRandom {
    fn next_index(&mut self, upper: usize) -> usize {
        self.entropy.choose_index(upper)
    }
}

#[cfg(all(test, feature = "runtime-test-dig"))]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use crate::test_support::initialize_test_database as initialize_or_migrate;
    use cama_app::trivia_questions::{AbilityData, Difficulty, HeroData, ItemData, TriviaQuestion};
    use chrono::Timelike;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::discord_transport::{GuildPlayerNameDirectory, GuildPlayerNameResolver};
    use crate::registration::{InteractionMessageDelivery, InteractionResponseError};

    const GUILD: i64 = 70_001;
    const CHANNEL: i64 = 70_002;
    const USER: i64 = 70_003;

    #[derive(Default)]
    struct FakeResponder {
        follows: Mutex<Vec<InteractionResponse>>,
        updates: Mutex<Vec<InteractionResponse>>,
    }

    #[async_trait]
    impl InteractionResponder for FakeResponder {
        async fn respond(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.follows.lock().expect("follow lock").push(response);
            Ok(())
        }

        async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
            Ok(())
        }

        async fn followup(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.follows.lock().expect("follow lock").push(response);
            Ok(())
        }

        async fn followup_with_receipt(
            &self,
            response: InteractionResponse,
        ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
            self.follows.lock().expect("follow lock").push(response);
            Ok(Some(InteractionMessageReceipt {
                message_id: 900,
                channel_id: CHANNEL as u64,
                delivery: InteractionMessageDelivery::InteractionFollowup,
            }))
        }

        async fn update(
            &self,
            response: InteractionResponse,
        ) -> Result<(), InteractionResponseError> {
            self.updates.lock().expect("update lock").push(response);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeDiscord {
        members: Vec<GuildMember>,
        sent: Mutex<Vec<InteractionResponse>>,
        edits: Mutex<Vec<(InteractionMessageReceipt, InteractionResponse)>>,
        next_message_id: AtomicI64,
        send_failure: Mutex<Option<DigBonusSendFailure>>,
    }

    #[async_trait]
    impl DigBonusDiscordPort for FakeDiscord {
        async fn guild_members(&self, _guild_id: i64) -> Result<Vec<GuildMember>, String> {
            Ok(self.members.clone())
        }

        async fn send_bonus(
            &self,
            _responder: Arc<dyn InteractionResponder>,
            response: InteractionResponse,
        ) -> Result<InteractionMessageReceipt, DigBonusSendFailure> {
            if let Some(error) = self.send_failure.lock().expect("failure lock").take() {
                return Err(error);
            }
            self.sent.lock().expect("sent lock").push(response);
            Ok(InteractionMessageReceipt {
                message_id: self.next_message_id.fetch_add(1, Ordering::Relaxed) as u64 + 1,
                channel_id: CHANNEL as u64,
                delivery: InteractionMessageDelivery::InteractionFollowup,
            })
        }

        async fn edit_bonus_message(
            &self,
            receipt: InteractionMessageReceipt,
            response: InteractionResponse,
        ) -> Result<(), String> {
            self.edits
                .lock()
                .expect("edit lock")
                .push((receipt, response));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeWheel {
        calls: AtomicI64,
        result: Mutex<Option<DigBonusWheelFailure>>,
    }

    #[async_trait]
    impl DigBonusWheelPort for FakeWheel {
        async fn trigger_bonus_spin(
            &self,
            _user_id: i64,
            _guild_id: i64,
            _channel_id: i64,
            _responder: Arc<dyn InteractionResponder>,
        ) -> Result<(), DigBonusWheelFailure> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match self.result.lock().expect("wheel lock").take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    struct FakeRewardSource(i64);

    impl DigBonusRewardSource for FakeRewardSource {
        fn event_adjusted_gross_jc(
            &self,
            _action_id: i64,
            _user_id: i64,
            _guild_id: i64,
        ) -> Result<i64, String> {
            Ok(self.0)
        }
    }

    struct FixedPlayerNames(GuildPlayerNameDirectory);

    impl GuildPlayerNameResolver for FixedPlayerNames {
        fn names_for_guild(
            &self,
            _guild_id: i64,
            _player_ids: &[i64],
        ) -> Result<GuildPlayerNameDirectory, String> {
            Ok(self.0.clone())
        }
    }

    struct RecordingRewardSource {
        value: i64,
        observed_timestamp: Arc<Mutex<Option<i64>>>,
    }

    impl DigBonusRewardSource for RecordingRewardSource {
        fn event_adjusted_gross_jc(
            &self,
            _action_id: i64,
            _user_id: i64,
            _guild_id: i64,
        ) -> Result<i64, String> {
            Ok(self.value)
        }

        fn event_adjusted_gross_jc_at(
            &self,
            _action_id: i64,
            _user_id: i64,
            _guild_id: i64,
            dispatch_timestamp: i64,
        ) -> Result<i64, String> {
            *self.observed_timestamp.lock().expect("timestamp lock") = Some(dispatch_timestamp);
            Ok(self.value)
        }
    }

    fn fixture() -> NamedTempFile {
        let database = NamedTempFile::new().expect("temporary Dig bonus database");
        initialize_or_migrate(database.path()).expect("migrate Dig bonus database");
        database
    }

    fn seed_player(database: &NamedTempFile, user_id: i64, guild_id: i64, last_match_date: bool) {
        let connection = Connection::open(database.path()).expect("open fixture");
        connection
            .execute_batch("PRAGMA foreign_keys=OFF")
            .expect("match production repository foreign-key policy");
        connection
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username,
                     jopacoin_balance, lowest_balance_ever, last_match_date
                 ) VALUES (?1, ?2, 'dig-bonus', 100, 100, ?3)",
                rusqlite::params![
                    user_id,
                    guild_id,
                    last_match_date.then(|| chrono::Utc::now().to_rfc3339())
                ],
            )
            .expect("seed player");
    }

    fn runtime(database: &NamedTempFile, discord: Arc<FakeDiscord>) -> DigBonusRuntime {
        DigBonusRuntime::with_ports(
            database.path(),
            DigBonusRuntimeConfig::default(),
            Arc::new(TriviaCatalog::default()),
            Arc::new(FakeWheel::default()),
            Arc::new(FakeRewardSource(15)),
            discord,
        )
    }

    fn trivia_question() -> TriviaQuestion {
        TriviaQuestion {
            text: "Which hero?".to_owned(),
            options: ["Axe", "Bane", "Chen", "Drow"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            correct_index: 1,
            difficulty: Difficulty::Easy,
            image_url: None,
            category: "hero_quote",
            explanation: None,
        }
    }

    fn generated_catalog() -> TriviaCatalog {
        TriviaCatalog {
            heroes: (0..12)
                .map(|index| HeroData {
                    id: index + 1,
                    localized_name: format!("Hero {index}"),
                    primary_attribute: Some("strength".to_owned()),
                    is_melee: index < 6,
                    base_movement: Some(300 + index),
                    attack_rate: Some(1.7),
                    strength_gain: Some(2.0),
                    agility_gain: Some(2.0),
                    intelligence_gain: Some(2.0),
                    damage_min_at_level_one: Some(40),
                    damage_max_at_level_one: Some(50),
                    night_vision: Some(800),
                    turn_rate: Some(0.6),
                    image_url: Some(format!("https://cdn.test/hero{index}.png")),
                    ..HeroData::default()
                })
                .collect(),
            abilities: (0..8)
                .map(|index| AbilityData {
                    localized_name: format!("Spell {index}"),
                    hero_id: Some(index + 1),
                    hero_name: Some(format!("Hero {index}")),
                    damage_type: Some("magical".to_owned()),
                    damage: Some("100".to_owned()),
                    cooldown: Some("10".to_owned()),
                    icon_url: Some(format!("https://cdn.test/spell{index}.png")),
                    ..AbilityData::default()
                })
                .collect(),
            items: (0..12)
                .map(|index| ItemData {
                    localized_name: format!("Item {index}"),
                    cost: Some(100 + index * 10),
                    neutral_tier: Some((index % 5) + 1),
                    icon_url: Some(format!("https://cdn.test/item{index}.png")),
                    ..ItemData::default()
                })
                .collect(),
            voicelines: Vec::new(),
        }
    }

    #[test]
    fn sqlite_reward_source_uses_fixed_trivia_gross_and_active_edict() {
        let database = fixture();
        let now = Utc::now().timestamp();
        let dig_created_at = now - 86_400;
        let connection = Connection::open(database.path()).expect("open fixture");
        connection
            .execute(
                "INSERT INTO dig_actions (
                    guild_id, actor_id, action_type, depth_before, depth_after,
                    jc_delta, detail, created_at
                 ) VALUES (?1, ?2, 'dig', 1, 2, -91, '{\"economy_adjusted_jc\":999}', ?3)",
                rusqlite::params![GUILD, USER, dig_created_at],
            )
            .expect("seed action");
        let local = chrono::DateTime::from_timestamp(now - 7 * 3_600, 0)
            .expect("current timestamp")
            .naive_utc();
        let event_date = (if local.hour() < 10 {
            local.date() - chrono::Duration::days(1)
        } else {
            local.date()
        })
        .format("%Y-%m-%d")
        .to_string();
        connection
            .execute(
                "INSERT INTO economy_daily_events (
                    guild_id,event_date,name,hero,direction,severity,
                    target_effect_jc,forecast_flow_jc,expected_effect_jc,
                    monetary_stock_before,effects,announcement,starts_at,ends_at,created_at
                 ) VALUES (?1,?2,'Edict','Axe','boon',1,0,0,0,0,
                    '{\"reward_multiplier\":2.0}','Edict',?3,?4,?3)",
                rusqlite::params![GUILD, event_date, now - 60, now + 60],
            )
            .expect("seed active edict");
        let source =
            SqliteDigBonusRewardSource::new(database.path(), EconomyEventConfig::default());
        // Trivia applies the current dispatch-time edict to its fixed gross;
        // the prior day's Dig reward and persisted detail are intentionally
        // unrelated to this settlement.
        assert_eq!(
            source
                .event_adjusted_gross_jc(1, USER, GUILD)
                .expect("adjust reward"),
            30
        );
    }

    #[tokio::test]
    async fn package_dispatch_uses_four_truncated_candidates_and_persists_session() {
        let database = fixture();
        for id in USER..(USER + 6) {
            seed_player(&database, id, GUILD, true);
        }
        let discord = Arc::new(FakeDiscord {
            members: (USER..(USER + 6))
                .map(|id| GuildMember {
                    id,
                    display_name: format!("{}{}", "x".repeat(100), id),
                    bot: false,
                })
                .collect(),
            ..FakeDiscord::default()
        });
        let runtime = runtime(&database, Arc::clone(&discord));
        let responder: Arc<dyn InteractionResponder> = Arc::new(FakeResponder::default());
        runtime
            .dispatch_bonus(8, USER, GUILD, CHANNEL, DigBonus::PackageDeal, responder)
            .await
            .expect("dispatch package bonus");
        assert_eq!(runtime.active_session_count(), 1);
        let sent = discord.sent.lock().expect("sent lock");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].embeds[0].color, Some(0xF1_C4_0F));
        let buttons = &sent[0].components[0].buttons;
        assert_eq!(buttons.len(), 4);
        assert!(
            buttons
                .iter()
                .all(|button| button.label.chars().count() <= 80)
        );
        assert!(buttons.iter().all(|button| button.emoji.is_none()));
    }

    #[tokio::test]
    async fn package_candidate_buttons_prefer_server_nicknames_over_transport_names() {
        let database = fixture();
        for id in USER..(USER + 6) {
            seed_player(&database, id, GUILD, true);
        }
        let discord = Arc::new(FakeDiscord {
            members: (USER..(USER + 6))
                .map(|id| GuildMember {
                    id,
                    display_name: format!("Global {id}"),
                    bot: false,
                })
                .collect(),
            ..FakeDiscord::default()
        });
        let nicknames = (USER..(USER + 6))
            .map(|id| (id as u64, format!("Server {id}")))
            .collect();
        let runtime = runtime(&database, Arc::clone(&discord)).with_player_names(Arc::new(
            FixedPlayerNames(GuildPlayerNameDirectory::new(Some(nicknames))),
        ));

        runtime
            .dispatch_bonus(
                108,
                USER,
                GUILD,
                CHANNEL,
                DigBonus::PackageDeal,
                Arc::new(FakeResponder::default()),
            )
            .await
            .expect("dispatch package bonus");

        let sent = discord.sent.lock().expect("sent lock");
        assert!(
            sent[0].components[0]
                .buttons
                .iter()
                .all(|button| button.label.starts_with("Server "))
        );
        assert!(
            sent[0].components[0]
                .buttons
                .iter()
                .all(|button| !button.label.starts_with("Global "))
        );
    }

    #[tokio::test]
    async fn trivia_dispatch_freezes_event_adjusted_reward_in_presented_view() {
        let database = fixture();
        seed_player(&database, USER, GUILD, false);
        let discord = Arc::new(FakeDiscord::default());
        let catalog = Arc::new(generated_catalog());
        let action_id = (1_i64..100_i64)
            .find(|action_id| {
                let mut random = SeededTriviaRandom::new(action_id.unsigned_abs());
                generate_question(&catalog, 0, &[], 15, &mut random).is_some()
            })
            .expect("generated fixture question");
        let observed_timestamp = Arc::new(Mutex::new(None));
        let runtime = DigBonusRuntime::with_ports(
            database.path(),
            DigBonusRuntimeConfig::default(),
            catalog,
            Arc::new(FakeWheel::default()),
            Arc::new(RecordingRewardSource {
                value: 30,
                observed_timestamp: Arc::clone(&observed_timestamp),
            }),
            discord.clone(),
        );
        let responder: Arc<dyn InteractionResponder> = Arc::new(FakeResponder::default());
        let before_dispatch = Utc::now().timestamp();
        runtime
            .dispatch_bonus(action_id, USER, GUILD, CHANNEL, DigBonus::Trivia, responder)
            .await
            .expect("dispatch trivia bonus");
        let after_dispatch = Utc::now().timestamp();
        let observed_timestamp = observed_timestamp
            .lock()
            .expect("timestamp lock")
            .expect("dispatch timestamp");
        assert!((before_dispatch..=after_dispatch).contains(&observed_timestamp));
        let sent = discord.sent.lock().expect("sent lock");
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0].embeds[0]
                .footer
                .as_deref()
                .is_some_and(|footer| footer.contains("+20 JC") && footer.ends_with("• 15s"))
        );
        assert_eq!(sent[0].embeds[0].color, Some(0x58_65_F2));
        assert_eq!(sent[0].components[0].buttons.len(), 4);
        assert_eq!(runtime.active_session_count(), 1);
    }

    #[tokio::test]
    async fn component_owner_check_preserves_session_and_valid_component_acks_update() {
        let database = fixture();
        for id in USER..(USER + 6) {
            seed_player(&database, id, GUILD, true);
        }
        let discord = Arc::new(FakeDiscord {
            members: (USER..(USER + 6))
                .map(|id| GuildMember {
                    id,
                    display_name: id.to_string(),
                    bot: false,
                })
                .collect(),
            ..FakeDiscord::default()
        });
        let runtime = runtime(&database, Arc::clone(&discord));
        let dispatch_responder: Arc<dyn InteractionResponder> = Arc::new(FakeResponder::default());
        runtime
            .dispatch_bonus(
                9,
                USER,
                GUILD,
                CHANNEL,
                DigBonus::PackageDeal,
                dispatch_responder,
            )
            .await
            .expect("dispatch package bonus");
        let button_id = discord.sent.lock().expect("sent lock")[0].components[0].buttons[0]
            .custom_id
            .clone();
        let wrong = Arc::new(FakeResponder::default());
        runtime
            .handle_component(
                InteractionRequest::Component {
                    interaction_id: 1,
                    custom_id: button_id.clone(),
                    user_id: (USER + 100) as u64,
                    user_display_name: "wrong".to_owned(),
                    guild_id: Some(GUILD as u64),
                    channel_id: Some(CHANNEL as u64),
                    member_permissions: None,
                    values: Vec::new(),
                },
                wrong.clone(),
            )
            .await
            .expect("owner rejection response");
        assert_eq!(runtime.active_session_count(), 1);
        assert!(
            wrong.follows.lock().expect("follow lock")[0]
                .content
                .contains("isn't your")
        );

        let owner = Arc::new(FakeResponder::default());
        runtime
            .handle_component(
                InteractionRequest::Component {
                    interaction_id: 2,
                    custom_id: button_id,
                    user_id: USER as u64,
                    user_display_name: "owner".to_owned(),
                    guild_id: Some(GUILD as u64),
                    channel_id: Some(CHANNEL as u64),
                    member_permissions: None,
                    values: Vec::new(),
                },
                owner.clone(),
            )
            .await
            .expect("owner update");
        assert_eq!(owner.updates.lock().expect("update lock").len(), 1);
        assert_eq!(runtime.active_session_count(), 0);
    }

    #[tokio::test]
    async fn trivia_timeout_settles_minus_five_and_edits_message() {
        let database = fixture();
        seed_player(&database, USER, GUILD, false);
        let discord = Arc::new(FakeDiscord::default());
        let runtime = runtime(&database, Arc::clone(&discord));
        let receipt = InteractionMessageReceipt {
            message_id: 44,
            channel_id: CHANNEL as u64,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        };
        runtime.state.sessions.lock().expect("session lock").insert(
            trivia_session_key(12),
            LiveBonusSession::Trivia {
                owner_id: USER,
                guild_id: GUILD,
                receipt,
                view: DigTriviaSession::new(USER, GUILD, trivia_question(), 20),
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        assert_eq!(runtime.expire_sessions().await.expect("expire session"), 1);
        assert_eq!(runtime.active_session_count(), 0);
        let balance = Connection::open(database.path())
            .expect("open fixture")
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                rusqlite::params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("read balance");
        assert_eq!(balance, 95);
        let edits = discord.edits.lock().expect("edit lock");
        assert_eq!(edits.len(), 1);
        assert!(
            edits[0].1.embeds[0]
                .title
                .as_deref()
                .is_some_and(|title| title.contains("Time's up"))
        );
    }

    #[tokio::test]
    async fn process_restart_drops_old_process_local_package_view() {
        let database = fixture();
        let discord = Arc::new(FakeDiscord::default());
        let first = runtime(&database, Arc::clone(&discord));
        first.state.sessions.lock().expect("session lock").insert(
            package_session_key(88),
            LiveBonusSession::Package {
                action_id: 88,
                owner_id: USER,
                guild_id: GUILD,
                receipt: InteractionMessageReceipt {
                    message_id: 88,
                    channel_id: CHANNEL as u64,
                    delivery: InteractionMessageDelivery::InteractionFollowup,
                },
                view: PackageDealSession::new(
                    USER,
                    GUILD,
                    vec![GuildMember {
                        id: USER + 1,
                        display_name: "Partner".to_owned(),
                        bot: false,
                    }],
                ),
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );
        assert_eq!(first.active_session_count(), 1);

        let restarted = runtime(&database, Arc::clone(&discord));
        let responder = Arc::new(FakeResponder::default());
        restarted
            .handle_component(
                InteractionRequest::Component {
                    interaction_id: 3,
                    custom_id: format!("{DIG_BONUS_COMPONENT_PREFIX}package:88:{}", USER + 1),
                    user_id: USER as u64,
                    user_display_name: "owner".to_owned(),
                    guild_id: Some(GUILD as u64),
                    channel_id: Some(CHANNEL as u64),
                    member_permissions: None,
                    values: Vec::new(),
                },
                responder.clone(),
            )
            .await
            .expect("stale view response");
        assert_eq!(restarted.active_session_count(), 0);
        assert!(
            responder.follows.lock().expect("follow lock")[0]
                .content
                .contains("no longer active")
        );
    }

    #[tokio::test]
    async fn package_timeout_keeps_four_disabled_controls_and_clears_offer_embed() {
        let database = fixture();
        let discord = Arc::new(FakeDiscord::default());
        let runtime = runtime(&database, Arc::clone(&discord));
        let candidates = (0..4)
            .map(|index| GuildMember {
                id: USER + index + 1,
                display_name: format!("Miner {index}"),
                bot: false,
            })
            .collect();
        runtime.state.sessions.lock().expect("session lock").insert(
            package_session_key(89),
            LiveBonusSession::Package {
                action_id: 89,
                owner_id: USER,
                guild_id: GUILD,
                receipt: InteractionMessageReceipt {
                    message_id: 89,
                    channel_id: CHANNEL as u64,
                    delivery: InteractionMessageDelivery::InteractionFollowup,
                },
                view: PackageDealSession::new(USER, GUILD, candidates),
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );

        assert_eq!(runtime.expire_sessions().await.expect("expire package"), 1);
        let edits = discord.edits.lock().expect("edit lock");
        assert_eq!(edits.len(), 1);
        let response = &edits[0].1;
        assert!(response.embeds.is_empty());
        assert_eq!(response.components.len(), 1);
        assert_eq!(response.components[0].buttons.len(), 4);
        assert!(
            response.components[0]
                .buttons
                .iter()
                .all(|button| button.disabled)
        );
    }

    #[test]
    fn trivia_buttons_use_python_delimiter_and_unicode_safe_eighty_char_limit() {
        let question = TriviaQuestion {
            text: "Question".to_owned(),
            options: [
                "é".repeat(100),
                "B".to_owned(),
                "C".to_owned(),
                "D".to_owned(),
            ]
            .into_iter()
            .collect(),
            correct_index: 0,
            difficulty: Difficulty::Easy,
            image_url: Some("https://cdn.test/question.png".to_owned()),
            category: "test",
            explanation: None,
        };
        let (_embed, session) = prepare_trivia_bonus(USER, GUILD, question, 15);
        let response = trivia_offer_response(
            99,
            cama_domain::embed_safety::EmbedModel {
                title: Some("⛏️ Unearthed in the Mine — Dota 2 Trivia".to_owned()),
                footer: Some("+10 JC correct • -5 JC wrong or timeout".to_owned()),
                ..cama_domain::embed_safety::EmbedModel::default()
            },
            &session,
            Duration::from_secs(15),
        );
        let button = &response.components[0].buttons[0];
        assert!(button.label.starts_with("A. "));
        assert_eq!(button.label.chars().count(), 80);
        assert_eq!(
            response.embeds[0].thumbnail_url.as_deref(),
            Some("https://cdn.test/question.png")
        );
        assert_eq!(response.embeds[0].color, Some(0x58_65_F2));
        assert!(
            response.embeds[0]
                .footer
                .as_deref()
                .is_some_and(|footer| footer.ends_with("• 15s"))
        );
    }

    #[tokio::test]
    async fn ambiguous_wheel_failure_is_terminal_and_does_not_return_retry_error() {
        let database = fixture();
        let wheel = Arc::new(FakeWheel {
            calls: AtomicI64::new(0),
            result: Mutex::new(Some(DigBonusWheelFailure::Ambiguous("accepted".to_owned()))),
        });
        let discord = Arc::new(FakeDiscord::default());
        let runtime = DigBonusRuntime::with_ports(
            database.path(),
            DigBonusRuntimeConfig::default(),
            Arc::new(TriviaCatalog::default()),
            wheel.clone(),
            Arc::new(FakeRewardSource(15)),
            discord,
        );
        let responder: Arc<dyn InteractionResponder> = Arc::new(FakeResponder::default());
        runtime
            .dispatch_bonus(13, USER, GUILD, CHANNEL, DigBonus::Wheel, responder)
            .await
            .expect("ambiguous wheel remains terminal");
        assert_eq!(wheel.calls.load(Ordering::Relaxed), 1);
    }
}
