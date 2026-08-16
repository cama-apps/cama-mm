//! Deterministic policy for Dota daily-play streaks and match awards.
//!
//! The streak schedule is reused directly from Dig, and game dates are reused
//! from the domain's fixed-PST policy. HTTP, Discord, and SQLite are expressed
//! as orchestration boundaries rather than hidden inside these calculations.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDateTime, Utc};
use thiserror::Error;

use cama_domain::game_date::{
    GameDateError, game_date_for, game_date_for_timestamp, streak_bonus_for, yesterday_of,
};

/// Dota and Dig deliberately share one schedule so their payouts cannot drift.
pub const DOTA_STREAK_SCHEDULE: [(i64, i64); 4] = crate::dig_service::STREAKS;

#[must_use]
pub fn dota_streak_bonus(streak_days: i64) -> i64 {
    streak_bonus_for(streak_days, DOTA_STREAK_SCHEDULE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreakAwardResult {
    pub days: i64,
    pub bonus: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditInstruction {
    pub discord_id: i64,
    pub streak_days: i64,
    pub bonus: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccumulatedAward {
    pub discord_id: i64,
    pub net: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreditMode {
    /// One ordered transaction, used when no Blood Pact can interleave.
    Batch,
    /// One gross credit followed by the applicable Blood Pact skim.
    Individual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchStreakAwardPlan {
    pub results: BTreeMap<i64, StreakAwardResult>,
    pub credits: Vec<CreditInstruction>,
    pub accumulated: Vec<AccumulatedAward>,
    pub credit_mode: CreditMode,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum MatchStreakAwardError {
    #[error("participant and streak result lengths differ ({participants} != {streaks})")]
    LengthMismatch { participants: usize, streaks: usize },
}

/// Build ordered credits and net accumulation from aligned repository results.
///
/// `Some(empty)` means the Blood Pact lookup succeeded and found no targets,
/// enabling the batch path. `None` preserves Python's compatibility fallback:
/// every award takes the individual path and asks the skim port.
pub fn plan_match_streak_awards(
    participant_ids: &[i64],
    streak_days: &[i64],
    pact_targets: Option<&BTreeSet<i64>>,
    mut apply_skim: impl FnMut(i64, i64) -> i64,
) -> Result<MatchStreakAwardPlan, MatchStreakAwardError> {
    if participant_ids.len() != streak_days.len() {
        return Err(MatchStreakAwardError::LengthMismatch {
            participants: participant_ids.len(),
            streaks: streak_days.len(),
        });
    }

    let mut results = BTreeMap::new();
    let mut credits = Vec::new();
    for (&discord_id, &days) in participant_ids.iter().zip(streak_days) {
        let bonus = dota_streak_bonus(days);
        results.insert(discord_id, StreakAwardResult { days, bonus });
        if bonus > 0 {
            credits.push(CreditInstruction {
                discord_id,
                streak_days: days,
                bonus,
            });
        }
    }

    let batch = pact_targets.is_some_and(BTreeSet::is_empty);
    let accumulated = credits
        .iter()
        .map(|credit| {
            let should_skim =
                pact_targets.is_none_or(|targets| targets.contains(&credit.discord_id));
            let skimmed = if batch || !should_skim {
                0
            } else {
                apply_skim(credit.discord_id, credit.bonus)
            };
            AccumulatedAward {
                discord_id: credit.discord_id,
                net: credit.bonus - skimmed,
            }
        })
        .collect();

    Ok(MatchStreakAwardPlan {
        results,
        credits,
        accumulated,
        credit_mode: if batch {
            CreditMode::Batch
        } else {
            CreditMode::Individual
        },
    })
}

pub trait DotaStreakClock {
    fn unix_timestamp(&self) -> i64;
}

pub fn current_game_date(clock: &impl DotaStreakClock) -> Result<String, GameDateError> {
    game_date_for_timestamp(clock.unix_timestamp() as f64)
}

#[must_use]
pub fn game_date_for_naive_utc(datetime: NaiveDateTime) -> String {
    game_date_for(datetime)
}

#[must_use]
pub fn game_date_for_aware_utc(datetime: DateTime<Utc>) -> String {
    game_date_for(datetime)
}

pub fn previous_game_date(today: &str) -> Result<String, GameDateError> {
    yesterday_of(today)
}

#[cfg(test)]
#[path = "dota_streak/tests.rs"]
mod tests;
