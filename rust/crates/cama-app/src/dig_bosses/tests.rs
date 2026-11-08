use std::collections::BTreeSet;

use super::*;

fn assert_close(left: f64, right: f64) {
    assert!((left - right).abs() < 1.0e-9, "{left} != {right}");
}

fn base_stats() -> CombatStats {
    CombatStats {
        player_hp: 5,
        boss_hp: 4,
        player_hit: 0.6,
        player_damage: 1,
        boss_hit: 0.3,
        boss_damage: 1,
    }
}

fn boss_ids(pool: &[&BossDefinition]) -> BTreeSet<&'static str> {
    pool.iter().map(|boss| boss.boss_id).collect()
}

/// Production duel odds without a pet damage assist.
fn win_probability(input: DuelOddsInput) -> f64 {
    duel_win_probability_with_damage_bonus(input, 0)
}

/// Whether the prestige-gated pool production locks from offers this boss.
fn pool_offers(depth: i32, prestige_level: i32, boss_id: &str) -> bool {
    boss_pool_for_tier(depth, prestige_level)
        .iter()
        .any(|boss| boss.boss_id == boss_id)
}

#[test]
fn test_regular_bosses_have_exactly_three_registered_mechanics() {
    for boss in BOSSES {
        assert_eq!(boss.mechanic_pool.len(), 3, "{}", boss.boss_id);
        assert!(boss.mechanic_pool.iter().all(|id| mechanic(id).is_some()));
    }
}

#[test]
fn test_pinnacle_phases_have_exactly_two_registered_mechanics() {
    for boss in PINNACLE_BOSSES {
        for phase in boss.phases {
            assert_eq!(phase.mechanic_pool.len(), 2);
            assert!(phase.mechanic_pool.iter().all(|id| mechanic(id).is_some()));
        }
    }
}

#[test]
fn test_assigned_mechanics_are_structurally_valid() {
    let assigned: BTreeSet<&str> = BOSSES
        .iter()
        .flat_map(|boss| boss.mechanic_pool)
        .chain(
            PINNACLE_BOSSES
                .iter()
                .flat_map(|boss| boss.phases)
                .flat_map(|phase| phase.mechanic_pool),
        )
        .collect();
    for mechanic_id in assigned {
        let definition = mechanic(mechanic_id).expect("assigned mechanic is registered");
        assert!((1..=BOSS_ROUND_CAP).contains(&definition.trigger_round));
        assert_eq!(definition.options.len(), 3);
        assert!(definition.safe_option_index < definition.options.len());
        for option in definition.options {
            assert_close(
                option
                    .outcome_rolls
                    .iter()
                    .map(|outcome| outcome.probability)
                    .sum(),
                1.0,
            );
        }
    }
}

#[test]
fn test_bright_no_penalty() {
    assert_eq!(luminosity_combat_penalty(100), (0.0, 0));
    assert_eq!(luminosity_combat_penalty(76), (0.0, 0));
}

#[test]
fn test_dim_minus_3pct_hit() {
    assert_eq!(luminosity_combat_penalty(50), (-0.03, 0));
}

#[test]
fn test_dark_minus_8pct_hit() {
    assert_eq!(luminosity_combat_penalty(1), (-0.08, 0));
}

#[test]
fn test_pitch_minus_15pct_hit_plus_1_dmg() {
    assert_eq!(luminosity_combat_penalty(0), (-0.15, 1));
}

#[test]
fn test_below_dim_threshold_uses_dark() {
    assert_eq!(luminosity_combat_penalty(25), (-0.08, 0));
}

#[test]
fn test_cap_applies_when_overwhelming_advantage() {
    let win = win_probability(DuelOddsInput {
        player_hp: 20,
        boss_hp: 2,
        player_hit: 0.95,
        player_damage: 5,
        boss_hit: 0.05,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
    });
    assert!(win <= WIN_CHANCE_CAP + 1.0e-9);
}

#[test]
fn test_low_win_unchanged() {
    let win = win_probability(DuelOddsInput {
        player_hp: 2,
        boss_hp: 20,
        player_hit: 0.10,
        player_damage: 1,
        boss_hit: 0.95,
        boss_damage: 5,
        critical_chance: 0.0,
        critical_bonus: 0,
    });
    assert!(win < 0.5);
}

#[test]
fn test_crit_raises_win_chance() {
    let input = DuelOddsInput {
        player_hp: 5,
        boss_hp: 10,
        player_hit: 0.5,
        player_damage: 1,
        boss_hit: 0.3,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 1,
    };
    let baseline = win_probability(input);
    let with_crit = win_probability(DuelOddsInput {
        critical_chance: 1.0,
        ..input
    });
    assert!(with_crit > baseline + 0.10, "{with_crit} <= {baseline}");
}

#[test]
fn survival_halves_bold_win_rate_gain_at_every_point_and_caps_at_twenty() {
    let input = DuelOddsInput {
        player_hp: 3,
        boss_hp: 5,
        player_hit: 0.42006,
        player_damage: 2,
        boss_hit: 0.45,
        boss_damage: 1,
        critical_chance: 0.15,
        critical_bonus: 1,
    };
    let baseline = win_probability(input);
    for points in 0..=25 {
        let full_hit = input.player_hit + cama_domain::dig_stats::survival_roll_bonus(points);
        let full = win_probability(DuelOddsInput {
            player_hit: full_hit,
            ..input
        });
        let hit = calibrate_player_hit_for_survival(input, full_hit, 0);
        let actual = win_probability(DuelOddsInput {
            player_hit: hit,
            ..input
        });
        assert!((actual - baseline - (full - baseline) * 0.5).abs() < 1e-12);
        if points >= 20 {
            assert!((actual - 0.698503).abs() < 0.000001);
        }
    }
}

#[test]
fn survival_calibration_hits_the_midpoint_of_the_raw_win_gain() {
    let input = DuelOddsInput {
        player_hp: 5,
        boss_hp: 4,
        player_hit: 0.60,
        player_damage: 1,
        boss_hit: 0.30,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
    };
    let full_hit = 0.80;
    let baseline = duel_win_probability_unclamped(input, 0);
    let full = duel_win_probability_unclamped(
        DuelOddsInput {
            player_hit: full_hit,
            ..input
        },
        0,
    );
    let calibrated = calibrate_player_hit_for_survival(input, full_hit, 0);
    let midpoint = duel_win_probability_unclamped(
        DuelOddsInput {
            player_hit: calibrated,
            ..input
        },
        0,
    );
    assert!((midpoint - (baseline + full) * 0.5).abs() < 1.0e-9);
    assert!(calibrated > input.player_hit);
    assert!(calibrated < full_hit);
}

#[test]
fn survival_calibration_uses_raw_probability_below_display_floor() {
    let input = DuelOddsInput {
        player_hp: 5,
        boss_hp: 10,
        player_hit: 0.05,
        player_damage: 1,
        boss_hit: 0.30,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
    };
    let full_hit = 0.60;
    let baseline = duel_win_probability_unclamped(input, 0);
    let full = duel_win_probability_unclamped(
        DuelOddsInput {
            player_hit: full_hit,
            ..input
        },
        0,
    );
    let calibrated = calibrate_player_hit_for_survival(input, full_hit, 0);
    let midpoint = duel_win_probability_unclamped(
        DuelOddsInput {
            player_hit: calibrated,
            ..input
        },
        0,
    );
    assert!(win_probability(input) <= WIN_CHANCE_FLOOR + 1.0e-9);
    assert!((midpoint - (baseline + full) * 0.5).abs() < 1.0e-9);
}

#[test]
fn survival_calibration_handles_free_fight_multiplier_and_pet_bonus() {
    let input = DuelOddsInput {
        player_hp: 5,
        boss_hp: 4,
        player_hit: 0.60 * 0.70,
        player_damage: 1,
        boss_hit: 0.30,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
    };
    let full_hit = (0.80_f64 * 0.70).min(0.90);
    let baseline = duel_win_probability_unclamped(input, 12);
    let full = duel_win_probability_unclamped(
        DuelOddsInput {
            player_hit: full_hit,
            ..input
        },
        12,
    );
    let calibrated = calibrate_player_hit_for_survival(input, full_hit, 12);
    let midpoint = duel_win_probability_unclamped(
        DuelOddsInput {
            player_hit: calibrated,
            ..input
        },
        12,
    );
    assert!((midpoint - (baseline + full) * 0.5).abs() < 1.0e-9);
    assert!(calibrated > input.player_hit);
    assert!(calibrated < full_hit);
}

#[test]
fn test_archetype_hp_multiplier_applied() {
    assert_eq!(
        scale_boss_stats(base_stats(), "pudge", 24, 0, false).boss_hp,
        6
    );
}

#[test]
fn test_glass_cannon_dmg_offset() {
    assert_eq!(
        scale_boss_stats(base_stats(), "lina", 0, 0, false).boss_damage,
        2
    );
}

#[test]
fn test_depth_scaling_hp() {
    assert_eq!(
        scale_boss_stats(base_stats(), "grothak", PINNACLE_DEPTH, 0, false).boss_hp,
        13
    );
}

#[test]
fn test_depth_scaling_hit_and_dmg() {
    let scaled = scale_boss_stats(base_stats(), "grothak", PINNACLE_DEPTH, 0, false);
    assert_close(scaled.boss_hit, 0.40);
    assert_eq!(scaled.boss_damage, 1);
}

#[test]
fn test_prestige_scaling() {
    let scaled = scale_boss_stats(base_stats(), "grothak", 25, 5, false);
    assert_eq!(scaled.boss_hp, 30);
    assert_close(scaled.boss_hit, 0.51);
    assert_eq!(scaled.boss_damage, 1);
}

#[test]
fn test_non_boundary_depth_falls_back_to_lower_tier() {
    assert_eq!(
        scale_boss_stats(base_stats(), "grothak", 130, 0, false).boss_hp,
        7
    );
    assert_eq!(
        scale_boss_stats(base_stats(), "grothak", 250, 0, false).boss_hp,
        10
    );
}

#[test]
fn test_echo_applies_25pct_hp_discount() {
    let regular = scale_boss_stats(base_stats(), "grothak", PINNACLE_DEPTH, 0, false);
    let echo = scale_boss_stats(base_stats(), "grothak", PINNACLE_DEPTH, 0, true);
    assert_eq!(
        echo.boss_hp,
        (f64::from(regular.boss_hp) * 0.75).round() as i32
    );
}

#[test]
fn test_all_standard_roster_bosses_have_first_meet() {
    for boss in BOSSES {
        if matches!(boss.boss_id, "blightcoil" | "rimebound_king" | "spineback") {
            continue;
        }
        let slots = dialogue_slots(boss.boss_id).expect("standard boss dialogue");
        assert!(slots.first_meet.len() >= 2);
        assert!(!slots.after_defeat.is_empty());
        assert!(!slots.after_retreat.is_empty());
        assert!(!slots.after_close_win.is_empty());
        assert!(!slots.after_scout.is_empty());
    }
}

#[test]
fn test_pinnacle_pool_has_all_three() {
    for boss_id in PINNACLE_POOL_IDS {
        assert!(pinnacle_by_id(boss_id).is_some());
        assert!(dialogue_slots(boss_id).is_some());
        assert!(pinnacle_relic_base_name(boss_id).is_some());
    }
}

#[test]
fn test_render_boss_bark_substitutes_tokens() {
    let rendered = render_boss_bark(
        "Streak {streak} days at depth {depth}, P{prestige}.",
        BarkContext {
            streak_days: 7,
            depth: 50,
            prestige_level: 2,
            killed_boss_name: Some("Grothak"),
        },
    );
    assert!(rendered.contains("7 days"));
    assert!(rendered.contains("depth 50"));
    assert!(rendered.contains("P2"));
}

#[test]
fn test_render_boss_bark_no_tokens_passes_through() {
    assert_eq!(
        render_boss_bark("No substitution here.", BarkContext::default()),
        "No substitution here."
    );
}

#[test]
fn test_render_boss_bark_killed_boss_fallback() {
    assert!(
        render_boss_bark("You who slew {killed_boss_name}.", BarkContext::default())
            .contains("the early dark")
    );
}

#[test]
fn test_pinnacle_pool_has_exactly_six_depth_350_candidates() {
    assert_eq!(
        PINNACLE_POOL_IDS,
        [
            "forgotten_king",
            "hollowforged",
            "first_digger",
            "bastion_below",
            "pale_surveyor",
            "lantern_engine"
        ]
    );
}

#[test]
fn test_new_pinnacles_have_complete_phase_and_relic_data() {
    for (boss_id, relic_name) in [
        ("bastion_below", "Standard"),
        ("pale_surveyor", "Compass"),
        ("lantern_engine", "Core"),
    ] {
        let boss = pinnacle_by_id(boss_id).expect("pinnacle exists");
        assert!(!boss.persona.is_empty());
        assert!(!boss.ascii_art.is_empty());
        assert_eq!(boss.phases.len(), 3);
        assert!(
            boss.phases
                .iter()
                .all(|phase| phase.mechanic_pool.len() == 2)
        );
        assert_eq!(pinnacle_relic_base_name(boss_id), Some(relic_name));
    }
}

#[test]
fn test_secret_phase_presentation_is_phase_three_only() {
    assert_close(PINNACLE_SECRET_PHASE_CHANCE, 0.10);
    for boss in PINNACLE_BOSSES {
        for phase in &boss.phases[..2] {
            assert!(phase.secret_title.is_none());
            assert!(phase.secret_dialogue.is_empty());
        }
        assert!(boss.phases[2].secret_title.is_some());
        assert!(!boss.phases[2].secret_dialogue.is_empty());
    }
}

#[test]
fn test_new_pinnacle_mechanics_are_registered() {
    let expected: BTreeSet<&str> = [
        "bastion_three_roads",
        "bastion_hidden_route",
        "bastion_fortify",
        "bastion_counter_siege",
        "bastion_throne_race",
        "bastion_last_reserve",
        "surveyor_marked_ground",
        "surveyor_false_landmark",
        "surveyor_memory_echo",
        "surveyor_folded_arena",
        "surveyor_corruption_ring",
        "surveyor_unwritten_end",
        "lantern_heat_cycle",
        "lantern_venting_protocol",
        "lantern_overclock",
        "lantern_gearstorm",
        "lantern_critical_mass",
        "lantern_infinite_feedback",
    ]
    .into_iter()
    .collect();
    let wired: BTreeSet<&str> = ["bastion_below", "pale_surveyor", "lantern_engine"]
        .into_iter()
        .flat_map(|id| pinnacle_by_id(id).expect("new pinnacle").phases)
        .flat_map(|phase| phase.mechanic_pool)
        .collect();
    assert_eq!(wired, expected);
    for mechanic_id in expected {
        let definition = mechanic(mechanic_id).expect("registered mechanic");
        let safe = &definition.options[definition.safe_option_index];
        assert_eq!(definition.options.len(), 3);
        assert!(
            safe.outcome_rolls
                .iter()
                .all(|roll| roll.status_effect.is_none() && roll.skip_next_round_for.is_none())
        );
    }
}

#[test]
fn test_each_pinnacle_has_three_phases() {
    assert!(PINNACLE_BOSSES.iter().all(|boss| boss.phases.len() == 3));
}

#[test]
fn test_relic_pool_non_empty() {
    assert!(PINNACLE_RELIC_STATS.len() >= 8);
}

#[test]
fn test_known_archetypes_defined() {
    for name in ["tank", "bruiser", "glass_cannon", "slippery"] {
        let definition = archetype(name);
        assert!(definition.hp_multiplier > 0.0);
        assert!(definition.hit_offset.abs() <= 0.10);
        assert!(definition.damage_offset >= 0);
    }
}

#[test]
fn test_king_feast_safe_option_is_a_low_risk_positioning_choice() {
    let definition = mechanic("king_feast").expect("king feast");
    assert_eq!(definition.safe_option_index, 2);
    let copy = definition
        .options
        .iter()
        .map(|option| format!("{} {}", option.label, option.flavor))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    assert!(
        ["pocket", "ration", "loaf", "inventory"]
            .iter()
            .all(|word| !copy.contains(word))
    );
    let safe = &definition.options[2];
    assert_eq!(
        safe.outcome_rolls,
        vec![roll(0.75, 0, 0, None, None), roll(0.25, -1, -1, None, None)]
    );
}

#[test]
fn test_existing_pinnacle_safe_choice_and_risk_regressions() {
    assert_eq!(
        mechanic("king_deathbed").expect("mechanic").options[0].outcome_rolls[0].boss_hp_delta,
        -1
    );
    assert!(
        mechanic("hollow_walls_close").expect("mechanic").options[1]
            .label
            .to_ascii_lowercase()
            .starts_with("risk")
    );
    assert_eq!(
        mechanic("digger_phasing")
            .expect("mechanic")
            .safe_option_index,
        0
    );
    for mechanic_id in ["digger_pickaxe_duel", "digger_tunnel_collapse"] {
        assert!(
            mechanic(mechanic_id).expect("mechanic").options[2]
                .outcome_rolls
                .iter()
                .any(|outcome| outcome.player_hp_delta <= -3)
        );
    }
    assert_eq!(
        mechanic("digger_last_excavation")
            .expect("mechanic")
            .trigger_round,
        4
    );
}

fn assert_hollow_safe_option(mechanic_id: &str, safe_option_index: usize) {
    let definition = mechanic(mechanic_id).expect("hollow mechanic");
    assert_eq!(definition.safe_option_index, safe_option_index);
    let damage: Vec<f64> = definition
        .options
        .iter()
        .map(|option| {
            option
                .outcome_rolls
                .iter()
                .map(|roll| roll.probability * f64::from(-roll.player_hp_delta))
                .sum()
        })
        .collect();
    assert!(
        damage
            .iter()
            .enumerate()
            .all(|(index, value)| index == safe_option_index || damage[safe_option_index] < *value)
    );
    assert!(
        definition.options[safe_option_index]
            .outcome_rolls
            .iter()
            .all(|roll| roll.status_effect.is_none())
    );
}

#[test]
fn test_hollowforged_safe_options_minimize_expected_player_damage_hollow_shape_shift_1() {
    assert_hollow_safe_option("hollow_shape_shift", 1);
}

#[test]
fn test_hollowforged_safe_options_minimize_expected_player_damage_hollow_many_voices_2() {
    assert_hollow_safe_option("hollow_many_voices", 2);
}

#[test]
fn test_pinnacle_barks_are_phase_neutral_and_king_streak_is_grammatical() {
    for boss_id in PINNACLE_POOL_IDS {
        let slots = dialogue_slots(boss_id).expect("pinnacle dialogue");
        for line in slots
            .first_meet
            .iter()
            .chain(slots.after_defeat)
            .chain(slots.after_retreat)
            .chain(slots.after_close_win)
            .chain(slots.after_scout)
        {
            assert!(!line.contains("Round two"));
        }
    }
    assert!(
        !dialogue_slots("forgotten_king")
            .expect("king dialogue")
            .after_defeat
            .contains(&"Streak {streak} days, you say? I have stood here {streak} centuries.")
    );
}

#[test]
fn test_drop_creates_artifact_with_two_stats() {
    let relic = roll_pinnacle_relic(0, "forgotten_king", 0, 1).expect("relic");
    assert!(relic.name.starts_with("Crown of "));
    assert_eq!(relic.stats.len(), 2);
    assert_eq!(relic.stat_ids.len(), 2);
    assert_ne!(relic.stat_ids[0], relic.stat_ids[1]);
    assert!(relic.artifact_id.starts_with("pinnacle:"));
}

#[test]
fn test_drop_name_derives_suffix_from_stats() {
    for (index, pinnacle_id) in PINNACLE_POOL_IDS.into_iter().enumerate() {
        let relic = roll_pinnacle_relic(0, pinnacle_id, index, index + 1).expect("relic");
        let base = pinnacle_relic_base_name(pinnacle_id).expect("base");
        let suffix = relic
            .name
            .strip_prefix(&format!("{base} of "))
            .expect("suffix");
        assert_eq!(suffix, pinnacle_suffix_from_stats(&relic.stat_ids));
        assert!(!suffix.is_empty());
    }
}

const AFFECTED_TIERS: [i32; 3] = [150, 200, 275];

#[test]
fn prestige_five_bosses_unlock_and_keep_their_identity() {
    for (depth, id) in [
        (150, "last_conductor"),
        (200, "neon_bailiff"),
        (275, "hollow_cantor"),
    ] {
        for prestige in 0..=4 {
            assert!(!pool_offers(depth, prestige, id));
        }
        for prestige in [5, 6, 8] {
            assert!(pool_offers(depth, prestige, id));
        }
        let boss = boss_by_id(id).expect("prestige five boss");
        assert_eq!(boss.prestige_required, 5);
        for phase in [2, 3] {
            let definition = regular_phase(id, phase).expect("boss-specific phase");
            assert_eq!(definition.depth, depth);
            assert_ne!(definition.dialogue, PHASE_DIALOGUE);
        }
        let dialogue = dialogue_slots(id).expect("authored dialogue");
        assert_ne!(dialogue.first_meet, FIRST_MEET);
        for lines in [
            dialogue.first_meet,
            dialogue.after_defeat,
            dialogue.after_retreat,
            dialogue.after_close_win,
            dialogue.after_scout,
        ] {
            assert!(!lines.is_empty());
        }
    }
}

fn expected_prestige_two(tier: i32) -> BTreeSet<&'static str> {
    match tier {
        150 => ["aegis_warden", "cairn_general"].into_iter().collect(),
        200 => ["heartspire", "brass_croupier"].into_iter().collect(),
        275 => ["emberwright", "saltveil_commodore"].into_iter().collect(),
        _ => BTreeSet::new(),
    }
}

#[test]
fn test_pool_unlocks_prestige2_bosses_at_p2() {
    for tier in AFFECTED_TIERS {
        let pool = boss_pool_for_tier(tier, 2);
        assert_eq!(pool.len(), 5);
        assert!(expected_prestige_two(tier).is_subset(&boss_ids(&pool)));
    }
}

#[test]
fn test_pool_full_at_p3() {
    let late: BTreeSet<&str> = ["xalatath", "lilith", "underlord"].into_iter().collect();
    for tier in AFFECTED_TIERS {
        let pool = boss_pool_for_tier(tier, 3);
        assert_eq!(pool.len(), 6);
        assert!(!boss_ids(&pool).is_disjoint(&late));
    }
}

#[test]
fn test_pool_filtered_below_p2() {
    let p2: BTreeSet<&str> = AFFECTED_TIERS
        .into_iter()
        .flat_map(expected_prestige_two)
        .collect();
    let late: BTreeSet<&str> = ["xalatath", "lilith", "underlord"].into_iter().collect();
    for tier in AFFECTED_TIERS {
        let ids = boss_ids(&boss_pool_for_tier(tier, 1));
        assert_eq!(ids.len(), 3);
        assert!(ids.is_disjoint(&p2));
        assert!(ids.is_disjoint(&late));
    }
}

#[test]
fn test_deep_tier_prestige_composition() {
    for tier in AFFECTED_TIERS {
        let full = full_boss_pool_for_tier(tier);
        assert_eq!(
            full.iter()
                .filter(|boss| boss.prestige_required == 0)
                .count(),
            3
        );
        assert_eq!(
            full.iter()
                .filter(|boss| boss.prestige_required == 2)
                .map(|boss| boss.boss_id)
                .collect::<BTreeSet<_>>(),
            expected_prestige_two(tier),
        );
        assert_eq!(
            full.iter()
                .filter(|boss| boss.prestige_required == 3)
                .count(),
            1
        );
        assert_eq!(
            full.iter()
                .filter(|boss| boss.prestige_required == 4)
                .count(),
            1
        );
    }
}

#[test]
fn test_prestige_pool_sizes_at_each_unlock_level() {
    let expected = [(0, 3), (1, 3), (2, 5), (3, 6), (4, 7), (5, 8), (8, 8)];
    for tier in AFFECTED_TIERS {
        for (prestige, size) in expected {
            assert_eq!(boss_pool_for_tier(tier, prestige).len(), size);
        }
    }
}

#[test]
fn test_prestige2_bosses_remain_available_after_unlock() {
    for tier in AFFECTED_TIERS {
        for prestige in [2, 3, 4, 8] {
            assert!(
                expected_prestige_two(tier)
                    .is_subset(&boss_ids(&boss_pool_for_tier(tier, prestige)))
            );
        }
    }
}

#[test]
fn test_every_gated_boss_remains_available_after_unlock() {
    for tier in BOSS_BOUNDARIES {
        for boss in full_boss_pool_for_tier(tier)
            .into_iter()
            .filter(|boss| boss.prestige_required > 0)
        {
            for prestige in boss.prestige_required..9 {
                assert!(boss_ids(&boss_pool_for_tier(tier, prestige)).contains(boss.boss_id));
            }
        }
    }
}

#[test]
fn test_pool_default_returns_full_pool() {
    for tier in AFFECTED_TIERS {
        assert_eq!(full_boss_pool_for_tier(tier).len(), 8);
    }
}

#[test]
fn test_unaffected_tiers_unchanged() {
    for tier in [25, 50, 75, 100] {
        assert_eq!(boss_pool_for_tier(tier, 0).len(), 3);
        assert_eq!(boss_pool_for_tier(tier, 10).len(), 3);
    }
}

#[test]
fn test_prestige2_field_correct() {
    for boss_id in AFFECTED_TIERS.into_iter().flat_map(expected_prestige_two) {
        assert_eq!(boss_by_id(boss_id).expect("boss").prestige_required, 2);
    }
}

#[test]
fn test_late_prestige_field_correct() {
    for boss_id in ["xalatath", "lilith", "underlord"] {
        assert_eq!(boss_by_id(boss_id).expect("boss").prestige_required, 3);
    }
}

#[test]
fn test_existing_bosses_default_prestige_required() {
    for boss_id in ["grothak", "treant_protector", "oracle", "terrorblade"] {
        assert_eq!(boss_by_id(boss_id).expect("boss").prestige_required, 0);
    }
}

#[test]
fn test_prestige2_boss_combat_content_registered() {
    for boss_id in AFFECTED_TIERS.into_iter().flat_map(expected_prestige_two) {
        let boss = boss_by_id(boss_id).expect("boss");
        assert!(boss.mechanic_pool.iter().all(|id| mechanic(id).is_some()));
    }
    for (boss_id, mechanics, stinger) in [
        (
            "cairn_general",
            [
                "cairn_remnant",
                "cairn_rolling_fault",
                "cairn_magnetic_recall",
            ],
            "cairn_aftershock",
        ),
        (
            "brass_croupier",
            [
                "croupier_malice_cube",
                "croupier_clockwork_ballet",
                "croupier_overdraw",
            ],
            "croupier_collection",
        ),
        (
            "saltveil_commodore",
            [
                "saltveil_spyglass",
                "saltveil_chainshot",
                "saltveil_boarding",
            ],
            "saltveil_pressgang",
        ),
    ] {
        let boss = boss_by_id(boss_id).expect("new P2 boss");
        assert_eq!(boss.mechanic_pool, mechanics);
        assert_eq!(boss.stinger_id, stinger);
    }
}

fn assert_new_prestige_two_phase_identity(
    boss_id: &str,
    depth: i32,
    phase_two_identity: (&str, &str),
    phase_three_identity: (&str, &str),
) {
    let phase_two = regular_phase(boss_id, 2).expect("phase two");
    let phase_three = regular_phase(boss_id, 3).expect("phase three");
    assert_eq!((phase_two.name, phase_two.title), phase_two_identity);
    assert_eq!((phase_three.name, phase_three.title), phase_three_identity);
    assert_eq!(phase_two.depth, depth);
    assert_eq!(phase_three.depth, depth);
    assert_ne!(
        phase_two,
        default_regular_phase(depth, 2).expect("default phase two")
    );
    assert_ne!(
        phase_three,
        default_regular_phase(depth, 3).expect("default phase three")
    );
}

#[test]
fn test_new_prestige2_bosses_keep_their_identity_across_phases_cairn_general_150() {
    assert_new_prestige_two_phase_identity(
        "cairn_general",
        150,
        ("The Cairn General, Reformed", "The Fallen Close Ranks"),
        ("The Buried Host", "Every Stone Takes the Order"),
    );
}

#[test]
fn test_new_prestige2_bosses_keep_their_identity_across_phases_brass_croupier_200() {
    assert_new_prestige_two_phase_identity(
        "brass_croupier",
        200,
        (
            "The Brass Croupier, Re-Dealt",
            "The House Opens Another Table",
        ),
        ("The Hungry Engine", "The House Was Inside the Machine"),
    );
}

#[test]
fn test_new_prestige2_bosses_keep_their_identity_across_phases_saltveil_commodore_275() {
    assert_new_prestige_two_phase_identity(
        "saltveil_commodore",
        275,
        ("The Saltveil Commodore, Broadside", "All Guns Below"),
        ("The Drowned Fleet", "Every Bell Answers"),
    );
}

#[test]
fn test_no_boss_borrows_another_bosses_phase() {
    for boss in BOSSES {
        if default_regular_phase(boss.depth, 2).is_some() {
            assert!(
                regular_phase(boss.boss_id, 2).is_some(),
                "{} phase two",
                boss.boss_id
            );
        }
        if default_regular_phase(boss.depth, 3).is_some() {
            assert!(
                regular_phase(boss.boss_id, 3).is_some(),
                "{} phase three",
                boss.boss_id
            );
        }
    }
}

#[test]
fn test_phase_names_are_unique_across_bosses() {
    for phases in [&PHASE_TWO[..], &PHASE_THREE[..]] {
        let names: BTreeSet<&str> = phases.iter().map(|phase| phase.name).collect();
        assert_eq!(names.len(), phases.len());
    }
}

#[test]
fn test_phase_defs_match_their_boss_and_keep_the_tier_penalty() {
    for phases in [&PHASE_TWO[..], &PHASE_THREE[..]] {
        let mut penalties: BTreeMap<i32, BTreeSet<i32>> = BTreeMap::new();
        for phase in phases {
            let boss = boss_by_id(phase.boss_id).expect("phase owner");
            assert_eq!(phase.depth, boss.depth);
            assert!(!phase.name.is_empty());
            assert!(!phase.title.is_empty());
            assert_eq!(phase.dialogue.len(), 3);
            assert!(phase.win_odds_penalty < 0.0);
            penalties
                .entry(phase.depth)
                .or_default()
                .insert((phase.win_odds_penalty * 100.0).round() as i32);
        }
        assert!(penalties.values().all(|values| values.len() <= 2));
    }
}

#[test]
fn test_p0_player_never_locks_late_prestige_boss() {
    let pool = boss_pool_for_tier(200, 0);
    assert_eq!(
        boss_ids(&pool),
        ["chronofrost", "faceless_void", "weaver"]
            .into_iter()
            .collect()
    );
}

#[test]
fn test_p3_player_can_lock_late_prestige_boss() {
    assert!(pool_offers(200, 3, "lilith"));
}

#[test]
fn test_p1_player_never_locks_prestige2_boss() {
    let pool = boss_pool_for_tier(150, 1);
    assert_eq!(
        boss_ids(&pool),
        ["sporeling_sovereign", "treant_protector", "broodmother"]
            .into_iter()
            .collect()
    );
}

#[test]
fn test_p2_player_can_lock_prestige2_boss() {
    assert!(pool_offers(150, 2, "aegis_warden"));
}

fn assert_cannot_lock_new_prestige_two(tier: i32, boss_id: &str) {
    assert!(!pool_offers(tier, 1, boss_id));
    assert!(!boss_pool_for_tier(tier, 1).is_empty());
}

#[test]
fn test_p1_player_cannot_lock_new_prestige2_boss_150_cairn_general() {
    assert_cannot_lock_new_prestige_two(150, "cairn_general");
}

#[test]
fn test_p1_player_cannot_lock_new_prestige2_boss_200_brass_croupier() {
    assert_cannot_lock_new_prestige_two(200, "brass_croupier");
}

#[test]
fn test_p1_player_cannot_lock_new_prestige2_boss_275_saltveil_commodore() {
    assert_cannot_lock_new_prestige_two(275, "saltveil_commodore");
}

fn assert_new_prestige_two_locks(tier: i32, boss_id: &str, prestige_level: i32) {
    assert!(pool_offers(tier, prestige_level, boss_id));
}

macro_rules! lock_case {
    ($name:ident, $tier:literal, $boss_id:literal, $prestige:literal) => {
        #[test]
        fn $name() {
            assert_new_prestige_two_locks($tier, $boss_id, $prestige);
        }
    };
}

lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_150_cairn_general_2,
    150,
    "cairn_general",
    2
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_150_cairn_general_3,
    150,
    "cairn_general",
    3
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_150_cairn_general_4,
    150,
    "cairn_general",
    4
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_150_cairn_general_8,
    150,
    "cairn_general",
    8
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_200_brass_croupier_2,
    200,
    "brass_croupier",
    2
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_200_brass_croupier_3,
    200,
    "brass_croupier",
    3
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_200_brass_croupier_4,
    200,
    "brass_croupier",
    4
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_200_brass_croupier_8,
    200,
    "brass_croupier",
    8
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_275_saltveil_commodore_2,
    275,
    "saltveil_commodore",
    2
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_275_saltveil_commodore_3,
    275,
    "saltveil_commodore",
    3
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_275_saltveil_commodore_4,
    275,
    "saltveil_commodore",
    4
);
lock_case!(
    test_new_prestige2_boss_locks_at_and_after_unlock_275_saltveil_commodore_8,
    275,
    "saltveil_commodore",
    8
);
