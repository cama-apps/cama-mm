//! Existing-schema persistence for cross-player Dig event settlements.
//!
//! Python remains the production runtime and sole migration authority. This
//! adapter opens only an existing database through
//! [`crate::open_runtime_connection`]; it never creates or alters production
//! schema. A steal, its two wallet movements, central-ledger context, and both
//! Dig audit rows share one `BEGIN IMMEDIATE` transaction.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StealSettlementRequest<'a> {
    pub thief_discord_id: i64,
    pub victim_discord_id: i64,
    pub guild_id: Option<i64>,
    pub amount: i64,
    pub minimum_victim_balance: i64,
    pub event_name: &'a str,
    pub event_key: &'a str,
    pub detail: &'a str,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StealSettlement {
    Applied {
        amount: i64,
        thief_new_balance: i64,
        victim_new_balance: i64,
        applied_now: bool,
    },
    SkippedBelowThreshold {
        victim_balance: i64,
    },
    MissingThief,
    MissingVictim,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigSplashAction {
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: String,
    pub jc_delta: i64,
    pub detail: Option<String>,
}

#[derive(Debug, Error)]
pub enum DigNewEventsRepositoryError {
    #[error("steal amount must be positive: {0}")]
    InvalidAmount(i64),
    #[error("minimum victim balance cannot be negative: {0}")]
    InvalidMinimumBalance(i64),
    #[error("event key cannot be empty")]
    EmptyEventKey,
    #[error("Dig event arithmetic exceeded SQLite's signed 64-bit range")]
    AmountOverflow,
    #[error("Dig new-event SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigNewEventsRepository {
    path: PathBuf,
}

impl DigNewEventsRepository {
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

    /// Return deterministic candidates for the `random_active` selector.
    ///
    /// The live adapter may shuffle this list through injected application
    /// entropy. The query remains guild-scoped and excludes the digger.
    pub fn random_active_candidates(
        &self,
        guild_id: Option<i64>,
        digger_id: i64,
    ) -> Result<Vec<i64>, DigNewEventsRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id FROM players
             WHERE guild_id = ?1 AND discord_id != ?2
               AND last_match_date IS NOT NULL
             ORDER BY discord_id",
        )?;
        let rows = statement.query_map(
            params![Self::normalize_guild_id(guild_id), digger_id],
            |row| row.get(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, DigNewEventsRepositoryError> {
        let connection = self.connection()?;
        query_balance(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    pub fn splash_actions(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Vec<DigSplashAction>, DigNewEventsRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT actor_id, target_id, action_type, jc_delta, detail
             FROM dig_actions
             WHERE guild_id = ?1
               AND action_type IN ('splash_victim', 'splash_thief')
             ORDER BY id",
        )?;
        let rows = statement.query_map(params![Self::normalize_guild_id(guild_id)], |row| {
            Ok(DigSplashAction {
                actor_id: row.get(0)?,
                target_id: row.get(1)?,
                action_type: row.get(2)?,
                jc_delta: row.get(3)?,
                detail: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Transfer one hostile splash amount and audit both sides atomically.
    ///
    /// Repeating the same non-empty `event_key` for the same pair is
    /// idempotent: the committed balances are returned without another write.
    pub fn settle_steal_atomic(
        &self,
        request: StealSettlementRequest<'_>,
    ) -> Result<StealSettlement, DigNewEventsRepositoryError> {
        if request.amount <= 0 {
            return Err(DigNewEventsRepositoryError::InvalidAmount(request.amount));
        }
        if request.minimum_victim_balance < 0 {
            return Err(DigNewEventsRepositoryError::InvalidMinimumBalance(
                request.minimum_victim_balance,
            ));
        }
        if request.event_key.is_empty() {
            return Err(DigNewEventsRepositoryError::EmptyEventKey);
        }

        let guild_id = Self::normalize_guild_id(request.guild_id);
        let idempotency_detail = format!("{} [event_key={}]", request.detail, request.event_key);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let duplicate_amount = transaction
            .query_row(
                "SELECT jc_delta FROM dig_actions
                 WHERE guild_id = ?1 AND actor_id = ?2 AND target_id = ?3
                   AND action_type = 'splash_thief' AND detail = ?4
                 ORDER BY id LIMIT 1",
                params![
                    guild_id,
                    request.thief_discord_id,
                    request.victim_discord_id,
                    &idempotency_detail
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(amount) = duplicate_amount {
            let thief_balance = query_balance(&transaction, request.thief_discord_id, guild_id)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let victim_balance = query_balance(&transaction, request.victim_discord_id, guild_id)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            transaction.commit()?;
            return Ok(StealSettlement::Applied {
                amount,
                thief_new_balance: thief_balance,
                victim_new_balance: victim_balance,
                applied_now: false,
            });
        }

        let Some(victim_balance) =
            query_balance(&transaction, request.victim_discord_id, guild_id)?
        else {
            transaction.commit()?;
            return Ok(StealSettlement::MissingVictim);
        };
        let Some(thief_balance) = query_balance(&transaction, request.thief_discord_id, guild_id)?
        else {
            transaction.commit()?;
            return Ok(StealSettlement::MissingThief);
        };
        if victim_balance < request.minimum_victim_balance {
            transaction.commit()?;
            return Ok(StealSettlement::SkippedBelowThreshold { victim_balance });
        }

        let victim_new_balance = victim_balance
            .checked_sub(request.amount)
            .ok_or(DigNewEventsRepositoryError::AmountOverflow)?;
        let thief_new_balance = thief_balance
            .checked_add(request.amount)
            .ok_or(DigNewEventsRepositoryError::AmountOverflow)?;

        set_ledger_context(
            &transaction,
            request.thief_discord_id,
            request.event_name,
            &format!("dig splash steal victim debit [{}]", request.event_key),
            &idempotency_detail,
        )?;
        transaction.execute(
            "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![victim_new_balance, request.victim_discord_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;

        set_ledger_context(
            &transaction,
            request.thief_discord_id,
            request.event_name,
            &format!("dig splash steal thief credit [{}]", request.event_key),
            &idempotency_detail,
        )?;
        transaction.execute(
            "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![thief_new_balance, request.thief_discord_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;

        insert_action(
            &transaction,
            ActionRecord {
                actor_id: request.victim_discord_id,
                target_id: request.thief_discord_id,
                guild_id,
                action_type: "splash_victim",
                jc_delta: -request.amount,
                detail: &idempotency_detail,
                now: request.now,
            },
        )?;
        insert_action(
            &transaction,
            ActionRecord {
                actor_id: request.thief_discord_id,
                target_id: request.victim_discord_id,
                guild_id,
                action_type: "splash_thief",
                jc_delta: request.amount,
                detail: &idempotency_detail,
                now: request.now,
            },
        )?;
        transaction.commit()?;
        Ok(StealSettlement::Applied {
            amount: request.amount,
            thief_new_balance,
            victim_new_balance,
            applied_now: true,
        })
    }
}

fn query_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0) FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    event_name: &str,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT OR REPLACE INTO economy_ledger_context
         (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'dig', ?1, 'splash_victim', ?2, ?3, ?4)",
        params![actor_id, event_name, reason, metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context WHERE id = 1", [])?;
    Ok(())
}

struct ActionRecord<'a> {
    actor_id: i64,
    target_id: i64,
    guild_id: i64,
    action_type: &'a str,
    jc_delta: i64,
    detail: &'a str,
    now: i64,
}

fn insert_action(
    transaction: &Transaction<'_>,
    action: ActionRecord<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_actions
         (guild_id, actor_id, target_id, action_type, depth_before,
          depth_after, jc_delta, detail, created_at)
         VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, ?6, ?7)",
        params![
            action.guild_id,
            action.actor_id,
            action.target_id,
            action.action_type,
            action.jc_delta,
            action.detail,
            action.now
        ],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_new_events_repository/tests.rs"]
mod tests;
