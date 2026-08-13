//! Existing-schema persistence for rare Dig bonus events.
//!
//! Python remains the schema and live runtime authority. This repository only
//! opens an already-migrated database through [`crate::open_runtime_connection`].
//! Trivia settlement installs the audit context and changes the player balance
//! inside one `BEGIN IMMEDIATE` transaction so the existing Python trigger
//! emits a matching economy-ledger entry.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BalanceAdjustmentRequest<'a> {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub delta: i64,
    pub source: &'a str,
    pub actor_id: i64,
    pub related_type: &'a str,
    pub related_id: &'a str,
    pub reason: &'a str,
    /// Already-serialized JSON, matching the Python repository boundary.
    pub metadata_json: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BalanceAdjustment {
    pub balance_before: i64,
    pub delta: i64,
    pub balance_after: i64,
}

#[derive(Debug, Error)]
pub enum DigBonusRepositoryError {
    #[error("player {discord_id} was not found in guild {guild_id}")]
    PlayerNotFound { discord_id: i64, guild_id: i64 },
    #[error("Dig bonus balance arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigBonusRepository {
    path: PathBuf,
}

impl DigBonusRepository {
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

    /// Return detached IDs active on or after the caller-supplied ISO cutoff.
    pub fn recently_active_player_ids(
        &self,
        guild_id: Option<i64>,
        cutoff_iso: &str,
    ) -> Result<Vec<i64>, DigBonusRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id
               FROM players
              WHERE guild_id = ?1
                AND last_match_date IS NOT NULL
                AND last_match_date >= ?2",
        )?;
        statement
            .query_map(
                params![Self::normalize_guild_id(guild_id), cutoff_iso],
                |row| row.get(0),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Atomically apply one signed delta and let the existing ledger trigger
    /// copy the installed context into `economy_ledger_entries`.
    pub fn adjust_balance_atomic(
        &self,
        request: BalanceAdjustmentRequest<'_>,
    ) -> Result<BalanceAdjustment, DigBonusRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let balance_before = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0)
                   FROM players
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![request.discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => DigBonusRepositoryError::PlayerNotFound {
                    discord_id: request.discord_id,
                    guild_id,
                },
                other => DigBonusRepositoryError::Sqlite(other),
            })?;
        let balance_after = balance_before
            .checked_add(request.delta)
            .ok_or(DigBonusRepositoryError::ArithmeticOverflow)?;

        set_ledger_context(&transaction, request)?;
        let changed = transaction.execute(
            "UPDATE players
                SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
              WHERE discord_id = ?2 AND guild_id = ?3",
            params![balance_after, request.discord_id, guild_id],
        )?;
        if changed != 1 {
            return Err(DigBonusRepositoryError::PlayerNotFound {
                discord_id: request.discord_id,
                guild_id,
            });
        }
        clear_ledger_context(&transaction)?;
        transaction.commit()?;
        Ok(BalanceAdjustment {
            balance_before,
            delta: request.delta,
            balance_after,
        })
    }
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: BalanceAdjustmentRequest<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            request.source,
            request.actor_id,
            request.related_type,
            request.related_id,
            request.reason,
            request.metadata_json,
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_bonus_events/tests.rs"]
mod tests;
