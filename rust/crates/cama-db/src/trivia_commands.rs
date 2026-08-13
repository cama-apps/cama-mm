//! Existing-schema persistence for the classic `/trivia` command.
//!
//! Python remains the schema and runtime authority. This repository opens
//! only an existing database through [`crate::open_runtime_connection`].
//! Cooldown claims and all writes use `BEGIN IMMEDIATE` so concurrent command
//! invocations cannot both claim the same cooldown window.

use std::path::{Path, PathBuf};

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

    /// Credit one settled trivia answer with the same trigger context emitted
    /// by Python's `PlayerService.adjust_balance` call. Positive trivia rewards
    /// do not alter `lowest_balance_ever`.
    pub fn credit_trivia_reward(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        amount: i64,
        question_number: i64,
    ) -> Result<i64, TriviaCommandsRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM economy_ledger_context", [])?;
        transaction.execute(
            "INSERT INTO economy_ledger_context (
                 id,source,actor_id,related_type,related_id,reason,metadata
             ) VALUES (1,'trivia',?1,'trivia_answer',?2,
                       'scaled trivia reward',?3)",
            params![
                discord_id,
                question_number.to_string(),
                format!(r#"{{"adjusted_reward": {amount}}}"#),
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3",
            params![amount, discord_id, guild_id],
        );
        let clear = transaction.execute("DELETE FROM economy_ledger_context", []);
        let changed = match changed {
            Ok(changed) => {
                clear?;
                changed
            }
            Err(error) => {
                let _ = clear;
                return Err(error.into());
            }
        };
        if changed == 0 {
            return Err(TriviaCommandsRepositoryError::MissingPlayer {
                discord_id,
                guild_id,
            });
        }
        let balance = transaction.query_row(
            "SELECT COALESCE(jopacoin_balance,0)
             FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(balance)
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

#[cfg(test)]
#[path = "trivia_commands/tests.rs"]
mod tests;
