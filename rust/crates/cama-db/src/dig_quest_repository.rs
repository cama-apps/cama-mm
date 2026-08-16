//! Existing-schema persistence for per-(player, guild) Dig quest progress.
//!
//! It consumes the Python-owned `dig_quests` table and never creates or
//! migrates it. Startup migration remains the only schema authority. The
//! repository's transaction boundaries mirror `repositories.dig_quest_repository`
//! so an active quest and its completed-list update are committed together.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use thiserror::Error;

/// Durable quest state for one player in one guild.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestState {
    pub discord_id: i64,
    pub guild_id: i64,
    pub active_quest_id: Option<String>,
    pub active_quest_step: Option<i64>,
    pub completed_quests: Vec<String>,
    pub last_updated_at: Option<i64>,
}

impl QuestState {
    /// Whether the requested quest is the currently active stage chain.
    #[must_use]
    pub fn is_active(&self, quest_id: &str) -> bool {
        self.active_quest_id.as_deref() == Some(quest_id) && self.active_quest_step.is_some()
    }

    /// Whether this quest was completed previously in this guild.
    #[must_use]
    pub fn has_completed(&self, quest_id: &str) -> bool {
        self.completed_quests
            .iter()
            .any(|completed| completed == quest_id)
    }
}

#[derive(Debug, Error)]
pub enum DigQuestRepositoryError {
    #[error(
        "player {discord_id} guild {guild_id} already has active quest {active_quest_id:?}; refusing to overwrite with {requested_quest_id:?}"
    )]
    ActiveQuestConflict {
        discord_id: i64,
        guild_id: i64,
        active_quest_id: String,
        requested_quest_id: String,
    },
    #[error("Dig quest SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Dig quest JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Repository over an already-migrated `dig_quests` table.
#[derive(Clone, Debug)]
pub struct DigQuestRepository {
    path: PathBuf,
}

impl DigQuestRepository {
    /// Construct an adapter. Construction performs no I/O and no migration.
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Python normalizes a missing guild to the global guild bucket `0`.
    #[must_use]
    pub const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
        match guild_id {
            Some(guild_id) => guild_id,
            None => 0,
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        // Keep the same runtime SQLite settings as cama-db's shared opener,
        // while making this unadmitted file independently testable. The
        // eventual admission can replace this helper with
        // `crate::open_runtime_connection` without changing the repository
        // contract.
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", false)?;
        Ok(connection)
    }

    /// Read a state row. Missing rows return an empty state rather than an
    /// error, exactly as Python's repository does.
    pub fn get_state(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<QuestState, DigQuestRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT active_quest_id, active_quest_step,
                        completed_quests, last_updated_at
                   FROM dig_quests
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;

        let Some((active_quest_id, active_quest_step, completed_raw, last_updated_at)) = row else {
            return Ok(QuestState {
                discord_id,
                guild_id,
                active_quest_id: None,
                active_quest_step: None,
                completed_quests: Vec::new(),
                last_updated_at: None,
            });
        };
        Ok(QuestState {
            discord_id,
            guild_id,
            active_quest_id,
            active_quest_step,
            completed_quests: decode_completed_quests(completed_raw.as_deref()),
            last_updated_at,
        })
    }

    /// Start or advance a quest, refusing to replace another active quest.
    pub fn set_active(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        quest_id: &str,
        step: i64,
        now: i64,
    ) -> Result<QuestState, DigQuestRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT active_quest_id, completed_quests
                   FROM dig_quests
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;

        let completed_quests = if let Some((active_quest_id, completed_raw)) = existing {
            if let Some(active_quest_id) = active_quest_id
                && active_quest_id != quest_id
            {
                return Err(DigQuestRepositoryError::ActiveQuestConflict {
                    discord_id,
                    guild_id,
                    active_quest_id,
                    requested_quest_id: quest_id.to_owned(),
                });
            }
            transaction.execute(
                "UPDATE dig_quests
                    SET active_quest_id = ?1,
                        active_quest_step = ?2,
                        last_updated_at = ?3
                  WHERE discord_id = ?4 AND guild_id = ?5",
                params![quest_id, step, now, discord_id, guild_id],
            )?;
            decode_completed_quests(completed_raw.as_deref())
        } else {
            transaction.execute(
                "INSERT INTO dig_quests
                    (discord_id, guild_id, active_quest_id,
                     active_quest_step, completed_quests, last_updated_at)
                 VALUES (?1, ?2, ?3, ?4, '[]', ?5)",
                params![discord_id, guild_id, quest_id, step, now],
            )?;
            Vec::new()
        };
        transaction.commit()?;

        Ok(QuestState {
            discord_id,
            guild_id,
            active_quest_id: Some(quest_id.to_owned()),
            active_quest_step: Some(step),
            completed_quests,
            last_updated_at: Some(now),
        })
    }

    /// Clear active state and append a completion exactly once.
    pub fn complete_quest(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        quest_id: &str,
        now: i64,
    ) -> Result<QuestState, DigQuestRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT completed_quests
                   FROM dig_quests
                  WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, guild_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;

        let completed_raw = existing.as_ref().and_then(|raw| raw.as_deref());
        let mut completed_quests = decode_completed_quests(completed_raw);
        if !completed_quests
            .iter()
            .any(|completed| completed == quest_id)
        {
            completed_quests.push(quest_id.to_owned());
        }
        let completed_json = serde_json::to_string(&completed_quests)?;
        if existing.is_some() {
            transaction.execute(
                "UPDATE dig_quests
                    SET active_quest_id = NULL,
                        active_quest_step = NULL,
                        completed_quests = ?1,
                        last_updated_at = ?2
                  WHERE discord_id = ?3 AND guild_id = ?4",
                params![completed_json, now, discord_id, guild_id],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO dig_quests
                    (discord_id, guild_id, active_quest_id,
                     active_quest_step, completed_quests, last_updated_at)
                 VALUES (?1, ?2, NULL, NULL, ?3, ?4)",
                params![discord_id, guild_id, completed_json, now],
            )?;
        }
        transaction.commit()?;

        Ok(QuestState {
            discord_id,
            guild_id,
            active_quest_id: None,
            active_quest_step: None,
            completed_quests,
            last_updated_at: Some(now),
        })
    }

    /// Clear an active quest without marking it complete.
    pub fn abandon_active(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<QuestState, DigQuestRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE dig_quests
                SET active_quest_id = NULL,
                    active_quest_step = NULL,
                    last_updated_at = ?1
              WHERE discord_id = ?2 AND guild_id = ?3",
            params![now, discord_id, guild_id],
        )?;
        transaction.commit()?;
        self.get_state(discord_id, Some(guild_id))
    }

    /// Persist a quest-finale relic in the already-migrated artifact table.
    ///
    /// Quest progression and event settlement remain separate transaction
    /// boundaries in the Python service: completion is recorded first and the
    /// finale reward follows. This focused adapter mirrors that contract and
    /// deliberately performs no schema creation or migration.
    pub fn add_relic_artifact(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        artifact_id: &str,
        found_at: i64,
    ) -> Result<i64, DigQuestRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO dig_artifacts(
                 discord_id, guild_id, artifact_id, found_at, is_relic, equipped
             ) VALUES (?1, ?2, ?3, ?4, 1, 0)",
            params![discord_id, guild_id, artifact_id, found_at],
        )?;
        let artifact_row_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(artifact_row_id)
    }
}

fn decode_completed_quests(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Vec::new();
    };
    let Ok(Value::Array(values)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    values
        .into_iter()
        .map(json_value_as_python_string)
        .collect()
}

fn json_value_as_python_string(value: Value) -> String {
    match value {
        Value::String(value) => value,
        Value::Null => "None".to_owned(),
        Value::Bool(value) => {
            if value {
                "True".to_owned()
            } else {
                "False".to_owned()
            }
        }
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    const PLAYER: i64 = 1;
    const GUILD: i64 = 9_001;
    const NOW: i64 = 1_700_000_000;

    struct Fixture {
        _file: NamedTempFile,
        repository: DigQuestRepository,
    }

    impl Fixture {
        fn new() -> Self {
            let file = NamedTempFile::new().expect("temp database");
            let connection = Connection::open(file.path()).expect("open temp database");
            connection
                .execute_batch(include_str!("../../../schema/canonical_schema.sql"))
                .expect("load canonical migrated schema");
            drop(connection);
            Self {
                repository: DigQuestRepository::new(file.path()),
                _file: file,
            }
        }
    }

    #[test]
    fn get_state_returns_empty_for_unknown_player() {
        let fixture = Fixture::new();
        let state = fixture
            .repository
            .get_state(PLAYER, Some(GUILD))
            .expect("read state");
        assert_eq!(state.active_quest_id, None);
        assert_eq!(state.active_quest_step, None);
        assert!(state.completed_quests.is_empty());
    }

    #[test]
    fn set_active_inserts_new_row() {
        let fixture = Fixture::new();
        let state = fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 1, NOW)
            .expect("set active");
        assert_eq!(state.active_quest_id.as_deref(), Some("agh_trial"));
        assert_eq!(state.active_quest_step, Some(1));
        assert!(state.completed_quests.is_empty());
        assert_eq!(state.last_updated_at, Some(NOW));
    }

    #[test]
    fn set_active_advances_step() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 1, NOW)
            .expect("set stage one");
        let state = fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 2, NOW + 1)
            .expect("advance stage");
        assert_eq!(state.active_quest_step, Some(2));
    }

    #[test]
    fn set_active_refuses_to_overwrite_different_quest() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 3, NOW)
            .expect("set first quest");
        let error = fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "necropolis", 1, NOW + 1)
            .expect_err("different active quest must be rejected");
        assert!(matches!(
            error,
            DigQuestRepositoryError::ActiveQuestConflict { .. }
        ));
    }

    #[test]
    fn complete_quest_clears_active_and_appends_completed() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 5, NOW)
            .expect("set final stage");
        let state = fixture
            .repository
            .complete_quest(PLAYER, Some(GUILD), "agh_trial", NOW + 1)
            .expect("complete quest");
        assert_eq!(state.active_quest_id, None);
        assert_eq!(state.active_quest_step, None);
        assert!(state.has_completed("agh_trial"));
    }

    #[test]
    fn complete_quest_is_idempotent() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 5, NOW)
            .expect("set final stage");
        fixture
            .repository
            .complete_quest(PLAYER, Some(GUILD), "agh_trial", NOW + 1)
            .expect("first completion");
        let state = fixture
            .repository
            .complete_quest(PLAYER, Some(GUILD), "agh_trial", NOW + 2)
            .expect("second completion");
        assert_eq!(
            state
                .completed_quests
                .iter()
                .filter(|quest_id| quest_id.as_str() == "agh_trial")
                .count(),
            1
        );
    }

    #[test]
    fn complete_quest_then_start_new_quest() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 5, NOW)
            .expect("set final stage");
        fixture
            .repository
            .complete_quest(PLAYER, Some(GUILD), "agh_trial", NOW + 1)
            .expect("complete first quest");
        let state = fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "necropolis", 1, NOW + 2)
            .expect("start second quest");
        assert_eq!(state.active_quest_id.as_deref(), Some("necropolis"));
        assert_eq!(state.active_quest_step, Some(1));
        assert!(state.has_completed("agh_trial"));
    }

    #[test]
    fn guild_isolation() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 3, NOW)
            .expect("set source guild quest");
        fixture
            .repository
            .complete_quest(PLAYER, Some(GUILD), "agh_trial", NOW + 1)
            .expect("complete source guild quest");

        let other_guild = GUILD + 1;
        let state = fixture
            .repository
            .get_state(PLAYER, Some(other_guild))
            .expect("read other guild");
        assert_eq!(state.active_quest_id, None);
        assert!(state.completed_quests.is_empty());
        let state = fixture
            .repository
            .set_active(PLAYER, Some(other_guild), "agh_trial", 1, NOW + 2)
            .expect("start same quest in other guild");
        assert_eq!(state.active_quest_id.as_deref(), Some("agh_trial"));
    }

    #[test]
    fn abandon_active_clears_quest_but_keeps_completed_list() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "agh_trial", 5, NOW)
            .expect("set first final stage");
        fixture
            .repository
            .complete_quest(PLAYER, Some(GUILD), "agh_trial", NOW + 1)
            .expect("complete first quest");
        fixture
            .repository
            .set_active(PLAYER, Some(GUILD), "necropolis", 2, NOW + 2)
            .expect("set second quest");

        let state = fixture
            .repository
            .abandon_active(PLAYER, Some(GUILD), NOW + 3)
            .expect("abandon active quest");
        assert_eq!(state.active_quest_id, None);
        assert_eq!(state.active_quest_step, None);
        assert!(state.has_completed("agh_trial"));
    }

    #[test]
    fn quest_state_helpers() {
        let state = QuestState {
            discord_id: PLAYER,
            guild_id: 0,
            active_quest_id: Some("x".to_owned()),
            active_quest_step: Some(2),
            completed_quests: vec!["a".to_owned(), "b".to_owned()],
            last_updated_at: None,
        };
        assert!(state.is_active("x"));
        assert!(!state.is_active("y"));
        assert!(state.has_completed("a"));
        assert!(!state.has_completed("z"));
    }

    #[test]
    fn guild_id_none_normalizes_to_zero() {
        let fixture = Fixture::new();
        fixture
            .repository
            .set_active(PLAYER, None, "agh_trial", 1, NOW)
            .expect("set global guild quest");
        let state = fixture
            .repository
            .get_state(PLAYER, Some(0))
            .expect("read normalized guild");
        assert_eq!(state.active_quest_id.as_deref(), Some("agh_trial"));
    }
}
