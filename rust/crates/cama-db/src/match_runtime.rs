//! Durable pending-match persistence for the live match runtime.
//!
//! The Python-owned schema stores the complete pending match as JSON in
//! `pending_matches.payload`.  This adapter opens only an already-migrated
//! database through [`crate::open_runtime_connection`]; production code in
//! this module never creates or alters schema.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::open_runtime_connection;

/// One result/abort submission embedded in a pending-match payload.
///
/// The flattened map deliberately retains fields added by a newer runtime so
/// an older Rust binary can read and rewrite the document without erasing
/// them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingMatchSubmission {
    pub result: Option<String>,
    pub is_admin: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Typed form of `pending_matches.payload`.
///
/// Missing fields use the same defaults as Python's `PendingMatchState` and
/// unknown fields survive round trips through `extra`.  The row primary key
/// intentionally lives on [`PendingMatchRecord`] and is never emitted into
/// the JSON payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingMatchState {
    pub radiant_team_ids: Vec<i64>,
    pub dire_team_ids: Vec<i64>,
    pub excluded_player_ids: Vec<i64>,
    pub excluded_conditional_player_ids: Vec<i64>,

    pub radiant_roles: Vec<String>,
    pub dire_roles: Vec<String>,
    pub radiant_value: f64,
    pub dire_value: f64,
    pub value_diff: f64,
    pub first_pick_team: Option<String>,

    pub record_submissions: BTreeMap<i64, PendingMatchSubmission>,

    pub shuffle_timestamp: Option<i64>,
    pub lock_time: Option<i64>,
    pub bet_lock_until: Option<i64>,

    pub betting_mode: String,
    pub is_bomb_pot: bool,
    pub is_openskill_shuffle: bool,
    pub is_draft: bool,
    pub balancing_rating_system: String,
    pub lobby_kind: Option<String>,

    pub bet_seed_reserved: i64,
    pub bet_seed_radiant: i64,
    pub bet_seed_dire: i64,
    pub bet_seed_bonus: i64,
    pub first_game_pool_reserved: i64,

    pub blind_bets_result: Option<Value>,

    pub effective_avoid_ids: Vec<i64>,
    pub effective_deal_ids: Vec<i64>,
    pub exclusion_updates_deferred: bool,
    pub full_exclusion_increment_ids: Vec<i64>,
    pub half_exclusion_increment_ids: Vec<i64>,

    pub shuffle_channel_id: Option<i64>,
    pub shuffle_message_id: Option<i64>,
    pub shuffle_message_jump_url: Option<String>,
    pub cmd_shuffle_channel_id: Option<i64>,
    pub cmd_shuffle_message_id: Option<i64>,
    pub thread_shuffle_message_id: Option<i64>,
    pub thread_shuffle_thread_id: Option<i64>,
    pub origin_channel_id: Option<i64>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for PendingMatchState {
    fn default() -> Self {
        Self {
            radiant_team_ids: Vec::new(),
            dire_team_ids: Vec::new(),
            excluded_player_ids: Vec::new(),
            excluded_conditional_player_ids: Vec::new(),
            radiant_roles: Vec::new(),
            dire_roles: Vec::new(),
            radiant_value: 0.0,
            dire_value: 0.0,
            value_diff: 0.0,
            first_pick_team: None,
            record_submissions: BTreeMap::new(),
            shuffle_timestamp: None,
            lock_time: None,
            bet_lock_until: None,
            betting_mode: "pool".to_owned(),
            is_bomb_pot: false,
            is_openskill_shuffle: false,
            is_draft: false,
            balancing_rating_system: "glicko".to_owned(),
            lobby_kind: None,
            bet_seed_reserved: 0,
            bet_seed_radiant: 0,
            bet_seed_dire: 0,
            bet_seed_bonus: 0,
            first_game_pool_reserved: 0,
            blind_bets_result: None,
            effective_avoid_ids: Vec::new(),
            effective_deal_ids: Vec::new(),
            exclusion_updates_deferred: false,
            full_exclusion_increment_ids: Vec::new(),
            half_exclusion_increment_ids: Vec::new(),
            shuffle_channel_id: None,
            shuffle_message_id: None,
            shuffle_message_jump_url: None,
            cmd_shuffle_channel_id: None,
            cmd_shuffle_message_id: None,
            thread_shuffle_message_id: None,
            thread_shuffle_thread_id: None,
            origin_channel_id: None,
            extra: BTreeMap::new(),
        }
    }
}

impl PendingMatchState {
    /// Actual match participants, excluding players left out of the game.
    #[must_use]
    pub fn participant_ids(&self) -> BTreeSet<i64> {
        self.radiant_team_ids
            .iter()
            .chain(&self.dire_team_ids)
            .copied()
            .collect()
    }

    /// Everyone from the shuffled lobby who may vote to abort.
    #[must_use]
    pub fn full_lobby_player_ids(&self) -> BTreeSet<i64> {
        self.radiant_team_ids
            .iter()
            .chain(&self.dire_team_ids)
            .chain(&self.excluded_player_ids)
            .chain(&self.excluded_conditional_player_ids)
            .copied()
            .collect()
    }

    #[must_use]
    pub fn contains_participant(&self, player_id: i64) -> bool {
        self.radiant_team_ids.contains(&player_id) || self.dire_team_ids.contains(&player_id)
    }

    #[must_use]
    pub fn contains_full_lobby_player(&self, player_id: i64) -> bool {
        self.contains_participant(player_id)
            || self.excluded_player_ids.contains(&player_id)
            || self.excluded_conditional_player_ids.contains(&player_id)
    }
}

/// Row metadata kept outside the JSON payload.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingMatchRecord {
    pub pending_match_id: i64,
    pub guild_id: i64,
    pub state: PendingMatchState,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingBettingExtension {
    pub pending_match: PendingMatchRecord,
    pub old_lock_until: i64,
    pub new_lock_until: i64,
}

#[derive(Debug, Error)]
pub enum PendingMatchRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("pending match {pending_match_id} contains malformed JSON: {message}")]
    MalformedPayload {
        pending_match_id: i64,
        message: String,
    },
    #[error("failed to encode pending-match JSON: {0}")]
    EncodePayload(String),
    #[error("pending match {0} was not found")]
    PendingMatchNotFound(i64),
    #[error("pending match {0} has no betting window")]
    MissingBettingWindow(i64),
    #[error("betting extension must be a positive number of seconds")]
    InvalidBettingExtension,
    #[error("integer arithmetic overflow")]
    ArithmeticOverflow,
}

#[derive(Clone, Debug)]
pub struct PendingMatchRepository {
    path: PathBuf,
}

impl PendingMatchRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Insert a new row. Multiple pending matches in one guild are allowed.
    pub fn create_pending_match(
        &self,
        guild_id: i64,
        state: &PendingMatchState,
    ) -> Result<PendingMatchRecord, PendingMatchRepositoryError> {
        let encoded = encode_state(state)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO pending_matches (guild_id,payload,updated_at)
             VALUES (?1,?2,CURRENT_TIMESTAMP)",
            params![guild_id, encoded],
        )?;
        let pending_match_id = transaction.last_insert_rowid();
        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let record = decode_raw(raw)?;
        transaction.commit()?;
        Ok(record)
    }

    /// Load one explicitly selected row, always guarded by its guild.
    pub fn pending_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        query_raw(&connection, guild_id, pending_match_id)?
            .map(decode_raw)
            .transpose()
    }

    /// Backward-compatible selection: return a row only when the guild has
    /// exactly one pending match.
    pub fn single_pending_match(
        &self,
        guild_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT pending_match_id,guild_id,payload,created_at,updated_at
               FROM pending_matches
              WHERE guild_id=?1
              ORDER BY created_at,pending_match_id
              LIMIT 2",
        )?;
        let rows = statement
            .query_map(params![guild_id], raw_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() != 1 {
            return Ok(None);
        }
        rows.into_iter().next().map(decode_raw).transpose()
    }

    /// Load every pending match in stable creation order.
    pub fn pending_matches(
        &self,
        guild_id: i64,
    ) -> Result<Vec<PendingMatchRecord>, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        query_all_raw(&connection, guild_id)?
            .into_iter()
            .map(decode_raw)
            .collect()
    }

    /// Select the oldest pending match in which the player is on a team.
    pub fn pending_match_for_participant(
        &self,
        guild_id: i64,
        player_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        Ok(self
            .pending_matches(guild_id)?
            .into_iter()
            .find(|pending| pending.state.contains_participant(player_id)))
    }

    /// Select the oldest pending match whose complete shuffled lobby contains
    /// the player. This additionally includes both exclusion lists.
    pub fn pending_match_for_full_lobby_player(
        &self,
        guild_id: i64,
        player_id: i64,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        Ok(self
            .pending_matches(guild_id)?
            .into_iter()
            .find(|pending| pending.state.contains_full_lobby_player(player_id)))
    }

    /// All actual participants across the guild's pending matches.
    pub fn all_pending_player_ids(
        &self,
        guild_id: i64,
    ) -> Result<BTreeSet<i64>, PendingMatchRepositoryError> {
        Ok(self
            .pending_matches(guild_id)?
            .iter()
            .flat_map(|pending| pending.state.participant_ids())
            .collect())
    }

    /// Replace the complete JSON document under a strict guild guard.
    ///
    /// Unknown fields loaded into `state.extra` remain present in the encoded
    /// document. Returns `false` for a missing or cross-guild row.
    pub fn update_pending_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        state: &PendingMatchState,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let encoded = encode_state(state)?;
        let connection = open_runtime_connection(&self.path)?;
        let changed = connection.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        Ok(changed == 1)
    }

    /// Atomically reload, mutate, and persist one pending-match document.
    ///
    /// The immediate writer lock is acquired before the row is read. This
    /// serializes the mutation with voting's payload CAS and other SQLite
    /// writers, preventing a metadata refresh from replacing a newly cast
    /// vote. Unknown JSON fields remain present through `state.extra`.
    pub fn mutate_pending_match<R>(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        mutate: impl FnOnce(&mut PendingMatchState) -> R,
    ) -> Result<Option<(PendingMatchRecord, R)>, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(raw) = query_raw(&transaction, guild_id, pending_match_id)? else {
            transaction.commit()?;
            return Ok(None);
        };
        let mut pending = decode_raw(raw)?;
        let output = mutate(&mut pending.state);
        let encoded = encode_state(&pending.state)?;
        let changed = transaction.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending_match_id,
            ));
        }
        let updated_raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let updated = decode_raw(updated_raw)?;
        transaction.commit()?;
        Ok(Some((updated, output)))
    }

    /// Delete one explicitly selected row under a strict guild guard.
    pub fn delete_pending_match(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<bool, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let changed = connection.execute(
            "DELETE FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
        )?;
        Ok(changed == 1)
    }

    /// Delete every pending row for a guild. The count is returned so callers
    /// can distinguish a no-op from a legacy bulk clear.
    pub fn delete_pending_matches(
        &self,
        guild_id: i64,
    ) -> Result<usize, PendingMatchRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .execute(
                "DELETE FROM pending_matches WHERE guild_id=?1",
                params![guild_id],
            )
            .map_err(Into::into)
    }

    /// Atomically return and delete one pending match.
    ///
    /// With an explicit ID this consumes only that guild-scoped row. Without
    /// an ID it preserves the legacy Python contract and consumes only when
    /// the guild has exactly one pending match.
    pub fn consume_pending_match(
        &self,
        guild_id: i64,
        pending_match_id: Option<i64>,
    ) -> Result<Option<PendingMatchRecord>, PendingMatchRepositoryError> {
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut rows = if let Some(pending_match_id) = pending_match_id {
            query_raw(&transaction, guild_id, pending_match_id)?
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            query_all_raw(&transaction, guild_id)?
        };
        if rows.len() != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        let pending = decode_raw(rows.pop().expect("one pending row"))?;
        let changed = transaction.execute(
            "DELETE FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending.pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending.pending_match_id,
            ));
        }
        transaction.commit()?;
        Ok(Some(pending))
    }

    /// Extend betting from `max(current_lock, now)` under one immediate
    /// SQLite writer lock.
    ///
    /// This mutation operates on the raw JSON object and changes only
    /// `bet_lock_until`, which guarantees that fields unknown to this binary
    /// survive the write. A missing, null, or zero lock is treated as no
    /// betting window, matching the Python command.
    pub fn extend_betting_atomic(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        now_unix: i64,
        extension_seconds: i64,
    ) -> Result<PendingBettingExtension, PendingMatchRepositoryError> {
        if extension_seconds <= 0 {
            return Err(PendingMatchRepositoryError::InvalidBettingExtension);
        }

        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let mut payload = decode_value(pending_match_id, &raw.payload)?;
        let object = payload.as_object_mut().ok_or_else(|| {
            PendingMatchRepositoryError::MalformedPayload {
                pending_match_id,
                message: "root value is not an object".to_owned(),
            }
        })?;
        let old_lock_until = object
            .get("bet_lock_until")
            .and_then(Value::as_i64)
            .filter(|lock| *lock != 0)
            .ok_or(PendingMatchRepositoryError::MissingBettingWindow(
                pending_match_id,
            ))?;
        let new_lock_until = old_lock_until
            .max(now_unix)
            .checked_add(extension_seconds)
            .ok_or(PendingMatchRepositoryError::ArithmeticOverflow)?;
        object.insert("bet_lock_until".to_owned(), Value::from(new_lock_until));
        let encoded = serde_json::to_string(&payload)
            .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE pending_matches
                SET payload=?1,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?2 AND pending_match_id=?3",
            params![encoded, guild_id, pending_match_id],
        )?;
        if changed != 1 {
            return Err(PendingMatchRepositoryError::PendingMatchNotFound(
                pending_match_id,
            ));
        }
        let updated_raw = query_raw(&transaction, guild_id, pending_match_id)?.ok_or(
            PendingMatchRepositoryError::PendingMatchNotFound(pending_match_id),
        )?;
        let pending_match = decode_raw(updated_raw)?;
        transaction.commit()?;
        Ok(PendingBettingExtension {
            pending_match,
            old_lock_until,
            new_lock_until,
        })
    }
}

#[derive(Debug)]
struct RawPendingMatch {
    pending_match_id: i64,
    guild_id: i64,
    payload: String,
    created_at: Option<String>,
    updated_at: Option<String>,
}

fn raw_from_row(row: &rusqlite::Row<'_>) -> Result<RawPendingMatch, rusqlite::Error> {
    Ok(RawPendingMatch {
        pending_match_id: row.get(0)?,
        guild_id: row.get(1)?,
        payload: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn query_raw(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<Option<RawPendingMatch>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT pending_match_id,guild_id,payload,created_at,updated_at
               FROM pending_matches
              WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
            raw_from_row,
        )
        .optional()
}

fn query_all_raw(
    connection: &Connection,
    guild_id: i64,
) -> Result<Vec<RawPendingMatch>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT pending_match_id,guild_id,payload,created_at,updated_at
           FROM pending_matches
          WHERE guild_id=?1
          ORDER BY created_at,pending_match_id",
    )?;
    statement
        .query_map(params![guild_id], raw_from_row)?
        .collect()
}

fn decode_value(
    pending_match_id: i64,
    payload: &str,
) -> Result<Value, PendingMatchRepositoryError> {
    serde_json::from_str(payload).map_err(|error| PendingMatchRepositoryError::MalformedPayload {
        pending_match_id,
        message: error.to_string(),
    })
}

fn decode_raw(raw: RawPendingMatch) -> Result<PendingMatchRecord, PendingMatchRepositoryError> {
    let state = serde_json::from_str::<PendingMatchState>(&raw.payload).map_err(|error| {
        PendingMatchRepositoryError::MalformedPayload {
            pending_match_id: raw.pending_match_id,
            message: error.to_string(),
        }
    })?;
    Ok(PendingMatchRecord {
        pending_match_id: raw.pending_match_id,
        guild_id: raw.guild_id,
        state,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

fn encode_state(state: &PendingMatchState) -> Result<String, PendingMatchRepositoryError> {
    let mut payload = serde_json::to_value(state)
        .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))?;
    if let Some(object) = payload.as_object_mut() {
        // This is row metadata even if a caller manually placed it in `extra`.
        object.remove("pending_match_id");
    }
    serde_json::to_string(&payload)
        .map_err(|error| PendingMatchRepositoryError::EncodePayload(error.to_string()))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    const GUILD_A: i64 = 7;
    const GUILD_B: i64 = 8;

    fn fixture() -> (NamedTempFile, PendingMatchRepository) {
        let file = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute_batch(
                "CREATE TABLE pending_matches (
                    pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
                 CREATE INDEX idx_pending_matches_guild
                    ON pending_matches(guild_id);",
            )
            .expect("create fixture schema");
        drop(connection);
        let repository = PendingMatchRepository::new(file.path());
        (file, repository)
    }

    fn state(players: &[i64]) -> PendingMatchState {
        PendingMatchState {
            radiant_team_ids: players.iter().copied().take(2).collect(),
            dire_team_ids: players.iter().copied().skip(2).collect(),
            bet_lock_until: Some(1_000),
            ..PendingMatchState::default()
        }
    }

    #[test]
    fn typed_round_trip_preserves_unknown_fields_but_not_row_id() {
        let (_file, repository) = fixture();
        let mut pending = state(&[11, 12, 13]);
        pending.extra.insert(
            "future_runtime_field".to_owned(),
            json!({"nested": [1, 2, 3]}),
        );
        pending
            .extra
            .insert("pending_match_id".to_owned(), json!(999));

        let created = repository
            .create_pending_match(GUILD_A, &pending)
            .expect("create pending match");
        assert_eq!(created.state.betting_mode, "pool");
        assert_eq!(
            created.state.extra.get("future_runtime_field"),
            Some(&json!({"nested": [1, 2, 3]}))
        );
        assert!(!created.state.extra.contains_key("pending_match_id"));

        let raw = Connection::open(repository.path)
            .expect("open fixture")
            .query_row(
                "SELECT payload FROM pending_matches WHERE pending_match_id=?1",
                [created.pending_match_id],
                |row| row.get::<_, String>(0),
            )
            .expect("read raw payload");
        assert!(!raw.contains("pending_match_id"));
    }

    #[test]
    fn single_and_player_selection_are_scoped_and_deterministic() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        let mut second_state = state(&[21, 22, 23]);
        second_state.excluded_player_ids = vec![90];
        let second = repository
            .create_pending_match(GUILD_A, &second_state)
            .expect("create second");
        repository
            .create_pending_match(GUILD_B, &state(&[11, 99]))
            .expect("create other guild");

        assert!(
            repository
                .single_pending_match(GUILD_A)
                .expect("select single")
                .is_none()
        );
        assert_eq!(
            repository
                .pending_match_for_participant(GUILD_A, 11)
                .expect("participant lookup")
                .expect("participant match")
                .pending_match_id,
            first.pending_match_id
        );
        assert!(
            repository
                .pending_match_for_participant(GUILD_A, 90)
                .expect("participant lookup")
                .is_none()
        );
        assert_eq!(
            repository
                .pending_match_for_full_lobby_player(GUILD_A, 90)
                .expect("full lobby lookup")
                .expect("full lobby match")
                .pending_match_id,
            second.pending_match_id
        );
        assert_eq!(
            repository
                .all_pending_player_ids(GUILD_A)
                .expect("all pending ids"),
            BTreeSet::from([11, 12, 13, 21, 22, 23])
        );
    }

    #[test]
    fn update_and_delete_have_strict_guild_guards() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending match");
        let mut changed_state = created.state.clone();
        changed_state.lobby_kind = Some("open".to_owned());

        assert!(
            !repository
                .update_pending_match(GUILD_B, created.pending_match_id, &changed_state)
                .expect("cross-guild update")
        );
        assert_eq!(
            repository
                .pending_match(GUILD_A, created.pending_match_id)
                .expect("load pending")
                .expect("pending exists")
                .state
                .lobby_kind,
            None
        );
        assert!(
            !repository
                .delete_pending_match(GUILD_B, created.pending_match_id)
                .expect("cross-guild delete")
        );
        assert!(
            repository
                .delete_pending_match(GUILD_A, created.pending_match_id)
                .expect("delete pending")
        );
    }

    #[test]
    fn test_consume_pending_match_by_id_guild_guard() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_B, &state(&[11, 12]))
            .expect("create guild B pending match");

        assert!(
            !repository
                .delete_pending_match(GUILD_A, created.pending_match_id)
                .expect("reject cross-guild consume")
        );
        assert!(
            repository
                .pending_match(GUILD_B, created.pending_match_id)
                .expect("load guild B pending match")
                .is_some()
        );
    }

    #[test]
    fn test_update_pending_match_guild_guard() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create guild A pending match");
        let mut tampered = created.state.clone();
        tampered.extra.insert("tampered".to_owned(), json!(true));

        assert!(
            !repository
                .update_pending_match(GUILD_B, created.pending_match_id, &tampered)
                .expect("reject cross-guild update")
        );
        let stored = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .expect("load guild A pending match")
            .expect("pending match exists");
        assert!(!stored.state.extra.contains_key("tampered"));
    }

    #[test]
    fn test_update_pending_match_correct_guild_succeeds() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create guild A pending match");
        let mut updated = created.state.clone();
        updated.extra.insert("updated".to_owned(), json!(true));

        assert!(
            repository
                .update_pending_match(GUILD_A, created.pending_match_id, &updated)
                .expect("update matching guild")
        );
        let stored = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .expect("load guild A pending match")
            .expect("pending match exists");
        assert_eq!(stored.state.extra.get("updated"), Some(&json!(true)));
    }

    #[test]
    fn test_get_pending_match_by_id_guild_guard() {
        let (_file, repository) = fixture();
        let mut original = state(&[11, 12]);
        original.extra.insert("original".to_owned(), json!(true));
        let created = repository
            .create_pending_match(GUILD_A, &original)
            .expect("create guild A pending match");

        assert!(
            repository
                .pending_match(GUILD_B, created.pending_match_id)
                .expect("cross-guild lookup")
                .is_none()
        );
        let stored = repository
            .pending_match(GUILD_A, created.pending_match_id)
            .expect("matching-guild lookup")
            .expect("pending match exists");
        assert_eq!(stored.state.extra.get("original"), Some(&json!(true)));
    }

    #[test]
    fn extension_uses_later_base_and_preserves_unknown_json() {
        let (_file, repository) = fixture();
        let mut pending = state(&[11, 12]);
        pending
            .extra
            .insert("future_runtime_field".to_owned(), json!({"keep": true}));
        let created = repository
            .create_pending_match(GUILD_A, &pending)
            .expect("create pending");

        let extension = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 2_000, 300)
            .expect("extend closed window");
        assert_eq!(extension.old_lock_until, 1_000);
        assert_eq!(extension.new_lock_until, 2_300);
        assert_eq!(
            extension
                .pending_match
                .state
                .extra
                .get("future_runtime_field"),
            Some(&json!({"keep": true}))
        );

        let extension = repository
            .extend_betting_atomic(GUILD_A, created.pending_match_id, 2_100, 60)
            .expect("extend open window");
        assert_eq!(extension.old_lock_until, 2_300);
        assert_eq!(extension.new_lock_until, 2_360);
    }

    #[test]
    fn atomic_mutation_starts_from_latest_complete_document() {
        let (_file, repository) = fixture();
        let created = repository
            .create_pending_match(GUILD_A, &state(&[11, 12]))
            .expect("create pending");

        // Stand in for voting's independently committed payload CAS.
        let connection = Connection::open(&repository.path).expect("open fixture");
        let mut payload = serde_json::to_value(&created.state).expect("encode payload");
        payload["record_submissions"] = json!({
            "11": {"result": "radiant", "is_admin": false, "future_vote_field": 9}
        });
        payload["future_root_field"] = json!("keep");
        connection
            .execute(
                "UPDATE pending_matches SET payload=?1 WHERE pending_match_id=?2",
                params![payload.to_string(), created.pending_match_id],
            )
            .expect("persist simulated vote");
        drop(connection);

        let (updated, old_channel) = repository
            .mutate_pending_match(GUILD_A, created.pending_match_id, |state| {
                let old_channel = state.shuffle_channel_id;
                state.shuffle_channel_id = Some(99);
                old_channel
            })
            .expect("atomic metadata mutation")
            .expect("pending exists");
        assert_eq!(old_channel, None);
        assert_eq!(updated.state.shuffle_channel_id, Some(99));
        assert_eq!(
            updated.state.extra.get("future_root_field"),
            Some(&json!("keep"))
        );
        let vote = updated
            .state
            .record_submissions
            .get(&11)
            .expect("vote survived");
        assert_eq!(vote.result.as_deref(), Some("radiant"));
        assert_eq!(vote.extra.get("future_vote_field"), Some(&json!(9)));
    }

    #[test]
    fn test_two_shuffles_create_distinct_pending_matches() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&(1_000..1_010).collect::<Vec<_>>()))
            .expect("create first shuffle");
        let second = repository
            .create_pending_match(GUILD_A, &state(&(2_000..2_010).collect::<Vec<_>>()))
            .expect("create second shuffle");
        assert_ne!(first.pending_match_id, second.pending_match_id);
        assert_eq!(repository.pending_matches(GUILD_A).unwrap().len(), 2);
    }

    #[test]
    fn test_pending_matches_retain_source_lobby_kind() {
        let (_file, repository) = fixture();
        let mut open = state(&(3_000..3_010).collect::<Vec<_>>());
        open.lobby_kind = Some("open".to_owned());
        let mut lowskill = state(&(4_000..4_010).collect::<Vec<_>>());
        lowskill.lobby_kind = Some("lowskill".to_owned());
        let open = repository
            .create_pending_match(GUILD_A, &open)
            .expect("create open shuffle");
        let lowskill = repository
            .create_pending_match(GUILD_A, &lowskill)
            .expect("create lowskill shuffle");
        assert_eq!(open.state.lobby_kind.as_deref(), Some("open"));
        assert_eq!(lowskill.state.lobby_kind.as_deref(), Some("lowskill"));
    }

    #[test]
    fn test_get_last_shuffle_with_specific_id_returns_correct_match() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[1_000, 1_001, 1_002, 1_003]))
            .expect("create first shuffle");
        let second = repository
            .create_pending_match(GUILD_A, &state(&[2_000, 2_001, 2_002, 2_003]))
            .expect("create second shuffle");
        assert!(repository.single_pending_match(GUILD_A).unwrap().is_none());
        let first_loaded = repository
            .pending_match(GUILD_A, first.pending_match_id)
            .unwrap()
            .expect("load first");
        let second_loaded = repository
            .pending_match(GUILD_A, second.pending_match_id)
            .unwrap()
            .expect("load second");
        assert_eq!(
            first_loaded.state.participant_ids(),
            BTreeSet::from([1_000, 1_001, 1_002, 1_003])
        );
        assert_eq!(
            second_loaded.state.participant_ids(),
            BTreeSet::from([2_000, 2_001, 2_002, 2_003])
        );
    }

    #[test]
    fn test_get_all_pending_player_ids_returns_all_players() {
        let (_file, repository) = fixture();
        repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        repository
            .create_pending_match(GUILD_A, &state(&[21, 22, 23]))
            .expect("create second");
        assert_eq!(
            repository.all_pending_player_ids(GUILD_A).unwrap(),
            BTreeSet::from([11, 12, 13, 21, 22, 23])
        );
    }

    #[test]
    fn test_get_pending_match_for_player_finds_correct_match() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        let second = repository
            .create_pending_match(GUILD_A, &state(&[21, 22, 23]))
            .expect("create second");
        assert_eq!(
            repository
                .pending_match_for_participant(GUILD_A, 12)
                .unwrap()
                .expect("first participant")
                .pending_match_id,
            first.pending_match_id
        );
        assert_eq!(
            repository
                .pending_match_for_participant(GUILD_A, 23)
                .unwrap()
                .expect("second participant")
                .pending_match_id,
            second.pending_match_id
        );
    }

    #[test]
    fn test_clear_specific_match_preserves_others() {
        let (_file, repository) = fixture();
        let first = repository
            .create_pending_match(GUILD_A, &state(&[11, 12, 13]))
            .expect("create first");
        let second = repository
            .create_pending_match(GUILD_A, &state(&[21, 22, 23]))
            .expect("create second");
        assert!(
            repository
                .delete_pending_match(GUILD_A, first.pending_match_id)
                .unwrap()
        );
        assert!(
            repository
                .pending_match(GUILD_A, first.pending_match_id)
                .unwrap()
                .is_none()
        );
        assert!(
            repository
                .pending_match(GUILD_A, second.pending_match_id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_consume_pending_match_is_single_use_idempotent() {
        let (_file, repository) = fixture();
        let mut payload = state(&[1, 2, 3]);
        payload.extra.insert("test".to_owned(), json!("data"));
        payload.extra.insert("winning_team".to_owned(), json!(2));
        let created = repository
            .create_pending_match(GUILD_A, &payload)
            .expect("save distinctive pending match");
        let consumed = repository
            .consume_pending_match(GUILD_A, None)
            .expect("consume pending match")
            .expect("first consume returns payload");
        assert_eq!(consumed.pending_match_id, created.pending_match_id);
        assert_eq!(consumed.state, payload);
        assert!(
            repository
                .consume_pending_match(GUILD_A, None)
                .expect("second consume")
                .is_none()
        );
        assert!(
            repository
                .consume_pending_match(GUILD_A, None)
                .expect("third consume")
                .is_none()
        );
    }
}
