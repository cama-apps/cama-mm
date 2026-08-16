//! Exact catalog counterparts for Python's pacing-v2 event tranche.

use std::collections::BTreeSet;

use cama_app::dig_loot::{CanonicalEventOutcome, canonical_event, canonical_event_catalog};

const PACING_V2_IDS: [&str; 20] = [
    "crimson_drizzle",
    "glyph_pulse",
    "mossfire_echo",
    "mana_fountain_crack",
    "the_hooked_stranger",
    "whispering_token",
    "the_beasts_pit",
    "aegis_denial",
    "sanguine_pact",
    "time_walker",
    "bounty_marker",
    "mages_archive",
    "phantom_strike",
    "silken_cocoon",
    "the_mothers_mark",
    "helltide_bell",
    "the_sundering",
    "the_black_kings_bargain",
    "the_old_gods_tongue",
    "crimson_rain_ladder",
];

fn carries_threat(outcome: &CanonicalEventOutcome) -> bool {
    outcome.cave_in
        || outcome.streak_loss > 0
        || outcome.curse.is_some()
        || outcome.jc < 0
        || outcome.advance < 0
}

#[test]
fn dig_pacing_v2_all_new_events_are_in_typed_random_catalog() {
    let ids = canonical_event_catalog()
        .iter()
        .map(|event| event.id.as_str())
        .collect::<BTreeSet<_>>();
    let missing = PACING_V2_IDS
        .iter()
        .copied()
        .filter(|id| !ids.contains(id))
        .collect::<Vec<_>>();

    assert!(missing.is_empty(), "missing pacing-v2 events: {missing:?}");
}

#[test]
fn dig_pacing_v2_all_new_events_are_resolvable_from_event_pool() {
    for event_id in PACING_V2_IDS {
        let event = canonical_event(event_id).expect("pacing-v2 event is addressable");
        assert_eq!(event.id, event_id);
        assert!(!event.name.is_empty());
        assert!(!event.descriptions.is_empty());
    }
}

#[test]
fn dig_pacing_v2_event_count_is_at_least_157() {
    assert!(canonical_event_catalog().len() >= 157);
}

#[test]
fn dig_pacing_v2_each_new_event_has_safe_and_risky_choices() {
    for event_id in PACING_V2_IDS {
        let event = canonical_event(event_id).expect("pacing-v2 event");
        assert!(
            event.safe_option.is_some(),
            "{event_id} missing safe option"
        );
        assert!(
            event.risky_option.is_some(),
            "{event_id} missing risky option"
        );
    }
}

#[test]
fn dig_pacing_v2_each_new_event_carries_a_threat() {
    for event_id in PACING_V2_IDS {
        let event = canonical_event(event_id).expect("pacing-v2 event");
        let risky = event.risky_option.as_ref().expect("risky option");
        let threatening =
            risky.failure.as_ref().is_none_or(carries_threat) || carries_threat(&risky.success);
        assert!(threatening, "{event_id}: risky branch carries no threat");
    }
}
