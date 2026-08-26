//! Migrated-SQLite persistence for the Dig miner profile command group.
//!
//! The application layer owns validation, sanitization, stat formulas, and
//! player-facing copy. This repository owns profile creation and every guarded
//! mutation, including the wallet/stat/audit respec transaction.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigMinerTunnelSnapshot {
    pub tunnel_name: String,
    pub miner_about: String,
    pub stat_strength: i64,
    pub stat_smarts: i64,
    pub stat_stamina: i64,
    pub stat_points: i64,
    pub prestige_level: i64,
    pub stat_boss_awards_json: Option<String>,
    pub auto_buy_torch: bool,
    pub auto_buy_hard_hat: bool,
    pub auto_buy_grappling_hook: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigMinerSnapshot {
    pub discord_id: i64,
    pub guild_id: i64,
    pub player_exists: bool,
    pub balance: Option<i64>,
    pub tunnel: Option<DigMinerTunnelSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigMinerMutationStatus {
    Applied,
    MissingPlayer,
    MissingTunnel,
    Locked,
    InsufficientPoints,
    NoAllocatedPoints,
    InsufficientBalance,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigMinerMutationOutcome {
    pub status: DigMinerMutationStatus,
    pub snapshot: DigMinerSnapshot,
    pub returned_points: i64,
    pub action_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigMinerAllocation {
    pub strength: i64,
    pub smarts: i64,
    pub stamina: i64,
}

impl DigMinerAllocation {
    #[must_use]
    pub const fn total(self) -> i64 {
        self.strength
            .saturating_add(self.smarts)
            .saturating_add(self.stamina)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigMinerAutoBuyUpdate {
    pub torch: Option<bool>,
    pub hard_hat: Option<bool>,
    pub grappling_hook: Option<bool>,
}

#[derive(Debug, Error)]
pub enum DigMinerRuntimeRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid Dig miner mutation")]
    InvalidMutation,
    #[error("injected Dig miner settlement failure")]
    InjectedFailure,
}

#[derive(Clone, Debug)]
pub struct DigMinerRuntimeRepository {
    path: PathBuf,
}

impl DigMinerRuntimeRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigMinerSnapshot, DigMinerRuntimeRepositoryError> {
        let connection = self.connection()?;
        snapshot_in(&connection, discord_id, guild_id).map_err(Into::into)
    }

    pub fn ensure_tunnel(
        &self,
        discord_id: i64,
        guild_id: i64,
        tunnel_name: &str,
        created_at: i64,
    ) -> Result<DigMinerMutationOutcome, DigMinerRuntimeRepositoryError> {
        if tunnel_name.trim().is_empty() {
            return Err(DigMinerRuntimeRepositoryError::InvalidMutation);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !player_exists(&transaction, discord_id, guild_id)? {
            let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(outcome(DigMinerMutationStatus::MissingPlayer, snapshot));
        }
        transaction.execute(
            "INSERT OR IGNORE INTO tunnels
                (discord_id,guild_id,depth,max_depth,total_digs,total_jc_earned,
                 streak_days,pickaxe_tier,prestige_level,tunnel_name,trap_active,
                 trap_free_today,paid_digs_today,stat_points,created_at)
             VALUES (?1,?2,0,0,0,0,0,0,0,?3,0,1,0,5,?4)",
            params![discord_id, guild_id, tunnel_name.trim(), created_at],
        )?;
        let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(outcome(DigMinerMutationStatus::Applied, snapshot))
    }

    pub fn lock_backstory(
        &self,
        discord_id: i64,
        guild_id: i64,
        expected_about: &str,
        backstory: &str,
    ) -> Result<DigMinerMutationOutcome, DigMinerRuntimeRepositoryError> {
        if backstory.is_empty() {
            return Err(DigMinerRuntimeRepositoryError::InvalidMutation);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = tunnel_in(&transaction, discord_id, guild_id)?;
        let status = match current {
            None => DigMinerMutationStatus::MissingTunnel,
            Some(tunnel) if !tunnel.miner_about.trim().is_empty() => DigMinerMutationStatus::Locked,
            Some(tunnel) if tunnel.miner_about != expected_about => {
                DigMinerMutationStatus::Conflict
            }
            Some(_) => {
                let changed = transaction.execute(
                    "UPDATE tunnels SET miner_about=?1
                     WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(miner_about,'')=?4",
                    params![backstory, discord_id, guild_id, expected_about],
                )?;
                if changed == 1 {
                    DigMinerMutationStatus::Applied
                } else {
                    DigMinerMutationStatus::Conflict
                }
            }
        };
        let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(outcome(status, snapshot))
    }

    pub fn allocate_stats(
        &self,
        discord_id: i64,
        guild_id: i64,
        expected: &DigMinerTunnelSnapshot,
        allocation: DigMinerAllocation,
    ) -> Result<DigMinerMutationOutcome, DigMinerRuntimeRepositoryError> {
        if allocation.strength < 0
            || allocation.smarts < 0
            || allocation.stamina < 0
            || allocation.total() <= 0
        {
            return Err(DigMinerRuntimeRepositoryError::InvalidMutation);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status = match tunnel_in(&transaction, discord_id, guild_id)? {
            None => DigMinerMutationStatus::MissingTunnel,
            Some(current) if !same_stats(&current, expected) => DigMinerMutationStatus::Conflict,
            Some(current)
                if allocation.total()
                    > normalized_stat_points(current.stat_points).saturating_sub(
                        current
                            .stat_strength
                            .max(0)
                            .saturating_add(current.stat_smarts.max(0))
                            .saturating_add(current.stat_stamina.max(0)),
                    ) =>
            {
                DigMinerMutationStatus::InsufficientPoints
            }
            Some(_) => {
                let changed = transaction.execute(
                    "UPDATE tunnels
                     SET stat_strength=stat_strength+?1,
                         stat_smarts=stat_smarts+?2,
                         stat_stamina=stat_stamina+?3
                     WHERE discord_id=?4 AND guild_id=?5
                       AND stat_strength=?6 AND stat_smarts=?7 AND stat_stamina=?8
                       AND stat_points=?9",
                    params![
                        allocation.strength,
                        allocation.smarts,
                        allocation.stamina,
                        discord_id,
                        guild_id,
                        expected.stat_strength,
                        expected.stat_smarts,
                        expected.stat_stamina,
                        expected.stat_points,
                    ],
                )?;
                if changed == 1 {
                    DigMinerMutationStatus::Applied
                } else {
                    DigMinerMutationStatus::Conflict
                }
            }
        };
        let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(outcome(status, snapshot))
    }

    pub fn set_auto_buy(
        &self,
        discord_id: i64,
        guild_id: i64,
        update: DigMinerAutoBuyUpdate,
    ) -> Result<DigMinerMutationOutcome, DigMinerRuntimeRepositoryError> {
        if update.torch.is_none() && update.hard_hat.is_none() {
            return Err(DigMinerRuntimeRepositoryError::InvalidMutation);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(current) = tunnel_in(&transaction, discord_id, guild_id)? else {
            let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(outcome(DigMinerMutationStatus::MissingTunnel, snapshot));
        };
        let changed = transaction.execute(
            "UPDATE tunnels SET auto_buy_torch=?1,auto_buy_hard_hat=?2,auto_buy_grappling_hook=?3
             WHERE discord_id=?4 AND guild_id=?5
               AND auto_buy_torch=?6 AND auto_buy_hard_hat=?7 AND auto_buy_grappling_hook=?8",
            params![
                i64::from(update.torch.unwrap_or(current.auto_buy_torch)),
                i64::from(update.hard_hat.unwrap_or(current.auto_buy_hard_hat)),
                i64::from(update.grappling_hook.unwrap_or(current.auto_buy_grappling_hook)),
                discord_id,
                guild_id,
                i64::from(current.auto_buy_torch),
                i64::from(current.auto_buy_hard_hat),
                i64::from(current.auto_buy_grappling_hook),
            ],
        )?;
        let status = if changed == 1 {
            DigMinerMutationStatus::Applied
        } else {
            DigMinerMutationStatus::Conflict
        };
        let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(outcome(status, snapshot))
    }

    pub fn respec(
        &self,
        discord_id: i64,
        guild_id: i64,
        cost: i64,
        created_at: i64,
    ) -> Result<DigMinerMutationOutcome, DigMinerRuntimeRepositoryError> {
        self.respec_inner(discord_id, guild_id, cost, created_at, false)
    }

    fn respec_inner(
        &self,
        discord_id: i64,
        guild_id: i64,
        cost: i64,
        created_at: i64,
        fail_after_debit: bool,
    ) -> Result<DigMinerMutationOutcome, DigMinerRuntimeRepositoryError> {
        if cost < 0 {
            return Err(DigMinerRuntimeRepositoryError::InvalidMutation);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(tunnel) = tunnel_in(&transaction, discord_id, guild_id)? else {
            let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(outcome(DigMinerMutationStatus::NoAllocatedPoints, snapshot));
        };
        let returned_points = tunnel
            .stat_strength
            .max(0)
            .saturating_add(tunnel.stat_smarts.max(0))
            .saturating_add(tunnel.stat_stamina.max(0));
        if returned_points <= 0 {
            let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(outcome(DigMinerMutationStatus::NoAllocatedPoints, snapshot));
        }
        let balance = player_balance(&transaction, discord_id, guild_id)?;
        if balance.is_none_or(|balance| balance < cost) {
            let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(outcome(
                DigMinerMutationStatus::InsufficientBalance,
                snapshot,
            ));
        }
        let detail = serde_json::json!({
            "cost": cost,
            "returned_points": returned_points,
            "previous_stats": {
                "strength": tunnel.stat_strength,
                "smarts": tunnel.stat_smarts,
                "stamina": tunnel.stat_stamina,
            },
        })
        .to_string();
        set_ledger_context(&transaction, discord_id, &detail)?;
        let debited = transaction.execute(
            "UPDATE players SET jopacoin_balance=jopacoin_balance-?1,
                    updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3 AND jopacoin_balance>=?1",
            params![cost, discord_id, guild_id],
        );
        clear_ledger_context(&transaction)?;
        if debited? != 1 {
            transaction.rollback()?;
            return Ok(outcome(
                DigMinerMutationStatus::InsufficientBalance,
                self.snapshot(discord_id, guild_id)?,
            ));
        }
        if fail_after_debit {
            return Err(DigMinerRuntimeRepositoryError::InjectedFailure);
        }
        transaction.execute(
            "UPDATE players SET lowest_balance_ever=jopacoin_balance
             WHERE discord_id=?1 AND guild_id=?2
               AND (lowest_balance_ever IS NULL OR jopacoin_balance<lowest_balance_ever)",
            params![discord_id, guild_id],
        )?;
        let changed = transaction.execute(
            "UPDATE tunnels SET stat_strength=0,stat_smarts=0,stat_stamina=0
             WHERE discord_id=?1 AND guild_id=?2
               AND stat_strength=?3 AND stat_smarts=?4 AND stat_stamina=?5",
            params![
                discord_id,
                guild_id,
                tunnel.stat_strength,
                tunnel.stat_smarts,
                tunnel.stat_stamina,
            ],
        )?;
        if changed != 1 {
            transaction.rollback()?;
            return Ok(outcome(
                DigMinerMutationStatus::Conflict,
                self.snapshot(discord_id, guild_id)?,
            ));
        }
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                 jc_delta,detail,created_at)
             VALUES (?1,?2,NULL,'miner_respec',0,0,?3,?4,?5)",
            params![guild_id, discord_id, -cost, detail, created_at],
        )?;
        let action_id = transaction.last_insert_rowid();
        let snapshot = snapshot_in(&transaction, discord_id, guild_id)?;
        transaction.commit()?;
        Ok(DigMinerMutationOutcome {
            status: DigMinerMutationStatus::Applied,
            snapshot,
            returned_points,
            action_id: Some(action_id),
        })
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(&self.path)
    }
}

fn outcome(status: DigMinerMutationStatus, snapshot: DigMinerSnapshot) -> DigMinerMutationOutcome {
    DigMinerMutationOutcome {
        status,
        snapshot,
        returned_points: 0,
        action_id: None,
    }
}

const fn normalized_stat_points(points: i64) -> i64 {
    if points < 5 { 5 } else { points }
}

fn same_stats(left: &DigMinerTunnelSnapshot, right: &DigMinerTunnelSnapshot) -> bool {
    left.stat_strength == right.stat_strength
        && left.stat_smarts == right.stat_smarts
        && left.stat_stamina == right.stat_stamina
        && left.stat_points == right.stat_points
}

fn snapshot_in(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<DigMinerSnapshot, rusqlite::Error> {
    Ok(DigMinerSnapshot {
        discord_id,
        guild_id,
        player_exists: player_exists(connection, discord_id, guild_id)?,
        balance: player_balance(connection, discord_id, guild_id)?,
        tunnel: tunnel_in(connection, discord_id, guild_id)?,
    })
}

fn player_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn player_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn tunnel_in(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<DigMinerTunnelSnapshot>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(tunnel_name,'Unknown Tunnel'),COALESCE(miner_about,''),
                    COALESCE(stat_strength,0),COALESCE(stat_smarts,0),
                    COALESCE(stat_stamina,0),COALESCE(stat_points,5),
                    COALESCE(prestige_level,0),stat_boss_awards,
                    COALESCE(auto_buy_torch,0),COALESCE(auto_buy_hard_hat,0),
                    COALESCE(auto_buy_grappling_hook,0)
             FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok(DigMinerTunnelSnapshot {
                    tunnel_name: row.get(0)?,
                    miner_about: row.get(1)?,
                    stat_strength: row.get(2)?,
                    stat_smarts: row.get(3)?,
                    stat_stamina: row.get(4)?,
                    stat_points: row.get(5)?,
                    prestige_level: row.get(6)?,
                    stat_boss_awards_json: row.get(7)?,
                    auto_buy_torch: row.get::<_, i64>(8)? != 0,
                    auto_buy_hard_hat: row.get::<_, i64>(9)? != 0,
                    auto_buy_grappling_hook: row.get::<_, i64>(10)? != 0,
                })
            },
        )
        .optional()
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    detail: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
            (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,'dig',?1,'miner_respec','s_points','dig miner respec debit',?2)",
        params![actor_id, detail],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_miner_runtime/tests.rs"]
mod tests;
