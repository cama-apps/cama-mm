use std::cell::Cell;

use super::*;

#[test]
fn test_regular_dig_payout_reflects_the_multiplier_once() {
    let calls = Cell::new(0);
    let outcome = regular_dig_reward(25, |reward| {
        calls.set(calls.get() + 1);
        reward * 2
    });
    assert_eq!(outcome.credited, 50);
    assert_eq!(outcome.event_adjustments, 1);
    assert_eq!(calls.get(), 1);
}

#[test]
fn test_deterministic_fallback_dig_reports_consumed() {
    let outcome = fallback_outcome(false);
    assert!(outcome.success);
    assert!(outcome.dig_consumed);
    assert!(!outcome.cave_in);
}

#[test]
fn test_dm_outcome_dig_reports_consumed() {
    let outcome = direct_message_outcome(false);
    assert!(outcome.success);
    assert!(outcome.dig_consumed);
    assert!(!outcome.cave_in);
}

#[test]
fn test_dm_cave_in_dig_reports_consumed() {
    let outcome = direct_message_outcome(true);
    assert!(outcome.success);
    assert!(outcome.dig_consumed);
    assert!(outcome.cave_in);
}

#[test]
fn test_protection_shield_block_conserves_jc() {
    let result = funded_protection(ProtectionSource::Shield, 20, 10);
    assert_eq!(result.actor_delta, -20);
    assert_eq!(result.target_delta, 10);
    assert!(result.net_delta() <= 0);
}

#[test]
fn test_manashop_ward_block_conserves_jc_on_every_call() {
    let mut actor = 200;
    let mut target = 0;
    for _ in 0..5 {
        let result = funded_protection(ProtectionSource::Ward, 20, 10);
        actor += result.actor_delta;
        target += result.target_delta;
        assert!(result.target_delta <= -result.actor_delta);
        assert!(result.net_delta() <= 0);
    }
    assert_eq!((actor, target), (100, 50));
}

#[test]
fn test_aegis_charge_block_conserves_jc() {
    let result = funded_protection(ProtectionSource::AegisCharge, 15, 100);
    assert_eq!(result.target_delta, 15);
    assert_eq!(result.net_delta(), 0);
}

#[test]
fn test_forfeit_is_charged_before_the_new_wager_is_validated() {
    let outcome = plan_duel_start(DuelStartRequest {
        balance: 1_000,
        stale: Some(StaleDuel {
            wager: 1_000,
            carried_wager: None,
        }),
        new_wager: 1_000,
    });
    assert_eq!(
        outcome,
        DuelStartOutcome::InsufficientAfterForfeit {
            balance_after_forfeit: 0,
            required: 1_000,
            cleared_carried_wager: false
        }
    );
}

#[test]
fn test_pinnacle_carry_is_not_charged_twice() {
    let outcome = plan_duel_start(DuelStartRequest {
        balance: 1_100,
        stale: Some(StaleDuel {
            wager: 500,
            carried_wager: Some(500),
        }),
        new_wager: 500,
    });
    assert_eq!(
        outcome,
        DuelStartOutcome::Started {
            balance_after_forfeit: 600,
            cleared_carried_wager: true
        }
    );
}

fn assert_paths_match(item: Consumable, cave_roll: f64) {
    let initial = ChargeColumns::default();
    let live = resolve_consumable_dig(ExecutionPath::Live, initial, item, cave_roll, 10_000);
    let deterministic = resolve_consumable_dig(
        ExecutionPath::Deterministic,
        initial,
        item,
        cave_roll,
        10_000,
    );
    assert_eq!(live, deterministic);
}

#[test]
fn test_charge_columns_match_live_099_no_cave_in_reinforcement() {
    assert_paths_match(Consumable::Reinforcement, 0.99);
}

#[test]
fn test_charge_columns_match_live_099_no_cave_in_sonar_pulse() {
    assert_paths_match(Consumable::SonarPulse, 0.99);
}

#[test]
fn test_charge_columns_match_live_099_no_cave_in_grappling_hook() {
    assert_paths_match(Consumable::GrapplingHook, 0.99);
}

#[test]
fn test_charge_columns_match_live_099_no_cave_in_void_bait() {
    assert_paths_match(Consumable::VoidBait, 0.99);
}

#[test]
fn test_charge_columns_match_live_099_no_cave_in_hard_hat() {
    assert_paths_match(Consumable::HardHat, 0.99);
}

#[test]
fn test_charge_columns_match_live_0001_cave_in_reinforcement() {
    assert_paths_match(Consumable::Reinforcement, 0.001);
}

#[test]
fn test_charge_columns_match_live_0001_cave_in_sonar_pulse() {
    assert_paths_match(Consumable::SonarPulse, 0.001);
}

#[test]
fn test_charge_columns_match_live_0001_cave_in_grappling_hook() {
    assert_paths_match(Consumable::GrapplingHook, 0.001);
}

#[test]
fn test_charge_columns_match_live_0001_cave_in_void_bait() {
    assert_paths_match(Consumable::VoidBait, 0.001);
}

#[test]
fn test_charge_columns_match_live_0001_cave_in_hard_hat() {
    assert_paths_match(Consumable::HardHat, 0.001);
}

#[test]
fn test_ward_block_leaves_the_target_still_sabotageable() {
    let ward = funded_protection(ProtectionSource::Ward, 20, 10);
    assert_eq!(ward.audit_action, "sabotage_ward_blocked");
    assert!(!sabotage_cooldown_consumed(ward.audit_action));
    assert!(sabotage_cooldown_consumed("sabotage"));
}
