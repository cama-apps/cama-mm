//! Existing-schema data repair for the bounded Dig relic-loadout rollout.
//!
//! Python remains the sole schema, migration, and live-runtime authority. This
//! adapter does not add columns or register a migration: it only reapplies the
//! post-migration data repair against an already-upgraded schema. Flagging
//! affected tunnels and trimming old equipped relics commit together under
//! `BEGIN IMMEDIATE`.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};
use thiserror::Error;

use crate::open_runtime_connection;

pub const RELIC_LOADOUT_MAX: i64 = 6;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RelicCapRepairReport {
    pub flagged_tunnels: usize,
    pub unequipped_relics: usize,
}

#[derive(Debug, Error)]
pub enum DigRelicReworkRepositoryError {
    #[error("Dig relic-rework SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigRelicReworkRepository {
    path: PathBuf,
}

impl DigRelicReworkRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Reapply only the data portion of Python's relic-cap rollout.
    ///
    /// The schema must already expose `tunnels.relic_trim_notice` and the
    /// Python-owned `dig_artifacts` columns. Affected miners are identified
    /// before trimming, then only their newest six equipped relic rows (highest
    /// database ids) remain equipped. Owned inventory rows are never deleted.
    pub fn repair_relic_loadout_cap(
        &self,
    ) -> Result<RelicCapRepairReport, DigRelicReworkRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let flagged_tunnels = transaction.execute(
            "UPDATE tunnels SET relic_trim_notice = 1
             WHERE (
                 SELECT COUNT(*) FROM dig_artifacts d
                 WHERE d.is_relic = 1 AND d.equipped = 1
                   AND d.discord_id = tunnels.discord_id
                   AND d.guild_id = tunnels.guild_id
             ) > 6",
            [],
        )?;
        let unequipped_relics = transaction.execute(
            "UPDATE dig_artifacts SET equipped = 0
             WHERE id IN (
                 SELECT id FROM (
                     SELECT id, ROW_NUMBER() OVER (
                         PARTITION BY discord_id, guild_id ORDER BY id DESC
                     ) AS rn
                     FROM dig_artifacts WHERE is_relic = 1 AND equipped = 1
                 ) WHERE rn > 6
             )",
            [],
        )?;
        transaction.commit()?;
        Ok(RelicCapRepairReport {
            flagged_tunnels,
            unequipped_relics,
        })
    }
}

#[cfg(test)]
#[path = "dig_relic_rework/tests.rs"]
mod tests;
