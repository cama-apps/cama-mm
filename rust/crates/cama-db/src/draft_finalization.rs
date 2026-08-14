//! Atomic pending-match creation for a fenced Immortal Draft.
//!
//! The Draft envelope and pending match live in separate legacy tables. This
//! repository bridges them under one `BEGIN IMMEDIATE` transaction so a
//! process cannot publish an orphan pending match or lose the pending-match
//! identity from its recovery envelope.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use thiserror::Error;

use crate::draft_state::{DRAFT_STATE_ENVELOPE_VERSION, DRAFT_STATE_KEY, DraftStateRecord};
use crate::open_runtime_connection;

/// Deterministic idempotency identity for one Draft completion.
#[must_use]
pub fn draft_completion_key(guild_id: i64, session_id: u64) -> String {
    format!("draft:{guild_id}:{session_id}")
}

/// The linked Draft envelope and pending match returned by the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftFinalizationRecord {
    pub completion_key: String,
    pub pending_match_id: i64,
    pub pending_payload_json: String,
    pub pending_created: bool,
    pub draft: DraftStateRecord,
}

/// Failures from the atomic Draft-finalization seam.
#[derive(Debug, Error)]
pub enum DraftFinalizationError {
    #[error("draft finalization SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("draft state does not exist for guild {guild_id}")]
    NotFound { guild_id: i64 },
    #[error(
        "draft state session is stale for guild {guild_id}: expected {expected}, found {actual:?}"
    )]
    StaleSession {
        guild_id: i64,
        expected: u64,
        actual: Option<u64>,
    },
    #[error(
        "draft state revision is stale for guild {guild_id}: expected {expected}, found {actual:?}"
    )]
    StaleRevision {
        guild_id: i64,
        expected: u64,
        actual: Option<u64>,
    },
    #[error("draft finalization envelope is invalid: {0}")]
    InvalidDraftEnvelope(String),
    #[error("pending-match payload is invalid: {0}")]
    InvalidPendingPayload(String),
    #[error("draft finalization conflict for guild {guild_id}: {reason}")]
    Conflict { guild_id: i64, reason: String },
}

/// Path-backed atomic Draft-finalization repository.
#[derive(Clone, Debug)]
pub struct DraftFinalizationRepository {
    path: PathBuf,
}

impl DraftFinalizationRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Find or create exactly one pending match and link it into a fenced
    /// Draft envelope under the same immediate SQLite writer transaction.
    ///
    /// A retry of an already-linked completion returns the original row even
    /// when the caller still carries the pre-link revision. Any operation
    /// that would mutate an unlinked row still requires an exact revision.
    pub fn link_pending_match(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
    ) -> Result<DraftFinalizationRecord, DraftFinalizationError> {
        self.link_pending_match_validated(
            guild_id,
            expected_session_id,
            expected_revision,
            pending_payload_json,
            |_| Ok(()),
        )
    }

    /// Variant of [`Self::link_pending_match`] that lets the application
    /// validate its complete typed envelope while the writer transaction is
    /// still open. The validator runs before any pending row or Draft row is
    /// changed, so a future application-schema rejection cannot strand a
    /// pending match after this transaction commits.
    pub fn link_pending_match_validated<F>(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
        validate_draft_envelope: F,
    ) -> Result<DraftFinalizationRecord, DraftFinalizationError>
    where
        F: FnOnce(&str) -> Result<(), String>,
    {
        validate_pending_payload(pending_payload_json)?;
        let completion_key = draft_completion_key(guild_id, expected_session_id);
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_json = transaction
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
                params![guild_id, DRAFT_STATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(DraftFinalizationError::NotFound { guild_id })?;
        let mut envelope = parse_fenced_envelope(guild_id, &current_json)?;
        if envelope.session_id != expected_session_id {
            return Err(DraftFinalizationError::StaleSession {
                guild_id,
                expected: expected_session_id,
                actual: Some(envelope.session_id),
            });
        }
        validate_draft_envelope(&current_json)
            .map_err(DraftFinalizationError::InvalidDraftEnvelope)?;

        let keyed = pending_by_completion_key(&transaction, &completion_key)?;
        if let Some(linked_id) = envelope.pending_match_id {
            let linked = pending_by_id(&transaction, guild_id, linked_id)?.ok_or_else(|| {
                DraftFinalizationError::Conflict {
                    guild_id,
                    reason: format!("linked pending match {linked_id} does not exist"),
                }
            })?;
            match linked.completion_key.as_deref() {
                Some(key) if key == completion_key => {
                    if keyed
                        .as_ref()
                        .is_some_and(|row| row.pending_match_id != linked_id)
                    {
                        return Err(DraftFinalizationError::Conflict {
                            guild_id,
                            reason: "completion key identifies a different pending match"
                                .to_owned(),
                        });
                    }
                    ensure_same_pending_payload(guild_id, &linked.payload, pending_payload_json)?;
                    let draft = envelope.record(current_json)?;
                    transaction.commit()?;
                    return Ok(DraftFinalizationRecord {
                        completion_key,
                        pending_match_id: linked_id,
                        pending_payload_json: linked.payload,
                        pending_created: false,
                        draft,
                    });
                }
                Some(_) => {
                    return Err(DraftFinalizationError::Conflict {
                        guild_id,
                        reason: format!(
                            "linked pending match {linked_id} belongs to another completion"
                        ),
                    });
                }
                None => {
                    ensure_revision(guild_id, expected_revision, envelope.revision)?;
                    ensure_same_pending_payload(guild_id, &linked.payload, pending_payload_json)?;
                    if keyed
                        .as_ref()
                        .is_some_and(|row| row.pending_match_id != linked_id)
                    {
                        return Err(DraftFinalizationError::Conflict {
                            guild_id,
                            reason: "completion key already identifies a different pending match"
                                .to_owned(),
                        });
                    }
                    let changed = transaction.execute(
                        "UPDATE pending_matches
                            SET completion_key=?1,updated_at=CURRENT_TIMESTAMP
                          WHERE guild_id=?2 AND pending_match_id=?3 AND completion_key IS NULL",
                        params![completion_key, guild_id, linked_id],
                    )?;
                    if changed != 1 {
                        return Err(DraftFinalizationError::Conflict {
                            guild_id,
                            reason: "linked pending match changed while adopting completion key"
                                .to_owned(),
                        });
                    }
                    let draft = envelope.record(current_json)?;
                    transaction.commit()?;
                    return Ok(DraftFinalizationRecord {
                        completion_key,
                        pending_match_id: linked_id,
                        pending_payload_json: linked.payload,
                        pending_created: false,
                        draft,
                    });
                }
            }
        }

        ensure_revision(guild_id, expected_revision, envelope.revision)?;
        let (pending_match_id, persisted_payload, pending_created) = if let Some(keyed) = keyed {
            if keyed.guild_id != guild_id {
                return Err(DraftFinalizationError::Conflict {
                    guild_id,
                    reason: "completion key belongs to another guild".to_owned(),
                });
            }
            ensure_same_pending_payload(guild_id, &keyed.payload, pending_payload_json)?;
            (keyed.pending_match_id, keyed.payload, false)
        } else {
            transaction.execute(
                "INSERT INTO pending_matches(guild_id,payload,completion_key,updated_at)
                 VALUES (?1,?2,?3,CURRENT_TIMESTAMP)",
                params![guild_id, pending_payload_json, completion_key],
            )?;
            (
                transaction.last_insert_rowid(),
                pending_payload_json.to_owned(),
                true,
            )
        };

        let next_revision =
            envelope
                .revision
                .checked_add(1)
                .ok_or_else(|| DraftFinalizationError::Conflict {
                    guild_id,
                    reason: "draft revision exhausted".to_owned(),
                })?;
        envelope.revision = next_revision;
        envelope.pending_match_id = Some(pending_match_id);
        let updated_json = envelope.encode()?;
        let changed = transaction.execute(
            "UPDATE app_kv SET value=?1
             WHERE guild_id=?2 AND key=?3 AND value=?4",
            params![updated_json, guild_id, DRAFT_STATE_KEY, current_json],
        )?;
        if changed != 1 {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "draft row changed while linking pending match".to_owned(),
            });
        }
        let draft = envelope.record(updated_json)?;
        transaction.commit()?;
        Ok(DraftFinalizationRecord {
            completion_key,
            pending_match_id,
            pending_payload_json: persisted_payload,
            pending_created,
            draft,
        })
    }

    /// Load the exact raw payload for the pending match already named by a
    /// fenced Draft. This is intentionally byte-preserving: callers can feed
    /// the value back into [`Self::link_pending_match_validated`] without
    /// losing future JSON fields or changing key order/whitespace.
    pub fn linked_pending_payload(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<String, DraftFinalizationError> {
        let completion_key = draft_completion_key(guild_id, session_id);
        let connection = open_runtime_connection(&self.path)?;
        let pending = connection
            .query_row(
                "SELECT pending_match_id,guild_id,payload,completion_key
                   FROM pending_matches
                  WHERE guild_id=?1 AND pending_match_id=?2",
                params![guild_id, pending_match_id],
                raw_pending_from_row,
            )
            .optional()?
            .ok_or_else(|| DraftFinalizationError::Conflict {
                guild_id,
                reason: format!("linked pending match {pending_match_id} does not exist"),
            })?;
        if pending
            .completion_key
            .as_deref()
            .is_some_and(|key| key != completion_key)
        {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: format!(
                    "linked pending match {pending_match_id} belongs to another completion"
                ),
            });
        }
        Ok(pending.payload)
    }
}

#[derive(Debug)]
struct ParsedDraftEnvelope {
    value: Value,
    guild_id: i64,
    session_id: u64,
    revision: u64,
    pending_match_id: Option<i64>,
}

impl ParsedDraftEnvelope {
    fn encode(&mut self) -> Result<String, DraftFinalizationError> {
        let object = self.value.as_object_mut().ok_or_else(|| {
            DraftFinalizationError::InvalidDraftEnvelope("expected an object".to_owned())
        })?;
        object.insert("revision".to_owned(), Value::from(self.revision));
        object.insert(
            "pending_match_id".to_owned(),
            self.pending_match_id.map_or(Value::Null, Value::from),
        );
        serde_json::to_string(&self.value)
            .map_err(|error| DraftFinalizationError::InvalidDraftEnvelope(error.to_string()))
    }

    fn record(&self, envelope_json: String) -> Result<DraftStateRecord, DraftFinalizationError> {
        Ok(DraftStateRecord {
            guild_id: self.guild_id,
            session_id: self.session_id,
            revision: self.revision,
            envelope_json,
        })
    }
}

#[derive(Debug)]
struct RawPendingMatch {
    pending_match_id: i64,
    guild_id: i64,
    payload: String,
    completion_key: Option<String>,
}

fn parse_fenced_envelope(
    guild_id: i64,
    raw: &str,
) -> Result<ParsedDraftEnvelope, DraftFinalizationError> {
    let mut value: Value = serde_json::from_str(raw)
        .map_err(|error| DraftFinalizationError::InvalidDraftEnvelope(error.to_string()))?;
    let object = value.as_object_mut().ok_or_else(|| {
        DraftFinalizationError::InvalidDraftEnvelope("expected an object".to_owned())
    })?;
    let schema_version = object
        .get("schema_version")
        .or_else(|| object.get("version"))
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            DraftFinalizationError::InvalidDraftEnvelope("missing schema_version".to_owned())
        })?;
    if schema_version != DRAFT_STATE_ENVELOPE_VERSION {
        return Err(DraftFinalizationError::InvalidDraftEnvelope(format!(
            "unsupported schema_version {schema_version}"
        )));
    }
    let envelope_guild = object
        .get("guild_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            DraftFinalizationError::InvalidDraftEnvelope("missing guild_id".to_owned())
        })?;
    if envelope_guild != guild_id {
        return Err(DraftFinalizationError::InvalidDraftEnvelope(format!(
            "guild row {guild_id} contains envelope for guild {envelope_guild}"
        )));
    }
    let session_id = object
        .get("session_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            DraftFinalizationError::InvalidDraftEnvelope("missing session_id".to_owned())
        })?;
    let revision = object
        .get("revision")
        .and_then(Value::as_u64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| {
            DraftFinalizationError::InvalidDraftEnvelope("invalid revision".to_owned())
        })?;
    if object.get("active").and_then(Value::as_bool) != Some(true) {
        return Err(DraftFinalizationError::Conflict {
            guild_id,
            reason: "draft is not active".to_owned(),
        });
    }
    if object.get("finalizing").and_then(Value::as_bool) != Some(true) {
        return Err(DraftFinalizationError::Conflict {
            guild_id,
            reason: "draft is not fenced for finalization".to_owned(),
        });
    }
    let state = object
        .get("state")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            DraftFinalizationError::InvalidDraftEnvelope("missing state object".to_owned())
        })?;
    if state.get("guild_id").and_then(Value::as_i64) != Some(guild_id) {
        return Err(DraftFinalizationError::InvalidDraftEnvelope(
            "embedded state guild does not match envelope".to_owned(),
        ));
    }
    if state.get("session_id").and_then(Value::as_u64) != Some(session_id) {
        return Err(DraftFinalizationError::InvalidDraftEnvelope(
            "embedded state session does not match envelope".to_owned(),
        ));
    }
    if state.get("phase").and_then(Value::as_str) != Some("complete") {
        return Err(DraftFinalizationError::Conflict {
            guild_id,
            reason: "draft phase is not complete".to_owned(),
        });
    }
    let pending_match_id = match object.get("pending_match_id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_i64().filter(|id| *id > 0).ok_or_else(|| {
            DraftFinalizationError::InvalidDraftEnvelope(
                "pending_match_id must be a positive integer or null".to_owned(),
            )
        })?),
    };
    object.insert("revision".to_owned(), Value::from(revision));
    object.insert(
        "pending_match_id".to_owned(),
        pending_match_id.map_or(Value::Null, Value::from),
    );
    Ok(ParsedDraftEnvelope {
        value,
        guild_id,
        session_id,
        revision,
        pending_match_id,
    })
}

fn ensure_revision(
    guild_id: i64,
    expected: u64,
    actual: u64,
) -> Result<(), DraftFinalizationError> {
    if expected == actual {
        Ok(())
    } else {
        Err(DraftFinalizationError::StaleRevision {
            guild_id,
            expected,
            actual: Some(actual),
        })
    }
}

fn validate_pending_payload(raw: &str) -> Result<(), DraftFinalizationError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| DraftFinalizationError::InvalidPendingPayload(error.to_string()))?;
    if value.is_object() {
        Ok(())
    } else {
        Err(DraftFinalizationError::InvalidPendingPayload(
            "root value must be an object".to_owned(),
        ))
    }
}

fn ensure_same_pending_payload(
    guild_id: i64,
    persisted: &str,
    requested: &str,
) -> Result<(), DraftFinalizationError> {
    if persisted == requested {
        Ok(())
    } else {
        Err(DraftFinalizationError::Conflict {
            guild_id,
            reason: "completion key was retried with a different pending-match payload".to_owned(),
        })
    }
}

fn pending_by_completion_key(
    transaction: &Transaction<'_>,
    completion_key: &str,
) -> Result<Option<RawPendingMatch>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT pending_match_id,guild_id,payload,completion_key
             FROM pending_matches WHERE completion_key=?1",
            [completion_key],
            raw_pending_from_row,
        )
        .optional()
}

fn pending_by_id(
    transaction: &Transaction<'_>,
    guild_id: i64,
    pending_match_id: i64,
) -> Result<Option<RawPendingMatch>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT pending_match_id,guild_id,payload,completion_key
             FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
            raw_pending_from_row,
        )
        .optional()
}

fn raw_pending_from_row(row: &rusqlite::Row<'_>) -> Result<RawPendingMatch, rusqlite::Error> {
    Ok(RawPendingMatch {
        pending_match_id: row.get(0)?,
        guild_id: row.get(1)?,
        payload: row.get(2)?,
        completion_key: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::draft_state::DraftStateRepository;

    fn fixture() -> NamedTempFile {
        let file = NamedTempFile::new().expect("create migrated fixture");
        crate::schema_manager::initialize_or_migrate(file.path())
            .expect("initialize migrated fixture");
        file
    }

    fn fenced(repository: &DraftStateRepository, guild_id: i64) -> DraftStateRecord {
        repository
            .create_envelope(
                guild_id,
                &json!({
                    "schema_version": 1,
                    "guild_id": guild_id,
                    "session_id": 0,
                    "revision": 1,
                    "active": true,
                    "finalizing": true,
                    "pending_match_id": null,
                    "future_envelope": {"keep": [1, 2, 3]},
                    "state": {
                        "guild_id": guild_id,
                        "session_id": 0,
                        "phase": "complete",
                        "future_state": "preserved"
                    }
                })
                .to_string(),
            )
            .expect("create fenced draft")
    }

    #[test]
    fn migrated_atomic_link_preserves_unknown_json_and_retries_same_pending_match() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let payload = "{\"radiant_team_ids\":[1,2],\"future_pending\":{\"raw\":true}}";

        let linked = repository
            .link_pending_match(42, draft.session_id, draft.revision, payload)
            .expect("link pending match");
        assert!(linked.pending_created);
        assert_eq!(
            linked.completion_key,
            format!("draft:42:{}", draft.session_id)
        );
        assert_eq!(linked.pending_payload_json, payload);
        assert_eq!(linked.draft.revision, 2);
        let envelope: Value =
            serde_json::from_str(linked.draft.raw_json()).expect("decode linked envelope");
        assert_eq!(envelope["pending_match_id"], json!(linked.pending_match_id));
        assert_eq!(envelope["future_envelope"], json!({"keep": [1, 2, 3]}));
        assert_eq!(envelope["state"]["future_state"], json!("preserved"));

        // Simulate a crash after commit: reload the current revision and
        // retry the exact operation. No second row or revision is consumed.
        let reloaded = drafts.load(42).expect("reload draft").expect("draft row");
        let retried = repository
            .link_pending_match(42, draft.session_id, reloaded.revision, payload)
            .expect("retry linked pending match");
        assert!(!retried.pending_created);
        assert_eq!(retried.pending_match_id, linked.pending_match_id);
        assert_eq!(retried.pending_payload_json, payload);
        assert_eq!(retried.draft.revision, linked.draft.revision);
        assert!(matches!(
            repository.link_pending_match(
                42,
                draft.session_id,
                reloaded.revision,
                "{\"different_retry_payload\":true}",
            ),
            Err(DraftFinalizationError::Conflict { .. })
        ));
        let connection = Connection::open(file.path()).expect("open migrated fixture");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count pending rows"),
            1
        );
    }

    #[test]
    fn concurrent_link_calls_return_one_pending_match() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let repository = Arc::new(DraftFinalizationRepository::new(file.path()));
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let repository = Arc::clone(&repository);
                let barrier = Arc::clone(&barrier);
                let draft = draft.clone();
                thread::spawn(move || {
                    barrier.wait();
                    repository.link_pending_match(
                        42,
                        draft.session_id,
                        draft.revision,
                        "{\"same_concurrent_payload\":true}",
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("join finalizer"))
            .collect::<Vec<_>>();
        assert!(results.iter().all(Result::is_ok));
        let ids = results
            .into_iter()
            .map(|result| result.expect("concurrent result").pending_match_id)
            .collect::<Vec<_>>();
        assert_eq!(ids[0], ids[1]);
        let connection = Connection::open(file.path()).expect("open migrated fixture");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count pending rows"),
            1
        );
    }

    #[test]
    fn stale_revision_and_session_do_not_create_or_link_pending_match() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let stale_revision = repository
            .link_pending_match(42, draft.session_id, draft.revision + 1, "{}")
            .expect_err("reject stale revision");
        assert!(matches!(
            stale_revision,
            DraftFinalizationError::StaleRevision { .. }
        ));
        let stale_session = repository
            .link_pending_match(42, draft.session_id + 1, draft.revision, "{}")
            .expect_err("reject stale session");
        assert!(matches!(
            stale_session,
            DraftFinalizationError::StaleSession { .. }
        ));
        let connection = Connection::open(file.path()).expect("open migrated fixture");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count pending rows"),
            0
        );
        assert_eq!(drafts.load(42).expect("reload draft"), Some(draft));
    }

    #[test]
    fn malformed_schema_or_embedded_identity_never_creates_pending_match() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let valid: Value = serde_json::from_str(draft.raw_json()).expect("decode valid envelope");
        let malformed = [
            {
                let mut value = valid.clone();
                value["schema_version"] = json!(99);
                value
            },
            {
                let mut value = valid.clone();
                value["state"]["guild_id"] = json!(99);
                value
            },
            {
                let mut value = valid;
                value["state"]["session_id"] = json!(draft.session_id + 1);
                value
            },
        ];

        for value in malformed {
            let connection = Connection::open(file.path()).expect("open migrated fixture");
            connection
                .execute(
                    "UPDATE app_kv SET value=?1 WHERE guild_id=42 AND key=?2",
                    params![value.to_string(), DRAFT_STATE_KEY],
                )
                .expect("install malformed envelope");
            drop(connection);
            assert!(matches!(
                repository.link_pending_match(42, draft.session_id, draft.revision, "{}"),
                Err(DraftFinalizationError::InvalidDraftEnvelope(_))
            ));
            let connection = Connection::open(file.path()).expect("reopen migrated fixture");
            assert_eq!(
                connection
                    .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                        .get::<_, i64>(0))
                    .expect("count pending rows"),
                0
            );
        }
    }

    #[test]
    fn legacy_linked_pending_match_requires_exact_payload_before_key_adoption() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let legacy_payload = "{\"legacy_pending\":true,\"future\":7}";
        let connection = Connection::open(file.path()).expect("open migrated fixture");
        connection
            .execute(
                "INSERT INTO pending_matches(guild_id,payload,updated_at)
                 VALUES (42,?1,CURRENT_TIMESTAMP)",
                [legacy_payload],
            )
            .expect("insert legacy pending row");
        let pending_match_id = connection.last_insert_rowid();
        drop(connection);
        let mut linked_envelope: Value =
            serde_json::from_str(draft.raw_json()).expect("decode draft envelope");
        linked_envelope["pending_match_id"] = json!(pending_match_id);
        let linked_draft = drafts
            .replace_envelope_if_revision(
                42,
                draft.session_id,
                draft.revision,
                &linked_envelope.to_string(),
            )
            .expect("install legacy pending link");

        assert!(matches!(
            repository.link_pending_match(
                42,
                draft.session_id,
                linked_draft.revision,
                "{\"unrelated\":true}",
            ),
            Err(DraftFinalizationError::Conflict { .. })
        ));
        let connection = Connection::open(file.path()).expect("open migrated fixture");
        assert_eq!(
            connection
                .query_row(
                    "SELECT completion_key FROM pending_matches WHERE pending_match_id=?1",
                    [pending_match_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .expect("load unadopted key"),
            None
        );
        drop(connection);

        let adopted = repository
            .link_pending_match(42, draft.session_id, linked_draft.revision, legacy_payload)
            .expect("adopt exact legacy pending link");
        assert_eq!(adopted.pending_match_id, pending_match_id);
        assert!(!adopted.pending_created);
        assert_eq!(adopted.draft.revision, linked_draft.revision);
    }

    #[test]
    fn draft_update_failure_rolls_back_pending_insert_and_retry_succeeds() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let connection = Connection::open(file.path()).expect("open migrated fixture");
        connection
            .execute_batch(
                "CREATE TRIGGER fail_draft_finalization
                 BEFORE UPDATE ON app_kv
                 WHEN OLD.guild_id=42 AND OLD.key='draft:state'
                 BEGIN
                   SELECT RAISE(ABORT, 'simulated draft update failure');
                 END;",
            )
            .expect("install failure trigger");
        drop(connection);

        assert!(matches!(
            repository.link_pending_match(42, draft.session_id, draft.revision, "{\"x\":1}"),
            Err(DraftFinalizationError::Sqlite(_))
        ));
        let connection = Connection::open(file.path()).expect("open migrated fixture");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count rolled-back pending rows"),
            0
        );
        connection
            .execute_batch("DROP TRIGGER fail_draft_finalization")
            .expect("drop failure trigger");
        drop(connection);
        assert_eq!(drafts.load(42).expect("reload draft"), Some(draft.clone()));
        assert!(
            repository
                .link_pending_match(42, draft.session_id, draft.revision, "{\"x\":1}")
                .is_ok()
        );
    }
}
