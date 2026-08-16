//! Existing-schema persistence for prediction refresh and digest workers.
//!
//! `app_kv` is the schema-compatible transactional outbox. A refresh mutation
//! and every publication describing it commit in one `BEGIN IMMEDIATE`
//! transaction. Digest cursors, one-shot flags, and digest payloads use the
//! same boundary, so a process or Discord failure can only leave retryable
//! work behind; it cannot silently lose a committed publication.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::open_runtime_connection;
use crate::predictions_repository::{Book, Market, MarketSnapshot, NewLevel, Trade};

const REFRESH_EMBED_PREFIX: &str = "prediction_refresh_embed:";
const REFRESH_SUMMARY_PREFIX: &str = "prediction_refresh_summary:";
const DIGEST_CURSOR_KEY: &str = "prediction_digest_cursor";
const DIGEST_OUTBOX_PREFIX: &str = "prediction_digest_outbox:";
const SPLIT_ANNOUNCED_KEY: &str = "split_announced";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionRefreshSettings {
    pub refresh_seconds: i64,
    pub levels_per_side: usize,
    pub size_per_level: i64,
    pub outer_level_sizes: Vec<i64>,
    pub refresh_spread_ticks: i64,
    pub initial_spread_ticks: i64,
    pub tick_size: i64,
    pub fade_ticks: i64,
    pub price_low: i64,
    pub price_high: i64,
    pub initial_fair: i64,
    pub recent_trades_shown: i64,
    pub economy_events_enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuePredictionMarket {
    pub prediction_id: i64,
    pub guild_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionRefreshOutcome {
    pub prediction_id: i64,
    pub guild_id: i64,
    pub old_price: i64,
    pub new_price: i64,
    pub drift: i64,
    pub trade_count: usize,
    pub generation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionRefreshPublication {
    pub guild_id: i64,
    pub key: String,
    pub value: String,
    pub kind: PredictionRefreshPublicationKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PredictionRefreshPublicationKind {
    MarketEmbed {
        prediction_id: i64,
    },
    ThreadSummary {
        prediction_id: i64,
        thread_id: i64,
        content: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionMarketDisplay {
    pub snapshot: MarketSnapshot,
    pub fair_history: Vec<(i64, i64)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PredictionDigestMarket {
    pub prediction_id: i64,
    pub question: String,
    pub current_price: Option<i64>,
    pub prev_price: Option<i64>,
    pub guild_id: i64,
    pub created_at: i64,
    pub thread_id: Option<i64>,
    pub embed_message_id: Option<i64>,
    pub volume_recent: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PredictionDigestPayload {
    pub slot: i64,
    pub split_banner: bool,
    pub markets: Vec<PredictionDigestMarket>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionDigestPublication {
    pub guild_id: i64,
    pub key: String,
    pub value: String,
    pub payload: PredictionDigestPayload,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionDigestQueueOutcome {
    pub queued: bool,
    pub market_count: usize,
}

#[derive(Debug, Error)]
pub enum PredictionWorkerRepositoryError {
    #[error("invalid prediction refresh settings: {0}")]
    InvalidSettings(String),
    #[error("invalid prediction worker outbox row: {0}")]
    InvalidOutbox(String),
    #[error("prediction worker JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("prediction worker SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct PredictionWorkerRepository {
    path: PathBuf,
}

impl PredictionWorkerRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn due_refresh_markets(
        &self,
        now: i64,
        refresh_seconds: i64,
    ) -> Result<Vec<DuePredictionMarket>, PredictionWorkerRepositoryError> {
        if refresh_seconds <= 0 {
            return Err(PredictionWorkerRepositoryError::InvalidSettings(
                "refresh interval must be positive".to_owned(),
            ));
        }
        let connection = open_runtime_connection(&self.path)?;
        let cutoff = now.saturating_sub(refresh_seconds);
        let mut statement = connection.prepare(
            "SELECT prediction_id,guild_id FROM predictions
             WHERE status='open' AND COALESCE(last_refresh_at,0)<=?1
             ORDER BY COALESCE(last_refresh_at,0),prediction_id",
        )?;
        let rows = statement.query_map([cutoff], |row| {
            Ok(DuePredictionMarket {
                prediction_id: row.get(0)?,
                guild_id: row.get(1)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Recheck due/open state, refresh one market, and enqueue every Discord
    /// side effect in the same immediate transaction.
    pub fn refresh_market_and_queue(
        &self,
        prediction_id: i64,
        now: i64,
        drift: i64,
        settings: &PredictionRefreshSettings,
    ) -> Result<Option<PredictionRefreshOutcome>, PredictionWorkerRepositoryError> {
        validate_refresh_settings(settings)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(market) = select_market(&transaction, prediction_id)? else {
            transaction.commit()?;
            return Ok(None);
        };
        if market.status != "open"
            || market.last_refresh_at.unwrap_or(0) > now.saturating_sub(settings.refresh_seconds)
        {
            transaction.commit()?;
            return Ok(None);
        }

        let previous_refresh = market.last_refresh_at.unwrap_or(0);
        let old_price = market.current_price.unwrap_or(settings.initial_fair);
        let book = read_book(&transaction, prediction_id)?;
        let observed_mid = observed_mid(&transaction, &market, &book, settings.fade_ticks)?;
        let new_price =
            (python_round(observed_mid) + drift).clamp(settings.price_low, settings.price_high);
        let spread_delta = if settings.economy_events_enabled {
            active_prediction_spread_delta(&transaction, market.guild_id, now)?
        } else {
            0
        };
        let maximum_spread = (98 / settings.tick_size.max(1)).max(1);
        let adjusted_refresh_spread =
            (settings.refresh_spread_ticks + spread_delta).clamp(1, maximum_spread);
        let adjusted_initial_spread =
            (settings.initial_spread_ticks + spread_delta).clamp(1, maximum_spread);
        let levels = build_levels(new_price, adjusted_refresh_spread, settings);
        let trades = trades_since(&transaction, prediction_id, previous_refresh)?;

        let minimum_quote_offset = adjusted_initial_spread
            .checked_mul(settings.tick_size)
            .ok_or_else(|| {
                PredictionWorkerRepositoryError::InvalidSettings(
                    "minimum quote offset overflowed".to_owned(),
                )
            })?;
        if minimum_quote_offset > 0 {
            transaction.execute(
                "DELETE FROM prediction_levels WHERE prediction_id=?1 AND (
                    (side='yes_ask' AND price>?2 AND price<?3) OR
                    (side='yes_bid' AND price<?2 AND price>?4))",
                params![
                    prediction_id,
                    new_price,
                    new_price.saturating_add(minimum_quote_offset),
                    new_price.saturating_sub(minimum_quote_offset)
                ],
            )?;
        }
        for level in &levels {
            let existing: Option<i64> = transaction
                .query_row(
                    "SELECT level_id FROM prediction_levels
                     WHERE prediction_id=?1 AND side=?2 AND price=?3",
                    params![prediction_id, level_side(*level), level.price],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(level_id) = existing {
                transaction.execute(
                    "UPDATE prediction_levels
                     SET remaining_size=remaining_size+?1,posted_at=?2
                     WHERE level_id=?3",
                    params![level.size, now, level_id],
                )?;
            } else {
                transaction.execute(
                    "INSERT INTO prediction_levels
                         (prediction_id,side,price,remaining_size,posted_at)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        prediction_id,
                        level_side(*level),
                        level.price,
                        level.size,
                        now
                    ],
                )?;
            }
        }
        transaction.execute(
            "UPDATE predictions SET prev_price=current_price,current_price=?1,
                 last_refresh_at=?2 WHERE prediction_id=?3 AND status='open'",
            params![new_price, now, prediction_id],
        )?;
        transaction.execute(
            "INSERT INTO prediction_fair_snapshots
                 (market_id,guild_id,snapshot_at,fair_pct,reason)
             VALUES (?1,?2,?3,?4,'refresh')",
            params![prediction_id, market.guild_id, now, new_price],
        )?;
        let generation = transaction.last_insert_rowid().to_string();

        if market.thread_id.is_some() && market.embed_message_id.is_some() {
            transaction.execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)
                 ON CONFLICT(guild_id,key) DO UPDATE SET value=excluded.value",
                params![
                    market.guild_id,
                    format!("{REFRESH_EMBED_PREFIX}{prediction_id}"),
                    generation
                ],
            )?;
        }
        if let Some(thread_id) = market.thread_id.filter(|_| !trades.is_empty()) {
            let content = refresh_summary_message(old_price, new_price, &trades);
            let payload = RefreshSummaryPayload {
                prediction_id,
                thread_id,
                content,
            };
            transaction.execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)",
                params![
                    market.guild_id,
                    format!("{REFRESH_SUMMARY_PREFIX}{prediction_id}:{generation}"),
                    serde_json::to_string(&payload)?
                ],
            )?;
        }
        transaction.commit()?;
        Ok(Some(PredictionRefreshOutcome {
            prediction_id,
            guild_id: market.guild_id,
            old_price,
            new_price,
            drift,
            trade_count: trades.len(),
            generation,
        }))
    }

    pub fn pending_refresh_publications(
        &self,
    ) -> Result<Vec<PredictionRefreshPublication>, PredictionWorkerRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id,key,value FROM app_kv
             WHERE key GLOB 'prediction_refresh_embed:*'
                OR key GLOB 'prediction_refresh_summary:*'
             ORDER BY guild_id,key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut publications = Vec::new();
        for row in rows {
            let (guild_id, key, value) = row?;
            let kind = if let Some(raw_id) = key.strip_prefix(REFRESH_EMBED_PREFIX) {
                PredictionRefreshPublicationKind::MarketEmbed {
                    prediction_id: raw_id
                        .parse()
                        .map_err(|_| PredictionWorkerRepositoryError::InvalidOutbox(key.clone()))?,
                }
            } else if key.starts_with(REFRESH_SUMMARY_PREFIX) {
                let payload: RefreshSummaryPayload = serde_json::from_str(&value)?;
                PredictionRefreshPublicationKind::ThreadSummary {
                    prediction_id: payload.prediction_id,
                    thread_id: payload.thread_id,
                    content: payload.content,
                }
            } else {
                return Err(PredictionWorkerRepositoryError::InvalidOutbox(key));
            };
            publications.push(PredictionRefreshPublication {
                guild_id,
                key,
                value,
                kind,
            });
        }
        publications.sort_by_key(|publication| match &publication.kind {
            PredictionRefreshPublicationKind::ThreadSummary { .. } => {
                (publication.guild_id, 0, publication.key.clone())
            }
            PredictionRefreshPublicationKind::MarketEmbed { .. } => {
                (publication.guild_id, 1, publication.key.clone())
            }
        });
        Ok(publications)
    }

    pub fn acknowledge_refresh_publication(
        &self,
        publication: &PredictionRefreshPublication,
    ) -> Result<bool, PredictionWorkerRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "DELETE FROM app_kv WHERE guild_id=?1 AND key=?2 AND value=?3",
            params![publication.guild_id, publication.key, publication.value],
        )? == 1)
    }

    pub fn market_is_open(
        &self,
        prediction_id: i64,
    ) -> Result<bool, PredictionWorkerRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection
            .query_row(
                "SELECT status='open' FROM predictions WHERE prediction_id=?1",
                [prediction_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn market_display(
        &self,
        prediction_id: i64,
        recent_limit: i64,
    ) -> Result<Option<PredictionMarketDisplay>, PredictionWorkerRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction()?;
        let Some(market) = select_market(&transaction, prediction_id)? else {
            return Ok(None);
        };
        let book = read_book(&transaction, prediction_id)?;
        let recent_trades = recent_trades(&transaction, prediction_id, recent_limit.max(0))?;
        let volume_since_refresh = transaction.query_row(
            "SELECT COALESCE(SUM(contracts),0) FROM prediction_trades
             WHERE prediction_id=?1 AND trade_time>=?2",
            params![prediction_id, market.last_refresh_at.unwrap_or(0)],
            |row| row.get(0),
        )?;
        let fair_history = fair_history(&transaction, prediction_id, market.guild_id)?;
        transaction.commit()?;
        Ok(Some(PredictionMarketDisplay {
            snapshot: MarketSnapshot {
                market,
                book,
                recent_trades,
                viewer_position: None,
                volume_since_refresh,
            },
            fair_history,
        }))
    }

    /// Queue the latest scheduled digest once for one live guild. An empty
    /// market list still advances the durable cursor, matching Python's quiet
    /// no-op while preventing every wake from reprocessing the same slot.
    pub fn queue_digest_if_due(
        &self,
        guild_id: i64,
        slot: i64,
        now: i64,
        refresh_seconds: i64,
    ) -> Result<PredictionDigestQueueOutcome, PredictionWorkerRepositoryError> {
        if refresh_seconds <= 0 {
            return Err(PredictionWorkerRepositoryError::InvalidSettings(
                "refresh interval must be positive".to_owned(),
            ));
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = transaction
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
                params![guild_id, DIGEST_CURSOR_KEY],
                |row| row.get(0),
            )
            .optional()?;
        if previous
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .is_some_and(|cursor| cursor >= slot)
        {
            transaction.commit()?;
            return Ok(PredictionDigestQueueOutcome {
                queued: false,
                market_count: 0,
            });
        }

        let markets = digest_markets(&transaction, guild_id, now, refresh_seconds)?;
        let mut queued = false;
        if !markets.is_empty() {
            let split_banner: Option<String> = transaction
                .query_row(
                    "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
                    params![guild_id, SPLIT_ANNOUNCED_KEY],
                    |row| row.get(0),
                )
                .optional()?;
            let split_banner = split_banner.as_deref() == Some("0");
            if split_banner {
                transaction.execute(
                    "UPDATE app_kv SET value='1' WHERE guild_id=?1 AND key=?2 AND value='0'",
                    params![guild_id, SPLIT_ANNOUNCED_KEY],
                )?;
            }
            let payload = PredictionDigestPayload {
                slot,
                split_banner,
                markets,
            };
            transaction.execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)
                 ON CONFLICT(guild_id,key) DO NOTHING",
                params![
                    guild_id,
                    format!("{DIGEST_OUTBOX_PREFIX}{slot}"),
                    serde_json::to_string(&payload)?
                ],
            )?;
            queued = true;
        }
        transaction.execute(
            "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)
             ON CONFLICT(guild_id,key) DO UPDATE SET value=excluded.value",
            params![guild_id, DIGEST_CURSOR_KEY, slot.to_string()],
        )?;
        let market_count = markets_len_for_outcome(&transaction, guild_id, slot, queued)?;
        transaction.commit()?;
        Ok(PredictionDigestQueueOutcome {
            queued,
            market_count,
        })
    }

    pub fn pending_digest_publications(
        &self,
    ) -> Result<Vec<PredictionDigestPublication>, PredictionWorkerRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id,key,value FROM app_kv
             WHERE key GLOB 'prediction_digest_outbox:*'
             ORDER BY CAST(substr(key,26) AS INTEGER),guild_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut publications = Vec::new();
        for row in rows {
            let (guild_id, key, value) = row?;
            publications.push(PredictionDigestPublication {
                guild_id,
                key,
                payload: serde_json::from_str(&value)?,
                value,
            });
        }
        Ok(publications)
    }

    pub fn acknowledge_digest_publication(
        &self,
        publication: &PredictionDigestPublication,
    ) -> Result<bool, PredictionWorkerRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "DELETE FROM app_kv WHERE guild_id=?1 AND key=?2 AND value=?3",
            params![publication.guild_id, publication.key, publication.value],
        )? == 1)
    }

    pub fn digest_market_history(
        &self,
        guild_id: i64,
        prediction_id: i64,
    ) -> Result<Vec<(i64, i64)>, PredictionWorkerRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        fair_history(&connection, prediction_id, guild_id).map_err(Into::into)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct RefreshSummaryPayload {
    prediction_id: i64,
    thread_id: i64,
    content: String,
}

fn validate_refresh_settings(
    settings: &PredictionRefreshSettings,
) -> Result<(), PredictionWorkerRepositoryError> {
    if settings.refresh_seconds <= 0 {
        return Err(PredictionWorkerRepositoryError::InvalidSettings(
            "refresh interval must be positive".to_owned(),
        ));
    }
    if settings.tick_size <= 0 {
        return Err(PredictionWorkerRepositoryError::InvalidSettings(
            "tick size must be positive".to_owned(),
        ));
    }
    if settings.price_low > settings.price_high {
        return Err(PredictionWorkerRepositoryError::InvalidSettings(
            "price bounds are reversed".to_owned(),
        ));
    }
    if settings.levels_per_side > 10_000 {
        return Err(PredictionWorkerRepositoryError::InvalidSettings(
            "refresh level count is unreasonably large".to_owned(),
        ));
    }
    Ok(())
}

fn build_levels(
    fair: i64,
    spread_ticks: i64,
    settings: &PredictionRefreshSettings,
) -> Vec<NewLevel> {
    let size_per_level = settings.size_per_level.max(0);
    if size_per_level == 0 {
        return Vec::new();
    }
    let sizes = std::iter::repeat_n(size_per_level, settings.levels_per_side)
        .chain(settings.outer_level_sizes.iter().map(|size| (*size).max(0)));
    let mut levels = Vec::new();
    for (index, size) in sizes.enumerate() {
        if size == 0 {
            continue;
        }
        let Ok(index) = i64::try_from(index) else {
            break;
        };
        let offset = spread_ticks
            .saturating_add(index)
            .saturating_mul(settings.tick_size);
        let ask = fair.saturating_add(offset);
        let bid = fair.saturating_sub(offset);
        if (1..=99).contains(&ask) {
            levels.push(NewLevel::ask(ask, size));
        }
        if (1..=99).contains(&bid) {
            levels.push(NewLevel::bid(bid, size));
        }
    }
    levels
}

fn level_side(level: NewLevel) -> &'static str {
    match level.side.as_str() {
        "yes_ask" => "yes_ask",
        _ => "yes_bid",
    }
}

fn observed_mid(
    connection: &Connection,
    market: &Market,
    book: &Book,
    fade_ticks: i64,
) -> Result<f64, rusqlite::Error> {
    let since = market.last_refresh_at.unwrap_or(0);
    match (book.yes_asks.first(), book.yes_bids.first()) {
        (Some(ask), Some(bid)) => Ok((ask.0 + bid.0) as f64 / 2.0),
        (None, Some(bid)) => Ok(last_fill_price_since(
            connection,
            market.prediction_id,
            since,
            "buy_yes",
            "sell_no",
        )?
        .unwrap_or(bid.0) as f64
            + fade_ticks as f64),
        (Some(ask), None) => Ok(last_fill_price_since(
            connection,
            market.prediction_id,
            since,
            "buy_no",
            "sell_yes",
        )?
        .unwrap_or(ask.0) as f64
            - fade_ticks as f64),
        (None, None) => {
            let lifted = last_fill_price_since(
                connection,
                market.prediction_id,
                since,
                "buy_yes",
                "sell_no",
            )?;
            let hit = last_fill_price_since(
                connection,
                market.prediction_id,
                since,
                "buy_no",
                "sell_yes",
            )?;
            Ok(match (lifted, hit) {
                (Some(ask), Some(bid)) => (ask + bid) as f64 / 2.0,
                (Some(ask), None) => (ask + fade_ticks) as f64,
                (None, Some(bid)) => (bid - fade_ticks) as f64,
                (None, None) => market.current_price.unwrap_or(50) as f64,
            })
        }
    }
}

fn python_round(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if (fraction - 0.5).abs() < f64::EPSILON {
        let floor = floor as i64;
        if floor % 2 == 0 { floor } else { floor + 1 }
    } else {
        value.round() as i64
    }
}

fn last_fill_price_since(
    connection: &Connection,
    prediction_id: i64,
    since: i64,
    first_action: &str,
    second_action: &str,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT last_fill_price FROM prediction_trades
             WHERE prediction_id=?1 AND trade_time>=?2
               AND action IN (?3,?4) AND last_fill_price IS NOT NULL
             ORDER BY trade_id DESC LIMIT 1",
            params![prediction_id, since, first_action, second_action],
            |row| row.get(0),
        )
        .optional()
}

fn active_prediction_spread_delta(
    connection: &Connection,
    guild_id: i64,
    now: i64,
) -> Result<i64, rusqlite::Error> {
    Ok(connection
        .query_row(
            "SELECT CAST(COALESCE(CASE WHEN json_valid(effects)
                 THEN json_extract(effects,'$.prediction_spread_ticks_delta') END,0)
                 AS INTEGER)
             FROM economy_daily_events
             WHERE guild_id=?1 AND starts_at<=?2 AND ?2<ends_at
             ORDER BY starts_at DESC LIMIT 1",
            params![guild_id, now],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn trades_since(
    connection: &Connection,
    prediction_id: i64,
    since: i64,
) -> Result<Vec<Trade>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT trade_id,discord_id,action,contracts,jopacoins,vwap_x100,
                last_fill_price,trade_time
         FROM prediction_trades WHERE prediction_id=?1 AND trade_time>=?2
         ORDER BY trade_id ASC",
    )?;
    let rows = statement.query_map(params![prediction_id, since], trade_from_row)?;
    rows.collect()
}

fn recent_trades(
    connection: &Connection,
    prediction_id: i64,
    limit: i64,
) -> Result<Vec<Trade>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT trade_id,discord_id,action,contracts,jopacoins,vwap_x100,
                last_fill_price,trade_time
         FROM prediction_trades WHERE prediction_id=?1
         ORDER BY trade_id DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![prediction_id, limit], trade_from_row)?;
    rows.collect()
}

fn trade_from_row(row: &Row<'_>) -> Result<Trade, rusqlite::Error> {
    Ok(Trade {
        trade_id: row.get(0)?,
        discord_id: row.get(1)?,
        action: row.get(2)?,
        contracts: row.get(3)?,
        jopacoins: row.get(4)?,
        vwap_x100: row.get(5)?,
        last_fill_price: row.get(6)?,
        trade_time: row.get(7)?,
    })
}

fn refresh_summary_message(old_price: i64, new_price: i64, trades: &[Trade]) -> String {
    let total_volume = trades.iter().map(|trade| trade.contracts).sum::<i64>();
    let yes_volume = trades
        .iter()
        .filter(|trade| matches!(trade.action.as_str(), "buy_yes" | "sell_yes"))
        .map(|trade| trade.contracts)
        .sum::<i64>();
    let no_volume = total_volume - yes_volume;
    let biggest = trades
        .iter()
        .max_by_key(|trade| (trade.jopacoins.saturating_abs(), Reverse(trade.trade_id)));
    let biggest = biggest.map_or_else(String::new, |trade| {
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
        let average = (trade.vwap_x100 + 50) / 100;
        format!(
            "\n  biggest: <@{}> {verb} {} {side} @ {average}",
            trade.discord_id, trade.contracts
        )
    });
    format!(
        "📈 **Daily refresh** — volume {total_volume} contracts ({yes_volume} YES / {no_volume} NO), YES {old_price}% → {new_price}%{biggest}"
    )
}

fn select_market(
    connection: &Connection,
    prediction_id: i64,
) -> Result<Option<Market>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT prediction_id,guild_id,creator_id,question,status,outcome,
                    current_price,initial_fair,prev_price,last_refresh_at,
                    COALESCE(lp_pnl,0),created_at,resolved_at,resolved_by,
                    channel_id,thread_id,embed_message_id,channel_message_id
             FROM predictions WHERE prediction_id=?1",
            [prediction_id],
            market_from_row,
        )
        .optional()
}

fn market_from_row(row: &Row<'_>) -> Result<Market, rusqlite::Error> {
    Ok(Market {
        prediction_id: row.get(0)?,
        guild_id: row.get(1)?,
        creator_id: row.get(2)?,
        question: row.get(3)?,
        status: row.get(4)?,
        outcome: row.get(5)?,
        current_price: row.get(6)?,
        initial_fair: row.get(7)?,
        prev_price: row.get(8)?,
        last_refresh_at: row.get(9)?,
        lp_pnl: row.get(10)?,
        created_at: row.get(11)?,
        resolved_at: row.get(12)?,
        resolved_by: row.get(13)?,
        channel_id: row.get(14)?,
        thread_id: row.get(15)?,
        embed_message_id: row.get(16)?,
        channel_message_id: row.get(17)?,
    })
}

fn read_book(connection: &Connection, prediction_id: i64) -> Result<Book, rusqlite::Error> {
    let current_price = connection
        .query_row(
            "SELECT current_price FROM predictions WHERE prediction_id=?1",
            [prediction_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let mut statement = connection.prepare(
        "SELECT side,price,remaining_size FROM prediction_levels
         WHERE prediction_id=?1 AND remaining_size>0",
    )?;
    let rows = statement.query_map([prediction_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut asks = Vec::new();
    let mut bids = Vec::new();
    for row in rows {
        let (side, price, size) = row?;
        match side.as_str() {
            "yes_ask" => asks.push((price, size)),
            "yes_bid" => bids.push((price, size)),
            _ => {}
        }
    }
    asks.sort_by_key(|level| level.0);
    bids.sort_by_key(|level| Reverse(level.0));
    Ok(Book {
        current_price,
        yes_asks: asks,
        yes_bids: bids,
    })
}

fn fair_history(
    connection: &Connection,
    prediction_id: i64,
    guild_id: i64,
) -> Result<Vec<(i64, i64)>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT snapshot_at,fair_pct FROM prediction_fair_snapshots
         WHERE market_id=?1 AND guild_id=?2 ORDER BY snapshot_at",
    )?;
    let rows = statement.query_map(params![prediction_id, guild_id], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    rows.collect()
}

fn digest_markets(
    connection: &Connection,
    guild_id: i64,
    now: i64,
    refresh_seconds: i64,
) -> Result<Vec<PredictionDigestMarket>, rusqlite::Error> {
    let cutoff = now.saturating_sub(refresh_seconds);
    let mut statement = connection.prepare(
        "SELECT p.prediction_id,p.question,p.current_price,p.prev_price,p.guild_id,
                p.created_at,p.thread_id,p.embed_message_id,
                CAST(COALESCE((SELECT SUM(contracts) FROM prediction_trades
                    WHERE prediction_id=p.prediction_id AND trade_time>=?1),0) AS INTEGER)
         FROM predictions p WHERE p.guild_id=?2 AND p.status='open'
         ORDER BY 9 DESC,p.created_at DESC",
    )?;
    let rows = statement.query_map(params![cutoff, guild_id], |row| {
        Ok(PredictionDigestMarket {
            prediction_id: row.get(0)?,
            question: row.get(1)?,
            current_price: row.get(2)?,
            prev_price: row.get(3)?,
            guild_id: row.get(4)?,
            created_at: row.get(5)?,
            thread_id: row.get(6)?,
            embed_message_id: row.get(7)?,
            volume_recent: row.get(8)?,
        })
    })?;
    rows.collect()
}

fn markets_len_for_outcome(
    transaction: &Transaction<'_>,
    guild_id: i64,
    slot: i64,
    queued: bool,
) -> Result<usize, PredictionWorkerRepositoryError> {
    if !queued {
        return Ok(0);
    }
    let value: String = transaction.query_row(
        "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
        params![guild_id, format!("{DIGEST_OUTBOX_PREFIX}{slot}")],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str::<PredictionDigestPayload>(&value)?
        .markets
        .len())
}

#[cfg(test)]
#[path = "prediction_workers/tests.rs"]
mod tests;
