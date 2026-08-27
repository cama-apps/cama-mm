//! Production `/notify` provider: per-user ntfy.sh push notifications for
//! readycheck launches and lobby-confirmed events.
//!
//! Delivery is fire-and-forget from the caller's perspective: both hooks
//! spawn a detached task so a slow or unreachable ntfy server never delays a
//! Discord interaction response. Failures are logged, not surfaced, matching
//! the existing one-shot lobby alert precedent in `registration_provider.rs`.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::ntfy_http::{DEFAULT_NTFY_SERVER, NtfyBuildError, NtfyHttpClient};
use cama_db::push_notifications::{
    PushNotificationConfig, PushNotificationKind, PushNotificationRepository,
};
use tracing::warn;

use crate::registration::{
    CommandSpec, ComponentRoute, InteractionAcknowledgementPolicy, InteractionActionRow,
    InteractionButton, InteractionButtonStyle, InteractionHandler, InteractionHandlerError,
    InteractionModal, InteractionRequest, InteractionResponder, InteractionResponse,
    InteractionTextInput, RegistrationError, RegistrationProvider, RegistryBuilder,
};

const COMMAND_NAME: &str = "notify";
const COMPONENT_PREFIX: &str = "notify_";
const MODAL_CUSTOM_ID: &str = "notify_modal";
const BUTTON_SET: &str = "notify_set";
const BUTTON_TOGGLE_READYCHECK: &str = "notify_toggle_readycheck";
const BUTTON_TOGGLE_LOBBY: &str = "notify_toggle_lobby";
const BUTTON_TEST: &str = "notify_test";
const BUTTON_CLEAR: &str = "notify_clear";
const TOPIC_FIELD: &str = "topic";
const SERVER_FIELD: &str = "server";

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
        Ok(Self {
            handler: Arc::new(PushNotificationHandler {
                repository: PushNotificationRepository::new(database_path),
                client: NtfyHttpClient::new()?,
            }),
        })
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
            description: "Configure ntfy.sh push notifications for readychecks and full lobbies."
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
    client: NtfyHttpClient,
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
            for (discord_id, target) in targets {
                if let Err(error) = handler
                    .client
                    .publish(&target.server, &target.topic, title, message)
                    .await
                {
                    warn!(discord_id, %error, "push notification delivery failed");
                }
            }
        });
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
        let result = self
            .client
            .publish(
                &config.target.server,
                &config.target.topic,
                TEST_TITLE,
                TEST_MESSAGE,
            )
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

    async fn set_target(
        &self,
        discord_id: i64,
        guild_id: i64,
        fields: &std::collections::BTreeMap<String, String>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(topic) = fields
            .get(TOPIC_FIELD)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        else {
            return respond_ephemeral(responder, "\u{274c} A topic is required.").await;
        };
        if topic.contains('/') || topic.contains(char::is_whitespace) {
            return respond_ephemeral(responder, "\u{274c} Topic must not contain spaces or `/`.")
                .await;
        }
        let server = fields
            .get(SERVER_FIELD)
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_NTFY_SERVER.to_owned());
        if !server.starts_with("http://") && !server.starts_with("https://") {
            return respond_ephemeral(
                responder,
                "\u{274c} Server must be a full URL, e.g. `https://ntfy.sh`.",
            )
            .await;
        }

        let repository = self.repository.clone();
        let updated_at = now_seconds();
        {
            let server = server.clone();
            let topic = topic.clone();
            tokio::task::spawn_blocking(move || {
                repository.set_target(discord_id, Some(guild_id), &server, &topic, updated_at)
            })
            .await
            .map_err(|error| format!("push notification set-target task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        }
        let config = self.config(discord_id, guild_id).await?;
        responder
            .respond(status_response(config.as_ref()))
            .await
            .map_err(|error| error.to_string().into())
    }
}

#[async_trait]
impl InteractionHandler for PushNotificationHandler {
    fn acknowledgement_policy(
        &self,
        request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        match request {
            InteractionRequest::Component { custom_id, .. } if custom_id == BUTTON_SET => {
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
                    BUTTON_SET => {
                        let mut topic_input =
                            InteractionTextInput::short(TOPIC_FIELD, "ntfy Topic");
                        topic_input.placeholder = Some("a long random string".to_owned());
                        let mut server_input =
                            InteractionTextInput::short(SERVER_FIELD, "ntfy Server (optional)");
                        server_input.required = false;
                        server_input.placeholder = Some(DEFAULT_NTFY_SERVER.to_owned());
                        responder
                            .show_modal(InteractionModal {
                                custom_id: MODAL_CUSTOM_ID.to_owned(),
                                title: "Set ntfy Target".to_owned(),
                                inputs: vec![topic_input, server_input],
                            })
                            .await
                            .map_err(|error| error.to_string().into())
                    }
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
            InteractionRequest::Modal {
                custom_id,
                user_id,
                guild_id,
                fields,
                ..
            } => {
                if custom_id != MODAL_CUSTOM_ID {
                    return Err(
                        format!("push notification handler received modal {custom_id:?}").into(),
                    );
                }
                let (discord_id, guild_id) = signed_ids(user_id, guild_id)?;
                self.set_target(discord_id, guild_id, &fields, &responder)
                    .await
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

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    message: impl Into<String>,
) -> Result<(), InteractionHandlerError> {
    responder
        .respond(
            InteractionResponse::message(message)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string().into())
}

fn status_response(config: Option<&PushNotificationConfig>) -> InteractionResponse {
    let mut lines = vec![
        "**Push notifications (ntfy.sh)**".to_owned(),
        "Get an alert on your phone/tablet when a readycheck launches with you in it, or a lobby you're in fills up.".to_owned(),
        String::new(),
    ];
    let mut buttons = Vec::new();
    match config {
        Some(config) => {
            lines.push(format!(
                "Target: `{}` on `{}`",
                config.target.topic, config.target.server
            ));
            buttons.push(
                InteractionButton::new(BUTTON_SET, "Change Target")
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
            lines.push("No ntfy target configured yet.".to_owned());
            buttons.push(
                InteractionButton::new(BUTTON_SET, "Set ntfy Target")
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
