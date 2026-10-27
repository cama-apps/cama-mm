//! Existing-schema persistence for the classic `/trivia` command.
//!
//! Schema creation and migration belong to the startup schema manager. This repository opens
//! only an existing database through [`crate::open_runtime_connection`].
//! Cooldown claims and all writes use `BEGIN IMMEDIATE` so concurrent command
//! invocations cannot both claim the same cooldown window.

use std::path::{Path, PathBuf};

use cama_db_core::profit_deductions::{
    DeductionLedgerContext, ProfitDeductionPolicy, withhold_profit_deductions,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Debug, Error)]
pub enum TriviaCommandsRepositoryError {
    #[error("trivia SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("player {discord_id} is not registered in guild {guild_id}")]
    MissingPlayer { discord_id: i64, guild_id: i64 },
    #[error("cooldown seconds must not be negative")]
    InvalidCooldown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TriviaLeaderboardEntry {
    pub discord_id: i64,
    pub best_streak: i64,
}

/// What one settled trivia answer moved.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TriviaRewardSettlement {
    /// Bankruptcy, vanity, and low-priority shares withheld from the gross.
    pub withheld: i64,
    /// Plains tithe moved to the nonprofit reserve.
    pub tithe: i64,
    /// Net JC the player kept.
    pub reward: i64,
    /// Player balance after settlement; `None` when nothing was credited.
    pub balance: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct TriviaCommandsRepository {
    path: PathBuf,
}

impl TriviaCommandsRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

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

    pub fn get_last_trivia_session(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, TriviaCommandsRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT last_trivia_session
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(Into::into)
    }

    pub fn try_claim_trivia_session(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
        cooldown_seconds: i64,
    ) -> Result<bool, TriviaCommandsRepositoryError> {
        if cooldown_seconds < 0 {
            return Err(TriviaCommandsRepositoryError::InvalidCooldown);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE players
             SET last_trivia_session = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3
               AND (last_trivia_session IS NULL OR last_trivia_session < ?4)",
            params![
                now,
                discord_id,
                Self::normalize_guild_id(guild_id),
                now - cooldown_seconds
            ],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub fn reset_trivia_cooldown(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, TriviaCommandsRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE players SET last_trivia_session = NULL,
                    updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?1 AND guild_id = ?2
               AND last_trivia_session IS NOT NULL",
            params![discord_id, Self::normalize_guild_id(guild_id)],
        )?;
        transaction.commit()?;
        Ok(changed > 0)
    }

    pub fn get_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, TriviaCommandsRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0)
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn adjust_balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
    ) -> Result<i64, TriviaCommandsRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE players SET
                 jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                 lowest_balance_ever = MIN(
                     COALESCE(lowest_balance_ever, COALESCE(jopacoin_balance, 0) + ?1),
                     COALESCE(jopacoin_balance, 0) + ?1
                 ),
                 updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![amount, discord_id, guild_id],
        )?;
        if changed == 0 {
            return Err(TriviaCommandsRepositoryError::MissingPlayer {
                discord_id,
                guild_id,
            });
        }
        let balance = transaction.query_row(
            "SELECT COALESCE(jopacoin_balance, 0)
             FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(balance)
    }

    /// Settle one correct answer in a single `BEGIN IMMEDIATE` transaction:
    /// credit the gross reward, withhold the shared profit deductions as their
    /// own ledger rows, then move the Plains tithe (a share of what the player
    /// keeps) from the player to the nonprofit reserve. Any failure rolls the
    /// whole settlement back, so the tithe can never be booked without the
    /// reward. Positive trivia rewards do not alter `lowest_balance_ever`.
    pub fn settle_trivia_reward(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        gross: i64,
        question_number: i64,
        deduction_policy: &ProfitDeductionPolicy<'_>,
        tithe_rate: f64,
    ) -> Result<TriviaRewardSettlement, TriviaCommandsRepositoryError> {
        if gross <= 0 {
            return Ok(TriviaRewardSettlement::default());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let missing = || TriviaCommandsRepositoryError::MissingPlayer {
            discord_id,
            guild_id,
        };
        let changed = with_trivia_context(
            &transaction,
            discord_id,
            "trivia_answer",
            &question_number.to_string(),
            "scaled trivia reward",
            &format!(r#"{{"adjusted_reward": {gross}}}"#),
            || {
                transaction.execute(
                    "UPDATE players
                     SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                         updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3",
                    params![gross, discord_id, guild_id],
                )
            },
        )?;
        if changed == 0 {
            return Err(missing());
        }
        let deductions = withhold_profit_deductions(
            &transaction,
            guild_id,
            discord_id,
            gross,
            deduction_policy,
            &DeductionLedgerContext {
                source: "trivia",
                related_type: Some("trivia_answer"),
                related_id: Some(question_number),
                reason: "trivia reward",
            },
        )?
        .ok_or_else(missing)?;
        let kept = deductions.net();
        let tithe = if tithe_rate > 0.0 && kept > 1 {
            let tithe = ((kept as f64 * tithe_rate) as i64).max(1);
            if tithe < kept { tithe } else { 0 }
        } else {
            0
        };
        if tithe > 0 {
            let metadata = format!(r#"{{"tithe": {tithe}, "net_jc_before_tithe": {kept}}}"#);
            with_trivia_context(
                &transaction,
                discord_id,
                "plains_tithe",
                &question_number.to_string(),
                "trivia plains tithe reserve credit",
                &metadata,
                || {
                    transaction.execute(
                        "UPDATE players
                         SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                             updated_at=CURRENT_TIMESTAMP
                         WHERE discord_id=?2 AND guild_id=?3",
                        params![tithe, discord_id, guild_id],
                    )?;
                    transaction.execute(
                        "INSERT INTO nonprofit_fund (guild_id, total_collected, updated_at)
                         VALUES (?1, ?2, CURRENT_TIMESTAMP)
                         ON CONFLICT(guild_id) DO UPDATE SET
                             total_collected = COALESCE(nonprofit_fund.total_collected, 0)
                                 + excluded.total_collected,
                             updated_at = CURRENT_TIMESTAMP",
                        params![guild_id, tithe],
                    )
                },
            )?;
        }
        let balance = transaction.query_row(
            "SELECT COALESCE(jopacoin_balance,0)
             FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(TriviaRewardSettlement {
            withheld: deductions.total(),
            tithe,
            reward: kept - tithe,
            balance: Some(balance),
        })
    }

    pub fn record_trivia_session(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        streak: i64,
        jc_earned: i64,
        played_at: i64,
    ) -> Result<i64, TriviaCommandsRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO trivia_sessions (
                 discord_id, guild_id, streak, jc_earned, played_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                streak,
                jc_earned,
                played_at
            ],
        )?;
        let session_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(session_id)
    }

    pub fn get_trivia_leaderboard(
        &self,
        guild_id: Option<i64>,
        since_timestamp: i64,
        limit: usize,
    ) -> Result<Vec<TriviaLeaderboardEntry>, TriviaCommandsRepositoryError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id, MAX(streak) AS best_streak
             FROM trivia_sessions
             WHERE guild_id = ?1 AND played_at >= ?2
             GROUP BY discord_id
             ORDER BY best_streak DESC, discord_id ASC
             LIMIT ?3",
        )?;
        statement
            .query_map(
                params![
                    Self::normalize_guild_id(guild_id),
                    since_timestamp,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |row| {
                    Ok(TriviaLeaderboardEntry {
                        discord_id: row.get(0)?,
                        best_streak: row.get(1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// Run `operation` under a `trivia` ledger context, clearing it afterwards
/// even when the operation fails.
fn with_trivia_context<T>(
    connection: &Connection,
    actor_id: i64,
    related_type: &str,
    related_id: &str,
    reason: &str,
    metadata: &str,
    operation: impl FnOnce() -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    connection.execute(
        "INSERT INTO economy_ledger_context (
             id,source,actor_id,related_type,related_id,reason,metadata
         ) VALUES (1,'trivia',?1,?2,?3,?4,?5)",
        params![actor_id, related_type, related_id, reason, metadata],
    )?;
    let outcome = operation();
    let cleared = connection.execute("DELETE FROM economy_ledger_context", []);
    let value = outcome?;
    cleared?;
    Ok(value)
}

#[cfg(test)]
#[path = "trivia_commands/tests.rs"]
mod tests;
