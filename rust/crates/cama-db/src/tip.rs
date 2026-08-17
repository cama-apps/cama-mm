//! Python-compatible tip history and atomic-transfer persistence.
//!
//! Python remains the schema-migration authority during the side-by-side
//! rewrite. This module only opens an existing SQLite database and uses the
//! already-migrated `tip_transactions`, `players`, `nonprofit_fund`, and
//! economy-ledger tables. Atomic transfers acquire the same `BEGIN IMMEDIATE`
//! lock as Python so balance validation, both wallet changes, the nonprofit
//! credit, lowest-balance tracking, and trigger-generated ledger rows commit
//! or roll back together.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, params_from_iter,
};
use thiserror::Error;

use crate::open_runtime_connection;

/// One persisted Python `tip_transactions` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipTransaction {
    pub id: i64,
    pub sender_id: i64,
    pub recipient_id: i64,
    pub amount: i64,
    pub fee: i64,
    pub guild_id: i64,
    pub timestamp: i64,
}

/// Python's direction annotation for a tip involving one requested user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipDirection {
    Sent,
    Received,
}

/// A transaction returned by `get_all_tips_for_user`, including direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectedTipTransaction {
    pub tip: TipTransaction,
    pub direction: TipDirection,
}

/// One aggregate row returned by the sender and receiver leaderboards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipLeaderboardEntry {
    pub discord_id: i64,
    pub total_amount: i64,
    pub tip_count: i64,
}

/// Python's five per-user tip aggregates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserTipStats {
    pub total_sent: i64,
    pub tips_sent_count: i64,
    pub fees_paid: i64,
    pub total_received: i64,
    pub tips_received_count: i64,
}

/// Guild-scoped aggregate volume returned by Python's repository.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TipVolume {
    pub total_amount: i64,
    pub total_fees: i64,
    pub total_transactions: i64,
}

/// Result of an atomic player-to-player tip transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipTransferResult {
    pub amount: i64,
    pub fee: i64,
    pub tithe: i64,
    pub nonprofit_credit: i64,
    pub from_new_balance: i64,
    pub to_new_balance: i64,
}

#[derive(Debug, Error)]
pub enum TipRepositoryError {
    #[error("Amount must be positive.")]
    InvalidAmount,
    #[error("Fee cannot be negative.")]
    NegativeFee,
    #[error("Tithe cannot be negative.")]
    NegativeTithe,
    #[error("Sender not found.")]
    SenderNotFound,
    #[error("Recipient not found.")]
    RecipientNotFound,
    #[error(
        "Insufficient balance. You need {required} (tip: {amount}, fee: {fee}, tithe: {tithe}). You have {available}."
    )]
    InsufficientBalance {
        required: i64,
        available: i64,
        amount: i64,
        fee: i64,
        tithe: i64,
    },
    #[error("tip amount arithmetic exceeded SQLite's signed 64-bit range")]
    AmountOverflow,
    #[error("Sender debit failed after balance validation.")]
    SenderDebitFailed,
    #[error("Recipient credit failed after player validation.")]
    RecipientCreditFailed,
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("tip SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Repository for Python's existing tip and economy tables.
#[derive(Clone, Debug)]
pub struct TipRepository {
    path: PathBuf,
}

impl TipRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Convert Python's nullable guild convention to its persisted sentinel.
    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Persist an already-completed tip transaction.
    pub fn log_tip(
        &self,
        sender_id: i64,
        recipient_id: i64,
        amount: i64,
        fee: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, TipRepositoryError> {
        let timestamp = unix_timestamp()?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO tip_transactions (
                 sender_id, recipient_id, amount, fee, guild_id, timestamp
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                sender_id,
                recipient_id,
                amount,
                fee,
                Self::normalize_guild_id(guild_id),
                timestamp
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Return one sender's most recent tips from exactly one normalized guild.
    pub fn get_tips_by_sender(
        &self,
        sender_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipTransaction>, TipRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, sender_id, recipient_id, amount, fee, guild_id, timestamp
             FROM tip_transactions
             WHERE sender_id = ?1 AND guild_id = ?2
             ORDER BY timestamp DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![sender_id, Self::normalize_guild_id(guild_id), limit],
            row_to_tip,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Return one recipient's most recent tips from exactly one normalized guild.
    pub fn get_tips_by_recipient(
        &self,
        recipient_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipTransaction>, TipRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, sender_id, recipient_id, amount, fee, guild_id, timestamp
             FROM tip_transactions
             WHERE recipient_id = ?1 AND guild_id = ?2
             ORDER BY timestamp DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![recipient_id, Self::normalize_guild_id(guild_id), limit],
            row_to_tip,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Return every guild-scoped tip involving one user, newest first.
    ///
    /// A self-tip is emitted once as `Sent`, matching Python's `CASE` order.
    pub fn get_all_tips_for_user(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<DirectedTipTransaction>, TipRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, sender_id, recipient_id, amount, fee, guild_id, timestamp,
                    CASE WHEN sender_id = ?1 THEN 'sent' ELSE 'received' END
             FROM tip_transactions
             WHERE (sender_id = ?1 OR recipient_id = ?1) AND guild_id = ?2
             ORDER BY timestamp DESC",
        )?;
        let rows = statement.query_map(
            params![discord_id, Self::normalize_guild_id(guild_id)],
            |row| {
                let direction = match row.get::<_, String>(7)?.as_str() {
                    "sent" => TipDirection::Sent,
                    _ => TipDirection::Received,
                };
                Ok(DirectedTipTransaction {
                    tip: row_to_tip(row)?,
                    direction,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sum fees either across the whole database or within one explicit guild.
    ///
    /// This intentionally differs from the other nullable-guild methods:
    /// Python treats `None` here as "all guilds", not guild sentinel zero.
    pub fn get_total_fees_collected(
        &self,
        guild_id: Option<i64>,
    ) -> Result<i64, TipRepositoryError> {
        let connection = self.connection()?;
        let total = if let Some(guild_id) = guild_id {
            connection.query_row(
                "SELECT COALESCE(SUM(fee), 0)
                 FROM tip_transactions WHERE guild_id = ?1",
                params![guild_id],
                |row| row.get(0),
            )?
        } else {
            connection.query_row(
                "SELECT COALESCE(SUM(fee), 0) FROM tip_transactions",
                [],
                |row| row.get(0),
            )?
        };
        Ok(total)
    }

    /// Rank senders by amount, using the Discord ID as the stable tie-breaker.
    pub fn get_top_senders(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipLeaderboardEntry>, TipRepositoryError> {
        self.get_top_participants(guild_id, limit, ParticipantDirection::Sender)
    }

    /// Rank receivers by amount, using the Discord ID as the stable tie-breaker.
    pub fn get_top_receivers(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TipLeaderboardEntry>, TipRepositoryError> {
        self.get_top_participants(guild_id, limit, ParticipantDirection::Recipient)
    }

    fn get_top_participants(
        &self,
        guild_id: Option<i64>,
        limit: i64,
        direction: ParticipantDirection,
    ) -> Result<Vec<TipLeaderboardEntry>, TipRepositoryError> {
        let participant_column = match direction {
            ParticipantDirection::Sender => "sender_id",
            ParticipantDirection::Recipient => "recipient_id",
        };
        let sql = format!(
            "SELECT {participant_column}, SUM(amount), COUNT(*)
             FROM tip_transactions
             WHERE guild_id = ?1
             GROUP BY {participant_column}
             ORDER BY SUM(amount) DESC, {participant_column} ASC
             LIMIT ?2"
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare(&sql)?;
        let rows =
            statement.query_map(params![Self::normalize_guild_id(guild_id), limit], |row| {
                Ok(TipLeaderboardEntry {
                    discord_id: row.get(0)?,
                    total_amount: row.get(1)?,
                    tip_count: row.get(2)?,
                })
            })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Return one user's aggregate sent and received history.
    pub fn get_user_tip_stats(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<UserTipStats, TipRepositoryError> {
        let connection = self.connection()?;
        query_user_tip_stats(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    /// Return per-user aggregates using a single connection.
    ///
    /// Input IDs are de-duplicated in first-seen order before queries are
    /// chunked below SQLite's default host-parameter ceiling, matching Python.
    pub fn get_user_tip_stats_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<HashMap<i64, UserTipStats>, TipRepositoryError> {
        let mut seen = HashSet::new();
        let unique_ids = discord_ids
            .iter()
            .copied()
            .filter(|discord_id| seen.insert(*discord_id))
            .collect::<Vec<_>>();
        if unique_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut stats = unique_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, UserTipStats::default()))
            .collect::<HashMap<_, _>>();
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;

        for chunk in unique_ids.chunks(900) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sent_sql = format!(
                "SELECT sender_id, COALESCE(SUM(amount), 0), COUNT(*),
                        COALESCE(SUM(fee), 0)
                 FROM tip_transactions
                 WHERE guild_id = ? AND sender_id IN ({placeholders})
                 GROUP BY sender_id"
            );
            let received_sql = format!(
                "SELECT recipient_id, COALESCE(SUM(amount), 0), COUNT(*)
                 FROM tip_transactions
                 WHERE guild_id = ? AND recipient_id IN ({placeholders})
                 GROUP BY recipient_id"
            );
            let parameters = std::iter::once(Value::Integer(guild_id))
                .chain(chunk.iter().copied().map(Value::Integer))
                .collect::<Vec<_>>();

            {
                let mut statement = connection.prepare(&sent_sql)?;
                let rows = statement.query_map(params_from_iter(&parameters), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?;
                for row in rows {
                    let (discord_id, total_sent, tips_sent_count, fees_paid) = row?;
                    if let Some(user_stats) = stats.get_mut(&discord_id) {
                        user_stats.total_sent = total_sent;
                        user_stats.tips_sent_count = tips_sent_count;
                        user_stats.fees_paid = fees_paid;
                    }
                }
            }

            {
                let mut statement = connection.prepare(&received_sql)?;
                let rows = statement.query_map(params_from_iter(&parameters), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
                for row in rows {
                    let (discord_id, total_received, tips_received_count) = row?;
                    if let Some(user_stats) = stats.get_mut(&discord_id) {
                        user_stats.total_received = total_received;
                        user_stats.tips_received_count = tips_received_count;
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Return aggregate tip amount, fees, and transaction count for one guild.
    pub fn get_total_tip_volume(
        &self,
        guild_id: Option<i64>,
    ) -> Result<TipVolume, TipRepositoryError> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COALESCE(SUM(amount), 0), COALESCE(SUM(fee), 0), COUNT(*)
             FROM tip_transactions
             WHERE guild_id = ?1",
            params![Self::normalize_guild_id(guild_id)],
            |row| {
                Ok(TipVolume {
                    total_amount: row.get(0)?,
                    total_fees: row.get(1)?,
                    total_transactions: row.get(2)?,
                })
            },
        )?)
    }

    /// Atomically move a tip and its nonprofit fee/tithe between existing rows.
    pub fn tip_atomic(
        &self,
        guild_id: Option<i64>,
        from_discord_id: i64,
        to_discord_id: i64,
        amount: i64,
        fee: i64,
        tithe: i64,
    ) -> Result<TipTransferResult, TipRepositoryError> {
        if amount <= 0 {
            return Err(TipRepositoryError::InvalidAmount);
        }
        if fee < 0 {
            return Err(TipRepositoryError::NegativeFee);
        }
        if tithe < 0 {
            return Err(TipRepositoryError::NegativeTithe);
        }
        let nonprofit_credit = fee
            .checked_add(tithe)
            .ok_or(TipRepositoryError::AmountOverflow)?;
        let total_cost = amount
            .checked_add(nonprofit_credit)
            .ok_or(TipRepositoryError::AmountOverflow)?;
        let guild_id = Self::normalize_guild_id(guild_id);

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let from_balance = player_balance(&transaction, from_discord_id, guild_id)?
            .ok_or(TipRepositoryError::SenderNotFound)?;
        if from_balance < total_cost {
            return Err(TipRepositoryError::InsufficientBalance {
                required: total_cost,
                available: from_balance,
                amount,
                fee,
                tithe,
            });
        }
        let to_balance = player_balance(&transaction, to_discord_id, guild_id)?
            .ok_or(TipRepositoryError::RecipientNotFound)?;

        let sender_metadata = format!(
            r#"{{"amount": {amount}, "fee": {fee}, "tithe": {tithe}, "total_cost": {total_cost}}}"#
        );
        let sender_updates = execute_with_ledger_context(
            &transaction,
            LedgerContext {
                source: "tip",
                actor_id: from_discord_id,
                related_type: "player_tip",
                related_id: to_discord_id,
                reason: "tip sender debit",
                metadata: &sender_metadata,
            },
            "UPDATE players
             SET jopacoin_balance = jopacoin_balance - ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![total_cost, from_discord_id, guild_id],
        )?;
        if sender_updates != 1 {
            return Err(TipRepositoryError::SenderDebitFailed);
        }

        let recipient_metadata =
            format!(r#"{{"amount": {amount}, "fee": {fee}, "tithe": {tithe}}}"#);
        let recipient_updates = execute_with_ledger_context(
            &transaction,
            LedgerContext {
                source: "tip",
                actor_id: from_discord_id,
                related_type: "player_tip",
                related_id: to_discord_id,
                reason: "tip recipient credit",
                metadata: &recipient_metadata,
            },
            "UPDATE players
             SET jopacoin_balance = jopacoin_balance + ?1,
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, to_discord_id, guild_id],
        )?;
        if recipient_updates != 1 {
            return Err(TipRepositoryError::RecipientCreditFailed);
        }

        let new_from_balance = from_balance - total_cost;
        transaction.execute(
            "UPDATE players
             SET lowest_balance_ever = ?1
             WHERE discord_id = ?2 AND guild_id = ?3
               AND (lowest_balance_ever IS NULL OR ?1 < lowest_balance_ever)",
            params![new_from_balance, from_discord_id, guild_id],
        )?;

        if nonprofit_credit > 0 {
            let nonprofit_metadata = format!(
                r#"{{"amount": {amount}, "fee": {fee}, "tithe": {tithe}, "nonprofit_credit": {nonprofit_credit}}}"#
            );
            execute_with_ledger_context(
                &transaction,
                LedgerContext {
                    source: "tip",
                    actor_id: from_discord_id,
                    related_type: "player_tip",
                    related_id: to_discord_id,
                    reason: "tip fee and tithe reserve credit",
                    metadata: &nonprofit_metadata,
                },
                "INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                 VALUES (?1, ?2, CURRENT_TIMESTAMP)
                 ON CONFLICT(guild_id) DO UPDATE SET
                     total_collected = total_collected + excluded.total_collected,
                     updated_at = CURRENT_TIMESTAMP",
                params![guild_id, nonprofit_credit],
            )?;
        }

        transaction.commit()?;
        Ok(TipTransferResult {
            amount,
            fee,
            tithe,
            nonprofit_credit,
            from_new_balance: new_from_balance,
            to_new_balance: to_balance + amount,
        })
    }
}

#[derive(Clone, Copy)]
enum ParticipantDirection {
    Sender,
    Recipient,
}

#[derive(Clone, Copy)]
struct LedgerContext<'a> {
    source: &'a str,
    actor_id: i64,
    related_type: &'a str,
    related_id: i64,
    reason: &'a str,
    metadata: &'a str,
}

fn row_to_tip(row: &Row<'_>) -> Result<TipTransaction, rusqlite::Error> {
    Ok(TipTransaction {
        id: row.get(0)?,
        sender_id: row.get(1)?,
        recipient_id: row.get(2)?,
        amount: row.get(3)?,
        fee: row.get(4)?,
        guild_id: row.get(5)?,
        timestamp: row.get(6)?,
    })
}

fn query_user_tip_stats(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<UserTipStats, rusqlite::Error> {
    let (total_sent, tips_sent_count, fees_paid) = connection.query_row(
        "SELECT COALESCE(SUM(amount), 0), COUNT(*), COALESCE(SUM(fee), 0)
         FROM tip_transactions
         WHERE sender_id = ?1 AND guild_id = ?2",
        params![discord_id, guild_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let (total_received, tips_received_count) = connection.query_row(
        "SELECT COALESCE(SUM(amount), 0), COUNT(*)
         FROM tip_transactions
         WHERE recipient_id = ?1 AND guild_id = ?2",
        params![discord_id, guild_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(UserTipStats {
        total_sent,
        tips_sent_count,
        fees_paid,
        total_received,
        tips_received_count,
    })
}

fn player_balance(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0)
             FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn execute_with_ledger_context<P>(
    transaction: &Transaction<'_>,
    context: LedgerContext<'_>,
    sql: &str,
    parameters: P,
) -> Result<usize, rusqlite::Error>
where
    P: rusqlite::Params,
{
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            context.source,
            context.actor_id,
            context.related_type,
            context.related_id.to_string(),
            context.reason,
            context.metadata,
        ],
    )?;
    let operation_result = transaction.execute(sql, parameters);
    let clear_result = transaction.execute("DELETE FROM economy_ledger_context", []);
    clear_result?;
    operation_result
}

fn unix_timestamp() -> Result<i64, TipRepositoryError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TipRepositoryError::ClockBeforeUnixEpoch)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| TipRepositoryError::AmountOverflow)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;

    use rusqlite::{Connection, params};
    use tempfile::NamedTempFile;

    use super::{
        TipDirection, TipRepository, TipRepositoryError, TipTransferResult, TipVolume, UserTipStats,
    };

    const TEST_GUILD_ID: i64 = 123;

    static FIXTURE_DATABASE_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

    struct Fixture {
        file: NamedTempFile,
        repository: TipRepository,
    }

    impl Fixture {
        fn new() -> Self {
            let template = FIXTURE_DATABASE_TEMPLATE.get_or_init(|| {
                let file = NamedTempFile::new().expect("create temporary SQLite template");
                let connection =
                    Connection::open(file.path()).expect("open temporary SQLite template");
                connection
                    .execute_batch(
                        "PRAGMA journal_mode=WAL;
                     CREATE TABLE players (
                         discord_id          INTEGER NOT NULL,
                         guild_id            INTEGER NOT NULL DEFAULT 0,
                         discord_username    TEXT NOT NULL,
                         jopacoin_balance     INTEGER DEFAULT 3,
                         lowest_balance_ever INTEGER,
                         updated_at          TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                         PRIMARY KEY (discord_id, guild_id)
                     );
                     CREATE TABLE nonprofit_fund (
                         guild_id        INTEGER PRIMARY KEY DEFAULT 0,
                         total_collected INTEGER DEFAULT 0,
                         updated_at      TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                     );
                     CREATE TABLE tip_transactions (
                         id           INTEGER PRIMARY KEY AUTOINCREMENT,
                         sender_id    INTEGER NOT NULL,
                         recipient_id INTEGER NOT NULL,
                         amount       INTEGER NOT NULL,
                         fee          INTEGER NOT NULL,
                         guild_id     INTEGER NOT NULL DEFAULT 0,
                         timestamp    INTEGER NOT NULL,
                         FOREIGN KEY (sender_id) REFERENCES players(discord_id),
                         FOREIGN KEY (recipient_id) REFERENCES players(discord_id)
                     );
                     CREATE INDEX idx_tip_transactions_sender
                         ON tip_transactions(sender_id);
                     CREATE INDEX idx_tip_transactions_recipient
                         ON tip_transactions(recipient_id);
                     CREATE INDEX idx_tip_transactions_timestamp
                         ON tip_transactions(timestamp);
                     CREATE INDEX idx_tip_transactions_guild_sender_time
                         ON tip_transactions(guild_id, sender_id, timestamp DESC);
                     CREATE INDEX idx_tip_transactions_guild_recipient_time
                         ON tip_transactions(guild_id, recipient_id, timestamp DESC);
                     CREATE TABLE economy_ledger_entries (
                         ledger_id      INTEGER PRIMARY KEY AUTOINCREMENT,
                         guild_id       INTEGER NOT NULL,
                         account_type   TEXT NOT NULL
                             CHECK(account_type IN ('player', 'nonprofit')),
                         account_id     INTEGER,
                         delta          INTEGER NOT NULL,
                         balance_before INTEGER NOT NULL,
                         balance_after  INTEGER NOT NULL,
                         source         TEXT NOT NULL DEFAULT 'balance_update',
                         actor_id       INTEGER,
                         related_type   TEXT,
                         related_id     TEXT,
                         reason         TEXT,
                         metadata       TEXT,
                         created_at     INTEGER NOT NULL
                             DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
                     );
                     CREATE TABLE economy_ledger_context (
                         id           INTEGER PRIMARY KEY CHECK(id = 1),
                         source       TEXT,
                         actor_id     INTEGER,
                         related_type TEXT,
                         related_id   TEXT,
                         reason       TEXT,
                         metadata     TEXT
                     );
                     CREATE TRIGGER trg_economy_ledger_players_update
                     AFTER UPDATE OF jopacoin_balance ON players
                     WHEN COALESCE(OLD.jopacoin_balance, 0)
                          != COALESCE(NEW.jopacoin_balance, 0)
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                             COALESCE(NEW.jopacoin_balance, 0)
                                 - COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(OLD.jopacoin_balance, 0),
                             COALESCE(NEW.jopacoin_balance, 0),
                             COALESCE(
                                 (SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'balance_update'
                             ),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;
                     CREATE TRIGGER trg_economy_ledger_nonprofit_insert
                     AFTER INSERT ON nonprofit_fund
                     WHEN COALESCE(NEW.total_collected, 0) != 0
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'nonprofit',
                             COALESCE(NEW.guild_id, 0),
                             COALESCE(NEW.total_collected, 0), 0,
                             COALESCE(NEW.total_collected, 0),
                             COALESCE(
                                 (SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'nonprofit_insert'
                             ),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;
                     CREATE TRIGGER trg_economy_ledger_nonprofit_update
                     AFTER UPDATE OF total_collected ON nonprofit_fund
                     WHEN COALESCE(OLD.total_collected, 0)
                          != COALESCE(NEW.total_collected, 0)
                     BEGIN
                         INSERT INTO economy_ledger_entries (
                             guild_id, account_type, account_id, delta,
                             balance_before, balance_after, source, actor_id,
                             related_type, related_id, reason, metadata
                         ) VALUES (
                             COALESCE(NEW.guild_id, 0), 'nonprofit',
                             COALESCE(NEW.guild_id, 0),
                             COALESCE(NEW.total_collected, 0)
                                 - COALESCE(OLD.total_collected, 0),
                             COALESCE(OLD.total_collected, 0),
                             COALESCE(NEW.total_collected, 0),
                             COALESCE(
                                 (SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'nonprofit_update'
                             ),
                             (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                             (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                             (SELECT reason FROM economy_ledger_context WHERE id = 1),
                             (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                         );
                     END;",
                    )
                    .expect("create Python-compatible tip schema template");
                drop(connection);
                std::fs::read(file.path()).expect("read tip schema template")
            });
            let mut file = NamedTempFile::new().expect("create temporary SQLite file");
            file.as_file_mut()
                .write_all(template)
                .expect("copy tip schema template");
            let repository = TipRepository::new(file.path());
            Self { file, repository }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.file.path()).expect("open fixture database")
        }

        fn seed_player(&self, discord_id: i64, balance: i64) {
            self.seed_player_in_guild(discord_id, TEST_GUILD_ID, balance);
        }

        fn seed_player_in_guild(&self, discord_id: i64, guild_id: i64, balance: i64) {
            self.connection()
                .execute(
                    "INSERT INTO players (
                         discord_id, guild_id, discord_username, jopacoin_balance
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![discord_id, guild_id, format!("Player{discord_id}"), balance],
                )
                .expect("seed registered player");
        }

        fn seed_players(&self, discord_ids: &[i64]) {
            for discord_id in discord_ids {
                self.seed_player(*discord_id, 100);
            }
        }

        fn log(&self, sender_id: i64, recipient_id: i64, amount: i64, fee: i64) -> i64 {
            self.log_in_guild(sender_id, recipient_id, amount, fee, None)
        }

        fn log_in_guild(
            &self,
            sender_id: i64,
            recipient_id: i64,
            amount: i64,
            fee: i64,
            guild_id: Option<i64>,
        ) -> i64 {
            self.repository
                .log_tip(sender_id, recipient_id, amount, fee, guild_id)
                .expect("log tip")
        }

        fn balance(&self, discord_id: i64) -> i64 {
            self.connection()
                .query_row(
                    "SELECT jopacoin_balance FROM players
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, TEST_GUILD_ID],
                    |row| row.get(0),
                )
                .expect("read player balance")
        }

        fn lowest_balance(&self, discord_id: i64) -> Option<i64> {
            self.connection()
                .query_row(
                    "SELECT lowest_balance_ever FROM players
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, TEST_GUILD_ID],
                    |row| row.get(0),
                )
                .expect("read lowest balance")
        }

        fn nonprofit_fund(&self) -> i64 {
            self.connection()
                .query_row(
                    "SELECT COALESCE((
                         SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?1
                     ), 0)",
                    params![TEST_GUILD_ID],
                    |row| row.get(0),
                )
                .expect("read nonprofit fund")
        }
    }

    #[test]
    fn test_log_tip_creates_record() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        let tip_id = fixture
            .repository
            .log_tip(1, 2, 10, 1, Some(12_345))
            .unwrap();
        assert!(tip_id > 0);
    }

    #[test]
    fn test_guild_scoped_queries_have_composite_indexes() {
        let fixture = Fixture::new();
        let connection = fixture.connection();
        let mut index_statement = connection
            .prepare("PRAGMA index_list('tip_transactions')")
            .unwrap();
        let index_names = index_statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(index_names.contains(&"idx_tip_transactions_guild_sender_time".to_owned()));
        assert!(index_names.contains(&"idx_tip_transactions_guild_recipient_time".to_owned()));

        let columns = |index: &str| {
            let sql = format!("PRAGMA index_info('{index}')");
            let mut statement = connection.prepare(&sql).unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(2))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            columns("idx_tip_transactions_guild_sender_time"),
            ["guild_id", "sender_id", "timestamp"]
        );
        assert_eq!(
            columns("idx_tip_transactions_guild_recipient_time"),
            ["guild_id", "recipient_id", "timestamp"]
        );
    }

    #[test]
    fn test_log_tip_stores_correct_values() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        fixture.log_in_guild(1, 2, 50, 5, Some(99_999));
        let tips = fixture
            .repository
            .get_tips_by_sender(1, Some(99_999), 10)
            .unwrap();
        assert_eq!(tips.len(), 1);
        let tip = &tips[0];
        assert_eq!(tip.sender_id, 1);
        assert_eq!(tip.recipient_id, 2);
        assert_eq!(tip.amount, 50);
        assert_eq!(tip.fee, 5);
        assert_eq!(tip.guild_id, 99_999);
        assert!(tip.timestamp > 0);
    }

    #[test]
    fn test_log_tip_null_guild_id_normalized_to_zero() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        fixture.log(1, 2, 10, 1);
        assert_eq!(
            fixture.repository.get_tips_by_sender(1, None, 10).unwrap()[0].guild_id,
            0
        );
    }

    #[test]
    fn test_get_tips_by_sender_returns_multiple() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3]);
        fixture.log(1, 2, 10, 1);
        fixture.log(1, 3, 20, 2);
        fixture.log(1, 2, 30, 3);
        assert_eq!(
            fixture
                .repository
                .get_tips_by_sender(1, None, 10)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn test_get_tips_by_sender_respects_limit() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        for amount in 10..20 {
            fixture.log(1, 2, amount, 1);
        }
        assert_eq!(
            fixture
                .repository
                .get_tips_by_sender(1, None, 3)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn test_get_tips_by_sender_ordered_by_timestamp_desc() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        for (amount, fee) in [(10, 1), (20, 2), (30, 3)] {
            fixture.log(1, 2, amount, fee);
        }
        let tips = fixture.repository.get_tips_by_sender(1, None, 10).unwrap();
        assert_eq!(tips.len(), 3);
        let mut amounts = tips.iter().map(|tip| tip.amount).collect::<Vec<_>>();
        amounts.sort_unstable();
        assert_eq!(amounts, [10, 20, 30]);
        assert!(
            tips.windows(2)
                .all(|window| window[0].timestamp >= window[1].timestamp)
        );
    }

    #[test]
    fn test_get_tips_by_recipient_returns_multiple() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3]);
        fixture.log(1, 3, 10, 1);
        fixture.log(2, 3, 20, 2);
        assert_eq!(
            fixture
                .repository
                .get_tips_by_recipient(3, None, 10)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn test_get_tips_by_recipient_isolates_user() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3]);
        fixture.log(1, 2, 10, 1);
        fixture.log(1, 3, 20, 2);
        let tips = fixture
            .repository
            .get_tips_by_recipient(2, None, 10)
            .unwrap();
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].recipient_id, 2);
    }

    #[test]
    fn all_tips_for_user_labels_self_tip_once_as_sent_and_isolates_guild() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        fixture.log(1, 2, 10, 1);
        fixture.log(2, 1, 20, 2);
        fixture.log(1, 1, 30, 3);
        fixture.log_in_guild(1, 2, 999, 99, Some(222));

        let tips = fixture.repository.get_all_tips_for_user(1, None).unwrap();
        assert_eq!(tips.len(), 3);
        assert_eq!(
            tips.iter()
                .filter(|entry| entry.tip.sender_id == 1 && entry.tip.recipient_id == 1)
                .map(|entry| entry.direction)
                .collect::<Vec<_>>(),
            [TipDirection::Sent]
        );
        assert!(tips.iter().all(|entry| entry.tip.guild_id == 0));
        assert!(
            tips.iter()
                .any(|entry| entry.direction == TipDirection::Received)
        );
    }

    #[test]
    fn test_get_total_fees_collected_all_guilds() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        fixture.log_in_guild(1, 2, 100, 5, Some(111));
        fixture.log_in_guild(1, 2, 200, 10, Some(222));
        fixture.log(1, 2, 50, 3);
        assert_eq!(
            fixture.repository.get_total_fees_collected(None).unwrap(),
            18
        );
    }

    #[test]
    fn test_get_total_fees_collected_specific_guild() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        fixture.log_in_guild(1, 2, 100, 5, Some(111));
        fixture.log_in_guild(1, 2, 200, 10, Some(222));
        fixture.log_in_guild(1, 2, 50, 3, Some(111));
        assert_eq!(
            fixture
                .repository
                .get_total_fees_collected(Some(111))
                .unwrap(),
            8
        );
    }

    #[test]
    fn test_get_total_fees_collected_empty_returns_zero() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.repository.get_total_fees_collected(None).unwrap(),
            0
        );
    }

    #[test]
    fn test_get_top_senders() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3, 4]);
        fixture.log_in_guild(1, 4, 20, 2, Some(111));
        fixture.log_in_guild(1, 4, 30, 3, Some(111));
        fixture.log_in_guild(2, 4, 100, 10, Some(111));
        fixture.log_in_guild(3, 4, 25, 2, Some(111));
        let top = fixture.repository.get_top_senders(Some(111), 10).unwrap();
        assert_eq!(
            top.iter()
                .map(|row| (row.discord_id, row.total_amount, row.tip_count))
                .collect::<Vec<_>>(),
            [(2, 100, 1), (1, 50, 2), (3, 25, 1)]
        );
    }

    #[test]
    fn test_get_top_senders_respects_limit() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3, 4]);
        for (sender, amount) in [(1, 50), (2, 40), (3, 30)] {
            fixture.log_in_guild(sender, 4, amount, 5, Some(111));
        }
        assert_eq!(
            fixture
                .repository
                .get_top_senders(Some(111), 2)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn test_get_top_senders_filters_by_guild() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3]);
        fixture.log_in_guild(1, 3, 100, 10, Some(111));
        fixture.log_in_guild(2, 3, 50, 5, Some(222));
        let top = fixture.repository.get_top_senders(Some(111), 10).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].discord_id, 1);
    }

    #[test]
    fn test_top_tip_lists_use_discord_id_for_amount_ties() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3, 4]);
        fixture.log_in_guild(2, 4, 50, 5, Some(111));
        fixture.log_in_guild(1, 3, 50, 5, Some(111));
        assert_eq!(
            fixture
                .repository
                .get_top_senders(Some(111), 10)
                .unwrap()
                .iter()
                .map(|entry| entry.discord_id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            fixture
                .repository
                .get_top_receivers(Some(111), 10)
                .unwrap()
                .iter()
                .map(|entry| entry.discord_id)
                .collect::<Vec<_>>(),
            [3, 4]
        );
    }

    #[test]
    fn test_get_top_receivers() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3, 4]);
        fixture.log_in_guild(1, 2, 100, 10, Some(111));
        fixture.log_in_guild(1, 3, 30, 3, Some(111));
        fixture.log_in_guild(4, 3, 30, 3, Some(111));
        fixture.log_in_guild(1, 4, 20, 2, Some(111));
        let top = fixture.repository.get_top_receivers(Some(111), 10).unwrap();
        assert_eq!(
            top.iter()
                .map(|row| (row.discord_id, row.total_amount, row.tip_count))
                .collect::<Vec<_>>(),
            [(2, 100, 1), (3, 60, 2), (4, 20, 1)]
        );
    }

    #[test]
    fn test_get_user_tip_stats() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3]);
        fixture.log_in_guild(1, 2, 50, 5, Some(111));
        fixture.log_in_guild(1, 3, 30, 3, Some(111));
        fixture.log_in_guild(2, 1, 20, 2, Some(111));
        assert_eq!(
            fixture.repository.get_user_tip_stats(1, Some(111)).unwrap(),
            UserTipStats {
                total_sent: 80,
                tips_sent_count: 2,
                fees_paid: 8,
                total_received: 20,
                tips_received_count: 1,
            }
        );
    }

    #[test]
    fn test_get_user_tip_stats_no_history() {
        let fixture = Fixture::new();
        fixture.seed_player(1, 100);
        assert_eq!(
            fixture.repository.get_user_tip_stats(1, Some(111)).unwrap(),
            UserTipStats::default()
        );
    }

    #[test]
    fn test_get_user_tip_stats_filters_by_guild() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        fixture.log_in_guild(1, 2, 100, 10, Some(111));
        fixture.log_in_guild(1, 2, 50, 5, Some(222));
        let first = fixture.repository.get_user_tip_stats(1, Some(111)).unwrap();
        let second = fixture.repository.get_user_tip_stats(1, Some(222)).unwrap();
        assert_eq!((first.total_sent, first.tips_sent_count), (100, 1));
        assert_eq!((second.total_sent, second.tips_sent_count), (50, 1));
    }

    #[test]
    fn test_get_user_tip_stats_bulk_is_single_connection_and_guild_scoped() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3]);
        fixture.log_in_guild(1, 2, 50, 5, Some(111));
        fixture.log_in_guild(1, 3, 30, 3, Some(111));
        fixture.log_in_guild(2, 1, 20, 2, Some(111));
        fixture.log_in_guild(1, 2, 999, 99, Some(222));
        let stats = fixture
            .repository
            .get_user_tip_stats_bulk(&[2, 1, 4, 2], Some(111))
            .unwrap();
        assert_eq!(
            stats,
            HashMap::from([
                (
                    2,
                    UserTipStats {
                        total_sent: 20,
                        tips_sent_count: 1,
                        fees_paid: 2,
                        total_received: 50,
                        tips_received_count: 1,
                    }
                ),
                (
                    1,
                    UserTipStats {
                        total_sent: 80,
                        tips_sent_count: 2,
                        fees_paid: 8,
                        total_received: 20,
                        tips_received_count: 1,
                    }
                ),
                (4, UserTipStats::default()),
            ])
        );
        assert!(
            fixture
                .repository
                .get_user_tip_stats_bulk(&[], Some(111))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_get_total_tip_volume() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2, 3]);
        fixture.log_in_guild(1, 2, 100, 10, Some(111));
        fixture.log_in_guild(2, 3, 50, 5, Some(111));
        fixture.log_in_guild(3, 1, 25, 2, Some(111));
        assert_eq!(
            fixture.repository.get_total_tip_volume(Some(111)).unwrap(),
            TipVolume {
                total_amount: 175,
                total_fees: 17,
                total_transactions: 3,
            }
        );
    }

    #[test]
    fn test_get_total_tip_volume_empty() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.repository.get_total_tip_volume(Some(111)).unwrap(),
            TipVolume::default()
        );
    }

    #[test]
    fn test_get_total_tip_volume_filters_by_guild() {
        let fixture = Fixture::new();
        fixture.seed_players(&[1, 2]);
        fixture.log_in_guild(1, 2, 100, 10, Some(111));
        fixture.log_in_guild(1, 2, 50, 5, Some(222));
        let first = fixture.repository.get_total_tip_volume(Some(111)).unwrap();
        let second = fixture.repository.get_total_tip_volume(Some(222)).unwrap();
        assert_eq!((first.total_amount, first.total_transactions), (100, 1));
        assert_eq!((second.total_amount, second.total_transactions), (50, 1));
    }

    fn atomic_fixture(sender_balance: i64, recipient_balance: i64) -> Fixture {
        let fixture = Fixture::new();
        fixture.seed_player(1, sender_balance);
        fixture.seed_player(2, recipient_balance);
        fixture
    }

    fn tip(fixture: &Fixture, amount: i64, fee: i64) -> TipTransferResult {
        fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, amount, fee, 0)
            .expect("atomic tip succeeds")
    }

    #[test]
    fn test_tip_atomic_transfers_correct_amounts() {
        let fixture = atomic_fixture(100, 50);
        let result = tip(&fixture, 20, 2);
        assert_eq!(result.from_new_balance, 78);
        assert_eq!(result.to_new_balance, 70);
        assert_eq!(result.amount, 20);
        assert_eq!(result.fee, 2);
        assert_eq!((fixture.balance(1), fixture.balance(2)), (78, 70));
    }

    #[test]
    fn test_tip_atomic_credits_fee_to_nonprofit() {
        let fixture = atomic_fixture(100, 50);
        let before = fixture.nonprofit_fund();
        tip(&fixture, 20, 5);
        assert_eq!((fixture.balance(1), fixture.balance(2)), (75, 70));
        assert_eq!(fixture.nonprofit_fund() - before, 5);
    }

    #[test]
    fn test_tip_atomic_insufficient_funds_raises() {
        let fixture = atomic_fixture(10, 50);
        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, 10, 1, 0)
            .expect_err("total cost exceeds sender balance");
        assert!(matches!(
            error,
            TipRepositoryError::InsufficientBalance {
                required: 11,
                available: 10,
                ..
            }
        ));
        assert!(error.to_string().contains("Insufficient"));
    }

    #[test]
    fn test_tip_atomic_sender_not_found_raises() {
        let fixture = Fixture::new();
        fixture.seed_player(2, 50);
        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 999, 2, 10, 1, 0)
            .expect_err("sender is absent");
        assert!(matches!(error, TipRepositoryError::SenderNotFound));
        assert!(error.to_string().contains("Sender"));
    }

    #[test]
    fn test_tip_atomic_recipient_not_found_raises() {
        let fixture = Fixture::new();
        fixture.seed_player(1, 100);
        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 999, 10, 1, 0)
            .expect_err("recipient is absent");
        assert!(matches!(error, TipRepositoryError::RecipientNotFound));
        assert!(error.to_string().contains("Recipient"));
    }

    #[test]
    fn test_tip_atomic_negative_amount_raises() {
        let fixture = atomic_fixture(100, 50);
        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, -10, 1, 0)
            .expect_err("negative amount");
        assert!(matches!(error, TipRepositoryError::InvalidAmount));
        assert!(error.to_string().contains("positive"));
    }

    #[test]
    fn test_tip_atomic_zero_amount_raises() {
        let fixture = atomic_fixture(100, 50);
        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, 0, 1, 0)
            .expect_err("zero amount");
        assert!(matches!(error, TipRepositoryError::InvalidAmount));
        assert!(error.to_string().contains("positive"));
    }

    #[test]
    fn test_tip_atomic_negative_fee_raises() {
        let fixture = atomic_fixture(100, 50);
        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, 10, -1, 0)
            .expect_err("negative fee");
        assert!(matches!(error, TipRepositoryError::NegativeFee));
        assert!(error.to_string().contains("negative"));
    }

    #[test]
    fn test_tip_atomic_zero_fee_allowed() {
        let fixture = atomic_fixture(100, 50);
        let result = tip(&fixture, 10, 0);
        assert_eq!(result.from_new_balance, 90);
        assert_eq!(result.to_new_balance, 60);
        assert_eq!(result.fee, 0);
        assert_eq!(fixture.nonprofit_fund(), 0);
    }

    #[test]
    fn test_tip_atomic_exact_balance() {
        let fixture = atomic_fixture(11, 0);
        let result = tip(&fixture, 10, 1);
        assert_eq!(result.from_new_balance, 0);
        assert_eq!(result.to_new_balance, 10);
        assert_eq!((fixture.balance(1), fixture.balance(2)), (0, 10));
    }

    #[test]
    fn test_tip_atomic_is_atomic_on_failure() {
        let fixture = atomic_fixture(5, 50);
        let original = (fixture.balance(1), fixture.balance(2));
        assert!(
            fixture
                .repository
                .tip_atomic(Some(TEST_GUILD_ID), 1, 2, 10, 1, 0)
                .is_err()
        );
        assert_eq!((fixture.balance(1), fixture.balance(2)), original);
        assert_eq!(fixture.nonprofit_fund(), 0);
    }

    #[test]
    fn test_fee_and_tithe_credit_nonprofit_no_burn() {
        let fixture = atomic_fixture(200, 50);
        let fund_before = fixture.nonprofit_fund();
        let result = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, 40, 5, 3)
            .expect("fee-and-tithe tip commits atomically");

        assert_eq!(result.from_new_balance, 152);
        assert_eq!(result.to_new_balance, 90);
        assert_eq!(result.nonprofit_credit, 8);
        assert_eq!(fixture.nonprofit_fund() - fund_before, 8);
        assert_eq!(
            fixture.balance(1) + fixture.balance(2) + fixture.nonprofit_fund(),
            200 + 50 + fund_before
        );
    }

    #[test]
    fn test_insufficient_balance_rolls_back_everything() {
        let fixture = atomic_fixture(10, 0);
        let before = (
            fixture.balance(1),
            fixture.balance(2),
            fixture.nonprofit_fund(),
        );
        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, 20, 5, 0)
            .expect_err("insufficient sender balance must reject the whole transfer");

        assert!(matches!(
            error,
            TipRepositoryError::InsufficientBalance { .. }
        ));
        assert_eq!(
            (
                fixture.balance(1),
                fixture.balance(2),
                fixture.nonprofit_fund(),
            ),
            before
        );
    }

    #[test]
    fn test_tip_atomic_large_amounts() {
        let fixture = atomic_fixture(1_000_000, 0);
        let result = tip(&fixture, 500_000, 50_000);
        assert_eq!(result.from_new_balance, 450_000);
        assert_eq!(result.to_new_balance, 500_000);
    }

    #[test]
    fn test_tip_atomic_tracks_lowest_balance() {
        let fixture = atomic_fixture(100, 50);
        tip(&fixture, 90, 5);
        assert_eq!(fixture.balance(1), 5);
        assert!(fixture.lowest_balance(1).is_some_and(|lowest| lowest <= 5));
    }

    #[test]
    fn atomic_tip_writes_contextual_player_and_nonprofit_ledger_rows() {
        let fixture = atomic_fixture(100, 50);
        fixture
            .connection()
            .execute("DELETE FROM economy_ledger_entries", [])
            .unwrap();
        tip(&fixture, 20, 2);

        let connection = fixture.connection();
        let mut statement = connection
            .prepare(
                "SELECT account_type, account_id, delta, balance_before, balance_after,
                        source, actor_id, related_type, related_id, reason, metadata
                 FROM economy_ledger_entries ORDER BY ledger_id",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            (
                "player".to_owned(),
                1,
                -22,
                100,
                78,
                "tip".to_owned(),
                1,
                "player_tip".to_owned(),
                "2".to_owned(),
                "tip sender debit".to_owned(),
                r#"{"amount": 20, "fee": 2, "tithe": 0, "total_cost": 22}"#.to_owned(),
            )
        );
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].2),
            ("player", 2, 20)
        );
        assert_eq!(
            (rows[2].0.as_str(), rows[2].1, rows[2].2),
            ("nonprofit", TEST_GUILD_ID, 2)
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn failure_after_sender_debit_rolls_back_balances_fund_and_ledger() {
        let fixture = atomic_fixture(100, 50);
        fixture
            .connection()
            .execute_batch(
                "DELETE FROM economy_ledger_entries;
                 CREATE TRIGGER fail_tip_recipient_credit
                 BEFORE UPDATE OF jopacoin_balance ON players
                 WHEN OLD.discord_id = 2
                 BEGIN
                     SELECT RAISE(ABORT, 'forced recipient failure');
                 END;",
            )
            .unwrap();

        let error = fixture
            .repository
            .tip_atomic(Some(TEST_GUILD_ID), 1, 2, 20, 2, 0)
            .expect_err("recipient trigger aborts transaction");
        assert!(matches!(error, TipRepositoryError::Sqlite(_)));
        assert!(error.to_string().contains("forced recipient failure"));
        assert_eq!((fixture.balance(1), fixture.balance(2)), (100, 50));
        assert_eq!(fixture.nonprofit_fund(), 0);
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn concurrent_tips_serialize_and_cannot_overspend_sender() {
        let fixture = atomic_fixture(100, 0);
        let repository = Arc::new(fixture.repository.clone());
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    repository.tip_atomic(Some(TEST_GUILD_ID), 1, 2, 60, 1, 0)
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("tip thread"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(TipRepositoryError::InsufficientBalance { .. })
                ))
                .count(),
            1
        );
        assert_eq!((fixture.balance(1), fixture.balance(2)), (39, 60));
        assert_eq!(fixture.nonprofit_fund(), 1);
    }
}
