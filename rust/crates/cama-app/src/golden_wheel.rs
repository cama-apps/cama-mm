//! Golden Wheel live calibration: the OVEREXTENDED wedges are recomputed
//! from current guild wealth so the wheel keeps its target expected value.

use cama_domain::economy_scaling::scale_minigame_jc_delta;

use crate::wheel::{
    WheelEconomyPolicy, WheelKind, WheelMechanic, WheelValue, WheelWedge, wheel_catalog,
};

pub const HEIST_AVERAGE_RATE: f64 = 0.085;
pub const MARKET_CRASH_AVERAGE_RATE: f64 = 0.115;
pub const TRICKLE_DOWN_RATE_MIN: f64 = 0.02;
pub const TRICKLE_DOWN_RATE_MAX: f64 = 0.05;
pub const DIVIDEND_RATE: f64 = 0.005;
pub const RECESSION_TOP_RATE: f64 = 0.06;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveGoldenEconomy<'a> {
    pub spinner_balance: i64,
    pub other_top_balances: &'a [i64],
    pub rank_next_balance: Option<i64>,
    pub total_positive_balance: i64,
    pub bottom_player_balances: &'a [i64],
}

/// Estimate one special wedge using the same live-state formulas as Python.
#[must_use]
pub fn live_golden_mechanic_ev(mechanic: WheelMechanic, economy: LiveGoldenEconomy<'_>) -> f64 {
    match mechanic {
        WheelMechanic::RedShell | WheelMechanic::GreenShell => 0.0,
        WheelMechanic::BlueShell => -4.0,
        WheelMechanic::Heist => economy
            .bottom_player_balances
            .iter()
            .map(|balance| ((*balance as f64 * HEIST_AVERAGE_RATE) as i64).max(1))
            .sum::<i64>() as f64,
        WheelMechanic::MarketCrash => {
            if economy.other_top_balances.is_empty() {
                25.0
            } else {
                economy
                    .other_top_balances
                    .iter()
                    .map(|balance| ((*balance as f64 * MARKET_CRASH_AVERAGE_RATE) as i64).max(1))
                    .sum::<i64>() as f64
            }
        }
        WheelMechanic::CompoundInterest => 100.0,
        WheelMechanic::TrickleDown => {
            let others_positive = economy
                .total_positive_balance
                .saturating_sub(economy.spinner_balance.max(0))
                .max(0);
            (others_positive as f64 * ((TRICKLE_DOWN_RATE_MIN + TRICKLE_DOWN_RATE_MAX) / 2.0))
                as i64 as f64
        }
        WheelMechanic::Dividend => {
            ((economy.total_positive_balance as f64 * DIVIDEND_RATE) as i64).max(10) as f64
        }
        WheelMechanic::HostileTakeover => economy
            .rank_next_balance
            .filter(|value| *value != 0)
            .map_or(40.0, |balance| {
                ((balance as f64 * MARKET_CRASH_AVERAGE_RATE) as i64).max(1) as f64
            }),
        WheelMechanic::Recession => {
            -((economy.spinner_balance.max(0) as f64 * RECESSION_TOP_RATE) as i64) as f64
        }
        WheelMechanic::BananaPeel => -23.5,
        WheelMechanic::BombOmb => -52.5,
        _ => 0.0,
    }
}

/// Recalibrate the two OVEREXTENDED wedges from current guild wealth.
#[must_use]
pub fn compute_live_golden_wedges(
    economy: LiveGoldenEconomy<'_>,
    policy: WheelEconomyPolicy,
) -> Vec<WheelWedge> {
    let mut wedges = wheel_catalog(WheelKind::Golden, policy);
    let non_overextended_sum = wedges
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value >= 0 => Some(value),
            _ => None,
        })
        .sum::<i64>();
    let special_sum = wedges
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(mechanic) => Some(scale_minigame_jc_delta(
                live_golden_mechanic_ev(mechanic, economy),
                policy.minigame_scale,
            )),
            WheelValue::Numeric(_) => None,
        })
        .sum::<i64>();
    let overextended_count = wedges
        .iter()
        .filter(|wedge| matches!(wedge.value, WheelValue::Numeric(value) if value < 0))
        .count() as i64;
    let target_sum = scale_minigame_jc_delta(policy.golden_target_ev, policy.minigame_scale)
        .saturating_mul(i64::try_from(wedges.len()).expect("wheel length fits i64"));
    let overextended = if overextended_count == 0 {
        -1
    } else {
        (target_sum
            .saturating_sub(non_overextended_sum)
            .saturating_sub(special_sum)
            / overextended_count)
            .min(-1)
    };
    for wedge in &mut wedges {
        if matches!(wedge.value, WheelValue::Numeric(value) if value < 0) {
            wedge.label = overextended.to_string();
            wedge.value = WheelValue::Numeric(overextended);
        }
    }
    wedges
}

#[cfg(test)]
fn live_golden_expected_value(
    wedges: &[WheelWedge],
    economy: LiveGoldenEconomy<'_>,
    policy: WheelEconomyPolicy,
) -> f64 {
    if wedges.is_empty() {
        return 0.0;
    }
    let sum = wedges
        .iter()
        .map(|wedge| match wedge.value {
            WheelValue::Numeric(value) => value,
            WheelValue::Mechanic(mechanic) => scale_minigame_jc_delta(
                live_golden_mechanic_ev(mechanic, economy),
                policy.minigame_scale,
            ),
        })
        .sum::<i64>();
    sum as f64 / wedges.len() as f64
}

#[cfg(test)]
mod tests;
