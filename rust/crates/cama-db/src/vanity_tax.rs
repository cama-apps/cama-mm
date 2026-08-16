//! Existing-schema persistence for manual vanity-tax enforcement.
//!
//! Python remains the migration authority during the side-by-side rewrite.
//! This adapter therefore opens only an existing SQLite database, never
//! creates schema, and serializes its read-modify-read write with
//! `BEGIN IMMEDIATE` so the returned set is authoritative across runtimes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Debug, Error)]
pub enum VanityTaxRepositoryError {
    #[error("vanity-tax SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("system clock precedes the Unix epoch")]
    ClockBeforeEpoch,
}

/// Persistent manual-enforcement rows from Python's
/// `vanity_tax_enforcements` table.
#[derive(Clone, Debug)]
pub struct VanityTaxRepository {
    path: PathBuf,
}

impl VanityTaxRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(value) => value,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn get_vanity_tax_enforcements(
        &self,
        guild_id: Option<i64>,
    ) -> Result<BTreeSet<i64>, VanityTaxRepositoryError> {
        let connection = self.connection()?;
        query_enforcements(&connection, Self::normalize_guild_id(guild_id))
    }

    /// Force or clear one enforcement and return the authoritative guild set.
    pub fn set_vanity_tax_enforcement(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        enforced: bool,
        actor_id: i64,
    ) -> Result<BTreeSet<i64>, VanityTaxRepositoryError> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| VanityTaxRepositoryError::ClockBeforeEpoch)?
            .as_secs()
            .try_into()
            .unwrap_or(i64::MAX);
        self.set_vanity_tax_enforcement_at(guild_id, discord_id, enforced, actor_id, created_at)
    }

    /// Deterministic form used by adapters that already own a clock.
    pub fn set_vanity_tax_enforcement_at(
        &self,
        guild_id: Option<i64>,
        discord_id: i64,
        enforced: bool,
        actor_id: i64,
        created_at: i64,
    ) -> Result<BTreeSet<i64>, VanityTaxRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if enforced {
            transaction.execute(
                "INSERT INTO vanity_tax_enforcements (
                     guild_id, discord_id, enforced_by, created_at
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(guild_id, discord_id) DO UPDATE SET
                     enforced_by=excluded.enforced_by,
                     created_at=excluded.created_at",
                params![guild_id, discord_id, actor_id, created_at],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM vanity_tax_enforcements
                 WHERE guild_id=?1 AND discord_id=?2",
                params![guild_id, discord_id],
            )?;
        }
        let result = query_enforcements(&transaction, guild_id)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn query_enforcements(
    connection: &Connection,
    guild_id: i64,
) -> Result<BTreeSet<i64>, VanityTaxRepositoryError> {
    let mut statement = connection.prepare(
        "SELECT discord_id FROM vanity_tax_enforcements
         WHERE guild_id=?1 ORDER BY discord_id",
    )?;
    statement
        .query_map([guild_id], |row| row.get(0))?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(Into::into)
}

#[cfg(test)]
#[path = "vanity_tax/tests.rs"]
mod tests;
