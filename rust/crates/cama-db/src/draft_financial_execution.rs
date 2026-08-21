//! Atomic execution of a frozen Immortal Draft financial-setup plan.
//!
//! The setup job is deliberately narrower than finalization recovery: it
//! owns only nonprofit seed state and automatic bets. Every mutation, effect
//! receipt, pending-match summary, progress hash, and stage CAS commits under
//! one `BEGIN IMMEDIATE`, so `linked` can never describe a partially applied
//! setup produced by this executor.

use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::draft_finalization::{
    DRAFT_FINALIZATION_INITIAL_STAGE, DRAFT_FINALIZATION_PLAN_VERSION,
};
use crate::draft_financial_setup::{
    DraftFinancialEffectIntent, DraftFinancialEffectKind, DraftFinancialIntentStatus,
    DraftFinancialSetupPlan,
};
use crate::open_runtime_connection;

/// Durable stage reached only after every frozen financial effect commits.
pub const DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE: &str = "setup_complete";

/// Stable result returned by first execution and idempotent replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftFinancialSetupRun {
    pub completion_key: String,
    pub guild_id: i64,
    pub session_id: u64,
    pub pending_match_id: i64,
    pub job_revision: u64,
    pub plan_sha256: String,
    pub ledger_sha256: String,
    pub pending_sha256: String,
    pub summary_sha256: String,
    pub summary_json: String,
    pub replayed: bool,
}

#[derive(Debug, Error)]
pub enum DraftFinancialExecutionError {
    #[error("Draft financial setup SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Draft financial setup job {completion_key} does not exist")]
    JobNotFound { completion_key: String },
    #[error("Draft financial setup plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("Draft financial setup conflict for {completion_key}: {reason}")]
    Conflict {
        completion_key: String,
        reason: String,
    },
    #[error(
        "Draft financial setup lease for {completion_key} is not held by {expected_owner}: current owner {actual_owner:?}, lease until {lease_until:?}"
    )]
    LeaseLost {
        completion_key: String,
        expected_owner: String,
        actual_owner: Option<String>,
        lease_until: Option<i64>,
    },
    #[error("injected Draft financial setup failure after effect {ordinal}")]
    InjectedFailure { ordinal: u64 },
}

#[derive(Debug)]
struct JobRow {
    completion_key: String,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    revision: u64,
    stage: String,
    plan_json: String,
    progress_json: String,
    lease_owner: Option<String>,
    lease_until: Option<i64>,
}

/// What a linked plan must contain to be executable. Every field here is
/// mandatory, so `crate::draft_finalization`'s link boundary validates the same
/// shape: a plan this refuses could only pin its job at `linked` forever.
#[derive(Debug, Deserialize)]
struct RawFinalizationPlan {
    schema_version: u64,
    completion_key: String,
    guild_id: i64,
    session_id: u64,
    pending_payload_sha256: String,
    shuffle_timestamp: i64,
    is_bomb_pot: bool,
    financial_setup: RawFinancialSetup,
}

#[derive(Debug, Deserialize)]
struct RawFinancialSetup {
    pending_match_id: i64,
    effects: Vec<RawEffect>,
}

#[derive(Debug, Deserialize)]
struct RawEffect {
    effect_key: String,
    effect_kind: DraftFinancialEffectKind,
    ordinal: u64,
    intended_status: DraftFinancialIntentStatus,
    intended: Box<RawValue>,
}

struct ExecutionPlan {
    typed: DraftFinancialSetupPlan,
    raw_effects: Vec<RawEffect>,
    pending_payload_sha256: String,
    shuffle_timestamp: i64,
    is_bomb_pot: bool,
    plan_sha256: String,
}

#[derive(Clone, Debug)]
struct LedgerRow {
    effect_key: String,
    completion_key: String,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
    effect_kind: String,
    ordinal: u64,
    plan_sha256: String,
    intended_json: String,
    status: String,
    receipt_json: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FundState {
    total: i64,
    queued: i64,
    open: i64,
    low_skill: i64,
}

#[derive(Clone, Copy)]
struct SetupIdentity<'a> {
    completion_key: &'a str,
    guild_id: i64,
    session_id: u64,
    pending_match_id: i64,
}

pub(crate) fn run_leased_financial_setup(
    path: &Path,
    completion_key: &str,
    expected_revision: u64,
    lease_owner: &str,
    now: i64,
) -> Result<DraftFinancialSetupRun, DraftFinancialExecutionError> {
    run_leased_financial_setup_internal(
        path,
        completion_key,
        expected_revision,
        lease_owner,
        now,
        None,
    )
}

fn run_leased_financial_setup_internal(
    path: &Path,
    completion_key: &str,
    expected_revision: u64,
    lease_owner: &str,
    now: i64,
    fail_after_ordinal: Option<u64>,
) -> Result<DraftFinancialSetupRun, DraftFinancialExecutionError> {
    let lease_owner = lease_owner.trim();
    if lease_owner.is_empty() {
        return conflict(completion_key, "lease owner must not be empty");
    }
    let mut connection = open_runtime_connection(path)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let job = load_job(&transaction, completion_key)?.ok_or_else(|| {
        DraftFinancialExecutionError::JobNotFound {
            completion_key: completion_key.to_owned(),
        }
    })?;
    if job.completion_key != completion_key {
        return conflict(completion_key, "loaded job identity does not match");
    }
    validate_lease(&job, lease_owner, now)?;
    let replay_revision = job.revision == expected_revision.saturating_add(1);
    match job.stage.as_str() {
        DRAFT_FINALIZATION_INITIAL_STAGE if job.revision == expected_revision => {}
        DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE
            if job.revision == expected_revision || replay_revision => {}
        DRAFT_FINALIZATION_INITIAL_STAGE => {
            return conflict(
                completion_key,
                format!(
                    "job revision is stale: expected {expected_revision}, found {}",
                    job.revision
                ),
            );
        }
        DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE => {
            return conflict(
                completion_key,
                format!(
                    "completed job revision is not replay-compatible: expected {expected_revision}, found {}",
                    job.revision
                ),
            );
        }
        _ => {
            return conflict(
                completion_key,
                format!("job stage {} is not financial-setup runnable", job.stage),
            );
        }
    }

    let identity = SetupIdentity {
        completion_key,
        guild_id: job.guild_id,
        session_id: job.session_id,
        pending_match_id: job.pending_match_id,
    };
    let plan = parse_execution_plan(&job.plan_json, identity)?;
    let pending_payload = load_pending_payload(&transaction, identity)?;

    if job.stage == DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE {
        let result =
            validate_completed_replay(&transaction, &job, &plan, &pending_payload, identity)?;
        transaction.commit()?;
        return Ok(result);
    }

    if sha256_hex(pending_payload.as_bytes()) != plan.pending_payload_sha256 {
        return conflict(
            completion_key,
            "pending payload changed after the financial plan was frozen",
        );
    }
    let existing_rows = load_ledger_rows(&transaction, completion_key)?;
    if !existing_rows.is_empty() {
        return conflict(
            completion_key,
            "linked job already has financial-effect rows",
        );
    }
    let existing_bets = transaction.query_row(
        "SELECT COUNT(*) FROM bets
          WHERE guild_id=?1 AND pending_match_id=?2",
        params![identity.guild_id, identity.pending_match_id],
        |row| row.get::<_, i64>(0),
    )?;
    if existing_bets != 0 {
        return conflict(
            completion_key,
            "linked pending match already has bets before financial setup",
        );
    }

    for (typed, raw) in plan.typed.effects.iter().zip(&plan.raw_effects) {
        let receipt_json =
            apply_effect(&transaction, identity, &plan, typed, raw, &pending_payload)?;
        insert_ledger_row(
            &transaction,
            identity,
            &plan.plan_sha256,
            raw,
            &receipt_json,
        )?;
        if fail_after_ordinal == Some(raw.ordinal) {
            return Err(DraftFinancialExecutionError::InjectedFailure {
                ordinal: raw.ordinal,
            });
        }
    }

    let ledger = load_ledger_rows(&transaction, completion_key)?;
    validate_ledger(&transaction, identity, &plan, &ledger)?;
    let ledger_sha256 = ledger_sha256(&ledger);
    let summary = build_summary(&plan.typed, plan.is_bomb_pot)?;
    let summary_json = serde_json::to_string(&summary)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    let summary_sha256 = sha256_hex(summary_json.as_bytes());
    // Seed reservation fields are themselves one of this transaction's
    // frozen effects, so the final summary CAS must compare against the
    // transaction-local post-effect payload rather than the pre-effect hash.
    let payload_after_effects = load_pending_payload(&transaction, identity)?;
    let changed = transaction.execute(
        "UPDATE pending_matches
            SET payload=json_set(
                    CASE WHEN json_valid(payload) THEN payload ELSE '{}' END,
                    '$.blind_bets_result',json(?1)
                ),updated_at=CURRENT_TIMESTAMP
          WHERE guild_id=?2 AND pending_match_id=?3 AND completion_key=?4 AND payload=?5",
        params![
            summary_json,
            identity.guild_id,
            identity.pending_match_id,
            identity.completion_key,
            payload_after_effects
        ],
    )?;
    if changed != 1 {
        return conflict(completion_key, "pending summary CAS failed");
    }
    let persisted_pending_payload = load_pending_payload(&transaction, identity)?;
    let pending_sha256 = sha256_hex(persisted_pending_payload.as_bytes());

    let mut progress = parse_object(&job.progress_json, "job progress")?;
    if progress.contains_key("financial_setup") {
        return conflict(
            completion_key,
            "linked job already contains financial progress",
        );
    }
    progress.insert(
        "financial_setup".to_owned(),
        json!({
            "schema_version": 1,
            "plan_sha256": plan.plan_sha256,
            "ledger_sha256": ledger_sha256,
            "pending_sha256": pending_sha256,
            "summary_sha256": summary_sha256,
            "effect_count": ledger.len(),
            "summary": summary,
        }),
    );
    let progress_json = serde_json::to_string(&Value::Object(progress))
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    let next_revision =
        job.revision
            .checked_add(1)
            .ok_or_else(|| DraftFinancialExecutionError::Conflict {
                completion_key: completion_key.to_owned(),
                reason: "job revision exhausted".to_owned(),
            })?;
    let changed = transaction.execute(
        "UPDATE draft_finalization_jobs
            SET revision=?1,stage=?2,progress_json=?3,updated_at=CURRENT_TIMESTAMP
          WHERE completion_key=?4 AND revision=?5 AND stage=?6
            AND lease_owner=?7 AND lease_until>?8",
        params![
            sqlite_u64(next_revision, completion_key, "next revision")?,
            DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE,
            progress_json,
            completion_key,
            sqlite_u64(job.revision, completion_key, "revision")?,
            DRAFT_FINALIZATION_INITIAL_STAGE,
            lease_owner,
            now,
        ],
    )?;
    if changed != 1 {
        return conflict(completion_key, "financial setup stage CAS failed");
    }
    transaction.commit()?;
    Ok(DraftFinancialSetupRun {
        completion_key: completion_key.to_owned(),
        guild_id: identity.guild_id,
        session_id: identity.session_id,
        pending_match_id: identity.pending_match_id,
        job_revision: next_revision,
        plan_sha256: plan.plan_sha256,
        ledger_sha256,
        pending_sha256,
        summary_sha256,
        summary_json,
        replayed: false,
    })
}

fn load_job(
    transaction: &Transaction<'_>,
    completion_key: &str,
) -> Result<Option<JobRow>, DraftFinancialExecutionError> {
    transaction
        .query_row(
            "SELECT completion_key,guild_id,session_id,pending_match_id,revision,stage,
                    plan_json,progress_json,lease_owner,lease_until
               FROM draft_finalization_jobs WHERE completion_key=?1",
            [completion_key],
            |row| {
                let session = row.get::<_, i64>(2)?;
                let revision = row.get::<_, i64>(4)?;
                Ok(JobRow {
                    completion_key: row.get(0)?,
                    guild_id: row.get(1)?,
                    session_id: u64::try_from(session).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    pending_match_id: row.get(3)?,
                    revision: u64::try_from(revision).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    stage: row.get(5)?,
                    plan_json: row.get(6)?,
                    progress_json: row.get(7)?,
                    lease_owner: row.get(8)?,
                    lease_until: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn validate_lease(
    job: &JobRow,
    expected_owner: &str,
    now: i64,
) -> Result<(), DraftFinancialExecutionError> {
    if job.lease_owner.as_deref() == Some(expected_owner)
        && job.lease_until.is_some_and(|until| until > now)
    {
        Ok(())
    } else {
        Err(DraftFinancialExecutionError::LeaseLost {
            completion_key: job.completion_key.clone(),
            expected_owner: expected_owner.to_owned(),
            actual_owner: job.lease_owner.clone(),
            lease_until: job.lease_until,
        })
    }
}

fn parse_execution_plan(
    raw: &str,
    identity: SetupIdentity<'_>,
) -> Result<ExecutionPlan, DraftFinancialExecutionError> {
    let raw_plan: RawFinalizationPlan = serde_json::from_str(raw)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    if raw_plan.schema_version != DRAFT_FINALIZATION_PLAN_VERSION
        || raw_plan.completion_key != identity.completion_key
        || raw_plan.guild_id != identity.guild_id
        || raw_plan.session_id != identity.session_id
        || raw_plan.financial_setup.pending_match_id != identity.pending_match_id
    {
        return Err(DraftFinancialExecutionError::InvalidPlan(
            "schema-v2 plan identity does not match its job".to_owned(),
        ));
    }
    if raw_plan.pending_payload_sha256.len() != 64
        || !raw_plan
            .pending_payload_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DraftFinancialExecutionError::InvalidPlan(
            "pending payload hash is not lowercase SHA-256".to_owned(),
        ));
    }
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    let typed: DraftFinancialSetupPlan =
        serde_json::from_value(value.get("financial_setup").cloned().ok_or_else(|| {
            DraftFinancialExecutionError::InvalidPlan(
                "schema-v2 plan is missing financial_setup".to_owned(),
            )
        })?)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    typed
        .validate_for_identity(
            identity.guild_id,
            identity.session_id,
            identity.pending_match_id,
        )
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    if typed.effects.len() != raw_plan.financial_setup.effects.len() {
        return Err(DraftFinancialExecutionError::InvalidPlan(
            "typed/raw financial effect count differs".to_owned(),
        ));
    }
    for (typed_effect, raw_effect) in typed.effects.iter().zip(&raw_plan.financial_setup.effects) {
        let raw_intended: Value = serde_json::from_str(raw_effect.intended.get())
            .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
        if typed_effect.effect_key != raw_effect.effect_key
            || typed_effect.effect_kind != raw_effect.effect_kind
            || typed_effect.ordinal != raw_effect.ordinal
            || typed_effect.intended_status != raw_effect.intended_status
            || typed_effect.intended != raw_intended
        {
            return Err(DraftFinancialExecutionError::InvalidPlan(
                "typed/raw financial effect differs".to_owned(),
            ));
        }
    }
    Ok(ExecutionPlan {
        typed,
        raw_effects: raw_plan.financial_setup.effects,
        pending_payload_sha256: raw_plan.pending_payload_sha256,
        shuffle_timestamp: raw_plan.shuffle_timestamp,
        is_bomb_pot: raw_plan.is_bomb_pot,
        plan_sha256: sha256_hex(raw.as_bytes()),
    })
}

fn load_pending_payload(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
) -> Result<String, DraftFinancialExecutionError> {
    transaction
        .query_row(
            "SELECT payload FROM pending_matches
              WHERE guild_id=?1 AND pending_match_id=?2 AND completion_key=?3",
            params![
                identity.guild_id,
                identity.pending_match_id,
                identity.completion_key
            ],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| DraftFinancialExecutionError::Conflict {
            completion_key: identity.completion_key.to_owned(),
            reason: "linked pending match is missing or belongs to another completion".to_owned(),
        })
}

fn apply_effect(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
    plan: &ExecutionPlan,
    typed: &DraftFinancialEffectIntent,
    raw: &RawEffect,
    pending_payload: &str,
) -> Result<String, DraftFinancialExecutionError> {
    match typed.effect_kind {
        DraftFinancialEffectKind::Seed => apply_seed(transaction, identity, typed, raw),
        DraftFinancialEffectKind::Blind
        | DraftFinancialEffectKind::Investment
        | DraftFinancialEffectKind::Spectator => {
            apply_wager(transaction, identity, plan, typed, raw, pending_payload)
        }
    }
}

fn apply_seed(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
    typed: &DraftFinancialEffectIntent,
    raw: &RawEffect,
) -> Result<String, DraftFinancialExecutionError> {
    if typed.intended_status != DraftFinancialIntentStatus::Applied {
        return conflict(identity.completion_key, "seed intent must be applied");
    }
    let intended = raw_object(raw)?;
    let betting_mode = text(&intended, "betting_mode")?;
    let pool_mode = match betting_mode {
        "pool" => true,
        "house" => false,
        _ => return Err(invalid("seed betting_mode is invalid")),
    };
    let expected_before = fund_state_from_value(required(&intended, "expected_fund_before")?)?;
    let expected_exists = required(&intended, "expected_fund_before")?
        .get("exists")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("seed expected_fund_before.exists is invalid"))?;
    let current_before = read_fund(transaction, identity.guild_id)?;
    if current_before.is_some() != expected_exists
        || current_before.unwrap_or_default() != expected_before
    {
        return conflict(
            identity.completion_key,
            "nonprofit fund changed after financial planning",
        );
    }
    if current_before.is_none() {
        set_ledger_context(
            transaction,
            "dota_bet_seed",
            "pending_match",
            &identity.pending_match_id.to_string(),
            "initialize nonprofit reserve for Draft seed",
            json!({"completion_key": identity.completion_key}),
        )?;
        transaction.execute(
            "INSERT INTO nonprofit_fund(
                 guild_id,total_collected,next_match_pot,
                 first_game_open_pool,first_game_lowskill_pool,updated_at
             ) VALUES (?1,0,0,0,0,CURRENT_TIMESTAMP)",
            [identity.guild_id],
        )?;
        clear_ledger_context(transaction)?;
    }

    let funding_steps = required(&intended, "funding_steps")?
        .as_array()
        .ok_or_else(|| invalid("seed funding_steps must be an array"))?;
    if !pool_mode && !funding_steps.is_empty() {
        return Err(invalid("house-mode seed cannot fund first-game pools"));
    }
    for step in funding_steps {
        let step = step
            .as_object()
            .ok_or_else(|| invalid("seed funding step must be an object"))?;
        let game_date = text(step, "game_date")?;
        let daily_amount = integer(step, "daily_amount")?;
        let funded = step
            .get("funded")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid("seed funding step funded is invalid"))?;
        if transaction
            .query_row(
                "SELECT 1 FROM first_game_pool_funding_days
                  WHERE guild_id=?1 AND game_date=?2",
                params![identity.guild_id, game_date],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return conflict(
                identity.completion_key,
                "first-game funding day changed after financial planning",
            );
        }
        if funded {
            let pair_cost = daily_amount
                .checked_mul(2)
                .ok_or_else(|| invalid("seed daily pair cost overflowed"))?;
            set_ledger_context(
                transaction,
                "first_game_pool_funding",
                "game_date",
                game_date,
                "daily Open and Lowskill first-game pool funding",
                json!({"daily_amount": daily_amount, "pair_cost": pair_cost}),
            )?;
            let changed = transaction.execute(
                "UPDATE nonprofit_fund
                    SET total_collected=total_collected-?1,
                        first_game_open_pool=first_game_open_pool+?2,
                        first_game_lowskill_pool=first_game_lowskill_pool+?2,
                        updated_at=CURRENT_TIMESTAMP
                  WHERE guild_id=?3 AND total_collected>=?1",
                params![pair_cost, daily_amount, identity.guild_id],
            )?;
            clear_ledger_context(transaction)?;
            if changed != 1 {
                return conflict(
                    identity.completion_key,
                    "nonprofit funding source changed after financial planning",
                );
            }
        }
        transaction.execute(
            "INSERT INTO first_game_pool_funding_days(
                 guild_id,game_date,daily_amount,funded
             ) VALUES (?1,?2,?3,?4)",
            params![
                identity.guild_id,
                game_date,
                daily_amount,
                i64::from(funded)
            ],
        )?;
    }

    let game_date = text(&intended, "game_date")?;
    let lobby_kind = text(&intended, "lobby_kind")?;
    let daily_amount = integer(&intended, "first_game_daily_amount")?;
    let expected_date_claimed = intended
        .get("expected_date_claimed")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("seed expected_date_claimed is invalid"))?;
    let date_claimed = if pool_mode {
        transaction
            .query_row(
                "SELECT 1 FROM first_game_pool_claims
                  WHERE guild_id=?1 AND lobby_kind=?2 AND game_date=?3",
                params![identity.guild_id, lobby_kind, game_date],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
    } else {
        false
    };
    if pool_mode && date_claimed != expected_date_claimed {
        return conflict(
            identity.completion_key,
            "first-game claim changed after financial planning",
        );
    }
    if !pool_mode && expected_date_claimed {
        return Err(invalid("house-mode seed cannot claim a first-game pool"));
    }
    if transaction
        .query_row(
            "SELECT 1 FROM first_game_pool_claims
              WHERE guild_id=?1 AND pending_match_id=?2",
            params![identity.guild_id, identity.pending_match_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return conflict(
            identity.completion_key,
            "pending match already has a first-game claim",
        );
    }
    let first_game_reserved = integer(&intended, "first_game_reserved")?;
    let mut claim_created = false;
    if pool_mode && daily_amount > 0 && !expected_date_claimed {
        let pool_column = match lobby_kind {
            "open" => "first_game_open_pool",
            "lowskill" => "first_game_lowskill_pool",
            _ => return Err(invalid("seed lobby_kind is invalid")),
        };
        let select =
            format!("SELECT COALESCE({pool_column},0) FROM nonprofit_fund WHERE guild_id=?1");
        let pool =
            transaction.query_row(&select, [identity.guild_id], |row| row.get::<_, i64>(0))?;
        if pool != first_game_reserved {
            return conflict(
                identity.completion_key,
                "first-game pool amount changed after financial planning",
            );
        }
        if pool > 0 {
            let clear = format!(
                "UPDATE nonprofit_fund SET {pool_column}=0,updated_at=CURRENT_TIMESTAMP
                  WHERE guild_id=?1 AND {pool_column}=?2"
            );
            if transaction.execute(&clear, params![identity.guild_id, pool])? != 1 {
                return conflict(identity.completion_key, "first-game pool clear CAS failed");
            }
        }
        transaction.execute(
            "INSERT INTO first_game_pool_claims(
                 guild_id,lobby_kind,game_date,pending_match_id,amount
             ) VALUES (?1,?2,?3,?4,?5)",
            params![
                identity.guild_id,
                lobby_kind,
                game_date,
                identity.pending_match_id,
                first_game_reserved
            ],
        )?;
        claim_created = true;
    } else if first_game_reserved != 0 {
        return Err(invalid(
            "seed first_game_reserved is nonzero without a new claim",
        ));
    }

    let regular_reserved = integer(&intended, "regular_reserved")?;
    let seed_max_amount = integer(&intended, "seed_max_amount")?;
    let current = read_fund(transaction, identity.guild_id)?.unwrap_or_default();
    if current.queued > 0 {
        if regular_reserved != current.queued {
            return conflict(
                identity.completion_key,
                "queued seed amount changed after financial planning",
            );
        }
        if transaction.execute(
            "UPDATE nonprofit_fund SET next_match_pot=0,updated_at=CURRENT_TIMESTAMP
              WHERE guild_id=?1 AND next_match_pot=?2",
            params![identity.guild_id, current.queued],
        )? != 1
        {
            return conflict(identity.completion_key, "queued seed clear CAS failed");
        }
    } else {
        let expected_regular = seed_max_amount.max(0).min(current.total.max(0));
        if regular_reserved != expected_regular {
            return conflict(
                identity.completion_key,
                "reserve-backed seed amount changed after financial planning",
            );
        }
        if regular_reserved > 0 {
            set_ledger_context(
                transaction,
                "dota_bet_seed",
                "pending_match",
                &identity.pending_match_id.to_string(),
                "reserve-backed Dota betting seed",
                json!({"completion_key": identity.completion_key}),
            )?;
            if transaction.execute(
                "UPDATE nonprofit_fund
                    SET total_collected=total_collected-?1,updated_at=CURRENT_TIMESTAMP
                  WHERE guild_id=?2 AND total_collected=?3",
                params![regular_reserved, identity.guild_id, current.total],
            )? != 1
            {
                return conflict(identity.completion_key, "reserve seed debit CAS failed");
            }
            clear_ledger_context(transaction)?;
        }
    }

    let reserved = integer(&intended, "reserved")?;
    let radiant = integer(&intended, "radiant")?;
    let dire = integer(&intended, "dire")?;
    let bonus = integer(&intended, "bonus")?;
    if reserved
        != regular_reserved
            .checked_add(first_game_reserved)
            .ok_or_else(|| invalid("seed reservation overflowed"))?
        || reserved != radiant.saturating_add(dire).saturating_add(bonus)
    {
        return Err(invalid("seed split does not match the reserved amount"));
    }
    let existing_seed = transaction.query_row(
        "SELECT
             CAST(COALESCE(json_extract(payload,'$.bet_seed_reserved'),0) AS INTEGER),
             CAST(COALESCE(json_extract(payload,'$.bet_seed_radiant'),0) AS INTEGER),
             CAST(COALESCE(json_extract(payload,'$.bet_seed_dire'),0) AS INTEGER),
             CAST(COALESCE(json_extract(payload,'$.bet_seed_bonus'),0) AS INTEGER),
             CAST(COALESCE(json_extract(payload,'$.first_game_pool_reserved'),0) AS INTEGER)
           FROM pending_matches WHERE guild_id=?1 AND pending_match_id=?2",
        params![identity.guild_id, identity.pending_match_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    )?;
    if existing_seed != (0, 0, 0, 0, 0) {
        return conflict(
            identity.completion_key,
            "pending seed state changed after financial planning",
        );
    }
    if reserved > 0 {
        transaction.execute(
            "UPDATE pending_matches SET payload=json_set(
                 payload,
                 '$.bet_seed_reserved',?1,
                 '$.bet_seed_radiant',?2,
                 '$.bet_seed_dire',?3,
                 '$.bet_seed_bonus',?4,
                 '$.first_game_pool_reserved',?5
             ),updated_at=CURRENT_TIMESTAMP
             WHERE guild_id=?6 AND pending_match_id=?7",
            params![
                reserved,
                radiant,
                dire,
                bonus,
                first_game_reserved,
                identity.guild_id,
                identity.pending_match_id
            ],
        )?;
    }
    let after = read_fund(transaction, identity.guild_id)?.unwrap_or_default();
    json_string(json!({
        "effect_key": raw.effect_key,
        "status": "applied",
        "reserved": reserved,
        "radiant": radiant,
        "dire": dire,
        "bonus": bonus,
        "first_game_reserved": first_game_reserved,
        "claim_created": claim_created,
        "fund_after": fund_json(after),
    }))
}

fn apply_wager(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
    plan: &ExecutionPlan,
    typed: &DraftFinancialEffectIntent,
    raw: &RawEffect,
    pending_payload: &str,
) -> Result<String, DraftFinancialExecutionError> {
    let intended = raw_object(raw)?;
    let discord_id = integer(&intended, "discord_id")?;
    let before = integer(&intended, "expected_balance_before")?;
    let after = integer(&intended, "expected_balance_after")?;
    let current = transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
              WHERE guild_id=?1 AND discord_id=?2",
            params![identity.guild_id, discord_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if current != Some(before) {
        return conflict(
            identity.completion_key,
            format!("wallet {discord_id} changed after financial planning"),
        );
    }
    if typed.intended_status == DraftFinancialIntentStatus::Skipped {
        if after != before {
            return Err(invalid("skipped wager changes its frozen wallet"));
        }
        return json_string(json!({
            "effect_key": raw.effect_key,
            "status": "skipped",
            "reason": intended.get("reason").cloned().unwrap_or(Value::Null),
            "balance_before": before,
            "balance_after": after,
        }));
    }

    let amount = integer(&intended, "amount")?;
    if amount <= 0 || before.checked_sub(amount) != Some(after) {
        return Err(invalid(
            "applied wager amount/balance transition is invalid",
        ));
    }
    let team = text(&intended, "team")?;
    if !matches!(team, "radiant" | "dire") {
        return Err(invalid("wager team is invalid"));
    }
    let bet_time = integer(&intended, "bet_time")?;
    validate_pending_wager(pending_payload, discord_id, team, bet_time)?;
    let odds_bits = intended.get("odds_bits").and_then(Value::as_str);
    validate_frozen_odds(
        transaction,
        identity,
        plan.shuffle_timestamp,
        team,
        odds_bits,
    )?;
    set_ledger_context(
        transaction,
        "bet",
        "pending_match",
        &identity.pending_match_id.to_string(),
        "automatic pending match bet",
        json!({
            "effect_key": raw.effect_key,
            "amount": amount,
            "team": team,
        }),
    )?;
    let changed = transaction.execute(
        "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
          WHERE guild_id=?2 AND discord_id=?3
            AND COALESCE(jopacoin_balance,0)=?4",
        params![after, identity.guild_id, discord_id, before],
    )?;
    clear_ledger_context(transaction)?;
    if changed != 1 {
        return conflict(identity.completion_key, "wallet debit CAS failed");
    }
    let odds = odds_bits.map(parse_f64_bits).transpose()?;
    let attribution = if typed.effect_kind == DraftFinancialEffectKind::Investment {
        Some((
            integer(&intended, "target_id")?,
            text(&intended, "direction")?.to_owned(),
        ))
    } else {
        None
    };
    transaction.execute(
        "INSERT INTO bets(
             guild_id,match_id,discord_id,team_bet_on,amount,bet_time,
             leverage,is_blind,odds_at_placement,pending_match_id,
             investment_target_id,investment_direction
         ) VALUES (?1,NULL,?2,?3,?4,?5,1,1,?6,?7,?8,?9)",
        params![
            identity.guild_id,
            discord_id,
            team,
            amount,
            bet_time,
            odds,
            identity.pending_match_id,
            attribution.as_ref().map(|value| value.0),
            attribution.as_ref().map(|value| value.1.as_str()),
        ],
    )?;
    let bet_id = transaction.last_insert_rowid();
    if plan.is_bomb_pot
        && typed.effect_kind == DraftFinancialEffectKind::Blind
        && after < -plan.typed.policy.max_debt
    {
        return Err(invalid("bomb-pot wager exceeds the frozen debt limit"));
    }
    json_string(json!({
        "effect_key": raw.effect_key,
        "status": "applied",
        "bet_id": bet_id,
        "balance_before": before,
        "balance_after": after,
        "odds_bits": odds_bits,
    }))
}

fn validate_pending_wager(
    raw: &str,
    discord_id: i64,
    team: &str,
    bet_time: i64,
) -> Result<(), DraftFinancialExecutionError> {
    let pending: Value = serde_json::from_str(raw)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    let lock = pending
        .get("bet_lock_until")
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid("pending bet_lock_until is invalid"))?;
    if bet_time >= lock {
        return Err(invalid("frozen wager is outside the betting window"));
    }
    for (field, expected_team) in [("radiant_team_ids", "radiant"), ("dire_team_ids", "dire")] {
        if pending
            .get(field)
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|id| id.as_i64() == Some(discord_id)))
            && team != expected_team
        {
            return Err(invalid("participant wager targets the wrong team"));
        }
    }
    Ok(())
}

fn validate_frozen_odds(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
    shuffle_timestamp: i64,
    team: &str,
    planned_bits: Option<&str>,
) -> Result<(), DraftFinancialExecutionError> {
    let (radiant, dire) = transaction.query_row(
        "SELECT
             COALESCE(SUM(CASE WHEN team_bet_on='radiant' THEN amount*COALESCE(leverage,1) ELSE 0 END),0),
             COALESCE(SUM(CASE WHEN team_bet_on='dire' THEN amount*COALESCE(leverage,1) ELSE 0 END),0)
           FROM bets WHERE guild_id=?1 AND match_id IS NULL
             AND (
                 pending_match_id=?2
                 OR (pending_match_id IS NULL AND bet_time>=?3)
             )",
        params![
            identity.guild_id,
            identity.pending_match_id,
            shuffle_timestamp
        ],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let total = radiant.saturating_add(dire);
    let side = if team == "radiant" { radiant } else { dire };
    let expected =
        (total > 0 && side > 0).then(|| format!("{:016x}", (total as f64 / side as f64).to_bits()));
    if expected.as_deref() == planned_bits {
        Ok(())
    } else {
        conflict(
            identity.completion_key,
            "pending bet totals changed after financial planning",
        )
    }
}

fn insert_ledger_row(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
    plan_sha256: &str,
    effect: &RawEffect,
    receipt_json: &str,
) -> Result<(), DraftFinancialExecutionError> {
    transaction.execute(
        "INSERT INTO draft_financial_effects(
             effect_key,completion_key,guild_id,session_id,pending_match_id,
             effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            effect.effect_key,
            identity.completion_key,
            identity.guild_id,
            sqlite_u64(identity.session_id, identity.completion_key, "session")?,
            identity.pending_match_id,
            effect_kind_name(effect.effect_kind),
            sqlite_u64(effect.ordinal, identity.completion_key, "effect ordinal")?,
            plan_sha256,
            effect.intended.get(),
            intent_status_name(effect.intended_status),
            receipt_json,
        ],
    )?;
    Ok(())
}

fn load_ledger_rows(
    transaction: &Transaction<'_>,
    completion_key: &str,
) -> Result<Vec<LedgerRow>, DraftFinancialExecutionError> {
    let mut statement = transaction.prepare(
        "SELECT effect_key,completion_key,guild_id,session_id,pending_match_id,
                effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json
           FROM draft_financial_effects
          WHERE completion_key=?1 ORDER BY ordinal",
    )?;
    statement
        .query_map([completion_key], |row| {
            let session = row.get::<_, i64>(3)?;
            let ordinal = row.get::<_, i64>(6)?;
            Ok(LedgerRow {
                effect_key: row.get(0)?,
                completion_key: row.get(1)?,
                guild_id: row.get(2)?,
                session_id: u64::try_from(session).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
                pending_match_id: row.get(4)?,
                effect_kind: row.get(5)?,
                ordinal: u64::try_from(ordinal).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
                plan_sha256: row.get(7)?,
                intended_json: row.get(8)?,
                status: row.get(9)?,
                receipt_json: row.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn validate_ledger(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
    plan: &ExecutionPlan,
    rows: &[LedgerRow],
) -> Result<(), DraftFinancialExecutionError> {
    if rows.len() != plan.raw_effects.len() {
        return conflict(
            identity.completion_key,
            "financial effect ledger count does not match the plan",
        );
    }
    let mut expected_bet_ids = Vec::new();
    for ((typed, raw), row) in plan.typed.effects.iter().zip(&plan.raw_effects).zip(rows) {
        if row.effect_key != raw.effect_key
            || row.completion_key != identity.completion_key
            || row.guild_id != identity.guild_id
            || row.session_id != identity.session_id
            || row.pending_match_id != identity.pending_match_id
            || row.effect_kind != effect_kind_name(raw.effect_kind)
            || row.ordinal != raw.ordinal
            || row.plan_sha256 != plan.plan_sha256
            || row.intended_json != raw.intended.get()
            || row.status != intent_status_name(raw.intended_status)
        {
            return conflict(
                identity.completion_key,
                format!("financial ledger row {} was tampered", raw.effect_key),
            );
        }
        let receipt = parse_object(&row.receipt_json, "effect receipt")?;
        if receipt.get("effect_key").and_then(Value::as_str) != Some(raw.effect_key.as_str())
            || receipt.get("status").and_then(Value::as_str)
                != Some(intent_status_name(raw.intended_status))
        {
            return conflict(
                identity.completion_key,
                format!("financial receipt {} was tampered", raw.effect_key),
            );
        }
        if typed.effect_kind != DraftFinancialEffectKind::Seed
            && typed.intended_status == DraftFinancialIntentStatus::Applied
        {
            validate_applied_bet(transaction, identity, raw, &receipt)?;
            expected_bet_ids.push(positive_integer(&receipt, "bet_id")?);
        }
    }
    expected_bet_ids.sort_unstable();
    let mut statement = transaction.prepare(
        "SELECT bet_id FROM bets
          WHERE guild_id=?1 AND pending_match_id=?2
          ORDER BY bet_id",
    )?;
    let actual_bet_ids = statement
        .query_map(
            params![identity.guild_id, identity.pending_match_id],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if actual_bet_ids != expected_bet_ids {
        return conflict(
            identity.completion_key,
            "pending financial bet set does not match effect receipts",
        );
    }
    Ok(())
}

fn validate_applied_bet(
    transaction: &Transaction<'_>,
    identity: SetupIdentity<'_>,
    raw: &RawEffect,
    receipt: &Map<String, Value>,
) -> Result<(), DraftFinancialExecutionError> {
    let intended = raw_object(raw)?;
    let bet_id = positive_integer(receipt, "bet_id")?;
    let expected_odds = intended.get("odds_bits").and_then(Value::as_str);
    let row = transaction
        .query_row(
            "SELECT guild_id,discord_id,team_bet_on,amount,bet_time,leverage,is_blind,
                    odds_at_placement,pending_match_id,investment_target_id,
                    investment_direction,match_id
               FROM bets WHERE bet_id=?1",
            [bet_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return conflict(
            identity.completion_key,
            "financial bet receipt names a missing bet",
        );
    };
    let target = intended.get("target_id").and_then(Value::as_i64);
    let direction = intended.get("direction").and_then(Value::as_str);
    let odds = row.7.map(|value| format!("{:016x}", value.to_bits()));
    if row.0 != identity.guild_id
        || row.1 != integer(&intended, "discord_id")?
        || row.2 != text(&intended, "team")?
        || row.3 != integer(&intended, "amount")?
        || row.4 != integer(&intended, "bet_time")?
        || row.5 != 1
        || row.6 != 1
        || odds.as_deref() != expected_odds
        || row.8 != Some(identity.pending_match_id)
        || row.9 != target
        || row.10.as_deref() != direction
        || row.11.is_some()
    {
        return conflict(identity.completion_key, "financial bet row was tampered");
    }
    Ok(())
}

fn validate_completed_replay(
    transaction: &Transaction<'_>,
    job: &JobRow,
    plan: &ExecutionPlan,
    pending_payload: &str,
    identity: SetupIdentity<'_>,
) -> Result<DraftFinancialSetupRun, DraftFinancialExecutionError> {
    let ledger = load_ledger_rows(transaction, identity.completion_key)?;
    validate_ledger(transaction, identity, plan, &ledger)?;
    let ledger_sha256 = ledger_sha256(&ledger);
    let summary = build_summary(&plan.typed, plan.is_bomb_pot)?;
    let summary_json = serde_json::to_string(&summary)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    let summary_sha256 = sha256_hex(summary_json.as_bytes());
    let pending_sha256 = sha256_hex(pending_payload.as_bytes());
    let progress = parse_object(&job.progress_json, "job progress")?;
    let financial = progress
        .get("financial_setup")
        .and_then(Value::as_object)
        .ok_or_else(|| DraftFinancialExecutionError::Conflict {
            completion_key: identity.completion_key.to_owned(),
            reason: "setup-complete job has no financial progress".to_owned(),
        })?;
    if financial.get("plan_sha256").and_then(Value::as_str) != Some(&plan.plan_sha256)
        || financial.get("ledger_sha256").and_then(Value::as_str) != Some(&ledger_sha256)
        || financial.get("pending_sha256").and_then(Value::as_str) != Some(&pending_sha256)
        || financial.get("summary_sha256").and_then(Value::as_str) != Some(&summary_sha256)
        || financial.get("effect_count").and_then(Value::as_u64) != Some(ledger.len() as u64)
        || financial.get("summary") != Some(&summary)
    {
        return conflict(
            identity.completion_key,
            "setup-complete financial progress was tampered",
        );
    }
    let pending: Value = serde_json::from_str(pending_payload)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    if pending.get("blind_bets_result") != Some(&summary) {
        return conflict(
            identity.completion_key,
            "pending financial summary was tampered",
        );
    }
    Ok(DraftFinancialSetupRun {
        completion_key: identity.completion_key.to_owned(),
        guild_id: identity.guild_id,
        session_id: identity.session_id,
        pending_match_id: identity.pending_match_id,
        job_revision: job.revision,
        plan_sha256: plan.plan_sha256.clone(),
        ledger_sha256,
        pending_sha256,
        summary_sha256,
        summary_json,
        replayed: true,
    })
}

fn build_summary(
    plan: &DraftFinancialSetupPlan,
    is_bomb_pot: bool,
) -> Result<Value, DraftFinancialExecutionError> {
    let blind_percentage = if is_bomb_pot {
        parse_f64_bits(&plan.policy.bomb_blind_percentage_bits)?
    } else {
        parse_f64_bits(&plan.policy.normal_blind_percentage_bits)?
    };
    let mut blind_bets = Vec::new();
    let mut blind_skipped = Vec::new();
    let mut blind_radiant = 0_i64;
    let mut blind_dire = 0_i64;
    let mut investment_bets = Vec::new();
    let mut investment_skipped = Vec::new();
    let mut investment_radiant = 0_i64;
    let mut investment_dire = 0_i64;
    let mut spectator_bets = Vec::new();
    let mut spectator_skipped = Vec::new();
    let mut spectator_radiant = 0_i64;
    let mut spectator_dire = 0_i64;
    for effect in &plan.effects {
        let object = effect
            .intended
            .as_object()
            .ok_or_else(|| invalid("financial intended payload must be an object"))?;
        match effect.effect_kind {
            DraftFinancialEffectKind::Seed => {}
            DraftFinancialEffectKind::Blind => {
                if effect.intended_status == DraftFinancialIntentStatus::Applied {
                    let amount = integer(object, "amount")?;
                    let team = text(object, "team")?;
                    if team == "radiant" {
                        blind_radiant = blind_radiant.saturating_add(amount);
                    } else {
                        blind_dire = blind_dire.saturating_add(amount);
                    }
                    blind_bets.push(json!({
                        "discord_id": integer(object, "discord_id")?,
                        "team": team,
                        "amount": amount,
                    }));
                } else {
                    blind_skipped.push(json!({
                        "discord_id": integer(object, "discord_id")?,
                        "reason": object.get("reason").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            DraftFinancialEffectKind::Investment => {
                if effect.intended_status == DraftFinancialIntentStatus::Applied {
                    let amount = integer(object, "amount")?;
                    let team = text(object, "team")?;
                    if team == "radiant" {
                        investment_radiant = investment_radiant.saturating_add(amount);
                    } else {
                        investment_dire = investment_dire.saturating_add(amount);
                    }
                    investment_bets.push(json!({
                        "investor_id": integer(object, "discord_id")?,
                        "target_id": integer(object, "target_id")?,
                        "direction": text(object, "direction")?,
                        "percentage": integer(object, "percentage")?,
                        "team": team,
                        "amount": amount,
                    }));
                } else {
                    investment_skipped.push(json!({
                        "investor_id": integer(object, "discord_id")?,
                        "target_id": integer(object, "target_id")?,
                        "reason": object.get("reason").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            DraftFinancialEffectKind::Spectator => {
                if effect.intended_status == DraftFinancialIntentStatus::Applied {
                    let amount = integer(object, "amount")?;
                    let team = text(object, "team")?;
                    if team == "radiant" {
                        spectator_radiant = spectator_radiant.saturating_add(amount);
                    } else {
                        spectator_dire = spectator_dire.saturating_add(amount);
                    }
                    spectator_bets.push(json!({
                        "discord_id": integer(object, "discord_id")?,
                        "team": team,
                        "amount": amount,
                        "networth": integer(object, "networth")?,
                        "percentage": parse_f64_bits(text(object, "percentage_bits")?)?,
                    }));
                } else {
                    spectator_skipped.push(json!({
                        "discord_id": integer(object, "discord_id")?,
                        "reason": object.get("reason").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
        }
    }
    let top_percentage = parse_f64_bits(&plan.policy.spectator_top_percentage_bits)?;
    let top_count = if top_percentage > 0.0 {
        plan.policy
            .spectator_top_count
            .min(plan.policy.spectator_total_count)
    } else {
        0
    };
    Ok(json!({
        "created": blind_bets.len(),
        "total_radiant": blind_radiant,
        "total_dire": blind_dire,
        "percentage": blind_percentage,
        "bets": blind_bets,
        "skipped": blind_skipped,
        "is_bomb_pot": is_bomb_pot,
        "investment_bets": {
            "created": investment_bets.len(),
            "total_radiant": investment_radiant,
            "total_dire": investment_dire,
            "bets": investment_bets,
            "skipped": investment_skipped,
        },
        "spectator_bets": {
            "created": spectator_bets.len(),
            "total_radiant": spectator_radiant,
            "total_dire": spectator_dire,
            "percentage": parse_f64_bits(&plan.policy.spectator_base_percentage_bits)?,
            "top_percentage": top_percentage,
            "total_count": plan.policy.spectator_total_count,
            "top_count": top_count,
            "bets": spectator_bets,
            "skipped": spectator_skipped,
        }
    }))
}

fn read_fund(
    transaction: &Transaction<'_>,
    guild_id: i64,
) -> Result<Option<FundState>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT COALESCE(total_collected,0),COALESCE(next_match_pot,0),
                    COALESCE(first_game_open_pool,0),
                    COALESCE(first_game_lowskill_pool,0)
               FROM nonprofit_fund WHERE guild_id=?1",
            [guild_id],
            |row| {
                // Clamped exactly as the planner's fund_snapshot does: a
                // negative column judged raw here would never equal the
                // clamped value the plan was frozen against, so finalization
                // would report a conflict on every retry.
                Ok(FundState {
                    total: row.get::<_, i64>(0)?.max(0),
                    queued: row.get::<_, i64>(1)?.max(0),
                    open: row.get::<_, i64>(2)?.max(0),
                    low_skill: row.get::<_, i64>(3)?.max(0),
                })
            },
        )
        .optional()
}

fn fund_state_from_value(value: &Value) -> Result<FundState, DraftFinancialExecutionError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("seed fund snapshot must be an object"))?;
    Ok(FundState {
        total: integer(object, "total")?,
        queued: integer(object, "queued")?,
        open: integer(object, "open")?,
        low_skill: integer(object, "low_skill")?,
    })
}

fn fund_json(fund: FundState) -> Value {
    json!({
        "total": fund.total,
        "queued": fund.queued,
        "open": fund.open,
        "low_skill": fund.low_skill,
    })
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    source: &str,
    related_type: &str,
    related_id: &str,
    reason: &str,
    metadata: Value,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT OR REPLACE INTO economy_ledger_context(
             id,source,related_type,related_id,reason,metadata
         ) VALUES (1,?1,?2,?3,?4,?5)",
        params![
            source,
            related_type,
            related_id,
            reason,
            metadata.to_string()
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context WHERE id=1", [])?;
    Ok(())
}

fn parse_object(
    raw: &str,
    label: &str,
) -> Result<Map<String, Value>, DraftFinancialExecutionError> {
    serde_json::from_str::<Value>(raw)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(format!("{label} must be an object")))
}

fn raw_object(raw: &RawEffect) -> Result<Map<String, Value>, DraftFinancialExecutionError> {
    parse_object(raw.intended.get(), "effect intended payload")
}

fn required<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Value, DraftFinancialExecutionError> {
    object
        .get(field)
        .ok_or_else(|| invalid(format!("financial intended field {field} is missing")))
}

fn integer(object: &Map<String, Value>, field: &str) -> Result<i64, DraftFinancialExecutionError> {
    object.get(field).and_then(Value::as_i64).ok_or_else(|| {
        invalid(format!(
            "financial intended field {field} is not an integer"
        ))
    })
}

fn positive_integer(
    object: &Map<String, Value>,
    field: &str,
) -> Result<i64, DraftFinancialExecutionError> {
    integer(object, field).and_then(|value| {
        (value > 0)
            .then_some(value)
            .ok_or_else(|| invalid(format!("financial receipt field {field} is not positive")))
    })
}

fn text<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, DraftFinancialExecutionError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("financial intended field {field} is not text")))
}

fn effect_kind_name(kind: DraftFinancialEffectKind) -> &'static str {
    match kind {
        DraftFinancialEffectKind::Seed => "seed",
        DraftFinancialEffectKind::Blind => "blind",
        DraftFinancialEffectKind::Investment => "investment",
        DraftFinancialEffectKind::Spectator => "spectator",
    }
}

fn intent_status_name(status: DraftFinancialIntentStatus) -> &'static str {
    match status {
        DraftFinancialIntentStatus::Applied => "applied",
        DraftFinancialIntentStatus::Skipped => "skipped",
    }
}

fn parse_f64_bits(bits: &str) -> Result<f64, DraftFinancialExecutionError> {
    if bits.len() != 16
        || !bits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("f64 bits must be 16 lowercase hex digits"));
    }
    let value = u64::from_str_radix(bits, 16)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))?;
    let value = f64::from_bits(value);
    if value.is_finite() {
        Ok(value)
    } else {
        Err(invalid("f64 bits must encode a finite value"))
    }
}

fn json_string(value: Value) -> Result<String, DraftFinancialExecutionError> {
    serde_json::to_string(&value)
        .map_err(|error| DraftFinancialExecutionError::InvalidPlan(error.to_string()))
}

fn ledger_sha256(rows: &[LedgerRow]) -> String {
    let mut digest = Sha256::new();
    for row in rows {
        for field in [
            row.effect_key.as_str(),
            row.plan_sha256.as_str(),
            row.intended_json.as_str(),
            row.status.as_str(),
            row.receipt_json.as_str(),
        ] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sqlite_u64(
    value: u64,
    completion_key: &str,
    field: &str,
) -> Result<i64, DraftFinancialExecutionError> {
    i64::try_from(value).map_err(|_| DraftFinancialExecutionError::Conflict {
        completion_key: completion_key.to_owned(),
        reason: format!("{field} exceeds SQLite integer range"),
    })
}

fn invalid(message: impl Into<String>) -> DraftFinancialExecutionError {
    DraftFinancialExecutionError::InvalidPlan(message.into())
}

fn conflict<T>(
    completion_key: &str,
    reason: impl Into<String>,
) -> Result<T, DraftFinancialExecutionError> {
    Err(DraftFinancialExecutionError::Conflict {
        completion_key: completion_key.to_owned(),
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::thread;

    use rusqlite::{Connection, OptionalExtension, params};
    use serde_json::{Value, json};
    use tempfile::NamedTempFile;

    use super::*;
    use crate::draft_finalization::{
        DRAFT_FINALIZATION_PLAN_VERSION, DraftFinalizationRepository, draft_completion_key,
    };
    use crate::draft_financial_setup::{
        DRAFT_FINANCIAL_SETUP_PLAN_VERSION, DraftFinancialSetupPolicy,
    };
    use crate::draft_state::DraftStateRepository;
    use crate::test_support::{FastTestDatabase, fast_migrated_database};

    const GUILD_ID: i64 = 42;
    const SHUFFLE_AT: i64 = 1_700_000_000;
    const LEASE_NOW: i64 = 100;
    const LEASE_UNTIL: i64 = 500;

    enum FinancialTestDatabase {
        Fast(FastTestDatabase),
        Durable(NamedTempFile),
    }

    impl FinancialTestDatabase {
        fn path(&self) -> &Path {
            match self {
                Self::Fast(database) => database.path(),
                Self::Durable(database) => database.path(),
            }
        }
    }

    struct LinkedFixture {
        _file: FinancialTestDatabase,
        path: PathBuf,
        repository: DraftFinalizationRepository,
        completion_key: String,
        pending_match_id: i64,
        claimed_revision: u64,
    }

    fn bits(value: f64) -> String {
        format!("{:016x}", value.to_bits())
    }

    fn policy(with_effects: bool) -> DraftFinancialSetupPolicy {
        DraftFinancialSetupPolicy {
            schema_version: DRAFT_FINANCIAL_SETUP_PLAN_VERSION,
            game_date: "2026-08-14".to_owned(),
            betting_mode: "pool".to_owned(),
            seed_max_amount: if with_effects { 100 } else { 0 },
            first_game_daily_amount: 0,
            auto_blind_enabled: with_effects,
            normal_blind_threshold: 50,
            normal_blind_percentage_bits: bits(0.1),
            bomb_blind_percentage_bits: bits(0.5),
            bomb_ante: 10,
            max_debt: 20,
            spectator_enabled: with_effects,
            spectator_total_count: if with_effects { 2 } else { 0 },
            spectator_top_count: if with_effects { 1 } else { 0 },
            spectator_base_percentage_bits: bits(0.01),
            spectator_top_percentage_bits: bits(0.02),
            extensions: BTreeMap::new(),
        }
    }

    fn pending_payload() -> String {
        json!({
            "radiant_team_ids": [1],
            "dire_team_ids": [2],
            "shuffle_timestamp": SHUFFLE_AT,
            "bet_lock_until": SHUFFLE_AT + 600,
            "betting_mode": "pool",
            "is_bomb_pot": false,
            "lobby_kind": "open"
        })
        .to_string()
    }

    fn create_fenced_draft(path: &Path, guild_id: i64) -> crate::draft_state::DraftStateRecord {
        DraftStateRepository::new(path)
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
                    "future_envelope": {"preserve": true},
                    "state": {
                        "guild_id": guild_id,
                        "session_id": 0,
                        "phase": "complete"
                    }
                })
                .to_string(),
            )
            .expect("create fenced Draft fixture")
    }

    fn seed_json(session_id: u64, payload: &str, financial: DraftFinancialSetupPolicy) -> String {
        json!({
            "schema_version": DRAFT_FINALIZATION_PLAN_VERSION,
            "completion_key": draft_completion_key(GUILD_ID, session_id),
            "guild_id": GUILD_ID,
            "session_id": session_id,
            "pending_payload_sha256": sha256_hex(payload.as_bytes()),
            "started_at": SHUFFLE_AT,
            "shuffle_timestamp": SHUFFLE_AT,
            "bet_lock_until": SHUFFLE_AT + 600,
            "is_bomb_pot": false,
            "financial_setup": financial,
            "future_plan": {"preserve": [1, 2, 3]}
        })
        .to_string()
    }

    fn insert_players_and_financial_state(path: &Path, with_effects: bool) {
        let connection = Connection::open(path).expect("open financial execution fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable unrelated legacy foreign-key mismatch for fixture setup");
        for (discord_id, balance) in if with_effects {
            vec![(1_i64, 100_i64), (2, 100), (11, 1_000), (12, 900)]
        } else {
            vec![(1_i64, 0_i64), (2, 0)]
        } {
            connection
                .execute(
                    "INSERT INTO players(
                         discord_id,guild_id,discord_username,jopacoin_balance
                     ) VALUES (?1,?2,?3,?4)",
                    params![
                        discord_id,
                        GUILD_ID,
                        format!("player-{discord_id}"),
                        balance
                    ],
                )
                .expect("insert financial execution player");
        }
        if with_effects {
            connection
                .execute(
                    "INSERT INTO autobet_investments(
                         guild_id,investor_id,target_id,direction,percentage
                     ) VALUES (?1,11,1,'long',10)",
                    [GUILD_ID],
                )
                .expect("insert financial execution investment");
            connection
                .execute(
                    "INSERT INTO nonprofit_fund(
                         guild_id,total_collected,next_match_pot,
                         first_game_open_pool,first_game_lowskill_pool
                     ) VALUES (?1,200,0,0,0)",
                    [GUILD_ID],
                )
                .expect("insert financial execution fund");
        }
    }

    fn link_prepared_fixture(
        file: FinancialTestDatabase,
        payload: String,
        financial: DraftFinancialSetupPolicy,
    ) -> LinkedFixture {
        let draft = create_fenced_draft(file.path(), GUILD_ID);
        let repository = DraftFinalizationRepository::new(file.path());
        let linked = repository
            .link_pending_match_with_financial_plan_validated(
                GUILD_ID,
                draft.session_id,
                draft.revision,
                &payload,
                &seed_json(draft.session_id, &payload, financial),
                |_| Ok(()),
            )
            .expect("link frozen financial execution plan");
        let claimed = repository
            .claim_job_lease(
                &linked.completion_key,
                linked.job.revision,
                "worker-a",
                LEASE_NOW,
                LEASE_UNTIL,
            )
            .expect("claim financial execution job");
        LinkedFixture {
            path: file.path().to_path_buf(),
            _file: file,
            repository,
            completion_key: linked.completion_key,
            pending_match_id: linked.pending_match_id,
            claimed_revision: claimed.revision,
        }
    }

    fn linked_fixture(with_effects: bool) -> LinkedFixture {
        linked_fixture_with_database(
            FinancialTestDatabase::Fast(fast_migrated_database()),
            with_effects,
        )
    }

    fn durable_linked_fixture(with_effects: bool) -> LinkedFixture {
        let file = NamedTempFile::new().expect("create financial execution fixture");
        crate::test_support::copy_migrated_database(file.path())
            .expect("copy financial execution fixture");
        linked_fixture_with_database(FinancialTestDatabase::Durable(file), with_effects)
    }

    fn linked_fixture_with_database(
        file: FinancialTestDatabase,
        with_effects: bool,
    ) -> LinkedFixture {
        insert_players_and_financial_state(file.path(), with_effects);
        link_prepared_fixture(file, pending_payload(), policy(with_effects))
    }

    fn scalar_i64(path: &Path, sql: &str) -> i64 {
        Connection::open(path)
            .expect("open scalar fixture")
            .query_row(sql, [], |row| row.get(0))
            .expect("query scalar fixture")
    }

    fn job_stage(path: &Path, completion_key: &str) -> (u64, String) {
        let (revision, stage) = Connection::open(path)
            .expect("open job fixture")
            .query_row(
                "SELECT revision,stage FROM draft_finalization_jobs WHERE completion_key=?1",
                [completion_key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("load job stage");
        (u64::try_from(revision).expect("positive revision"), stage)
    }

    #[test]
    fn leased_setup_commits_all_effects_summary_progress_and_idempotent_replay() {
        let fixture = linked_fixture(true);
        let result = fixture
            .repository
            .run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("execute frozen financial setup");
        assert!(!result.replayed);
        assert_eq!(result.job_revision, fixture.claimed_revision + 1);
        assert_eq!(result.pending_match_id, fixture.pending_match_id);
        assert_eq!(
            job_stage(&fixture.path, &fixture.completion_key),
            (
                result.job_revision,
                DRAFT_FINANCIAL_SETUP_COMPLETE_STAGE.to_owned()
            )
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM draft_financial_effects"
            ),
            6
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM bets WHERE pending_match_id IS NOT NULL"
            ),
            5
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=42"
            ),
            100
        );
        let pending_summary: String = Connection::open(&fixture.path)
            .expect("open pending summary fixture")
            .query_row(
                "SELECT json_extract(payload,'$.blind_bets_result')
                   FROM pending_matches WHERE pending_match_id=?1",
                [fixture.pending_match_id],
                |row| row.get(0),
            )
            .expect("load pending financial summary");
        assert_eq!(
            serde_json::from_str::<Value>(&pending_summary).expect("decode pending summary"),
            serde_json::from_str::<Value>(&result.summary_json).expect("decode result summary")
        );

        let replay = fixture
            .repository
            .run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 2,
            )
            .expect("replay committed financial setup");
        assert!(replay.replayed);
        assert_eq!(replay.summary_json, result.summary_json);
        assert_eq!(replay.ledger_sha256, result.ledger_sha256);
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM draft_financial_effects"
            ),
            6
        );
    }

    #[test]
    fn legacy_unmatched_shuffle_bets_seed_frozen_and_executed_odds() {
        let file = FinancialTestDatabase::Fast(fast_migrated_database());
        insert_players_and_financial_state(file.path(), true);
        let connection = Connection::open(file.path()).expect("open legacy-odds fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable legacy fixture foreign keys");
        connection
            .execute(
                "INSERT INTO bets(
                     guild_id,match_id,discord_id,team_bet_on,amount,bet_time,
                     leverage,is_blind,pending_match_id
                 ) VALUES (42,NULL,1,'radiant',40,?1,1,0,NULL)",
                [SHUFFLE_AT],
            )
            .expect("insert legacy unmatched shuffle bet");
        let fixture = link_prepared_fixture(file, pending_payload(), policy(true));
        let raw_plan: String = Connection::open(&fixture.path)
            .expect("open frozen legacy-odds plan")
            .query_row(
                "SELECT plan_json FROM draft_finalization_jobs WHERE completion_key=?1",
                [&fixture.completion_key],
                |row| row.get(0),
            )
            .expect("load frozen legacy-odds plan");
        let plan: Value = serde_json::from_str(&raw_plan).expect("decode frozen legacy-odds plan");
        let first_blind = plan["financial_setup"]["effects"]
            .as_array()
            .expect("financial effects")
            .iter()
            .find(|effect| effect["effect_kind"] == "blind")
            .expect("first blind intent");
        assert_eq!(first_blind["intended"]["odds_bits"], bits(1.0));

        fixture
            .repository
            .run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("execute with legacy unmatched odds liquidity");
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM bets WHERE pending_match_id IS NULL"
            ),
            1
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM bets WHERE pending_match_id IS NOT NULL"
            ),
            5
        );
    }

    #[test]
    fn house_mode_reserves_bonus_without_funding_or_claiming_first_game_pools() {
        let file = FinancialTestDatabase::Fast(fast_migrated_database());
        insert_players_and_financial_state(file.path(), false);
        Connection::open(file.path())
            .expect("open house financial fixture")
            .execute(
                "INSERT INTO nonprofit_fund(
                     guild_id,total_collected,next_match_pot,
                     first_game_open_pool,first_game_lowskill_pool
                 ) VALUES (42,1000,0,0,0)",
                [],
            )
            .expect("insert house nonprofit reserve");
        let mut financial = policy(false);
        financial.betting_mode = "house".to_owned();
        financial.seed_max_amount = 100;
        financial.first_game_daily_amount = 50;
        let payload = json!({
            "radiant_team_ids": [1],
            "dire_team_ids": [2],
            "shuffle_timestamp": SHUFFLE_AT,
            "bet_lock_until": SHUFFLE_AT + 600,
            "betting_mode": "house",
            "is_bomb_pot": false,
            "lobby_kind": "open"
        })
        .to_string();
        let fixture = link_prepared_fixture(file, payload, financial);
        let raw_plan: String = Connection::open(&fixture.path)
            .expect("open house plan fixture")
            .query_row(
                "SELECT plan_json FROM draft_finalization_jobs WHERE completion_key=?1",
                [&fixture.completion_key],
                |row| row.get(0),
            )
            .expect("load house plan");
        let plan: Value = serde_json::from_str(&raw_plan).expect("decode house plan");
        let seed = &plan["financial_setup"]["effects"][0]["intended"];
        assert_eq!(seed["funding_steps"], json!([]));
        assert_eq!(seed["first_game_reserved"], 0);
        assert_eq!(seed["regular_reserved"], 100);
        assert_eq!(seed["bonus"], 100);

        fixture
            .repository
            .run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("execute house financial setup");
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=42"
            ),
            900
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM first_game_pool_funding_days"
            ),
            0
        );
        assert_eq!(
            scalar_i64(&fixture.path, "SELECT COUNT(*) FROM first_game_pool_claims"),
            0
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT CAST(json_extract(payload,'$.bet_seed_bonus') AS INTEGER)
                   FROM pending_matches WHERE pending_match_id=1"
            ),
            100
        );
    }

    #[test]
    fn frozen_policy_skips_are_receipted_without_wallet_or_bet_mutation() {
        let file = FinancialTestDatabase::Fast(fast_migrated_database());
        insert_players_and_financial_state(file.path(), false);
        let mut financial = policy(false);
        financial.auto_blind_enabled = true;
        let fixture = link_prepared_fixture(file, pending_payload(), financial);
        let result = fixture
            .repository
            .run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("execute frozen skipped effects");
        let summary: Value =
            serde_json::from_str(&result.summary_json).expect("decode skipped-effect summary");
        assert_eq!(summary["created"], 0);
        assert_eq!(
            summary["skipped"]
                .as_array()
                .expect("blind skip receipts")
                .len(),
            2
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM draft_financial_effects WHERE status='skipped'"
            ),
            2
        );
        assert_eq!(scalar_i64(&fixture.path, "SELECT COUNT(*) FROM bets"), 0);
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT SUM(jopacoin_balance) FROM players WHERE guild_id=42"
            ),
            0
        );
    }

    #[test]
    fn zero_seed_still_records_completion_and_replays_once() {
        let fixture = linked_fixture(false);
        let result = fixture
            .repository
            .run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("execute zero financial setup");
        assert!(!result.replayed);
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM draft_financial_effects WHERE effect_kind='seed'"
            ),
            1
        );
        assert_eq!(scalar_i64(&fixture.path, "SELECT COUNT(*) FROM bets"), 0);
        assert!(
            fixture
                .repository
                .run_leased_financial_setup(
                    &fixture.completion_key,
                    result.job_revision,
                    "worker-a",
                    LEASE_NOW + 2,
                )
                .expect("replay zero financial setup")
                .replayed
        );
    }

    #[test]
    fn injected_failure_rolls_back_every_financial_mutation() {
        let fixture = linked_fixture(true);
        let error = run_leased_financial_setup_internal(
            &fixture.path,
            &fixture.completion_key,
            fixture.claimed_revision,
            "worker-a",
            LEASE_NOW + 1,
            Some(5),
        )
        .expect_err("inject failure after every frozen effect");
        assert!(matches!(
            error,
            DraftFinancialExecutionError::InjectedFailure { ordinal: 5 }
        ));
        assert_eq!(
            job_stage(&fixture.path, &fixture.completion_key),
            (
                fixture.claimed_revision,
                DRAFT_FINALIZATION_INITIAL_STAGE.to_owned()
            )
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM draft_financial_effects"
            ),
            0
        );
        assert_eq!(scalar_i64(&fixture.path, "SELECT COUNT(*) FROM bets"), 0);
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=42"
            ),
            200
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT jopacoin_balance FROM players WHERE guild_id=42 AND discord_id=1"
            ),
            100
        );
    }

    #[test]
    fn wallet_and_fund_drift_fail_closed_without_partial_receipts() {
        for drift_wallet in [true, false] {
            let fixture = linked_fixture(true);
            let connection = Connection::open(&fixture.path).expect("open drift fixture");
            if drift_wallet {
                connection
                    .execute(
                        "UPDATE players SET jopacoin_balance=jopacoin_balance+1
                          WHERE guild_id=42 AND discord_id=1",
                        [],
                    )
                    .expect("drift planned wallet");
            } else {
                connection
                    .execute(
                        "UPDATE nonprofit_fund SET total_collected=total_collected+1
                          WHERE guild_id=42",
                        [],
                    )
                    .expect("drift planned fund");
            }
            let error = fixture
                .repository
                .run_leased_financial_setup(
                    &fixture.completion_key,
                    fixture.claimed_revision,
                    "worker-a",
                    LEASE_NOW + 1,
                )
                .expect_err("mutable financial drift must conflict");
            assert!(matches!(
                error,
                DraftFinancialExecutionError::Conflict { .. }
            ));
            assert_eq!(
                job_stage(&fixture.path, &fixture.completion_key).1,
                DRAFT_FINALIZATION_INITIAL_STAGE
            );
            assert_eq!(
                scalar_i64(
                    &fixture.path,
                    "SELECT COUNT(*) FROM draft_financial_effects"
                ),
                0
            );
            assert_eq!(scalar_i64(&fixture.path, "SELECT COUNT(*) FROM bets"), 0);
        }
    }

    #[test]
    fn executor_rejects_wrong_or_expired_lease_and_stale_revision() {
        let fixture = linked_fixture(false);
        for (revision, owner, now) in [
            (fixture.claimed_revision, "worker-b", LEASE_NOW + 1),
            (fixture.claimed_revision, "worker-a", LEASE_UNTIL),
        ] {
            assert!(matches!(
                fixture.repository.run_leased_financial_setup(
                    &fixture.completion_key,
                    revision,
                    owner,
                    now
                ),
                Err(DraftFinancialExecutionError::LeaseLost { .. })
            ));
        }
        assert!(matches!(
            fixture.repository.run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision + 1,
                "worker-a",
                LEASE_NOW + 1
            ),
            Err(DraftFinancialExecutionError::Conflict { .. })
        ));
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM draft_financial_effects"
            ),
            0
        );
    }

    #[test]
    fn concurrent_same_revision_execution_converges_on_one_commit_and_one_replay() {
        let fixture = durable_linked_fixture(true);
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = fixture.path.clone();
            let completion_key = fixture.completion_key.clone();
            let barrier = Arc::clone(&barrier);
            let revision = fixture.claimed_revision;
            handles.push(thread::spawn(move || {
                let repository = DraftFinalizationRepository::new(path);
                barrier.wait();
                repository.run_leased_financial_setup(
                    &completion_key,
                    revision,
                    "worker-a",
                    LEASE_NOW + 1,
                )
            }));
        }
        barrier.wait();
        let mut results = handles
            .into_iter()
            .map(|handle| handle.join().expect("join financial executor"))
            .collect::<Result<Vec<_>, _>>()
            .expect("both concurrent executions converge");
        results.sort_by_key(|result| result.replayed);
        assert!(!results[0].replayed);
        assert!(results[1].replayed);
        assert_eq!(results[0].summary_json, results[1].summary_json);
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM draft_financial_effects"
            ),
            6
        );
        assert_eq!(
            scalar_i64(
                &fixture.path,
                "SELECT COUNT(*) FROM bets WHERE pending_match_id IS NOT NULL"
            ),
            5
        );
    }

    #[test]
    fn linked_rows_and_completed_receipt_or_summary_tamper_fail_closed() {
        let fixture = linked_fixture(false);
        let connection = Connection::open(&fixture.path).expect("open linked tamper fixture");
        connection
            .execute(
                "INSERT INTO draft_financial_effects(
                     effect_key,completion_key,guild_id,session_id,pending_match_id,
                     effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json
                 )
                 SELECT ?1,completion_key,guild_id,session_id,pending_match_id,
                        'seed',99,lower(hex(randomblob(32))),'{}','applied','{}'
                   FROM draft_finalization_jobs WHERE completion_key=?2",
                params![
                    format!("{}:unexpected", fixture.completion_key),
                    fixture.completion_key
                ],
            )
            .expect("insert unexpected linked ledger row");
        assert!(matches!(
            fixture.repository.run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 1
            ),
            Err(DraftFinancialExecutionError::Conflict { .. })
        ));

        let completed = linked_fixture(false);
        let first = completed
            .repository
            .run_leased_financial_setup(
                &completed.completion_key,
                completed.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("complete fixture before receipt tamper");
        let connection = Connection::open(&completed.path).expect("open receipt tamper fixture");
        connection
            .execute(
                "UPDATE draft_financial_effects
                    SET receipt_json=json_set(receipt_json,'$.status','skipped')
                  WHERE completion_key=?1",
                [&completed.completion_key],
            )
            .expect("tamper financial receipt");
        assert!(matches!(
            completed.repository.run_leased_financial_setup(
                &completed.completion_key,
                first.job_revision,
                "worker-a",
                LEASE_NOW + 2
            ),
            Err(DraftFinancialExecutionError::Conflict { .. })
        ));

        let summary = linked_fixture(false);
        let first = summary
            .repository
            .run_leased_financial_setup(
                &summary.completion_key,
                summary.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("complete fixture before summary tamper");
        Connection::open(&summary.path)
            .expect("open summary tamper fixture")
            .execute(
                "UPDATE pending_matches
                    SET payload=json_set(payload,'$.blind_bets_result.created',999)
                  WHERE pending_match_id=?1",
                [summary.pending_match_id],
            )
            .expect("tamper pending financial summary");
        assert!(matches!(
            summary.repository.run_leased_financial_setup(
                &summary.completion_key,
                first.job_revision,
                "worker-a",
                LEASE_NOW + 2
            ),
            Err(DraftFinancialExecutionError::Conflict { .. })
        ));

        let extra_bet = linked_fixture(false);
        let first = extra_bet
            .repository
            .run_leased_financial_setup(
                &extra_bet.completion_key,
                extra_bet.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("complete fixture before extra-bet tamper");
        let connection = Connection::open(&extra_bet.path).expect("open extra-bet tamper fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("disable extra-bet fixture foreign keys");
        connection
            .execute(
                "INSERT INTO bets(
                     guild_id,match_id,discord_id,team_bet_on,amount,bet_time,
                     leverage,is_blind,pending_match_id
                 ) VALUES (42,NULL,1,'radiant',1,?1,1,1,?2)",
                params![SHUFFLE_AT, extra_bet.pending_match_id],
            )
            .expect("insert unreceipted pending bet");
        assert!(matches!(
            extra_bet.repository.run_leased_financial_setup(
                &extra_bet.completion_key,
                first.job_revision,
                "worker-a",
                LEASE_NOW + 2
            ),
            Err(DraftFinancialExecutionError::Conflict { .. })
        ));
    }

    #[test]
    fn completed_progress_tamper_is_not_accepted_as_idempotent_replay() {
        let fixture = linked_fixture(false);
        let first = fixture
            .repository
            .run_leased_financial_setup(
                &fixture.completion_key,
                fixture.claimed_revision,
                "worker-a",
                LEASE_NOW + 1,
            )
            .expect("complete fixture before progress tamper");
        Connection::open(&fixture.path)
            .expect("open progress tamper fixture")
            .execute(
                "UPDATE draft_finalization_jobs
                    SET progress_json=json_set(
                        progress_json,'$.financial_setup.ledger_sha256',lower(hex(randomblob(32)))
                    ) WHERE completion_key=?1",
                [&fixture.completion_key],
            )
            .expect("tamper financial progress");
        assert!(matches!(
            fixture.repository.run_leased_financial_setup(
                &fixture.completion_key,
                first.job_revision,
                "worker-a",
                LEASE_NOW + 2
            ),
            Err(DraftFinancialExecutionError::Conflict { .. })
        ));
    }

    #[test]
    fn missing_job_is_distinct_from_financial_conflict() {
        let file = FinancialTestDatabase::Fast(fast_migrated_database());
        assert!(matches!(
            run_leased_financial_setup(file.path(), "draft:42:999", 1, "worker", 100),
            Err(DraftFinancialExecutionError::JobNotFound { .. })
        ));
        assert_eq!(
            Connection::open(file.path())
                .expect("open missing-job fixture")
                .query_row(
                    "SELECT lease_owner FROM draft_finalization_jobs LIMIT 1",
                    [],
                    |row| row.get::<_, Option<String>>(0)
                )
                .optional()
                .expect("query empty job table"),
            None
        );
    }
}
