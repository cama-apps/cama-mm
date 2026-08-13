// Unadmitted parity tests for the pure-policy Dig tranche.
//
// This file is intentionally not declared from `cama-app/src/lib.rs` yet.
// The parity admission step can attach it once the owner is ready.  Every
// test has a one-to-one Python source node; ignored tests are concrete API
// gaps, not aliases or catalog-only substitutes.

use std::collections::BTreeSet;

use cama_domain::dig_gear::{GearLoadout, GearPiece, unique_gear};
use serde_json::json;

use crate::boss_multi_tier::{
    CombatEffects, CombatState, Combatant, MechanicOption, MechanicStatusEffect, OutcomeRoll,
    SequenceEntropy, apply_option_outcome, run_combat_round,
};
use crate::dig_loot::{
    CanonicalEventOutcome, CanonicalEventPolicy, CanonicalEventResolution, CanonicalEventRolls,
    CanonicalReward, canonical_event, canonical_event_catalog, resolve_canonical_event_with_policy,
    select_canonical_reward,
};
use crate::dig_prestige_runtime::{
    event_has_risk, reading_the_stone_direction, reading_the_stone_hint,
};

fn policy() -> CanonicalEventPolicy {
    CanonicalEventPolicy {
        luminosity: 100,
        ..CanonicalEventPolicy::default()
    }
}

fn rolls(success_roll: f64) -> CanonicalEventRolls {
    CanonicalEventRolls {
        success_roll,
        ..CanonicalEventRolls::default()
    }
}

fn resolution_outcome(resolution: &CanonicalEventResolution) -> CanonicalEventOutcome {
    CanonicalEventOutcome {
        description: resolution.message.clone(),
        advance: resolution.advance,
        jc: resolution.economy_gross_jc,
        cave_in: resolution.cave_in,
        streak_loss: resolution.streak_loss,
        curse: resolution.curse.clone(),
        gear_reward_pool: resolution.gear_reward_pool.clone(),
        consumable_reward_pool: resolution.consumable_reward_pool.clone(),
        artifact_reward_pool: resolution.artifact_reward_pool.clone(),
    }
}

fn combat(effects: CombatEffects) -> CombatState {
    CombatState {
        player_hp: 5,
        boss_hp: 5,
        player_hit: 0.0,
        player_damage: 1,
        boss_hit: 0.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects,
    }
}

fn one_unique_piece(item_id: &str, durability: i32) -> GearPiece {
    let definition = unique_gear(item_id).expect("known unique gear");
    GearPiece {
        id: 1,
        discord_id: 1,
        guild_id: 1,
        slot: definition.slot,
        tier: definition.reference_tier,
        durability,
        equipped: true,
        acquired_at: 0,
        source: "event:test".to_owned(),
        item_id: Some(item_id.to_owned()),
    }
}

fn assert_gear_reward(event_id: &str, item_id: &str) {
    let resolution = resolve_canonical_event_with_policy(event_id, "risky", rolls(0.0), policy())
        .expect("authored ring event resolves");
    assert!(resolution.succeeded);
    assert_eq!(resolution.gear_reward_pool, [item_id]);
    let selected = select_canonical_reward(
        &resolution_outcome(&resolution),
        &BTreeSet::new(),
        &BTreeSet::new(),
        0,
        8,
        0.0,
    )
    .expect("reward selection succeeds");
    assert_eq!(
        selected.reward,
        Some(CanonicalReward::Gear(item_id.to_owned()))
    );
}

// test_dig_ring_effects.py::test_surveyors_loop_adds_one_block_to_safe_success
#[test]
fn dig_ring_effects_surveyors_loop_adds_one_block_to_safe_success() {
    let ordinary =
        resolve_canonical_event_with_policy("surveyors_cairn", "safe", rolls(0.0), policy())
            .expect("ordinary survey event resolves");
    let with_loop = resolve_canonical_event_with_policy(
        "surveyors_cairn",
        "safe",
        rolls(0.0),
        CanonicalEventPolicy {
            surveyors_loop: true,
            ..policy()
        },
    )
    .expect("survey event with ring resolves");
    assert!(ordinary.succeeded && with_loop.succeeded);
    assert_eq!(ordinary.advance + 1, with_loop.advance);
}

// test_dig_ring_effects.py::test_surveyors_loop_penalizes_risky_choices_only_while_functional
#[test]
fn dig_ring_effects_surveyors_loop_penalizes_risky_choices_only_while_functional() {
    let active = resolve_canonical_event_with_policy(
        "surveyors_cairn",
        "risky",
        rolls(0.43),
        CanonicalEventPolicy {
            surveyors_loop: true,
            ..policy()
        },
    )
    .expect("active loop resolves");
    let broken =
        resolve_canonical_event_with_policy("surveyors_cairn", "risky", rolls(0.43), policy())
            .expect("broken loop baseline resolves");
    assert!(!active.succeeded);
    assert!(broken.succeeded);
}

// test_dig_ring_effects.py::test_ruinwager_signet_improves_risky_success_chance
#[test]
fn dig_ring_effects_ruinwager_signet_improves_risky_success_chance() {
    let without =
        resolve_canonical_event_with_policy("ruinwager_vault", "risky", rolls(0.40), policy())
            .expect("baseline vault resolves");
    let with_signet = resolve_canonical_event_with_policy(
        "ruinwager_vault",
        "risky",
        rolls(0.40),
        CanonicalEventPolicy {
            ruinwager_edge: true,
            ..policy()
        },
    )
    .expect("signet vault resolves");
    assert!(!without.succeeded);
    assert!(with_signet.succeeded);
}

// test_dig_ring_effects.py::test_ruinwager_signet_makes_failed_jc_losses_one_quarter_harsher
#[test]
fn dig_ring_effects_ruinwager_signet_makes_failed_jc_losses_one_quarter_harsher() {
    let baseline =
        resolve_canonical_event_with_policy("ruinwager_vault", "risky", rolls(0.99), policy())
            .expect("baseline loss resolves");
    let with_signet = resolve_canonical_event_with_policy(
        "ruinwager_vault",
        "risky",
        rolls(0.99),
        CanonicalEventPolicy {
            ruinwager_edge: true,
            ..policy()
        },
    )
    .expect("signet loss resolves");
    assert!(!baseline.succeeded && !with_signet.succeeded);
    assert_eq!(baseline.jc, -24);
    assert_eq!(with_signet.jc, -31);
}

// test_dig_ring_effects.py::test_red_thread_band_rerolls_only_the_first_missed_attack
#[test]
fn dig_ring_effects_red_thread_band_rerolls_only_the_first_missed_attack() {
    let mut state = combat(CombatEffects {
        gear_reroll_first_miss: true,
        ..CombatEffects::default()
    });
    state.player_hit = 0.50;
    state.boss_hit = 0.50;
    let mut entropy = SequenceEntropy::new(vec![0.90, 0.20, 0.90], vec![], vec![]);
    let (first, first_terminal) = run_combat_round(1, &mut state, &mut entropy);
    let (second, second_terminal) = run_combat_round(2, &mut state, &mut entropy);
    assert_eq!(first_terminal, None);
    assert_eq!(second_terminal, None);
    assert!(first.player_hit);
    assert!(first.effect_log.red_thread_reroll);
    assert_eq!(first.boss_hp, 4);
    assert!(!second.player_hit);
    assert!(!second.effect_log.red_thread_reroll);
    assert!(!state.effects.gear_reroll_first_miss);
}

// test_dig_ring_effects.py::test_red_thread_band_preserves_reroll_during_silenced_round
#[test]
fn dig_ring_effects_red_thread_band_preserves_reroll_during_silenced_round() {
    let mut state = combat(CombatEffects {
        gear_reroll_first_miss: true,
        silenced_next_round: true,
        ..CombatEffects::default()
    });
    state.player_hit = 0.50;
    let mut entropy = SequenceEntropy::new(vec![0.90, 0.90], vec![], vec![]);
    let (round, terminal) = run_combat_round(1, &mut state, &mut entropy);
    assert_eq!(terminal, None);
    assert!(!round.player_hit);
    assert!(!round.effect_log.red_thread_reroll);
    assert!(state.effects.gear_reroll_first_miss);
}

// test_dig_ring_events.py::test_ring_encounters_are_depth_staggered_genuine_choices
#[test]
fn dig_ring_events_are_depth_staggered_genuine_choices() {
    let expected = [
        ("surveyors_cairn", "surveyors_loop", 25_i64),
        ("ruinwager_vault", "ruinwager_signet", 75_i64),
        ("red_thread_shrine", "red_thread_band", 125_i64),
    ];
    for (event_id, item_id, min_depth) in expected {
        let event = canonical_event(event_id).expect("ring event in canonical catalog");
        assert_eq!(event.min_depth, Some(min_depth));
        let safe = event.safe_option.as_ref().expect("safe option");
        let risky = event.risky_option.as_ref().expect("risky option");
        assert_eq!(safe.success_chance, 1.0);
        assert!(safe.success.advance > 0);
        assert!(risky.success_chance < 1.0);
        assert_eq!(risky.success.gear_reward_pool, [item_id]);
        assert!(
            risky
                .failure
                .as_ref()
                .is_some_and(|failure| failure.advance < 0 || failure.jc < 0 || failure.cave_in)
        );
    }
    assert!(canonical_event_catalog().len() >= 203);
}

// test_dig_ring_events.py::test_ring_encounter_risky_success_grants_its_unique_ring[surveyors_cairn-surveyors_loop]
#[test]
fn dig_ring_events_surveyors_cairn_risky_success_grants_unique_ring() {
    assert_gear_reward("surveyors_cairn", "surveyors_loop");
}

// test_dig_ring_events.py::test_ring_encounter_risky_success_grants_its_unique_ring[ruinwager_vault-ruinwager_signet]
#[test]
fn dig_ring_events_ruinwager_vault_risky_success_grants_unique_ring() {
    assert_gear_reward("ruinwager_vault", "ruinwager_signet");
}

// test_dig_ring_events.py::test_ring_encounter_risky_success_grants_its_unique_ring[red_thread_shrine-red_thread_band]
#[test]
fn dig_ring_events_red_thread_shrine_risky_success_grants_unique_ring() {
    assert_gear_reward("red_thread_shrine", "red_thread_band");
}

// test_dig_ring_events.py::test_repeat_ring_encounter_keeps_outcome_without_duplicate_gear
#[test]
fn dig_ring_events_repeat_keeps_outcome_without_duplicate_gear() {
    let resolution =
        resolve_canonical_event_with_policy("surveyors_cairn", "risky", rolls(0.0), policy())
            .expect("repeat encounter resolves");
    let owned = BTreeSet::from(["surveyors_loop".to_owned()]);
    let selected = select_canonical_reward(
        &resolution_outcome(&resolution),
        &owned,
        &BTreeSet::new(),
        0,
        8,
        0.0,
    )
    .expect("repeat reward selection succeeds");
    assert!(resolution.succeeded);
    assert!(resolution.advance > 0);
    assert_eq!(selected.reward, None);
    assert!(selected.duplicate_gear_reward);
    assert!(selected.eligible_gear.is_empty());
}

// test_dig_unique_gear_effects.py::test_broken_unique_gear_does_not_seed_its_combat_effect
#[test]
fn dig_unique_gear_effects_broken_unique_gear_does_not_seed_its_combat_effect() {
    let loadout = GearLoadout {
        armor: Some(one_unique_piece("briarplate", 0)),
        ..GearLoadout::default()
    };
    let modifiers = loadout.combat_modifiers();
    assert_eq!(modifiers.player_hp_bonus, 0);
    assert_eq!(modifiers.player_dmg, 0);
}

// test_dig_unique_gear_effects.py::test_nullweave_blocks_first_status_and_anchor_blocks_first_player_skip
#[test]
fn dig_unique_gear_effects_nullweave_blocks_first_status_and_anchor_blocks_first_player_skip() {
    let option = MechanicOption {
        label: "status".to_owned(),
        flavor: "status".to_owned(),
        outcome_rolls: vec![OutcomeRoll {
            probability: 1.0,
            player_hp_delta: 0,
            boss_hp_delta: 0,
            skip_next_round_for: None,
            status_effect: Some(MechanicStatusEffect::Burn),
            narrative: "burn".to_owned(),
        }],
    };
    let mut effects = CombatEffects {
        gear_block_first_status: true,
        gear_block_first_skip: true,
        ..CombatEffects::default()
    };
    effects =
        apply_option_outcome(&option, 5, 5, &effects, &mut SequenceEntropy::constant(0.0)).effects;
    assert!(!effects.gear_block_first_status);
    assert_eq!(effects.nullweave_blocked, Some(MechanicStatusEffect::Burn));
    assert_eq!(effects.burn_rounds_remaining, 0);

    let skip = MechanicOption {
        label: "skip".to_owned(),
        flavor: "skip".to_owned(),
        outcome_rolls: vec![OutcomeRoll {
            probability: 1.0,
            player_hp_delta: 0,
            boss_hp_delta: 0,
            skip_next_round_for: Some(Combatant::Player),
            status_effect: None,
            narrative: "skip".to_owned(),
        }],
    };
    effects =
        apply_option_outcome(&skip, 5, 5, &effects, &mut SequenceEntropy::constant(0.0)).effects;
    assert!(!effects.gear_block_first_skip);
    assert!(effects.anchor_boots_blocked);
    assert_eq!(effects.skip_next_round_for, None);
}

// test_dig_unique_gear_effects.py::test_briarplate_reflects_first_landed_boss_hit
#[test]
fn dig_unique_gear_effects_briarplate_reflects_first_landed_boss_hit() {
    let mut state = combat(CombatEffects {
        gear_reflect_first_hit: true,
        ..CombatEffects::default()
    });
    state.boss_hit = 1.0;
    let mut entropy = SequenceEntropy::new(vec![0.0, 0.0], vec![], vec![]);
    let (round, terminal) = run_combat_round(1, &mut state, &mut entropy);
    assert_eq!(terminal, None);
    assert_eq!(round.player_hp, 4);
    assert_eq!(round.boss_hp, 4);
    assert!(round.effect_log.briarplate_reflect);
    assert!(!state.effects.gear_reflect_first_hit);
}

// test_dig_unique_gear_effects.py::test_springheel_boots_counter_first_dodge
#[test]
fn dig_unique_gear_effects_springheel_boots_counter_first_dodge() {
    let mut state = combat(CombatEffects {
        gear_springheel_counter: true,
        ..CombatEffects::default()
    });
    let mut entropy = SequenceEntropy::new(vec![0.0, 0.99], vec![], vec![]);
    let (round, terminal) = run_combat_round(1, &mut state, &mut entropy);
    assert_eq!(terminal, None);
    assert_eq!(round.boss_hp, 4);
    assert!(round.effect_log.springheel_counter);
    assert!(!state.effects.gear_springheel_counter);
}

// test_dig_unique_gear_effects.py::test_blood_locket_heals_on_first_crit_without_overhealing
#[test]
fn dig_unique_gear_effects_blood_locket_heals_on_first_crit_without_overhealing() {
    let mut state = combat(CombatEffects {
        gear_heal_first_crit: true,
        trophy_start_hp: Some(5),
        ..CombatEffects::default()
    });
    state.player_hp = 4;
    state.player_hit = 1.0;
    state.critical_chance = 1.0;
    let mut entropy = SequenceEntropy::new(vec![0.0, 0.0, 0.99], vec![], vec![]);
    let (round, terminal) = run_combat_round(1, &mut state, &mut entropy);
    assert_eq!(terminal, None);
    assert_eq!(round.player_hp, 5);
    assert!(round.effect_log.blood_locket_heal);
    assert!(!state.effects.gear_heal_first_crit);
}

// test_dig_new_relic_effects.py::test_chipped_compass_adds_one_block_to_safe_success
#[test]
fn dig_new_relic_effects_chipped_compass_adds_one_block_to_safe_success() {
    let ordinary =
        resolve_canonical_event_with_policy("surveyors_cairn", "safe", rolls(0.0), policy())
            .expect("ordinary event resolves");
    let with_compass = resolve_canonical_event_with_policy(
        "surveyors_cairn",
        "safe",
        rolls(0.0),
        CanonicalEventPolicy {
            chipped_compass: true,
            ..policy()
        },
    )
    .expect("compass event resolves");
    assert!(ordinary.succeeded && with_compass.succeeded);
    assert_eq!(ordinary.advance + 1, with_compass.advance);
}

// test_dig_new_relic_effects.py::test_black_wax_seal_boosts_cursed_risky_success_and_spends_duration
#[test]
fn dig_new_relic_effects_black_wax_seal_boosts_cursed_risky_success_and_spends_duration() {
    let resolution = resolve_canonical_event_with_policy(
        "ruinwager_vault",
        "risky",
        rolls(0.42),
        CanonicalEventPolicy {
            active_temp_curse: true,
            active_curse_remaining: Some(3),
            black_wax_seal: true,
            ..policy()
        },
    )
    .expect("black-wax event resolves");
    assert!(resolution.succeeded);
    assert!(resolution.black_wax_seal_spent);
    assert_eq!(resolution.active_curse_remaining_after, Some(2));
}

// test_dig_new_relic_effects.py::test_burning_ledger_increases_event_gains_and_strengthened_losses
#[test]
fn dig_new_relic_effects_burning_ledger_increases_event_gains_and_strengthened_losses() {
    let gain = |burning_ledger| {
        resolve_canonical_event_with_policy(
            "arcanist_library",
            "risky",
            rolls(0.0),
            CanonicalEventPolicy {
                burning_ledger,
                ..policy()
            },
        )
        .expect("ledger gain resolves")
    };
    let loss = |burning_ledger| {
        resolve_canonical_event_with_policy(
            "ruinwager_vault",
            "risky",
            rolls(0.99),
            CanonicalEventPolicy {
                burning_ledger,
                ..policy()
            },
        )
        .expect("ledger loss resolves")
    };
    let ordinary_gain = gain(false);
    let ledger_gain = gain(true);
    let ordinary_loss = loss(false);
    let ledger_loss = loss(true);
    assert_eq!(ordinary_gain.jc, 73);
    assert_eq!(ledger_gain.jc, 84);
    assert_eq!(ordinary_loss.jc, -24);
    assert_eq!(ledger_loss.jc, -31);
}

// The Lantern Stub and Bone Abacus source nodes now have their unique active
// contracts in `dig_relic_rework::tests`; keep this unadmitted audit file from
// defining duplicate test identities before TSV admission.

// test_dig_new_relic_effects.py::test_paper_crane_blocks_first_boss_status
#[test]
fn dig_new_relic_effects_paper_crane_blocks_first_boss_status() {
    let option = MechanicOption {
        label: "status".to_owned(),
        flavor: "status".to_owned(),
        outcome_rolls: vec![OutcomeRoll {
            probability: 1.0,
            player_hp_delta: 0,
            boss_hp_delta: 0,
            skip_next_round_for: None,
            status_effect: Some(MechanicStatusEffect::Burn),
            narrative: "burn".to_owned(),
        }],
    };
    let effects = CombatEffects {
        relic_paper_crane: true,
        ..CombatEffects::default()
    };
    let result = apply_option_outcome(&option, 5, 5, &effects, &mut SequenceEntropy::constant(0.0));
    assert!(!result.effects.relic_paper_crane);
    assert_eq!(
        result.effects.paper_crane_blocked,
        Some(MechanicStatusEffect::Burn)
    );
    assert_eq!(result.effects.burn_rounds_remaining, 0);
}

// test_dig_new_relic_effects.py::test_bottled_quake_adds_damage_to_first_landed_hit
#[test]
fn dig_new_relic_effects_bottled_quake_adds_damage_to_first_landed_hit() {
    let mut state = combat(CombatEffects {
        relic_bottled_quake: true,
        ..CombatEffects::default()
    });
    state.player_hit = 1.0;
    let mut entropy = SequenceEntropy::new(vec![0.0, 0.99], vec![], vec![]);
    let (round, terminal) = run_combat_round(1, &mut state, &mut entropy);
    assert_eq!(terminal, None);
    assert_eq!(round.boss_hp, 3);
    assert!(round.effect_log.bottled_quake);
    assert!(!state.effects.relic_bottled_quake);
}

// Shifting Idol's source node likewise lives in the active
// `dig_relic_rework::tests` module now that the fresh-attempt API is public.

// test_orphan_perks.py::TestEventHasRisk::test_event_with_only_positive_outcomes_is_not_risky
#[test]
fn orphan_event_with_only_positive_outcomes_is_not_risky() {
    let event = json!({
        "safe_option": {
            "success": {"jc": 2},
            "failure": {"jc": 0}
        }
    });
    assert!(!event_has_risk(&event));
}

// test_orphan_perks.py::TestEventHasRisk::test_legacy_outcomes_format_with_negative
#[test]
fn orphan_legacy_outcomes_format_with_negative_is_risky() {
    let event = json!({
        "outcomes": {
            "safe": {"jc": 1},
            "risky": {"jc": -10}
        }
    });
    assert!(event_has_risk(&event));
}

// test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure0]
#[test]
fn orphan_non_jc_advance_downside_is_risky() {
    let event = json!({"risky_option": {"success": {"jc": 5}, "failure": {"advance": -1}}});
    assert!(event_has_risk(&event));
}

// test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure1]
#[test]
fn orphan_non_jc_cave_in_downside_is_risky() {
    let event = json!({"risky_option": {"success": {"jc": 5}, "failure": {"cave_in": true}}});
    assert!(event_has_risk(&event));
}

// test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure2]
#[test]
fn orphan_non_jc_streak_loss_downside_is_risky() {
    let event = json!({"risky_option": {"success": {"jc": 5}, "failure": {"streak_loss": 1}}});
    assert!(event_has_risk(&event));
}

// test_orphan_perks.py::TestEventHasRisk::test_non_jc_downsides_are_risky[failure3]
#[test]
fn orphan_non_jc_curse_downside_is_risky() {
    let event = json!({
        "risky_option": {
            "success": {"jc": 5},
            "failure": {"curse": {"id": "bad_luck"}}
        }
    });
    assert!(event_has_risk(&event));
}

// test_orphan_perks.py::TestReadingTheStoneHint::test_returns_none_for_event_with_no_options
#[test]
fn orphan_reading_the_stone_returns_none_without_options() {
    assert_eq!(reading_the_stone_direction(&json!({})), None);
    assert_eq!(reading_the_stone_hint(&json!({}), 0), None);
    assert_eq!(
        reading_the_stone_hint(&json!({"name": "no choices"}), 1),
        None
    );
}
