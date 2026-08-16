//! Scratch parity harness for the 31 Python prestige-perk/orphan-perk tests.
//!
//! This file intentionally does not participate in the application module graph. The
//! pure policy below is a standalone extraction of the Python contract: the current
//! Rust prestige module is still unadmitted and cannot be imported from this crate
//! without its pending DB/crypto dependencies. Results here therefore validate the
//! contract and vectors, not production admission or provider wiring.

use std::collections::BTreeMap;

use serde_json::json;
use serde_json::{Map, Value};

const READING_STONE_SAFE_HINTS: [&str; 3] = [
    "The walls whisper of patience here.",
    "A familiar rhythm — caution holds today.",
    "Stillness gathers along the safer passage.",
];
const READING_STONE_RISKY_HINTS: [&str; 3] = [
    "The stones hum louder beside the bolder path.",
    "Something glints just past the edge of the dark.",
    "An unseen pull tugs you onward.",
];
const READING_STONE_DESPERATE_HINTS: [&str; 3] = [
    "Old bones remember reckless feet.",
    "The rock itself seems to dare you forward.",
    "A wild current beckons from the deepest dark.",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadingStoneDirection {
    Safe,
    Risky,
    Desperate,
}

impl ReadingStoneDirection {
    const fn hints(self) -> &'static [&'static str; 3] {
        match self {
            Self::Safe => &READING_STONE_SAFE_HINTS,
            Self::Risky => &READING_STONE_RISKY_HINTS,
            Self::Desperate => &READING_STONE_DESPERATE_HINTS,
        }
    }
}

fn prestige_perk_effects(perk: &str) -> &'static [(&'static str, f64)] {
    match perk {
        "advance_boost" => &[("advance_min_bonus", 1.0)],
        "cave_in_resistance" => &[("cave_in_reduction", 0.05)],
        "loot_multiplier" => &[("jc_bonus", 1.0)],
        "mixed_bonus" => &[
            ("advance_min_bonus", 0.5),
            ("cave_in_reduction", 0.02),
            ("jc_bonus", 0.5),
        ],
        "veteran_miner" => &[("risky_success_bonus", 0.05)],
        "tunnel_mastery" => &[("expedition_reward_bonus", 0.50)],
        "patient_step" => &[("streak_bonus_multiplier", 0.5)],
        "steady_hands" => &[("cave_in_loss_reduction", 0.25)],
        "reading_the_stone" => &[("event_choice_reveal", 1.0)],
        _ => &[],
    }
}

fn aggregate_prestige_perk_effects(perks: &[String]) -> BTreeMap<&'static str, f64> {
    let mut effects = BTreeMap::new();
    for perk in perks {
        for &(key, value) in prestige_perk_effects(perk) {
            *effects.entry(key).or_insert(0.0) += value;
        }
    }
    effects
}

fn event_has_risk(event: &Value) -> bool {
    let Some(event) = event.as_object() else {
        return false;
    };
    for option_key in ["safe_option", "risky_option", "desperate_option"] {
        let Some(option) = event.get(option_key).and_then(Value::as_object) else {
            continue;
        };
        if ["success", "failure"].into_iter().any(|outcome| {
            option
                .get(outcome)
                .and_then(Value::as_object)
                .is_some_and(outcome_has_risk)
        }) {
            return true;
        }
    }
    event
        .get("outcomes")
        .and_then(Value::as_object)
        .is_some_and(|outcomes| {
            outcomes
                .values()
                .filter_map(Value::as_object)
                .any(outcome_has_risk)
        })
}

fn outcome_has_risk(outcome: &Map<String, Value>) -> bool {
    ["jc", "advance"]
        .into_iter()
        .any(|key| outcome.get(key).is_some_and(has_negative_number))
        || outcome.get("cave_in").is_some_and(python_truthy)
        || outcome
            .get("streak_loss")
            .and_then(json_integer)
            .is_some_and(|loss| loss > 0)
        || outcome.get("curse").is_some_and(python_truthy)
}

fn has_negative_number(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_f64().is_some_and(|number| number < 0.0),
        Value::Array(values) => values
            .iter()
            .any(|value| value.as_f64().is_some_and(|number| number < 0.0)),
        _ => false,
    }
}

fn json_integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn reading_the_stone_direction(event: &Value) -> Option<ReadingStoneDirection> {
    let event = event.as_object()?;
    let mut best: Option<(ReadingStoneDirection, f64)> = None;
    for (key, direction) in [
        ("safe_option", ReadingStoneDirection::Safe),
        ("risky_option", ReadingStoneDirection::Risky),
        ("desperate_option", ReadingStoneDirection::Desperate),
    ] {
        let Some(option) = event.get(key).and_then(Value::as_object) else {
            continue;
        };
        let success_chance = option
            .get("success_chance")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        let success = option
            .get("success")
            .and_then(Value::as_object)
            .and_then(|outcome| outcome.get("jc"))
            .map_or(0.0, average_jc);
        let failure = option
            .get("failure")
            .and_then(Value::as_object)
            .and_then(|outcome| outcome.get("jc"))
            .map_or(0.0, average_jc);
        let expected_value = success_chance * success + (1.0 - success_chance) * failure;
        if best.is_none_or(|(_, best_value)| expected_value > best_value) {
            best = Some((direction, expected_value));
        }
    }
    best.map(|(direction, _)| direction)
}

fn reading_the_stone_hint(event: &Value, hint_choice: usize) -> Option<&'static str> {
    let hints = reading_the_stone_direction(event)?.hints();
    Some(hints[hint_choice % hints.len()])
}

fn average_jc(value: &Value) -> f64 {
    match value {
        Value::Number(value) => value.as_f64().unwrap_or(0.0),
        Value::Array(values) if !values.is_empty() => {
            values.iter().filter_map(Value::as_f64).sum::<f64>() / values.len() as f64
        }
        _ => 0.0,
    }
}

fn scratch_has_perk(tunnel_perks: Option<&[&str]>, perk: &str) -> bool {
    tunnel_perks.is_some_and(|perks| perks.contains(&perk))
}

#[test]
fn empty_perks_returns_empty_dict() {
    assert!(aggregate_prestige_perk_effects(&[]).is_empty());
}

#[test]
fn single_perk_returns_its_effects() {
    assert_eq!(
        aggregate_prestige_perk_effects(&["advance_boost".to_owned()]).get("advance_min_bonus"),
        Some(&1.0)
    );
}

#[test]
fn multiple_perks_sum_independently() {
    let effects = aggregate_prestige_perk_effects(
        &["advance_boost", "cave_in_resistance", "loot_multiplier"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    assert_eq!(effects.get("advance_min_bonus"), Some(&1.0));
    assert_eq!(effects.get("cave_in_reduction"), Some(&0.05));
    assert_eq!(effects.get("jc_bonus"), Some(&1.0));
}

#[test]
fn overlapping_perks_sum_same_key() {
    let effects = aggregate_prestige_perk_effects(
        &["advance_boost", "mixed_bonus"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    assert_eq!(effects.get("advance_min_bonus"), Some(&1.5));
    assert_eq!(effects.get("cave_in_reduction"), Some(&0.02));
    assert_eq!(effects.get("jc_bonus"), Some(&0.5));
}

#[test]
fn unknown_perk_contributes_nothing() {
    assert!(aggregate_prestige_perk_effects(&["does_not_exist".to_owned()]).is_empty());
}

#[test]
fn all_new_perks_have_values() {
    for perk in ["patient_step", "steady_hands", "reading_the_stone"] {
        assert!(!prestige_perk_effects(perk).is_empty());
    }
}

#[test]
fn patient_step_value() {
    assert_eq!(
        prestige_perk_effects("patient_step"),
        &[("streak_bonus_multiplier", 0.5)]
    );
}

#[test]
fn patient_step_aggregates() {
    assert_eq!(
        aggregate_prestige_perk_effects(&["patient_step".to_owned()])
            .get("streak_bonus_multiplier"),
        Some(&0.5)
    );
}

#[test]
fn steady_hands_value() {
    assert_eq!(
        prestige_perk_effects("steady_hands"),
        &[("cave_in_loss_reduction", 0.25)]
    );
}

#[test]
fn steady_hands_aggregates() {
    assert_eq!(
        aggregate_prestige_perk_effects(&["steady_hands".to_owned()]).get("cave_in_loss_reduction"),
        Some(&0.25)
    );
}

#[test]
fn reading_the_stone_value() {
    assert_eq!(
        prestige_perk_effects("reading_the_stone"),
        &[("event_choice_reveal", 1.0)]
    );
}

#[test]
fn reading_the_stone_aggregates() {
    assert_eq!(
        aggregate_prestige_perk_effects(&["reading_the_stone".to_owned()])
            .get("event_choice_reveal"),
        Some(&1.0)
    );
}

#[test]
fn mixed_bonus_rounds_advance_half_up() {
    let value = aggregate_prestige_perk_effects(&["mixed_bonus".to_owned()])["advance_min_bonus"];
    assert_eq!(value, 0.5);
    assert_eq!((value + 0.5) as i64, 1);
}

#[test]
fn mixed_bonus_rounds_jc_half_up() {
    let value = aggregate_prestige_perk_effects(&["mixed_bonus".to_owned()])["jc_bonus"];
    assert_eq!(value, 0.5);
    assert_eq!((value + 0.5) as i64, 1);
}

#[test]
fn advance_boost_plus_mixed_rounds_to_two() {
    let value = aggregate_prestige_perk_effects(
        &["advance_boost", "mixed_bonus"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )["advance_min_bonus"];
    assert_eq!(value, 1.5);
    assert_eq!((value + 0.5) as i64, 2);
}

#[test]
fn has_perk_returns_false_for_no_tunnel() {
    assert!(!scratch_has_perk(None, "advance_boost"));
}

#[test]
fn event_with_negative_failure_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"jc": -3}}
    })));
}

#[test]
fn event_with_only_positive_outcomes_is_not_risky() {
    assert!(!event_has_risk(&json!({
        "safe_option": {"success": {"jc": 2}, "failure": {"jc": 0}}
    })));
}

#[test]
fn event_with_negative_jc_range_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {
            "success": {"jc": [3, 8]},
            "failure": {"jc": [-5, -1]}
        }
    })));
}

#[test]
fn legacy_outcomes_with_negative_jc_is_risky() {
    assert!(event_has_risk(&json!({
        "outcomes": {"safe": {"jc": 1}, "risky": {"jc": -10}}
    })));
}

#[test]
fn negative_advance_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"advance": -1}}
    })));
}

#[test]
fn cave_in_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"cave_in": true}}
    })));
}

#[test]
fn streak_loss_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"streak_loss": 1}}
    })));
}

#[test]
fn curse_is_risky() {
    assert!(event_has_risk(&json!({
        "risky_option": {
            "success": {"jc": 5},
            "failure": {"curse": {"id": "bad_luck"}}
        }
    })));
}

#[test]
fn empty_event_is_not_risky() {
    assert!(!event_has_risk(&json!({})));
}

#[test]
fn reading_hint_is_none_without_options() {
    assert_eq!(reading_the_stone_hint(&json!({}), 0), None);
    assert_eq!(
        reading_the_stone_hint(&json!({"name": "no choices"}), 0),
        None
    );
}

#[test]
fn reading_hint_picks_safe_when_safe_has_higher_ev() {
    let event = json!({
        "safe_option": {
            "success_chance": 1.0,
            "success": {"jc": 10},
            "failure": {"jc": 0}
        },
        "risky_option": {
            "success_chance": 0.5,
            "success": {"jc": 5},
            "failure": {"jc": -10}
        }
    });
    assert_eq!(
        reading_the_stone_direction(&event),
        Some(ReadingStoneDirection::Safe)
    );
    assert!(READING_STONE_SAFE_HINTS.contains(&reading_the_stone_hint(&event, 0).unwrap()));
}

#[test]
fn reading_hint_picks_risky_when_risky_has_higher_ev() {
    let event = json!({
        "safe_option": {
            "success_chance": 1.0,
            "success": {"jc": 1},
            "failure": {"jc": 0}
        },
        "risky_option": {
            "success_chance": 0.9,
            "success": {"jc": 20},
            "failure": {"jc": 0}
        }
    });
    assert_eq!(
        reading_the_stone_direction(&event),
        Some(ReadingStoneDirection::Risky)
    );
    assert!(READING_STONE_RISKY_HINTS.contains(&reading_the_stone_hint(&event, 0).unwrap()));
}

#[test]
fn reading_hint_contains_no_numbers() {
    let events = [
        json!({
            "safe_option": {"success_chance": 1.0, "success": {"jc": 5}, "failure": {"jc": 0}},
            "risky_option": {"success_chance": 0.5, "success": {"jc": 10}, "failure": {"jc": -5}}
        }),
        json!({
            "desperate_option": {"success_chance": 0.3, "success": {"jc": 50}, "failure": {"jc": -20}}
        }),
    ];
    for event in events {
        let hint = reading_the_stone_hint(&event, 0).unwrap();
        assert!(!hint.chars().any(|character| character.is_ascii_digit()));
    }
}

#[test]
fn orphan_consumer_keys_are_exposed() {
    assert!(
        prestige_perk_effects("veteran_miner")
            .iter()
            .any(|(key, _)| *key == "risky_success_bonus")
    );
    assert!(
        prestige_perk_effects("tunnel_mastery")
            .iter()
            .any(|(key, _)| *key == "expedition_reward_bonus")
    );
    assert!(
        prestige_perk_effects("reading_the_stone")
            .iter()
            .any(|(key, _)| *key == "event_choice_reveal")
    );
}

#[test]
fn duplicate_stacks_sum() {
    let loot = aggregate_prestige_perk_effects(
        &["loot_multiplier", "loot_multiplier", "loot_multiplier"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    assert_eq!(loot.get("jc_bonus"), Some(&3.0));
    let veteran = aggregate_prestige_perk_effects(
        &["veteran_miner", "veteran_miner"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    assert_eq!(veteran.get("risky_success_bonus"), Some(&0.10));
}
