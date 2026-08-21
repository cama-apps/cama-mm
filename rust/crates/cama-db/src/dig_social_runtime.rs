//! Cohesive SQLite settlement for Dig social actions.
//!
//! Application policy computes random advances, cooldowns, rewards, and
//! presentation. This repository owns the migrated-schema snapshots and the
//! exact transactional boundaries for help, gifting, and sabotage.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigSocialKey {
    pub discord_id: i64,
    pub guild_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigHelpTunnelSnapshot {
    pub key: DigSocialKey,
    pub depth: i64,
    pub max_depth: i64,
    pub last_dig_at: Option<i64>,
    pub stat_stamina: i64,
    pub mutations_json: Option<String>,
    pub injury_state_json: Option<String>,
    pub temp_curses_json: Option<String>,
    pub boss_progress_json: Option<String>,
    pub tunnel_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigHelpSnapshot {
    pub helper_key: DigSocialKey,
    pub target_key: DigSocialKey,
    pub helper_player_exists: bool,
    pub target_player_exists: bool,
    pub helper_tunnel: Option<DigHelpTunnelSnapshot>,
    pub target_tunnel: Option<DigHelpTunnelSnapshot>,
    pub helper_equipped_relic_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicDigHelpSettlement<'a> {
    pub expected: &'a DigHelpSnapshot,
    pub new_target_depth: i64,
    pub helper_last_dig_at: i64,
    pub helper_reward: i64,
    pub create_helper_tunnel_name: Option<&'a str>,
    pub detail_json: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigHelpSettlementReceipt {
    pub target_depth_after: i64,
    pub helper_balance_after: i64,
    pub action_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigHelpSettlementOutcome {
    Applied(DigHelpSettlementReceipt),
    Conflict,
    MissingHelperPlayer,
    MissingTargetPlayer,
    MissingTargetTunnel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGiftReceipt {
    pub source_row_id: i64,
    pub receiver_row_id: i64,
    pub artifact_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DigGiftOutcome {
    Applied(DigGiftReceipt),
    SelfGift,
    MissingRelic,
    NonRelic,
    MissingReceiverTunnel,
    Conflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSabotageTunnelSnapshot {
    pub key: DigSocialKey,
    pub depth: i64,
    pub tunnel_name: String,
    pub trap_active: bool,
    pub insured_until: Option<i64>,
    pub reinforced_until: Option<i64>,
    pub revenge_target: Option<i64>,
    pub revenge_type: Option<String>,
    pub revenge_until: Option<i64>,
    pub pickaxe_tier: i64,
    pub equipped_weapon_tier: Option<i64>,
    pub equipped_weapon_durability: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigRecentSabotage {
    pub target_id: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSabotageSnapshot {
    pub actor_key: DigSocialKey,
    pub target_key: DigSocialKey,
    pub actor_balance: Option<i64>,
    pub target_balance: Option<i64>,
    pub actor_tunnel: Option<DigSabotageTunnelSnapshot>,
    pub target_tunnel: Option<DigSabotageTunnelSnapshot>,
    pub target_equipped_relic_ids: Vec<String>,
    pub recent_sabotages: Vec<DigRecentSabotage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSabotageRevenge<'a> {
    pub target_id: i64,
    pub kind: &'a str,
    pub until: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicDigSabotageSettlement<'a> {
    pub expected: &'a DigSabotageSnapshot,
    pub target_depth_delta: i64,
    pub actor_jc_cost: i64,
    pub target_jc_credit: i64,
    pub actor_depth_delta: i64,
    pub clear_target_trap: bool,
    pub revenge: Option<DigSabotageRevenge<'a>>,
    pub action_type: &'a str,
    pub detail_json: &'a str,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigSabotageSettlementReceipt {
    pub actor_balance_after: i64,
    pub target_balance_after: Option<i64>,
    pub actor_depth_after: Option<i64>,
    pub target_depth_after: i64,
    pub action_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigSabotageSettlementOutcome {
    Applied(DigSabotageSettlementReceipt),
    Conflict,
    MissingActorPlayer,
    MissingTargetTunnel,
}

/// One vendetta reflect: the attacker's reflected damage, the defender's
/// bonus, and the audit row, settled together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigVendettaReflectSettlement<'a> {
    pub actor_id: i64,
    pub target_id: i64,
    pub guild_id: i64,
    pub reflect: i64,
    pub bonus: i64,
    pub reflect_detail_json: &'a str,
    pub bonus_detail_json: &'a str,
    pub created_at: i64,
}

/// What a vendetta reflect actually moved. Both amounts are the credited
/// figures, not the requested ones, so the caller cannot report a bonus that
/// never landed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigVendettaReflectReceipt {
    pub reflected: i64,
    pub bonus_credited: i64,
    pub action_id: i64,
}

struct SocialCredit<'a> {
    recipient_id: i64,
    actor_id: i64,
    guild_id: i64,
    amount: i64,
    related_type: &'a str,
    reason: &'a str,
    detail_json: &'a str,
}

#[derive(Debug, Error)]
pub enum DigSocialRuntimeRepositoryError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("negative Dig social reward")]
    NegativeReward,
    #[error("negative Dig sabotage value")]
    NegativeSabotageValue,
    #[error("invalid Dig social detail JSON")]
    InvalidDetail,
    #[error("injected Dig social settlement failure")]
    InjectedFailure,
}

#[derive(Clone, Debug)]
pub struct DigSocialRuntimeRepository {
    path: PathBuf,
}

impl DigSocialRuntimeRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn help_snapshot(
        &self,
        helper_id: i64,
        target_id: i64,
        guild_id: i64,
    ) -> Result<DigHelpSnapshot, DigSocialRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let helper_key = DigSocialKey {
            discord_id: helper_id,
            guild_id,
        };
        let target_key = DigSocialKey {
            discord_id: target_id,
            guild_id,
        };
        let snapshot = DigHelpSnapshot {
            helper_key,
            target_key,
            helper_player_exists: player_exists(&transaction, helper_key)?,
            target_player_exists: player_exists(&transaction, target_key)?,
            helper_tunnel: help_tunnel(&transaction, helper_key)?,
            target_tunnel: help_tunnel(&transaction, target_key)?,
            helper_equipped_relic_ids: equipped_relic_ids(&transaction, helper_key)?,
        };
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn atomic_help(
        &self,
        settlement: AtomicDigHelpSettlement<'_>,
    ) -> Result<DigHelpSettlementOutcome, DigSocialRuntimeRepositoryError> {
        self.atomic_help_inner(settlement, false)
    }

    fn atomic_help_inner(
        &self,
        settlement: AtomicDigHelpSettlement<'_>,
        fail_after_target_update: bool,
    ) -> Result<DigHelpSettlementOutcome, DigSocialRuntimeRepositoryError> {
        if settlement.helper_reward < 0 {
            return Err(DigSocialRuntimeRepositoryError::NegativeReward);
        }
        serde_json::from_str::<serde_json::Value>(settlement.detail_json)
            .map_err(|_| DigSocialRuntimeRepositoryError::InvalidDetail)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expected = settlement.expected;
        if !player_exists(&transaction, expected.helper_key)? {
            transaction.commit()?;
            return Ok(DigHelpSettlementOutcome::MissingHelperPlayer);
        }
        if !player_exists(&transaction, expected.target_key)? {
            transaction.commit()?;
            return Ok(DigHelpSettlementOutcome::MissingTargetPlayer);
        }
        let Some(expected_target) = expected.target_tunnel.as_ref() else {
            transaction.commit()?;
            return Ok(DigHelpSettlementOutcome::MissingTargetTunnel);
        };
        let Some(current_target) = help_tunnel(&transaction, expected.target_key)? else {
            transaction.commit()?;
            return Ok(DigHelpSettlementOutcome::MissingTargetTunnel);
        };
        if current_target.depth != expected_target.depth
            || current_target.boss_progress_json != expected_target.boss_progress_json
        {
            transaction.commit()?;
            return Ok(DigHelpSettlementOutcome::Conflict);
        }
        let current_helper = help_tunnel(&transaction, expected.helper_key)?;
        if !same_helper_revision(current_helper.as_ref(), expected.helper_tunnel.as_ref()) {
            transaction.commit()?;
            return Ok(DigHelpSettlementOutcome::Conflict);
        }

        let target_changed = transaction.execute(
            "UPDATE tunnels SET depth=?1
             WHERE discord_id=?2 AND guild_id=?3 AND depth=?4
               AND COALESCE(boss_progress,'')=COALESCE(?5,'')",
            params![
                settlement.new_target_depth,
                expected.target_key.discord_id,
                expected.target_key.guild_id,
                expected_target.depth,
                expected_target.boss_progress_json,
            ],
        )?;
        if target_changed != 1 {
            // Nothing has been written yet, but every guard below this point
            // has: rolling back keeps the whole settlement all-or-nothing.
            transaction.rollback()?;
            return Ok(DigHelpSettlementOutcome::Conflict);
        }
        if fail_after_target_update {
            return Err(DigSocialRuntimeRepositoryError::InjectedFailure);
        }

        if expected.helper_tunnel.is_none() {
            let Some(name) = settlement.create_helper_tunnel_name else {
                transaction.commit()?;
                return Ok(DigHelpSettlementOutcome::Conflict);
            };
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO tunnels
                    (discord_id,guild_id,depth,max_depth,total_digs,total_jc_earned,
                     last_dig_at,streak_days,pickaxe_tier,prestige_level,tunnel_name,
                     trap_active,trap_free_today,paid_digs_today,created_at)
                 VALUES (?1,?2,0,0,0,0,NULL,0,0,0,?3,0,1,0,?4)",
                params![
                    expected.helper_key.discord_id,
                    expected.helper_key.guild_id,
                    name,
                    settlement.created_at,
                ],
            )?;
            if inserted != 1 {
                // The target's depth update already applied; committing here
                // would advance the target without ever crediting the helper.
                transaction.rollback()?;
                return Ok(DigHelpSettlementOutcome::Conflict);
            }
        }
        let helper_changed = transaction.execute(
            "UPDATE tunnels SET last_dig_at=?1
             WHERE discord_id=?2 AND guild_id=?3
               AND COALESCE(last_dig_at,-1)=COALESCE(?4,-1)",
            params![
                settlement.helper_last_dig_at,
                expected.helper_key.discord_id,
                expected.helper_key.guild_id,
                expected
                    .helper_tunnel
                    .as_ref()
                    .and_then(|tunnel| tunnel.last_dig_at),
            ],
        )?;
        if helper_changed != 1 {
            transaction.rollback()?;
            return Ok(DigHelpSettlementOutcome::Conflict);
        }

        if settlement.helper_reward > 0 {
            set_ledger_context(
                &transaction,
                expected.helper_key.discord_id,
                "help",
                expected.target_key.discord_id,
                "dig help reward",
                settlement.detail_json,
            )?;
            let updated = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    settlement.helper_reward,
                    expected.helper_key.discord_id,
                    expected.helper_key.guild_id,
                ],
            );
            clear_ledger_context(&transaction)?;
            if updated? != 1 {
                transaction.rollback()?;
                return Ok(DigHelpSettlementOutcome::MissingHelperPlayer);
            }
        }
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                 jc_delta,detail,created_at)
             VALUES (?1,?2,?3,'help',0,?4,?5,?6,?7)",
            params![
                expected.helper_key.guild_id,
                expected.helper_key.discord_id,
                expected.target_key.discord_id,
                settlement.new_target_depth,
                settlement.helper_reward,
                settlement.detail_json,
                settlement.created_at,
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        let helper_balance_after = player_balance(&transaction, expected.helper_key)?
            .expect("helper existence checked in transaction");
        transaction.commit()?;
        Ok(DigHelpSettlementOutcome::Applied(
            DigHelpSettlementReceipt {
                target_depth_after: settlement.new_target_depth,
                helper_balance_after,
                action_id,
            },
        ))
    }

    pub fn credit_help_target_bonus(
        &self,
        helper_id: i64,
        target_id: i64,
        guild_id: i64,
        amount: i64,
        detail_json: &str,
    ) -> Result<Option<i64>, DigSocialRuntimeRepositoryError> {
        if amount < 0 {
            return Err(DigSocialRuntimeRepositoryError::NegativeReward);
        }
        serde_json::from_str::<serde_json::Value>(detail_json)
            .map_err(|_| DigSocialRuntimeRepositoryError::InvalidDetail)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = DigSocialKey {
            discord_id: target_id,
            guild_id,
        };
        if !player_exists(&transaction, key)? {
            transaction.commit()?;
            return Ok(None);
        }
        if amount > 0 {
            set_ledger_context(
                &transaction,
                helper_id,
                "mentor_bonus",
                helper_id,
                "dig mentor target bonus",
                detail_json,
            )?;
            let result = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![amount, target_id, guild_id],
            );
            clear_ledger_context(&transaction)?;
            result?;
        }
        let balance = player_balance(&transaction, key)?;
        transaction.commit()?;
        Ok(balance)
    }

    pub fn sabotage_snapshot(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
        since: i64,
    ) -> Result<DigSabotageSnapshot, DigSocialRuntimeRepositoryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let actor_key = DigSocialKey {
            discord_id: actor_id,
            guild_id,
        };
        let target_key = DigSocialKey {
            discord_id: target_id,
            guild_id,
        };
        let snapshot = DigSabotageSnapshot {
            actor_key,
            target_key,
            actor_balance: player_balance(&transaction, actor_key)?,
            target_balance: player_balance(&transaction, target_key)?,
            actor_tunnel: sabotage_tunnel(&transaction, actor_key)?,
            target_tunnel: sabotage_tunnel(&transaction, target_key)?,
            target_equipped_relic_ids: equipped_relic_ids(&transaction, target_key)?,
            recent_sabotages: recent_sabotages(&transaction, actor_key, since)?,
        };
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn atomic_sabotage(
        &self,
        settlement: AtomicDigSabotageSettlement<'_>,
    ) -> Result<DigSabotageSettlementOutcome, DigSocialRuntimeRepositoryError> {
        self.atomic_sabotage_inner(settlement, false)
    }

    fn atomic_sabotage_inner(
        &self,
        settlement: AtomicDigSabotageSettlement<'_>,
        fail_after_actor_debit: bool,
    ) -> Result<DigSabotageSettlementOutcome, DigSocialRuntimeRepositoryError> {
        if settlement.actor_jc_cost < 0 || settlement.target_jc_credit < 0 {
            return Err(DigSocialRuntimeRepositoryError::NegativeSabotageValue);
        }
        serde_json::from_str::<serde_json::Value>(settlement.detail_json)
            .map_err(|_| DigSocialRuntimeRepositoryError::InvalidDetail)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expected = settlement.expected;
        let Some(actor_balance) = player_balance(&transaction, expected.actor_key)? else {
            transaction.commit()?;
            return Ok(DigSabotageSettlementOutcome::MissingActorPlayer);
        };
        let Some(expected_balance) = expected.actor_balance else {
            transaction.commit()?;
            return Ok(DigSabotageSettlementOutcome::MissingActorPlayer);
        };
        let Some(current_target) = sabotage_tunnel(&transaction, expected.target_key)? else {
            transaction.commit()?;
            return Ok(DigSabotageSettlementOutcome::MissingTargetTunnel);
        };
        let Some(expected_target) = expected.target_tunnel.as_ref() else {
            transaction.commit()?;
            return Ok(DigSabotageSettlementOutcome::MissingTargetTunnel);
        };
        let current_actor = sabotage_tunnel(&transaction, expected.actor_key)?;
        if actor_balance != expected_balance
            || !same_sabotage_tunnel_revision(Some(&current_target), Some(expected_target))
            || !same_sabotage_tunnel_revision(
                current_actor.as_ref(),
                expected.actor_tunnel.as_ref(),
            )
        {
            transaction.commit()?;
            return Ok(DigSabotageSettlementOutcome::Conflict);
        }

        if settlement.actor_jc_cost > 0 {
            set_ledger_context(
                &transaction,
                expected.actor_key.discord_id,
                settlement.action_type,
                expected.target_key.discord_id,
                "dig sabotage cost",
                settlement.detail_json,
            )?;
            let result = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    settlement.actor_jc_cost,
                    expected.actor_key.discord_id,
                    expected.actor_key.guild_id
                ],
            );
            clear_ledger_context(&transaction)?;
            result?;
        }
        if fail_after_actor_debit {
            return Err(DigSocialRuntimeRepositoryError::InjectedFailure);
        }
        if settlement.target_jc_credit > 0 {
            set_ledger_context(
                &transaction,
                expected.actor_key.discord_id,
                settlement.action_type,
                expected.target_key.discord_id,
                "dig sabotage target credit",
                settlement.detail_json,
            )?;
            let result = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    settlement.target_jc_credit,
                    expected.target_key.discord_id,
                    expected.target_key.guild_id
                ],
            );
            clear_ledger_context(&transaction)?;
            result?;
        }
        if settlement.target_depth_delta != 0 {
            transaction.execute(
                "UPDATE tunnels SET depth=MAX(0,depth+?1)
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    settlement.target_depth_delta,
                    expected.target_key.discord_id,
                    expected.target_key.guild_id
                ],
            )?;
        }
        if settlement.actor_depth_delta != 0 {
            transaction.execute(
                "UPDATE tunnels SET depth=MAX(0,depth+?1)
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    settlement.actor_depth_delta,
                    expected.actor_key.discord_id,
                    expected.actor_key.guild_id
                ],
            )?;
        }
        if settlement.clear_target_trap {
            transaction.execute(
                "UPDATE tunnels SET trap_active=0
                 WHERE discord_id=?1 AND guild_id=?2",
                params![expected.target_key.discord_id, expected.target_key.guild_id],
            )?;
        }
        if let Some(revenge) = settlement.revenge.as_ref() {
            transaction.execute(
                "UPDATE tunnels
                 SET revenge_target=?1,revenge_type=?2,revenge_until=?3
                 WHERE discord_id=?4 AND guild_id=?5",
                params![
                    revenge.target_id,
                    revenge.kind,
                    revenge.until,
                    expected.target_key.discord_id,
                    expected.target_key.guild_id
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                 jc_delta,detail,created_at)
             VALUES (?1,?2,?3,?4,0,0,0,?5,?6)",
            params![
                expected.actor_key.guild_id,
                expected.actor_key.discord_id,
                expected.target_key.discord_id,
                settlement.action_type,
                settlement.detail_json,
                settlement.created_at
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        let actor_balance_after = player_balance(&transaction, expected.actor_key)?
            .expect("actor existence checked inside sabotage transaction");
        let target_balance_after = player_balance(&transaction, expected.target_key)?;
        let actor_depth_after =
            sabotage_tunnel(&transaction, expected.actor_key)?.map(|tunnel| tunnel.depth);
        let target_depth_after = sabotage_tunnel(&transaction, expected.target_key)?
            .expect("target existence checked inside sabotage transaction")
            .depth;
        transaction.commit()?;
        Ok(DigSabotageSettlementOutcome::Applied(
            DigSabotageSettlementReceipt {
                actor_balance_after,
                target_balance_after,
                actor_depth_after,
                target_depth_after,
                action_id,
            },
        ))
    }

    pub fn credit_sabotage_mana_steal(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
        amount: i64,
        detail_json: &str,
    ) -> Result<Option<i64>, DigSocialRuntimeRepositoryError> {
        self.credit_social_side_effect(SocialCredit {
            recipient_id: actor_id,
            actor_id: target_id,
            guild_id,
            amount,
            related_type: "sabotage_steal",
            reason: "dig black mana sabotage steal",
            detail_json,
        })
    }

    /// Settle a vendetta reflect in one guarded transaction.
    ///
    /// The attacker's reflect debit, the defender's bonus credit, and the
    /// `vendetta_reflect` audit row used to be three separate commits, so a
    /// failure between them could leave the attacker debited with the defender
    /// uncredited and no audit trail -- and the caller still reported the full
    /// bonus as paid. The receipt now carries what actually moved.
    pub fn atomic_vendetta_reflect(
        &self,
        settlement: DigVendettaReflectSettlement<'_>,
    ) -> Result<DigVendettaReflectReceipt, DigSocialRuntimeRepositoryError> {
        self.atomic_vendetta_reflect_inner(settlement, false)
    }

    fn atomic_vendetta_reflect_inner(
        &self,
        settlement: DigVendettaReflectSettlement<'_>,
        fail_after_reflect_debit: bool,
    ) -> Result<DigVendettaReflectReceipt, DigSocialRuntimeRepositoryError> {
        if settlement.reflect <= 0 {
            return Err(DigSocialRuntimeRepositoryError::NegativeSabotageValue);
        }
        if settlement.bonus < 0 {
            return Err(DigSocialRuntimeRepositoryError::NegativeReward);
        }
        for detail in [settlement.reflect_detail_json, settlement.bonus_detail_json] {
            serde_json::from_str::<serde_json::Value>(detail)
                .map_err(|_| DigSocialRuntimeRepositoryError::InvalidDetail)?;
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        set_ledger_context(
            &transaction,
            settlement.target_id,
            "vendetta_reflect",
            settlement.target_id,
            "dig vendetta reflect debit",
            settlement.reflect_detail_json,
        )?;
        // The balance floor is what makes a broke attacker reflect nothing
        // instead of going negative.
        let debited = transaction.execute(
            "UPDATE players
             SET jopacoin_balance=COALESCE(jopacoin_balance,0)-?1,
                 updated_at=CURRENT_TIMESTAMP
             WHERE discord_id=?2 AND guild_id=?3
               AND COALESCE(jopacoin_balance,0)>=?1",
            params![settlement.reflect, settlement.actor_id, settlement.guild_id],
        );
        clear_ledger_context(&transaction)?;
        let reflected = if debited? == 1 { settlement.reflect } else { 0 };
        if fail_after_reflect_debit {
            // Dropping the transaction unread rolls the debit back.
            return Err(DigSocialRuntimeRepositoryError::InjectedFailure);
        }
        let target_key = DigSocialKey {
            discord_id: settlement.target_id,
            guild_id: settlement.guild_id,
        };
        let bonus_credited = if settlement.bonus > 0 && player_exists(&transaction, target_key)? {
            set_ledger_context(
                &transaction,
                settlement.actor_id,
                "vendetta_reflect",
                settlement.actor_id,
                "dig vendetta reflect bonus",
                settlement.bonus_detail_json,
            )?;
            let credited = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![settlement.bonus, settlement.target_id, settlement.guild_id],
            );
            clear_ledger_context(&transaction)?;
            if credited? == 1 { settlement.bonus } else { 0 }
        } else {
            0
        };
        let audit = serde_json::json!({
            "attacker_id": settlement.actor_id,
            "reflected": reflected,
            "target_bonus": bonus_credited,
        })
        .to_string();
        transaction.execute(
            "INSERT INTO dig_actions
                (guild_id,actor_id,target_id,action_type,depth_before,depth_after,
                 jc_delta,detail,created_at)
             VALUES (?1,?2,NULL,'vendetta_reflect',0,0,0,?3,?4)",
            params![
                settlement.guild_id,
                settlement.target_id,
                audit,
                settlement.created_at
            ],
        )?;
        let action_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(DigVendettaReflectReceipt {
            reflected,
            bonus_credited,
            action_id,
        })
    }

    fn credit_social_side_effect(
        &self,
        credit: SocialCredit<'_>,
    ) -> Result<Option<i64>, DigSocialRuntimeRepositoryError> {
        if credit.amount < 0 {
            return Err(DigSocialRuntimeRepositoryError::NegativeReward);
        }
        serde_json::from_str::<serde_json::Value>(credit.detail_json)
            .map_err(|_| DigSocialRuntimeRepositoryError::InvalidDetail)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key = DigSocialKey {
            discord_id: credit.recipient_id,
            guild_id: credit.guild_id,
        };
        if !player_exists(&transaction, key)? {
            transaction.commit()?;
            return Ok(None);
        }
        if credit.amount > 0 {
            set_ledger_context(
                &transaction,
                credit.actor_id,
                credit.related_type,
                credit.actor_id,
                credit.reason,
                credit.detail_json,
            )?;
            let result = transaction.execute(
                "UPDATE players
                 SET jopacoin_balance=COALESCE(jopacoin_balance,0)+?1,
                     updated_at=CURRENT_TIMESTAMP
                 WHERE discord_id=?2 AND guild_id=?3",
                params![credit.amount, credit.recipient_id, credit.guild_id],
            );
            clear_ledger_context(&transaction)?;
            result?;
        }
        let balance = player_balance(&transaction, key)?;
        transaction.commit()?;
        Ok(balance)
    }

    pub fn gift_relic(
        &self,
        giver_id: i64,
        receiver_id: i64,
        guild_id: i64,
        artifact_id: &str,
        found_at: i64,
    ) -> Result<DigGiftOutcome, DigSocialRuntimeRepositoryError> {
        self.gift_relic_inner(
            giver_id,
            receiver_id,
            guild_id,
            artifact_id,
            found_at,
            false,
        )
    }

    fn gift_relic_inner(
        &self,
        giver_id: i64,
        receiver_id: i64,
        guild_id: i64,
        artifact_id: &str,
        found_at: i64,
        fail_after_delete: bool,
    ) -> Result<DigGiftOutcome, DigSocialRuntimeRepositoryError> {
        if giver_id == receiver_id {
            return Ok(DigGiftOutcome::SelfGift);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let receiver_has_tunnel: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM tunnels WHERE discord_id=?1 AND guild_id=?2)",
            params![receiver_id, guild_id],
            |row| row.get(0),
        )?;
        if !receiver_has_tunnel {
            transaction.commit()?;
            return Ok(DigGiftOutcome::MissingReceiverTunnel);
        }
        let source = transaction
            .query_row(
                "SELECT id,artifact_id,is_relic FROM dig_artifacts
                 WHERE discord_id=?1 AND guild_id=?2 AND artifact_id=?3
                 ORDER BY id LIMIT 1",
                params![giver_id, guild_id, artifact_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((source_row_id, stored_artifact_id, is_relic)) = source else {
            transaction.commit()?;
            return Ok(DigGiftOutcome::MissingRelic);
        };
        if !is_relic {
            transaction.commit()?;
            return Ok(DigGiftOutcome::NonRelic);
        }
        transaction.execute(
            "UPDATE dig_artifacts SET equipped=0
             WHERE discord_id=?1 AND guild_id=?2 AND artifact_id=?3",
            params![giver_id, guild_id, stored_artifact_id],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM dig_artifacts
             WHERE id=?1 AND discord_id=?2 AND guild_id=?3 AND artifact_id=?4 AND is_relic=1",
            params![source_row_id, giver_id, guild_id, stored_artifact_id],
        )?;
        if deleted != 1 {
            transaction.commit()?;
            return Ok(DigGiftOutcome::Conflict);
        }
        if fail_after_delete {
            return Err(DigSocialRuntimeRepositoryError::InjectedFailure);
        }
        transaction.execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,?3,?4,1,0)",
            params![receiver_id, guild_id, stored_artifact_id, found_at],
        )?;
        let receiver_row_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(DigGiftOutcome::Applied(DigGiftReceipt {
            source_row_id,
            receiver_row_id,
            artifact_id: stored_artifact_id,
        }))
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(&self.path)
    }
}

fn help_tunnel(
    transaction: &Transaction<'_>,
    key: DigSocialKey,
) -> Result<Option<DigHelpTunnelSnapshot>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT depth,max_depth,last_dig_at,COALESCE(stat_stamina,0),mutations,
                    injury_state,temp_curses,boss_progress,
                    COALESCE(tunnel_name,'Unknown Tunnel')
             FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![key.discord_id, key.guild_id],
            |row| {
                Ok(DigHelpTunnelSnapshot {
                    key,
                    depth: row.get(0)?,
                    max_depth: row.get(1)?,
                    last_dig_at: row.get(2)?,
                    stat_stamina: row.get(3)?,
                    mutations_json: row.get(4)?,
                    injury_state_json: row.get(5)?,
                    temp_curses_json: row.get(6)?,
                    boss_progress_json: row.get(7)?,
                    tunnel_name: row.get(8)?,
                })
            },
        )
        .optional()
}

fn same_helper_revision(
    current: Option<&DigHelpTunnelSnapshot>,
    expected: Option<&DigHelpTunnelSnapshot>,
) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(current), Some(expected)) => {
            current.last_dig_at == expected.last_dig_at
                && current.stat_stamina == expected.stat_stamina
                && current.mutations_json == expected.mutations_json
                && current.injury_state_json == expected.injury_state_json
                && current.temp_curses_json == expected.temp_curses_json
        }
        _ => false,
    }
}

fn sabotage_tunnel(
    transaction: &Transaction<'_>,
    key: DigSocialKey,
) -> Result<Option<DigSabotageTunnelSnapshot>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT t.depth,COALESCE(t.tunnel_name,'Unknown Tunnel'),
                    COALESCE(t.trap_active,0),t.insured_until,t.reinforced_until,
                    t.revenge_target,t.revenge_type,t.revenge_until,
                    COALESCE(t.pickaxe_tier,0),
                    (SELECT g.tier FROM dig_gear g
                     WHERE g.discord_id=t.discord_id AND g.guild_id=t.guild_id
                       AND g.slot='weapon' AND g.equipped=1
                     ORDER BY g.id LIMIT 1),
                    (SELECT g.durability FROM dig_gear g
                     WHERE g.discord_id=t.discord_id AND g.guild_id=t.guild_id
                       AND g.slot='weapon' AND g.equipped=1
                     ORDER BY g.id LIMIT 1)
             FROM tunnels t WHERE t.discord_id=?1 AND t.guild_id=?2",
            params![key.discord_id, key.guild_id],
            |row| {
                Ok(DigSabotageTunnelSnapshot {
                    key,
                    depth: row.get(0)?,
                    tunnel_name: row.get(1)?,
                    trap_active: row.get(2)?,
                    insured_until: row.get(3)?,
                    reinforced_until: row.get(4)?,
                    revenge_target: row.get(5)?,
                    revenge_type: row.get(6)?,
                    revenge_until: row.get(7)?,
                    pickaxe_tier: row.get(8)?,
                    equipped_weapon_tier: row.get(9)?,
                    equipped_weapon_durability: row.get(10)?,
                })
            },
        )
        .optional()
}

fn same_sabotage_tunnel_revision(
    current: Option<&DigSabotageTunnelSnapshot>,
    expected: Option<&DigSabotageTunnelSnapshot>,
) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(current), Some(expected)) => {
            current.depth == expected.depth
                && current.trap_active == expected.trap_active
                && current.insured_until == expected.insured_until
                && current.reinforced_until == expected.reinforced_until
                && current.revenge_target == expected.revenge_target
                && current.revenge_type == expected.revenge_type
                && current.revenge_until == expected.revenge_until
                && current.pickaxe_tier == expected.pickaxe_tier
                && current.equipped_weapon_tier == expected.equipped_weapon_tier
                && current.equipped_weapon_durability == expected.equipped_weapon_durability
        }
        _ => false,
    }
}

fn recent_sabotages(
    transaction: &Transaction<'_>,
    actor: DigSocialKey,
    since: i64,
) -> Result<Vec<DigRecentSabotage>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT target_id,detail,created_at FROM dig_actions
         WHERE guild_id=?1 AND actor_id=?2 AND action_type='sabotage'
           AND created_at>=?3
         ORDER BY created_at DESC,id DESC",
    )?;
    let rows = statement.query_map(params![actor.guild_id, actor.discord_id, since], |row| {
        let target_id = row.get::<_, Option<i64>>(0)?;
        let detail = row.get::<_, Option<String>>(1)?;
        let created_at = row.get::<_, i64>(2)?;
        let detail_target = detail
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| value.get("target_id").and_then(serde_json::Value::as_i64));
        Ok(detail_target
            .or(target_id)
            .map(|target_id| DigRecentSabotage {
                target_id,
                created_at,
            }))
    })?;
    rows.filter_map(Result::transpose).collect()
}

fn equipped_relic_ids(
    transaction: &Transaction<'_>,
    key: DigSocialKey,
) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT artifact_id FROM dig_artifacts
         WHERE discord_id=?1 AND guild_id=?2 AND is_relic=1 AND equipped=1
         ORDER BY id",
    )?;
    statement
        .query_map(params![key.discord_id, key.guild_id], |row| row.get(0))?
        .collect()
}

fn player_exists(
    transaction: &Transaction<'_>,
    key: DigSocialKey,
) -> Result<bool, rusqlite::Error> {
    transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2)",
        params![key.discord_id, key.guild_id],
        |row| row.get(0),
    )
}

fn player_balance(
    transaction: &Transaction<'_>,
    key: DigSocialKey,
) -> Result<Option<i64>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT COALESCE(jopacoin_balance,0) FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![key.discord_id, key.guild_id],
            |row| row.get(0),
        )
        .optional()
}

fn set_ledger_context(
    transaction: &Transaction<'_>,
    actor_id: i64,
    related_type: &str,
    related_id: i64,
    reason: &str,
    metadata: &str,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO economy_ledger_context
            (id,source,actor_id,related_type,related_id,reason,metadata)
         VALUES (1,'dig',?1,?2,?3,?4,?5)
         ON CONFLICT(id) DO UPDATE SET
            source=excluded.source,actor_id=excluded.actor_id,
            related_type=excluded.related_type,related_id=excluded.related_id,
            reason=excluded.reason,metadata=excluded.metadata",
        params![actor_id, related_type, related_id, reason, metadata],
    )?;
    Ok(())
}

fn clear_ledger_context(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM economy_ledger_context WHERE id=1", [])?;
    Ok(())
}

#[cfg(test)]
#[path = "dig_social_runtime/tests.rs"]
mod tests;
