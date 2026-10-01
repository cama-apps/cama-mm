//! Role-based rating adjustment: scale a player's effective value by how well
//! they have actually performed in the role they are being assigned.
//!
//! Replaces the flat off-role multiplier with a continuous factor derived from
//! the player's win rate in that specific role. A player assigned a role they
//! rarely play is left unadjusted, because they will not have the minimum
//! sample.
//!
//! Kept pure here (no database dependency) so the scale is unit-testable and
//! usable from both the materialized team API and the shuffler's optimized
//! role metrics.

/// Minimum games in a role before win rate is trusted at all.
pub const MIN_GAMES_FOR_WINRATE: u32 = 3;

/// Multiplier applied to anyone below [`MIN_GAMES_FOR_WINRATE`] games in the
/// assigned role.
pub const UNPROVEN_ROLE_FACTOR: f64 = 1.0;

/// Multiplier applied at a 0% win rate with a sufficient sample.
pub const MIN_ROLE_FACTOR: f64 = 0.9;

/// Multiplier applied at a 100% win rate with a sufficient sample.
pub const MAX_ROLE_FACTOR: f64 = 1.1;

/// A player's win/loss record in one specific role.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RoleRecord {
    pub wins: u32,
    pub losses: u32,
}

impl RoleRecord {
    #[must_use]
    pub const fn new(wins: u32, losses: u32) -> Self {
        Self { wins, losses }
    }

    #[must_use]
    pub const fn games(&self) -> u32 {
        self.wins + self.losses
    }
}

/// The multiplier for a role record, linear in win rate across
/// [`MIN_ROLE_FACTOR`]..=[`MAX_ROLE_FACTOR`].
///
/// A 50% win rate maps to exactly 1.0, leaving the rating untouched. Records
/// below [`MIN_GAMES_FOR_WINRATE`] games — including a player who has never
/// been assigned the role — take [`UNPROVEN_ROLE_FACTOR`], so an unproven
/// assignment is assumed average until there is a sample to judge it by.
#[must_use]
pub fn role_factor(record: RoleRecord) -> f64 {
    let games = record.games();
    if games < MIN_GAMES_FOR_WINRATE {
        return UNPROVEN_ROLE_FACTOR;
    }
    let win_rate = f64::from(record.wins) / f64::from(games);
    MIN_ROLE_FACTOR + (MAX_ROLE_FACTOR - MIN_ROLE_FACTOR) * win_rate
}

/// Apply [`role_factor`] to a base rating value, clamped at zero to match the
/// effective-value contract used elsewhere in team scoring.
#[must_use]
pub fn adjusted_value(base_value: f64, record: RoleRecord) -> f64 {
    (base_value * role_factor(record)).max(0.0)
}

#[cfg(test)]
#[path = "role_performance/tests.rs"]
mod tests;
