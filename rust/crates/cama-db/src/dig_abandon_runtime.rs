//! Existing-schema persistence for one guarded Dig tunnel abandonment.
//!
//! The refund policy stays in `cama-app`. This repository preserves Python's
//! one-transaction boundary for wallet credit, the exact partial tunnel reset,
//! the 24-hour admission check, economy-ledger context, and the Dig audit row.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

pub const ABANDON_COOLDOWN_SECONDS: i64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigAbandonKey {
    pub discord_id: i64,
    pub guild_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigAbandonSnapshot {
    pub key: DigAbandonKey,
    pub depth: i64,
    pub prestige_level: i64,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct AtomicDigAbandonSettlement<'a> {
    pub expected: DigAbandonSnapshot,
    pub refund: i64,
    pub boss_progress_json: &'a str,
    pub detail_json: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigAbandonSettlementReceipt {
    pub depth_lost: i64,
    pub prestige_level: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub audit_action_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigAbandonSettlementOutcome {
    Applied(DigAbandonSettlementReceipt),
    Conflict,
    Cooldown,
    TooShallow,
    MissingTunnel,
    MissingPlayer,
}

#[derive(Debug, Error)]
pub enum DigAbandonRuntimeRepositoryError {
    #[error("Dig abandon SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigAbandonRuntimeRepository {
    path: PathBuf,
}

impl DigAbandonRuntimeRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn snapshot(
        &self,
        key: DigAbandonKey,
    ) -> Result<Option<DigAbandonSnapshot>, DigAbandonRuntimeRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT t.depth, COALESCE(t.prestige_level, 0), p.jopacoin_balance
                   FROM tunnels t
                   JOIN players p
                     ON p.discord_id=t.discord_id AND p.guild_id=t.guild_id
                  WHERE t.discord_id=?1 AND t.guild_id=?2",
                params![key.discord_id, key.guild_id],
                |row| {
                    Ok(DigAbandonSnapshot {
                        key,
                        depth: row.get(0)?,
                        prestige_level: row.get(1)?,
                        balance: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn atomic_settle(
        &self,
        request: AtomicDigAbandonSettlement<'_>,
    ) -> Result<DigAbandonSettlementOutcome, DigAbandonRuntimeRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT t.depth, COALESCE(t.prestige_level, 0), p.jopacoin_balance
                   FROM tunnels t
                   LEFT JOIN players p
                     ON p.discord_id=t.discord_id AND p.guild_id=t.guild_id
                  WHERE t.discord_id=?1 AND t.guild_id=?2",
                params![
                    request.expected.key.discord_id,
                    request.expected.key.guild_id
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((depth, prestige_level, balance)) = current else {
            return Ok(DigAbandonSettlementOutcome::MissingTunnel);
        };
        let Some(balance_before) = balance else {
            return Ok(DigAbandonSettlementOutcome::MissingPlayer);
        };
        if depth != request.expected.depth {
            return Ok(DigAbandonSettlementOutcome::Conflict);
        }
        if depth < 10 {
            return Ok(DigAbandonSettlementOutcome::TooShallow);
        }
        let cutoff = request.created_at.saturating_sub(ABANDON_COOLDOWN_SECONDS);
        let already_abandoned = transaction
            .query_row(
                "SELECT 1 FROM dig_actions
                  WHERE guild_id=?1 AND actor_id=?2 AND action_type='abandon'
                    AND created_at>=?3
                  ORDER BY created_at DESC LIMIT 1",
                params![
                    request.expected.key.guild_id,
                    request.expected.key.discord_id,
                    cutoff
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if already_abandoned {
            return Ok(DigAbandonSettlementOutcome::Cooldown);
        }

        if request.refund != 0 {
            set_ledger_context(&transaction, request)?;
            let update = transaction.execute(
                "UPDATE players
                    SET jopacoin_balance=jopacoin_balance+?1,
                        updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3",
                params![
                    request.refund,
                    request.expected.key.discord_id,
                    request.expected.key.guild_id
                ],
            );
            let clear = clear_ledger_context(&transaction);
            if update? == 0 {
                return Ok(DigAbandonSettlementOutcome::MissingPlayer);
            }
            clear?;
        }

        let updated = transaction.execute(
            "UPDATE tunnels
                SET depth=0,
                    boss_progress=?1,
                    boss_attempts=0,
                    injury_state=NULL,
                    cheer_data=NULL,
                    streak_days=0,
                    pinnacle_boss_id=NULL,
                    pinnacle_phase=0,
                    pinnacle_hp_remaining=NULL,
                    pinnacle_last_engaged_at=NULL,
                    route_state=NULL
              WHERE discord_id=?2 AND guild_id=?3 AND depth=?4",
            params![
                request.boss_progress_json,
                request.expected.key.discord_id,
                request.expected.key.guild_id,
                request.expected.depth,
            ],
        )?;
        if updated == 0 {
            return Ok(DigAbandonSettlementOutcome::Conflict);
        }
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at)
             VALUES (?1, ?2, NULL, 'abandon', 0, 0, ?3, ?4, ?5)",
            params![
                request.expected.key.guild_id,
                request.expected.key.discord_id,
                request.refund,
                request.detail_json,
                request.created_at,
            ],
        )?;
        let audit_action_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(DigAbandonSettlementOutcome::Applied(
            DigAbandonSettlementReceipt {
                depth_lost: depth,
                prestige_level,
                balance_before,
                balance_after: balance_before.saturating_add(request.refund),
                audit_action_id,
            },
        ))
    }
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: AtomicDigAbandonSettlement<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
            (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'dig', ?1, 'abandon', ?2, 'dig abandon credit', ?3)",
        params![
            request.expected.key.discord_id,
            request.expected.depth,
            request.detail_json,
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_abandon_runtime/tests.rs"]
mod tests;
