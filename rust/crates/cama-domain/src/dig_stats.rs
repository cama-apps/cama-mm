//! Pure miner-stat and wager-skin policies for `/dig`.
//!
//! Persistence normalizes the three miner stats before this arithmetic runs.
//! This module makes that boundary explicit while preserving the exact Python
//! thresholds used by advancement, cave-ins, cooldowns, paid costs, and the
//! silent wager-size combat bonus.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// Strength points required for one extra maximum advancement step.
pub const STRENGTH_MAX_ADVANCE_INTERVAL: u64 = 2;
/// Strength points required for one extra minimum advancement step.
pub const STRENGTH_MIN_ADVANCE_INTERVAL: u64 = 5;
/// Cave-in probability removed by each Smarts point.
pub const SMARTS_CAVE_IN_REDUCTION: f64 = 0.02;
/// Cooldown and paid-cost reduction contributed by each Stamina point.
pub const STAMINA_COOLDOWN_REDUCTION: f64 = 0.04;
/// Maximum cooldown and paid-cost reduction contributed by Stamina.
pub const STAMINA_MAX_REDUCTION: f64 = 0.50;
/// Stake that reaches the full wager-skin hit bonus.
pub const WAGER_SKIN_BONUS_DENOMINATOR: i64 = 500;
/// Largest silent hit-chance bonus granted for a wager.
pub const WAGER_SKIN_BONUS_MAX: f64 = 0.03;

/// Identifies one of the three miner stats at an input boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MinerStat {
    /// Controls minimum and maximum advancement bonuses.
    Strength,
    /// Controls cave-in chance reduction.
    Smarts,
    /// Controls cooldown and paid-cost multipliers.
    Stamina,
}

impl Display for MinerStat {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Strength => "strength",
            Self::Smarts => "smarts",
            Self::Stamina => "stamina",
        };
        formatter.write_str(name)
    }
}

/// Invalid input supplied to the normalized miner-stat model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinerStatsInputError {
    /// Stat whose value failed validation.
    pub stat: MinerStat,
    /// Rejected signed database value.
    pub value: i64,
}

impl Display for MinerStatsInputError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} must be non-negative, got {}",
            self.stat, self.value
        )
    }
}

impl Error for MinerStatsInputError {}

/// Validated miner stats after the service's normalization boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinerStats {
    strength: u64,
    smarts: u64,
    stamina: u64,
}

impl MinerStats {
    /// Validate signed values read from storage and construct miner stats.
    pub fn new(strength: i64, smarts: i64, stamina: i64) -> Result<Self, MinerStatsInputError> {
        Ok(Self {
            strength: non_negative(MinerStat::Strength, strength)?,
            smarts: non_negative(MinerStat::Smarts, smarts)?,
            stamina: non_negative(MinerStat::Stamina, stamina)?,
        })
    }

    /// Return the normalized Strength point count.
    #[must_use]
    pub const fn strength(self) -> u64 {
        self.strength
    }

    /// Return the normalized Smarts point count.
    #[must_use]
    pub const fn smarts(self) -> u64 {
        self.smarts
    }

    /// Return the normalized Stamina point count.
    #[must_use]
    pub const fn stamina(self) -> u64 {
        self.stamina
    }
}

fn non_negative(stat: MinerStat, value: i64) -> Result<u64, MinerStatsInputError> {
    u64::try_from(value).map_err(|_| MinerStatsInputError { stat, value })
}

/// Mechanical modifiers produced by normalized miner stats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinerStatEffects {
    /// Whole steps added to the minimum advancement roll.
    pub advance_min_bonus: u64,
    /// Whole steps added to the maximum advancement roll.
    pub advance_max_bonus: u64,
    /// Absolute reduction applied to cave-in probability.
    pub cave_in_reduction: f64,
    /// Multiplier applied to cooldown duration.
    pub cooldown_multiplier: f64,
    /// Multiplier applied to paid action costs.
    pub paid_cost_multiplier: f64,
}

/// Translate normalized miner stats into the exact Python service modifiers.
#[must_use]
pub fn miner_stat_effects(stats: MinerStats) -> MinerStatEffects {
    let stamina_reduction =
        (stats.stamina() as f64 * STAMINA_COOLDOWN_REDUCTION).min(STAMINA_MAX_REDUCTION);
    let stamina_multiplier = 1.0 - stamina_reduction;

    MinerStatEffects {
        advance_min_bonus: stats.strength() / STRENGTH_MIN_ADVANCE_INTERVAL,
        advance_max_bonus: stats.strength() / STRENGTH_MAX_ADVANCE_INTERVAL,
        cave_in_reduction: stats.smarts() as f64 * SMARTS_CAVE_IN_REDUCTION,
        cooldown_multiplier: stamina_multiplier,
        paid_cost_multiplier: stamina_multiplier,
    }
}

/// Return the silent hit-chance bonus contributed by a wager.
///
/// The denominator is deliberately flat: staking a small entire balance does
/// not grant the maximum bonus. Zero and negative wagers defensively return
/// zero, while wagers of 500 or more are capped at three percentage points.
#[must_use]
pub fn wager_skin_bonus(wager: i64) -> f64 {
    if wager <= 0 {
        return 0.0;
    }

    let ratio = (wager as f64 / WAGER_SKIN_BONUS_DENOMINATOR as f64).min(1.0);
    ratio * WAGER_SKIN_BONUS_MAX
}

#[cfg(test)]
mod tests {
    use super::{
        MinerStat, MinerStats, MinerStatsInputError, miner_stat_effects, wager_skin_bonus,
    };

    fn effects(strength: i64, smarts: i64, stamina: i64) -> super::MinerStatEffects {
        miner_stat_effects(
            MinerStats::new(strength, smarts, stamina).expect("parity fixture has valid stats"),
        )
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-12,
            "expected {actual} to be within 1e-12 of {expected}"
        );
    }

    fn assert_strength(strength: i64, expected_min: u64, expected_max: u64) {
        let actual = effects(strength, 0, 0);
        assert_eq!(actual.advance_min_bonus, expected_min);
        assert_eq!(actual.advance_max_bonus, expected_max);
    }

    #[test]
    fn test_strength_bonuses_change_at_exact_thresholds_0_0_0() {
        assert_strength(0, 0, 0);
    }

    #[test]
    fn test_strength_bonuses_change_at_exact_thresholds_1_0_0() {
        assert_strength(1, 0, 0);
    }

    #[test]
    fn test_strength_bonuses_change_at_exact_thresholds_2_0_1() {
        assert_strength(2, 0, 1);
    }

    #[test]
    fn test_strength_bonuses_change_at_exact_thresholds_4_0_2() {
        assert_strength(4, 0, 2);
    }

    #[test]
    fn test_strength_bonuses_change_at_exact_thresholds_5_1_2() {
        assert_strength(5, 1, 2);
    }

    #[test]
    fn test_strength_bonuses_change_at_exact_thresholds_10_2_5() {
        assert_strength(10, 2, 5);
    }

    fn assert_smarts(smarts: i64, expected: f64) {
        assert_close(effects(0, smarts, 0).cave_in_reduction, expected);
    }

    #[test]
    fn test_smarts_reduces_cave_in_chance_by_two_percent_per_point_0_0_0() {
        assert_smarts(0, 0.0);
    }

    #[test]
    fn test_smarts_reduces_cave_in_chance_by_two_percent_per_point_1_0_02() {
        assert_smarts(1, 0.02);
    }

    #[test]
    fn test_smarts_reduces_cave_in_chance_by_two_percent_per_point_5_0_1() {
        assert_smarts(5, 0.10);
    }

    fn assert_stamina(stamina: i64, expected: f64) {
        let actual = effects(0, 0, stamina);
        assert_close(actual.cooldown_multiplier, expected);
        assert_close(actual.paid_cost_multiplier, expected);
    }

    #[test]
    fn test_stamina_reduces_costs_four_percent_per_point_with_half_cap_0_1_0() {
        assert_stamina(0, 1.0);
    }

    #[test]
    fn test_stamina_reduces_costs_four_percent_per_point_with_half_cap_1_0_96() {
        assert_stamina(1, 0.96);
    }

    #[test]
    fn test_stamina_reduces_costs_four_percent_per_point_with_half_cap_12_0_52() {
        assert_stamina(12, 0.52);
    }

    #[test]
    fn test_stamina_reduces_costs_four_percent_per_point_with_half_cap_13_0_5() {
        assert_stamina(13, 0.50);
    }

    #[test]
    fn test_stamina_reduces_costs_four_percent_per_point_with_half_cap_100_0_5() {
        assert_stamina(100, 0.50);
    }

    #[test]
    fn test_zero_wager_no_bonus() {
        assert_eq!(wager_skin_bonus(0), 0.0);
    }

    #[test]
    fn test_max_wager_hits_three_percent() {
        assert_close(wager_skin_bonus(500), 0.03);
    }

    #[test]
    fn test_wager_above_cap_does_not_exceed_three_percent() {
        assert_close(wager_skin_bonus(2_000), 0.03);
        assert_close(wager_skin_bonus(50_000), 0.03);
    }

    #[test]
    fn test_partial_wager_partial_bonus() {
        assert_close(wager_skin_bonus(250), 0.015);
    }

    #[test]
    fn test_low_balance_all_in_does_not_max() {
        assert_close(wager_skin_bonus(200), 0.012);
    }

    #[test]
    fn test_negative_wager_no_bonus() {
        assert_eq!(wager_skin_bonus(-50), 0.0);
    }

    #[test]
    fn normalized_miner_stats_reject_negative_storage_values() {
        for (actual, stat, value) in [
            (MinerStats::new(-1, 0, 0), MinerStat::Strength, -1),
            (MinerStats::new(0, -2, 0), MinerStat::Smarts, -2),
            (MinerStats::new(0, 0, -3), MinerStat::Stamina, -3),
        ] {
            assert_eq!(actual, Err(MinerStatsInputError { stat, value }));
        }
    }
}
