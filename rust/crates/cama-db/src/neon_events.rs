//! Existing-schema persistence for Neon Terminal one-time events.
//!
//! Production code only opens an initialized, migrated database. In particular,
//! this adapter never creates or alters `neon_events`; test schema setup stays
//! with the Rust migration authority.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NeonEventRecord {
    pub discord_id: i64,
    pub guild_id: i64,
    pub event_type: String,
}

#[derive(Debug, Error)]
pub enum NeonEventRepositoryError {
    #[error("Neon event SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct NeonEventRepository {
    path: PathBuf,
}

impl NeonEventRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    pub fn load_one_time_events(&self) -> Result<Vec<NeonEventRecord>, NeonEventRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT discord_id,COALESCE(guild_id,0),event_type
             FROM neon_events WHERE one_time=1",
        )?;
        statement
            .query_map([], |row| {
                Ok(NeonEventRecord {
                    discord_id: row.get(0)?,
                    guild_id: row.get(1)?,
                    event_type: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn check_one_time_event(
        &self,
        discord_id: i64,
        guild_id: i64,
        event_type: &str,
    ) -> Result<bool, NeonEventRepositoryError> {
        self.connection()?
            .query_row(
                "SELECT 1 FROM neon_events
                 WHERE discord_id=?1 AND COALESCE(guild_id,0)=?2
                   AND event_type=?3 AND one_time=1 LIMIT 1",
                params![discord_id, guild_id, event_type],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(Into::into)
    }

    /// Preserve Python's idempotent `INSERT OR IGNORE` behavior.
    pub fn persist_one_time_event(
        &self,
        discord_id: i64,
        guild_id: i64,
        event_type: &str,
        layer: i64,
        fired_at: i64,
    ) -> Result<(), NeonEventRepositoryError> {
        self.connection()?.execute(
            "INSERT OR IGNORE INTO neon_events
             (discord_id,guild_id,event_type,layer,one_time,fired_at)
             VALUES (?1,?2,?3,?4,1,?5)",
            params![discord_id, guild_id, event_type, layer, fired_at],
        )?;
        Ok(())
    }

    /// Atomically claim a one-time event without requiring a schema change.
    ///
    /// The legacy table intentionally has no uniqueness constraint because it
    /// also stores non-one-time history.  A short IMMEDIATE transaction makes
    /// the debrief/recovery claim race-safe while preserving that existing
    /// schema and Python's INSERT-OR-IGNORE semantics.
    pub fn claim_one_time_event(
        &self,
        discord_id: i64,
        guild_id: i64,
        event_type: &str,
        layer: i64,
        fired_at: i64,
    ) -> Result<bool, NeonEventRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM neon_events
                 WHERE discord_id=?1 AND COALESCE(guild_id,0)=?2
                   AND event_type=?3 AND one_time=1 LIMIT 1",
                params![discord_id, guild_id, event_type],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO neon_events
             (discord_id,guild_id,event_type,layer,one_time,fired_at)
             VALUES (?1,?2,?3,?4,1,?5)",
            params![discord_id, guild_id, event_type, layer, fired_at],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Release a claim when Discord delivery fails, allowing READY recovery
    /// to retry the committed match presentation.
    pub fn release_one_time_event(
        &self,
        discord_id: i64,
        guild_id: i64,
        event_type: &str,
    ) -> Result<(), NeonEventRepositoryError> {
        self.connection()?.execute(
            "DELETE FROM neon_events
             WHERE discord_id=?1 AND COALESCE(guild_id,0)=?2
               AND event_type=?3 AND one_time=1",
            params![discord_id, guild_id, event_type],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::NamedTempFile;

    use super::NeonEventRepository;
    use crate::test_support::copy_migrated_database;

    #[test]
    fn concurrent_one_time_claim_has_exactly_one_winner() {
        let database = NamedTempFile::new().expect("temporary database");
        copy_migrated_database(database.path()).expect("canonical schema");
        let path = database.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    NeonEventRepository::new(path)
                        .claim_one_time_event(101, 202, "dig:303:bonus:wheel", 404, 505)
                        .expect("concurrent one-time claim")
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();

        let mut results = handles
            .into_iter()
            .map(|handle| handle.join().expect("claim thread"))
            .collect::<Vec<_>>();
        results.sort_unstable();
        assert_eq!(results, vec![false, true]);
    }
}
