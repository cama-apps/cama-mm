use std::collections::BTreeMap;

use crate::dig_service::PICKAXE_TIERS;
use serde_json::json;

use super::*;

/// Deterministic SplitMix64 entropy for the corruption policy.
struct SplitMixEntropy {
    state: u64,
}

impl SplitMixEntropy {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }
}

impl EntropyPort for SplitMixEntropy {
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

fn number_effect(effects: &BTreeMap<&'static str, EffectValue>, key: &str) -> Option<f64> {
    effects.get(key).and_then(|effect| effect.number())
}

fn assert_near(left: f64, right: f64) {
    assert!((left - right).abs() < 1e-12, "{left} != {right}");
}

fn perk_ids(perks: &[&str]) -> Vec<String> {
    perks.iter().map(|perk| (*perk).to_owned()).collect()
}

#[test]
fn test_max_prestige_is_20() {
    assert_eq!(MAX_PRESTIGE, 20);
}

#[test]
fn test_pickaxe_tiers_define_full_ladder() {
    assert_eq!(PICKAXE_TIERS.len(), 8);
    assert_eq!(PICKAXE_TIERS.last().unwrap().name, "Void-Touched");
    assert_eq!(PICKAXE_TIERS[5].name, "Stormrend");
}

#[test]
fn test_nine_prestige_perks() {
    assert_eq!(PRESTIGE_PERKS.len(), 12);
    for perk in [
        "deep_sight",
        "the_endless",
        "patient_step",
        "steady_hands",
        "reading_the_stone",
    ] {
        assert!(PRESTIGE_PERKS.contains(&perk));
    }
}

// tests/test_dig_perks.py::TestPerkEffectAggregation::test_empty_perks_returns_empty_dict
#[test]
fn dig_perks_empty_perks_returns_empty_effects() {
    assert!(aggregate_prestige_perk_effects(&[]).is_empty());
}

// tests/test_dig_perks.py::TestPerkEffectAggregation::test_single_perk_returns_its_effects
#[test]
fn dig_perks_single_perk_returns_its_effects() {
    assert_eq!(
        aggregate_prestige_perk_effects(&perk_ids(&["advance_boost"])),
        BTreeMap::from([("advance_min_bonus", 1.0)])
    );
}

// tests/test_dig_perks.py::TestPerkEffectAggregation::test_multiple_perks_sum_independently
#[test]
fn dig_perks_multiple_perks_sum_independently() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&[
        "advance_boost",
        "cave_in_resistance",
        "loot_multiplier",
    ]));
    assert_eq!(effects.get("advance_min_bonus"), Some(&1.0));
    assert_eq!(effects.get("cave_in_reduction"), Some(&0.05));
    assert_eq!(effects.get("jc_bonus"), Some(&1.0));
}

// tests/test_dig_perks.py::TestPerkEffectAggregation::test_overlapping_perks_sum_same_key
#[test]
fn dig_perks_overlapping_perks_sum_same_key() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&["advance_boost", "mixed_bonus"]));
    assert_near(effects["advance_min_bonus"], 1.5);
    assert_near(effects["cave_in_reduction"], 0.02);
    assert_near(effects["jc_bonus"], 0.5);
}

// tests/test_dig_perks.py::TestPerkEffectAggregation::test_unknown_perk_contributes_nothing
#[test]
fn dig_perks_unknown_perk_contributes_nothing() {
    assert!(aggregate_prestige_perk_effects(&perk_ids(&["does_not_exist"])).is_empty());
}

// tests/test_dig_perks.py::TestPerkEffectAggregation::test_all_new_perks_have_values
#[test]
fn dig_perks_all_new_perks_have_values() {
    for perk in ["patient_step", "steady_hands", "reading_the_stone"] {
        assert!(PRESTIGE_PERKS.contains(&perk));
        assert!(!prestige_perk_effects(perk).is_empty());
    }
}

// tests/test_dig_perks.py::TestPatientStep::test_patient_step_value
#[test]
fn dig_perks_patient_step_has_exact_value() {
    assert_eq!(
        prestige_perk_effects("patient_step"),
        [("streak_bonus_multiplier", 0.5)]
    );
}

// tests/test_dig_perks.py::TestPatientStep::test_aggregator_surfaces_streak_multiplier
#[test]
fn dig_perks_aggregator_surfaces_streak_multiplier() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&["patient_step"]));
    assert_eq!(effects.get("streak_bonus_multiplier"), Some(&0.5));
}

// tests/test_dig_perks.py::TestSteadyHands::test_steady_hands_value
#[test]
fn dig_perks_steady_hands_has_exact_value() {
    assert_eq!(
        prestige_perk_effects("steady_hands"),
        [("cave_in_loss_reduction", 0.25)]
    );
}

// tests/test_dig_perks.py::TestSteadyHands::test_aggregator_surfaces_loss_reduction
#[test]
fn dig_perks_aggregator_surfaces_loss_reduction() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&["steady_hands"]));
    assert_eq!(effects.get("cave_in_loss_reduction"), Some(&0.25));
}

// tests/test_dig_perks.py::TestReadingTheStone::test_reading_the_stone_value
#[test]
fn dig_perks_reading_the_stone_has_exact_value() {
    assert_eq!(
        prestige_perk_effects("reading_the_stone"),
        [("event_choice_reveal", 1.0)]
    );
}

// tests/test_dig_perks.py::TestReadingTheStone::test_aggregator_surfaces_reveal_flag
#[test]
fn dig_perks_aggregator_surfaces_reveal_flag() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&["reading_the_stone"]));
    assert!(effects["event_choice_reveal"] >= 1.0);
}

// tests/test_dig_perks.py::TestPerkFractionalRoundsHalfUp::test_mixed_bonus_alone_gives_one_advance
#[test]
fn dig_perks_mixed_bonus_alone_gives_one_advance() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&["mixed_bonus"]));
    assert_eq!(effects.get("advance_min_bonus"), Some(&0.5));
    assert_eq!(
        round_prestige_perk_bonus_half_up(effects["advance_min_bonus"]),
        1
    );
}

// tests/test_dig_perks.py::TestPerkFractionalRoundsHalfUp::test_mixed_bonus_alone_gives_one_jc
#[test]
fn dig_perks_mixed_bonus_alone_gives_one_jc() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&["mixed_bonus"]));
    assert_eq!(effects.get("jc_bonus"), Some(&0.5));
    assert_eq!(round_prestige_perk_bonus_half_up(effects["jc_bonus"]), 1);
}

// tests/test_dig_perks.py::TestPerkFractionalRoundsHalfUp::test_advance_boost_plus_mixed_stacks_to_two
#[test]
fn dig_perks_advance_boost_plus_mixed_stacks_to_two() {
    let effects = aggregate_prestige_perk_effects(&perk_ids(&["advance_boost", "mixed_bonus"]));
    assert_eq!(effects.get("advance_min_bonus"), Some(&1.5));
    assert_eq!(
        round_prestige_perk_bonus_half_up(effects["advance_min_bonus"]),
        2
    );
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_event_with_negative_failure_is_risky
#[test]
fn orphan_perks_event_with_negative_failure_is_risky() {
    let event = json!({
        "risky_option": {
            "success": {"jc": 5},
            "failure": {"jc": -3}
        }
    });
    assert!(event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_event_with_only_positive_outcomes_is_not_risky
#[test]
fn orphan_perks_event_with_only_positive_outcomes_is_not_risky() {
    let event = json!({
        "safe_option": {
            "success": {"jc": 2},
            "failure": {"jc": 0}
        }
    });
    assert!(!event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_event_with_negative_jc_range_is_risky
#[test]
fn orphan_perks_event_with_negative_jc_range_is_risky() {
    let event = json!({
        "risky_option": {
            "success": {"jc": [3, 8]},
            "failure": {"jc": [-5, -1]}
        }
    });
    assert!(event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_legacy_outcomes_format_with_negative
#[test]
fn orphan_perks_legacy_outcomes_format_with_negative_is_risky() {
    let event = json!({
        "outcomes": {
            "safe": {"jc": 1},
            "risky": {"jc": -10}
        }
    });
    assert!(event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure0]
#[test]
fn orphan_perks_negative_advance_is_risky() {
    let event = json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"advance": -1}}
    });
    assert!(event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure1]
#[test]
fn orphan_perks_cave_in_is_risky() {
    let event = json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"cave_in": true}}
    });
    assert!(event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure2]
#[test]
fn orphan_perks_streak_loss_is_risky() {
    let event = json!({
        "risky_option": {"success": {"jc": 5}, "failure": {"streak_loss": 1}}
    });
    assert!(event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure3]
#[test]
fn orphan_perks_curse_is_risky() {
    let event = json!({
        "risky_option": {
            "success": {"jc": 5},
            "failure": {"curse": {"id": "bad_luck"}}
        }
    });
    assert!(event_has_risk(&event));
}

// tests/test_orphan_perks.py::TestEventHasRisk::test_empty_event_is_not_risky
#[test]
fn orphan_perks_empty_event_is_not_risky() {
    assert!(!event_has_risk(&json!({})));
}

// tests/test_orphan_perks.py::TestReadingTheStoneHint::test_returns_none_for_event_with_no_options
#[test]
fn orphan_perks_reading_the_stone_returns_none_without_options() {
    assert_eq!(reading_the_stone_direction(&json!({})), None);
    assert_eq!(reading_the_stone_hint(&json!({}), 0), None);
    assert_eq!(
        reading_the_stone_hint(&json!({"name": "no choices"}), 1),
        None
    );
}

// tests/test_orphan_perks.py::TestReadingTheStoneHint::test_picks_safe_when_safe_has_higher_ev
#[test]
fn orphan_perks_reading_the_stone_picks_safe_when_safe_has_higher_ev() {
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
    assert!(READING_STONE_SAFE_HINTS.contains(&reading_the_stone_hint(&event, 1).unwrap()));
}

// tests/test_orphan_perks.py::TestReadingTheStoneHint::test_picks_risky_when_risky_has_higher_ev
#[test]
fn orphan_perks_reading_the_stone_picks_risky_when_risky_has_higher_ev() {
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
    assert!(READING_STONE_RISKY_HINTS.contains(&reading_the_stone_hint(&event, 2).unwrap()));
}

// tests/test_orphan_perks.py::TestReadingTheStoneHint::test_hint_contains_no_numbers
#[test]
fn orphan_perks_reading_the_stone_hints_contain_no_numbers() {
    let events = [
        json!({
            "safe_option": {
                "success_chance": 1.0,
                "success": {"jc": 5},
                "failure": {"jc": 0}
            },
            "risky_option": {
                "success_chance": 0.5,
                "success": {"jc": 10},
                "failure": {"jc": -5}
            }
        }),
        json!({
            "desperate_option": {
                "success_chance": 0.3,
                "success": {"jc": 50},
                "failure": {"jc": -20}
            }
        }),
    ];
    for event in &events {
        for hint_choice in 0..3 {
            let hint = reading_the_stone_hint(event, hint_choice).unwrap();
            assert!(!hint.chars().any(|character| character.is_ascii_digit()));
        }
    }
}

// tests/test_orphan_perks.py::TestTunnelMasteryAndVeteranMinerInjection::test_aggregator_exposes_orphan_keys
#[test]
fn orphan_perks_aggregator_exposes_exact_consumer_keys() {
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

// tests/test_orphan_perks.py::TestTunnelMasteryAndVeteranMinerInjection::test_aggregator_sums_stacks
#[test]
fn orphan_perks_aggregator_sums_duplicate_stacks() {
    let loot = aggregate_prestige_perk_effects(&perk_ids(&["loot_multiplier"; 3]));
    assert_eq!(loot.get("jc_bonus"), Some(&3.0));
    let veteran = aggregate_prestige_perk_effects(&perk_ids(&["veteran_miner"; 2]));
    assert_near(veteran["risky_success_bonus"], 0.10);
}

#[test]
fn test_crowns_for_all_levels() {
    assert_eq!(PRESTIGE_CROWNS.len(), MAX_PRESTIGE as usize + 1);
    assert!(PRESTIGE_CROWNS[0].is_empty());
    assert!(PRESTIGE_CROWNS[1..].iter().all(|crown| !crown.is_empty()));
}

#[test]
fn test_get_ascension_effects_level_0() {
    assert!(ascension_effects(0).is_empty());
}

#[test]
fn test_get_ascension_effects_level_1() {
    let effects = ascension_effects(1);
    assert!(!effects.contains_key("advance_penalty"));
    assert_near(number_effect(&effects, "jc_multiplier").unwrap(), 0.18);
}

#[test]
fn test_ascension_effects_cumulative() {
    let effects = ascension_effects(3);
    assert!(!effects.contains_key("advance_penalty"));
    assert_near(number_effect(&effects, "jc_multiplier").unwrap(), 0.18);
    assert_near(number_effect(&effects, "cave_in_bonus").unwrap(), 0.03);
    assert_near(
        number_effect(&effects, "event_chance_multiplier").unwrap(),
        0.20,
    );
    assert_near(
        number_effect(&effects, "luminosity_drain_multiplier").unwrap(),
        0.25,
    );
    assert_near(
        number_effect(&effects, "rare_event_multiplier").unwrap(),
        0.50,
    );
}

#[test]
fn test_jc_layer_penalty_stacks_from_p2_through_p6() {
    let expected = [
        (1, 0.0),
        (2, 0.03),
        (3, 0.05),
        (4, 0.10),
        (5, 0.17),
        (6, 0.22),
        (10, 0.22),
    ];
    for (level, expected_penalty) in expected {
        let penalty = number_effect(&ascension_effects(level), "jc_layer_penalty").unwrap_or(0.0);
        assert_near(penalty, expected_penalty);
    }
}

#[test]
fn test_boss_rage_ascension_has_no_dead_phase2_flag() {
    let boss_rage = ASCENSION_MODIFIERS[3];
    assert_eq!(boss_rage.name, "Boss Rage");
    assert!(
        !boss_rage
            .effects
            .iter()
            .any(|effect| effect.key == "boss_phase2")
    );
    assert_near(
        boss_rage
            .effects
            .iter()
            .find(|effect| effect.key == "boss_payout_multiplier")
            .unwrap()
            .value
            .number()
            .unwrap(),
        0.50,
    );
}

#[test]
fn test_no_corruption_below_p6() {
    let mut entropy = SplitMixEntropy::new(42);
    for level in 0..6 {
        assert_eq!(roll_corruption(level, &mut entropy), None);
    }
}

#[test]
fn test_corruption_at_p6() {
    let mut entropy = SplitMixEntropy::new(42);
    let result = roll_corruption(6, &mut entropy).unwrap();
    assert!(!result.id.is_empty());
    assert!(!result.description.is_empty());
    assert!(!result.effects.is_empty());
}

#[test]
fn test_corruption_effect_has_valid_fields() {
    for seed in 0..50 {
        let mut entropy = SplitMixEntropy::new(seed);
        let result = roll_corruption(8, &mut entropy).unwrap();
        assert!(!result.id.is_empty());
        assert_eq!(
            result.weird,
            result.id.starts_with("corrupt_") && CORRUPTION_WEIRD.contains(&result)
        );
    }
}

#[test]
fn test_corruption_weird_ratio() {
    let mut entropy = SplitMixEntropy::new(12_345);
    let weird_count = (0..500)
        .filter(|_| roll_corruption(6, &mut entropy).unwrap().weird)
        .count();
    assert!(
        (50..=175).contains(&weird_count),
        "weird ratio {weird_count}/500"
    );
}

#[test]
fn test_apply_mutation_effects() {
    let effects = mutation_effects(&[
        mutation_by_id("cave_in_loot").unwrap(),
        mutation_by_id("brittle_walls").unwrap(),
    ]);
    assert_near(
        number_effect(&effects, "cave_in_loot_chance").unwrap(),
        0.30,
    );
    assert_near(number_effect(&effects, "cave_in_loss_bonus").unwrap(), 2.0);
}

#[test]
fn test_apply_mutation_effects_stacks_numeric() {
    let effects = mutation_effects(&[
        mutation_by_id("event_magnet").unwrap(),
        mutation_by_id("treasure_sense").unwrap(),
    ]);
    assert_near(number_effect(&effects, "event_chance_bonus").unwrap(), 0.30);
    assert_near(
        number_effect(&effects, "artifact_chance_bonus").unwrap(),
        0.25,
    );
}

#[test]
fn test_get_mutations_empty() {
    assert!(mutations_from_json(None).is_empty());
    assert!(mutations_from_json(Some("")).is_empty());
}

#[test]
fn test_get_mutations_parses_json() {
    let result = mutations_from_json(Some(r#"[{"id": "cave_in_loot", "name": "Lucky Rubble"}]"#));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, "cave_in_loot");
}
