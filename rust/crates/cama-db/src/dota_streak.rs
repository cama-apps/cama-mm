//! Existing-schema persistence for Dota daily-play streaks.
//!
//! Python remains the schema, migration, and live runtime authority. This
//! adapter opens only an already-migrated database through
//! [`crate::open_runtime_connection`]. A bulk advance holds one
//! `BEGIN IMMEDIATE` transaction so concurrent match finalizations can advance
//! a game day only once. Ordered award credits use a separate immediate
//! transaction, matching Python's streak-then-credit orchestration and keeping
//! duplicate participant ledger entries distinct.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DotaStreakState {
    pub days: i64,
    pub last_played_date: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DotaStreakCredit {
    pub discord_id: i64,
    pub streak_days: i64,
    pub bonus: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditedDotaStreak {
    pub discord_id: i64,
    pub bonus: i64,
    pub balance_after: i64,
    /// False when an exact-once match credit was already present and this
    /// call reconstructed it without moving the balance again.
    pub applied: bool,
}

#[derive(Debug, Error)]
pub enum DotaStreakRepositoryError {
    #[error("Dota streak SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Dota streak arithmetic exceeded SQLite's signed 64-bit range")]
    AmountOverflow,
    #[error("Dota streak bonus must be non-negative")]
    NegativeBonus,
    #[error("player {discord_id} does not exist in guild {guild_id}")]
    MissingPlayer { discord_id: i64, guild_id: i64 },
}

#[derive(Clone, Debug)]
pub struct DotaStreakRepository {
    path: PathBuf,
}

impl DotaStreakRepository {
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

    /// Advance each distinct existing player once and align results to input.
    pub fn advance_bulk_atomic(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        today: &str,
        yesterday: &str,
    ) -> Result<Vec<i64>, DotaStreakRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(Vec::new());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let unique_ids = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut results = BTreeMap::new();

        for discord_id in unique_ids {
            let state = transaction
                .query_row(
                    "SELECT COALESCE(dota_streak_days, 0), dota_last_played_date
                     FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                    params![discord_id, guild_id],
                    |row| {
                        Ok(DotaStreakState {
                            days: row.get(0)?,
                            last_played_date: row.get(1)?,
                        })
                    },
                )
                .optional()?;
            let Some(state) = state else {
                results.insert(discord_id, 0);
                continue;
            };
            let next_days = if state.last_played_date.as_deref() == Some(today) {
                state.days
            } else if state.last_played_date.as_deref() == Some(yesterday) {
                state
                    .days
                    .checked_add(1)
                    .ok_or(DotaStreakRepositoryError::AmountOverflow)?
            } else {
                1
            };
            if state.last_played_date.as_deref() != Some(today) {
                transaction.execute(
                    "UPDATE players
                     SET dota_streak_days = ?1, dota_last_played_date = ?2,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?3 AND guild_id = ?4",
                    params![next_days, today, discord_id, guild_id],
                )?;
            }
            results.insert(discord_id, next_days);
        }
        transaction.commit()?;
        Ok(discord_ids
            .iter()
            .map(|discord_id| results.get(discord_id).copied().unwrap_or(0))
            .collect())
    }

    pub fn get_streak(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<DotaStreakState, DotaStreakRepositoryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(dota_streak_days, 0), dota_last_played_date
                 FROM players WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, Self::normalize_guild_id(guild_id)],
                |row| {
                    Ok(DotaStreakState {
                        days: row.get(0)?,
                        last_played_date: row.get(1)?,
                    })
                },
            )
            .optional()
            .map(Option::unwrap_or_default)
            .map_err(Into::into)
    }

    /// Credit ordered awards with one trigger context and ledger row per item.
    pub fn credit_awards_ordered_atomic(
        &self,
        credits: &[DotaStreakCredit],
        guild_id: Option<i64>,
    ) -> Result<Vec<CreditedDotaStreak>, DotaStreakRepositoryError> {
        if credits.is_empty() {
            return Ok(Vec::new());
        }
        if credits.iter().any(|credit| credit.bonus < 0) {
            return Err(DotaStreakRepositoryError::NegativeBonus);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcomes = Vec::with_capacity(credits.len());
        for credit in credits {
            set_ledger_context(&transaction, *credit)?;
            let balance_after = transaction
                .query_row(
                    "UPDATE players
                     SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE discord_id = ?2 AND guild_id = ?3
                     RETURNING jopacoin_balance",
                    params![credit.bonus, credit.discord_id, guild_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(DotaStreakRepositoryError::MissingPlayer {
                    discord_id: credit.discord_id,
                    guild_id,
                })?;
            clear_ledger_context(&transaction)?;
            outcomes.push(CreditedDotaStreak {
                discord_id: credit.discord_id,
                bonus: credit.bonus,
                balance_after,
                applied: true,
            });
        }
        transaction.commit()?;
        Ok(outcomes)
    }

    /// Credit one streak component per player and match exactly once.
    ///
    /// Match recovery may re-enter after the daily streak was advanced and
    /// credited but before the public result or pending-row cleanup.  The
    /// ordinary ordered primitive intentionally permits duplicate credits;
    /// this production variant uses the durable economy ledger tuple as its
    /// completion marker and reconstructs already-applied rows.
    pub fn credit_match_awards_ordered_atomic(
        &self,
        credits: &[DotaStreakCredit],
        guild_id: Option<i64>,
        match_id: i64,
    ) -> Result<Vec<CreditedDotaStreak>, DotaStreakRepositoryError> {
        if credits.is_empty() {
            return Ok(Vec::new());
        }
        if credits.iter().any(|credit| credit.bonus < 0) {
            return Err(DotaStreakRepositoryError::NegativeBonus);
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let related_id = match_id.to_string();
        let mut outcomes = Vec::with_capacity(credits.len());
        for credit in credits {
            let already_applied = transaction
                .query_row(
                    "SELECT 1 FROM economy_ledger_entries
                     WHERE guild_id=?1 AND account_type='player' AND account_id=?2
                       AND source='match_streak' AND related_type='match'
                       AND related_id=?3 LIMIT 1",
                    params![guild_id, credit.discord_id, related_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if already_applied {
                let balance_after = transaction
                    .query_row(
                        "SELECT COALESCE(jopacoin_balance,0) FROM players
                         WHERE discord_id=?1 AND guild_id=?2",
                        params![credit.discord_id, guild_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(DotaStreakRepositoryError::MissingPlayer {
                        discord_id: credit.discord_id,
                        guild_id,
                    })?;
                outcomes.push(CreditedDotaStreak {
                    discord_id: credit.discord_id,
                    bonus: credit.bonus,
                    balance_after,
                    applied: false,
                });
                continue;
            }

            set_match_ledger_context(&transaction, *credit, match_id)?;
            let balance_after = transaction
                .query_row(
                    "UPDATE players
                     SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                         updated_at=CURRENT_TIMESTAMP
                     WHERE discord_id=?2 AND guild_id=?3
                     RETURNING jopacoin_balance",
                    params![credit.bonus, credit.discord_id, guild_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(DotaStreakRepositoryError::MissingPlayer {
                    discord_id: credit.discord_id,
                    guild_id,
                })?;
            clear_ledger_context(&transaction)?;
            outcomes.push(CreditedDotaStreak {
                discord_id: credit.discord_id,
                bonus: credit.bonus,
                balance_after,
                applied: true,
            });
        }
        transaction.commit()?;
        Ok(outcomes)
    }
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    credit: DotaStreakCredit,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    let metadata = format!(
        "{{\"streak_days\":{},\"bonus\":{}}}",
        credit.streak_days, credit.bonus
    );
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'match_streak', NULL, 'dota_streak', NULL,
                 'Dota streak match bonus', ?1)",
        [metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn set_match_ledger_context(
    transaction: &Transaction<'_>,
    credit: DotaStreakCredit,
    match_id: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    let metadata = format!(
        "{{\"streak_days\":{},\"bonus\":{}}}",
        credit.streak_days, credit.bonus
    );
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,'match_streak',NULL,'match',?1,
                 'Dota streak match bonus',?2)",
        params![match_id.to_string(), metadata],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "dota_streak/tests.rs"]
mod tests;
