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
