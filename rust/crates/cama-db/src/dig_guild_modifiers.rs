//! Existing-schema repository for guild-wide Dig modifiers.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, PartialEq)]
pub struct DigGuildModifier {
    pub modifier_id: String,
    pub expires_at: i64,
    pub payload: Value,
}

#[derive(Debug, Error)]
pub enum DigGuildModifierError {
    #[error("SQLite guild modifier operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigGuildModifierRepository {
    path: PathBuf,
}

impl DigGuildModifierRepository {
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

    pub fn get_active(
        &self,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<Vec<DigGuildModifier>, DigGuildModifierError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT modifier_id,expires_at,payload_json
               FROM dig_guild_modifiers
              WHERE guild_id=?1 AND expires_at>?2
              ORDER BY modifier_id",
        )?;
        let rows =
            statement.query_map(params![Self::normalize_guild_id(guild_id), now], |row| {
                let raw = row.get::<_, Option<String>>(2)?.unwrap_or_default();
                Ok(DigGuildModifier {
                    modifier_id: row.get(0)?,
                    expires_at: row.get(1)?,
                    payload: serde_json::from_str(&raw)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| Value::Object(Default::default())),
                })
            })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn is_active(
        &self,
        guild_id: Option<i64>,
        modifier_id: &str,
        now: i64,
    ) -> Result<bool, DigGuildModifierError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT 1 FROM dig_guild_modifiers
                  WHERE guild_id=?1 AND modifier_id=?2 AND expires_at>?3
                  LIMIT 1",
                params![Self::normalize_guild_id(guild_id), modifier_id, now],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub fn clear_expired(&self, now: i64) -> Result<usize, DigGuildModifierError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let removed = transaction.execute(
            "DELETE FROM dig_guild_modifiers WHERE expires_at<=?1",
            [now],
        )?;
        transaction.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use crate::test_support::copy_migrated_database;

    use super::*;

    const GUILD: i64 = 99_001;
    const OTHER_GUILD: i64 = 99_002;
    const NOW: i64 = 1_800_000_000;

    fn fixture() -> (NamedTempFile, DigGuildModifierRepository) {
        let database = NamedTempFile::new().unwrap();
        copy_migrated_database(database.path()).unwrap();
        let repository = DigGuildModifierRepository::new(database.path());
        (database, repository)
    }

    /// Persist a modifier row the way the live event settlement writes it.
    fn seed(
        database: &NamedTempFile,
        guild_id: i64,
        modifier_id: &str,
        expires_at: i64,
        payload: &str,
    ) {
        Connection::open(database.path())
            .unwrap()
            .execute(
                "INSERT INTO dig_guild_modifiers
                    (guild_id,modifier_id,expires_at,payload_json,created_at)
                 VALUES (?1,?2,?3,?4,?5)",
                params![guild_id, modifier_id, expires_at, payload, NOW],
            )
            .unwrap();
    }

    #[test]
    fn test_set_then_active() {
        let (database, repository) = fixture();
        seed(
            &database,
            GUILD,
            "helltide_active",
            NOW + 600,
            r#"{"tax_per_dig":5}"#,
        );
        assert!(
            repository
                .is_active(Some(GUILD), "helltide_active", NOW)
                .unwrap()
        );
    }

    #[test]
    fn test_inactive_when_unset() {
        let (_database, repository) = fixture();
        assert!(
            !repository
                .is_active(Some(GUILD), "no_such_mod", NOW)
                .unwrap()
        );
    }

    #[test]
    fn test_get_active_returns_payload() {
        let (database, repository) = fixture();
        seed(
            &database,
            GUILD,
            "helltide_active",
            NOW + 600,
            r#"{"tax_per_dig":7}"#,
        );
        let rows = repository.get_active(Some(GUILD), NOW).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].modifier_id, "helltide_active");
        assert_eq!(rows[0].payload["tax_per_dig"], 7);
    }

    #[test]
    fn test_get_active_filters_expired() {
        let (database, repository) = fixture();
        seed(&database, GUILD, "stale", NOW + 600, "{}");
        assert!(
            repository
                .get_active(Some(GUILD), NOW + 600)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_clear_expired_drops_rows() {
        let (database, repository) = fixture();
        seed(&database, GUILD, "stale", NOW + 600, "{}");
        assert_eq!(repository.clear_expired(NOW + 600).unwrap(), 1);
        let count = Connection::open(database.path())
            .unwrap()
            .query_row("SELECT COUNT(*) FROM dig_guild_modifiers", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_modifier_scoped_to_guild() {
        let (database, repository) = fixture();
        seed(&database, GUILD, "helltide_active", NOW + 600, "{}");
        assert!(
            repository
                .is_active(Some(GUILD), "helltide_active", NOW)
                .unwrap()
        );
        assert!(
            !repository
                .is_active(Some(OTHER_GUILD), "helltide_active", NOW)
                .unwrap()
        );
    }
}
