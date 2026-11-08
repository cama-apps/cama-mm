//! Existing-schema persistence for wagers carried between Dig pinnacle phases.
//!
//! The Rust schema manager owns schema and migrations. This adapter only opens
//! an already-migrated database through
//! [`crate::open_runtime_connection`]. Carry-marker changes and balance
//! settlement share one `BEGIN IMMEDIATE` transaction guarded by the exact
//! tunnel snapshot, so retries cannot settle a stake twice.

use std::path::{Path, PathBuf};

use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, types::Value,
};
use thiserror::Error;

use crate::open_runtime_connection;

/// Regular boss boundaries followed by the pinnacle boundary.
pub const CARRY_BOUNDARIES: [i64; 8] = [25, 50, 75, 100, 150, 200, 275, 350];

/// A player tunnel scoped to one Discord guild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarryTunnelKey {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
}

/// The persisted wager field, retaining malformed values for policy rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistedWager {
    Missing,
    Integer(i64),
    Invalid,
}

/// Carry-relevant fields extracted from one JSON boss-progress entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarryProgressEntry {
    pub status: Option<String>,
    pub has_wager_marker: bool,
    pub wager: PersistedWager,
    pub has_risk_marker: bool,
    pub risk_tier: Option<String>,
}

impl CarryProgressEntry {
    #[must_use]
    pub const fn has_either_marker(&self) -> bool {
        self.has_wager_marker || self.has_risk_marker
    }
}

/// Exact persisted state used by app policy and later CAS writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarryTunnelSnapshot {
    pub key: CarryTunnelKey,
    pub depth: i64,
    pub pinnacle_phase: i64,
    pub boss_progress: String,
    pub balance: Option<i64>,
    pub entries: Vec<(i64, CarryProgressEntry)>,
}

impl CarryTunnelSnapshot {
    #[must_use]
    pub fn entry(&self, boundary: i64) -> Option<&CarryProgressEntry> {
        self.entries
            .iter()
            .find_map(|(candidate, entry)| (*candidate == boundary).then_some(entry))
    }

    /// Whether the wager carried at `boundary` was escrowed when it was
    /// carried. A carry without the marker predates escrow.
    #[must_use]
    pub fn carried_wager_escrowed(&self, boundary: i64) -> bool {
        serde_json::from_str::<serde_json::Value>(&self.boss_progress)
            .ok()
            .and_then(|progress| {
                progress
                    .get(boundary.to_string())
                    .and_then(|entry| entry.get(CARRIED_WAGER_ESCROWED_KEY))
                    .and_then(serde_json::Value::as_bool)
            })
            .unwrap_or(false)
    }
}

/// Progress-entry marker set beside `carried_wager` when the carried stake is
/// already escrowed.
pub const CARRIED_WAGER_ESCROWED_KEY: &str = "carried_wager_escrowed";

/// JSON mutation applied when stale carry markers are cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarryProgressMutation<'a> {
    /// Remove only the carry fields, optionally changing the encounter status.
    Clear {
        boundary: i64,
        status_after: Option<&'a str>,
    },
}

impl CarryProgressMutation<'_> {
    const fn boundary(self) -> i64 {
        match self {
            Self::Clear { boundary, .. } => boundary,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClearCarryOutcome {
    Cleared { boss_progress: String },
    Conflict,
    MissingTunnel,
}

#[derive(Debug, Error)]
pub enum DigCarryWagerRepositoryError {
    #[error("unknown Dig carry boundary: {0}")]
    UnknownBoundary(i64),
    #[error("Dig carry arithmetic exceeded SQLite's signed 64-bit range")]
    AmountOverflow,
    #[error("Dig carry SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Existing-schema repository for carried pinnacle wagers.
#[derive(Clone, Debug)]
pub struct DigCarryWagerRepository {
    path: PathBuf,
}

impl DigCarryWagerRepository {
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

    fn connection(&self) -> Result<crate::RuntimeConnection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Read one exact tunnel snapshot without creating or migrating schema.
    pub fn snapshot(
        &self,
        key: CarryTunnelKey,
    ) -> Result<Option<CarryTunnelSnapshot>, DigCarryWagerRepositoryError> {
        let connection = self.connection()?;
        query_snapshot(&connection, key).map_err(Into::into)
    }

    /// Best-effort cleanup for stale regular-boss markers, guarded by the
    /// exact state that caused the cleanup decision.
    pub fn clear_carry_markers(
        &self,
        snapshot: &CarryTunnelSnapshot,
        boundary: i64,
    ) -> Result<ClearCarryOutcome, DigCarryWagerRepositoryError> {
        validate_boundary(boundary)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let progress_after = mutate_progress(
            &transaction,
            &snapshot.boss_progress,
            CarryProgressMutation::Clear {
                boundary,
                status_after: None,
            },
        )?;
        let guild_id = Self::normalize_guild_id(snapshot.key.guild_id);
        let changed = transaction.execute(
            "UPDATE tunnels SET boss_progress = ?1
             WHERE discord_id = ?2 AND guild_id = ?3
               AND COALESCE(depth, 0) = ?4
               AND COALESCE(pinnacle_phase, 1) = ?5
               AND COALESCE(boss_progress, '{}') = ?6",
            params![
                progress_after,
                snapshot.key.discord_id,
                guild_id,
                snapshot.depth,
                snapshot.pinnacle_phase,
                snapshot.boss_progress
            ],
        )?;
        if changed == 1 {
            transaction.commit()?;
            return Ok(ClearCarryOutcome::Cleared {
                boss_progress: progress_after,
            });
        }
        let exists = tunnel_exists(&transaction, snapshot.key.discord_id, guild_id)?;
        transaction.commit()?;
        Ok(if exists {
            ClearCarryOutcome::Conflict
        } else {
            ClearCarryOutcome::MissingTunnel
        })
    }
}

fn validate_boundary(boundary: i64) -> Result<(), DigCarryWagerRepositoryError> {
    if CARRY_BOUNDARIES.contains(&boundary) {
        Ok(())
    } else {
        Err(DigCarryWagerRepositoryError::UnknownBoundary(boundary))
    }
}

fn query_snapshot(
    connection: &Connection,
    key: CarryTunnelKey,
) -> Result<Option<CarryTunnelSnapshot>, rusqlite::Error> {
    let guild_id = DigCarryWagerRepository::normalize_guild_id(key.guild_id);
    let row = connection
        .query_row(
            "SELECT COALESCE(depth, 0), COALESCE(pinnacle_phase, 1),
                    COALESCE(boss_progress, '{}')
             FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2",
            params![key.discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((depth, pinnacle_phase, boss_progress)) = row else {
        return Ok(None);
    };
    let balance = connection
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![key.discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()?;
    let valid_progress =
        if connection.query_row("SELECT json_valid(?1)", params![boss_progress], |row| {
            row.get::<_, bool>(0)
        })? {
            boss_progress.as_str()
        } else {
            "{}"
        };
    let entries = CARRY_BOUNDARIES
        .into_iter()
        .map(|boundary| query_progress_entry(connection, valid_progress, boundary))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(CarryTunnelSnapshot {
        key,
        depth,
        pinnacle_phase,
        boss_progress,
        balance,
        entries,
    }))
}

fn query_progress_entry(
    connection: &Connection,
    progress: &str,
    boundary: i64,
) -> Result<(i64, CarryProgressEntry), rusqlite::Error> {
    let entry_path = format!(r#"$."{boundary}""#);
    let status_path = format!(r#"{entry_path}.status"#);
    let wager_path = format!(r#"{entry_path}.carried_wager"#);
    let risk_path = format!(r#"{entry_path}.carried_risk_tier"#);
    let (entry_type, entry_value, status_value, wager_type, wager_value, risk_type, risk_value) =
        connection.query_row(
            "SELECT json_type(?1, ?2), json_extract(?1, ?2),
                    json_extract(?1, ?3),
                    json_type(?1, ?4), json_extract(?1, ?4),
                    json_type(?1, ?5), json_extract(?1, ?5)",
            params![progress, entry_path, status_path, wager_path, risk_path],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Value>(1)?,
                    row.get::<_, Value>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Value>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Value>(6)?,
                ))
            },
        )?;
    let status_source = if entry_type.as_deref() == Some("object") {
        status_value
    } else {
        entry_value
    };
    let has_wager_marker = wager_type.is_some();
    let wager = if has_wager_marker {
        persisted_wager(wager_value)
    } else {
        PersistedWager::Missing
    };
    Ok((
        boundary,
        CarryProgressEntry {
            status: value_as_string(status_source),
            has_wager_marker,
            wager,
            has_risk_marker: risk_type.is_some(),
            risk_tier: value_as_string(risk_value),
        },
    ))
}

fn persisted_wager(value: Value) -> PersistedWager {
    match value {
        Value::Integer(value) => PersistedWager::Integer(value),
        Value::Real(value) if value.is_finite() => PersistedWager::Integer(value as i64),
        Value::Text(value) => value
            .parse::<i64>()
            .map_or(PersistedWager::Invalid, PersistedWager::Integer),
        _ => PersistedWager::Invalid,
    }
}

fn value_as_string(value: Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value),
        Value::Integer(value) => Some(value.to_string()),
        Value::Real(value) => Some(value.to_string()),
        _ => None,
    }
}

fn mutate_progress(
    transaction: &Transaction<'_>,
    raw_progress: &str,
    mutation: CarryProgressMutation<'_>,
) -> Result<String, rusqlite::Error> {
    let boundary = mutation.boundary();
    let entry_path = format!(r#"$."{boundary}""#);
    let status_path = format!(r#"{entry_path}.status"#);
    let wager_path = format!(r#"{entry_path}.carried_wager"#);
    let risk_path = format!(r#"{entry_path}.carried_risk_tier"#);
    let escrow_path = format!(r#"{entry_path}.{CARRIED_WAGER_ESCROWED_KEY}"#);
    let base = if transaction.query_row("SELECT json_valid(?1)", params![raw_progress], |row| {
        row.get::<_, bool>(0)
    })? {
        raw_progress.to_owned()
    } else {
        "{}".to_owned()
    };
    let entry_type = transaction.query_row(
        "SELECT json_type(?1, ?2)",
        params![base, entry_path],
        |row| row.get::<_, Option<String>>(0),
    )?;
    let normalized = match entry_type.as_deref() {
        Some("object") => base,
        Some("text") => transaction.query_row(
            "SELECT json_set(?1, ?2, json_object('status', json_extract(?1, ?2)))",
            params![base, entry_path],
            |row| row.get(0),
        )?,
        _ => transaction.query_row(
            "SELECT json_set(?1, ?2, json('{}'))",
            params![base, entry_path],
            |row| row.get(0),
        )?,
    };
    match mutation {
        CarryProgressMutation::Clear { status_after, .. } => {
            let cleared: String = transaction.query_row(
                "SELECT json_remove(?1, ?2, ?3, ?4)",
                params![normalized, wager_path, risk_path, escrow_path],
                |row| row.get(0),
            )?;
            if let Some(status_after) = status_after {
                transaction.query_row(
                    "SELECT json_set(?1, ?2, ?3)",
                    params![cleared, status_path, status_after],
                    |row| row.get(0),
                )
            } else {
                Ok(cleared)
            }
        }
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

#[cfg(test)]
#[path = "dig_carry_wager/tests.rs"]
mod tests;
