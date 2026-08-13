//! Existing-schema persistence for the prestige-four Dig content tranche.
//!
//! Python remains the sole migration and live-runtime authority. This adapter
//! opens only an already-migrated database through
//! [`crate::open_runtime_connection`]. Paused duel claiming, guarded boss
//! victory, and optional artifact drops deliberately remain separate
//! transactions because that is the production Python seam.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrestigeContentKey {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrestigeTunnelSnapshot {
    pub key: PrestigeContentKey,
    pub depth: i64,
    pub max_depth: i64,
    pub prestige_level: i64,
    pub boss_progress: Option<String>,
    pub balance: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactRow {
    pub database_id: i64,
    pub artifact_id: String,
    pub found_at: i64,
    pub is_relic: bool,
    pub equipped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedDuel {
    pub key: PrestigeContentKey,
    pub boss_id: String,
    pub tier: i64,
    pub mechanic_id: String,
    pub risk_tier: String,
    pub wager: i64,
    pub player_hp: i64,
    pub boss_hp: i64,
    pub round_num: i64,
    pub round_log_json: String,
    pub status_effects_json: String,
    pub player_hit: f64,
    pub player_damage: i64,
    pub boss_hit: f64,
    pub boss_damage: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactGrantOutcome {
    Granted { database_id: i64 },
    AlreadyOwned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EquipRelicOutcome {
    Equipped,
    Missing,
    NotRelic,
    AlreadyEquipped,
    DuplicateAlreadyEquipped,
    AtCap { cap: i64 },
}

#[derive(Clone, Copy, Debug)]
pub struct BossVictoryRequest<'a> {
    pub expected: &'a PrestigeTunnelSnapshot,
    pub boss_id: &'a str,
    pub depth_after: i64,
    pub max_depth_after: i64,
    pub boss_progress_after: &'a str,
    pub balance_delta: i64,
    pub echo_window_seconds: i64,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BossVictoryOutcome {
    Applied { balance_after: i64 },
    Conflict,
    MissingTunnel,
    MissingPlayer,
}

#[derive(Debug, Error)]
pub enum DigPrestige4RepositoryError {
    #[error("prestige-four arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error("prestige-four SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigPrestige4Repository {
    path: PathBuf,
}

impl DigPrestige4Repository {
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

    pub fn tunnel_snapshot(
        &self,
        key: PrestigeContentKey,
    ) -> Result<Option<PrestigeTunnelSnapshot>, DigPrestige4RepositoryError> {
        let connection = self.connection()?;
        query_tunnel(&connection, key).map_err(Into::into)
    }

    pub fn artifacts(
        &self,
        key: PrestigeContentKey,
    ) -> Result<Vec<ArtifactRow>, DigPrestige4RepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut statement = connection.prepare(
            "SELECT id, artifact_id, found_at, is_relic, equipped
               FROM dig_artifacts
              WHERE discord_id = ?1 AND guild_id = ?2
              ORDER BY found_at DESC, id DESC",
        )?;
        statement
            .query_map(params![key.discord_id, guild_id], |row| {
                Ok(ArtifactRow {
                    database_id: row.get(0)?,
                    artifact_id: row.get(1)?,
                    found_at: row.get(2)?,
                    is_relic: row.get::<_, i64>(3)? != 0,
                    equipped: row.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Atomic check-and-insert used by trophy, prestige, and raw-find policy.
    pub fn grant_artifact_unique(
        &self,
        key: PrestigeContentKey,
        artifact_id: &str,
        is_relic: bool,
        found_at: i64,
    ) -> Result<ArtifactGrantOutcome, DigPrestige4RepositoryError> {
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owned = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM dig_artifacts
                  WHERE discord_id = ?1 AND guild_id = ?2 AND artifact_id = ?3
             )",
            params![key.discord_id, guild_id, artifact_id],
            |row| row.get::<_, bool>(0),
        )?;
        if owned {
            transaction.commit()?;
            return Ok(ArtifactGrantOutcome::AlreadyOwned);
        }
        transaction.execute(
            "INSERT INTO dig_artifacts
                 (discord_id, guild_id, artifact_id, found_at, is_relic, equipped)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![key.discord_id, guild_id, artifact_id, found_at, is_relic],
        )?;
        let database_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(ArtifactGrantOutcome::Granted { database_id })
    }

    /// Compatibility helper for tests/legacy duplicate rows. Live grant paths
    /// should use [`Self::grant_artifact_unique`].
    pub fn insert_artifact_unchecked(
        &self,
        key: PrestigeContentKey,
        artifact_id: &str,
        is_relic: bool,
        found_at: i64,
    ) -> Result<i64, DigPrestige4RepositoryError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO dig_artifacts
                 (discord_id, guild_id, artifact_id, found_at, is_relic, equipped)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
            params![
                key.discord_id,
                Self::normalize_guild_id(key.guild_id),
                artifact_id,
                found_at,
                is_relic,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn equip_relic_unique(
        &self,
        key: PrestigeContentKey,
        artifact_database_id: i64,
        relic_slot_base: i64,
        relic_slot_max: i64,
    ) -> Result<EquipRelicOutcome, DigPrestige4RepositoryError> {
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target = transaction
            .query_row(
                "SELECT artifact_id, is_relic, equipped
                   FROM dig_artifacts
                  WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3",
                params![artifact_database_id, key.discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? != 0,
                        row.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()?;
        let Some((artifact_id, is_relic, equipped)) = target else {
            transaction.commit()?;
            return Ok(EquipRelicOutcome::Missing);
        };
        if !is_relic {
            transaction.commit()?;
            return Ok(EquipRelicOutcome::NotRelic);
        }
        if equipped {
            transaction.commit()?;
            return Ok(EquipRelicOutcome::AlreadyEquipped);
        }
        let duplicate = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM dig_artifacts
                  WHERE discord_id = ?1 AND guild_id = ?2
                    AND artifact_id = ?3 AND equipped = 1
             )",
            params![key.discord_id, guild_id, artifact_id],
            |row| row.get::<_, bool>(0),
        )?;
        if duplicate {
            transaction.commit()?;
            return Ok(EquipRelicOutcome::DuplicateAlreadyEquipped);
        }
        let prestige = transaction
            .query_row(
                "SELECT COALESCE(prestige_level, 0) FROM tunnels
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![key.discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let cap = prestige
            .saturating_add(relic_slot_base)
            .min(relic_slot_max)
            .max(0);
        let equipped_count = transaction.query_row(
            "SELECT COUNT(*) FROM dig_artifacts
              WHERE discord_id = ?1 AND guild_id = ?2
                AND is_relic = 1 AND equipped = 1",
            params![key.discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )?;
        if equipped_count >= cap {
            transaction.commit()?;
            return Ok(EquipRelicOutcome::AtCap { cap });
        }
        transaction.execute(
            "UPDATE dig_artifacts SET equipped = 1
              WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3",
            params![artifact_database_id, key.discord_id, guild_id],
        )?;
        transaction.commit()?;
        Ok(EquipRelicOutcome::Equipped)
    }

    /// Unequip one relic only when the supplied player/guild owns the row.
    /// Forged artifact database IDs are a no-op rather than a cross-player
    /// mutation, matching the command repository's ownership boundary.
    pub fn unequip_relic_owned(
        &self,
        key: PrestigeContentKey,
        artifact_database_id: i64,
    ) -> Result<bool, DigPrestige4RepositoryError> {
        let changed = self.connection()?.execute(
            "UPDATE dig_artifacts SET equipped = 0
              WHERE id = ?1 AND discord_id = ?2 AND guild_id = ?3
                AND is_relic = 1",
            params![
                artifact_database_id,
                key.discord_id,
                Self::normalize_guild_id(key.guild_id)
            ],
        )?;
        Ok(changed > 0)
    }

    /// Claim one paused duel exactly once. No victory/drop side effects occur.
    pub fn claim_active_duel(
        &self,
        key: PrestigeContentKey,
    ) -> Result<Option<ClaimedDuel>, DigPrestige4RepositoryError> {
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let duel = transaction
            .query_row(
                "SELECT boss_id, tier, mechanic_id, risk_tier, wager,
                        player_hp, boss_hp, round_num, round_log, status_effects,
                        player_hit, player_dmg, boss_hit, boss_dmg
                   FROM dig_active_duels
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![key.discord_id, guild_id],
                |row| {
                    Ok(ClaimedDuel {
                        key,
                        boss_id: row.get(0)?,
                        tier: row.get(1)?,
                        mechanic_id: row.get(2)?,
                        risk_tier: row.get(3)?,
                        wager: row.get(4)?,
                        player_hp: row.get(5)?,
                        boss_hp: row.get(6)?,
                        round_num: row.get(7)?,
                        round_log_json: row.get(8)?,
                        status_effects_json: row.get(9)?,
                        player_hit: row.get(10)?,
                        player_damage: row.get(11)?,
                        boss_hit: row.get(12)?,
                        boss_damage: row.get(13)?,
                    })
                },
            )
            .optional()?;
        if duel.is_some() {
            transaction.execute(
                "DELETE FROM dig_active_duels
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![key.discord_id, guild_id],
            )?;
        }
        transaction.commit()?;
        Ok(duel)
    }

    /// Guarded boss-state/balance/echo/audit transaction. Optional artifact
    /// rolls intentionally happen only after this method returns `Applied`.
    pub fn complete_boss_victory(
        &self,
        request: BossVictoryRequest<'_>,
    ) -> Result<BossVictoryOutcome, DigPrestige4RepositoryError> {
        let expected = request.expected;
        let guild_id = Self::normalize_guild_id(expected.key.guild_id);
        let balance_after = expected
            .balance
            .checked_add(request.balance_delta)
            .ok_or(DigPrestige4RepositoryError::ArithmeticOverflow)?;
        let weakened_until = request
            .now
            .checked_add(request.echo_window_seconds)
            .ok_or(DigPrestige4RepositoryError::ArithmeticOverflow)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE tunnels
                SET depth = ?1, max_depth = ?2, boss_progress = ?3
              WHERE discord_id = ?4 AND guild_id = ?5
                AND COALESCE(depth, 0) = ?6
                AND COALESCE(max_depth, 0) = ?7
                AND COALESCE(prestige_level, 0) = ?8
                AND boss_progress IS ?9",
            params![
                request.depth_after,
                request.max_depth_after,
                request.boss_progress_after,
                expected.key.discord_id,
                guild_id,
                expected.depth,
                expected.max_depth,
                expected.prestige_level,
                expected.boss_progress,
            ],
        )?;
        if changed != 1 {
            let exists = row_exists(&transaction, "tunnels", expected.key.discord_id, guild_id)?;
            transaction.rollback()?;
            return Ok(if exists {
                BossVictoryOutcome::Conflict
            } else {
                BossVictoryOutcome::MissingTunnel
            });
        }
        let actual_balance = transaction
            .query_row(
                "SELECT COALESCE(jopacoin_balance, 0) FROM players
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![expected.key.discord_id, guild_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(actual_balance) = actual_balance else {
            transaction.rollback()?;
            return Ok(BossVictoryOutcome::MissingPlayer);
        };
        if actual_balance != expected.balance {
            transaction.rollback()?;
            return Ok(BossVictoryOutcome::Conflict);
        }
        if request.balance_delta != 0 {
            set_ledger_context(&transaction, request)?;
            transaction.execute(
                "UPDATE players SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                  WHERE discord_id = ?2 AND guild_id = ?3",
                params![balance_after, expected.key.discord_id, guild_id],
            )?;
            clear_ledger_context(&transaction)?;
        }
        transaction.execute(
            "INSERT INTO dig_boss_echoes
                 (guild_id, boss_id, depth, killer_discord_id, weakened_until)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(guild_id, boss_id) DO UPDATE SET
                 depth = excluded.depth,
                 killer_discord_id = excluded.killer_discord_id,
                 weakened_until = excluded.weakened_until",
            params![
                guild_id,
                request.boss_id,
                request.depth_after,
                expected.key.discord_id,
                weakened_until,
            ],
        )?;
        transaction.execute(
            "INSERT INTO dig_actions
                 (guild_id, actor_id, target_id, action_type, depth_before,
                  depth_after, jc_delta, detail, created_at)
             VALUES (?1, ?2, NULL, 'boss_fight', ?3, ?4, ?5, ?6, ?7)",
            params![
                guild_id,
                expected.key.discord_id,
                expected.depth,
                request.depth_after,
                request.balance_delta,
                request.detail_json,
                request.now,
            ],
        )?;
        transaction.commit()?;
        Ok(BossVictoryOutcome::Applied { balance_after })
    }
}

fn query_tunnel(
    connection: &Connection,
    key: PrestigeContentKey,
) -> Result<Option<PrestigeTunnelSnapshot>, rusqlite::Error> {
    let guild_id = DigPrestige4Repository::normalize_guild_id(key.guild_id);
    connection
        .query_row(
            "SELECT COALESCE(t.depth, 0), COALESCE(t.max_depth, 0),
                    COALESCE(t.prestige_level, 0), t.boss_progress,
                    COALESCE(p.jopacoin_balance, 0)
               FROM tunnels AS t
               LEFT JOIN players AS p
                 ON p.discord_id = t.discord_id AND p.guild_id = t.guild_id
              WHERE t.discord_id = ?1 AND t.guild_id = ?2",
            params![key.discord_id, guild_id],
            |row| {
                Ok(PrestigeTunnelSnapshot {
                    key,
                    depth: row.get(0)?,
                    max_depth: row.get(1)?,
                    prestige_level: row.get(2)?,
                    boss_progress: row.get(3)?,
                    balance: row.get(4)?,
                })
            },
        )
        .optional()
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: BossVictoryRequest<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id, source, actor_id, related_type, related_id, reason, metadata)
         VALUES (1, 'dig', ?1, 'boss_fight', ?2, 'boss fight reward', ?3)",
        params![
            request.expected.key.discord_id,
            request.boss_id,
            request.detail_json,
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn row_exists(
    transaction: &Transaction<'_>,
    table: &str,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    debug_assert_eq!(table, "tunnels");
    transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2
         )",
        params![discord_id, guild_id],
        |row| row.get::<_, bool>(0),
    )
}

#[cfg(test)]
#[path = "dig_prestige4_content/tests.rs"]
mod tests;
