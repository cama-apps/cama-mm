//! Existing-schema bankruptcy persistence and atomic economy transitions.
//!
//! Python remains the migration and live runtime authority. This adapter only
//! opens an already-migrated database through [`crate::open_runtime_connection`].
//! Bankruptcy declaration acquires `BEGIN IMMEDIATE` before validating or
//! mutating state.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cama_db_core::profit_deductions::penalty_games_remaining;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawBankruptcyState {
    pub discord_id: i64,
    pub guild_id: i64,
    pub last_bankruptcy_at: Option<i64>,
    pub penalty_games_remaining: i64,
    pub bankruptcy_count: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclarationRequest {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub fresh_start_balance: i64,
    pub cooldown_seconds: i64,
    pub penalty_games: i64,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationOutcome {
    Applied {
        debt_cleared: i64,
        new_balance: i64,
        penalty_games: i64,
        bankruptcy_count: i64,
    },
    PlayerNotFound,
    NotInDebt {
        balance: i64,
    },
    Cooldown {
        remaining_seconds: i64,
    },
}

#[derive(Debug, Error)]
pub enum BankruptcyRepositoryError {
    #[error("fresh-start balance cannot be negative: {0}")]
    InvalidFreshStart(i64),
    #[error("cooldown seconds cannot be negative: {0}")]
    InvalidCooldown(i64),
    #[error("penalty games cannot be negative: {0}")]
    InvalidPenaltyGames(i64),
    #[error("bankruptcy arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct BankruptcyRepository {
    path: PathBuf,
}

impl BankruptcyRepository {
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

    pub fn balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, BankruptcyRepositoryError> {
        let connection = self.connection()?;
        query_balance(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    pub fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<RawBankruptcyState>, BankruptcyRepositoryError> {
        let connection = self.connection()?;
        query_state(&connection, discord_id, Self::normalize_guild_id(guild_id)).map_err(Into::into)
    }

    /// Return only IDs that have persisted bankruptcy rows.
    pub fn get_bulk_states(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, RawBankruptcyState>, BankruptcyRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(guild_id);
        let unique_ids = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut states = BTreeMap::new();
        for chunk in unique_ids.iter().copied().collect::<Vec<_>>().chunks(900) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT discord_id, guild_id, last_bankruptcy_at,
                        COALESCE(penalty_games_remaining, 0),
                        COALESCE(bankruptcy_count, 0)
                   FROM bankruptcy_state
                  WHERE guild_id = ? AND discord_id IN ({placeholders})"
            );
            let mut statement = connection.prepare(&sql)?;
            let bindings = std::iter::once(guild_id).chain(chunk.iter().copied());
            let rows = statement.query_map(rusqlite::params_from_iter(bindings), state_from_row)?;
            for state in rows {
                let state = state?;
                states.insert(state.discord_id, state);
            }
        }
        Ok(states)
    }

    pub fn get_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyRepositoryError> {
        Ok(self
            .get_state(discord_id, guild_id)?
            .map_or(0, |state| state.penalty_games_remaining))
    }

    /// Atomically re-check eligibility, reset debt, and increment filing count.
    pub fn declare_atomic(
        &self,
        request: DeclarationRequest,
    ) -> Result<DeclarationOutcome, BankruptcyRepositoryError> {
        if request.fresh_start_balance < 0 {
            return Err(BankruptcyRepositoryError::InvalidFreshStart(
                request.fresh_start_balance,
            ));
        }
        if request.cooldown_seconds < 0 {
            return Err(BankruptcyRepositoryError::InvalidCooldown(
                request.cooldown_seconds,
            ));
        }
        if request.penalty_games < 0 {
            return Err(BankruptcyRepositoryError::InvalidPenaltyGames(
                request.penalty_games,
            ));
        }
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(balance) = query_balance(&transaction, request.discord_id, guild_id)? else {
            transaction.commit()?;
            return Ok(DeclarationOutcome::PlayerNotFound);
        };
        if balance >= 0 {
            transaction.commit()?;
            return Ok(DeclarationOutcome::NotInDebt { balance });
        }
        if let Some(state) = query_state(&transaction, request.discord_id, guild_id)?
            && let Some(last_bankruptcy_at) = state.last_bankruptcy_at.filter(|value| *value != 0)
        {
            let elapsed = request.now.saturating_sub(last_bankruptcy_at);
            if elapsed < request.cooldown_seconds {
                transaction.commit()?;
                return Ok(DeclarationOutcome::Cooldown {
                    remaining_seconds: request.cooldown_seconds.saturating_sub(elapsed),
                });
            }
        }
        let debt_cleared = balance
            .checked_abs()
            .ok_or(BankruptcyRepositoryError::ArithmeticOverflow)?;

        with_ledger_context(
            &transaction,
            "bankruptcy",
            request.discord_id,
            "bankruptcy fresh start balance reset",
            &format!(
                "debt_cleared={debt_cleared};fresh_start_balance={};penalty_games={}",
                request.fresh_start_balance, request.penalty_games
            ),
            || {
                transaction.execute(
                    "UPDATE players
                        SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                      WHERE discord_id = ?2 AND guild_id = ?3",
                    params![request.fresh_start_balance, request.discord_id, guild_id],
                )
            },
        )?;
        transaction.execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, last_bankruptcy_at,
                 penalty_games_remaining, bankruptcy_count, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 last_bankruptcy_at = excluded.last_bankruptcy_at,
                 penalty_games_remaining = excluded.penalty_games_remaining,
                 bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                 updated_at = CURRENT_TIMESTAMP",
            params![
                request.discord_id,
                guild_id,
                request.now,
                request.penalty_games
            ],
        )?;
        let count = query_state(&transaction, request.discord_id, guild_id)?
            .expect("state was upserted")
            .bankruptcy_count;
        transaction.commit()?;
        Ok(DeclarationOutcome::Applied {
            debt_cleared,
            new_balance: request.fresh_start_balance,
            penalty_games: request.penalty_games,
            bankruptcy_count: count,
        })
    }

    /// Repository-compatible state upsert that increments filing count.
    pub fn upsert_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        last_bankruptcy_at: i64,
        penalty_games_remaining: i64,
    ) -> Result<(), BankruptcyRepositoryError> {
        if penalty_games_remaining < 0 {
            return Err(BankruptcyRepositoryError::InvalidPenaltyGames(
                penalty_games_remaining,
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, last_bankruptcy_at,
                 penalty_games_remaining, bankruptcy_count, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 last_bankruptcy_at = excluded.last_bankruptcy_at,
                 penalty_games_remaining = excluded.penalty_games_remaining,
                 bankruptcy_count = COALESCE(bankruptcy_state.bankruptcy_count, 0) + 1,
                 updated_at = CURRENT_TIMESTAMP",
            params![
                discord_id,
                Self::normalize_guild_id(guild_id),
                last_bankruptcy_at,
                penalty_games_remaining
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn reset_cooldown_only(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        last_bankruptcy_at: i64,
        penalty_games_remaining: i64,
    ) -> Result<bool, BankruptcyRepositoryError> {
        if penalty_games_remaining < 0 {
            return Err(BankruptcyRepositoryError::InvalidPenaltyGames(
                penalty_games_remaining,
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE bankruptcy_state
                SET last_bankruptcy_at = ?1, penalty_games_remaining = ?2,
                    updated_at = CURRENT_TIMESTAMP
              WHERE discord_id = ?3 AND guild_id = ?4",
            params![
                last_bankruptcy_at,
                penalty_games_remaining,
                discord_id,
                Self::normalize_guild_id(guild_id)
            ],
        )? > 0;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn decrement_penalty_games(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<i64, BankruptcyRepositoryError> {
        Ok(self
            .decrement_penalty_games_bulk(&[discord_id], guild_id)?
            .get(&discord_id)
            .copied()
            .unwrap_or(0))
    }

    /// Adjust a persisted bankruptcy-game counter without changing the filing
    /// timestamp.  Wheel `JAILBREAK` and `EXTEND_*` outcomes use this narrow
    /// mutation; unlike `upsert_state`, it does not increment bankruptcy_count
    /// or fabricate a new bankruptcy declaration.
    pub fn adjust_penalty_games_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        delta: i64,
    ) -> Result<i64, BankruptcyRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO bankruptcy_state (
                 discord_id, guild_id, last_bankruptcy_at,
                 penalty_games_remaining, bankruptcy_count, updated_at
             ) VALUES (?1, ?2, NULL, MAX(0, ?3), 0, CURRENT_TIMESTAMP)
             ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                 penalty_games_remaining = MAX(
                     0, COALESCE(bankruptcy_state.penalty_games_remaining, 0) + ?3
                 ),
                 updated_at = CURRENT_TIMESTAMP",
            params![discord_id, guild_id, delta],
        )?;
        let remaining = penalty_games_remaining(&transaction, guild_id, discord_id)?;
        transaction.commit()?;
        Ok(remaining)
    }

    /// Decrement once per input occurrence in one guild-scoped transaction.
    pub fn decrement_penalty_games_bulk(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, BankruptcyRepositoryError> {
        if discord_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let guild_id = Self::normalize_guild_id(guild_id);
        let unique_ids = discord_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for discord_id in discord_ids {
            transaction.execute(
                "UPDATE bankruptcy_state
                    SET penalty_games_remaining = MAX(
                            0, COALESCE(penalty_games_remaining, 0) - 1
                        ),
                        updated_at = CURRENT_TIMESTAMP
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
            )?;
        }
        let mut remaining = unique_ids
            .iter()
            .copied()
            .map(|discord_id| (discord_id, 0))
            .collect::<BTreeMap<_, _>>();
        for discord_id in unique_ids {
            if let Some(state) = query_state(&transaction, discord_id, guild_id)? {
                remaining.insert(discord_id, state.penalty_games_remaining);
            }
        }
        transaction.commit()?;
        Ok(remaining)
    }
}

fn query_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0)
               FROM players WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn query_state(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<RawBankruptcyState>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT discord_id, guild_id, last_bankruptcy_at,
                    COALESCE(penalty_games_remaining, 0),
                    COALESCE(bankruptcy_count, 0)
               FROM bankruptcy_state
              WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            state_from_row,
        )
        .optional()
}

fn state_from_row(row: &rusqlite::Row<'_>) -> Result<RawBankruptcyState, rusqlite::Error> {
    Ok(RawBankruptcyState {
        discord_id: row.get(0)?,
        guild_id: row.get(1)?,
        last_bankruptcy_at: row.get(2)?,
        penalty_games_remaining: row.get(3)?,
        bankruptcy_count: row.get(4)?,
    })
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: i64,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, ?1, ?2, 'bankruptcy', ?3, ?4, ?5)",
        params![source, actor_id, actor_id, reason, metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn with_ledger_context<T>(
    transaction: &Transaction<'_>,
    source: &str,
    actor_id: i64,
    reason: &str,
    metadata: &str,
    operation: impl FnOnce() -> Result<T, rusqlite::Error>,
) -> Result<T, rusqlite::Error> {
    set_ledger_context(transaction, source, actor_id, reason, metadata)?;
    let result = operation();
    let clear = clear_ledger_context(transaction);
    match result {
        Ok(value) => {
            clear?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "bankruptcy/tests.rs"]
mod tests;
