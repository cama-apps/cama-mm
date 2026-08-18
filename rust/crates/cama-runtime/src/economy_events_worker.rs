//! Production background controller for daily economy events.
//!
//! Event activation and both announcement stamps use the existing SQLite
//! schema. A committed event row is the durable activation marker; nullable
//! initial/reminder timestamps are the delivery outbox. Discord failures leave
//! those stamps untouched, so the next wake retries without applying direct
//! monetary effects twice.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_app::bot_tasks::{EconomyEvent as DisplayEconomyEvent, build_public_economy_event_embed};
use cama_app::economy_event_service::{EVENT_CATALOG, EconomyEventConfig};
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_db::economy_event_repository::EconomyEvent;
use chrono::Utc;
use tracing::{info, warn};

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::gamba_guild_source::GambaGuildSource;
use crate::registration::{InteractionEmbed, InteractionResponse};
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const ECONOMY_EVENTS_WORKER_NAME: &str = "economy_events";

#[derive(Clone, Debug)]
pub struct EconomyEventsWorkerConfig {
    pub enabled: bool,
    pub wake_interval: Duration,
    pub event: EconomyEventConfig,
}

impl EconomyEventsWorkerConfig {
    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        let values = &config.values;
        Self {
            enabled: values.economy_events_enabled,
            wake_interval: Duration::from_secs(
                u64::try_from(values.economy_event_wake_seconds.max(1)).unwrap_or(u64::MAX),
            ),
            event: EconomyEventConfig {
                enabled: values.economy_events_enabled,
                normal_annual_rate: values.economy_normal_annual_rate,
                inflation_ceiling: values.economy_inflation_ceiling,
                lookback_days: values.economy_event_lookback_days,
                max_reserve_burn_pct: values.economy_event_max_reserve_burn_pct,
                max_wallet_burn_pct: values.economy_event_max_wallet_burn_pct,
                trigger_hour_local: u8::try_from(
                    values.economy_event_trigger_hour_local.clamp(0, 23),
                )
                .unwrap_or(10),
            }
            .normalized(),
        }
    }
}

trait EconomyEventsClock: Send + Sync {
    fn now(&self) -> i64;
}

struct SystemEconomyEventsClock;

impl EconomyEventsClock for SystemEconomyEventsClock {
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

#[async_trait]
pub trait EconomyEventAnnouncementPort: Send + Sync {
    async fn announce(&self, channel_id: i64, event: &EconomyEvent) -> Result<(), String>;
}

struct DiscordEconomyEventAnnouncement {
    discord: Arc<dyn DiscordTransport>,
}

#[async_trait]
impl EconomyEventAnnouncementPort for DiscordEconomyEventAnnouncement {
    async fn announce(&self, channel_id: i64, event: &EconomyEvent) -> Result<(), String> {
        let channel_id = u64::try_from(channel_id)
            .map_err(|_| format!("Discord channel id {channel_id} is invalid"))?;
        let icon_url = EVENT_CATALOG
            .iter()
            .copied()
            .find(|template| template.name == event.name)
            .map(|template| template.ability_icon_url());
        let display = DisplayEconomyEvent {
            name: Some(event.name.clone()),
            severity: u8::try_from(event.severity.clamp(1, 5)).unwrap_or(1),
            direction: event.direction.as_str().to_owned(),
            announcement: Some(event.announcement.clone()),
            ends_at: Some(event.ends_at),
            effects: cama_app::economy_event_service::EconomyEventEffects {
                reward_multiplier: event.effects.reward_multiplier,
                gamba_win_multiplier: event.effects.gamba_win_multiplier,
                gamba_loss_multiplier: event.effects.gamba_loss_multiplier,
                bet_payout_multiplier: event.effects.bet_payout_multiplier,
                prediction_payout_multiplier: event.effects.prediction_payout_multiplier,
                prediction_depth_multiplier: event.effects.prediction_depth_multiplier,
                prediction_spread_ticks_delta: i32::try_from(
                    event.effects.prediction_spread_ticks_delta,
                )
                .unwrap_or_default(),
                reserve_burn_jc: event.effects.reserve_burn_jc,
                reserve_release_jc: event.effects.reserve_release_jc,
                wallet_burn_rate: event.effects.wallet_burn_rate,
                wallet_burn_jc: event.effects.wallet_burn_jc,
                reserve_voting_disabled: event.effects.reserve_voting_disabled
                    || (event.direction.as_str() == "deflationary" && event.severity >= 3),
            },
        };
        let public = build_public_economy_event_embed(&display, icon_url.as_deref());
        let mut embed = InteractionEmbed::titled(public.title)
            .description(public.description)
            .color(public.color)
            .footer(public.footer);
        if let Some(image_url) = public.thumbnail_url {
            embed = embed.thumbnail(image_url);
        }
        for (name, value) in public.fields {
            embed = embed.field(name, value, false);
        }
        self.discord
            .send_message(
                channel_id,
                DiscordMessage::silent(InteractionResponse::message("").embed(embed)),
            )
            .await
            .map(|_| ())
    }
}

pub struct EconomyEventsWorker {
    service: Arc<SqliteEconomyEventService>,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    gamba_source: Arc<dyn GambaGuildSource>,
    announcements: Arc<dyn EconomyEventAnnouncementPort>,
    clock: Arc<dyn EconomyEventsClock>,
    wake_interval: Duration,
}

impl EconomyEventsWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: EconomyEventsWorkerConfig,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        gamba_source: Arc<dyn GambaGuildSource>,
        announcements: Arc<dyn EconomyEventAnnouncementPort>,
    ) -> Self {
        Self {
            service: Arc::new(SqliteEconomyEventService::new(database_path, config.event)),
            guild_source,
            gamba_source,
            announcements,
            clock: Arc::new(SystemEconomyEventsClock),
            wake_interval: config.wake_interval,
        }
    }

    #[cfg(all(test, feature = "runtime-test-dig"))]
    fn with_clock(
        database_path: impl AsRef<Path>,
        config: EconomyEventsWorkerConfig,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        gamba_source: Arc<dyn GambaGuildSource>,
        announcements: Arc<dyn EconomyEventAnnouncementPort>,
        clock: Arc<dyn EconomyEventsClock>,
    ) -> Self {
        let mut worker = Self::new(
            database_path,
            config,
            guild_source,
            gamba_source,
            announcements,
        );
        worker.clock = clock;
        worker
    }

    async fn ensure_guild(&self, guild_id: i64, now: i64) -> Result<Option<EconomyEvent>, String> {
        let service = Arc::clone(&self.service);
        tokio::task::spawn_blocking(move || {
            service
                .ensure_daily_event_at(guild_id, now)
                .map(|(event, _created)| event)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("economy-event SQLite task failed: {error}"))?
    }

    async fn stamp_delivery(
        &self,
        guild_id: i64,
        event_id: i64,
        slot: &'static str,
        now: i64,
    ) -> Result<(), String> {
        let service = Arc::clone(&self.service);
        tokio::task::spawn_blocking(move || {
            let result = if slot == "initial" {
                service.mark_event_announced_at(guild_id, event_id, now)
            } else {
                service.mark_event_reminder_announced_at(guild_id, event_id, now)
            };
            result.map(|_| ()).map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("economy-event stamp SQLite task failed: {error}"))?
    }

    fn sleep_duration(&self, now: i64) -> Result<Duration, String> {
        let seconds = self
            .service
            .seconds_until_next_trigger_at(now)
            .map_err(|error| error.to_string())?;
        let until_trigger = Duration::from_secs(u64::try_from(seconds.max(0)).unwrap_or(0));
        Ok(self.wake_interval.min(until_trigger))
    }

    async fn wake_once(&self, context: &mut WorkerContext) -> Result<Duration, String> {
        let now = self.clock.now();
        let guild_ids = self
            .guild_source
            .live_guild_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let destinations = self
            .gamba_source
            .live_gamba_destinations()?
            .into_iter()
            .map(|destination| (destination.guild_id, destination.channel_id))
            .collect::<BTreeMap<_, _>>();
        let mut persistence_failures = Vec::new();

        for guild_id in guild_ids {
            if context.shutdown_requested() {
                return Ok(Duration::ZERO);
            }
            let event = match self.ensure_guild(guild_id, now).await {
                Ok(event) => event,
                Err(error) => {
                    warn!(guild_id, %error, "economy event activation failed");
                    persistence_failures.push(format!("guild {guild_id} activation: {error}"));
                    continue;
                }
            };
            let Some(event) = event else {
                continue;
            };
            let Some(slot) = SqliteEconomyEventService::<
                cama_app::economy_event_service::SystemClock,
                cama_app::economy_event_service::DeterministicEventRng,
            >::pending_announcement_slot(&event, now) else {
                continue;
            };
            let Some(&channel_id) = destinations.get(&guild_id) else {
                warn!(
                    guild_id,
                    event_id = event.event_id,
                    slot,
                    "economy event announcement remains pending: no live gamba channel"
                );
                continue;
            };
            let delivery = self.announcements.announce(channel_id, &event);
            let delivered = tokio::select! {
                result = delivery => result,
                () = context.cancelled() => return Ok(Duration::ZERO),
            };
            if let Err(error) = delivered {
                warn!(
                    guild_id,
                    channel_id,
                    event_id = event.event_id,
                    slot,
                    %error,
                    "economy event announcement remains pending"
                );
                continue;
            }
            if let Err(error) = self
                .stamp_delivery(guild_id, event.event_id, slot, now)
                .await
            {
                warn!(
                    guild_id,
                    event_id = event.event_id,
                    slot,
                    %error,
                    "economy event delivery stamp failed"
                );
                persistence_failures
                    .push(format!("guild {guild_id} {slot} delivery stamp: {error}"));
                continue;
            }
            info!(
                guild_id,
                channel_id,
                event_id = event.event_id,
                slot,
                "announced economy event"
            );
        }

        if persistence_failures.is_empty() {
            self.sleep_duration(now)
        } else {
            Err(persistence_failures.join("; "))
        }
    }
}

/// Build the retained production worker only when economy events are enabled.
#[must_use]
pub fn economy_events_worker_spec(
    database_path: impl AsRef<Path>,
    application_config: &ApplicationConfig,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    gamba_source: Arc<dyn GambaGuildSource>,
    discord: Arc<dyn DiscordTransport>,
) -> Option<BackgroundWorkerSpec> {
    let config = EconomyEventsWorkerConfig::from_application_config(application_config);
    let announcements: Arc<dyn EconomyEventAnnouncementPort> =
        Arc::new(DiscordEconomyEventAnnouncement { discord });
    economy_events_worker_spec_with_announcement(
        database_path,
        config,
        guild_source,
        gamba_source,
        announcements,
    )
}

fn economy_events_worker_spec_with_announcement(
    database_path: impl AsRef<Path>,
    config: EconomyEventsWorkerConfig,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    gamba_source: Arc<dyn GambaGuildSource>,
    announcements: Arc<dyn EconomyEventAnnouncementPort>,
) -> Option<BackgroundWorkerSpec> {
    config.enabled.then(|| {
        BackgroundWorkerSpec::new(
            ECONOMY_EVENTS_WORKER_NAME,
            Arc::new(EconomyEventsWorker::new(
                database_path,
                config,
                guild_source,
                gamba_source,
                announcements,
            )),
        )
    })
}

#[async_trait]
impl BackgroundWorker for EconomyEventsWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            let sleep = match self.wake_once(&mut context).await {
                Ok(sleep) => sleep,
                Err(_) if context.shutdown_requested() => return Ok(()),
                Err(error) => return Err(error),
            };
            if !context.sleep(sleep).await {
                return Ok(());
            }
        }
    }
}

#[cfg(all(test, feature = "runtime-test-dig"))]
#[path = "economy_events_worker/tests.rs"]
mod tests;
