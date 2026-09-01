//! Existing-schema persistence for ready-check lobby join timestamps.
//!
//! Python remains the schema and migration authority. This adapter opens an
//! existing database exclusively through [`crate::open_runtime_connection`]
//! and never creates or alters production schema. Test-only fixtures create a
//! disposable `lobby_state` table with the Python-owned shape.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::open_runtime_connection;

const PRUNED_NOTICE_PREFIX: &str = "readycheck:pruned_notice:";

/// Service-facing representation of one row in Python's `lobby_state` table.
#[derive(Clone, Debug, PartialEq)]
pub struct PersistedLobbyState {
    pub lobby_id: i64,
    pub guild_id: i64,
    pub players: Vec<i64>,
    pub conditional_players: Vec<i64>,
    pub status: String,
    pub created_by: i64,
    pub created_at: String,
    pub message_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub thread_id: Option<i64>,
    pub embed_message_id: Option<i64>,
    pub origin_channel_id: Option<i64>,
    pub player_join_times: BTreeMap<i64, f64>,
}

impl PersistedLobbyState {
    #[must_use]
    pub fn open(
        lobby_id: i64,
        guild_id: Option<i64>,
        created_by: i64,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            lobby_id,
            guild_id: ReadycheckRepository::normalize_guild_id(guild_id),
            players: Vec::new(),
            conditional_players: Vec::new(),
            status: "open".to_owned(),
            created_by,
            created_at: created_at.into(),
            message_id: None,
            channel_id: None,
            thread_id: None,
            embed_message_id: None,
            origin_channel_id: None,
            player_join_times: BTreeMap::new(),
        }
    }

    /// Drop the retired conditional queue and any join timestamps that do not
    /// belong to an active regular player. Returns whether the persisted row
    /// changed and should be written back during startup hydration.
    pub fn sanitize_legacy_conditionals(&mut self) -> bool {
        let original_conditionals = self.conditional_players.len();
        let original_join_times = self.player_join_times.len();
        self.conditional_players.clear();
        self.player_join_times
            .retain(|player_id, _| self.players.contains(player_id));
        original_conditionals != 0 || original_join_times != self.player_join_times.len()
    }
}

#[derive(Debug, Error)]
pub enum ReadycheckRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("player_join_times contains a non-finite timestamp for player {0}")]
    NonFiniteTimestamp(i64),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid ready-check removal notice: {0}")]
    InvalidPrunedNotice(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadycheckPrunedNotice {
    pub guild_id: i64,
    pub lobby_id: i64,
    pub channel_id: i64,
    pub player_ids: BTreeSet<i64>,
    delivery_nonce: String,
    key: String,
    value: String,
}

impl ReadycheckPrunedNotice {
    #[must_use]
    pub fn delivery_nonce(&self) -> &str {
        &self.delivery_nonce
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ReadycheckPrunedNoticePayload {
    lobby_id: i64,
    channel_id: i64,
    player_ids: BTreeSet<i64>,
}

/// Fail-open pruned-notice listing: rows that cannot be decoded are reported
/// by key instead of failing the whole listing, so one corrupted or
/// future-format `app_kv` row cannot wedge notice recovery for every guild.
/// Skipped rows are retained in the database for diagnosis, matching how an
/// undeliverable-but-decodable notice is retained rather than destroyed.
#[derive(Debug, Default)]
pub struct PendingPrunedNotices {
    pub notices: Vec<ReadycheckPrunedNotice>,
    pub skipped_keys: Vec<String>,
}

/// Narrow port used by the ready-check application model.
pub trait ReadycheckLobbyStore {
    type Error;

    fn save_lobby_state(&self, state: &PersistedLobbyState) -> Result<(), Self::Error>;
    fn load_lobby_state(
        &self,
        lobby_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PersistedLobbyState>, Self::Error>;

    fn load_all_lobby_states(&self) -> Result<Vec<PersistedLobbyState>, Self::Error>;

    fn clear_lobby_state(&self, lobby_id: i64, guild_id: Option<i64>) -> Result<bool, Self::Error>;
}

#[derive(Clone, Debug)]
pub struct ReadycheckRepository {
    path: PathBuf,
}

impl ReadycheckRepository {
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

    /// Upsert the complete Python lobby row so a timestamp-only change cannot
    /// accidentally discard message routing or legacy compatibility fields.
    pub fn save(&self, state: &PersistedLobbyState) -> Result<(), ReadycheckRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        save_lobby_state(&connection, state)
    }

    /// Commit a stale ready-check sweep and its public notice as one durable
    /// mutation. If either table write fails, neither change is visible.
    pub fn save_with_pruned_notice(
        &self,
        state: &PersistedLobbyState,
        channel_id: i64,
        player_ids: &BTreeSet<i64>,
    ) -> Result<(), ReadycheckRepositoryError> {
        if player_ids.is_empty() {
            return Err(ReadycheckRepositoryError::InvalidPrunedNotice(
                "player list is empty".to_owned(),
            ));
        }
        let payload = ReadycheckPrunedNoticePayload {
            lobby_id: state.lobby_id,
            channel_id,
            player_ids: player_ids.clone(),
        };
        let key = format!(
            "{PRUNED_NOTICE_PREFIX}{}:{:016x}",
            state.lobby_id,
            fastrand::u64(..)
        );
        let value = serde_json::to_string(&payload)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction()?;
        save_lobby_state(&transaction, state)?;
        transaction.execute(
            "INSERT INTO app_kv(guild_id,key,value) VALUES (?1,?2,?3)",
            params![state.guild_id, key, value],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_pruned_notices(
        &self,
    ) -> Result<PendingPrunedNotices, ReadycheckRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id,key,value FROM app_kv
             WHERE key GLOB 'readycheck:pruned_notice:*'
             ORDER BY guild_id,key",
        )?;
        collect_pruned_notices(&mut statement, [])
    }

    pub fn pending_pruned_notices_for(
        &self,
        lobby_id: i64,
        guild_id: i64,
    ) -> Result<PendingPrunedNotices, ReadycheckRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let key_pattern = format!("{PRUNED_NOTICE_PREFIX}{lobby_id}:*");
        let mut statement = connection.prepare(
            "SELECT guild_id,key,value FROM app_kv
             WHERE guild_id=?1 AND key GLOB ?2
             ORDER BY key",
        )?;
        collect_pruned_notices(&mut statement, params![guild_id, key_pattern])
    }

    pub fn acknowledge_pruned_notice(
        &self,
        notice: &ReadycheckPrunedNotice,
    ) -> Result<bool, ReadycheckRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "DELETE FROM app_kv WHERE guild_id=?1 AND key=?2 AND value=?3",
            params![notice.guild_id, notice.key, notice.value],
        )? == 1)
    }

    pub fn load(
        &self,
        lobby_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PersistedLobbyState>, ReadycheckRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT lobby_id, guild_id, players, conditional_players,
                        status, created_by, created_at, message_id, channel_id,
                        thread_id, embed_message_id, origin_channel_id,
                        player_join_times
                   FROM lobby_state
                  WHERE lobby_id = ?1 AND guild_id = ?2",
                params![lobby_id, guild_id],
                persisted_lobby_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn load_all(&self) -> Result<Vec<PersistedLobbyState>, ReadycheckRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT lobby_id, guild_id, players, conditional_players,
                    status, created_by, created_at, message_id, channel_id,
                    thread_id, embed_message_id, origin_channel_id,
                    player_join_times
               FROM lobby_state
              ORDER BY guild_id, lobby_id",
        )?;
        statement
            .query_map([], persisted_lobby_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn clear(
        &self,
        lobby_id: i64,
        guild_id: Option<i64>,
    ) -> Result<bool, ReadycheckRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = open_runtime_connection(&self.path)?;
        Ok(connection.execute(
            "DELETE FROM lobby_state WHERE lobby_id = ?1 AND guild_id = ?2",
            params![lobby_id, guild_id],
        )? != 0)
    }
}

fn collect_pruned_notices(
    statement: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<PendingPrunedNotices, ReadycheckRepositoryError> {
    let rows = statement.query_map(params, |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut listing = PendingPrunedNotices::default();
    for row in rows {
        let (guild_id, key, value) = row?;
        match decode_pruned_notice(guild_id, key.clone(), value) {
            Ok(notice) => listing.notices.push(notice),
            Err(_) => listing.skipped_keys.push(key),
        }
    }
    Ok(listing)
}

fn decode_pruned_notice(
    guild_id: i64,
    key: String,
    value: String,
) -> Result<ReadycheckPrunedNotice, ReadycheckRepositoryError> {
    let payload: ReadycheckPrunedNoticePayload = serde_json::from_str(&value)?;
    let key_parts = key
        .strip_prefix(PRUNED_NOTICE_PREFIX)
        .and_then(|suffix| suffix.split_once(':'));
    let key_lobby_id = key_parts.and_then(|(lobby_id, _)| lobby_id.parse::<i64>().ok());
    let delivery_id = key_parts.map(|(_, delivery_id)| delivery_id);
    if payload.player_ids.is_empty()
        || !matches!(payload.lobby_id, 1 | 2)
        || key_lobby_id != Some(payload.lobby_id)
        || !delivery_id.is_some_and(|delivery_id| {
            delivery_id.len() == 16
                && delivery_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(ReadycheckRepositoryError::InvalidPrunedNotice(key));
    }
    // Discord limits message nonce strings to 25 characters. Keep the
    // descriptive key for durable app_kv identity and use its random u64
    // token in a compact, lobby-scoped wire identity (22 characters).
    let delivery_nonce = format!(
        "rcp:{}:{}",
        payload.lobby_id,
        delivery_id.expect("validated ready-check delivery id")
    );
    Ok(ReadycheckPrunedNotice {
        guild_id,
        lobby_id: payload.lobby_id,
        channel_id: payload.channel_id,
        player_ids: payload.player_ids,
        delivery_nonce,
        key,
        value,
    })
}

fn save_lobby_state(
    connection: &rusqlite::Connection,
    state: &PersistedLobbyState,
) -> Result<(), ReadycheckRepositoryError> {
    let players = encode_i64_array(&state.players);
    let conditional_players = encode_i64_array(&state.conditional_players);
    let player_join_times = encode_join_times(&state.player_join_times)?;
    connection.execute(
        "INSERT INTO lobby_state (
             lobby_id, guild_id, players, conditional_players, status,
             created_by, created_at, message_id, channel_id, thread_id,
             embed_message_id, origin_channel_id, player_join_times
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
         )
         ON CONFLICT(lobby_id, guild_id) DO UPDATE SET
             players = excluded.players,
             conditional_players = excluded.conditional_players,
             status = excluded.status,
             created_by = excluded.created_by,
             created_at = excluded.created_at,
             message_id = excluded.message_id,
             channel_id = excluded.channel_id,
             thread_id = excluded.thread_id,
             embed_message_id = excluded.embed_message_id,
             origin_channel_id = excluded.origin_channel_id,
             player_join_times = excluded.player_join_times,
             updated_at = CURRENT_TIMESTAMP",
        params![
            state.lobby_id,
            state.guild_id,
            players,
            conditional_players,
            state.status,
            state.created_by,
            state.created_at,
            state.message_id,
            state.channel_id,
            state.thread_id,
            state.embed_message_id,
            state.origin_channel_id,
            player_join_times,
        ],
    )?;
    Ok(())
}

impl ReadycheckLobbyStore for ReadycheckRepository {
    type Error = ReadycheckRepositoryError;

    fn save_lobby_state(&self, state: &PersistedLobbyState) -> Result<(), Self::Error> {
        self.save(state)
    }

    fn load_lobby_state(
        &self,
        lobby_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<PersistedLobbyState>, Self::Error> {
        self.load(lobby_id, guild_id)
    }

    fn load_all_lobby_states(&self) -> Result<Vec<PersistedLobbyState>, Self::Error> {
        self.load_all()
    }

    fn clear_lobby_state(&self, lobby_id: i64, guild_id: Option<i64>) -> Result<bool, Self::Error> {
        self.clear(lobby_id, guild_id)
    }
}

fn persisted_lobby_from_row(
    row: &rusqlite::Row<'_>,
) -> Result<PersistedLobbyState, rusqlite::Error> {
    let players = row.get::<_, Option<String>>(2)?.unwrap_or_default();
    let conditional_players = row.get::<_, Option<String>>(3)?.unwrap_or_default();
    let player_join_times = row.get::<_, Option<String>>(12)?.unwrap_or_default();
    Ok(PersistedLobbyState {
        lobby_id: row.get(0)?,
        guild_id: row.get(1)?,
        players: decode_i64_array_or_default(&players),
        conditional_players: decode_i64_array_or_default(&conditional_players),
        status: row.get(4)?,
        created_by: row.get(5)?,
        created_at: row.get(6)?,
        message_id: row.get(7)?,
        channel_id: row.get(8)?,
        thread_id: row.get(9)?,
        embed_message_id: row.get(10)?,
        origin_channel_id: row.get(11)?,
        player_join_times: decode_join_times_or_default(&player_join_times),
    })
}

fn encode_i64_array(values: &[i64]) -> String {
    let body = values
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

fn decode_i64_array_or_default(raw: &str) -> Vec<i64> {
    let raw = raw.trim();
    let Some(body) = raw
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Vec::new();
    };
    if body.trim().is_empty() {
        return Vec::new();
    }
    let values = body
        .split(',')
        .map(|value| value.trim().parse::<i64>())
        .collect::<Result<Vec<_>, _>>();
    values.unwrap_or_default()
}

fn encode_join_times(values: &BTreeMap<i64, f64>) -> Result<String, ReadycheckRepositoryError> {
    let mut fields = Vec::with_capacity(values.len());
    for (player_id, timestamp) in values {
        if !timestamp.is_finite() {
            return Err(ReadycheckRepositoryError::NonFiniteTimestamp(*player_id));
        }
        fields.push(format!("\"{player_id}\":{timestamp}"));
    }
    Ok(format!("{{{}}}", fields.join(",")))
}

fn decode_join_times_or_default(raw: &str) -> BTreeMap<i64, f64> {
    let raw = raw.trim();
    let Some(body) = raw
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    else {
        return BTreeMap::new();
    };
    if body.trim().is_empty() {
        return BTreeMap::new();
    }
    let mut result = BTreeMap::new();
    for field in body.split(',') {
        let Some((key, value)) = field.split_once(':') else {
            return BTreeMap::new();
        };
        let key = key.trim();
        let Some(key) = key.strip_prefix('"').and_then(|key| key.strip_suffix('"')) else {
            return BTreeMap::new();
        };
        let (Ok(player_id), Ok(timestamp)) = (key.parse::<i64>(), value.trim().parse::<f64>())
        else {
            return BTreeMap::new();
        };
        if !timestamp.is_finite() {
            return BTreeMap::new();
        }
        result.insert(player_id, timestamp);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;
    use tempfile::{NamedTempFile, TempDir};

    use super::*;

    fn repository() -> (NamedTempFile, ReadycheckRepository) {
        let file = NamedTempFile::new().expect("create disposable readycheck database");
        let connection = Connection::open(file.path()).expect("open disposable database");
        connection
            .execute_batch(
                "CREATE TABLE lobby_state (
                     lobby_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     players TEXT,
                     conditional_players TEXT DEFAULT '[]',
                     status TEXT,
                     created_by INTEGER,
                     created_at TEXT,
                     message_id INTEGER,
                     channel_id INTEGER,
                     thread_id INTEGER,
                     embed_message_id INTEGER,
                     origin_channel_id INTEGER,
                     player_join_times TEXT DEFAULT '{}',
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (lobby_id, guild_id)
                 );",
            )
            .expect("create Python-owned schema in disposable fixture");
        drop(connection);
        let repository = ReadycheckRepository::new(file.path());
        (file, repository)
    }

    fn state() -> PersistedLobbyState {
        let mut state = PersistedLobbyState::open(1, None, 0, "2026-07-14T12:34:56");
        state.players = vec![100, 200];
        state
    }

    fn guild_state(guild_id: i64, created_by: i64, players: &[i64]) -> PersistedLobbyState {
        let mut state = PersistedLobbyState::open(
            1,
            Some(guild_id),
            created_by,
            format!("2026-07-22T12:{guild_id:02}:00+00:00"),
        );
        state.players = players.to_vec();
        state
    }

    #[test]
    fn test_save_and_load_join_times() {
        let (_file, repository) = repository();
        let mut state = state();
        state.player_join_times = BTreeMap::from([(100, 1_700_000_000.0), (200, 1_700_000_060.0)]);

        repository.save(&state).expect("save lobby timestamps");
        let loaded = repository
            .load(1, None)
            .expect("load lobby timestamps")
            .expect("saved row");

        assert_eq!(loaded.player_join_times, state.player_join_times);
    }

    #[test]
    fn test_load_without_join_times_defaults_empty() {
        let (_file, repository) = repository();
        let state = state();

        repository.save(&state).expect("save lobby default");
        let loaded = repository
            .load(1, None)
            .expect("load lobby default")
            .expect("saved row");

        assert!(loaded.player_join_times.is_empty());
    }

    #[test]
    fn pruned_notice_commit_rolls_back_the_lobby_without_an_outbox() {
        let (_file, repository) = repository();
        let original = state();
        repository.save(&original).expect("save original lobby");
        let mut pruned = original.clone();
        pruned.players = vec![100];

        let error = repository
            .save_with_pruned_notice(&pruned, 333, &BTreeSet::from([200]))
            .expect_err("missing app_kv must reject the atomic sweep");

        assert!(error.to_string().contains("app_kv"));
        assert_eq!(
            repository
                .load(1, None)
                .expect("load lobby after rollback")
                .expect("original lobby"),
            original
        );
    }

    #[test]
    fn pruned_notice_roundtrips_and_acknowledges_by_exact_value() {
        let (file, repository) = repository();
        Connection::open(file.path())
            .expect("open fixture")
            .execute_batch(
                "CREATE TABLE app_kv (
                     guild_id INTEGER NOT NULL,
                     key TEXT NOT NULL,
                     value TEXT NOT NULL,
                     PRIMARY KEY (guild_id, key)
                 );",
            )
            .expect("create existing-schema outbox");
        let mut pruned = guild_state(42, 100, &[100]);
        pruned.thread_id = Some(333);

        repository
            .save_with_pruned_notice(&pruned, 333, &BTreeSet::from([200, 300]))
            .expect("commit lobby and removal notice");
        let notices = repository
            .pending_pruned_notices()
            .expect("load pending notices")
            .notices;

        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].guild_id, 42);
        assert_eq!(notices[0].lobby_id, 1);
        assert_eq!(notices[0].channel_id, 333);
        assert_eq!(notices[0].player_ids, BTreeSet::from([200, 300]));
        assert!(notices[0].delivery_nonce().starts_with("rcp:1:"));
        assert!(
            notices[0].delivery_nonce().len() <= 25,
            "Discord rejects nonce strings longer than 25 characters"
        );
        assert!(
            repository
                .acknowledge_pruned_notice(&notices[0])
                .expect("acknowledge exact notice")
        );
        assert!(
            repository
                .pending_pruned_notices()
                .expect("reload notices")
                .notices
                .is_empty()
        );
    }

    #[test]
    fn pruned_notice_delivery_nonces_cover_both_lobbies_within_discord_limit() {
        let (file, repository) = repository();
        Connection::open(file.path())
            .expect("open fixture")
            .execute_batch(
                "CREATE TABLE app_kv (
                     guild_id INTEGER NOT NULL,
                     key TEXT NOT NULL,
                     value TEXT NOT NULL,
                     PRIMARY KEY (guild_id, key)
                 );",
            )
            .expect("create existing-schema outbox");

        for lobby_id in [1, 2] {
            let mut pruned = guild_state(42, 100, &[100]);
            pruned.lobby_id = lobby_id;
            repository
                .save_with_pruned_notice(&pruned, 333, &BTreeSet::from([200]))
                .expect("commit removal notice");
        }

        let notices = repository
            .pending_pruned_notices()
            .expect("load pending notices")
            .notices;
        assert_eq!(notices.len(), 2);
        for notice in notices {
            assert!(
                notice.delivery_nonce().len() <= 25,
                "lobby {} nonce is too long: {}",
                notice.lobby_id,
                notice.delivery_nonce()
            );
            assert!(
                notice
                    .delivery_nonce()
                    .starts_with(&format!("rcp:{}:", notice.lobby_id))
            );
        }
    }

    #[test]
    fn pruned_notice_listing_skips_undecodable_rows_instead_of_failing() {
        let (file, repository) = repository();
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute_batch(
                "CREATE TABLE app_kv (
                     guild_id INTEGER NOT NULL,
                     key TEXT NOT NULL,
                     value TEXT NOT NULL,
                     PRIMARY KEY (guild_id, key)
                 );",
            )
            .expect("create existing-schema outbox");
        // Undecodable payload plus a decodable payload behind an
        // unrecognizable key: neither may fail the listing for other rows.
        connection
            .execute(
                "INSERT INTO app_kv(guild_id,key,value)
                 VALUES (42,'readycheck:pruned_notice:1:0000000000000bad','not-json')",
                [],
            )
            .expect("insert corrupted notice row");
        connection
            .execute(
                "INSERT INTO app_kv(guild_id,key,value)
                 VALUES (42,'readycheck:pruned_notice:9:future-format',
                         '{\"lobby_id\":9,\"channel_id\":333,\"player_ids\":[200]}')",
                [],
            )
            .expect("insert future-format notice row");
        drop(connection);
        let pruned = guild_state(42, 100, &[100]);
        repository
            .save_with_pruned_notice(&pruned, 333, &BTreeSet::from([200]))
            .expect("commit valid removal notice");

        let listing = repository
            .pending_pruned_notices()
            .expect("corrupt rows must not fail the listing");
        assert_eq!(listing.notices.len(), 1, "valid notice survives");
        assert_eq!(listing.notices[0].player_ids, BTreeSet::from([200]));
        assert_eq!(
            listing.skipped_keys,
            [
                "readycheck:pruned_notice:1:0000000000000bad",
                "readycheck:pruned_notice:9:future-format",
            ]
        );

        let scoped = repository
            .pending_pruned_notices_for(1, 42)
            .expect("corrupt rows must not fail the scoped listing");
        assert_eq!(scoped.notices.len(), 1);
        assert_eq!(
            scoped.skipped_keys,
            ["readycheck:pruned_notice:1:0000000000000bad"]
        );
        // Skipped rows stay behind for diagnosis rather than being deleted.
        assert!(
            repository
                .acknowledge_pruned_notice(&listing.notices[0])
                .expect("acknowledge valid notice")
        );
        assert_eq!(
            repository
                .pending_pruned_notices()
                .expect("reload listing")
                .skipped_keys
                .len(),
            2
        );
    }

    #[test]
    fn test_full_lobby_roundtrip_with_join_times() {
        let (_file, repository) = repository();
        let mut state = state();
        state.conditional_players = vec![300];
        state.message_id = Some(111);
        state.channel_id = Some(222);
        state.thread_id = Some(333);
        state.embed_message_id = Some(444);
        state.origin_channel_id = Some(555);
        state.player_join_times = BTreeMap::from([(100, 100.5), (200, 200.5)]);

        repository.save(&state).expect("save complete lobby state");
        let restored = repository
            .load(1, None)
            .expect("load complete lobby state")
            .expect("saved row");

        assert_eq!(restored, state);
    }

    #[test]
    fn stronger_repository_isolates_signed_ids_and_guilds() {
        let (_file, repository) = repository();
        let mut default = PersistedLobbyState::open(1, None, -7, "default");
        default.players = vec![-7];
        default.player_join_times.insert(-7, 10.25);
        let mut other = PersistedLobbyState::open(1, Some(-42), -7, "other");
        other.players = vec![-7];
        other.player_join_times.insert(-7, 20.5);

        repository.save(&default).expect("save default guild");
        repository.save(&other).expect("save signed guild");

        assert_eq!(
            repository
                .load(1, None)
                .expect("load default")
                .expect("default row")
                .player_join_times,
            BTreeMap::from([(-7, 10.25)])
        );
        assert_eq!(
            repository
                .load(1, Some(-42))
                .expect("load signed guild")
                .expect("signed row")
                .player_join_times,
            BTreeMap::from([(-7, 20.5)])
        );
    }

    #[test]
    fn stronger_repository_refuses_to_create_a_missing_database() {
        let directory = TempDir::new().expect("create temp directory");
        let path = directory.path().join("missing.db");
        let repository = ReadycheckRepository::new(&path);

        assert!(repository.load(1, None).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn stronger_malformed_legacy_json_uses_python_defaults() {
        let (file, repository) = repository();
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute(
                "INSERT INTO lobby_state (
                     lobby_id, guild_id, players, status, created_by, created_at,
                     player_join_times
                 ) VALUES (1, 0, 'not-json', 'open', 0, 'now', '{broken}')",
                [],
            )
            .expect("insert malformed legacy row");
        drop(connection);

        let loaded = repository
            .load(1, None)
            .expect("decode with compatibility defaults")
            .expect("legacy row");

        assert!(loaded.players.is_empty());
        assert!(loaded.player_join_times.is_empty());

        // Keep `fs` exercised so this invariant also checks the fixture is a
        // real seekable SQLite file rather than an in-memory stand-in.
        assert!(fs::metadata(file.path()).expect("fixture metadata").len() > 0);
    }

    #[test]
    fn test_startup_persists_sanitized_legacy_conditional_state() {
        let (_file, repository) = repository();
        let mut legacy = guild_state(42, 1, &[1]);
        legacy.conditional_players = vec![99];
        legacy.player_join_times = BTreeMap::from([(1, 1_000.0), (99, 2_000.0)]);
        legacy.message_id = Some(100);
        legacy.channel_id = Some(200);
        repository.save(&legacy).expect("save legacy lobby row");

        let mut restored = repository
            .load_all()
            .expect("bulk hydrate lobby rows")
            .pop()
            .expect("legacy row");
        assert!(restored.sanitize_legacy_conditionals());
        repository
            .save(&restored)
            .expect("persist sanitized startup state");

        let stored = repository
            .load(1, Some(42))
            .expect("reload sanitized row")
            .expect("row remains open");
        assert!(stored.conditional_players.is_empty());
        assert_eq!(stored.player_join_times, BTreeMap::from([(1, 1_000.0)]));
        assert_eq!(stored.message_id, Some(100));
        assert_eq!(stored.channel_id, Some(200));
    }

    #[test]
    fn test_restart_scenario_preserves_lobby_and_message_state() {
        let (_file, repository) = repository();
        let mut first_session = guild_state(0, 99_999, &[1_001, 1_002]);
        first_session.message_id = Some(111_222_333);
        first_session.channel_id = Some(444_555_666);
        repository
            .save(&first_session)
            .expect("persist first session");

        let mut second_session = repository
            .load(1, None)
            .expect("restart load")
            .expect("open lobby");
        assert_eq!(second_session.created_by, 99_999);
        assert_eq!(second_session.status, "open");
        assert_eq!(second_session.players, vec![1_001, 1_002]);
        second_session.players.push(1_003);
        second_session
            .players
            .retain(|player_id| *player_id != 1_001);
        repository
            .save(&second_session)
            .expect("persist post-restart mutations");

        let third_session = repository
            .load(1, None)
            .expect("second restart load")
            .expect("open lobby");
        assert_eq!(third_session.players, vec![1_002, 1_003]);
        assert_eq!(third_session.message_id, Some(111_222_333));
        assert_eq!(third_session.channel_id, Some(444_555_666));
    }

    #[test]
    fn test_closed_lobby_not_restored_after_restart() {
        let (_file, repository) = repository();
        let mut lobby = guild_state(0, 12_345, &[]);
        lobby.message_id = Some(111);
        lobby.channel_id = Some(222);
        repository.save(&lobby).expect("save lobby");

        assert!(repository.clear(1, None).expect("reset lobby"));
        assert!(repository.load(1, None).expect("restart lookup").is_none());
        assert!(
            repository
                .load_all()
                .expect("bulk restart lookup")
                .is_empty()
        );
    }

    #[test]
    fn test_message_id_edge_cases_persist() {
        let (_file, repository) = repository();
        let mut lobby = guild_state(0, 12_345, &[]);
        lobby.message_id = Some(111);
        repository
            .save(&lobby)
            .expect("save message without channel");

        let mut restarted = repository
            .load(1, None)
            .expect("restart lookup")
            .expect("empty open lobby");
        assert!(restarted.players.is_empty());
        assert_eq!(restarted.message_id, Some(111));
        assert_eq!(restarted.channel_id, None);

        restarted.message_id = Some(333);
        restarted.channel_id = Some(444);
        repository.save(&restarted).expect("update routed message");
        let final_restart = repository
            .load(1, None)
            .expect("final restart")
            .expect("updated lobby");
        assert_eq!(final_restart.message_id, Some(333));
        assert_eq!(final_restart.channel_id, Some(444));
    }

    #[test]
    fn test_lobby_state_is_per_guild() {
        let (_file, repository) = repository();
        repository
            .save(&guild_state(42, 1_001, &[111, 222]))
            .expect("save guild A");
        repository
            .save(&guild_state(84, 2_001, &[333, 444]))
            .expect("save guild B");

        let guild_a = repository
            .load(1, Some(42))
            .expect("load guild A")
            .expect("guild A row");
        let guild_b = repository
            .load(1, Some(84))
            .expect("load guild B")
            .expect("guild B row");
        assert_eq!(guild_a.players, vec![111, 222]);
        assert_eq!(guild_b.players, vec![333, 444]);
        assert!(
            !guild_a
                .players
                .iter()
                .any(|id| guild_b.players.contains(id))
        );
    }

    #[test]
    fn test_reset_one_guild_leaves_other_alone() {
        let (_file, repository) = repository();
        let mut guild_a = guild_state(42, 1, &[111]);
        guild_a.message_id = Some(9_001);
        let mut guild_b = guild_state(84, 2, &[222]);
        guild_b.message_id = Some(9_101);
        repository.save(&guild_a).expect("save guild A");
        repository.save(&guild_b).expect("save guild B");

        assert!(repository.clear(1, Some(42)).expect("reset guild A"));
        assert!(
            repository
                .load(1, Some(42))
                .expect("load reset guild A")
                .is_none()
        );
        assert_eq!(
            repository
                .load(1, Some(84))
                .expect("load guild B")
                .expect("guild B survives"),
            guild_b
        );
    }

    #[test]
    fn test_multi_guild_lobbies_survive_restart() {
        let (_file, repository) = repository();
        let guild_a = guild_state(42, 1_001, &[111, 222]);
        let guild_b = guild_state(84, 2_001, &[333]);
        repository.save(&guild_a).expect("save guild A");
        repository.save(&guild_b).expect("save guild B");

        assert_eq!(
            repository.load_all().expect("bulk restart hydration"),
            vec![guild_a, guild_b]
        );
    }

    #[test]
    fn test_leave_is_scoped_to_guild() {
        let (_file, repository) = repository();
        let mut guild_a = guild_state(42, 1, &[555]);
        let guild_b = guild_state(84, 2, &[555]);
        repository.save(&guild_a).expect("save guild A");
        repository.save(&guild_b).expect("save guild B");

        guild_a.players.clear();
        repository.save(&guild_a).expect("leave guild A");

        assert!(
            repository
                .load(1, Some(42))
                .expect("load guild A")
                .expect("guild A lobby")
                .players
                .is_empty()
        );
        assert_eq!(
            repository
                .load(1, Some(84))
                .expect("load guild B")
                .expect("guild B lobby")
                .players,
            vec![555]
        );
    }
}
