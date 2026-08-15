//! Existing-schema persistence for pending-match votes.
//!
//! Python remains the production runtime and sole schema/migration authority.
//! This adapter opens only an already-migrated database through
//! [`crate::open_runtime_connection`] and preserves the full pending payload
//! while atomically replacing one user's submission with an exact-payload CAS.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, types::Value};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingVoteKey {
    pub guild_id: Option<i64>,
    pub pending_match_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedVoteSubmission {
    pub user_id: Option<i64>,
    pub result: Option<String>,
    pub is_admin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingVoteSnapshot {
    pub pending_match_id: i64,
    pub guild_id: i64,
    pub payload: String,
    pub radiant_team_ids: Vec<i64>,
    pub dire_team_ids: Vec<i64>,
    pub excluded_player_ids: Vec<i64>,
    pub excluded_conditional_player_ids: Vec<i64>,
    pub submissions: Vec<PersistedVoteSubmission>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoteSubmissionCas<'a> {
    pub guild_id: Option<i64>,
    pub pending_match_id: i64,
    pub expected_payload: &'a str,
    pub user_id: i64,
    pub result: &'a str,
    pub is_admin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoteSubmissionCasOutcome {
    Applied(PendingVoteSnapshot),
    Conflict(PendingVoteSnapshot),
    MissingMatch,
}

#[derive(Debug, Error)]
pub enum MatchVotingRepositoryError {
    #[error("pending match {0} contains malformed JSON")]
    MalformedPayload(i64),
    #[error("match-voting SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Existing-schema repository for pending-match voting payloads.
#[derive(Clone, Debug)]
pub struct MatchVotingRepository {
    path: PathBuf,
}

impl MatchVotingRepository {
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

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Load an explicit match, or the guild's match only when exactly one is
    /// pending. An unknown explicit ID never falls back to another row.
    pub fn snapshot(
        &self,
        key: PendingVoteKey,
    ) -> Result<Option<PendingVoteSnapshot>, MatchVotingRepositoryError> {
        let connection = self.connection()?;
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let pending_match_id = resolve_match_id(&connection, guild_id, key.pending_match_id)?;
        pending_match_id
            .map(|pending_match_id| query_snapshot(&connection, guild_id, pending_match_id))
            .transpose()
    }

    /// Atomically write one shared-slot submission while preserving every
    /// unrelated JSON field. The caller must re-evaluate policy on conflict.
    pub fn compare_and_set_submission(
        &self,
        request: VoteSubmissionCas<'_>,
    ) -> Result<VoteSubmissionCasOutcome, MatchVotingRepositoryError> {
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated_payload = payload_with_submission(
            &transaction,
            request.expected_payload,
            request.user_id,
            request.result,
            request.is_admin,
        )?;
        let changed = transaction.execute(
            "UPDATE pending_matches
             SET payload = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE pending_match_id = ?2 AND guild_id = ?3 AND payload = ?4",
            params![
                updated_payload,
                request.pending_match_id,
                guild_id,
                request.expected_payload
            ],
        )?;
        if changed == 1 {
            let snapshot = query_snapshot(&transaction, guild_id, request.pending_match_id)?;
            transaction.commit()?;
            return Ok(VoteSubmissionCasOutcome::Applied(snapshot));
        }
        let persisted = query_snapshot_optional(&transaction, guild_id, request.pending_match_id)?;
        transaction.commit()?;
        Ok(persisted.map_or(
            VoteSubmissionCasOutcome::MissingMatch,
            VoteSubmissionCasOutcome::Conflict,
        ))
    }
}

fn resolve_match_id(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: Option<i64>,
) -> Result<Option<i64>, rusqlite::Error> {
    if let Some(pending_match_id) = pending_match_id {
        return connection
            .query_row(
                "SELECT pending_match_id FROM pending_matches
                 WHERE pending_match_id = ?1 AND guild_id = ?2",
                params![pending_match_id, guild_id],
                |row| row.get(0),
            )
            .optional();
    }
    let mut statement = connection.prepare(
        "SELECT pending_match_id FROM pending_matches
         WHERE guild_id = ?1 ORDER BY created_at, pending_match_id LIMIT 2",
    )?;
    let ids = statement
        .query_map(params![guild_id], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(if ids.len() == 1 { Some(ids[0]) } else { None })
}

fn query_snapshot_optional(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<Option<PendingVoteSnapshot>, MatchVotingRepositoryError> {
    let payload = connection
        .query_row(
            "SELECT payload FROM pending_matches
             WHERE pending_match_id = ?1 AND guild_id = ?2",
            params![pending_match_id, guild_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    payload
        .map(|payload| decode_snapshot(connection, guild_id, pending_match_id, payload))
        .transpose()
}

fn query_snapshot(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<PendingVoteSnapshot, MatchVotingRepositoryError> {
    Ok(
        query_snapshot_optional(connection, guild_id, pending_match_id)?
            .expect("pending match was resolved inside the same transaction"),
    )
}

fn decode_snapshot(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: i64,
    payload: String,
) -> Result<PendingVoteSnapshot, MatchVotingRepositoryError> {
    let valid = connection.query_row("SELECT json_valid(?1)", params![payload], |row| {
        row.get::<_, bool>(0)
    })?;
    if !valid {
        return Err(MatchVotingRepositoryError::MalformedPayload(
            pending_match_id,
        ));
    }
    Ok(PendingVoteSnapshot {
        pending_match_id,
        guild_id,
        radiant_team_ids: read_integer_array(connection, &payload, "$.radiant_team_ids")?,
        dire_team_ids: read_integer_array(connection, &payload, "$.dire_team_ids")?,
        excluded_player_ids: read_integer_array(connection, &payload, "$.excluded_player_ids")?,
        excluded_conditional_player_ids: read_integer_array(
            connection,
            &payload,
            "$.excluded_conditional_player_ids",
        )?,
        submissions: read_submissions(connection, &payload)?,
        payload,
    })
}

fn read_integer_array(
    connection: &Connection,
    payload: &str,
    path: &str,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut statement = connection.prepare("SELECT value FROM json_each(?1, ?2) ORDER BY id")?;
    let values = statement
        .query_map(params![payload, path], |row| row.get::<_, Value>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values
        .into_iter()
        .filter_map(|value| match value {
            Value::Integer(value) => Some(value),
            _ => None,
        })
        .collect())
}

fn read_submissions(
    connection: &Connection,
    payload: &str,
) -> Result<Vec<PersistedVoteSubmission>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT key, json_extract(value, '$.result'),
                json_extract(value, '$.is_admin')
         FROM json_each(?1, '$.record_submissions') ORDER BY id",
    )?;
    statement
        .query_map(params![payload], |row| {
            let key = row.get::<_, String>(0)?;
            let result = row.get::<_, Value>(1)?;
            let is_admin = row.get::<_, Value>(2)?;
            Ok(PersistedVoteSubmission {
                user_id: key.parse().ok(),
                result: match result {
                    Value::Text(result) => Some(result),
                    _ => None,
                },
                is_admin: python_truthy(&is_admin),
            })
        })?
        .collect()
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Integer(value) => *value != 0,
        Value::Real(value) => *value != 0.0,
        Value::Text(value) => !value.is_empty(),
        Value::Blob(value) => !value.is_empty(),
    }
}

fn payload_with_submission(
    connection: &Connection,
    expected_payload: &str,
    user_id: i64,
    result: &str,
    is_admin: bool,
) -> Result<String, rusqlite::Error> {
    let submissions_type = connection.query_row(
        "SELECT json_type(?1, '$.record_submissions')",
        params![expected_payload],
        |row| row.get::<_, Option<String>>(0),
    )?;
    let base = if submissions_type.as_deref() == Some("object") {
        expected_payload.to_owned()
    } else {
        connection.query_row(
            "SELECT json_set(?1, '$.record_submissions', json('{}'))",
            params![expected_payload],
            |row| row.get(0),
        )?
    };
    let submission_path = format!(r#"$.record_submissions."{user_id}""#);
    let boolean_json = if is_admin { "true" } else { "false" };
    connection.query_row(
        "SELECT json_set(
             ?1, ?2,
             json_object('result', ?3, 'is_admin', json(?4))
         )",
        params![base, submission_path, result, boolean_json],
        |row| row.get(0),
    )
}

#[cfg(test)]
mod tests;
