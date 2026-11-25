//! Role-based rating adjustment: scale a player's effective value by how they
//! perform in the role they are being assigned, measured against their own
//! form across every role.
//!
//! A rating already carries a player's overall level, because every win moved
//! it. What one rating cannot carry is that a player is stronger in one
//! position than in another, so that difference is the only thing measured
//! here: a role's win rate is compared with the same player's win rate across
//! all of their roles, never with 50%.
//!
//! Comparing with 50% reads a rating climb as role skill. A player seeded low
//! who wins their way up keeps a lifetime win rate well above half in the role
//! they always play, long after the rating has caught up, and records never
//! decay - so they would be balanced above their rating in every game for
//! wins the rating has already paid them for. Measured against their own
//! form, a player who only ever plays one role is always neutral, and a flex
//! player is valued a little higher in their best role and a little lower in
//! their worst.
//!
//! Kept pure here (no database dependency) so the scale is unit-testable and
//! usable from both the materialized team API and the shuffler's optimized
//! role metrics.

/// Minimum games in a role before its win rate is trusted at all.
pub const MIN_GAMES_FOR_WINRATE: u32 = 10;

/// Games at the player's own overall win rate assumed for every role, so a
/// thin sample moves the factor less than a deep one.
///
/// A role with exactly this many games counts for half of its measured gap;
/// the weight is `games / (games + ROLE_SAMPLE_PRIOR_GAMES)`.
pub const ROLE_SAMPLE_PRIOR_GAMES: u32 = 20;

/// Lowest multiplier the factor can reach.
pub const MIN_ROLE_FACTOR: f64 = 0.9;

/// Highest multiplier the factor can reach.
pub const MAX_ROLE_FACTOR: f64 = 1.1;

/// A win/loss record: one role's, or a player's combined across roles.
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
        self.wins.saturating_add(self.losses)
    }

    /// Whether this record is deep enough for its win rate to be trusted.
    #[must_use]
    pub const fn has_minimum_sample(&self) -> bool {
        self.games() >= MIN_GAMES_FOR_WINRATE
    }

    /// The combined record across `records`: a player's overall form when
    /// given every one of their role records.
    #[must_use]
    pub fn total<'a>(records: impl IntoIterator<Item = &'a Self>) -> Self {
        records.into_iter().fold(Self::default(), |total, record| {
            Self::new(
                total.wins.saturating_add(record.wins),
                total.losses.saturating_add(record.losses),
            )
        })
    }

    /// Share of games won. Callers guarantee at least one game.
    fn win_rate(self) -> f64 {
        f64::from(self.wins) / f64::from(self.games())
    }
}

/// The multiplier for being assigned a role with `record`, for a player whose
/// combined record across every role - that one included - is `overall`.
///
/// The factor is `1 + (MAX_ROLE_FACTOR - MIN_ROLE_FACTOR) * gap * weight`,
/// held inside [`MIN_ROLE_FACTOR`]..=[`MAX_ROLE_FACTOR`], where `gap` is the
/// role's win rate minus the player's overall win rate and `weight` is
/// `games / (games + ROLE_SAMPLE_PRIOR_GAMES)`.
///
/// So a role won exactly as often as the player wins overall is 1.0, which
/// covers anyone who has only ever played one role. Records below
/// [`MIN_GAMES_FOR_WINRATE`] games - including a role never played - also
/// take 1.0: a small sample is noise, not evidence in either direction.
#[must_use]
pub fn role_factor(record: RoleRecord, overall: RoleRecord) -> f64 {
    if !record.has_minimum_sample() || overall.games() == 0 {
        return 1.0;
    }
    let games = f64::from(record.games());
    let gap = record.win_rate() - overall.win_rate();
    let weight = games / (games + f64::from(ROLE_SAMPLE_PRIOR_GAMES));
    (1.0 + (MAX_ROLE_FACTOR - MIN_ROLE_FACTOR) * gap * weight)
        .clamp(MIN_ROLE_FACTOR, MAX_ROLE_FACTOR)
}

#[cfg(test)]
#[path = "role_performance/tests.rs"]
mod tests;
