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

fn all_tiers_defeated() -> BossProgress {
    BossRunState::with_all_tiers_defeated(PINNACLE_DEPTH - 1).boss_progress
}

fn entry(status: BossStatus) -> BossProgressValue {
    BossProgressValue::Entry(BossProgressEntry {
        status,
        ..BossProgressEntry::default()
    })
}

fn boss_ids(pool: &[&BossDefinition]) -> BTreeSet<&'static str> {
    pool.iter().map(|boss| boss.boss_id).collect()
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
    let win = approximate_duel_win_probability(DuelOddsInput {
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
    let win = approximate_duel_win_probability(DuelOddsInput {
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
    let baseline = approximate_duel_win_probability(input);
    let with_crit = approximate_duel_win_probability(DuelOddsInput {
        critical_chance: 1.0,
        ..input
    });
    assert!(with_crit > baseline + 0.10, "{with_crit} <= {baseline}");
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
fn test_no_persisted_hp_returns_fresh() {
    assert_eq!(
        resolve_persisted_boss_hp(&BossProgress::new(), "25", 10, 0),
        (10, 10)
    );
}

#[test]
fn test_legacy_string_treated_as_fresh() {
    let progress = BTreeMap::from([(
        "25".to_owned(),
        BossProgressValue::Legacy(BossStatus::Active),
    )]);
    assert_eq!(resolve_persisted_boss_hp(&progress, "25", 10, 0), (10, 10));
}

fn progress_with_hp(hp_remaining: i32, hp_max: i32, last_engaged_at: Option<i64>) -> BossProgress {
    BTreeMap::from([(
        "25".to_owned(),
        BossProgressValue::Entry(BossProgressEntry {
            hp_remaining: Some(hp_remaining),
            hp_max: Some(hp_max),
            last_engaged_at,
            ..BossProgressEntry::default()
        }),
    )])
}

#[test]
fn test_persisted_hp_used_when_present() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(4, 12, Some(0)), "25", 12, 0),
        (4, 12)
    );
}

#[test]
fn test_regen_caps_at_hp_max() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(4, 12, Some(0)), "25", 12, 100 * 86_400),
        (12, 12)
    );
}

#[test]
fn test_regen_adds_one_hp_per_completed_day() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(4, 12, Some(0)), "25", 12, 3 * 86_400).0,
        7
    );
}

#[test]
fn test_regen_waits_for_a_complete_day() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(4, 12, Some(0)), "25", 12, 86_399).0,
        4
    );
}

#[test]
fn test_fresh_cap_reduction_does_not_replace_active_boss_hp() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(7, 12, Some(0)), "25", 9, 0),
        (7, 12)
    );
}

#[test]
fn test_fresh_cap_increase_does_not_heal_active_boss() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(7, 12, Some(0)), "25", 15, 0),
        (7, 12)
    );
}

#[test]
fn test_regen_uses_active_boss_cap_when_fresh_cap_changes() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(7, 12, Some(0)), "25", 9, 2 * 86_400),
        (9, 12)
    );
}

#[test]
fn test_missing_regen_timestamp_still_clamps_hp_to_active_cap() {
    assert_eq!(
        resolve_persisted_boss_hp(&progress_with_hp(20, 12, None), "25", 15, 0),
        (12, 12)
    );
}

#[test]
fn test_persist_after_loss_writes_entry() {
    let mut progress = BossProgress::new();
    persist_boss_hp_after_fight(
        &mut progress,
        PersistBossHp {
            key: "25",
            boss_id: "grothak",
            ending_hp: 3,
            hp_max: 10,
            won: false,
            outcome: "loss",
            now: 42,
        },
    );
    let BossProgressValue::Entry(stored) = &progress["25"] else {
        panic!("entry expected")
    };
    assert_eq!(stored.hp_remaining, Some(3));
    assert_eq!(stored.hp_max, Some(10));
    assert_eq!(stored.last_engaged_at, Some(42));
    assert_eq!(stored.last_outcome.as_deref(), Some("loss"));
    assert!(stored.first_meet_seen);
    assert_eq!(stored.status, BossStatus::Active);
}

#[test]
fn test_persist_preserves_existing_status() {
    let mut progress = BTreeMap::from([(
        "25".to_owned(),
        BossProgressValue::Entry(BossProgressEntry {
            status: BossStatus::PhaseOneDefeated,
            boss_id: Some("pudge".to_owned()),
            ..BossProgressEntry::default()
        }),
    )]);
    persist_boss_hp_after_fight(
        &mut progress,
        PersistBossHp {
            key: "25",
            boss_id: "pudge",
            ending_hp: 2,
            hp_max: 8,
            won: false,
            outcome: "loss",
            now: 100,
        },
    );
    assert_eq!(progress["25"].status(), BossStatus::PhaseOneDefeated);
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
    assert!(PINNACLE_RELIC_SUFFIX_POOL.len() >= 6);
}

#[test]
fn test_phase_event_pool_non_empty() {
    assert!(PHASE_TRANSITION_EVENT_IDS.len() >= 4);
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
fn test_no_row_returns_none() {
    let store = InMemoryBossStore::default();
    assert!(store.active_boss_echo(Some(1), "grothak", 1_000).is_none());
}

#[test]
fn test_record_then_read() {
    let mut store = InMemoryBossStore::default();
    store.record_boss_echo(Some(1), "grothak", 25, 777, 3_600, 1_000);
    let echo = store
        .active_boss_echo(Some(1), "grothak", 1_001)
        .expect("active echo");
    assert_eq!(echo.killer_discord_id, 777);
    assert_eq!(echo.depth, 25);
    assert!(echo.weakened_until > 1_001);
}

#[test]
fn test_expired_row_returns_none() {
    let mut store = InMemoryBossStore::default();
    store.record_boss_echo(Some(1), "crystalia", 50, 111, 60, 1_000);
    assert!(
        store
            .active_boss_echo(Some(1), "crystalia", 4_600)
            .is_none()
    );
}

#[test]
fn test_overwrite_restarts_window() {
    let mut store = InMemoryBossStore::default();
    store.record_boss_echo(Some(1), "magmus_rex", 75, 111, 60, 1_000);
    store.record_boss_echo(Some(1), "magmus_rex", 75, 222, 3_600, 1_010);
    assert_eq!(
        store
            .active_boss_echo(Some(1), "magmus_rex", 1_011)
            .expect("echo")
            .killer_discord_id,
        222
    );
}

#[test]
fn test_boss_id_isolation() {
    let mut store = InMemoryBossStore::default();
    store.record_boss_echo(Some(1), "pudge", 25, 1, 3_600, 1_000);
    store.record_boss_echo(Some(1), "crystalia", 50, 2, 3_600, 1_000);
    assert_eq!(
        store
            .active_boss_echo(Some(1), "pudge", 1_001)
            .expect("pudge")
            .killer_discord_id,
        1
    );
    assert_eq!(
        store
            .active_boss_echo(Some(1), "crystalia", 1_001)
            .expect("crystalia")
            .killer_discord_id,
        2
    );
    assert!(store.active_boss_echo(Some(1), "grothak", 1_001).is_none());
    assert!(
        store
            .active_boss_echo(Some(1), "ogre_magi", 1_001)
            .is_none()
    );
}

#[test]
fn test_guild_isolation() {
    let mut store = InMemoryBossStore::default();
    store.record_boss_echo(Some(1_000), "grothak", 25, 1, 3_600, 1_000);
    store.record_boss_echo(Some(2_000), "grothak", 25, 2, 3_600, 1_000);
    assert_eq!(
        store
            .active_boss_echo(Some(1_000), "grothak", 1_001)
            .expect("guild one")
            .killer_discord_id,
        1
    );
    assert_eq!(
        store
            .active_boss_echo(Some(2_000), "grothak", 1_001)
            .expect("guild two")
            .killer_discord_id,
        2
    );
    assert!(store.active_boss_echo(None, "grothak", 1_001).is_none());
}

#[test]
fn test_pinnacle_boundary_requires_all_tiers_defeated() {
    let mut progress = all_tiers_defeated();
    progress.insert("275".to_owned(), entry(BossStatus::Active));
    assert_eq!(at_boss_boundary(PINNACLE_DEPTH - 1, &progress), Some(275));
}

#[test]
fn test_pinnacle_boundary_fires_when_tiers_cleared() {
    assert_eq!(
        at_boss_boundary(PINNACLE_DEPTH - 1, &all_tiers_defeated()),
        Some(PINNACLE_DEPTH)
    );
}

#[test]
fn test_pinnacle_does_not_re_fire_after_defeat() {
    let mut progress = all_tiers_defeated();
    progress.insert(PINNACLE_DEPTH.to_string(), entry(BossStatus::Defeated));
    assert_eq!(at_boss_boundary(PINNACLE_DEPTH - 1, &progress), None);
}

#[test]
fn test_reproc_fires_past_threshold_when_pinnacle_undefeated() {
    assert_eq!(
        at_boss_boundary(PINNACLE_REPROC_DEPTH + 56, &all_tiers_defeated()),
        Some(PINNACLE_DEPTH)
    );
}

#[test]
fn test_reproc_fires_at_hard_cap_so_prestige_can_unlock() {
    assert_eq!(
        at_boss_boundary(PRESTIGE_HARD_CAP, &all_tiers_defeated()),
        Some(PINNACLE_DEPTH)
    );
}

#[test]
fn test_pinnacle_reproc_yields_to_active_tier_boss() {
    let mut progress = all_tiers_defeated();
    progress.insert("275".to_owned(), entry(BossStatus::Active));
    assert_eq!(
        at_boss_boundary(PINNACLE_REPROC_DEPTH + 50, &progress),
        Some(275)
    );
}

#[test]
fn test_reproc_does_not_fire_if_pinnacle_already_defeated() {
    let mut progress = all_tiers_defeated();
    progress.insert(PINNACLE_DEPTH.to_string(), entry(BossStatus::Defeated));
    assert_eq!(
        at_boss_boundary(PINNACLE_REPROC_DEPTH + 50, &progress),
        None
    );
}

#[test]
fn test_pinnacle_reproc_fires_immediately_past_threshold() {
    let below_legacy_reproc = PINNACLE_REPROC_DEPTH - 50;
    assert_eq!(
        at_boss_boundary(below_legacy_reproc, &all_tiers_defeated()),
        Some(PINNACLE_DEPTH)
    );
}

#[test]
fn test_next_boundary_returns_skipped_active_boss() {
    let mut progress = all_tiers_defeated();
    progress.insert("200".to_owned(), entry(BossStatus::Active));
    assert_eq!(next_boss_boundary(&progress), Some(200));
}

#[test]
fn test_at_boundary_fires_for_parked_player() {
    let mut progress = all_tiers_defeated();
    progress.insert("200".to_owned(), entry(BossStatus::Active));
    assert_eq!(at_boss_boundary(250, &progress), Some(200));
}

#[test]
fn test_lowest_active_returned_when_multiple_skipped() {
    let mut progress = all_tiers_defeated();
    progress.insert("100".to_owned(), entry(BossStatus::Active));
    progress.insert("200".to_owned(), entry(BossStatus::Active));
    assert_eq!(next_boss_boundary(&progress), Some(100));
    assert_eq!(at_boss_boundary(250, &progress), Some(100));
}

#[test]
fn test_partial_phase_state_also_catches_up() {
    let mut progress = all_tiers_defeated();
    progress.insert("200".to_owned(), entry(BossStatus::PhaseOneDefeated));
    assert_eq!(at_boss_boundary(240, &progress), Some(200));
}

#[test]
fn test_lock_picks_from_pool_and_persists() {
    let mut state = BossRunState::default();
    let first = lock_pinnacle_boss(&mut state, None).expect("lock");
    assert!(PINNACLE_POOL_IDS.contains(&first));
    let second = lock_pinnacle_boss(&mut state, Some("lantern_engine")).expect("idempotent lock");
    assert_eq!(first, second);
}

fn assert_pinnacle_lock(boss_id: &str) {
    let mut state = BossRunState::default();
    let locked = lock_pinnacle_boss(&mut state, Some(boss_id)).expect("lock chosen pinnacle");
    assert_eq!(locked, boss_id);
    assert_eq!(state.pinnacle_boss_id.as_deref(), Some(boss_id));
    assert_eq!(lock_pinnacle_boss(&mut state, None), Ok(locked));
}

#[test]
fn test_new_pinnacle_choice_is_locked_and_persisted_bastion_below() {
    assert_pinnacle_lock("bastion_below");
}

#[test]
fn test_new_pinnacle_choice_is_locked_and_persisted_pale_surveyor() {
    assert_pinnacle_lock("pale_surveyor");
}

#[test]
fn test_new_pinnacle_choice_is_locked_and_persisted_lantern_engine() {
    assert_pinnacle_lock("lantern_engine");
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

fn pinnacle_state(boss_id: &str) -> BossRunState {
    let mut state = BossRunState::with_all_tiers_defeated(PINNACLE_DEPTH - 1);
    state.pinnacle_boss_id = Some(boss_id.to_owned());
    state.pinnacle_phase = 1;
    state
}

#[test]
fn test_phase1_win_advances_to_phase2() {
    let mut state = pinnacle_state("first_digger");
    let result = start_pinnacle_fight(&mut state, PinnacleFightDirective::WinWithoutMechanic)
        .expect("fight");
    assert!(result.success);
    assert_eq!(result.won, Some(true));
    assert!(result.is_pinnacle);
    assert_eq!(result.phase, 1);
    assert!(result.phase_two_incoming);
    assert_eq!(state.pinnacle_phase, 2);
    assert_eq!(
        state.boss_progress[&PINNACLE_DEPTH.to_string()].status(),
        BossStatus::PhaseOneDefeated
    );
}

#[test]
fn test_pinnacle_mechanic_pauses_then_resume_resolves() {
    let mut state = pinnacle_state("forgotten_king");
    let pending = start_pinnacle_fight(
        &mut state,
        PinnacleFightDirective::PauseAtMechanic { mechanic_index: 0 },
    )
    .expect("start");
    assert!(pending.pending_prompt);
    assert_eq!(pending.mechanic_id.as_deref(), Some("king_decree"));
    assert_eq!(pending.phase, 1);
    assert_eq!(
        state.active_duel.as_ref().expect("active duel").boss_id,
        "forgotten_king"
    );
    let resumed = resume_pinnacle_fight(&mut state, 0, false).expect("resume");
    assert!(resumed.success);
    assert!(!resumed.pending_prompt);
    assert_eq!(resumed.boss_id, "forgotten_king");
    assert!(state.active_duel.is_none());
}

#[test]
fn test_pinnacle_resume_processes_mechanic_status_effects() {
    let mut state = pinnacle_state("forgotten_king");
    let pending = start_pinnacle_fight(
        &mut state,
        PinnacleFightDirective::PauseAtMechanic { mechanic_index: 1 },
    )
    .expect("start");
    assert_eq!(pending.mechanic_id.as_deref(), Some("king_crownfall"));
    let resumed = resume_pinnacle_fight(&mut state, 2, true).expect("resume");
    assert_eq!(resumed.observed_bleed_rounds, [3, 2]);
}

#[test]
fn test_loss_persists_phase_hp() {
    let mut state = pinnacle_state("forgotten_king");
    let result = start_pinnacle_fight(
        &mut state,
        PinnacleFightDirective::LoseWithoutMechanic {
            ending_boss_hp: 4,
            boss_hp_max: 18,
        },
    )
    .expect("fight");
    assert_eq!(result.won, Some(false));
    assert_eq!(result.boss_id, "forgotten_king");
    assert_eq!(result.boundary, PINNACLE_DEPTH);
    let phase_key = format!("{PINNACLE_DEPTH}:1");
    let BossProgressValue::Entry(stored) = &state.boss_progress[&phase_key] else {
        panic!("phase hp")
    };
    assert!(stored.hp_remaining.expect("hp") >= 0);
}

#[test]
fn test_cannot_prestige_without_pinnacle_defeat() {
    let state = BossRunState::with_all_tiers_defeated(PINNACLE_DEPTH);
    let check = can_prestige(&state, 10);
    assert!(!check.can_prestige);
    assert!(check.reason.expect("reason").contains("stirs deeper"));
}

fn prestige_ready_state() -> BossRunState {
    let mut state = BossRunState::with_all_tiers_defeated(PINNACLE_DEPTH);
    state
        .boss_progress
        .insert(PINNACLE_DEPTH.to_string(), entry(BossStatus::Defeated));
    state
}

#[test]
fn test_can_prestige_with_pinnacle_defeated() {
    assert!(can_prestige(&prestige_ready_state(), 10).can_prestige);
}

#[test]
fn test_prestige_resets_pinnacle_state() {
    let mut state = prestige_ready_state();
    state.pinnacle_boss_id = Some("forgotten_king".to_owned());
    state.pinnacle_phase = 3;
    prestige_reset(&mut state, 10).expect("prestige");
    assert!(state.pinnacle_boss_id.is_none());
    assert_eq!(state.pinnacle_phase, 0);
}

#[test]
fn test_prestige_clears_stale_pinnacle_phase_keys() {
    let mut state = prestige_ready_state();
    for phase in 1..=3 {
        state.boss_progress.insert(
            format!("{PINNACLE_DEPTH}:{phase}"),
            entry(BossStatus::Active),
        );
    }
    prestige_reset(&mut state, 10).expect("prestige");
    for key in [
        PINNACLE_DEPTH.to_string(),
        format!("{PINNACLE_DEPTH}:1"),
        format!("{PINNACLE_DEPTH}:2"),
        format!("{PINNACLE_DEPTH}:3"),
    ] {
        assert!(!state.boss_progress.contains_key(&key), "{key}");
    }
}

#[test]
fn test_foreshadow_after_t275_cleared() {
    let state = BossRunState::with_all_tiers_defeated(290);
    let line = pinnacle_foreshadow_line(&state).expect("foreshadow");
    assert!(PINNACLE_FORESHADOW_LINES.contains(&line));
}

#[test]
fn test_no_foreshadow_when_pinnacle_defeated() {
    let state = prestige_ready_state();
    assert!(pinnacle_foreshadow_line(&state).is_none());
}

#[test]
fn test_no_foreshadow_when_tiers_incomplete() {
    let mut state = BossRunState::with_all_tiers_defeated(274);
    state
        .boss_progress
        .insert("275".to_owned(), entry(BossStatus::Active));
    assert!(pinnacle_foreshadow_line(&state).is_none());
}

#[test]
fn test_drop_creates_artifact_with_two_stats() {
    let relic = roll_pinnacle_relic(0, "forgotten_king", 0, 1).expect("relic");
    assert!(relic.name.starts_with("Crown of "));
    assert_eq!(relic.stats.len(), 2);
    assert_eq!(relic.stat_ids.len(), 2);
    assert_ne!(relic.stat_ids[0], relic.stat_ids[1]);
    let mut store = InMemoryBossStore::default();
    assert!(
        store.artifacts(10_001, 1).is_empty(),
        "rolling alone must not persist"
    );
    let id = store.settle_relic(10_001, 1, &relic);
    assert!(
        store
            .artifacts(10_001, 1)
            .iter()
            .any(|artifact| artifact.artifact_id.starts_with("pinnacle:"))
    );
    assert!(
        store
            .artifacts(10_001, 1)
            .iter()
            .any(|artifact| artifact.id == id)
    );
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

#[test]
fn test_pinnacle_info_returns_phase_title() {
    let mut state = pinnacle_state("forgotten_king");
    state.pinnacle_phase = 2;
    let info = build_pinnacle_info(&state).expect("info");
    assert!(info.is_pinnacle);
    assert_eq!(info.phase, 2);
    assert_eq!(info.phase_total, 3);
    assert_eq!(info.name, "The Crowned Hunger");
}

const AFFECTED_TIERS: [i32; 3] = [150, 200, 275];

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
    let expected = [(0, 3), (1, 3), (2, 5), (3, 6), (4, 7), (8, 7)];
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
        assert_eq!(full_boss_pool_for_tier(tier).len(), 7);
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
        assert!(stinger_registered(boss.stinger_id));
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
    let mut state = BossRunState {
        prestige_level: 0,
        ..BossRunState::default()
    };
    let pool = boss_pool_for_tier(200, state.prestige_level);
    assert_eq!(
        boss_ids(&pool),
        ["chronofrost", "faceless_void", "weaver"]
            .into_iter()
            .collect()
    );
    let locked = lock_regular_boss(&mut state, 200, None).expect("lock");
    assert_eq!(locked.boss_id, "chronofrost");
}

#[test]
fn test_p3_player_can_lock_late_prestige_boss() {
    let mut state = BossRunState {
        prestige_level: 3,
        ..BossRunState::default()
    };
    assert_eq!(
        lock_regular_boss(&mut state, 200, Some("lilith"))
            .expect("P3 lock")
            .boss_id,
        "lilith"
    );
}

#[test]
fn test_p1_player_never_locks_prestige2_boss() {
    let mut state = BossRunState {
        prestige_level: 1,
        ..BossRunState::default()
    };
    let pool = boss_pool_for_tier(150, state.prestige_level);
    assert_eq!(
        boss_ids(&pool),
        ["sporeling_sovereign", "treant_protector", "broodmother"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        lock_regular_boss(&mut state, 150, None)
            .expect("lock")
            .boss_id,
        "sporeling_sovereign"
    );
}

#[test]
fn test_p2_player_can_lock_prestige2_boss() {
    let mut state = BossRunState {
        prestige_level: 2,
        ..BossRunState::default()
    };
    assert_eq!(
        lock_regular_boss(&mut state, 150, Some("aegis_warden"))
            .expect("P2 lock")
            .boss_id,
        "aegis_warden"
    );
}

fn assert_cannot_lock_new_prestige_two(tier: i32, boss_id: &str) {
    let mut state = BossRunState {
        prestige_level: 1,
        ..BossRunState::default()
    };
    assert_eq!(
        lock_regular_boss(&mut state, tier, Some(boss_id)),
        Err(BossLockError::PreferredBossUnavailable(boss_id.to_owned())),
    );
    let locked = lock_regular_boss(&mut state, tier, None).expect("fallback lock");
    assert_ne!(locked.boss_id, boss_id);
    let BossProgressValue::Entry(stored) = &state.boss_progress[&tier.to_string()] else {
        panic!("persisted lock")
    };
    assert_ne!(stored.boss_id.as_deref(), Some(boss_id));
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
    let mut state = BossRunState {
        prestige_level,
        ..BossRunState::default()
    };
    let locked = lock_regular_boss(&mut state, tier, Some(boss_id)).expect("unlocked boss");
    assert_eq!(locked.boss_id, boss_id);
    let BossProgressValue::Entry(stored) = &state.boss_progress[&tier.to_string()] else {
        panic!("persisted lock")
    };
    assert_eq!(stored.boss_id.as_deref(), Some(boss_id));
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

fn wrapped_pinnacle_result() -> DictObject {
    let relic = DictObject::new(BTreeMap::from([
        (
            "name".to_owned(),
            NestedValue::Text("Knot of Voices".to_owned()),
        ),
        (
            "stats".to_owned(),
            NestedValue::TextList(vec!["Tougher skin".to_owned(), "Richer veins".to_owned()]),
        ),
    ]));
    DictObject::new(BTreeMap::from([(
        "pinnacle_relic".to_owned(),
        NestedValue::Object(relic),
    )]))
}

#[test]
fn test_get_on_wrapped_nested_dict_returns_value() {
    let wrapped = wrapped_pinnacle_result();
    let Some(NestedValue::Object(relic)) = wrapped.get("pinnacle_relic") else {
        panic!("nested relic")
    };
    assert_eq!(
        relic.get("name"),
        Some(&NestedValue::Text("Knot of Voices".to_owned()))
    );
    assert_eq!(
        relic.get("stats"),
        Some(&NestedValue::TextList(vec![
            "Tougher skin".to_owned(),
            "Richer veins".to_owned()
        ])),
    );
    assert_eq!(
        relic
            .get("missing")
            .unwrap_or(&NestedValue::Text("default".to_owned())),
        &NestedValue::Text("default".to_owned())
    );
}

#[test]
fn test_pinnacle_phase3_result_renders_to_embed() {
    let embed = build_pinnacle_victory_embed(&wrapped_pinnacle_result());
    let relic_fields: Vec<&EmbedField> = embed
        .fields
        .iter()
        .filter(|field| field.name.starts_with("Relic:"))
        .collect();
    assert_eq!(relic_fields.len(), 1);
    assert!(relic_fields[0].name.contains("Knot of Voices"));
}

#[test]
fn test_returns_true_when_lantern_in_inventory() {
    let mut store = InMemoryBossStore::default();
    assert!(!store.has_scout_lantern(10_001, 1));
    store.add_inventory_item(10_001, 1, "lantern");
    assert!(store.has_scout_lantern(10_001, 1));
}

#[test]
fn test_returns_true_for_great_lantern_owner() {
    let mut store = InMemoryBossStore::default();
    store.add_inventory_item(10_001, 1, "great_lantern");
    assert!(store.has_scout_lantern(10_001, 1));
}

#[test]
fn test_false_when_no_lantern_owned() {
    assert!(!InMemoryBossStore::default().has_scout_lantern(10_001, 1));
}
