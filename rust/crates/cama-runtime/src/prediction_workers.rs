//! Production prediction-market refresh and digest workers.
//!
//! Every SQLite call runs on Tokio's blocking pool. Repository transactions
//! commit market/digest state together with `app_kv` outbox publications; the
//! async workers drain those publications through live Discord transports and
//! acknowledge only the exact generation delivered.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::drawing::draw_prediction_market_chart;
use cama_app::predictions::neutralize_discord_mentions;
use cama_db::prediction_worker_repository::{
    DuePredictionMarket, PredictionDigestMarket, PredictionDigestPayload,
    PredictionDigestPublication, PredictionRefreshPublication, PredictionRefreshPublicationKind,
    PredictionRefreshSettings, PredictionWorkerRepository,
};
use cama_db::predictions_repository::{Book, MarketSnapshot, Trade};
use chrono::{DateTime, TimeZone, Utc};
use tracing::{info, warn};

use crate::discord_transport::{DiscordAllowedMentions, DiscordMessage, DiscordTransport};
use crate::first_game_pool_worker::FirstGamePoolGuildSource;
use crate::gamba_guild_source::{GambaDestination, GambaGuildSource};
use crate::ids::blocking;
use crate::registration::{InteractionAttachment, InteractionEmbed, InteractionResponse};
use crate::{ApplicationConfig, BackgroundWorker, BackgroundWorkerSpec, WorkerContext};

pub const PREDICTION_REFRESH_WORKER_NAME: &str = "prediction_refresh";
pub const PREDICTION_DIGEST_WORKER_NAME: &str = "prediction_digest";
pub const PREDICTION_DIGEST_WAKE_INTERVAL: Duration = Duration::from_secs(60);
const FIELD_CAP: usize = 25;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionWorkerConfig {
    pub refresh: PredictionRefreshSettings,
    pub refresh_wake_interval: Duration,
    pub contract_value: i64,
    pub digest_hour_utc: u32,
    pub digest_wake_interval: Duration,
}

impl PredictionWorkerConfig {
    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        let values = &config.values;
        Self {
            refresh: PredictionRefreshSettings {
                refresh_seconds: values.prediction_refresh_seconds,
                levels_per_side: usize::try_from(values.prediction_refresh_levels_per_side.max(0))
                    .unwrap_or(usize::MAX),
                size_per_level: values.prediction_refresh_size_per_level,
                outer_level_sizes: values.prediction_refresh_outer_level_sizes.clone(),
                refresh_spread_ticks: values.prediction_refresh_spread_ticks,
                initial_spread_ticks: values.prediction_spread_ticks,
                tick_size: values.prediction_tick_size,
                fade_ticks: values.prediction_fade_ticks,
                price_low: values.prediction_price_low,
                price_high: values.prediction_price_high,
                initial_fair: values.prediction_initial_fair_default,
                recent_trades_shown: values.prediction_recent_trades_shown,
                economy_events_enabled: values.economy_events_enabled,
            },
            refresh_wake_interval: configured_duration(values.prediction_refresh_wake_seconds),
            contract_value: values.prediction_contract_value,
            digest_hour_utc: values.prediction_digest_hour_utc.rem_euclid(24) as u32,
            digest_wake_interval: PREDICTION_DIGEST_WAKE_INTERVAL,
        }
    }
}

fn configured_duration(seconds: i64) -> Duration {
    Duration::from_secs(u64::try_from(seconds.max(1)).unwrap_or(u64::MAX))
}

#[async_trait]
pub trait PredictionDiscordPort: FirstGamePoolGuildSource + Send + Sync {
    /// Fetch the persisted message before reviving its thread, then render the
    /// live market embed/chart while retaining the existing persistent view.
    async fn edit_prediction_market_message(
        &self,
        thread_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String>;

    /// Revive an archived market thread with the one-week archive window and
    /// send its activity summary.
    async fn send_prediction_thread_message(
        &self,
        thread_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String>;
}

trait PredictionClock: Send + Sync {
    fn now_seconds(&self) -> i64;
}

#[derive(Default)]
struct SystemPredictionClock;

impl PredictionClock for SystemPredictionClock {
    fn now_seconds(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs().try_into().unwrap_or(i64::MAX))
            .unwrap_or_default()
    }
}

trait PredictionDriftSource: Send + Sync {
    fn drift(&self, minimum: i64, maximum: i64) -> i64;
}

#[derive(Default)]
struct UniformPredictionDrift;

impl PredictionDriftSource for UniformPredictionDrift {
    fn drift(&self, minimum: i64, maximum: i64) -> i64 {
        if minimum >= maximum {
            return minimum;
        }
        fastrand::i64(minimum..=maximum)
    }
}

pub struct PredictionRefreshWorker {
    repository: PredictionWorkerRepository,
    settings: PredictionRefreshSettings,
    drift_min: i64,
    drift_max: i64,
    wake_interval: Duration,
    contract_value: i64,
    discord: Arc<dyn PredictionDiscordPort>,
    clock: Arc<dyn PredictionClock>,
    drift: Arc<dyn PredictionDriftSource>,
}

impl PredictionRefreshWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn PredictionDiscordPort>,
    ) -> Self {
        let worker_config = PredictionWorkerConfig::from_application_config(config);
        Self {
            repository: PredictionWorkerRepository::new(database_path),
            settings: worker_config.refresh,
            drift_min: config.values.prediction_drift_min,
            drift_max: config.values.prediction_drift_max,
            wake_interval: worker_config.refresh_wake_interval,
            contract_value: worker_config.contract_value,
            discord,
            clock: Arc::new(SystemPredictionClock),
            drift: Arc::new(UniformPredictionDrift),
        }
    }

    #[cfg(test)]
    fn with_test_dependencies(
        database_path: impl AsRef<Path>,
        settings: PredictionRefreshSettings,
        wake_interval: Duration,
        discord: Arc<dyn PredictionDiscordPort>,
        clock: Arc<dyn PredictionClock>,
        drift: Arc<dyn PredictionDriftSource>,
    ) -> Self {
        Self {
            repository: PredictionWorkerRepository::new(database_path),
            settings,
            drift_min: -3,
            drift_max: 3,
            wake_interval,
            contract_value: 10,
            discord,
            clock,
            drift,
        }
    }

    async fn due_markets(&self, now: i64) -> Result<Vec<DuePredictionMarket>, String> {
        let repository = self.repository.clone();
        let refresh_seconds = self.settings.refresh_seconds;
        blocking("prediction refresh scan blocking", move || {
            repository
                .due_refresh_markets(now, refresh_seconds)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn refresh_market(&self, market: DuePredictionMarket, now: i64) -> Result<(), String> {
        let repository = self.repository.clone();
        let settings = self.settings.clone();
        let drift = self.drift.drift(self.drift_min, self.drift_max);
        let outcome = blocking("prediction market refresh blocking", move || {
            repository
                .refresh_market_and_queue(market.prediction_id, now, drift, &settings)
                .map_err(|error| error.to_string())
        })
        .await?;
        if let Some(outcome) = outcome {
            info!(
                prediction_id = outcome.prediction_id,
                guild_id = outcome.guild_id,
                old_price = outcome.old_price,
                new_price = outcome.new_price,
                trade_count = outcome.trade_count,
                "refreshed prediction market and queued publications"
            );
        }
        Ok(())
    }

    async fn pending_publications(&self) -> Result<Vec<PredictionRefreshPublication>, String> {
        let repository = self.repository.clone();
        blocking("prediction refresh outbox read blocking", move || {
            repository
                .pending_refresh_publications()
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn deliver_publication(
        &self,
        publication: &PredictionRefreshPublication,
        now: i64,
    ) -> Result<(), String> {
        match &publication.kind {
            PredictionRefreshPublicationKind::MarketEmbed { prediction_id } => {
                let repository = self.repository.clone();
                let prediction_id = *prediction_id;
                let recent_limit = self.settings.recent_trades_shown;
                let display = blocking("prediction market display read blocking", move || {
                    repository
                        .market_display(prediction_id, recent_limit)
                        .map_err(|error| error.to_string())
                })
                .await?;
                let Some(display) = display else {
                    return Ok(());
                };
                if display.snapshot.market.status != "open" {
                    return Ok(());
                }
                let Some(thread_id) = display.snapshot.market.thread_id else {
                    return Ok(());
                };
                let Some(message_id) = display.snapshot.market.embed_message_id else {
                    return Ok(());
                };
                let message = prediction_market_message(
                    &display.snapshot,
                    &display.fair_history,
                    now,
                    self.contract_value,
                );
                self.discord
                    .edit_prediction_market_message(thread_id, message_id, message)
                    .await
            }
            PredictionRefreshPublicationKind::ThreadSummary {
                prediction_id,
                thread_id,
                content,
            } => {
                let repository = self.repository.clone();
                let prediction_id = *prediction_id;
                let open = blocking("prediction summary open-status check blocking", move || {
                    repository
                        .market_is_open(prediction_id)
                        .map_err(|error| error.to_string())
                })
                .await?;
                if !open {
                    return Ok(());
                }
                let mut users = summary_mention_users(content);
                users.sort_unstable();
                users.dedup();
                let response = InteractionResponse::message(content.clone());
                let message = if users.is_empty() {
                    DiscordMessage::silent(response)
                } else {
                    DiscordMessage::mentioning(response, users.into_iter().collect::<BTreeSet<_>>())
                };
                self.discord
                    .send_prediction_thread_message(*thread_id, message)
                    .await
            }
        }
    }

    async fn acknowledge(&self, publication: PredictionRefreshPublication) -> Result<(), String> {
        let repository = self.repository.clone();
        blocking(
            "prediction refresh outbox acknowledgement blocking",
            move || {
                repository
                    .acknowledge_refresh_publication(&publication)
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        )
        .await
    }

    async fn wake_once(&self, context: &WorkerContext) -> Result<(), String> {
        let now = self.clock.now_seconds();
        let live_guilds = self
            .discord
            .live_guild_ids()?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut failures = Vec::new();
        match self.due_markets(now).await {
            Ok(markets) => {
                for market in markets
                    .into_iter()
                    .filter(|market| live_guilds.contains(&market.guild_id))
                {
                    if context.shutdown_requested() {
                        return Ok(());
                    }
                    if let Err(error) = self.refresh_market(market.clone(), now).await {
                        warn!(
                            prediction_id = market.prediction_id,
                            guild_id = market.guild_id,
                            %error,
                            "prediction market refresh failed"
                        );
                        failures.push(format!("market {} refresh: {error}", market.prediction_id));
                    }
                }
            }
            Err(error) => failures.push(format!("due-market scan: {error}")),
        }

        match self.pending_publications().await {
            Ok(publications) => {
                for publication in publications
                    .into_iter()
                    .filter(|publication| live_guilds.contains(&publication.guild_id))
                {
                    if context.shutdown_requested() {
                        return Ok(());
                    }
                    match self.deliver_publication(&publication, now).await {
                        Ok(()) => {
                            if let Err(error) = self.acknowledge(publication.clone()).await {
                                failures
                                    .push(format!("{} acknowledgement: {error}", publication.key));
                            }
                        }
                        Err(error) => {
                            warn!(
                                guild_id = publication.guild_id,
                                key = publication.key,
                                %error,
                                "prediction refresh publication remains pending"
                            );
                            failures.push(format!("{} delivery: {error}", publication.key));
                        }
                    }
                }
            }
            Err(error) => failures.push(format!("outbox read: {error}")),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

#[async_trait]
impl BackgroundWorker for PredictionRefreshWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            self.wake_once(&context).await?;
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

pub struct PredictionDigestWorker {
    repository: PredictionWorkerRepository,
    guild_source: Arc<dyn GambaGuildSource>,
    discord: Arc<dyn DiscordTransport>,
    refresh_seconds: i64,
    anchor_hour_utc: u32,
    wake_interval: Duration,
    clock: Arc<dyn PredictionClock>,
}

impl PredictionDigestWorker {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        guild_source: Arc<dyn GambaGuildSource>,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        let worker_config = PredictionWorkerConfig::from_application_config(config);
        Self {
            repository: PredictionWorkerRepository::new(database_path),
            guild_source,
            discord,
            refresh_seconds: worker_config.refresh.refresh_seconds,
            anchor_hour_utc: worker_config.digest_hour_utc,
            wake_interval: worker_config.digest_wake_interval,
            clock: Arc::new(SystemPredictionClock),
        }
    }

    #[cfg(test)]
    fn with_test_dependencies(
        database_path: impl AsRef<Path>,
        guild_source: Arc<dyn GambaGuildSource>,
        discord: Arc<dyn DiscordTransport>,
        refresh_seconds: i64,
        anchor_hour_utc: u32,
        wake_interval: Duration,
        clock: Arc<dyn PredictionClock>,
    ) -> Self {
        Self {
            repository: PredictionWorkerRepository::new(database_path),
            guild_source,
            discord,
            refresh_seconds,
            anchor_hour_utc,
            wake_interval,
            clock,
        }
    }

    async fn queue_due_digest(
        &self,
        destination: GambaDestination,
        slot: i64,
        now: i64,
    ) -> Result<(), String> {
        let repository = self.repository.clone();
        let refresh_seconds = self.refresh_seconds;
        blocking("prediction digest queue blocking", move || {
            repository
                .queue_digest_if_due(destination.guild_id, slot, now, refresh_seconds)
                .map(drop)
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn pending_publications(&self) -> Result<Vec<PredictionDigestPublication>, String> {
        let repository = self.repository.clone();
        blocking("prediction digest outbox read blocking", move || {
            repository
                .pending_digest_publications()
                .map_err(|error| error.to_string())
        })
        .await
    }

    async fn deliver_publication(
        &self,
        publication: &PredictionDigestPublication,
        channel_id: i64,
        now: i64,
    ) -> Result<(), String> {
        let mut payload = publication.payload.clone();
        payload
            .markets
            .sort_by_key(|market| std::cmp::Reverse(market.volume_recent));
        let biggest = biggest_mover(&payload.markets).cloned();
        let attachment = if let Some(market) = &biggest {
            let repository = self.repository.clone();
            let guild_id = publication.guild_id;
            let prediction_id = market.prediction_id;
            let history = blocking("prediction digest chart history blocking", move || {
                repository
                    .digest_market_history(guild_id, prediction_id)
                    .map_err(|error| error.to_string())
            })
            .await?;
            Some(prediction_chart_attachment(market, &history, now))
        } else {
            None
        };
        let has_attachment = attachment.is_some();
        let message = prediction_digest_message(&payload, biggest.as_ref(), attachment);
        let channel_id = u64::try_from(channel_id)
            .map_err(|_| "Discord gamba channel ID exceeds u64".to_owned())?;
        match self.discord.send_message(channel_id, message).await {
            Ok(_) => Ok(()),
            Err(error) if has_attachment => {
                warn!(
                    guild_id = publication.guild_id,
                    %error,
                    "prediction digest attachment failed; retrying embed-only"
                );
                self.discord
                    .send_message(
                        channel_id,
                        prediction_digest_message(&payload, biggest.as_ref(), None),
                    )
                    .await
                    .map(drop)
                    .map_err(|fallback| {
                        format!("attachment delivery failed: {error}; embed-only retry: {fallback}")
                    })
            }
            Err(error) => Err(error),
        }
    }

    async fn acknowledge(&self, publication: PredictionDigestPublication) -> Result<(), String> {
        let repository = self.repository.clone();
        blocking(
            "prediction digest outbox acknowledgement blocking",
            move || {
                repository
                    .acknowledge_digest_publication(&publication)
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        )
        .await
    }

    async fn wake_once(&self, context: &WorkerContext) -> Result<(), String> {
        let destinations = self.guild_source.live_gamba_destinations()?;
        let by_guild = destinations
            .iter()
            .map(|destination| (destination.guild_id, destination.channel_id))
            .collect::<BTreeMap<_, _>>();
        let now = self.clock.now_seconds();
        let slot = latest_digest_slot(now, self.anchor_hour_utc)?;
        let mut failures = Vec::new();
        for destination in destinations {
            if context.shutdown_requested() {
                return Ok(());
            }
            if let Err(error) = self.queue_due_digest(destination, slot, now).await {
                warn!(guild_id = destination.guild_id, %error, "prediction digest queue failed");
                failures.push(format!("guild {} queue: {error}", destination.guild_id));
            }
        }
        match self.pending_publications().await {
            Ok(publications) => {
                for publication in publications {
                    if context.shutdown_requested() {
                        return Ok(());
                    }
                    let Some(&channel_id) = by_guild.get(&publication.guild_id) else {
                        continue;
                    };
                    match self
                        .deliver_publication(&publication, channel_id, now)
                        .await
                    {
                        Ok(()) => {
                            if let Err(error) = self.acknowledge(publication.clone()).await {
                                failures
                                    .push(format!("{} acknowledgement: {error}", publication.key));
                            }
                        }
                        Err(error) => {
                            warn!(
                                guild_id = publication.guild_id,
                                key = publication.key,
                                %error,
                                "prediction digest publication remains pending"
                            );
                            failures.push(format!("{} delivery: {error}", publication.key));
                        }
                    }
                }
            }
            Err(error) => failures.push(format!("outbox read: {error}")),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

#[async_trait]
impl BackgroundWorker for PredictionDigestWorker {
    async fn run(&self, mut context: WorkerContext) -> Result<(), String> {
        loop {
            if context.shutdown_requested() {
                return Ok(());
            }
            self.wake_once(&context).await?;
            if !context.sleep(self.wake_interval).await {
                return Ok(());
            }
        }
    }
}

#[must_use]
pub fn prediction_refresh_worker_spec(
    database_path: impl AsRef<Path>,
    config: &ApplicationConfig,
    discord: Arc<dyn PredictionDiscordPort>,
) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        PREDICTION_REFRESH_WORKER_NAME,
        Arc::new(PredictionRefreshWorker::new(database_path, config, discord)),
    )
}

#[must_use]
pub fn prediction_digest_worker_spec(
    database_path: impl AsRef<Path>,
    config: &ApplicationConfig,
    guild_source: Arc<dyn GambaGuildSource>,
    discord: Arc<dyn DiscordTransport>,
) -> BackgroundWorkerSpec {
    BackgroundWorkerSpec::new(
        PREDICTION_DIGEST_WORKER_NAME,
        Arc::new(PredictionDigestWorker::new(
            database_path,
            config,
            guild_source,
            discord,
        )),
    )
}

fn latest_digest_slot(now: i64, anchor_hour_utc: u32) -> Result<i64, String> {
    let datetime = DateTime::<Utc>::from_timestamp(now, 0)
        .ok_or_else(|| format!("prediction digest timestamp {now} is out of range"))?;
    let hours = [anchor_hour_utc % 24, (anchor_hour_utc + 12) % 24];
    let date = datetime.date_naive();
    let mut latest = None;
    for day_offset in [0_i64, -1] {
        let Some(candidate_date) = date.checked_add_signed(chrono::Duration::days(day_offset))
        else {
            continue;
        };
        for hour in hours {
            let Some(naive) = candidate_date.and_hms_opt(hour, 0, 0) else {
                continue;
            };
            let timestamp = Utc.from_utc_datetime(&naive).timestamp();
            if timestamp <= now {
                latest = Some(latest.map_or(timestamp, |value: i64| value.max(timestamp)));
            }
        }
    }
    latest.ok_or_else(|| "could not calculate prediction digest slot".to_owned())
}

fn prediction_market_message(
    snapshot: &MarketSnapshot,
    fair_history: &[(i64, i64)],
    now: i64,
    contract_value: i64,
) -> DiscordMessage {
    let market = &snapshot.market;
    let chart = prediction_chart_attachment(
        &PredictionDigestMarket {
            prediction_id: market.prediction_id,
            question: market.question.clone(),
            current_price: market.current_price,
            prev_price: market.prev_price,
            guild_id: market.guild_id,
            created_at: market.created_at,
            thread_id: market.thread_id,
            embed_message_id: market.embed_message_id,
            volume_recent: snapshot.volume_since_refresh,
        },
        fair_history,
        now,
    );
    let embed = market_embed(snapshot, &chart.filename, contract_value);
    let response = InteractionResponse::message("")
        .embed(embed)
        .attachment(chart);
    DiscordMessage::silent(response).preserving_content()
}

fn market_embed(
    snapshot: &MarketSnapshot,
    chart_filename: &str,
    contract_value: i64,
) -> InteractionEmbed {
    let market = &snapshot.market;
    let price = market
        .current_price
        .map_or_else(|| "?".to_owned(), |value| format!("{value}%"));
    InteractionEmbed::titled(format!("📈 Market #{}", market.prediction_id))
        .description(format!(
            "\"{}\"",
            neutralize_discord_mentions(&market.question)
        ))
        .color(0x3498DB)
        .field(
            "Current",
            format!(
                "YES **{price}**\ncontracts traded since refresh: **{}**",
                snapshot.volume_since_refresh
            ),
            false,
        )
        .field("Order book", format_ladder(&snapshot.book), false)
        .field(
            "Recent",
            format_recent_trades(&snapshot.recent_trades),
            false,
        )
        .image(format!("attachment://{chart_filename}"))
        .footer(format!("{contract_value} jopa per winning contract"))
}

fn format_ladder(book: &Book) -> String {
    const MAX_ROWS: usize = 8;
    let asks = book.yes_asks.iter().take(MAX_ROWS).collect::<Vec<_>>();
    let bids = book.yes_bids.iter().take(MAX_ROWS).collect::<Vec<_>>();
    let mut lines = vec!["🟢 Buy YES".to_owned()];
    if asks.is_empty() {
        lines.push("  (none — refreshes daily)".to_owned());
    } else {
        for (price, size) in asks.iter().rev() {
            lines.push(format!("  {price:>3}  {size:>3}"));
        }
    }
    let yes = asks
        .first()
        .map_or_else(|| "—".to_owned(), |(price, _)| format!("{price}%"));
    let no = bids
        .first()
        .map_or_else(|| "—".to_owned(), |(price, _)| format!("{}%", 100 - price));
    lines.push(format!("\n  ── YES {yes} · NO {no} ──\n"));
    lines.push("🔴 Buy NO".to_owned());
    if bids.is_empty() {
        lines.push("  (none — refreshes daily)".to_owned());
    } else {
        for (price, size) in bids {
            lines.push(format!("  {:>3}  {size:>3}", 100 - price));
        }
    }
    format!("```\n{}\n```", lines.join("\n"))
}

fn format_recent_trades(trades: &[Trade]) -> String {
    if trades.is_empty() {
        return "_no trades yet_".to_owned();
    }
    trades
        .iter()
        .map(|trade| {
            let verb = if trade.action.starts_with("buy") {
                "bought"
            } else {
                "sold"
            };
            let side = if trade.action.ends_with("yes") {
                "YES"
            } else {
                "NO"
            };
            format!(
                "  <@{}> {verb} {} {side} @ {}",
                trade.discord_id,
                trade.contracts,
                (trade.vwap_x100 + 50) / 100
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prediction_chart_attachment(
    market: &PredictionDigestMarket,
    fair_history: &[(i64, i64)],
    now: i64,
) -> InteractionAttachment {
    let title = neutralize_discord_mentions(&market.question);
    let chart = draw_prediction_market_chart(
        market.prediction_id,
        Some(&title),
        fair_history,
        market.created_at,
        now,
    );
    InteractionAttachment::bytes(
        format!("predict_{}.png", market.prediction_id),
        chart.into_inner(),
    )
}

fn biggest_mover(markets: &[PredictionDigestMarket]) -> Option<&PredictionDigestMarket> {
    markets
        .iter()
        .filter(|market| {
            market.current_price.is_some()
                && market.prev_price.is_some()
                && market.current_price != market.prev_price
        })
        .max_by_key(|market| {
            (market.current_price.unwrap_or_default() - market.prev_price.unwrap_or_default()).abs()
        })
}

fn prediction_digest_message(
    payload: &PredictionDigestPayload,
    biggest: Option<&PredictionDigestMarket>,
    attachment: Option<InteractionAttachment>,
) -> DiscordMessage {
    let mut description = Vec::new();
    if payload.split_banner {
        description.push(
            "**Markets stock-split 10:1** — quantities have been restated; jopa balances unchanged."
                .to_owned(),
        );
    }
    let mut embed = InteractionEmbed::titled("📈 Today in prediction markets").color(0x3498DB);
    if let Some(market) = biggest {
        let current = market.current_price.unwrap_or_default();
        let previous = market.prev_price.unwrap_or_default();
        let arrow = if current > previous { "↑" } else { "↓" };
        description.push(format!(
            "📊 **Biggest mover:** #{} {arrow}{} today ({}% → {}%)",
            market.prediction_id,
            (current - previous).abs(),
            previous,
            current
        ));
        embed = embed.image(format!("attachment://predict_{}.png", market.prediction_id));
    }
    if !description.is_empty() {
        embed = embed.description(description.join("\n\n"));
    }
    for market in payload.markets.iter().take(FIELD_CAP) {
        let (name, value) = digest_market_field(market);
        embed = embed.field(name, value, false);
    }
    if payload.markets.len() > FIELD_CAP {
        embed = embed.footer(format!(
            "+{} more — use /predict list",
            payload.markets.len() - FIELD_CAP
        ));
    }
    let response = match attachment {
        Some(attachment) => InteractionResponse::message("")
            .embed(embed)
            .attachment(attachment),
        None => InteractionResponse::message("").embed(embed),
    };
    DiscordMessage {
        response,
        allowed_mentions: DiscordAllowedMentions::None,
        content_mode: Default::default(),
    }
}

fn digest_market_field(market: &PredictionDigestMarket) -> (String, String) {
    let price = market
        .current_price
        .map_or_else(|| "?".to_owned(), |value| format!("{value}%"));
    let delta = match (market.current_price, market.prev_price) {
        (Some(current), Some(previous)) if current != previous => {
            let arrow = if current > previous { "↑" } else { "↓" };
            format!("  ({arrow}{} today)", (current - previous).abs())
        }
        _ => String::new(),
    };
    let name = format!("📈 #{}  ·  YES {price}{delta}", market.prediction_id);
    let mut question = neutralize_discord_mentions(market.question.trim());
    if question.chars().count() > 200 {
        question = format!("{}...", question.chars().take(197).collect::<String>());
    }
    let mut value = if question.is_empty() {
        "—".to_owned()
    } else {
        format!("\"{question}\"")
    };
    if let Some(thread_id) = market.thread_id {
        let link = market.embed_message_id.map_or_else(
            || {
                format!(
                    "https://discord.com/channels/{}/{thread_id}",
                    market.guild_id
                )
            },
            |message_id| {
                format!(
                    "https://discord.com/channels/{}/{thread_id}/{message_id}",
                    market.guild_id
                )
            },
        );
        value.push_str(&format!("\n[Trade →]({link})"));
    }
    (name, value)
}

fn summary_mention_users(content: &str) -> Vec<u64> {
    let mut users = Vec::new();
    let mut remaining = content;
    while let Some(start) = remaining.find("<@") {
        remaining = &remaining[start + 2..];
        let raw = remaining.strip_prefix('!').unwrap_or(remaining);
        let Some(end) = raw.find('>') else {
            break;
        };
        if let Ok(user_id) = raw[..end].parse() {
            users.push(user_id);
        }
        remaining = &raw[end + 1..];
    }
    users
}

#[cfg(test)]
#[path = "prediction_workers/tests.rs"]
mod tests;
