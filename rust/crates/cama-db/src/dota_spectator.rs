//! Durable ownership and delivery state for per-match spectator channels.
//!
//! Rows deliberately outlive pending matches. The caller saves intent before
//! Discord requests and deletes a row only after external cleanup is confirmed.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::Value;
use thiserror::Error;

use crate::open_runtime_connection;

const MAX_LIST_RECORDS: usize = 1_000;
const MAX_SUBSCRIBERS: i64 = 80;

#[derive(Clone, Debug, PartialEq)]
pub struct DotaSpectatorRecord {
    pub guild_id: i64,
    pub pending_match_id: i64,
    pub marker: String,
    pub channel_id: Option<i64>,
    pub payload: Value,
    pub expires_at: Option<i64>,
    pub revision: i64,
    pub updated_at: i64,
}

#[derive(Debug, Error)]
pub enum DotaSpectatorRepositoryError {
    #[error("spectator persistence failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("spectator guild, match/message, channel, and user IDs must be positive")]
    InvalidIdentity,
    #[error("spectator ownership marker must contain 1 to 200 characters")]
    InvalidMarker,
    #[error("spectator payload must be a JSON object")]
    InvalidPayload,
    #[error("spectator expiry must not be negative")]
    InvalidExpiry,
    #[error("spectator revision is negative or exhausted")]
    InvalidRevision,
    #[error("this lobby already has the maximum of 80 spectator subscribers")]
    SubscriberLimit,
}

#[derive(Clone, Debug)]
pub struct DotaSpectatorRepository {
    path: PathBuf,
}

impl DotaSpectatorRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// The runtime validates that the reaction belongs to this guild's lobby
    /// message before calling this durable, idempotent subscription mutation.
    pub fn update_subscription(
        &self,
        guild_id: i64,
        lobby_message_id: i64,
        user_id: i64,
        subscribed: bool,
        now: i64,
    ) -> Result<(), DotaSpectatorRepositoryError> {
        validate_identity(guild_id, lobby_message_id)?;
        if user_id <= 0 {
            return Err(DotaSpectatorRepositoryError::InvalidIdentity);
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if subscribed {
            let existing: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM dota_spectator_subscriptions
                 WHERE guild_id=?1 AND lobby_message_id=?2 AND user_id=?3)",
                params![guild_id, lobby_message_id, user_id],
                |row| row.get(0),
            )?;
            if !existing {
                let count: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM dota_spectator_subscriptions WHERE guild_id=?1 AND lobby_message_id=?2",
                    params![guild_id, lobby_message_id], |row| row.get(0),
                )?;
                if count >= MAX_SUBSCRIBERS {
                    return Err(DotaSpectatorRepositoryError::SubscriberLimit);
                }
            }
            transaction.execute(
                "INSERT INTO dota_spectator_subscriptions(guild_id,lobby_message_id,user_id,updated_at)
                 VALUES(?1,?2,?3,?4) ON CONFLICT(guild_id,lobby_message_id,user_id) DO UPDATE SET updated_at=excluded.updated_at",
                params![guild_id, lobby_message_id, user_id, now],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM dota_spectator_subscriptions WHERE guild_id=?1 AND lobby_message_id=?2 AND user_id=?3",
                params![guild_id, lobby_message_id, user_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn subscribers(
        &self,
        guild_id: i64,
        lobby_message_id: i64,
    ) -> Result<Vec<i64>, DotaSpectatorRepositoryError> {
        validate_identity(guild_id, lobby_message_id)?;
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT user_id FROM dota_spectator_subscriptions
             WHERE guild_id=?1 AND lobby_message_id=?2 ORDER BY user_id LIMIT ?3",
        )?;
        statement
            .query_map(
                params![guild_id, lobby_message_id, MAX_SUBSCRIBERS],
                |row| row.get(0),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Preserve the first resource identity and initial intent on retries.
    pub fn create_or_get(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        marker: &str,
        payload: Value,
        now: i64,
    ) -> Result<DotaSpectatorRecord, DotaSpectatorRepositoryError> {
        validate_identity(guild_id, pending_match_id)?;
        validate_marker(marker)?;
        if !payload.is_object() {
            return Err(DotaSpectatorRepositoryError::InvalidPayload);
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO dota_spectators(guild_id,pending_match_id,marker,payload,updated_at)
             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(guild_id,pending_match_id) DO NOTHING",
            params![guild_id, pending_match_id, marker, payload.to_string(), now],
        )?;
        let record = query_record(&transaction, guild_id, pending_match_id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        transaction.commit()?;
        Ok(record)
    }

    pub fn get(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<Option<DotaSpectatorRecord>, DotaSpectatorRepositoryError> {
        validate_identity(guild_id, pending_match_id)?;
        query_record(
            &open_runtime_connection(&self.path)?,
            guild_id,
            pending_match_id,
        )
        .map_err(Into::into)
    }

    /// Include expired rows for cleanup. Oldest updates come first, bounded to
    /// 1000 rows per reconciliation pass; successful saves rotate them back.
    pub fn list(&self) -> Result<Vec<DotaSpectatorRecord>, DotaSpectatorRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id,pending_match_id,marker,channel_id,payload,expires_at,revision,updated_at
             FROM dota_spectators ORDER BY updated_at,guild_id,pending_match_id LIMIT ?1",
        )?;
        statement
            .query_map([MAX_LIST_RECORDS as i64], row_to_record)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// False means missing identity, changed ownership marker, or stale
    /// revision. The caller must reload before another external side effect.
    pub fn save(
        &self,
        record: &DotaSpectatorRecord,
        expected_revision: i64,
        now: i64,
    ) -> Result<bool, DotaSpectatorRepositoryError> {
        validate_identity(record.guild_id, record.pending_match_id)?;
        validate_marker(&record.marker)?;
        if record.channel_id.is_some_and(|id| id <= 0) {
            return Err(DotaSpectatorRepositoryError::InvalidIdentity);
        }
        if !record.payload.is_object() {
            return Err(DotaSpectatorRepositoryError::InvalidPayload);
        }
        if record.expires_at.is_some_and(|expiry| expiry < 0) {
            return Err(DotaSpectatorRepositoryError::InvalidExpiry);
        }
        if expected_revision < 0 {
            return Err(DotaSpectatorRepositoryError::InvalidRevision);
        }
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(DotaSpectatorRepositoryError::InvalidRevision)?;
        let changed = open_runtime_connection(&self.path)?.execute(
            "UPDATE dota_spectators SET channel_id=?1,payload=?2,expires_at=?3,revision=?4,updated_at=?5
             WHERE guild_id=?6 AND pending_match_id=?7 AND revision=?8 AND marker=?9",
            params![record.channel_id, record.payload.to_string(), record.expires_at, next_revision,
                now, record.guild_id, record.pending_match_id, expected_revision, record.marker],
        )?;
        Ok(changed == 1)
    }

    /// Remove only the revision whose external cleanup the caller confirmed.
    pub fn delete(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        revision: i64,
    ) -> Result<bool, DotaSpectatorRepositoryError> {
        validate_identity(guild_id, pending_match_id)?;
        if revision < 0 {
            return Err(DotaSpectatorRepositoryError::InvalidRevision);
        }
        let changed = open_runtime_connection(&self.path)?.execute(
            "DELETE FROM dota_spectators WHERE guild_id=?1 AND pending_match_id=?2 AND revision=?3",
            params![guild_id, pending_match_id, revision],
        )?;
        Ok(changed == 1)
    }
}

fn validate_identity(
    guild_id: i64,
    pending_match_id: i64,
) -> Result<(), DotaSpectatorRepositoryError> {
    if guild_id <= 0 || pending_match_id <= 0 {
        return Err(DotaSpectatorRepositoryError::InvalidIdentity);
    }
    Ok(())
}

fn validate_marker(marker: &str) -> Result<(), DotaSpectatorRepositoryError> {
    if marker.trim().is_empty() || marker.chars().count() > 200 {
        return Err(DotaSpectatorRepositoryError::InvalidMarker);
    }
    Ok(())
}

fn query_record(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<Option<DotaSpectatorRecord>, rusqlite::Error> {
    connection.query_row(
        "SELECT guild_id,pending_match_id,marker,channel_id,payload,expires_at,revision,updated_at
         FROM dota_spectators WHERE guild_id=?1 AND pending_match_id=?2",
        params![guild_id, pending_match_id], row_to_record,
    ).optional()
}

fn row_to_record(row: &Row<'_>) -> Result<DotaSpectatorRecord, rusqlite::Error> {
    let raw: String = row.get(4)?;
    let payload: Value = serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    if !payload.is_object() {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "spectator payload must be an object",
            )),
        ));
    }
    Ok(DotaSpectatorRecord {
        guild_id: row.get(0)?,
        pending_match_id: row.get(1)?,
        marker: row.get(2)?,
        channel_id: row.get(3)?,
        payload,
        expires_at: row.get(5)?,
        revision: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
#[path = "dota_spectator/tests.rs"]
mod tests;
