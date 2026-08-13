//! Existing-schema persistence for standalone Dig splash effects.
//!
//! The application layer chooses the authored mode and amount.  This module
//! owns the read-side victim projections and the write-side boundary: one
//! `BEGIN IMMEDIATE` transaction settles every selected victim, records the
//! hostile-loss/protection rows, updates the economy ledger context, and
//! appends the Dig audit actions.  It deliberately does not create or alter
//! schema.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cama_domain::dig_splash::HOSTILE_LOSS_MIN_BALANCE;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use thiserror::Error;

use crate::open_runtime_connection;

/// Seven-day window used by the active-digger victim strategy.
pub const ACTIVE_DIGGERS_LOOKBACK_SECONDS: i64 = 7 * 86_400;

/// Fourteen-day window used by the recently-active player strategy.
pub const RANDOM_ACTIVE_LOOKBACK_SECONDS: i64 = 14 * 86_400;

/// Typed victim-pool strategy.  Unknown strings are rejected at the boundary
/// and are intentionally handled as an empty, fail-soft splash by the app.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplashStrategy {
    /// Randomly ordered recently active guild players.
    RandomActive,
    /// Players with the largest positive balances.
    RichestN,
    /// Players with a recent Dig action.
    ActiveDiggers,
    /// Players with the deepest positive tunnel depths.
    DeepestN,
}

impl SplashStrategy {
    /// Parse the Python strategy spelling without accepting unknown values.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "random_active" => Some(Self::RandomActive),
            "richest_n" => Some(Self::RichestN),
            "active_diggers" => Some(Self::ActiveDiggers),
            "deepest_n" => Some(Self::DeepestN),
            _ => None,
        }
    }

    /// Return the stable wire spelling used in audit metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RandomActive => "random_active",
            Self::RichestN => "richest_n",
            Self::ActiveDiggers => "active_diggers",
            Self::DeepestN => "deepest_n",
        }
    }
}

/// The three splash balance semantics supported by Python.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplashMode {
    /// Destroy the victim's JC, optionally after white protection absorbs it.
    Burn,
    /// Mint/credit the requested amount to every selected recipient.
    Grant,
    /// Transfer the requested amount from each victim to the digger.
    Steal,
}

impl SplashMode {
    /// Parse the Python mode spelling.  Unknown modes fail closed.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "burn" => Some(Self::Burn),
            "grant" => Some(Self::Grant),
            "steal" => Some(Self::Steal),
            _ => None,
        }
    }

    /// Return the stable wire spelling used in audit metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Burn => "burn",
            Self::Grant => "grant",
            Self::Steal => "steal",
        }
    }
}

/// Query parameters for a victim-pool projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplashCandidateQuery {
    /// Guild whose players are eligible.
    pub guild_id: i64,
    /// Actor to exclude from every pool.
    pub digger_id: i64,
    /// Pool ordering policy.
    pub strategy: SplashStrategy,
    /// Maximum number of IDs returned.
    pub victim_count: usize,
    /// Unix timestamp lower bound for active strategies.
    pub active_since: i64,
}

/// One selected splash victim and the amount actually moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplashVictim {
    /// Victim Discord ID.
    pub victim_id: i64,
    /// Positive amount debited or credited.
    pub amount: i64,
}

/// Durable result of a splash batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplashSettlement {
    /// Amounts actually moved after balance clamps and protection.
    pub victims: Vec<SplashVictim>,
    /// Sum of `victims.amount`.
    pub total_moved: i64,
    /// JC absorbed by protection pools.
    pub absorbed_total: i64,
    /// Number of victims whose loss was at least partly protected.
    pub shielded_count: usize,
    /// Whether this call inserted at least one new durable row.
    pub applied_now: bool,
}

/// One atomically settled splash batch.
#[derive(Clone, Debug)]
pub struct SplashSettlementRequest<'a> {
    /// Guild whose balances are affected.
    pub guild_id: i64,
    /// Digger/caster, excluded from selection and recipient of steals.
    pub digger_id: i64,
    /// Human-readable event name retained in metadata.
    pub event_name: &'a str,
    /// Typed strategy retained in metadata.
    pub strategy: SplashStrategy,
    /// Balance semantics.
    pub mode: SplashMode,
    /// Authored/scaled positive amount requested per victim.
    pub amount: i64,
    /// IDs selected by the application entropy adapter.
    pub victim_ids: &'a [i64],
    /// Prefix used to build a stable per-victim idempotency key.
    pub event_key_prefix: &'a str,
    /// Unix timestamp for audit/protection rows.
    pub now: i64,
    /// Game date used by the White protection gateway.
    pub mana_date: &'a str,
}

/// Errors raised by the splash persistence seam.
#[derive(Debug, Error)]
pub enum DigSplashRuntimeRepositoryError {
    /// The amount must be strictly positive.
    #[error("splash amount must be positive")]
    InvalidAmount,
    /// A victim count cannot be empty for settlement.
    #[error("splash victim list is empty")]
    EmptyVictimList,
    /// The idempotency prefix is required.
    #[error("splash event key prefix cannot be empty")]
    EmptyEventKey,
    /// The event name is retained in audit rows and cannot be blank.
    #[error("splash event name cannot be empty")]
    EmptyEventName,
    /// A player row required by a transfer was absent.
    #[error("splash player {discord_id} is not registered in guild {guild_id}")]
    MissingPlayer { discord_id: i64, guild_id: i64 },
    /// A persisted idempotency key was reused with a different payload.
    #[error("splash event key was reused with a different payload")]
    EventPayloadConflict,
    /// An arithmetic operation exceeded SQLite's integer range.
    #[error("splash arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    /// The existing-schema operation failed.
    #[error("Dig splash SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Protection JSON was malformed and could not be interpreted.
    #[error("Dig splash protection JSON is invalid: {0}")]
    InvalidProtectionJson(String),
}

/// Existing-schema repository for Dig splash selection and settlement.
#[derive(Clone, Debug)]
pub struct DigSplashRuntimeRepository {
    path: PathBuf,
}

impl DigSplashRuntimeRepository {
    /// Open a repository against an already migrated SQLite database.
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        open_runtime_connection(&self.path)
    }

    /// Read candidate IDs with Python-compatible exclusions and ordering.
    pub fn candidate_ids(
        &self,
        query: SplashCandidateQuery,
    ) -> Result<Vec<i64>, DigSplashRuntimeRepositoryError> {
        let connection = self.connection()?;
        candidate_ids_on(&connection, query)
    }

    /// Settle every selected victim under one immediate transaction.
    ///
    /// Random strategies receive application-selected IDs, but every ID is
    /// revalidated while the write lock is held.  Each victim has its own
    /// savepoint, matching Python's batch behavior: one malformed/missing
    /// victim cannot roll back already valid victims, while the outer commit
    /// keeps ledger, protection, hostile-loss, and Dig audit rows coherent.
    pub fn settle_batch(
        &self,
        request: &SplashSettlementRequest<'_>,
    ) -> Result<SplashSettlement, DigSplashRuntimeRepositoryError> {
        validate_request(request)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut victims = Vec::new();
        let mut absorbed_total = 0_i64;
        let mut shielded_count = 0_usize;
        let mut applied_now = false;
        let mut seen = BTreeSet::new();

        for (index, victim_id) in request.victim_ids.iter().copied().enumerate() {
            if victim_id == request.digger_id || !seen.insert(victim_id) {
                continue;
            }
            let savepoint = format!("dig_splash_{index}");
            transaction.execute_batch(&format!("SAVEPOINT {savepoint}"))?;
            let outcome = settle_one(
                &transaction,
                request,
                victim_id,
                &mut absorbed_total,
                &mut shielded_count,
            );
            match outcome {
                Ok(Some((amount, inserted))) => {
                    transaction.execute_batch(&format!("RELEASE SAVEPOINT {savepoint}"))?;
                    if amount > 0 {
                        victims.push(SplashVictim { victim_id, amount });
                    }
                    applied_now |= inserted;
                }
                Ok(None) => {
                    transaction.execute_batch(&format!("RELEASE SAVEPOINT {savepoint}"))?;
                }
                Err(error) => {
                    transaction.execute_batch(&format!(
                        "ROLLBACK TO SAVEPOINT {savepoint}; RELEASE SAVEPOINT {savepoint}"
                    ))?;
                    // A missing/low-balance victim is a normal fail-soft skip;
                    // malformed data and SQLite errors still abort the batch.
                    if matches!(error, DigSplashRuntimeRepositoryError::MissingPlayer { .. }) {
                        continue;
                    }
                    return Err(error);
                }
            }
        }
        transaction.commit()?;
        let total_moved = victims.iter().try_fold(0_i64, |total, victim| {
            total
                .checked_add(victim.amount)
                .ok_or(DigSplashRuntimeRepositoryError::ArithmeticOverflow)
        })?;
        Ok(SplashSettlement {
            victims,
            total_moved,
            absorbed_total,
            shielded_count,
            applied_now,
        })
    }
}

fn validate_request(
    request: &SplashSettlementRequest<'_>,
) -> Result<(), DigSplashRuntimeRepositoryError> {
    if request.amount <= 0 {
        return Err(DigSplashRuntimeRepositoryError::InvalidAmount);
    }
    if request.victim_ids.is_empty() {
        return Err(DigSplashRuntimeRepositoryError::EmptyVictimList);
    }
    if request.event_key_prefix.trim().is_empty() {
        return Err(DigSplashRuntimeRepositoryError::EmptyEventKey);
    }
    if request.event_name.trim().is_empty() {
        return Err(DigSplashRuntimeRepositoryError::EmptyEventName);
    }
    Ok(())
}

fn candidate_ids_on(
    connection: &Connection,
    query: SplashCandidateQuery,
) -> Result<Vec<i64>, DigSplashRuntimeRepositoryError> {
    if query.victim_count == 0 {
        return Ok(Vec::new());
    }
    let limit = i64::try_from(query.victim_count)
        .map_err(|_| DigSplashRuntimeRepositoryError::ArithmeticOverflow)?;
    let sql = match query.strategy {
        SplashStrategy::RandomActive => {
            "SELECT discord_id FROM players
              WHERE guild_id=?1 AND discord_id != ?2
                AND last_match_date IS NOT NULL
                AND CAST(strftime('%s', last_match_date) AS INTEGER) >= ?3
              ORDER BY discord_id LIMIT ?4"
        }
        SplashStrategy::RichestN => {
            "SELECT discord_id FROM players
              WHERE guild_id=?1 AND discord_id != ?2
                AND COALESCE(jopacoin_balance,0) > 0
              ORDER BY COALESCE(jopacoin_balance,0) DESC, discord_id LIMIT ?4"
        }
        SplashStrategy::ActiveDiggers => {
            "SELECT actor_id FROM dig_actions
              WHERE guild_id=?1 AND actor_id != ?2
                AND action_type='dig' AND created_at >= ?3
              GROUP BY actor_id
              ORDER BY MAX(created_at) DESC, actor_id LIMIT ?4"
        }
        SplashStrategy::DeepestN => {
            "SELECT discord_id FROM tunnels
              WHERE guild_id=?1 AND discord_id != ?2
                AND COALESCE(depth,0) > 0
              ORDER BY COALESCE(depth,0) DESC, discord_id LIMIT ?4"
        }
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(
        params![query.guild_id, query.digger_id, query.active_since, limit],
        |row| row.get::<_, i64>(0),
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn settle_one(
    transaction: &Transaction<'_>,
    request: &SplashSettlementRequest<'_>,
    victim_id: i64,
    absorbed_total: &mut i64,
    shielded_count: &mut usize,
) -> Result<Option<(i64, bool)>, DigSplashRuntimeRepositoryError> {
    let detail_json = json!({
        "event_name": request.event_name,
        "strategy": request.strategy.as_str(),
        "digger_id": request.digger_id,
        "event_key": format!("{}:{victim_id}", request.event_key_prefix),
        "penalty_scaled": request.amount,
        "mode": request.mode.as_str(),
    })
    .to_string();
    let event_key = format!("{}:{victim_id}", request.event_key_prefix);

    match request.mode {
        SplashMode::Grant => {
            settle_grant(transaction, request, victim_id, &event_key, &detail_json)
        }
        SplashMode::Burn | SplashMode::Steal => settle_hostile(
            transaction,
            request,
            victim_id,
            &event_key,
            &detail_json,
            absorbed_total,
            shielded_count,
        ),
    }
}

fn settle_grant(
    transaction: &Transaction<'_>,
    request: &SplashSettlementRequest<'_>,
    victim_id: i64,
    event_key: &str,
    detail_json: &str,
) -> Result<Option<(i64, bool)>, DigSplashRuntimeRepositoryError> {
    if let Some(existing) = existing_action(transaction, request.guild_id, victim_id, event_key)? {
        return Ok(Some((existing, false)));
    }
    let balance = query_balance(transaction, victim_id, request.guild_id)?.ok_or(
        DigSplashRuntimeRepositoryError::MissingPlayer {
            discord_id: victim_id,
            guild_id: request.guild_id,
        },
    )?;
    let after = balance
        .checked_add(request.amount)
        .ok_or(DigSplashRuntimeRepositoryError::ArithmeticOverflow)?;
    set_ledger_context(
        transaction,
        request,
        "dig",
        "splash_victim",
        request.event_name,
        "dig splash victim grant",
    )?;
    update_balance(transaction, victim_id, request.guild_id, after, false)?;
    clear_ledger_context(transaction)?;
    insert_action(
        transaction,
        request,
        victim_id,
        None,
        request.amount,
        detail_json,
    )?;
    Ok(Some((request.amount, true)))
}

fn settle_hostile(
    transaction: &Transaction<'_>,
    request: &SplashSettlementRequest<'_>,
    victim_id: i64,
    event_key: &str,
    detail_json: &str,
    absorbed_total: &mut i64,
    shielded_count: &mut usize,
) -> Result<Option<(i64, bool)>, DigSplashRuntimeRepositoryError> {
    let existing = transaction
        .query_row(
            "SELECT event_id,requested,kind,destination,actor_id,recipient_id,
                    applied,absorbed
               FROM hostile_loss_events
              WHERE guild_id=?1 AND victim_id=?2 AND event_key=?3",
            params![request.guild_id, victim_id, event_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    if let Some((
        _event_id,
        requested,
        kind,
        destination,
        actor_id,
        recipient_id,
        applied,
        absorbed,
    )) = existing
    {
        let expected_kind = format!("dig_splash_{}", request.mode.as_str());
        let expected_destination = request.mode == SplashMode::Steal;
        if requested != request.amount
            || kind != expected_kind
            || destination
                != if expected_destination {
                    "player"
                } else {
                    "burn"
                }
            || actor_id != Some(request.digger_id)
            || recipient_id != expected_destination.then_some(request.digger_id)
        {
            return Err(DigSplashRuntimeRepositoryError::EventPayloadConflict);
        }
        if absorbed > 0 {
            *absorbed_total = absorbed_total
                .checked_add(absorbed)
                .ok_or(DigSplashRuntimeRepositoryError::ArithmeticOverflow)?;
            *shielded_count += 1;
        }
        return Ok(Some((applied, false)));
    }

    let victim_before = query_balance(transaction, victim_id, request.guild_id)?.ok_or(
        DigSplashRuntimeRepositoryError::MissingPlayer {
            discord_id: victim_id,
            guild_id: request.guild_id,
        },
    )?;
    if victim_before < HOSTILE_LOSS_MIN_BALANCE {
        return Ok(None);
    }
    let attempted = if request.mode == SplashMode::Burn {
        request.amount.min(victim_before.max(0))
    } else {
        request.amount
    };
    if attempted <= 0 {
        return Ok(None);
    }
    let shieldable = request.digger_id != victim_id;
    let mut remaining = attempted;
    let mut protection_details = Vec::new();
    let mut pool_keys = Vec::new();
    if shieldable {
        apply_protection(
            transaction,
            request,
            victim_id,
            &mut remaining,
            &mut protection_details,
            &mut pool_keys,
        )?;
    }
    let applied = remaining;
    let absorbed = attempted.saturating_sub(applied);
    if absorbed > 0 {
        *absorbed_total = absorbed_total
            .checked_add(absorbed)
            .ok_or(DigSplashRuntimeRepositoryError::ArithmeticOverflow)?;
        *shielded_count += 1;
    }
    let victim_after = victim_before
        .checked_sub(applied)
        .ok_or(DigSplashRuntimeRepositoryError::ArithmeticOverflow)?;
    let destination = if request.mode == SplashMode::Steal {
        "player"
    } else {
        "burn"
    };
    let destination_before = if request.mode == SplashMode::Steal {
        Some(
            query_balance(transaction, request.digger_id, request.guild_id)?.ok_or(
                DigSplashRuntimeRepositoryError::MissingPlayer {
                    discord_id: request.digger_id,
                    guild_id: request.guild_id,
                },
            )?,
        )
    } else {
        None
    };
    let destination_after = destination_before
        .map(|balance| {
            balance
                .checked_add(applied)
                .ok_or(DigSplashRuntimeRepositoryError::ArithmeticOverflow)
        })
        .transpose()?;
    let details_json = protection_details_json(&protection_details);
    transaction.execute(
        "INSERT INTO hostile_loss_events (
             guild_id,victim_id,actor_id,event_key,kind,destination,recipient_id,
             requested,attempted,absorbed,applied,victim_balance_before,
             victim_balance_after,destination_balance_before,destination_balance_after,
             shieldable,retro_covered,protection_details,metadata,occurred_at,created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,0,?17,?18,?19,?19)",
        params![
            request.guild_id,
            victim_id,
            request.digger_id,
            event_key,
            format!("dig_splash_{}", request.mode.as_str()),
            destination,
            (request.mode == SplashMode::Steal).then_some(request.digger_id),
            request.amount,
            attempted,
            absorbed,
            applied,
            victim_before,
            victim_after,
            destination_before,
            destination_after,
            i64::from(shieldable),
            details_json,
            detail_json,
            request.now,
        ],
    )?;
    let hostile_event_id = transaction.last_insert_rowid();
    for (detail, pool_key) in protection_details.iter().zip(pool_keys.iter()) {
        insert_protection_event(
            transaction,
            hostile_event_id,
            request,
            victim_id,
            detail,
            pool_key,
        )?;
    }
    if applied > 0 {
        let reason = format!("dig_splash_{} hostile loss", request.mode.as_str());
        set_ledger_context(
            transaction,
            request,
            "hostile_loss",
            "hostile_loss_event",
            &hostile_event_id.to_string(),
            &reason,
        )?;
        update_balance(transaction, victim_id, request.guild_id, victim_after, true)?;
        if request.mode == SplashMode::Steal {
            let after = destination_after.expect("steal recipient balance");
            update_balance(
                transaction,
                request.digger_id,
                request.guild_id,
                after,
                false,
            )?;
        }
        clear_ledger_context(transaction)?;
    }
    if applied > 0 {
        insert_action(transaction, request, victim_id, None, -applied, detail_json)?;
    }
    if request.mode == SplashMode::Steal && applied > 0 {
        insert_action(
            transaction,
            request,
            request.digger_id,
            Some(victim_id),
            applied,
            detail_json,
        )?;
    }
    Ok(Some((applied, true)))
}

#[derive(Clone, Debug)]
struct ProtectionDetail {
    source: String,
    absorbed: i64,
    rate: f64,
    capacity_before: Option<i64>,
    capacity_after: Option<i64>,
    buff_id: Option<i64>,
}

fn apply_protection(
    transaction: &Transaction<'_>,
    request: &SplashSettlementRequest<'_>,
    victim_id: i64,
    remaining: &mut i64,
    details: &mut Vec<ProtectionDetail>,
    pool_keys: &mut Vec<String>,
) -> Result<(), DigSplashRuntimeRepositoryError> {
    if let Some(buff_id) = active_buff_id(
        transaction,
        victim_id,
        request.guild_id,
        "counterspell",
        request.now,
        false,
    )? {
        details.push(ProtectionDetail {
            source: "counterspell".to_owned(),
            absorbed: *remaining,
            rate: 1.0,
            capacity_before: None,
            capacity_after: None,
            buff_id: Some(buff_id),
        });
        pool_keys.push(format!("counterspell:{buff_id}"));
        *remaining = 0;
        return Ok(());
    }
    for (buff_type, default_capacity, default_rate, shared) in [
        ("aegis", 75_i64, 1.0_f64, false),
        ("sanctuary", 150_i64, 1.0_f64, true),
        ("reprieve", 25_i64, 0.5_f64, false),
    ] {
        if *remaining <= 0 {
            break;
        }
        let buffs = active_buffs(
            transaction,
            victim_id,
            request.guild_id,
            buff_type,
            request.now,
            shared,
        )?;
        for buff in buffs {
            let (mut data, capacity, rate) =
                decode_pool(&buff.data, default_capacity, default_rate)?;
            if capacity <= 0 || rate <= 0.0 {
                continue;
            }
            let absorbed = capacity
                .min(*remaining)
                .min(rate_absorption(*remaining, rate));
            if absorbed <= 0 {
                continue;
            }
            let after = capacity - absorbed;
            data["capacity_remaining"] = Value::from(after);
            transaction.execute(
                "UPDATE manashop_buffs SET data=?1,triggered=?2 WHERE id=?3",
                params![data.to_string(), i64::from(after <= 0), buff.id],
            )?;
            details.push(ProtectionDetail {
                source: buff_type.to_owned(),
                absorbed,
                rate,
                capacity_before: Some(capacity),
                capacity_after: Some(after),
                buff_id: Some(buff.id),
            });
            pool_keys.push(format!("buff:{}", buff.id));
            *remaining -= absorbed;
            if *remaining <= 0 {
                break;
            }
        }
    }
    if *remaining > 0 {
        let capacity = transaction
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                  WHERE discord_id=?1 AND guild_id=?2 AND current_land='Plains'
                    AND assigned_date=?3 AND consumed_today=0",
                params![victim_id, request.guild_id, request.mana_date],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or_default();
        if capacity > 0 {
            let absorbed = capacity.min(rate_absorption(*remaining, 0.5));
            let after = capacity - absorbed;
            transaction.execute(
                "UPDATE player_mana SET white_shield_remaining=?1,updated_at=CURRENT_TIMESTAMP
                  WHERE discord_id=?2 AND guild_id=?3",
                params![after, victim_id, request.guild_id],
            )?;
            details.push(ProtectionDetail {
                source: "guardian".to_owned(),
                absorbed,
                rate: 0.5,
                capacity_before: Some(capacity),
                capacity_after: Some(after),
                buff_id: None,
            });
            pool_keys.push(format!("guardian:{}", request.mana_date));
            *remaining -= absorbed;
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct ActiveBuff {
    id: i64,
    data: Option<String>,
}

fn active_buff_id(
    transaction: &Transaction<'_>,
    victim_id: i64,
    guild_id: i64,
    buff_type: &str,
    now: i64,
    shared: bool,
) -> Result<Option<i64>, rusqlite::Error> {
    let sql = if shared {
        "SELECT id FROM manashop_buffs WHERE (discord_id=?1 OR target_id=?1)
         AND guild_id=?2 AND buff_type=?3 AND triggered=0 AND expires_at>?4
         ORDER BY expires_at,granted_at,id LIMIT 1"
    } else {
        "SELECT id FROM manashop_buffs WHERE discord_id=?1 AND guild_id=?2
         AND buff_type=?3 AND triggered=0 AND expires_at>?4
         ORDER BY expires_at,granted_at,id LIMIT 1"
    };
    transaction
        .query_row(sql, params![victim_id, guild_id, buff_type, now], |row| {
            row.get(0)
        })
        .optional()
}

fn active_buffs(
    transaction: &Transaction<'_>,
    victim_id: i64,
    guild_id: i64,
    buff_type: &str,
    now: i64,
    shared: bool,
) -> Result<Vec<ActiveBuff>, rusqlite::Error> {
    let sql = if shared {
        "SELECT id,data FROM manashop_buffs WHERE (discord_id=?1 OR target_id=?1)
         AND guild_id=?2 AND buff_type=?3 AND triggered=0 AND expires_at>?4
         ORDER BY expires_at,granted_at,id"
    } else {
        "SELECT id,data FROM manashop_buffs WHERE discord_id=?1 AND guild_id=?2
         AND buff_type=?3 AND triggered=0 AND expires_at>?4
         ORDER BY expires_at,granted_at,id"
    };
    transaction
        .prepare(sql)?
        .query_map(params![victim_id, guild_id, buff_type, now], |row| {
            Ok(ActiveBuff {
                id: row.get(0)?,
                data: row.get(1)?,
            })
        })?
        .collect()
}

fn decode_pool(
    raw: &Option<String>,
    default_capacity: i64,
    default_rate: f64,
) -> Result<(Value, i64, f64), DigSplashRuntimeRepositoryError> {
    let mut data = match raw.as_deref() {
        Some(raw) => serde_json::from_str::<Value>(raw).map_err(|error| {
            DigSplashRuntimeRepositoryError::InvalidProtectionJson(error.to_string())
        })?,
        None => json!({}),
    };
    if !data.is_object() {
        data = json!({});
    }
    let capacity = data
        .get("capacity_remaining")
        .and_then(Value::as_i64)
        .unwrap_or(default_capacity)
        .max(0);
    let rate = data
        .get("rate")
        .and_then(Value::as_f64)
        .unwrap_or(default_rate)
        .clamp(0.0, 1.0);
    Ok((data, capacity, rate))
}

fn rate_absorption(amount: i64, rate: f64) -> i64 {
    if amount <= 0 || rate <= 0.0 {
        0
    } else if rate >= 1.0 {
        amount
    } else {
        ((amount as f64 * rate).floor() as i64).max(1)
    }
}

fn protection_details_json(details: &[ProtectionDetail]) -> String {
    details
        .iter()
        .map(|detail| {
            json!({
                "source": detail.source,
                "absorbed": detail.absorbed,
                "rate": detail.rate,
                "capacity_before": detail.capacity_before,
                "capacity_after": detail.capacity_after,
                "buff_id": detail.buff_id,
                "retroactive": false,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .collect::<Value>()
        .to_string()
}

fn insert_protection_event(
    transaction: &Transaction<'_>,
    hostile_event_id: i64,
    request: &SplashSettlementRequest<'_>,
    victim_id: i64,
    detail: &ProtectionDetail,
    pool_key: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO mana_protection_events (
             hostile_loss_event_id,guild_id,victim_id,protection_type,pool_key,
             buff_id,amount,rate,capacity_before,capacity_after,retroactive,
             created_at,details
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0,?11,?12)",
        params![
            hostile_event_id,
            request.guild_id,
            victim_id,
            detail.source,
            pool_key,
            detail.buff_id,
            detail.absorbed,
            detail.rate,
            detail.capacity_before,
            detail.capacity_after,
            request.now,
            json!({"dig_splash": true}).to_string(),
        ],
    )?;
    Ok(())
}

fn query_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
              WHERE discord_id=?1 AND guild_id=?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn update_balance(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    balance: i64,
    track_lowest: bool,
) -> Result<(), DigSplashRuntimeRepositoryError> {
    let changed = transaction.execute(
        "UPDATE players SET jopacoin_balance=?1,updated_at=CURRENT_TIMESTAMP
          WHERE discord_id=?2 AND guild_id=?3",
        params![balance, discord_id, guild_id],
    )?;
    if changed != 1 {
        return Err(DigSplashRuntimeRepositoryError::MissingPlayer {
            discord_id,
            guild_id,
        });
    }
    if track_lowest {
        transaction.execute(
            "UPDATE players SET lowest_balance_ever=jopacoin_balance
              WHERE discord_id=?1 AND guild_id=?2
                AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)",
            params![discord_id, guild_id],
        )?;
    }
    Ok(())
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: &SplashSettlementRequest<'_>,
    source: &str,
    related_type: &str,
    related_id: &str,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context
             (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,?1,?2,?3,?4,?5,?6)",
        params![
            source,
            request.digger_id,
            related_type,
            related_id,
            reason,
            json!({"event_name": request.event_name, "mode": request.mode.as_str()}).to_string(),
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn insert_action(
    transaction: &Transaction<'_>,
    request: &SplashSettlementRequest<'_>,
    actor_id: i64,
    target_id: Option<i64>,
    delta: i64,
    detail_json: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_actions (
             guild_id,actor_id,target_id,action_type,depth_before,depth_after,
             jc_delta,detail,created_at
         ) VALUES (?1,?2,?3,?4,0,0,?5,?6,?7)",
        params![
            request.guild_id,
            actor_id,
            target_id,
            "splash_victim",
            delta,
            detail_json,
            request.now,
        ],
    )?;
    Ok(())
}

fn existing_action(
    transaction: &Transaction<'_>,
    guild_id: i64,
    actor_id: i64,
    event_key: &str,
) -> Result<Option<i64>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT jc_delta FROM dig_actions
              WHERE guild_id=?1 AND actor_id=?2 AND action_type='splash_victim'
                AND json_valid(detail)
                AND json_extract(detail, '$.event_key')=?3 ORDER BY id LIMIT 1",
            params![guild_id, actor_id, event_key],
            |row| row.get(0),
        )
        .optional()
}

#[cfg(test)]
#[path = "dig_splash_runtime/tests.rs"]
mod tests;
