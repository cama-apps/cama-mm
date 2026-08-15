//! Existing-schema atomic persistence for Dig sweep regressions.
//!
//! Python remains the migration and live runtime authority. This adapter only
//! opens a migrated database through [`crate::open_runtime_connection`]. Every
//! operation that spans tunnel, inventory, balance, artifact, duel, or audit
//! state uses `BEGIN IMMEDIATE`, making a failed compare-and-swap a complete
//! no-op rather than a partially committed dig.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TunnelKey {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrestigeClaimRequest<'a> {
    pub key: TunnelKey,
    pub expected_depth: i64,
    pub expected_prestige_level: i64,
    pub reward: i64,
    pub relic_id: &'a str,
    pub found_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrestigeClaimOutcome {
    Claimed {
        prestige_level: i64,
        balance_after: i64,
        artifact_id: i64,
    },
    Conflict,
    MissingTunnel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbandonOutcome {
    Abandoned { refund: i64, balance_after: i64 },
    Conflict,
    MissingTunnel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockedAttemptRequest<'a> {
    pub guild_id: Option<i64>,
    pub actor_id: i64,
    pub target_id: i64,
    pub cost: i64,
    pub requested_tip: i64,
    pub action_type: &'a str,
    pub detail_json: &'a str,
    pub depth: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedAttemptOutcome {
    pub action_id: i64,
    pub actor_balance_after: i64,
    pub target_balance_after: i64,
    pub funded_tip: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct SabotageRequest<'a> {
    pub guild_id: Option<i64>,
    pub actor_id: i64,
    pub target_id: i64,
    pub actor_jc_cost: i64,
    pub target_jc_credit: i64,
    pub target_depth_delta: i64,
    pub actor_depth_delta: i64,
    pub clear_target_trap: bool,
    pub action_type: &'a str,
    pub detail_json: Option<&'a str>,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SabotageOutcome {
    pub target_depth: i64,
    pub target_trap_active: bool,
    pub actor_depth: i64,
    pub actor_balance_after: i64,
    pub target_balance_after: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanternRestoreOutcome {
    Restored { luminosity: i64 },
    AlreadyRestored { luminosity: i64 },
    MissingTunnel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumableColumns {
    pub depth: i64,
    pub hard_hat_charges: i64,
    pub grappling_hook_charges: i64,
    pub sonar_skip_pending: bool,
    pub reinforced_until: Option<i64>,
    pub void_bait_digs: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct CommitConsumableDigRequest<'a> {
    pub key: TunnelKey,
    pub expected: ConsumableColumns,
    pub next: ConsumableColumns,
    pub consumed_item_ids: &'a [i64],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitConsumableDigOutcome {
    Committed(ConsumableColumns),
    Conflict,
    MissingTunnel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedDuel {
    pub boss_id: String,
    pub mechanic_id: String,
    pub wager: i64,
    pub last_interaction_at: i64,
}

#[derive(Debug, Error)]
pub enum DigSweepRepositoryError {
    #[error("Dig sweep SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("player {discord_id} is missing in guild {guild_id}")]
    MissingPlayer { discord_id: i64, guild_id: i64 },
    #[error("player {discord_id} has balance {balance}, below required cost {cost}")]
    InsufficientBalance {
        discord_id: i64,
        balance: i64,
        cost: i64,
    },
    #[error("amounts must be non-negative")]
    NegativeAmount,
    #[error("queued consumable {0} disappeared before the atomic commit")]
    MissingQueuedItem(i64),
    #[error("tunnel for player {discord_id} is missing in guild {guild_id}")]
    MissingTunnel { discord_id: i64, guild_id: i64 },
}

#[derive(Clone, Debug)]
pub struct DigSweepRepository {
    path: PathBuf,
}

impl DigSweepRepository {
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

    /// Claim a specific prestige snapshot and settle every side effect once.
    pub fn claim_prestige_atomic(
        &self,
        request: PrestigeClaimRequest<'_>,
    ) -> Result<PrestigeClaimOutcome, DigSweepRepositoryError> {
        if request.reward < 0 {
            return Err(DigSweepRepositoryError::NegativeAmount);
        }
        let guild_id = Self::normalize_guild_id(request.key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels
             SET depth = 0,
                 prestige_level = prestige_level + 1,
                 boss_progress = '{}',
                 current_run_jc = 0,
                 current_run_artifacts = 0,
                 current_run_events = 0,
                 pinnacle_boss_id = NULL,
                 pinnacle_phase = 0,
                 pinnacle_hp_remaining = NULL,
                 pinnacle_last_engaged_at = NULL
             WHERE discord_id = ?1 AND guild_id = ?2
               AND depth = ?3 AND prestige_level = ?4",
            params![
                request.key.discord_id,
                guild_id,
                request.expected_depth,
                request.expected_prestige_level
            ],
        )?;
        if changed == 0 {
            let exists = tunnel_exists(&transaction, request.key.discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(if exists {
                PrestigeClaimOutcome::Conflict
            } else {
                PrestigeClaimOutcome::MissingTunnel
            });
        }

        credit_player(
            &transaction,
            request.key.discord_id,
            guild_id,
            request.reward,
            "dig_prestige",
            "atomic prestige reward",
        )?;
        transaction.execute(
            "INSERT INTO dig_artifacts
                 (discord_id, guild_id, artifact_id, found_at, is_relic, equipped)
             VALUES (?1, ?2, ?3, ?4, 1, 0)",
            params![
                request.key.discord_id,
                guild_id,
                request.relic_id,
                request.found_at
            ],
        )?;
        let artifact_id = transaction.last_insert_rowid();
        let (prestige_level, balance_after) = transaction.query_row(
            "SELECT t.prestige_level, p.jopacoin_balance
             FROM tunnels t JOIN players p
               ON p.discord_id = t.discord_id AND p.guild_id = t.guild_id
             WHERE t.discord_id = ?1 AND t.guild_id = ?2",
            params![request.key.discord_id, guild_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        transaction.commit()?;
        Ok(PrestigeClaimOutcome::Claimed {
            prestige_level,
            balance_after,
            artifact_id,
        })
    }

    /// Reset an exact tunnel run and pay its already-scaled refund atomically.
    pub fn abandon_atomic(
        &self,
        key: TunnelKey,
        expected_depth: i64,
        refund: i64,
    ) -> Result<AbandonOutcome, DigSweepRepositoryError> {
        if refund < 0 {
            return Err(DigSweepRepositoryError::NegativeAmount);
        }
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels
             SET depth = 0,
                 boss_progress = '{}',
                 current_run_jc = 0,
                 current_run_artifacts = 0,
                 current_run_events = 0,
                 pinnacle_boss_id = NULL,
                 pinnacle_phase = 0,
                 pinnacle_hp_remaining = NULL,
                 pinnacle_last_engaged_at = NULL
             WHERE discord_id = ?1 AND guild_id = ?2 AND depth = ?3",
            params![key.discord_id, guild_id, expected_depth],
        )?;
        if changed == 0 {
            let exists = tunnel_exists(&transaction, key.discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(if exists {
                AbandonOutcome::Conflict
            } else {
                AbandonOutcome::MissingTunnel
            });
        }
        credit_player(
            &transaction,
            key.discord_id,
            guild_id,
            refund,
            "dig_abandon",
            "atomic abandon refund",
        )?;
        let balance_after = player_balance(&transaction, key.discord_id, guild_id)?;
        transaction.commit()?;
        Ok(AbandonOutcome::Abandoned {
            refund,
            balance_after,
        })
    }

    /// Add boss lock fields to one JSON entry without replacing preparation.
    pub fn merge_locked_boss(
        &self,
        key: TunnelKey,
        boundary: i64,
        boss_id: &str,
    ) -> Result<Option<String>, DigSweepRepositoryError> {
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let boss_path = format!("$.\"{boundary}\".boss_id");
        let status_path = format!("$.\"{boundary}\".status");
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let progress = transaction
            .query_row(
                "UPDATE tunnels
                 SET boss_progress = json_set(
                     CASE WHEN json_valid(COALESCE(boss_progress, '{}'))
                          THEN COALESCE(boss_progress, '{}') ELSE '{}' END,
                     ?1, ?2, ?3, 'active')
                 WHERE discord_id = ?4 AND guild_id = ?5
                 RETURNING boss_progress",
                params![boss_path, boss_id, status_path, key.discord_id, guild_id],
                |row| row.get(0),
            )
            .optional()?;
        transaction.commit()?;
        Ok(progress)
    }

    /// Debit a blocked saboteur, fund a bounded tip, and append its audit row.
    pub fn record_blocked_attempt_atomic(
        &self,
        request: BlockedAttemptRequest<'_>,
    ) -> Result<BlockedAttemptOutcome, DigSweepRepositoryError> {
        if request.cost < 0 || request.requested_tip < 0 {
            return Err(DigSweepRepositoryError::NegativeAmount);
        }
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let funded_tip = request.requested_tip.min(request.cost);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor_balance = player_balance(&transaction, request.actor_id, guild_id)?;
        if actor_balance < request.cost {
            return Err(DigSweepRepositoryError::InsufficientBalance {
                discord_id: request.actor_id,
                balance: actor_balance,
                cost: request.cost,
            });
        }
        set_ledger_context(
            &transaction,
            request.actor_id,
            request.action_type,
            "funded blocked sabotage",
        )?;
        transaction.execute(
            "UPDATE players SET jopacoin_balance = jopacoin_balance - ?1
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![request.cost, request.actor_id, guild_id],
        )?;
        if funded_tip > 0 {
            let changed = transaction.execute(
                "UPDATE players SET jopacoin_balance = jopacoin_balance + ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![funded_tip, request.target_id, guild_id],
            )?;
            if changed == 0 {
                return Err(DigSweepRepositoryError::MissingPlayer {
                    discord_id: request.target_id,
                    guild_id,
                });
            }
        } else {
            // Still validate the target so the audit cannot name a nonexistent player.
            let _ = player_balance(&transaction, request.target_id, guild_id)?;
        }
        clear_ledger_context(&transaction)?;
        transaction.execute(
            "INSERT INTO dig_actions
                 (guild_id, actor_id, target_id, action_type, depth_before,
                  depth_after, jc_delta, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8)",
            params![
                guild_id,
                request.actor_id,
                request.target_id,
                request.action_type,
                request.depth,
                -request.cost,
                request.detail_json,
                request.created_at
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        let actor_balance_after = player_balance(&transaction, request.actor_id, guild_id)?;
        let target_balance_after = player_balance(&transaction, request.target_id, guild_id)?;
        transaction.commit()?;
        Ok(BlockedAttemptOutcome {
            action_id,
            actor_balance_after,
            target_balance_after,
            funded_tip,
        })
    }

    /// Apply both wallets, both tunnel deltas, trap state, and the optional
    /// sabotage audit row under one immediate write lock.
    pub fn atomic_sabotage(
        &self,
        request: SabotageRequest<'_>,
    ) -> Result<SabotageOutcome, DigSweepRepositoryError> {
        if request.actor_jc_cost < 0 || request.target_jc_credit < 0 {
            return Err(DigSweepRepositoryError::NegativeAmount);
        }
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _ = player_balance(&transaction, request.actor_id, guild_id)?;
        let _ = player_balance(&transaction, request.target_id, guild_id)?;

        if request.actor_jc_cost > 0 {
            set_ledger_context(
                &transaction,
                request.actor_id,
                request.action_type,
                "dig sabotage cost",
            )?;
            transaction.execute(
                "UPDATE players SET jopacoin_balance = jopacoin_balance - ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![request.actor_jc_cost, request.actor_id, guild_id],
            )?;
            clear_ledger_context(&transaction)?;
        }
        if request.target_jc_credit > 0 {
            set_ledger_context(
                &transaction,
                request.actor_id,
                request.action_type,
                "dig sabotage target credit",
            )?;
            transaction.execute(
                "UPDATE players SET jopacoin_balance = jopacoin_balance + ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![request.target_jc_credit, request.target_id, guild_id],
            )?;
            clear_ledger_context(&transaction)?;
        }

        let target_changed = transaction.execute(
            "UPDATE tunnels
             SET depth = MAX(0, depth + ?1),
                 trap_active = CASE WHEN ?2 THEN 0 ELSE trap_active END
             WHERE discord_id = ?3 AND guild_id = ?4",
            params![
                request.target_depth_delta,
                request.clear_target_trap,
                request.target_id,
                guild_id
            ],
        )?;
        if target_changed != 1 {
            return Err(DigSweepRepositoryError::MissingTunnel {
                discord_id: request.target_id,
                guild_id,
            });
        }
        let actor_changed = transaction.execute(
            "UPDATE tunnels SET depth = MAX(0, depth + ?1)
             WHERE discord_id = ?2 AND guild_id = ?3",
            params![request.actor_depth_delta, request.actor_id, guild_id],
        )?;
        if actor_changed != 1 {
            return Err(DigSweepRepositoryError::MissingTunnel {
                discord_id: request.actor_id,
                guild_id,
            });
        }
        if let Some(detail_json) = request.detail_json {
            transaction.execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, ?3, ?4, 0, 0, 0, ?5, ?6)",
                params![
                    guild_id,
                    request.actor_id,
                    request.target_id,
                    request.action_type,
                    detail_json,
                    request.created_at
                ],
            )?;
        }
        let (target_depth, target_trap_active) = transaction.query_row(
            "SELECT depth, trap_active FROM tunnels
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![request.target_id, guild_id],
            |row| Ok((row.get(0)?, row.get::<_, bool>(1)?)),
        )?;
        let actor_depth = transaction.query_row(
            "SELECT depth FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2",
            params![request.actor_id, guild_id],
            |row| row.get(0),
        )?;
        let actor_balance_after = player_balance(&transaction, request.actor_id, guild_id)?;
        let target_balance_after = player_balance(&transaction, request.target_id, guild_id)?;
        transaction.commit()?;
        Ok(SabotageOutcome {
            target_depth,
            target_trap_active,
            actor_depth,
            actor_balance_after,
            target_balance_after,
        })
    }

    /// Restore luminosity once per game date using the date as the claim key.
    pub fn restore_lantern_once_atomic(
        &self,
        key: TunnelKey,
        game_date: &str,
        amount: i64,
    ) -> Result<LanternRestoreOutcome, DigSweepRepositoryError> {
        if amount < 0 {
            return Err(DigSweepRepositoryError::NegativeAmount);
        }
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let restored = transaction
            .query_row(
                "UPDATE tunnels
                 SET luminosity = MIN(100, luminosity + ?1), lantern_stub_date = ?2
                 WHERE discord_id = ?3 AND guild_id = ?4
                   AND (lantern_stub_date IS NULL OR lantern_stub_date <> ?2)
                 RETURNING luminosity",
                params![amount, game_date, key.discord_id, guild_id],
                |row| row.get(0),
            )
            .optional()?;
        let outcome = if let Some(luminosity) = restored {
            LanternRestoreOutcome::Restored { luminosity }
        } else {
            let luminosity = transaction
                .query_row(
                    "SELECT luminosity FROM tunnels
                     WHERE discord_id = ?1 AND guild_id = ?2",
                    params![key.discord_id, guild_id],
                    |row| row.get(0),
                )
                .optional()?;
            match luminosity {
                Some(luminosity) => LanternRestoreOutcome::AlreadyRestored { luminosity },
                None => LanternRestoreOutcome::MissingTunnel,
            }
        };
        transaction.commit()?;
        Ok(outcome)
    }

    /// Commit all consumable columns and remove exactly the queued items.
    pub fn commit_consumable_dig_atomic(
        &self,
        request: CommitConsumableDigRequest<'_>,
    ) -> Result<CommitConsumableDigOutcome, DigSweepRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels
             SET depth = ?1, hard_hat_charges = ?2, grappling_hook_charges = ?3,
                 sonar_skip_pending = ?4, reinforced_until = ?5, void_bait_digs = ?6
             WHERE discord_id = ?7 AND guild_id = ?8
               AND depth = ?9 AND hard_hat_charges = ?10
               AND grappling_hook_charges = ?11 AND sonar_skip_pending = ?12
               AND COALESCE(reinforced_until, -1) = COALESCE(?13, -1)
               AND void_bait_digs = ?14",
            params![
                request.next.depth,
                request.next.hard_hat_charges,
                request.next.grappling_hook_charges,
                i64::from(request.next.sonar_skip_pending),
                request.next.reinforced_until,
                request.next.void_bait_digs,
                request.key.discord_id,
                guild_id,
                request.expected.depth,
                request.expected.hard_hat_charges,
                request.expected.grappling_hook_charges,
                i64::from(request.expected.sonar_skip_pending),
                request.expected.reinforced_until,
                request.expected.void_bait_digs
            ],
        )?;
        if changed == 0 {
            let exists = tunnel_exists(&transaction, request.key.discord_id, guild_id)?;
            transaction.commit()?;
            return Ok(if exists {
                CommitConsumableDigOutcome::Conflict
            } else {
                CommitConsumableDigOutcome::MissingTunnel
            });
        }
        for item_id in request.consumed_item_ids {
            let deleted = transaction.execute(
                "DELETE FROM dig_inventory
                 WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3 AND queued = 1",
                params![item_id, request.key.discord_id, guild_id],
            )?;
            if deleted != 1 {
                return Err(DigSweepRepositoryError::MissingQueuedItem(*item_id));
            }
        }
        transaction.commit()?;
        Ok(CommitConsumableDigOutcome::Committed(request.next))
    }

    /// Atomically take ownership of a paused duel; a retry observes no row.
    pub fn claim_active_duel_atomic(
        &self,
        key: TunnelKey,
    ) -> Result<Option<ClaimedDuel>, DigSweepRepositoryError> {
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let duel = transaction
            .query_row(
                "DELETE FROM dig_active_duels
                 WHERE discord_id = ?1 AND guild_id = ?2
                 RETURNING boss_id, mechanic_id, wager, last_interaction_at",
                params![key.discord_id, guild_id],
                |row| {
                    Ok(ClaimedDuel {
                        boss_id: row.get(0)?,
                        mechanic_id: row.get(1)?,
                        wager: row.get(2)?,
                        last_interaction_at: row.get(3)?,
                    })
                },
            )
            .optional()?;
        transaction.commit()?;
        Ok(duel)
    }
}

fn tunnel_exists(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT 1 FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
}

fn player_balance(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
) -> Result<i64, DigSweepRepositoryError> {
    transaction
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(DigSweepRepositoryError::MissingPlayer {
            discord_id,
            guild_id,
        })
}

fn credit_player(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    amount: i64,
    related_type: &str,
    reason: &str,
) -> Result<(), DigSweepRepositoryError> {
    set_ledger_context(transaction, discord_id, related_type, reason)?;
    let changed = transaction.execute(
        "UPDATE players SET jopacoin_balance = jopacoin_balance + ?1
         WHERE discord_id = ?2 AND guild_id = ?3",
        params![amount, discord_id, guild_id],
    )?;
    clear_ledger_context(transaction)?;
    if changed == 0 {
        return Err(DigSweepRepositoryError::MissingPlayer {
            discord_id,
            guild_id,
        });
    }
    Ok(())
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    related_type: &str,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'dig', ?1, ?2, NULL, ?3, NULL)",
        params![actor_id, related_type, reason],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_sweep_fixes/tests.rs"]
mod tests;
