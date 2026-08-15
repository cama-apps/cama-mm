//! Existing-schema persistence for atomic Dig relic recycling.
//!
//! Python remains the schema, migration, and live-runtime authority. Production code here opens
//! only an existing database through [`crate::open_runtime_connection`]. The single mutation is a
//! `BEGIN IMMEDIATE` transaction that revalidates three selected inventory rows, deletes exactly
//! those rows, and inserts the output relic. All DDL is confined to disposable tests.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, TransactionBehavior, params, params_from_iter};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelicOwner {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistedRelicRow {
    pub row_id: i64,
    pub artifact_id: String,
    pub is_relic: bool,
    pub equipped: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecycledRelicRow {
    pub row_id: i64,
    pub artifact_id: String,
    pub found_at: i64,
}

#[derive(Debug, Error)]
pub enum DigRelicRecyclingRepositoryError {
    #[error("Dig relic-recycling SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Expected exactly three eligible relic rows.")]
    ExpectedThreeEligibleRelicRows,
}

#[derive(Clone, Debug)]
pub struct DigRelicRecyclingRepository {
    path: PathBuf,
}

impl DigRelicRecyclingRepository {
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

    pub fn relic_rows(
        &self,
        owner: RelicOwner,
    ) -> Result<Vec<PersistedRelicRow>, DigRelicRecyclingRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, artifact_id, is_relic, equipped
             FROM dig_artifacts
             WHERE discord_id = ?1 AND guild_id = ?2
             ORDER BY id",
        )?;
        statement
            .query_map(
                params![owner.discord_id, Self::normalize_guild_id(owner.guild_id)],
                |row| {
                    Ok(PersistedRelicRow {
                        row_id: row.get(0)?,
                        artifact_id: row.get(1)?,
                        is_relic: row.get(2)?,
                        equipped: row.get(3)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Replace exactly three still-owned, unequipped relic rows with one output relic.
    ///
    /// The output is intentionally not checked for uniqueness. Recycling is the duplicate sink's
    /// inverse: canonical behavior permits rolling a relic the player already owns.
    pub fn atomic_recycle_relics(
        &self,
        owner: RelicOwner,
        selected_row_ids: &[i64],
        output_artifact_id: &str,
        found_at: i64,
    ) -> Result<RecycledRelicRow, DigRelicRecyclingRepositoryError> {
        let distinct = selected_row_ids.iter().copied().collect::<BTreeSet<_>>();
        if selected_row_ids.len() != 3 || distinct.len() != 3 {
            return Err(DigRelicRecyclingRepositoryError::ExpectedThreeEligibleRelicRows);
        }

        let guild_id = Self::normalize_guild_id(owner.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let selected = {
            let mut values = vec![Value::Integer(owner.discord_id), Value::Integer(guild_id)];
            values.extend(selected_row_ids.iter().copied().map(Value::Integer));
            let mut statement = transaction.prepare(
                "SELECT id FROM dig_artifacts
                 WHERE discord_id = ? AND guild_id = ?
                   AND is_relic = 1 AND equipped = 0
                   AND id IN (?, ?, ?)
                 ORDER BY id",
            )?;
            statement
                .query_map(params_from_iter(values), |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if selected.len() != 3 {
            return Err(DigRelicRecyclingRepositoryError::ExpectedThreeEligibleRelicRows);
        }

        let mut values = vec![Value::Integer(owner.discord_id), Value::Integer(guild_id)];
        values.extend(selected_row_ids.iter().copied().map(Value::Integer));
        let deleted = transaction.execute(
            "DELETE FROM dig_artifacts
             WHERE discord_id = ? AND guild_id = ?
               AND is_relic = 1 AND equipped = 0
               AND id IN (?, ?, ?)",
            params_from_iter(values),
        )?;
        if deleted != 3 {
            return Err(DigRelicRecyclingRepositoryError::ExpectedThreeEligibleRelicRows);
        }

        transaction.execute(
            "INSERT INTO dig_artifacts
                 (discord_id, guild_id, artifact_id, found_at, is_relic, equipped)
             VALUES (?1, ?2, ?3, ?4, 1, 0)",
            params![owner.discord_id, guild_id, output_artifact_id, found_at],
        )?;
        let row_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(RecycledRelicRow {
            row_id,
            artifact_id: output_artifact_id.to_owned(),
            found_at,
        })
    }
}

#[cfg(test)]
#[path = "dig_relic_recycling/tests.rs"]
mod tests;
