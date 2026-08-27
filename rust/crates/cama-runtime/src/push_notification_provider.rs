//! Production `/notify` provider: per-user ntfy.sh push notifications for
//! readycheck launches and lobby-confirmed events.
//!
//! Delivery is fire-and-forget from the caller's perspective: both hooks
//! spawn a detached task so a slow or unreachable ntfy server never delays a
//! Discord interaction response. Failures are logged, not surfaced, matching
//! the existing one-shot lobby alert precedent in `registration_provider.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::ntfy_http::{DEFAULT_NTFY_SERVER, NtfyBuildError, NtfyHttpClient};
use cama_db::push_notifications::{
    PushNotificationConfig, PushNotificationKind, PushNotificationRepository,
};
use tokio::sync::Semaphore;
use tracing::warn;

use crate::registration::{
    CommandSpec, ComponentRoute, InteractionAcknowledgementPolicy, InteractionActionRow,
    InteractionButton, InteractionButtonStyle, InteractionHandler, InteractionHandlerError,
    InteractionRequest, InteractionResponder, InteractionResponse, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

const COMMAND_NAME: &str = "notify";
const COMPONENT_PREFIX: &str = "notify_";
const BUTTON_SET: &str = "notify_set";
const BUTTON_TOGGLE_READYCHECK: &str = "notify_toggle_readycheck";
const BUTTON_TOGGLE_LOBBY: &str = "notify_toggle_lobby";
const BUTTON_TEST: &str = "notify_test";
const BUTTON_CLEAR: &str = "notify_clear";
const TOPIC_PREFIX: &str = "cama-";
const TOPIC_RANDOM_BYTES: usize = 24;
const DELIVERY_CONCURRENCY: usize = 4;
const TEST_DELIVERY_COOLDOWN: Duration = Duration::from_secs(60);

const READYCHECK_TITLE: &str = "\u{2694}\u{fe0f} Readycheck!";
const READYCHECK_MESSAGE: &str = "Your readycheck just launched \u{2014} react before it expires!";
const LOBBY_TITLE: &str = "\u{1f3ae} Lobby Ready!";
const LOBBY_MESSAGE: &str = "Your lobby just filled up \u{2014} time to ready up!";
const TEST_TITLE: &str = "\u{1f9ea} Test Notification";
const TEST_MESSAGE: &str = "Your Cama push notifications are working.";

#[derive(Clone)]
pub struct PushNotificationRegistrationProvider {
    handler: Arc<PushNotificationHandler>,
}

impl PushNotificationRegistrationProvider {
    pub fn new(database_path: impl AsRef<Path>) -> Result<Self, NtfyBuildError> {
        Ok(Self::with_publisher(
            database_path,
            Arc::new(NtfyHttpClient::new()?),
        ))
    }

    fn with_publisher(database_path: impl AsRef<Path>, publisher: Arc<dyn PushPublisher>) -> Self {
        Self {
            handler: Arc::new(PushNotificationHandler {
                repository: PushNotificationRepository::new(database_path),
                publisher,
                delivery_semaphore: Arc::new(Semaphore::new(DELIVERY_CONCURRENCY)),
                test_cooldowns: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_publisher(
        database_path: impl AsRef<Path>,
        publisher: Arc<dyn PushPublisher>,
    ) -> Self {
        Self::with_publisher(database_path, publisher)
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
            description: "Configure best-effort ntfy.sh alerts for readychecks and full lobbies."
                .to_owned(),
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

    /// Notify enabled subscribers among `discord_ids` that their lobby just
    /// reached its ready threshold.
    pub fn notify_lobby_confirmed(&self, guild_id: u64, discord_ids: &BTreeSet<u64>) {
        let targets = discord_ids
            .iter()
            .copied()
            .filter_map(|discord_id| i64::try_from(discord_id).ok())
            .collect::<Vec<_>>();
        PushNotificationHandler::spawn_notify(
            &self.handler,
            guild_id,
            targets,
            PushNotificationKind::Lobby,
            LOBBY_TITLE,
            LOBBY_MESSAGE,
        );
    }
}

struct PushNotificationHandler {
    repository: PushNotificationRepository,
    publisher: Arc<dyn PushPublisher>,
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

    /// Fire-and-forget delivery: looks up enabled targets on a blocking task,
    /// then publishes to each on the async runtime, all inside a detached
    /// task so the caller never waits on SQLite or ntfy network I/O.
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
            let repository = handler.repository.clone();
            let targets = tokio::task::spawn_blocking(move || {
                repository.enabled_targets(Some(guild_id), &discord_ids, kind)
            })
            .await;
            let targets = match targets {
                Ok(Ok(targets)) => targets,
                Ok(Err(error)) => {
                    warn!(%error, "push notification target lookup failed");
                    return;
                }
                Err(error) => {
                    warn!(%error, "push notification target lookup task failed");
                    return;
                }
            };
            let mut targets = targets.into_iter();
            let mut deliveries = tokio::task::JoinSet::new();
            for _ in 0..DELIVERY_CONCURRENCY {
                let Some((discord_id, target)) = targets.next() else {
                    break;
                };
                deliveries.spawn(Self::deliver_target(
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
                        warn!(discord_id, %error, "push notification delivery failed");
                    }
                    Err(error) => warn!(%error, "push notification delivery task failed"),
                }
                if let Some((discord_id, target)) = targets.next() {
                    deliveries.spawn(Self::deliver_target(
                        Arc::clone(&handler.publisher),
                        Arc::clone(&handler.delivery_semaphore),
                        discord_id,
                        target.topic,
                        title,
                        message,
                    ));
                }
            }
        });
    }

    async fn deliver_target(
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

    async fn toggle(
        &self,
        discord_id: i64,
        guild_id: i64,
        kind: PushNotificationKind,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        let updated_at = now_seconds();
        let existing_config = self.config(discord_id, guild_id).await?;
        let Some(existing) = existing_config else {
            return responder
                .update(status_response(None))
                .await
                .map_err(|error| error.to_string().into());
        };
        let enabled = !existing.enabled(kind);
        let changed = tokio::task::spawn_blocking(move || {
            repository.set_enabled(discord_id, Some(guild_id), kind, enabled, updated_at)
        })
        .await
        .map_err(|error| format!("push notification toggle task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if !changed {
            let kind = kind_str(kind);
            warn!(
                discord_id,
                guild_id, kind, "push notification toggle raced with target deletion"
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
        let _permit = self
            .delivery_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "push notification delivery semaphore closed".to_owned())?;
        let result = self
            .publisher
            .publish(&config.target.topic, TEST_TITLE, TEST_MESSAGE)
            .await;
        let mut response = status_response(Some(&config));
        if let Err(error) = result {
            warn!(discord_id, %error, "push notification test delivery failed");
            response.content.push_str(&format!(
                "\n\n\u{26a0}\u{fe0f} Test delivery failed: {error}"
            ));
        } else {
            response
                .content
                .push_str("\n\n\u{2705} Test notification sent.");
        }
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

    async fn clear(
        &self,
        discord_id: i64,
        guild_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || repository.delete_target(discord_id, Some(guild_id)))
            .await
            .map_err(|error| format!("push notification clear task failed: {error}"))?
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
                    BUTTON_TOGGLE_READYCHECK => {
                        self.toggle(
                            discord_id,
                            guild_id,
                            PushNotificationKind::Readycheck,
                            &responder,
                        )
                        .await
                    }
                    BUTTON_TOGGLE_LOBBY => {
                        self.toggle(
                            discord_id,
                            guild_id,
                            PushNotificationKind::Lobby,
                            &responder,
                        )
                        .await
                    }
                    BUTTON_TEST => self.send_test(discord_id, guild_id, &responder).await,
                    BUTTON_CLEAR => self.clear(discord_id, guild_id, &responder).await,
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
        PushNotificationKind::Lobby => "lobby",
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

fn status_response(config: Option<&PushNotificationConfig>) -> InteractionResponse {
    let mut lines = vec![
        "**Push notifications (ntfy.sh)**".to_owned(),
        "Get a best-effort alert when a readycheck launches with you in it, or a lobby you're in fills up.".to_owned(),
        String::new(),
    ];
    let mut buttons = Vec::new();
    match config {
        Some(config) => {
            lines.push(format!("Server: `{DEFAULT_NTFY_SERVER}`"));
            lines.push(format!("Topic: `{}`", config.target.topic));
            lines.push(
                "Keep this topic private; anyone who knows it can receive or publish alerts."
                    .to_owned(),
            );
            buttons.push(
                InteractionButton::new(BUTTON_SET, "Regenerate Topic")
                    .emoji("\u{1f527}")
                    .style(InteractionButtonStyle::Secondary),
            );
            buttons.push(
                InteractionButton::new(
                    BUTTON_TOGGLE_READYCHECK,
                    format!(
                        "Readycheck: {}",
                        if config.readycheck_enabled {
                            "ON"
                        } else {
                            "OFF"
                        }
                    ),
                )
                .emoji(if config.readycheck_enabled {
                    "\u{1f514}"
                } else {
                    "\u{1f515}"
                })
                .style(if config.readycheck_enabled {
                    InteractionButtonStyle::Success
                } else {
                    InteractionButtonStyle::Secondary
                }),
            );
            buttons.push(
                InteractionButton::new(
                    BUTTON_TOGGLE_LOBBY,
                    format!("Lobby: {}", if config.lobby_enabled { "ON" } else { "OFF" }),
                )
                .emoji(if config.lobby_enabled {
                    "\u{1f514}"
                } else {
                    "\u{1f515}"
                })
                .style(if config.lobby_enabled {
                    InteractionButtonStyle::Success
                } else {
                    InteractionButtonStyle::Secondary
                }),
            );
            buttons.push(
                InteractionButton::new(BUTTON_TEST, "Send Test")
                    .emoji("\u{1f9ea}")
                    .style(InteractionButtonStyle::Secondary),
            );
            buttons.push(
                InteractionButton::new(BUTTON_CLEAR, "Clear")
                    .emoji("\u{1f5d1}\u{fe0f}")
                    .style(InteractionButtonStyle::Danger),
            );
        }
        None => {
            lines.push("No ntfy topic configured yet.".to_owned());
            buttons.push(
                InteractionButton::new(BUTTON_SET, "Create ntfy Topic")
                    .emoji("\u{1f527}")
                    .style(InteractionButtonStyle::Primary),
            );
        }
    }
    InteractionResponse::message(lines.join("\n"))
        .ephemeral()
        .action_row(InteractionActionRow::buttons(buttons))
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "push_notification_provider/tests.rs"]
mod tests;
