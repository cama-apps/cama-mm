use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, TimeZone, Utc};

use super::*;

#[test]
fn test_unified_schedule_values() {
    assert_eq!(DOTA_STREAK_SCHEDULE, [(3, 1), (7, 3), (14, 6), (30, 10)]);
    assert_eq!(DOTA_STREAK_SCHEDULE, crate::dig_service::STREAKS);
}

#[test]
fn test_streak_bonus_for_each_tier_0_0() {
    assert_eq!(dota_streak_bonus(0), 0);
}

#[test]
fn test_streak_bonus_for_each_tier_1_0() {
    assert_eq!(dota_streak_bonus(1), 0);
}

#[test]
fn test_streak_bonus_for_each_tier_2_0() {
    assert_eq!(dota_streak_bonus(2), 0);
}

#[test]
fn test_streak_bonus_for_each_tier_3_1() {
    assert_eq!(dota_streak_bonus(3), 1);
}

#[test]
fn test_streak_bonus_for_each_tier_6_1() {
    assert_eq!(dota_streak_bonus(6), 1);
}

#[test]
fn test_streak_bonus_for_each_tier_7_3() {
    assert_eq!(dota_streak_bonus(7), 3);
}

#[test]
fn test_streak_bonus_for_each_tier_13_3() {
    assert_eq!(dota_streak_bonus(13), 3);
}

#[test]
fn test_streak_bonus_for_each_tier_14_6() {
    assert_eq!(dota_streak_bonus(14), 6);
}

#[test]
fn test_streak_bonus_for_each_tier_29_6() {
    assert_eq!(dota_streak_bonus(29), 6);
}

#[test]
fn test_streak_bonus_for_each_tier_30_10() {
    assert_eq!(dota_streak_bonus(30), 10);
}

#[test]
fn test_streak_bonus_for_each_tier_55_10() {
    assert_eq!(dota_streak_bonus(55), 10);
}

#[test]
fn test_bulk_advance_keeps_per_participant_bonus_and_skim_handling() {
    let targets = BTreeSet::from([10, 20]);
    let skims = RefCell::new(vec![1, 1, 0].into_iter());
    let calls = RefCell::new(Vec::new());
    let plan = plan_match_streak_awards(
        &[20, 10, 20, 99_999],
        &[3, 7, 3, 0],
        Some(&targets),
        |discord_id, bonus| {
            calls.borrow_mut().push((discord_id, bonus));
            skims
                .borrow_mut()
                .next()
                .expect("one skim result per award")
        },
    )
    .expect("aligned streak results");

    assert_eq!(plan.credit_mode, CreditMode::Individual);
    assert_eq!(
        plan.credits,
        vec![
            CreditInstruction {
                discord_id: 20,
                streak_days: 3,
                bonus: 1,
            },
            CreditInstruction {
                discord_id: 10,
                streak_days: 7,
                bonus: 3,
            },
            CreditInstruction {
                discord_id: 20,
                streak_days: 3,
                bonus: 1,
            },
        ]
    );
    assert_eq!(*calls.borrow(), vec![(20, 1), (10, 3), (20, 1)]);
    assert_eq!(
        plan.accumulated,
        vec![
            AccumulatedAward {
                discord_id: 20,
                net: 0,
            },
            AccumulatedAward {
                discord_id: 10,
                net: 2,
            },
            AccumulatedAward {
                discord_id: 20,
                net: 1,
            },
        ]
    );
    assert_eq!(
        plan.results,
        BTreeMap::from([
            (10, StreakAwardResult { days: 7, bonus: 3 }),
            (20, StreakAwardResult { days: 3, bonus: 1 }),
            (99_999, StreakAwardResult { days: 0, bonus: 0 }),
        ])
    );
}

#[test]
fn test_no_blood_pact_batches_credits_with_per_award_metadata() {
    let targets = BTreeSet::new();
    let plan = plan_match_streak_awards(
        &[20, 10, 20, 99_999],
        &[3, 7, 3, 0],
        Some(&targets),
        |_discord_id, _bonus| panic!("batch path must not invoke Blood Pact"),
    )
    .expect("aligned streak results");

    assert_eq!(plan.credit_mode, CreditMode::Batch);
    assert_eq!(
        plan.credits
            .iter()
            .map(|credit| (
                credit.discord_id,
                credit.bonus,
                (credit.streak_days, credit.bonus)
            ))
            .collect::<Vec<_>>(),
        vec![(20, 1, (3, 1)), (10, 3, (7, 3)), (20, 1, (3, 1))]
    );
    assert_eq!(
        plan.accumulated,
        vec![
            AccumulatedAward {
                discord_id: 20,
                net: 1,
            },
            AccumulatedAward {
                discord_id: 10,
                net: 3,
            },
            AccumulatedAward {
                discord_id: 20,
                net: 1,
            },
        ]
    );
}

#[test]
fn test_yesterday_of_basic() {
    assert_eq!(
        previous_game_date("2026-05-07").expect("valid date"),
        "2026-05-06"
    );
    assert_eq!(
        previous_game_date("2026-01-01").expect("valid date"),
        "2025-12-31"
    );
}

struct FixedClock(i64);

impl DotaStreakClock for FixedClock {
    fn unix_timestamp(&self) -> i64 {
        self.0
    }
}

#[test]
fn test_get_game_date_uses_pst_4am_rollover() {
    assert_eq!(
        current_game_date(&FixedClock(1_778_153_400)).expect("valid timestamp"),
        "2026-05-06"
    );
}

#[test]
fn test_game_date_for_handles_naive_dt() {
    let datetime = NaiveDate::from_ymd_opt(2026, 5, 7)
        .expect("valid date")
        .and_hms_opt(11, 30, 0)
        .expect("valid time");
    assert_eq!(game_date_for_naive_utc(datetime), "2026-05-06");
}

#[test]
fn test_game_date_for_handles_aware_dt() {
    let datetime = Utc
        .with_ymd_and_hms(2026, 5, 7, 18, 0, 0)
        .single()
        .expect("valid UTC datetime");
    assert_eq!(game_date_for_aware_utc(datetime), "2026-05-07");
}
