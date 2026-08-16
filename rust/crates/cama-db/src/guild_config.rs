//! SQLite implementation of the guild configuration store.

use std::path::{Path, PathBuf};

use cama_domain::guild_config::{GuildConfig, GuildConfigStore};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::open_runtime_connection;

/// Repository that reads and writes Python's existing `guild_config` table.
#[derive(Clone, Debug)]
pub struct GuildConfigRepository {
    path: PathBuf,
    ai_features_default: bool,
}

impl GuildConfigRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>, ai_features_default: bool) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            ai_features_default,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }
}

impl GuildConfigStore for GuildConfigRepository {
    type Error = rusqlite::Error;

    fn get_config(&self, guild_id: i64) -> Result<Option<GuildConfig>, Self::Error> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT guild_id, league_id, auto_enrich_matches, ai_features_enabled, \
                 created_at, updated_at FROM guild_config WHERE guild_id = ?1",
                [guild_id],
                |row| {
                    Ok(GuildConfig {
                        guild_id: row.get(0)?,
                        league_id: row.get(1)?,
                        auto_enrich_matches: sqlite_truthy(row, 2)?.unwrap_or(false),
                        ai_features_enabled: sqlite_truthy(row, 3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()
    }

    fn set_league_id(&self, guild_id: i64, league_id: i64) -> Result<(), Self::Error> {
        self.connection()?.execute(
            "INSERT INTO guild_config (guild_id, league_id) VALUES (?1, ?2) \
             ON CONFLICT(guild_id) DO UPDATE SET league_id = ?2, updated_at = CURRENT_TIMESTAMP",
            params![guild_id, league_id],
        )?;
        Ok(())
    }

    fn get_league_id(&self, guild_id: i64) -> Result<Option<i64>, Self::Error> {
        self.connection()?
            .query_row(
                "SELECT league_id FROM guild_config WHERE guild_id = ?1",
                [guild_id],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
    }

    fn set_auto_enrich(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error> {
        let enabled = i64::from(enabled);
        self.connection()?.execute(
            "INSERT INTO guild_config (guild_id, auto_enrich_matches) VALUES (?1, ?2) \
             ON CONFLICT(guild_id) DO UPDATE SET auto_enrich_matches = ?2, \
             updated_at = CURRENT_TIMESTAMP",
            params![guild_id, enabled],
        )?;
        Ok(())
    }

    fn get_auto_enrich(&self, guild_id: i64) -> Result<bool, Self::Error> {
        let stored = self
            .connection()?
            .query_row(
                "SELECT auto_enrich_matches FROM guild_config WHERE guild_id = ?1",
                [guild_id],
                |row| sqlite_truthy(row, 0),
            )
            .optional()?;
        // Python defaults only a missing row to true; bool(NULL) is false.
        Ok(stored.is_none_or(|enabled| enabled.unwrap_or(false)))
    }

    fn set_ai_enabled(&self, guild_id: i64, enabled: bool) -> Result<(), Self::Error> {
        let enabled = i64::from(enabled);
        self.connection()?.execute(
            "INSERT INTO guild_config (guild_id, ai_features_enabled) VALUES (?1, ?2) \
             ON CONFLICT(guild_id) DO UPDATE SET ai_features_enabled = ?2, \
             updated_at = CURRENT_TIMESTAMP",
            params![guild_id, enabled],
        )?;
        Ok(())
    }

    fn get_ai_enabled(&self, guild_id: i64) -> Result<bool, Self::Error> {
        let stored = self
            .connection()?
            .query_row(
                "SELECT ai_features_enabled FROM guild_config WHERE guild_id = ?1",
                [guild_id],
                |row| sqlite_truthy(row, 0),
            )
            .optional()?;
        Ok(stored.flatten().unwrap_or(self.ai_features_default))
    }
}

/// Reproduce Python truthiness for dynamically typed SQLite values.
fn sqlite_truthy(row: &Row<'_>, index: usize) -> Result<Option<bool>, rusqlite::Error> {
    Ok(match row.get_ref(index)? {
        ValueRef::Null => None,
        ValueRef::Integer(value) => Some(value != 0),
        ValueRef::Real(value) => Some(value != 0.0),
        ValueRef::Text(value) | ValueRef::Blob(value) => Some(!value.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::GuildConfigRepository;
    use cama_domain::guild_config::GuildConfigStore;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    fn repository() -> (NamedTempFile, GuildConfigRepository) {
        let file = NamedTempFile::new().expect("temporary guild config database");
        let connection = Connection::open(file.path()).expect("open guild config database");
        connection
            .execute_batch(
                "CREATE TABLE guild_config (
                    guild_id INTEGER PRIMARY KEY,
                    league_id INTEGER,
                    auto_enrich_matches INTEGER DEFAULT 1,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    ai_features_enabled INTEGER DEFAULT 0
                );",
            )
            .expect("create Python-compatible guild_config table");
        drop(connection);
        let repository = GuildConfigRepository::new(file.path(), false);
        (file, repository)
    }

    #[test]
    fn test_get_defaults_when_missing() {
        let (_file, repository) = repository();
        assert_eq!(repository.get_config(123).unwrap(), None);
        assert_eq!(repository.get_league_id(123).unwrap(), None);
        assert!(repository.get_auto_enrich(123).unwrap());
    }

    #[test]
    fn test_set_and_get_league_id() {
        let (_file, repository) = repository();
        repository.set_league_id(123, 777).unwrap();
        assert_eq!(repository.get_league_id(123).unwrap(), Some(777));
        let config = repository.get_config(123).unwrap().expect("config row");
        assert_eq!(config.guild_id, 123);
        assert_eq!(config.league_id, Some(777));
    }

    #[test]
    fn test_auto_enrich_toggle_preserves_league_id() {
        let (_file, repository) = repository();
        repository.set_league_id(55, 999).unwrap();
        repository.set_auto_enrich(55, false).unwrap();
        assert!(!repository.get_auto_enrich(55).unwrap());
        assert_eq!(
            repository
                .get_config(55)
                .unwrap()
                .expect("config row")
                .league_id,
            Some(999)
        );
        repository.set_auto_enrich(55, true).unwrap();
        assert!(repository.get_auto_enrich(55).unwrap());
    }

    #[test]
    fn test_get_config_includes_ai_features_enabled() {
        let (_file, repository) = repository();
        repository.set_ai_enabled(88, true).unwrap();
        let config = repository.get_config(88).unwrap().expect("config row");
        assert_eq!(config.ai_features_enabled, Some(true));
    }

    #[test]
    fn python_shaped_and_rust_writes_interoperate() {
        let (file, repository) = repository();
        let connection = Connection::open(file.path()).expect("open raw SQLite connection");
        connection
            .execute(
                "INSERT INTO guild_config (guild_id, league_id, auto_enrich_matches, \
                 ai_features_enabled) VALUES (1, 44, 0, NULL)",
                [],
            )
            .expect("insert Python-shaped row");
        drop(connection);

        let config = repository.get_config(1).unwrap().expect("read raw row");
        assert_eq!(config.league_id, Some(44));
        assert!(!config.auto_enrich_matches);
        assert!(!repository.get_ai_enabled(1).unwrap());

        repository.set_league_id(1, 55).unwrap();
        let connection = Connection::open(file.path()).expect("reopen raw SQLite connection");
        assert_eq!(
            connection
                .query_row(
                    "SELECT league_id FROM guild_config WHERE guild_id = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            55
        );
    }

    #[test]
    fn legacy_dynamic_boolean_values_follow_python_truthiness() {
        let (file, repository) = repository();
        let connection = Connection::open(file.path()).expect("open raw SQLite connection");
        connection
            .execute(
                "INSERT INTO guild_config (guild_id, auto_enrich_matches, ai_features_enabled) \
                 VALUES (1, NULL, NULL), (2, 'false', ''), (3, 0.0, X'01')",
                [],
            )
            .expect("insert dynamically typed rows");
        drop(connection);

        assert!(!repository.get_auto_enrich(1).unwrap());
        assert!(!repository.get_ai_enabled(1).unwrap());
        assert!(repository.get_auto_enrich(2).unwrap());
        assert!(!repository.get_ai_enabled(2).unwrap());
        assert!(!repository.get_auto_enrich(3).unwrap());
        assert!(repository.get_ai_enabled(3).unwrap());
    }
}
