//! Discord-neutral policy for wagers carried across Dig pinnacle phases.
//!
//! The module owns no command framework or live randomness. It expresses the
//! carry lifecycle through typed repository and entropy ports; the existing
//! Python runtime remains authoritative while parity is additive.

use cama_db::dig_carry_wager::{
    AtomicCarrySettlement, CarryProgressMutation, CarrySettlementOutcome, CarryTunnelKey,
    CarryTunnelSnapshot, ClearCarryOutcome, DigCarryWagerRepository, DigCarryWagerRepositoryError,
    PersistedWager,
};
use cama_domain::dig_economy::{
    DigEconomyInputError, WagerMultiplier, WinChance, effective_wager_multiplier,
    scale_positive_dig_jc, settled_wager_multiplier,
};
use thiserror::Error;

use crate::boss_duel::RiskTier;

pub const PINNACLE_BOUNDARY: i64 = 350;
pub const MAX_NEW_BOSS_WAGER: i64 = 1_000;
pub const OVER_CAP_MESSAGE: &str = "Boss wagers cannot exceed 1,000 JC.";
const PINNACLE_BASE_JC_REWARD: i64 = 500;
const PINNACLE_JC_PER_PRESTIGE: i64 = 100;
const REGULAR_BOUNDARIES: [i64; 7] = [25, 50, 75, 100, 150, 200, 275];

/// Typed persistence boundary used by the policy service.
pub trait DigCarryWagerPort {
    type Error;

    fn snapshot(&self, key: CarryTunnelKey) -> Result<Option<CarryTunnelSnapshot>, Self::Error>;

    fn clear_carry_markers(
        &self,
        snapshot: &CarryTunnelSnapshot,
        boundary: i64,
    ) -> Result<ClearCarryOutcome, Self::Error>;

    fn settle_atomic(
        &self,
        request: AtomicCarrySettlement<'_>,
    ) -> Result<CarrySettlementOutcome, Self::Error>;
}

impl DigCarryWagerPort for DigCarryWagerRepository {
    type Error = DigCarryWagerRepositoryError;

    fn snapshot(&self, key: CarryTunnelKey) -> Result<Option<CarryTunnelSnapshot>, Self::Error> {
        DigCarryWagerRepository::snapshot(self, key)
    }

    fn clear_carry_markers(
        &self,
        snapshot: &CarryTunnelSnapshot,
        boundary: i64,
    ) -> Result<ClearCarryOutcome, Self::Error> {
        DigCarryWagerRepository::clear_carry_markers(self, snapshot, boundary)
    }

    fn settle_atomic(
        &self,
        request: AtomicCarrySettlement<'_>,
    ) -> Result<CarrySettlementOutcome, Self::Error> {
        DigCarryWagerRepository::settle_atomic(self, request)
    }
}

/// Injectable randomness required by retreat and loss knockback policy.
pub trait DigCarryEntropy {
    fn retreat_block_loss(&mut self) -> i64;
    fn pinnacle_loss_knockback(&mut self) -> i64;
}

/// A coherent stake inherited by pinnacle phase 2 or 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarriedWager {
    pub wager: i64,
    pub risk_tier: RiskTier,
    pub boundary: i64,
}

/// Typed version of the mutable fields canonical Python preserves around
/// `_set_carried_wager` and `_clear_carried_wager`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditableCarryEntry {
    pub status: Option<String>,
    pub boss_id: Option<String>,
    pub hp_max: Option<i64>,
    pub carried_wager: Option<i64>,
    pub carried_risk_tier: Option<RiskTier>,
}

pub fn set_carried_wager(entry: &mut EditableCarryEntry, wager: i64, risk_tier: RiskTier) {
    entry.carried_wager = Some(wager);
    entry.carried_risk_tier = Some(risk_tier);
}

pub fn clear_carried_wager(entry: &mut EditableCarryEntry) {
    entry.carried_wager = None;
    entry.carried_risk_tier = None;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BossResolver {
    FightBoss,
    StartBossDuel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedBossFight {
    pub resolver: BossResolver,
    pub boundary: i64,
    pub wager: i64,
    pub risk_tier: RiskTier,
    pub inherited_carry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FightPreparation {
    Ready(PreparedBossFight),
    Rejected { error: &'static str },
    MissingTunnel,
    NotAtBoss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BossEncounterInfo {
    pub boundary: i64,
    pub carried_wager: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetreatResult {
    Applied {
        boundary: i64,
        block_loss: i64,
        new_depth: i64,
        carried_wager_forfeit: i64,
        actual_balance_delta: i64,
    },
    Conflict,
    MissingTunnel,
    MissingPlayer,
    NotAtBoss,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinnacleFinalizeInput {
    pub phase: u8,
    pub won: bool,
    pub wager: i64,
    pub risk_tier: RiskTier,
    pub win_chance: f64,
    pub prestige_level: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnaclePayout {
    pub scaled_base_reward: i64,
    pub wager_payout: i64,
    pub payout: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinalizeResult {
    Applied {
        phase: u8,
        won: bool,
        next_phase: u8,
        carry_forwarded: Option<CarriedWager>,
        wager_payout: i64,
        payout: i64,
        actual_balance_delta: i64,
    },
    Conflict,
    MissingTunnel,
    MissingPlayer,
    NotAtPinnacle,
    MissingActiveCarry,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum CarryPolicyError {
    #[error("pinnacle phase must be 1, 2, or 3, got {0}")]
    InvalidPhase(u8),
    #[error("wager cannot be negative: {0}")]
    NegativeWager(i64),
    #[error("prestige level cannot be negative: {0}")]
    NegativePrestige(i64),
    #[error("pinnacle reward arithmetic overflowed")]
    AmountOverflow,
    #[error("invalid Dig economy input: {0}")]
    Economy(#[from] DigEconomyInputError),
}

#[derive(Debug)]
pub enum CarryServiceError<E> {
    Port(E),
    Policy(CarryPolicyError),
}

/// Carry orchestration independent of Discord and the Python service object.
#[derive(Clone, Debug)]
pub struct DigCarryWagerService<P, E> {
    port: P,
    entropy: E,
}

impl<P, E> DigCarryWagerService<P, E>
where
    P: DigCarryWagerPort,
    E: DigCarryEntropy,
{
    #[must_use]
    pub const fn new(port: P, entropy: E) -> Self {
        Self { port, entropy }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    /// Return only a coherent pinnacle carry. A regular marker is stale and
    /// is removed with an exact-state CAS before returning `None`.
    pub fn get_carried_wager(&self, key: CarryTunnelKey) -> Result<Option<CarriedWager>, P::Error> {
        let Some(snapshot) = self.port.snapshot(key)? else {
            return Ok(None);
        };
        let Some(boundary) = current_boss_boundary(&snapshot) else {
            return Ok(None);
        };
        if boundary != PINNACLE_BOUNDARY {
            if snapshot
                .entry(boundary)
                .is_some_and(cama_db::dig_carry_wager::CarryProgressEntry::has_either_marker)
            {
                let _ = self.port.clear_carry_markers(&snapshot, boundary)?;
            }
            return Ok(None);
        }
        Ok(active_pinnacle_carry(&snapshot, boundary))
    }

    /// Resolve inherited source priority before applying the cap to a newly
    /// requested wager. This intentionally performs no persistence writes.
    pub fn prepare_fight(
        &self,
        key: CarryTunnelKey,
        resolver: BossResolver,
        requested_risk: RiskTier,
        requested_wager: i64,
    ) -> Result<FightPreparation, P::Error> {
        let Some(snapshot) = self.port.snapshot(key)? else {
            return Ok(FightPreparation::MissingTunnel);
        };
        let Some(boundary) = current_boss_boundary(&snapshot) else {
            return Ok(FightPreparation::NotAtBoss);
        };
        if let Some(carried) = active_pinnacle_carry(&snapshot, boundary) {
            return Ok(FightPreparation::Ready(PreparedBossFight {
                resolver,
                boundary,
                wager: carried.wager,
                risk_tier: carried.risk_tier,
                inherited_carry: true,
            }));
        }
        if requested_wager > MAX_NEW_BOSS_WAGER {
            return Ok(FightPreparation::Rejected {
                error: OVER_CAP_MESSAGE,
            });
        }
        Ok(FightPreparation::Ready(PreparedBossFight {
            resolver,
            boundary,
            wager: requested_wager.max(0),
            risk_tier: requested_risk,
            inherited_carry: false,
        }))
    }

    pub fn encounter_info(
        &self,
        key: CarryTunnelKey,
    ) -> Result<Option<BossEncounterInfo>, P::Error> {
        let Some(snapshot) = self.port.snapshot(key)? else {
            return Ok(None);
        };
        let Some(boundary) = current_boss_boundary(&snapshot) else {
            return Ok(None);
        };
        Ok(Some(BossEncounterInfo {
            boundary,
            carried_wager: active_pinnacle_carry(&snapshot, boundary).map(|carry| carry.wager),
        }))
    }

    pub fn retreat(&mut self, key: CarryTunnelKey) -> Result<RetreatResult, P::Error> {
        let Some(snapshot) = self.port.snapshot(key)? else {
            return Ok(RetreatResult::MissingTunnel);
        };
        let Some(boundary) = current_boss_boundary(&snapshot) else {
            return Ok(RetreatResult::NotAtBoss);
        };
        let block_loss = self.entropy.retreat_block_loss().clamp(2, 3);
        let new_depth = snapshot.depth.saturating_sub(block_loss).max(0);
        let carried_wager_forfeit =
            active_pinnacle_carry(&snapshot, boundary).map_or(0, |carry| carry.wager / 2);
        let requested_balance_delta = -carried_wager_forfeit;
        let outcome = self.port.settle_atomic(AtomicCarrySettlement {
            key,
            expected_depth: snapshot.depth,
            expected_pinnacle_phase: snapshot.pinnacle_phase,
            expected_boss_progress: &snapshot.boss_progress,
            depth_after: new_depth,
            pinnacle_phase_after: snapshot.pinnacle_phase,
            requested_balance_delta,
            mutation: CarryProgressMutation::Clear {
                boundary,
                status_after: None,
            },
            ledger_related_type: "boss_retreat",
            ledger_related_id: "carried_wager",
            ledger_reason: "forfeited half carried wager on retreat",
            ledger_metadata: r#"{"policy":"dig_carry_wager"}"#,
        })?;
        Ok(match outcome {
            CarrySettlementOutcome::Applied {
                actual_balance_delta,
                ..
            } => RetreatResult::Applied {
                boundary,
                block_loss,
                new_depth,
                carried_wager_forfeit,
                actual_balance_delta,
            },
            CarrySettlementOutcome::Conflict => RetreatResult::Conflict,
            CarrySettlementOutcome::MissingTunnel => RetreatResult::MissingTunnel,
            CarrySettlementOutcome::MissingPlayer => RetreatResult::MissingPlayer,
        })
    }

    pub fn finalize_pinnacle(
        &mut self,
        key: CarryTunnelKey,
        input: PinnacleFinalizeInput,
    ) -> Result<FinalizeResult, CarryServiceError<P::Error>> {
        validate_finalize_input(input).map_err(CarryServiceError::Policy)?;
        let snapshot = self.port.snapshot(key).map_err(CarryServiceError::Port)?;
        let Some(snapshot) = snapshot else {
            return Ok(FinalizeResult::MissingTunnel);
        };
        if current_boss_boundary(&snapshot) != Some(PINNACLE_BOUNDARY) {
            return Ok(FinalizeResult::NotAtPinnacle);
        }
        if i64::from(input.phase) != snapshot.pinnacle_phase {
            return Ok(FinalizeResult::MissingActiveCarry);
        }

        let (wager, risk_tier) = if input.phase == 1 {
            (input.wager, input.risk_tier)
        } else if let Some(carry) = active_pinnacle_carry(&snapshot, PINNACLE_BOUNDARY) {
            (carry.wager, carry.risk_tier)
        } else {
            return Ok(FinalizeResult::MissingActiveCarry);
        };

        let transition = finalize_transition(&snapshot, input, wager, risk_tier, &mut self.entropy)
            .map_err(CarryServiceError::Policy)?;
        let outcome = self
            .port
            .settle_atomic(transition.request)
            .map_err(CarryServiceError::Port)?;
        Ok(match outcome {
            CarrySettlementOutcome::Applied {
                actual_balance_delta,
                ..
            } => FinalizeResult::Applied {
                phase: input.phase,
                won: input.won,
                next_phase: transition.next_phase,
                carry_forwarded: transition.carry_forwarded,
                wager_payout: transition.wager_payout,
                payout: transition.payout,
                actual_balance_delta,
            },
            CarrySettlementOutcome::Conflict => FinalizeResult::Conflict,
            CarrySettlementOutcome::MissingTunnel => FinalizeResult::MissingTunnel,
            CarrySettlementOutcome::MissingPlayer => FinalizeResult::MissingPlayer,
        })
    }
}

struct FinalizeTransition<'a> {
    request: AtomicCarrySettlement<'a>,
    next_phase: u8,
    carry_forwarded: Option<CarriedWager>,
    wager_payout: i64,
    payout: i64,
}

fn finalize_transition<'a, E: DigCarryEntropy>(
    snapshot: &'a CarryTunnelSnapshot,
    input: PinnacleFinalizeInput,
    wager: i64,
    risk_tier: RiskTier,
    entropy: &mut E,
) -> Result<FinalizeTransition<'a>, CarryPolicyError> {
    let mut next_phase = input.phase;
    let mut carry_forwarded = None;
    let mut wager_payout = 0;
    let mut payout = 0;
    let (depth_after, pinnacle_phase_after, requested_balance_delta, mutation) = if !input.won {
        let knockback = entropy.pinnacle_loss_knockback().clamp(8, 16);
        (
            snapshot.depth.saturating_sub(knockback).max(0),
            snapshot.pinnacle_phase,
            wager
                .checked_neg()
                .ok_or(CarryPolicyError::AmountOverflow)?,
            CarryProgressMutation::Clear {
                boundary: PINNACLE_BOUNDARY,
                status_after: None,
            },
        )
    } else if input.phase < 3 {
        next_phase = input.phase + 1;
        let status_after = if input.phase == 1 {
            "phase1_defeated"
        } else {
            "phase2_defeated"
        };
        if wager > 0 {
            let carried = CarriedWager {
                wager,
                risk_tier,
                boundary: PINNACLE_BOUNDARY,
            };
            carry_forwarded = Some(carried);
            (
                snapshot.depth,
                i64::from(next_phase),
                0,
                CarryProgressMutation::Forward {
                    boundary: PINNACLE_BOUNDARY,
                    status_after,
                    wager,
                    risk_tier: risk_name(risk_tier),
                },
            )
        } else {
            (
                snapshot.depth,
                i64::from(next_phase),
                0,
                CarryProgressMutation::Clear {
                    boundary: PINNACLE_BOUNDARY,
                    status_after: Some(status_after),
                },
            )
        }
    } else {
        let settlement = pinnacle_payout(wager, risk_tier, input.win_chance, input.prestige_level)?;
        wager_payout = settlement.wager_payout;
        payout = settlement.payout;
        next_phase = 0;
        (
            snapshot.depth,
            0,
            payout,
            CarryProgressMutation::Clear {
                boundary: PINNACLE_BOUNDARY,
                status_after: Some("defeated"),
            },
        )
    };
    Ok(FinalizeTransition {
        request: AtomicCarrySettlement {
            key: snapshot.key,
            expected_depth: snapshot.depth,
            expected_pinnacle_phase: snapshot.pinnacle_phase,
            expected_boss_progress: &snapshot.boss_progress,
            depth_after,
            pinnacle_phase_after,
            requested_balance_delta,
            mutation,
            ledger_related_type: "pinnacle_fight",
            ledger_related_id: "carried_wager",
            ledger_reason: if requested_balance_delta < 0 {
                "forfeited carried wager"
            } else {
                "settled pinnacle victory"
            },
            ledger_metadata: r#"{"policy":"dig_carry_wager"}"#,
        },
        next_phase,
        carry_forwarded,
        wager_payout,
        payout,
    })
}

fn validate_finalize_input(input: PinnacleFinalizeInput) -> Result<(), CarryPolicyError> {
    if !(1..=3).contains(&input.phase) {
        return Err(CarryPolicyError::InvalidPhase(input.phase));
    }
    if input.wager < 0 {
        return Err(CarryPolicyError::NegativeWager(input.wager));
    }
    if input.prestige_level < 0 {
        return Err(CarryPolicyError::NegativePrestige(input.prestige_level));
    }
    WinChance::new(input.win_chance)?;
    Ok(())
}

/// Canonical scaled base plus log-compressed, chance-tapered wager profit.
pub fn pinnacle_payout(
    wager: i64,
    risk_tier: RiskTier,
    win_chance: f64,
    prestige_level: i64,
) -> Result<PinnaclePayout, CarryPolicyError> {
    if wager < 0 {
        return Err(CarryPolicyError::NegativeWager(wager));
    }
    if prestige_level < 0 {
        return Err(CarryPolicyError::NegativePrestige(prestige_level));
    }
    let prestige_reward = prestige_level
        .checked_mul(PINNACLE_JC_PER_PRESTIGE)
        .ok_or(CarryPolicyError::AmountOverflow)?;
    let gross_base = PINNACLE_BASE_JC_REWARD
        .checked_add(prestige_reward)
        .ok_or(CarryPolicyError::AmountOverflow)?;
    let scaled_base_reward = scale_positive_dig_jc(gross_base);
    let wager_payout = pinnacle_wager_profit(wager, risk_tier, win_chance)?;
    let payout = scaled_base_reward
        .checked_add(wager_payout)
        .ok_or(CarryPolicyError::AmountOverflow)?;
    Ok(PinnaclePayout {
        scaled_base_reward,
        wager_payout,
        payout,
    })
}

pub fn pinnacle_wager_profit(
    wager: i64,
    risk_tier: RiskTier,
    win_chance: f64,
) -> Result<i64, CarryPolicyError> {
    if wager < 0 {
        return Err(CarryPolicyError::NegativeWager(wager));
    }
    if wager == 0 {
        return Ok(0);
    }
    let authored = match risk_tier {
        RiskTier::Cautious => 2.5,
        RiskTier::Bold => 3.8,
        RiskTier::Reckless => 7.5,
    };
    let base = WagerMultiplier::new(authored)?;
    let chance = WinChance::new(win_chance)?;
    let tapered = effective_wager_multiplier(base, chance);
    let settled = settled_wager_multiplier(tapered, None).value();
    Ok((wager as f64 * (settled - 1.0)) as i64)
}

#[must_use]
pub fn current_boss_boundary(snapshot: &CarryTunnelSnapshot) -> Option<i64> {
    for boundary in REGULAR_BOUNDARIES {
        let status = snapshot
            .entry(boundary)
            .and_then(|entry| entry.status.as_deref());
        if snapshot.depth >= boundary - 1 && is_unfinished_status(status) {
            return Some(boundary);
        }
    }
    if snapshot.depth < PINNACLE_BOUNDARY - 1 {
        return None;
    }
    let all_regular_defeated = REGULAR_BOUNDARIES.iter().all(|boundary| {
        snapshot
            .entry(*boundary)
            .and_then(|entry| entry.status.as_deref())
            == Some("defeated")
    });
    let pinnacle_status = snapshot
        .entry(PINNACLE_BOUNDARY)
        .and_then(|entry| entry.status.as_deref());
    (all_regular_defeated
        && matches!(
            pinnacle_status,
            None | Some("active" | "phase1_defeated" | "phase2_defeated")
        ))
    .then_some(PINNACLE_BOUNDARY)
}

#[must_use]
pub fn active_pinnacle_carry(
    snapshot: &CarryTunnelSnapshot,
    boundary: i64,
) -> Option<CarriedWager> {
    if boundary != PINNACLE_BOUNDARY {
        return None;
    }
    let expected_status = match snapshot.pinnacle_phase {
        2 => "phase1_defeated",
        3 => "phase2_defeated",
        _ => return None,
    };
    let entry = snapshot.entry(boundary)?;
    if entry.status.as_deref() != Some(expected_status) {
        return None;
    }
    let PersistedWager::Integer(wager) = entry.wager else {
        return None;
    };
    if wager <= 0 {
        return None;
    }
    let risk_tier = parse_risk(entry.risk_tier.as_deref()?)?;
    Some(CarriedWager {
        wager,
        risk_tier,
        boundary,
    })
}

fn is_unfinished_status(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("active" | "phase1_defeated" | "phase2_defeated")
    )
}

const fn risk_name(risk_tier: RiskTier) -> &'static str {
    match risk_tier {
        RiskTier::Cautious => "cautious",
        RiskTier::Bold => "bold",
        RiskTier::Reckless => "reckless",
    }
}

fn parse_risk(value: &str) -> Option<RiskTier> {
    match value {
        "cautious" => Some(RiskTier::Cautious),
        "bold" => Some(RiskTier::Bold),
        "reckless" => Some(RiskTier::Reckless),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
