//! Supervised daily Dig weather broadcast worker.
//!
//! The worker starts after the gateway's Ready event, snapshots the current
//! game date without broadcasting it, and then checks every wall-clock minute. On a
//! rollover it records one forecast per live guild through SQLite's blocking
//! pool, queues a per-guild/day delivery marker, and drains that marker through
//! Discord. The forecast and delivery state survive a process restart even
//! though the startup date snapshot still follows Python's in-memory
//! `_last_weather_date` suppression.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_db::dig_weather::{DigWeatherBroadcast, DigWeatherEntry, DigWeatherRepository};
use chrono::Utc;
use tracing::{debug, info, warn};

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordMessage, DiscordTransport};
use crate::duel_challenges_worker::{DuelChannelKind, DuelDiscordPort};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::gamba_guild_source::GambaGuildSource;
use crate::registration::{InteractionEmbed, InteractionResponse};
use crate::serenity_transport::SerenityDiscordTransport;
use crate::{BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const DIG_WEATHER_WORKER_NAME: &str = "dig_weather_broadcast";
pub const DIG_WEATHER_WAKE_INTERVAL: Duration = Duration::from_secs(60);

/// Keep polling aligned to wall-clock boundaries instead of accumulating the
/// time spent querying SQLite and sending Discord messages. The game day rolls
/// at an exact minute, so a healthy worker now observes it on that boundary;
/// late wakes realign on the following iteration.
fn aligned_wake_delay(now: i64, interval: Duration) -> Duration {
    let interval_seconds = interval.as_secs();
    if interval_seconds == 0 {
        return interval;
    }
    let Ok(interval_seconds_i64) = i64::try_from(interval_seconds) else {
        return interval;
    };
    let elapsed = now.rem_euclid(interval_seconds_i64) as u64;
    Duration::from_secs(if elapsed == 0 {
        interval_seconds
    } else {
        interval_seconds - elapsed
    })
}

trait DigWeatherClock: Send + Sync {
    fn game_date(&self) -> String;
    fn now(&self) -> i64;
}

struct SystemDigWeatherClock;

impl DigWeatherClock for SystemDigWeatherClock {
    fn game_date(&self) -> String {
        cama_domain::game_date::get_game_date()
    }

    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

#[async_trait]
trait DigWeatherDiscordPort: Send + Sync {
    /// Resolve the configured channel only when it is a cached, same-guild,
    /// sendable text channel. Python uses `guild.get_channel`, so this never
    /// performs a network fetch.
    async fn configured_channel(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<Option<i64>, String>;

    async fn announce(&self, channel_id: i64, weather: &[DigWeatherEntry]) -> Result<(), String>;
}

#[async_trait]
impl DigWeatherDiscordPort for SerenityDiscordTransport {
    async fn configured_channel(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<Option<i64>, String> {
        let channel = DuelDiscordPort::cached_channel(self, channel_id).await?;
        Ok(channel
            .filter(|channel| {
                channel.guild_id == Some(guild_id)
                    && channel.kind == DuelChannelKind::Text
                    && channel.can_send
            })
            .map(|channel| channel.id))
    }

    async fn announce(&self, channel_id: i64, weather: &[DigWeatherEntry]) -> Result<(), String> {
        let channel_id = u64::try_from(channel_id)
            .map_err(|_| format!("Discord channel id {channel_id} is invalid"))?;
        let Some(embed) = weather_embed(weather) else {
            return Ok(());
        };
        DiscordTransport::send_message(
            self,
            channel_id,
            DiscordMessage::default_mentions(InteractionResponse::message("").embed(embed)),
        )
        .await
        .map(|_| ())
    }
}

fn weather_embed(weather: &[DigWeatherEntry]) -> Option<InteractionEmbed> {
    let definitions = weather
        .iter()
        .filter_map(DigWeatherEntry::definition)
        .collect::<Vec<_>>();
    if definitions.is_empty() {
        return None;
    }

    let mut embed = InteractionEmbed::titled("⛅ Daily Layer Weather")
        .description("New conditions have settled across the depths.")
        .color(0x58_65_F2)
        .footer("Weather affects all diggers in that layer today. Use /dig weather for details.");
    for weather in definitions {
        embed = embed.field(
            format!("{} — {}", weather.layer, weather.name),
            format!("*{}*", weather.description),
            false,
        );
    }
    Some(embed)
}

pub struct DigWeatherWorker {
    repository: DigWeatherRepository,
    configured_channel_id: Option<i64>,
    guild_source: Arc<dyn FirstGamePoolGuildSource>,
    gamba_source: Arc<dyn GambaGuildSource>,
    discord: Arc<dyn DigWeatherDiscordPort>,
    clock: Arc<dyn DigWeatherClock>,
    wake_interval: Duration,
}

impl DigWeatherWorker {
    #[must_use]
    fn new(
        database_path: impl AsRef<Path>,
        configured_channel_id: Option<i64>,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        gamba_source: Arc<dyn GambaGuildSource>,
        discord: Arc<dyn DigWeatherDiscordPort>,
    ) -> Self {
        Self {
            repository: DigWeatherRepository::new(database_path),
            configured_channel_id,
            guild_source,
            gamba_source,
            discord,
            clock: Arc::new(SystemDigWeatherClock),
            wake_interval: DIG_WEATHER_WAKE_INTERVAL,
        }
    }

    #[cfg(all(test, feature = "runtime-test-dig"))]
    fn with_clock_and_interval(
        database_path: impl AsRef<Path>,
        configured_channel_id: Option<i64>,
        guild_source: Arc<dyn FirstGamePoolGuildSource>,
        gamba_source: Arc<dyn GambaGuildSource>,
        discord: Arc<dyn DigWeatherDiscordPort>,
        clock: Arc<dyn DigWeatherClock>,
        wake_interval: Duration,
    ) -> Self {
        let mut worker = Self::new(
            database_path,
            configured_channel_id,
            guild_source,
            gamba_source,
            discord,
        );
        worker.clock = clock;
        worker.wake_interval = wake_interval;
        worker
    }

    async fn ensure_weather(
        &self,
        guild_id: i64,
        game_date: String,
        now: i64,
    ) -> Result<Vec<DigWeatherEntry>, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository
                .ensure_for_day(guild_id, &game_date, now)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("Dig weather SQLite task failed: {error}"))?
    }

    async fn ensure_weather_and_queue(
        &self,
        guild_id: i64,
        game_date: String,
        now: i64,
    ) -> Result<(), String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository
                .ensure_for_day_and_queue_broadcast(guild_id, &game_date, now)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("Dig weather SQLite queue task failed: {error}"))?
    }

    async fn pending_broadcasts(&self) -> Result<Vec<DigWeatherBroadcast>, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository
                .pending_broadcasts()
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("Dig weather outbox read task failed: {error}"))?
    }

    async fn mark_broadcast_sent(&self, guild_id: i64, game_date: String) -> Result<bool, String> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository
                .mark_broadcast_sent(guild_id, &game_date)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("Dig weather outbox stamp task failed: {error}"))?
    }

    async fn channel_for_guild(
        &self,
        guild_id: i64,
        fallback_channels: &BTreeMap<i64, i64>,
    ) -> Option<i64> {
        if let Some(configured_channel_id) = self.configured_channel_id {
            match self
                .discord
                .configured_channel(guild_id, configured_channel_id)
                .await
            {
                Ok(Some(channel_id)) => return Some(channel_id),
                Ok(None) => {}
                Err(error) => debug!(
                    guild_id,
                    channel_id = configured_channel_id,
                    %error,
                    "configured Dig weather channel lookup failed; using gamba fallback"
                ),
            }
        }
        fallback_channels.get(&guild_id).copied()
    }

    async fn queue_rollover(&self, game_date: &str) -> Result<bool, String> {
        let guild_ids = self
            .guild_source
            .live_guild_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let now = self.clock.now();
        let mut complete = true;

        for guild_id in guild_ids {
            if let Err(error) = self
                .ensure_weather_and_queue(guild_id, game_date.to_owned(), now)
                .await
            {
                warn!(guild_id, %error, "failed to roll Dig weather for guild");
                complete = false;
            }
        }
        Ok(complete)
    }

    async fn drain_pending(&self, context: &mut WorkerContext) -> Result<(), String> {
        let pending = self.pending_broadcasts().await?;
        if pending.is_empty() {
            return Ok(());
        }
        let live_guilds = self
            .guild_source
            .live_guild_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let fallback_channels = self
            .gamba_source
            .live_gamba_destinations()?
            .into_iter()
            .map(|destination| (destination.guild_id, destination.channel_id))
            .collect::<BTreeMap<_, _>>();

        for delivery in pending {
            if context.shutdown_requested() {
                return Ok(());
            }
            if !live_guilds.contains(&delivery.guild_id) {
                continue;
            }
            let weather = match self
                .ensure_weather(
                    delivery.guild_id,
                    delivery.game_date.clone(),
                    self.clock.now(),
                )
                .await
            {
                Ok(weather) => weather,
                Err(error) => {
                    warn!(
                        guild_id = delivery.guild_id,
                        game_date = %delivery.game_date,
                        %error,
                        "failed to load queued Dig weather"
                    );
                    continue;
                }
            };
            let Some(channel_id) = self
                .channel_for_guild(delivery.guild_id, &fallback_channels)
                .await
            else {
                debug!(
                    guild_id = delivery.guild_id,
                    game_date = %delivery.game_date,
                    "no Dig weather broadcast channel found"
                );
                continue;
            };

            let result = tokio::select! {
                result = self.discord.announce(channel_id, &weather) => result,
                () = context.cancelled() => return Ok(()),
            };
            match result {
                Ok(()) => {
                    match self
                        .mark_broadcast_sent(delivery.guild_id, delivery.game_date.clone())
                        .await
                    {
                        Ok(true) => info!(
                            guild_id = delivery.guild_id,
                            channel_id,
                            game_date = %delivery.game_date,
                            "broadcast Dig weather"
                        ),
                        Ok(false) => debug!(
                            guild_id = delivery.guild_id,
                            channel_id,
                            game_date = %delivery.game_date,
                            "Dig weather broadcast was already stamped"
                        ),
                        Err(error) => warn!(
                            guild_id = delivery.guild_id,
                            channel_id,
                            game_date = %delivery.game_date,
                            %error,
                            "Dig weather sent but durable stamp failed; retry may duplicate"
                        ),
                    }
                }
                Err(error) => warn!(
                    guild_id = delivery.guild_id,
                    channel_id,
                    game_date = %delivery.game_date,
                    %error,
                    "failed to broadcast Dig weather for guild"
                ),
            }
        }
        Ok(())
    }

    #[cfg(all(test, feature = "runtime-test-dig"))]
    async fn broadcast_rollover(
        &self,
        game_date: &str,
        context: &mut WorkerContext,
    ) -> Result<(), String> {
        self.queue_rollover(game_date).await?;
        self.drain_pending(context).await
    }

    async fn poll_once(
        &self,
        last_weather_date: &mut String,
        context: &mut WorkerContext,
    ) -> Result<(), String> {
        let today = self.clock.game_date();
        if today != *last_weather_date {
            // Queue the immutable forecast before advancing the in-memory
            // marker. A failed queue transaction is therefore retried on the
            // next wake, while the subsequent Discord delivery is durable.
            let queue_complete = match self.queue_rollover(&today).await {
                Ok(complete) => complete,
                Err(error) => {
                    warn!(game_date = %today, %error, "failed to enumerate Dig weather guilds");
                    false
                }
            };
            if queue_complete && !context.shutdown_requested() {
                *last_weather_date = today;
            }
        }
        if let Err(error) = self.drain_pending(context).await {
            warn!(%error, "failed to drain queued Dig weather broadcasts");
        }
        Ok(())
    }
}

/// Build the production worker retained and supervised by [`crate::Runtime`].
/// The caller needs only the migrated database, typed configuration, and the
/// already-shared live Serenity transport.
#[must_use]
pub fn dig_weather_worker_spec(
    database_path: impl AsRef<Path>,
    application_config: &ApplicationConfig,
    discord: Arc<SerenityDiscordTransport>,
) -> BackgroundWorkerSpec {
    let guild_source: Arc<dyn FirstGamePoolGuildSource> = discord.clone();
    let gamba_source: Arc<dyn GambaGuildSource> = discord.clone();
    let weather_discord: Arc<dyn DigWeatherDiscordPort> = discord;
    BackgroundWorkerSpec::new(
        DIG_WEATHER_WORKER_NAME,
        Arc::new(DigWeatherWorker::new(
            database_path,
            application_config.channels.dig,
            guild_source,
            gamba_source,
            weather_discord,
        )),
    )
}

#[async_trait]
impl BackgroundWorker for DigWeatherWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        if context.shutdown_requested() {
            return Ok(());
        }

        // Workers are started only after Ready. Capturing this date first is
        // the exact startup suppression performed by Python's before-loop hook.
        let mut last_weather_date = self.clock.game_date();
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            self.poll_once(&mut last_weather_date, &mut context).await?;
            let wake_delay = aligned_wake_delay(self.clock.now(), self.wake_interval);
            if !context.sleep(wake_delay).await {
                return Ok(());
            }
        }
    }
}

#[cfg(all(test, feature = "runtime-test-dig"))]
#[path = "dig_weather_worker/tests.rs"]
mod tests;
