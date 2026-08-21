//! Application orchestration for `/dig help` and `/dig gift`.
//!
//! Random advance, relic effects, cooldown policy, daily economy adjustment,
//! and player-facing errors live here. SQLite owns the guarded actor/target
//! settlement so Discord remains a transport adapter.

use std::path::{Path, PathBuf};

use cama_db::dig_event_threats::ThreatCurseEffects;
use cama_db::dig_social_runtime::{
    AtomicDigHelpSettlement, AtomicDigSabotageSettlement, DigGiftOutcome, DigHelpSettlementOutcome,
    DigHelpSnapshot, DigSabotageRevenge, DigSabotageSettlementOutcome, DigSabotageTunnelSnapshot,
    DigSocialRuntimeRepository, DigSocialRuntimeRepositoryError, DigVendettaReflectSettlement,
};
use cama_db::mana_service_repository::ManaRepository;
use cama_db::predictions_repository::{ContractSide, PredictionRepository};
use cama_db::shop_runtime::ShopRuntimeRepository;
use cama_domain::dig_economy::{DIG_POSITIVE_JC_MULTIPLIER, scale_positive_dig_jc};
use cama_domain::game_date::game_date_for_timestamp;
use cama_domain::mana::{ManaEffects, apply_sabotage_modifiers};
use serde_json::Value;
use thiserror::Error;

use crate::dig_event_threats::cursed_cooldown;
use crate::dig_loot::artifact_catalog;
use crate::dig_runtime::DigRuntimeConfig;
use crate::dig_service::{PICKAXE_TIERS, SABOTAGE_COOLDOWN_SECONDS};
use crate::dig_service::{cooldown_duration, cooldown_remaining, layer_at};
use crate::dig_sweep_fixes::{ProtectionSource, funded_protection};
use crate::dig_tunnel_naming::{DigTunnelNamingService, TunnelNameEntropy};
use crate::dig_tunnels::{mutation_effects, mutations_from_json};
use crate::economy_event_sqlite::{SqliteEconomyEventError, SqliteEconomyEventService};

pub trait DigSocialEntropy: TunnelNameEntropy {
    fn inclusive_i64(&mut self, low: i64, high: i64) -> i64;
}

#[derive(Debug)]
pub struct FastrandDigSocialEntropy {
    rng: fastrand::Rng,
}

impl Default for FastrandDigSocialEntropy {
    fn default() -> Self {
        Self {
            rng: fastrand::Rng::new(),
        }
    }
}

impl FastrandDigSocialEntropy {
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self {
            rng: fastrand::Rng::with_seed(seed),
        }
    }
}

impl TunnelNameEntropy for FastrandDigSocialEntropy {
    fn unit_f64(&mut self) -> f64 {
        self.rng.f64()
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        self.rng.usize(0..upper_exclusive)
    }
}

impl DigSocialEntropy for FastrandDigSocialEntropy {
    fn inclusive_i64(&mut self, low: i64, high: i64) -> i64 {
        self.rng.i64(low..=high)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigHelpResult {
    pub advance: i64,
    pub target_tunnel: String,
    pub target_depth_after: i64,
    pub helper_cooldown_until: i64,
    pub helper_reward: i64,
    pub mentor_helper_bonus: i64,
    pub mentor_target_bonus: i64,
    pub helper_balance_after: i64,
    pub action_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGiftResult {
    pub artifact_id: String,
    pub artifact_name: String,
    pub source_row_id: i64,
    pub receiver_row_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSabotagePreview {
    pub cost: i64,
    pub damage_range: &'static str,
    pub target_depth: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSabotageClue {
    pub kind: &'static str,
    pub hint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSabotageTrapDetail {
    pub jc_lost: i64,
    pub blocks_lost: i64,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigPredictionContractSteal {
    pub prediction_id: i64,
    pub side: &'static str,
    pub contracts: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigSabotageResult {
    pub cost: i64,
    pub damage: i64,
    pub target_tunnel: String,
    pub target_depth_after: i64,
    pub trap_triggered: bool,
    pub trap_detail: Option<DigSabotageTrapDetail>,
    pub sabotage_hit: bool,
    pub clue: Option<DigSabotageClue>,
    pub is_reveal: bool,
    pub insurance_applied: bool,
    pub damage_reduced: bool,
    pub absorbed_by_aegis: bool,
    pub protection_source: Option<String>,
    pub victim_tip: i64,
    pub mana_steal_jc: i64,
    pub attacker_block_reward: i64,
    pub vendetta_reflect: i64,
    pub vendetta_bonus: i64,
    pub prediction_contract_steal: Option<DigPredictionContractSteal>,
    pub action_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VendettaSideEffectRequest {
    actor_id: i64,
    target_id: i64,
    guild_id: i64,
    damage: i64,
    reflect: i64,
    bonus: i64,
    bonus_gross: i64,
    now: i64,
}

#[derive(Debug, Error)]
pub enum DigSocialRuntimeError {
    #[error("You can't help yourself.")]
    SelfHelp,
    #[error("You can't gift to yourself.")]
    SelfGift,
    #[error("You can't sabotage yourself.")]
    SelfSabotage,
    #[error("You're on cooldown ({0}s remaining).")]
    HelpCooldown(i64),
    #[error("You must be registered first. Use `/player register`.")]
    MissingHelperPlayer,
    #[error("That player isn't registered.")]
    MissingTargetPlayer,
    #[error("That player doesn't have a tunnel.")]
    MissingTargetTunnel,
    #[error("You don't have that artifact.")]
    MissingArtifact,
    #[error("Only relics can be gifted.")]
    NonRelic,
    #[error("Receiver doesn't have a tunnel.")]
    MissingReceiverTunnel,
    #[error("Sabotage costs {cost} JC but you only have {balance} JC.")]
    InsufficientSabotageFunds { cost: i64, balance: i64 },
    #[error("You already sabotaged this player in the last 12 hours.")]
    SabotageCooldown,
    #[error("That social action was already processed or its tunnel changed. Try again.")]
    Conflict,
    #[error(transparent)]
    Repository(#[from] DigSocialRuntimeRepositoryError),
    #[error(transparent)]
    Economy(#[from] SqliteEconomyEventError),
    #[error("invalid Dig social state: {0}")]
    InvalidState(String),
}

#[derive(Clone, Debug)]
pub struct DigSocialRuntimeService {
    path: PathBuf,
    repository: DigSocialRuntimeRepository,
    config: DigRuntimeConfig,
}

impl DigSocialRuntimeService {
    #[must_use]
    pub fn sqlite(path: impl AsRef<Path>) -> Self {
        Self::sqlite_with_config(path, DigRuntimeConfig::default())
    }

    #[must_use]
    pub fn sqlite_with_config(path: impl AsRef<Path>, config: DigRuntimeConfig) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            repository: DigSocialRuntimeRepository::new(&path),
            path,
            config,
        }
    }

    pub fn help(
        &self,
        helper_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigHelpResult, DigSocialRuntimeError> {
        let mut entropy = FastrandDigSocialEntropy::default();
        self.help_with_entropy(helper_id, target_id, guild_id, now, &mut entropy)
    }

    pub fn help_with_entropy(
        &self,
        helper_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
        entropy: &mut impl DigSocialEntropy,
    ) -> Result<DigHelpResult, DigSocialRuntimeError> {
        if helper_id == target_id {
            return Err(DigSocialRuntimeError::SelfHelp);
        }
        let snapshot = self
            .repository
            .help_snapshot(helper_id, target_id, guild_id)?;
        validate_help_snapshot(&snapshot)?;
        let target = snapshot
            .target_tunnel
            .as_ref()
            .ok_or(DigSocialRuntimeError::MissingTargetTunnel)?;
        let mana = active_mana_effects(&self.path, helper_id, guild_id, now);
        if let Some(helper) = snapshot.helper_tunnel.as_ref() {
            let cooldown = effective_help_cooldown(helper, &mana);
            let remaining = cooldown_remaining(helper.last_dig_at, now, cooldown);
            if remaining > 0 {
                return Err(DigSocialRuntimeError::HelpCooldown(remaining));
            }
        }

        let layer = layer_at(target.depth);
        let mut advance = entropy.inclusive_i64(layer.advance_range.0, layer.advance_range.1);
        let mycelium = snapshot
            .helper_equipped_relic_ids
            .iter()
            .any(|relic| relic == "mycelium_link");
        if mycelium {
            advance = advance.saturating_add(1);
        }
        if let Some(boundary) = next_unfinished_boss_boundary(target.boss_progress_json.as_deref())
            && target.depth.saturating_add(advance) >= boundary
        {
            advance = (boundary - 1 - target.depth).max(0);
        }
        let target_depth_after = target.depth.saturating_add(advance);
        let mentor_active = snapshot
            .helper_equipped_relic_ids
            .iter()
            .any(|relic| relic == "mentors_lantern");
        let helper_gross = 1 + if mentor_active { 10 } else { 0 };
        let target_gross = if mentor_active { 10 } else { 0 };
        let economy = SqliteEconomyEventService::new(&self.path, self.config.economy_event.clone());
        let helper_reward =
            scale_positive_dig_jc(economy.adjust_reward_at(guild_id, helper_gross, now)?);
        let target_reward =
            scale_positive_dig_jc(economy.adjust_reward_at(guild_id, target_gross, now)?);
        let detail_json = serde_json::json!({
            "target_id": target_id,
            "advance": advance,
            "target_depth_before": target.depth,
            "target_depth_after": target_depth_after,
            "mentor_bonus": mentor_active,
            "gross_jc": helper_gross,
            "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
        })
        .to_string();
        let create_name = if snapshot.helper_tunnel.is_none() {
            Some(DigTunnelNamingService::new(entropy).generate_tunnel_name())
        } else {
            None
        };
        let settlement = self.repository.atomic_help(AtomicDigHelpSettlement {
            expected: &snapshot,
            new_target_depth: target_depth_after,
            helper_last_dig_at: now,
            helper_reward,
            create_helper_tunnel_name: create_name.as_deref(),
            detail_json: &detail_json,
            created_at: now,
        })?;
        let receipt = match settlement {
            DigHelpSettlementOutcome::Applied(receipt) => receipt,
            DigHelpSettlementOutcome::Conflict => return Err(DigSocialRuntimeError::Conflict),
            DigHelpSettlementOutcome::MissingHelperPlayer => {
                return Err(DigSocialRuntimeError::MissingHelperPlayer);
            }
            DigHelpSettlementOutcome::MissingTargetPlayer => {
                return Err(DigSocialRuntimeError::MissingTargetTunnel);
            }
            DigHelpSettlementOutcome::MissingTargetTunnel => {
                return Err(DigSocialRuntimeError::MissingTargetTunnel);
            }
        };
        let target_bonus_detail = serde_json::json!({
            "advance": advance,
            "bonus": target_reward,
            "gross_jc": target_gross,
            "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
        })
        .to_string();
        let mentor_target_bonus = if target_reward > 0 {
            match self.repository.credit_help_target_bonus(
                helper_id,
                target_id,
                guild_id,
                target_reward,
                &target_bonus_detail,
            ) {
                Ok(Some(_)) => target_reward,
                Ok(None) | Err(_) => 0,
            }
        } else {
            0
        };
        let helper_cooldown_until = now.saturating_add(effective_new_helper_cooldown(
            snapshot.helper_tunnel.as_ref(),
            &mana,
        ));
        Ok(DigHelpResult {
            advance,
            target_tunnel: target.tunnel_name.clone(),
            target_depth_after,
            helper_cooldown_until,
            helper_reward,
            mentor_helper_bonus: if mentor_active { helper_reward } else { 0 },
            mentor_target_bonus,
            helper_balance_after: receipt.helper_balance_after,
            action_id: receipt.action_id,
        })
    }

    pub fn sabotage_preview(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
    ) -> Result<DigSabotagePreview, DigSocialRuntimeError> {
        if actor_id == target_id {
            return Err(DigSocialRuntimeError::SelfSabotage);
        }
        let snapshot = self
            .repository
            .sabotage_snapshot(actor_id, target_id, guild_id, 0)?;
        let target = snapshot
            .target_tunnel
            .as_ref()
            .ok_or(DigSocialRuntimeError::MissingTargetTunnel)?;
        Ok(DigSabotagePreview {
            cost: sabotage_base_cost(target.depth),
            damage_range: "3-8",
            target_depth: target.depth,
        })
    }

    pub fn sabotage(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
    ) -> Result<DigSabotageResult, DigSocialRuntimeError> {
        let mut entropy = FastrandDigSocialEntropy::default();
        self.sabotage_with_entropy(actor_id, target_id, guild_id, now, &mut entropy)
    }

    pub fn sabotage_with_entropy(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
        now: i64,
        entropy: &mut impl DigSocialEntropy,
    ) -> Result<DigSabotageResult, DigSocialRuntimeError> {
        if actor_id == target_id {
            return Err(DigSocialRuntimeError::SelfSabotage);
        }
        let snapshot = self.repository.sabotage_snapshot(
            actor_id,
            target_id,
            guild_id,
            now.saturating_sub(7 * 86_400),
        )?;
        let target = snapshot
            .target_tunnel
            .as_ref()
            .ok_or(DigSocialRuntimeError::MissingTargetTunnel)?;
        let mana = active_mana_effects(&self.path, actor_id, guild_id, now);
        let sabotage_modifiers = apply_sabotage_modifiers(&mana, sabotage_base_cost(target.depth));
        let cost = sabotage_modifiers.cost;
        let balance = snapshot.actor_balance.unwrap_or(0);
        if balance < cost {
            return Err(DigSocialRuntimeError::InsufficientSabotageFunds { cost, balance });
        }

        let economy = SqliteEconomyEventService::new(&self.path, self.config.economy_event.clone());
        let victim_tip_gross = (cost / 2).max(25);
        let victim_tip =
            scale_positive_dig_jc(economy.adjust_reward_at(guild_id, victim_tip_gross, now)?);
        let protection_key = format!(
            "sabotage:{guild_id}:{actor_id}:{target_id}:{}",
            now / SABOTAGE_COOLDOWN_SECONDS
        );
        if let Ok(protection) = ShopRuntimeRepository::new(&self.path).block_non_jc_attack(
            target_id,
            guild_id,
            Some(actor_id),
            &protection_key,
            now,
        ) && protection.blocked
        {
            let source = protection.source.unwrap_or_else(|| "shield".to_owned());
            let funded = funded_protection(
                if protection.duplicate {
                    ProtectionSource::DuplicateShield
                } else if matches!(source.as_str(), "aegis" | "first_aegis_today") {
                    ProtectionSource::AegisCharge
                } else {
                    ProtectionSource::Shield
                },
                cost,
                victim_tip,
            );
            let awarded_tip = funded.target_delta;
            let detail = serde_json::json!({
                "target_id": target_id,
                "absorbed": true,
                "duplicate_block": protection.duplicate,
                "cost": cost,
                "victim_tip": awarded_tip,
                "gross_jc": victim_tip_gross,
                "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
                "protection_source": source,
            })
            .to_string();
            let receipt = settle_sabotage(
                &self.repository,
                AtomicDigSabotageSettlement {
                    expected: &snapshot,
                    target_depth_delta: 0,
                    actor_jc_cost: cost,
                    target_jc_credit: awarded_tip,
                    actor_depth_delta: 0,
                    clear_target_trap: false,
                    revenge: None,
                    action_type: "sabotage",
                    detail_json: &detail,
                    created_at: now,
                },
            )?;
            return Ok(DigSabotageResult {
                cost,
                damage: 0,
                target_tunnel: target.tunnel_name.clone(),
                target_depth_after: receipt.target_depth_after,
                trap_triggered: false,
                trap_detail: None,
                sabotage_hit: true,
                clue: None,
                is_reveal: false,
                insurance_applied: false,
                damage_reduced: true,
                absorbed_by_aegis: source == "aegis",
                protection_source: Some(source),
                victim_tip: awarded_tip,
                mana_steal_jc: 0,
                attacker_block_reward: 0,
                vendetta_reflect: 0,
                vendetta_bonus: 0,
                prediction_contract_steal: None,
                action_id: receipt.action_id,
            });
        }

        if snapshot.recent_sabotages.iter().any(|action| {
            action.target_id == target_id
                && action.created_at >= now.saturating_sub(SABOTAGE_COOLDOWN_SECONDS)
        }) {
            return Err(DigSocialRuntimeError::SabotageCooldown);
        }

        if target.trap_active {
            let actor_loss = entropy.inclusive_i64(3, 5);
            let trap_cost = cost.saturating_mul(2);
            let target_credit = cost.saturating_add(victim_tip);
            let detail = serde_json::json!({
                "target_id": target_id,
                "trap_triggered": true,
                "jc_lost": trap_cost,
                "blocks_lost": actor_loss,
                "victim_tip": victim_tip,
            })
            .to_string();
            let receipt = settle_sabotage(
                &self.repository,
                AtomicDigSabotageSettlement {
                    expected: &snapshot,
                    target_depth_delta: 0,
                    actor_jc_cost: trap_cost,
                    target_jc_credit: target_credit,
                    actor_depth_delta: if snapshot.actor_tunnel.is_some() {
                        -actor_loss
                    } else {
                        0
                    },
                    clear_target_trap: true,
                    revenge: None,
                    action_type: "sabotage",
                    detail_json: &detail,
                    created_at: now,
                },
            )?;
            return Ok(DigSabotageResult {
                cost: trap_cost,
                damage: 0,
                target_tunnel: target.tunnel_name.clone(),
                target_depth_after: receipt.target_depth_after,
                trap_triggered: true,
                trap_detail: Some(DigSabotageTrapDetail {
                    jc_lost: trap_cost,
                    blocks_lost: actor_loss,
                    message: format!(
                        "Trap triggered! You lost {trap_cost} JC and {actor_loss} blocks!"
                    ),
                }),
                sabotage_hit: false,
                clue: None,
                is_reveal: false,
                insurance_applied: false,
                damage_reduced: false,
                absorbed_by_aegis: false,
                protection_source: None,
                victim_tip,
                mana_steal_jc: 0,
                attacker_block_reward: 0,
                vendetta_reflect: 0,
                vendetta_bonus: 0,
                prediction_contract_steal: None,
                action_id: receipt.action_id,
            });
        }

        if entropy.unit_f64() >= 0.50 {
            let detail = serde_json::json!({
                "target_id": target_id,
                "damage": 0,
                "cost": cost,
                "trap_triggered": false,
                "sabotage_hit": false,
                "attacker_block_reward": 0,
            })
            .to_string();
            let receipt = settle_sabotage(
                &self.repository,
                AtomicDigSabotageSettlement {
                    expected: &snapshot,
                    target_depth_delta: 0,
                    actor_jc_cost: cost,
                    target_jc_credit: 0,
                    actor_depth_delta: 0,
                    clear_target_trap: false,
                    revenge: None,
                    action_type: "sabotage",
                    detail_json: &detail,
                    created_at: now,
                },
            )?;
            return Ok(DigSabotageResult {
                cost,
                damage: 0,
                target_tunnel: target.tunnel_name.clone(),
                target_depth_after: receipt.target_depth_after,
                trap_triggered: false,
                trap_detail: None,
                sabotage_hit: false,
                clue: None,
                is_reveal: false,
                insurance_applied: false,
                damage_reduced: false,
                absorbed_by_aegis: false,
                protection_source: None,
                victim_tip: 0,
                mana_steal_jc: 0,
                attacker_block_reward: 0,
                vendetta_reflect: 0,
                vendetta_bonus: 0,
                prediction_contract_steal: None,
                action_id: receipt.action_id,
            });
        }

        let raw_damage = entropy.inclusive_i64(3, 8);
        let reduction = sabotage_damage_reduction(target, &snapshot.target_equipped_relic_ids, now);
        let damage = ((raw_damage as f64 * (1.0 - reduction)) as i64).max(1);
        let clue = sabotage_clue(snapshot.actor_tunnel.as_ref(), entropy.index(3));
        let same_target_count = snapshot
            .recent_sabotages
            .iter()
            .filter(|action| action.target_id == target_id)
            .count();
        let is_reveal = same_target_count >= 2;
        const REVENGE_TYPES: [&str; 3] = ["discount", "free", "damage"];
        let revenge_type = REVENGE_TYPES[entropy.index(REVENGE_TYPES.len())];
        let attacker_block_reward = sabotage_attacker_reward(target.depth);
        let detail = serde_json::json!({
            "target_id": target_id,
            "damage": damage,
            "cost": cost,
            "trap_triggered": false,
            "attacker_block_reward": attacker_block_reward,
        })
        .to_string();
        let receipt = settle_sabotage(
            &self.repository,
            AtomicDigSabotageSettlement {
                expected: &snapshot,
                target_depth_delta: -damage,
                actor_jc_cost: cost,
                target_jc_credit: 0,
                actor_depth_delta: attacker_block_reward,
                clear_target_trap: false,
                revenge: Some(DigSabotageRevenge {
                    target_id: actor_id,
                    kind: revenge_type,
                    until: now.saturating_add(6 * 3_600),
                }),
                action_type: "sabotage",
                detail_json: &detail,
                created_at: now,
            },
        )?;

        let prediction_contract_steal =
            self.maybe_steal_prediction_contracts(actor_id, target_id, guild_id, entropy);
        let mana_steal_jc = if sabotage_modifiers.steal_depth_pct > 0.0 && target.depth > 0 {
            let gross =
                ((target.depth as f64 * sabotage_modifiers.steal_depth_pct * 0.5) as i64).max(1);
            let amount = economy
                .adjust_reward_at(guild_id, gross, now)
                .ok()
                .map(scale_positive_dig_jc)
                .unwrap_or(0);
            let side_detail = serde_json::json!({
                "target_id": target_id,
                "target_depth": target.depth,
                "amount": amount,
                "gross_jc": gross,
                "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
            })
            .to_string();
            if amount > 0
                && self
                    .repository
                    .credit_sabotage_mana_steal(actor_id, target_id, guild_id, amount, &side_detail)
                    .is_ok()
            {
                amount
            } else {
                0
            }
        } else {
            0
        };

        let (vendetta_reflect, vendetta_bonus) = if snapshot
            .target_equipped_relic_ids
            .iter()
            .any(|relic| relic == "vendetta_coin")
        {
            let bonus_gross = 5;
            let bonus =
                scale_positive_dig_jc(economy.adjust_reward_at(guild_id, bonus_gross, now)?);
            let reflect = ((damage as f64 * 0.5) as i64).max(1);
            self.apply_vendetta_side_effects(VendettaSideEffectRequest {
                actor_id,
                target_id,
                guild_id,
                damage,
                reflect,
                bonus,
                bonus_gross,
                now,
            })
        } else {
            (0, 0)
        };

        Ok(DigSabotageResult {
            cost,
            damage,
            target_tunnel: target.tunnel_name.clone(),
            target_depth_after: receipt.target_depth_after,
            trap_triggered: false,
            trap_detail: None,
            sabotage_hit: true,
            clue: Some(clue),
            is_reveal,
            insurance_applied: reduction > 0.0,
            damage_reduced: reduction > 0.0,
            absorbed_by_aegis: false,
            protection_source: None,
            victim_tip: 0,
            mana_steal_jc,
            attacker_block_reward,
            vendetta_reflect,
            vendetta_bonus,
            prediction_contract_steal,
            action_id: receipt.action_id,
        })
    }

    fn maybe_steal_prediction_contracts(
        &self,
        actor_id: i64,
        target_id: i64,
        guild_id: i64,
        entropy: &mut impl DigSocialEntropy,
    ) -> Option<DigPredictionContractSteal> {
        if entropy.unit_f64() >= 0.50 {
            return None;
        }
        let repository = PredictionRepository::new(&self.path);
        let positions = repository
            .get_user_open_positions(target_id, Some(guild_id))
            .ok()?;
        let sides = positions
            .into_iter()
            .flat_map(|marked| {
                let position = marked.position;
                [
                    (
                        position.prediction_id,
                        ContractSide::Yes,
                        position.yes_contracts,
                    ),
                    (
                        position.prediction_id,
                        ContractSide::No,
                        position.no_contracts,
                    ),
                ]
                .into_iter()
                .filter(|(_, _, contracts)| *contracts > 0)
            })
            .collect::<Vec<_>>();
        if sides.is_empty() {
            return None;
        }
        let (prediction_id, side, held) = sides[entropy.index(sides.len())];
        let requested = entropy.inclusive_i64(1, held.min(5));
        let moved = repository
            .transfer_position_contracts(prediction_id, target_id, actor_id, side, requested)
            .ok()??;
        Some(DigPredictionContractSteal {
            prediction_id,
            side: match side {
                ContractSide::Yes => "yes",
                ContractSide::No => "no",
            },
            contracts: moved,
        })
    }

    fn apply_vendetta_side_effects(&self, request: VendettaSideEffectRequest) -> (i64, i64) {
        let debit_detail = serde_json::json!({
            "target_id": request.target_id,
            "damage": request.damage,
        })
        .to_string();
        let bonus_detail = serde_json::json!({
            "attacker_id": request.actor_id,
            "damage": request.damage,
            "gross_jc": request.bonus_gross,
            "reward_multiplier": DIG_POSITIVE_JC_MULTIPLIER,
        })
        .to_string();
        // One transaction: a failure must not leave the attacker debited with
        // the defender uncredited, and the reported figures are the ones the
        // settlement actually moved.
        self.repository
            .atomic_vendetta_reflect(DigVendettaReflectSettlement {
                actor_id: request.actor_id,
                target_id: request.target_id,
                guild_id: request.guild_id,
                reflect: request.reflect,
                bonus: request.bonus,
                reflect_detail_json: &debit_detail,
                bonus_detail_json: &bonus_detail,
                created_at: request.now,
            })
            .map_or((0, 0), |receipt| {
                (receipt.reflected, receipt.bonus_credited)
            })
    }

    pub fn gift_relic(
        &self,
        giver_id: i64,
        receiver_id: i64,
        guild_id: i64,
        artifact_id: &str,
        now: i64,
    ) -> Result<DigGiftResult, DigSocialRuntimeError> {
        let outcome =
            self.repository
                .gift_relic(giver_id, receiver_id, guild_id, artifact_id, now)?;
        match outcome {
            DigGiftOutcome::Applied(receipt) => {
                let artifact_name = artifact_catalog()
                    .into_iter()
                    .find(|artifact| artifact.id == receipt.artifact_id)
                    .map_or_else(
                        || receipt.artifact_id.clone(),
                        |artifact| artifact.name.to_owned(),
                    );
                Ok(DigGiftResult {
                    artifact_id: receipt.artifact_id,
                    artifact_name,
                    source_row_id: receipt.source_row_id,
                    receiver_row_id: receipt.receiver_row_id,
                })
            }
            DigGiftOutcome::SelfGift => Err(DigSocialRuntimeError::SelfGift),
            DigGiftOutcome::MissingRelic => Err(DigSocialRuntimeError::MissingArtifact),
            DigGiftOutcome::NonRelic => Err(DigSocialRuntimeError::NonRelic),
            DigGiftOutcome::MissingReceiverTunnel => {
                Err(DigSocialRuntimeError::MissingReceiverTunnel)
            }
            DigGiftOutcome::Conflict => Err(DigSocialRuntimeError::Conflict),
        }
    }
}

fn validate_help_snapshot(snapshot: &DigHelpSnapshot) -> Result<(), DigSocialRuntimeError> {
    if !snapshot.helper_player_exists {
        return Err(DigSocialRuntimeError::MissingHelperPlayer);
    }
    if snapshot.target_tunnel.is_none() {
        return Err(DigSocialRuntimeError::MissingTargetTunnel);
    }
    Ok(())
}

fn settle_sabotage(
    repository: &DigSocialRuntimeRepository,
    settlement: AtomicDigSabotageSettlement<'_>,
) -> Result<cama_db::dig_social_runtime::DigSabotageSettlementReceipt, DigSocialRuntimeError> {
    match repository.atomic_sabotage(settlement)? {
        DigSabotageSettlementOutcome::Applied(receipt) => Ok(receipt),
        DigSabotageSettlementOutcome::MissingTargetTunnel => {
            Err(DigSocialRuntimeError::MissingTargetTunnel)
        }
        DigSabotageSettlementOutcome::MissingActorPlayer => Err(
            DigSocialRuntimeError::InvalidState("sabotage actor player disappeared".to_owned()),
        ),
        DigSabotageSettlementOutcome::Conflict => Err(DigSocialRuntimeError::Conflict),
    }
}

fn sabotage_base_cost(target_depth: i64) -> i64 {
    (target_depth.max(0) / 5).max(5)
}

fn sabotage_damage_reduction(
    target: &DigSabotageTunnelSnapshot,
    relic_ids: &[String],
    now: i64,
) -> f64 {
    let mut reduction = 0.0_f64;
    if target.insured_until.is_some_and(|until| now < until) {
        reduction += 0.50;
    }
    if target.reinforced_until.is_some_and(|until| now < until) {
        reduction += 0.25;
    }
    if relic_ids.iter().any(|relic| relic == "obsidian_shield") {
        reduction += 0.15;
    }
    reduction.min(0.70)
}

fn sabotage_attacker_reward(target_depth: i64) -> i64 {
    if target_depth < 100 {
        3
    } else if target_depth < 250 {
        5
    } else {
        7
    }
}

fn sabotage_clue(actor: Option<&DigSabotageTunnelSnapshot>, choice: usize) -> DigSabotageClue {
    match choice {
        0 => {
            let first = actor
                .and_then(|tunnel| tunnel.tunnel_name.chars().next())
                .unwrap_or('?');
            DigSabotageClue {
                kind: "first_letter",
                hint: format!("Saboteur's tunnel starts with '{first}'"),
            }
        }
        1 => {
            let depth = actor.map_or(0, |tunnel| tunnel.depth);
            let low = depth / 10 * 10;
            DigSabotageClue {
                kind: "depth_range",
                hint: format!("Saboteur is between depth {low}-{}", low + 10),
            }
        }
        _ => {
            let tier = actor.map_or(0, active_pickaxe_tier);
            let name = usize::try_from(tier)
                .ok()
                .and_then(|tier| PICKAXE_TIERS.get(tier))
                .map_or("Basic", |tier| tier.name);
            DigSabotageClue {
                kind: "pickaxe_tier",
                hint: format!("Saboteur uses a {name} pickaxe"),
            }
        }
    }
}

fn active_pickaxe_tier(tunnel: &DigSabotageTunnelSnapshot) -> i64 {
    match (
        tunnel.equipped_weapon_tier,
        tunnel.equipped_weapon_durability,
    ) {
        (Some(_), Some(durability)) if durability <= 0 => 0,
        (Some(tier), _) => tier,
        _ => tunnel.pickaxe_tier,
    }
}

fn active_mana_effects(path: &Path, discord_id: i64, guild_id: i64, now: i64) -> ManaEffects {
    let Ok(today) = game_date_for_timestamp(now as f64) else {
        return ManaEffects::default();
    };
    let Ok(row) = ManaRepository::new(path).get_mana(discord_id, Some(guild_id)) else {
        return ManaEffects::default();
    };
    match row {
        Some(row) if row.assigned_date == today && !row.consumed_today => {
            mana_effects_for_land(&row.current_land)
        }
        _ => ManaEffects::default(),
    }
}

fn mana_effects_for_land(land: &str) -> ManaEffects {
    let color = match land {
        "Mountain" => Some("Red"),
        "Island" => Some("Blue"),
        "Forest" => Some("Green"),
        "Plains" => Some("White"),
        "Swamp" => Some("Black"),
        _ => None,
    };
    ManaEffects::for_color(color, color.map(|_| land))
}

fn effective_new_helper_cooldown(
    helper: Option<&cama_db::dig_social_runtime::DigHelpTunnelSnapshot>,
    mana: &ManaEffects,
) -> i64 {
    helper.map_or_else(
        || cooldown_duration(0, false, false, mana.dig_cooldown_reduction_seconds),
        |helper| effective_help_cooldown(helper, mana),
    )
}

fn effective_help_cooldown(
    helper: &cama_db::dig_social_runtime::DigHelpTunnelSnapshot,
    mana: &ManaEffects,
) -> i64 {
    let mutations = mutations_from_json(helper.mutations_json.as_deref());
    let restless = mutation_effects(&mutations)
        .get("cooldown_bonus_seconds")
        .and_then(|effect| effect.number())
        .is_some_and(|seconds| seconds > 0.0);
    let slower_cooldown = json_string_eq(
        helper.injury_state_json.as_deref(),
        "type",
        "slower_cooldown",
    );
    let after_stamina = cooldown_duration(helper.stat_stamina, restless, slower_cooldown, 0);
    cursed_cooldown(
        after_stamina,
        active_cooldown_curse(helper.temp_curses_json.as_deref()),
        mana.dig_cooldown_reduction_seconds,
    )
}

fn active_cooldown_curse(raw: Option<&str>) -> ThreatCurseEffects {
    let Some(value) = raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok()) else {
        return ThreatCurseEffects::default();
    };
    if value
        .get("digs_remaining")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        <= 0
    {
        return ThreatCurseEffects::default();
    }
    ThreatCurseEffects {
        cooldown_penalty: value
            .get("effect")
            .or_else(|| value.get("effects"))
            .and_then(|effect| effect.get("cooldown_penalty"))
            .and_then(Value::as_f64),
        ..ThreatCurseEffects::default()
    }
}

fn json_string_eq(raw: Option<&str>, key: &str, expected: &str) -> bool {
    raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.get(key).and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|value| value == expected)
}

fn next_unfinished_boss_boundary(raw: Option<&str>) -> Option<i64> {
    const BOUNDARIES: [i64; 7] = [25, 50, 75, 100, 150, 200, 275];
    let value = raw
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    for boundary in BOUNDARIES {
        let status = value
            .get(boundary.to_string())
            .and_then(|entry| {
                entry
                    .as_str()
                    .or_else(|| entry.get("status").and_then(Value::as_str))
            })
            .unwrap_or("active");
        if matches!(status, "active" | "phase1_defeated" | "phase2_defeated") {
            return Some(boundary);
        }
    }
    None
}

#[cfg(test)]
#[path = "dig_social_runtime/tests.rs"]
mod tests;
