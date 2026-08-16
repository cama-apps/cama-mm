//! Guarded SQLite persistence for live Dig gear/relic management.
//!
//! The application layer rehydrates the authoritative `cama-domain` gear
//! aggregate, executes one policy command, and returns a proposed state. This
//! repository validates that the complete player snapshot is still current
//! under `BEGIN IMMEDIATE`, then applies balance, gear, relic, tunnel mirror,
//! ledger context, and audit changes as one commit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cama_domain::dig_gear::{Artifact, GearPiece, GearSlot, Tunnel};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearRuntimeSnapshot {
    pub discord_id: i64,
    pub guild_id: i64,
    pub tunnel: Tunnel,
    pub balance: i64,
    pub gear: Vec<GearPiece>,
    pub artifacts: Vec<Artifact>,
    pub next_gear_id: i64,
    pub next_artifact_id: i64,
    pub clock: i64,
}

/// Read-only state used to project Python's live `/dig shop` catalog.
///
/// A registered player may open the shop before their first dig, so the
/// tunnel is optional here even though every mutating gear command requires
/// the full [`DigGearRuntimeSnapshot`]. The equipped weapon tier deliberately
/// ignores durability: a broken high-tier pickaxe still owns that rung and
/// must not expose lower upgrades again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearShopSnapshot {
    pub discord_id: i64,
    pub guild_id: i64,
    pub balance: i64,
    pub tunnel: Option<Tunnel>,
    pub equipped_weapon_tier: Option<u8>,
    pub inventory_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearRuntimeState {
    pub tunnel: Tunnel,
    pub balance: i64,
    pub gear: Vec<GearPiece>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Copy, Debug)]
pub struct DigGearRuntimeCommit<'a> {
    pub expected: &'a DigGearRuntimeSnapshot,
    pub proposed: &'a DigGearRuntimeState,
    pub action_type: &'a str,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigGearRuntimeReceipt {
    pub action_id: i64,
    pub balance_after: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigGearRuntimeCommitOutcome {
    Applied(DigGearRuntimeReceipt),
    Conflict,
    MissingPlayer,
    MissingTunnel,
}

#[derive(Debug, Error)]
pub enum DigGearRuntimeRepositoryError {
    #[error("Dig gear SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Dig gear row has invalid slot `{0}`")]
    InvalidSlot(String),
    #[error("Dig gear proposed state is invalid: {0}")]
    InvalidState(&'static str),
}

#[derive(Clone, Debug)]
pub struct DigGearRuntimeRepository {
    database_path: PathBuf,
}

impl DigGearRuntimeRepository {
    #[must_use]
    pub fn new(database_path: impl AsRef<Path>) -> Self {
        Self {
            database_path: database_path.as_ref().to_path_buf(),
        }
    }

    pub fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigGearRuntimeSnapshot>, DigGearRuntimeRepositoryError> {
        let connection = Connection::open(&self.database_path)?;
        query_snapshot(&connection, discord_id, guild_id)
    }

    pub fn shop_snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigGearShopSnapshot>, DigGearRuntimeRepositoryError> {
        let connection = Connection::open(&self.database_path)?;
        query_shop_snapshot(&connection, discord_id, guild_id)
    }

    pub fn commit(
        &self,
        request: DigGearRuntimeCommit<'_>,
    ) -> Result<DigGearRuntimeCommitOutcome, DigGearRuntimeRepositoryError> {
        validate_proposed(request.expected, request.proposed)?;
        let mut connection = Connection::open(&self.database_path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = query_snapshot(
            &transaction,
            request.expected.discord_id,
            request.expected.guild_id,
        )?;
        let Some(current) = current else {
            let player = player_exists(
                &transaction,
                request.expected.discord_id,
                request.expected.guild_id,
            )?;
            let tunnel = tunnel_exists(
                &transaction,
                request.expected.discord_id,
                request.expected.guild_id,
            )?;
            transaction.rollback()?;
            return Ok(if !player {
                DigGearRuntimeCommitOutcome::MissingPlayer
            } else if !tunnel {
                DigGearRuntimeCommitOutcome::MissingTunnel
            } else {
                DigGearRuntimeCommitOutcome::Conflict
            });
        };
        if !same_persisted_snapshot(&current, request.expected) {
            transaction.rollback()?;
            return Ok(DigGearRuntimeCommitOutcome::Conflict);
        }

        apply_tunnel(&transaction, request.expected, request.proposed)?;
        apply_gear(&transaction, request.expected, request.proposed)?;
        apply_artifacts(&transaction, request.expected, request.proposed)?;
        apply_balance(&transaction, request)?;
        let action_id = insert_audit(&transaction, request)?;
        transaction.commit()?;
        Ok(DigGearRuntimeCommitOutcome::Applied(
            DigGearRuntimeReceipt {
                action_id,
                balance_after: request.proposed.balance,
            },
        ))
    }
}

fn query_shop_snapshot(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<DigGearShopSnapshot>, DigGearRuntimeRepositoryError> {
    let balance = connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(balance) = balance else {
        return Ok(None);
    };
    let tunnel = connection
        .query_row(
            "SELECT depth,max_depth,prestige_level,pickaxe_tier,
                    COALESCE(relic_trim_notice,0),COALESCE(cavein_free_streak,0)
               FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok(Tunnel {
                    depth: row.get(0)?,
                    max_depth: row.get(1)?,
                    prestige_level: row.get(2)?,
                    pickaxe_tier: row.get(3)?,
                    relic_trim_notice: row.get::<_, i64>(4)? != 0,
                    cavein_free_streak: row.get(5)?,
                })
            },
        )
        .optional()?;
    let equipped_weapon_tier = connection
        .query_row(
            "SELECT tier FROM dig_gear
              WHERE discord_id=?1 AND guild_id=?2
                AND slot='weapon' AND equipped=1
              ORDER BY id DESC LIMIT 1",
            params![discord_id, guild_id],
            |row| {
                let tier = row.get::<_, i64>(0)?;
                u8::try_from(tier).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, tier))
            },
        )
        .optional()?;
    let inventory_count = connection.query_row(
        "SELECT COUNT(*) FROM dig_inventory
          WHERE discord_id=?1 AND guild_id=?2",
        params![discord_id, guild_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(Some(DigGearShopSnapshot {
        discord_id,
        guild_id,
        balance,
        tunnel,
        equipped_weapon_tier,
        inventory_count: usize::try_from(inventory_count).unwrap_or(usize::MAX),
    }))
}

fn query_snapshot(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<DigGearRuntimeSnapshot>, DigGearRuntimeRepositoryError> {
    let balance = connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(balance) = balance else {
        return Ok(None);
    };
    let tunnel = connection
        .query_row(
            "SELECT depth,max_depth,prestige_level,pickaxe_tier,
                    COALESCE(relic_trim_notice,0),COALESCE(cavein_free_streak,0)
               FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| {
                Ok(Tunnel {
                    depth: row.get(0)?,
                    max_depth: row.get(1)?,
                    prestige_level: row.get(2)?,
                    pickaxe_tier: row.get(3)?,
                    relic_trim_notice: row.get::<_, i64>(4)? != 0,
                    cavein_free_streak: row.get(5)?,
                })
            },
        )
        .optional()?;
    let Some(tunnel) = tunnel else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT id,slot,tier,durability,equipped,acquired_at,source,item_id
           FROM dig_gear WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    let gear = statement
        .query_map(params![discord_id, guild_id], |row| {
            let slot = row.get::<_, String>(1)?;
            let Some(slot) = GearSlot::parse(&slot) else {
                return Err(rusqlite::Error::InvalidColumnType(
                    1,
                    "slot".to_owned(),
                    rusqlite::types::Type::Text,
                ));
            };
            let tier = row.get::<_, i64>(2)?;
            let Ok(tier) = u8::try_from(tier) else {
                return Err(rusqlite::Error::IntegralValueOutOfRange(2, tier));
            };
            let durability = row.get::<_, i64>(3)?;
            let Ok(durability) = i32::try_from(durability) else {
                return Err(rusqlite::Error::IntegralValueOutOfRange(3, durability));
            };
            Ok(GearPiece {
                id: row.get(0)?,
                discord_id,
                guild_id,
                slot,
                tier,
                durability,
                equipped: row.get::<_, i64>(4)? != 0,
                acquired_at: row.get(5)?,
                source: row.get(6)?,
                item_id: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut statement = connection.prepare(
        "SELECT id,artifact_id,is_relic,equipped FROM dig_artifacts
          WHERE discord_id=?1 AND guild_id=?2 ORDER BY id",
    )?;
    let artifacts = statement
        .query_map(params![discord_id, guild_id], |row| {
            Ok(Artifact {
                id: row.get(0)?,
                discord_id,
                guild_id,
                artifact_id: row.get(1)?,
                is_relic: row.get::<_, i64>(2)? != 0,
                equipped: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let next_gear_id = next_id(connection, "dig_gear")?;
    let next_artifact_id = next_id(connection, "dig_artifacts")?;
    let clock = gear
        .iter()
        .map(|piece| piece.acquired_at)
        .max()
        .unwrap_or(0)
        .max(current_unix_floor(connection)?)
        .saturating_add(1);
    Ok(Some(DigGearRuntimeSnapshot {
        discord_id,
        guild_id,
        tunnel,
        balance,
        gear,
        artifacts,
        next_gear_id,
        next_artifact_id,
        clock,
    }))
}

fn same_persisted_snapshot(left: &DigGearRuntimeSnapshot, right: &DigGearRuntimeSnapshot) -> bool {
    left.discord_id == right.discord_id
        && left.guild_id == right.guild_id
        && left.tunnel == right.tunnel
        && left.balance == right.balance
        && left.gear == right.gear
        && left.artifacts == right.artifacts
        && left.next_gear_id == right.next_gear_id
        && left.next_artifact_id == right.next_artifact_id
}

fn next_id(connection: &Connection, table: &str) -> Result<i64, rusqlite::Error> {
    let sql = match table {
        "dig_gear" => "SELECT COALESCE(MAX(id),0)+1 FROM dig_gear",
        "dig_artifacts" => "SELECT COALESCE(MAX(id),0)+1 FROM dig_artifacts",
        _ => unreachable!("fixed Dig gear table"),
    };
    connection.query_row(sql, [], |row| row.get(0))
}

fn current_unix_floor(connection: &Connection) -> Result<i64, rusqlite::Error> {
    connection.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |row| {
        row.get(0)
    })
}

fn validate_proposed(
    expected: &DigGearRuntimeSnapshot,
    proposed: &DigGearRuntimeState,
) -> Result<(), DigGearRuntimeRepositoryError> {
    if proposed.balance < 0 {
        return Err(DigGearRuntimeRepositoryError::InvalidState(
            "balance cannot be negative",
        ));
    }
    let expected_gear = expected
        .gear
        .iter()
        .map(|piece| (piece.id, piece))
        .collect::<BTreeMap<_, _>>();
    let proposed_gear = proposed
        .gear
        .iter()
        .map(|piece| (piece.id, piece))
        .collect::<BTreeMap<_, _>>();
    if proposed_gear.len() != proposed.gear.len()
        || !expected_gear
            .keys()
            .all(|id| proposed_gear.contains_key(id))
    {
        return Err(DigGearRuntimeRepositoryError::InvalidState(
            "gear identities cannot be duplicated or removed",
        ));
    }
    let mut equipped = BTreeSet::new();
    for piece in &proposed.gear {
        if piece.discord_id != expected.discord_id
            || piece.guild_id != expected.guild_id
            || piece.durability < 0
            || piece.durability > piece.max_durability()
        {
            return Err(DigGearRuntimeRepositoryError::InvalidState(
                "gear ownership or durability is invalid",
            ));
        }
        if piece.equipped && !equipped.insert(piece.slot) {
            return Err(DigGearRuntimeRepositoryError::InvalidState(
                "only one gear piece per slot may be equipped",
            ));
        }
        if let Some(original) = expected_gear.get(&piece.id) {
            if piece.discord_id != original.discord_id
                || piece.guild_id != original.guild_id
                || piece.slot != original.slot
                || piece.tier != original.tier
                || piece.acquired_at != original.acquired_at
                || piece.source != original.source
                || piece.item_id != original.item_id
            {
                return Err(DigGearRuntimeRepositoryError::InvalidState(
                    "persisted gear identity fields are immutable",
                ));
            }
        } else if piece.id < expected.next_gear_id {
            return Err(DigGearRuntimeRepositoryError::InvalidState(
                "new gear identity precedes the repository sequence",
            ));
        }
    }
    let expected_artifacts = expected
        .artifacts
        .iter()
        .map(|artifact| (artifact.id, artifact))
        .collect::<BTreeMap<_, _>>();
    if expected_artifacts.len() != proposed.artifacts.len() {
        return Err(DigGearRuntimeRepositoryError::InvalidState(
            "gear management cannot add or remove artifacts",
        ));
    }
    for artifact in &proposed.artifacts {
        let Some(original) = expected_artifacts.get(&artifact.id) else {
            return Err(DigGearRuntimeRepositoryError::InvalidState(
                "artifact identity is invalid",
            ));
        };
        if artifact.discord_id != original.discord_id
            || artifact.guild_id != original.guild_id
            || artifact.artifact_id != original.artifact_id
            || artifact.is_relic != original.is_relic
        {
            return Err(DigGearRuntimeRepositoryError::InvalidState(
                "artifact identity fields are immutable",
            ));
        }
    }
    if proposed.tunnel.depth != expected.tunnel.depth
        || proposed.tunnel.max_depth != expected.tunnel.max_depth
        || proposed.tunnel.prestige_level != expected.tunnel.prestige_level
        || proposed.tunnel.cavein_free_streak != expected.tunnel.cavein_free_streak
    {
        return Err(DigGearRuntimeRepositoryError::InvalidState(
            "gear management cannot mutate progression",
        ));
    }
    Ok(())
}

fn apply_tunnel(
    transaction: &Transaction<'_>,
    expected: &DigGearRuntimeSnapshot,
    proposed: &DigGearRuntimeState,
) -> Result<(), DigGearRuntimeRepositoryError> {
    let changed = transaction.execute(
        "UPDATE tunnels SET pickaxe_tier=?1,relic_trim_notice=?2
          WHERE discord_id=?3 AND guild_id=?4 AND pickaxe_tier=?5
            AND COALESCE(relic_trim_notice,0)=?6",
        params![
            i64::from(proposed.tunnel.pickaxe_tier),
            i64::from(proposed.tunnel.relic_trim_notice),
            expected.discord_id,
            expected.guild_id,
            i64::from(expected.tunnel.pickaxe_tier),
            i64::from(expected.tunnel.relic_trim_notice),
        ],
    )?;
    if changed != 1 {
        return Err(DigGearRuntimeRepositoryError::InvalidState(
            "tunnel CAS changed after snapshot",
        ));
    }
    Ok(())
}

fn apply_gear(
    transaction: &Transaction<'_>,
    expected: &DigGearRuntimeSnapshot,
    proposed: &DigGearRuntimeState,
) -> Result<(), DigGearRuntimeRepositoryError> {
    let originals = expected
        .gear
        .iter()
        .map(|piece| (piece.id, piece))
        .collect::<BTreeMap<_, _>>();
    // Clear first so the partial unique index permits a slot swap.
    transaction.execute(
        "UPDATE dig_gear SET equipped=0 WHERE discord_id=?1 AND guild_id=?2",
        params![expected.discord_id, expected.guild_id],
    )?;
    for piece in &proposed.gear {
        if originals.contains_key(&piece.id) {
            let changed = transaction.execute(
                "UPDATE dig_gear SET durability=?1,equipped=?2 WHERE id=?3
                   AND discord_id=?4 AND guild_id=?5",
                params![
                    piece.durability,
                    i64::from(piece.equipped),
                    piece.id,
                    expected.discord_id,
                    expected.guild_id,
                ],
            )?;
            if changed != 1 {
                return Err(DigGearRuntimeRepositoryError::InvalidState(
                    "gear row disappeared during commit",
                ));
            }
        } else {
            transaction.execute(
                "INSERT INTO dig_gear
                 (id,discord_id,guild_id,slot,tier,durability,equipped,
                  acquired_at,source,item_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    piece.id,
                    piece.discord_id,
                    piece.guild_id,
                    piece.slot.as_str(),
                    i64::from(piece.tier),
                    piece.durability,
                    i64::from(piece.equipped),
                    piece.acquired_at,
                    piece.source,
                    piece.item_id,
                ],
            )?;
        }
    }
    Ok(())
}

fn apply_artifacts(
    transaction: &Transaction<'_>,
    expected: &DigGearRuntimeSnapshot,
    proposed: &DigGearRuntimeState,
) -> Result<(), DigGearRuntimeRepositoryError> {
    for artifact in &proposed.artifacts {
        let changed = transaction.execute(
            "UPDATE dig_artifacts SET equipped=?1 WHERE id=?2
               AND discord_id=?3 AND guild_id=?4",
            params![
                i64::from(artifact.equipped),
                artifact.id,
                expected.discord_id,
                expected.guild_id,
            ],
        )?;
        if changed != 1 {
            return Err(DigGearRuntimeRepositoryError::InvalidState(
                "artifact row disappeared during commit",
            ));
        }
    }
    Ok(())
}

fn apply_balance(
    transaction: &Transaction<'_>,
    request: DigGearRuntimeCommit<'_>,
) -> Result<(), DigGearRuntimeRepositoryError> {
    if request.proposed.balance == request.expected.balance {
        return Ok(());
    }
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
         (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,'dig',?1,'gear',?2,'dig gear cost',?3)",
        params![
            request.expected.discord_id,
            request.action_type,
            request.detail_json
        ],
    )?;
    let changed = transaction.execute(
        "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
          WHERE discord_id=?2 AND guild_id=?3 AND COALESCE(jopacoin_balance,0)=?4",
        params![
            request.proposed.balance,
            request.expected.discord_id,
            request.expected.guild_id,
            request.expected.balance,
        ],
    )?;
    if changed != 1 {
        return Err(DigGearRuntimeRepositoryError::InvalidState(
            "player balance CAS changed after snapshot",
        ));
    }
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn insert_audit(
    transaction: &Transaction<'_>,
    request: DigGearRuntimeCommit<'_>,
) -> Result<i64, rusqlite::Error> {
    let delta = request
        .proposed
        .balance
        .saturating_sub(request.expected.balance);
    transaction.execute(
        "INSERT INTO dig_actions
         (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
          jc_delta,detail,created_at)
         VALUES (?1,?2,NULL,?3,?4,?4,?5,?6,?7)",
        params![
            request.expected.guild_id,
            request.expected.discord_id,
            request.action_type,
            request.expected.tunnel.depth,
            delta,
            request.detail_json,
            request.now,
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn player_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
        params![discord_id, guild_id],
        |row| row.get(0),
    )
}

fn tunnel_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tunnels WHERE discord_id=?1 AND guild_id=?2)",
        params![discord_id, guild_id],
        |row| row.get(0),
    )
}

#[cfg(test)]
#[path = "dig_gear_runtime/tests.rs"]
mod tests;
