//! Discord-neutral policy for wagers carried across Dig pinnacle phases.
//!
//! The boss runtime owns the carry lifecycle; this module reads a carried
//! stake from the persisted snapshot and prices pinnacle wager profit.

use std::collections::BTreeSet;

use cama_db::dig_carry_wager::{CarryTunnelSnapshot, PersistedWager};
use cama_domain::dig_economy::{
    DigEconomyInputError, WagerMultiplier, WinChance, effective_wager_multiplier,
    settled_wager_multiplier,
};
use thiserror::Error;

use crate::boss_duel::RiskTier;
use crate::dig_service::parked_boss_boundary;

pub const PINNACLE_BOUNDARY: i64 = 350;
pub const MAX_NEW_BOSS_WAGER: i64 = 1_000;

/// A coherent stake inherited by pinnacle phase 2 or 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CarriedWager {
    pub wager: i64,
    pub risk_tier: RiskTier,
    pub boundary: i64,
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

/// The boss the carry snapshot is parked on, by the same rule as the dig gate.
#[must_use]
pub fn current_boss_boundary(snapshot: &CarryTunnelSnapshot) -> Option<i64> {
    let defeated = snapshot
        .entries
        .iter()
        .filter(|(_, entry)| entry.status.as_deref() == Some("defeated"))
        .map(|(boundary, _)| *boundary)
        .collect::<BTreeSet<i64>>();
    parked_boss_boundary(snapshot.depth, &defeated)
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
