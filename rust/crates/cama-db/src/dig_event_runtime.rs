//! Atomic persistence for one resolved `/dig` event actor outcome.
//!
//! The application layer owns event policy and random selection. This module
//! only persists the resulting actor tunnel state, wallet delta, optional
//! reward, ledger context, and audit row in one guarded transaction. Splash
//! victims deliberately remain outside this transaction, matching Python's
//! visible failure boundary. Guild modifiers and quest progression are also
//! separate, fail-soft application follow-ups.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use thiserror::Error;

use crate::open_runtime_connection;

const BET_QUEST_LOOKBACK_SECONDS: i64 = 7 * 24 * 60 * 60;

/// A player tunnel key. `None` retains Python's historical global-guild
/// sentinel (`0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigEventActorKey {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
}

/// State read before event policy runs and guarded at settlement time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigEventActorSnapshot {
    pub key: DigEventActorKey,
    pub depth: i64,
    pub luminosity: i64,
    pub prestige_level: i64,
    pub prestige_perks_json: String,
    pub boss_progress_json: String,
    pub streak_days: i64,
    pub temp_buff_json: Option<String>,
    pub temp_curse_json: Option<String>,
    pub balance: i64,
    pub inventory_count: usize,
    pub owned_gear: BTreeSet<String>,
    pub equipped_gear: BTreeSet<String>,
    pub owned_artifacts: BTreeSet<String>,
    pub equipped_relics: BTreeSet<String>,
}

/// Selective ownership reads needed by non-JC event reward admission.
///
/// The event policy always needs the equipped sets, but ordinary events do not
/// need to read the player's full gear, inventory, or artifact collections.
/// Keeping those reads explicit also lets the application load the selected
/// reward branch after the outcome is known, matching Python's lazy calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DigEventRewardQueryPlan {
    pub owned_gear: bool,
    pub inventory_count: bool,
    pub owned_artifacts: bool,
}

/// The ownership/capacity projection used to select one event reward.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DigEventRewardSnapshot {
    pub inventory_count: usize,
    pub owned_gear: BTreeSet<String>,
    pub owned_artifacts: BTreeSet<String>,
}

/// Durable event identity emitted by the actor's successful Dig action.
///
/// Discord custom ids only carry the compact `dig_actions.id`.  Resolving the
/// authored event from the audit row keeps the component below Discord's
/// 100-character limit and, more importantly, prevents a client from
/// substituting a different event id into an otherwise valid interaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigPendingEvent {
    pub action_id: i64,
    pub actor_id: i64,
    pub guild_id: i64,
    pub event_id: String,
    pub created_at: i64,
}

/// Mutation for one nullable JSON tunnel column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventJsonMutation<'a> {
    Preserve,
    Clear,
    Replace(&'a str),
}

/// A non-JC event reward selected by the application policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigEventReward<'a> {
    Gear {
        item_id: &'a str,
        slot: &'a str,
        tier: i64,
        durability: i64,
        source: &'a str,
    },
    Consumable {
        item_type: &'a str,
    },
    Artifact {
        artifact_id: &'a str,
        is_relic: bool,
    },
}

/// Fully calculated event actor settlement. `detail_json` must be a JSON
/// object; the repository adds durable idempotency and receipt fields.
#[derive(Clone, Copy, Debug)]
pub struct AtomicDigEventSettlement<'a> {
    pub expected: &'a DigEventActorSnapshot,
    pub event_key: &'a str,
    pub event_id: &'a str,
    pub choice: &'a str,
    pub depth_after: i64,
    pub streak_days_after: i64,
    pub buff_mutation: EventJsonMutation<'a>,
    pub curse_mutation: EventJsonMutation<'a>,
    pub balance_delta: i64,
    pub reward: Option<DigEventReward<'a>>,
    pub inventory_capacity: usize,
    pub detail_json: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigEventSettlementReceipt {
    pub action_id: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
    pub reward_row_id: Option<i64>,
    pub applied_now: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DigEventSettlementOutcome {
    Applied(DigEventSettlementReceipt),
    Conflict,
    MissingTunnel,
    MissingPlayer,
}

/// Fail-soft state read by canonical quest eligibility and progression.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DigEventQuestSnapshot {
    pub active_quest_id: Option<String>,
    pub active_quest_step: Option<i64>,
    pub completed_quest_ids: BTreeSet<String>,
    pub recent_bet: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigEventQuestMutation<'a> {
    SetActive { quest_id: &'a str, next_step: i64 },
    Complete { quest_id: &'a str },
}

/// Quest transition receipt. Duplicate transitions return the current state
/// without appending the completed quest twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigEventQuestReceipt {
    pub state: DigEventQuestSnapshot,
    pub applied_now: bool,
}

/// One fail-soft guild modifier configured by a successful event/finale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigEventGuildModifierReceipt {
    pub expires_at: i64,
    pub applied_now: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DigEventGuildModifierRequest<'a> {
    pub guild_id: Option<i64>,
    pub actor_id: i64,
    pub modifier_id: &'a str,
    pub duration_seconds: i64,
    pub payload_json: &'a str,
    pub event_key: &'a str,
    pub now: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct DigEventFinaleJcRequest<'a> {
    pub key: DigEventActorKey,
    pub quest_id: &'a str,
    pub event_key: &'a str,
    pub gross_jc: i64,
    pub net_jc: i64,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct DigEventFinaleRelicRequest<'a> {
    pub key: DigEventActorKey,
    pub quest_id: &'a str,
    pub event_key: &'a str,
    pub artifact_id: &'a str,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigEventFinaleReceipt {
    pub balance_after: Option<i64>,
    pub reward_row_id: Option<i64>,
    pub action_id: i64,
    pub applied_now: bool,
}

/// One cooperative splash grant. Each victim is committed independently so
/// an unavailable target cannot roll back already-settled targets.
#[derive(Clone, Copy, Debug)]
pub struct DigSplashGrantRequest<'a> {
    pub guild_id: Option<i64>,
    pub digger_id: i64,
    pub victim_id: i64,
    pub event_name: &'a str,
    pub strategy: &'a str,
    pub event_key: &'a str,
    pub amount: i64,
    pub gross_jc: i64,
    pub detail_json: &'a str,
    pub created_at: i64,
}

/// Audit rows appended after a protected burn/steal settlement. The hostile
/// wallet movement remains the authority and owns its own transaction.
#[derive(Clone, Copy, Debug)]
pub struct DigHostileSplashAuditRequest<'a> {
    pub guild_id: Option<i64>,
    pub digger_id: i64,
    pub victim_id: i64,
    pub event_name: &'a str,
    pub strategy: &'a str,
    pub mode: &'a str,
    pub event_key: &'a str,
    pub amount: i64,
    pub detail_json: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigSplashVictimReceipt {
    pub amount: i64,
    pub victim_balance_after: Option<i64>,
    pub victim_action_id: i64,
    pub digger_action_id: Option<i64>,
    pub applied_now: bool,
}

#[derive(Debug, Error)]
pub enum DigEventRuntimeRepositoryError {
    #[error("event key cannot be empty")]
    EmptyEventKey,
    #[error("event id cannot be empty")]
    EmptyEventId,
    #[error("event detail must be a JSON object")]
    InvalidDetail,
    #[error("event JSON tunnel mutation is invalid: {0}")]
    InvalidTunnelJson(#[source] serde_json::Error),
    #[error("event reward identifier cannot be empty")]
    EmptyRewardId,
    #[error("event reward has invalid numeric fields")]
    InvalidReward,
    #[error("splash amount must be positive")]
    InvalidSplashAmount,
    #[error("splash mode must be burn or steal")]
    InvalidSplashMode,
    #[error("splash victim is not registered")]
    MissingSplashVictim,
    #[error("quest id cannot be empty")]
    EmptyQuestId,
    #[error("quest step must be positive")]
    InvalidQuestStep,
    #[error("a different quest is already active")]
    ActiveQuestConflict,
    #[error("guild modifier id cannot be empty")]
    EmptyModifierId,
    #[error("guild modifier duration cannot be negative")]
    InvalidModifierDuration,
    #[error("quest finale JC must be positive")]
    InvalidFinaleJc,
    #[error("quest finale player is not registered")]
    MissingFinalePlayer,
    #[error("event settlement arithmetic exceeded SQLite's signed 64-bit range")]
    ArithmeticOverflow,
    #[error("event key was already committed with a different settlement payload")]
    IdempotencyConflict,
    #[error("persisted event receipt is malformed")]
    MalformedReceipt,
    #[error("Dig event runtime SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Dig event runtime JSON operation failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct DigEventRuntimeRepository {
    path: PathBuf,
}

impl DigEventRuntimeRepository {
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

    /// Read all actor state used by event policy and reward admission.
    pub fn actor_snapshot(
        &self,
        key: DigEventActorKey,
    ) -> Result<Option<DigEventActorSnapshot>, DigEventRuntimeRepositoryError> {
        let connection = self.connection()?;
        query_actor_snapshot(
            &connection,
            key,
            DigEventRewardQueryPlan {
                owned_gear: true,
                inventory_count: true,
                owned_artifacts: true,
            },
        )
        .map_err(Into::into)
    }

    /// Read the actor fields used by event policy without loading any reward
    /// ownership collections. Reward reads happen only after the selected
    /// outcome is known through [`Self::reward_snapshot`].
    pub fn actor_snapshot_for_event(
        &self,
        key: DigEventActorKey,
    ) -> Result<Option<DigEventActorSnapshot>, DigEventRuntimeRepositoryError> {
        let connection = self.connection()?;
        query_actor_snapshot(&connection, key, DigEventRewardQueryPlan::default())
            .map_err(Into::into)
    }

    /// Read only the ownership/capacity columns required by one selected
    /// reward branch. Each requested collection is one set query; artifact
    /// admission deliberately uses this single owned-artifact query rather
    /// than probing one candidate id at a time.
    pub fn reward_snapshot(
        &self,
        key: DigEventActorKey,
        plan: DigEventRewardQueryPlan,
    ) -> Result<DigEventRewardSnapshot, DigEventRuntimeRepositoryError> {
        let connection = self.connection()?;
        query_reward_snapshot(&connection, key, plan).map_err(Into::into)
    }

    /// Resolve the event attached to one committed Dig action.
    ///
    /// Only `dig` and `paid_dig` rows are admitted. The actor/guild guard is
    /// part of the query so a component copied to another user or server is
    /// indistinguishable from an expired interaction.
    pub fn pending_event(
        &self,
        action_id: i64,
        actor_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<DigPendingEvent>, DigEventRuntimeRepositoryError> {
        let guild_id = Self::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, actor_id, guild_id,
                        json_extract(detail, '$.event'), created_at
                   FROM dig_actions
                  WHERE id = ?1 AND actor_id = ?2 AND guild_id = ?3
                    AND action_type IN ('dig', 'paid_dig')
                    AND json_valid(detail)
                    AND json_type(detail, '$.event') = 'text'",
                params![action_id, actor_id, guild_id],
                |row| {
                    Ok(DigPendingEvent {
                        action_id: row.get(0)?,
                        actor_id: row.get(1)?,
                        guild_id: row.get(2)?,
                        event_id: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Read quest state plus the only cross-system starter predicate used by
    /// the canonical catalog (`bet_within_7d`). Missing quest rows are the
    /// default idle state, just like Python.
    pub fn quest_snapshot(
        &self,
        key: DigEventActorKey,
        now: i64,
    ) -> Result<DigEventQuestSnapshot, DigEventRuntimeRepositoryError> {
        let connection = self.connection()?;
        query_quest_snapshot(&connection, key, now).map_err(Into::into)
    }

    /// Persist one quest transition in its own fail-soft transaction. This is
    /// intentionally separate from actor event settlement to preserve
    /// Python's visible ordering and partial-failure boundary.
    pub fn apply_quest_mutation(
        &self,
        key: DigEventActorKey,
        mutation: DigEventQuestMutation<'_>,
        now: i64,
    ) -> Result<DigEventQuestReceipt, DigEventRuntimeRepositoryError> {
        validate_quest_mutation(mutation)?;
        let guild_id = Self::normalize_guild_id(key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = query_quest_snapshot(&transaction, key, now)?;
        let applied_now = match mutation {
            DigEventQuestMutation::SetActive {
                quest_id,
                next_step,
            } => {
                if current.completed_quest_ids.contains(quest_id)
                    || current.active_quest_id.as_deref() == Some(quest_id)
                        && current.active_quest_step == Some(next_step)
                {
                    false
                } else {
                    if current
                        .active_quest_id
                        .as_deref()
                        .is_some_and(|active| active != quest_id)
                    {
                        return Err(DigEventRuntimeRepositoryError::ActiveQuestConflict);
                    }
                    transaction.execute(
                        "INSERT INTO dig_quests (
                             discord_id, guild_id, active_quest_id,
                             active_quest_step, completed_quests, last_updated_at
                         ) VALUES (?1, ?2, ?3, ?4, '[]', ?5)
                         ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                             active_quest_id=excluded.active_quest_id,
                             active_quest_step=excluded.active_quest_step,
                             last_updated_at=excluded.last_updated_at",
                        params![key.discord_id, guild_id, quest_id, next_step, now],
                    )?;
                    true
                }
            }
            DigEventQuestMutation::Complete { quest_id } => {
                if current.completed_quest_ids.contains(quest_id) {
                    false
                } else {
                    let mut completed = current.completed_quest_ids.clone();
                    completed.insert(quest_id.to_owned());
                    let completed_json = serde_json::to_string(&completed)?;
                    transaction.execute(
                        "INSERT INTO dig_quests (
                             discord_id, guild_id, active_quest_id,
                             active_quest_step, completed_quests, last_updated_at
                         ) VALUES (?1, ?2, NULL, NULL, ?3, ?4)
                         ON CONFLICT(discord_id, guild_id) DO UPDATE SET
                             active_quest_id=NULL, active_quest_step=NULL,
                             completed_quests=excluded.completed_quests,
                             last_updated_at=excluded.last_updated_at",
                        params![key.discord_id, guild_id, completed_json, now],
                    )?;
                    true
                }
            }
        };
        transaction.commit()?;
        Ok(DigEventQuestReceipt {
            state: self.quest_snapshot(key, now)?,
            applied_now,
        })
    }

    /// Set or extend one event guild modifier. Re-triggering an active row
    /// extends from its current expiry; an expired row restarts from `now`.
    pub fn set_guild_modifier(
        &self,
        request: DigEventGuildModifierRequest<'_>,
    ) -> Result<DigEventGuildModifierReceipt, DigEventRuntimeRepositoryError> {
        if request.modifier_id.trim().is_empty() {
            return Err(DigEventRuntimeRepositoryError::EmptyModifierId);
        }
        if request.duration_seconds < 0 {
            return Err(DigEventRuntimeRepositoryError::InvalidModifierDuration);
        }
        if request.event_key.trim().is_empty() {
            return Err(DigEventRuntimeRepositoryError::EmptyEventKey);
        }
        if serde_json::from_str::<JsonValue>(request.payload_json)?
            .as_object()
            .is_none()
        {
            return Err(DigEventRuntimeRepositoryError::InvalidDetail);
        }
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((expires_at, modifier_id)) = transaction
            .query_row(
                "SELECT json_extract(detail, '$.expires_at'),
                        json_extract(detail, '$.modifier_id')
                   FROM dig_actions
                  WHERE guild_id=?1 AND actor_id=?2
                    AND action_type='event_modifier' AND json_valid(detail)
                    AND json_extract(detail, '$.event_key')=?3
                  ORDER BY id LIMIT 1",
                params![guild_id, request.actor_id, request.event_key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if modifier_id != request.modifier_id {
                return Err(DigEventRuntimeRepositoryError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(DigEventGuildModifierReceipt {
                expires_at,
                applied_now: false,
            });
        }
        let existing = transaction
            .query_row(
                "SELECT expires_at, payload_json FROM dig_guild_modifiers
                  WHERE guild_id=?1 AND modifier_id=?2",
                params![guild_id, request.modifier_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let base = existing.as_ref().map_or(request.now, |(expires_at, _)| {
            (*expires_at).max(request.now)
        });
        let expires_at = base
            .checked_add(request.duration_seconds)
            .ok_or(DigEventRuntimeRepositoryError::ArithmeticOverflow)?;
        transaction.execute(
            "INSERT INTO dig_guild_modifiers (
                 guild_id, modifier_id, expires_at, payload_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(guild_id, modifier_id) DO UPDATE SET
                 expires_at=excluded.expires_at,
                 payload_json=excluded.payload_json",
            params![
                guild_id,
                request.modifier_id,
                expires_at,
                request.payload_json,
                request.now
            ],
        )?;
        let detail = json!({
            "event_key": request.event_key,
            "modifier_id": request.modifier_id,
            "duration_seconds": request.duration_seconds,
            "expires_at": expires_at,
        })
        .to_string();
        insert_splash_action(
            &transaction,
            guild_id,
            request.actor_id,
            None,
            "event_modifier",
            0,
            &detail,
            request.now,
        )?;
        transaction.commit()?;
        Ok(DigEventGuildModifierReceipt {
            expires_at,
            applied_now: true,
        })
    }

    /// Credit a quest-finale reward and append its ledger/audit row in one
    /// follow-up transaction. Quest completion intentionally precedes this
    /// call, matching Python's complete-first, reward-second boundary.
    pub fn settle_finale_jc_atomic(
        &self,
        request: DigEventFinaleJcRequest<'_>,
    ) -> Result<DigEventFinaleReceipt, DigEventRuntimeRepositoryError> {
        validate_finale_common(request.quest_id, request.event_key, request.detail_json)?;
        if request.gross_jc <= 0 || request.net_jc <= 0 {
            return Err(DigEventRuntimeRepositoryError::InvalidFinaleJc);
        }
        let guild_id = Self::normalize_guild_id(request.key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = duplicate_finale_receipt(
            &transaction,
            guild_id,
            request.key.discord_id,
            request.event_key,
            "quest_finale_jc",
        )? {
            transaction.commit()?;
            return Ok(receipt);
        }
        let balance_before = query_balance(&transaction, request.key.discord_id, guild_id)?
            .ok_or(DigEventRuntimeRepositoryError::MissingFinalePlayer)?;
        let balance_after = balance_before
            .checked_add(request.net_jc)
            .ok_or(DigEventRuntimeRepositoryError::ArithmeticOverflow)?;
        let detail = finale_detail(
            request.detail_json,
            request.event_key,
            request.quest_id,
            Some(request.gross_jc),
            Some(request.net_jc),
            Some(balance_after),
            None,
        )?;
        set_finale_ledger_context(
            &transaction,
            request.key.discord_id,
            request.quest_id,
            &detail,
        )?;
        transaction.execute(
            "UPDATE players SET jopacoin_balance=?1, updated_at=CURRENT_TIMESTAMP
              WHERE discord_id=?2 AND guild_id=?3",
            params![balance_after, request.key.discord_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        let action_id = insert_splash_action(
            &transaction,
            guild_id,
            request.key.discord_id,
            None,
            "quest_finale_jc",
            request.net_jc,
            &detail,
            request.now,
        )?;
        transaction.commit()?;
        Ok(DigEventFinaleReceipt {
            balance_after: Some(balance_after),
            reward_row_id: None,
            action_id,
            applied_now: true,
        })
    }

    /// Grant a quest-final relic after durable quest completion. The audit
    /// marker makes component retries return the original inserted row.
    pub fn grant_finale_relic_atomic(
        &self,
        request: DigEventFinaleRelicRequest<'_>,
    ) -> Result<DigEventFinaleReceipt, DigEventRuntimeRepositoryError> {
        validate_finale_common(request.quest_id, request.event_key, request.detail_json)?;
        if request.artifact_id.trim().is_empty() {
            return Err(DigEventRuntimeRepositoryError::EmptyRewardId);
        }
        let guild_id = Self::normalize_guild_id(request.key.guild_id);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = duplicate_finale_receipt(
            &transaction,
            guild_id,
            request.key.discord_id,
            request.event_key,
            "quest_finale_relic",
        )? {
            transaction.commit()?;
            return Ok(receipt);
        }
        transaction.execute(
            "INSERT INTO dig_artifacts (
                 discord_id,guild_id,artifact_id,found_at,is_relic,equipped
             ) VALUES (?1,?2,?3,?4,1,0)",
            params![
                request.key.discord_id,
                guild_id,
                request.artifact_id,
                request.now
            ],
        )?;
        let reward_row_id = transaction.last_insert_rowid();
        let detail = finale_detail(
            request.detail_json,
            request.event_key,
            request.quest_id,
            None,
            None,
            None,
            Some(reward_row_id),
        )?;
        let action_id = insert_splash_action(
            &transaction,
            guild_id,
            request.key.discord_id,
            None,
            "quest_finale_relic",
            0,
            &detail,
            request.now,
        )?;
        transaction.commit()?;
        Ok(DigEventFinaleReceipt {
            balance_after: None,
            reward_row_id: Some(reward_row_id),
            action_id,
            applied_now: true,
        })
    }

    /// Commit one event actor outcome with optimistic state admission and a
    /// durable event key. Duplicate delivery returns the original receipt
    /// before checking the now-advanced snapshot.
    pub fn settle_actor_atomic(
        &self,
        request: AtomicDigEventSettlement<'_>,
    ) -> Result<DigEventSettlementOutcome, DigEventRuntimeRepositoryError> {
        self.settle_actor_atomic_inner(request, true)
    }

    /// Settle an event whose policy snapshot intentionally omitted reward
    /// ownership collections. The transaction still rechecks all tunnel,
    /// equipped-loadout, and wallet fields, while reward admission uses the
    /// collections already loaded for this exact event branch. This keeps a
    /// plain event free of reward-table reads and keeps an artifact branch to
    /// one owned-artifact query.
    pub fn settle_actor_atomic_for_event(
        &self,
        request: AtomicDigEventSettlement<'_>,
    ) -> Result<DigEventSettlementOutcome, DigEventRuntimeRepositoryError> {
        self.settle_actor_atomic_inner(request, false)
    }

    fn settle_actor_atomic_inner(
        &self,
        request: AtomicDigEventSettlement<'_>,
        compare_reward_snapshot: bool,
    ) -> Result<DigEventSettlementOutcome, DigEventRuntimeRepositoryError> {
        validate_request(request)?;
        let buff_after = apply_json_mutation(
            request.expected.temp_buff_json.as_deref(),
            request.buff_mutation,
        )?;
        let curse_after = apply_json_mutation(
            request.expected.temp_curse_json.as_deref(),
            request.curse_mutation,
        )?;
        let balance_after = request
            .expected
            .balance
            .checked_add(request.balance_delta)
            .ok_or(DigEventRuntimeRepositoryError::ArithmeticOverflow)?;
        let fingerprint = settlement_fingerprint(
            request,
            buff_after.as_deref(),
            curse_after.as_deref(),
            balance_after,
        );
        let guild_id = Self::normalize_guild_id(request.expected.key.guild_id);

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = duplicate_receipt(
            &transaction,
            request.expected.key.discord_id,
            guild_id,
            request.event_key,
            request.event_id,
            request.choice,
        )? {
            transaction.commit()?;
            return Ok(DigEventSettlementOutcome::Applied(receipt));
        }

        let current = query_actor_snapshot(
            &transaction,
            request.expected.key,
            if compare_reward_snapshot {
                DigEventRewardQueryPlan {
                    owned_gear: true,
                    inventory_count: true,
                    owned_artifacts: true,
                }
            } else {
                DigEventRewardQueryPlan::default()
            },
        )?;
        let Some(current) = current else {
            let tunnel_exists =
                tunnel_exists(&transaction, request.expected.key.discord_id, guild_id)?;
            let player_exists =
                player_exists(&transaction, request.expected.key.discord_id, guild_id)?;
            transaction.rollback()?;
            return Ok(if !tunnel_exists {
                DigEventSettlementOutcome::MissingTunnel
            } else if !player_exists {
                DigEventSettlementOutcome::MissingPlayer
            } else {
                DigEventSettlementOutcome::Conflict
            });
        };
        let matches = if compare_reward_snapshot {
            current == *request.expected
        } else {
            event_policy_snapshot_matches(&current, request.expected)
        };
        if !matches {
            transaction.rollback()?;
            return Ok(DigEventSettlementOutcome::Conflict);
        }

        let reward_row_id = insert_reward(&transaction, request)?;
        let detail = settlement_detail(request, &fingerprint, balance_after, reward_row_id)?;

        if request.balance_delta != 0 {
            set_ledger_context(&transaction, request, &detail)?;
            let changed = transaction.execute(
                "UPDATE players
                    SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
                  WHERE discord_id = ?2 AND guild_id = ?3
                    AND COALESCE(jopacoin_balance, 0) = ?4",
                params![
                    balance_after,
                    request.expected.key.discord_id,
                    guild_id,
                    request.expected.balance,
                ],
            )?;
            if changed != 1 {
                transaction.rollback()?;
                return Ok(DigEventSettlementOutcome::Conflict);
            }
            clear_ledger_context(&transaction)?;
        }

        let tunnel_changed = transaction.execute(
            "UPDATE tunnels
                SET depth = ?1, streak_days = ?2, temp_buffs = ?3,
                    temp_curses = ?4
              WHERE discord_id = ?5 AND guild_id = ?6",
            params![
                request.depth_after,
                request.streak_days_after,
                buff_after,
                curse_after,
                request.expected.key.discord_id,
                guild_id,
            ],
        )?;
        if tunnel_changed != 1 {
            transaction.rollback()?;
            return Ok(DigEventSettlementOutcome::Conflict);
        }

        transaction.execute(
            "INSERT INTO dig_actions (
                 guild_id, actor_id, target_id, action_type, depth_before,
                 depth_after, jc_delta, detail, created_at
             ) VALUES (?1, ?2, NULL, 'event', ?3, ?4, ?5, ?6, ?7)",
            params![
                guild_id,
                request.expected.key.discord_id,
                request.expected.depth,
                request.depth_after,
                request.balance_delta,
                detail,
                request.created_at,
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(DigEventSettlementOutcome::Applied(
            DigEventSettlementReceipt {
                action_id,
                balance_before: request.expected.balance,
                balance_after,
                depth_before: request.expected.depth,
                depth_after: request.depth_after,
                jc_delta: request.balance_delta,
                reward_row_id,
                applied_now: true,
            },
        ))
    }

    /// Credit one grant-mode splash target and append its Dig audit in one
    /// victim-scoped transaction. Repeating `event_key` is idempotent.
    pub fn settle_splash_grant_atomic(
        &self,
        request: DigSplashGrantRequest<'_>,
    ) -> Result<DigSplashVictimReceipt, DigEventRuntimeRepositoryError> {
        validate_splash_common(request.event_key, request.amount, request.detail_json)?;
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let detail = splash_detail(
            request.detail_json,
            request.event_key,
            request.event_name,
            request.strategy,
            "grant",
            request.digger_id,
            request.gross_jc,
        )?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(action_id) = find_splash_action(
            &transaction,
            guild_id,
            request.victim_id,
            "splash_victim",
            request.event_key,
        )? {
            let balance = query_balance(&transaction, request.victim_id, guild_id)?;
            transaction.commit()?;
            return Ok(DigSplashVictimReceipt {
                amount: request.amount,
                victim_balance_after: balance,
                victim_action_id: action_id,
                digger_action_id: None,
                applied_now: false,
            });
        }
        let balance_before = query_balance(&transaction, request.victim_id, guild_id)?
            .ok_or(DigEventRuntimeRepositoryError::MissingSplashVictim)?;
        let balance_after = balance_before
            .checked_add(request.amount)
            .ok_or(DigEventRuntimeRepositoryError::ArithmeticOverflow)?;
        set_splash_ledger_context(
            &transaction,
            request.digger_id,
            request.event_name,
            "dig splash victim grant",
            &detail,
        )?;
        transaction.execute(
            "UPDATE players
                SET jopacoin_balance = ?1, updated_at = CURRENT_TIMESTAMP
              WHERE discord_id = ?2 AND guild_id = ?3",
            params![balance_after, request.victim_id, guild_id],
        )?;
        clear_ledger_context(&transaction)?;
        let victim_action_id = insert_splash_action(
            &transaction,
            guild_id,
            request.victim_id,
            None,
            "splash_victim",
            request.amount,
            &detail,
            request.created_at,
        )?;
        transaction.commit()?;
        Ok(DigSplashVictimReceipt {
            amount: request.amount,
            victim_balance_after: Some(balance_after),
            victim_action_id,
            digger_action_id: None,
            applied_now: true,
        })
    }

    /// Append the canonical Dig action history after a separately committed
    /// protected hostile loss. Burn creates a victim row; steal creates the
    /// victim debit and digger credit together. Replays are idempotent.
    pub fn record_hostile_splash_audit(
        &self,
        request: DigHostileSplashAuditRequest<'_>,
    ) -> Result<DigSplashVictimReceipt, DigEventRuntimeRepositoryError> {
        validate_splash_common(request.event_key, request.amount, request.detail_json)?;
        if !matches!(request.mode, "burn" | "steal") {
            return Err(DigEventRuntimeRepositoryError::InvalidSplashMode);
        }
        let guild_id = Self::normalize_guild_id(request.guild_id);
        let detail = splash_detail(
            request.detail_json,
            request.event_key,
            request.event_name,
            request.strategy,
            request.mode,
            request.digger_id,
            0,
        )?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(action_id) = find_splash_action(
            &transaction,
            guild_id,
            request.victim_id,
            "splash_victim",
            request.event_key,
        )? {
            let digger_action_id = if request.mode == "steal" {
                find_splash_action(
                    &transaction,
                    guild_id,
                    request.digger_id,
                    "splash_thief",
                    request.event_key,
                )?
            } else {
                None
            };
            transaction.commit()?;
            return Ok(DigSplashVictimReceipt {
                amount: request.amount,
                victim_balance_after: None,
                victim_action_id: action_id,
                digger_action_id,
                applied_now: false,
            });
        }
        let victim_action_id = insert_splash_action(
            &transaction,
            guild_id,
            request.victim_id,
            None,
            "splash_victim",
            -request.amount,
            &detail,
            request.created_at,
        )?;
        let digger_action_id = if request.mode == "steal" {
            Some(insert_splash_action(
                &transaction,
                guild_id,
                request.digger_id,
                Some(request.victim_id),
                "splash_thief",
                request.amount,
                &detail,
                request.created_at,
            )?)
        } else {
            None
        };
        transaction.commit()?;
        Ok(DigSplashVictimReceipt {
            amount: request.amount,
            victim_balance_after: None,
            victim_action_id,
            digger_action_id,
            applied_now: true,
        })
    }
}

fn validate_splash_common(
    event_key: &str,
    amount: i64,
    detail_json: &str,
) -> Result<(), DigEventRuntimeRepositoryError> {
    if event_key.trim().is_empty() {
        return Err(DigEventRuntimeRepositoryError::EmptyEventKey);
    }
    if amount <= 0 {
        return Err(DigEventRuntimeRepositoryError::InvalidSplashAmount);
    }
    if serde_json::from_str::<JsonValue>(detail_json)?
        .as_object()
        .is_none()
    {
        return Err(DigEventRuntimeRepositoryError::InvalidDetail);
    }
    Ok(())
}

fn validate_finale_common(
    quest_id: &str,
    event_key: &str,
    detail_json: &str,
) -> Result<(), DigEventRuntimeRepositoryError> {
    if quest_id.trim().is_empty() {
        return Err(DigEventRuntimeRepositoryError::EmptyQuestId);
    }
    if event_key.trim().is_empty() {
        return Err(DigEventRuntimeRepositoryError::EmptyEventKey);
    }
    if serde_json::from_str::<JsonValue>(detail_json)?
        .as_object()
        .is_none()
    {
        return Err(DigEventRuntimeRepositoryError::InvalidDetail);
    }
    Ok(())
}

fn finale_detail(
    detail_json: &str,
    event_key: &str,
    quest_id: &str,
    gross_jc: Option<i64>,
    net_jc: Option<i64>,
    balance_after: Option<i64>,
    reward_row_id: Option<i64>,
) -> Result<String, DigEventRuntimeRepositoryError> {
    let JsonValue::Object(mut detail) = serde_json::from_str(detail_json)? else {
        return Err(DigEventRuntimeRepositoryError::InvalidDetail);
    };
    insert_detail(&mut detail, "event_key", event_key);
    insert_detail(&mut detail, "quest_id", quest_id);
    if let Some(gross_jc) = gross_jc {
        detail.insert("gross_jc".to_owned(), JsonValue::from(gross_jc));
    }
    if let Some(net_jc) = net_jc {
        detail.insert("net_jc".to_owned(), JsonValue::from(net_jc));
    }
    if let Some(balance_after) = balance_after {
        detail.insert("balance_after".to_owned(), JsonValue::from(balance_after));
    }
    if let Some(reward_row_id) = reward_row_id {
        detail.insert("reward_row_id".to_owned(), JsonValue::from(reward_row_id));
    }
    Ok(JsonValue::Object(detail).to_string())
}

fn duplicate_finale_receipt(
    transaction: &Transaction<'_>,
    guild_id: i64,
    actor_id: i64,
    event_key: &str,
    action_type: &str,
) -> Result<Option<DigEventFinaleReceipt>, DigEventRuntimeRepositoryError> {
    let row = transaction
        .query_row(
            "SELECT id, detail FROM dig_actions
              WHERE guild_id=?1 AND actor_id=?2 AND action_type=?3
                AND json_valid(detail)
                AND json_extract(detail, '$.event_key')=?4
              ORDER BY id LIMIT 1",
            params![guild_id, actor_id, action_type, event_key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((action_id, detail_json)) = row else {
        return Ok(None);
    };
    let detail = serde_json::from_str::<JsonValue>(&detail_json)?;
    let balance_after = detail.get("balance_after").and_then(JsonValue::as_i64);
    Ok(Some(DigEventFinaleReceipt {
        balance_after,
        reward_row_id: detail.get("reward_row_id").and_then(JsonValue::as_i64),
        action_id,
        applied_now: false,
    }))
}

fn set_finale_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    quest_id: &str,
    detail: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id,source,actor_id,related_type,related_id,reason,metadata
         ) VALUES (1,'dig',?1,'quest_finale',?2,
                   'dig quest finale credit',?3)",
        params![actor_id, quest_id, detail],
    )?;
    Ok(())
}

fn splash_detail(
    detail_json: &str,
    event_key: &str,
    event_name: &str,
    strategy: &str,
    mode: &str,
    digger_id: i64,
    gross_jc: i64,
) -> Result<String, DigEventRuntimeRepositoryError> {
    let JsonValue::Object(mut detail) = serde_json::from_str(detail_json)? else {
        return Err(DigEventRuntimeRepositoryError::InvalidDetail);
    };
    insert_detail(&mut detail, "event_key", event_key);
    insert_detail(&mut detail, "event_name", event_name);
    insert_detail(&mut detail, "strategy", strategy);
    insert_detail(&mut detail, "mode", mode);
    detail.insert("digger_id".to_owned(), JsonValue::from(digger_id));
    if gross_jc > 0 {
        detail.insert("gross_jc".to_owned(), JsonValue::from(gross_jc));
    }
    Ok(JsonValue::Object(detail).to_string())
}

fn query_balance(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT COALESCE(jopacoin_balance, 0) FROM players
              WHERE discord_id = ?1 AND guild_id = ?2",
            params![discord_id, guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn find_splash_action(
    connection: &Connection,
    guild_id: i64,
    actor_id: i64,
    action_type: &str,
    event_key: &str,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT id FROM dig_actions
              WHERE guild_id = ?1 AND actor_id = ?2 AND action_type = ?3
                AND json_valid(detail)
                AND json_extract(detail, '$.event_key') = ?4
              ORDER BY id LIMIT 1",
            params![guild_id, actor_id, action_type, event_key],
            |row| row.get(0),
        )
        .optional()
}

#[allow(clippy::too_many_arguments)]
fn insert_splash_action(
    transaction: &Transaction<'_>,
    guild_id: i64,
    actor_id: i64,
    target_id: Option<i64>,
    action_type: &str,
    jc_delta: i64,
    detail: &str,
    created_at: i64,
) -> Result<i64, rusqlite::Error> {
    transaction.execute(
        "INSERT INTO dig_actions (
             guild_id, actor_id, target_id, action_type, depth_before,
             depth_after, jc_delta, detail, created_at
         ) VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, ?6, ?7)",
        params![
            guild_id,
            actor_id,
            target_id,
            action_type,
            jc_delta,
            detail,
            created_at,
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn set_splash_ledger_context(
    transaction: &Transaction<'_>,
    digger_id: i64,
    event_name: &str,
    reason: &str,
    detail: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'dig', ?1, 'splash_victim', ?2, ?3, ?4)",
        params![digger_id, event_name, reason, detail],
    )?;
    Ok(())
}

fn validate_request(
    request: AtomicDigEventSettlement<'_>,
) -> Result<(), DigEventRuntimeRepositoryError> {
    if request.event_key.trim().is_empty() {
        return Err(DigEventRuntimeRepositoryError::EmptyEventKey);
    }
    if request.event_id.trim().is_empty() {
        return Err(DigEventRuntimeRepositoryError::EmptyEventId);
    }
    if request
        .detail_json
        .parse::<JsonValue>()?
        .as_object()
        .is_none()
    {
        return Err(DigEventRuntimeRepositoryError::InvalidDetail);
    }
    if let Some(reward) = request.reward {
        match reward {
            DigEventReward::Gear {
                item_id,
                slot,
                tier,
                durability,
                source,
            } => {
                if item_id.trim().is_empty() || slot.trim().is_empty() || source.trim().is_empty() {
                    return Err(DigEventRuntimeRepositoryError::EmptyRewardId);
                }
                if tier < 0 || durability < 0 {
                    return Err(DigEventRuntimeRepositoryError::InvalidReward);
                }
            }
            DigEventReward::Consumable { item_type } => {
                if item_type.trim().is_empty() {
                    return Err(DigEventRuntimeRepositoryError::EmptyRewardId);
                }
            }
            DigEventReward::Artifact { artifact_id, .. } => {
                if artifact_id.trim().is_empty() {
                    return Err(DigEventRuntimeRepositoryError::EmptyRewardId);
                }
            }
        }
    }
    Ok(())
}

fn apply_json_mutation(
    current: Option<&str>,
    mutation: EventJsonMutation<'_>,
) -> Result<Option<String>, DigEventRuntimeRepositoryError> {
    match mutation {
        EventJsonMutation::Preserve => Ok(current.map(ToOwned::to_owned)),
        EventJsonMutation::Clear => Ok(None),
        EventJsonMutation::Replace(value) => {
            serde_json::from_str::<JsonValue>(value)
                .map_err(DigEventRuntimeRepositoryError::InvalidTunnelJson)?;
            Ok(Some(value.to_owned()))
        }
    }
}

fn settlement_fingerprint(
    request: AtomicDigEventSettlement<'_>,
    buff_after: Option<&str>,
    curse_after: Option<&str>,
    balance_after: i64,
) -> String {
    json!({
        "event_id": request.event_id,
        "choice": request.choice,
        "depth_before": request.expected.depth,
        "depth_after": request.depth_after,
        "streak_before": request.expected.streak_days,
        "streak_after": request.streak_days_after,
        "buff_before": request.expected.temp_buff_json,
        "buff_after": buff_after,
        "curse_before": request.expected.temp_curse_json,
        "curse_after": curse_after,
        "balance_before": request.expected.balance,
        "balance_after": balance_after,
        "reward": reward_json(request.reward),
    })
    .to_string()
}

fn reward_json(reward: Option<DigEventReward<'_>>) -> JsonValue {
    match reward {
        None => JsonValue::Null,
        Some(DigEventReward::Gear {
            item_id,
            slot,
            tier,
            durability,
            source,
        }) => json!({
            "kind": "gear",
            "item_id": item_id,
            "slot": slot,
            "tier": tier,
            "durability": durability,
            "source": source,
        }),
        Some(DigEventReward::Consumable { item_type }) => {
            json!({"kind": "consumable", "item_type": item_type})
        }
        Some(DigEventReward::Artifact {
            artifact_id,
            is_relic,
        }) => json!({
            "kind": "artifact",
            "artifact_id": artifact_id,
            "is_relic": is_relic,
        }),
    }
}

fn settlement_detail(
    request: AtomicDigEventSettlement<'_>,
    fingerprint: &str,
    balance_after: i64,
    reward_row_id: Option<i64>,
) -> Result<String, DigEventRuntimeRepositoryError> {
    let JsonValue::Object(mut detail) = serde_json::from_str(request.detail_json)? else {
        return Err(DigEventRuntimeRepositoryError::InvalidDetail);
    };
    insert_detail(&mut detail, "event_key", request.event_key);
    insert_detail(&mut detail, "event_id", request.event_id);
    insert_detail(&mut detail, "choice", request.choice);
    insert_detail(&mut detail, "settlement_fingerprint", fingerprint);
    detail.insert(
        "balance_before".to_owned(),
        JsonValue::from(request.expected.balance),
    );
    detail.insert("balance_after".to_owned(), JsonValue::from(balance_after));
    detail.insert(
        "reward_row_id".to_owned(),
        reward_row_id.map_or(JsonValue::Null, JsonValue::from),
    );
    Ok(JsonValue::Object(detail).to_string())
}

fn insert_detail(detail: &mut JsonMap<String, JsonValue>, key: &str, value: &str) {
    detail.insert(key.to_owned(), JsonValue::String(value.to_owned()));
}

fn duplicate_receipt(
    transaction: &Transaction<'_>,
    discord_id: i64,
    guild_id: i64,
    event_key: &str,
    event_id: &str,
    choice: &str,
) -> Result<Option<DigEventSettlementReceipt>, DigEventRuntimeRepositoryError> {
    type ReceiptRow = (i64, i64, i64, i64, String);
    let row: Option<ReceiptRow> = transaction
        .query_row(
            "SELECT id, depth_before, depth_after, jc_delta, detail
               FROM dig_actions
              WHERE guild_id = ?1 AND actor_id = ?2 AND action_type = 'event'
                AND json_valid(detail)
                AND json_extract(detail, '$.event_key') = ?3
              ORDER BY id LIMIT 1",
            params![guild_id, discord_id, event_key],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((action_id, depth_before, depth_after, jc_delta, detail_raw)) = row else {
        return Ok(None);
    };
    let detail = serde_json::from_str::<JsonValue>(&detail_raw)?;
    if detail.get("event_id").and_then(JsonValue::as_str) != Some(event_id)
        || detail.get("choice").and_then(JsonValue::as_str) != Some(choice)
    {
        return Err(DigEventRuntimeRepositoryError::IdempotencyConflict);
    }
    let balance_before = detail
        .get("balance_before")
        .and_then(JsonValue::as_i64)
        .ok_or(DigEventRuntimeRepositoryError::MalformedReceipt)?;
    let balance_after = detail
        .get("balance_after")
        .and_then(JsonValue::as_i64)
        .ok_or(DigEventRuntimeRepositoryError::MalformedReceipt)?;
    let reward_row_id = detail.get("reward_row_id").and_then(JsonValue::as_i64);
    Ok(Some(DigEventSettlementReceipt {
        action_id,
        balance_before,
        balance_after,
        depth_before,
        depth_after,
        jc_delta,
        reward_row_id,
        applied_now: false,
    }))
}

fn insert_reward(
    transaction: &Transaction<'_>,
    request: AtomicDigEventSettlement<'_>,
) -> Result<Option<i64>, DigEventRuntimeRepositoryError> {
    let Some(reward) = request.reward else {
        return Ok(None);
    };
    let discord_id = request.expected.key.discord_id;
    let guild_id = DigEventRuntimeRepository::normalize_guild_id(request.expected.key.guild_id);
    match reward {
        DigEventReward::Gear {
            item_id,
            slot,
            tier,
            durability,
            source,
        } => {
            if request.expected.owned_gear.contains(item_id) {
                return Err(DigEventRuntimeRepositoryError::InvalidReward);
            }
            transaction.execute(
                "INSERT INTO dig_gear (
                     discord_id, guild_id, slot, tier, durability, equipped,
                     acquired_at, source, item_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)",
                params![
                    discord_id,
                    guild_id,
                    slot,
                    tier,
                    durability,
                    request.created_at,
                    source,
                    item_id,
                ],
            )?;
        }
        DigEventReward::Consumable { item_type } => {
            if request.expected.inventory_count >= request.inventory_capacity {
                return Err(DigEventRuntimeRepositoryError::InvalidReward);
            }
            transaction.execute(
                "INSERT INTO dig_inventory (
                     discord_id, guild_id, item_type, queued, created_at
                 ) VALUES (?1, ?2, ?3, 0, ?4)",
                params![discord_id, guild_id, item_type, request.created_at],
            )?;
        }
        DigEventReward::Artifact {
            artifact_id,
            is_relic,
        } => {
            if request.expected.owned_artifacts.contains(artifact_id) {
                return Err(DigEventRuntimeRepositoryError::InvalidReward);
            }
            transaction.execute(
                "INSERT INTO dig_artifacts (
                     discord_id, guild_id, artifact_id, found_at, is_relic, equipped
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![
                    discord_id,
                    guild_id,
                    artifact_id,
                    request.created_at,
                    is_relic,
                ],
            )?;
        }
    }
    Ok(Some(transaction.last_insert_rowid()))
}

fn query_actor_snapshot(
    connection: &Connection,
    key: DigEventActorKey,
    reward_plan: DigEventRewardQueryPlan,
) -> Result<Option<DigEventActorSnapshot>, rusqlite::Error> {
    let guild_id = DigEventRuntimeRepository::normalize_guild_id(key.guild_id);
    let inventory_count_projection = if reward_plan.inventory_count {
        "(SELECT COUNT(*) FROM dig_inventory AS i
                      WHERE i.discord_id = t.discord_id AND i.guild_id = t.guild_id)"
    } else {
        "0"
    };
    type ActorRow = (
        i64,
        i64,
        i64,
        String,
        String,
        i64,
        Option<String>,
        Option<String>,
        i64,
        i64,
    );
    let sql = format!(
        "SELECT COALESCE(t.depth, 0), COALESCE(t.luminosity, 100),
                COALESCE(t.prestige_level, 0),
                COALESCE(t.prestige_perks, '[]'),
                COALESCE(t.boss_progress, '{{}}'),
                COALESCE(t.streak_days, 0), t.temp_buffs, t.temp_curses,
                COALESCE(p.jopacoin_balance, 0),
                {inventory_count_projection}
           FROM tunnels AS t
           JOIN players AS p
             ON p.discord_id = t.discord_id AND p.guild_id = t.guild_id
          WHERE t.discord_id = ?1 AND t.guild_id = ?2"
    );
    let row: Option<ActorRow> = connection
        .query_row(&sql, params![key.discord_id, guild_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get::<_, i64>(9)?,
            ))
        })
        .optional()?;
    let Some((
        depth,
        luminosity,
        prestige_level,
        prestige_perks_json,
        boss_progress_json,
        streak_days,
        temp_buff_json,
        temp_curse_json,
        balance,
        inventory_count,
    )) = row
    else {
        return Ok(None);
    };
    let inventory_count = usize::try_from(inventory_count).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    let owned_gear = if reward_plan.owned_gear {
        query_owned_ids(connection, "dig_gear", "item_id", key.discord_id, guild_id)?
    } else {
        BTreeSet::new()
    };
    let owned_artifacts = if reward_plan.owned_artifacts {
        query_owned_ids(
            connection,
            "dig_artifacts",
            "artifact_id",
            key.discord_id,
            guild_id,
        )?
    } else {
        BTreeSet::new()
    };
    Ok(Some(DigEventActorSnapshot {
        key,
        depth,
        luminosity,
        prestige_level,
        prestige_perks_json,
        boss_progress_json,
        streak_days,
        temp_buff_json,
        temp_curse_json,
        balance,
        inventory_count,
        owned_gear,
        equipped_gear: query_equipped_gear(connection, key.discord_id, guild_id)?,
        owned_artifacts,
        equipped_relics: query_equipped_relics(connection, key.discord_id, guild_id)?,
    }))
}

fn query_reward_snapshot(
    connection: &Connection,
    key: DigEventActorKey,
    plan: DigEventRewardQueryPlan,
) -> Result<DigEventRewardSnapshot, rusqlite::Error> {
    let guild_id = DigEventRuntimeRepository::normalize_guild_id(key.guild_id);
    let inventory_count = if plan.inventory_count {
        let count = connection.query_row(
            "SELECT COUNT(*) FROM dig_inventory
              WHERE discord_id = ?1 AND guild_id = ?2",
            params![key.discord_id, guild_id],
            |row| row.get::<_, i64>(0),
        )?;
        usize::try_from(count).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?
    } else {
        0
    };
    let owned_gear = if plan.owned_gear {
        query_owned_ids(connection, "dig_gear", "item_id", key.discord_id, guild_id)?
    } else {
        BTreeSet::new()
    };
    let owned_artifacts = if plan.owned_artifacts {
        query_owned_ids(
            connection,
            "dig_artifacts",
            "artifact_id",
            key.discord_id,
            guild_id,
        )?
    } else {
        BTreeSet::new()
    };
    Ok(DigEventRewardSnapshot {
        inventory_count,
        owned_gear,
        owned_artifacts,
    })
}

fn event_policy_snapshot_matches(
    current: &DigEventActorSnapshot,
    expected: &DigEventActorSnapshot,
) -> bool {
    current.key == expected.key
        && current.depth == expected.depth
        && current.luminosity == expected.luminosity
        && current.prestige_level == expected.prestige_level
        && current.prestige_perks_json == expected.prestige_perks_json
        && current.boss_progress_json == expected.boss_progress_json
        && current.streak_days == expected.streak_days
        && current.temp_buff_json == expected.temp_buff_json
        && current.temp_curse_json == expected.temp_curse_json
        && current.balance == expected.balance
        && current.equipped_gear == expected.equipped_gear
        && current.equipped_relics == expected.equipped_relics
}

fn query_equipped_gear(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<BTreeSet<String>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT item_id FROM dig_gear
          WHERE discord_id = ?1 AND guild_id = ?2
            AND equipped = 1 AND durability > 0 AND item_id IS NOT NULL
          ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| row.get::<_, String>(0))?
        .collect()
}

fn query_equipped_relics(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<BTreeSet<String>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT artifact_id FROM dig_artifacts
          WHERE discord_id = ?1 AND guild_id = ?2
            AND is_relic = 1 AND equipped = 1
          ORDER BY id",
    )?;
    statement
        .query_map(params![discord_id, guild_id], |row| row.get::<_, String>(0))?
        .collect()
}

fn query_quest_snapshot(
    connection: &Connection,
    key: DigEventActorKey,
    now: i64,
) -> Result<DigEventQuestSnapshot, rusqlite::Error> {
    let guild_id = DigEventRuntimeRepository::normalize_guild_id(key.guild_id);
    let state = connection
        .query_row(
            "SELECT active_quest_id, active_quest_step, completed_quests
               FROM dig_quests WHERE discord_id=?1 AND guild_id=?2",
            params![key.discord_id, guild_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let (active_quest_id, active_quest_step, completed_quest_ids) = state.map_or_else(
        || (None, None, BTreeSet::new()),
        |(active, step, completed_json)| {
            let completed = serde_json::from_str::<Vec<String>>(&completed_json)
                .unwrap_or_default()
                .into_iter()
                .collect();
            (active, step, completed)
        },
    );
    let recent_since = now.saturating_sub(BET_QUEST_LOOKBACK_SECONDS);
    let recent_bet = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM bets
              WHERE discord_id=?1 AND guild_id=?2 AND bet_time >= ?3
         )",
        params![key.discord_id, guild_id, recent_since],
        |row| row.get(0),
    )?;
    Ok(DigEventQuestSnapshot {
        active_quest_id,
        active_quest_step,
        completed_quest_ids,
        recent_bet,
    })
}

fn validate_quest_mutation(
    mutation: DigEventQuestMutation<'_>,
) -> Result<(), DigEventRuntimeRepositoryError> {
    let (quest_id, step) = match mutation {
        DigEventQuestMutation::SetActive {
            quest_id,
            next_step,
        } => (quest_id, Some(next_step)),
        DigEventQuestMutation::Complete { quest_id } => (quest_id, None),
    };
    if quest_id.trim().is_empty() {
        return Err(DigEventRuntimeRepositoryError::EmptyQuestId);
    }
    if step.is_some_and(|step| step <= 0) {
        return Err(DigEventRuntimeRepositoryError::InvalidQuestStep);
    }
    Ok(())
}

fn query_owned_ids(
    connection: &Connection,
    table: &str,
    column: &str,
    discord_id: i64,
    guild_id: i64,
) -> Result<BTreeSet<String>, rusqlite::Error> {
    let sql = match (table, column) {
        ("dig_gear", "item_id") => {
            "SELECT item_id FROM dig_gear
              WHERE discord_id = ?1 AND guild_id = ?2 AND item_id IS NOT NULL"
        }
        ("dig_artifacts", "artifact_id") => {
            "SELECT artifact_id FROM dig_artifacts
              WHERE discord_id = ?1 AND guild_id = ?2"
        }
        _ => unreachable!("fixed Dig event ownership query"),
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params![discord_id, guild_id], |row| row.get(0))?;
    rows.collect()
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    request: AtomicDigEventSettlement<'_>,
    detail: &str,
) -> Result<(), rusqlite::Error> {
    let reason = if request.balance_delta > 0 {
        "dig event credit"
    } else if request.balance_delta < 0 {
        "dig event debit"
    } else {
        "dig event balance change"
    };
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    transaction.execute(
        "INSERT INTO economy_ledger_context (
             id, source, actor_id, related_type, related_id, reason, metadata
         ) VALUES (1, 'dig', ?1, 'event', ?2, ?3, ?4)",
        params![
            request.expected.key.discord_id,
            request.event_id,
            reason,
            detail,
        ],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context", [])?;
    Ok(())
}

fn tunnel_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM tunnels WHERE discord_id = ?1 AND guild_id = ?2
         )",
        params![discord_id, guild_id],
        |row| row.get(0),
    )
}

fn player_exists(
    connection: &Connection,
    discord_id: i64,
    guild_id: i64,
) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM players WHERE discord_id = ?1 AND guild_id = ?2
         )",
        params![discord_id, guild_id],
        |row| row.get(0),
    )
}

#[cfg(test)]
#[path = "dig_event_runtime/tests.rs"]
mod tests;
