//! Dota daily-play streak bonus and game-date helpers. The streak schedule
//! is shared with Dig, and game dates use the domain's fixed-PST policy.

use cama_domain::game_date::{GameDateError, streak_bonus_for, yesterday_of};

/// Dota and Dig deliberately share one schedule so their payouts cannot drift.
pub const DOTA_STREAK_SCHEDULE: [(i64, i64); 4] = crate::dig_service::STREAKS;

#[must_use]
pub fn dota_streak_bonus(streak_days: i64) -> i64 {
    streak_bonus_for(streak_days, DOTA_STREAK_SCHEDULE)
}

pub fn previous_game_date(today: &str) -> Result<String, GameDateError> {
    yesterday_of(today)
}

#[cfg(test)]
#[path = "dota_streak/tests.rs"]
mod tests;
