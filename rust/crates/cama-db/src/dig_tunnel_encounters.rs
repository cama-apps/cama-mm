//! Existing-schema settlement for deflationary Dig tunnel encounters.
//!
//! Python remains the production runtime and migration authority. This
//! adapter opens only an already-migrated database through
//! [`crate::open_runtime_connection`]. Candidate selection, victim burns,
//! proportional actor payout (or failure loss), ledger context, and Dig audit
//! rows commit together under one `BEGIN IMMEDIATE` transaction.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_splash::{
    BurnSplashResult, HOSTILE_LOSS_MIN_BALANCE, strengthen_dig_event_penalty,
};
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncounterVictimStrategy {
    RichestN,
    ActiveDiggers,
    RandomActive,
    DeepestN,
}

impl EncounterVictimStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RichestN => "richest_n",
            Self::ActiveDiggers => "active_diggers",
            Self::RandomActive => "random_active",
            Self::DeepestN => "deepest_n",
        }
    }

    #[must_use]
    pub const fn uses_random_sample(self) -> bool {
        matches!(self, Self::ActiveDiggers | Self::RandomActive)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncounterCandidateQuery {
    pub guild_id: Option<i64>,
    pub digger_discord_id: i64,
    pub strategy: EncounterVictimStrategy,
    pub active_since: i64,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncounterOutcome {
    Success {
        strategy: EncounterVictimStrategy,
        victim_count: usize,
        authored_penalty_jc: i64,
        jittered_authored_reward_jc: i64,
        /// Entropy-selected IDs for random strategies. Deterministic
        /// strategies are selected again while holding the write lock.
        preferred_victim_ids: Vec<i64>,
    },
    Failure {
        /// Negative-event multiplier and jitter have already been applied;
        /// Dig-specific strengthening and structural scaling happen here.
        tuned_authored_loss_jc: i64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncounterSettlementRequest {
    pub digger_discord_id: i64,
    pub guild_id: Option<i64>,
    pub event_id: String,
    pub event_name: String,
    pub event_key: String,
    pub outcome: EncounterOutcome,
    pub minigame_jc_delta_scale: f64,
    pub active_since: i64,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncounterSettlement {
    pub succeeded: bool,
    pub actor_delta: i64,
    pub actor_balance: i64,
    pub splash: Option<BurnSplashResult>,
    pub applied_now: bool,
}

#[derive(Debug, Error)]
pub enum DigTunnelEncounterRepositoryError {
    #[error("event id cannot be empty")]
    EmptyEventId,
    #[error("event key cannot be empty")]
    EmptyEventKey,
    #[error("minigame scale must be finite and positive: {0}")]
    InvalidMinigameScale(f64),
    #[error("successful encounter victim count must be positive")]
    EmptyVictimCount,
    #[error("successful encounter penalty must be positive: {0}")]
    InvalidPenalty(i64),
    #[error("successful encounter reward cannot be negative: {0}")]
    InvalidReward(i64),
    #[error("failed encounter loss cannot be positive: {0}")]
    InvalidLoss(i64),
    #[error("player {discord_id} was not found in guild {guild_id}")]
    PlayerNotFound { discord_id: i64, guild_id: i64 },
    #[error("encounter arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error("Dig tunnel-encounter SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug)]
pub struct DigTunnelEncounterRepository {
    path: PathBuf,
}

impl DigTunnelEncounterRepository {
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

    pub fn balance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<i64>, DigTunnelEncounterRepositoryError> {
        let connection = self.connection()?;
        query_balance(&connection, discord_id, Self::normalize_guild_id(guild_id))
            .map_err(Into::into)
    }

    /// Read a deterministic candidate pool. Application entropy samples the
    /// two random strategies before passing IDs back to atomic settlement.
    pub fn candidate_ids(
        &self,
        query: EncounterCandidateQuery,
    ) -> Result<Vec<i64>, DigTunnelEncounterRepositoryError> {
        let connection = self.connection()?;
        candidate_ids_on(&connection, query)
    }

    /// Atomically burn selected victims and apply the proportional actor
    /// result. A repeated `(guild, actor, event id, event key)` returns the
    /// committed result without applying another balance change.
    pub fn settle_encounter_atomic(
        &self,
        request: &EncounterSettlementRequest,
    ) -> Result<EncounterSettlement, DigTunnelEncounterRepositoryError> {
        validate_request(request)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let detail = format!(
            "dig-encounter:{}:{}:{}",
            request.event_id, request.event_key, request.event_name
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let actor_balance_before =
            query_balance(&transaction, request.digger_discord_id, guild_id)?.ok_or(
                DigTunnelEncounterRepositoryError::PlayerNotFound {
                    discord_id: request.digger_discord_id,
                    guild_id,
                },
            )?;

        if let Some(actor_delta) = transaction
            .query_row(
                "SELECT jc_delta
                   FROM dig_actions
                  WHERE guild_id = ?1 AND actor_id = ?2
                    AND action_type = 'event' AND detail = ?3
                  ORDER BY id LIMIT 1",
                params![guild_id, request.digger_discord_id, &detail],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            let victims = committed_victims(&transaction, guild_id, &detail)?;
            let actor_balance = query_balance(&transaction, request.digger_discord_id, guild_id)?
                .ok_or(DigTunnelEncounterRepositoryError::PlayerNotFound {
                discord_id: request.digger_discord_id,
                guild_id,
            })?;
            transaction.commit()?;
            return Ok(EncounterSettlement {
                succeeded: matches!(request.outcome, EncounterOutcome::Success { .. }),
                actor_delta,
                actor_balance,
                splash: splash_from_victims(victims),
                applied_now: false,
            });
        }

        let (succeeded, actor_delta, victims) = match &request.outcome {
            EncounterOutcome::Success {
                strategy,
                victim_count,
                authored_penalty_jc,
                jittered_authored_reward_jc,
                preferred_victim_ids,
            } => {
                let penalty = scale_deflationary_minigame_jc_delta(
                    strengthen_dig_event_penalty(*authored_penalty_jc) as f64,
                    request.minigame_jc_delta_scale,
                );
                let selected = select_victims_under_lock(
                    &transaction,
                    guild_id,
                    request.digger_discord_id,
                    *strategy,
                    *victim_count,
                    request.active_since,
                    preferred_victim_ids,
                )?;
                let mut victims = Vec::new();
                let mut total_burned = 0_i64;
                for victim_id in selected {
                    let Some(balance) = query_balance(&transaction, victim_id, guild_id)? else {
                        continue;
                    };
                    if balance < HOSTILE_LOSS_MIN_BALANCE {
                        continue;
                    }
                    let actual = penalty.min(balance);
                    if actual <= 0 {
                        continue;
                    }
                    let new_balance = balance
                        .checked_sub(actual)
                        .ok_or(DigTunnelEncounterRepositoryError::ArithmeticOverflow)?;
                    set_ledger_context(
                        &transaction,
                        request,
                        "splash_victim",
                        "dig splash victim penalty",
                    )?;
                    update_balance(&transaction, victim_id, guild_id, new_balance)?;
                    clear_ledger_context(&transaction)?;
                    insert_action(
                        &transaction,
                        ActionRecord {
                            actor_id: victim_id,
                            target_id: Some(request.digger_discord_id),
                            guild_id,
                            action_type: "splash_victim",
                            jc_delta: -actual,
                            detail: &detail,
                            now: request.now,
                        },
                    )?;
                    total_burned = total_burned
                        .checked_add(actual)
                        .ok_or(DigTunnelEncounterRepositoryError::ArithmeticOverflow)?;
                    victims.push((victim_id, actual));
                }

                let nominal_burn = i128::from(penalty)
                    .checked_mul(
                        i128::try_from(*victim_count)
                            .map_err(|_| DigTunnelEncounterRepositoryError::ArithmeticOverflow)?,
                    )
                    .ok_or(DigTunnelEncounterRepositoryError::ArithmeticOverflow)?;
                let proportional_authored = round_ratio_half_even(
                    i128::from(*jittered_authored_reward_jc)
                        .checked_mul(i128::from(total_burned))
                        .ok_or(DigTunnelEncounterRepositoryError::ArithmeticOverflow)?,
                    nominal_burn,
                )?;
                let structurally_scaled = scale_minigame_jc_delta(
                    proportional_authored as f64,
                    request.minigame_jc_delta_scale,
                );
                let payout = scale_positive_dig_jc(structurally_scaled);
                (true, payout, victims)
            }
            EncounterOutcome::Failure {
                tuned_authored_loss_jc,
            } => {
                let strengthened = strengthen_dig_event_penalty(*tuned_authored_loss_jc);
                let loss = scale_deflationary_minigame_jc_delta(
                    strengthened as f64,
                    request.minigame_jc_delta_scale,
                );
                (false, loss, Vec::new())
            }
        };

        let actor_balance = actor_balance_before
            .checked_add(actor_delta)
            .ok_or(DigTunnelEncounterRepositoryError::ArithmeticOverflow)?;
        if actor_delta != 0 {
            set_ledger_context(&transaction, request, "event", "dig event result")?;
            update_balance(
                &transaction,
                request.digger_discord_id,
                guild_id,
                actor_balance,
            )?;
            clear_ledger_context(&transaction)?;
        }
        insert_action(
            &transaction,
            ActionRecord {
                actor_id: request.digger_discord_id,
                target_id: None,
                guild_id,
                action_type: "event",
                jc_delta: actor_delta,
                detail: &detail,
                now: request.now,
            },
        )?;
        transaction.commit()?;

        Ok(EncounterSettlement {
            succeeded,
            actor_delta,
            actor_balance,
            splash: splash_from_victims(victims),
            applied_now: true,
        })
    }
}

fn validate_request(
    request: &EncounterSettlementRequest,
) -> Result<(), DigTunnelEncounterRepositoryError> {
    if request.event_id.is_empty() {
        return Err(DigTunnelEncounterRepositoryError::EmptyEventId);
    }
    if request.event_key.is_empty() {
        return Err(DigTunnelEncounterRepositoryError::EmptyEventKey);
    }
    if !request.minigame_jc_delta_scale.is_finite() || request.minigame_jc_delta_scale <= 0.0 {
        return Err(DigTunnelEncounterRepositoryError::InvalidMinigameScale(
            request.minigame_jc_delta_scale,
        ));
    }
    match request.outcome {
        EncounterOutcome::Success {
            victim_count,
            authored_penalty_jc,
            jittered_authored_reward_jc,
            ..
        } => {
            if victim_count == 0 {
                return Err(DigTunnelEncounterRepositoryError::EmptyVictimCount);
            }
            if authored_penalty_jc <= 0 {
                return Err(DigTunnelEncounterRepositoryError::InvalidPenalty(
                    authored_penalty_jc,
                ));
            }
            if jittered_authored_reward_jc < 0 {
                return Err(DigTunnelEncounterRepositoryError::InvalidReward(
                    jittered_authored_reward_jc,
                ));
            }
        }
        EncounterOutcome::Failure {
            tuned_authored_loss_jc,
        } if tuned_authored_loss_jc > 0 => {
            return Err(DigTunnelEncounterRepositoryError::InvalidLoss(
                tuned_authored_loss_jc,
            ));
        }
        EncounterOutcome::Failure { .. } => {}
    }
    Ok(())
}

fn candidate_ids_on(
    connection: &Connection,
    query: EncounterCandidateQuery,
) -> Result<Vec<i64>, DigTunnelEncounterRepositoryError> {
    let guild_id = DigTunnelEncounterRepository::normalize_guild_id(query.guild_id);
    let limit = match query.limit {
        Some(limit) => i64::try_from(limit)
            .map_err(|_| DigTunnelEncounterRepositoryError::ArithmeticOverflow)?,
        None => -1,
    };
    if limit == 0 {
        return Ok(Vec::new());
    }
    let sql = match query.strategy {
        EncounterVictimStrategy::RichestN => {
            "SELECT discord_id
               FROM players
              WHERE guild_id = ?1 AND discord_id != ?2
                AND COALESCE(jopacoin_balance, 0) >= 1
              ORDER BY COALESCE(jopacoin_balance, 0) DESC, discord_id
              LIMIT ?4"
        }
        EncounterVictimStrategy::ActiveDiggers => {
            "SELECT actions.actor_id
               FROM dig_actions AS actions
               JOIN players
                 ON players.guild_id = actions.guild_id
                AND players.discord_id = actions.actor_id
              WHERE actions.guild_id = ?1 AND actions.actor_id != ?2
                AND actions.action_type = 'dig' AND actions.created_at >= ?3
              GROUP BY actions.actor_id
              ORDER BY MAX(actions.created_at) DESC, actions.actor_id
              LIMIT ?4"
        }
        EncounterVictimStrategy::RandomActive => {
            "SELECT discord_id
               FROM players
              WHERE guild_id = ?1 AND discord_id != ?2
                AND last_match_date IS NOT NULL
                AND CAST(strftime('%s', last_match_date) AS INTEGER) >= ?3
              ORDER BY discord_id
              LIMIT ?4"
        }
        EncounterVictimStrategy::DeepestN => {
            "SELECT tunnels.discord_id
               FROM tunnels
               JOIN players
                 ON players.guild_id = tunnels.guild_id
                AND players.discord_id = tunnels.discord_id
              WHERE tunnels.guild_id = ?1 AND tunnels.discord_id != ?2
                AND COALESCE(tunnels.depth, 0) > 0
              ORDER BY COALESCE(tunnels.depth, 0) DESC, tunnels.discord_id
              LIMIT ?4"
        }
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(
        params![guild_id, query.digger_discord_id, query.active_since, limit],
        |row| row.get::<_, i64>(0),
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn select_victims_under_lock(
    transaction: &Transaction<'_>,
    guild_id: i64,
    digger_id: i64,
    strategy: EncounterVictimStrategy,
    victim_count: usize,
    active_since: i64,
    preferred_victim_ids: &[i64],
) -> Result<Vec<i64>, DigTunnelEncounterRepositoryError> {
    if !strategy.uses_random_sample() {
        return candidate_ids_on(
            transaction,
            EncounterCandidateQuery {
                guild_id: Some(guild_id),
                digger_discord_id: digger_id,
                strategy,
                active_since,
                limit: Some(victim_count),
            },
        );
    }

    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    for &victim_id in preferred_victim_ids {
        if victim_id == digger_id || !seen.insert(victim_id) {
            continue;
        }
        if stochastic_candidate_is_still_eligible(
            transaction,
            guild_id,
            victim_id,
            strategy,
            active_since,
        )? {
            selected.push(victim_id);
        }
        if selected.len() == victim_count {
            break;
        }
    }
    Ok(selected)
}

fn stochastic_candidate_is_still_eligible(
    transaction: &Transaction<'_>,
    guild_id: i64,
    victim_id: i64,
    strategy: EncounterVictimStrategy,
    active_since: i64,
) -> Result<bool, rusqlite::Error> {
    match strategy {
        EncounterVictimStrategy::RandomActive => transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM players
                  WHERE guild_id = ?1 AND discord_id = ?2
                    AND last_match_date IS NOT NULL
                    AND CAST(strftime('%s', last_match_date) AS INTEGER) >= ?3
             )",
            params![guild_id, victim_id, active_since],
            |row| row.get(0),
        ),
        EncounterVictimStrategy::ActiveDiggers => transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1
                   FROM players
                   JOIN dig_actions
                     ON dig_actions.guild_id = players.guild_id
                    AND dig_actions.actor_id = players.discord_id
                  WHERE players.guild_id = ?1 AND players.discord_id = ?2
                    AND dig_actions.action_type = 'dig'
                    AND dig_actions.created_at >= ?3
             )",
            params![guild_id, victim_id, active_since],
            |row| row.get(0),
        ),
        EncounterVictimStrategy::RichestN | EncounterVictimStrategy::DeepestN => Ok(false),
    }
}

fn round_ratio_half_even(
    numerator: i128,
    denominator: i128,
) -> Result<i64, DigTunnelEncounterRepositoryError> {
    if numerator < 0 || denominator <= 0 {
        return Err(DigTunnelEncounterRepositoryError::ArithmeticOverflow);
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let doubled = remainder
        .checked_mul(2)
        .ok_or(DigTunnelEncounterRepositoryError::ArithmeticOverflow)?;
    let rounded = if doubled > denominator || (doubled == denominator && quotient % 2 != 0) {
        quotient
            .checked_add(1)
            .ok_or(DigTunnelEncounterRepositoryError::ArithmeticOverflow)?
    } else {
        quotient
    };
    i64::try_from(rounded).map_err(|_| DigTunnelEncounterRepositoryError::ArithmeticOverflow)
}

fn query_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0)
               FROM players
              WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn update_balance(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    new_balance: i64,
) -> Result<(), DigTunnelEncounterRepositoryError> {
    let changed = transaction.execute(
        "UPDATE players
            SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
          WHERE discord_id = ?2 AND guild_id = ?3",
        params![new_balance, discord_id, guild_id],
    )?;
    if changed != 1 {
        return Err(DigTunnelEncounterRepositoryError::PlayerNotFound {
            discord_id,
            guild_id,
        });
    }
    Ok(())
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: &EncounterSettlementRequest,
    related_type: &str,
    reason: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'dig', ?1, ?2, ?3, ?4, '{}')",
        params![
            request.digger_discord_id,
            related_type,
            request.event_id,
            reason
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

#[derive(Clone, Copy)]
struct ActionRecord<'a> {
    actor_id: i64,
    target_id: Option<i64>,
    guild_id: i64,
    action_type: &'a str,
    jc_delta: i64,
    detail: &'a str,
    now: i64,
}

fn insert_action(
    transaction: &Transaction<'_>,
    action: ActionRecord<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_actions (
             guild_id, actor_id, target_id, action_type,
             depth_before, depth_after, jc_delta, detail, created_at
         ) VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, ?6, ?7)",
        params![
            action.guild_id,
            action.actor_id,
            action.target_id,
            action.action_type,
            action.jc_delta,
            action.detail,
            action.now
        ],
    )?;
    Ok(())
}

fn committed_victims(
    transaction: &Transaction<'_>,
    guild_id: i64,
    detail: &str,
) -> Result<Vec<(i64, i64)>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT actor_id, -jc_delta
           FROM dig_actions
          WHERE guild_id = ?1 AND action_type = 'splash_victim' AND detail = ?2
          ORDER BY id",
    )?;
    let rows = statement.query_map(params![guild_id, detail], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.collect()
}

fn splash_from_victims(victims: Vec<(i64, i64)>) -> Option<BurnSplashResult> {
    if victims.is_empty() {
        None
    } else {
        Some(BurnSplashResult {
            total_burned: victims.iter().map(|(_, amount)| amount).sum(),
            victims,
        })
    }
}

#[cfg(test)]
#[path = "dig_tunnel_encounters/tests.rs"]
mod tests;
