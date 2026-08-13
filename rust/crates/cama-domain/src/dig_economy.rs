//! Pure `/dig` reward and wager-settlement policies.
//!
//! The Discord/service layer owns persistence and side effects. This module
//! keeps only the arithmetic shared by reward crediting, boss previews, and
//! boss settlement so those paths cannot drift during the Rust migration.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// Fraction of newly minted positive `/dig` Jopacoin that is credited.
pub const DIG_POSITIVE_JC_MULTIPLIER: f64 = 0.65;
/// Authored wager multipliers are untouched at or below this win chance.
pub const WAGER_TAPER_KNEE: f64 = 0.65;
/// Maximum displayed and computed boss win chance.
pub const WIN_CHANCE_CAP: f64 = 0.95;
/// Maximum wager profit as a ratio of the stake.
pub const WAGER_PROFIT_CAP_RATIO: f64 = 4.0;

/// Invalid numeric input rejected at the boundary of the pure economy model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DigEconomyInputError {
    /// A win chance must be a finite probability.
    InvalidWinChance(f64),
    /// A total-return or payout multiplier must be finite and non-negative.
    InvalidMultiplier(f64),
}

impl Display for DigEconomyInputError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWinChance(value) => {
                write!(
                    formatter,
                    "win chance must be finite and between 0 and 1, got {value}"
                )
            }
            Self::InvalidMultiplier(value) => {
                write!(
                    formatter,
                    "multiplier must be finite and non-negative, got {value}"
                )
            }
        }
    }
}

impl Error for DigEconomyInputError {}

/// A validated finite probability used by wager tapering.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct WinChance(f64);

impl WinChance {
    /// Validate and construct a win chance.
    pub fn new(value: f64) -> Result<Self, DigEconomyInputError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(DigEconomyInputError::InvalidWinChance(value))
        }
    }

    /// Return the underlying probability.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for WinChance {
    type Error = DigEconomyInputError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A validated total-return or payout multiplier.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct WagerMultiplier(f64);

impl WagerMultiplier {
    /// The identity multiplier used when no boss payout bonus is present.
    pub const ONE: Self = Self(1.0);

    /// Validate and construct a multiplier.
    pub fn new(value: f64) -> Result<Self, DigEconomyInputError> {
        if value.is_finite() && value >= 0.0 {
            Ok(Self(value))
        } else {
            Err(DigEconomyInputError::InvalidMultiplier(value))
        }
    }

    /// Return the underlying multiplier.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for WagerMultiplier {
    type Error = DigEconomyInputError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Scale newly minted positive `/dig` Jopacoin with half-up integer rounding.
///
/// Zero and negative values are sinks or neutral deltas rather than minted
/// rewards, so they pass through unchanged. Every positive gross reward
/// credits at least one Jopacoin.
#[must_use]
pub fn scale_positive_dig_jc(amount: i64) -> i64 {
    if amount <= 0 {
        return amount;
    }

    ((amount as f64 * DIG_POSITIVE_JC_MULTIPLIER + 0.5).floor() as i64).max(1)
}

/// Taper an authored wager multiplier toward fair odds above the 65% knee.
///
/// The blend reaches `1 / win_chance` at [`WIN_CHANCE_CAP`]. Taking the
/// minimum with the authored value ensures tapering can only reduce a payout.
#[must_use]
pub fn effective_wager_multiplier(
    base_multiplier: WagerMultiplier,
    win_chance: WinChance,
) -> WagerMultiplier {
    let base = base_multiplier.value();
    let chance = win_chance.value();
    if chance <= WAGER_TAPER_KNEE {
        return base_multiplier;
    }

    let fair = 1.0 / chance;
    let span = (WIN_CHANCE_CAP - WAGER_TAPER_KNEE).max(1e-6);
    let blend = ((chance - WAGER_TAPER_KNEE) / span).min(1.0);
    let effective = base * (1.0 - blend) + fair * blend;
    WagerMultiplier(base.min(effective))
}

/// Log-compress a total-return wager multiplier and cap profit at four stakes.
///
/// Values at or below break-even are deliberately returned untouched. Above
/// one, profit is `ln(multiplier)` and the total return cannot exceed 5x.
#[must_use]
pub fn log_capped_multiplier(multiplier: WagerMultiplier) -> WagerMultiplier {
    let raw = multiplier.value();
    if raw <= 1.0 {
        return multiplier;
    }

    WagerMultiplier(1.0 + WAGER_PROFIT_CAP_RATIO.min(raw.ln()))
}

/// Fold an optional boss payout bonus inside logarithmic compression and cap.
///
/// `None` is the Rust representation of Python's default `1.0` bonus.
#[must_use]
pub fn settled_wager_multiplier(
    tapered_multiplier: WagerMultiplier,
    boss_payout_multiplier: Option<WagerMultiplier>,
) -> WagerMultiplier {
    let raw = tapered_multiplier.value()
        * boss_payout_multiplier
            .unwrap_or(WagerMultiplier::ONE)
            .value();
    log_capped_multiplier(WagerMultiplier(raw))
}

#[cfg(test)]
mod tests {
    use super::{
        DigEconomyInputError, WIN_CHANCE_CAP, WagerMultiplier, WinChance,
        effective_wager_multiplier, log_capped_multiplier, scale_positive_dig_jc,
        settled_wager_multiplier,
    };

    fn multiplier(value: f64) -> WagerMultiplier {
        WagerMultiplier::new(value).expect("test multiplier is valid")
    }

    fn chance(value: f64) -> WinChance {
        WinChance::new(value).expect("test win chance is valid")
    }

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {actual} to be within {tolerance} of {expected}"
        );
    }

    fn assert_positive_scale(gross: i64, expected: i64) {
        assert_eq!(scale_positive_dig_jc(gross), expected);
    }

    #[test]
    fn test_positive_dig_jc_uses_half_up_rounding_with_minimum_one_1_1() {
        assert_positive_scale(1, 1);
    }

    #[test]
    fn test_positive_dig_jc_uses_half_up_rounding_with_minimum_one_2_1() {
        assert_positive_scale(2, 1);
    }

    #[test]
    fn test_positive_dig_jc_uses_half_up_rounding_with_minimum_one_3_2() {
        assert_positive_scale(3, 2);
    }

    #[test]
    fn test_positive_dig_jc_uses_half_up_rounding_with_minimum_one_5_3() {
        assert_positive_scale(5, 3);
    }

    #[test]
    fn test_positive_dig_jc_uses_half_up_rounding_with_minimum_one_15_10() {
        assert_positive_scale(15, 10);
    }

    #[test]
    fn test_positive_dig_jc_uses_half_up_rounding_with_minimum_one_100_65() {
        assert_positive_scale(100, 65);
    }

    #[test]
    fn test_positive_dig_jc_uses_half_up_rounding_with_minimum_one_1000_650() {
        assert_positive_scale(1_000, 650);
    }

    #[test]
    fn test_dig_jc_policy_does_not_scale_non_positive_amounts_0() {
        assert_eq!(scale_positive_dig_jc(0), 0);
    }

    #[test]
    fn test_dig_jc_policy_does_not_scale_non_positive_amounts_negative_1() {
        assert_eq!(scale_positive_dig_jc(-1), -1);
    }

    #[test]
    fn test_dig_jc_policy_does_not_scale_non_positive_amounts_negative_5() {
        assert_eq!(scale_positive_dig_jc(-5), -5);
    }

    #[test]
    fn test_dig_jc_policy_does_not_scale_non_positive_amounts_negative_100() {
        assert_eq!(scale_positive_dig_jc(-100), -100);
    }

    fn assert_multiplier_unchanged_at_or_below_knee(win_chance: f64) {
        let authored = multiplier(4.8);
        assert_eq!(
            effective_wager_multiplier(authored, chance(win_chance)),
            authored
        );
    }

    #[test]
    fn test_multiplier_unchanged_at_or_below_knee_0_05() {
        assert_multiplier_unchanged_at_or_below_knee(0.05);
    }

    #[test]
    fn test_multiplier_unchanged_at_or_below_knee_0_4() {
        assert_multiplier_unchanged_at_or_below_knee(0.40);
    }

    #[test]
    fn test_multiplier_unchanged_at_or_below_knee_0_55() {
        assert_multiplier_unchanged_at_or_below_knee(0.55);
    }

    #[test]
    fn test_multiplier_unchanged_at_or_below_knee_0_65() {
        assert_multiplier_unchanged_at_or_below_knee(0.65);
    }

    #[test]
    fn test_multiplier_is_fair_odds_at_win_chance_cap() {
        let effective = effective_wager_multiplier(multiplier(4.8), chance(WIN_CHANCE_CAP)).value();
        assert_close(effective, 1.0 / WIN_CHANCE_CAP, 1e-12);
        assert_close(WIN_CHANCE_CAP * effective, 1.0, 1e-12);
    }

    #[test]
    fn test_multiplier_tapers_monotonically_above_knee() {
        let chances = [0.70, 0.80, 0.88, 0.95];
        let effective: Vec<f64> = chances
            .into_iter()
            .map(|value| effective_wager_multiplier(multiplier(9.7), chance(value)).value())
            .collect();

        assert!(effective.windows(2).all(|pair| pair[0] > pair[1]));
        for (win_chance, tapered) in chances.into_iter().zip(effective) {
            assert!(1.0 / win_chance <= tapered);
            assert!(tapered < 9.7);
        }
    }

    #[test]
    fn test_taper_never_raises_a_low_base_multiplier() {
        let effective = effective_wager_multiplier(multiplier(1.5), chance(0.95));
        assert!(effective.value() <= 1.5);
    }

    #[test]
    fn test_at_or_below_break_even_is_untouched() {
        assert_eq!(log_capped_multiplier(multiplier(1.0)).value(), 1.0);
        assert_eq!(log_capped_multiplier(multiplier(0.8)).value(), 0.8);
    }

    #[test]
    fn test_low_multipliers_compress_mildly() {
        let effective = log_capped_multiplier(multiplier(1.5)).value();
        assert_close(effective, 1.405, 0.005);
    }

    fn assert_top_end_compresses_to_ln(raw: f64, expected: f64) {
        let effective = log_capped_multiplier(multiplier(raw)).value();
        assert_close(effective, expected, 0.005);
        assert!(effective <= 5.0);
    }

    #[test]
    fn test_top_end_compresses_to_ln_4_3_2_459() {
        assert_top_end_compresses_to_ln(4.3, 2.459);
    }

    #[test]
    fn test_top_end_compresses_to_ln_8_4_3_128() {
        assert_top_end_compresses_to_ln(8.4, 3.128);
    }

    #[test]
    fn test_top_end_compresses_to_ln_10_3_3_332() {
        assert_top_end_compresses_to_ln(10.3, 3.332);
    }

    #[test]
    fn test_top_end_compresses_to_ln_15_450000000000001_3_738() {
        assert_top_end_compresses_to_ln(10.3 * 1.5, 3.738);
    }

    #[test]
    fn test_profit_hard_caps_at_four_times_the_stake() {
        assert_close(log_capped_multiplier(multiplier(60.0)).value(), 5.0, 1e-12);
        assert_close(
            log_capped_multiplier(multiplier(1_000.0)).value(),
            5.0,
            1e-12,
        );
    }

    #[test]
    fn test_monotonic_below_the_cap() {
        let raw = [1.2, 1.5, 1.8, 2.1, 2.4];
        let effective: Vec<f64> = raw
            .into_iter()
            .map(|value| log_capped_multiplier(multiplier(value)).value())
            .collect();

        assert!(effective.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            effective
                .into_iter()
                .all(|value| 1.0 < value && value <= 2.0)
        );
    }

    #[test]
    fn test_never_raises_a_multiplier() {
        for raw in [1.05, 1.5, 2.0, 4.8, 9.7] {
            assert!(log_capped_multiplier(multiplier(raw)).value() <= raw);
        }
    }

    #[test]
    fn test_settled_multiplier_folds_bonus_inside_the_cap() {
        let settled = settled_wager_multiplier(multiplier(10.3), Some(multiplier(1.5))).value();
        assert_close(settled, 1.0 + (10.3_f64 * 1.5).ln(), 1e-12);

        let capped = settled_wager_multiplier(multiplier(60.0), Some(multiplier(2.0))).value();
        assert_close(capped, 5.0, 1e-12);

        assert_eq!(
            settled_wager_multiplier(multiplier(1.5), None),
            log_capped_multiplier(multiplier(1.5))
        );
    }

    #[test]
    fn test_composes_with_taper_near_fair_odds() {
        let tapered = effective_wager_multiplier(multiplier(4.8), chance(WIN_CHANCE_CAP));
        let effective = log_capped_multiplier(tapered);
        assert_close(effective.value(), tapered.value(), 0.002);
    }

    #[test]
    fn typed_inputs_reject_invalid_economy_values() {
        assert!(matches!(
            WinChance::new(1.01),
            Err(DigEconomyInputError::InvalidWinChance(value)) if value == 1.01
        ));
        assert!(matches!(
            WinChance::new(f64::NAN),
            Err(DigEconomyInputError::InvalidWinChance(value)) if value.is_nan()
        ));
        assert!(matches!(
            WagerMultiplier::new(-0.01),
            Err(DigEconomyInputError::InvalidMultiplier(value)) if value == -0.01
        ));
        assert!(matches!(
            WagerMultiplier::new(f64::INFINITY),
            Err(DigEconomyInputError::InvalidMultiplier(value)) if value.is_infinite()
        ));
    }
}
