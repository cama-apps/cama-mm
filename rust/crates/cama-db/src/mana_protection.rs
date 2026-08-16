//! Retroactive White-mana Guardian reconciliation.
//!
//! A daily Plains claim may be made after hostile losses already happened in
//! the same four-AM mana day. Python reconciles those uncovered losses under
//! one immediate transaction, credits the player through the economy ledger,
//! consumes the Guardian pool, and records one idempotency row per hostile
//! event. This focused adapter owns that exact existing-schema transition.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::json;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Debug, Error)]
pub enum ManaProtectionRepositoryError {
    #[error("retroactive Guardian recipient {0} is not registered")]
    PlayerNotFound(i64),
    #[error("mana protection SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct ManaProtectionRepository {
    path: PathBuf,
}

impl ManaProtectionRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn reconcile_guardian(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        since_timestamp: i64,
        mana_date: &str,
        now: i64,
    ) -> Result<i64, ManaProtectionRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let refunded = reconcile_one(
            &transaction,
            discord_id,
            guild_id.unwrap_or_default(),
            since_timestamp,
            mana_date,
            now,
        )?;
        transaction.commit()?;
        Ok(refunded)
    }

    /// Reconcile an ordered batch under one write lock. Each recipient is
    /// isolated by a savepoint; a malformed recipient is omitted while the
    /// remaining claims commit, matching Python's `dict[id] = Exception`
    /// result shape as consumed by the mana service.
    pub fn reconcile_guardians(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
        since_timestamp: i64,
        mana_date: &str,
        now: i64,
    ) -> Result<BTreeMap<i64, i64>, ManaProtectionRepositoryError> {
        let mut seen = BTreeSet::new();
        let unique_ids = discord_ids
            .iter()
            .copied()
            .filter(|discord_id| seen.insert(*discord_id))
            .collect::<Vec<_>>();
        if unique_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let guild_id = guild_id.unwrap_or_default();
        let mut refunds = BTreeMap::new();
        for discord_id in unique_ids {
            transaction.execute_batch("SAVEPOINT guardian_recipient")?;
            match reconcile_one(
                &transaction,
                discord_id,
                guild_id,
                since_timestamp,
                mana_date,
                now,
            ) {
                Ok(refunded) => {
                    transaction.execute_batch("RELEASE SAVEPOINT guardian_recipient")?;
                    refunds.insert(discord_id, refunded);
                }
                Err(_) => {
                    transaction.execute_batch(
                        "ROLLBACK TO SAVEPOINT guardian_recipient;
                         RELEASE SAVEPOINT guardian_recipient",
                    )?;
                }
            }
        }
        transaction.commit()?;
        Ok(refunds)
    }
}

#[derive(Clone, Debug)]
struct RetroEvent {
    event_id: i64,
    event_key: String,
    kind: String,
    applied: i64,
    retro_covered: i64,
}

fn reconcile_one(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
    since_timestamp: i64,
    mana_date: &str,
    now: i64,
) -> Result<i64, ManaProtectionRepositoryError> {
    let mut capacity = connection
        .query_row(
            "SELECT white_shield_remaining
             FROM player_mana
             WHERE discord_id = ?1 AND guild_id = ?2
               AND current_land = 'Plains' AND assigned_date = ?3
               AND consumed_today = 0",
            params![discord_id, guild_id, mana_date],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
        .unwrap_or_default();
    if capacity <= 0 {
        return Ok(0);
    }

    let pool_key = format!("guardian:{mana_date}");
    let events = {
        let mut statement = connection.prepare(
            "SELECT h.event_id, h.event_key, h.kind, h.applied, h.retro_covered
             FROM hostile_loss_events h
             WHERE h.guild_id = ?1 AND h.victim_id = ?2
               AND h.shieldable = 1
               AND h.occurred_at >= ?3 AND h.occurred_at <= ?4
               AND h.applied > h.retro_covered
               AND NOT EXISTS (
                   SELECT 1 FROM mana_protection_events p
                   WHERE p.hostile_loss_event_id = h.event_id
                     AND p.pool_key = ?5
               )
             ORDER BY h.occurred_at ASC, h.event_id ASC",
        )?;
        statement
            .query_map(
                params![guild_id, discord_id, since_timestamp, now, pool_key],
                |row| {
                    Ok(RetroEvent {
                        event_id: row.get(0)?,
                        event_key: row.get(1)?,
                        kind: row.get(2)?,
                        applied: row.get(3)?,
                        retro_covered: row.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut refunded = 0_i64;
    for event in events {
        let uncovered = event.applied - event.retro_covered;
        let rate_absorption = (uncovered / 2).max(1);
        let amount = capacity.min(rate_absorption);
        if amount <= 0 {
            continue;
        }
        let capacity_after = capacity - amount;
        credit_retro_event(
            connection,
            &event,
            guild_id,
            discord_id,
            amount,
            capacity,
            capacity_after,
            &pool_key,
            now,
        )?;
        capacity = capacity_after;
        refunded += amount;
        if capacity <= 0 {
            break;
        }
    }

    connection.execute(
        "UPDATE player_mana
         SET white_shield_remaining = ?1, updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3",
        params![capacity, discord_id, guild_id],
    )?;
    Ok(refunded)
}

#[allow(clippy::too_many_arguments)]
fn credit_retro_event(
    connection: &Connection,
    event: &RetroEvent,
    guild_id: i64,
    discord_id: i64,
    amount: i64,
    capacity_before: i64,
    capacity_after: i64,
    pool_key: &str,
    now: i64,
) -> Result<(), ManaProtectionRepositoryError> {
    let metadata = json!({
        "event_key": event.event_key,
        "kind": event.kind,
        "amount": amount,
        "protection_type": "guardian",
        "pool_key": pool_key,
    })
    .to_string();
    set_ledger_context(
        connection,
        "mana_protection",
        "hostile_loss_event",
        event.event_id,
        "retroactive guardian reimbursement",
        &metadata,
    )?;
    let update = connection.execute(
        "UPDATE players
         SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?1,
             updated_at = CURRENT_TIMESTAMP
         WHERE discord_id = ?2 AND guild_id = ?3",
        params![amount, discord_id, guild_id],
    );
    let clear = connection.execute("DELETE FROM economy_ledger_context", []);
    let changed = update?;
    clear?;
    if changed != 1 {
        return Err(ManaProtectionRepositoryError::PlayerNotFound(discord_id));
    }

    connection.execute(
        "UPDATE hostile_loss_events
         SET retro_covered = retro_covered + ?1 WHERE event_id = ?2",
        params![amount, event.event_id],
    )?;
    connection.execute(
        "INSERT INTO mana_protection_events (
             hostile_loss_event_id, guild_id, victim_id, protection_type,
             pool_key, buff_id, amount, rate, capacity_before,
             capacity_after, retroactive, created_at, details
         ) VALUES (?1, ?2, ?3, 'guardian', ?4, NULL, ?5, 0.5, ?6, ?7, 1, ?8, ?9)",
        params![
            event.event_id,
            guild_id,
            discord_id,
            pool_key,
            amount,
            capacity_before,
            capacity_after,
            now,
            json!({ "event_key": event.event_key }).to_string(),
        ],
    )?;
    // Retroactive reconciliation is intentionally represented in the
    // normalized protection table; Python does not rewrite the hostile row's
    // original `protection_details` JSON.
    Ok(())
}

fn set_ledger_context(
    connection: &Connection,
    source: &str,
    related_type: &str,
    related_id: i64,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    connection.execute("DELETE FROM economy_ledger_context", [])?;
    connection.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, ?1, NULL, ?2, ?3, ?4, ?5)",
        params![
            source,
            related_type,
            related_id.to_string(),
            reason,
            metadata
        ],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "mana_protection/tests.rs"]
mod tests;
