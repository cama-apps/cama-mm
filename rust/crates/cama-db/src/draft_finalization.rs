//! Atomic pending-match creation for a fenced Immortal Draft.
//!
//! The Draft envelope and pending match live in separate legacy tables. This
//! repository bridges them under one `BEGIN IMMEDIATE` transaction so a
//! process cannot publish an orphan pending match or lose the pending-match
//! identity from its recovery envelope.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::draft_financial_execution::DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE;
use crate::draft_state::{DRAFT_STATE_ENVELOPE_VERSION, DRAFT_STATE_KEY, DraftStateRecord};
use crate::open_runtime_connection;

/// Current immutable Draft-finalization plan schema.
pub const DRAFT_FINALIZATION_PLAN_VERSION: u64 = 2;
/// Legacy delivery-only plan schema. It remains readable but cannot execute
/// durable financial setup.
pub const DRAFT_FINALIZATION_LEGACY_PLAN_VERSION: u64 = 1;
/// First recovery stage installed atomically with the pending-match link.
pub const DRAFT_FINALIZATION_INITIAL_STAGE: &str = "linked";
/// Terminal stage excluded by [`DraftFinalizationRepository::load_incomplete_jobs`].
pub const DRAFT_FINALIZATION_COMPLETE_STAGE: &str = "complete";
/// Stage reached after every Discord delivery has been reconciled and before
/// the terminal job/envelope CAS.  Keeping this distinct from financial setup
/// lets a restart resume publication without replaying ledger effects.
pub const DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE: &str = "published";

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
    pub job: DraftFinalizationJob,
    pub job_created: bool,
    pub draft: DraftStateRecord,
}

/// Forward-compatible durable progress record for one Draft completion.
///
/// JSON is intentionally returned raw. The repository owns only identity,
/// leasing, and CAS; application versions own the typed plan/progress fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftFinalizationJob {
    pub completion_key: String,
    pub guild_id: i64,
    pub session_id: u64,
    pub pending_match_id: i64,
    pub revision: u64,
    pub stage: String,
    pub plan_json: String,
    pub progress_json: String,
    pub lease_owner: Option<String>,
    pub lease_until: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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
    #[error("draft finalization plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("draft finalization progress patch is invalid: {0}")]
    InvalidProgress(String),
    #[error("draft finalization job {completion_key} does not exist")]
    JobNotFound { completion_key: String },
    #[error(
        "draft finalization job revision is stale for {completion_key}: expected {expected}, found {actual}"
    )]
    StaleJobRevision {
        completion_key: String,
        expected: u64,
        actual: u64,
    },
    #[error(
        "draft finalization job stage is stale for {completion_key}: expected {expected}, found {actual}"
    )]
    StaleJobStage {
        completion_key: String,
        expected: String,
        actual: String,
    },
    #[error("draft finalization job {completion_key} is leased by {owner} until {lease_until}")]
    LeaseHeld {
        completion_key: String,
        owner: String,
        lease_until: i64,
    },
    #[error(
        "draft finalization job {completion_key} lease is not held by {expected_owner}: current owner {actual_owner:?}, lease until {lease_until:?}"
    )]
    LeaseLost {
        completion_key: String,
        expected_owner: String,
        actual_owner: Option<String>,
        lease_until: Option<i64>,
    },
    #[error("draft finalization conflict for guild {guild_id}: {reason}")]
    Conflict { guild_id: i64, reason: String },
}

/// Path-backed atomic Draft-finalization repository.
#[derive(Clone, Debug)]
pub struct DraftFinalizationRepository {
    path: PathBuf,
}

#[derive(Clone, Copy)]
enum PlanRequest<'a> {
    Frozen(&'a str),
    FinancialSeed(&'a str),
}

impl DraftFinalizationRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Execute or idempotently recover the frozen schema-v2 financial setup
    /// while the caller owns an unexpired finalization-job lease.
    pub fn run_leased_financial_setup(
        &self,
        completion_key: &str,
        expected_revision: u64,
        lease_owner: &str,
        now: i64,
    ) -> Result<
        crate::draft_financial_execution::DraftFinancialSetupRun,
        crate::draft_financial_execution::DraftFinancialExecutionError,
    > {
        crate::draft_financial_execution::run_leased_financial_setup(
            &self.path,
            completion_key,
            expected_revision,
            lease_owner,
            now,
        )
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
        plan_json: &str,
    ) -> Result<DraftFinalizationRecord, DraftFinalizationError> {
        self.link_pending_match_validated(
            guild_id,
            expected_session_id,
            expected_revision,
            pending_payload_json,
            plan_json,
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
        plan_json: &str,
        validate_draft_envelope: F,
    ) -> Result<DraftFinalizationRecord, DraftFinalizationError>
    where
        F: FnOnce(&str) -> Result<(), String>,
    {
        self.link_pending_match_internal(
            guild_id,
            expected_session_id,
            expected_revision,
            pending_payload_json,
            PlanRequest::Frozen(plan_json),
            validate_draft_envelope,
        )
    }

    /// Atomically expand a schema-v2 policy seed from the same writer snapshot
    /// that creates and links the pending match. The returned job contains the
    /// complete frozen plan, not the seed supplied by the caller.
    pub fn link_pending_match_with_financial_plan_validated<F>(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
        plan_seed_json: &str,
        validate_draft_envelope: F,
    ) -> Result<DraftFinalizationRecord, DraftFinalizationError>
    where
        F: FnOnce(&str) -> Result<(), String>,
    {
        self.link_pending_match_internal(
            guild_id,
            expected_session_id,
            expected_revision,
            pending_payload_json,
            PlanRequest::FinancialSeed(plan_seed_json),
            validate_draft_envelope,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn link_pending_match_internal<F>(
        &self,
        guild_id: i64,
        expected_session_id: u64,
        expected_revision: u64,
        pending_payload_json: &str,
        plan_request: PlanRequest<'_>,
        validate_draft_envelope: F,
    ) -> Result<DraftFinalizationRecord, DraftFinalizationError>
    where
        F: FnOnce(&str) -> Result<(), String>,
    {
        let completion_key = draft_completion_key(guild_id, expected_session_id);
        validate_pending_payload(pending_payload_json)?;
        match plan_request {
            PlanRequest::Frozen(plan_json) => {
                validate_plan_json(plan_json, guild_id, expected_session_id, &completion_key)?;
            }
            PlanRequest::FinancialSeed(plan_seed_json) => validate_plan_seed_json(
                plan_seed_json,
                guild_id,
                expected_session_id,
                &completion_key,
            )?,
        }
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
                    let job = require_matching_finalization_job_request(
                        &transaction,
                        &completion_key,
                        guild_id,
                        expected_session_id,
                        linked_id,
                        plan_request,
                    )?;
                    ensure_plan_pending_hash(&job.plan_json, &linked.payload)?;
                    ensure_plan_financial_identity(
                        &job.plan_json,
                        guild_id,
                        expected_session_id,
                        linked_id,
                    )?;
                    let draft = envelope.record(current_json)?;
                    transaction.commit()?;
                    return Ok(DraftFinalizationRecord {
                        completion_key,
                        pending_match_id: linked_id,
                        pending_payload_json: linked.payload,
                        pending_created: false,
                        job,
                        job_created: false,
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
                    // This row predates the immutable finalization plan. Its
                    // original timestamps and destinations cannot be
                    // reconstructed safely, so leave it untouched for an
                    // explicit manual recovery decision.
                    return Err(DraftFinalizationError::JobNotFound { completion_key });
                }
            }
        }

        ensure_revision(guild_id, expected_revision, envelope.revision)?;
        if keyed.is_some() {
            // A keyed pending row without the matching envelope link cannot
            // be produced by this transaction. Treat it as legacy/corrupt
            // state instead of manufacturing a recovery plan for it.
            return Err(DraftFinalizationError::JobNotFound { completion_key });
        }
        transaction.execute(
            "INSERT INTO pending_matches(guild_id,payload,completion_key,updated_at)
             VALUES (?1,?2,?3,CURRENT_TIMESTAMP)",
            params![guild_id, pending_payload_json, completion_key],
        )?;
        let pending_match_id = transaction.last_insert_rowid();
        let persisted_payload = pending_payload_json.to_owned();
        let frozen_plan_json = match plan_request {
            PlanRequest::Frozen(plan_json) => plan_json.to_owned(),
            PlanRequest::FinancialSeed(plan_seed_json) => {
                crate::draft_financial_setup::freeze_finalization_plan_json(
                    &transaction,
                    &completion_key,
                    guild_id,
                    expected_session_id,
                    pending_match_id,
                    pending_payload_json,
                    plan_seed_json,
                )
                .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?
            }
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
        let (job, job_created) = ensure_finalization_job(
            &transaction,
            &completion_key,
            guild_id,
            expected_session_id,
            pending_match_id,
            pending_payload_json,
            &frozen_plan_json,
        )?;
        let draft = envelope.record(updated_json)?;
        transaction.commit()?;
        Ok(DraftFinalizationRecord {
            completion_key,
            pending_match_id,
            pending_payload_json: persisted_payload,
            pending_created: true,
            job,
            job_created,
            draft,
        })
    }

    /// Load every non-terminal finalization job in deterministic creation
    /// order. Raw JSON fields are returned exactly as stored.
    pub fn load_incomplete_jobs(
        &self,
    ) -> Result<Vec<DraftFinalizationJob>, DraftFinalizationError> {
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT completion_key,guild_id,session_id,pending_match_id,revision,stage,
                    plan_json,progress_json,lease_owner,lease_until,last_error,
                    created_at,updated_at
               FROM draft_finalization_jobs
              WHERE stage<>?1
              ORDER BY created_at,completion_key",
        )?;
        statement
            .query_map([DRAFT_FINALIZATION_COMPLETE_STAGE], job_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Load one job by its deterministic completion identity.
    pub fn job(
        &self,
        completion_key: &str,
    ) -> Result<Option<DraftFinalizationJob>, DraftFinalizationError> {
        let connection = open_runtime_connection(&self.path)?;
        job_by_completion_key(&connection, completion_key).map_err(Into::into)
    }

    /// Claim or renew an incomplete job lease under a revision CAS.
    ///
    /// Another owner may take the job when the prior lease expires at or
    /// before `now`. Claiming increments the job revision so stale workers
    /// cannot advance it afterward.
    pub fn claim_job_lease(
        &self,
        completion_key: &str,
        expected_revision: u64,
        owner: &str,
        now: i64,
        lease_until: i64,
    ) -> Result<DraftFinalizationJob, DraftFinalizationError> {
        self.claim_job_lease_checked(
            completion_key,
            expected_revision,
            owner,
            now,
            lease_until,
            None,
        )
    }

    /// Claim a lease while asserting the complete guild/session/pending
    /// identity inside the same SQLite transaction as the revision CAS.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_job_lease_for_identity(
        &self,
        completion_key: &str,
        expected_revision: u64,
        owner: &str,
        now: i64,
        lease_until: i64,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<DraftFinalizationJob, DraftFinalizationError> {
        self.claim_job_lease_checked(
            completion_key,
            expected_revision,
            owner,
            now,
            lease_until,
            Some((guild_id, session_id, pending_match_id)),
        )
    }

    fn claim_job_lease_checked(
        &self,
        completion_key: &str,
        expected_revision: u64,
        owner: &str,
        now: i64,
        lease_until: i64,
        expected_identity: Option<(i64, u64, i64)>,
    ) -> Result<DraftFinalizationJob, DraftFinalizationError> {
        let owner = owner.trim();
        if owner.is_empty() {
            return Err(DraftFinalizationError::Conflict {
                guild_id: 0,
                reason: "draft finalization lease owner must not be empty".to_owned(),
            });
        }
        if lease_until <= now {
            return Err(DraftFinalizationError::Conflict {
                guild_id: 0,
                reason: "draft finalization lease must expire after now".to_owned(),
            });
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = require_job(&transaction, completion_key)?;
        if let Some((guild_id, session_id, pending_match_id)) = expected_identity
            && (current.guild_id != guild_id
                || current.session_id != session_id
                || current.pending_match_id != pending_match_id)
        {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "draft finalization lease identity does not match job".to_owned(),
            });
        }
        ensure_job_revision(&current, expected_revision)?;
        if current.stage == DRAFT_FINALIZATION_COMPLETE_STAGE {
            return Err(DraftFinalizationError::Conflict {
                guild_id: current.guild_id,
                reason: "completed draft finalization cannot be leased".to_owned(),
            });
        }
        if let (Some(current_owner), Some(current_until)) =
            (current.lease_owner.as_deref(), current.lease_until)
            && current_owner != owner
            && current_until > now
        {
            return Err(DraftFinalizationError::LeaseHeld {
                completion_key: completion_key.to_owned(),
                owner: current_owner.to_owned(),
                lease_until: current_until,
            });
        }
        let next_revision = next_job_revision(&current)?;
        let next_revision_sql = job_integer(current.guild_id, "revision", next_revision)?;
        let expected_revision_sql =
            job_integer(current.guild_id, "expected revision", expected_revision)?;
        let changed = transaction.execute(
            "UPDATE draft_finalization_jobs
                SET revision=?1,lease_owner=?2,lease_until=?3,
                    updated_at=CURRENT_TIMESTAMP
              WHERE completion_key=?4 AND revision=?5",
            params![
                next_revision_sql,
                owner,
                lease_until,
                completion_key,
                expected_revision_sql
            ],
        )?;
        if changed != 1 {
            return Err(DraftFinalizationError::StaleJobRevision {
                completion_key: completion_key.to_owned(),
                expected: expected_revision,
                actual: current.revision,
            });
        }
        let claimed = require_job(&transaction, completion_key)?;
        transaction.commit()?;
        Ok(claimed)
    }

    /// Advance a job under both revision and stage CAS while recursively
    /// merging an object patch into progress. Existing and future progress
    /// keys not named by the patch survive the update.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_job_stage(
        &self,
        completion_key: &str,
        expected_revision: u64,
        expected_stage: &str,
        next_stage: &str,
        progress_patch_json: &str,
        lease_owner: &str,
        now: i64,
    ) -> Result<DraftFinalizationJob, DraftFinalizationError> {
        self.advance_job_stage_checked(
            completion_key,
            expected_revision,
            expected_stage,
            next_stage,
            progress_patch_json,
            lease_owner,
            now,
            None,
        )
    }

    /// Advance a job while asserting the full completion identity inside the
    /// same SQLite stage/revision CAS.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_job_stage_for_identity(
        &self,
        completion_key: &str,
        expected_revision: u64,
        expected_stage: &str,
        next_stage: &str,
        progress_patch_json: &str,
        lease_owner: &str,
        now: i64,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<DraftFinalizationJob, DraftFinalizationError> {
        self.advance_job_stage_checked(
            completion_key,
            expected_revision,
            expected_stage,
            next_stage,
            progress_patch_json,
            lease_owner,
            now,
            Some((guild_id, session_id, pending_match_id)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_job_stage_checked(
        &self,
        completion_key: &str,
        expected_revision: u64,
        expected_stage: &str,
        next_stage: &str,
        progress_patch_json: &str,
        lease_owner: &str,
        now: i64,
        expected_identity: Option<(i64, u64, i64)>,
    ) -> Result<DraftFinalizationJob, DraftFinalizationError> {
        if expected_stage.is_empty() || next_stage.is_empty() {
            return Err(DraftFinalizationError::InvalidProgress(
                "job stages must not be empty".to_owned(),
            ));
        }
        if next_stage == DRAFT_FINALIZATION_COMPLETE_STAGE {
            return Err(DraftFinalizationError::InvalidProgress(
                "terminal completion must atomically delete the fenced Draft envelope".to_owned(),
            ));
        }
        let patch =
            parse_json_object(progress_patch_json, DraftFinalizationError::InvalidProgress)?;
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = require_job(&transaction, completion_key)?;
        if let Some((guild_id, session_id, pending_match_id)) = expected_identity
            && (current.guild_id != guild_id
                || current.session_id != session_id
                || current.pending_match_id != pending_match_id)
        {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "draft finalization stage identity does not match job".to_owned(),
            });
        }
        ensure_job_revision(&current, expected_revision)?;
        if current.stage != expected_stage {
            return Err(DraftFinalizationError::StaleJobStage {
                completion_key: completion_key.to_owned(),
                expected: expected_stage.to_owned(),
                actual: current.stage,
            });
        }
        if current.lease_owner.as_deref() != Some(lease_owner)
            || current
                .lease_until
                .is_none_or(|lease_until| lease_until <= now)
        {
            return Err(DraftFinalizationError::LeaseLost {
                completion_key: completion_key.to_owned(),
                expected_owner: lease_owner.to_owned(),
                actual_owner: current.lease_owner,
                lease_until: current.lease_until,
            });
        }
        let valid_transition = expected_stage == next_stage
            || (expected_stage == DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE
                && next_stage == DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE);
        if !valid_transition {
            return Err(DraftFinalizationError::InvalidProgress(format!(
                "invalid draft finalization stage transition {expected_stage} -> {next_stage}"
            )));
        }
        let mut progress = parse_json_object(
            &current.progress_json,
            DraftFinalizationError::InvalidProgress,
        )?;
        merge_json_preserving_unknown(&mut progress, patch);
        let progress_json = serde_json::to_string(&progress)
            .map_err(|error| DraftFinalizationError::InvalidProgress(error.to_string()))?;
        let next_revision = next_job_revision(&current)?;
        let next_revision_sql = job_integer(current.guild_id, "revision", next_revision)?;
        let expected_revision_sql =
            job_integer(current.guild_id, "expected revision", expected_revision)?;
        let changed = transaction.execute(
            "UPDATE draft_finalization_jobs
                SET revision=?1,stage=?2,progress_json=?3,
                    updated_at=CURRENT_TIMESTAMP
              WHERE completion_key=?4 AND revision=?5 AND stage=?6
                AND lease_owner=?7 AND lease_until>?8",
            params![
                next_revision_sql,
                next_stage,
                progress_json,
                completion_key,
                expected_revision_sql,
                expected_stage,
                lease_owner,
                now
            ],
        )?;
        if changed != 1 {
            return Err(DraftFinalizationError::Conflict {
                guild_id: current.guild_id,
                reason: "draft finalization job changed during stage CAS".to_owned(),
            });
        }
        let advanced = require_job(&transaction, completion_key)?;
        transaction.commit()?;
        Ok(advanced)
    }

    /// Atomically mark the exact leased linked job complete and delete its
    /// exact fenced Draft envelope. A failure in either write rolls both back,
    /// so startup can never observe a terminal job beside an active envelope.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_job_and_delete_draft(
        &self,
        completion_key: &str,
        expected_job_revision: u64,
        lease_owner: &str,
        now: i64,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
        expected_draft_revision: u64,
        expected_job_stage: &str,
    ) -> Result<DraftFinalizationJob, DraftFinalizationError> {
        if expected_job_stage != DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE {
            return Err(DraftFinalizationError::InvalidProgress(
                "terminal completion requires the published stage".to_owned(),
            ));
        }
        let mut connection = open_runtime_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_job = require_job(&transaction, completion_key)?;
        ensure_job_revision(&current_job, expected_job_revision)?;
        if current_job.guild_id != guild_id
            || current_job.session_id != session_id
            || current_job.pending_match_id != pending_match_id
        {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "terminal Draft finalization job identity does not match".to_owned(),
            });
        }
        if current_job.stage != expected_job_stage {
            return Err(DraftFinalizationError::StaleJobStage {
                completion_key: completion_key.to_owned(),
                expected: expected_job_stage.to_owned(),
                actual: current_job.stage,
            });
        }
        if current_job.lease_owner.as_deref() != Some(lease_owner)
            || current_job
                .lease_until
                .is_none_or(|lease_until| lease_until <= now)
        {
            return Err(DraftFinalizationError::LeaseLost {
                completion_key: completion_key.to_owned(),
                expected_owner: lease_owner.to_owned(),
                actual_owner: current_job.lease_owner,
                lease_until: current_job.lease_until,
            });
        }

        let current_json = transaction
            .query_row(
                "SELECT value FROM app_kv WHERE guild_id=?1 AND key=?2",
                params![guild_id, DRAFT_STATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(DraftFinalizationError::NotFound { guild_id })?;
        let envelope = parse_fenced_envelope(guild_id, &current_json)?;
        if envelope.session_id != session_id {
            return Err(DraftFinalizationError::StaleSession {
                guild_id,
                expected: session_id,
                actual: Some(envelope.session_id),
            });
        }
        ensure_revision(guild_id, expected_draft_revision, envelope.revision)?;
        if envelope.pending_match_id != Some(pending_match_id) {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "terminal Draft envelope names a different pending match".to_owned(),
            });
        }

        let next_revision = next_job_revision(&current_job)?;
        let next_revision_sql = job_integer(guild_id, "revision", next_revision)?;
        let expected_job_revision_sql =
            job_integer(guild_id, "expected revision", expected_job_revision)?;
        let changed = transaction.execute(
            "UPDATE draft_finalization_jobs
                SET revision=?1,stage=?2,lease_owner=NULL,lease_until=NULL,
                    updated_at=CURRENT_TIMESTAMP
              WHERE completion_key=?3 AND guild_id=?4 AND session_id=?5
                AND pending_match_id=?6 AND revision=?7 AND stage=?8
                AND lease_owner=?9 AND lease_until>?10",
            params![
                next_revision_sql,
                DRAFT_FINALIZATION_COMPLETE_STAGE,
                completion_key,
                guild_id,
                job_integer(guild_id, "session_id", session_id)?,
                pending_match_id,
                expected_job_revision_sql,
                expected_job_stage,
                lease_owner,
                now,
            ],
        )?;
        if changed != 1 {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "Draft finalization job changed during terminal CAS".to_owned(),
            });
        }
        let deleted = transaction.execute(
            "DELETE FROM app_kv WHERE guild_id=?1 AND key=?2 AND value=?3",
            params![guild_id, DRAFT_STATE_KEY, current_json],
        )?;
        if deleted != 1 {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "Draft envelope changed during terminal deletion".to_owned(),
            });
        }
        let completed = require_job(&transaction, completion_key)?;
        transaction.commit()?;
        Ok(completed)
    }

    /// Load the exact immutable plan already attached to a linked Draft.
    pub fn linked_plan_json(
        &self,
        guild_id: i64,
        session_id: u64,
        pending_match_id: i64,
    ) -> Result<String, DraftFinalizationError> {
        let completion_key = draft_completion_key(guild_id, session_id);
        let connection = open_runtime_connection(&self.path)?;
        let job = job_by_completion_key(&connection, &completion_key)?.ok_or_else(|| {
            DraftFinalizationError::JobNotFound {
                completion_key: completion_key.clone(),
            }
        })?;
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "linked Draft finalization job identity does not match".to_owned(),
            });
        }
        Ok(job.plan_json)
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
                "SELECT pending_match_id,payload,completion_key
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
        if pending.completion_key.as_deref() != Some(completion_key.as_str()) {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: format!(
                    "linked pending match {pending_match_id} is not owned by this completion"
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

fn validate_plan_json(
    raw: &str,
    guild_id: i64,
    session_id: u64,
    completion_key: &str,
) -> Result<(), DraftFinalizationError> {
    let value = parse_json_object(raw, DraftFinalizationError::InvalidPlan)?;
    let object = value.as_object().expect("object parser returns an object");
    let schema_version = object.get("schema_version").and_then(Value::as_u64);
    if !matches!(
        schema_version,
        Some(DRAFT_FINALIZATION_LEGACY_PLAN_VERSION) | Some(DRAFT_FINALIZATION_PLAN_VERSION)
    ) {
        return Err(DraftFinalizationError::InvalidPlan(format!(
            "unsupported schema_version {schema_version:?}"
        )));
    }
    if object.get("completion_key").and_then(Value::as_str) != Some(completion_key) {
        return Err(DraftFinalizationError::InvalidPlan(
            "completion_key does not match the Draft identity".to_owned(),
        ));
    }
    if object.get("guild_id").and_then(Value::as_i64) != Some(guild_id) {
        return Err(DraftFinalizationError::InvalidPlan(
            "guild_id does not match the Draft identity".to_owned(),
        ));
    }
    if object.get("session_id").and_then(Value::as_u64) != Some(session_id) {
        return Err(DraftFinalizationError::InvalidPlan(
            "session_id does not match the Draft identity".to_owned(),
        ));
    }
    let payload_hash = object
        .get("pending_payload_sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if payload_hash.len() != 64
        || !payload_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DraftFinalizationError::InvalidPlan(
            "pending_payload_sha256 must be a lowercase SHA-256 digest".to_owned(),
        ));
    }
    match schema_version {
        Some(DRAFT_FINALIZATION_LEGACY_PLAN_VERSION) => {
            if object.contains_key("financial_setup") {
                return Err(DraftFinalizationError::InvalidPlan(
                    "legacy plan must not contain financial_setup".to_owned(),
                ));
            }
        }
        Some(DRAFT_FINALIZATION_PLAN_VERSION) => {
            let financial = object.get("financial_setup").cloned().ok_or_else(|| {
                DraftFinalizationError::InvalidPlan(
                    "schema-v2 plan is missing financial_setup".to_owned(),
                )
            })?;
            let plan: crate::draft_financial_setup::DraftFinancialSetupPlan =
                serde_json::from_value(financial)
                    .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?;
            plan.validate()
                .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?;
        }
        _ => unreachable!("validated plan version"),
    }
    Ok(())
}

fn validate_plan_seed_json(
    raw: &str,
    guild_id: i64,
    session_id: u64,
    completion_key: &str,
) -> Result<(), DraftFinalizationError> {
    let value = parse_json_object(raw, DraftFinalizationError::InvalidPlan)?;
    let object = value.as_object().expect("object parser returns an object");
    if object.get("schema_version").and_then(Value::as_u64) != Some(DRAFT_FINALIZATION_PLAN_VERSION)
    {
        return Err(DraftFinalizationError::InvalidPlan(
            "financial plan seed must use schema version 2".to_owned(),
        ));
    }
    if object.get("completion_key").and_then(Value::as_str) != Some(completion_key)
        || object.get("guild_id").and_then(Value::as_i64) != Some(guild_id)
        || object.get("session_id").and_then(Value::as_u64) != Some(session_id)
    {
        return Err(DraftFinalizationError::InvalidPlan(
            "financial plan seed identity does not match the Draft".to_owned(),
        ));
    }
    let policy: crate::draft_financial_setup::DraftFinancialSetupPolicy =
        serde_json::from_value(object.get("financial_setup").cloned().ok_or_else(|| {
            DraftFinalizationError::InvalidPlan(
                "financial plan seed is missing its policy".to_owned(),
            )
        })?)
        .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?;
    policy
        .validate()
        .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?;
    let payload_hash = object
        .get("pending_payload_sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if payload_hash.len() != 64
        || !payload_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DraftFinalizationError::InvalidPlan(
            "pending_payload_sha256 must be a lowercase SHA-256 digest".to_owned(),
        ));
    }
    Ok(())
}

fn pending_payload_sha256(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_json_object(
    raw: &str,
    error: fn(String) -> DraftFinalizationError,
) -> Result<Value, DraftFinalizationError> {
    let value: Value = serde_json::from_str(raw).map_err(|source| error(source.to_string()))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(error("root value must be an object".to_owned()))
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

fn ensure_finalization_job(
    transaction: &Transaction<'_>,
    completion_key: &str,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    pending_payload_json: &str,
    plan_json: &str,
) -> Result<(DraftFinalizationJob, bool), DraftFinalizationError> {
    let session_id_sql = job_integer(guild_id, "session_id", session_id)?;
    validate_plan_json(plan_json, guild_id, session_id, completion_key)?;
    ensure_plan_pending_hash(plan_json, pending_payload_json)?;
    ensure_plan_financial_identity(plan_json, guild_id, session_id, pending_match_id)?;
    if let Some(job) = job_by_completion_key(transaction, completion_key)? {
        if job.guild_id != guild_id
            || job.session_id != session_id
            || job.pending_match_id != pending_match_id
        {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "completion key identifies a different finalization job".to_owned(),
            });
        }
        if job.plan_json != plan_json {
            return Err(DraftFinalizationError::Conflict {
                guild_id,
                reason: "completion key was retried with a different finalization plan".to_owned(),
            });
        }
        return Ok((job, false));
    }
    if transaction
        .query_row(
            "SELECT 1 FROM draft_finalization_jobs
              WHERE (guild_id=?1 AND session_id=?2) OR pending_match_id=?3
              LIMIT 1",
            params![guild_id, session_id_sql, pending_match_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(DraftFinalizationError::Conflict {
            guild_id,
            reason: "Draft session or pending match belongs to another finalization job".to_owned(),
        });
    }
    transaction.execute(
        "INSERT INTO draft_finalization_jobs(
             completion_key,guild_id,session_id,pending_match_id,revision,stage,
             plan_json,progress_json,created_at,updated_at
         ) VALUES (?1,?2,?3,?4,1,?5,?6,'{}',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)",
        params![
            completion_key,
            guild_id,
            session_id_sql,
            pending_match_id,
            DRAFT_FINALIZATION_INITIAL_STAGE,
            plan_json
        ],
    )?;
    Ok((require_job(transaction, completion_key)?, true))
}

fn require_matching_finalization_job_request(
    transaction: &Transaction<'_>,
    completion_key: &str,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    plan_request: PlanRequest<'_>,
) -> Result<DraftFinalizationJob, DraftFinalizationError> {
    let job = require_job(transaction, completion_key)?;
    if job.guild_id != guild_id
        || job.session_id != session_id
        || job.pending_match_id != pending_match_id
    {
        return Err(DraftFinalizationError::Conflict {
            guild_id,
            reason: "completion key identifies a different finalization job".to_owned(),
        });
    }
    let matches = match plan_request {
        PlanRequest::Frozen(plan_json) => job.plan_json == plan_json,
        PlanRequest::FinancialSeed(plan_seed_json) => {
            frozen_plan_matches_seed(&job.plan_json, plan_seed_json)?
        }
    };
    if !matches {
        return Err(DraftFinalizationError::Conflict {
            guild_id,
            reason: "completion key was retried with a different finalization plan".to_owned(),
        });
    }
    Ok(job)
}

fn frozen_plan_matches_seed(
    frozen_plan_json: &str,
    plan_seed_json: &str,
) -> Result<bool, DraftFinalizationError> {
    let mut frozen = parse_json_object(frozen_plan_json, DraftFinalizationError::InvalidPlan)?;
    let seed = parse_json_object(plan_seed_json, DraftFinalizationError::InvalidPlan)?;
    let frozen_object = frozen
        .as_object_mut()
        .expect("object parser returns object");
    let financial: crate::draft_financial_setup::DraftFinancialSetupPlan = serde_json::from_value(
        frozen_object
            .get("financial_setup")
            .cloned()
            .ok_or_else(|| {
                DraftFinalizationError::InvalidPlan(
                    "frozen plan is missing financial_setup".to_owned(),
                )
            })?,
    )
    .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?;
    frozen_object.insert(
        "financial_setup".to_owned(),
        serde_json::to_value(financial.policy)
            .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?,
    );
    Ok(frozen == seed)
}

fn ensure_plan_pending_hash(
    plan_json: &str,
    pending_payload_json: &str,
) -> Result<(), DraftFinalizationError> {
    let plan = parse_json_object(plan_json, DraftFinalizationError::InvalidPlan)?;
    let actual = plan
        .get("pending_payload_sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected = pending_payload_sha256(pending_payload_json);
    if actual == expected {
        Ok(())
    } else {
        Err(DraftFinalizationError::InvalidPlan(
            "pending_payload_sha256 does not match the exact initial pending payload".to_owned(),
        ))
    }
}

fn ensure_plan_financial_identity(
    plan_json: &str,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
) -> Result<(), DraftFinalizationError> {
    let plan = parse_json_object(plan_json, DraftFinalizationError::InvalidPlan)?;
    if plan.get("schema_version").and_then(Value::as_u64) == Some(DRAFT_FINALIZATION_PLAN_VERSION) {
        let financial: crate::draft_financial_setup::DraftFinancialSetupPlan =
            serde_json::from_value(plan.get("financial_setup").cloned().ok_or_else(|| {
                DraftFinalizationError::InvalidPlan(
                    "schema-v2 plan is missing financial_setup".to_owned(),
                )
            })?)
            .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?;
        financial
            .validate_for_identity(guild_id, session_id, pending_match_id)
            .map_err(|error| DraftFinalizationError::InvalidPlan(error.to_string()))?;
    }
    Ok(())
}

fn require_job(
    connection: &rusqlite::Connection,
    completion_key: &str,
) -> Result<DraftFinalizationJob, DraftFinalizationError> {
    job_by_completion_key(connection, completion_key)?.ok_or_else(|| {
        DraftFinalizationError::JobNotFound {
            completion_key: completion_key.to_owned(),
        }
    })
}

fn job_by_completion_key(
    connection: &rusqlite::Connection,
    completion_key: &str,
) -> Result<Option<DraftFinalizationJob>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT completion_key,guild_id,session_id,pending_match_id,revision,stage,
                    plan_json,progress_json,lease_owner,lease_until,last_error,
                    created_at,updated_at
               FROM draft_finalization_jobs
              WHERE completion_key=?1",
            [completion_key],
            job_from_row,
        )
        .optional()
}

fn job_from_row(row: &rusqlite::Row<'_>) -> Result<DraftFinalizationJob, rusqlite::Error> {
    let session_id = positive_job_integer(row, 2, "session_id")?;
    let revision = positive_job_integer(row, 4, "revision")?;
    Ok(DraftFinalizationJob {
        completion_key: row.get(0)?,
        guild_id: row.get(1)?,
        session_id,
        pending_match_id: row.get(3)?,
        revision,
        stage: row.get(5)?,
        plan_json: row.get(6)?,
        progress_json: row.get(7)?,
        lease_owner: row.get(8)?,
        lease_until: row.get(9)?,
        last_error: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn job_integer(guild_id: i64, field: &str, value: u64) -> Result<i64, DraftFinalizationError> {
    i64::try_from(value).map_err(|_| DraftFinalizationError::Conflict {
        guild_id,
        reason: format!("draft finalization {field} exceeds SQLite integer range"),
    })
}

fn positive_job_integer(
    row: &rusqlite::Row<'_>,
    index: usize,
    field: &str,
) -> Result<u64, rusqlite::Error> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("draft finalization {field} must be nonnegative"),
            )
            .into(),
        )
    })
}

fn ensure_job_revision(
    job: &DraftFinalizationJob,
    expected: u64,
) -> Result<(), DraftFinalizationError> {
    if job.revision == expected {
        Ok(())
    } else {
        Err(DraftFinalizationError::StaleJobRevision {
            completion_key: job.completion_key.clone(),
            expected,
            actual: job.revision,
        })
    }
}

fn next_job_revision(job: &DraftFinalizationJob) -> Result<u64, DraftFinalizationError> {
    job.revision
        .checked_add(1)
        .ok_or_else(|| DraftFinalizationError::Conflict {
            guild_id: job.guild_id,
            reason: "draft finalization job revision exhausted".to_owned(),
        })
}

fn merge_json_preserving_unknown(current: &mut Value, patch: Value) {
    match (current, patch) {
        (Value::Object(current), Value::Object(patch)) => {
            for (key, value) in patch {
                if let Some(existing) = current.get_mut(&key) {
                    merge_json_preserving_unknown(existing, value);
                } else {
                    current.insert(key, value);
                }
            }
        }
        (current, patch) => *current = patch,
    }
}

fn pending_by_completion_key(
    transaction: &Transaction<'_>,
    completion_key: &str,
) -> Result<Option<RawPendingMatch>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT pending_match_id,payload,completion_key
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
            "SELECT pending_match_id,payload,completion_key
             FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
            params![guild_id, pending_match_id],
            raw_pending_from_row,
        )
        .optional()
}

fn raw_pending_from_row(row: &rusqlite::Row<'_>) -> Result<RawPendingMatch, rusqlite::Error> {
    Ok(RawPendingMatch {
        pending_match_id: row.get(0)?,
        payload: row.get(1)?,
        completion_key: row.get(2)?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::draft_state::DraftStateRepository;

    fn fixture() -> NamedTempFile {
        let file = NamedTempFile::new().expect("create migrated fixture");
        crate::test_support::copy_migrated_database(file.path()).expect("copy migrated fixture");
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

    fn plan(guild_id: i64, session_id: u64, payload: &str) -> String {
        json!({
            "schema_version": DRAFT_FINALIZATION_LEGACY_PLAN_VERSION,
            "completion_key": draft_completion_key(guild_id, session_id),
            "guild_id": guild_id,
            "session_id": session_id,
            "pending_payload_sha256": pending_payload_sha256(payload),
            "future_plan": {"preserved": true}
        })
        .to_string()
    }

    fn financial_policy() -> crate::draft_financial_setup::DraftFinancialSetupPolicy {
        crate::draft_financial_setup::DraftFinancialSetupPolicy {
            schema_version: crate::draft_financial_setup::DRAFT_FINANCIAL_SETUP_PLAN_VERSION,
            game_date: "2026-08-14".to_owned(),
            betting_mode: "pool".to_owned(),
            seed_max_amount: 100,
            first_game_daily_amount: 0,
            auto_blind_enabled: true,
            normal_blind_threshold: 50,
            normal_blind_percentage_bits: format!("{:016x}", 0.1_f64.to_bits()),
            bomb_blind_percentage_bits: format!("{:016x}", 0.15_f64.to_bits()),
            bomb_ante: 10,
            max_debt: 500,
            spectator_enabled: true,
            spectator_total_count: 2,
            spectator_top_count: 1,
            spectator_base_percentage_bits: format!("{:016x}", 0.01_f64.to_bits()),
            spectator_top_percentage_bits: format!("{:016x}", 0.02_f64.to_bits()),
            extensions: BTreeMap::new(),
        }
    }

    fn plan_seed(guild_id: i64, session_id: u64, payload: &str) -> String {
        json!({
            "schema_version": DRAFT_FINALIZATION_PLAN_VERSION,
            "completion_key": draft_completion_key(guild_id, session_id),
            "guild_id": guild_id,
            "session_id": session_id,
            "pending_payload_sha256": pending_payload_sha256(payload),
            "started_at": 1_700_000_000,
            "shuffle_timestamp": 1_700_000_000,
            "bet_lock_until": 1_700_000_600,
            "is_bomb_pot": false,
            "financial_setup": financial_policy(),
            "future_plan": {"preserved": true}
        })
        .to_string()
    }

    fn insert_financial_players(connection: &Connection) {
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable unrelated legacy foreign-key mismatch for fixture setup");
        for discord_id in 1_i64..=13 {
            let balance = match discord_id {
                1 => 55,
                11 => 1_000,
                12 => 900,
                13 => 800,
                _ => 100,
            };
            connection
                .execute(
                    "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
                     VALUES (?1,42,?2,?3)",
                    params![discord_id, format!("player-{discord_id}"), balance],
                )
                .expect("insert financial player");
        }
        connection
            .execute(
                "INSERT INTO autobet_investments(
                     guild_id,investor_id,target_id,direction,percentage
                 ) VALUES (42,11,1,'long',10)",
                [],
            )
            .expect("insert frozen investment");
    }

    #[test]
    fn v2_link_freezes_financial_intents_in_atomic_writer_snapshot() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let connection = Connection::open(file.path()).expect("open v2 fixture");
        insert_financial_players(&connection);
        connection
            .execute(
                "INSERT INTO nonprofit_fund(
                     guild_id,total_collected,next_match_pot,
                     first_game_open_pool,first_game_lowskill_pool
                 ) VALUES (42,200,0,0,0)
                 ON CONFLICT(guild_id) DO UPDATE SET total_collected=200",
                [],
            )
            .expect("seed nonprofit fund");
        drop(connection);
        let payload = json!({
            "radiant_team_ids": [1,2,3,4,5],
            "dire_team_ids": [6,7,8,9,10],
            "shuffle_timestamp": 1_700_000_000_i64,
            "bet_lock_until": 1_700_000_600_i64,
            "betting_mode": "pool",
            "is_bomb_pot": false,
            "lobby_kind": "open"
        })
        .to_string();
        let seed = plan_seed(42, draft.session_id, &payload);

        let linked = repository
            .link_pending_match_with_financial_plan_validated(
                42,
                draft.session_id,
                draft.revision,
                &payload,
                &seed,
                |_| Ok(()),
            )
            .expect("link v2 financial plan");
        let frozen_raw = linked.job.plan_json.clone();
        let frozen: Value = serde_json::from_str(&frozen_raw).expect("decode frozen plan");
        assert_eq!(frozen["schema_version"], json!(2));
        assert_eq!(frozen["future_plan"], json!({"preserved": true}));
        assert_eq!(
            frozen["financial_setup"]["pending_match_id"],
            json!(linked.pending_match_id)
        );
        assert_eq!(
            frozen["financial_setup"]["investment_positions"][0]["starting_balance"],
            json!(1_000)
        );
        assert_eq!(
            frozen["financial_setup"]["wealth_snapshot"]
                .as_array()
                .expect("wealth array")
                .len(),
            13
        );
        let effects = frozen["financial_setup"]["effects"]
            .as_array()
            .expect("effects array");
        assert_eq!(effects[0]["effect_kind"], json!("seed"));
        assert_eq!(effects[1]["effect_kind"], json!("blind"));
        assert_eq!(effects[1]["intended"]["amount"], json!(6));
        assert!(effects.iter().any(|effect| {
            effect["effect_kind"] == json!("investment")
                && effect["intended"]["target_id"] == json!(1)
        }));
        assert!(
            effects
                .iter()
                .any(|effect| effect["effect_kind"] == json!("spectator"))
        );

        let connection = Connection::open(file.path()).expect("reopen v2 fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable unrelated legacy foreign-key mismatch for fixture mutation");
        connection
            .execute(
                "UPDATE players SET jopacoin_balance=1 WHERE guild_id=42",
                [],
            )
            .expect("mutate balances after link");
        connection
            .execute("DELETE FROM autobet_investments WHERE guild_id=42", [])
            .expect("mutate investments after link");
        connection
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (99,42,'late-player',999999)",
                [],
            )
            .expect("add late wealthy player");
        drop(connection);

        let retried = repository
            .link_pending_match_with_financial_plan_validated(
                42,
                draft.session_id,
                linked.draft.revision,
                &payload,
                &seed,
                |_| Ok(()),
            )
            .expect("retry v2 link from frozen plan");
        assert!(!retried.pending_created);
        assert!(!retried.job_created);
        assert_eq!(retried.job.plan_json, frozen_raw);
    }

    #[test]
    fn v2_planner_failure_rolls_back_and_policy_mismatch_fails_closed() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let connection = Connection::open(file.path()).expect("open v2 rollback fixture");
        insert_financial_players(&connection);
        drop(connection);
        let malformed_payload = json!({
            "dire_team_ids": [6,7,8,9,10],
            "shuffle_timestamp": 1_700_000_000_i64,
            "betting_mode": "pool",
            "is_bomb_pot": false,
            "lobby_kind": "open"
        })
        .to_string();
        let malformed_seed = plan_seed(42, draft.session_id, &malformed_payload);
        assert!(matches!(
            repository.link_pending_match_with_financial_plan_validated(
                42,
                draft.session_id,
                draft.revision,
                &malformed_payload,
                &malformed_seed,
                |_| Ok(()),
            ),
            Err(DraftFinalizationError::InvalidPlan(_))
        ));
        let connection = Connection::open(file.path()).expect("inspect v2 rollback fixture");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count rolled-back pending rows"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count rolled-back jobs"),
            0
        );
        drop(connection);
        let unchanged = drafts
            .load(42)
            .expect("reload rolled-back Draft")
            .expect("rolled-back Draft remains active");
        assert_eq!(unchanged.revision, draft.revision);
        assert_eq!(
            serde_json::from_str::<Value>(&unchanged.envelope_json)
                .expect("decode rolled-back envelope")["pending_match_id"],
            Value::Null
        );

        let payload = json!({
            "radiant_team_ids": [1,2,3,4,5],
            "dire_team_ids": [6,7,8,9,10],
            "shuffle_timestamp": 1_700_000_000_i64,
            "bet_lock_until": 1_700_000_600_i64,
            "betting_mode": "pool",
            "is_bomb_pot": false,
            "lobby_kind": "open"
        })
        .to_string();
        let seed = plan_seed(42, draft.session_id, &payload);
        let linked = repository
            .link_pending_match_with_financial_plan_validated(
                42,
                draft.session_id,
                draft.revision,
                &payload,
                &seed,
                |_| Ok(()),
            )
            .expect("link valid v2 plan after rollback");
        let original_plan = linked.job.plan_json.clone();
        let mut changed_seed: Value = serde_json::from_str(&seed).expect("decode v2 seed");
        changed_seed["financial_setup"]["seed_max_amount"] = json!(999);
        assert!(matches!(
            repository.link_pending_match_with_financial_plan_validated(
                42,
                draft.session_id,
                linked.draft.revision,
                &payload,
                &changed_seed.to_string(),
                |_| Ok(()),
            ),
            Err(DraftFinalizationError::Conflict { .. })
        ));
        assert_eq!(
            repository
                .job(&linked.completion_key)
                .expect("reload v2 job after mismatch")
                .expect("v2 job remains")
                .plan_json,
            original_plan
        );
    }

    #[test]
    fn migrated_atomic_link_preserves_unknown_json_and_retries_same_pending_match() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let payload = "{\"radiant_team_ids\":[1,2],\"future_pending\":{\"raw\":true}}";

        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                payload,
                &plan(42, draft.session_id, payload),
            )
            .expect("link pending match");
        assert!(linked.pending_created);
        assert_eq!(
            linked.completion_key,
            format!("draft:42:{}", draft.session_id)
        );
        assert_eq!(linked.pending_payload_json, payload);
        assert!(linked.job_created);
        assert_eq!(linked.job.completion_key, linked.completion_key);
        assert_eq!(linked.job.pending_match_id, linked.pending_match_id);
        assert_eq!(linked.job.stage, DRAFT_FINALIZATION_INITIAL_STAGE);
        assert_eq!(linked.job.revision, 1);
        assert_eq!(linked.job.progress_json, "{}");
        assert_eq!(linked.job.plan_json, plan(42, draft.session_id, payload));
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
            .link_pending_match(
                42,
                draft.session_id,
                reloaded.revision,
                payload,
                &plan(42, draft.session_id, payload),
            )
            .expect("retry linked pending match");
        assert!(!retried.pending_created);
        assert!(!retried.job_created);
        assert_eq!(retried.pending_match_id, linked.pending_match_id);
        assert_eq!(retried.pending_payload_json, payload);
        assert_eq!(retried.draft.revision, linked.draft.revision);
        assert!(matches!(
            repository.link_pending_match(
                42,
                draft.session_id,
                reloaded.revision,
                "{\"different_retry_payload\":true}",
                &plan(42, draft.session_id, "{\"different_retry_payload\":true}",),
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
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count finalization jobs"),
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
                        &plan(42, draft.session_id, "{\"same_concurrent_payload\":true}"),
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
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count finalization jobs"),
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
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision + 1,
                "{}",
                &plan(42, draft.session_id, "{}"),
            )
            .expect_err("reject stale revision");
        assert!(matches!(
            stale_revision,
            DraftFinalizationError::StaleRevision { .. }
        ));
        let stale_session = repository
            .link_pending_match(
                42,
                draft.session_id + 1,
                draft.revision,
                "{}",
                &plan(42, draft.session_id + 1, "{}"),
            )
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
                repository.link_pending_match(
                    42,
                    draft.session_id,
                    draft.revision,
                    "{}",
                    &plan(42, draft.session_id, "{}"),
                ),
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
    fn pre_job_linked_pending_match_fails_closed_without_plan_backfill() {
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
                legacy_payload,
                &plan(42, draft.session_id, legacy_payload),
            ),
            Err(DraftFinalizationError::JobNotFound { .. })
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
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count absent jobs"),
            0
        );
        connection
            .execute(
                "UPDATE pending_matches SET completion_key=?1 WHERE pending_match_id=?2",
                params![draft_completion_key(42, draft.session_id), pending_match_id],
            )
            .expect("install pre-job completion key");
        drop(connection);

        assert!(matches!(
            repository.link_pending_match(
                42,
                draft.session_id,
                linked_draft.revision,
                legacy_payload,
                &plan(42, draft.session_id, legacy_payload),
            ),
            Err(DraftFinalizationError::JobNotFound { .. })
        ));
        assert!(
            repository
                .job(&draft_completion_key(42, draft.session_id))
                .expect("load absent job")
                .is_none()
        );
        assert_eq!(
            drafts.load(42).expect("reload untouched legacy draft"),
            Some(linked_draft)
        );
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
            repository.link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{\"x\":1}",
                &plan(42, draft.session_id, "{\"x\":1}"),
            ),
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
                .link_pending_match(
                    42,
                    draft.session_id,
                    draft.revision,
                    "{\"x\":1}",
                    &plan(42, draft.session_id, "{\"x\":1}"),
                )
                .is_ok()
        );
    }

    #[test]
    fn plan_schema_identity_and_initial_payload_hash_are_validated_before_commit() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let payload = "{\"pending\":true}";
        let valid: Value =
            serde_json::from_str(&plan(42, draft.session_id, payload)).expect("decode plan");
        let malformed = [
            {
                let mut value = valid.clone();
                value["schema_version"] = json!(99);
                value
            },
            {
                let mut value = valid.clone();
                value["completion_key"] = json!("draft:42:999");
                value
            },
            {
                let mut value = valid.clone();
                value["guild_id"] = json!(99);
                value
            },
            {
                let mut value = valid.clone();
                value["session_id"] = json!(draft.session_id + 1);
                value
            },
            {
                let mut value = valid;
                value["pending_payload_sha256"] = json!("0".repeat(64));
                value
            },
        ];

        for malformed_plan in malformed {
            assert!(matches!(
                repository.link_pending_match(
                    42,
                    draft.session_id,
                    draft.revision,
                    payload,
                    &malformed_plan.to_string(),
                ),
                Err(DraftFinalizationError::InvalidPlan(_))
            ));
            let connection = Connection::open(file.path()).expect("open rejected-plan fixture");
            assert_eq!(
                connection
                    .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                        .get::<_, i64>(0))
                    .expect("count pending rows"),
                0
            );
            assert_eq!(
                connection
                    .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("count finalization jobs"),
                0
            );
            assert_eq!(
                drafts.load(42).expect("reload rejected draft"),
                Some(draft.clone())
            );
        }
    }

    #[test]
    fn same_key_retry_requires_the_exact_immutable_plan() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let payload = "{\"pending\":true}";
        let original_plan = plan(42, draft.session_id, payload);
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                payload,
                &original_plan,
            )
            .expect("link original plan");
        let mut changed_plan: Value =
            serde_json::from_str(&original_plan).expect("decode original plan");
        changed_plan["future_plan"]["new_value"] = json!(7);

        assert!(matches!(
            repository.link_pending_match(
                42,
                draft.session_id,
                linked.draft.revision,
                payload,
                &changed_plan.to_string(),
            ),
            Err(DraftFinalizationError::Conflict { .. })
        ));
        assert_eq!(
            repository
                .job(&linked.completion_key)
                .expect("load job")
                .expect("linked job")
                .plan_json,
            original_plan
        );
    }

    #[test]
    fn terminal_cas_and_exact_envelope_delete_commit_together() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{}",
                &plan(42, draft.session_id, "{}"),
            )
            .expect("link terminal job");
        let _claimed = repository
            .claim_job_lease(&linked.completion_key, 1, "worker-a", 100, 200)
            .expect("claim terminal job");
        Connection::open(file.path())
            .expect("open terminal publication fixture")
            .execute(
                "UPDATE draft_finalization_jobs SET stage='setup_complete' WHERE completion_key=?1",
                [&linked.completion_key],
            )
            .expect("simulate financial executor stage");
        let setup_job = repository
            .job(&linked.completion_key)
            .expect("reload terminal setup stage")
            .expect("terminal setup job");
        let published = repository
            .advance_job_stage(
                &linked.completion_key,
                setup_job.revision,
                DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                "{}",
                "worker-a",
                101,
            )
            .expect("advance terminal publication stage");

        let completed = repository
            .complete_job_and_delete_draft(
                &linked.completion_key,
                published.revision,
                "worker-a",
                102,
                42,
                draft.session_id,
                linked.pending_match_id,
                linked.draft.revision,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
            )
            .expect("commit terminal job and envelope delete");
        assert_eq!(completed.stage, DRAFT_FINALIZATION_COMPLETE_STAGE);
        assert_eq!(completed.revision, 4);
        assert_eq!(completed.lease_owner, None);
        assert_eq!(completed.lease_until, None);
        assert_eq!(drafts.load(42).expect("load deleted Draft"), None);
        assert!(
            repository
                .load_incomplete_jobs()
                .expect("load incomplete jobs")
                .is_empty()
        );
    }

    #[test]
    fn terminal_envelope_delete_failure_rolls_back_the_stage_cas() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{}",
                &plan(42, draft.session_id, "{}"),
            )
            .expect("link rollback job");
        let _claimed = repository
            .claim_job_lease(&linked.completion_key, 1, "worker-a", 100, 200)
            .expect("claim rollback job");
        Connection::open(file.path())
            .expect("open rollback publication fixture")
            .execute(
                "UPDATE draft_finalization_jobs SET stage='setup_complete' WHERE completion_key=?1",
                [&linked.completion_key],
            )
            .expect("simulate rollback financial executor stage");
        let setup_job = repository
            .job(&linked.completion_key)
            .expect("reload rollback setup stage")
            .expect("rollback setup job");
        let published = repository
            .advance_job_stage(
                &linked.completion_key,
                setup_job.revision,
                DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                "{}",
                "worker-a",
                101,
            )
            .expect("advance rollback publication stage");
        Connection::open(file.path())
            .expect("open terminal rollback fixture")
            .execute_batch(
                "CREATE TRIGGER fail_terminal_envelope_delete
                 BEFORE DELETE ON app_kv
                 WHEN OLD.guild_id=42 AND OLD.key='draft:state'
                 BEGIN
                     SELECT RAISE(ABORT, 'simulated terminal envelope delete crash');
                 END;",
            )
            .expect("install terminal rollback trigger");

        assert!(matches!(
            repository.complete_job_and_delete_draft(
                &linked.completion_key,
                published.revision,
                "worker-a",
                102,
                42,
                draft.session_id,
                linked.pending_match_id,
                linked.draft.revision,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
            ),
            Err(DraftFinalizationError::Sqlite(_))
        ));
        assert_eq!(
            repository
                .job(&linked.completion_key)
                .expect("reload rolled-back job")
                .expect("rolled-back job"),
            published
        );
        assert_eq!(
            drafts.load(42).expect("reload retained Draft"),
            Some(linked.draft)
        );
    }

    #[test]
    fn terminal_cas_rejects_stale_or_expired_lease_owner_without_deleting() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{}",
                &plan(42, draft.session_id, "{}"),
            )
            .expect("link stale-owner job");
        let _claimed = repository
            .claim_job_lease(&linked.completion_key, 1, "worker-a", 100, 200)
            .expect("claim stale-owner job");
        Connection::open(file.path())
            .expect("open stale-owner publication fixture")
            .execute(
                "UPDATE draft_finalization_jobs SET stage='setup_complete' WHERE completion_key=?1",
                [&linked.completion_key],
            )
            .expect("simulate stale-owner financial executor stage");
        let setup_job = repository
            .job(&linked.completion_key)
            .expect("reload stale-owner setup stage")
            .expect("stale-owner setup job");
        let published = repository
            .advance_job_stage(
                &linked.completion_key,
                setup_job.revision,
                DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                "{}",
                "worker-a",
                101,
            )
            .expect("advance stale-owner publication stage");

        for (owner, now) in [("worker-b", 101), ("worker-a", 200)] {
            assert!(matches!(
                repository.complete_job_and_delete_draft(
                    &linked.completion_key,
                    published.revision,
                    owner,
                    now,
                    42,
                    draft.session_id,
                    linked.pending_match_id,
                    linked.draft.revision,
                    DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                ),
                Err(DraftFinalizationError::LeaseLost { .. })
            ));
        }
        assert_eq!(
            repository
                .job(&linked.completion_key)
                .expect("reload uncompleted job")
                .expect("uncompleted job"),
            published
        );
        assert_eq!(
            drafts.load(42).expect("reload undeleted Draft"),
            Some(linked.draft)
        );
    }

    #[test]
    fn incomplete_load_and_stage_cas_preserve_unknown_progress() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let payload = "{}";
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                payload,
                &plan(42, draft.session_id, payload),
            )
            .expect("link job");
        Connection::open(file.path())
            .expect("open progress fixture")
            .execute(
                "UPDATE draft_finalization_jobs SET progress_json=?1 WHERE completion_key=?2",
                params![
                    r#"{"future":{"keep":1,"change":"old"},"untouched":[1,2]}"#,
                    linked.completion_key
                ],
            )
            .expect("install future progress");

        let incomplete = repository
            .load_incomplete_jobs()
            .expect("load incomplete jobs");
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].completion_key, linked.completion_key);
        assert!(matches!(
            repository.advance_job_stage(
                &linked.completion_key,
                1,
                DRAFT_FINALIZATION_INITIAL_STAGE,
                "setup_complete",
                "{}",
                "worker-a",
                100,
            ),
            Err(DraftFinalizationError::LeaseLost { .. })
        ));
        let claimed = repository
            .claim_job_lease(&linked.completion_key, 1, "worker-a", 100, 500)
            .expect("claim progress job");
        assert!(matches!(
            repository.advance_job_stage(
                &linked.completion_key,
                claimed.revision,
                DRAFT_FINALIZATION_INITIAL_STAGE,
                "setup_complete",
                "{}",
                "worker-a",
                101,
            ),
            Err(DraftFinalizationError::InvalidProgress(_))
        ));
        let progressed = repository
            .advance_job_stage(
                &linked.completion_key,
                claimed.revision,
                DRAFT_FINALIZATION_INITIAL_STAGE,
                DRAFT_FINALIZATION_INITIAL_STAGE,
                r#"{"future":{"change":"new","added":2},"known":true}"#,
                "worker-a",
                101,
            )
            .expect("advance linked progress");
        assert_eq!(progressed.revision, 3);
        assert_eq!(progressed.stage, DRAFT_FINALIZATION_INITIAL_STAGE);
        let progress: Value =
            serde_json::from_str(&progressed.progress_json).expect("decode merged progress");
        assert_eq!(progress["future"]["keep"], json!(1));
        assert_eq!(progress["future"]["change"], json!("new"));
        assert_eq!(progress["future"]["added"], json!(2));
        assert_eq!(progress["untouched"], json!([1, 2]));
        assert_eq!(progress["known"], json!(true));
        Connection::open(file.path())
            .expect("open executor-stage fixture")
            .execute(
                "UPDATE draft_finalization_jobs SET stage='setup_complete' WHERE completion_key=?1",
                [&linked.completion_key],
            )
            .expect("simulate financial executor stage");
        let setup_job = repository
            .job(&linked.completion_key)
            .expect("reload executor stage")
            .expect("executor stage job");
        let advanced = repository
            .advance_job_stage(
                &linked.completion_key,
                setup_job.revision,
                "setup_complete",
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                "{}",
                "worker-a",
                101,
            )
            .expect("advance publication stage");
        assert_eq!(advanced.revision, 4);
        assert_eq!(
            advanced.stage,
            DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE
        );
        assert!(matches!(
            repository.advance_job_stage(
                &linked.completion_key,
                claimed.revision,
                "setup_complete",
                "lobby_reconciled",
                "{}",
                "worker-a",
                102,
            ),
            Err(DraftFinalizationError::StaleJobRevision { .. })
        ));
        assert!(matches!(
            repository.advance_job_stage(
                &linked.completion_key,
                advanced.revision,
                DRAFT_FINALIZATION_INITIAL_STAGE,
                "lobby_reconciled",
                "{}",
                "worker-a",
                102,
            ),
            Err(DraftFinalizationError::StaleJobStage { .. })
        ));
        assert!(matches!(
            repository.advance_job_stage(
                &linked.completion_key,
                advanced.revision,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                DRAFT_FINALIZATION_COMPLETE_STAGE,
                "{}",
                "worker-a",
                102,
            ),
            Err(DraftFinalizationError::InvalidProgress(_))
        ));
        let completed = repository
            .complete_job_and_delete_draft(
                &linked.completion_key,
                advanced.revision,
                "worker-a",
                102,
                42,
                draft.session_id,
                linked.pending_match_id,
                linked.draft.revision,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
            )
            .expect("atomically complete progressed job");
        assert_eq!(completed.revision, 5);
        assert!(
            repository
                .load_incomplete_jobs()
                .expect("reload incomplete jobs")
                .is_empty()
        );
        assert!(
            repository
                .job(&linked.completion_key)
                .expect("load terminal job")
                .is_some()
        );
        assert_eq!(drafts.load(42).expect("load deleted Draft"), None);
    }

    #[test]
    fn lease_claim_is_revision_guarded_and_expires_at_the_deadline() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{}",
                &plan(42, draft.session_id, "{}"),
            )
            .expect("link leased job");
        let first = repository
            .claim_job_lease(&linked.completion_key, 1, "worker-a", 100, 200)
            .expect("claim first lease");
        assert_eq!(first.revision, 2);
        assert_eq!(first.lease_owner.as_deref(), Some("worker-a"));
        assert_eq!(first.lease_until, Some(200));
        assert!(matches!(
            repository.claim_job_lease(
                &linked.completion_key,
                first.revision,
                "worker-b",
                199,
                300
            ),
            Err(DraftFinalizationError::LeaseHeld { .. })
        ));
        assert!(matches!(
            repository.advance_job_stage(
                &linked.completion_key,
                first.revision,
                DRAFT_FINALIZATION_INITIAL_STAGE,
                DRAFT_FINALIZATION_PUBLICATION_COMPLETE_STAGE,
                "{}",
                "worker-a",
                200,
            ),
            Err(DraftFinalizationError::LeaseLost { .. })
        ));
        let expired = repository
            .claim_job_lease(&linked.completion_key, first.revision, "worker-b", 200, 300)
            .expect("claim expired lease");
        assert_eq!(expired.revision, 3);
        assert_eq!(expired.lease_owner.as_deref(), Some("worker-b"));
        assert!(matches!(
            repository.advance_job_stage(
                &linked.completion_key,
                expired.revision,
                DRAFT_FINALIZATION_INITIAL_STAGE,
                "setup_complete",
                "{}",
                "worker-a",
                201,
            ),
            Err(DraftFinalizationError::LeaseLost { .. })
        ));
        assert!(matches!(
            repository.claim_job_lease(
                &linked.completion_key,
                first.revision,
                "worker-a",
                201,
                400
            ),
            Err(DraftFinalizationError::StaleJobRevision { .. })
        ));
    }

    #[test]
    fn foreign_pending_identity_does_not_consume_lease_or_revision() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{}",
                &plan(42, draft.session_id, "{}"),
            )
            .expect("link identity fixture");
        assert!(matches!(
            repository.claim_job_lease_for_identity(
                &linked.completion_key,
                1,
                "worker-a",
                100,
                200,
                42,
                draft.session_id,
                linked.pending_match_id + 1,
            ),
            Err(DraftFinalizationError::Conflict { .. })
        ));
        let unchanged = repository
            .job(&linked.completion_key)
            .expect("reload identity fixture")
            .expect("identity job");
        assert_eq!(unchanged.revision, 1);
        assert_eq!(unchanged.lease_owner, None);
        assert_eq!(unchanged.lease_until, None);
    }

    #[test]
    fn concurrent_lease_claim_has_one_revision_cas_winner() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let repository = Arc::new(DraftFinalizationRepository::new(file.path()));
        let linked = repository
            .link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{}",
                &plan(42, draft.session_id, "{}"),
            )
            .expect("link concurrent lease job");
        let barrier = Arc::new(Barrier::new(3));
        let handles = ["worker-a", "worker-b"].map(|owner| {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            let completion_key = linked.completion_key.clone();
            thread::spawn(move || {
                barrier.wait();
                repository.claim_job_lease(&completion_key, 1, owner, 100, 200)
            })
        });
        barrier.wait();
        let results = handles.map(|handle| handle.join().expect("join lease claimant"));
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(DraftFinalizationError::StaleJobRevision { .. })
                ))
                .count(),
            1
        );
    }

    #[test]
    fn job_insert_failure_rolls_back_pending_envelope_and_retry_succeeds() {
        let file = fixture();
        let drafts = DraftStateRepository::new(file.path());
        let repository = DraftFinalizationRepository::new(file.path());
        let draft = fenced(&drafts, 42);
        let connection = Connection::open(file.path()).expect("open job rollback fixture");
        connection
            .execute_batch(
                "CREATE TRIGGER fail_draft_finalization_job
                 BEFORE INSERT ON draft_finalization_jobs
                 BEGIN
                   SELECT RAISE(ABORT, 'simulated job insert failure');
                 END;",
            )
            .expect("install job failure trigger");
        drop(connection);

        assert!(matches!(
            repository.link_pending_match(
                42,
                draft.session_id,
                draft.revision,
                "{}",
                &plan(42, draft.session_id, "{}"),
            ),
            Err(DraftFinalizationError::Sqlite(_))
        ));
        let connection = Connection::open(file.path()).expect("inspect rolled-back job");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pending_matches", [], |row| row
                    .get::<_, i64>(0))
                .expect("count pending rows"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count finalization jobs"),
            0
        );
        assert_eq!(
            drafts.load(42).expect("reload rolled-back draft"),
            Some(draft.clone())
        );
        connection
            .execute_batch("DROP TRIGGER fail_draft_finalization_job")
            .expect("drop job failure trigger");
        drop(connection);
        assert!(
            repository
                .link_pending_match(
                    42,
                    draft.session_id,
                    draft.revision,
                    "{}",
                    &plan(42, draft.session_id, "{}"),
                )
                .is_ok()
        );
    }
}
