//! Production `/setreminder` provider and startup reminder recovery.
//!
//! Preferences and authoritative cooldown anchors live in the existing SQLite
//! schema. All such calls are offloaded with `spawn_blocking`; Tokio owns only
//! generation-checked sleeps and Discord delivery. A scheduled DM is consumed
//! after exactly one attempt, including a Discord failure, matching Python.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cama_app::reminder_sqlite::SqliteReminderStore;
use cama_app::reminders::{
    GuildId, LobbyKind, ReminderCooldowns, ReminderDelivery, ReminderKind, ReminderPreferences,
    ReminderService, ReminderTaskKey, RuntimeDiscordPort, ScheduledReminder, SystemReminderClock,
    TaskId, UserId,
};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::registration::{
    CommandSpec, ComponentRoute, InteractionActionRow, InteractionButton, InteractionButtonStyle,
    InteractionHandler, InteractionHandlerError, InteractionRequest, InteractionResponder,
    InteractionResponse, RegistrationError, RegistrationProvider, RegistryBuilder,
};

const OBSERVER_NAME: &str = "stored-reminders";
const COMPONENT_PREFIX: &str = "reminder_";

type LiveReminderService =
    ReminderService<SqliteReminderStore, SystemReminderClock, RuntimeDiscordPort>;

#[derive(Clone, Copy)]
struct ReminderLabel {
    kind: ReminderKind,
    label: &'static str,
    description: &'static str,
}

const DISPLAYED_REMINDERS: [ReminderLabel; 5] = [
    ReminderLabel {
        kind: ReminderKind::Wheel,
        label: "Gamba/Wheel",
        description: "DM when your wheel cooldown expires",
    },
    ReminderLabel {
        kind: ReminderKind::Trivia,
        label: "Trivia",
        description: "DM when your trivia cooldown expires",
    },
    ReminderLabel {
        kind: ReminderKind::Betting,
        label: "Betting",
        description: "DM when a new match opens for betting",
    },
    ReminderLabel {
        kind: ReminderKind::Dig,
        label: "Dig",
        description: "DM when your free dig cooldown expires",
    },
    ReminderLabel {
        kind: ReminderKind::Pet,
        label: "Pet",
        description: "DM when your cama gets dangerously hungry (and when it dies)",
    },
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReminderDeliveryReport {
    pub attempted: usize,
    pub delivered: usize,
    pub failures: Vec<(i64, String)>,
}

#[derive(Clone)]
pub struct ReminderRegistrationProvider {
    handler: Arc<ReminderHandler>,
    observer: Arc<ReminderRecoveryObserver>,
    hooks: ReminderHooks,
}

impl ReminderRegistrationProvider {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        let store = SqliteReminderStore::new(database_path, config.values.pet_hunger_decay_per_day);
        let service = ReminderService::new(store, SystemReminderClock, RuntimeDiscordPort)
            .with_cooldowns(ReminderCooldowns {
                wheel_seconds: config.values.wheel_cooldown_seconds,
                trivia_seconds: config.values.trivia_cooldown_seconds,
            });
        Self::from_service(service, discord)
    }

    fn from_service(service: LiveReminderService, discord: Arc<dyn DiscordTransport>) -> Self {
        let state = Arc::new(ReminderRuntimeState {
            service: Mutex::new(service),
            timers: Mutex::new(BTreeMap::new()),
            discord,
            recovery_guard: tokio::sync::Mutex::new(()),
        });
        Self {
            handler: Arc::new(ReminderHandler {
                state: Arc::clone(&state),
            }),
            observer: Arc::new(ReminderRecoveryObserver {
                state: Arc::clone(&state),
            }),
            hooks: ReminderHooks { state },
        }
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        self.observer.clone()
    }

    #[must_use]
    pub fn hooks(&self) -> ReminderHooks {
        self.hooks.clone()
    }
}

impl RegistrationProvider for ReminderRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "setreminder".to_owned(),
            description: "Configure DM reminders for cooldowns and match betting windows."
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

struct TimerGeneration {
    task_id: TaskId,
    handle: JoinHandle<()>,
}

struct ReminderRuntimeState {
    service: Mutex<LiveReminderService>,
    timers: Mutex<BTreeMap<ReminderTaskKey, TimerGeneration>>,
    discord: Arc<dyn DiscordTransport>,
    recovery_guard: tokio::sync::Mutex<()>,
}

impl ReminderRuntimeState {
    fn lock_service(&self) -> Result<std::sync::MutexGuard<'_, LiveReminderService>, String> {
        self.service
            .lock()
            .map_err(|_| "reminder service lock was poisoned".to_owned())
    }

    fn lock_timers(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<ReminderTaskKey, TimerGeneration>>, String> {
        self.timers
            .lock()
            .map_err(|_| "reminder timer lock was poisoned".to_owned())
    }

    fn task_snapshot(&self, key: ReminderTaskKey) -> Result<Option<ScheduledReminder>, String> {
        Ok(self.lock_service()?.task(key).cloned())
    }

    fn sync_task(self: &Arc<Self>, key: ReminderTaskKey) -> Result<(), String> {
        let task = self.task_snapshot(key)?;
        let mut timers = self.lock_timers()?;
        if let (Some(existing), Some(task)) = (timers.get(&key), task.as_ref())
            && existing.task_id == task.id
        {
            return Ok(());
        }
        if let Some(existing) = timers.remove(&key) {
            existing.handle.abort();
        }
        let Some(task) = task else {
            return Ok(());
        };

        let state = Arc::clone(self);
        let task_id = task.id;
        let delay = Duration::from_secs(u64::try_from(task.delay_seconds).unwrap_or_default());
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            state.fire_task(key, task_id).await;
        });
        timers.insert(key, TimerGeneration { task_id, handle });
        drop(timers);

        // A zero-delay task can complete before insertion. Reconcile once so a
        // completed generation never leaves a stale timer registry entry.
        if self.task_snapshot(key)?.is_none()
            && let Some(completed) = self.lock_timers()?.remove(&key)
        {
            completed.handle.abort();
        }
        Ok(())
    }

    fn sync_all_tasks(self: &Arc<Self>) -> Result<(), String> {
        let service_keys: BTreeSet<_> = self.lock_service()?.tasks().keys().copied().collect();
        let timer_keys: BTreeSet<_> = self.lock_timers()?.keys().copied().collect();
        for key in service_keys.union(&timer_keys).copied() {
            self.sync_task(key)?;
        }
        Ok(())
    }

    async fn fire_task(self: Arc<Self>, key: ReminderTaskKey, task_id: TaskId) {
        let claimed = match self.lock_service() {
            Ok(mut service) => service.claim_task(key, task_id),
            Err(error) => {
                warn!(%error, ?key, "reminder timer could not claim service state");
                None
            }
        };
        if let Ok(mut timers) = self.lock_timers()
            && timers.get(&key).map(|timer| timer.task_id) == Some(task_id)
        {
            timers.remove(&key);
        }
        let Some(task) = claimed else {
            return;
        };
        let Ok(user_id) = u64::try_from(task.key.user_id.value()) else {
            debug!(
                user_id = task.key.user_id.value(),
                "invalid reminder DM recipient"
            );
            return;
        };
        if let Err(error) = self
            .discord
            .send_direct_message(
                user_id,
                DiscordMessage::silent(InteractionResponse::message(task.message)),
            )
            .await
        {
            // Python consumes a cooldown reminder after one attempt and does
            // not persist or retry Discord failures.
            debug!(%error, user_id, kind = %task.key.kind, "reminder DM attempt failed");
        }
    }

    async fn preferences(
        self: &Arc<Self>,
        user_id: UserId,
        guild_id: GuildId,
    ) -> Result<ReminderPreferences, String> {
        let state = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            state
                .lock_service()?
                .get_preferences(user_id, Some(guild_id))
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("reminder preference task failed: {error}"))?
    }

    async fn toggle(
        self: &Arc<Self>,
        user_id: UserId,
        guild_id: GuildId,
        kind: ReminderKind,
    ) -> Result<bool, String> {
        let state = Arc::clone(self);
        let enabled = tokio::task::spawn_blocking(move || {
            state
                .lock_service()?
                .toggle_preference(user_id, Some(guild_id), kind)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("reminder toggle task failed: {error}"))??;
        self.sync_task(ReminderTaskKey::new(user_id, guild_id, kind))?;
        Ok(enabled)
    }

    async fn arm(
        self: &Arc<Self>,
        user_id: UserId,
        guild_id: GuildId,
        kind: ReminderKind,
    ) -> Result<(), String> {
        let state = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            state
                .lock_service()?
                .arm_preference(user_id, Some(guild_id), kind)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("reminder arm task failed: {error}"))??;
        self.sync_task(ReminderTaskKey::new(user_id, guild_id, kind))
    }

    async fn send_delivery(&self, delivery: ReminderDelivery) -> ReminderDeliveryReport {
        let mut report = ReminderDeliveryReport::default();
        for recipient in delivery.recipients {
            report.attempted += 1;
            let result = match u64::try_from(recipient.value()) {
                Ok(user_id) => {
                    self.discord
                        .send_direct_message(
                            user_id,
                            DiscordMessage::silent(InteractionResponse::message(
                                delivery.message.clone(),
                            )),
                        )
                        .await
                }
                Err(error) => Err(error.to_string()),
            };
            match result {
                Ok(()) => report.delivered += 1,
                Err(error) => report.failures.push((recipient.value(), error)),
            }
        }
        report
    }
}

struct ReminderHandler {
    state: Arc<ReminderRuntimeState>,
}

#[async_trait]
impl InteractionHandler for ReminderHandler {
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
                if name != "setreminder" {
                    return Err(format!("reminder handler received command {name:?}").into());
                }
                let (user_id, guild_id) = signed_ids(user_id, guild_id)?;
                let preferences = self.state.preferences(user_id, guild_id).await?;
                responder
                    .respond(reminder_response(preferences))
                    .await
                    .map_err(|error| error.to_string().into())
            }
            InteractionRequest::Component {
                custom_id,
                user_id,
                guild_id,
                ..
            } => {
                let kind = component_kind(&custom_id)?;
                let (user_id, guild_id) = signed_ids(user_id, guild_id)?;
                let enabled = self.state.toggle(user_id, guild_id, kind).await?;
                if enabled && let Err(error) = self.state.arm(user_id, guild_id, kind).await {
                    // Python persists the toggle and still refreshes the UI if
                    // immediate arming fails.
                    warn!(%error, %kind, user_id = user_id.value(), "failed to arm enabled reminder");
                }
                let preferences = self.state.preferences(user_id, guild_id).await?;
                responder
                    .update(reminder_response(preferences))
                    .await
                    .map_err(|error| error.to_string().into())
            }
            InteractionRequest::Autocomplete { .. } | InteractionRequest::Modal { .. } => {
                Err("reminder handler received an unsupported interaction".into())
            }
        }
    }
}

pub struct ReminderRecoveryObserver {
    state: Arc<ReminderRuntimeState>,
}

#[async_trait]
impl GatewayEventObserver for ReminderRecoveryObserver {
    fn name(&self) -> &'static str {
        OBSERVER_NAME
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let _recovery = self.state.recovery_guard.lock().await;
        let mut report = ReadyRecoveryReport::empty(OBSERVER_NAME, context.guild_ids().len());
        let converted = context
            .guild_ids()
            .iter()
            .copied()
            .map(|guild_id| {
                i64::try_from(guild_id)
                    .map(|signed| (guild_id, Some(GuildId::new(signed))))
                    .map_err(|_| ReadyRecoveryFailure {
                        guild_id,
                        message: format!(
                            "Discord guild snowflake {guild_id} exceeds SQLite INTEGER range"
                        ),
                    })
            })
            .collect::<Vec<_>>();
        let guilds: Vec<_> = converted
            .iter()
            .filter_map(|item| item.as_ref().ok().map(|v| v.1))
            .collect();
        report
            .failures
            .extend(converted.into_iter().filter_map(Result::err));

        let state = Arc::clone(&self.state);
        let recovered = tokio::task::spawn_blocking(move || {
            Ok::<_, String>(state.lock_service()?.reschedule_all(&guilds))
        })
        .await;
        match recovered {
            Ok(Ok(recovery)) => {
                let failed_guilds: BTreeSet<_> = recovery
                    .failures
                    .iter()
                    .map(|failure| failure.guild_id.value())
                    .collect();
                report
                    .failures
                    .extend(
                        recovery
                            .failures
                            .into_iter()
                            .map(|failure| ReadyRecoveryFailure {
                                guild_id: u64::try_from(failure.guild_id.value())
                                    .unwrap_or_default(),
                                message: format!("{:?}: {}", failure.stage, failure.error),
                            }),
                    );
                report.guilds_refreshed = context
                    .guild_ids()
                    .iter()
                    .filter(|guild_id| {
                        i64::try_from(**guild_id)
                            .is_ok_and(|guild_id| !failed_guilds.contains(&guild_id))
                    })
                    .count();
                if let Err(error) = self.state.sync_all_tasks() {
                    for &guild_id in context.guild_ids() {
                        report.failures.push(ReadyRecoveryFailure {
                            guild_id,
                            message: error.clone(),
                        });
                    }
                    report.guilds_refreshed = 0;
                }
            }
            Ok(Err(message)) => {
                report
                    .failures
                    .extend(
                        context
                            .guild_ids()
                            .iter()
                            .map(|&guild_id| ReadyRecoveryFailure {
                                guild_id,
                                message: message.clone(),
                            }),
                    );
            }
            Err(error) => {
                let message = format!("reminder recovery task failed: {error}");
                report
                    .failures
                    .extend(
                        context
                            .guild_ids()
                            .iter()
                            .map(|&guild_id| ReadyRecoveryFailure {
                                guild_id,
                                message: message.clone(),
                            }),
                    );
            }
        }
        report
    }
}

/// Typed integration surface for future Rust action providers. Retaining the
/// command and ready observer does not itself wire these action-time calls.
#[derive(Clone)]
pub struct ReminderHooks {
    state: Arc<ReminderRuntimeState>,
}

impl ReminderHooks {
    #[cfg(all(test, feature = "runtime-test-dig"))]
    pub(crate) fn test_task_snapshot(
        &self,
        key: ReminderTaskKey,
    ) -> Result<Option<ScheduledReminder>, String> {
        self.state.task_snapshot(key)
    }

    /// Read the durable per-guild pet-DM preference without blocking Tokio.
    pub async fn pet_enabled(&self, user_id: i64, guild_id: i64) -> Result<bool, String> {
        self.state
            .preferences(UserId::new(user_id), GuildId::new(guild_id))
            .await
            .map(|preferences| preferences.enabled(ReminderKind::Pet))
    }

    pub async fn schedule_wheel(
        &self,
        user_id: i64,
        guild_id: i64,
        ready_at: i64,
    ) -> Result<(), String> {
        self.schedule_cooldown(ReminderKind::Wheel, user_id, guild_id, ready_at)
            .await
    }

    pub async fn schedule_trivia(
        &self,
        user_id: i64,
        guild_id: i64,
        ready_at: i64,
    ) -> Result<(), String> {
        self.schedule_cooldown(ReminderKind::Trivia, user_id, guild_id, ready_at)
            .await
    }

    async fn schedule_cooldown(
        &self,
        kind: ReminderKind,
        user_id: i64,
        guild_id: i64,
        ready_at: i64,
    ) -> Result<(), String> {
        let user_id = UserId::new(user_id);
        let guild_id = GuildId::new(guild_id);
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            let mut service = state.lock_service()?;
            match kind {
                ReminderKind::Wheel => {
                    service.schedule_wheel_reminder(user_id, Some(guild_id), ready_at, None)
                }
                ReminderKind::Trivia => {
                    service.schedule_trivia_reminder(user_id, Some(guild_id), ready_at, None)
                }
                _ => unreachable!("typed cooldown hook has a closed kind"),
            }
            .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("reminder schedule task failed: {error}"))??;
        self.state
            .sync_task(ReminderTaskKey::new(user_id, guild_id, kind))
    }

    pub async fn reconcile_dig(
        &self,
        user_id: i64,
        guild_id: i64,
        reference_now: Option<i64>,
    ) -> Result<(), String> {
        let user_id = UserId::new(user_id);
        let guild_id = GuildId::new(guild_id);
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            state
                .lock_service()?
                .reconcile_dig_reminder(user_id, Some(guild_id), reference_now, None)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("dig reminder reconcile task failed: {error}"))??;
        self.state
            .sync_task(ReminderTaskKey::new(user_id, guild_id, ReminderKind::Dig))
    }

    pub fn cancel_dig(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        let key = ReminderTaskKey::new(
            UserId::new(user_id),
            GuildId::new(guild_id),
            ReminderKind::Dig,
        );
        self.state
            .lock_service()?
            .cancel_dig_reminder(key.user_id, Some(key.guild_id));
        self.state.sync_task(key)
    }

    pub async fn rearm_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        self.state
            .arm(
                UserId::new(user_id),
                GuildId::new(guild_id),
                ReminderKind::Pet,
            )
            .await
    }

    pub fn cancel_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        let key = ReminderTaskKey::new(
            UserId::new(user_id),
            GuildId::new(guild_id),
            ReminderKind::Pet,
        );
        self.state
            .lock_service()?
            .cancel_pet_reminder(key.user_id, Some(key.guild_id));
        self.state.sync_task(key)
    }

    /// Cancel the durable pet reminder and its in-memory timer off Tokio's
    /// async worker threads.
    pub async fn cancel_pet_async(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        let hooks = self.clone();
        tokio::task::spawn_blocking(move || hooks.cancel_pet(user_id, guild_id))
            .await
            .map_err(|error| format!("pet reminder cancellation task failed: {error}"))?
    }

    pub async fn notify_betting(
        &self,
        guild_id: i64,
        bet_lock_until: i64,
        pending_match_id: Option<i64>,
        lobby_kind: Option<LobbyKind>,
    ) -> Result<ReminderDeliveryReport, String> {
        let state = Arc::clone(&self.state);
        let delivery = tokio::task::spawn_blocking(move || {
            state
                .lock_service()?
                .prepare_betting_delivery(
                    Some(GuildId::new(guild_id)),
                    bet_lock_until,
                    pending_match_id,
                    lobby_kind,
                )
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("betting reminder task failed: {error}"))??;
        Ok(match delivery {
            Some(delivery) => self.state.send_delivery(delivery).await,
            None => ReminderDeliveryReport::default(),
        })
    }
}

fn signed_ids(user_id: u64, guild_id: Option<u64>) -> Result<(UserId, GuildId), String> {
    let user_id = i64::try_from(user_id)
        .map_err(|_| format!("Discord user snowflake {user_id} exceeds SQLite INTEGER range"))?;
    let guild_id = guild_id
        .map(i64::try_from)
        .transpose()
        .map_err(|_| "Discord guild snowflake exceeds SQLite INTEGER range".to_owned())?
        .unwrap_or_default();
    Ok((UserId::new(user_id), GuildId::new(guild_id)))
}

fn component_kind(custom_id: &str) -> Result<ReminderKind, String> {
    DISPLAYED_REMINDERS
        .iter()
        .find(|reminder| custom_id == format!("{COMPONENT_PREFIX}{}", reminder.kind.as_str()))
        .map(|reminder| reminder.kind)
        .ok_or_else(|| format!("unknown reminder component {custom_id:?}"))
}

fn reminder_response(preferences: ReminderPreferences) -> InteractionResponse {
    let mut lines = vec!["**Your reminder settings:**\n".to_owned()];
    let buttons = DISPLAYED_REMINDERS
        .iter()
        .map(|reminder| {
            let enabled = preferences.enabled(reminder.kind);
            let icon = if enabled { "🔔" } else { "🔕" };
            lines.push(format!(
                "{icon} **{}** — {}",
                reminder.label, reminder.description
            ));
            InteractionButton::new(
                format!("{COMPONENT_PREFIX}{}", reminder.kind.as_str()),
                format!("{}: {}", reminder.label, if enabled { "ON" } else { "OFF" }),
            )
            .emoji(icon)
            .style(if enabled {
                InteractionButtonStyle::Success
            } else {
                InteractionButtonStyle::Secondary
            })
        })
        .collect();
    InteractionResponse::message(lines.join("\n"))
        .ephemeral()
        .action_row(InteractionActionRow::buttons(buttons))
}

#[cfg(all(test, feature = "runtime-test-dig"))]
#[path = "reminder_provider/tests.rs"]
pub(crate) mod tests;
