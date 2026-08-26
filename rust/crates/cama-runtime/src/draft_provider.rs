//! Production `/draft` command and component provider.
//!
//! This module is the gateway-facing adapter for [`cama_app::draft`]: all
//! lobby membership,
//! reservations, reset generations, and operation locks are borrowed from the
//! live [`MatchLobbyPort`], while the process-local [`DraftStateManager`] is
//! supplied by the lobby provider.  The provider does not construct either a
//! second lobby service or a second draft manager.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::betting_service::{
    AutomaticSpectatorConfig, WealthSnapshot, plan_automatic_spectator_bets,
};
use cama_app::draft::{
    DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE, DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE,
    DRAFT_FINANCIAL_SETUP_PLAN_VERSION, DRAFT_TOTAL_PICKS, DRAFTING_TIMEOUT_SECONDS, DraftEmbed,
    DraftEmbedField, DraftFinalizationJobSnapshot, DraftFinalizationPlan,
    DraftFinalizationPlanSeed, DraftFinancialSetupPolicy, DraftFinancialSetupStage, DraftPhase,
    DraftState, DraftStateEnvelope, DraftStateManager, DraftStatePersistencePort,
    LobbyKind as DraftLobbyKind, PRE_DRAFT_TIMEOUT_SECONDS, PlayerPoolEntry,
    SqliteDraftStatePersistence, draft_finalization_process_owner,
};
use cama_app::embeds::LobbyKind as AppLobbyKind;
use cama_db::autobet_investments::AutobetInvestmentRepository;
use cama_db::betting_service_repository::{
    AutomaticBasisPointBetRequest, BettingServiceRepository, BlindBetCandidate, BlindBetPolicy,
    PlaceBetRequest,
};
use cama_db::core_repositories::{CoreRepositoryError, NewPlayer, PlayerRepository};
use cama_db::dota_bet_seed_repository::{
    BettingMode as SeedBettingMode, BettingTeam, DotaBetSeedRepository, FirstGameLobby,
};
use cama_db::match_runtime::{PendingMatchRecord, PendingMatchRepository, PendingMatchState};
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::player::Player;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::task::{JoinHandle, spawn_blocking};
use tracing::{debug, warn};

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{
    DiscordDestinationStatus, DiscordMessage, DiscordMessageReceipt, DiscordTransport,
    GuildPlayerNameDirectory,
};
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::lobby_provider::{MatchLobbyPort, MatchLobbySnapshot};
use crate::match_provider::MatchRegistrationProvider;
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionButton, InteractionButtonStyle, InteractionEmbed,
    InteractionEmbedField, InteractionHandler, InteractionHandlerError, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};

const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const OPERATION_LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const DRAFT_COMPONENT_PREFIX: &str = "draft";
const COMPLETION_ERROR_WITHOUT_MATCH: &str = "⚠️ Draft complete but encountered an error during match setup. Use `/draft start` to try again.";
const DRAFT_FINALIZATION_LEASE_SECONDS: i64 = 120;

fn same_database_path(left: &Path, right: &Path) -> bool {
    left == right
        || matches!(
            (left.canonicalize(), right.canonicalize()),
            (Ok(left), Ok(right)) if left == right
        )
}

/// Runtime-facing build failures are configuration or construction errors;
/// ordinary command failures remain user-visible interaction responses.
#[derive(Debug, Error)]
pub enum DraftProviderBuildError {
    #[error("invalid draft lobby ready threshold")]
    ReadyThreshold,
    #[error("invalid draft bet lock seconds")]
    BetLockSeconds,
    #[error("invalid draft database path: {0}")]
    Database(String),
}

/// Narrow completion hook implemented by the live match provider at
/// composition time. Draft owns durable pending-match creation; the match
/// provider owns reminder task cancellation/replacement and Discord reminder
/// delivery. The default adapter is intentionally inert for transitional
/// construction and tests that do not install MatchProvider.
#[async_trait]
pub trait DraftReminderScheduler: Send + Sync {
    async fn schedule_betting_reminders(&self, pending: PendingMatchRecord) -> Result<(), String>;

    /// Rebuild durable timer state after a restart without replaying the
    /// immediate subscriber notification that belongs to the original
    /// publication attempt.  Adapters that cannot separate those effects
    /// retain the source-compatible behavior as a conservative fallback.
    async fn schedule_betting_reminders_recovery(
        &self,
        pending: PendingMatchRecord,
    ) -> Result<(), String> {
        self.schedule_betting_reminders(pending).await
    }
}

struct NoopDraftReminderScheduler;

#[async_trait]
impl DraftReminderScheduler for NoopDraftReminderScheduler {
    async fn schedule_betting_reminders(&self, _pending: PendingMatchRecord) -> Result<(), String> {
        Ok(())
    }
}

#[async_trait]
impl DraftReminderScheduler for MatchRegistrationProvider {
    async fn schedule_betting_reminders(&self, pending: PendingMatchRecord) -> Result<(), String> {
        self.schedule_draft_betting_reminders(pending)
    }

    async fn schedule_betting_reminders_recovery(
        &self,
        pending: PendingMatchRecord,
    ) -> Result<(), String> {
        self.schedule_draft_betting_reminders_recovery(pending)
    }
}

/// Typed Neon seam for the three Draft events owned by the Python command.
/// The live composition layer can adapt the existing Neon service without
/// making this provider depend on Neon implementation details.  Returning
/// `None` is a normal no-event result; an error is logged and never turns a
/// durable draft failure-prone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftNeonResult {
    pub text_block: String,
    pub attachment: Option<crate::registration::InteractionAttachment>,
}

#[async_trait]
pub trait DraftNeonObserver: Send + Sync {
    async fn on_draft_coinflip(
        &self,
        guild_id: i64,
        winner_id: i64,
        loser_id: i64,
    ) -> Result<Option<DraftNeonResult>, String>;

    async fn on_captain_symmetry(
        &self,
        guild_id: i64,
        radiant_captain_id: i64,
        dire_captain_id: i64,
        rating_diff: i64,
    ) -> Result<Option<DraftNeonResult>, String>;

    async fn on_bomb_pot(
        &self,
        guild_id: i64,
        pool_amount: i64,
        contributor_count: i64,
    ) -> Result<Option<DraftNeonResult>, String>;
}

struct NoopDraftNeonObserver;

#[async_trait]
impl DraftNeonObserver for NoopDraftNeonObserver {
    async fn on_draft_coinflip(
        &self,
        _guild_id: i64,
        _winner_id: i64,
        _loser_id: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        Ok(None)
    }

    async fn on_captain_symmetry(
        &self,
        _guild_id: i64,
        _radiant_captain_id: i64,
        _dire_captain_id: i64,
        _rating_diff: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        Ok(None)
    }

    async fn on_bomb_pot(
        &self,
        _guild_id: i64,
        _pool_amount: i64,
        _contributor_count: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        Ok(None)
    }
}

#[derive(Clone)]
pub struct DraftRegistrationProvider {
    handler: Arc<DraftHandler>,
}

type DraftOperationLocks = Arc<Mutex<BTreeMap<(i64, u64), Arc<tokio::sync::Mutex<()>>>>>;

fn cleanup_unused_draft_operation_lock_entry(
    locks: &DraftOperationLocks,
    drafts: &DraftStateManager,
    guild_id: i64,
    session_id: u64,
    operation_lock: &Arc<tokio::sync::Mutex<()>>,
) {
    let active_same_session = drafts
        .get_state(Some(guild_id))
        .is_some_and(|state| lock_state(&state).session_id == session_id);
    if active_same_session {
        return;
    }
    let mut locks = lock_recover(locks);
    let removable = locks
        .get(&(guild_id, session_id))
        .is_some_and(|stored| Arc::ptr_eq(stored, operation_lock))
        && Arc::strong_count(operation_lock) == 2;
    if removable {
        locks.remove(&(guild_id, session_id));
    }
}

struct CompletionMessageContext<'a> {
    guild_id: i64,
    pending_match_id: i64,
    state: &'a DraftState,
    published: &'a [(u64, DiscordMessageReceipt)],
    thread_receipt: Option<&'a DiscordMessageReceipt>,
    lobby_channel_id: Option<u64>,
    origin_channel_id: Option<u64>,
    draft_receipt: Option<&'a DiscordMessageReceipt>,
}

impl DraftRegistrationProvider {
    /// Build a draft provider over the already-admitted SQLite database and
    /// the same lobby/draft handles used by `/lobby` and `/shuffle`.
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        lobbies: MatchLobbyPort,
        drafts: Arc<DraftStateManager>,
        discord: Arc<dyn DiscordTransport>,
    ) -> Result<Self, DraftProviderBuildError> {
        Self::new_with_reminder_scheduler(
            database_path,
            config,
            lobbies,
            drafts,
            discord,
            Arc::new(NoopDraftReminderScheduler),
        )
    }

    /// Build with the live MatchProvider reminder owner. This additive
    /// constructor keeps the old five-argument constructor source-compatible
    /// while allowing composition to install the exact runtime scheduler.
    pub fn new_with_reminder_scheduler(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        lobbies: MatchLobbyPort,
        drafts: Arc<DraftStateManager>,
        discord: Arc<dyn DiscordTransport>,
        reminders: Arc<dyn DraftReminderScheduler>,
    ) -> Result<Self, DraftProviderBuildError> {
        Self::new_with_reminder_scheduler_and_neon(
            database_path,
            config,
            lobbies,
            drafts,
            discord,
            reminders,
            Arc::new(NoopDraftNeonObserver),
        )
    }

    /// Build with both the live reminder owner and an injectable Neon event
    /// observer.  Runtime composition can install the production Neon
    /// adapter without changing Draft's command/state ownership.
    pub fn new_with_reminder_scheduler_and_neon(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        lobbies: MatchLobbyPort,
        drafts: Arc<DraftStateManager>,
        discord: Arc<dyn DiscordTransport>,
        reminders: Arc<dyn DraftReminderScheduler>,
        neon: Arc<dyn DraftNeonObserver>,
    ) -> Result<Self, DraftProviderBuildError> {
        Self::new_with_reminder_scheduler_and_neon_and_persistence(
            database_path,
            config,
            lobbies,
            drafts,
            discord,
            reminders,
            neon,
            None,
        )
    }

    /// Build a provider with an optional durable draft-state port.
    ///
    /// Existing constructors intentionally pass `None` so source-compatible
    /// unit fixtures can continue to create process-local drafts. Production
    /// composition should pass [`SqliteDraftStatePersistence`] (or another
    /// implementation) here and call [`Self::hydrate`] before registering
    /// gateway routes.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_reminder_scheduler_and_neon_and_persistence(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        lobbies: MatchLobbyPort,
        drafts: Arc<DraftStateManager>,
        discord: Arc<dyn DiscordTransport>,
        reminders: Arc<dyn DraftReminderScheduler>,
        neon: Arc<dyn DraftNeonObserver>,
        persistence: Option<Arc<dyn DraftStatePersistencePort>>,
    ) -> Result<Self, DraftProviderBuildError> {
        if config.values.lobby_ready_threshold <= 0 {
            return Err(DraftProviderBuildError::ReadyThreshold);
        }
        if config.values.bet_lock_seconds < 0 {
            return Err(DraftProviderBuildError::BetLockSeconds);
        }
        let path = database_path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() {
            return Err(DraftProviderBuildError::Database(
                "database path is empty".to_owned(),
            ));
        }
        let handler = Arc::new(DraftHandler {
            config: DraftConfig::from_application(config),
            database_path: path.clone(),
            players: PlayerRepository::new(&path),
            pending: PendingMatchRepository::new(&path),
            seeds: DotaBetSeedRepository::new(&path),
            bets: BettingServiceRepository::new(&path),
            investments: AutobetInvestmentRepository::new(&path),
            lobbies,
            drafts,
            discord,
            reminders,
            neon,
            service: Mutex::new(cama_app::draft::DraftService::new()),
            draft_messages: Arc::new(Mutex::new(BTreeMap::new())),
            timeout_tasks: Mutex::new(BTreeMap::new()),
            completing: Mutex::new(BTreeSet::new()),
            persistence,
            persisted_envelopes: Arc::new(Mutex::new(BTreeMap::new())),
            draft_operation_locks: Arc::new(Mutex::new(BTreeMap::new())),
        });
        Ok(Self { handler })
    }

    /// Attach the canonical SQLite persistence adapter to a provider that was
    /// composed with the source-compatible constructor.  Keeping this as a
    /// consuming step lets older tests and embedders retain `persistence=None`
    /// while production can make the durable boundary explicit at composition.
    pub fn with_sqlite_persistence(
        self,
        persistence_database_path: impl AsRef<Path>,
    ) -> Result<Self, DraftProviderBuildError> {
        let persistence_database_path = persistence_database_path.as_ref();
        if !same_database_path(&self.handler.database_path, persistence_database_path) {
            return Err(DraftProviderBuildError::Database(
                "draft persistence and pending matches must share one SQLite database".to_owned(),
            ));
        }
        self.with_persistence(Arc::new(SqliteDraftStatePersistence::new(
            persistence_database_path,
        )))
    }

    /// Attach an already-constructed durable persistence port.  Production
    /// composition uses this to make the canonical SQLite path visible at
    /// the process boundary; tests may inject an in-memory/failing adapter.
    pub fn with_persistence(
        self,
        persistence: Arc<dyn DraftStatePersistencePort>,
    ) -> Result<Self, DraftProviderBuildError> {
        let mut handler = Arc::try_unwrap(self.handler).map_err(|_| {
            DraftProviderBuildError::Database(
                "draft persistence must be attached before the provider is cloned".to_owned(),
            )
        })?;
        handler.persistence = Some(persistence);
        Ok(Self {
            handler: Arc::new(handler),
        })
    }

    /// Register the Draft READY/resume recovery observer.  Serenity attaches
    /// its HTTP/cache context before invoking this observer, so hydration can
    /// safely repaint durable component views and restore their timeout owner.
    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(DraftGatewayObserver {
            provider: self.clone(),
            ready_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Build a provider with SQLite-backed draft persistence and the default
    /// reminder/Neon adapters.  The caller still owns startup hydration.
    pub fn new_with_persistence(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        lobbies: MatchLobbyPort,
        drafts: Arc<DraftStateManager>,
        discord: Arc<dyn DiscordTransport>,
        persistence_database_path: impl AsRef<Path>,
    ) -> Result<Self, DraftProviderBuildError> {
        let database_path = database_path.as_ref();
        let persistence_database_path = persistence_database_path.as_ref();
        if !same_database_path(database_path, persistence_database_path) {
            return Err(DraftProviderBuildError::Database(
                "draft persistence and pending matches must share one SQLite database".to_owned(),
            ));
        }
        Self::new_with_reminder_scheduler_and_neon_and_persistence(
            database_path,
            config,
            lobbies,
            drafts,
            discord,
            Arc::new(NoopDraftReminderScheduler),
            Arc::new(NoopDraftNeonObserver),
            Some(Arc::new(SqliteDraftStatePersistence::new(
                persistence_database_path,
            ))),
        )
    }

    /// Explicitly resume only the durable financial-setup stage for a
    /// finalizing Draft after process recreation.
    ///
    /// This deliberately does not publish Discord output, restore lobby
    /// reservations, or run terminal reconciliation; those remain owned by a
    /// future recovery worker rather than READY composition.
    pub async fn resume_finalizing_financial_setup(
        &self,
        guild_id: i64,
    ) -> Result<PendingMatchRecord, String> {
        self.handler
            .resume_finalizing_financial_setup(guild_id)
            .await
    }

    /// Reconcile every durable finalization row after a process restart.
    /// Financial effects are resumed from their immutable job stage, Discord
    /// deliveries use the plan's deterministic nonce, and the final envelope
    /// deletion is a leased revision/stage CAS.
    pub async fn recover_finalizing(&self) -> Result<usize, String> {
        self.recover_finalizing_scoped(None).await
    }

    async fn recover_finalizing_for_guilds(
        &self,
        guild_ids: &BTreeSet<i64>,
    ) -> Result<usize, String> {
        self.recover_finalizing_scoped(Some(guild_ids)).await
    }

    async fn recover_finalizing_scoped(
        &self,
        guild_ids: Option<&BTreeSet<i64>>,
    ) -> Result<usize, String> {
        let Some(persistence) = self.handler.persistence.clone() else {
            return Ok(0);
        };
        let envelopes = spawn_blocking(move || persistence.load_all())
            .await
            .map_err(|error| format!("draft finalization load task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        let mut recovered = 0usize;
        let mut failures = Vec::new();
        for envelope in envelopes {
            if guild_ids.is_some_and(|guild_ids| !guild_ids.contains(&envelope.guild_id)) {
                continue;
            }
            if !envelope.finalizing && envelope.pending_match_id.is_none() {
                continue;
            }
            let guild_id = envelope.guild_id;
            let session_id = envelope.session_id;
            let kind = to_runtime_kind(envelope.state.as_state().lobby_kind);
            let draft_operation_lock = self.handler.draft_operation_lock(guild_id, session_id);
            let _draft_operation_guard = draft_operation_lock.lock().await;
            let lobby_operation_lock = self.handler.lobbies.operation_lock(guild_id, kind);
            let _lobby_operation_guard = lobby_operation_lock.lock().await;
            let result = if envelope.finalizing && envelope.pending_match_id.is_none() {
                self.handler
                    .recover_unlinked_finalization_envelope(envelope, &draft_operation_lock)
                    .await
            } else {
                self.handler.recover_finalizing_envelope(envelope).await
            };
            match result {
                Ok(()) => recovered = recovered.saturating_add(1),
                Err(error) => failures.push(format!("guild {guild_id}: {error}")),
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "Draft finalization recovery failed for {} row(s): {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        Ok(recovered)
    }

    /// Hydrate active, non-finalizing drafts from the configured persistence
    /// port and restore their timeout ownership.
    ///
    /// Finalization recovery is deliberately not attempted here.  A row with
    /// `finalizing` or `pending_match_id` requires an idempotent pending-match
    /// reconciliation protocol before it can safely be published or cleared.
    pub async fn hydrate(&self) -> Result<usize, String> {
        self.hydrate_scoped(None).await
    }

    async fn hydrate_for_guilds(&self, guild_ids: &BTreeSet<i64>) -> Result<usize, String> {
        self.hydrate_scoped(Some(guild_ids)).await
    }

    async fn hydrate_scoped(&self, guild_ids: Option<&BTreeSet<i64>>) -> Result<usize, String> {
        let Some(persistence) = self.handler.persistence.clone() else {
            return Ok(0);
        };
        let envelopes = spawn_blocking(move || persistence.load_all())
            .await
            .map_err(|error| format!("draft state load task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        let mut restored = 0usize;
        let mut failures = Vec::new();
        for envelope in envelopes {
            if guild_ids.is_some_and(|guild_ids| !guild_ids.contains(&envelope.guild_id)) {
                continue;
            }
            // One guild's durable row must not abort recovery for every other
            // guild: a deleted draft message here would leave every later guild
            // unhydrated and, because ready_recovery aborts, unfinalized too.
            let envelope_guild_id = envelope.guild_id;
            let hydrated: Result<bool, String> = async {
                let guild_id = envelope.guild_id;
                let session_id = envelope.session_id;
                let initial_kind = to_runtime_kind(envelope.state.as_state().lobby_kind);
                let operation_lock = self.handler.draft_operation_lock(guild_id, session_id);
                let _operation_guard = operation_lock.lock().await;
                let lobby_operation_lock = self.handler.lobbies.operation_lock(guild_id, initial_kind);
                let _lobby_operation_guard = lobby_operation_lock.lock().await;
                let envelope = match self.handler.reload_durable_envelope(guild_id).await {
                    Ok(Some(envelope)) => envelope,
                    Ok(None) => {
                        let live_same_session = self
                            .handler
                            .drafts
                            .get_state(Some(guild_id))
                            .is_some_and(|state| lock_state(&state).session_id == session_id);
                        self.handler.cleanup_unused_draft_operation_lock(
                            guild_id,
                            session_id,
                            &operation_lock,
                        );
                        if live_same_session {
                            return Err(format!(
                                "live draft session {session_id} has no durable recovery row"
                            ));
                        }
                        return Ok(false);
                    }
                    Err(error) => {
                        self.handler.cleanup_unused_draft_operation_lock(
                            guild_id,
                            session_id,
                            &operation_lock,
                        );
                        return Err(error);
                    }
                };
                if envelope.session_id != session_id {
                    self.handler.cleanup_unused_draft_operation_lock(
                        guild_id,
                        session_id,
                        &operation_lock,
                    );
                    return Err(format!(
                        "draft guild {guild_id} changed sessions during hydration: loaded {session_id}, current {}",
                        envelope.session_id
                    ));
                }
                if !envelope.active
                    || envelope.finalizing
                    || envelope.pending_match_id.is_some()
                    || envelope.state.as_state().phase == DraftPhase::Complete
                {
                    let live_same_session = self
                        .handler
                        .drafts
                        .get_state(Some(guild_id))
                        .is_some_and(|state| lock_state(&state).session_id == session_id);
                    self.handler.cleanup_unused_draft_operation_lock(
                        guild_id,
                        session_id,
                        &operation_lock,
                    );
                    if live_same_session && (envelope.finalizing || envelope.pending_match_id.is_some())
                    {
                        // A same-process completion may still own the live
                        // Draft while READY fires. Leave that state intact; the
                        // READY observer invokes the durable recovery worker
                        // immediately after hydration and its terminal CAS owns
                        // the eventual local cleanup.
                        warn!(
                            guild_id,
                            session_id, "live finalizing Draft retained for READY recovery"
                        );
                        return Ok(false);
                    }
                    if live_same_session {
                        return Err(format!(
                            "live draft session {session_id} now requires finalization recovery"
                        ));
                    }
                    // TODO(draft-persistence): implement idempotent pending-match
                    // finalization recovery before hydrating these rows.
                    warn!(
                        guild_id,
                        session_id,
                        finalizing = envelope.finalizing,
                        pending_match_id = ?envelope.pending_match_id,
                        "skipping draft row that requires finalization recovery"
                    );
                    return Ok(false);
                }
                let state = envelope.state.clone().into_state();
                let kind = to_runtime_kind(state.lobby_kind);
                if kind != initial_kind {
                    self.handler.cleanup_unused_draft_operation_lock(
                        guild_id,
                        session_id,
                        &operation_lock,
                    );
                    return Err(format!(
                        "draft guild {guild_id} changed lobby scope during hydration"
                    ));
                }
                let existing = self.handler.drafts.get_state(Some(guild_id));
                let (handle, live_state, active_envelope, freshly_restored) = if let Some(existing) =
                    existing
                {
                    let existing_session = lock_state(&existing).session_id;
                    if existing_session != session_id {
                        self.handler.cleanup_unused_draft_operation_lock(
                            guild_id,
                            session_id,
                            &operation_lock,
                        );
                        return Err(format!(
                            "draft guild {guild_id} already has session {existing_session}, refusing to hydrate stale session {session_id}"
                        ));
                    }
                    let Some(cached) = lock_recover(&self.handler.persisted_envelopes)
                        .get(&(guild_id, session_id))
                        .cloned()
                    else {
                        return Err(format!(
                            "draft session {session_id} is live without a hydrated CAS envelope"
                        ));
                    };
                    if envelope != cached {
                        return Err(format!(
                            "draft session {session_id} durable envelope does not match the live CAS cache: cached revision {}, durable revision {}",
                            cached.revision, envelope.revision
                        ));
                    }
                    // Repeated READY is strictly idempotent from the already-live
                    // state. Never overwrite an in-flight or freshly committed
                    // mutation with the load snapshot.
                    let live_state = lock_state(&existing).clone();
                    if live_state.to_snapshot() != cached.state {
                        return Err(format!(
                            "draft session {session_id} live state does not match its CAS cache"
                        ));
                    }
                    (existing, live_state, cached, false)
                } else {
                    let handle = self
                        .handler
                        .reserve_and_restore_hydrated_state(&state, &operation_lock)?;
                    lock_recover(&self.handler.persisted_envelopes)
                        .insert((guild_id, session_id), envelope.clone());
                    (handle, state.clone(), envelope, true)
                };
                self.handler.restore_hydrated_receipt(&live_state);
                if let Err(error) = self.handler.republish_hydrated_view(&live_state).await {
                    if freshly_restored {
                        self.handler
                            .unwind_fresh_hydration(&handle, &live_state, &operation_lock);
                    }
                    return Err(format!(
                        "draft recovery repaint failed for guild {guild_id}, session {session_id}: {error}"
                    ));
                }
                let seconds = active_envelope
                    .deadline_at
                    .map(|deadline| deadline.saturating_sub(unix_now()).max(1) as u64)
                    .unwrap_or_else(|| {
                        if lock_state(&handle).phase == DraftPhase::Drafting {
                            DRAFTING_TIMEOUT_SECONDS
                        } else {
                            PRE_DRAFT_TIMEOUT_SECONDS
                        }
                    });
                self.handler.schedule_timeout(guild_id, session_id, seconds);
                Ok(true)
            }
            .await;
            match hydrated {
                Ok(true) => restored += 1,
                Ok(false) => {}
                Err(error) => failures.push(format!("guild {}: {error}", envelope_guild_id)),
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "Draft hydration failed for {} row(s): {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        Ok(restored)
    }

    /// Expose the shared manager for composition and readiness diagnostics.
    #[must_use]
    pub fn state_manager(&self) -> Arc<DraftStateManager> {
        Arc::clone(&self.handler.drafts)
    }
}

impl RegistrationProvider for DraftRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "draft".to_owned(),
            description: "Start or inspect an Immortal Draft captain mode".to_owned(),
            options: draft_options(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: DRAFT_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

#[derive(Clone, Debug)]
struct DraftConfig {
    admin_user_ids: BTreeSet<i64>,
    ready_threshold: usize,
    bet_lock_seconds: i64,
    bomb_pot_chance: f64,
    bomb_pot_ante: i64,
    bomb_pot_blind_percentage: f64,
    auto_blind_enabled: bool,
    auto_blind_threshold: i64,
    auto_blind_percentage: f64,
    auto_spectator_bet_enabled: bool,
    auto_spectator_bet_count: usize,
    auto_spectator_bet_percentage: f64,
    auto_spectator_bet_top_count: usize,
    auto_spectator_bet_top_percentage: f64,
    max_debt: i64,
    dota_bet_seed_amount: i64,
    first_game_pool_daily_amount: i64,
}

impl DraftConfig {
    fn from_application(config: &ApplicationConfig) -> Self {
        Self {
            admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            ready_threshold: usize::try_from(config.values.lobby_ready_threshold).unwrap_or(10),
            bet_lock_seconds: config.values.bet_lock_seconds,
            bomb_pot_chance: config.values.bomb_pot_chance,
            bomb_pot_ante: config.values.bomb_pot_ante,
            bomb_pot_blind_percentage: config.values.bomb_pot_blind_percentage,
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
            dota_bet_seed_amount: config.values.dota_bet_seed_amount,
            first_game_pool_daily_amount: config.values.first_game_pool_daily_amount,
        }
    }
}

struct DraftHandler {
    config: DraftConfig,
    database_path: std::path::PathBuf,
    players: PlayerRepository,
    pending: PendingMatchRepository,
    seeds: DotaBetSeedRepository,
    bets: BettingServiceRepository,
    investments: AutobetInvestmentRepository,
    lobbies: MatchLobbyPort,
    drafts: Arc<DraftStateManager>,
    discord: Arc<dyn DiscordTransport>,
    reminders: Arc<dyn DraftReminderScheduler>,
    neon: Arc<dyn DraftNeonObserver>,
    service: Mutex<cama_app::draft::DraftService>,
    draft_messages: Arc<Mutex<BTreeMap<(i64, u64), DiscordMessageReceipt>>>,
    timeout_tasks: Mutex<BTreeMap<i64, JoinHandle<()>>>,
    completing: Mutex<BTreeSet<(i64, u64)>>,
    persistence: Option<Arc<dyn DraftStatePersistencePort>>,
    persisted_envelopes: Arc<Mutex<BTreeMap<(i64, u64), DraftStateEnvelope>>>,
    draft_operation_locks: DraftOperationLocks,
}

#[derive(Debug)]
struct LinkedDraftFinalization {
    pending: PendingMatchRecord,
    plan: DraftFinalizationPlan,
}

struct DraftGatewayObserver {
    provider: DraftRegistrationProvider,
    ready_lock: Arc<tokio::sync::Mutex<()>>,
}

#[async_trait]
impl GatewayEventObserver for DraftGatewayObserver {
    fn name(&self) -> &'static str {
        "draft-state-recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let _ready_guard = self.ready_lock.lock().await;
        let mut report = ReadyRecoveryReport::empty(self.name(), context.guild_ids().len());
        let guild_ids = context
            .guild_ids()
            .iter()
            .filter_map(|guild_id| i64::try_from(*guild_id).ok())
            .collect::<BTreeSet<_>>();
        match self.provider.hydrate_for_guilds(&guild_ids).await {
            Ok(_restored) => {
                if let Err(error) = self
                    .provider
                    .recover_finalizing_for_guilds(&guild_ids)
                    .await
                {
                    for guild_id in context.guild_ids() {
                        report.failures.push(ReadyRecoveryFailure {
                            guild_id: *guild_id,
                            message: error.clone(),
                        });
                    }
                    return report;
                }
                // Hydration loads the single canonical database and filters
                // rows by their guild IDs internally.  A successful pass is
                // therefore a successful recovery attempt for every guild in
                // this gateway snapshot, even when a guild has no draft row.
                report.guilds_refreshed = context.guild_ids().len();
            }
            Err(error) => {
                for guild_id in context.guild_ids() {
                    report.failures.push(ReadyRecoveryFailure {
                        guild_id: *guild_id,
                        message: error.clone(),
                    });
                }
            }
        }
        report
    }
}

#[async_trait]
impl InteractionHandler for DraftHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                name,
                user_id,
                user_display_name,
                guild_id,
                channel_id,
                member_permissions,
                options,
                ..
            } => {
                let user_id = signed_id(user_id, "user")?;
                let guild_id = guild_id.map(|id| signed_id(id, "guild")).transpose()?;
                let channel_id = channel_id.map(|id| signed_id(id, "channel")).transpose()?;
                if name != "draft" {
                    return Err(format!("draft handler received command {name:?}").into());
                }
                self.handle_command(
                    user_id,
                    user_display_name,
                    guild_id,
                    channel_id,
                    member_permissions,
                    options,
                    responder,
                )
                .await
                .map_err(Into::into)
            }
            InteractionRequest::Component {
                custom_id,
                user_id,
                user_display_name,
                guild_id,
                channel_id,
                member_permissions,
                ..
            } => {
                let user_id = signed_id(user_id, "user")?;
                let guild_id = guild_id.map(|id| signed_id(id, "guild")).transpose()?;
                let channel_id = channel_id.map(|id| signed_id(id, "channel")).transpose()?;
                self.handle_component(
                    user_id,
                    user_display_name,
                    guild_id,
                    channel_id,
                    member_permissions,
                    &custom_id,
                    responder,
                )
                .await
                .map_err(Into::into)
            }
            _ => Err("draft extension only handles commands and components".into()),
        }
    }
}

impl DraftHandler {
    fn player_names(&self, guild_id: i64, player_ids: &[i64]) -> GuildPlayerNameDirectory {
        let discord_player_ids = player_ids
            .iter()
            .filter_map(|player_id| u64::try_from(*player_id).ok())
            .collect::<Vec<_>>();
        let nicknames = u64::try_from(guild_id)
            .ok()
            .filter(|guild_id| *guild_id != 0)
            .and_then(|guild_id| {
                self.discord
                    .cached_guild_member_server_nicknames(guild_id, &discord_player_ids)
                    .ok()
            })
            .flatten();
        GuildPlayerNameDirectory::new(nicknames)
    }

    fn render_state(&self, state: &DraftState) -> DraftState {
        draft_render_state(
            state,
            &self.player_names(state.guild_id, &state.player_pool_ids),
        )
    }

    fn render_choice(
        &self,
        state: &DraftState,
        title: &str,
        description: &str,
    ) -> InteractionResponse {
        choice_embed(&self.render_state(state), title, description)
    }

    fn render_live(&self, state: &DraftState) -> (InteractionEmbed, Vec<InteractionActionRow>) {
        let state = self.render_state(state);
        (live_embed(&state), drafting_view(&state))
    }

    fn render_completion(
        &self,
        state: &DraftState,
        pending: &PendingMatchRecord,
    ) -> InteractionEmbed {
        completion_embed(&self.render_state(state), pending)
    }

    fn draft_operation_lock(&self, guild_id: i64, session_id: u64) -> Arc<tokio::sync::Mutex<()>> {
        lock_recover(&self.draft_operation_locks)
            .entry((guild_id, session_id))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn remove_draft_operation_lock(&self, guild_id: i64, session_id: u64) {
        lock_recover(&self.draft_operation_locks).remove(&(guild_id, session_id));
    }

    /// Drop an operation-lock entry only when it no longer protects a live
    /// session and no other callback has already cloned that same lock.
    fn cleanup_unused_draft_operation_lock(
        &self,
        guild_id: i64,
        session_id: u64,
        operation_lock: &Arc<tokio::sync::Mutex<()>>,
    ) {
        cleanup_unused_draft_operation_lock_entry(
            &self.draft_operation_locks,
            &self.drafts,
            guild_id,
            session_id,
            operation_lock,
        );
    }

    async fn reload_durable_envelope(
        &self,
        guild_id: i64,
    ) -> Result<Option<DraftStateEnvelope>, String> {
        let Some(persistence) = self.persistence.clone() else {
            return Ok(None);
        };
        spawn_blocking(move || persistence.load_all())
            .await
            .map_err(|error| format!("draft state reload task failed: {error}"))?
            .map_err(|error| error.to_string())
            .map(|envelopes| {
                envelopes
                    .into_iter()
                    .find(|envelope| envelope.guild_id == guild_id)
            })
    }

    /// Reserve a recovered draft's pool and publish its handle as one logical
    /// startup step. The caller holds both the draft-session and lobby-scope
    /// operation locks. If manager insertion loses a race, unwind the newly
    /// acquired reservation before returning the error.
    fn reserve_and_restore_hydrated_state(
        &self,
        state: &DraftState,
        operation_lock: &Arc<tokio::sync::Mutex<()>>,
    ) -> Result<Arc<Mutex<DraftState>>, String> {
        let guild_id = state.guild_id;
        let session_id = state.session_id;
        let kind = to_runtime_kind(state.lobby_kind);
        let player_ids = state
            .player_pool_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let reserved = if player_ids.is_empty() {
            false
        } else if self.lobbies.reserve_players(guild_id, kind, &player_ids) {
            true
        } else {
            self.cleanup_unused_draft_operation_lock(guild_id, session_id, operation_lock);
            return Err(format!(
                "could not re-reserve hydrated draft players for guild {guild_id}"
            ));
        };
        match self.drafts.restore_draft(state.clone()) {
            Ok(handle) => Ok(handle),
            Err(error) => {
                if reserved {
                    self.lobbies.release_players(guild_id, kind, &player_ids);
                }
                self.cleanup_unused_draft_operation_lock(guild_id, session_id, operation_lock);
                Err(format!("could not restore draft {guild_id}: {error}"))
            }
        }
    }

    /// Undo only process-local effects of a failed first hydration.  The
    /// durable row is intentionally retained so READY can retry recovery.
    fn unwind_fresh_hydration(
        &self,
        handle: &Arc<Mutex<DraftState>>,
        state: &DraftState,
        operation_lock: &Arc<tokio::sync::Mutex<()>>,
    ) {
        let key = (state.guild_id, state.session_id);
        self.cancel_timeout(state.guild_id);
        lock_recover(&self.draft_messages).remove(&key);
        lock_recover(&self.persisted_envelopes).remove(&key);
        self.drafts.clear_state(Some(state.guild_id), Some(handle));
        let player_ids = state
            .player_pool_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if !player_ids.is_empty() {
            self.lobbies.release_players(
                state.guild_id,
                to_runtime_kind(state.lobby_kind),
                &player_ids,
            );
        }
        self.cleanup_unused_draft_operation_lock(state.guild_id, state.session_id, operation_lock);
    }

    fn restore_hydrated_receipt(&self, state: &DraftState) {
        let key = (state.guild_id, state.session_id);
        let ids = (
            state.draft_channel_id.and_then(|id| u64::try_from(id).ok()),
            state.draft_message_id.and_then(|id| u64::try_from(id).ok()),
        );
        if let (Some(channel_id), Some(message_id)) = ids {
            lock_recover(&self.draft_messages).insert(
                key,
                DiscordMessageReceipt {
                    channel_id,
                    message_id,
                    jump_url: format!(
                        "https://discord.com/channels/{}/{channel_id}/{message_id}",
                        state.guild_id
                    ),
                },
            );
        } else {
            lock_recover(&self.draft_messages).remove(&key);
        }
    }

    /// Repaint a recovered source message from durable state.  Message ids
    /// are part of the authoritative snapshot; the receipt cache is rebuilt
    /// by [`DraftRegistrationProvider::hydrate`] so normal completion cleanup
    /// still knows which source message to remove.
    async fn republish_hydrated_view(&self, state: &DraftState) -> Result<(), String> {
        let (Some(channel_id), Some(message_id)) = (state.draft_channel_id, state.draft_message_id)
        else {
            return Err("durable draft has no source message identity".to_owned());
        };
        let (Ok(channel_id), Ok(message_id)) = (to_u64(channel_id), to_u64(message_id)) else {
            return Err("durable draft has an invalid Discord message identity".to_owned());
        };
        let rendered_state = self.render_state(state);
        let state = &rendered_state;
        let response = match state.phase {
            DraftPhase::WinnerChoice => InteractionResponse::message("")
                .embed(opening_embed(state, "Recovered after restart"))
                .action_rows(pre_choice_view(
                    state.session_id,
                    state.coinflip_winner_id.unwrap_or_default(),
                    false,
                    true,
                )),
            DraftPhase::WinnerSideChoice => {
                choice_embed(state, "Choose Your Side", "Pick Radiant or Dire.").action_rows(
                    pre_choice_view(
                        state.session_id,
                        state.coinflip_winner_id.unwrap_or_default(),
                        true,
                        false,
                    ),
                )
            }
            DraftPhase::WinnerHeroChoice => choice_embed(
                state,
                "Choose Hero Pick Order",
                "Pick First or Second hero pick (in-game).",
            )
            .action_rows(pre_choice_view(
                state.session_id,
                other_captain(state, state.coinflip_winner_id.unwrap_or_default())
                    .unwrap_or_default(),
                false,
                false,
            )),
            DraftPhase::LoserChoice => {
                let loser = other_captain(state, state.coinflip_winner_id.unwrap_or_default())
                    .unwrap_or_default();
                let side_choice = state.winner_choice_type.as_deref() == Some("hero_pick");
                (if side_choice {
                    choice_embed(state, "Choose Your Side", "Pick Radiant or Dire.")
                } else {
                    choice_embed(
                        state,
                        "Choose Hero Pick Order",
                        "Pick First or Second hero pick (in-game).",
                    )
                })
                .action_rows(pre_choice_view(
                    state.session_id,
                    loser,
                    side_choice,
                    false,
                ))
            }
            DraftPhase::Drafting => InteractionResponse::message("")
                .embed(live_embed(state))
                .action_rows(drafting_view(state)),
            DraftPhase::Complete => {
                return Err("completed draft requires finalization recovery".to_owned());
            }
            DraftPhase::Coinflip => {
                return Err("pre-publication draft requires startup recovery".to_owned());
            }
        };
        self.discord
            .edit_message(channel_id, message_id, DiscordMessage::silent(response))
            .await
            .map_err(|error| format!("Discord source edit failed: {error}"))
    }

    /// Create the durable row before any component-bearing Discord message is
    /// published.  SQLite owns the session identity; the returned snapshot is
    /// adopted by the manager before the first custom id is rendered.
    async fn persist_create(
        &self,
        handle: &Arc<Mutex<DraftState>>,
        deadline_at: Option<i64>,
    ) -> Result<DraftStateEnvelope, String> {
        let snapshot = lock_state(handle).clone();
        let mut requested = snapshot.to_snapshot().envelope(1);
        requested.deadline_at = deadline_at;
        let Some(persistence) = self.persistence.clone() else {
            return Ok(requested);
        };
        let returned = spawn_blocking(move || persistence.create_envelope(&requested))
            .await
            .map_err(|error| format!("draft persistence create task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if returned.guild_id != snapshot.guild_id {
            return Err("draft persistence returned the wrong guild".to_owned());
        }
        *lock_state(handle) = returned.state.clone().into_state();
        lock_recover(&self.persisted_envelopes)
            .insert((returned.guild_id, returned.session_id), returned.clone());
        Ok(returned)
    }

    fn cached_deadline(&self, state: &DraftState) -> Option<i64> {
        lock_recover(&self.persisted_envelopes)
            .get(&(state.guild_id, state.session_id))
            .and_then(|envelope| envelope.deadline_at)
    }

    /// CAS a state transition before its corresponding gateway publication.
    /// On failure the process-local state is rolled back so a backend outage
    /// cannot make callbacks observe a transition that was never durable.
    async fn persist_transition(
        &self,
        handle: &Arc<Mutex<DraftState>>,
        previous: DraftState,
        next: DraftState,
        deadline_at: Option<i64>,
        finalizing: bool,
    ) -> Result<(), String> {
        let Some(persistence) = self.persistence.clone() else {
            return Ok(());
        };
        let key = (next.guild_id, next.session_id);
        let Some(current) = lock_recover(&self.persisted_envelopes).get(&key).cloned() else {
            *lock_state(handle) = previous;
            return Err(format!(
                "draft persistence session {} is not hydrated",
                next.session_id
            ));
        };
        let mut requested = current.clone();
        requested.deadline_at = deadline_at;
        requested.finalizing = finalizing;
        requested.pending_match_id = None;
        requested.state = next.to_snapshot();
        let expected_revision = current.revision;
        let returned = spawn_blocking(move || {
            persistence.replace_envelope_if_revision(key.0, key.1, expected_revision, &requested)
        })
        .await
        .map_err(|error| format!("draft persistence replace task failed: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()));
        match returned {
            Ok(returned) => {
                lock_recover(&self.persisted_envelopes).insert(key, returned);
                Ok(())
            }
            Err(error) => {
                *lock_state(handle) = previous;
                Err(error)
            }
        }
    }

    async fn delete_persisted(&self, state: &DraftState) -> Result<bool, String> {
        let Some(persistence) = self.persistence.clone() else {
            return Ok(true);
        };
        let key = (state.guild_id, state.session_id);
        let Some(current) = lock_recover(&self.persisted_envelopes).get(&key).cloned() else {
            return Ok(false);
        };
        let expected_revision = current.revision;
        let deleted =
            spawn_blocking(move || persistence.delete_if_revision(key.0, key.1, expected_revision))
                .await
                .map_err(|error| format!("draft persistence delete task failed: {error}"))?
                .map_err(|error| error.to_string())?;
        if deleted {
            lock_recover(&self.persisted_envelopes).remove(&key);
        }
        Ok(deleted)
    }

    async fn complete_persisted_finalization(
        &self,
        state: &DraftState,
        pending_match_id: i64,
    ) -> Result<(), String> {
        let Some(persistence) = self.persistence.clone() else {
            return Ok(());
        };
        let guild_id = state.guild_id;
        let session_id = state.session_id;
        let key = (guild_id, session_id);
        let current = lock_recover(&self.persisted_envelopes)
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("draft persistence session {session_id} is not hydrated"))?;
        if !current.finalizing || current.pending_match_id != Some(pending_match_id) {
            return Err(
                "draft finalization envelope is not linked to the pending match".to_owned(),
            );
        }
        let expected_draft_revision = current.revision;
        let now = unix_now();
        spawn_blocking(move || {
            persistence.complete_finalization_job(
                guild_id,
                session_id,
                pending_match_id,
                expected_draft_revision,
                now,
            )
        })
        .await
        .map_err(|error| format!("draft finalization completion task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        lock_recover(&self.persisted_envelopes).remove(&key);
        Ok(())
    }

    /// Fence a created pending match before removing the process-local draft.
    /// Hydration intentionally skips this marker until a complete
    /// reconciliation protocol exists, so a crash cannot replay Discord
    /// publication as if the draft were still interactive.
    async fn mark_finalizing(
        &self,
        state: &DraftState,
        pending_match_id: Option<i64>,
    ) -> Result<(), String> {
        let Some(persistence) = self.persistence.clone() else {
            return Ok(());
        };
        let key = (state.guild_id, state.session_id);
        let Some(current) = lock_recover(&self.persisted_envelopes).get(&key).cloned() else {
            return Err(format!(
                "draft persistence session {} is not hydrated",
                state.session_id
            ));
        };
        if current.finalizing
            && current.pending_match_id == pending_match_id
            && current.state.as_state() == state
        {
            return Ok(());
        }
        if pending_match_id.is_none()
            && current.finalizing
            && current.pending_match_id.is_some()
            && current.state.as_state() == state
        {
            // A retry that has already crossed the atomic pending-link seam
            // must never clear the recovered match identity.
            return Ok(());
        }
        let mut requested = current;
        requested.state = state.to_snapshot();
        requested.deadline_at = None;
        requested.finalizing = true;
        requested.pending_match_id = pending_match_id;
        let expected_revision = requested.revision;
        let returned = spawn_blocking(move || {
            persistence.replace_envelope_if_revision(key.0, key.1, expected_revision, &requested)
        })
        .await
        .map_err(|error| format!("draft finalization marker task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        lock_recover(&self.persisted_envelopes).insert(key, returned);
        Ok(())
    }

    async fn link_finalizing_pending_match(
        &self,
        state: &DraftState,
        pending_state: Option<&PendingMatchState>,
        lobby_channel_id: Option<u64>,
        origin_channel_id: Option<u64>,
        thread_id: Option<u64>,
    ) -> Result<LinkedDraftFinalization, String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let key = (state.guild_id, state.session_id);
        let current = lock_recover(&self.persisted_envelopes)
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "draft persistence session {} is not hydrated",
                    state.session_id
                )
            })?;
        if let Some(pending_match_id) = current.pending_match_id {
            let pending_persistence = Arc::clone(&persistence);
            let guild_id = state.guild_id;
            let session_id = state.session_id;
            let pending_payload_json = spawn_blocking(move || {
                pending_persistence.linked_pending_payload(guild_id, session_id, pending_match_id)
            })
            .await
            .map_err(|error| format!("draft raw pending-load task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            let plan_persistence = Arc::clone(&persistence);
            let plan_json = spawn_blocking(move || {
                plan_persistence.linked_finalization_plan(guild_id, session_id, pending_match_id)
            })
            .await
            .map_err(|error| format!("draft raw finalization-plan load task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            let plan =
                DraftFinalizationPlan::from_json(&plan_json).map_err(|error| error.to_string())?;
            if plan.schema_version != cama_app::draft::DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION {
                return Err("linked Draft finalization plan is not schema v2".to_owned());
            }
            let guild_id = state.guild_id;
            let session_id = state.session_id;
            let stage_loader = Arc::clone(&persistence);
            let stage = spawn_blocking(move || {
                stage_loader.finalizing_financial_setup_stage(
                    guild_id,
                    session_id,
                    pending_match_id,
                )
            })
            .await
            .map_err(|error| format!("draft finalization stage load task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            if stage == DraftFinancialSetupStage::Linked {
                // The plan hash is immutable at the link boundary.  The
                // financial executor deliberately mutates the pending row,
                // so a retry after setup must validate only identity below.
                plan.validate(&pending_payload_json)
                    .map_err(|error| error.to_string())?;
            }
            if plan
                .financial_setup
                .as_ref()
                .map(|financial| financial.pending_match_id)
                != Some(pending_match_id)
            {
                return Err("linked Draft financial plan names another pending match".to_owned());
            }
            let pending = self
                .pending
                .pending_match(state.guild_id, pending_match_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    format!(
                        "atomically linked pending match {pending_match_id} could not be reloaded"
                    )
                })?;
            return Ok(LinkedDraftFinalization { pending, plan });
        }

        let mut frozen_pending = pending_state
            .ok_or_else(|| "first Draft finalization writer has no pending-match state".to_owned())?
            .clone();
        frozen_pending.origin_channel_id = origin_channel_id.and_then(|id| i64::try_from(id).ok());
        frozen_pending.thread_shuffle_thread_id = thread_id.and_then(|id| i64::try_from(id).ok());
        let pending_payload_json =
            serde_json::to_string(&frozen_pending).map_err(|error| error.to_string())?;
        let shuffle_timestamp = frozen_pending
            .shuffle_timestamp
            .ok_or_else(|| "Draft pending match has no shuffle timestamp".to_owned())?;
        let bet_lock_until = frozen_pending
            .bet_lock_until
            .ok_or_else(|| "Draft pending match has no bet-lock timestamp".to_owned())?;
        let policy = self.financial_setup_policy(&frozen_pending)?;
        let seed = DraftFinalizationPlanSeed::new(
            state.guild_id,
            state.session_id,
            &pending_payload_json,
            shuffle_timestamp,
            shuffle_timestamp,
            bet_lock_until,
            frozen_pending.is_bomb_pot,
            state.lobby_kind,
            state.draft_channel_id,
            state.draft_message_id,
            signed_destination_id(lobby_channel_id, "lobby channel")?,
            signed_destination_id(origin_channel_id, "origin channel")?,
            signed_destination_id(thread_id, "lobby thread")?,
            policy,
        )
        .map_err(|error| error.to_string())?;
        let plan_seed_json = seed
            .to_json(&pending_payload_json)
            .map_err(|error| error.to_string())?;
        let expected_revision = current.revision;
        let guild_id = state.guild_id;
        let session_id = state.session_id;
        let linked = spawn_blocking(move || {
            persistence.link_finalizing_pending_match_v2(
                guild_id,
                session_id,
                expected_revision,
                &pending_payload_json,
                &plan_seed_json,
            )
        })
        .await
        .map_err(|error| format!("draft pending-link task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if linked.envelope.guild_id != state.guild_id
            || linked.envelope.session_id != state.session_id
            || linked.envelope.pending_match_id != Some(linked.pending_match_id)
        {
            return Err("draft pending-link result does not match the active session".to_owned());
        }
        let plan = DraftFinalizationPlan::from_json(&linked.plan_json)
            .map_err(|error| error.to_string())?;
        plan.validate(&linked.pending_payload_json)
            .map_err(|error| error.to_string())?;
        if plan.schema_version != cama_app::draft::DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION
            || plan
                .financial_setup
                .as_ref()
                .map(|financial| financial.pending_match_id)
                != Some(linked.pending_match_id)
        {
            return Err("Draft v2 pending-link returned the wrong financial plan".to_owned());
        }
        lock_recover(&self.persisted_envelopes).insert(key, linked.envelope);
        let pending = self
            .pending
            .pending_match(state.guild_id, linked.pending_match_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "atomically linked pending match {} could not be reloaded",
                    linked.pending_match_id
                )
            })?;
        Ok(LinkedDraftFinalization { pending, plan })
    }

    fn financial_setup_policy(
        &self,
        pending: &PendingMatchState,
    ) -> Result<DraftFinancialSetupPolicy, String> {
        let shuffle_timestamp = pending
            .shuffle_timestamp
            .ok_or_else(|| "Draft pending match has no shuffle timestamp".to_owned())?;
        let game_date = cama_domain::game_date::game_date_for_timestamp(shuffle_timestamp as f64)
            .map_err(|error| error.to_string())?;
        let spectator_total_count =
            u64::try_from(self.config.auto_spectator_bet_count).map_err(|_| {
                "Draft spectator total count exceeds the financial plan range".to_owned()
            })?;
        let spectator_top_count = u64::try_from(self.config.auto_spectator_bet_top_count)
            .map_err(|_| "Draft spectator top count exceeds the financial plan range".to_owned())?;
        let policy = DraftFinancialSetupPolicy {
            schema_version: DRAFT_FINANCIAL_SETUP_PLAN_VERSION,
            game_date,
            betting_mode: pending.betting_mode.clone(),
            seed_max_amount: self.config.dota_bet_seed_amount,
            first_game_daily_amount: self.config.first_game_pool_daily_amount,
            auto_blind_enabled: self.config.auto_blind_enabled,
            normal_blind_threshold: self.config.auto_blind_threshold,
            normal_blind_percentage_bits: f64_bits(self.config.auto_blind_percentage),
            bomb_blind_percentage_bits: f64_bits(self.config.bomb_pot_blind_percentage),
            bomb_ante: self.config.bomb_pot_ante,
            max_debt: self.config.max_debt,
            spectator_enabled: self.config.auto_spectator_bet_enabled,
            spectator_total_count,
            spectator_top_count,
            spectator_base_percentage_bits: f64_bits(self.config.auto_spectator_bet_percentage),
            spectator_top_percentage_bits: f64_bits(self.config.auto_spectator_bet_top_percentage),
            extensions: BTreeMap::new(),
        };
        policy.validate().map_err(|error| error.to_string())?;
        Ok(policy)
    }

    async fn run_persisted_financial_setup(
        &self,
        state: &DraftState,
        pending_match_id: i64,
    ) -> Result<(), String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let guild_id = state.guild_id;
        let session_id = state.session_id;
        let stage_loader = Arc::clone(&persistence);
        let stage = spawn_blocking(move || {
            stage_loader.finalizing_financial_setup_stage(guild_id, session_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("Draft financial stage task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if stage == DraftFinancialSetupStage::SetupComplete {
            return Ok(());
        }
        let now = unix_now();
        let result = spawn_blocking(move || {
            persistence.run_finalizing_financial_setup(guild_id, session_id, pending_match_id, now)
        })
        .await
        .map_err(|error| format!("Draft financial setup task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if result.guild_id != guild_id
            || result.session_id != session_id
            || result.pending_match_id != pending_match_id
        {
            return Err("Draft financial setup returned the wrong job identity".to_owned());
        }
        Ok(())
    }

    async fn resume_finalizing_financial_setup(
        &self,
        guild_id: i64,
    ) -> Result<PendingMatchRecord, String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let loader = Arc::clone(&persistence);
        let envelope = spawn_blocking(move || loader.load_all())
            .await
            .map_err(|error| format!("Draft finalization resume load task failed: {error}"))?
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|envelope| envelope.guild_id == guild_id)
            .ok_or_else(|| format!("draft does not exist for guild {guild_id}"))?;
        let pending_match_id = envelope
            .pending_match_id
            .filter(|_| envelope.finalizing)
            .ok_or_else(|| "Draft envelope is not linked for finalization".to_owned())?;
        let session_id = envelope.session_id;
        let raw_loader = Arc::clone(&persistence);
        let _pending_payload = spawn_blocking(move || {
            raw_loader.linked_pending_payload(guild_id, session_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("Draft resume pending-load task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let plan_loader = Arc::clone(&persistence);
        let plan_json = spawn_blocking(move || {
            plan_loader.linked_finalization_plan(guild_id, session_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("Draft resume plan-load task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let plan =
            DraftFinalizationPlan::from_json(&plan_json).map_err(|error| error.to_string())?;
        if plan.schema_version != cama_app::draft::DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION {
            return Err("linked Draft finalization plan is not schema v2".to_owned());
        }
        if plan
            .financial_setup
            .as_ref()
            .map(|financial| financial.pending_match_id)
            != Some(pending_match_id)
        {
            return Err("linked Draft financial plan names another pending match".to_owned());
        }
        let state = envelope.state.clone().into_state();
        self.run_persisted_financial_setup(&state, pending_match_id)
            .await?;
        self.pending
            .pending_match(guild_id, pending_match_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("linked pending match {pending_match_id} disappeared"))
    }

    async fn recover_finalizing_envelope(
        &self,
        envelope: DraftStateEnvelope,
    ) -> Result<(), String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let guild_id = envelope.guild_id;
        let session_id = envelope.session_id;
        let pending_match_id = envelope
            .pending_match_id
            .filter(|_| envelope.finalizing)
            .ok_or_else(|| {
                format!(
                    "draft guild {guild_id} has recovery metadata without a finalizing pending match"
                )
            })?;
        let pending_payload_loader = Arc::clone(&persistence);
        let pending_payload_json = spawn_blocking(move || {
            pending_payload_loader.linked_pending_payload(guild_id, session_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("Draft recovery pending-load task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let plan_loader = Arc::clone(&persistence);
        let plan_json = spawn_blocking(move || {
            plan_loader.linked_finalization_plan(guild_id, session_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("Draft recovery plan-load task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let plan = DraftFinalizationPlan::from_json(&plan_json)
            .map_err(|error| format!("Draft recovery plan is invalid: {error}"))?;
        if plan.schema_version != cama_app::draft::DRAFT_FINALIZATION_PLAN_SCHEMA_VERSION {
            return Err("linked Draft finalization plan is not schema v2".to_owned());
        }
        if plan
            .financial_setup
            .as_ref()
            .map(|financial| financial.pending_match_id)
            != Some(pending_match_id)
        {
            return Err("linked Draft financial plan names another pending match".to_owned());
        }
        let state = envelope.state.clone().into_state();
        let job_loader = Arc::clone(&persistence);
        let mut job = spawn_blocking(move || {
            job_loader.finalization_job(guild_id, session_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("Draft recovery job-load task failed: {error}"))?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "linked Draft finalization job is missing".to_owned())?;
        if job.stage == cama_db::draft_finalization::DRAFT_FINALIZATION_COMPLETE_STAGE {
            return Err("linked Draft envelope still exists beside a terminal job".to_owned());
        }

        if job.stage == cama_db::draft_finalization::DRAFT_FINALIZATION_INITIAL_STAGE {
            // The immutable plan hashes the payload at the link boundary.
            // Financial setup intentionally mutates that pending row, so a
            // post-setup restart must validate identity/stage without
            // comparing the evolved payload to the original hash again.
            plan.validate(&pending_payload_json)
                .map_err(|error| format!("Draft recovery pending payload is invalid: {error}"))?;
            self.run_persisted_financial_setup(&state, pending_match_id)
                .await?;
            let job_loader = Arc::clone(&persistence);
            job = spawn_blocking(move || {
                job_loader.finalization_job(guild_id, session_id, pending_match_id)
            })
            .await
            .map_err(|error| format!("Draft recovery setup reload task failed: {error}"))?
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Draft finalization job disappeared after financial setup".to_owned())?;
        }
        if !matches!(
            job.stage.as_str(),
            cama_app::draft::DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE
                | DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE
        ) {
            return Err(format!(
                "Draft finalization job is at unexpected recovery stage {}",
                job.stage
            ));
        }

        let owner = draft_finalization_process_owner(guild_id, session_id);
        let now = unix_now();
        if job.lease_owner.as_deref() != Some(owner.as_str())
            || job.lease_until.is_none_or(|until| until <= now)
        {
            let claim_persistence = Arc::clone(&persistence);
            let claim_owner = owner.clone();
            let lease_until = now
                .checked_add(DRAFT_FINALIZATION_LEASE_SECONDS)
                .ok_or_else(|| "Draft finalization recovery lease overflowed".to_owned())?;
            job = spawn_blocking(move || {
                claim_persistence.claim_finalization_job(
                    guild_id,
                    session_id,
                    pending_match_id,
                    job.revision,
                    &claim_owner,
                    now,
                    lease_until,
                )
            })
            .await
            .map_err(|error| format!("Draft recovery lease task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        }

        let pending = self
            .pending
            .pending_match(guild_id, pending_match_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("linked pending match {pending_match_id} disappeared"))?;
        self.reconcile_finalizing_lobby(&state).await;
        if job.stage == DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE {
            self.schedule_persisted_betting_reminders(&mut job, &owner, pending.clone(), true)
                .await?;
            self.complete_recovered_finalization(&state, pending_match_id, envelope.revision)
                .await?;
            return Ok(());
        }

        let embed = self.render_completion(&state, &pending);
        let source_receipt = self
            .reconcile_source_delivery(&mut job, &owner, &plan, &state, &embed)
            .await?;
        let lobby_receipt = self
            .reconcile_delivery(
                &mut job,
                &owner,
                &plan.lobby,
                Self::embed_message(&embed),
                "lobby",
                plan.started_at,
            )
            .await?;
        let origin_receipt = self
            .reconcile_delivery(
                &mut job,
                &owner,
                &plan.origin,
                Self::embed_message(&embed),
                "origin",
                plan.started_at,
            )
            .await?;
        let (thread_embed_receipt, thread_ping_receipt) = self
            .reconcile_thread_deliveries(&mut job, &owner, &plan, &state, &embed)
            .await?;

        let mut published = Vec::new();
        if let Some(receipt) = lobby_receipt.as_ref() {
            published.push((receipt.channel_id, receipt.clone()));
        }
        if let Some(receipt) = origin_receipt.as_ref() {
            published.push((receipt.channel_id, receipt.clone()));
        }
        self.store_completion_message_info(CompletionMessageContext {
            guild_id,
            pending_match_id,
            state: &state,
            published: &published,
            thread_receipt: thread_embed_receipt.as_ref(),
            lobby_channel_id: plan.lobby.channel_id.and_then(|id| u64::try_from(id).ok()),
            origin_channel_id: plan.origin.channel_id.and_then(|id| u64::try_from(id).ok()),
            draft_receipt: source_receipt.as_ref(),
        })?;
        let _ = thread_ping_receipt;
        self.schedule_persisted_betting_reminders(&mut job, &owner, pending.clone(), true)
            .await?;
        let completion_persistence = Arc::clone(&persistence);
        let completed_job = spawn_blocking(move || {
            completion_persistence.advance_finalization_job(
                guild_id,
                session_id,
                pending_match_id,
                job.revision,
                &job.stage,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                &json!({"publication": {"completed_at": unix_now()}}).to_string(),
                &owner,
                unix_now(),
            )
        })
        .await
        .map_err(|error| format!("Draft publication stage task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if completed_job.stage != DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE {
            return Err("Draft publication stage CAS returned the wrong stage".to_owned());
        }
        self.complete_recovered_finalization(&state, pending_match_id, envelope.revision)
            .await
    }

    /// A final-pick callback can persist the Complete fence before its
    /// interaction defer succeeds, leaving no pending-match identity.  The
    /// exact durable player/captain state is sufficient to remove that final
    /// pick and safely reopen the interactive draft; this avoids a permanent
    /// manual-only recovery window without inventing financial timestamps.
    async fn recover_unlinked_finalization_envelope(
        &self,
        envelope: DraftStateEnvelope,
        operation_lock: &Arc<tokio::sync::Mutex<()>>,
    ) -> Result<(), String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let guild_id = envelope.guild_id;
        let session_id = envelope.session_id;
        let restored = rollback_final_pick_state(envelope.state.as_state())?;
        let mut requested = envelope.clone();
        requested.active = true;
        requested.finalizing = false;
        requested.pending_match_id = None;
        requested.deadline_at = Some(
            unix_now()
                .checked_add(DRAFTING_TIMEOUT_SECONDS as i64)
                .ok_or_else(|| "Draft recovery deadline overflowed".to_owned())?,
        );
        requested.state = restored.to_snapshot();
        let expected_revision = envelope.revision;
        let returned = spawn_blocking(move || {
            persistence.replace_envelope_if_revision(
                guild_id,
                session_id,
                expected_revision,
                &requested,
            )
        })
        .await
        .map_err(|error| format!("Draft unlinked-fence rollback task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let key = (guild_id, session_id);
        if let Some(handle) = self.drafts.get_state(Some(guild_id)) {
            if lock_state(&handle).session_id == session_id {
                *lock_state(&handle) = restored.clone();
            } else {
                self.reserve_and_restore_hydrated_state(&restored, operation_lock)?;
            }
        } else {
            self.reserve_and_restore_hydrated_state(&restored, operation_lock)?;
        }
        lock_recover(&self.persisted_envelopes).insert(key, returned);
        self.restore_hydrated_receipt(&restored);
        if let Err(error) = self.republish_hydrated_view(&restored).await {
            warn!(%error, guild_id, session_id, "reopened Draft view repaint failed after final-pick rollback");
        }
        self.schedule_timeout(guild_id, session_id, DRAFTING_TIMEOUT_SECONDS);
        Ok(())
    }

    async fn reconcile_finalizing_lobby(&self, state: &DraftState) {
        let kind = to_runtime_kind(state.lobby_kind);
        let matched = state
            .radiant_player_ids
            .iter()
            .chain(&state.dire_player_ids)
            .copied()
            .collect::<BTreeSet<_>>();
        if let Err(error) = self
            .lobbies
            .evict_from_other_lobby(state.guild_id, kind, &matched)
            .await
        {
            warn!(%error, guild_id = state.guild_id, "Draft recovery sibling-lobby eviction failed");
        }
        if let Err(error) = self.lobbies.reset_after_shuffle(state.guild_id, kind).await {
            warn!(%error, guild_id = state.guild_id, "Draft recovery lobby reset failed");
        }
        self.lobbies.release_players(
            state.guild_id,
            kind,
            &state.player_pool_ids.iter().copied().collect(),
        );
    }

    /// Publish a persistence-backed completion through the durable receipt
    /// ledger. Normal command completion uses this same path as READY
    /// recovery, so a failed lobby/origin/thread operation is returned to the
    /// caller and the terminal Draft CAS remains retryable.
    async fn publish_persisted_completion(
        &self,
        state: &DraftState,
        pending: &PendingMatchRecord,
        plan: &DraftFinalizationPlan,
    ) -> Result<(), String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let guild_id = state.guild_id;
        let session_id = state.session_id;
        let pending_match_id = pending.pending_match_id;
        let job_loader = Arc::clone(&persistence);
        let mut job = spawn_blocking(move || {
            job_loader.finalization_job(guild_id, session_id, pending_match_id)
        })
        .await
        .map_err(|error| format!("Draft publication job-load task failed: {error}"))?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Draft finalization publication job is missing".to_owned())?;
        if job.stage == DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE {
            return Ok(());
        }
        if job.stage != DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE {
            return Err(format!(
                "Draft finalization publication started at unexpected stage {}",
                job.stage
            ));
        }
        let owner = draft_finalization_process_owner(guild_id, session_id);
        let now = unix_now();
        if job.lease_owner.as_deref() != Some(owner.as_str())
            || job.lease_until.is_none_or(|until| until <= now)
        {
            let claim_persistence = Arc::clone(&persistence);
            let claim_owner = owner.clone();
            let lease_until = now
                .checked_add(DRAFT_FINALIZATION_LEASE_SECONDS)
                .ok_or_else(|| "Draft publication lease overflowed".to_owned())?;
            job = spawn_blocking(move || {
                claim_persistence.claim_finalization_job(
                    guild_id,
                    session_id,
                    pending_match_id,
                    job.revision,
                    &claim_owner,
                    now,
                    lease_until,
                )
            })
            .await
            .map_err(|error| format!("Draft publication lease task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        }
        let embed = self.render_completion(state, pending);
        let source_receipt = self
            .reconcile_source_delivery(&mut job, &owner, plan, state, &embed)
            .await?;
        let lobby_receipt = self
            .reconcile_delivery(
                &mut job,
                &owner,
                &plan.lobby,
                Self::embed_message(&embed),
                "lobby",
                plan.started_at,
            )
            .await?;
        let origin_receipt = self
            .reconcile_delivery(
                &mut job,
                &owner,
                &plan.origin,
                Self::embed_message(&embed),
                "origin",
                plan.started_at,
            )
            .await?;
        let (thread_embed_receipt, _thread_ping_receipt) = self
            .reconcile_thread_deliveries(&mut job, &owner, plan, state, &embed)
            .await?;
        let published = lobby_receipt
            .into_iter()
            .chain(origin_receipt)
            .map(|receipt| (receipt.channel_id, receipt))
            .collect::<Vec<_>>();
        self.store_completion_message_info(CompletionMessageContext {
            guild_id,
            pending_match_id,
            state,
            published: &published,
            thread_receipt: thread_embed_receipt.as_ref(),
            lobby_channel_id: plan.lobby.channel_id.and_then(|id| u64::try_from(id).ok()),
            origin_channel_id: plan.origin.channel_id.and_then(|id| u64::try_from(id).ok()),
            draft_receipt: source_receipt.as_ref(),
        })?;
        self.schedule_persisted_betting_reminders(&mut job, &owner, pending.clone(), false)
            .await?;
        let completion_persistence = Arc::clone(&persistence);
        let completed_job = spawn_blocking(move || {
            completion_persistence.advance_finalization_job(
                guild_id,
                session_id,
                pending_match_id,
                job.revision,
                &job.stage,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                &json!({"publication": {"completed_at": unix_now()}}).to_string(),
                &owner,
                unix_now(),
            )
        })
        .await
        .map_err(|error| format!("Draft publication stage task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if completed_job.stage != DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE {
            return Err("Draft publication stage CAS returned the wrong stage".to_owned());
        }
        Ok(())
    }

    async fn schedule_persisted_betting_reminders(
        &self,
        job: &mut DraftFinalizationJobSnapshot,
        owner: &str,
        pending: PendingMatchRecord,
        recovery: bool,
    ) -> Result<(), String> {
        let progress: Value = serde_json::from_str(&job.progress_json)
            .map_err(|error| format!("Draft reminder progress is invalid: {error}"))?;
        let reminder_marker = progress
            .get("reminders")
            .and_then(Value::as_object)
            .and_then(|reminders| reminders.get("scheduled"))
            .and_then(Value::as_bool)
            == Some(true);
        if recovery {
            self.reminders
                .schedule_betting_reminders_recovery(pending)
                .await
                .map_err(|error| {
                    format!("Draft recovery betting reminder scheduling failed: {error}")
                })?;
            if reminder_marker {
                return Ok(());
            }
            self.persist_progress(job, owner, &json!({"reminders": {"scheduled": true}}))
                .await?;
            return Ok(());
        }
        if reminder_marker {
            return Ok(());
        }
        {
            self.reminders
                .schedule_betting_reminders(pending)
                .await
                .map_err(|error| format!("Draft betting reminder scheduling failed: {error}"))?;
        }
        self.persist_progress(job, owner, &json!({"reminders": {"scheduled": true}}))
            .await
    }

    async fn complete_recovered_finalization(
        &self,
        state: &DraftState,
        pending_match_id: i64,
        expected_draft_revision: u64,
    ) -> Result<(), String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let guild_id = state.guild_id;
        let session_id = state.session_id;
        spawn_blocking(move || {
            persistence.complete_finalization_job(
                guild_id,
                session_id,
                pending_match_id,
                expected_draft_revision,
                unix_now(),
            )
        })
        .await
        .map_err(|error| format!("Draft recovery terminal task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if let Some(handle) = self.drafts.get_state(Some(guild_id)) {
            let live_state = lock_state(&handle).clone();
            if live_state.session_id == session_id {
                self.finish_draft_without_match(&handle, &live_state);
            }
        }
        lock_recover(&self.draft_messages).remove(&(guild_id, session_id));
        self.cancel_timeout(guild_id);
        self.remove_draft_operation_lock(guild_id, session_id);
        lock_recover(&self.persisted_envelopes).remove(&(guild_id, session_id));
        Ok(())
    }

    async fn reconcile_source_delivery(
        &self,
        job: &mut DraftFinalizationJobSnapshot,
        owner: &str,
        plan: &DraftFinalizationPlan,
        state: &DraftState,
        embed: &InteractionEmbed,
    ) -> Result<Option<DiscordMessageReceipt>, String> {
        if let Some(delivery) = persisted_delivery(&job.progress_json, "source") {
            return Ok(delivery);
        }
        let receipt = match (
            plan.source.channel_id.and_then(|id| u64::try_from(id).ok()),
            plan.source.message_id.and_then(|id| u64::try_from(id).ok()),
        ) {
            (Some(channel_id), Some(message_id)) => {
                let edited = async {
                    if self
                        .discord
                        .fetch_message(channel_id, message_id)
                        .await?
                        .is_none()
                    {
                        // The plan's message id is immutable, so a deleted
                        // source message can never be edited into success.
                        return Ok(false);
                    }
                    self.discord
                        .edit_message(
                            channel_id,
                            message_id,
                            DiscordMessage::default_mentions(
                                InteractionResponse::message("").embed(embed.clone()),
                            ),
                        )
                        .await?;
                    Ok::<bool, String>(true)
                }
                .await;
                match edited {
                    Ok(true) => Some(DiscordMessageReceipt {
                        channel_id,
                        message_id,
                        jump_url: discord_jump_url(state.guild_id, channel_id, message_id),
                    }),
                    Ok(false) => None,
                    Err(error) => {
                        self.skip_or_fail(channel_id, "source", job.guild_id, &error)
                            .await?;
                        None
                    }
                }
            }
            _ => None,
        };
        self.persist_delivery(job, owner, "source", receipt.clone())
            .await?;
        Ok(receipt)
    }

    /// Whether a failed delivery to `channel_id` may be recorded as a
    /// permanent skip.
    ///
    /// A finalization plan freezes its channel ids at link time, so a
    /// destination Discord reports as gone or inaccessible can never be
    /// retried into success -- and because the publication CAS and the
    /// terminal job/envelope delete both sit behind these deliveries, a
    /// permanent failure leaves the guild's `finalizing` envelope in place and
    /// blocks every later `/draft start`. Anything Discord does not prove
    /// permanent stays retryable.
    async fn skip_or_fail(
        &self,
        channel_id: u64,
        name: &str,
        guild_id: i64,
        error: &str,
    ) -> Result<(), String> {
        if self.discord.destination_status(channel_id).await
            == DiscordDestinationStatus::Undeliverable
        {
            warn!(
                %error,
                channel_id,
                delivery = name,
                guild_id,
                "draft finalization destination is permanently undeliverable; recording a skip"
            );
            return Ok(());
        }
        Err(error.to_owned())
    }

    /// Perform one plan-frozen delivery, without deciding what a failure means.
    async fn attempt_delivery(
        &self,
        channel_id: u64,
        delivery: &cama_app::draft::DraftFinalizationDeliveryPlan,
        message: DiscordMessage,
        name: &str,
        after_unix_seconds: i64,
    ) -> Result<DiscordMessageReceipt, String> {
        if let Some(message_id) = delivery.message_id.and_then(|id| u64::try_from(id).ok()) {
            self.discord
                .fetch_message(channel_id, message_id)
                .await?
                .ok_or_else(|| {
                    format!(
                        "Discord {name} message {channel_id}/{message_id} disappeared before recovery"
                    )
                })?;
            self.discord
                .edit_message(channel_id, message_id, message)
                .await?;
            return Ok(DiscordMessageReceipt {
                channel_id,
                message_id,
                jump_url: discord_jump_url(0, channel_id, message_id),
            });
        }
        if let Some(receipt) = self
            .discord
            .find_message_by_delivery_key(
                channel_id,
                &delivery.delivery_key,
                after_unix_seconds,
                500,
            )
            .await?
        {
            return Ok(receipt);
        }
        match self
            .discord
            .send_message_with_delivery_key(channel_id, &delivery.delivery_key, message)
            .await
        {
            Ok(receipt) => Ok(receipt),
            // Discord may have accepted a nonce-bearing request whose response
            // was lost, so history is checked before the failure is reported.
            Err(send_error) => self
                .discord
                .find_message_by_delivery_key(
                    channel_id,
                    &delivery.delivery_key,
                    after_unix_seconds,
                    500,
                )
                .await?
                .ok_or(send_error),
        }
    }

    async fn reconcile_delivery(
        &self,
        job: &mut DraftFinalizationJobSnapshot,
        owner: &str,
        delivery: &cama_app::draft::DraftFinalizationDeliveryPlan,
        message: DiscordMessage,
        name: &str,
        after_unix_seconds: i64,
    ) -> Result<Option<DiscordMessageReceipt>, String> {
        if let Some(receipt) = persisted_delivery(&job.progress_json, name) {
            return Ok(receipt);
        }
        let Some(channel_id) = delivery.channel_id.and_then(|id| u64::try_from(id).ok()) else {
            self.persist_delivery(job, owner, name, None).await?;
            return Ok(None);
        };
        let receipt = match self
            .attempt_delivery(channel_id, delivery, message, name, after_unix_seconds)
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                self.skip_or_fail(channel_id, name, job.guild_id, &error)
                    .await?;
                self.persist_delivery(job, owner, name, None).await?;
                return Ok(None);
            }
        };
        self.persist_delivery(job, owner, name, Some(receipt.clone()))
            .await?;
        Ok(Some(receipt))
    }

    fn embed_message(embed: &InteractionEmbed) -> DiscordMessage {
        DiscordMessage::default_mentions(InteractionResponse::message("").embed(embed.clone()))
    }

    async fn reconcile_thread_deliveries(
        &self,
        job: &mut DraftFinalizationJobSnapshot,
        owner: &str,
        plan: &DraftFinalizationPlan,
        state: &DraftState,
        embed: &InteractionEmbed,
    ) -> Result<(Option<DiscordMessageReceipt>, Option<DiscordMessageReceipt>), String> {
        let Some(thread_id) = plan
            .thread_embed
            .channel_id
            .and_then(|id| u64::try_from(id).ok())
        else {
            self.persist_delivery(job, owner, "thread-embed", None)
                .await?;
            self.persist_delivery(job, owner, "thread-ping", None)
                .await?;
            return Ok((None, None));
        };
        let name = format!(
            "🔒 {} Draft Complete - Awaiting Results",
            to_runtime_kind(state.lobby_kind).label()
        );
        if let Err(error) = self
            .discord
            .edit_thread(thread_id, &name, false, false)
            .await
        {
            // An unreachable thread cannot host either delivery, and stranding
            // them would keep the guild's envelope finalizing forever.
            self.skip_or_fail(thread_id, "thread-embed", job.guild_id, &error)
                .await?;
            self.persist_delivery(job, owner, "thread-embed", None)
                .await?;
            self.persist_delivery(job, owner, "thread-ping", None)
                .await?;
            return Ok((None, None));
        }
        let embed_receipt = self
            .reconcile_delivery(
                job,
                owner,
                &plan.thread_embed,
                Self::embed_message(embed),
                "thread-embed",
                plan.started_at,
            )
            .await?;
        let mentions = state
            .radiant_player_ids
            .iter()
            .chain(&state.dire_player_ids)
            .copied()
            .filter(|id| *id > 0)
            .filter_map(|id| u64::try_from(id).ok())
            .collect::<Vec<_>>();
        let content = mentions
            .iter()
            .map(|id| format!("<@{id}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let ping_receipt = if content.is_empty() {
            self.persist_delivery(job, owner, "thread-ping", None)
                .await?;
            None
        } else {
            let message = DiscordMessage::mentioning(
                InteractionResponse::message(format!(
                    "{content}\nPlayers, please take your starting positions!"
                )),
                mentions.iter().copied().collect(),
            );
            self.reconcile_delivery(
                job,
                owner,
                &plan.thread_ping,
                message,
                "thread-ping",
                plan.started_at,
            )
            .await?
        };
        if let Err(error) = self
            .discord
            .edit_thread(thread_id, &name, false, true)
            .await
        {
            // Publication already succeeded; a cosmetic re-lock must never
            // wedge the terminal CAS.
            warn!(
                %error,
                thread_id,
                guild_id = job.guild_id,
                "draft finalization could not re-lock its thread"
            );
        }
        Ok((embed_receipt, ping_receipt))
    }

    async fn persist_delivery(
        &self,
        job: &mut DraftFinalizationJobSnapshot,
        owner: &str,
        name: &str,
        receipt: Option<DiscordMessageReceipt>,
    ) -> Result<(), String> {
        let patch = if let Some(receipt) = receipt {
            json!({
                "deliveries": {
                    name: {
                        "channel_id": receipt.channel_id,
                        "message_id": receipt.message_id,
                        "jump_url": receipt.jump_url,
                    }
                }
            })
        } else {
            json!({"deliveries": {name: {"skipped": true}}})
        };
        self.persist_progress(job, owner, &patch).await
    }

    async fn persist_progress(
        &self,
        job: &mut DraftFinalizationJobSnapshot,
        owner: &str,
        patch: &Value,
    ) -> Result<(), String> {
        let persistence = self
            .persistence
            .clone()
            .ok_or_else(|| "draft persistence is not configured".to_owned())?;
        let guild_id = job.guild_id;
        let session_id = job.session_id;
        let pending_match_id = job.pending_match_id;
        let expected_revision = job.revision;
        let expected_stage = job.stage.clone();
        let owner = owner.to_owned();
        let now = unix_now();
        let patch_json = patch.to_string();
        let advanced = spawn_blocking(move || {
            persistence.advance_finalization_job(
                guild_id,
                session_id,
                pending_match_id,
                expected_revision,
                &expected_stage,
                &expected_stage,
                &patch_json,
                &owner,
                now,
            )
        })
        .await
        .map_err(|error| format!("Draft delivery progress task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        *job = advanced;
        Ok(())
    }

    async fn send_neon_followup(
        &self,
        responder: &Arc<dyn InteractionResponder>,
        result: DraftNeonResult,
    ) {
        if result.text_block.is_empty() && result.attachment.is_none() {
            return;
        }
        let mut response = InteractionResponse::message(result.text_block);
        if let Some(attachment) = result.attachment {
            response = response.attachment(attachment);
        }
        if let Err(error) = responder.followup(response).await {
            warn!(%error, "draft Neon Discord follow-up failed");
        }
    }

    async fn emit_captain_symmetry(
        &self,
        guild_id: i64,
        handle: &Arc<Mutex<DraftState>>,
        responder: &Arc<dyn InteractionResponder>,
    ) {
        let state = lock_state(handle).clone();
        let (Some(radiant_captain_id), Some(dire_captain_id)) =
            (state.radiant_captain_id, state.dire_captain_id)
        else {
            return;
        };
        let rating_diff = (state.captain1_rating - state.captain2_rating)
            .abs()
            .floor() as i64;
        if rating_diff > 50 {
            return;
        }
        match self
            .neon
            .on_captain_symmetry(guild_id, radiant_captain_id, dire_captain_id, rating_diff)
            .await
        {
            Ok(Some(result)) => self.send_neon_followup(responder, result).await,
            Ok(None) => {}
            Err(error) => warn!(%error, guild_id, "draft captain-symmetry Neon hook failed"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_command(
        &self,
        user_id: i64,
        user_display_name: String,
        guild_id: Option<i64>,
        channel_id: Option<i64>,
        permissions: Option<u64>,
        options: Vec<InteractionOption>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let (subcommand, sub_options) = leaf_command(&options)?;
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(&responder, "This command must be used in a server.").await;
        };
        match subcommand {
            "start" => {
                let explicit_kind = optional_lobby_kind(sub_options)?;
                let captain1 = optional_user(sub_options, "captain1")?;
                let captain2 = optional_user(sub_options, "captain2")?;
                self.start(
                    StartContext {
                        user_id,
                        guild_id,
                        channel_id: channel_id.unwrap_or_default(),
                        explicit_kind,
                        captain1,
                        captain2,
                    },
                    responder,
                )
                .await
            }
            "restart" => {
                self.restart(
                    guild_id,
                    user_id,
                    user_display_name,
                    is_admin(&self.config, user_id, permissions),
                    responder,
                )
                .await
            }
            "sampleinprogress" => {
                self.sample_in_progress(guild_id, user_id, permissions, responder)
                    .await
            }
            "samplecomplete" => {
                self.sample_complete(guild_id, user_id, permissions, responder)
                    .await
            }
            other => Err(format!("unknown /draft subcommand {other:?}")),
        }
    }

    async fn start(
        &self,
        context: StartContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let memberships = self
            .lobbies
            .lobby_kinds_for_player(context.guild_id, context.user_id);
        let kind = match resolve_lobby_kind(&memberships, context.explicit_kind) {
            Ok(kind) => kind,
            Err(message) => return respond_ephemeral(&responder, message).await,
        };
        let lock = self.lobbies.operation_lock(context.guild_id, kind);
        let Ok(_guard) = tokio::time::timeout(OPERATION_LOCK_TIMEOUT, lock.lock()).await else {
            // Nothing has acknowledged this interaction yet, so the reply has
            // to be the initial response; a followup on an unacknowledged
            // interaction is rejected and the user sees nothing.
            return respond_ephemeral(
                &responder,
                format!(
                    "A {} shuffle or draft is already being processed. Please wait.",
                    kind.label()
                ),
            )
            .await;
        };

        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;

        if self.drafts.has_active_draft(Some(context.guild_id)) {
            return followup_ephemeral(
                &responder,
                "❌ A draft is already in progress. Use `/draft restart` to restart it first.",
            )
            .await;
        }
        let Some(snapshot) = self.lobbies.snapshot(context.guild_id, kind) else {
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ No active {} lobby. Use `/lobby` to create one first.",
                    kind.label()
                ),
            )
            .await;
        };
        if !snapshot.player_ids.contains(&context.user_id) {
            return followup_ephemeral(
                &responder,
                &format!("❌ Join {} before using `/draft start`.", kind.label()),
            )
            .await;
        }
        if snapshot.player_ids.len() < self.config.ready_threshold {
            return followup_ephemeral(
                &responder,
                &format!(
                    "❌ {} needs at least {} players. Currently have {}.",
                    kind.label(),
                    self.config.ready_threshold,
                    snapshot.player_ids.len()
                ),
            )
            .await;
        }
        if let Some(captain) = context.captain1
            && !snapshot.player_ids.contains(&captain)
        {
            return followup_ephemeral(
                &responder,
                &format!("❌ Captain {captain} is not in the lobby."),
            )
            .await;
        }
        if let Some(captain) = context.captain2
            && !snapshot.player_ids.contains(&captain)
        {
            return followup_ephemeral(
                &responder,
                &format!("❌ Captain {captain} is not in the lobby."),
            )
            .await;
        }
        if context.captain1.is_some() && context.captain1 == context.captain2 {
            return followup_ephemeral(
                &responder,
                "❌ Cannot specify the same player as both captains.",
            )
            .await;
        }

        let progress = responder
            .followup_with_receipt(InteractionResponse::message(format!(
                "⚙️ {}: building the Immortal Draft player pool…",
                kind.label()
            )))
            .await
            .map_err(|error| error.to_string())?;
        let result = self
            .execute_start(&context, kind, snapshot, responder.clone(), progress)
            .await;
        if let Err(error) = result {
            warn!(%error, guild_id = context.guild_id, "draft start failed");
            return Err(error);
        }
        Ok(())
    }

    async fn execute_start(
        &self,
        context: &StartContext,
        kind: AppLobbyKind,
        snapshot: MatchLobbySnapshot,
        responder: Arc<dyn InteractionResponder>,
        progress: Option<crate::registration::InteractionMessageReceipt>,
    ) -> Result<(), String> {
        let guild_id = context.guild_id;
        let regular_ids = snapshot.player_ids.clone();
        let players = self
            .load_players(&regular_ids, guild_id)
            .await
            .map_err(|error| format!("could not load draft players: {error}"))?;
        let ratings = players
            .iter()
            .filter_map(|player| {
                player
                    .discord_id
                    .map(|id| (id, player.glicko_rating.unwrap_or(1500.0)))
            })
            .collect::<BTreeMap<_, _>>();
        let exclusions = self
            .players
            .get_exclusion_counts(&regular_ids, Some(guild_id))
            .map_err(|error| error.to_string())?
            .iter()
            .map(|(id, count)| (*id, *count))
            .collect::<BTreeMap<_, _>>();
        let captain_candidates = regular_ids.clone();
        let captain_pair = self
            .service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .select_captains(
                &captain_candidates,
                &ratings,
                context.captain1,
                context.captain2,
            )
            .map_err(|error| error.message)?;
        let pool = self
            .service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .select_player_pool(
                &regular_ids,
                &[],
                &exclusions,
                &ratings,
                &[captain_pair.captain1_id, captain_pair.captain2_id],
                cama_app::draft::DRAFT_POOL_SIZE,
            )
            .map_err(|error| error.message)?;
        let selected = pool.selected_ids.iter().copied().collect::<BTreeSet<_>>();
        if !self.lobbies.reserve_players(guild_id, kind, &selected) {
            delete_followup(&responder, progress).await;
            return followup_ephemeral(
                &responder,
                "❌ The lobby changed while the draft was starting. Please try again.",
            )
            .await;
        }

        let state = match self
            .drafts
            .create_draft(Some(guild_id), to_draft_kind(kind))
        {
            Ok(state) => state,
            Err(error) => {
                self.lobbies.release_players(guild_id, kind, &selected);
                delete_followup(&responder, progress).await;
                return followup_ephemeral(&responder, &format!("❌ {error}")).await;
            }
        };
        let setup_result = self
            .finish_start(
                context,
                kind,
                snapshot,
                players,
                pool,
                captain_pair,
                state.clone(),
                responder.clone(),
                progress,
            )
            .await;
        if let Err(error) = setup_result {
            let state_snapshot = lock_state(&state).clone();
            let session_id = state_snapshot.session_id;
            if let Err(cleanup_error) = self.delete_persisted(&state_snapshot).await {
                warn!(%cleanup_error, guild_id, session_id, "draft start persistence cleanup failed");
            }
            lock_recover(&self.draft_messages).remove(&(guild_id, session_id));
            self.drafts.clear_state(Some(guild_id), Some(&state));
            self.remove_draft_operation_lock(guild_id, session_id);
            self.lobbies.release_players(guild_id, kind, &selected);
            if let Some(message_id) = state_snapshot.captain_ping_message_id
                && let Some(channel_id) = state_snapshot.draft_channel_id
            {
                let _ = self
                    .discord
                    .delete_message(to_u64(channel_id)?, to_u64(message_id)?)
                    .await;
            }
            if let Some(message_id) = state_snapshot.draft_message_id
                && let Some(channel_id) = state_snapshot.draft_channel_id
            {
                let _ = self
                    .discord
                    .delete_message(to_u64(channel_id)?, to_u64(message_id)?)
                    .await;
            }
            delete_followup(&responder, progress).await;
            return Err(error);
        }
        let session_id = lock_state(&state).session_id;
        self.schedule_timeout(guild_id, session_id, PRE_DRAFT_TIMEOUT_SECONDS);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_start(
        &self,
        context: &StartContext,
        kind: AppLobbyKind,
        snapshot: MatchLobbySnapshot,
        players: Vec<Player>,
        pool: cama_app::draft::PoolSelectionResult,
        captain_pair: cama_app::draft::CaptainPair,
        state: Arc<Mutex<DraftState>>,
        responder: Arc<dyn InteractionResponder>,
        progress: Option<crate::registration::InteractionMessageReceipt>,
    ) -> Result<(), String> {
        let channel_id = snapshot
            .lobby_channel_id
            .or(snapshot.origin_channel_id)
            .map(|id| i64::try_from(id).map_err(|_| "draft channel exceeds SQLite range"))
            .transpose()?
            .unwrap_or(context.channel_id);
        let coinflip_winner = self
            .service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .coinflip(captain_pair.captain1_id, captain_pair.captain2_id);
        let opening = {
            let mut current = lock_state(&state);
            current.player_pool_ids = pool.selected_ids.clone();
            current.excluded_player_ids = pool.excluded_ids.clone();
            current.full_exclusion_increment_ids = pool
                .excluded_ids
                .iter()
                .copied()
                .filter(|id| snapshot.player_ids.contains(id))
                .collect();
            current.half_exclusion_increment_ids = Vec::new();
            current.captain1_id = Some(captain_pair.captain1_id);
            current.captain2_id = Some(captain_pair.captain2_id);
            current.captain1_rating = captain_pair.captain1_rating;
            current.captain2_rating = captain_pair.captain2_rating;
            current.draft_channel_id = Some(channel_id);
            current.player_pool_data = players
                .iter()
                .filter_map(|player| {
                    let id = player.discord_id?;
                    pool.selected_ids.contains(&id).then(|| {
                        (
                            id,
                            PlayerPoolEntry {
                                name: player.name.clone(),
                                rating: player.glicko_rating.unwrap_or(1500.0),
                                roles: player.preferred_roles.clone().unwrap_or_default(),
                            },
                        )
                    })
                })
                .collect();
            current.coinflip_winner_id = Some(coinflip_winner);
            current.phase = DraftPhase::WinnerChoice;
            let rendered = self.render_state(&current);
            let starter_stored = players
                .iter()
                .find(|player| player.discord_id == Some(context.user_id))
                .map_or_else(
                    || format!("User {}", context.user_id),
                    |player| player.name.clone(),
                );
            let starter = self
                .player_names(context.guild_id, &[context.user_id])
                .resolve(context.user_id, &starter_stored)
                .to_owned();
            opening_embed(&rendered, &starter)
        };

        // Persist the complete setup before deleting the progress response or
        // publishing a component-bearing Discord message.  The database may
        // replace the process-local session id here.
        let created = self
            .persist_create(
                &state,
                Some(unix_now().saturating_add(PRE_DRAFT_TIMEOUT_SECONDS as i64)),
            )
            .await?;
        let session_id = created.session_id;

        delete_followup(&responder, progress).await;
        let ping = self
            .discord
            .send_message(
                to_u64(channel_id)?,
                DiscordMessage::mentioning(
                    InteractionResponse::message(format!(
                        "<@{}> <@{}> {} draft starting!",
                        captain_pair.captain1_id,
                        captain_pair.captain2_id,
                        kind.label()
                    )),
                    BTreeSet::from([
                        to_u64(captain_pair.captain1_id)?,
                        to_u64(captain_pair.captain2_id)?,
                    ]),
                ),
            )
            .await;
        if let Ok(receipt) = ping {
            let previous = lock_state(&state).clone();
            let mut next = previous.clone();
            let receipt_id = match to_i64(receipt.message_id) {
                Ok(id) => id,
                Err(error) => {
                    let _ = self
                        .discord
                        .delete_message(receipt.channel_id, receipt.message_id)
                        .await;
                    return Err(error);
                }
            };
            next.captain_ping_message_id = Some(receipt_id);
            *lock_state(&state) = next.clone();
            if let Err(error) = self
                .persist_transition(&state, previous, next, created.deadline_at, false)
                .await
            {
                let _ = self
                    .discord
                    .delete_message(receipt.channel_id, receipt.message_id)
                    .await;
                return Err(error);
            }
        }
        let opening_view = pre_choice_view(session_id, coinflip_winner, false, true);
        let opening_receipt = self
            .discord
            .send_message(
                to_u64(channel_id)?,
                DiscordMessage::default_mentions(
                    InteractionResponse::message("")
                        .embed(opening)
                        .action_rows(opening_view),
                ),
            )
            .await
            .map_err(|error| format!("draft opening message failed: {error}"))?;
        let previous = lock_state(&state).clone();
        let mut next = previous.clone();
        let opening_message_id = match to_i64(opening_receipt.message_id) {
            Ok(id) => id,
            Err(error) => {
                let _ = self
                    .discord
                    .delete_message(opening_receipt.channel_id, opening_receipt.message_id)
                    .await;
                return Err(error);
            }
        };
        next.draft_message_id = Some(opening_message_id);
        *lock_state(&state) = next.clone();
        if let Err(error) = self
            .persist_transition(&state, previous, next.clone(), created.deadline_at, false)
            .await
        {
            let _ = self
                .discord
                .delete_message(opening_receipt.channel_id, opening_receipt.message_id)
                .await;
            return Err(error);
        }
        lock_recover(&self.draft_messages).insert((next.guild_id, session_id), opening_receipt);

        let loser_id = if coinflip_winner == captain_pair.captain1_id {
            captain_pair.captain2_id
        } else {
            captain_pair.captain1_id
        };
        match self
            .neon
            .on_draft_coinflip(context.guild_id, coinflip_winner, loser_id)
            .await
        {
            Ok(Some(result)) => {
                self.send_neon_followup(&responder, result).await;
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => {
                warn!(%error, guild_id = context.guild_id, "draft coinflip Neon hook failed");
                Ok(())
            }
        }
    }

    async fn restart(
        &self,
        guild_id: i64,
        user_id: i64,
        _user_name: String,
        admin: bool,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        // Lock waits and the persistence delete can outlast Discord's
        // three-second initial-response window, so the interaction is
        // acknowledged before any of that work begins.
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let Some(handle) = self.drafts.get_state(Some(guild_id)) else {
            return followup_ephemeral(&responder, "❌ No active draft to restart.").await;
        };
        let initial = lock_state(&handle).clone();
        let kind = to_runtime_kind(initial.lobby_kind);
        let draft_operation_lock = self.draft_operation_lock(guild_id, initial.session_id);
        let Ok(_draft_operation_guard) =
            tokio::time::timeout(OPERATION_LOCK_TIMEOUT, draft_operation_lock.lock()).await
        else {
            return followup_ephemeral(
                &responder,
                "A draft is already being processed. Please wait.",
            )
            .await;
        };
        let kind_lock = self.lobbies.operation_lock(guild_id, kind);
        let Ok(_guard) = tokio::time::timeout(OPERATION_LOCK_TIMEOUT, kind_lock.lock()).await
        else {
            return followup_ephemeral(
                &responder,
                "A draft is already being processed. Please wait.",
            )
            .await;
        };
        let Some(current) = self.drafts.get_state(Some(guild_id)) else {
            return followup_ephemeral(&responder, "❌ No active draft to restart.").await;
        };
        if !Arc::ptr_eq(&current, &handle) {
            return followup_ephemeral(
                &responder,
                "❌ This draft is no longer active — use the controls on the current draft.",
            )
            .await;
        }
        let snapshot = lock_state(&current).clone();
        if snapshot.session_id != initial.session_id {
            return followup_ephemeral(
                &responder,
                "❌ This draft is no longer active — use the controls on the current draft.",
            )
            .await;
        }
        let is_captain = [snapshot.captain1_id, snapshot.captain2_id]
            .into_iter()
            .flatten()
            .any(|captain| captain == user_id);
        if !admin && !is_captain {
            return followup_ephemeral(
                &responder,
                "❌ Only captains or server admins can restart the draft.",
            )
            .await;
        }
        if snapshot.phase == DraftPhase::Complete {
            return followup_ephemeral(
                &responder,
                "⏳ Draft results are still being finalized. Please wait.",
            )
            .await;
        }
        if self.persistence.is_some() {
            match self.delete_persisted(&snapshot).await {
                Ok(true) => {}
                Ok(false) => {
                    return followup_ephemeral(
                        &responder,
                        "❌ This draft is no longer active — use the controls on the current draft.",
                    )
                    .await;
                }
                Err(error) => {
                    warn!(%error, guild_id, session_id = snapshot.session_id, "draft restart persistence cleanup failed");
                    return followup_ephemeral(
                        &responder,
                        "⚠️ Draft restart could not be saved. Please try again.",
                    )
                    .await;
                }
            }
        }
        self.drafts.clear_state(Some(guild_id), Some(&handle));
        self.remove_draft_operation_lock(guild_id, snapshot.session_id);
        lock_recover(&self.draft_messages).remove(&(guild_id, snapshot.session_id));
        self.lobbies.release_players(
            guild_id,
            kind,
            &snapshot.player_pool_ids.iter().copied().collect(),
        );
        self.cancel_timeout(guild_id);
        if let (Some(channel_id), Some(message_id)) =
            (snapshot.draft_channel_id, snapshot.captain_ping_message_id)
        {
            let _ = self
                .discord
                .delete_message(to_u64(channel_id)?, to_u64(message_id)?)
                .await;
        }
        let players = self.players.clone();
        let stored_name = spawn_blocking(move || {
            players
                .get_by_id(user_id, Some(guild_id))
                .map_err(|error| error.to_string())
                .map(|player| {
                    player.map_or_else(|| format!("User {user_id}"), |player| player.name)
                })
        })
        .await
        .map_err(|error| format!("draft restart player-name task failed: {error}"))??;
        let user_name = self
            .player_names(guild_id, &[user_id])
            .resolve(user_id, &stored_name)
            .to_owned();
        responder
            .respond(InteractionResponse::message(format!(
                "🔄 **Draft Restarted** by {user_name}\n\nThe lobby has been preserved. Use `/draft start` to start a new draft."
            )))
            .await
            .map_err(|error| error.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_component(
        &self,
        user_id: i64,
        user_name: String,
        guild_id: Option<i64>,
        _channel_id: Option<i64>,
        permissions: Option<u64>,
        custom_id: &str,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(&responder, "This command must be used in a server.").await;
        };
        let component = parse_component(custom_id)?;
        let Some(handle) = self.drafts.get_state(Some(guild_id)) else {
            return respond_ephemeral(
                &responder,
                "❌ This draft is no longer active — use the controls on the current draft.",
            )
            .await;
        };
        let session = lock_state(&handle).session_id;
        if self.persistence.is_some() && component.session_id.is_none() {
            // Legacy ids have no durable session fence and may be replayed
            // after a restart.  Keep them accepted by the in-memory fixture
            // path, but reject them once the production persistence port is
            // installed.
            return respond_ephemeral(
                &responder,
                "❌ This draft control is no longer valid — use the controls on the current draft.",
            )
            .await;
        }
        if component
            .session_id
            .is_some_and(|expected| expected != session)
        {
            return respond_ephemeral(
                &responder,
                "❌ This draft is no longer active — use the controls on the current draft.",
            )
            .await;
        }
        let participant = {
            let state = lock_state(&handle);
            state.player_pool_ids.is_empty()
                || state.player_pool_ids.contains(&user_id)
                || state.captain1_id == Some(user_id)
                || state.captain2_id == Some(user_id)
        };
        if !participant {
            return respond_ephemeral(&responder, "❌ You are not part of this draft.").await;
        }
        let operation_lock = self.draft_operation_lock(guild_id, session);
        let Ok(_operation_guard) =
            tokio::time::timeout(OPERATION_LOCK_TIMEOUT, operation_lock.lock()).await
        else {
            self.cleanup_unused_draft_operation_lock(guild_id, session, &operation_lock);
            return respond_ephemeral(
                &responder,
                "A draft action is already being processed. Please wait.",
            )
            .await;
        };
        let Some(current) = self.drafts.get_state(Some(guild_id)) else {
            self.cleanup_unused_draft_operation_lock(guild_id, session, &operation_lock);
            return respond_ephemeral(
                &responder,
                "❌ This draft is no longer active — use the controls on the current draft.",
            )
            .await;
        };
        if !Arc::ptr_eq(&current, &handle) || lock_state(&current).session_id != session {
            self.cleanup_unused_draft_operation_lock(guild_id, session, &operation_lock);
            return respond_ephemeral(
                &responder,
                "❌ This draft is no longer active — use the controls on the current draft.",
            )
            .await;
        }
        match component.action {
            DraftComponent::ChoiceSide => {
                self.choice_side(guild_id, user_id, handle, &responder)
                    .await
            }
            DraftComponent::ChoiceHero => {
                self.choice_hero(guild_id, user_id, handle, &responder)
                    .await
            }
            DraftComponent::Side(side) => {
                self.choose_side(guild_id, user_id, handle, side, &responder)
                    .await
            }
            DraftComponent::Hero(order) => {
                self.choose_hero(guild_id, user_id, handle, order, &responder)
                    .await
            }
            DraftComponent::Pick(player_id) => {
                self.pick_player(guild_id, user_id, handle, player_id, user_name, &responder)
                    .await
            }
            DraftComponent::Preference(side) => {
                self.side_preference(guild_id, user_id, handle, side, &responder)
                    .await
            }
            DraftComponent::Unknown => {
                let _ = permissions;
                respond_ephemeral(&responder, "❌ This draft control is no longer valid.").await
            }
        }
    }

    async fn choice_side(
        &self,
        guild_id: i64,
        user_id: i64,
        handle: Arc<Mutex<DraftState>>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let initial = lock_state(&handle).clone();
        let phase = initial.phase;
        let valid =
            phase == DraftPhase::WinnerChoice && initial.coinflip_winner_id == Some(user_id);
        if phase != DraftPhase::WinnerChoice {
            return respond_ephemeral(responder, wrong_choice_message(phase)).await;
        }
        if !valid {
            let response = "Only the coinflip winner can make this choice.";
            return respond_ephemeral(responder, response).await;
        }
        let (session, embed, ping) = {
            let mut state = lock_state(&handle);
            let ping = state
                .captain_ping_message_id
                .zip(state.draft_channel_id)
                .map(|(message_id, channel_id)| (channel_id, message_id));
            state.captain_ping_message_id = None;
            state.winner_choice_type = Some("side".to_owned());
            state.phase = DraftPhase::WinnerSideChoice;
            (
                state.session_id,
                self.render_choice(&state, "Choose Your Side", "Pick Radiant or Dire."),
                ping,
            )
        };
        let next = lock_state(&handle).clone();
        self.persist_transition(
            &handle,
            initial,
            next,
            Some(unix_now().saturating_add(PRE_DRAFT_TIMEOUT_SECONDS as i64)),
            false,
        )
        .await?;
        if let Some((channel_id, message_id)) = ping {
            let _ = self
                .discord
                .delete_message(to_u64(channel_id)?, to_u64(message_id)?)
                .await;
        }
        self.cancel_timeout(guild_id);
        self.schedule_timeout(guild_id, session, PRE_DRAFT_TIMEOUT_SECONDS);
        responder
            .update(embed.action_rows(pre_choice_view(session, user_id, true, false)))
            .await
            .map_err(|error| error.to_string())
    }

    async fn choice_hero(
        &self,
        guild_id: i64,
        user_id: i64,
        handle: Arc<Mutex<DraftState>>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let initial = lock_state(&handle).clone();
        let phase = initial.phase;
        let valid =
            phase == DraftPhase::WinnerChoice && initial.coinflip_winner_id == Some(user_id);
        if phase != DraftPhase::WinnerChoice {
            return respond_ephemeral(responder, wrong_choice_message(phase)).await;
        }
        if !valid {
            let response = "Only the coinflip winner can make this choice.";
            return respond_ephemeral(responder, response).await;
        }
        let (session, loser, embed, ping) = {
            let mut state = lock_state(&handle);
            let ping = state
                .captain_ping_message_id
                .zip(state.draft_channel_id)
                .map(|(message_id, channel_id)| (channel_id, message_id));
            state.captain_ping_message_id = None;
            state.winner_choice_type = Some("hero_pick".to_owned());
            state.phase = DraftPhase::WinnerHeroChoice;
            let loser = other_captain(&state, user_id).unwrap_or_default();
            (
                state.session_id,
                loser,
                self.render_choice(
                    &state,
                    "Choose Hero Pick Order",
                    "Pick First or Second hero pick (in-game).",
                ),
                ping,
            )
        };
        let next = lock_state(&handle).clone();
        self.persist_transition(
            &handle,
            initial,
            next,
            Some(unix_now().saturating_add(PRE_DRAFT_TIMEOUT_SECONDS as i64)),
            false,
        )
        .await?;
        if let Some((channel_id, message_id)) = ping {
            let _ = self
                .discord
                .delete_message(to_u64(channel_id)?, to_u64(message_id)?)
                .await;
        }
        self.cancel_timeout(guild_id);
        self.schedule_timeout(guild_id, session, PRE_DRAFT_TIMEOUT_SECONDS);
        responder
            .update(embed.action_rows(pre_choice_view(session, loser, false, false)))
            .await
            .map_err(|error| error.to_string())
    }

    async fn choose_side(
        &self,
        guild_id: i64,
        user_id: i64,
        handle: Arc<Mutex<DraftState>>,
        side: Side,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let previous = lock_state(&handle).clone();
        let result: Result<(InteractionResponse, Option<u64>), &'static str> = {
            let mut state = lock_state(&handle);
            let is_winner = state.phase == DraftPhase::WinnerSideChoice;
            let expected_user = if is_winner {
                state.coinflip_winner_id
            } else if state.winner_choice_type.as_deref() == Some("hero_pick") {
                other_captain(&state, state.coinflip_winner_id.unwrap_or_default())
            } else {
                None
            };
            if expected_user != Some(user_id)
                || (!is_winner && state.phase != DraftPhase::LoserChoice)
            {
                Err(
                    if (is_winner && state.phase == DraftPhase::WinnerSideChoice)
                        || (!is_winner && state.phase == DraftPhase::LoserChoice)
                    {
                        "Only the designated captain can make this choice."
                    } else {
                        wrong_choice_message(state.phase)
                    },
                )
            } else if is_winner {
                let loser = other_captain(&state, user_id).unwrap_or_default();
                state.winner_choice_value = Some(side.as_str().to_owned());
                assign_captains(&mut state, user_id, loser, side);
                state.phase = DraftPhase::LoserChoice;
                let session = state.session_id;
                let embed = self.render_choice(
                    &state,
                    "Choose Hero Pick Order",
                    "The other captain picks First or Second hero pick (in-game).",
                );
                Ok((
                    embed.action_rows(pre_choice_view(session, loser, false, false)),
                    None,
                ))
            } else {
                let winner = state.coinflip_winner_id.unwrap_or_default();
                state.loser_choice_value = Some(side.as_str().to_owned());
                assign_captains(&mut state, user_id, winner, side);
                let session = state.session_id;
                complete_pre_draft(&mut state);
                let (embed, view) = self.render_live(&state);
                Ok((
                    InteractionResponse::message("")
                        .embed(embed)
                        .action_rows(view),
                    Some(session),
                ))
            }
        };
        let (response, drafting_session) = match result {
            Ok(result) => result,
            Err(message) => return respond_ephemeral(responder, message).await,
        };
        let next = lock_state(&handle).clone();
        let timeout = if drafting_session.is_some() {
            DRAFTING_TIMEOUT_SECONDS
        } else {
            PRE_DRAFT_TIMEOUT_SECONDS
        };
        self.persist_transition(
            &handle,
            previous,
            next,
            Some(unix_now().saturating_add(timeout as i64)),
            false,
        )
        .await?;
        // The update below is this interaction's initial callback, so it has to
        // land before any followup: the component auto-ack only fires after a
        // 1.5s deadline, and on the fast path Discord rejects a followup sent
        // while the interaction is still unacknowledged.
        let symmetry_session = drafting_session;
        if let Some(session) = symmetry_session {
            self.cancel_timeout(guild_id);
            self.schedule_timeout(guild_id, session, DRAFTING_TIMEOUT_SECONDS);
        }
        responder
            .update(response)
            .await
            .map_err(|error| error.to_string())?;
        if symmetry_session.is_some() {
            self.emit_captain_symmetry(guild_id, &handle, responder)
                .await;
        }
        Ok(())
    }

    async fn choose_hero(
        &self,
        guild_id: i64,
        user_id: i64,
        handle: Arc<Mutex<DraftState>>,
        order: HeroOrder,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let previous = lock_state(&handle).clone();
        let result: Result<(InteractionResponse, Option<u64>), &'static str> = {
            let mut state = lock_state(&handle);
            let winner_choice = state.phase == DraftPhase::WinnerHeroChoice;
            let loser_hero_choice = state.phase == DraftPhase::LoserChoice
                && state.winner_choice_type.as_deref() == Some("side");
            let expected = if winner_choice {
                state.coinflip_winner_id
            } else if loser_hero_choice {
                other_captain(&state, state.coinflip_winner_id.unwrap_or_default())
            } else {
                None
            };
            if expected != Some(user_id) {
                Err(
                    if (winner_choice && state.phase == DraftPhase::WinnerHeroChoice)
                        || (loser_hero_choice && state.phase == DraftPhase::LoserChoice)
                    {
                        "Only the designated captain can make this choice."
                    } else {
                        wrong_choice_message(state.phase)
                    },
                )
            } else if winner_choice {
                state.winner_choice_value = Some(order.as_str().to_owned());
                state.phase = DraftPhase::LoserChoice;
                let loser = other_captain(&state, user_id).unwrap_or_default();
                let session = state.session_id;
                let embed = self.render_choice(&state, "Choose Your Side", "Pick Radiant or Dire.");
                Ok((
                    embed.action_rows(pre_choice_view(session, loser, true, false)),
                    None,
                ))
            } else {
                state.loser_choice_value = Some(order.as_str().to_owned());
                let session = state.session_id;
                complete_pre_draft(&mut state);
                let (embed, view) = self.render_live(&state);
                Ok((
                    InteractionResponse::message("")
                        .embed(embed)
                        .action_rows(view),
                    Some(session),
                ))
            }
        };
        let (response, drafting_session) = match result {
            Ok(result) => result,
            Err(message) => return respond_ephemeral(responder, message).await,
        };
        let next = lock_state(&handle).clone();
        let timeout = if drafting_session.is_some() {
            DRAFTING_TIMEOUT_SECONDS
        } else {
            PRE_DRAFT_TIMEOUT_SECONDS
        };
        self.persist_transition(
            &handle,
            previous,
            next,
            Some(unix_now().saturating_add(timeout as i64)),
            false,
        )
        .await?;
        // The update below is this interaction's initial callback, so it has to
        // land before any followup: the component auto-ack only fires after a
        // 1.5s deadline, and on the fast path Discord rejects a followup sent
        // while the interaction is still unacknowledged.
        let symmetry_session = drafting_session;
        if let Some(session) = symmetry_session {
            self.cancel_timeout(guild_id);
            self.schedule_timeout(guild_id, session, DRAFTING_TIMEOUT_SECONDS);
        }
        responder
            .update(response)
            .await
            .map_err(|error| error.to_string())?;
        if symmetry_session.is_some() {
            self.emit_captain_symmetry(guild_id, &handle, responder)
                .await;
        }
        Ok(())
    }

    async fn pick_player(
        &self,
        guild_id: i64,
        user_id: i64,
        handle: Arc<Mutex<DraftState>>,
        player_id: i64,
        _user_name: String,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let previous = lock_state(&handle).clone();
        let outcome = {
            let mut state = lock_state(&handle);
            if state.phase != DraftPhase::Drafting {
                Err("❌ Draft is not in picking phase.")
            } else if state.current_captain_id() != Some(user_id) {
                Err("❌ It's not your turn!")
            } else if !state.pick_player(player_id, Some(user_id)) {
                Err("❌ Cannot pick that player. They may already be picked.")
            } else {
                Ok((
                    state.phase == DraftPhase::Complete,
                    state.session_id,
                    state.clone(),
                ))
            }
        };
        let Ok((complete, session, state_snapshot)) = outcome else {
            return respond_ephemeral(responder, outcome.unwrap_err()).await;
        };
        self.persist_transition(
            &handle,
            previous,
            state_snapshot.clone(),
            if complete {
                None
            } else {
                Some(unix_now().saturating_add(DRAFTING_TIMEOUT_SECONDS as i64))
            },
            complete,
        )
        .await?;
        if complete {
            // The final-pick CAS has fenced this row for recovery.  Disarm
            // the interactive drafting timeout before any fallible Discord
            // defer so it cannot delete the fenced row while publication is
            // awaiting explicit finalization recovery.
            if self.persistence.is_some() {
                self.cancel_timeout(guild_id);
            }
            responder
                .defer(false)
                .await
                .map_err(|error| error.to_string())?;
            self.complete(guild_id, handle, state_snapshot, responder.clone(), true)
                .await
        } else {
            self.schedule_timeout(guild_id, session, DRAFTING_TIMEOUT_SECONDS);
            let (embed, view) = self.render_live(&state_snapshot);
            responder
                .update(
                    InteractionResponse::message("")
                        .embed(embed)
                        .action_rows(view),
                )
                .await
                .map_err(|error| error.to_string())
        }
    }

    async fn side_preference(
        &self,
        _guild_id: i64,
        user_id: i64,
        handle: Arc<Mutex<DraftState>>,
        side: Option<Side>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let previous = lock_state(&handle).clone();
        let outcome = {
            let mut state = lock_state(&handle);
            if state.phase != DraftPhase::Drafting {
                Err("❌ Draft is not in picking phase.".to_owned())
            } else {
                let value = side.map(Side::as_str);
                if !state.set_side_preference(user_id, value) {
                    Err("❌ You have already been picked or are not in this draft.".to_owned())
                } else {
                    let message = match side {
                        Some(side) => format!("✅ Preference set to **{}**", side.title()),
                        None => "✅ Preference cleared".to_owned(),
                    };
                    Ok((message, state.clone()))
                }
            }
        };
        let Ok((message, snapshot)) = outcome else {
            return respond_ephemeral(responder, outcome.unwrap_err()).await;
        };
        self.persist_transition(
            &handle,
            previous,
            snapshot.clone(),
            self.cached_deadline(&snapshot),
            false,
        )
        .await?;
        let response = InteractionResponse::message(message).ephemeral();
        responder
            .respond(response)
            .await
            .map_err(|error| error.to_string())?;
        if let (Some(channel_id), Some(message_id)) =
            (snapshot.draft_channel_id, snapshot.draft_message_id)
        {
            let (embed, view) = self.render_live(&snapshot);
            let _ = self
                .discord
                .edit_message(
                    to_u64(channel_id)?,
                    to_u64(message_id)?,
                    DiscordMessage::silent(
                        InteractionResponse::message("")
                            .embed(embed)
                            .action_rows(view),
                    )
                    .preserving_content(),
                )
                .await;
        }
        Ok(())
    }

    async fn complete(
        &self,
        guild_id: i64,
        handle: Arc<Mutex<DraftState>>,
        state: DraftState,
        responder: Arc<dyn InteractionResponder>,
        deferred: bool,
    ) -> Result<(), String> {
        let key = (guild_id, state.session_id);
        let inserted = {
            let mut completing = lock_recover(&self.completing);
            completing.insert(key)
        };
        if !inserted {
            return respond_ephemeral(
                &responder,
                "⏳ Draft results are still being finalized. Please wait.",
            )
            .await;
        }
        let result = self
            .complete_owned(guild_id, handle, state, responder.clone(), deferred)
            .await;
        lock_recover(&self.completing).remove(&key);
        result
    }

    async fn complete_owned(
        &self,
        guild_id: i64,
        handle: Arc<Mutex<DraftState>>,
        state: DraftState,
        responder: Arc<dyn InteractionResponder>,
        deferred: bool,
    ) -> Result<(), String> {
        let kind = to_runtime_kind(state.lobby_kind);
        let operation_lock = self.lobbies.operation_lock(guild_id, kind);
        let Ok(_operation_guard) =
            tokio::time::timeout(OPERATION_LOCK_TIMEOUT, operation_lock.lock()).await
        else {
            return completion_response(
                &responder,
                None,
                "A draft or shuffle is already being finalized.".to_owned(),
                deferred,
            )
            .await;
        };
        let snapshot = self.lobbies.snapshot(guild_id, kind);
        let thread_id = snapshot.as_ref().and_then(|snapshot| snapshot.thread_id);
        let origin_channel_id = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.origin_channel_id)
            .or_else(|| state.draft_channel_id.and_then(|id| u64::try_from(id).ok()));
        let linked_retry = self.persistence.is_some()
            && lock_recover(&self.persisted_envelopes)
                .get(&(state.guild_id, state.session_id))
                .and_then(|envelope| envelope.pending_match_id)
                .is_some();
        let prepared_pending = if linked_retry {
            None
        } else {
            let now = unix_now();
            let is_bomb_pot = fastrand::f64() < self.config.bomb_pot_chance;
            Some((
                now,
                is_bomb_pot,
                db_pending_state(&state, now, self.config.bet_lock_seconds, is_bomb_pot)?,
            ))
        };
        if let Err(error) = self.mark_finalizing(&state, None).await {
            warn!(%error, guild_id, session_id = state.session_id, "draft finalization fence failed before pending match creation");
            return completion_response(&responder, None, error, deferred).await;
        }
        let mut finalization_plan = None;
        let (pending, effective_is_bomb_pot) = if self.persistence.is_some() {
            let linked = match self
                .link_finalizing_pending_match(
                    &state,
                    prepared_pending.as_ref().map(|(_, _, pending)| pending),
                    snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.lobby_channel_id),
                    origin_channel_id,
                    thread_id,
                )
                .await
            {
                Ok(linked) => linked,
                Err(error) => {
                    warn!(%error, guild_id, session_id = state.session_id, "atomic draft pending-match link failed");
                    return completion_response(&responder, None, error, deferred).await;
                }
            };
            let pending_match_id = linked.pending.pending_match_id;
            finalization_plan = Some(linked.plan.clone());
            if let Err(error) = self
                .run_persisted_financial_setup(&state, pending_match_id)
                .await
            {
                warn!(
                    %error,
                    pending_match_id,
                    "durable draft financial setup failed and remains retryable"
                );
                return financial_setup_retry_response(
                    &responder,
                    pending_match_id,
                    error,
                    deferred,
                )
                .await;
            }
            let pending = self
                .pending
                .pending_match(guild_id, pending_match_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "pending draft match disappeared after financial setup".to_owned()
                })?;
            (pending, linked.plan.is_bomb_pot)
        } else {
            let (now, is_bomb_pot, pending_state) = prepared_pending
                .as_ref()
                .expect("non-persistent Draft completion always prepares pending state");
            let pending = match self
                .pending
                .create_pending_match(guild_id, pending_state)
                .map_err(|error| error.to_string())
            {
                Ok(pending) => pending,
                Err(error) => {
                    if let Err(cleanup_error) = self.delete_persisted(&state).await {
                        warn!(%cleanup_error, guild_id, "failed to delete failed draft finalization fence");
                    }
                    self.finish_draft_without_match(&handle, &state);
                    return completion_response(&responder, None, error, deferred).await;
                }
            };
            if let Err(error) =
                self.pending
                    .mutate_pending_match(guild_id, pending.pending_match_id, |state| {
                        state.origin_channel_id =
                            origin_channel_id.and_then(|id| i64::try_from(id).ok());
                        state.thread_shuffle_thread_id =
                            thread_id.and_then(|id| i64::try_from(id).ok());
                    })
            {
                warn!(%error, pending_match_id = pending.pending_match_id, "draft thread metadata persistence failed");
            }
            if let Err(error) = self
                .reserve_seed_and_bets(&pending, kind, &state, *now, *is_bomb_pot)
                .await
            {
                warn!(
                    %error,
                    pending_match_id = pending.pending_match_id,
                    "draft seed/bet setup failed"
                );
                self.finish_draft_without_match(&handle, &state);
                return completion_response(
                    &responder,
                    Some(pending.pending_match_id),
                    error,
                    deferred,
                )
                .await;
            }
            let pending = self
                .pending
                .pending_match(guild_id, pending.pending_match_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "pending draft match disappeared after seed reservation".to_owned()
                })?;
            (pending, *is_bomb_pot)
        };

        if effective_is_bomb_pot && let Some(blind) = pending.state.blind_bets_result.as_ref() {
            let pool_amount = blind
                .get("total_radiant")
                .and_then(Value::as_i64)
                .unwrap_or_default()
                .saturating_add(
                    blind
                        .get("total_dire")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                );
            let contributor_count = blind
                .get("created")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            if contributor_count > 0 {
                match self
                    .neon
                    .on_bomb_pot(guild_id, pool_amount, contributor_count)
                    .await
                {
                    Ok(Some(result)) => self.send_neon_followup(&responder, result).await,
                    Ok(None) => {}
                    Err(error) => {
                        warn!(%error, guild_id, "draft bomb-pot Neon hook failed");
                    }
                }
            }
        }

        // A durable match consumes only the ten drafted players; the excluded
        // bench remains in the sibling lobby until this point as in Python.
        let matched = state
            .radiant_player_ids
            .iter()
            .chain(&state.dire_player_ids)
            .copied()
            .collect::<BTreeSet<_>>();
        if let Err(error) = self
            .lobbies
            .evict_from_other_lobby(guild_id, kind, &matched)
            .await
        {
            warn!(%error, "draft sibling lobby eviction failed");
        }
        if let Err(error) = self.lobbies.reset_after_shuffle(guild_id, kind).await {
            warn!(%error, "draft lobby reset failed after pending match creation");
        }
        self.cancel_timeout(guild_id);
        let draft_receipt = lock_recover(&self.draft_messages)
            .get(&(guild_id, state.session_id))
            .cloned();
        lock_recover(&self.draft_messages).remove(&(guild_id, state.session_id));
        self.drafts.clear_state(Some(guild_id), Some(&handle));
        self.remove_draft_operation_lock(guild_id, state.session_id);
        self.lobbies.release_players(
            guild_id,
            kind,
            &state.player_pool_ids.iter().copied().collect(),
        );

        let embed = self.render_completion(&state, &pending);
        let completion_message = InteractionResponse::message("").embed(embed.clone());
        let edit_result = if deferred {
            responder.edit_original(completion_message).await
        } else {
            responder.update(completion_message).await
        };
        if let Err(error) = edit_result {
            warn!(%error, pending_match_id = pending.pending_match_id, "draft completion source edit failed");
            drop(_operation_guard);
            let _ = self.lobbies.refresh_first_game_pool_lobbies(guild_id).await;
            return completion_response(
                &responder,
                Some(pending.pending_match_id),
                error.to_string(),
                deferred,
            )
            .await;
        }
        if let Some(plan) = finalization_plan.as_ref() {
            self.publish_persisted_completion(&state, &pending, plan)
                .await?;
            // The refresh helper acquires both lobby operation locks itself.
            // Do not hold the source-scope guard while calling it or a
            // successful completion deadlocks on the same open-lobby mutex.
            drop(_operation_guard);
            let _ = self.lobbies.refresh_first_game_pool_lobbies(guild_id).await;
            self.complete_persisted_finalization(&state, pending.pending_match_id)
                .await?;
            return Ok(());
        }
        let destinations = [
            snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.lobby_channel_id),
            origin_channel_id,
        ]
        .into_iter()
        .flatten()
        .filter(|channel_id| {
            Some(*channel_id) != state.draft_channel_id.and_then(|id| u64::try_from(id).ok())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
        let mut published = Vec::new();
        if let [first, second] = destinations.as_slice() {
            // Python publishes distinct lobby/origin copies with one gather,
            // so a slow or failed channel cannot serialize the other copy.
            let first_key = finalization_plan.as_ref().and_then(|plan| {
                (plan.lobby.channel_id == i64::try_from(*first).ok())
                    .then_some(plan.lobby.delivery_key.as_str())
                    .or_else(|| {
                        (plan.origin.channel_id == i64::try_from(*first).ok())
                            .then_some(plan.origin.delivery_key.as_str())
                    })
            });
            let second_key = finalization_plan.as_ref().and_then(|plan| {
                (plan.lobby.channel_id == i64::try_from(*second).ok())
                    .then_some(plan.lobby.delivery_key.as_str())
                    .or_else(|| {
                        (plan.origin.channel_id == i64::try_from(*second).ok())
                            .then_some(plan.origin.delivery_key.as_str())
                    })
            });
            let (first_result, second_result) = tokio::join!(
                self.send_completion_copy(*first, embed.clone(), first_key),
                self.send_completion_copy(*second, embed.clone(), second_key),
            );
            if let Ok(receipt) = first_result {
                published.push((*first, receipt));
            }
            if let Ok(receipt) = second_result {
                published.push((*second, receipt));
            }
        } else if let Some(channel_id) = destinations.first().copied() {
            let delivery_key = finalization_plan.as_ref().and_then(|plan| {
                [
                    (&plan.lobby, &plan.lobby.delivery_key),
                    (&plan.origin, &plan.origin.delivery_key),
                ]
                .into_iter()
                .find(|(delivery, _)| delivery.channel_id == i64::try_from(channel_id).ok())
                .map(|(_, key)| key.as_str())
            });
            if let Ok(receipt) = self
                .send_completion_copy(channel_id, embed.clone(), delivery_key)
                .await
            {
                published.push((channel_id, receipt));
            }
        }
        self.store_completion_message_info(CompletionMessageContext {
            guild_id,
            pending_match_id: pending.pending_match_id,
            state: &state,
            published: &published,
            thread_receipt: None,
            lobby_channel_id: snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.lobby_channel_id),
            origin_channel_id,
            draft_receipt: draft_receipt.as_ref(),
        })?;
        if let Some(thread_id) = thread_id {
            self.publish_thread(
                thread_id,
                &state,
                &embed,
                pending.pending_match_id,
                finalization_plan.as_ref(),
            )
            .await;
        }
        if let Err(error) = self.schedule_betting_reminders(pending.clone()).await {
            warn!(%error, pending_match_id = pending.pending_match_id, "draft betting reminder scheduling failed");
        }
        // The refresh helper acquires both lobby operation locks itself. Do
        // not hold the source-scope guard while calling it or a successful
        // completion deadlocks on the same open-lobby mutex.
        drop(_operation_guard);
        let _ = self.lobbies.refresh_first_game_pool_lobbies(guild_id).await;
        self.complete_persisted_finalization(&state, pending.pending_match_id)
            .await?;
        Ok(())
    }

    async fn send_completion_copy(
        &self,
        channel_id: u64,
        embed: InteractionEmbed,
        delivery_key: Option<&str>,
    ) -> Result<DiscordMessageReceipt, String> {
        let message =
            DiscordMessage::default_mentions(InteractionResponse::message("").embed(embed));
        if let Some(delivery_key) = delivery_key {
            self.discord
                .send_message_with_delivery_key(channel_id, delivery_key, message)
                .await
        } else {
            self.discord.send_message(channel_id, message).await
        }
    }

    async fn reserve_seed_and_bets(
        &self,
        pending: &PendingMatchRecord,
        kind: AppLobbyKind,
        state: &DraftState,
        now: i64,
        is_bomb_pot: bool,
    ) -> Result<(), String> {
        let first_game_reserved = if self.config.first_game_pool_daily_amount > 0 {
            let date = cama_domain::game_date::get_game_date();
            self.seeds
                .fund_first_game_pools(
                    Some(pending.guild_id),
                    &date,
                    self.config.first_game_pool_daily_amount,
                )
                .map_err(|error| error.to_string())?;
            self.seeds
                .claim_first_game_pool(
                    Some(pending.guild_id),
                    match kind {
                        AppLobbyKind::Open => FirstGameLobby::Open,
                        AppLobbyKind::LowSkill => FirstGameLobby::LowSkill,
                    },
                    &date,
                    pending.pending_match_id,
                )
                .map_err(|error| error.to_string())?
        } else {
            0
        };
        self.seeds
            .reserve_seed_atomic(
                Some(pending.guild_id),
                pending.pending_match_id,
                self.config.dota_bet_seed_amount,
                first_game_reserved,
                SeedBettingMode::Pool,
            )
            .map_err(|error| error.to_string())?;
        let candidates = state
            .radiant_player_ids
            .iter()
            .copied()
            .map(|discord_id| BlindBetCandidate {
                discord_id,
                team: BettingTeam::Radiant,
                ante_override: None,
            })
            .chain(
                state
                    .dire_player_ids
                    .iter()
                    .copied()
                    .map(|discord_id| BlindBetCandidate {
                        discord_id,
                        team: BettingTeam::Dire,
                        ante_override: None,
                    }),
            )
            .collect::<Vec<_>>();
        let investment_starting_balances = self
            .investment_starting_balances(pending.guild_id, state)
            .unwrap_or_default();

        // Python deliberately treats automatic liquidity as a best-effort
        // post-persistence hook.  A single wallet/DB candidate must not turn
        // a durable draft into a failed completion, so each service seam is
        // isolated and the pending state is updated only after the hooks have
        // returned their summaries.
        let mut blind_result = json!({
            "created": 0,
            "total_radiant": 0,
            "total_dire": 0,
            "percentage": if is_bomb_pot {
                self.config.bomb_pot_blind_percentage
            } else {
                self.config.auto_blind_percentage
            },
            "is_bomb_pot": is_bomb_pot,
            "bets": [],
            "skipped": [],
        });
        if self.config.auto_blind_enabled {
            match self.bets.create_auto_blind_bets_atomic(
                Some(pending.guild_id),
                Some(pending.pending_match_id),
                now,
                &candidates,
                BlindBetPolicy {
                    is_bomb_pot,
                    normal_threshold: self.config.auto_blind_threshold,
                    normal_percentage: percentage_points(self.config.auto_blind_percentage),
                    bomb_percentage: percentage_points(self.config.bomb_pot_blind_percentage),
                    bomb_ante: self.config.bomb_pot_ante,
                    max_debt: self.config.max_debt,
                },
            ) {
                Ok(outcome) => {
                    blind_result = blind_outcome_json(&outcome);
                    blind_result["percentage"] = json!(if is_bomb_pot {
                        self.config.bomb_pot_blind_percentage
                    } else {
                        self.config.auto_blind_percentage
                    });
                }
                Err(error) => {
                    warn!(%error, pending_match_id = pending.pending_match_id, "draft blind-bet hook failed");
                }
            }
        }

        let investment_result = match self.create_investment_bets(
            pending,
            state,
            now,
            &investment_starting_balances,
        ) {
            Ok(result) => Some(result),
            Err(error) => {
                warn!(%error, pending_match_id = pending.pending_match_id, "draft configured-investment hook failed");
                None
            }
        };

        let spectator_result = self
            .create_spectator_bets(pending, state, now)
            .map_err(|error| {
                warn!(%error, pending_match_id = pending.pending_match_id, "draft spectator auto-wager hook failed");
                error
            })
            .ok();

        if let Some(result) = investment_result {
            blind_result["investment_bets"] = result;
        }
        if let Some(result) = spectator_result {
            blind_result["spectator_bets"] = result;
        }
        let has_automatic_bets = blind_result
            .get("created")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            > 0
            || blind_result
                .get("investment_bets")
                .and_then(|result| result.get("created"))
                .and_then(Value::as_i64)
                .unwrap_or_default()
                > 0
            || blind_result
                .get("spectator_bets")
                .and_then(|result| result.get("created"))
                .and_then(Value::as_i64)
                .unwrap_or_default()
                > 0;
        if has_automatic_bets
            && let Err(error) = self.pending.mutate_pending_match(
                pending.guild_id,
                pending.pending_match_id,
                |state| state.blind_bets_result = Some(blind_result),
            )
        {
            warn!(%error, pending_match_id = pending.pending_match_id, "draft automatic-bet result persistence failed");
        }
        Ok(())
    }

    fn create_investment_bets(
        &self,
        pending: &PendingMatchRecord,
        state: &DraftState,
        now: i64,
        starting_balances: &BTreeMap<i64, i64>,
    ) -> Result<Value, String> {
        let target_ids = state
            .radiant_player_ids
            .iter()
            .chain(&state.dire_player_ids)
            .copied()
            .collect::<Vec<_>>();
        let positions = self
            .investments
            .for_targets(Some(pending.guild_id), &target_ids)
            .map_err(|error| error.to_string())?;
        if positions.is_empty() {
            return Ok(json!({
                "created": 0,
                "total_radiant": 0,
                "total_dire": 0,
                "bets": [],
                "skipped": [],
            }));
        }
        let investor_ids = positions
            .iter()
            .map(|position| position.investor_id)
            .collect::<Vec<_>>();
        // Python prepares the shuffle-start gate before blind bets, then
        // sizes each investment from the post-blind wallet snapshot.
        let current_balances = self
            .players
            .get_balances_bulk(&investor_ids, Some(pending.guild_id))
            .map_err(|error| error.to_string())?;
        let team_by_player = state
            .radiant_player_ids
            .iter()
            .map(|id| (*id, BettingTeam::Radiant))
            .chain(
                state
                    .dire_player_ids
                    .iter()
                    .map(|id| (*id, BettingTeam::Dire)),
            )
            .collect::<BTreeMap<_, _>>();
        let timestamp = pending.state.shuffle_timestamp.unwrap_or(now);
        let mut totals = self
            .bets
            .get_pending_totals(
                Some(pending.guild_id),
                timestamp,
                Some(pending.pending_match_id),
            )
            .map_err(|error| error.to_string())?;
        let mut investment_total_radiant = 0_i64;
        let mut investment_total_dire = 0_i64;
        let mut created = Vec::new();
        let mut skipped = Vec::new();
        for position in positions {
            let Some(target_team) = team_by_player.get(&position.target_id).copied() else {
                continue;
            };
            let starting_balance = starting_balances
                .get(&position.investor_id)
                .copied()
                .unwrap_or_default();
            if starting_balance < 50 {
                skipped.push(json!({
                    "investor_id": position.investor_id,
                    "target_id": position.target_id,
                    "reason": format!("shuffle-start balance {starting_balance} < 50"),
                }));
                continue;
            }
            let balance = current_balances
                .get(&position.investor_id)
                .copied()
                .unwrap_or_default();
            if balance <= 0 {
                skipped.push(json!({
                    "investor_id": position.investor_id,
                    "target_id": position.target_id,
                    "reason": format!("balance {balance} is not positive"),
                }));
                continue;
            }
            if position.direction == "short" && position.investor_id == position.target_id {
                skipped.push(json!({
                    "investor_id": position.investor_id,
                    "target_id": position.target_id,
                    "reason": "cannot short yourself",
                }));
                continue;
            }
            let team = if position.direction == "long" {
                target_team
            } else {
                match target_team {
                    BettingTeam::Radiant => BettingTeam::Dire,
                    BettingTeam::Dire => BettingTeam::Radiant,
                }
            };
            if team_by_player
                .get(&position.investor_id)
                .is_some_and(|investor_team| *investor_team != team)
            {
                skipped.push(json!({
                    "investor_id": position.investor_id,
                    "target_id": position.target_id,
                    "reason": "investment would bet against the investor's team",
                }));
                continue;
            }
            let amount = balance
                .checked_mul(position.percentage)
                .ok_or_else(|| "investment amount overflow".to_owned())?
                / 100;
            if amount < 1 {
                skipped.push(json!({
                    "investor_id": position.investor_id,
                    "target_id": position.target_id,
                    "reason": format!("investment amount {amount} < 1"),
                }));
                continue;
            }
            let total_pool = totals.total();
            let team_total = match team {
                BettingTeam::Radiant => totals.radiant,
                BettingTeam::Dire => totals.dire,
            };
            let request = PlaceBetRequest {
                guild_id: Some(pending.guild_id),
                pending_match_id: pending.pending_match_id,
                discord_id: position.investor_id,
                team,
                amount,
                bet_time: timestamp,
                leverage: 1,
                max_debt: self.config.max_debt,
                is_blind: true,
                odds_at_placement: (total_pool > 0 && team_total > 0)
                    .then_some(total_pool as f64 / team_total as f64),
            };
            match self.bets.place_investment_bet_atomic(
                request,
                position.target_id,
                &position.direction,
            ) {
                Ok(record) => {
                    match team {
                        BettingTeam::Radiant => {
                            totals.radiant = totals.radiant.saturating_add(amount);
                            investment_total_radiant =
                                investment_total_radiant.saturating_add(amount);
                        }
                        BettingTeam::Dire => {
                            totals.dire = totals.dire.saturating_add(amount);
                            investment_total_dire = investment_total_dire.saturating_add(amount);
                        }
                    }
                    created.push(json!({
                        "investor_id": record.discord_id,
                        "target_id": position.target_id,
                        "direction": position.direction,
                        "percentage": position.percentage,
                        "team": betting_team_name(team),
                        "amount": amount,
                    }));
                }
                Err(error) => skipped.push(json!({
                    "investor_id": position.investor_id,
                    "target_id": position.target_id,
                    "reason": error.to_string(),
                })),
            }
        }
        Ok(json!({
            "created": created.len(),
            "total_radiant": investment_total_radiant,
            "total_dire": investment_total_dire,
            "bets": created,
            "skipped": skipped,
        }))
    }

    fn investment_starting_balances(
        &self,
        guild_id: i64,
        state: &DraftState,
    ) -> Result<BTreeMap<i64, i64>, String> {
        let target_ids = state
            .radiant_player_ids
            .iter()
            .chain(&state.dire_player_ids)
            .copied()
            .collect::<Vec<_>>();
        let positions = self
            .investments
            .for_targets(Some(guild_id), &target_ids)
            .map_err(|error| error.to_string())?;
        let investor_ids = positions
            .iter()
            .map(|position| position.investor_id)
            .collect::<Vec<_>>();
        self.players
            .get_balances_bulk(&investor_ids, Some(guild_id))
            .map_err(|error| error.to_string())
            .map(|balances| balances.into_iter().collect())
    }

    fn create_spectator_bets(
        &self,
        pending: &PendingMatchRecord,
        state: &DraftState,
        now: i64,
    ) -> Result<Value, String> {
        let candidates = self
            .players
            .get_all(Some(pending.guild_id))
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(|player| {
                player.discord_id.map(|discord_id| WealthSnapshot {
                    discord_id,
                    balance: player.jopacoin_balance,
                })
            })
            .collect::<Vec<_>>();
        let participants = state
            .radiant_player_ids
            .iter()
            .chain(&state.dire_player_ids)
            .copied()
            .collect::<BTreeSet<_>>();
        let plan = plan_automatic_spectator_bets(
            &candidates,
            &participants,
            AutomaticSpectatorConfig {
                enabled: self.config.auto_spectator_bet_enabled,
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
        let outcome = self
            .bets
            .place_automatic_basis_point_bets_atomic(
                Some(pending.guild_id),
                Some(pending.pending_match_id),
                now,
                &requests,
                self.config.max_debt,
            )
            .map_err(|error| error.to_string())?;
        let plan_by_id = plan
            .bets
            .iter()
            .map(|bet| (bet.discord_id, *bet))
            .collect::<BTreeMap<_, _>>();
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
        let bets = outcome
            .created
            .iter()
            .map(|bet| {
                let plan = plan_by_id.get(&bet.discord_id);
                json!({
                    "discord_id": bet.discord_id,
                    "team": betting_team_name(bet.team),
                    "amount": bet.amount,
                    "networth": plan.map_or(0, |plan| plan.networth),
                    "percentage": plan.map_or(0.0, |plan| {
                        plan.basis_points as f64 / 10_000.0
                    }),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "created": outcome.created.len(),
            "total_radiant": total_radiant,
            "total_dire": total_dire,
            "percentage": self.config.auto_spectator_bet_percentage,
            "top_percentage": self.config.auto_spectator_bet_top_percentage,
            "total_count": plan.total_count,
            "top_count": plan.top_count,
            "bets": bets,
            "skipped": outcome.skipped,
        }))
    }

    fn finish_draft_without_match(&self, handle: &Arc<Mutex<DraftState>>, state: &DraftState) {
        self.cancel_timeout(state.guild_id);
        lock_recover(&self.draft_messages).remove(&(state.guild_id, state.session_id));
        self.drafts.clear_state(Some(state.guild_id), Some(handle));
        self.remove_draft_operation_lock(state.guild_id, state.session_id);
        self.lobbies.release_players(
            state.guild_id,
            to_runtime_kind(state.lobby_kind),
            &state.player_pool_ids.iter().copied().collect(),
        );
    }

    fn store_completion_message_info(
        &self,
        context: CompletionMessageContext<'_>,
    ) -> Result<(), String> {
        let CompletionMessageContext {
            guild_id,
            pending_match_id,
            state,
            published,
            thread_receipt,
            lobby_channel_id,
            origin_channel_id,
            draft_receipt,
        } = context;
        self.pending
            .mutate_pending_match(guild_id, pending_match_id, |pending| {
                pending.shuffle_channel_id = state.draft_channel_id;
                pending.shuffle_message_id = state.draft_message_id;
                pending.origin_channel_id = origin_channel_id.and_then(|id| i64::try_from(id).ok());
                let primary = lobby_channel_id
                    .and_then(|channel_id| {
                        published
                            .iter()
                            .find(|(published_channel, _)| *published_channel == channel_id)
                            .map(|(published_channel, receipt)| (*published_channel, receipt))
                    })
                    .or_else(|| draft_receipt.map(|receipt| (receipt.channel_id, receipt)))
                    .or_else(|| {
                        published
                            .first()
                            .map(|(channel_id, receipt)| (*channel_id, receipt))
                    });
                if let Some((channel_id, receipt)) = primary {
                    pending.shuffle_channel_id = i64::try_from(channel_id).ok();
                    pending.shuffle_message_id = i64::try_from(receipt.message_id).ok();
                    pending.shuffle_message_jump_url = Some(receipt.jump_url.clone());
                }
                let primary_channel = pending
                    .shuffle_channel_id
                    .and_then(|id| u64::try_from(id).ok());
                let command_receipt = origin_channel_id.and_then(|origin_channel| {
                    published
                        .iter()
                        .find(|(channel_id, _)| *channel_id == origin_channel)
                        .map(|(channel_id, receipt)| (*channel_id, receipt))
                        .or_else(|| {
                            draft_receipt
                                .filter(|receipt| receipt.channel_id == origin_channel)
                                .map(|receipt| (receipt.channel_id, receipt))
                        })
                });
                if origin_channel_id != primary_channel
                    && let Some((channel_id, receipt)) = command_receipt
                {
                    pending.cmd_shuffle_channel_id = i64::try_from(channel_id).ok();
                    pending.cmd_shuffle_message_id = i64::try_from(receipt.message_id).ok();
                }
                if let Some(receipt) = thread_receipt {
                    pending.thread_shuffle_thread_id = i64::try_from(receipt.channel_id).ok();
                    pending.thread_shuffle_message_id = i64::try_from(receipt.message_id).ok();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn publish_thread(
        &self,
        thread_id: u64,
        state: &DraftState,
        embed: &InteractionEmbed,
        pending_match_id: i64,
        plan: Option<&DraftFinalizationPlan>,
    ) {
        let kind = to_runtime_kind(state.lobby_kind);
        let name = format!("🔒 {} Draft Complete - Awaiting Results", kind.label());
        // Renaming is independent of the ordered embed -> player-ping path;
        // Python starts both before waiting, then locks only after publication.
        let rename = self.discord.edit_thread(thread_id, &name, false, false);
        let post = async {
            let embed_message = DiscordMessage::default_mentions(
                InteractionResponse::message("").embed(embed.clone()),
            );
            let receipt = if let Some(plan) = plan {
                self.discord
                    .send_message_with_delivery_key(
                        thread_id,
                        &plan.thread_embed.delivery_key,
                        embed_message,
                    )
                    .await?
            } else {
                self.discord.send_message(thread_id, embed_message).await?
            };
            let mentions = state
                .radiant_player_ids
                .iter()
                .chain(&state.dire_player_ids)
                .copied()
                .filter(|id| *id > 0)
                .filter_map(|id| u64::try_from(id).ok())
                .collect::<Vec<_>>();
            let content = mentions
                .iter()
                .map(|id| format!("<@{id}>"))
                .collect::<Vec<_>>()
                .join(" ");
            if !content.is_empty() {
                let ping_message = DiscordMessage::mentioning(
                    InteractionResponse::message(format!(
                        "{content}\nPlayers, please take your starting positions!"
                    )),
                    mentions.into_iter().collect(),
                );
                if let Some(plan) = plan {
                    self.discord
                        .send_message_with_delivery_key(
                            thread_id,
                            &plan.thread_ping.delivery_key,
                            ping_message,
                        )
                        .await?;
                } else {
                    self.discord.send_message(thread_id, ping_message).await?;
                }
            }
            self.pending
                .mutate_pending_match(state.guild_id, pending_match_id, |pending| {
                    pending.thread_shuffle_thread_id =
                        Some(i64::try_from(thread_id).unwrap_or_default());
                    pending.thread_shuffle_message_id =
                        Some(i64::try_from(receipt.message_id).unwrap_or_default());
                })
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        };
        let (rename_result, post_result) = tokio::join!(rename, post);
        if let Err(error) = rename_result {
            debug!(%error, thread_id, "draft thread rename failed");
        }
        if let Err(error) = post_result {
            debug!(%error, thread_id, "draft thread publication failed");
        }
        if let Err(error) = self
            .discord
            .edit_thread(thread_id, &name, false, true)
            .await
        {
            debug!(%error, thread_id, "draft thread lock failed");
        }
        // The production Python path leaves the thread locked but not
        // archived until `/record` or `/record abort` finalizes it.
    }

    fn schedule_timeout(&self, guild_id: i64, session_id: u64, seconds: u64) {
        self.cancel_timeout(guild_id);
        let drafts = Arc::clone(&self.drafts);
        let draft_messages = Arc::clone(&self.draft_messages);
        let persistence = self.persistence.clone();
        let persisted_envelopes = Arc::clone(&self.persisted_envelopes);
        let draft_operation_locks = Arc::clone(&self.draft_operation_locks);
        let discord = Arc::clone(&self.discord);
        let lobbies = self.lobbies.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            let Some(state) = drafts.get_state(Some(guild_id)) else {
                return;
            };
            let snapshot = lock_state(&state).clone();
            if snapshot.session_id != session_id {
                return;
            }
            let draft_operation_lock = lock_recover(&draft_operation_locks)
                .entry((guild_id, session_id))
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            // Components hold this fence across their CAS and Discord
            // publication.  Acquire it before the lobby lock so completion
            // (which takes the reverse order) cannot deadlock at expiry.
            let _draft_operation_guard = draft_operation_lock.lock().await;
            let kind = to_runtime_kind(snapshot.lobby_kind);
            let operation_lock = lobbies.operation_lock(guild_id, kind);
            let _operation_guard = operation_lock.lock().await;
            let Some(current) = drafts.get_state(Some(guild_id)) else {
                cleanup_unused_draft_operation_lock_entry(
                    &draft_operation_locks,
                    &drafts,
                    guild_id,
                    session_id,
                    &draft_operation_lock,
                );
                return;
            };
            if !Arc::ptr_eq(&current, &state) || lock_state(&current).session_id != session_id {
                cleanup_unused_draft_operation_lock_entry(
                    &draft_operation_locks,
                    &drafts,
                    guild_id,
                    session_id,
                    &draft_operation_lock,
                );
                return;
            }
            if let Some(persistence) = persistence {
                let key = (guild_id, session_id);
                let Some(envelope) = lock_recover(&persisted_envelopes).get(&key).cloned() else {
                    warn!(
                        guild_id,
                        session_id, "draft timeout has no durable session fence"
                    );
                    return;
                };
                let expected_revision = envelope.revision;
                let deleted = match spawn_blocking(move || {
                    persistence.delete_if_revision(key.0, key.1, expected_revision)
                })
                .await
                {
                    Ok(Ok(deleted)) => deleted,
                    Ok(Err(error)) => {
                        warn!(%error, guild_id, session_id, "draft timeout persistence cleanup failed");
                        return;
                    }
                    Err(error) => {
                        warn!(%error, guild_id, session_id, "draft timeout persistence task failed");
                        return;
                    }
                };
                if !deleted {
                    // A replacement session won the CAS race.  Never clear
                    // local state or publish a timeout for that replacement.
                    return;
                }
                lock_recover(&persisted_envelopes).remove(&key);
            }
            lock_recover(&draft_messages).remove(&(guild_id, session_id));
            drafts.clear_state(Some(guild_id), Some(&state));
            let ids = snapshot
                .player_pool_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            lobbies.release_players(guild_id, kind, &ids);
            if let (Some(channel_id), Some(message_id)) =
                (snapshot.draft_channel_id, snapshot.captain_ping_message_id)
            {
                let _ = discord
                    .delete_message(
                        to_u64(channel_id).unwrap_or_default(),
                        to_u64(message_id).unwrap_or_default(),
                    )
                    .await;
            }
            if let (Some(channel_id), Some(message_id)) =
                (snapshot.draft_channel_id, snapshot.draft_message_id)
            {
                let _ = discord
                    .edit_message(
                        to_u64(channel_id).unwrap_or_default(),
                        to_u64(message_id).unwrap_or_default(),
                        DiscordMessage::silent(
                            InteractionResponse::message("").embed(timeout_embed()),
                        ),
                    )
                    .await;
            }
            cleanup_unused_draft_operation_lock_entry(
                &draft_operation_locks,
                &drafts,
                guild_id,
                session_id,
                &draft_operation_lock,
            );
        });
        lock_recover(&self.timeout_tasks).insert(guild_id, handle);
    }

    fn cancel_timeout(&self, guild_id: i64) {
        if let Some(handle) = lock_recover(&self.timeout_tasks).remove(&guild_id) {
            handle.abort();
        }
    }

    async fn sample_in_progress(
        &self,
        guild_id: i64,
        user_id: i64,
        permissions: Option<u64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !is_admin(&self.config, user_id, permissions) {
            return respond_ephemeral(&responder, "❌ Admin only command.").await;
        }
        let state = sample_state(guild_id, false);
        let players = sample_players();
        self.ensure_sample_players(guild_id, &players)?;
        responder
            .respond(
                InteractionResponse::message("**[SAMPLE UI - Not a real draft]**")
                    .embed(sample_live_embed(&state))
                    .action_rows(drafting_sample_view(&state)),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn sample_complete(
        &self,
        guild_id: i64,
        user_id: i64,
        permissions: Option<u64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !is_admin(&self.config, user_id, permissions) {
            return respond_ephemeral(&responder, "❌ Admin only command.").await;
        }
        let state = sample_state(guild_id, true);
        self.ensure_sample_players(guild_id, &sample_players())?;
        let pending = db_pending_state(&state, unix_now(), self.config.bet_lock_seconds, false)?;
        let mut pending = PendingMatchRecord {
            pending_match_id: 0,
            guild_id,
            state: pending,
            created_at: None,
            updated_at: None,
        };
        pending.state.blind_bets_result = Some(json!({
            "created": 3,
            "total_radiant": 15,
            "total_dire": 12,
            "percentage": 0.1,
            "is_bomb_pot": false,
        }));
        responder
            .respond(
                InteractionResponse::message("**[SAMPLE UI - Not a real draft]**")
                    .embed(completion_embed(&state, &pending)),
            )
            .await
            .map_err(|error| error.to_string())
    }

    fn ensure_sample_players(
        &self,
        guild_id: i64,
        players: &[(i64, &str, f64, &[&str])],
    ) -> Result<(), String> {
        for (id, name, rating, roles) in players {
            if self
                .players
                .get_by_id(*id, Some(guild_id))
                .map_err(|error| error.to_string())?
                .is_some()
            {
                continue;
            }
            let mut player = NewPlayer::new(*id, *name, Some(guild_id));
            player.glicko_rating = Some(*rating);
            player.glicko_rd = Some(100.0);
            player.glicko_volatility = Some(0.06);
            player.preferred_roles = Some(roles.iter().map(|role| (*role).to_owned()).collect());
            match self.players.add(&player) {
                Ok(()) | Err(CoreRepositoryError::DuplicatePlayer { .. }) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    async fn load_players(
        &self,
        ids: &[i64],
        guild_id: i64,
    ) -> Result<Vec<Player>, CoreRepositoryError> {
        let players = self.players.clone();
        let ids = ids.to_vec();
        tokio::task::spawn_blocking(move || players.get_by_ids(&ids, Some(guild_id)))
            .await
            .map_err(|error| {
                CoreRepositoryError::InvalidInput(format!("player load task failed: {error}"))
            })?
    }

    async fn schedule_betting_reminders(&self, pending: PendingMatchRecord) -> Result<(), String> {
        self.reminders.schedule_betting_reminders(pending).await
    }
}

#[derive(Clone)]
struct StartContext {
    user_id: i64,
    guild_id: i64,
    channel_id: i64,
    explicit_kind: Option<AppLobbyKind>,
    captain1: Option<i64>,
    captain2: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Radiant,
    Dire,
}

impl Side {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Radiant => "radiant",
            Self::Dire => "dire",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Radiant => "Radiant",
            Self::Dire => "Dire",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeroOrder {
    First,
    Second,
}

impl HeroOrder {
    const fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Second => "second",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DraftComponent {
    ChoiceSide,
    ChoiceHero,
    Side(Side),
    Hero(HeroOrder),
    Pick(i64),
    Preference(Option<Side>),
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedComponent {
    session_id: Option<u64>,
    action: DraftComponent,
}

fn draft_options() -> Vec<CommandOptionSpec> {
    vec![
        subcommand(
            "start",
            "Start an Immortal Draft with captain-based player selection",
            vec![
                CommandOptionSpec::new(
                    "captain1",
                    "(Optional) Specify first captain",
                    CommandOptionKind::User,
                ),
                CommandOptionSpec::new(
                    "captain2",
                    "(Optional) Specify second captain",
                    CommandOptionKind::User,
                ),
                choices(
                    CommandOptionSpec::new(
                        "lobby",
                        "Lobby to draft from (defaults to the lobby you're in)",
                        CommandOptionKind::String,
                    ),
                    &[
                        ("🍽️ All You Can Feed", "open"),
                        ("🧀 Whine & Cheese", "lowskill"),
                    ],
                ),
            ],
        ),
        subcommand(
            "restart",
            "Restart the current Immortal Draft (preserves lobby)",
            Vec::new(),
        ),
        subcommand(
            "sampleinprogress",
            "[Admin] Show sample draft UI mid-draft for testing",
            Vec::new(),
        ),
        subcommand(
            "samplecomplete",
            "[Admin] Show sample draft complete UI for testing",
            Vec::new(),
        ),
    ]
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
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

fn leaf_command(options: &[InteractionOption]) -> Result<(&str, &[InteractionOption]), String> {
    let option = options
        .iter()
        .find(|option| matches!(option.value, InteractionValue::Subcommand(_)))
        .ok_or_else(|| "/draft requires a subcommand".to_owned())?;
    let InteractionValue::Subcommand(children) = &option.value else {
        unreachable!()
    };
    Ok((&option.name, children.as_slice()))
}

fn optional_lobby_kind(options: &[InteractionOption]) -> Result<Option<AppLobbyKind>, String> {
    let Some(value) = options.iter().find(|option| option.name == "lobby") else {
        return Ok(None);
    };
    let InteractionValue::String(value) = &value.value else {
        return Err("draft lobby option must be a string".to_owned());
    };
    match value.as_str() {
        "open" => Ok(Some(AppLobbyKind::Open)),
        "lowskill" => Ok(Some(AppLobbyKind::LowSkill)),
        _ => Err("draft lobby option must be open or lowskill".to_owned()),
    }
}

fn optional_user(options: &[InteractionOption], name: &str) -> Result<Option<i64>, String> {
    let Some(option) = options.iter().find(|option| option.name == name) else {
        return Ok(None);
    };
    let InteractionValue::User { id, .. } = &option.value else {
        return Err(format!("draft {name} option must be a user"));
    };
    signed_id(*id, name).map(Some)
}

fn resolve_lobby_kind(
    memberships: &[AppLobbyKind],
    explicit: Option<AppLobbyKind>,
) -> Result<AppLobbyKind, String> {
    if let Some(kind) = explicit {
        return Ok(kind);
    }
    match memberships {
        [] => Err("❌ Join a lobby before using `/draft start`.".to_owned()),
        [kind] => Ok(*kind),
        _ => Err(cama_app::draft::AMBIGUOUS_LOBBY_MESSAGE.to_owned()),
    }
}

fn parse_component(custom_id: &str) -> Result<ParsedComponent, String> {
    if let Some(action) = custom_id.strip_prefix("draft_") {
        let action = match action {
            "choice_side" => DraftComponent::ChoiceSide,
            "choice_hero_pick" => DraftComponent::ChoiceHero,
            value if value.starts_with("pick_") => value[5..]
                .parse::<i64>()
                .map_or(DraftComponent::Unknown, DraftComponent::Pick),
            "pref_radiant" => DraftComponent::Preference(Some(Side::Radiant)),
            "pref_dire" => DraftComponent::Preference(Some(Side::Dire)),
            "pref_clear" => DraftComponent::Preference(None),
            _ => DraftComponent::Unknown,
        };
        return Ok(ParsedComponent {
            session_id: None,
            action,
        });
    }
    let mut parts = custom_id.split(':');
    if parts.next() != Some("draft") {
        return Err("invalid draft component prefix".to_owned());
    }
    let first = parts
        .next()
        .ok_or_else(|| "draft component is incomplete".to_owned())?;
    let (session_id, action_name) = if let Ok(session) = first.parse::<u64>() {
        (
            Some(session),
            parts
                .next()
                .ok_or_else(|| "draft component is incomplete".to_owned())?,
        )
    } else {
        (None, first)
    };
    let action = match action_name {
        "choice_side" => DraftComponent::ChoiceSide,
        "choice_hero" => DraftComponent::ChoiceHero,
        "side" => match parts.next() {
            Some("radiant") => DraftComponent::Side(Side::Radiant),
            Some("dire") => DraftComponent::Side(Side::Dire),
            _ => DraftComponent::Unknown,
        },
        "hero" => match parts.next() {
            Some("first") => DraftComponent::Hero(HeroOrder::First),
            Some("second") => DraftComponent::Hero(HeroOrder::Second),
            _ => DraftComponent::Unknown,
        },
        "pick" => parts
            .next()
            .and_then(|id| id.parse::<i64>().ok())
            .map_or(DraftComponent::Unknown, DraftComponent::Pick),
        "pref" => match parts.next() {
            Some("radiant") => DraftComponent::Preference(Some(Side::Radiant)),
            Some("dire") => DraftComponent::Preference(Some(Side::Dire)),
            Some("clear") => DraftComponent::Preference(None),
            _ => DraftComponent::Unknown,
        },
        _ => DraftComponent::Unknown,
    };
    Ok(ParsedComponent { session_id, action })
}

fn pre_choice_view(
    session: u64,
    chooser_id: i64,
    side_choice: bool,
    winner_choice: bool,
) -> Vec<InteractionActionRow> {
    if winner_choice {
        return vec![InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("draft:{session}:choice_side"), "Choose Side")
                .emoji("🗺️"),
            InteractionButton::new(
                format!("draft:{session}:choice_hero"),
                "Choose Hero Pick Order",
            )
            .emoji("⚔️"),
        ])];
    }
    if side_choice {
        vec![InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("draft:{session}:side:radiant"), "Radiant")
                .style(InteractionButtonStyle::Success)
                .emoji("🟢"),
            InteractionButton::new(format!("draft:{session}:side:dire"), "Dire")
                .style(InteractionButtonStyle::Danger)
                .emoji("🔴"),
        ])]
    } else {
        let _ = chooser_id;
        vec![InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("draft:{session}:hero:first"), "First Pick").emoji("1️⃣"),
            InteractionButton::new(format!("draft:{session}:hero:second"), "Second Pick")
                .style(InteractionButtonStyle::Secondary)
                .emoji("2️⃣"),
        ])]
    }
}

fn drafting_view(state: &DraftState) -> Vec<InteractionActionRow> {
    drafting_view_with_session(state, Some(state.session_id))
}

fn drafting_sample_view(state: &DraftState) -> Vec<InteractionActionRow> {
    drafting_view_with_session(state, None)
}

fn drafting_view_with_session(
    state: &DraftState,
    session: Option<u64>,
) -> Vec<InteractionActionRow> {
    let mut available = state
        .available_player_ids()
        .into_iter()
        .filter_map(|id| state.player_pool_data.get(&id).map(|data| (id, data)))
        .collect::<Vec<_>>();
    available.sort_by(|(_, left), (_, right)| {
        right
            .rating
            .partial_cmp(&left.rating)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rows = Vec::new();
    for chunk in available.into_iter().take(8).collect::<Vec<_>>().chunks(4) {
        rows.push(InteractionActionRow::buttons(
            chunk
                .iter()
                .map(|(id, player)| {
                    let label = format!(
                        "{} ({:.0}) {}",
                        player.name,
                        player.rating,
                        format_roles(&player.roles)
                    );
                    let custom_id = session.map_or_else(
                        || format!("draft_pick_{id}"),
                        |session| format!("draft:{session}:pick:{id}"),
                    );
                    InteractionButton::new(custom_id, label.chars().take(80).collect::<String>())
                })
                .collect(),
        ));
    }
    rows.push(InteractionActionRow::buttons(vec![
        InteractionButton::new(
            session.map_or_else(
                || "draft_pref_radiant".to_owned(),
                |session| format!("draft:{session}:pref:radiant"),
            ),
            "Prefer Radiant",
        )
        .style(InteractionButtonStyle::Success)
        .emoji("🟢"),
        InteractionButton::new(
            session.map_or_else(
                || "draft_pref_dire".to_owned(),
                |session| format!("draft:{session}:pref:dire"),
            ),
            "Prefer Dire",
        )
        .style(InteractionButtonStyle::Danger)
        .emoji("🔴"),
        InteractionButton::new(
            session.map_or_else(
                || "draft_pref_clear".to_owned(),
                |session| format!("draft:{session}:pref:clear"),
            ),
            "Clear Preference",
        )
        .style(InteractionButtonStyle::Secondary)
        .emoji("❌"),
    ]));
    rows
}

fn draft_render_state(state: &DraftState, player_names: &GuildPlayerNameDirectory) -> DraftState {
    let mut rendered = state.clone();
    for (discord_id, player) in &mut rendered.player_pool_data {
        player.name = player_names.resolve(*discord_id, &player.name).to_owned();
    }
    rendered
}

fn opening_embed(state: &DraftState, starter: &str) -> InteractionEmbed {
    let captain1 = state
        .captain1_id
        .and_then(|id| state.player_pool_data.get(&id))
        .map_or("Unknown", |player| player.name.as_str());
    let captain2 = state
        .captain2_id
        .and_then(|id| state.player_pool_data.get(&id))
        .map_or("Unknown", |player| player.name.as_str());
    let winner = if state.coinflip_winner_id == state.captain1_id {
        captain1
    } else {
        captain2
    };
    interaction_embed(DraftEmbed {
        title: format!(
            "🎲 IMMORTAL DRAFT — {}",
            to_runtime_kind(state.lobby_kind).label()
        ),
        description: String::new(),
        footer: Some(format!("Started by {starter}")),
        fields: vec![
            DraftEmbedField {
                name: "👑 Captains".to_owned(),
                value: format!(
                    "**{captain1}** ({:.0})\n**{captain2}** ({:.0})",
                    state.captain1_rating, state.captain2_rating
                ),
                inline: false,
            },
            DraftEmbedField {
                name: "🎰 Coinflip Result".to_owned(),
                value: format!("**{winner}** won the coinflip!"),
                inline: false,
            },
            DraftEmbedField {
                name: "Next Step".to_owned(),
                value: format!("**{winner}**, choose whether to pick Side or Hero Pick Order."),
                inline: false,
            },
            DraftEmbedField {
                name: "📋 Player Pool".to_owned(),
                value: build_pool_field(state),
                inline: false,
            },
        ],
    })
}

fn choice_embed(state: &DraftState, title: &str, description: &str) -> InteractionResponse {
    InteractionResponse::message("").embed(interaction_embed(DraftEmbed {
        title: format!(
            "🎲 IMMORTAL DRAFT — {}",
            to_runtime_kind(state.lobby_kind).label()
        ),
        description: description.to_owned(),
        fields: vec![
            DraftEmbedField {
                name: title.to_owned(),
                value: description.to_owned(),
                inline: false,
            },
            DraftEmbedField {
                name: "📋 Player Pool".to_owned(),
                value: build_pool_field(state),
                inline: false,
            },
        ],
        ..DraftEmbed::default()
    }))
}

fn live_embed(state: &DraftState) -> InteractionEmbed {
    let current = state
        .current_captain_id()
        .and_then(|id| state.player_pool_data.get(&id))
        .map_or("N/A", |player| player.name.as_str());
    let team = state.current_captain_team().unwrap_or("N/A");
    let first_is_radiant = state.current_round_first_captain_id == state.radiant_captain_id;
    let (first, second) = if first_is_radiant {
        ("🟢 Radiant", "🔴 Dire")
    } else {
        ("🔴 Dire", "🟢 Radiant")
    };
    interaction_embed(DraftEmbed {
        title: format!(
            "⚔️ IMMORTAL DRAFT — {} - Player Selection",
            to_runtime_kind(state.lobby_kind).label()
        ),
        description: format!(
            "**Round {}/4:** {first} → {second}\n**Team Totals:** 🟢 {:.0} • 🔴 {:.0}\n\n**{current}** ({}) to pick! (Pick {}/2)",
            (state.current_pick_index / 2 + 1).min(4),
            state.radiant_rating_total(),
            state.dire_rating_total(),
            title_case(team),
            state.current_pick_index % 2 + 1,
        ),
        footer: Some(format!(
            "Pick #{}/{} | Click a player name to pick",
            (state.current_pick_index + 1).min(DRAFT_TOTAL_PICKS),
            DRAFT_TOTAL_PICKS
        )),
        fields: vec![
            DraftEmbedField {
                name: format!("🟢 Radiant ({}/5)", state.radiant_player_ids.len()),
                value: team_field(state, &state.radiant_player_ids),
                inline: true,
            },
            DraftEmbedField {
                name: format!("🔴 Dire ({}/5)", state.dire_player_ids.len()),
                value: team_field(state, &state.dire_player_ids),
                inline: true,
            },
            DraftEmbedField {
                name: format!("📋 Available ({})", state.available_player_ids().len()),
                value: available_field(state),
                inline: false,
            },
            DraftEmbedField {
                name: "ℹ️ In-Game Hero Draft".to_owned(),
                value: format!(
                    "**{}** picks first",
                    if state.radiant_hero_pick_order == Some(1) {
                        "Radiant"
                    } else {
                        "Dire"
                    }
                ),
                inline: false,
            },
        ],
    })
}

fn sample_live_embed(state: &DraftState) -> InteractionEmbed {
    live_embed(state)
}

fn completion_embed(state: &DraftState, pending: &PendingMatchRecord) -> InteractionEmbed {
    let first_team = if state.radiant_hero_pick_order == Some(1) {
        "Radiant"
    } else {
        "Dire"
    };
    let mut fields = vec![
        DraftEmbedField {
            name: format!("🟢 Radiant ({:.0})", pending.state.radiant_value),
            value: team_field(state, &state.radiant_player_ids),
            inline: true,
        },
        DraftEmbedField {
            name: format!("🔴 Dire ({:.0})", pending.state.dire_value),
            value: team_field(state, &state.dire_player_ids),
            inline: true,
        },
        DraftEmbedField {
            name: "📊 Balance".to_owned(),
            value: format!("**Value diff:** {:.0}", pending.state.value_diff),
            inline: false,
        },
    ];
    if let Some(blind) = pending.state.blind_bets_result.as_ref()
        && blind
            .get("created")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            > 0
    {
        fields.push(DraftEmbedField {
            name: if pending.state.is_bomb_pot {
                "💣 Bomb Pot Stakes".to_owned()
            } else {
                "🎲 Blind Bets".to_owned()
            },
            value: blind_bet_display(blind, pending.state.is_bomb_pot),
            inline: false,
        });
    }
    if let Some(spectator) = pending
        .state
        .blind_bets_result
        .as_ref()
        .and_then(|result| result.get("spectator_bets"))
        && spectator
            .get("created")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            > 0
    {
        fields.push(DraftEmbedField {
            name: "💸 Spectator Auto-Wagers".to_owned(),
            value: spectator_bet_display(spectator),
            inline: false,
        });
    }
    interaction_embed(DraftEmbed {
        title: format!(
            "{} IMMORTAL DRAFT — {} — Match #{} - Complete!",
            if pending.state.is_bomb_pot {
                "💣 BOMB POT 💣"
            } else {
                "⚔️"
            },
            to_runtime_kind(state.lobby_kind).label(),
            pending.pending_match_id
        ),
        description: format!("**{first_team}** picks first in hero draft"),
        footer: Some("Use /record to record the match result when finished.".to_owned()),
        fields,
    })
}

fn blind_bet_display(blind: &Value, is_bomb_pot: bool) -> String {
    let created = blind
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let radiant = blind
        .get("total_radiant")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let dire = blind
        .get("total_dire")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if is_bomb_pot {
        let percentage = blind
            .get("percentage")
            .and_then(Value::as_f64)
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

fn spectator_bet_display(spectator: &Value) -> String {
    let created = spectator
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let radiant = spectator
        .get("total_radiant")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let dire = spectator
        .get("total_dire")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    format!(
        "**Auto-wagers:** {created} spectators contributed\n🟢 Radiant: {radiant} {JOPACOIN_EMOTE} | 🔴 Dire: {dire} {JOPACOIN_EMOTE}"
    )
}

fn interaction_embed(embed: DraftEmbed) -> InteractionEmbed {
    InteractionEmbed {
        title: Some(embed.title),
        description: (!embed.description.is_empty()).then_some(embed.description),
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

fn timeout_embed() -> InteractionEmbed {
    interaction_embed(DraftEmbed {
        title: "⏰ Draft Timed Out".to_owned(),
        description: "The draft was automatically cancelled due to inactivity.\n\nThe lobby has been preserved. Use `/draft start` to begin a new draft.".to_owned(),
        ..DraftEmbed::default()
    })
}

fn build_pool_field(state: &DraftState) -> String {
    let mut players = state
        .player_pool_ids
        .iter()
        .copied()
        .filter(|id| Some(*id) != state.captain1_id && Some(*id) != state.captain2_id)
        .filter_map(|id| state.player_pool_data.get(&id))
        .collect::<Vec<_>>();
    players.sort_by(|left, right| {
        right
            .rating
            .partial_cmp(&left.rating)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if players.is_empty() {
        return "No players in pool".to_owned();
    }
    players
        .into_iter()
        .map(|player| {
            format!(
                "{} ({:.0}) {}",
                player.name,
                player.rating,
                format_roles(&player.roles)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn team_field(state: &DraftState, ids: &[i64]) -> String {
    if ids.is_empty() {
        return "Empty".to_owned();
    }
    ids.iter()
        .map(|id| {
            let captain =
                Some(*id) == state.radiant_captain_id || Some(*id) == state.dire_captain_id;
            let player = state.player_pool_data.get(id);
            let name = player.map_or_else(|| format!("Player {id}"), |player| player.name.clone());
            let rating = player.map_or(1500.0, |player| player.rating);
            format!(
                "{} {} ({rating:.0})",
                if captain { "👑" } else { "⠀⠀" },
                name
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn available_field(state: &DraftState) -> String {
    state
        .available_player_ids()
        .into_iter()
        .filter_map(|id| state.player_pool_data.get(&id).map(|player| (id, player)))
        .map(|(id, player)| {
            let preference = match state.side_preferences.get(&id) {
                Some(side) if side == "radiant" => " 🟢",
                Some(side) if side == "dire" => " 🔴",
                _ => "",
            };
            format!(
                "{} ({:.0}) {}{preference}",
                player.name,
                player.rating,
                format_roles(&player.roles)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_roles(roles: &[String]) -> String {
    if roles.is_empty() {
        return String::new();
    }
    let mut roles = roles.iter().cloned().enumerate().collect::<Vec<_>>();
    roles.sort_by_key(|(index, role)| (role.parse::<i64>().unwrap_or(99), *index));
    format!(
        "[{}]",
        roles
            .into_iter()
            .map(|(_, role)| role)
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn sample_state(guild_id: i64, complete: bool) -> DraftState {
    let mut state = DraftState::with_session(guild_id, DraftLobbyKind::Open, 0);
    state.captain1_id = Some(-101);
    state.captain2_id = Some(-102);
    state.captain1_rating = 1650.0;
    state.captain2_rating = 1580.0;
    state.radiant_captain_id = Some(-101);
    state.dire_captain_id = Some(-102);
    state.coinflip_winner_id = Some(-101);
    state.radiant_hero_pick_order = Some(1);
    state.dire_hero_pick_order = Some(2);
    state.player_pool_ids = vec![-101, -102, -103, -104, -105, -106, -107, -108, -109, -110];
    state.excluded_player_ids = vec![-111, -112];
    for (id, name, rating, roles) in sample_players() {
        state.player_pool_data.insert(
            id,
            PlayerPoolEntry {
                name: name.to_owned(),
                rating,
                roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            },
        );
    }
    state.radiant_player_ids = if complete {
        vec![-101, -103, -105, -107, -109]
    } else {
        vec![-101]
    };
    state.dire_player_ids = if complete {
        vec![-102, -104, -106, -108, -110]
    } else {
        vec![-102, -103]
    };
    state.side_preferences.insert(-105, "radiant".to_owned());
    state.side_preferences.insert(-107, "dire".to_owned());
    state.current_pick_index = if complete { DRAFT_TOTAL_PICKS } else { 1 };
    state.phase = if complete {
        DraftPhase::Complete
    } else {
        DraftPhase::Drafting
    };
    state.current_round_first_captain_id = Some(-102);
    state
}

fn sample_players() -> Vec<(i64, &'static str, f64, &'static [&'static str])> {
    vec![
        (-101, "SampleCapt1", 1650.0, &["1", "2"]),
        (-102, "SampleCapt2", 1580.0, &["1", "3"]),
        (-103, "SamplePlayer3", 1720.0, &["2"]),
        (-104, "SamplePlayer4", 1540.0, &["3", "4"]),
        (-105, "SamplePlayer5", 1610.0, &["4", "5"]),
        (-106, "SamplePlayer6", 1490.0, &["5"]),
        (-107, "SamplePlayer7", 1555.0, &["1", "2", "3"]),
        (-108, "SamplePlayer8", 1480.0, &["4", "5"]),
        (-109, "SamplePlayer9", 1630.0, &["2", "3"]),
        (-110, "SamplePlayer10", 1520.0, &["3", "4", "5"]),
        (-111, "ExcludedPlayer1", 1450.0, &["5"]),
        (-112, "ExcludedPlayer2", 1410.0, &["4", "5"]),
    ]
}

fn db_pending_state(
    state: &DraftState,
    now: i64,
    bet_lock_seconds: i64,
    is_bomb_pot: bool,
) -> Result<PendingMatchState, String> {
    let radiant_value = state.radiant_rating_total();
    let dire_value = state.dire_rating_total();
    let lock_until = now
        .checked_add(bet_lock_seconds)
        .ok_or_else(|| "betting lock timestamp overflow".to_owned())?;
    Ok(PendingMatchState {
        radiant_team_ids: state.radiant_player_ids.clone(),
        dire_team_ids: state.dire_player_ids.clone(),
        excluded_player_ids: state.excluded_player_ids.clone(),
        excluded_conditional_player_ids: state.conditional_exclusion_ids().to_vec(),
        radiant_value,
        dire_value,
        value_diff: (radiant_value - dire_value).abs(),
        first_pick_team: Some(
            if state.radiant_hero_pick_order == Some(1) {
                "Radiant"
            } else {
                "Dire"
            }
            .to_owned(),
        ),
        shuffle_timestamp: Some(now),
        bet_lock_until: Some(lock_until),
        betting_mode: "pool".to_owned(),
        is_bomb_pot,
        is_draft: true,
        lobby_kind: Some(state.lobby_kind.value().to_owned()),
        exclusion_updates_deferred: true,
        full_exclusion_increment_ids: state.full_exclusion_increment_ids.clone(),
        half_exclusion_increment_ids: state.half_exclusion_increment_ids.clone(),
        shuffle_message_id: state.draft_message_id,
        shuffle_channel_id: state.draft_channel_id,
        origin_channel_id: state.draft_channel_id,
        ..PendingMatchState::default()
    })
}

fn rollback_final_pick_state(state: &DraftState) -> Result<DraftState, String> {
    if state.phase != DraftPhase::Complete || state.current_pick_index < DRAFT_TOTAL_PICKS {
        return Err("unlinked Draft finalization row is not a completed final pick".to_owned());
    }
    let first_captain = state
        .current_round_first_captain_id
        .ok_or_else(|| "completed Draft has no final-round captain identity".to_owned())?;
    let previous_pick_index = state.current_pick_index.saturating_sub(1);
    let picker = if previous_pick_index.is_multiple_of(2) {
        Some(first_captain)
    } else if state.captain1_id == Some(first_captain) {
        state.captain2_id
    } else {
        state.captain1_id
    };
    let picker =
        picker.ok_or_else(|| "completed Draft has no opposing final-round captain".to_owned())?;
    let mut restored = state.clone();
    let team = if restored.radiant_captain_id == Some(picker) {
        &mut restored.radiant_player_ids
    } else if restored.dire_captain_id == Some(picker) {
        &mut restored.dire_player_ids
    } else {
        return Err("completed Draft final picker is not assigned to a team".to_owned());
    };
    team.pop().ok_or_else(|| {
        "completed Draft final picker has no selected player to roll back".to_owned()
    })?;
    restored.current_pick_index = previous_pick_index;
    restored.phase = DraftPhase::Drafting;
    Ok(restored)
}

fn assign_captains(state: &mut DraftState, chooser: i64, other: i64, side: Side) {
    match side {
        Side::Radiant => {
            state.radiant_captain_id = Some(chooser);
            state.dire_captain_id = Some(other);
        }
        Side::Dire => {
            state.dire_captain_id = Some(chooser);
            state.radiant_captain_id = Some(other);
        }
    }
}

fn complete_pre_draft(state: &mut DraftState) {
    let hero_choice = if state.winner_choice_type.as_deref() == Some("side") {
        state.loser_choice_value.as_deref()
    } else {
        state.winner_choice_value.as_deref()
    };
    let chooser = if state.winner_choice_type.as_deref() == Some("hero_pick") {
        state.coinflip_winner_id
    } else {
        state
            .captain1_id
            .filter(|id| Some(*id) != state.coinflip_winner_id)
            .or(state.captain2_id)
    };
    let chooser_is_radiant = chooser == state.radiant_captain_id;
    let first = hero_choice == Some("first");
    state.radiant_hero_pick_order = Some(if chooser_is_radiant == first { 1 } else { 2 });
    state.dire_hero_pick_order = Some(if chooser_is_radiant == first { 2 } else { 1 });
    state.start_player_draft();
}

fn wrong_choice_message(phase: DraftPhase) -> &'static str {
    if phase == DraftPhase::Complete {
        "⏳ Draft results are still being finalized. Please wait."
    } else {
        "❌ That choice has already been made."
    }
}

fn other_captain(state: &DraftState, captain: i64) -> Option<i64> {
    if state.captain1_id == Some(captain) {
        state.captain2_id
    } else if state.captain2_id == Some(captain) {
        state.captain1_id
    } else {
        None
    }
}

fn to_draft_kind(kind: AppLobbyKind) -> DraftLobbyKind {
    match kind {
        AppLobbyKind::Open => DraftLobbyKind::Open,
        AppLobbyKind::LowSkill => DraftLobbyKind::LowSkill,
    }
}

fn to_runtime_kind(kind: DraftLobbyKind) -> AppLobbyKind {
    match kind {
        DraftLobbyKind::Open => AppLobbyKind::Open,
        DraftLobbyKind::LowSkill => AppLobbyKind::LowSkill,
    }
}

fn is_admin(config: &DraftConfig, user_id: i64, permissions: Option<u64>) -> bool {
    config.admin_user_ids.contains(&user_id)
        || permissions.is_some_and(|permissions| {
            permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
        })
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn percentage_points(value: f64) -> i64 {
    if value <= 0.0 {
        0
    } else if value < 1.0 {
        (value * 100.0).round() as i64
    } else {
        value.round() as i64
    }
}

fn f64_bits(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

fn percentage_basis_points(value: f64) -> i64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value * 10_000.0).round() as i64
    }
}

fn betting_team_name(team: BettingTeam) -> &'static str {
    match team {
        BettingTeam::Radiant => "radiant",
        BettingTeam::Dire => "dire",
    }
}

fn blind_outcome_json(outcome: &cama_db::betting_service_repository::BlindBetOutcome) -> Value {
    let bets = outcome
        .created
        .iter()
        .map(|bet| {
            json!({
                "discord_id": bet.discord_id,
                "team": betting_team_name(bet.team),
                "amount": bet.amount,
            })
        })
        .collect::<Vec<_>>();
    let skipped = outcome
        .skipped
        .iter()
        .map(|skip| {
            json!({
                "discord_id": skip.discord_id,
                "reason": skip.reason,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "created": outcome.created.len(),
        "total_radiant": outcome.total_radiant,
        "total_dire": outcome.total_dire,
        "is_bomb_pot": outcome.is_bomb_pot,
        "bets": bets,
        "skipped": skipped,
        "percentage": if outcome.is_bomb_pot { 0.15 } else { 0.1 },
    })
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn discord_jump_url(guild_id: i64, channel_id: u64, message_id: u64) -> String {
    format!(
        "https://discord.com/channels/{}/{channel_id}/{message_id}",
        guild_id.max(0)
    )
}

/// Decode one durable delivery marker. `Some(None)` means the destination was
/// intentionally absent/skipped; `None` means the crash boundary occurred
/// before its progress CAS and the delivery must be retried by nonce.
fn persisted_delivery(progress_json: &str, name: &str) -> Option<Option<DiscordMessageReceipt>> {
    let value: Value = serde_json::from_str(progress_json).ok()?;
    let delivery = value.get("deliveries")?.get(name)?;
    if delivery
        .get("skipped")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some(None);
    }
    let channel_id = delivery.get("channel_id")?.as_u64()?;
    let message_id = delivery.get("message_id")?.as_u64()?;
    let jump_url = delivery
        .get("jump_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some(Some(DiscordMessageReceipt {
        channel_id,
        message_id,
        jump_url,
    }))
}

fn signed_id(id: u64, kind: &str) -> Result<i64, String> {
    i64::try_from(id).map_err(|_| format!("Discord {kind} ID exceeds SQLite range"))
}

fn signed_destination_id(id: Option<u64>, kind: &str) -> Result<Option<i64>, String> {
    id.map(|id| signed_id(id, kind)).transpose()
}

fn to_i64(id: u64) -> Result<i64, String> {
    signed_id(id, "Discord")
}

fn to_u64(id: i64) -> Result<u64, String> {
    u64::try_from(id).map_err(|_| format!("SQLite ID {id} is negative"))
}

fn lock_state<'a>(state: &'a Arc<Mutex<DraftState>>) -> MutexGuard<'a, DraftState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .respond(
            InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .followup(
            InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

async fn delete_followup(
    responder: &Arc<dyn InteractionResponder>,
    receipt: Option<crate::registration::InteractionMessageReceipt>,
) {
    if let Some(receipt) = receipt {
        let _ = responder.delete_message(receipt).await;
    }
}

async fn completion_response(
    responder: &Arc<dyn InteractionResponder>,
    pending_match_id: Option<i64>,
    _error: String,
    deferred: bool,
) -> Result<(), String> {
    let content = pending_match_id.map_or_else(
        || COMPLETION_ERROR_WITHOUT_MATCH.to_owned(),
        |id| format!(
            "⚠️ Draft complete — Match #{id} was created, but an error occurred while announcing it. Betting and recording still work: play the match and use `/record`, or use `/record abort` to cancel it."
        ),
    );
    let response = InteractionResponse::message(content)
        .ephemeral()
        .without_mentions();
    if deferred {
        responder.followup(response).await
    } else {
        responder.respond(response).await
    }
    .map_err(|error| error.to_string())
}

async fn financial_setup_retry_response(
    responder: &Arc<dyn InteractionResponder>,
    pending_match_id: i64,
    _error: String,
    deferred: bool,
) -> Result<(), String> {
    let response = InteractionResponse::message(format!(
        "⚠️ Draft complete — Match #{pending_match_id} is safely retained, but its financial setup did not finish. Retry finalization before playing or recording the match. No financial effects were partially applied."
    ))
    .ephemeral()
    .without_mentions();
    if deferred {
        responder.followup(response).await
    } else {
        responder.respond(response).await
    }
    .map_err(|error| error.to_string())
}

#[cfg(all(test, feature = "runtime-test-draft"))]
#[path = "draft_provider/tests.rs"]
mod tests;
