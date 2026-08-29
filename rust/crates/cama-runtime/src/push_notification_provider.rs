//! Production `/notify` provider: per-user ntfy.sh push notifications and
//! Discord direct messages for full-lobby readychecks and shuffled matches.
//!
//! Delivery is fire-and-forget from the caller's perspective: both hooks
//! spawn a detached task so a slow or unreachable ntfy server, or a slow
//! Discord DM send, never delays a Discord interaction response. Failures
//! are logged, not surfaced, matching the existing one-shot lobby alert
//! precedent in `registration_provider.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::ntfy_http::{DEFAULT_NTFY_SERVER, NtfyBuildError, NtfyHttpClient};
use cama_db::push_notifications::{
    PushNotificationChannel, PushNotificationConfig, PushNotificationKind,
    PushNotificationRepository,
};
use tokio::sync::Semaphore;
use tracing::warn;

use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::registration::{
    CommandSpec, ComponentRoute, InteractionAcknowledgementPolicy, InteractionActionRow,
    InteractionButton, InteractionButtonStyle, InteractionHandler, InteractionHandlerError,
    InteractionRequest, InteractionResponder, InteractionResponse, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

const COMMAND_NAME: &str = "notify";
const COMMAND_DESCRIPTION: &str =
    "Configure ntfy.sh and Discord DM alerts for lobby readychecks and matches.";
const COMPONENT_PREFIX: &str = "notify_";
const BUTTON_SET: &str = "notify_set";
const BUTTON_TOGGLE_READYCHECK_NTFY: &str = "notify_toggle_readycheck_ntfy";
const BUTTON_TOGGLE_MATCH_STARTED_NTFY: &str = "notify_toggle_match_started_ntfy";
const BUTTON_TOGGLE_READYCHECK_DM: &str = "notify_toggle_readycheck_dm";
const BUTTON_TOGGLE_MATCH_STARTED_DM: &str = "notify_toggle_match_started_dm";
const BUTTON_TEST: &str = "notify_test";
const BUTTON_UNSUBSCRIBE: &str = "notify_unsubscribe";
const TOPIC_PREFIX: &str = "cama-";
const TOPIC_RANDOM_BYTES: usize = 24;
const DELIVERY_CONCURRENCY: usize = 4;
const TEST_DELIVERY_COOLDOWN: Duration = Duration::from_secs(60);

const READYCHECK_TITLE: &str = "\u{2694}\u{fe0f} Readycheck!";
const READYCHECK_MESSAGE: &str =
    "Your lobby is full and a readycheck just launched \u{2014} react before it expires!";
const MATCH_STARTED_TITLE: &str = "\u{1f3ae} Match Started!";
const MATCH_STARTED_MESSAGE: &str = "Your shuffled match has started \u{2014} good luck!";
const TEST_TITLE: &str = "\u{1f9ea} Test Notification";
const TEST_MESSAGE: &str = "Your Cama push notifications are working.";

#[derive(Clone)]
pub struct PushNotificationRegistrationProvider {
    handler: Arc<PushNotificationHandler>,
}

impl PushNotificationRegistrationProvider {
    pub fn new(
        database_path: impl AsRef<Path>,
        discord: Arc<dyn DiscordTransport>,
    ) -> Result<Self, NtfyBuildError> {
        Ok(Self::with_publisher(
            database_path,
            discord,
            Arc::new(NtfyHttpClient::new()?),
        ))
    }

    fn with_publisher(
        database_path: impl AsRef<Path>,
        discord: Arc<dyn DiscordTransport>,
        publisher: Arc<dyn PushPublisher>,
    ) -> Self {
        Self {
            handler: Arc::new(PushNotificationHandler {
                repository: PushNotificationRepository::new(database_path),
                publisher,
                discord,
                delivery_semaphore: Arc::new(Semaphore::new(DELIVERY_CONCURRENCY)),
                test_cooldowns: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_publisher(
        database_path: impl AsRef<Path>,
        discord: Arc<dyn DiscordTransport>,
        publisher: Arc<dyn PushPublisher>,
    ) -> Self {
        Self::with_publisher(database_path, discord, publisher)
    }

    /// Cheap, cloneable hooks for other providers to fire notifications from.
    #[must_use]
    pub fn hooks(&self) -> PushNotificationHooks {
        PushNotificationHooks {
            handler: Arc::clone(&self.handler),
        }
    }
}

impl RegistrationProvider for PushNotificationRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: COMMAND_NAME.to_owned(),
            description: COMMAND_DESCRIPTION.to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

#[derive(Clone)]
pub struct PushNotificationHooks {
    handler: Arc<PushNotificationHandler>,
}

impl PushNotificationHooks {
    /// Notify enabled subscribers among `discord_ids` that a readycheck just
    /// launched with them in it. Callers are expected to have already
    /// excluded anyone who does not need to react (an auto-confirmed
    /// invoker, a recently-ready player) — the same set Discord mentions in
    /// the readycheck message itself.
    pub fn notify_readycheck_launched(
        &self,
        guild_id: u64,
        discord_ids: impl IntoIterator<Item = u64>,
    ) {
        let targets = discord_ids
            .into_iter()
            .filter_map(|discord_id| i64::try_from(discord_id).ok())
            .collect::<Vec<_>>();
        PushNotificationHandler::spawn_notify(
            &self.handler,
            guild_id,
            targets,
            PushNotificationKind::Readycheck,
            READYCHECK_TITLE,
            READYCHECK_MESSAGE,
        );
    }

    /// Notify enabled subscribers among `discord_ids` that a shuffled match
    /// just started with them in it.
    pub fn notify_match_started(&self, guild_id: u64, discord_ids: &BTreeSet<u64>) {
        let targets = discord_ids
            .iter()
            .copied()
            .filter_map(|discord_id| i64::try_from(discord_id).ok())
            .collect::<Vec<_>>();
        PushNotificationHandler::spawn_notify(
            &self.handler,
            guild_id,
            targets,
            PushNotificationKind::MatchStarted,
            MATCH_STARTED_TITLE,
            MATCH_STARTED_MESSAGE,
        );
    }
}

struct PushNotificationHandler {
    repository: PushNotificationRepository,
    publisher: Arc<dyn PushPublisher>,
    discord: Arc<dyn DiscordTransport>,
    delivery_semaphore: Arc<Semaphore>,
    test_cooldowns: Mutex<BTreeMap<(i64, i64), Instant>>,
}

#[async_trait]
pub(crate) trait PushPublisher: Send + Sync {
    async fn publish(&self, topic: &str, title: &str, message: &str) -> Result<(), String>;
}

#[async_trait]
impl PushPublisher for NtfyHttpClient {
    async fn publish(&self, topic: &str, title: &str, message: &str) -> Result<(), String> {
        NtfyHttpClient::publish(self, topic, title, message)
            .await
            .map_err(|error| error.to_string())
    }
}

fn dm_message(title: &str, message: &str) -> DiscordMessage {
    DiscordMessage::silent(InteractionResponse::message(format!(
        "**{title}**\n{message}"
    )))
}

impl PushNotificationHandler {
    async fn config(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<PushNotificationConfig>, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || repository.get_config(discord_id, Some(guild_id)))
            .await
            .map_err(|error| format!("push notification config task failed: {error}"))?
            .map_err(|error| error.to_string())
    }

    /// Fire-and-forget delivery: looks up enabled targets per channel on a
    /// blocking task, then fans out on the async runtime, all inside a
    /// detached task so the caller never waits on SQLite, ntfy, or Discord
    /// I/O.
    fn spawn_notify(
        handler: &Arc<Self>,
        guild_id: u64,
        discord_ids: Vec<i64>,
        kind: PushNotificationKind,
        title: &'static str,
        message: &'static str,
    ) {
        if discord_ids.is_empty() {
            return;
        }
        let Ok(guild_id) = i64::try_from(guild_id) else {
            warn!(
                guild_id,
                "push notification guild snowflake exceeds SQLite INTEGER range"
            );
            return;
        };
        let handler = Arc::clone(handler);
        tokio::spawn(async move {
            Self::deliver_ntfy(&handler, guild_id, &discord_ids, kind, title, message).await;
            Self::deliver_dm(&handler, guild_id, &discord_ids, kind, title, message).await;
        });
    }

    async fn deliver_ntfy(
        handler: &Arc<Self>,
        guild_id: i64,
        discord_ids: &[i64],
        kind: PushNotificationKind,
        title: &'static str,
        message: &'static str,
    ) {
        let repository = handler.repository.clone();
        let discord_ids = discord_ids.to_vec();
        let targets = tokio::task::spawn_blocking(move || {
            repository.enabled_ntfy_targets(Some(guild_id), &discord_ids, kind)
        })
        .await;
        let targets = match targets {
            Ok(Ok(targets)) => targets,
            Ok(Err(error)) => {
                warn!(%error, "push notification ntfy target lookup failed");
                return;
            }
            Err(error) => {
                warn!(%error, "push notification ntfy target lookup task failed");
                return;
            }
        };
        let mut targets = targets.into_iter();
        let mut deliveries = tokio::task::JoinSet::new();
        for _ in 0..DELIVERY_CONCURRENCY {
            let Some((discord_id, target)) = targets.next() else {
                break;
            };
            deliveries.spawn(Self::deliver_ntfy_target(
                Arc::clone(&handler.publisher),
                Arc::clone(&handler.delivery_semaphore),
                discord_id,
                target.topic,
                title,
                message,
            ));
        }
        while let Some(delivery) = deliveries.join_next().await {
            match delivery {
                Ok((_, Ok(()))) => {}
                Ok((discord_id, Err(error))) => {
                    warn!(discord_id, %error, "push notification ntfy delivery failed");
                }
                Err(error) => warn!(%error, "push notification ntfy delivery task failed"),
            }
            if let Some((discord_id, target)) = targets.next() {
                deliveries.spawn(Self::deliver_ntfy_target(
                    Arc::clone(&handler.publisher),
                    Arc::clone(&handler.delivery_semaphore),
                    discord_id,
                    target.topic,
                    title,
                    message,
                ));
            }
        }
    }

    async fn deliver_ntfy_target(
        publisher: Arc<dyn PushPublisher>,
        semaphore: Arc<Semaphore>,
        discord_id: i64,
        topic: String,
        title: &'static str,
        message: &'static str,
    ) -> (i64, Result<(), String>) {
        let result = match semaphore.acquire_owned().await {
            Ok(_permit) => publisher.publish(&topic, title, message).await,
            Err(_) => Err("push notification delivery semaphore closed".to_owned()),
        };
        (discord_id, result)
    }

    async fn deliver_dm(
        handler: &Arc<Self>,
        guild_id: i64,
        discord_ids: &[i64],
        kind: PushNotificationKind,
        title: &'static str,
        message: &'static str,
    ) {
        let repository = handler.repository.clone();
        let discord_ids = discord_ids.to_vec();
        let ids = tokio::task::spawn_blocking(move || {
            repository.enabled_dm_ids(Some(guild_id), &discord_ids, kind)
        })
        .await;
        let ids = match ids {
            Ok(Ok(ids)) => ids,
            Ok(Err(error)) => {
                warn!(%error, "push notification DM target lookup failed");
                return;
            }
            Err(error) => {
                warn!(%error, "push notification DM target lookup task failed");
                return;
            }
        };
        let mut ids = ids.into_iter();
        let mut deliveries = tokio::task::JoinSet::new();
        for _ in 0..DELIVERY_CONCURRENCY {
            let Some(discord_id) = ids.next() else {
                break;
            };
            deliveries.spawn(Self::deliver_dm_target(
                Arc::clone(&handler.discord),
                Arc::clone(&handler.delivery_semaphore),
                discord_id,
                title,
                message,
            ));
        }
        while let Some(delivery) = deliveries.join_next().await {
            match delivery {
                Ok((_, Ok(()))) => {}
                Ok((discord_id, Err(error))) => {
                    warn!(discord_id, %error, "push notification DM delivery failed");
                }
                Err(error) => warn!(%error, "push notification DM delivery task failed"),
            }
            if let Some(discord_id) = ids.next() {
                deliveries.spawn(Self::deliver_dm_target(
                    Arc::clone(&handler.discord),
                    Arc::clone(&handler.delivery_semaphore),
                    discord_id,
                    title,
                    message,
                ));
            }
        }
    }

    async fn deliver_dm_target(
        discord: Arc<dyn DiscordTransport>,
        semaphore: Arc<Semaphore>,
        discord_id: i64,
        title: &'static str,
        message: &'static str,
    ) -> (i64, Result<(), String>) {
        let result = match semaphore.acquire_owned().await {
            Ok(_permit) => match u64::try_from(discord_id) {
                Ok(user_id) => {
                    discord
                        .send_direct_message(user_id, dm_message(title, message))
                        .await
                }
                Err(_) => Err("Discord ID exceeds Discord snowflake range".to_owned()),
            },
            Err(_) => Err("push notification delivery semaphore closed".to_owned()),
        };
        (discord_id, result)
    }

    async fn toggle(
        &self,
        discord_id: i64,
        guild_id: i64,
        kind: PushNotificationKind,
        channel: PushNotificationChannel,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let updated_at = now_seconds();
        let existing_config = self.config(discord_id, guild_id).await?;
        let enabled = !existing_config
            .as_ref()
            .is_some_and(|config| config.enabled(kind, channel));
        let changed = tokio::task::spawn_blocking(move || {
            repository.set_enabled(
                discord_id,
                Some(guild_id),
                kind,
                channel,
                enabled,
                updated_at,
            )
        })
        .await
        .map_err(|error| format!("push notification toggle task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if !changed {
            let kind = kind_str(kind);
            warn!(
                discord_id,
                guild_id, kind, "push notification ntfy toggle requires an existing target"
            );
        }
        let config = self.config(discord_id, guild_id).await?;
        responder
            .update(status_response(config.as_ref()))
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn send_test(
        &self,
        discord_id: i64,
        guild_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let config = self.config(discord_id, guild_id).await?;
        let Some(config) = config else {
            return responder
                .update(status_response(None))
                .await
                .map_err(|error| error.to_string().into());
        };
        let dm_active = config.dm_readycheck_enabled || config.dm_match_started_enabled;
        if config.target.is_none() && !dm_active {
            let mut response = status_response(Some(&config));
            response
                .content
                .push_str("\n\n\u{26a0}\u{fe0f} Nothing is enabled to test yet.");
            return responder
                .update(response)
                .await
                .map_err(|error| error.to_string().into());
        }
        if let Some(retry_after) = self.claim_test_delivery(discord_id, guild_id)? {
            let mut response = status_response(Some(&config));
            response.content.push_str(&format!(
                "\n\n⏳ Test delivery is rate-limited. Try again in {retry_after}s."
            ));
            return responder
                .update(response)
                .await
                .map_err(|error| error.to_string().into());
        }
        let mut notes = Vec::new();
        if let Some(target) = &config.target {
            let _permit = self
                .delivery_semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "push notification delivery semaphore closed".to_owned())?;
            match self
                .publisher
                .publish(&target.topic, TEST_TITLE, TEST_MESSAGE)
                .await
            {
                Ok(()) => notes.push("\u{2705} ntfy test notification sent.".to_owned()),
                Err(error) => {
                    warn!(discord_id, %error, "push notification ntfy test delivery failed");
                    notes.push(format!(
                        "\u{26a0}\u{fe0f} ntfy test notification failed: {error}"
                    ));
                }
            }
        }
        if dm_active {
            let _permit = self
                .delivery_semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "push notification delivery semaphore closed".to_owned())?;
            let result = match u64::try_from(discord_id) {
                Ok(user_id) => {
                    self.discord
                        .send_direct_message(user_id, dm_message(TEST_TITLE, TEST_MESSAGE))
                        .await
                }
                Err(_) => Err("Discord ID exceeds Discord snowflake range".to_owned()),
            };
            match result {
                Ok(()) => notes.push("\u{2705} DM test notification sent.".to_owned()),
                Err(error) => {
                    warn!(discord_id, %error, "push notification DM test delivery failed");
                    notes.push(format!(
                        "\u{26a0}\u{fe0f} DM test notification failed: {error}"
                    ));
                }
            }
        }
        let mut response = status_response(Some(&config));
        response
            .content
            .push_str(&format!("\n\n{}", notes.join("\n")));
        responder
            .update(response)
            .await
            .map_err(|error| error.to_string().into())
    }

    fn claim_test_delivery(&self, discord_id: i64, guild_id: i64) -> Result<Option<u64>, String> {
        let now = Instant::now();
        let mut cooldowns = self
            .test_cooldowns
            .lock()
            .map_err(|_| "push notification test cooldown lock was poisoned".to_owned())?;
        if cooldowns.len() >= 1_024 {
            cooldowns.retain(|_, claimed| now.duration_since(*claimed) < TEST_DELIVERY_COOLDOWN);
        }
        let key = (guild_id, discord_id);
        if let Some(claimed) = cooldowns.get(&key) {
            let elapsed = now.duration_since(*claimed);
            if elapsed < TEST_DELIVERY_COOLDOWN {
                return Ok(Some(
                    TEST_DELIVERY_COOLDOWN
                        .saturating_sub(elapsed)
                        .as_secs()
                        .max(1),
                ));
            }
        }
        cooldowns.insert(key, now);
        Ok(None)
    }

    async fn unsubscribe(
        &self,
        discord_id: i64,
        guild_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || repository.delete_target(discord_id, Some(guild_id)))
            .await
            .map_err(|error| format!("push notification unsubscribe task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        responder
            .update(status_response(None))
            .await
            .map_err(|error| error.to_string().into())
    }

    async fn create_target(
        &self,
        discord_id: i64,
        guild_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let topic = generate_topic()?;
        let repository = self.repository.clone();
        let updated_at = now_seconds();
        {
            let topic = topic.clone();
            tokio::task::spawn_blocking(move || {
                repository.set_target(discord_id, Some(guild_id), &topic, updated_at)
            })
            .await
            .map_err(|error| format!("push notification set-target task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        }
        let config = self.config(discord_id, guild_id).await?;
        responder
            .update(status_response(config.as_ref()))
            .await
            .map_err(|error| error.to_string().into())
    }
}

#[async_trait]
impl InteractionHandler for PushNotificationHandler {
    fn acknowledgement_policy(
        &self,
        _request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        InteractionAcknowledgementPolicy::Automatic
    }

    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                name,
                user_id,
                guild_id,
                ..
            } => {
                if name != COMMAND_NAME {
                    return Err(
                        format!("push notification handler received command {name:?}").into(),
                    );
                }
                let (discord_id, guild_id) = signed_ids(user_id, guild_id)?;
                responder
                    .defer(true)
                    .await
                    .map_err(|error| error.to_string())?;
                let config = self.config(discord_id, guild_id).await?;
                responder
                    .followup(status_response(config.as_ref()))
                    .await
                    .map_err(|error| error.to_string().into())
            }
            InteractionRequest::Component {
                custom_id,
                user_id,
                guild_id,
                ..
            } => {
                let (discord_id, guild_id) = signed_ids(user_id, guild_id)?;
                match custom_id.as_str() {
                    BUTTON_SET => self.create_target(discord_id, guild_id, &responder).await,
                    BUTTON_TOGGLE_READYCHECK_NTFY => {
                        self.toggle(
                            discord_id,
                            guild_id,
                            PushNotificationKind::Readycheck,
                            PushNotificationChannel::Ntfy,
                            &responder,
                        )
                        .await
                    }
                    BUTTON_TOGGLE_MATCH_STARTED_NTFY => {
                        self.toggle(
                            discord_id,
                            guild_id,
                            PushNotificationKind::MatchStarted,
                            PushNotificationChannel::Ntfy,
                            &responder,
                        )
                        .await
                    }
                    BUTTON_TOGGLE_READYCHECK_DM => {
                        self.toggle(
                            discord_id,
                            guild_id,
                            PushNotificationKind::Readycheck,
                            PushNotificationChannel::DirectMessage,
                            &responder,
                        )
                        .await
                    }
                    BUTTON_TOGGLE_MATCH_STARTED_DM => {
                        self.toggle(
                            discord_id,
                            guild_id,
                            PushNotificationKind::MatchStarted,
                            PushNotificationChannel::DirectMessage,
                            &responder,
                        )
                        .await
                    }
                    BUTTON_TEST => self.send_test(discord_id, guild_id, &responder).await,
                    BUTTON_UNSUBSCRIBE => self.unsubscribe(discord_id, guild_id, &responder).await,
                    other => Err(format!("unknown push notification component {other:?}").into()),
                }
            }
            InteractionRequest::Modal { .. } => {
                Err("push notification handler does not accept modal interactions".into())
            }
            InteractionRequest::Autocomplete { .. } => {
                Err("push notification handler received an autocomplete interaction".into())
            }
        }
    }
}

fn signed_ids(user_id: u64, guild_id: Option<u64>) -> Result<(i64, i64), String> {
    let user_id = i64::try_from(user_id)
        .map_err(|_| format!("Discord user snowflake {user_id} exceeds SQLite INTEGER range"))?;
    let guild_id = guild_id
        .map(i64::try_from)
        .transpose()
        .map_err(|_| "Discord guild snowflake exceeds SQLite INTEGER range".to_owned())?
        .unwrap_or_default();
    Ok((user_id, guild_id))
}

const fn kind_str(kind: PushNotificationKind) -> &'static str {
    match kind {
        PushNotificationKind::Readycheck => "readycheck",
        PushNotificationKind::MatchStarted => "match_started",
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default() as i64
}

fn generate_topic() -> Result<String, String> {
    let mut random = [0_u8; TOPIC_RANDOM_BYTES];
    getrandom::fill(&mut random)
        .map_err(|error| format!("could not generate a secure ntfy topic: {error}"))?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("{TOPIC_PREFIX}{suffix}"))
}

fn on_off(enabled: bool) -> &'static str {
    if enabled { "ON" } else { "OFF" }
}

fn bell_emoji(enabled: bool) -> &'static str {
    if enabled { "\u{1f514}" } else { "\u{1f515}" }
}

fn toggle_style(enabled: bool) -> InteractionButtonStyle {
    if enabled {
        InteractionButtonStyle::Success
    } else {
        InteractionButtonStyle::Secondary
    }
}

fn toggle_button(custom_id: &str, label: &str, enabled: bool) -> InteractionButton {
    InteractionButton::new(custom_id, format!("{label}: {}", on_off(enabled)))
        .emoji(bell_emoji(enabled))
        .style(toggle_style(enabled))
}

fn status_response(config: Option<&PushNotificationConfig>) -> InteractionResponse {
    let has_target = config.is_some_and(|config| config.target.is_some());
    let readycheck_ntfy = config.is_some_and(|config| config.readycheck_enabled);
    let match_started_ntfy = config.is_some_and(|config| config.match_started_enabled);
    let readycheck_dm = config.is_some_and(|config| config.dm_readycheck_enabled);
    let match_started_dm = config.is_some_and(|config| config.dm_match_started_enabled);
    let dm_active = readycheck_dm || match_started_dm;

    let mut lines = vec![
        "**Push notifications**".to_owned(),
        "Get a best-effort alert \u{2014} via ntfy.sh, a Discord DM, or both \u{2014} when your lobby is full and a readycheck launches needing your response, or a shuffled match starts with you in it.".to_owned(),
        String::new(),
    ];
    if let Some(target) = config.and_then(|config| config.target.as_ref()) {
        lines.push(format!("ntfy server: `{DEFAULT_NTFY_SERVER}`"));
        lines.push(format!("ntfy topic: `{}`", target.topic));
        lines.push(
            "Keep this topic private; anyone who knows it can receive or publish alerts."
                .to_owned(),
        );
    } else {
        lines.push(
            "No ntfy topic configured yet \u{2014} the DM toggles below don't need one.".to_owned(),
        );
    }

    let mut rows = Vec::new();

    rows.push(InteractionActionRow::buttons(vec![
        InteractionButton::new(
            BUTTON_SET,
            if has_target {
                "Regenerate ntfy Topic"
            } else {
                "Create ntfy Topic"
            },
        )
        .emoji("\u{1f527}")
        .style(if has_target {
            InteractionButtonStyle::Secondary
        } else {
            InteractionButtonStyle::Primary
        }),
    ]));

    if has_target {
        rows.push(InteractionActionRow::buttons(vec![
            toggle_button(
                BUTTON_TOGGLE_READYCHECK_NTFY,
                "Readycheck (ntfy)",
                readycheck_ntfy,
            ),
            toggle_button(
                BUTTON_TOGGLE_MATCH_STARTED_NTFY,
                "Match Started (ntfy)",
                match_started_ntfy,
            ),
        ]));
    }

    rows.push(InteractionActionRow::buttons(vec![
        toggle_button(
            BUTTON_TOGGLE_READYCHECK_DM,
            "Readycheck (DM)",
            readycheck_dm,
        ),
        toggle_button(
            BUTTON_TOGGLE_MATCH_STARTED_DM,
            "Match Started (DM)",
            match_started_dm,
        ),
    ]));

    let mut actions = Vec::new();
    if has_target || dm_active {
        actions.push(
            InteractionButton::new(BUTTON_TEST, "Send Test")
                .emoji("\u{1f9ea}")
                .style(InteractionButtonStyle::Secondary),
        );
    }
    if config.is_some() {
        actions.push(
            InteractionButton::new(BUTTON_UNSUBSCRIBE, "Unsubscribe")
                .emoji("\u{1f5d1}\u{fe0f}")
                .style(InteractionButtonStyle::Danger),
        );
    }
    if !actions.is_empty() {
        rows.push(InteractionActionRow::buttons(actions));
    }

    InteractionResponse::message(lines.join("\n"))
        .ephemeral()
        .action_rows(rows)
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "push_notification_provider/tests.rs"]
mod tests;
