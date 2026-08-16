//! Read-only views over Python's durable `pending_matches` payloads.
//!
//! Lobby admission and reset consult every pending match in the guild.
//! Unknown or absent lobby kinds remain legacy-unscoped so reset policy
//! conservatively guards both queues.

use std::path::{Path, PathBuf};

use rusqlite::params;
use serde_json::Value;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingLobbyMatch {
    pub pending_match_id: i64,
    pub guild_id: i64,
    pub lobby_kind: Option<String>,
    pub shuffle_message_jump_url: Option<String>,
    pub player_ids: Vec<i64>,
}

#[derive(Debug, Error)]
pub enum PendingLobbyRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("pending match {pending_match_id} contains malformed JSON: {message}")]
    MalformedPayload {
        pending_match_id: i64,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct PendingLobbyRepository {
    path: PathBuf,
}

impl PendingLobbyRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn all_for_guild(
        &self,
        guild_id: i64,
    ) -> Result<Vec<PendingLobbyMatch>, PendingLobbyRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT pending_match_id, payload
               FROM pending_matches
              WHERE guild_id = ?1
              ORDER BY created_at, pending_match_id",
        )?;
        statement
            .query_map(params![guild_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (pending_match_id, payload) = row?;
                decode_pending_match(pending_match_id, guild_id, &payload)
            })
            .collect()
    }

    pub fn for_player(
        &self,
        guild_id: i64,
        player_id: i64,
    ) -> Result<Option<PendingLobbyMatch>, PendingLobbyRepositoryError> {
        Ok(self
            .all_for_guild(guild_id)?
            .into_iter()
            .find(|pending| pending.player_ids.contains(&player_id)))
    }
}

fn decode_pending_match(
    pending_match_id: i64,
    guild_id: i64,
    payload: &str,
) -> Result<PendingLobbyMatch, PendingLobbyRepositoryError> {
    let value = serde_json::from_str::<Value>(payload).map_err(|error| {
        PendingLobbyRepositoryError::MalformedPayload {
            pending_match_id,
            message: error.to_string(),
        }
    })?;
    let object =
        value
            .as_object()
            .ok_or_else(|| PendingLobbyRepositoryError::MalformedPayload {
                pending_match_id,
                message: "root value is not an object".to_owned(),
            })?;
    let mut player_ids = Vec::new();
    for field in ["radiant_team_ids", "dire_team_ids"] {
        let Some(values) = object.get(field) else {
            continue;
        };
        let values =
            values
                .as_array()
                .ok_or_else(|| PendingLobbyRepositoryError::MalformedPayload {
                    pending_match_id,
                    message: format!("{field} is not an array"),
                })?;
        for player_id in values {
            let player_id = player_id.as_i64().ok_or_else(|| {
                PendingLobbyRepositoryError::MalformedPayload {
                    pending_match_id,
                    message: format!("{field} contains a non-integer player id"),
                }
            })?;
            if !player_ids.contains(&player_id) {
                player_ids.push(player_id);
            }
        }
    }
    let lobby_kind = object
        .get("lobby_kind")
        .and_then(Value::as_str)
        .filter(|kind| matches!(*kind, "open" | "lowskill"))
        .map(str::to_owned);
    let shuffle_message_jump_url = object
        .get("shuffle_message_jump_url")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(PendingLobbyMatch {
        pending_match_id,
        guild_id,
        lobby_kind,
        shuffle_message_jump_url,
        player_ids,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn reads_scoped_and_legacy_rows_without_cross_guild_leakage() {
        let file = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(file.path()).expect("open database");
        connection
            .execute_batch(
                "CREATE TABLE pending_matches (
                    pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
                 INSERT INTO pending_matches(guild_id,payload) VALUES
                    (7, '{\"lobby_kind\":\"open\",\"radiant_team_ids\":[11,12],\"dire_team_ids\":[21]}'),
                    (7, '{\"radiant_team_ids\":[31],\"dire_team_ids\":[],\"shuffle_message_jump_url\":\"https://discord.test/31\"}'),
                    (8, '{\"lobby_kind\":\"lowskill\",\"radiant_team_ids\":[99],\"dire_team_ids\":[]}');",
            )
            .expect("create fixture");
        drop(connection);

        let repository = PendingLobbyRepository::new(file.path());
        let rows = repository.all_for_guild(7).expect("read pending rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].lobby_kind.as_deref(), Some("open"));
        assert_eq!(rows[0].player_ids, [11, 12, 21]);
        assert_eq!(rows[1].lobby_kind, None);
        assert_eq!(
            repository
                .for_player(7, 31)
                .expect("read player")
                .expect("pending player")
                .shuffle_message_jump_url
                .as_deref(),
            Some("https://discord.test/31")
        );
        assert!(repository.for_player(7, 99).expect("read player").is_none());
    }

    #[test]
    fn malformed_payload_fails_closed() {
        let file = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(file.path()).expect("open database");
        connection
            .execute_batch(
                "CREATE TABLE pending_matches (
                    pending_match_id INTEGER PRIMARY KEY,
                    guild_id INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
                 INSERT INTO pending_matches VALUES (4, 7, 'not-json', CURRENT_TIMESTAMP);",
            )
            .expect("create fixture");
        drop(connection);

        assert!(matches!(
            PendingLobbyRepository::new(file.path()).all_for_guild(7),
            Err(PendingLobbyRepositoryError::MalformedPayload {
                pending_match_id: 4,
                ..
            })
        ));
    }
}
