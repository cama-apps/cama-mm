//! Existing-schema persistence for one guarded Dig prestige settlement.
//!
//! Policy and random selection stay in `cama-app`. This repository snapshots
//! the validated run, atomically commits the tunnel reset + wallet grant +
//! optional relic behind a prestige-level CAS, then exposes Python's separate
//! audit append as an intentionally distinct failure boundary.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigPrestigeKey {
    pub discord_id: i64,
    pub guild_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPrestigeSnapshot {
    pub key: DigPrestigeKey,
    pub depth: i64,
    pub prestige_level: i64,
    pub prestige_perks_json: String,
    pub boss_progress_json: String,
    pub current_run_jc: i64,
    pub current_run_artifacts: i64,
    pub current_run_events: i64,
    pub best_run_score: i64,
    pub total_prestige_score: i64,
    pub balance: i64,
    pub owned_artifact_ids: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct AtomicDigPrestigeSettlement<'a> {
    pub expected: &'a DigPrestigeSnapshot,
    pub next_prestige_level: i64,
    pub prestige_perks_json: &'a str,
    pub boss_progress_json: &'a str,
    pub mutations_json: Option<&'a str>,
    pub stat_boss_awards_json: &'a str,
    pub best_run_score: i64,
    pub total_prestige_score: i64,
    pub balance_delta: i64,
    pub relic_artifact_id: Option<&'a str>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPrestigeSettlementReceipt {
    pub prestige_level: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub relic_row_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DigPrestigeSettlementOutcome {
    Applied(DigPrestigeSettlementReceipt),
    Conflict,
    MissingTunnel,
    MissingPlayer,
}

#[derive(Debug, Error)]
pub enum DigPrestigeRuntimeRepositoryError {
    #[error("Dig prestige SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigPrestigeRuntimeRepository {
    path: PathBuf,
}

impl DigPrestigeRuntimeRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn snapshot(
        &self,
        key: DigPrestigeKey,
    ) -> Result<Option<DigPrestigeSnapshot>, DigPrestigeRuntimeRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let state = connection
            .query_row(
                "SELECT t.depth, t.prestige_level,
                        COALESCE(t.prestige_perks, '[]'),
                        COALESCE(t.boss_progress, '{}'),
                        COALESCE(t.current_run_jc, 0),
                        COALESCE(t.current_run_artifacts, 0),
                        COALESCE(t.current_run_events, 0),
                        COALESCE(t.best_run_score, 0),
                        COALESCE(t.total_prestige_score, 0),
                        p.jopacoin_balance
                   FROM tunnels t
                   JOIN players p
                     ON p.discord_id=t.discord_id AND p.guild_id=t.guild_id
                  WHERE t.discord_id=?1 AND t.guild_id=?2",
                params![key.discord_id, key.guild_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            depth,
            prestige_level,
            prestige_perks_json,
            boss_progress_json,
            current_run_jc,
            current_run_artifacts,
            current_run_events,
            best_run_score,
            total_prestige_score,
            balance,
        )) = state
        else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT artifact_id FROM dig_artifacts
              WHERE discord_id=?1 AND guild_id=?2",
        )?;
        let owned_artifact_ids = statement
            .query_map(params![key.discord_id, key.guild_id], |row| row.get(0))?
            .collect::<Result<BTreeSet<String>, _>>()?;
        Ok(Some(DigPrestigeSnapshot {
            key,
            depth,
            prestige_level,
            prestige_perks_json,
            boss_progress_json,
            current_run_jc,
            current_run_artifacts,
            current_run_events,
            best_run_score,
            total_prestige_score,
            balance,
            owned_artifact_ids,
        }))
    }

    pub fn atomic_settle(
        &self,
        request: AtomicDigPrestigeSettlement<'_>,
    ) -> Result<DigPrestigeSettlementOutcome, DigPrestigeRuntimeRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT t.prestige_level, p.jopacoin_balance
                   FROM tunnels t
                   LEFT JOIN players p
                     ON p.discord_id=t.discord_id AND p.guild_id=t.guild_id
                  WHERE t.discord_id=?1 AND t.guild_id=?2",
                params![
                    request.expected.key.discord_id,
                    request.expected.key.guild_id
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?;
        let Some((current_prestige, balance)) = current else {
            return Ok(DigPrestigeSettlementOutcome::MissingTunnel);
        };
        let Some(balance_before) = balance else {
            return Ok(DigPrestigeSettlementOutcome::MissingPlayer);
        };
        if current_prestige != request.expected.prestige_level {
            return Ok(DigPrestigeSettlementOutcome::Conflict);
        }

        if request.balance_delta != 0 {
            set_ledger_context(&transaction, request)?;
            let update = transaction.execute(
                "UPDATE players
                    SET jopacoin_balance=jopacoin_balance+?1,
                        updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3",
                params![
                    request.balance_delta,
                    request.expected.key.discord_id,
                    request.expected.key.guild_id
                ],
            );
            let clear = clear_ledger_context(&transaction);
            update?;
            clear?;
        }

        let updated = transaction.execute(
            "UPDATE tunnels
                SET depth=0,
                    boss_progress=?1,
                    boss_attempts=0,
                    prestige_level=?2,
                    prestige_perks=?3,
                    cheer_data=NULL,
                    injury_state=NULL,
                    best_run_score=?4,
                    current_run_jc=0,
                    current_run_artifacts=0,
                    current_run_events=0,
                    total_prestige_score=?5,
                    mutations=?6,
                    stat_boss_awards=?7,
                    pinnacle_boss_id=NULL,
                    pinnacle_phase=0,
                    pinnacle_hp_remaining=NULL,
                    pinnacle_last_engaged_at=NULL,
                    route_state=NULL
              WHERE discord_id=?8 AND guild_id=?9 AND prestige_level=?10",
            params![
                request.boss_progress_json,
                request.next_prestige_level,
                request.prestige_perks_json,
                request.best_run_score,
                request.total_prestige_score,
                request.mutations_json,
                request.stat_boss_awards_json,
                request.expected.key.discord_id,
                request.expected.key.guild_id,
                request.expected.prestige_level,
            ],
        )?;
        if updated == 0 {
            return Ok(DigPrestigeSettlementOutcome::Conflict);
        }

        let relic_row_id = if let Some(artifact_id) = request.relic_artifact_id {
            transaction.execute(
                "INSERT INTO dig_artifacts
                    (discord_id, guild_id, artifact_id, found_at, is_relic, equipped)
                 VALUES (?1, ?2, ?3, ?4, 1, 0)",
                params![
                    request.expected.key.discord_id,
                    request.expected.key.guild_id,
                    artifact_id,
                    request.created_at,
                ],
            )?;
            Some(transaction.last_insert_rowid())
        } else {
            None
        };
        let balance_after = balance_before.saturating_add(request.balance_delta);
        transaction.commit()?;
        Ok(DigPrestigeSettlementOutcome::Applied(
            DigPrestigeSettlementReceipt {
                prestige_level: request.next_prestige_level,
                balance_before,
                balance_after,
                relic_row_id,
            },
        ))
    }

    /// Append Python's post-commit prestige action. This is deliberately not
    /// fused into `atomic_settle`; an audit failure happens after the visible
    /// reset/grant transaction in the canonical implementation.
    pub fn append_audit(
        &self,
        key: DigPrestigeKey,
        jc_delta: i64,
        detail_json: &str,
        created_at: i64,
    ) -> Result<i64, DigPrestigeRuntimeRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection.execute(
            "INSERT INTO dig_actions
                (guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at)
             VALUES (?1, ?2, NULL, 'prestige', 0, 0, ?3, ?4, ?5)",
            params![
                key.guild_id,
                key.discord_id,
                jc_delta,
                detail_json,
                created_at
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: AtomicDigPrestigeSettlement<'_>,
) -> Result<(), rusqlite::Error> {
    let reason = if request.balance_delta > 0 {
        "dig payout"
    } else if request.balance_delta < 0 {
        "dig cost or penalty"
    } else {
        "dig balance change"
    };
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
            (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'dig', ?1, 'dig_action', NULL, ?2, NULL)",
        params![request.expected.key.discord_id, reason],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_prestige_runtime/tests.rs"]
mod tests;
