//! Pure bankruptcy-penalty policy shared by every profit payout.
//!
//! A flat share of profit is withheld for every penalty game the player still
//! has to win (`BANKRUPTCY_PENALTY_RATE_PER_GAME`, 5% by default), so each win
//! eases the penalty by one step and extensions raise it, capped at 100%.

/// Fraction of profit withheld while `penalty_games_remaining` wins are still
/// outstanding: `rate_per_game * remaining`, clamped to `0.0..=1.0`.
#[must_use]
pub fn withheld_rate(rate_per_game: f64, penalty_games_remaining: i64) -> f64 {
    if penalty_games_remaining <= 0 || !rate_per_game.is_finite() {
        return 0.0;
    }
    (rate_per_game.clamp(0.0, 1.0) * penalty_games_remaining as f64).clamp(0.0, 1.0)
}

/// Fraction of profit kept; the complement of [`withheld_rate`].
#[must_use]
pub fn kept_rate(rate_per_game: f64, penalty_games_remaining: i64) -> f64 {
    1.0 - withheld_rate(rate_per_game, penalty_games_remaining)
}

/// Configured per-game rate bundled for callers that thread it as one value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BankruptcyPenaltyPolicy {
    /// Share of profit withheld per outstanding penalty game.
    pub rate_per_game: f64,
}

impl BankruptcyPenaltyPolicy {
    /// See [`withheld_rate`].
    #[must_use]
    pub fn withheld_rate(self, penalty_games_remaining: i64) -> f64 {
        withheld_rate(self.rate_per_game, penalty_games_remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_outstanding_game_withholds_one_flat_step() {
        assert!((withheld_rate(0.05, 1) - 0.05).abs() < 1e-12);
        assert!((withheld_rate(0.05, 3) - 0.15).abs() < 1e-12);
        assert!((withheld_rate(0.05, 6) - 0.30).abs() < 1e-12);
        assert!((withheld_rate(0.1, 5) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn withholding_caps_at_the_whole_profit() {
        assert_eq!(withheld_rate(0.05, 20), 1.0);
        assert_eq!(withheld_rate(0.05, 200), 1.0);
        assert_eq!(withheld_rate(0.05, i64::MAX), 1.0);
    }

    #[test]
    fn no_remaining_games_means_no_withholding() {
        assert_eq!(withheld_rate(0.05, 0), 0.0);
        assert_eq!(withheld_rate(0.05, -1), 0.0);
        assert_eq!(kept_rate(0.05, 0), 1.0);
    }

    #[test]
    fn degenerate_rates_are_clamped() {
        assert_eq!(withheld_rate(1.5, 1), 1.0);
        assert_eq!(withheld_rate(-0.5, 3), 0.0);
        assert_eq!(withheld_rate(f64::NAN, 3), 0.0);
        assert_eq!(withheld_rate(f64::INFINITY, 3), 0.0);
    }

    #[test]
    fn policy_struct_delegates_to_the_free_function() {
        let policy = BankruptcyPenaltyPolicy {
            rate_per_game: 0.05,
        };
        assert_eq!(policy.withheld_rate(2), withheld_rate(0.05, 2));
        assert_eq!(policy.withheld_rate(0), 0.0);
    }

    #[test]
    fn kept_is_the_complement_of_withheld() {
        assert!((kept_rate(0.05, 2) - (1.0 - withheld_rate(0.05, 2))).abs() < 1e-12);
    }
}
