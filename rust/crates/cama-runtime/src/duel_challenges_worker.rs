//! Production worker for persisted duel reminders and expirations.
//!
//! The worker deliberately owns only runtime concerns: Tokio scheduling,
//! Discord channel discovery and delivery, and moving SQLite work onto the
//! blocking pool. Durable state transitions remain in
//! [`DuelApplicationService`], backed by the repository's `BEGIN IMMEDIATE`
//! operations.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::AIService;
use cama_app::duel::{
    DuelApplicationService, FlavorDetails, FlavorEvent, FlavorPort, MAIN_CHANNEL_NAME,
};
use cama_app::duel_flavor::{
    DuelFlavorChoicePort, DuelFlavorCompletionRequest, DuelFlavorConfigPort, DuelFlavorLlmPort,
    DuelFlavorPortError, DuelFlavorService, fallbacks,
};
use cama_db::duel_repository::{
    DuelChallenge, DuelChallengeRepository, DuelDueKind, DuelDueResult, DuelStatus, DuelTrial,
};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_domain::guild_config::GuildConfigStore;
use chrono::Utc;
use tracing::{debug, error, warn};

use crate::discord_transport::{DiscordAllowedMentions, DiscordMessage};
use crate::registration::{InteractionEmbed, InteractionResponse};
use crate::{BackgroundWorker, BackgroundWorkerSpec, SerenityDiscordTransport, WorkerContext};

pub const DUEL_CHALLENGES_WORKER_NAME: &str = "duel_challenges";
pub const DUEL_CHALLENGES_WAKE_INTERVAL: Duration = Duration::from_secs(60);

#[async_trait]
pub(crate) trait DuelFlavorRuntimePort: Send + Sync {
    async fn generate(&self, event: FlavorEvent, challenge: &DuelChallenge) -> String;
}

#[derive(Clone)]
struct ProductionDuelFlavorLlm(Arc<AIService>);

impl DuelFlavorLlmPort for ProductionDuelFlavorLlm {
    fn complete(
        &mut self,
        request: DuelFlavorCompletionRequest,
    ) -> Result<Option<String>, DuelFlavorPortError> {
        self.0
            .try_complete(
                &request.prompt,
                Some(&request.system_prompt),
                0.7,
                None,
                &request.feature,
            )
            .map(Some)
            .map_err(|error| DuelFlavorPortError::new(error.to_string()))
    }
}

#[derive(Clone)]
struct ProductionDuelFlavorConfig(GuildConfigRepository);

impl DuelFlavorConfigPort for ProductionDuelFlavorConfig {
    fn ai_enabled(&mut self, guild_id: i64) -> Result<bool, DuelFlavorPortError> {
        self.0
            .get_ai_enabled(guild_id)
            .map_err(|error| DuelFlavorPortError::new(error.to_string()))
    }
}

#[derive(Clone)]
struct ProductionDuelFlavorChoice {
    state: Arc<AtomicU64>,
}

impl ProductionDuelFlavorChoice {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_secs() ^ u64::from(duration.subsec_nanos())
            })
            ^ u64::from(std::process::id());
        Self {
            state: Arc::new(AtomicU64::new(seed.max(1))),
        }
    }

    fn advance(state: u64) -> u64 {
        let mut next = state.max(1);
        next ^= next << 13;
        next ^= next >> 7;
        next ^= next << 17;
        next.max(1)
    }
}

impl DuelFlavorChoicePort for ProductionDuelFlavorChoice {
    fn choose_index(&mut self, option_count: usize) -> usize {
        if option_count == 0 {
            return 0;
        }
        let prior = self
            .state
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |state| {
                Some(Self::advance(state))
            })
            .unwrap_or_else(|state| state);
        usize::try_from(Self::advance(prior) % option_count as u64).unwrap_or(0)
    }
}

pub(crate) struct ProductionDuelFlavorRuntime {
    ai_service: Option<Arc<AIService>>,
    config_repository: GuildConfigRepository,
    selector: ProductionDuelFlavorChoice,
}

impl ProductionDuelFlavorRuntime {
    pub(crate) fn new(
        database_path: impl AsRef<Path>,
        ai_service: Option<Arc<AIService>>,
        ai_features_default: bool,
    ) -> Self {
        Self {
            ai_service,
            config_repository: GuildConfigRepository::new(database_path, ai_features_default),
            selector: ProductionDuelFlavorChoice::new(),
        }
    }

    fn fallback(&self, event: FlavorEvent) -> String {
        let mut selector = self.selector.clone();
        let choices = fallbacks(event);
        choices[selector.choose_index(choices.len()) % choices.len()].to_owned()
    }
}

#[async_trait]
impl DuelFlavorRuntimePort for ProductionDuelFlavorRuntime {
    async fn generate(&self, event: FlavorEvent, challenge: &DuelChallenge) -> String {
        let ai_service = self.ai_service.clone().map(ProductionDuelFlavorLlm);
        let config = ProductionDuelFlavorConfig(self.config_repository.clone());
        let selector = self.selector.clone();
        let details = flavor_details(challenge);
        let guild_id = challenge.guild_id;
        match tokio::task::spawn_blocking(move || {
            let mut service = DuelFlavorService::new(ai_service, Some(config), selector);
            FlavorPort::generate(&mut service, event, guild_id, &details)
        })
        .await
        {
            Ok(flavor) => flavor,
            Err(join_error) => {
                warn!(error = %join_error, "duel flavor blocking task failed; using fallback");
                self.fallback(event)
            }
        }
    }
}

trait UnixClock: Send + Sync {
    fn now(&self) -> i64;
}

struct UtcClock;

impl UnixClock for UtcClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DuelChannelKind {
    Text,
    Thread,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DuelChannelSnapshot {
    pub id: i64,
    pub guild_id: Option<i64>,
    pub name: String,
    pub kind: DuelChannelKind,
    pub can_send: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DuelGuildSnapshot {
    pub id: i64,
    pub text_channels: Vec<DuelChannelSnapshot>,
}

/// Narrow async Discord boundary used by the background worker.
///
/// Keeping discovery here lets tests model cache misses and HTTP fetches while
/// the production implementation continues to use the context already held by
/// [`SerenityDiscordTransport`].
#[async_trait]
pub(crate) trait DuelDiscordPort: Send + Sync {
    async fn guild_snapshot(&self, guild_id: i64) -> Result<Option<DuelGuildSnapshot>, String>;

    async fn cached_channel(&self, channel_id: i64) -> Result<Option<DuelChannelSnapshot>, String>;

    async fn fetch_channel(&self, channel_id: i64) -> Result<Option<DuelChannelSnapshot>, String>;

    async fn send_message(&self, channel_id: i64, message: DiscordMessage) -> Result<(), String>;

    async fn edit_message(
        &self,
        channel_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String>;
}

/// Supervised 60-second adapter for Python-compatible duel lifecycle work.
pub struct DuelChallengesWorker {
    repository: DuelChallengeRepository,
    configured_channel_id: Option<i64>,
    discord: Arc<dyn DuelDiscordPort>,
    flavor: Arc<dyn DuelFlavorRuntimePort>,
    clock: Arc<dyn UnixClock>,
    wake_interval: Duration,
}

impl DuelChallengesWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        configured_channel_id: Option<i64>,
        discord: Arc<SerenityDiscordTransport>,
        ai_service: Option<Arc<AIService>>,
        ai_features_default: bool,
    ) -> Self {
        let database_path = database_path.as_ref();
        Self {
            repository: DuelChallengeRepository::new(database_path),
            configured_channel_id,
            discord,
            flavor: Arc::new(ProductionDuelFlavorRuntime::new(
                database_path,
                ai_service,
                ai_features_default,
            )),
            clock: Arc::new(UtcClock),
            wake_interval: DUEL_CHALLENGES_WAKE_INTERVAL,
        }
    }

    #[cfg(test)]
    fn with_runtime(
        database_path: impl AsRef<Path>,
        configured_channel_id: Option<i64>,
        discord: Arc<dyn DuelDiscordPort>,
        flavor: Arc<dyn DuelFlavorRuntimePort>,
        clock: Arc<dyn UnixClock>,
        wake_interval: Duration,
    ) -> Self {
        Self {
            repository: DuelChallengeRepository::new(database_path),
            configured_channel_id,
            discord,
            flavor,
            clock,
            wake_interval,
        }
    }

    async fn due_challenge_ids(&self, now: i64) -> Result<Vec<(i64, i64)>, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            let mut service = DuelApplicationService::new(repository, move || now as f64);
            service.get_due_challenge_ids(now)
        })
        .await
        .map_err(|error| format!("duel due-list blocking task failed: {error}"))?
        .map_err(|error| format!("duel due-list SQLite operation failed: {error}"))
    }

    async fn get_challenge(
        &self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<Option<DuelChallenge>, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            let mut service = DuelApplicationService::new(repository, move || now as f64);
            service.get_challenge(challenge_id, guild_id)
        })
        .await
        .map_err(|error| format!("duel challenge-read blocking task failed: {error}"))?
        .map_err(|error| format!("duel challenge-read SQLite operation failed: {error}"))
    }

    async fn claim_due(
        &self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        claim_reminders: bool,
    ) -> Result<Option<DuelDueResult>, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            let mut service = DuelApplicationService::new(repository, move || now as f64);
            service.process_due(challenge_id, guild_id, now, claim_reminders)
        })
        .await
        .map_err(|error| format!("duel claim blocking task failed: {error}"))?
        .map_err(|error| format!("duel claim SQLite operation failed: {error}"))
    }

    async fn rearm_reminder(&self, result: DuelDueResult) -> Result<bool, String> {
        let repository = self.repository.clone();
        let now = self.clock.now();
        tokio::task::spawn_blocking(move || {
            let mut service = DuelApplicationService::new(repository, move || now as f64);
            service.rearm_reminder(&result)
        })
        .await
        .map_err(|error| format!("duel rearm blocking task failed: {error}"))?
        .map_err(|error| format!("duel rearm SQLite operation failed: {error}"))
    }

    async fn confirm_expiry(&self, result: DuelDueResult) -> Result<bool, String> {
        let repository = self.repository.clone();
        let now = self.clock.now();
        tokio::task::spawn_blocking(move || {
            let mut service = DuelApplicationService::new(repository, move || now as f64);
            service.confirm_expiry_announced(&result)
        })
        .await
        .map_err(|error| format!("duel expiry-confirm blocking task failed: {error}"))?
        .map_err(|error| format!("duel expiry-confirm SQLite operation failed: {error}"))
    }

    fn can_send(channel: &DuelChannelSnapshot, guild_id: i64) -> bool {
        channel.guild_id == Some(guild_id)
            && channel.can_send
            && matches!(
                channel.kind,
                DuelChannelKind::Text | DuelChannelKind::Thread
            )
    }

    async fn resolve_lifecycle_channels(
        &self,
        challenge: &DuelChallenge,
    ) -> Result<Vec<i64>, String> {
        let Some(guild) = self.discord.guild_snapshot(challenge.guild_id).await? else {
            warn!(
                guild_id = challenge.guild_id,
                "duel reminder guild unavailable"
            );
            return Ok(Vec::new());
        };

        let mut main_channel = None;
        if let Some(configured_id) = self.configured_channel_id {
            match self.discord.cached_channel(configured_id).await? {
                Some(channel) if channel.guild_id == Some(challenge.guild_id) => {
                    if Self::can_send(&channel, challenge.guild_id) {
                        main_channel = Some(channel);
                    } else {
                        warn!(
                            guild_id = challenge.guild_id,
                            channel_id = configured_id,
                            "configured duel channel is not sendable"
                        );
                    }
                }
                Some(_) => debug!(
                    guild_id = challenge.guild_id,
                    channel_id = configured_id,
                    "configured duel channel belongs to another guild"
                ),
                None => match self.discord.fetch_channel(configured_id).await {
                    Ok(Some(channel)) if Self::can_send(&channel, challenge.guild_id) => {
                        main_channel = Some(channel);
                    }
                    Ok(_) => debug!(
                        guild_id = challenge.guild_id,
                        channel_id = configured_id,
                        "configured duel channel fetch did not resolve a sendable same-guild target"
                    ),
                    Err(fetch_error) => debug!(
                        guild_id = challenge.guild_id,
                        channel_id = configured_id,
                        error = %fetch_error,
                        "configured duel channel fetch rescue failed"
                    ),
                },
            }
        }

        if main_channel.is_none() {
            let mut matches = guild
                .text_channels
                .into_iter()
                .filter(|channel| {
                    channel.kind == DuelChannelKind::Text
                        && channel.name.contains(MAIN_CHANNEL_NAME)
                })
                .collect::<Vec<_>>();
            let exact = matches
                .iter()
                .filter(|channel| channel.name == MAIN_CHANNEL_NAME)
                .cloned()
                .collect::<Vec<_>>();
            if exact.len() == 1 {
                matches = exact;
            }
            if matches.len() == 1 {
                let channel = matches.remove(0);
                if Self::can_send(&channel, challenge.guild_id) {
                    main_channel = Some(channel);
                } else {
                    warn!(
                        guild_id = challenge.guild_id,
                        channel_id = channel.id,
                        "cannot send duel reminders in #dota-mm"
                    );
                }
            } else {
                debug!(
                    guild_id = challenge.guild_id,
                    matches = matches.len(),
                    "could not resolve a unique #dota-mm text channel"
                );
            }
        }

        let origin_channel = match self.discord.cached_channel(challenge.channel_id).await? {
            Some(channel) => Some(channel),
            None => match self.discord.fetch_channel(challenge.channel_id).await {
                Ok(channel) => channel,
                Err(fetch_error) => {
                    debug!(
                        guild_id = challenge.guild_id,
                        channel_id = challenge.channel_id,
                        error = %fetch_error,
                        "duel origin channel fetch failed"
                    );
                    None
                }
            },
        };

        let mut seen = BTreeSet::new();
        Ok([main_channel, origin_channel]
            .into_iter()
            .flatten()
            .filter(|channel| {
                Self::can_send(channel, challenge.guild_id) && seen.insert(channel.id)
            })
            .map(|channel| channel.id)
            .collect())
    }

    async fn current_status_matches(
        &self,
        result: &DuelDueResult,
        now: i64,
    ) -> Result<bool, String> {
        let expected = match result.kind {
            DuelDueKind::Reminder => DuelStatus::Pending,
            DuelDueKind::Unresolved => DuelStatus::Accepted,
            DuelDueKind::Expired => return Ok(true),
        };
        Ok(self
            .get_challenge(
                result.challenge.challenge_id,
                result.challenge.guild_id,
                now,
            )
            .await?
            .is_some_and(|challenge| challenge.status == expected))
    }

    async fn edit_expired_original(&self, result: &DuelDueResult, flavor: &str) {
        let challenge = &result.challenge;
        let Some(message_id) = challenge.message_id else {
            return;
        };
        let origin = match self.discord.cached_channel(challenge.channel_id).await {
            Ok(Some(channel)) => Some(channel),
            Ok(None) => self
                .discord
                .fetch_channel(challenge.channel_id)
                .await
                .unwrap_or(None),
            Err(_) => None,
        };
        if origin.as_ref().and_then(|channel| channel.guild_id) != Some(challenge.guild_id) {
            return;
        }
        let response = InteractionResponse::message("")
            .without_mentions()
            .embed(expired_challenge_embed(challenge, flavor));
        if let Err(edit_error) = self
            .discord
            .edit_message(
                challenge.channel_id,
                message_id,
                DiscordMessage::silent(response).preserving_content(),
            )
            .await
        {
            error!(
                challenge_id = challenge.challenge_id,
                guild_id = challenge.guild_id,
                channel_id = challenge.channel_id,
                error = %edit_error,
                "unable to update expired duel challenge message"
            );
        }
    }

    async fn deliver_due_result(
        &self,
        result: &DuelDueResult,
        channels: &[i64],
        now: i64,
        context: &mut WorkerContext,
    ) -> Result<DeliveryOutcome, String> {
        if channels.is_empty() {
            warn!(
                challenge_id = result.challenge.challenge_id,
                guild_id = result.challenge.guild_id,
                channel_id = result.challenge.channel_id,
                "duel lifecycle channel unavailable"
            );
            return Ok(DeliveryOutcome::Undelivered);
        }
        if !self.current_status_matches(result, now).await? {
            return Ok(DeliveryOutcome::Undelivered);
        }

        let flavor_event = match result.kind {
            DuelDueKind::Reminder => FlavorEvent::Reminder,
            DuelDueKind::Expired => FlavorEvent::Expired,
            DuelDueKind::Unresolved => FlavorEvent::Unresolved,
        };
        let flavor = self.flavor.generate(flavor_event, &result.challenge).await;
        if result.kind == DuelDueKind::Expired {
            self.edit_expired_original(result, &flavor).await;
        }
        let message = lifecycle_message(result, &flavor);

        for channel_id in channels {
            if !self.current_status_matches(result, now).await? {
                return Ok(DeliveryOutcome::Undelivered);
            }
            let send = self.discord.send_message(*channel_id, message.clone());
            tokio::pin!(send);
            let sent = tokio::select! {
                result = &mut send => Some(result),
                () = context.cancelled() => None,
            };
            let Some(sent) = sent else {
                return Ok(DeliveryOutcome::Cancelled);
            };
            match sent {
                Ok(()) => return Ok(DeliveryOutcome::Delivered),
                Err(send_error) => error!(
                    challenge_id = result.challenge.challenge_id,
                    guild_id = result.challenge.guild_id,
                    channel_id,
                    error = %send_error,
                    "unable to deliver duel lifecycle message"
                ),
            }
        }
        Ok(DeliveryOutcome::Undelivered)
    }

    async fn process_one(
        &self,
        challenge_id: i64,
        guild_id: i64,
        now: i64,
        context: &mut WorkerContext,
    ) -> Result<bool, String> {
        let Some(challenge) = self.get_challenge(challenge_id, guild_id, now).await? else {
            return Ok(false);
        };
        let channels = self.resolve_lifecycle_channels(&challenge).await?;
        let deferred = channels.is_empty();
        if deferred {
            warn!(
                guild_id,
                challenge_id, "no sendable channel for due duel work; deferring reminder claims"
            );
        }
        let Some(result) = self
            .claim_due(challenge_id, guild_id, now, !deferred)
            .await?
        else {
            return Ok(false);
        };

        let outcome = self
            .deliver_due_result(&result, &channels, now, context)
            .await?;
        match result.kind {
            DuelDueKind::Expired if outcome == DeliveryOutcome::Delivered => {
                let confirmed = self.confirm_expiry(result).await?;
                if !confirmed {
                    warn!(
                        challenge_id,
                        guild_id, "duel expiry announcement claim changed before confirmation"
                    );
                }
            }
            DuelDueKind::Expired if result.is_redelivery => warn!(
                challenge_id,
                guild_id, "duel expiry announcement still undelivered; durable retry remains armed"
            ),
            DuelDueKind::Expired => error!(
                challenge_id,
                guild_id,
                challenger_id = result.challenge.challenger_id,
                recipient_id = result.challenge.recipient_id,
                wager = result.challenge.wager,
                penalty = result.challenge.decline_penalty(),
                "duel expiry settled but its announcement was not delivered"
            ),
            _ if outcome != DeliveryOutcome::Delivered => {
                let rearmed = self.rearm_reminder(result).await?;
                if !rearmed {
                    debug!(
                        challenge_id,
                        guild_id, "claimed duel reminder changed before retry rearm"
                    );
                }
            }
            _ => {}
        }
        Ok(outcome == DeliveryOutcome::Cancelled)
    }

    async fn wake_once(&self, context: &mut WorkerContext) -> Result<bool, String> {
        let now = self.clock.now();
        let due = self.due_challenge_ids(now).await?;
        for (challenge_id, guild_id) in due {
            if context.shutdown_requested() {
                return Ok(true);
            }
            if self
                .process_one(challenge_id, guild_id, now, context)
                .await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryOutcome {
    Delivered,
    Undelivered,
    Cancelled,
}

/// Build the production specification retained by [`crate::Runtime`].
#[must_use]
pub fn duel_challenges_worker_spec(
    database_path: impl AsRef<Path>,
    configured_channel_id: Option<i64>,
    discord: Arc<SerenityDiscordTransport>,
    ai_service: Option<Arc<AIService>>,
    ai_features_default: bool,
) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        DUEL_CHALLENGES_WORKER_NAME,
        Arc::new(DuelChallengesWorker::new(
            database_path,
            configured_channel_id,
            discord,
            ai_service,
            ai_features_default,
        )),
    )
}

#[async_trait]
impl BackgroundWorker for DuelChallengesWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        // Discord delivery failures are handled per item above and retain a
        // durable retry marker. Persistence/join failures are different: they
        // return here so Runtime emits BackgroundWorkerFailed and applies its
        // supervised backoff instead of allowing an unhealthy wake to vanish
        // inside the loop.
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            if self.wake_once(&mut context).await? {
                return Ok(());
            }
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

fn lifecycle_message(result: &DuelDueResult, flavor: &str) -> DiscordMessage {
    let challenge = &result.challenge;
    let (content, mentioned_users) = match result.kind {
        DuelDueKind::Expired => (
            format!(
                "{flavor}\nChallenge #{} was declined in cowardice by silence. The {} JC penalty was paid to the challenger.",
                challenge.challenge_id,
                challenge.decline_penalty()
            ),
            BTreeSet::new(),
        ),
        DuelDueKind::Unresolved => {
            let trial = challenge.trial_type.map_or("The trial", trial_label);
            (
                format!(
                    "<@{}> <@{}>\n{flavor}\nChallenge #{} awaits its verdict: {trial}, {} JC a side. Settle it, then have an admin record the outcome with /duel resolve.",
                    challenge.challenger_id,
                    challenge.recipient_id,
                    challenge.challenge_id,
                    challenge.wager
                ),
                [challenge.challenger_id, challenge.recipient_id]
                    .into_iter()
                    .filter_map(|user_id| u64::try_from(user_id).ok())
                    .collect(),
            )
        }
        DuelDueKind::Reminder => {
            let mention = if result.ping_recipient {
                format!("<@{}>\n", challenge.recipient_id)
            } else {
                String::new()
            };
            let users = result
                .ping_recipient
                .then_some(challenge.recipient_id)
                .into_iter()
                .filter_map(|user_id| u64::try_from(user_id).ok())
                .collect();
            (
                format!(
                    "{mention}{flavor}\nChallenge #{} expires <t:{}:R>.",
                    challenge.challenge_id, challenge.expires_at
                ),
                users,
            )
        }
    };
    let response = if mentioned_users.is_empty() {
        InteractionResponse::message(content).without_mentions()
    } else {
        InteractionResponse::message(content)
            .with_user_mentions(mentioned_users.iter().copied().collect())
    };
    if mentioned_users.is_empty() {
        DiscordMessage::silent(response)
    } else {
        DiscordMessage {
            response,
            allowed_mentions: DiscordAllowedMentions::Users(mentioned_users),
            content_mode: crate::discord_transport::DiscordMessageContentMode::Replace,
        }
    }
}

fn flavor_details(challenge: &DuelChallenge) -> FlavorDetails {
    FlavorDetails {
        challenge_id: challenge.challenge_id,
        challenger: "the challenger",
        recipient: "the recipient",
        wager: challenge.wager,
        issuance_fee: challenge.issuance_fee,
        status: challenge.status.as_str(),
        trial: challenge.trial_type.map(DuelTrial::as_str),
    }
}

fn expired_challenge_embed(challenge: &DuelChallenge, flavor: &str) -> InteractionEmbed {
    InteractionEmbed::titled("Challenge of Honor")
        .description(flavor)
        .color(0x60_7D_8B)
        .field("Challenge", format!("#{}", challenge.challenge_id), true)
        .field("Status", "Expired", true)
        .field("Wager", format!("{} JC", challenge.wager), true)
        .field(
            "Issuance Fee",
            format!(
                "{} JC — nonrefundable after delivery",
                challenge.issuance_fee
            ),
            true,
        )
        .field(
            "Challenger",
            format!("<@{}>", challenge.challenger_id),
            true,
        )
        .field(
            "Challenger Rating",
            format!(
                "{:.0} ± {:.0}",
                challenge.challenger_glicko, challenge.challenger_rd
            ),
            true,
        )
        .field("Recipient", format!("<@{}>", challenge.recipient_id), true)
        .field(
            "Recipient Rating",
            format!(
                "{:.0} ± {:.0}",
                challenge.recipient_glicko, challenge.recipient_rd
            ),
            true,
        )
        .field(
            "Decline Penalty",
            format!("{} JC", challenge.decline_penalty()),
            true,
        )
}

fn trial_label(trial: DuelTrial) -> &'static str {
    match trial {
        DuelTrial::TrialByCombat => "Trial by Combat",
        DuelTrial::TrialOfFive => "Trial of Five",
    }
}

#[cfg(test)]
#[path = "duel_challenges_worker/tests.rs"]
mod tests;
