use std::collections::BTreeSet;

use super::*;
use crate::dig_loot::artifact_catalog;

fn close(left: f64, right: f64) {
    assert!((left - right).abs() < 1.0e-10, "{left} != {right}");
}

fn round(
    round_num: i64,
    player_hp: i64,
    boss_hp: i64,
    player_hit: f64,
    player_damage: i64,
    boss_hit: f64,
    boss_damage: i64,
) -> RelicRoundInput {
    RelicRoundInput {
        round_num,
        player_hp,
        boss_hp,
        player_hit,
        player_damage,
        boss_hit,
        boss_damage,
    }
}

#[test]
fn test_new_relics_present_in_catalog() {
    let ids = relic_catalog()
        .into_iter()
        .map(|relic| relic.id)
        .collect::<BTreeSet<_>>();
    assert!(NEW_RELIC_IDS.iter().all(|id| ids.contains(id)));
}

#[test]
fn test_slow_drip_tracks_gross_cap_but_credits_scaled_reward() {
    let claim = settle_slow_drip_claim(20, 0, 2.0);
    assert_eq!(claim.gross_jc, 10);
    assert_eq!(claim.credit_jc, 13);
    assert_eq!(claim.claimed_after, 10);

    let capped = settle_slow_drip_claim(400, 95, 2.0);
    assert_eq!(capped.gross_jc, 5);
    assert_eq!(capped.credit_jc, 7);
    assert_eq!(capped.claimed_after, 100);
}

#[test]
fn test_dormant_relics_present_in_catalog() {
    let ids = relic_catalog()
        .into_iter()
        .map(|relic| relic.id)
        .collect::<BTreeSet<_>>();
    assert!(DORMANT_RELIC_IDS.iter().all(|id| ids.contains(id)));
}

#[test]
fn test_new_relics_have_lore_and_effects() {
    for relic_id in NEW_RELIC_IDS {
        let content = relic_content(relic_id).expect("new relic content");
        assert!(content.definition.is_relic);
        assert!(!content.text.effect.is_empty());
        assert!(!content.text.lore.is_empty());
    }
}

#[test]
fn test_total_artifact_count_matches_expected() {
    assert_eq!(relic_catalog().len(), 45);
    assert_eq!(artifact_catalog().len(), 47);
}

#[test]
fn test_post_pinnacle_decay_factor_root_network_slows_decay() {
    let depth = PINNACLE_DEPTH + 100;
    let base = post_pinnacle_decay_factor(depth, &RelicSet::default());
    let root = post_pinnacle_decay_factor(depth, &RelicSet::new(["root_network"]));
    let frozen = post_pinnacle_decay_factor(depth, &RelicSet::new(["frozen_clock"]));
    close(base, 0.80);
    close(root, 0.85);
    close(frozen, 0.90);
    assert!(root > base);
    assert!(frozen > root);
}

#[test]
fn test_relic_yield_helper_echo_lantern_multiplies_jc() {
    let mut entropy = ScriptedRelicEntropy::constant(1.0);
    let multiplier = relic_jc_yield_multiplier(
        &RelicSet::new(["echo_lantern"]),
        YieldContext {
            include_random: false,
            ..YieldContext::default()
        },
        &mut entropy,
    );
    close(multiplier, 1.15);
}

#[test]
fn test_relic_yield_helper_stormcaller_storm() {
    let relics = RelicSet::new(["stormcaller"]);
    let mut entropy = ScriptedRelicEntropy::constant(1.0);
    for (weather, expected) in [("storm", 1.5), ("sunny", 1.10), ("rain", 1.0)] {
        close(
            relic_jc_yield_multiplier(
                &relics,
                YieldContext {
                    weather_code: Some(weather),
                    include_random: false,
                    ..YieldContext::default()
                },
                &mut entropy,
            ),
            expected,
        );
    }
}

#[test]
fn test_relic_storm_negates_hazard_only_during_storm() {
    let relics = RelicSet::new(["stormcaller"]);
    assert!(storm_negates_hazard(&relics, Some("storm")));
    assert!(!storm_negates_hazard(&relics, Some("sunny")));
    assert!(!storm_negates_hazard(&relics, None));
}

#[test]
fn test_relic_yield_helper_bloodstone_random_only_when_enabled() {
    let relics = RelicSet::new(["bloodstone"]);
    let mut deterministic_entropy = ScriptedRelicEntropy::new([0.0]);
    close(
        relic_jc_yield_multiplier(
            &relics,
            YieldContext {
                include_random: false,
                ..YieldContext::default()
            },
            &mut deterministic_entropy,
        ),
        1.0,
    );
    let mut entropy = ScriptedRelicEntropy::new([0.1, 0.9]);
    let first = relic_jc_yield_multiplier(
        &relics,
        YieldContext {
            include_random: true,
            ..YieldContext::default()
        },
        &mut entropy,
    );
    let second = relic_jc_yield_multiplier(
        &relics,
        YieldContext {
            include_random: true,
            ..YieldContext::default()
        },
        &mut entropy,
    );
    close(first, 1.5);
    close(second, 0.75);
}

#[test]
fn test_buff_fun_relics_present_with_lore_and_effects() {
    for relic_id in BUFF_FUN_RELIC_IDS {
        let content = relic_content(relic_id).expect("buff-fun relic content");
        assert!(content.definition.is_relic);
        assert!(!content.text.effect.is_empty());
        assert!(!content.text.lore.is_empty());
    }
}

#[test]
fn test_deaths_door_renamed_off_the_second_wind_mutation() {
    assert!(
        artifact_catalog()
            .iter()
            .all(|artifact| artifact.id != "second_wind")
    );
    assert_eq!(
        artifact_catalog()
            .into_iter()
            .find(|artifact| artifact.id == "deaths_door")
            .expect("Death's Door")
            .name,
        "Death's Door"
    );
}

#[test]
fn test_deaths_door_is_the_nameless_depth_trophy() {
    assert!(TROPHY_RELIC_IDS.contains(&"deaths_door"));
    assert_eq!(trophy_relic_for_boss("nameless_depth"), Some("deaths_door"));
}

#[test]
fn test_relic_slot_cap_has_a_ceiling() {
    assert_eq!(relic_slot_cap(0), 1);
    assert_eq!(relic_slot_cap(3), 4);
    assert_eq!(relic_slot_cap(5), 6);
    assert_eq!(relic_slot_cap(6), 6);
    assert_eq!(relic_slot_cap(50), 6);
}

#[test]
fn test_midas_splinter_doubles_on_proc() {
    let relics = RelicSet::new(["midas_splinter"]);
    let context = YieldContext {
        include_random: true,
        ..YieldContext::default()
    };
    close(
        relic_jc_yield_multiplier(&relics, context, &mut ScriptedRelicEntropy::constant(0.01)),
        2.0,
    );
    close(
        relic_jc_yield_multiplier(&relics, context, &mut ScriptedRelicEntropy::constant(0.5)),
        1.0,
    );
}

#[test]
fn test_lucky_seam_jackpot_on_proc() {
    let relics = RelicSet::new(["lucky_seam"]);
    let context = YieldContext {
        include_random: true,
        ..YieldContext::default()
    };
    close(
        relic_jc_yield_multiplier(&relics, context, &mut ScriptedRelicEntropy::constant(0.001)),
        10.0,
    );
    close(
        relic_jc_yield_multiplier(&relics, context, &mut ScriptedRelicEntropy::constant(0.5)),
        1.0,
    );
}

#[test]
fn test_first_light_doubles_first_dig_of_day() {
    let relics = RelicSet::new(["first_light"]);
    let mut entropy = ScriptedRelicEntropy::constant(1.0);
    close(
        relic_jc_yield_multiplier(
            &relics,
            YieldContext {
                is_first_dig_today: true,
                include_random: false,
                ..YieldContext::default()
            },
            &mut entropy,
        ),
        2.0,
    );
    close(
        relic_jc_yield_multiplier(
            &relics,
            YieldContext {
                is_first_dig_today: false,
                include_random: false,
                ..YieldContext::default()
            },
            &mut entropy,
        ),
        1.0,
    );
}

#[test]
fn test_is_first_dig_of_day() {
    let now = 1_700_000_000;
    assert!(is_first_dig_of_day(None, now));
    assert!(!is_first_dig_of_day(Some(now), now));
    assert!(is_first_dig_of_day(Some(now - 3 * 86_400), now));
}

// tests/test_dig_new_relic_effects.py::test_lantern_stub_restores_five_on_first_daily_dig
#[test]
fn test_lantern_stub_restores_five_on_first_daily_dig() {
    let relics = RelicSet::new(["lantern_stub"]);
    let first = apply_lantern_stub_restore(
        &relics,
        LanternStubRestoreInput {
            luminosity_after: 60,
            last_dig_at: None,
            lantern_stub_date: None,
            today: "2026-07-13",
        },
    );
    assert_eq!(
        first,
        LanternStubRestore {
            luminosity_after: 65,
            restored: 5,
            lantern_stub_date: Some("2026-07-13".to_owned()),
        }
    );

    // A failed-dig retry may still have no last_dig_at, so the returned date
    // stamp must independently close the same-day gate.
    assert_eq!(
        apply_lantern_stub_restore(
            &relics,
            LanternStubRestoreInput {
                luminosity_after: first.luminosity_after,
                last_dig_at: None,
                lantern_stub_date: first.lantern_stub_date.as_deref(),
                today: "2026-07-13",
            },
        ),
        LanternStubRestore {
            luminosity_after: 65,
            restored: 0,
            lantern_stub_date: None,
        }
    );
}

// tests/test_dig_new_relic_effects.py::test_bone_abacus_strengthens_stamina_discount_and_taxes_paid_yield
#[test]
fn test_bone_abacus_strengthens_stamina_discount_and_taxes_paid_yield() {
    let relics = RelicSet::new(["bone_abacus"]);
    assert_eq!(relic_aware_paid_cost(100, 5, &relics), 75);
    let multiplier = relic_jc_yield_multiplier(
        &relics,
        YieldContext {
            is_paid_dig: true,
            include_random: false,
            ..YieldContext::default()
        },
        &mut ScriptedRelicEntropy::constant(1.0),
    );
    close(multiplier, 0.90);
}

// tests/test_dig_new_relic_effects.py::test_shifting_idol_rolls_a_fresh_attempt_bonus
#[test]
fn test_shifting_idol_rolls_a_fresh_attempt_bonus() {
    let relics = RelicSet::new(["shifting_idol"]);
    let mut entropy = ScriptedRelicEntropy::new([0.0, 0.99]);
    let first = apply_shifting_idol_stats(&relics, 5, 0.60, 0.10, &mut entropy);
    let second = apply_shifting_idol_stats(&relics, 5, 0.60, 0.10, &mut entropy);

    assert_eq!(
        first,
        ShiftingIdolStats {
            player_hp: 6,
            player_hit: 0.60,
            crit_chance: 0.10,
            bonus: Some(ShiftingIdolBonus::Hp),
        }
    );
    assert_eq!(second.player_hp, 5);
    close(second.player_hit, 0.60);
    close(second.crit_chance, 0.15);
    assert_eq!(second.bonus, Some(ShiftingIdolBonus::Crit));
}

#[test]
fn test_gamblers_edge_doubles_a_hit() {
    let mut status = relic_combat_status(&RelicSet::new(["gamblers_edge"]));
    assert!(status.double_hit_enabled);
    let result = run_relic_round(
        round(1, 10, 100, 1.0, 2, 0.0, 1),
        &mut status,
        &mut ScriptedRelicEntropy::constant(0.0),
    );
    assert_eq!(result.boss_hp, 96);
    assert!(result.events.double_hit);
}

#[test]
fn test_berserkers_mark_ramps_and_caps() {
    let mut status = relic_combat_status(&RelicSet::new(["berserkers_mark"]));
    let mut entropy = ScriptedRelicEntropy::constant(0.0);
    let first = run_relic_round(round(1, 20, 100, 1.0, 2, 1.0, 1), &mut status, &mut entropy);
    let second = run_relic_round(
        round(2, first.player_hp, first.boss_hp, 1.0, 2, 1.0, 1),
        &mut status,
        &mut entropy,
    );
    let third = run_relic_round(
        round(3, second.player_hp, second.boss_hp, 1.0, 2, 1.0, 1),
        &mut status,
        &mut entropy,
    );
    assert_eq!(
        (
            100 - first.boss_hp,
            first.boss_hp - second.boss_hp,
            second.boss_hp - third.boss_hp,
        ),
        (2, 3, 4)
    );
    assert_eq!(status.berserk_rage, Some(2));
}

#[test]
fn test_deaths_door_survives_once_then_dies() {
    let mut status = relic_combat_status(&RelicSet::new(["deaths_door"]));
    assert!(status.deaths_door_available);
    let mut entropy = ScriptedRelicEntropy::constant(0.0);
    let first = run_relic_round(round(1, 1, 100, 0.0, 2, 1.0, 5), &mut status, &mut entropy);
    assert_eq!(first.terminal, None);
    assert_eq!(first.player_hp, 1);
    assert!(first.events.deaths_door);
    assert!(!status.deaths_door_available);
    let second = run_relic_round(
        round(2, 1, first.boss_hp, 0.0, 2, 1.0, 5),
        &mut status,
        &mut entropy,
    );
    assert_eq!(second.terminal, Some(RelicRoundTerminal::PlayerLost));
    assert!(second.player_hp <= 0);
}

#[test]
fn test_deaths_door_saves_from_a_dot_death() {
    let mut status = relic_combat_status(&RelicSet::new(["deaths_door"]));
    status.bleed_rounds_remaining = 1;
    let result = run_relic_round(
        round(1, 1, 100, 1.0, 2, 0.0, 1),
        &mut status,
        &mut ScriptedRelicEntropy::constant(0.0),
    );
    assert_eq!(result.terminal, None);
    assert_eq!(result.player_hp, 1);
    assert!(result.events.deaths_door);
    assert_eq!(result.events.player_hp, 1);
    assert!(!status.deaths_door_available);
}

#[test]
fn test_gamblers_edge_does_not_double_when_roll_misses() {
    let mut status = relic_combat_status(&RelicSet::new(["gamblers_edge"]));
    let result = run_relic_round(
        round(1, 10, 100, 1.0, 2, 0.0, 1),
        &mut status,
        &mut ScriptedRelicEntropy::constant(0.99),
    );
    assert_eq!(result.boss_hp, 98);
    assert!(!result.events.double_hit);
}
