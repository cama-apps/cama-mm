//! Existing-schema persistence for Immortal Draft state.
//!
//! The draft payload is deliberately opaque to `cama-db`.  The application
//! owns the serde-compatible state shape; this repository owns only the
//! durable envelope, the guild key, and the optimistic-concurrency/session
//! sequence rules.  Both session allocation and state creation/replacement
//! use `BEGIN IMMEDIATE` so a restart cannot reuse a session id or publish a
//! partially written state.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use thiserror::Error;

use crate::open_runtime_connection;

/// The generic key containing one active draft envelope per guild.
pub const DRAFT_STATE_KEY: &str = "draft:state";
/// The global key containing the last allocated draft session id.
pub const DRAFT_SESSION_SEQUENCE_KEY: &str = "draft:session_sequence";
/// Version of the durable draft envelope written by this adapter.
pub const DRAFT_STATE_ENVELOPE_VERSION: u64 = 1;

/// One raw draft envelope loaded from `app_kv`.
///
/// `envelope_json` is intentionally retained as text.  The application may
/// evolve its state payload without making the database crate depend on that
/// application type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftStateRecord {
    /// Guild owning the active draft.  `0` is the normalized DM key.
    pub guild_id: i64,
    /// Monotonic session identity from [`DRAFT_SESSION_SEQUENCE_KEY`].
    pub session_id: u64,
    /// Optimistic-concurrency revision of this envelope.
    pub revision: u64,
    /// Raw JSON envelope persisted in `app_kv.value`.
    pub envelope_json: String,
}

/// Backwards-friendly name for a loaded raw draft row.
pub type StoredDraftState = DraftStateRecord;

impl DraftStateRecord {
    /// Return the persisted raw envelope without parsing its application
    /// payload.
    #[must_use]
    pub fn raw_json(&self) -> &str {
        &self.envelope_json
    }
}

/// Errors returned by the draft state repository.
#[derive(Debug, Error)]
pub enum DraftStateRepositoryError {
    /// The existing SQLite database rejected an operation.
    #[error("draft state SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The supplied state payload was not valid JSON.
    #[error("draft state payload is not valid JSON: {0}")]
    InvalidPayload(String),
    /// A stored envelope is missing required metadata or has invalid types.
    #[error("draft state envelope is invalid: {0}")]
    InvalidEnvelope(String),
    /// A guild already has an active draft row.
    #[error("draft state already exists for guild {guild_id}")]
    Duplicate { guild_id: i64 },
    /// A requested guild row does not exist.
    #[error("draft state does not exist for guild {guild_id}")]
    NotFound { guild_id: i64 },
    /// An optimistic-concurrency token no longer identifies the current row.
    #[error(
        "draft state revision is stale for guild {guild_id}: expected {expected}, found {actual:?}"
    )]
    StaleRevision {
        guild_id: i64,
        expected: u64,
        actual: Option<u64>,
    },
    /// A stale callback belongs to an older draft session, even when its
    /// revision number happens to match a replacement session.
    #[error(
        "draft state session is stale for guild {guild_id}: expected {expected}, found {actual:?}"
    )]
    StaleSession {
        guild_id: i64,
        expected: u64,
        actual: Option<u64>,
    },
    /// The row changed in a way that cannot be represented by a normal CAS
    /// result.  This remains distinct from a stale caller token so callers
    /// can retry only the latter.
    #[error("draft state transaction conflict for guild {guild_id}: {reason}")]
    Conflict { guild_id: i64, reason: String },
    /// The signed SQLite integer sequence cannot allocate another id.
    #[error("draft session sequence is exhausted")]
    SessionSequenceExhausted,
    /// The persisted global sequence is malformed.
    #[error("draft session sequence is invalid: {0}")]
    InvalidSessionSequence(String),
}

/// Path-backed repository for the active Immortal Draft envelopes.
#[derive(Clone, Debug)]
pub struct DraftStateRepository {
    path: PathBuf,
}

impl DraftStateRepository {
    /// Open a repository against an already migrated SQLite database.
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Load every active guild draft in deterministic guild order.
    pub fn load_all(&self) -> Result<Vec<DraftStateRecord>, DraftStateRepositoryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT guild_id, value
             FROM app_kv
             WHERE key = ?1
             ORDER BY guild_id",
        )?;
        let rows = statement.query_map([DRAFT_STATE_KEY], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (guild_id, envelope_json) = row?;
            record_from_envelope(guild_id, envelope_json)
        })
        .collect()
    }

    /// Load one guild's active draft, if present.
    pub fn load(
        &self,
        guild_id: i64,
    ) -> Result<Option<DraftStateRecord>, DraftStateRepositoryError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id = ?1 AND key = ?2",
                params![guild_id, DRAFT_STATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|envelope_json| record_from_envelope(guild_id, envelope_json))
            .transpose()
    }

    /// Atomically allocate a never-before-used session id and create a guild
    /// envelope at revision one.
    ///
    /// `state_json` is the application-owned payload, not the outer durable
    /// envelope.  It is retained as JSON under the envelope's `state` field.
    /// If the payload is an object with a `session_id` field, that field is
    /// set to the newly allocated identity as part of the same transaction.
    pub fn create(
        &self,
        guild_id: i64,
        state_json: &str,
    ) -> Result<DraftStateRecord, DraftStateRepositoryError> {
        let state = parse_payload(state_json)?;
        let input = serde_json::to_string(&json!({ "state": state }))
            .map_err(|error| DraftStateRepositoryError::InvalidPayload(error.to_string()))?;
        self.create_envelope(guild_id, &input)
    }

    /// Atomically allocate a session id and create a raw JSON envelope.
    ///
    /// The input may be a complete envelope (in which case all unknown
    /// application metadata is preserved) or a bare state payload (the
    /// compatibility form accepted by [`Self::create`]).  Database-owned
    /// guild/session/revision fields are overwritten inside the transaction.
    pub fn create_envelope(
        &self,
        guild_id: i64,
        envelope_json: &str,
    ) -> Result<DraftStateRecord, DraftStateRepositoryError> {
        let input = parse_payload(envelope_json)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        if transaction
            .query_row(
                "SELECT 1 FROM app_kv WHERE guild_id = ?1 AND key = ?2",
                params![guild_id, DRAFT_STATE_KEY],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(DraftStateRepositoryError::Duplicate { guild_id });
        }

        let session_id = allocate_session_id(&transaction)?;
        let envelope_json = encode_envelope(guild_id, session_id, 1, input, None)?;
        transaction.execute(
            "INSERT INTO app_kv(guild_id, key, value) VALUES (?1, ?2, ?3)",
            params![guild_id, DRAFT_STATE_KEY, &envelope_json],
        )?;
        transaction.commit()?;
        Ok(DraftStateRecord {
            guild_id,
            session_id,
            revision: 1,
            envelope_json,
        })
    }

    /// Alias making the payload-vs-envelope distinction explicit at call
    /// sites that persist a serde snapshot.
    pub fn create_from_state_json(
        &self,
        guild_id: i64,
        state_json: &str,
    ) -> Result<DraftStateRecord, DraftStateRepositoryError> {
        self.create(guild_id, state_json)
    }

    /// Replace a guild's payload only when `expected_revision` is current.
    /// The session identity is preserved and the revision advances by one.
    pub fn replace_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        state_json: &str,
    ) -> Result<DraftStateRecord, DraftStateRepositoryError> {
        let state = parse_payload(state_json)?;
        let input = serde_json::to_string(&json!({ "state": state }))
            .map_err(|error| DraftStateRepositoryError::InvalidPayload(error.to_string()))?;
        self.replace_envelope_if_revision(guild_id, expected_session_id, expected_revision, &input)
    }

    /// Replace a raw JSON envelope only when both session identity and
    /// revision are current.  Unknown envelope metadata is retained.
    pub fn replace_envelope_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        envelope_json: &str,
    ) -> Result<DraftStateRecord, DraftStateRepositoryError> {
        let input = parse_payload(envelope_json)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_json = transaction
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id = ?1 AND key = ?2",
                params![guild_id, DRAFT_STATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(current_json) = current_json else {
            return Err(DraftStateRepositoryError::NotFound { guild_id });
        };
        let current = record_from_envelope(guild_id, current_json)?;
        if current.session_id != expected_session_id {
            return Err(DraftStateRepositoryError::StaleSession {
                guild_id,
                expected: expected_session_id,
                actual: Some(current.session_id),
            });
        }
        if current.revision != expected_revision {
            return Err(DraftStateRepositoryError::StaleRevision {
                guild_id,
                expected: expected_revision,
                actual: Some(current.revision),
            });
        }
        let next_revision =
            current
                .revision
                .checked_add(1)
                .ok_or(DraftStateRepositoryError::Conflict {
                    guild_id,
                    reason: "revision exhausted".to_owned(),
                })?;
        let current_value: Value = serde_json::from_str(&current.envelope_json)
            .map_err(|error| DraftStateRepositoryError::InvalidEnvelope(error.to_string()))?;
        let envelope_json = encode_envelope(
            guild_id,
            current.session_id,
            next_revision,
            input,
            Some(&current_value),
        )?;
        let changed = transaction.execute(
            "UPDATE app_kv SET value = ?1
             WHERE guild_id = ?2 AND key = ?3 AND value = ?4",
            params![
                &envelope_json,
                guild_id,
                DRAFT_STATE_KEY,
                &current.envelope_json
            ],
        )?;
        if changed != 1 {
            return Err(DraftStateRepositoryError::Conflict {
                guild_id,
                reason: "draft row changed during replacement".to_owned(),
            });
        }
        transaction.commit()?;
        Ok(DraftStateRecord {
            guild_id,
            session_id: current.session_id,
            revision: next_revision,
            envelope_json,
        })
    }

    /// Delete one guild's active draft only when its revision is current.
    /// The global session sequence is intentionally retained, so a later
    /// draft cannot reuse the deleted session identity.
    pub fn delete_if_revision(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
    ) -> Result<bool, DraftStateRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_json = transaction
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id = ?1 AND key = ?2",
                params![guild_id, DRAFT_STATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(current_json) = current_json else {
            return Err(DraftStateRepositoryError::NotFound { guild_id });
        };
        let current = record_from_envelope(guild_id, current_json)?;
        if current.session_id != expected_session_id {
            return Err(DraftStateRepositoryError::StaleSession {
                guild_id,
                expected: expected_session_id,
                actual: Some(current.session_id),
            });
        }
        if current.revision != expected_revision {
            return Err(DraftStateRepositoryError::StaleRevision {
                guild_id,
                expected: expected_revision,
                actual: Some(current.revision),
            });
        }
        let changed = transaction.execute(
            "DELETE FROM app_kv WHERE guild_id = ?1 AND key = ?2 AND value = ?3",
            params![guild_id, DRAFT_STATE_KEY, &current.envelope_json],
        )?;
        if changed != 1 {
            return Err(DraftStateRepositoryError::Conflict {
                guild_id,
                reason: "draft row changed during deletion".to_owned(),
            });
        }
        transaction.commit()?;
        Ok(true)
    }
}

fn parse_payload(state_json: &str) -> Result<Value, DraftStateRepositoryError> {
    serde_json::from_str(state_json)
        .map_err(|error| DraftStateRepositoryError::InvalidPayload(error.to_string()))
}

fn encode_envelope(
    guild_id: i64,
    session_id: u64,
    revision: u64,
    input: Value,
    previous: Option<&Value>,
) -> Result<String, DraftStateRepositoryError> {
    let (mut object, mut state) = match input {
        Value::Object(mut object) if object.contains_key("state") => {
            let state = object.remove("state").ok_or_else(|| {
                DraftStateRepositoryError::InvalidEnvelope("missing state".to_owned())
            })?;
            (object, state)
        }
        state => (serde_json::Map::new(), state),
    };
    if let Some(previous) = previous.and_then(Value::as_object) {
        for (key, value) in previous {
            if !matches!(
                key.as_str(),
                "schema_version" | "guild_id" | "session_id" | "revision" | "state"
            ) && !object.contains_key(key)
            {
                object.insert(key.clone(), value.clone());
            }
        }
    }
    if let Value::Object(object) = &mut state {
        object.insert("session_id".to_owned(), json!(session_id));
    }
    object.insert(
        "schema_version".to_owned(),
        json!(DRAFT_STATE_ENVELOPE_VERSION),
    );
    object.insert("guild_id".to_owned(), json!(guild_id));
    object.insert("session_id".to_owned(), json!(session_id));
    object.insert("revision".to_owned(), json!(revision));
    object
        .entry("deadline_at".to_owned())
        .or_insert(Value::Null);
    object.entry("active".to_owned()).or_insert(json!(true));
    object
        .entry("finalizing".to_owned())
        .or_insert(json!(false));
    object
        .entry("pending_match_id".to_owned())
        .or_insert(Value::Null);
    object.insert("state".to_owned(), state);
    serde_json::to_string(&Value::Object(object))
        .map_err(|error| DraftStateRepositoryError::InvalidEnvelope(error.to_string()))
}

fn record_from_envelope(
    guild_id: i64,
    envelope_json: String,
) -> Result<DraftStateRecord, DraftStateRepositoryError> {
    let value: Value = serde_json::from_str(&envelope_json)
        .map_err(|error| DraftStateRepositoryError::InvalidEnvelope(error.to_string()))?;
    let object = value.as_object().ok_or_else(|| {
        DraftStateRepositoryError::InvalidEnvelope("expected an object".to_owned())
    })?;
    let schema_version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            DraftStateRepositoryError::InvalidEnvelope("missing schema_version".to_owned())
        })?;
    if schema_version != DRAFT_STATE_ENVELOPE_VERSION {
        return Err(DraftStateRepositoryError::InvalidEnvelope(format!(
            "unsupported schema_version {schema_version}"
        )));
    }
    let envelope_guild_id = object
        .get("guild_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| DraftStateRepositoryError::InvalidEnvelope("missing guild_id".to_owned()))?;
    if envelope_guild_id != guild_id {
        return Err(DraftStateRepositoryError::InvalidEnvelope(format!(
            "guild row {guild_id} contains envelope for guild {envelope_guild_id}"
        )));
    }
    let session_id = object
        .get("session_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            DraftStateRepositoryError::InvalidEnvelope("missing session_id".to_owned())
        })?;
    let revision = object
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| DraftStateRepositoryError::InvalidEnvelope("missing revision".to_owned()))?;
    if revision == 0 {
        return Err(DraftStateRepositoryError::InvalidEnvelope(
            "revision must be positive".to_owned(),
        ));
    }
    if object.get("state").is_none() {
        return Err(DraftStateRepositoryError::InvalidEnvelope(
            "missing state".to_owned(),
        ));
    }
    Ok(DraftStateRecord {
        guild_id,
        session_id,
        revision,
        envelope_json,
    })
}

fn allocate_session_id(transaction: &Transaction<'_>) -> Result<u64, DraftStateRepositoryError> {
    let current_raw = transaction
        .query_row(
            "SELECT value FROM app_kv WHERE guild_id = 0 AND key = ?1",
            [DRAFT_SESSION_SEQUENCE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let current = match current_raw {
        None => 0,
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| DraftStateRepositoryError::InvalidSessionSequence(raw))?,
    };
    let next = current
        .checked_add(1)
        .filter(|next| i64::try_from(*next).is_ok())
        .ok_or(DraftStateRepositoryError::SessionSequenceExhausted)?;
    transaction.execute(
        "INSERT INTO app_kv(guild_id, key, value) VALUES (0, ?1, ?2)
         ON CONFLICT(guild_id, key) DO UPDATE SET value = excluded.value",
        params![DRAFT_SESSION_SEQUENCE_KEY, next.to_string()],
    )?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    fn migrated_fixture() -> NamedTempFile {
        let file = NamedTempFile::new().expect("create migrated fixture");
        crate::schema_manager::initialize_or_migrate(file.path())
            .expect("initialize migrated fixture");
        file
    }

    fn payload(label: &str) -> String {
        json!({"label": label, "phase": "coinflip"}).to_string()
    }

    #[test]
    fn migrated_sqlite_roundtrip_preserves_raw_json_envelope() {
        let fixture = migrated_fixture();
        let repository = DraftStateRepository::new(fixture.path());
        let created = repository
            .create(42, &payload("first"))
            .expect("create draft");
        assert_eq!(created.guild_id, 42);
        assert_eq!(created.session_id, 1);
        assert_eq!(created.revision, 1);
        let envelope: Value = serde_json::from_str(created.raw_json()).expect("decode envelope");
        assert_eq!(envelope["schema_version"], json!(1));
        assert_eq!(envelope["state"]["label"], json!("first"));
        assert_eq!(envelope["state"]["session_id"], json!(1));
        assert_eq!(
            repository.load(42).expect("load draft"),
            Some(created.clone())
        );
        assert_eq!(repository.load_all().expect("load all"), vec![created]);
    }

    #[test]
    fn envelope_metadata_survives_atomic_create_replace_and_reload() {
        let fixture = migrated_fixture();
        let repository = DraftStateRepository::new(fixture.path());
        let requested = json!({
            "schema_version": 1,
            "guild_id": 42,
            "session_id": 0,
            "revision": 1,
            "deadline_at": 1700000000,
            "active": true,
            "finalizing": true,
            "pending_match_id": 77,
            "future_mode": "alpha",
            "provider_context": {"attempt": 3, "flags": ["a", "b"]},
            "state": {"label": "first", "session_id": 0}
        })
        .to_string();
        let created = repository
            .create_envelope(42, &requested)
            .expect("create metadata envelope");
        let created_value: Value =
            serde_json::from_str(created.raw_json()).expect("decode metadata envelope");
        assert_eq!(created_value["deadline_at"], json!(1700000000));
        assert_eq!(created_value["active"], json!(true));
        assert_eq!(created_value["finalizing"], json!(true));
        assert_eq!(created_value["pending_match_id"], json!(77));
        assert_eq!(created_value["future_mode"], json!("alpha"));
        assert_eq!(
            created_value["provider_context"],
            json!({"attempt": 3, "flags": ["a", "b"]})
        );

        let replacement = json!({
            "schema_version": 1,
            "guild_id": 42,
            "session_id": created.session_id,
            "revision": 2,
            "deadline_at": 1700000100,
            "active": true,
            "finalizing": false,
            "pending_match_id": 88,
            "future_mode": "beta",
            "state": {"label": "second", "session_id": created.session_id}
        })
        .to_string();
        let replaced = repository
            .replace_envelope_if_revision(42, created.session_id, 1, &replacement)
            .expect("replace metadata envelope");
        let replaced_value: Value =
            serde_json::from_str(replaced.raw_json()).expect("decode replaced envelope");
        assert_eq!(replaced_value["deadline_at"], json!(1700000100));
        assert_eq!(replaced_value["finalizing"], json!(false));
        assert_eq!(replaced_value["pending_match_id"], json!(88));
        assert_eq!(replaced_value["future_mode"], json!("beta"));
        assert_eq!(
            replaced_value["provider_context"],
            json!({"attempt": 3, "flags": ["a", "b"]})
        );
        assert_eq!(
            repository.load(42).expect("reload metadata"),
            Some(replaced)
        );

        let preserved = repository
            .replace_if_revision(42, created.session_id, 2, &payload("third"))
            .expect("replace bare state while preserving metadata");
        let preserved_value: Value =
            serde_json::from_str(preserved.raw_json()).expect("decode preserved envelope");
        assert_eq!(preserved_value["deadline_at"], json!(1700000100));
        assert_eq!(preserved_value["finalizing"], json!(false));
        assert_eq!(preserved_value["pending_match_id"], json!(88));
        assert_eq!(preserved_value["future_mode"], json!("beta"));
        assert_eq!(
            preserved_value["provider_context"],
            json!({"attempt": 3, "flags": ["a", "b"]})
        );
        assert_eq!(preserved_value["state"]["label"], json!("third"));
    }

    #[test]
    fn guild_rows_are_isolated_and_duplicate_is_explicit() {
        let fixture = migrated_fixture();
        let repository = DraftStateRepository::new(fixture.path());
        let first = repository
            .create(42, &payload("first"))
            .expect("first create");
        let second = repository
            .create(43, &payload("second"))
            .expect("second create");
        assert_eq!(first.session_id, 1);
        assert_eq!(second.session_id, 2);
        assert!(matches!(
            repository.create(42, &payload("duplicate")),
            Err(DraftStateRepositoryError::Duplicate { guild_id: 42 })
        ));
        assert_eq!(repository.load(43).expect("load second"), Some(second));
        assert_eq!(repository.load(42).expect("load first"), Some(first));
    }

    #[test]
    fn deleted_sessions_are_not_reused_and_identity_delete_is_scoped() {
        let fixture = migrated_fixture();
        let repository = DraftStateRepository::new(fixture.path());
        let first = repository
            .create(42, &payload("first"))
            .expect("first create");
        let other = repository
            .create(43, &payload("other"))
            .expect("other create");
        assert!(
            repository
                .delete_if_revision(42, first.session_id, first.revision)
                .expect("delete first")
        );
        let replacement = repository
            .create(42, &payload("replacement"))
            .expect("replacement");
        assert_eq!(replacement.session_id, other.session_id + 1);
        assert!(matches!(
            repository.replace_if_revision(
                42,
                first.session_id,
                first.revision,
                &payload("stale session")
            ),
            Err(DraftStateRepositoryError::StaleSession {
                guild_id: 42,
                expected: 1,
                actual: Some(3)
            })
        ));
        assert!(matches!(
            repository.delete_if_revision(42, first.session_id, first.revision),
            Err(DraftStateRepositoryError::StaleSession {
                guild_id: 42,
                expected: 1,
                actual: Some(3)
            })
        ));
        assert_eq!(
            repository.load(42).expect("replacement survives"),
            Some(replacement.clone())
        );
        assert_eq!(repository.load(43).expect("other survives"), Some(other));
        let rows = repository.load_all().expect("load remaining");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.guild_id == 42));
        assert!(rows.iter().any(|row| row.guild_id == 43));
    }

    #[test]
    fn stale_cas_is_rejected_without_mutating_state() {
        let fixture = migrated_fixture();
        let repository = DraftStateRepository::new(fixture.path());
        let first = repository.create(42, &payload("first")).expect("create");
        let second = repository
            .replace_if_revision(42, first.session_id, first.revision, &payload("second"))
            .expect("replace");
        assert_eq!(second.revision, 2);
        assert!(matches!(
            repository
                .replace_if_revision(42, first.session_id, first.revision, &payload("stale"),),
            Err(DraftStateRepositoryError::StaleRevision {
                guild_id: 42,
                expected: 1,
                actual: Some(2)
            })
        ));
        assert!(matches!(
            repository.delete_if_revision(42, first.session_id, first.revision),
            Err(DraftStateRepositoryError::StaleRevision {
                guild_id: 42,
                expected: 1,
                actual: Some(2)
            })
        ));
        let loaded = repository
            .load(42)
            .expect("load current")
            .expect("current row");
        let envelope: Value = serde_json::from_str(loaded.raw_json()).expect("decode current");
        assert_eq!(envelope["state"]["label"], json!("second"));
    }

    #[test]
    fn create_rolls_back_sequence_when_state_insert_fails() {
        let fixture = migrated_fixture();
        Connection::open(fixture.path())
            .expect("open fixture")
            .execute_batch(
                "CREATE TRIGGER reject_draft_state_insert
                 BEFORE INSERT ON app_kv
                 WHEN NEW.key = 'draft:state'
                 BEGIN SELECT RAISE(ABORT, 'reject draft state'); END;",
            )
            .expect("install rejection trigger");
        let repository = DraftStateRepository::new(fixture.path());
        assert!(matches!(
            repository.create(42, &payload("rejected")),
            Err(DraftStateRepositoryError::Sqlite(_))
        ));
        Connection::open(fixture.path())
            .expect("reopen fixture")
            .execute("DROP TRIGGER reject_draft_state_insert", [])
            .expect("remove rejection trigger");
        let created = repository
            .create(42, &payload("accepted"))
            .expect("retry create");
        assert_eq!(created.session_id, 1);
    }

    #[test]
    fn concurrent_creates_serialize_session_allocation() {
        let fixture = migrated_fixture();
        let path = Arc::new(fixture.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(3));
        let handles = (0..2)
            .map(|offset| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let repository = DraftStateRepository::new(path.as_ref());
                    barrier.wait();
                    repository.create(100 + offset, &payload("concurrent"))
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let mut rows = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("join create")
                    .expect("concurrent create")
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| row.session_id);
        assert_eq!(
            rows.iter().map(|row| row.session_id).collect::<Vec<_>>(),
            [1, 2]
        );
        let sequence: String = Connection::open(fixture.path())
            .expect("open fixture")
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id = 0 AND key = ?1",
                [DRAFT_SESSION_SEQUENCE_KEY],
                |row| row.get(0),
            )
            .expect("read sequence");
        assert_eq!(sequence, "2");
    }

    #[test]
    fn malformed_global_sequence_is_a_conflict_safe_error() {
        let fixture = migrated_fixture();
        Connection::open(fixture.path())
            .expect("open fixture")
            .execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (0,?1,'not-a-number')",
                [DRAFT_SESSION_SEQUENCE_KEY],
            )
            .expect("insert malformed sequence");
        let repository = DraftStateRepository::new(fixture.path());
        assert!(matches!(
            repository.create(42, &payload("bad sequence")),
            Err(DraftStateRepositoryError::InvalidSessionSequence(_))
        ));
        assert!(repository.load_all().expect("no state row").is_empty());
    }

    #[test]
    fn app_kv_identity_is_not_confused_with_other_namespaces() {
        let fixture = migrated_fixture();
        Connection::open(fixture.path())
            .expect("open fixture")
            .execute(
                "INSERT INTO app_kv(guild_id,key,value) VALUES (42,'draft:other','keep')",
                [],
            )
            .expect("insert unrelated key");
        let repository = DraftStateRepository::new(fixture.path());
        let state = repository
            .create(42, &payload("state"))
            .expect("create state");
        repository
            .delete_if_revision(42, state.session_id, state.revision)
            .expect("delete state");
        let other: String = Connection::open(fixture.path())
            .expect("reopen fixture")
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=42 AND key='draft:other'",
                [],
                |row| row.get(0),
            )
            .expect("read unrelated key");
        assert_eq!(other, "keep");
    }
}
