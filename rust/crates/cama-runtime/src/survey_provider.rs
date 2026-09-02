//! Production Discord composition for one-off surveys.
//!
//! The Python implementation's durable SQLite state remains the authority:
//! every response mutation commits before a Discord edit, ambiguous DM sends
//! are reconciled from history before requeue, and READY/reconnect recovery is
//! repeatable. No respondent answer text is logged or retained outside the
//! short-lived render value used for an edit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::survey_commands as policy;
use cama_db::survey as db;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tracing::{error, warn};

use crate::discord_transport::{DiscordAllowedMentions, DiscordMessage, DiscordMessageReceipt};
use crate::gateway::ReconnectPolicy;
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::ids::blocking as sqlite;
use crate::option_ext::{
    boolean_option as optional_boolean, option_value, string_option as optional_string, user_option,
};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionAcknowledgementPolicy, InteractionActionRow, InteractionButton,
    InteractionButtonStyle, InteractionEmbed, InteractionHandler, InteractionHandlerError,
    InteractionModal, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionStringSelect, InteractionStringSelectOption,
    InteractionTextInput, InteractionTextInputStyle, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};
use crate::worker::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const SURVEY_RECOVERY_WORKER_NAME: &str = "survey_scheduled_delivery_recovery";
pub const SURVEY_RECOVERY_WAKE_INTERVAL: Duration = Duration::from_secs(30);
const SURVEY_RESPONSE_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(30 * 60);
pub const SURVEY_RESULTS_TIMEOUT: Duration =
    Duration::from_secs(policy::RESULTS_TIMEOUT_SECONDS as u64);
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const SURVEY_COMPONENT_PREFIX: &str = "survey:";
const TEXT_MODAL_FIELD: &str = "answer";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurveyDmHistory {
    pub channel_id: i64,
    pub bot_user_id: i64,
    pub messages: Vec<policy::HistoryMessage>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyDmErrorKind {
    Forbidden,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurveyDmError {
    pub kind: SurveyDmErrorKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurveyEditErrorKind {
    Forbidden,
    NotFound,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurveyEditError {
    pub kind: SurveyEditErrorKind,
    pub message: String,
}

/// Survey-specific live Discord operations. The concrete Serenity adapter
/// supplies the send nonce/history behavior unavailable on the generic port.
#[async_trait]
pub trait SurveyDiscordPort: Send + Sync {
    async fn survey_guild_snapshot(&self, guild_id: i64) -> Result<policy::GuildSnapshot, String>;

    async fn survey_role_snapshot(
        &self,
        guild_id: i64,
        role_id: i64,
    ) -> Result<Option<policy::RoleSnapshot>, String>;

    async fn survey_send_dm(
        &self,
        discord_id: i64,
        message: DiscordMessage,
        nonce: &str,
    ) -> Result<DiscordMessageReceipt, SurveyDmError>;

    async fn survey_dm_history(
        &self,
        discord_id: i64,
        after_unix_seconds: i64,
        limit: usize,
    ) -> Result<SurveyDmHistory, String>;

    async fn survey_edit_dm(
        &self,
        channel_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), SurveyEditError>;
}

#[derive(Debug, Error)]
pub enum SurveyProviderBuildError {
    #[error("survey provider database is not migrated: {0}")]
    Database(#[from] db::SurveyRepositoryError),
}

#[derive(Clone)]
pub struct SurveyRegistrationProvider {
    state: Arc<SurveyRuntimeState>,
    handler: Arc<SurveyInteractionHandler>,
}

impl SurveyRegistrationProvider {
    pub fn new(
        database_path: impl AsRef<Path>,
        discord: Arc<dyn SurveyDiscordPort>,
    ) -> Result<Self, SurveyProviderBuildError> {
        Self::new_with_results_timeout(database_path, discord, SURVEY_RESULTS_TIMEOUT)
    }

    fn new_with_results_timeout(
        database_path: impl AsRef<Path>,
        discord: Arc<dyn SurveyDiscordPort>,
        results_timeout: Duration,
    ) -> Result<Self, SurveyProviderBuildError> {
        let repository = db::SurveyRepository::new(database_path);
        repository.verify_schema()?;
        let state = Arc::new(SurveyRuntimeState {
            repository,
            discord,
            delivery_lock: AsyncMutex::new(()),
            ready_lock: AsyncMutex::new(()),
            recovery_edits: Arc::new(Semaphore::new(policy::DELIVERY_CONCURRENCY)),
            delivery_semaphore: Arc::new(Semaphore::new(policy::DELIVERY_CONCURRENCY)),
            response_locks: Mutex::new(BTreeMap::new()),
            fingerprints: Mutex::new(BTreeMap::new()),
            active_guilds: Mutex::new(BTreeSet::new()),
            response_edit_retries: Mutex::new(BTreeMap::new()),
            permanently_uneditable_responses: Mutex::new(BTreeSet::new()),
            pagers: Mutex::new(BTreeMap::new()),
            results_timeout,
        });
        Ok(Self {
            handler: Arc::new(SurveyInteractionHandler {
                state: Arc::clone(&state),
            }),
            state,
        })
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(SurveyReadyRecoveryObserver {
            state: Arc::clone(&self.state),
        })
    }

    #[must_use]
    pub fn recovery_worker_spec(&self) -> BackgroundWorkerSpec {
        BackgroundWorkerSpec::new(
            SURVEY_RECOVERY_WORKER_NAME,
            Arc::new(SurveyRecoveryWorker {
                state: Arc::clone(&self.state),
            }),
        )
        .restart_policy(ReconnectPolicy::new(
            Duration::from_secs(5),
            Duration::from_secs(policy::DELIVERY_STALE_SECONDS as u64),
        ))
    }
}

impl RegistrationProvider for SurveyRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "survey".to_owned(),
            description: "Create and manage private one-off surveys".to_owned(),
            options: survey_subcommands(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: SURVEY_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

type SurveyResponseKey = (i64, i64);
type SurveyResponseLock = Arc<AsyncMutex<()>>;
type SurveyPagerHandle = Arc<AsyncMutex<SurveyPager>>;

#[derive(Clone)]
struct SurveyPagerControls {
    previous_custom_id: String,
    next_custom_id: String,
}

struct SurveyPagerTimeout(tokio::task::AbortHandle);

impl Drop for SurveyPagerTimeout {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct SurveyPager {
    owner_id: u64,
    pages: Vec<policy::Embed>,
    index: usize,
    controls: SurveyPagerControls,
    edit_responder: Arc<dyn InteractionResponder>,
    timeout_generation: u64,
    timeout_task: Option<SurveyPagerTimeout>,
    retired: bool,
}

struct SurveyRuntimeState {
    repository: db::SurveyRepository,
    discord: Arc<dyn SurveyDiscordPort>,
    delivery_lock: AsyncMutex<()>,
    ready_lock: AsyncMutex<()>,
    recovery_edits: Arc<Semaphore>,
    delivery_semaphore: Arc<Semaphore>,
    response_locks: Mutex<BTreeMap<SurveyResponseKey, SurveyResponseLock>>,
    fingerprints: Mutex<BTreeMap<SurveyResponseKey, u64>>,
    active_guilds: Mutex<BTreeSet<i64>>,
    response_edit_retries: Mutex<BTreeMap<SurveyResponseKey, SurveyResponseEditRetry>>,
    permanently_uneditable_responses: Mutex<BTreeSet<SurveyResponseKey>>,
    pagers: Mutex<BTreeMap<String, SurveyPagerHandle>>,
    results_timeout: Duration,
}

#[derive(Clone, Copy)]
struct SurveyResponseEditRetry {
    failures: u32,
    retry_at: Instant,
}

#[derive(Clone, Copy)]
enum SurveyRecoveryScope {
    Guild(i64),
    Survey(i64, i64),
}

impl SurveyRecoveryScope {
    const fn guild_id(self) -> i64 {
        match self {
            Self::Guild(guild_id) | Self::Survey(guild_id, _) => guild_id,
        }
    }
}

impl SurveyRuntimeState {
    fn replace_active_guilds(&self, guild_ids: &[u64]) {
        let guilds = guild_ids
            .iter()
            .filter_map(|&guild_id| i64::try_from(guild_id).ok())
            .collect();
        *self
            .active_guilds
            .lock()
            .expect("survey active guild registry poisoned") = guilds;
    }

    fn recovery_scopes(&self, scope: Option<(i64, i64)>) -> Vec<SurveyRecoveryScope> {
        match scope {
            Some((guild_id, survey_id)) => {
                vec![SurveyRecoveryScope::Survey(guild_id, survey_id)]
            }
            None => self
                .active_guilds
                .lock()
                .expect("survey active guild registry poisoned")
                .iter()
                .copied()
                .map(SurveyRecoveryScope::Guild)
                .collect(),
        }
    }

    fn response_edit_is_due(&self, key: SurveyResponseKey) -> bool {
        if self
            .permanently_uneditable_responses
            .lock()
            .expect("survey permanent edit registry poisoned")
            .contains(&key)
        {
            return false;
        }
        self.response_edit_retries
            .lock()
            .expect("survey response retry registry poisoned")
            .get(&key)
            .is_none_or(|retry| Instant::now() >= retry.retry_at)
    }

    fn defer_response_edit(&self, key: SurveyResponseKey) -> Duration {
        let mut retries = self
            .response_edit_retries
            .lock()
            .expect("survey response retry registry poisoned");
        let failures = retries
            .get(&key)
            .map_or(1, |retry| retry.failures.saturating_add(1));
        let exponent = failures.saturating_sub(1).min(6);
        let delay = SURVEY_RECOVERY_WAKE_INTERVAL
            .saturating_mul(1_u32 << exponent)
            .min(SURVEY_RESPONSE_RETRY_MAX_INTERVAL);
        retries.insert(
            key,
            SurveyResponseEditRetry {
                failures,
                retry_at: Instant::now() + delay,
            },
        );
        delay
    }

    fn clear_response_edit_failure(&self, key: SurveyResponseKey) {
        self.response_edit_retries
            .lock()
            .expect("survey response retry registry poisoned")
            .remove(&key);
        self.permanently_uneditable_responses
            .lock()
            .expect("survey permanent edit registry poisoned")
            .remove(&key);
    }

    fn response_lock(&self, survey_id: i64, discord_id: i64) -> Arc<AsyncMutex<()>> {
        self.response_locks
            .lock()
            .expect("survey response lock registry poisoned")
            .entry((survey_id, discord_id))
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn register_pager(
        &self,
        owner_id: u64,
        pages: Vec<policy::Embed>,
        edit_responder: Arc<dyn InteractionResponder>,
    ) -> Result<SurveyPagerHandle, String> {
        let mut routes = self.pagers.lock().expect("survey pager registry poisoned");
        loop {
            let random_id = random_page_id()?;
            let controls = SurveyPagerControls {
                previous_custom_id: format!("survey:page:{random_id}:previous"),
                next_custom_id: format!("survey:page:{random_id}:next"),
            };
            if routes.contains_key(&controls.previous_custom_id)
                || routes.contains_key(&controls.next_custom_id)
            {
                continue;
            }
            let pager = Arc::new(AsyncMutex::new(SurveyPager {
                owner_id,
                pages,
                index: 0,
                controls: controls.clone(),
                edit_responder,
                timeout_generation: 0,
                timeout_task: None,
                retired: false,
            }));
            routes.insert(controls.previous_custom_id, Arc::clone(&pager));
            routes.insert(controls.next_custom_id, Arc::clone(&pager));
            return Ok(pager);
        }
    }

    fn pager(&self, custom_id: &str) -> Option<SurveyPagerHandle> {
        self.pagers
            .lock()
            .expect("survey pager registry poisoned")
            .get(custom_id)
            .cloned()
    }

    fn remove_pager_routes(&self, pager: &SurveyPagerHandle, controls: &SurveyPagerControls) {
        let mut routes = self.pagers.lock().expect("survey pager registry poisoned");
        for custom_id in [&controls.previous_custom_id, &controls.next_custom_id] {
            if routes
                .get(custom_id)
                .is_some_and(|registered| Arc::ptr_eq(registered, pager))
            {
                routes.remove(custom_id);
            }
        }
    }

    fn arm_pager_timeout_locked(self: &Arc<Self>, pager: &mut SurveyPager) {
        pager.timeout_generation = pager.timeout_generation.saturating_add(1);
        let generation = pager.timeout_generation;
        let route_id = pager.controls.previous_custom_id.clone();
        let weak_state: Weak<Self> = Arc::downgrade(self);
        let timeout = self.results_timeout;
        let task = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if let Some(state) = weak_state.upgrade() {
                state.expire_pager(route_id, generation).await;
            }
        });
        pager.timeout_task = Some(SurveyPagerTimeout(task.abort_handle()));
    }

    async fn arm_initial_pager_timeout(self: &Arc<Self>, pager_handle: &SurveyPagerHandle) {
        let mut pager = pager_handle.lock().await;
        if !pager.retired && pager.timeout_generation == 0 {
            self.arm_pager_timeout_locked(&mut pager);
        }
    }

    async fn discard_pager(&self, pager_handle: &SurveyPagerHandle) {
        let (controls, timeout_task) = {
            let mut pager = pager_handle.lock().await;
            pager.retired = true;
            (pager.controls.clone(), pager.timeout_task.take())
        };
        self.remove_pager_routes(pager_handle, &controls);
        drop(timeout_task);
    }

    async fn expire_pager(self: Arc<Self>, route_id: String, generation: u64) {
        let Some(pager_handle) = self.pager(&route_id) else {
            return;
        };
        let (controls, responder, response) = {
            let mut pager = pager_handle.lock().await;
            if pager.retired || pager.timeout_generation != generation {
                return;
            }
            pager.retired = true;
            let response = pager_response(&pager, true);
            (
                pager.controls.clone(),
                Arc::clone(&pager.edit_responder),
                response,
            )
        };
        self.remove_pager_routes(&pager_handle, &controls);
        match response {
            Ok(response) => {
                if let Err(error) = responder.edit_original(response).await {
                    warn!(%error, "survey results view could not be disabled after timeout");
                }
            }
            Err(error) => warn!(%error, "survey results view could not be rendered at timeout"),
        }
    }

    fn start_dispatch(self: &Arc<Self>, scope: Option<(i64, i64)>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(message) = state.recover(scope).await {
                error!(error = %message, "survey background dispatch failed");
            }
        });
    }

    async fn recover(self: &Arc<Self>, scope: Option<(i64, i64)>) -> Result<usize, String> {
        let _delivery = self.delivery_lock.lock().await;
        let mut delivered = 0usize;
        for recovery_scope in self.recovery_scopes(scope) {
            loop {
                self.reconcile_receipts(recovery_scope).await?;
                let repository = self.repository.clone();
                let guild_id = recovery_scope.guild_id();
                sqlite("queue survey retries", move || {
                    repository
                        .queue_requested_delivery_retries_for_guild(Some(guild_id))
                        .map_err(|error| error.to_string())
                })
                .await?;
                let repository = self.repository.clone();
                sqlite("recover stale survey claims", move || {
                    repository
                        .recover_stale_deliveries_for_guild(
                            Some(guild_id),
                            policy::DELIVERY_STALE_SECONDS,
                        )
                        .map_err(|error| error.to_string())
                })
                .await?;
                let repository = self.repository.clone();
                let claims = sqlite("claim survey deliveries", move || {
                    let result = match recovery_scope {
                        SurveyRecoveryScope::Guild(_) => repository.claim_deliveries_for_guild(
                            Some(guild_id),
                            policy::DELIVERY_BATCH_SIZE,
                            policy::DELIVERY_STALE_SECONDS,
                        ),
                        SurveyRecoveryScope::Survey(_, survey_id) => repository
                            .claim_deliveries_for_survey(
                                guild_id,
                                survey_id,
                                policy::DELIVERY_BATCH_SIZE,
                                policy::DELIVERY_STALE_SECONDS,
                            ),
                    };
                    result.map_err(|error| error.to_string())
                })
                .await?;
                if claims.is_empty() {
                    break;
                }
                delivered = delivered.saturating_add(claims.len());
                let mut tasks = tokio::task::JoinSet::new();
                for claim in claims {
                    let state = Arc::clone(self);
                    tasks.spawn(async move { state.deliver(claim).await });
                }
                while let Some(result) = tasks.join_next().await {
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(message)) => {
                            warn!(error = %message, "survey delivery attempt failed")
                        }
                        Err(error) => warn!(%error, "survey delivery task panicked"),
                    }
                }
            }
            self.reconcile_response_messages(recovery_scope).await?;
        }
        Ok(delivered)
    }

    async fn deliver(self: &Arc<Self>, claim: db::SurveyRecipient) -> Result<(), String> {
        let _permit = self
            .delivery_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "survey delivery semaphore closed".to_owned())?;
        let repository = self.repository.clone();
        let survey_id = claim.survey_id;
        let discord_id = claim.discord_id;
        let session = match sqlite("load survey delivery session", move || {
            repository
                .get_response_session(survey_id, discord_id)
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(session) => session,
            Err(message) => {
                self.mark_failed(&claim, "SessionUnavailable").await?;
                return Err(message);
            }
        };
        let Some(session) = session else {
            self.mark_failed(&claim, "SessionUnavailable").await?;
            return Ok(());
        };
        let repository = self.repository.clone();
        let recipient_id = claim.recipient_id;
        let attempt_count = claim.attempt_count;
        if let Err(message) = sqlite("renew survey delivery claim", move || {
            repository
                .renew_delivery_claim(recipient_id, attempt_count)
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await
        {
            self.mark_failed(&claim, "ClaimUnavailable").await?;
            return Err(message);
        }
        let message = render_message(&session)?;
        let nonce = policy::delivery_nonce(claim.recipient_id);
        let receipt = match self
            .discord
            .survey_send_dm(claim.discord_id, message, &nonce)
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                let category = match error.kind {
                    SurveyDmErrorKind::Forbidden => "Forbidden",
                    SurveyDmErrorKind::Unavailable => "Unavailable",
                };
                self.mark_failed(&claim, category).await?;
                return Ok(());
            }
        };
        let channel_id = signed(receipt.channel_id, "DM channel")?;
        let message_id = signed(receipt.message_id, "DM message")?;
        let repository = self.repository.clone();
        let marked = sqlite("persist survey delivery receipt", move || {
            repository
                .mark_delivery_sent(recipient_id, attempt_count, channel_id, message_id)
                .map_err(|error| error.to_string())
        })
        .await;
        if marked.is_err() {
            let repository = self.repository.clone();
            sqlite("reconcile survey delivery receipt", move || {
                repository
                    .reconcile_delivery_sent(recipient_id, attempt_count, channel_id, message_id)
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .await?;
            let repository = self.repository.clone();
            if let Some(latest) = sqlite("reload reconciled survey delivery", move || {
                repository
                    .get_response_session(survey_id, discord_id)
                    .map_err(|error| error.to_string())
            })
            .await?
                && latest.survey.status == db::SurveyStatus::Closed
            {
                self.reconcile_response(latest)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    async fn mark_failed(
        &self,
        claim: &db::SurveyRecipient,
        category: &'static str,
    ) -> Result<(), String> {
        let repository = self.repository.clone();
        let recipient_id = claim.recipient_id;
        let attempt_count = claim.attempt_count;
        sqlite("persist survey delivery failure", move || {
            repository
                .mark_delivery_failed(recipient_id, attempt_count, category)
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn reconcile_receipts(&self, scope: SurveyRecoveryScope) -> Result<usize, String> {
        let repository = self.repository.clone();
        let recipients = sqlite("list unconfirmed survey deliveries", move || {
            let result = match scope {
                SurveyRecoveryScope::Guild(guild_id) => repository
                    .list_unconfirmed_deliveries_for_guild(
                        guild_id,
                        policy::DELIVERY_STALE_SECONDS,
                        policy::DELIVERY_RECEIPT_GRACE_SECONDS,
                    ),
                SurveyRecoveryScope::Survey(guild_id, survey_id) => repository
                    .list_unconfirmed_deliveries(
                        Some((guild_id, survey_id)),
                        policy::DELIVERY_STALE_SECONDS,
                        policy::DELIVERY_RECEIPT_GRACE_SECONDS,
                    ),
            };
            result.map_err(|error| error.to_string())
        })
        .await?;
        let count = recipients.len();
        let mut tasks = tokio::task::JoinSet::new();
        for recipient in recipients {
            let repository = self.repository.clone();
            let discord = Arc::clone(&self.discord);
            let semaphore = Arc::clone(&self.delivery_semaphore);
            tasks.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| "survey delivery semaphore closed".to_owned())?;
                let history = discord
                    .survey_dm_history(
                        recipient.discord_id,
                        recipient.created_at.saturating_sub(30),
                        policy::DELIVERY_RECEIPT_SCAN_LIMIT,
                    )
                    .await?;
                match policy::reconcile_delivery_receipt(
                    &app_recipient(&recipient),
                    history.bot_user_id,
                    history.channel_id,
                    &history.messages,
                ) {
                    policy::ReceiptReconciliation::Found(message) => {
                        sqlite("recover survey receipt from DM history", move || {
                            repository
                                .reconcile_delivery_sent(
                                    recipient.recipient_id,
                                    recipient.attempt_count,
                                    message.channel_id,
                                    message.message_id,
                                )
                                .map(drop)
                                .map_err(|error| error.to_string())
                        })
                        .await
                    }
                    policy::ReceiptReconciliation::Checked(_) => {
                        sqlite("mark survey receipt inspected", move || {
                            repository
                                .mark_delivery_receipt_checked(&recipient)
                                .map(drop)
                                .map_err(|error| error.to_string())
                        })
                        .await
                    }
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(message)) => {
                    // Keep an unchecked attempt ineligible for requeue.
                    warn!(error = %message, "survey receipt inspection deferred");
                }
                Err(error) => warn!(%error, "survey receipt task panicked"),
            }
        }
        Ok(count)
    }

    async fn reconcile_response_messages(
        self: &Arc<Self>,
        scope: SurveyRecoveryScope,
    ) -> Result<usize, String> {
        let repository = self.repository.clone();
        let sessions = sqlite("list recoverable survey responses", move || {
            repository
                .list_recoverable_response_sessions_for_guild(Some(scope.guild_id()))
                .map_err(|error| error.to_string())
        })
        .await?;
        let sessions = sessions
            .into_iter()
            .filter(|session| match scope {
                SurveyRecoveryScope::Guild(_) => true,
                SurveyRecoveryScope::Survey(_, survey_id) => session.survey.survey_id == survey_id,
            })
            .filter(|session| {
                self.response_edit_is_due((session.survey.survey_id, session.recipient.discord_id))
            })
            .collect::<Vec<_>>();
        let count = sessions.len();
        let mut tasks = tokio::task::JoinSet::new();
        for session in sessions {
            let state = Arc::clone(self);
            tasks.spawn(async move { state.reconcile_response(session).await });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(message)) => warn!(error = %message, "survey response edit deferred"),
                Err(error) => warn!(%error, "survey response reconciliation task panicked"),
            }
        }
        Ok(count)
    }

    async fn reconcile_response(&self, snapshot: db::SurveySession) -> Result<(), String> {
        let _permit = self
            .recovery_edits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "survey recovery semaphore closed".to_owned())?;
        let response_lock =
            self.response_lock(snapshot.survey.survey_id, snapshot.recipient.discord_id);
        let _response = response_lock.lock().await;
        let repository = self.repository.clone();
        let survey_id = snapshot.survey.survey_id;
        let discord_id = snapshot.recipient.discord_id;
        let Some(latest) = sqlite("reload survey response", move || {
            repository
                .get_response_session(survey_id, discord_id)
                .map_err(|error| error.to_string())
        })
        .await?
        else {
            return Ok(());
        };
        let Some(message_id) = latest
            .recipient
            .ui_message_id
            .or(latest.recipient.dm_message_id)
        else {
            return Ok(());
        };
        let channel_id = latest
            .recipient
            .ui_channel_id
            .or(latest.recipient.dm_channel_id)
            .ok_or_else(|| "survey response is missing its DM channel".to_owned())?;
        let app = app_session(&latest);
        let fingerprint = policy::response_state_fingerprint(&app, message_id);
        let key = (survey_id, discord_id);
        let changed = self
            .fingerprints
            .lock()
            .expect("survey fingerprint lock poisoned")
            .get(&key)
            .copied()
            != Some(fingerprint);
        if changed {
            match self
                .discord
                .survey_edit_dm(channel_id, message_id, render_message(&latest)?)
                .await
            {
                Ok(()) => self.clear_response_edit_failure(key),
                Err(error) if error.kind == SurveyEditErrorKind::Unavailable => {
                    let delay = self.defer_response_edit(key);
                    return Err(format!(
                        "{}; retrying in {}s",
                        error.message,
                        delay.as_secs()
                    ));
                }
                Err(error) => {
                    if latest.recipient.submitted_at.is_some()
                        || latest.survey.status == db::SurveyStatus::Closed
                    {
                        let repository = self.repository.clone();
                        sqlite("retire inaccessible survey response controls", move || {
                            repository
                                .finalize_response_ui(survey_id, discord_id, message_id)
                                .map(drop)
                                .map_err(|error| error.to_string())
                        })
                        .await?;
                    } else {
                        self.permanently_uneditable_responses
                            .lock()
                            .expect("survey permanent edit registry poisoned")
                            .insert(key);
                    }
                    warn!(
                        survey_id,
                        discord_id,
                        error = %error.message,
                        "survey response edit retired after permanent Discord rejection"
                    );
                    return Ok(());
                }
            }
            self.fingerprints
                .lock()
                .expect("survey fingerprint lock poisoned")
                .insert(key, fingerprint);
        }
        if latest.recipient.submitted_at.is_some()
            || latest.survey.status == db::SurveyStatus::Closed
        {
            let repository = self.repository.clone();
            sqlite("finalize survey response controls", move || {
                repository
                    .finalize_response_ui(survey_id, discord_id, message_id)
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .await?;
            self.fingerprints
                .lock()
                .expect("survey fingerprint lock poisoned")
                .remove(&key);
        }
        Ok(())
    }

    async fn finalize_if_terminal(&self, session: &db::SurveySession) -> Result<(), String> {
        if session.recipient.submitted_at.is_none()
            && session.survey.status != db::SurveyStatus::Closed
        {
            return Ok(());
        }
        let Some(message_id) = session
            .recipient
            .ui_message_id
            .or(session.recipient.dm_message_id)
        else {
            return Ok(());
        };
        let repository = self.repository.clone();
        let survey_id = session.survey.survey_id;
        let discord_id = session.recipient.discord_id;
        sqlite("finalize committed survey response controls", move || {
            repository
                .finalize_response_ui(survey_id, discord_id, message_id)
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await
    }
}

struct SurveyReadyRecoveryObserver {
    state: Arc<SurveyRuntimeState>,
}

#[async_trait]
impl GatewayEventObserver for SurveyReadyRecoveryObserver {
    fn name(&self) -> &'static str {
        "survey-ready-recovery"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let Some(_ready) = self.state.ready_lock.try_lock().ok() else {
            let mut report = ReadyRecoveryReport::empty(self.name(), context.guild_ids().len());
            report.guilds_superseded = context.guild_ids().len();
            return report;
        };
        self.state.replace_active_guilds(context.guild_ids());
        self.state
            .response_edit_retries
            .lock()
            .expect("survey response retry registry poisoned")
            .clear();
        let mut report = ReadyRecoveryReport::empty(self.name(), context.guild_ids().len());
        match self.state.recover(None).await {
            Ok(recovered) => {
                report.guilds_refreshed = context.guild_ids().len();
                report.members_refreshed = recovered;
            }
            Err(message) => {
                report.failures = context
                    .guild_ids()
                    .iter()
                    .map(|&guild_id| ReadyRecoveryFailure {
                        guild_id,
                        message: message.clone(),
                    })
                    .collect();
            }
        }
        report
    }
}

struct SurveyRecoveryWorker {
    state: Arc<SurveyRuntimeState>,
}

#[async_trait]
impl BackgroundWorker for SurveyRecoveryWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            self.state.recover(None).await?;
            if !context.sleep(SURVEY_RECOVERY_WAKE_INTERVAL).await {
                return Ok(());
            }
        }
    }
}

struct SurveyInteractionHandler {
    state: Arc<SurveyRuntimeState>,
}

#[async_trait]
impl InteractionHandler for SurveyInteractionHandler {
    fn acknowledgement_policy(
        &self,
        request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        match request {
            InteractionRequest::Component { custom_id, .. } if custom_id.ends_with(":text") => {
                InteractionAcknowledgementPolicy::Modal
            }
            _ => InteractionAcknowledgementPolicy::Automatic,
        }
    }

    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let result = match &request {
            InteractionRequest::Command { .. } => {
                self.handle_command(request, Arc::clone(&responder)).await
            }
            InteractionRequest::Component { .. } => {
                self.handle_component(request, Arc::clone(&responder)).await
            }
            InteractionRequest::Modal { .. } => {
                self.handle_modal(request, Arc::clone(&responder)).await
            }
            InteractionRequest::Autocomplete { .. } => {
                Err("survey does not support autocomplete".to_owned())
            }
        };
        if let Err(message) = result {
            respond_error(responder.as_ref(), &message).await;
        }
        Ok(())
    }
}

impl SurveyInteractionHandler {
    async fn handle_command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            name,
            user_id,
            guild_id,
            member_permissions,
            options,
            ..
        } = request
        else {
            unreachable!();
        };
        if name != "survey" {
            return Err(format!("survey handler received command {name:?}"));
        }
        let guild_id = signed(
            guild_id.ok_or_else(|| "This command can only be used in a server.".to_owned())?,
            "guild",
        )?;
        let permissions = member_permissions.unwrap_or_default();
        if permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) == 0 {
            return Err(
                "Admin only. You need Administrator or Manage Server permissions.".to_owned(),
            );
        }
        responder
            .defer_thinking(true)
            .await
            .map_err(|error| error.to_string())?;
        let (command, leaf) = leaf_command(&options)?;
        match command.as_str() {
            "create" => self.command_create(guild_id, leaf, responder).await,
            "list" => self.command_list(guild_id, leaf, responder).await,
            "preview" => {
                self.command_preview(guild_id, user_id, leaf, responder)
                    .await
            }
            "delete" => self.command_delete(guild_id, leaf, responder).await,
            "question add" => self.command_question_add(guild_id, leaf, responder).await,
            "question edit" => self.command_question_edit(guild_id, leaf, responder).await,
            "question remove" => {
                self.command_question_remove(guild_id, leaf, responder)
                    .await
            }
            "question move" => self.command_question_move(guild_id, leaf, responder).await,
            "send" => self.command_send(guild_id, leaf, responder).await,
            "retry" => self.command_retry(guild_id, leaf, responder).await,
            "results" => {
                self.command_results(guild_id, user_id, leaf, responder)
                    .await
            }
            "close" => self.command_close(guild_id, leaf, responder).await,
            _ => Err(format!("unsupported survey command {command:?}")),
        }
    }

    async fn command_create(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let title = string_option(options, "title")?;
        let repository = self.state.repository.clone();
        let survey = sqlite("create survey", move || {
            repository
                .create_survey(guild_id, &title)
                .map_err(|error| error.to_string())
        })
        .await?;
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!(
                "✅ Created survey **#{} — {}**. Add questions before sending it.",
                survey.survey_id,
                policy::escape_discord_text(&survey.title)
            ))
            .ephemeral()
            .without_mentions(),
        )
        .await
    }

    async fn command_list(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let status = optional_string(options, "status")
            .map(parse_status)
            .transpose()?;
        let repository = self.state.repository.clone();
        let surveys = sqlite("list surveys", move || {
            repository
                .list_surveys(guild_id, status)
                .map_err(|error| error.to_string())
        })
        .await?;
        if surveys.is_empty() {
            return followup(
                responder.as_ref(),
                InteractionResponse::message("No surveys found.")
                    .ephemeral()
                    .without_mentions(),
            )
            .await;
        }
        let mut embed = InteractionEmbed::titled("Surveys").color(0x58_65_F2);
        for survey in surveys.iter().take(20) {
            embed = embed.field(
                format!("#{} • {}", survey.survey_id, status_label(survey.status)),
                truncate(&policy::escape_discord_text(&survey.title), 300),
                false,
            );
        }
        if surveys.len() > 20 {
            embed = embed.footer(format!("Showing 20 of {} surveys", surveys.len()));
        }
        followup(
            responder.as_ref(),
            InteractionResponse::message("")
                .embed(embed)
                .ephemeral()
                .without_mentions(),
        )
        .await
    }

    async fn send_paged_response(
        &self,
        owner_id: u64,
        pages: Vec<policy::Embed>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if pages.len() <= 1 {
            let page = pages
                .first()
                .ok_or_else(|| "survey report has no pages".to_owned())?;
            return followup(responder.as_ref(), page_response(page)).await;
        }
        let pager = self
            .state
            .register_pager(owner_id, pages, Arc::clone(&responder))?;
        let response = {
            let pager = pager.lock().await;
            pager_response(&pager, false)?
        };
        if let Err(error) = followup(responder.as_ref(), response).await {
            self.state.discard_pager(&pager).await;
            return Err(error);
        }
        self.state.arm_initial_pager_timeout(&pager).await;
        Ok(())
    }

    async fn command_preview(
        &self,
        guild_id: i64,
        owner_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let repository = self.state.repository.clone();
        let survey = sqlite("load survey preview", move || {
            repository
                .get_survey(guild_id, survey_id)
                .map_err(|error| error.to_string())
        })
        .await?
        .ok_or_else(|| "Survey not found in this server".to_owned())?;
        let repository = self.state.repository.clone();
        let questions = sqlite("load survey questions", move || {
            repository
                .get_questions(guild_id, survey_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        let pages = policy::preview_pages(
            &app_survey(&survey),
            &questions.iter().map(app_question).collect::<Vec<_>>(),
        );
        self.send_paged_response(owner_id, pages, responder).await
    }

    async fn command_delete(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let repository = self.state.repository.clone();
        let deleted = sqlite("delete survey", move || {
            repository
                .delete_survey(guild_id, survey_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        if !deleted {
            return Err("Survey not found in this server".to_owned());
        }
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!("✅ Deleted survey **#{survey_id}**."))
                .ephemeral()
                .without_mentions(),
        )
        .await
    }

    async fn command_question_add(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let prompt = string_option(options, "prompt")?;
        let question_type = parse_question_type(&string_option(options, "question_type")?)?;
        let required = optional_boolean(options, "required").unwrap_or(true);
        let repository = self.state.repository.clone();
        let question = sqlite("add survey question", move || {
            repository
                .add_question(guild_id, survey_id, &prompt, question_type, required)
                .map_err(|error| error.to_string())
        })
        .await?;
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!(
                "✅ Added question **{}** (ID `{}`).",
                question.position, question.question_id
            ))
            .ephemeral()
            .without_mentions(),
        )
        .await
    }

    async fn command_question_edit(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let question_id = integer_option(options, "question_id")?;
        let prompt = optional_string(options, "prompt").map(str::to_owned);
        let question_type = optional_string(options, "question_type")
            .map(parse_question_type)
            .transpose()?;
        let required = optional_boolean(options, "required");
        if prompt.is_none() && question_type.is_none() && required.is_none() {
            return Err("Provide at least one question change".to_owned());
        }
        let repository = self.state.repository.clone();
        let question = sqlite("edit survey question", move || {
            repository
                .edit_question(
                    guild_id,
                    survey_id,
                    question_id,
                    prompt.as_deref(),
                    question_type,
                    required,
                )
                .map_err(|error| error.to_string())
        })
        .await?;
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!("✅ Updated question **{}**.", question.position))
                .ephemeral()
                .without_mentions(),
        )
        .await
    }

    async fn command_question_remove(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let question_id = integer_option(options, "question_id")?;
        let repository = self.state.repository.clone();
        let removed = sqlite("remove survey question", move || {
            repository
                .remove_question(guild_id, survey_id, question_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        if !removed {
            return Err("Survey question not found".to_owned());
        }
        followup(
            responder.as_ref(),
            InteractionResponse::message("✅ Removed and resequenced the draft question.")
                .ephemeral()
                .without_mentions(),
        )
        .await
    }

    async fn command_question_move(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let question_id = integer_option(options, "question_id")?;
        let position = usize::try_from(integer_option(options, "new_position")?)
            .map_err(|_| "Question position must be positive".to_owned())?;
        let repository = self.state.repository.clone();
        let questions = sqlite("move survey question", move || {
            repository
                .move_question(guild_id, survey_id, question_id, position)
                .map_err(|error| error.to_string())
        })
        .await?;
        let moved = questions
            .iter()
            .find(|question| question.question_id == question_id)
            .ok_or_else(|| "Survey question not found".to_owned())?;
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!(
                "✅ Moved question `{question_id}` to position **{}**.",
                moved.position
            ))
            .ephemeral()
            .without_mentions(),
        )
        .await
    }

    async fn command_send(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let target_type = parse_target(&string_option(options, "target")?)?;
        let member = user_option(options, "member")?;
        let role = optional_role(options, "role");
        let registered_only = optional_boolean(options, "registered_only").unwrap_or(false);
        let guild = self.state.discord.survey_guild_snapshot(guild_id).await?;
        let repository = self.state.repository.clone();
        let registered = sqlite("load registered survey recipients", move || {
            repository
                .registered_discord_ids(guild_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        let recipients = match target_type {
            db::SurveyTargetType::AllRegistered => {
                if member.is_some() || role.is_some() {
                    return Err(
                        "All registered targeting does not accept a member or role".to_owned()
                    );
                }
                policy::resolve_recipients(
                    &guild,
                    policy::RecipientTarget::AllRegistered,
                    &registered,
                    registered_only,
                )
            }
            db::SurveyTargetType::Member => {
                if role.is_some() || member.is_none() {
                    return Err("Choose exactly one member for member targeting".to_owned());
                }
                let target_member = member.expect("checked");
                let member = policy::MemberSnapshot {
                    discord_id: target_member.id,
                    guild_id,
                    bot: target_member.is_bot.unwrap_or(false),
                };
                policy::resolve_recipients(
                    &guild,
                    policy::RecipientTarget::Member(&member),
                    &registered,
                    registered_only,
                )
            }
            db::SurveyTargetType::Role => {
                if member.is_some() || role.is_none() {
                    return Err("Choose exactly one role for role targeting".to_owned());
                }
                let role = self
                    .state
                    .discord
                    .survey_role_snapshot(guild_id, role.expect("checked"))
                    .await?
                    .ok_or_else(|| "That role is not in this server".to_owned())?;
                policy::resolve_recipients(
                    &guild,
                    policy::RecipientTarget::Role(&role),
                    &registered,
                    registered_only,
                )
            }
        }
        .map_err(|error| error.0.to_owned())?;
        let repository = self.state.repository.clone();
        let count = recipients.len();
        sqlite("open survey", move || {
            repository
                .open_survey(
                    guild_id,
                    survey_id,
                    &recipients,
                    target_type,
                    registered_only,
                )
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await?;
        self.state.start_dispatch(Some((guild_id, survey_id)));
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!(
                "✅ Survey **#{survey_id}** opened for **{count}** recipients. Delivering by DM — check `/survey results` for delivery progress."
            ))
            .ephemeral()
            .without_mentions(),
        )
        .await
    }

    async fn command_retry(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let repository = self.state.repository.clone();
        let queued = sqlite("request survey retry", move || {
            repository
                .retry_failed_deliveries(guild_id, survey_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        self.state.recover(Some((guild_id, survey_id))).await?;
        let repository = self.state.repository.clone();
        let results = sqlite("load survey delivery progress", move || {
            repository
                .get_results(guild_id, survey_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!(
                "Retry requested for **{queued}** failed deliveries. Delivered now: **{}/{}**.",
                results.delivered_count, results.recipient_count
            ))
            .ephemeral()
            .without_mentions(),
        )
        .await
    }

    async fn command_results(
        &self,
        guild_id: i64,
        owner_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let repository = self.state.repository.clone();
        let results = sqlite("load survey results", move || {
            repository
                .get_results(guild_id, survey_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        self.send_paged_response(
            owner_id,
            policy::results_pages(&app_results(&results)),
            responder,
        )
        .await
    }

    async fn command_close(
        &self,
        guild_id: i64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let survey_id = integer_option(options, "survey_id")?;
        let _delivery = self.state.delivery_lock.lock().await;
        self.state
            .reconcile_receipts(SurveyRecoveryScope::Survey(guild_id, survey_id))
            .await?;
        let repository = self.state.repository.clone();
        let survey = sqlite("close survey", move || {
            repository
                .close_survey(guild_id, survey_id)
                .map_err(|error| error.to_string())
        })
        .await?;
        self.state
            .reconcile_receipts(SurveyRecoveryScope::Survey(guild_id, survey_id))
            .await?;
        drop(_delivery);
        self.state
            .reconcile_response_messages(SurveyRecoveryScope::Survey(guild_id, survey_id))
            .await?;
        followup(
            responder.as_ref(),
            InteractionResponse::message(format!(
                "✅ Closed survey **#{} — {}**.",
                survey.survey_id,
                policy::escape_discord_text(&survey.title)
            ))
            .ephemeral()
            .without_mentions(),
        )
        .await
    }

    async fn handle_component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            custom_id,
            user_id,
            values,
            ..
        } = request
        else {
            unreachable!();
        };
        if let Some(page) = parse_page_component(&custom_id)? {
            return self.handle_page(page, user_id, responder).await;
        }
        let action = parse_response_component(&custom_id)?;
        let discord_id = signed(user_id, "respondent")?;
        if action.kind == ResponseActionKind::Text {
            return responder
                .show_modal(InteractionModal {
                    custom_id: format!(
                        "survey:{}:question:{}:text-modal",
                        action.survey_id,
                        action.question_id.unwrap_or_default()
                    ),
                    title: "Survey response".to_owned(),
                    inputs: vec![InteractionTextInput {
                        custom_id: TEXT_MODAL_FIELD.to_owned(),
                        label: "Your answer".to_owned(),
                        style: InteractionTextInputStyle::Paragraph,
                        required: true,
                        placeholder: None,
                        min_length: None,
                        max_length: Some(1_000),
                        value: None,
                    }],
                })
                .await
                .map_err(|error| error.to_string());
        }
        let repository = self.state.repository.clone();
        let session = sqlite("load survey interaction session", move || {
            repository
                .get_response_session(action.survey_id, discord_id)
                .map_err(|error| error.to_string())
        })
        .await?
        .ok_or_else(|| "This survey response isn't yours.".to_owned())?;
        if session.recipient.discord_id != discord_id {
            return Err("This survey response isn't yours.".to_owned());
        }
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let response_lock = self.state.response_lock(action.survey_id, discord_id);
        let _response = response_lock.lock().await;
        let next = match action.kind {
            ResponseActionKind::Score => {
                let value = values
                    .first()
                    .ok_or_else(|| "Choose one score".to_owned())?
                    .parse::<i32>()
                    .map_err(|_| "Choose a valid score".to_owned())?;
                let repository = self.state.repository.clone();
                let question_id = action.question_id.unwrap_or_default();
                sqlite("save survey score", move || {
                    repository
                        .save_answer(
                            action.survey_id,
                            discord_id,
                            question_id,
                            db::SurveyAnswer::Numeric(value),
                        )
                        .map_err(|error| error.to_string())
                })
                .await?
            }
            ResponseActionKind::Skip => {
                let repository = self.state.repository.clone();
                let question_id = action.question_id.unwrap_or_default();
                sqlite("skip survey question", move || {
                    repository
                        .skip_question(action.survey_id, discord_id, question_id)
                        .map_err(|error| error.to_string())
                })
                .await?
            }
            ResponseActionKind::ReviewReturn => {
                let repository = self.state.repository.clone();
                sqlite("return to survey review", move || {
                    repository
                        .begin_review(action.survey_id, discord_id)
                        .map_err(|error| error.to_string())
                })
                .await?
            }
            ResponseActionKind::ReviewEdit => {
                let question_id = values
                    .first()
                    .ok_or_else(|| "Choose one question".to_owned())?
                    .parse::<i64>()
                    .map_err(|_| "Choose a valid question".to_owned())?;
                let repository = self.state.repository.clone();
                sqlite("select survey review question", move || {
                    repository
                        .select_response_question(action.survey_id, discord_id, question_id)
                        .map_err(|error| error.to_string())
                })
                .await?
            }
            ResponseActionKind::Submit => {
                let repository = self.state.repository.clone();
                sqlite("submit survey response", move || {
                    repository
                        .submit_response(action.survey_id, discord_id)
                        .map_err(|error| error.to_string())
                })
                .await?;
                let repository = self.state.repository.clone();
                sqlite("reload submitted survey response", move || {
                    repository
                        .get_response_session(action.survey_id, discord_id)
                        .map_err(|error| error.to_string())
                })
                .await?
                .ok_or_else(|| "This survey response no longer exists".to_owned())?
            }
            ResponseActionKind::Text => unreachable!(),
        };
        let response = match interaction_response(&next) {
            Ok(response) => response,
            Err(message) => {
                warn!(error = %message, "survey mutation committed but response render failed");
                self.state.start_dispatch(None);
                return Ok(());
            }
        };
        if let Err(error) = responder.edit_original(response).await {
            warn!(%error, "survey mutation committed but DM edit was deferred");
            self.state.start_dispatch(None);
        } else if let Err(message) = self.state.finalize_if_terminal(&next).await {
            warn!(error = %message, "survey terminal UI was edited but finalization was deferred");
            self.state.start_dispatch(None);
        }
        Ok(())
    }

    async fn handle_modal(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Modal {
            custom_id,
            user_id,
            fields,
            ..
        } = request
        else {
            unreachable!();
        };
        let action = parse_response_component(&custom_id)?;
        if action.kind != ResponseActionKind::Text {
            return Err("unsupported survey modal".to_owned());
        }
        let discord_id = signed(user_id, "respondent")?;
        let answer = fields
            .get(TEXT_MODAL_FIELD)
            .cloned()
            .ok_or_else(|| "Text answer cannot be empty".to_owned())?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let response_lock = self.state.response_lock(action.survey_id, discord_id);
        let _response = response_lock.lock().await;
        let repository = self.state.repository.clone();
        let question_id = action.question_id.unwrap_or_default();
        let session = sqlite("save survey text answer", move || {
            repository
                .save_answer(
                    action.survey_id,
                    discord_id,
                    question_id,
                    db::SurveyAnswer::Text(answer),
                )
                .map_err(|error| error.to_string())
        })
        .await?;
        let response = match interaction_response(&session) {
            Ok(response) => response,
            Err(message) => {
                warn!(error = %message, "survey text answer committed but response render failed");
                self.state.start_dispatch(None);
                return Ok(());
            }
        };
        if let Err(error) = responder.edit_original(response).await {
            warn!(%error, "survey text answer committed but DM edit was deferred");
            self.state.start_dispatch(None);
        }
        Ok(())
    }

    async fn handle_page(
        &self,
        page: PageComponent,
        user_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(pager_handle) = self.state.pager(&page.custom_id) else {
            return responder
                .update(stale_pager_response(&page))
                .await
                .map_err(|error| error.to_string());
        };
        let mut pager = pager_handle.lock().await;
        if pager.retired {
            return responder
                .update(stale_pager_response(&page))
                .await
                .map_err(|error| error.to_string());
        }
        if pager.owner_id != user_id {
            drop(pager);
            return Err("Only the admin who opened this report can page through it.".to_owned());
        }
        let next = match page.direction {
            PageDirection::Previous => pager.index.saturating_sub(1),
            PageDirection::Next => pager
                .index
                .saturating_add(1)
                .min(pager.pages.len().saturating_sub(1)),
        };
        pager.index = next;
        self.state.arm_pager_timeout_locked(&mut pager);
        let response = pager_response(&pager, false)?;
        match responder.update(response).await {
            Ok(()) => {
                pager.edit_responder = responder;
                Ok(())
            }
            Err(error) if error.is_not_found() => {
                pager.retired = true;
                let controls = pager.controls.clone();
                let previous_responder = Arc::clone(&pager.edit_responder);
                let response = pager_response(&pager, true)?;
                pager.timeout_task.take();
                drop(pager);
                self.state.remove_pager_routes(&pager_handle, &controls);
                if let Err(retire_error) = previous_responder.edit_original(response).await {
                    warn!(%retire_error, "survey results view could not retire after dead click token");
                }
                Ok(())
            }
            Err(error) => {
                warn!(%error, "survey results page edit failed transiently");
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponseActionKind {
    Score,
    Text,
    Skip,
    ReviewReturn,
    ReviewEdit,
    Submit,
}

struct ResponseAction {
    survey_id: i64,
    question_id: Option<i64>,
    kind: ResponseActionKind,
}

fn parse_response_component(custom_id: &str) -> Result<ResponseAction, String> {
    let parts = custom_id.split(':').collect::<Vec<_>>();
    if parts.first() != Some(&"survey") {
        return Err("invalid survey component".to_owned());
    }
    let survey_id = parts
        .get(1)
        .ok_or_else(|| "invalid survey component".to_owned())?
        .parse::<i64>()
        .map_err(|_| "invalid survey ID".to_owned())?;
    match parts.as_slice() {
        [_, _, "question", question_id, action] => {
            let question_id = question_id
                .parse::<i64>()
                .map_err(|_| "invalid survey question ID".to_owned())?;
            let kind = match *action {
                "score" => ResponseActionKind::Score,
                "text" | "text-modal" => ResponseActionKind::Text,
                "skip" => ResponseActionKind::Skip,
                _ => return Err("unsupported survey question action".to_owned()),
            };
            Ok(ResponseAction {
                survey_id,
                question_id: Some(question_id),
                kind,
            })
        }
        [_, _, "review", action] => {
            let kind = match *action {
                "return" => ResponseActionKind::ReviewReturn,
                "edit" => ResponseActionKind::ReviewEdit,
                "submit" => ResponseActionKind::Submit,
                _ => return Err("unsupported survey review action".to_owned()),
            };
            Ok(ResponseAction {
                survey_id,
                question_id: None,
                kind,
            })
        }
        _ => Err("invalid survey component".to_owned()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageDirection {
    Previous,
    Next,
}

struct PageComponent {
    custom_id: String,
    direction: PageDirection,
}

fn parse_page_component(custom_id: &str) -> Result<Option<PageComponent>, String> {
    let parts = custom_id.split(':').collect::<Vec<_>>();
    if parts.get(1) != Some(&"page") {
        return Ok(None);
    }
    let [_, _, random_id, direction] = parts.as_slice() else {
        return Err("invalid survey page component".to_owned());
    };
    if random_id.len() != 32 || !random_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid survey page component".to_owned());
    }
    Ok(Some(PageComponent {
        custom_id: custom_id.to_owned(),
        direction: match *direction {
            "previous" => PageDirection::Previous,
            "next" => PageDirection::Next,
            _ => return Err("unsupported survey page direction".to_owned()),
        },
    }))
}

fn random_page_id() -> Result<String, String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| format!("create survey pager ID: {error}"))?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn pager_response(pager: &SurveyPager, retired: bool) -> Result<InteractionResponse, String> {
    let page = pager
        .pages
        .get(pager.index)
        .ok_or_else(|| "survey report has no pages".to_owned())?;
    Ok(
        page_response(page).action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new(&pager.controls.previous_custom_id, "Previous")
                .style(InteractionButtonStyle::Secondary)
                .disabled(retired || pager.index == 0),
            InteractionButton::new(&pager.controls.next_custom_id, "Next")
                .style(InteractionButtonStyle::Secondary)
                .disabled(retired || pager.index + 1 >= pager.pages.len()),
        ])),
    )
}

fn page_response(page: &policy::Embed) -> InteractionResponse {
    InteractionResponse::message("")
        .embed(interaction_embed(page).color(0x58_65_F2))
        .ephemeral()
        .without_mentions()
}

fn stale_pager_response(page: &PageComponent) -> InteractionResponse {
    let random_id = page
        .custom_id
        .split(':')
        .nth(2)
        .expect("validated survey pager custom ID");
    InteractionResponse::message("")
        .ephemeral()
        .without_mentions()
        .action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("survey:page:{random_id}:previous"), "Previous")
                .style(InteractionButtonStyle::Secondary)
                .disabled(true),
            InteractionButton::new(format!("survey:page:{random_id}:next"), "Next")
                .style(InteractionButtonStyle::Secondary)
                .disabled(true),
        ]))
}

fn interaction_response(session: &db::SurveySession) -> Result<InteractionResponse, String> {
    render_message(session).map(|message| message.response)
}

fn render_message(session: &db::SurveySession) -> Result<DiscordMessage, String> {
    let rendered = policy::render_response(&app_session(session))
        .ok_or_else(|| "Survey response has no current question".to_owned())?;
    let (embed, view, color) = match rendered {
        policy::RenderedResponse::Question { embed, view } => (embed, Some(view), 0x58_65_F2),
        policy::RenderedResponse::Review { embed, view } => (embed, Some(view), 0x57_F2_87),
        policy::RenderedResponse::Submitted { embed } => (embed, None, 0x57_F2_87),
        policy::RenderedResponse::Closed { embed } => (embed, None, 0x74_7F_8D),
    };
    let mut response = InteractionResponse::message("")
        .embed(interaction_embed(&embed).color(color))
        .without_mentions();
    if let Some(view) = view {
        response = response.action_rows(response_rows(&view));
    }
    Ok(DiscordMessage {
        response,
        allowed_mentions: DiscordAllowedMentions::None,
        content_mode: crate::discord_transport::DiscordMessageContentMode::Replace,
    })
}

fn response_rows(view: &policy::ResponseView) -> Vec<InteractionActionRow> {
    let mut rows = Vec::new();
    let mut buttons = Vec::new();
    for control in &view.controls {
        match control {
            policy::Control::Score { values, custom_id } => {
                rows.push(InteractionActionRow::string_select(
                    InteractionStringSelect::new(
                        custom_id,
                        if values.len() == 11 {
                            "Choose 0–10"
                        } else {
                            "Choose 1–5"
                        },
                        values
                            .iter()
                            .map(|value| {
                                InteractionStringSelectOption::new(
                                    value.to_string(),
                                    value.to_string(),
                                )
                            })
                            .collect(),
                    ),
                ));
            }
            policy::Control::Edit { options, custom_id } => {
                rows.push(InteractionActionRow::string_select(
                    InteractionStringSelect::new(
                        custom_id,
                        "Edit an answer",
                        options
                            .iter()
                            .enumerate()
                            .map(|(index, option)| {
                                InteractionStringSelectOption::new(
                                    format!("Question {}", index + 1),
                                    option.value.to_string(),
                                )
                                .description(&option.description)
                            })
                            .collect(),
                    ),
                ));
            }
            policy::Control::Text { custom_id } => buttons.push(
                InteractionButton::new(custom_id, "Write answer")
                    .style(InteractionButtonStyle::Primary),
            ),
            policy::Control::Skip { custom_id } => buttons.push(
                InteractionButton::new(custom_id, "Skip").style(InteractionButtonStyle::Secondary),
            ),
            policy::Control::ReturnToReview { custom_id } => buttons.push(
                InteractionButton::new(custom_id, "Back to review")
                    .style(InteractionButtonStyle::Secondary),
            ),
            policy::Control::Submit { custom_id } => buttons.push(
                InteractionButton::new(custom_id, "Submit response")
                    .style(InteractionButtonStyle::Success),
            ),
        }
    }
    if !buttons.is_empty() {
        rows.push(InteractionActionRow::buttons(buttons));
    }
    rows
}

fn interaction_embed(embed: &policy::Embed) -> InteractionEmbed {
    let mut converted = InteractionEmbed::titled(&embed.title).description(&embed.description);
    for field in &embed.fields {
        converted = converted.field(&field.name, &field.value, field.inline);
    }
    if let Some(footer) = &embed.footer {
        converted = converted.footer(footer);
    }
    converted
}

fn app_session(session: &db::SurveySession) -> policy::Session {
    policy::Session {
        survey: app_survey(&session.survey),
        recipient: app_recipient(&session.recipient),
        questions: session.questions.iter().map(app_question).collect(),
        answers: session
            .answers
            .iter()
            .map(|answer| policy::DraftAnswer {
                question_id: answer.question_id,
                numeric_value: answer.numeric_value,
                text_value: answer.text_value.clone(),
                skipped: answer.skipped,
                updated_at: answer.updated_at,
            })
            .collect(),
    }
}

fn app_survey(survey: &db::Survey) -> policy::Survey {
    policy::Survey {
        survey_id: survey.survey_id,
        guild_id: survey.guild_id,
        title: survey.title.clone(),
        status: match survey.status {
            db::SurveyStatus::Draft => policy::SurveyStatus::Draft,
            db::SurveyStatus::Open => policy::SurveyStatus::Open,
            db::SurveyStatus::Closed => policy::SurveyStatus::Closed,
        },
        target_type: survey.target_type.map(|target| match target {
            db::SurveyTargetType::AllRegistered => policy::SurveyTargetType::AllRegistered,
            db::SurveyTargetType::Member => policy::SurveyTargetType::Member,
            db::SurveyTargetType::Role => policy::SurveyTargetType::Role,
        }),
    }
}

fn app_question(question: &db::SurveyQuestion) -> policy::Question {
    policy::Question {
        question_id: question.question_id,
        position: question.position,
        prompt: question.prompt.clone(),
        question_type: match question.question_type {
            db::SurveyQuestionType::Nps => policy::QuestionType::Nps,
            db::SurveyQuestionType::Rating => policy::QuestionType::Rating,
            db::SurveyQuestionType::Text => policy::QuestionType::Text,
        },
        required: question.required,
    }
}

fn app_recipient(recipient: &db::SurveyRecipient) -> policy::Recipient {
    policy::Recipient {
        recipient_id: recipient.recipient_id,
        survey_id: recipient.survey_id,
        guild_id: recipient.guild_id,
        discord_id: recipient.discord_id,
        delivery_status: match recipient.delivery_status {
            db::SurveyDeliveryStatus::Pending => policy::DeliveryStatus::Pending,
            db::SurveyDeliveryStatus::Sending => policy::DeliveryStatus::Sending,
            db::SurveyDeliveryStatus::Sent => policy::DeliveryStatus::Sent,
            db::SurveyDeliveryStatus::Failed => policy::DeliveryStatus::Failed,
        },
        attempt_count: recipient.attempt_count,
        claimed_at: recipient.claimed_at,
        dm_channel_id: recipient.dm_channel_id,
        dm_message_id: recipient.dm_message_id,
        ui_message_id: recipient.ui_message_id,
        current_question_id: recipient.current_question_id,
        in_review: recipient.in_review,
        submitted_at: recipient.submitted_at,
        updated_at: recipient.updated_at,
    }
}

fn app_results(results: &db::SurveyResults) -> policy::SurveyResults {
    policy::SurveyResults {
        survey_id: results.survey.survey_id,
        title: results.survey.title.clone(),
        status: app_survey(&results.survey).status,
        recipient_count: results.recipient_count,
        delivered_count: results.delivered_count,
        started_count: results.started_count,
        completed_count: results.completed_count,
        delivery_rate: results.delivery_rate,
        started_rate: results.started_rate,
        completion_rate: results.completion_rate,
        response_rate: results.response_rate,
        questions: results
            .questions
            .iter()
            .map(|question| policy::QuestionResult {
                question_id: question.question_id,
                position: question.position,
                prompt: question.prompt.clone(),
                question_type: match question.question_type {
                    db::SurveyQuestionType::Nps => policy::QuestionType::Nps,
                    db::SurveyQuestionType::Rating => policy::QuestionType::Rating,
                    db::SurveyQuestionType::Text => policy::QuestionType::Text,
                },
                response_count: question.response_count,
                skipped_count: question.skipped_count,
                nps_score: question.nps_score,
                promoters: question.promoters,
                passives: question.passives,
                detractors: question.detractors,
                rating_average: question.rating_average,
                rating_distribution: question.rating_distribution,
                text_responses: question.text_responses.clone(),
            })
            .collect(),
    }
}

fn survey_subcommands() -> Vec<CommandOptionSpec> {
    vec![
        subcommand(
            "create",
            "Create a one-off survey draft",
            vec![string(
                "title",
                "Survey title shown to respondents",
                true,
                1,
                100,
            )],
        ),
        subcommand(
            "list",
            "List this server's surveys",
            vec![choices(
                "status",
                "Filter by status",
                false,
                &[("Draft", "draft"), ("Open", "open"), ("Closed", "closed")],
            )],
        ),
        subcommand(
            "preview",
            "Preview a survey and its questions",
            vec![integer("survey_id", "Survey number", true, None, None)],
        ),
        subcommand(
            "delete",
            "Delete a survey draft",
            vec![integer("survey_id", "Survey number", true, None, None)],
        ),
        CommandOptionSpec::new(
            "question",
            "Edit draft survey questions",
            CommandOptionKind::SubcommandGroup,
        )
        .options(vec![
            subcommand(
                "add",
                "Add a question to a survey draft",
                vec![
                    integer("survey_id", "Survey number", true, None, None),
                    choices(
                        "question_type",
                        "Answer format",
                        true,
                        &[
                            ("NPS (0–10)", "nps"),
                            ("Rating (1–5)", "rating"),
                            ("Free text", "text"),
                        ],
                    ),
                    string("prompt", "Question shown to respondents", true, 1, 1_000),
                    boolean(
                        "required",
                        "Whether respondents must answer this question",
                        false,
                    ),
                ],
            ),
            subcommand(
                "edit",
                "Edit a draft survey question",
                vec![
                    integer("survey_id", "Survey number", true, None, None),
                    integer(
                        "question_id",
                        "Question ID shown by /survey preview",
                        true,
                        None,
                        None,
                    ),
                    string("prompt", "Replacement prompt", false, 1, 1_000),
                    choices(
                        "question_type",
                        "Replacement answer format",
                        false,
                        &[
                            ("NPS (0–10)", "nps"),
                            ("Rating (1–5)", "rating"),
                            ("Free text", "text"),
                        ],
                    ),
                    boolean("required", "Replacement required/optional setting", false),
                ],
            ),
            subcommand(
                "remove",
                "Remove a draft survey question",
                vec![
                    integer("survey_id", "Survey number", true, None, None),
                    integer(
                        "question_id",
                        "Question ID shown by /survey preview",
                        true,
                        None,
                        None,
                    ),
                ],
            ),
            subcommand(
                "move",
                "Move a draft question to a new position",
                vec![
                    integer("survey_id", "Survey number", true, None, None),
                    integer(
                        "question_id",
                        "Question ID shown by /survey preview",
                        true,
                        None,
                        None,
                    ),
                    integer(
                        "new_position",
                        "New question position",
                        true,
                        Some(1),
                        Some(10),
                    ),
                ],
            ),
        ]),
        subcommand(
            "send",
            "Send a survey immediately by DM",
            vec![
                integer("survey_id", "Survey number", true, None, None),
                choices(
                    "target",
                    "Recipient source",
                    true,
                    &[
                        ("All registered players", "all_registered"),
                        ("One member", "member"),
                        ("A role", "role"),
                    ],
                ),
                scalar(
                    "member",
                    "Required only for member targeting",
                    CommandOptionKind::User,
                    false,
                ),
                scalar(
                    "role",
                    "Required only for role targeting",
                    CommandOptionKind::Role,
                    false,
                ),
                boolean(
                    "registered_only",
                    "For member/role targets, include only registered Cama players",
                    false,
                ),
            ],
        ),
        subcommand(
            "retry",
            "Retry failed DMs for an open survey",
            vec![integer("survey_id", "Survey number", true, None, None)],
        ),
        subcommand(
            "results",
            "View anonymous survey results",
            vec![integer("survey_id", "Survey number", true, None, None)],
        ),
        subcommand(
            "close",
            "Close an open survey",
            vec![integer("survey_id", "Survey number", true, None, None)],
        ),
    ]
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

fn scalar(
    name: &str,
    description: &str,
    kind: CommandOptionKind,
    required: bool,
) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, kind).required(required)
}

fn string(name: &str, description: &str, required: bool, min: u16, max: u16) -> CommandOptionSpec {
    CommandOptionSpec {
        min_length: Some(min),
        max_length: Some(max),
        ..scalar(name, description, CommandOptionKind::String, required)
    }
}

fn integer(
    name: &str,
    description: &str,
    required: bool,
    min: Option<i64>,
    max: Option<i64>,
) -> CommandOptionSpec {
    CommandOptionSpec {
        min_integer: min,
        max_integer: max,
        ..scalar(name, description, CommandOptionKind::Integer, required)
    }
}

fn boolean(name: &str, description: &str, required: bool) -> CommandOptionSpec {
    scalar(name, description, CommandOptionKind::Boolean, required)
}

fn choices(
    name: &str,
    description: &str,
    required: bool,
    values: &[(&str, &str)],
) -> CommandOptionSpec {
    CommandOptionSpec {
        choices: values
            .iter()
            .map(|(name, value)| CommandOptionChoice::String {
                name: (*name).to_owned(),
                value: (*value).to_owned(),
            })
            .collect(),
        ..scalar(name, description, CommandOptionKind::String, required)
    }
}

fn leaf_command(options: &[InteractionOption]) -> Result<(String, &[InteractionOption]), String> {
    let first = options
        .iter()
        .find(|option| {
            matches!(
                option.value,
                InteractionValue::Subcommand(_) | InteractionValue::SubcommandGroup(_)
            )
        })
        .ok_or_else(|| "Choose a survey action".to_owned())?;
    match &first.value {
        InteractionValue::Subcommand(children) => Ok((first.name.clone(), children)),
        InteractionValue::SubcommandGroup(children) => {
            let second = children
                .iter()
                .find(|option| matches!(option.value, InteractionValue::Subcommand(_)))
                .ok_or_else(|| "Choose a survey question action".to_owned())?;
            let InteractionValue::Subcommand(leaf) = &second.value else {
                unreachable!();
            };
            Ok((format!("{} {}", first.name, second.name), leaf))
        }
        _ => unreachable!(),
    }
}

fn string_option(options: &[InteractionOption], name: &str) -> Result<String, String> {
    optional_string(options, name)
        .map(str::to_owned)
        .ok_or_else(|| format!("Missing {name}"))
}

fn integer_option(options: &[InteractionOption], name: &str) -> Result<i64, String> {
    crate::option_ext::integer_option(options, name).ok_or_else(|| format!("Missing {name}"))
}

fn optional_role(options: &[InteractionOption], name: &str) -> Option<i64> {
    match option_value(options, name) {
        Some(InteractionValue::Role(id)) => i64::try_from(*id).ok(),
        _ => None,
    }
}

fn parse_status(value: &str) -> Result<db::SurveyStatus, String> {
    match value {
        "draft" => Ok(db::SurveyStatus::Draft),
        "open" => Ok(db::SurveyStatus::Open),
        "closed" => Ok(db::SurveyStatus::Closed),
        _ => Err("Expected one of: draft, open, closed".to_owned()),
    }
}

fn parse_question_type(value: &str) -> Result<db::SurveyQuestionType, String> {
    match value {
        "nps" => Ok(db::SurveyQuestionType::Nps),
        "rating" => Ok(db::SurveyQuestionType::Rating),
        "text" => Ok(db::SurveyQuestionType::Text),
        _ => Err("Expected one of: nps, rating, text".to_owned()),
    }
}

fn parse_target(value: &str) -> Result<db::SurveyTargetType, String> {
    match value {
        "all_registered" => Ok(db::SurveyTargetType::AllRegistered),
        "member" => Ok(db::SurveyTargetType::Member),
        "role" => Ok(db::SurveyTargetType::Role),
        _ => Err("Expected one of: all_registered, member, role".to_owned()),
    }
}

fn status_label(status: db::SurveyStatus) -> &'static str {
    match status {
        db::SurveyStatus::Draft => "Draft",
        db::SurveyStatus::Open => "Open",
        db::SurveyStatus::Closed => "Closed",
    }
}

fn truncate(value: &str, maximum: usize) -> String {
    let mut chars = value.chars();
    let head = chars.by_ref().take(maximum).collect::<String>();
    if chars.next().is_some() {
        format!(
            "{}…",
            head.chars()
                .take(maximum.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        head
    }
}

async fn respond_error(responder: &dyn InteractionResponder, message: &str) {
    let response = InteractionResponse::message(format!("❌ {message}"))
        .ephemeral()
        .without_mentions();
    let result = if responder.response_is_done() {
        responder.followup(response).await
    } else {
        responder.respond(response).await
    };
    if let Err(error) = result {
        warn!(%error, "survey error response could not be delivered");
    }
}

async fn followup(
    responder: &dyn InteractionResponder,
    response: InteractionResponse,
) -> Result<(), String> {
    responder
        .followup(response)
        .await
        .map_err(|error| error.to_string())
}

fn signed(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("Discord {label} exceeds SQLite INTEGER"))
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "survey_provider/tests.rs"]
mod tests;

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "survey_provider/runtime_tests.rs"]
mod runtime_tests;
