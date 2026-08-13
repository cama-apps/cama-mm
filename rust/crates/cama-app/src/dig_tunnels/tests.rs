use std::collections::BTreeMap;

use crate::dig_service::PICKAXE_TIERS;
use serde_json::json;

use super::*;

const GUILD_ID: GuildId = 12_345;
const DISCORD_ID: DiscordId = 10_001;
const NOW: i64 = 1_000_000;

type TestService = TunnelService<InMemoryTunnelRepository, SplitMixEntropy>;

fn key() -> TunnelKey {
    TunnelKey::new(DISCORD_ID, GUILD_ID)
}

fn tunnel_with_balance(balance: i64) -> Tunnel {
    let mut tunnel = Tunnel::new(key(), "Amber Descent", balance);
    tunnel.last_dig_at = Some(NOW);
    tunnel
}

fn ready_tunnel(prestige_level: i32) -> Tunnel {
    let mut tunnel = tunnel_with_balance(500);
    tunnel.prestige_level = prestige_level;
    tunnel.mark_prestige_ready();
    tunnel
}

fn service_with(tunnel: Tunnel) -> TestService {
    let mut repository = InMemoryTunnelRepository::default();
    repository.insert(tunnel);
    TunnelService::new(repository, SplitMixEntropy::new(42))
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
fn test_prestige_resets_depth() {
    let original = ready_tunnel(0);
    let mut service = service_with(original.clone());
    service.repository_mut().fail_next_atomic();
    assert!(service.prestige(key(), "advance_boost", None, NOW).is_err());
    assert_eq!(service.repository().tunnel(key()), Some(&original));

    let result = service
        .prestige(key(), "advance_boost", None, NOW)
        .expect("prestige should commit");
    assert_eq!(result.prestige_level, 1);
    assert_eq!(service.repository().tunnel(key()).unwrap().depth, 0);
    assert_eq!(service.repository().actions().len(), 1);
}

#[test]
fn test_prestige_keeps_pickaxe() {
    let mut tunnel = ready_tunnel(0);
    tunnel.pickaxe_tier = 1;
    let mut service = service_with(tunnel);
    service.prestige(key(), "advance_boost", None, NOW).unwrap();
    assert_eq!(service.repository().tunnel(key()).unwrap().pickaxe_tier, 1);
}

#[test]
fn test_prestige_adds_perk() {
    let mut service = service_with(ready_tunnel(0));
    let result = service.prestige(key(), "advance_boost", None, NOW).unwrap();
    assert_eq!(result.perk_chosen, "advance_boost");
    assert_eq!(
        service.repository().tunnel(key()).unwrap().prestige_perks,
        ["advance_boost"]
    );
}

#[test]
fn test_prestige_max() {
    let mut service = service_with(ready_tunnel(MAX_PRESTIGE));
    assert_eq!(
        service.prestige(key(), "advance_boost", None, NOW),
        Err(TunnelError::MaximumPrestige)
    );
}

#[test]
fn test_prestige_can_advance_beyond_p10_10_11() {
    let mut service = service_with(ready_tunnel(10));
    let result = service.prestige(key(), "advance_boost", None, NOW).unwrap();
    assert_eq!(result.prestige_level, 11);
    assert_eq!(
        service.repository().tunnel(key()).unwrap().prestige_level,
        11
    );
}

#[test]
fn test_prestige_can_advance_beyond_p10_19_20() {
    let mut service = service_with(ready_tunnel(19));
    let result = service.prestige(key(), "advance_boost", None, NOW).unwrap();
    assert_eq!(result.prestige_level, 20);
    assert_eq!(
        service.repository().tunnel(key()).unwrap().prestige_level,
        20
    );
}

#[test]
fn test_prestige_bosses_respawn() {
    let mut service = service_with(ready_tunnel(0));
    service.prestige(key(), "advance_boost", None, NOW).unwrap();
    let progress = &service.repository().tunnel(key()).unwrap().boss_progress;
    assert_eq!(progress.len(), BOSS_BOUNDARIES.len());
    assert!(
        progress
            .values()
            .all(|status| *status == BossStatus::Active)
    );
    assert!(!progress.contains_key(&PINNACLE_DEPTH));
}

#[test]
fn test_prestige_grants_jc_and_relic() {
    assert_eq!(PRESTIGE_RELICS.len(), 24);
    assert!(PRESTIGE_RELICS.iter().all(|relic| {
        matches!(relic.rarity, RelicRarity::Rare | RelicRarity::Legendary)
            && !TROPHY_RELIC_IDS.contains(&relic.id)
    }));
    let mut service = service_with(ready_tunnel(0));
    let balance_before = service.repository().tunnel(key()).unwrap().balance;
    let artifacts_before = service.repository().tunnel(key()).unwrap().artifacts.len();
    let result = service.prestige(key(), "advance_boost", None, NOW).unwrap();
    let tunnel = service.repository().tunnel(key()).unwrap();
    assert_eq!(
        tunnel.balance - balance_before,
        scale_positive_dig_jc(1_000)
    );
    assert_eq!(result.prestige_grant.jc, scale_positive_dig_jc(1_000));
    assert!(matches!(
        result.prestige_grant.relic.unwrap().rarity,
        RelicRarity::Rare | RelicRarity::Legendary
    ));
    assert_eq!(tunnel.artifacts.len(), artifacts_before + 1);
}

#[test]
fn test_abandon_refunds_10_percent() {
    let mut tunnel = tunnel_with_balance(100);
    tunnel.depth = 50;
    let mut service = service_with(tunnel);
    let before = service.repository().tunnel(key()).unwrap().balance;
    let result = service.abandon(key(), NOW).unwrap();
    let after = service.repository().tunnel(key()).unwrap().balance;
    assert_eq!(result.refund, scale_positive_dig_jc(5));
    assert_eq!(after - before, scale_positive_dig_jc(5));
    assert_eq!(
        service.repository().actions()[0].kind,
        TunnelActionKind::Abandon
    );
}

#[test]
fn test_abandon_min_depth_10() {
    let mut tunnel = tunnel_with_balance(100);
    tunnel.depth = 5;
    let mut service = service_with(tunnel);
    assert_eq!(
        service.abandon(key(), NOW),
        Err(TunnelError::TooShallowToAbandon)
    );
    assert_eq!(service.repository().tunnel(key()).unwrap().depth, 5);
}

#[test]
fn test_abandon_keeps_prestige() {
    let mut tunnel = tunnel_with_balance(100);
    tunnel.depth = 50;
    tunnel.prestige_level = 2;
    let mut service = service_with(tunnel);
    service.abandon(key(), NOW).unwrap();
    let tunnel = service.repository().tunnel(key()).unwrap();
    assert_eq!(tunnel.prestige_level, 2);
    assert_eq!(tunnel.depth, 0);
}

#[test]
fn test_help_returns_tunnel_name() {
    let service = service_with(tunnel_with_balance(100));
    let result = service.help_target(key()).unwrap();
    assert_eq!(result.tunnel_name, "Amber Descent");
}

#[test]
fn test_sabotage_returns_tunnel_name() {
    let mut tunnel = tunnel_with_balance(100);
    tunnel.depth = 30;
    let service = service_with(tunnel);
    let result = service.sabotage_target(key()).unwrap();
    assert_eq!(result.tunnel_name, "Amber Descent");
}

#[test]
fn test_get_flex_data_returns_tunnel_name() {
    let service = service_with(tunnel_with_balance(100));
    let result = service.flex_data(key()).unwrap();
    assert_eq!(result.tunnel_name, "Amber Descent");
}

#[test]
fn test_generate_clue_first_letter_uses_tunnel_name() {
    let service = service_with(tunnel_with_balance(100));
    let clue = service.first_letter_clue(key()).unwrap();
    assert!(clue.contains('A'));
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

// tests/test_dig_perks.py::TestHasPerk::test_returns_false_for_no_tunnel
#[test]
fn dig_perks_has_perk_returns_false_for_no_tunnel() {
    assert!(!owns_prestige_perk(None, "advance_boost"));
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
fn test_normalize_tunnel_casts_string_ints() {
    let mut row = BTreeMap::from([
        ("depth".to_owned(), RawTunnelValue::Text("49".to_owned())),
        (
            "luminosity".to_owned(),
            RawTunnelValue::Text("100".to_owned()),
        ),
        (
            "last_dig_at".to_owned(),
            RawTunnelValue::Text("1000000".to_owned()),
        ),
        (
            "boss_attempts".to_owned(),
            RawTunnelValue::Text("3".to_owned()),
        ),
        (
            "tunnel_name".to_owned(),
            RawTunnelValue::Text("Test Tunnel".to_owned()),
        ),
        (
            "boss_progress".to_owned(),
            RawTunnelValue::Text("{\"50\": \"active\"}".to_owned()),
        ),
    ]);
    normalize_tunnel_row(&mut row).unwrap();
    assert_eq!(row["depth"], RawTunnelValue::Integer(49));
    assert_eq!(row["luminosity"], RawTunnelValue::Integer(100));
    assert_eq!(row["last_dig_at"], RawTunnelValue::Integer(1_000_000));
    assert_eq!(row["boss_attempts"], RawTunnelValue::Integer(3));
    assert_eq!(
        row["tunnel_name"],
        RawTunnelValue::Text("Test Tunnel".to_owned())
    );
    assert_eq!(
        row["boss_progress"],
        RawTunnelValue::Text("{\"50\": \"active\"}".to_owned())
    );
}

#[test]
fn test_update_tunnel_accepts_last_cheer_at() {
    let mut repository = InMemoryTunnelRepository::default();
    repository.insert(tunnel_with_balance(100));
    repository.update_last_cheer_at(key(), 1_234_567).unwrap();
    assert_eq!(
        repository.tunnel(key()).unwrap().last_cheer_at,
        Some(1_234_567)
    );
}

#[test]
fn test_normalize_tunnel_handles_none_values() {
    let mut row = BTreeMap::from([
        ("depth".to_owned(), RawTunnelValue::Integer(10)),
        ("last_dig_at".to_owned(), RawTunnelValue::Null),
        ("luminosity".to_owned(), RawTunnelValue::Null),
    ]);
    normalize_tunnel_row(&mut row).unwrap();
    assert_eq!(row["depth"], RawTunnelValue::Integer(10));
    assert_eq!(row["last_dig_at"], RawTunnelValue::Null);
    assert_eq!(row["luminosity"], RawTunnelValue::Null);
}

#[test]
fn test_get_tunnel_returns_int_types() {
    let service = service_with(tunnel_with_balance(200));
    let tunnel = service.repository().tunnel(key()).unwrap();
    let depth: i64 = tunnel.depth;
    let luminosity: i64 = tunnel.luminosity;
    let last_dig_at: i64 = tunnel.last_dig_at.unwrap();
    assert_eq!((depth, luminosity, last_dig_at), (0, 100, NOW));
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
fn test_boss_phase2_at_prestige_2() {
    let result = regular_boss_win(2);
    assert!(result.phase_two_incoming);
    assert_eq!(result.phase, 1);
    assert_eq!(result.persisted_status, BossStatus::PhaseOneDefeated);
}

#[test]
fn test_boss_no_phase2_below_prestige_2() {
    let result = regular_boss_win(1);
    assert!(!result.phase_two_incoming);
    assert_eq!(result.phase, 1);
    assert_eq!(result.persisted_status, BossStatus::Defeated);
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
fn test_roll_mutations_returns_forced_and_choices() {
    let mut entropy = SplitMixEntropy::new(42);
    let rolled = roll_mutations_for_prestige(None, &mut entropy);
    assert!(!rolled.forced.id.is_empty());
    assert!(!rolled.forced.name.is_empty());
    assert!(!rolled.forced.description.is_empty());
    assert_eq!(rolled.choices.len(), 3);
    assert!(
        rolled
            .choices
            .iter()
            .all(|choice| !choice.id.is_empty() && !choice.name.is_empty())
    );
    assert!(!rolled.choices.contains(&rolled.forced));
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

#[test]
fn test_mutations_stored_in_prestige() {
    let mut service = service_with(ready_tunnel(7));
    let result = service.prestige(key(), "advance_boost", None, NOW).unwrap();
    assert_eq!(result.prestige_level, 8);
    assert!(result.mutations.is_some());
    assert!(
        !service
            .repository()
            .tunnel(key())
            .unwrap()
            .mutations
            .is_empty()
    );
}

#[test]
fn test_calculate_run_score_basic() {
    let mut tunnel = tunnel_with_balance(100);
    tunnel.depth = 100;
    tunnel.boss_progress.insert(25, BossStatus::Defeated);
    tunnel.boss_progress.insert(50, BossStatus::Defeated);
    tunnel.current_run_jc = 40;
    tunnel.current_run_events = 5;
    assert_eq!(calculate_run_score(&tunnel), 270);
}

#[test]
fn test_score_multiplier_at_higher_prestige() {
    let mut tunnel = tunnel_with_balance(100);
    tunnel.depth = 50;
    tunnel.boss_progress.insert(25, BossStatus::Defeated);
    tunnel.current_run_jc = 20;
    tunnel.current_run_artifacts = 1;
    tunnel.current_run_events = 3;
    let score_p0 = calculate_run_score(&tunnel);
    tunnel.prestige_level = 5;
    let score_p5 = calculate_run_score(&tunnel);
    assert!(score_p5 > score_p0);
    assert_eq!(score_p5, (score_p0 as f64 * 1.5) as i64);
}

#[test]
fn test_score_multiplier_p10_includes_ascension() {
    let mut tunnel = tunnel_with_balance(100);
    tunnel.depth = 100;
    tunnel.prestige_level = 10;
    assert_eq!(calculate_run_score(&tunnel), 400);
}

#[test]
fn test_prestige_stores_run_score() {
    let mut tunnel = ready_tunnel(0);
    tunnel.current_run_jc = 50;
    tunnel.current_run_artifacts = 3;
    tunnel.current_run_events = 10;
    let mut service = service_with(tunnel);
    let result = service.prestige(key(), "advance_boost", None, NOW).unwrap();
    assert!(result.run_score > 0);
    assert!(result.best_run_score > 0);
    let tunnel = service.repository().tunnel(key()).unwrap();
    assert_eq!(tunnel.current_run_jc, 0);
    assert_eq!(tunnel.current_run_artifacts, 0);
    assert_eq!(tunnel.current_run_events, 0);
}

#[test]
fn test_hall_of_fame_empty_guild() {
    let service = TunnelService::new(
        InMemoryTunnelRepository::default(),
        SplitMixEntropy::new(42),
    );
    assert!(service.hall_of_fame(GUILD_ID).is_empty());
}

#[test]
fn test_hall_of_fame_after_prestige() {
    let mut tunnel = ready_tunnel(0);
    tunnel.current_run_jc = 30;
    tunnel.current_run_artifacts = 2;
    tunnel.current_run_events = 5;
    let mut service = service_with(tunnel);
    service.prestige(key(), "advance_boost", None, NOW).unwrap();
    let entries = service.hall_of_fame(GUILD_ID);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].discord_id, DISCORD_ID);
    assert!(entries[0].best_run_score > 0);
}

#[test]
fn test_same_attempt_rolls_the_same_mutations() {
    let seed = prestige_mutation_seed(4_242, 99, 8);
    let mut first_entropy = SplitMixEntropy::new(1);
    let mut second_entropy = SplitMixEntropy::new(999);
    let first = roll_mutations_for_prestige(Some(&seed), &mut first_entropy);
    let second = roll_mutations_for_prestige(Some(&seed), &mut second_entropy);
    assert_eq!(first, second);
}

#[test]
fn test_different_players_and_levels_roll_independently() {
    let seeds = [
        prestige_mutation_seed(4_242, 99, 8),
        prestige_mutation_seed(4_243, 99, 8),
        prestige_mutation_seed(4_242, 99, 9),
    ];
    let rolls: Vec<_> = seeds
        .iter()
        .map(|seed| {
            let mut entropy = SplitMixEntropy::new(0);
            let rolled = roll_mutations_for_prestige(Some(seed), &mut entropy);
            (
                rolled.forced.id,
                rolled
                    .choices
                    .iter()
                    .map(|choice| choice.id)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_ne!(rolls[0], rolls[1]);
    assert_ne!(rolls[0], rolls[2]);
    assert_ne!(rolls[1], rolls[2]);
}

#[test]
fn test_unseeded_roll_still_varies() {
    let mut entropy = SplitMixEntropy::new(7);
    let mut forced_ids = Vec::new();
    for _ in 0..40 {
        let forced = roll_mutations_for_prestige(None, &mut entropy).forced.id;
        if !forced_ids.contains(&forced) {
            forced_ids.push(forced);
        }
    }
    assert!(forced_ids.len() > 1);
}
