//! Exact catalog-policy counterparts for Python's themed negative-value events.

use cama_app::dig_loot::{
    CANONICAL_EVENT_NEGATIVE_JC_MULTIPLIER, CanonicalEventChoice, canonical_event,
};

fn effective_jc(jc: i64) -> f64 {
    if jc < 0 {
        jc as f64 * CANONICAL_EVENT_NEGATIVE_JC_MULTIPLIER
    } else {
        jc as f64
    }
}

fn branch_ev(choice: &CanonicalEventChoice) -> f64 {
    let mut expected = choice.success_chance * effective_jc(choice.success.jc);
    if let Some(failure) = &choice.failure {
        expected += (1.0 - choice.success_chance) * effective_jc(failure.jc);
    }
    expected
}

fn assert_guaranteed_safe_loss(event_id: &str) {
    let event = canonical_event(event_id).expect("themed event");
    let safe = event.safe_option.as_ref().expect("safe option");
    assert_eq!(safe.success_chance, 1.0);
    assert!(safe.failure.is_none());
    assert!(safe.success.jc < 0);
}

fn assert_every_branch_is_negative_ev(event_id: &str) {
    let event = canonical_event(event_id).expect("themed event");
    let safe = branch_ev(event.safe_option.as_ref().expect("safe option"));
    let risky = branch_ev(event.risky_option.as_ref().expect("risky option"));
    assert!(safe < 0.0, "{event_id} safe EV {safe} is not negative");
    assert!(risky < 0.0, "{event_id} risky EV {risky} is not negative");
}

#[test]
fn dig_themed_new_pressure_events_are_registered() {
    for event_id in [
        "second_life_ledger",
        "twin_scar_altar",
        "ember_clearinghouse",
    ] {
        assert!(canonical_event(event_id).is_some(), "missing {event_id}");
    }
}

#[test]
fn dig_themed_second_life_ledger_safe_branch_is_a_guaranteed_loss() {
    assert_guaranteed_safe_loss("second_life_ledger");
}

#[test]
fn dig_themed_twin_scar_altar_safe_branch_is_a_guaranteed_loss() {
    assert_guaranteed_safe_loss("twin_scar_altar");
}

#[test]
fn dig_themed_ember_clearinghouse_safe_branch_is_a_guaranteed_loss() {
    assert_guaranteed_safe_loss("ember_clearinghouse");
}

#[test]
fn dig_themed_second_life_ledger_is_negative_ev_on_every_branch() {
    assert_every_branch_is_negative_ev("second_life_ledger");
}

#[test]
fn dig_themed_twin_scar_altar_is_negative_ev_on_every_branch() {
    assert_every_branch_is_negative_ev("twin_scar_altar");
}

#[test]
fn dig_themed_sealed_reliquary_safe_branch_is_a_guaranteed_loss() {
    assert_guaranteed_safe_loss("sealed_reliquary");
}

#[test]
fn dig_themed_grenths_tithe_safe_branch_is_a_guaranteed_loss() {
    assert_guaranteed_safe_loss("grenths_tithe");
}

#[test]
fn dig_themed_sealed_reliquary_is_negative_ev_on_every_branch() {
    assert_every_branch_is_negative_ev("sealed_reliquary");
}

#[test]
fn dig_themed_grenths_tithe_is_negative_ev_on_every_branch() {
    assert_every_branch_is_negative_ev("grenths_tithe");
}

#[test]
fn dig_themed_trap_risky_headlines_still_outreward_safe() {
    for event_id in ["sealed_reliquary", "grenths_tithe"] {
        let event = canonical_event(event_id).expect("trap event");
        let safe = event.safe_option.as_ref().expect("safe option");
        let risky = event.risky_option.as_ref().expect("risky option");
        assert!(risky.success.jc > safe.success.jc);
    }
}
