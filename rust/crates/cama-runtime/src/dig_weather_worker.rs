//! Supervised daily Dig weather broadcast worker.
//!
//! The worker starts after the gateway's Ready event, snapshots the current
//! game date without broadcasting it, and then checks every ten minutes. On a
//! rollover it records one forecast per live guild through SQLite's blocking
//! pool and attempts one Discord announcement, matching Python's in-memory
//! `_last_weather_date` delivery semantics.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cama_db::dig_weather::{DigWeatherEntry, DigWeatherRepository};
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
pub const DIG_WEATHER_WAKE_INTERVAL: Duration = Duration::from_secs(600);

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

    #[cfg(test)]
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

    async fn broadcast_rollover(
        &self,
        game_date: &str,
        context: &mut WorkerContext,
    ) -> Result<(), String> {
        let guild_ids = self
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
        let now = self.clock.now();

        for guild_id in guild_ids {
            if context.shutdown_requested() {
                return Ok(());
            }
            let weather = match self
                .ensure_weather(guild_id, game_date.to_owned(), now)
                .await
            {
                Ok(weather) => weather,
                Err(error) => {
                    warn!(guild_id, %error, "failed to roll Dig weather for guild");
                    continue;
                }
            };
            let Some(channel_id) = self.channel_for_guild(guild_id, &fallback_channels).await
            else {
                debug!(guild_id, "no Dig weather broadcast channel found");
                continue;
            };

            let delivery = self.discord.announce(channel_id, &weather);
            let result = tokio::select! {
                result = delivery => result,
                () = context.cancelled() => return Ok(()),
            };
            match result {
                Ok(()) => info!(guild_id, channel_id, game_date, "broadcast Dig weather"),
                Err(error) => warn!(
                    guild_id,
                    channel_id,
                    game_date,
                    %error,
                    "failed to broadcast Dig weather for guild"
                ),
            }
        }
        Ok(())
    }

    async fn poll_once(
        &self,
        last_weather_date: &mut String,
        context: &mut WorkerContext,
    ) -> Result<(), String> {
        let today = self.clock.game_date();
        if today == *last_weather_date {
            return Ok(());
        }

        // Python advances this in-memory marker before guild I/O and does not
        // retry failed announcements until the next date rollover.
        *last_weather_date = today.clone();
        self.broadcast_rollover(&today, context).await
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
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
#[path = "dig_weather_worker/tests.rs"]
mod tests;
