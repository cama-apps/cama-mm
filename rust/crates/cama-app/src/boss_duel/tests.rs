use super::*;

fn assert_close(left: f64, right: f64) {
    assert!((left - right).abs() < 1.0e-9, "{left} != {right}");
}

fn pet(mood: PetMood, stage: PetStage, training_xp: i32) -> PetSnapshot {
    PetSnapshot {
        name: "Dane".to_owned(),
        species_id: "common_cama".to_owned(),
        species_name: "Common Cama".to_owned(),
        training_xp,
        stage,
        mood,
    }
}

fn assist(mood: PetMood) -> PetAssist {
    pet_boss_assist(&pet(mood, PetStage::Adult, PET_TRAINING_CAP)).expect("eligible pet")
}

fn victory(wager: i64) -> VictoryInput {
    VictoryInput {
        depth: 25,
        prestige: 0,
        risk: RiskTier::Cautious,
        wager,
        win_chance: 0.50,
        echo_applied: false,
        drain_next_reward: false,
    }
}

fn progress(pending: Option<&str>) -> BossProgressEntry {
    BossProgressEntry {
        boss_id: "grothak".to_owned(),
        status: "phase1_defeated".to_owned(),
        pending_phase_event: pending.map(str::to_owned),
    }
}

fn gear() -> Vec<GearPiece> {
    vec![
        GearPiece {
            id: 1,
            name: "Wooden Pickaxe".to_owned(),
            slot: GearSlot::Weapon,
            durability: 20,
            equipped: true,
        },
        GearPiece {
            id: 2,
            name: "Leather Armor".to_owned(),
            slot: GearSlot::Armor,
            durability: 20,
            equipped: true,
        },
        GearPiece {
            id: 3,
            name: "Old Boots".to_owned(),
            slot: GearSlot::Boots,
            durability: 20,
            equipped: true,
        },
        GearPiece {
            id: 4,
            name: "Void-Touched Amulet".to_owned(),
            slot: GearSlot::Amulet,
            durability: 20,
            equipped: true,
        },
    ]
}

fn repository(balance: i64, depth: i32) -> InMemoryBossDuelRepository {
    let mut repository = InMemoryBossDuelRepository::default();
    repository.insert(
        1,
        PersistedDuelState {
            balance,
            depth,
            stat_points: 5,
            defeated_boundaries: BTreeSet::new(),
            revision: 0,
        },
    );
    repository
}

#[test]
fn test_fractional_damage_bonus_uses_stochastic_rounding_1_10_0_09_1() {
    let mut rng = SequenceRng::new(vec![0.09], vec![]);
    assert_eq!(fractional_damage_bonus(1, 10, &mut rng), 1);
}

#[test]
fn test_fractional_damage_bonus_uses_stochastic_rounding_1_10_0_1_0() {
    let mut rng = SequenceRng::new(vec![0.10], vec![]);
    assert_eq!(fractional_damage_bonus(1, 10, &mut rng), 0);
}

#[test]
fn test_fractional_damage_bonus_uses_stochastic_rounding_3_12_0_35_1() {
    let mut rng = SequenceRng::new(vec![0.35], vec![]);
    assert_eq!(fractional_damage_bonus(3, 12, &mut rng), 1);
}

#[test]
fn test_fractional_damage_bonus_uses_stochastic_rounding_3_12_0_36_0() {
    let mut rng = SequenceRng::new(vec![0.36], vec![]);
    assert_eq!(fractional_damage_bonus(3, 12, &mut rng), 0);
}

#[test]
fn test_fractional_damage_bonus_uses_stochastic_rounding_12_12_0_43_2() {
    let mut rng = SequenceRng::new(vec![0.43], vec![]);
    assert_eq!(fractional_damage_bonus(12, 12, &mut rng), 2);
}

#[test]
fn test_fractional_damage_bonus_uses_stochastic_rounding_12_12_0_44_1() {
    let mut rng = SequenceRng::new(vec![0.44], vec![]);
    assert_eq!(fractional_damage_bonus(12, 12, &mut rng), 1);
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_happy_12() {
    assert_eq!(assist(PetMood::Happy).bonus_percent, 12);
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_content_10() {
    assert_eq!(assist(PetMood::Content).bonus_percent, 10);
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_hungry_8() {
    assert_eq!(assist(PetMood::Hungry).bonus_percent, 8);
}

#[test]
fn test_fully_trained_adult_pet_assist_scales_with_mood_starving_5() {
    assert_eq!(assist(PetMood::Starving).bonus_percent, 5);
}

#[test]
fn test_pet_boss_assist_requires_adult_and_training_cap_baby_20() {
    assert_eq!(
        pet_boss_assist(&pet(PetMood::Happy, PetStage::Baby, PET_TRAINING_CAP)),
        None
    );
}

#[test]
fn test_pet_boss_assist_requires_adult_and_training_cap_adult_19() {
    assert_eq!(
        pet_boss_assist(&pet(PetMood::Happy, PetStage::Adult, PET_TRAINING_CAP - 1,)),
        None
    );
}

#[test]
fn test_win_probability_models_fractional_player_damage_bonus() {
    let stats = DuelStats {
        player_hp: 1,
        boss_hp: 2,
        player_hit: 1.0,
        player_damage: 1,
        boss_hit: 1.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
    };
    assert_close(exact_duel_win_probability(stats, 0), 0.05);
    assert_close(exact_duel_win_probability(stats, 100), 0.95);
}

#[test]
fn test_shared_round_applies_and_records_pet_assist() {
    let mut stats = DuelStats {
        player_hp: 5,
        boss_hp: 6,
        player_hit: 1.0,
        player_damage: 3,
        boss_hit: 0.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
    };
    let pet = PetAssist {
        bonus_percent: 100,
        ..assist(PetMood::Happy)
    };
    let mut rng = SequenceRng::new(vec![0.0], vec![]);
    let outcome = run_round(1, &mut stats, Some(&pet), &mut rng);
    assert_eq!(outcome.terminal, Some(true));
    assert_eq!(outcome.record.boss_hp, 0);
    assert_eq!(outcome.record.pet_assist, Some(pet));
    assert_eq!(outcome.record.pet_assist_damage, 3);
}

#[test]
fn test_legacy_boss_fight_applies_pet_assist() {
    let pet = assist(PetMood::Happy);
    let mut stats = DuelStats::cautious();
    let mut rng = SequenceRng::new(vec![0.0; 20], vec![]);
    let mut total_assist = 0;
    for round in 1..=20 {
        let outcome = run_round(round, &mut stats, Some(&pet), &mut rng);
        total_assist += outcome.record.pet_assist_damage;
        if outcome.terminal == Some(true) {
            break;
        }
    }
    assert!(total_assist > 0);
    assert_eq!(pet.bonus_percent, 12);
    assert_eq!(stats.boss_hp, 0);
}

#[test]
fn test_paused_boss_duel_persists_pet_assist() {
    let pet = assist(PetMood::Content);
    let mut rng = SequenceRng::new(vec![], vec![2]);
    let paused = start_paused_duel(
        &GROTHAK_MECHANICS,
        Some(pet.clone()),
        0.0,
        0,
        vec![],
        &mut rng,
    );
    let resumed = resume_paused_duel(&paused);
    assert_eq!(paused.pet_assist, Some(pet));
    assert_eq!(resumed.pet_assist, paused.pet_assist);
}

#[test]
fn test_regular_scout_models_and_reports_pet_assist() {
    let pet = assist(PetMood::Hungry);
    let stats = DuelStats::cautious();
    let baseline = exact_duel_win_probability(stats, 0);
    let assisted = exact_duel_win_probability(stats, pet.bonus_percent);
    assert_eq!(pet.bonus_percent, 8);
    assert!(assisted >= baseline);
}

#[test]
fn test_player_first_one_shot_boss_never_swings() {
    let mut stats = DuelStats {
        player_damage: 100,
        player_hit: 1.0,
        boss_hit: 1.0,
        ..DuelStats::reckless()
    };
    let mut rng = SequenceRng::new(vec![0.0, 0.0], vec![]);
    let outcome = run_round(1, &mut stats, None, &mut rng);
    assert_eq!(outcome.terminal, Some(true));
    assert_eq!(outcome.record.boss_hit, None);
}

#[test]
fn test_wagered_fights_keep_a_ten_percent_hit_floor_legacy() {
    assert_close(effective_player_hit(0.0, 10, false), 0.10);
}

#[test]
fn test_wagered_fights_keep_a_ten_percent_hit_floor_interactive() {
    let hit = effective_player_hit(-0.5, 10, false);
    let mut stats = DuelStats {
        player_hit: hit,
        player_damage: 100,
        ..DuelStats::reckless()
    };
    let mut rng = SequenceRng::new(vec![0.09], vec![]);
    assert_eq!(
        run_round(1, &mut stats, None, &mut rng).terminal,
        Some(true)
    );
}

#[test]
fn test_voluntary_free_fight_uses_seventy_percent_accuracy() {
    assert_close(effective_player_hit(0.60, 0, false), 0.42);
}

#[test]
fn test_pause_resume_uses_persisted_selected_mechanic() {
    let mut rng = SequenceRng::new(vec![], vec![2]);
    let paused = start_paused_duel(&GROTHAK_MECHANICS, None, 0.0, 0, vec![], &mut rng);
    let resumed = resume_paused_duel(&paused);
    assert_eq!(paused.mechanic_id, "grothak_bedrock_bellow");
    assert_eq!(resumed.mechanic_id, paused.mechanic_id);
}

#[test]
fn test_later_attempt_rerolls_from_full_pool_and_may_repeat() {
    let mut rng = SequenceRng::new(vec![], vec![0, 0]);
    let first = start_paused_duel(&GROTHAK_MECHANICS, None, 0.0, 0, vec![], &mut rng);
    let second = start_paused_duel(&GROTHAK_MECHANICS, None, 0.0, 0, vec![], &mut rng);
    assert_eq!(first.mechanic_id, "grothak_earthquake");
    assert_eq!(first.mechanic_id, second.mechanic_id);
    assert_eq!(GROTHAK_MECHANICS.len(), 3);
}

#[test]
fn test_boss_hp_scales_with_depth() {
    let stats = DuelStats::cautious().scaled("chronofrost", 200, 0, false);
    assert_eq!(stats.boss_hp - stats.player_damage, 8);
}

#[test]
fn test_boss_hp_scales_with_prestige() {
    let stats = DuelStats::cautious().scaled("grothak", 25, 3, false);
    assert_eq!(stats.boss_hp - stats.player_damage, 15);
}

#[test]
fn test_boss_hp_scales_extra_hard_at_p4() {
    let table_hp = 4 + 14;
    let stats = DuelStats::cautious().scaled("grothak", 25, 4, false);
    assert!(stats.boss_hp > table_hp);
    assert_eq!(stats.boss_hp - stats.player_damage, 18);
}

#[test]
fn test_positive_boss_payout_uses_dig_reward_policy() {
    let result = settle_victory(victory(0));
    assert_eq!(result.gross_base_reward, 15);
    assert_eq!(result.scaled_base_reward, 10);
    assert_eq!(result.credited, 10);
    assert_eq!(result.audit_jc_delta, result.credited);
}

#[test]
fn test_p4_ascension_scales_regular_boss_base_reward_legacy() {
    let result = settle_victory(VictoryInput {
        prestige: 4,
        ..victory(0)
    });
    assert_eq!(result.gross_base_reward, 22);
    assert_eq!(result.credited, scale_positive_dig_jc(22));
}

#[test]
fn test_p4_ascension_scales_regular_boss_base_reward_state_machine() {
    let result = settle_victory(VictoryInput {
        prestige: 4,
        ..victory(0)
    });
    assert_eq!(result.credited, 14);
    assert_eq!(result.audit_jc_delta, result.credited);
}

#[test]
fn test_win_pays_from_payout_table() {
    let result = settle_victory(victory(10));
    let expected_profit = (10.0 * 1.5_f64.ln()) as i64;
    assert_eq!(result.wager_profit, expected_profit);
    assert_eq!(result.credited, scale_positive_dig_jc(15) + expected_profit);
}

#[test]
fn test_duel_audit_log_jc_delta_matches_real_payout() {
    let result = settle_victory(victory(10));
    let mut service = BossDuelSettlementService::new(repository(200, 24));
    let state = service
        .settle_victory_atomic(1, 25, result)
        .expect("atomic victory");
    assert!(result.credited > 0);
    assert_eq!(state.balance - 200, result.credited);
    assert_eq!(service.repository().audits()[0].jc_delta, result.credited);
}

#[test]
fn test_loss_applies_knockback() {
    let result = settle_loss(LossInput {
        balance: 500,
        depth: 99,
        wager: 10,
        forced_no_wager_phase: false,
        knockback_roll: 16,
        resolved_at: 1_000_000,
    });
    assert!((BOSS_LOSS_KNOCKBACK_MIN..=BOSS_LOSS_KNOCKBACK_MAX).contains(&result.knockback));
    assert_eq!(result.depth_after, 99 - result.knockback);
    assert_eq!(result.balance_after, 490);
}

#[test]
fn test_loss_takes_extra_gear_tick_and_extends_cooldown() {
    let mut equipped = gear();
    let broken = tick_resolved_fight_gear(&mut equipped, false);
    assert!(broken.is_empty());
    assert_eq!(equipped[1].durability, 18);
    assert!(
        equipped
            .iter()
            .enumerate()
            .all(|(index, piece)| { index == 1 || piece.durability == 19 })
    );
    let result = settle_loss(LossInput {
        balance: 500,
        depth: 99,
        wager: 10,
        forced_no_wager_phase: false,
        knockback_roll: 11,
        resolved_at: 1_000_000,
    });
    assert_eq!(result.next_dig_at, 1_003_600);
}

#[test]
fn test_loss_suppresses_soften_line_when_no_chip_damage() {
    assert_eq!(soften_line(8, 8, 8), None);
}

#[test]
fn test_loss_soften_line_present_when_player_chips() {
    let line = soften_line(6, 5, 6).expect("chip damage has copy");
    assert!(line.contains("5/6"));
}

#[test]
fn test_milestone_awarded_once() {
    assert_eq!(milestone_bonus(40, 25), 0);
    assert_eq!(milestone_bonus(24, 25), 3);
}

#[test]
fn test_cautious_high_and_reckless_low_on_first_boss() {
    assert!(exact_duel_win_probability(DuelStats::cautious(), 0) > 0.65);
    assert!(exact_duel_win_probability(DuelStats::reckless(), 0) < 0.35);
}

#[test]
fn test_forced_no_wager_phase_previews_full_accuracy() {
    assert_close(effective_player_hit(0.55, 0, true), 0.55);
}

#[test]
fn test_phase_scout_matches_live_modifiers_without_consuming_event() {
    let scout_progress = progress(Some("fissure"));
    let mut live_progress = scout_progress.clone();
    let mut stats = DuelStats {
        player_hit: 0.60,
        ..DuelStats::cautious()
    };
    let preview = transition_event(
        scout_progress
            .pending_phase_event
            .as_deref()
            .expect("pending event"),
    )
    .expect("known event");
    let preview_hit = stats.player_hit + preview.player_hit_offset;
    live_progress.consume_transition(&mut stats);
    assert_eq!(
        scout_progress.pending_phase_event.as_deref(),
        Some("fissure")
    );
    assert_close(preview_hit, stats.player_hit);
}

#[test]
fn test_second_kill_reduces_only_wager_profit_legacy() {
    let normal = settle_victory(victory(10));
    let echo = settle_victory(VictoryInput {
        echo_applied: true,
        ..victory(10)
    });
    assert_eq!(echo.scaled_base_reward, normal.scaled_base_reward);
    assert_eq!(echo.wager_profit, (normal.wager_profit as f64 * 0.7) as i64);
}

#[test]
fn test_second_kill_reduces_only_wager_profit_state_machine() {
    let echo = BossEcho::refreshed(10_001, 1_000_000);
    assert!(echo.applies_to(10_002, 1_000_001));
    let result = settle_victory(VictoryInput {
        echo_applied: true,
        ..victory(10)
    });
    assert_eq!(
        result.credited,
        result.scaled_base_reward + result.wager_profit
    );
}

#[test]
fn test_echo_scout_multiplier_matches_profit_penalty() {
    let normal = scout_total_return_multiplier(victory(10));
    let echo = scout_total_return_multiplier(VictoryInput {
        echo_applied: true,
        ..victory(10)
    });
    assert_close(echo, 1.0 + (normal - 1.0) * 0.7);
}

#[test]
fn test_scout_uses_paid_and_free_accuracy_floors() {
    for _ in 0..3 {
        assert_close(effective_player_hit(0.0, 10, false), 0.10);
        assert_close(effective_player_hit(0.0, 0, false), 0.05);
    }
}

#[test]
fn test_p4_echo_scout_multiplier_includes_boss_rage() {
    let p4 = VictoryInput {
        prestige: 4,
        echo_applied: true,
        ..victory(10)
    };
    let live = scout_total_return_multiplier(VictoryInput {
        echo_applied: false,
        ..p4
    });
    assert_close(
        scout_total_return_multiplier(p4),
        1.0 + (live - 1.0) * ECHO_PROFIT_MULTIPLIER,
    );
}

#[test]
fn test_echo_reduces_profit_after_drain_penalty() {
    let drained = settle_victory(VictoryInput {
        wager: 100,
        echo_applied: false,
        drain_next_reward: true,
        ..victory(0)
    });
    let echoed = settle_victory(VictoryInput {
        wager: 100,
        echo_applied: true,
        drain_next_reward: true,
        ..victory(0)
    });
    assert_eq!(
        echoed.wager_profit,
        (drained.wager_profit as f64 * ECHO_PROFIT_MULTIPLIER) as i64
    );
}

#[test]
fn test_echo_result_copy_names_max_hp_and_wager_profit() {
    let copy = echo_explanation();
    assert!(copy.contains("-25% max HP"));
    assert!(copy.contains("30% less wager profit"));
}

#[test]
fn test_killer_reruns_get_no_discount() {
    let echo = BossEcho::refreshed(10_001, 1_000_000);
    assert!(!echo.applies_to(10_001, 1_000_001));
}

#[test]
fn test_beneficiary_kill_refreshes_echo_to_themselves() {
    let first = BossEcho::refreshed(10_001, 1_000_000);
    assert!(first.applies_to(10_002, 1_000_001));
    let refreshed = BossEcho::refreshed(10_002, 1_000_010);
    assert_eq!(refreshed.killer_id, 10_002);
    assert!(!refreshed.applies_to(10_002, 1_000_011));
}

#[test]
fn test_expired_echo_not_applied() {
    let echo = BossEcho {
        killer_id: 9_999,
        weakened_until: 1_000_060,
    };
    assert!(!echo.applies_to(10_001, 1_003_600));
}

#[test]
fn test_stale_row_triggers_cleanup_tick() {
    let mut equipped = gear();
    let broken = cleanup_abandoned_duel(&mut equipped, Some(&[]));
    assert!(broken.is_empty());
    assert!(equipped.iter().all(|piece| piece.durability == 19));
}

#[test]
fn test_stale_cleanup_reports_snapshot_gear_that_breaks() {
    let mut equipped = gear();
    equipped[1].durability = 1;
    equipped[1].equipped = false;
    let broken = cleanup_abandoned_duel(&mut equipped, Some(&[2]));
    assert_eq!(broken, ["Leather Armor"]);
    assert_eq!(equipped[1].durability, 0);
    assert_eq!(equipped[0].durability, 20);
}

#[test]
fn test_no_stale_row_means_no_cleanup_tick() {
    let mut equipped = gear();
    assert!(cleanup_abandoned_duel(&mut equipped, None).is_empty());
    assert!(equipped.iter().all(|piece| piece.durability == 20));
}

#[test]
fn test_fight_boss_stores_pending_phase_event_on_transition() {
    let mut entry = BossProgressEntry {
        boss_id: "grothak".to_owned(),
        status: "active".to_owned(),
        pending_phase_event: None,
    };
    let mut rng = SequenceRng::new(vec![], vec![1]);
    let event_id = entry.enter_next_phase(&mut rng);
    assert_eq!(entry.status, "phase1_defeated");
    assert_eq!(
        entry.pending_phase_event.as_deref(),
        Some(event_id.as_str())
    );
    assert!(transition_event(&event_id).is_some());
    assert_eq!(PHASE_TRANSITION_EVENTS.len(), 14);
}

#[test]
fn test_fight_boss_phase2_applies_and_consumes_event() {
    let mut entry = progress(Some("void_pull"));
    let mut stats = DuelStats::cautious();
    let baseline = stats;
    entry.consume_transition(&mut stats);
    assert_eq!(stats.player_hp, baseline.player_hp - 2);
    assert_eq!(stats.boss_hp, baseline.boss_hp - 2);
    assert_eq!(entry.pending_phase_event, None);
}

#[test]
fn test_start_boss_duel_stores_pending_phase_event_on_transition() {
    let mut entry = BossProgressEntry {
        boss_id: "grothak".to_owned(),
        status: "active".to_owned(),
        pending_phase_event: None,
    };
    let mut rng = SequenceRng::new(vec![], vec![5]);
    assert_eq!(entry.enter_next_phase(&mut rng), "void_pull");
    assert_eq!(entry.pending_phase_event.as_deref(), Some("void_pull"));
}

#[test]
fn test_start_boss_duel_phase2_applies_transition_event() {
    let mut entry = progress(Some("void_pull"));
    let mut state_machine_stats = DuelStats::cautious();
    entry.consume_transition(&mut state_machine_stats);
    assert_eq!(state_machine_stats.player_hp, 3);
    assert_eq!(state_machine_stats.boss_hp, 2);
}

#[test]
fn test_consumed_phase_event_not_resurrected_on_auto_resolve_loss() {
    let mut resolved = progress(Some("void_pull"));
    let mut stats = DuelStats::cautious();
    resolved.consume_transition(&mut stats);
    let mut fresh = progress(Some("void_pull"));
    merge_fresh_progress_preserving_consumption(&mut fresh, &resolved);
    assert_eq!(fresh.pending_phase_event, None);
}

#[test]
fn test_start_boss_duel_consumes_event_even_when_fight_pauses() {
    let mut entry = progress(Some("void_pull"));
    let mut stats = DuelStats::cautious();
    let event = entry.consume_transition(&mut stats);
    let mut rng = SequenceRng::new(vec![], vec![0]);
    let paused = start_paused_duel(&GROTHAK_MECHANICS, None, 0.0, 0, vec![], &mut rng);
    assert_eq!(event.map(|value| value.id), Some("void_pull"));
    assert_eq!(entry.pending_phase_event, None);
    assert_eq!(paused.mechanic_id, "grothak_earthquake");
}

#[test]
fn test_amulet_crit_persisted_in_paused_duel() {
    let mut rng = SequenceRng::new(vec![], vec![0]);
    let paused = start_paused_duel(&GROTHAK_MECHANICS, None, 0.10, 1, vec![4], &mut rng);
    let resumed = resume_paused_duel(&paused);
    assert_close(resumed.critical_chance, 0.10);
    assert_eq!(resumed.critical_bonus, 1);
    assert_eq!(resumed.gear_snapshot_ids, [4]);
}

#[test]
fn test_paused_duel_without_amulet_persists_zero_crit() {
    let mut rng = SequenceRng::new(vec![], vec![0]);
    let paused = start_paused_duel(&GROTHAK_MECHANICS, None, 0.0, 0, vec![], &mut rng);
    assert_close(paused.critical_chance, 0.0);
    assert_eq!(resume_paused_duel(&paused).critical_bonus, 0);
}

#[test]
fn test_wager_payout_tapers_at_high_win_chance() {
    let result = settle_victory(VictoryInput {
        wager: 100,
        win_chance: 0.95,
        ..victory(0)
    });
    assert_eq!(result.wager_profit, 5);
    assert_eq!(result.credited, scale_positive_dig_jc(15) + 5);
}

#[test]
fn test_won_wager_at_high_win_chance_never_loses_money() {
    let result = settle_victory(VictoryInput {
        wager: 100,
        win_chance: 0.95,
        echo_applied: true,
        ..victory(0)
    });
    assert!(result.credited >= 0);
    assert_eq!(result.audit_jc_delta, result.credited);
}

#[test]
fn test_first_clear_stat_point_commits_with_boss_clear_and_is_idempotent() {
    let mut service = BossDuelSettlementService::new(repository(300, 24));
    let payout = settle_victory(victory(20));
    let first = service
        .settle_victory_atomic(1, 25, payout)
        .expect("first clear commits");
    assert!(first.defeated_boundaries.contains(&25));
    assert_eq!(first.stat_points, 6);
    let second = service
        .settle_victory_atomic(1, 25, payout)
        .expect("retry remains valid");
    assert_eq!(second.stat_points, 6);
}

#[test]
fn test_loss_debits_wager_atomically() {
    let mut service = BossDuelSettlementService::new(repository(500, 24));
    let state = service
        .settle_loss_atomic(
            1,
            LossInput {
                balance: 0,
                depth: 0,
                wager: 30,
                forced_no_wager_phase: false,
                knockback_roll: 11,
                resolved_at: 1_000_000,
            },
        )
        .expect("loss commits");
    assert_eq!(state.balance, 470);
    assert!(state.depth < 24);
    assert_eq!(service.repository().audits()[0].jc_delta, -30);
}

#[test]
fn test_loss_with_no_wager_charges_repair_bill() {
    let result = settle_loss(LossInput {
        balance: 200,
        depth: 24,
        wager: 0,
        forced_no_wager_phase: false,
        knockback_roll: 11,
        resolved_at: 0,
    });
    assert_eq!(result.balance_after, 200 - BOSS_LOSS_REPAIR_BILL);
    assert_eq!(result.jc_delta, -BOSS_LOSS_REPAIR_BILL);
}

#[test]
fn test_forced_no_wager_phase_loss_does_not_charge_repair_bill() {
    let result = settle_loss(LossInput {
        balance: 200,
        depth: 24,
        wager: 0,
        forced_no_wager_phase: true,
        knockback_roll: 11,
        resolved_at: 0,
    });
    assert_eq!(result.balance_after, 200);
    assert_eq!(result.jc_delta, 0);
}

#[test]
fn test_loss_after_spending_during_pause_floors_at_zero() {
    let result = settle_loss(LossInput {
        balance: 30,
        depth: 24,
        wager: 100,
        forced_no_wager_phase: false,
        knockback_roll: 11,
        resolved_at: 0,
    });
    assert_eq!(result.balance_after, 0);
    assert_eq!(result.jc_delta, -30);
}

#[test]
fn test_loss_with_sufficient_balance_debits_full_wager() {
    let result = settle_loss(LossInput {
        balance: 500,
        depth: 24,
        wager: 100,
        forced_no_wager_phase: false,
        knockback_roll: 11,
        resolved_at: 0,
    });
    assert_eq!(result.balance_after, 400);
    assert_eq!(result.jc_delta, -100);
}

#[test]
fn test_in_debt_loss_is_not_credited() {
    let result = settle_loss(LossInput {
        balance: -100,
        depth: 24,
        wager: 0,
        forced_no_wager_phase: false,
        knockback_roll: 11,
        resolved_at: 0,
    });
    assert_eq!(result.balance_after, -100);
    assert_eq!(result.jc_delta, 0);
}
