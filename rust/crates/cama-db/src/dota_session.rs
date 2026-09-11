//! Durable lifecycle state for the Steam/Dota lobby worker.
//!
//! A Dota lobby is an external resource.  The worker therefore records its
//! identity and progress before making a Game Coordinator request and keeps
//! the row after the pending match is consumed.  A restart can use this table
//! to resume or reconcile the external lobby without creating a second one.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::match_runtime::{
    PendingBettingClose, PendingMatchRepository, PendingMatchRepositoryError,
};
use crate::open_runtime_connection;

/// The durable Dota lobby lifecycle.
///
/// `NeedsReview` is deliberately separate from `Failed`: a failed operation
/// known to have no external lobby can release the host, while an uncertain
/// response keeps the account leased until a reconciliation pass proves that
/// the lobby is gone or has been recorded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DotaSessionPhase {
    Creating,
    Gathering,
    Launching,
    Running,
    Finishing,
    Recorded,
    Cancelled,
    Failed,
    NeedsReview,
}

impl DotaSessionPhase {
    /// Stable value stored in SQLite and sent through JSON diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Gathering => "gathering",
            Self::Launching => "launching",
            Self::Running => "running",
            Self::Finishing => "finishing",
            Self::Recorded => "recorded",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::NeedsReview => "needs_review",
        }
    }

    /// Parse a value read from the durable row.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "creating" => Some(Self::Creating),
            "gathering" => Some(Self::Gathering),
            "launching" => Some(Self::Launching),
            "running" => Some(Self::Running),
            "finishing" => Some(Self::Finishing),
            "recorded" => Some(Self::Recorded),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            "needs_review" => Some(Self::NeedsReview),
            _ => None,
        }
    }

    /// Whether this phase keeps the dedicated Steam account leased.
    #[must_use]
    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Recorded | Self::Cancelled | Self::Failed)
    }
}

/// Durable identity and progress for one Dota automation attempt.
#[derive(Clone, Debug, PartialEq)]
pub struct DotaSessionRecord {
    pub guild_id: i64,
    pub pending_match_id: i64,
    pub account_key: String,
    pub phase: DotaSessionPhase,
    pub lobby_id: Option<String>,
    pub valve_match_id: Option<String>,
    pub payload: Value,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_error: Option<String>,
}

/// Result of claiming the durable `(guild, pending match)` identity.
#[derive(Clone, Debug, PartialEq)]
pub enum DotaSessionClaim {
    /// This caller created the first row for the pending match.
    Created(DotaSessionRecord),
    /// A prior row already exists for the pending match.  Its payload and
    /// lifecycle are authoritative, so the caller should resume it.
    Existing(DotaSessionRecord),
    /// The requested account is leased by another non-terminal session.
    Busy(DotaSessionRecord),
}

#[derive(Debug, Error)]
pub enum DotaSessionRepositoryError {
    #[error("Dota session SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Dota session payload could not be encoded: {0}")]
    EncodePayload(String),
    #[error("Dota session {guild_id}/{pending_match_id} was not found")]
    SessionNotFound {
        guild_id: i64,
        pending_match_id: i64,
    },
    #[error(
        "Dota session {guild_id}/{pending_match_id} revision is stale: expected {expected}, found {actual}"
    )]
    StaleRevision {
        guild_id: i64,
        pending_match_id: i64,
        expected: i64,
        actual: i64,
    },
    #[error("pending match {guild_id}/{pending_match_id} no longer reserves this Dota account")]
    ReservationUnavailable {
        guild_id: i64,
        pending_match_id: i64,
    },
    #[error("Dota session account key must not be empty")]
    EmptyAccountKey,
    #[error("Dota session account key is immutable")]
    AccountKeyImmutable,
    #[error("Dota session revision exhausted")]
    RevisionExhausted,
    #[error("Dota session history limit {0} exceeds the maximum of 1000")]
    InvalidHistoryLimit(usize),
    #[error("Dota session {guild_id}/{pending_match_id} has invalid phase {phase:?}")]
    InvalidPhase {
        guild_id: i64,
        pending_match_id: i64,
        phase: String,
    },
    #[error(
        "Valve match {valve_match_id} is already associated with Dota session {guild_id}/{pending_match_id}"
    )]
    ValveMatchAlreadyAssociated {
        valve_match_id: String,
        guild_id: i64,
        pending_match_id: i64,
    },
    #[error(transparent)]
    PendingMatch(#[from] PendingMatchRepositoryError),
}

#[derive(Clone, Debug)]
pub struct DotaSessionRepository {
    path: PathBuf,
}

impl DotaSessionRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Atomically reserve one pending-match identity and one dedicated host.
    ///
    /// The `(guild, pending match)` lookup happens before the account lease
    /// lookup so a retry receives `Existing` even when its account is already
    /// leased by that same row.  A terminal row remains authoritative forever
    /// and is never replaced by a retry.
    pub fn claim_session(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        account_key: &str,
        payload: Value,
        now: i64,
    ) -> Result<DotaSessionClaim, DotaSessionRepositoryError> {
        self.claim_session_inner(guild_id, pending_match_id, account_key, payload, now, false)
    }

    pub fn claim_reserved_session(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        account_key: &str,
        payload: Value,
        now: i64,
    ) -> Result<DotaSessionClaim, DotaSessionRepositoryError> {
        self.claim_session_inner(guild_id, pending_match_id, account_key, payload, now, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn claim_session_inner(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        account_key: &str,
        payload: Value,
        now: i64,
        reserved: bool,
    ) -> Result<DotaSessionClaim, DotaSessionRepositoryError> {
        if account_key.trim().is_empty() {
            return Err(DotaSessionRepositoryError::EmptyAccountKey);
        }
        let encoded_payload = encode_payload(&payload)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing) = query_session(&transaction, guild_id, pending_match_id)? {
            transaction.commit()?;
            return Ok(DotaSessionClaim::Existing(existing));
        }

        if reserved {
            let eligible: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM pending_matches p
                WHERE p.guild_id=?1 AND p.pending_match_id=?2
                  AND json_extract(p.payload,'$.dota_host_account_key')=?3
                  AND NOT EXISTS(SELECT 1 FROM app_kv k WHERE k.guild_id=0 AND k.key='dota_host_withdrawn:'||p.guild_id||':'||p.pending_match_id)
                  AND EXISTS(SELECT 1 FROM app_kv policy WHERE policy.guild_id=0 AND policy.key='dota_host_routing'
                    AND json_extract(policy.value,'$.account_key')=?3
                    AND EXISTS(SELECT 1 FROM json_each(policy.value,'$.guild_ids') WHERE value=?1))
                  AND COALESCE(json_extract(p.payload,'$.dota_hosting.hosting'),'bot')!='manual'
                  AND COALESCE(json_extract(p.payload,'$.shuffle_setup_complete'),1)!=0
                  AND COALESCE(json_extract(p.payload,'$.draft_setup_complete'),1)!=0
                  AND NOT EXISTS(SELECT 1 FROM matches m WHERE m.guild_id=?1 AND m.pending_match_id=?2))", params![guild_id,pending_match_id,account_key], |r|r.get(0))?;
            if !eligible {
                return Err(DotaSessionRepositoryError::ReservationUnavailable {
                    guild_id,
                    pending_match_id,
                });
            }
        }
        if let Some(busy) = query_active_account(&transaction, account_key)? {
            transaction.commit()?;
            return Ok(DotaSessionClaim::Busy(busy));
        }

        transaction.execute(
            "INSERT INTO dota_sessions(
                 guild_id, pending_match_id, account_key, phase,
                 lobby_id, valve_match_id, payload, revision,
                 created_at, updated_at, last_error
             ) VALUES (?1, ?2, ?3, 'creating', NULL, NULL, ?4, 0, ?5, ?5, NULL)",
            params![
                guild_id,
                pending_match_id,
                account_key,
                encoded_payload,
                now
            ],
        )?;
        let created = query_session(&transaction, guild_id, pending_match_id)?.ok_or(
            DotaSessionRepositoryError::SessionNotFound {
                guild_id,
                pending_match_id,
            },
        )?;
        transaction.commit()?;
        Ok(DotaSessionClaim::Created(created))
    }

    /// Load one session under an explicit guild and pending-match guard.
    pub fn session(
        &self,
        guild_id: i64,
        pending_match_id: i64,
    ) -> Result<Option<DotaSessionRecord>, DotaSessionRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        query_session(&connection, guild_id, pending_match_id).map_err(Into::into)
    }

    /// Load every non-terminal session in stable update order for restart
    /// reconciliation.  `NeedsReview` is included and keeps its host lease.
    pub fn active_sessions(&self) -> Result<Vec<DotaSessionRecord>, DotaSessionRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id, pending_match_id, account_key, phase,
                    lobby_id, valve_match_id, payload, revision,
                    created_at, updated_at, last_error
               FROM dota_sessions
              WHERE phase IN ('creating','gathering','launching','running',
                              'finishing','needs_review')
              ORDER BY updated_at, guild_id, pending_match_id",
        )?;
        statement
            .query_map([], row_to_session)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Durable terminal notification outbox, independent of status history limits.
    pub fn terminal_status_pending(
        &self,
        account_key: &str,
    ) -> Result<Vec<DotaSessionRecord>, DotaSessionRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id, pending_match_id, account_key, phase,
                    lobby_id, valve_match_id, payload, revision,
                    created_at, updated_at, last_error
             FROM dota_sessions
             WHERE account_key=?1 AND phase IN ('recorded','cancelled','failed')
               AND json_type(payload,'$.pending_status')='text'
               AND json_type(payload,'$.channel_id')='integer'
             ORDER BY updated_at, guild_id, pending_match_id",
        )?;
        statement
            .query_map([account_key], row_to_session)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Load the most recently updated sessions, including terminal rows.
    ///
    /// This bounded history is used by operator status and replay recovery;
    /// terminal `Recorded` rows intentionally remain visible even though they
    /// no longer reserve the Steam account.
    pub fn recent_sessions(
        &self,
        limit: usize,
    ) -> Result<Vec<DotaSessionRecord>, DotaSessionRepositoryError> {
        if limit > 1_000 {
            return Err(DotaSessionRepositoryError::InvalidHistoryLimit(limit));
        }
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id, pending_match_id, account_key, phase,
                    lobby_id, valve_match_id, payload, revision,
                    created_at, updated_at, last_error
               FROM dota_sessions
              ORDER BY updated_at DESC, guild_id DESC, pending_match_id DESC
              LIMIT ?1",
        )?;
        statement
            .query_map([i64::try_from(limit).unwrap_or(1_000)], row_to_session)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Apply guild isolation before the history limit so busy other guilds
    /// cannot hide this guild's current or recently completed sessions.
    pub fn recent_sessions_for_guild(
        &self,
        guild_id: i64,
        limit: usize,
    ) -> Result<Vec<DotaSessionRecord>, DotaSessionRepositoryError> {
        if limit > 1_000 {
            return Err(DotaSessionRepositoryError::InvalidHistoryLimit(limit));
        }
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT guild_id, pending_match_id, account_key, phase,
                    lobby_id, valve_match_id, payload, revision,
                    created_at, updated_at, last_error
               FROM dota_sessions
              WHERE guild_id=?1
              ORDER BY updated_at DESC, pending_match_id DESC
              LIMIT ?2",
        )?;
        statement
            .query_map(
                params![guild_id, i64::try_from(limit).unwrap_or(1_000)],
                row_to_session,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Load a session by its globally unique non-null Valve match ID.
    pub fn session_by_valve_match_id(
        &self,
        valve_match_id: &str,
    ) -> Result<Option<DotaSessionRecord>, DotaSessionRepositoryError> {
        let connection = open_runtime_connection(&self.path)?;
        connection
            .query_row(
                "SELECT guild_id, pending_match_id, account_key, phase,
                        lobby_id, valve_match_id, payload, revision,
                        created_at, updated_at, last_error
                   FROM dota_sessions WHERE valve_match_id=?1",
                [valve_match_id],
                row_to_session,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Persist all mutable lifecycle fields with an exact revision CAS.
    ///
    /// The account key and identity are immutable.  The returned record has
    /// the incremented revision and database timestamps; callers should use
    /// it as the next CAS base.  This update is valid even after the pending
    /// match row has been consumed.
    pub fn update(
        &self,
        record: &DotaSessionRecord,
        expected_revision: i64,
        now: i64,
    ) -> Result<DotaSessionRecord, DotaSessionRepositoryError> {
        validate_record(record)?;
        let encoded_payload = encode_payload(&record.payload)?;
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or(DotaSessionRepositoryError::RevisionExhausted)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = query_session(&transaction, record.guild_id, record.pending_match_id)?
            .ok_or(DotaSessionRepositoryError::SessionNotFound {
                guild_id: record.guild_id,
                pending_match_id: record.pending_match_id,
            })?;
        if current.revision != expected_revision {
            transaction.commit()?;
            return Err(DotaSessionRepositoryError::StaleRevision {
                guild_id: record.guild_id,
                pending_match_id: record.pending_match_id,
                expected: expected_revision,
                actual: current.revision,
            });
        }
        if current.account_key != record.account_key {
            transaction.commit()?;
            return Err(DotaSessionRepositoryError::AccountKeyImmutable);
        }
        ensure_valve_match_available(
            &transaction,
            record.valve_match_id.as_deref(),
            record.guild_id,
            record.pending_match_id,
        )?;
        let changed = transaction.execute(
            "UPDATE dota_sessions
                SET phase=?1, lobby_id=?2, valve_match_id=?3, payload=?4,
                    revision=?5, updated_at=?6, last_error=?7
              WHERE guild_id=?8 AND pending_match_id=?9 AND revision=?10",
            params![
                record.phase.as_str(),
                record.lobby_id,
                record.valve_match_id,
                encoded_payload,
                next_revision,
                now,
                record.last_error,
                record.guild_id,
                record.pending_match_id,
                expected_revision,
            ],
        )?;
        if changed != 1 {
            let actual = query_session(&transaction, record.guild_id, record.pending_match_id)?
                .map_or(expected_revision, |current| current.revision);
            transaction.commit()?;
            return Err(DotaSessionRepositoryError::StaleRevision {
                guild_id: record.guild_id,
                pending_match_id: record.pending_match_id,
                expected: expected_revision,
                actual,
            });
        }
        let updated = query_session(&transaction, record.guild_id, record.pending_match_id)?
            .ok_or(DotaSessionRepositoryError::SessionNotFound {
                guild_id: record.guild_id,
                pending_match_id: record.pending_match_id,
            })?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Alias with a descriptive name for callers that prefer explicit CAS
    /// terminology.
    pub fn update_session(
        &self,
        record: &DotaSessionRecord,
        expected_revision: i64,
        now: i64,
    ) -> Result<DotaSessionRecord, DotaSessionRepositoryError> {
        self.update(record, expected_revision, now)
    }

    /// Change only the phase through the same optimistic update boundary.
    pub fn transition_phase(
        &self,
        record: &DotaSessionRecord,
        phase: DotaSessionPhase,
        expected_revision: i64,
        now: i64,
    ) -> Result<DotaSessionRecord, DotaSessionRepositoryError> {
        let mut next = record.clone();
        next.phase = phase;
        self.update(&next, expected_revision, now)
    }

    /// Attach a Dota lobby ID and increment the lifecycle revision atomically.
    pub fn attach_lobby_id(
        &self,
        record: &DotaSessionRecord,
        lobby_id: impl Into<String>,
        expected_revision: i64,
        now: i64,
    ) -> Result<DotaSessionRecord, DotaSessionRepositoryError> {
        let mut next = record.clone();
        next.lobby_id = Some(lobby_id.into());
        self.update(&next, expected_revision, now)
    }

    /// Attach a Valve match ID and increment the lifecycle revision
    /// atomically.  The unique index rejects association with another row.
    pub fn attach_valve_match_id(
        &self,
        record: &DotaSessionRecord,
        valve_match_id: impl Into<String>,
        expected_revision: i64,
        now: i64,
    ) -> Result<DotaSessionRecord, DotaSessionRepositoryError> {
        let mut next = record.clone();
        next.valve_match_id = Some(valve_match_id.into());
        self.update(&next, expected_revision, now)
    }

    /// Close betting and mark the pending payload in the same immediate
    /// transaction used by the pending-match repository.  This wrapper keeps
    /// lobby orchestration free to depend on its own repository boundary.
    pub fn close_betting_now(
        &self,
        guild_id: i64,
        pending_match_id: i64,
        now: i64,
    ) -> Result<PendingBettingClose, DotaSessionRepositoryError> {
        PendingMatchRepository::new(&self.path)
            .close_betting_now(guild_id, pending_match_id, now)
            .map_err(Into::into)
    }
}

fn validate_record(record: &DotaSessionRecord) -> Result<(), DotaSessionRepositoryError> {
    if record.account_key.trim().is_empty() {
        return Err(DotaSessionRepositoryError::EmptyAccountKey);
    }
    // `as_str` is exhaustive over the enum and the match is kept here as a
    // single validation hook for any future storage-only phase additions.
    if DotaSessionPhase::parse(record.phase.as_str()).is_none() {
        return Err(DotaSessionRepositoryError::InvalidPhase {
            guild_id: record.guild_id,
            pending_match_id: record.pending_match_id,
            phase: record.phase.as_str().to_owned(),
        });
    }
    Ok(())
}

fn encode_payload(payload: &Value) -> Result<String, DotaSessionRepositoryError> {
    serde_json::to_string(payload)
        .map_err(|error| DotaSessionRepositoryError::EncodePayload(error.to_string()))
}

fn query_active_account(
    connection: &Connection,
    account_key: &str,
) -> Result<Option<DotaSessionRecord>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT guild_id, pending_match_id, account_key, phase,
                    lobby_id, valve_match_id, payload, revision,
                    created_at, updated_at, last_error
               FROM dota_sessions
              WHERE account_key=?1
                AND phase IN ('creating','gathering','launching','running',
                              'finishing','needs_review')
              ORDER BY updated_at, guild_id, pending_match_id LIMIT 1",
            [account_key],
            row_to_session,
        )
        .optional()
}

fn ensure_valve_match_available(
    connection: &Connection,
    valve_match_id: Option<&str>,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<(), DotaSessionRepositoryError> {
    let Some(valve_match_id) = valve_match_id else {
        return Ok(());
    };
    let conflict = connection
        .query_row(
            "SELECT guild_id, pending_match_id FROM dota_sessions
             WHERE valve_match_id=?1
               AND NOT (guild_id=?2 AND pending_match_id=?3)
             LIMIT 1",
            params![valve_match_id, guild_id, pending_match_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    if let Some((conflict_guild, conflict_pending)) = conflict {
        return Err(DotaSessionRepositoryError::ValveMatchAlreadyAssociated {
            valve_match_id: valve_match_id.to_owned(),
            guild_id: conflict_guild,
            pending_match_id: conflict_pending,
        });
    }
    Ok(())
}

fn query_session(
    connection: &Connection,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<Option<DotaSessionRecord>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT guild_id, pending_match_id, account_key, phase,
                    lobby_id, valve_match_id, payload, revision,
                    created_at, updated_at, last_error
               FROM dota_sessions
              WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
            row_to_session,
        )
        .optional()
}

fn row_to_session(row: &rusqlite::Row<'_>) -> Result<DotaSessionRecord, rusqlite::Error> {
    let guild_id = row.get(0)?;
    let pending_match_id = row.get(1)?;
    let phase_text: String = row.get(3)?;
    let phase = DotaSessionPhase::parse(&phase_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("unknown Dota session phase {phase_text:?}").into(),
        )
    })?;
    let payload_text: String = row.get(6)?;
    let payload = serde_json::from_str(&payload_text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(DotaSessionRecord {
        guild_id,
        pending_match_id,
        account_key: row.get(2)?,
        phase,
        lobby_id: row.get(4)?,
        valve_match_id: row.get(5)?,
        payload,
        revision: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        last_error: row.get(10)?,
    })
}

#[cfg(test)]
#[path = "dota_session/tests.rs"]
mod tests;
