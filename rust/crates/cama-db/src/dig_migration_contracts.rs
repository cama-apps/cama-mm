//! Read-only contracts for Dig schema migrations.
//!
//! The centralized Rust schema manager owns production migration. This module
//! independently validates the post-migration SQLite shape and provides pure,
//! deterministic models of the one-shot data repairs. It deliberately exposes
//! no production DDL or migration-ledger writer itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, params};
use thiserror::Error;

pub const BOSS_BOUNDARIES: [i64; 7] = [25, 50, 75, 100, 150, 200, 275];

const REQUIRED_DIG_MIGRATIONS: [&str; 8] = [
    "add_dig_action_history_indexes",
    "add_dig_action_type_history_index",
    "add_dig_auto_buy_settings",
    "backfill_missing_dig_weapon_gear",
    "clear_active_boss_ids_for_pool_reroll",
    "create_dig_gear_system",
    "reconcile_cumulative_prestige_boss_stat_points",
    "reconcile_post_prestige_boss_stat_points",
];

#[derive(Debug, Error)]
pub enum DigMigrationContractError {
    #[error("database path does not exist or is not a file: {0}")]
    MissingDatabase(PathBuf),
    #[error("SQLite contract audit failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DigMigrationAudit {
    pub missing_migrations: Vec<String>,
    pub missing_columns: Vec<String>,
    pub missing_indexes: Vec<String>,
    pub legacy_indexes: Vec<String>,
    pub malformed_indexes: Vec<String>,
    pub action_type_query_uses_index: bool,
}

impl DigMigrationAudit {
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.missing_migrations.is_empty()
            && self.missing_columns.is_empty()
            && self.missing_indexes.is_empty()
            && self.legacy_indexes.is_empty()
            && self.malformed_indexes.is_empty()
            && self.action_type_query_uses_index
    }
}

/// Audit the post-migration Dig schema without performing a logical write.
pub fn audit_existing_schema(
    path: impl AsRef<Path>,
) -> Result<DigMigrationAudit, DigMigrationContractError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(DigMigrationContractError::MissingDatabase(
            path.to_path_buf(),
        ));
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    audit_connection(&connection).map_err(Into::into)
}

fn audit_connection(connection: &Connection) -> Result<DigMigrationAudit, rusqlite::Error> {
    let mut audit = DigMigrationAudit::default();
    let applied = query_string_set(
        connection,
        "SELECT name FROM schema_migrations ORDER BY name",
    )?;
    audit.missing_migrations = REQUIRED_DIG_MIGRATIONS
        .iter()
        .filter(|name| !applied.contains(**name))
        .map(|name| (*name).to_owned())
        .collect();

    let tunnel_columns = table_columns(connection, "tunnels")?;
    for required in ["auto_buy_torch", "auto_buy_hard_hat"] {
        match tunnel_columns.get(required) {
            Some(Some(default)) if normalize_default(default) == "0" => {}
            Some(None) => audit
                .malformed_indexes
                .push(format!("tunnels.{required}:missing_default")),
            Some(Some(default)) => audit
                .malformed_indexes
                .push(format!("tunnels.{required}:default={default}")),
            None => audit.missing_columns.push(format!("tunnels.{required}")),
        }
    }

    for required in [
        "id",
        "discord_id",
        "guild_id",
        "slot",
        "tier",
        "durability",
        "equipped",
        "acquired_at",
        "source",
    ] {
        if !table_columns(connection, "dig_gear")?.contains_key(required) {
            audit.missing_columns.push(format!("dig_gear.{required}"));
        }
    }

    let action_indexes = index_names(connection, "dig_actions")?;
    for legacy in [
        "idx_dig_actions_guild_actor",
        "idx_dig_actions_guild_target",
    ] {
        if action_indexes.contains(legacy) {
            audit.legacy_indexes.push(legacy.to_owned());
        }
    }
    let expected_action_indexes = [
        (
            "idx_dig_actions_guild_actor_created",
            expected_index_columns(&[
                ("guild_id", false),
                ("actor_id", false),
                ("created_at", true),
            ]),
        ),
        (
            "idx_dig_actions_guild_target_created",
            expected_index_columns(&[
                ("guild_id", false),
                ("target_id", false),
                ("created_at", true),
            ]),
        ),
        (
            "idx_dig_actions_guild_type_created_actor",
            expected_index_columns(&[
                ("guild_id", false),
                ("action_type", false),
                ("created_at", true),
                ("actor_id", false),
            ]),
        ),
    ];
    for (name, expected) in expected_action_indexes {
        if !action_indexes.contains(name) {
            audit.missing_indexes.push(name.to_owned());
        } else if index_columns(connection, name)? != expected {
            audit.malformed_indexes.push(name.to_owned());
        }
    }

    let gear_indexes = index_metadata(connection, "dig_gear")?;
    match gear_indexes.get("idx_dig_gear_player_slot") {
        Some((false, false)) => {
            let expected = expected_index_columns(&[
                ("discord_id", false),
                ("guild_id", false),
                ("slot", false),
            ]);
            if index_columns(connection, "idx_dig_gear_player_slot")? != expected {
                audit
                    .malformed_indexes
                    .push("idx_dig_gear_player_slot".to_owned());
            }
        }
        Some(_) => audit
            .malformed_indexes
            .push("idx_dig_gear_player_slot".to_owned()),
        None => audit
            .missing_indexes
            .push("idx_dig_gear_player_slot".to_owned()),
    }
    match gear_indexes.get("uq_dig_gear_one_equipped_per_slot") {
        Some((true, true)) => {
            let expected = expected_index_columns(&[
                ("discord_id", false),
                ("guild_id", false),
                ("slot", false),
            ]);
            if index_columns(connection, "uq_dig_gear_one_equipped_per_slot")? != expected {
                audit
                    .malformed_indexes
                    .push("uq_dig_gear_one_equipped_per_slot".to_owned());
            }
        }
        Some(_) => audit
            .malformed_indexes
            .push("uq_dig_gear_one_equipped_per_slot".to_owned()),
        None => audit
            .missing_indexes
            .push("uq_dig_gear_one_equipped_per_slot".to_owned()),
    }

    audit.action_type_query_uses_index = query_plan_uses_action_type_index(connection)?;
    Ok(audit)
}

fn normalize_default(value: &str) -> &str {
    value.trim_matches(['(', ')', '\'', '"', ' '])
}

fn query_string_set(
    connection: &Connection,
    sql: &str,
) -> Result<BTreeSet<String>, rusqlite::Error> {
    connection
        .prepare(sql)?
        .query_map([], |row| row.get(0))?
        .collect()
}

fn table_columns(
    connection: &Connection,
    table: &str,
) -> Result<BTreeMap<String, Option<String>>, rusqlite::Error> {
    let sql = format!("PRAGMA table_info('{table}')");
    connection
        .prepare(&sql)?
        .query_map([], |row| Ok((row.get(1)?, row.get(4)?)))?
        .collect()
}

fn index_names(connection: &Connection, table: &str) -> Result<BTreeSet<String>, rusqlite::Error> {
    Ok(index_metadata(connection, table)?.into_keys().collect())
}

fn index_metadata(
    connection: &Connection,
    table: &str,
) -> Result<BTreeMap<String, (bool, bool)>, rusqlite::Error> {
    let sql = format!("PRAGMA index_list('{table}')");
    connection
        .prepare(&sql)?
        .query_map([], |row| {
            Ok((
                row.get(1)?,
                (row.get::<_, i64>(2)? != 0, row.get::<_, i64>(4)? != 0),
            ))
        })?
        .collect()
}

fn index_columns(
    connection: &Connection,
    index: &str,
) -> Result<Vec<(String, bool)>, rusqlite::Error> {
    let sql = format!("PRAGMA index_xinfo('{index}')");
    let raw = connection
        .prepare(&sql)?
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, i64>(5)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(raw
        .into_iter()
        .filter_map(|(name, desc, key)| key.then_some((name?, desc)))
        .collect())
}

fn expected_index_columns(columns: &[(&str, bool)]) -> Vec<(String, bool)> {
    columns
        .iter()
        .map(|(name, descending)| ((*name).to_owned(), *descending))
        .collect()
}

fn query_plan_uses_action_type_index(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let recent_digger_plans = connection
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT DISTINCT actor_id FROM dig_actions
             WHERE guild_id=?1 AND action_type='dig' AND created_at>=?2",
        )?
        .query_map(params![0_i64, 0_i64], |row| row.get::<_, String>(3))?
        .collect::<Result<Vec<_>, _>>()?;
    let boss_kill_plans = connection
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT COUNT(*) FROM dig_actions
             WHERE guild_id=?1 AND action_type='boss_fight' AND created_at>=?2
               AND detail LIKE '%\"won\": true%'",
        )?
        .query_map(params![0_i64, 0_i64], |row| row.get::<_, String>(3))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(recent_digger_plans
        .iter()
        .any(|detail| detail.contains("idx_dig_actions_guild_type_created_actor"))
        && boss_kill_plans
            .iter()
            .any(|detail| detail.contains("idx_dig_actions_guild_type_created_actor")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutoBuySettings {
    pub torch: bool,
    pub hard_hat: bool,
}

impl AutoBuySettings {
    pub fn update(&mut self, torch: bool, hard_hat: bool) {
        self.torch = torch;
        self.hard_hat = hard_hat;
    }
}

/// Clear authored boss IDs only for active object entries.
///
/// Invalid JSON, non-object roots, legacy string entries, and `NULL` are
/// preserved. SQLite JSON1 is used as a compatibility parser; no schema or
/// persistent database is touched.
pub fn clear_active_boss_ids(raw: Option<&str>) -> Result<Option<String>, rusqlite::Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let connection = Connection::open_in_memory()?;
    let valid =
        connection.query_row("SELECT json_valid(?1)", [raw], |row| row.get::<_, bool>(0))?;
    if !valid {
        return Ok(Some(raw.to_owned()));
    }
    let is_object = connection.query_row("SELECT json_type(?1)='object'", [raw], |row| {
        row.get::<_, bool>(0)
    })?;
    if !is_object {
        return Ok(Some(raw.to_owned()));
    }
    connection
        .query_row(
            "SELECT json_group_object(
                 key,
                 json(CASE
                     WHEN type='object'
                      AND json_extract(value, '$.status')='active'
                      AND COALESCE(json_extract(value, '$.boss_id'), '')!=''
                     THEN json_set(value, '$.boss_id', '')
                     WHEN type IN ('object', 'array') THEN value
                     ELSE json_quote(value)
                 END)
             ) FROM json_each(?1)",
            [raw],
            |row| row.get(0),
        )
        .map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwardLedger {
    Legacy(Vec<i64>),
    Scoped {
        prestige_level: i64,
        awards: Vec<i64>,
    },
    Invalid,
}

impl AwardLedger {
    fn awards_for(&self, prestige_level: i64) -> BTreeSet<i64> {
        match self {
            Self::Scoped {
                prestige_level: stored,
                awards,
            } if *stored == prestige_level => awards.iter().copied().collect(),
            Self::Legacy(_) | Self::Scoped { .. } | Self::Invalid => BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatRepairState {
    pub prestige_level: i64,
    pub stat_points: i64,
    pub ledger: AwardLedger,
    pub defeated: BTreeSet<i64>,
}

#[must_use]
pub fn reconcile_post_prestige(mut state: StatRepairState) -> StatRepairState {
    if state.prestige_level <= 0 || state.defeated.is_empty() {
        return state;
    }
    let awarded = state.ledger.awards_for(state.prestige_level);
    let missing = state
        .defeated
        .difference(&awarded)
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return state;
    }
    let expected = 5
        + state.prestige_level * i64::try_from(BOSS_BOUNDARIES.len()).unwrap_or(7)
        + i64::try_from(state.defeated.len()).unwrap_or(0);
    if state.stat_points < expected {
        state.stat_points = state.stat_points.max(5) + i64::try_from(missing.len()).unwrap_or(0);
    }
    state.ledger = AwardLedger::Scoped {
        prestige_level: state.prestige_level,
        awards: awarded.union(&state.defeated).copied().collect(),
    };
    state
}

#[must_use]
pub fn reconcile_cumulative(mut state: StatRepairState) -> StatRepairState {
    if state.prestige_level <= 0 {
        return state;
    }
    let expected = 5
        + state.prestige_level * i64::try_from(BOSS_BOUNDARIES.len()).unwrap_or(7)
        + i64::try_from(state.defeated.len()).unwrap_or(0);
    if state.stat_points >= expected {
        return state;
    }
    let awarded = state.ledger.awards_for(state.prestige_level);
    state.stat_points = expected;
    state.ledger = AwardLedger::Scoped {
        prestige_level: state.prestige_level,
        awards: awarded.union(&state.defeated).copied().collect(),
    };
    state
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelGearSeed {
    pub discord_id: i64,
    pub guild_id: i64,
    pub pickaxe_tier: i64,
    pub last_dig_at: Option<i64>,
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GearBackfillRow {
    pub discord_id: i64,
    pub guild_id: i64,
    pub slot: &'static str,
    pub tier: i64,
    pub durability: i64,
    pub equipped: bool,
    pub acquired_at: i64,
    pub source: &'static str,
}

#[must_use]
pub fn initial_weapon_backfill(
    tunnels: &[TunnelGearSeed],
    migration_already_applied: bool,
    now: i64,
) -> Vec<GearBackfillRow> {
    if migration_already_applied {
        return Vec::new();
    }
    tunnels
        .iter()
        .map(|tunnel| GearBackfillRow {
            discord_id: tunnel.discord_id,
            guild_id: tunnel.guild_id,
            slot: "weapon",
            tier: tunnel.pickaxe_tier,
            durability: 20,
            equipped: true,
            acquired_at: tunnel.last_dig_at.unwrap_or(now),
            source: "migration",
        })
        .collect()
}

#[must_use]
pub fn missing_weapon_backfill(
    tunnels: &[TunnelGearSeed],
    existing_weapon_owners: &BTreeSet<(i64, i64)>,
    now: i64,
) -> Vec<GearBackfillRow> {
    tunnels
        .iter()
        .filter(|tunnel| !existing_weapon_owners.contains(&(tunnel.discord_id, tunnel.guild_id)))
        .map(|tunnel| gear_row(tunnel, now))
        .collect()
}

fn gear_row(tunnel: &TunnelGearSeed, now: i64) -> GearBackfillRow {
    GearBackfillRow {
        discord_id: tunnel.discord_id,
        guild_id: tunnel.guild_id,
        slot: "weapon",
        tier: tunnel.pickaxe_tier,
        durability: 20,
        equipped: true,
        acquired_at: tunnel.last_dig_at.or(tunnel.created_at).unwrap_or(now),
        source: "missing_weapon_backfill",
    }
}

#[cfg(test)]
mod tests;
