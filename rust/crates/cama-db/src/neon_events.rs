//! Existing-schema persistence for Neon Terminal one-time events.
//!
//! Production code only opens an admitted, migrated database.  In particular,
//! this adapter never creates or alters `neon_events`; test schema setup stays
//! with the Rust migration authority.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
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
        let transaction = connection.transaction()?;
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
