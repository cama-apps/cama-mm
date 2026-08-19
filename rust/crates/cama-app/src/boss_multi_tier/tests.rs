use super::*;

type TestService = BossMultiTierService<
    InMemoryBossRepository,
    SequenceEntropy,
    FixedClock,
    NoEconomyEvent,
    NoBankruptcy,
    InMemoryGearRepair,
    InMemoryBossOutcomeSink,
>;

const PLAYER_ID: i64 = 10_001;
const GUILD_ID: i64 = 777;
const NOW: i64 = 1_000_000;

#[test]
fn bundled_mechanic_catalog_preserves_authored_ogre_fireblast() {
    assert_eq!(MECHANIC_CATALOG.len(), 135);
    let mechanic = mechanic_by_id("ogre_fireblast").expect("Ogre mechanic is bundled");
    assert_eq!(mechanic.trigger_round, 3);
    assert_eq!(
        mechanic.prompt_title,
        "The Twin-Skulled chants a slow fire blast"
    );
    assert_eq!(
        mechanic.prompt_description,
        "Left head counts down. Right head forgot the number."
    );
    assert_eq!(
        mechanic
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        [
            "Slap the left head",
            "Confuse both heads",
            "Stand in front and grin"
        ]
    );
    assert_eq!(mechanic.options[2].outcome_rolls[0].boss_hp_delta, -3);
    assert_eq!(
        mechanic.options[2].outcome_rolls[1].status_effect,
        Some(MechanicStatusEffect::Burn)
    );
}

fn key() -> PlayerKey {
    PlayerKey::new(PLAYER_ID, Some(GUILD_ID))
}

fn test_service(unit: f64) -> TestService {
    BossMultiTierService::new(
        InMemoryBossRepository::default(),
        SequenceEntropy::constant(unit),
        FixedClock { unix_seconds: NOW },
        NoEconomyEvent,
        NoBankruptcy,
        InMemoryGearRepair::default(),
        InMemoryBossOutcomeSink::default(),
    )
}

#[derive(Clone, Copy, Debug)]
struct OneHpCombatPreparation;

impl BossCombatPreparationPort for OneHpCombatPreparation {
    fn prepare(
        &self,
        _request: BossCombatPreparationRequest<'_>,
        mut base_combat: CombatState,
    ) -> BossCombatPreparation {
        base_combat.player_hp = 1;
        BossCombatPreparation::neutral(base_combat)
    }
}

fn entry(status: BossStatus, boss_id: &str) -> BossProgressValue {
    BossProgressValue::Entry(BossProgressEntry {
        status,
        boss_id: Some(boss_id.to_owned()),
        ..BossProgressEntry::default()
    })
}

fn wounded_entry(
    status: BossStatus,
    boss_id: &str,
    hp_remaining: i32,
    hp_max: i32,
) -> BossProgressValue {
    BossProgressValue::Entry(BossProgressEntry {
        status,
        boss_id: Some(boss_id.to_owned()),
        hp_remaining: Some(hp_remaining),
        hp_max: Some(hp_max),
        last_engaged_at: Some(NOW),
        ..BossProgressEntry::default()
    })
}

fn tunnel(depth: i32, prestige_level: i32, balance: i64) -> TunnelState {
    TunnelState {
        depth,
        max_depth: depth,
        prestige_level,
        balance,
        ..TunnelState::default()
    }
}

fn seed_at_first_boss(service: &mut TestService) {
    service.repository.insert_tunnel(key(), tunnel(24, 0, 500));
}

fn set_progress(service: &mut TestService, progress: BossProgress, depth: i32) {
    let mut current = service
        .repository
        .load_tunnel(key())
        .expect("test tunnel exists");
    current.boss_progress = progress;
    current.depth = depth;
    current.max_depth = current.max_depth.max(depth);
    service.repository.insert_tunnel(key(), current);
}

fn add_default_gear(service: &mut TestService) -> (i64, i64, i64) {
    let weapon = service.gear_repair.add_piece(key(), GearSlot::Weapon, 20);
    let armor = service.gear_repair.add_piece(key(), GearSlot::Armor, 20);
    let boots = service.gear_repair.add_piece(key(), GearSlot::Boots, 20);
    service
        .gear_repair
        .equip(key(), weapon)
        .expect("weapon exists");
    service
        .gear_repair
        .equip(key(), armor)
        .expect("armor exists");
    service
        .gear_repair
        .equip(key(), boots)
        .expect("boots exist");
    (weapon, armor, boots)
}

fn seed_paused(
    service: &mut TestService,
    boss_id: &str,
    mechanic_id: &str,
    combat: CombatState,
    boss_hp_max: i32,
    snapshot: GearSnapshot,
) {
    let mechanic = mechanic_by_id(mechanic_id).expect("test mechanic is registered");
    let player_hp_max = combat.player_hp.max(1);
    let duel = PausedBossDuel {
        boss_id: boss_id.to_owned(),
        boundary: 25,
        mechanic_id: mechanic_id.to_owned(),
        risk_tier: RiskTier::Cautious,
        wager: 10,
        combat,
        round_num: 3,
        round_log: Vec::new(),
        pending_prompt: PendingPrompt::from(&mechanic),
        attempts_this_fight: 1,
        initial_win_chance: 0.5,
        payout_multiplier: 1.5,
        player_hp_max,
        boss_hp_max,
        starting_boss_hp: boss_hp_max,
        gear_snapshot: snapshot,
        forced_no_wager_phase: false,
        echo_applied: false,
        echo_killer_id: None,
    };
    service
        .repository
        .save_active_duel(key(), duel)
        .expect("test duel saves");
}

fn combat(player_hp: i32, boss_hp: i32, player_hit: f64, boss_hit: f64) -> CombatState {
    CombatState {
        player_hp,
        boss_hp,
        player_hit,
        player_damage: 1,
        boss_hit,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects: CombatEffects::default(),
    }
}

fn winning_override() -> CombatState {
    CombatState {
        player_hp: 100,
        boss_hp: 1,
        player_hit: 1.0,
        player_damage: 100,
        boss_hit: 1.0,
        boss_damage: 0,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects: CombatEffects::default(),
    }
}

fn losing_override() -> CombatState {
    CombatState {
        player_hp: 1,
        boss_hp: 100,
        player_hit: 0.0,
        player_damage: 100,
        boss_hit: 1.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects: CombatEffects::default(),
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1.0e-9,
        "expected {actual} to equal {expected}"
    );
}

#[test]
fn test_pool_size_per_tier() {
    let expected = [
        (25, 3),
        (50, 3),
        (75, 3),
        (100, 3),
        (150, 7),
        (200, 7),
        (275, 7),
    ];
    for (tier, size) in expected {
        assert_eq!(boss_pool_for_tier(tier, 99).len(), size);
    }
}

#[test]
fn test_grandfathered_first() {
    let expected = [
        (25, "grothak"),
        (50, "crystalia"),
        (75, "magmus_rex"),
        (100, "void_warden"),
        (150, "sporeling_sovereign"),
        (200, "chronofrost"),
        (275, "nameless_depth"),
    ];
    for (tier, boss_id) in expected {
        assert_eq!(boss_pool_for_tier(tier, 99)[0].boss_id, boss_id);
    }
}

#[test]
fn test_every_boss_has_mechanic_and_stinger() {
    for boss in BOSSES {
        for mechanic_id in boss.mechanic_pool {
            assert!(mechanic_by_id(mechanic_id).is_some(), "{mechanic_id}");
        }
        assert!(
            stinger_by_id(boss.stinger_id).is_some(),
            "{}",
            boss.stinger_id
        );
    }
}

#[test]
fn test_boss_ids_unique() {
    let ids: BTreeSet<&str> = BOSSES.iter().map(|boss| boss.boss_id).collect();
    assert_eq!(ids.len(), BOSSES.len());
    assert_eq!(ids.len(), 33);
}

#[test]
fn test_next_boundary_is_unfinished_pinnacle_after_regular_clears() {
    let progress = BOSS_BOUNDARIES
        .into_iter()
        .map(|boundary| {
            (
                boundary.to_string(),
                BossProgressValue::Legacy(BossStatus::Defeated),
            )
        })
        .collect();
    assert_eq!(next_boss_boundary(&progress), Some(PINNACLE_DEPTH));
}

#[test]
fn test_three_options_each() {
    for boss in BOSSES {
        for mechanic_id in boss.mechanic_pool {
            let mechanic = mechanic_by_id(mechanic_id).expect("registered mechanic");
            assert_eq!(mechanic.options.len(), 3, "{mechanic_id}");
        }
    }
}

#[test]
fn test_probabilities_sum_to_one() {
    for boss in BOSSES {
        for mechanic_id in boss.mechanic_pool {
            let mechanic = mechanic_by_id(mechanic_id).expect("registered mechanic");
            for option in mechanic.options {
                let total: f64 = option
                    .outcome_rolls
                    .iter()
                    .map(|roll| roll.probability)
                    .sum();
                assert_close(total, 1.0);
            }
        }
    }
}

#[test]
fn test_safe_option_idx_in_range() {
    for boss in BOSSES {
        for mechanic_id in boss.mechanic_pool {
            let mechanic = mechanic_by_id(mechanic_id).expect("registered mechanic");
            assert!(mechanic.safe_option_index < 3, "{mechanic_id}");
        }
    }
}

#[test]
fn test_ensure_locks_on_first_call() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let first = service
        .ensure_boss_locked(key(), 25)
        .expect("first lock succeeds")
        .boss_id;
    let stored = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    assert_eq!(locked_boss_id(&stored.boss_progress, 25, 0), Some(first));
    let second = service
        .ensure_boss_locked(key(), 25)
        .expect("second lock succeeds")
        .boss_id;
    assert_eq!(first, second);
}

#[test]
fn test_rolls_when_locked_boss_id_is_empty() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, ""));
    set_progress(&mut service, progress, 24);
    let boss = service
        .ensure_boss_locked(key(), 25)
        .expect("empty lock is replaced");
    assert_eq!(boss.boss_id, "grothak");
    let stored = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    assert_eq!(
        locked_boss_id(&stored.boss_progress, 25, 0),
        Some("grothak")
    );
}

#[test]
fn test_fight_boss_locks_boss_id_when_empty() {
    let mut service = test_service(0.01);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, ""));
    set_progress(&mut service, progress, 24);
    let result = service
        .fight_boss_with_override(key(), RiskTier::Cautious, 10, Some(winning_override()))
        .expect("fight resolves");
    assert!(result.won);
    assert_eq!(result.boss_id, "grothak");
    assert!(
        service
            .repository
            .active_echo(key().guild_id, "grothak", NOW)
            .is_some()
    );
}

#[test]
fn test_distribution_across_many_tunnels() {
    let mut repository = InMemoryBossRepository::default();
    for player_id in 0..3 {
        repository.insert_tunnel(
            PlayerKey::new(player_id, Some(GUILD_ID)),
            tunnel(24, 0, 500),
        );
    }
    let entropy = SequenceEntropy::new(Vec::new(), vec![0, 1, 2], Vec::new());
    let mut service = BossMultiTierService::new(
        repository,
        entropy,
        FixedClock { unix_seconds: NOW },
        NoEconomyEvent,
        NoBankruptcy,
        InMemoryGearRepair::default(),
        InMemoryBossOutcomeSink::default(),
    );
    let mut seen = BTreeSet::new();
    for player_id in 0..3 {
        let selected = service
            .ensure_boss_locked(PlayerKey::new(player_id, Some(GUILD_ID)), 25)
            .expect("lock succeeds");
        seen.insert(selected.boss_id);
    }
    assert_eq!(seen, BTreeSet::from(["grothak", "ogre_magi", "pudge"]));
}

#[test]
fn test_reads_legacy_string_format() {
    let mut progress = BossProgress::new();
    progress.insert(
        "25".to_owned(),
        BossProgressValue::Legacy(BossStatus::Defeated),
    );
    progress.insert(
        "50".to_owned(),
        BossProgressValue::Legacy(BossStatus::Active),
    );
    let normalized = progress_statuses(&progress);
    assert_eq!(normalized[&25], BossStatus::Defeated);
    assert_eq!(normalized[&50], BossStatus::Active);
}

#[test]
fn test_reads_new_dict_format() {
    let mut progress = BossProgress::new();
    progress.insert("25".to_owned(), entry(BossStatus::Defeated, "pudge"));
    progress.insert("50".to_owned(), entry(BossStatus::Active, "crystalia"));
    let normalized = progress_statuses(&progress);
    assert_eq!(normalized[&25], BossStatus::Defeated);
    assert_eq!(normalized[&50], BossStatus::Active);
}

#[test]
fn test_get_locked_boss_id_legacy_fallback() {
    let progress = BTreeMap::from([(
        "25".to_owned(),
        BossProgressValue::Legacy(BossStatus::Active),
    )]);
    assert_eq!(locked_boss_id(&progress, 25, 0), Some("grothak"));
}

#[test]
fn test_get_locked_boss_id_from_new_format() {
    let progress = BTreeMap::from([("25".to_owned(), entry(BossStatus::Active, "pudge"))]);
    assert_eq!(locked_boss_id(&progress, 25, 0), Some("pudge"));
}

#[test]
fn test_resume_clears_state_row() {
    let mut service = test_service(0.0);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, "grothak"));
    set_progress(&mut service, progress, 24);
    seed_paused(
        &mut service,
        "grothak",
        "grothak_earthquake",
        combat(3, 3, 0.60, 0.30),
        3,
        GearSnapshot::default(),
    );
    let result = service.resume_boss_duel(key(), 0).expect("resume resolves");
    let mechanic = result
        .round_log
        .iter()
        .find_map(|round| round.mechanic.as_ref())
        .expect("selected mechanic is recorded");
    assert_eq!(mechanic.mechanic_id, "grothak_earthquake");
    assert_eq!(mechanic.option_label, "Brace against the wall");
    assert!(!mechanic.narrative.is_empty());
    assert!(service.repository.active_duel(key()).is_none());
}

#[test]
fn test_voluntary_free_duel_uses_seventy_percent_accuracy() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 0)
        .expect("duel starts");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected a prompt");
    };
    assert_close(paused.combat.player_hit, 0.42);
}

#[test]
fn test_forced_no_wager_phase_keeps_its_full_accuracy() {
    let mut service = test_service(0.99);
    service.repository.insert_tunnel(key(), tunnel(24, 2, 500));
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, "grothak"));
    set_progress(&mut service, progress, 24);
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 0)
        .expect("forced free phase starts");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected a prompt");
    };
    assert!(paused.forced_no_wager_phase);
    assert_close(paused.combat.player_hit, 0.55);
}

#[test]
fn test_paused_duel_snapshots_equipped_ring() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let ring = service.gear_repair.add_piece(key(), GearSlot::Ring, 20);
    service.gear_repair.equip(key(), ring).expect("ring exists");
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 0)
        .expect("duel starts");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected a prompt");
    };
    assert!(paused.gear_snapshot.gear_ids.contains(&ring));
}

#[test]
fn test_resume_loss_ticks_snapshot_armor_twice_after_a_gear_swap() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let (weapon, armor, boots) = add_default_gear(&mut service);
    let replacement = service.gear_repair.add_piece(key(), GearSlot::Armor, 20);
    let snapshot = service.gear_repair.snapshot(key());
    seed_paused(
        &mut service,
        "grothak",
        "grothak_earthquake",
        combat(1, 3, 0.0, 1.0),
        3,
        snapshot,
    );
    service
        .gear_repair
        .equip(key(), replacement)
        .expect("replacement exists");
    let result = service.resume_boss_duel(key(), 0).expect("resume resolves");
    assert!(!result.won);
    assert_eq!(
        service
            .gear_repair
            .piece(key(), armor)
            .expect("armor")
            .durability,
        18
    );
    assert_eq!(
        service
            .gear_repair
            .piece(key(), weapon)
            .expect("weapon")
            .durability,
        19
    );
    assert_eq!(
        service
            .gear_repair
            .piece(key(), boots)
            .expect("boots")
            .durability,
        19
    );
    assert_eq!(
        service
            .gear_repair
            .piece(key(), replacement)
            .expect("replacement")
            .durability,
        20
    );
}

#[test]
fn test_resume_claims_duel_atomically() {
    let mut service = test_service(0.0);
    seed_at_first_boss(&mut service);
    seed_paused(
        &mut service,
        "grothak",
        "grothak_earthquake",
        combat(3, 3, 0.60, 0.30),
        3,
        GearSnapshot::default(),
    );
    service
        .resume_boss_duel(key(), 0)
        .expect("first resume claims and resolves");
    assert_eq!(
        service.resume_boss_duel(key(), 0),
        Err(BossServiceError::NoActiveDuel)
    );
}

#[test]
fn test_resume_without_state_errors() {
    let mut service = test_service(0.0);
    seed_at_first_boss(&mut service);
    assert_eq!(
        service.resume_boss_duel(key(), 0),
        Err(BossServiceError::NoActiveDuel)
    );
}

#[test]
fn test_start_validation_errors() {
    let mut service = test_service(0.99);
    assert_eq!(
        service.start_boss_duel(key(), RiskTier::Cautious, 0),
        Err(BossServiceError::MissingTunnel)
    );
    service.repository.insert_tunnel(key(), tunnel(0, 0, 500));
    assert_eq!(
        service.start_boss_duel(key(), RiskTier::Cautious, 0),
        Err(BossServiceError::NotAtBossBoundary)
    );
}

#[test]
fn test_pending_prompt_shape() {
    let mut service = test_service(0.5);
    seed_at_first_boss(&mut service);
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 10)
        .expect("duel starts");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected a prompt");
    };
    assert!(!paused.pending_prompt.mechanic_id.is_empty());
    assert!(!paused.pending_prompt.prompt_title.is_empty());
    assert_eq!(paused.pending_prompt.options.len(), 3);
    for (index, option) in paused.pending_prompt.options.iter().enumerate() {
        assert_eq!(option.option_index, index);
        assert!(!option.label.is_empty());
    }
    assert_eq!(paused.round_num, 2);
    assert_eq!(paused.round_log.len(), 1);
    assert!(service.repository.active_duel(key()).is_some());
}

#[test]
fn interactive_round_one_loss_resolves_before_prompt() {
    let mut service = test_service(0.99).with_combat_preparation(OneHpCombatPreparation);
    service.entropy = SequenceEntropy::new(vec![0.99, 0.0], Vec::new(), Vec::new());
    seed_at_first_boss(&mut service);

    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 0)
        .expect("opening loss resolves");
    let StartBossDuelOutcome::Resolved(result) = outcome else {
        panic!("a lethal opening exchange must resolve before prompting");
    };
    assert!(!result.won);
    assert_eq!(result.round_log.len(), 1);
    assert!(service.repository.active_duel(key()).is_none());
}

fn assert_regular_mechanic_round_is_normalized(authored_round: u8) {
    let mut mechanic = mechanic_by_id("grothak_earthquake").expect("registered mechanic");
    mechanic.trigger_round = authored_round;
    assert_eq!(regular_mechanic_trigger_round(&mechanic), 2);

    let mut service = test_service(0.5);
    seed_at_first_boss(&mut service);
    let StartBossDuelOutcome::Paused(paused) = service
        .start_boss_duel(key(), RiskTier::Cautious, 0)
        .expect("regular duel pauses")
    else {
        panic!("viable regular fight must pause");
    };
    assert_eq!(paused.round_num, 2);
    assert_eq!(paused.round_log.len(), 1);
}

#[test]
fn mechanic_authored_round_one_interrupts_before_second_round() {
    assert_regular_mechanic_round_is_normalized(1);
}

#[test]
fn mechanic_authored_round_five_interrupts_before_second_round() {
    assert_regular_mechanic_round_is_normalized(5);
}

#[test]
fn test_start_boss_duel_reads_persisted_hp() {
    let mut service = test_service(0.5);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert(
        "25".to_owned(),
        wounded_entry(BossStatus::Active, "grothak", 1, 12),
    );
    set_progress(&mut service, progress, 24);
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 10)
        .expect("duel resolves");
    let StartBossDuelOutcome::Resolved(result) = outcome else {
        panic!("wounded boss should die before the prompt");
    };
    assert!(result.won);
}

#[test]
fn test_resumed_duel_preserves_active_boss_max_hp() {
    let mut service = test_service(0.5);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert(
        "25".to_owned(),
        wounded_entry(BossStatus::Active, "grothak", 10, 20),
    );
    set_progress(&mut service, progress, 24);
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 10)
        .expect("duel pauses");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected a prompt");
    };
    assert_eq!(paused.combat.boss_hp, 9);
    assert_eq!(paused.boss_hp_max, 20);
    service.entropy = SequenceEntropy::constant(0.99);
    let result = service
        .resume_boss_duel(key(), paused.pending_prompt.safe_option_index)
        .expect("resume resolves");
    assert!(!result.won);
    let fresh = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    let BossProgressValue::Entry(saved) = &fresh.boss_progress["25"] else {
        panic!("progress remains detailed");
    };
    assert_eq!(saved.hp_remaining, Some(9));
    assert_eq!(saved.hp_max, Some(20));
}

#[test]
fn test_scout_uses_active_boss_hp_for_every_risk_tier() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert(
        "25".to_owned(),
        wounded_entry(BossStatus::Active, "grothak", 7, 12),
    );
    set_progress(&mut service, progress, 24);
    let mut current = service
        .repository
        .load_tunnel(key())
        .expect("tunnel exists");
    current.lanterns = 1;
    service.repository.insert_tunnel(key(), current);
    let result = service.scout_boss(key()).expect("scout succeeds");
    assert_eq!(result.odds.for_risk(RiskTier::Cautious).boss_hp, 7);
    assert_eq!(result.odds.for_risk(RiskTier::Bold).boss_hp, 7);
    assert_eq!(result.odds.for_risk(RiskTier::Reckless).boss_hp, 7);
}

#[test]
fn test_loss_write_preserves_other_boundaries() {
    let mut service = test_service(0.99);
    service.repository.insert_tunnel(key(), tunnel(49, 0, 500));
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Defeated, "grothak"));
    progress.insert("50".to_owned(), entry(BossStatus::Active, "crystalia"));
    progress.insert(
        "100".to_owned(),
        wounded_entry(BossStatus::Active, "void_warden", 2, 10),
    );
    set_progress(&mut service, progress, 49);
    let result = service
        .fight_boss(key(), RiskTier::Cautious, 10)
        .expect("fight resolves");
    assert!(!result.won);
    let fresh = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    let BossProgressValue::Entry(saved) = &fresh.boss_progress["100"] else {
        panic!("unrelated entry remains detailed");
    };
    assert_eq!(saved.hp_remaining, Some(2));
    assert_eq!(saved.hp_max, Some(10));
}

#[test]
fn test_retreat_preserves_other_boundaries() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, "grothak"));
    progress.insert(
        "50".to_owned(),
        wounded_entry(BossStatus::Active, "crystalia", 3, 14),
    );
    set_progress(&mut service, progress, 24);
    service.retreat_boss(key()).expect("retreat succeeds");
    let fresh = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    let BossProgressValue::Entry(saved) = &fresh.boss_progress["50"] else {
        panic!("unrelated entry remains detailed");
    };
    assert_eq!(saved.hp_remaining, Some(3));
    assert_eq!(saved.hp_max, Some(14));
}

#[test]
fn test_applies_player_hp_delta() {
    let mechanic = mechanic_by_id("pudge_hook").expect("pudge mechanic");
    let mut entropy = SequenceEntropy::constant(0.0);
    let outcome = apply_option_outcome(
        &mechanic.options[0],
        5,
        5,
        &CombatEffects::default(),
        &mut entropy,
    );
    assert_eq!(outcome.player_hp, 5);
    assert_eq!(outcome.boss_hp, 5);
}

#[test]
fn test_applies_boss_hp_delta() {
    let mechanic = mechanic_by_id("pudge_hook").expect("pudge mechanic");
    let mut entropy = SequenceEntropy::constant(0.0);
    let outcome = apply_option_outcome(
        &mechanic.options[2],
        5,
        5,
        &CombatEffects::default(),
        &mut entropy,
    );
    assert_eq!(outcome.player_hp, 5);
    assert_eq!(outcome.boss_hp, 1);
}

#[test]
fn test_applies_skip_next_round() {
    let mechanic = mechanic_by_id("pudge_hook").expect("pudge mechanic");
    let mut entropy = SequenceEntropy::constant(0.99);
    let outcome = apply_option_outcome(
        &mechanic.options[1],
        5,
        5,
        &CombatEffects::default(),
        &mut entropy,
    );
    assert_eq!(outcome.effects.skip_next_round_for, Some(Combatant::Player));
}

#[test]
fn test_applies_status_effect() {
    let mechanic = mechanic_by_id("cm_frostbite").expect("frostbite mechanic");
    let mut entropy = SequenceEntropy::constant(0.99);
    let outcome = apply_option_outcome(
        &mechanic.options[1],
        5,
        5,
        &CombatEffects::default(),
        &mut entropy,
    );
    assert!(outcome.effects.frostbite_next_round);
}

fn combat_for_effect_test(effects: CombatEffects) -> CombatState {
    CombatState {
        player_hp: 5,
        boss_hp: 10,
        player_hit: 0.0,
        player_damage: 1,
        boss_hit: 0.0,
        boss_damage: 1,
        critical_chance: 0.0,
        critical_bonus: 0,
        effects,
    }
}

#[test]
fn test_python_trophy_relic_round_effects() {
    let mut venom = combat_for_effect_test(CombatEffects {
        trophy_venom_rounds: 4,
        ..CombatEffects::default()
    });
    let (record, terminal) = run_round(2, &mut venom, &mut SequenceEntropy::constant(0.99));
    assert_eq!(terminal, None);
    assert_eq!(venom.boss_hp, 9);
    assert!(record.effect_log.venom);

    let mut forewarned = combat_for_effect_test(CombatEffects {
        trophy_forewarned: true,
        ..CombatEffects::default()
    });
    forewarned.boss_hit = 1.0;
    let (record, _) = run_round(1, &mut forewarned, &mut SequenceEntropy::constant(0.0));
    assert_eq!(forewarned.player_hp, 5);
    assert!(record.effect_log.forewarned);
    let (record, _) = run_round(2, &mut forewarned, &mut SequenceEntropy::constant(0.0));
    assert_eq!(forewarned.player_hp, 4);
    assert!(!record.effect_log.forewarned);

    let mut lifesteal = combat_for_effect_test(CombatEffects {
        trophy_start_hp: Some(3),
        trophy_lifesteal: true,
        ..CombatEffects::default()
    });
    lifesteal.player_hp = 2;
    lifesteal.player_hit = 1.0;
    let (record, _) = run_round(1, &mut lifesteal, &mut SequenceEntropy::constant(0.0));
    assert_eq!(lifesteal.player_hp, 3);
    assert!(record.effect_log.lifesteal);
    assert!(!lifesteal.effects.trophy_lifesteal);

    let mut regrowth = combat_for_effect_test(CombatEffects {
        trophy_start_hp: Some(5),
        trophy_regrowth: true,
        ..CombatEffects::default()
    });
    regrowth.player_hp = 3;
    let (record, _) = run_round(2, &mut regrowth, &mut SequenceEntropy::constant(0.99));
    assert_eq!(regrowth.player_hp, 4);
    assert!(record.effect_log.regrowth);

    let mut laststand = combat_for_effect_test(CombatEffects {
        trophy_laststand: true,
        ..CombatEffects::default()
    });
    laststand.player_hp = 1;
    laststand.player_hit = 1.0;
    let (record, _) = run_round(2, &mut laststand, &mut SequenceEntropy::constant(0.0));
    assert_eq!(laststand.boss_hp, 8);
    assert!(record.effect_log.laststand);
}

#[test]
fn test_berserk_gambler_bottled_quake_and_deaths_door_match_python() {
    let mut combat = combat_for_effect_test(CombatEffects {
        relic_berserk_rage: Some(0),
        relic_double_hit: true,
        relic_bottled_quake: true,
        ..CombatEffects::default()
    });
    combat.player_hit = 1.0;
    combat.boss_hit = 1.0;
    combat.player_hp = 20;
    combat.boss_hp = 100;
    let (first, _) = run_round(1, &mut combat, &mut SequenceEntropy::constant(0.0));
    assert_eq!(combat.boss_hp, 96); // (1 base + 1 quake) doubled.
    assert!(first.effect_log.bottled_quake);
    assert!(first.effect_log.double_hit);
    assert_eq!(combat.effects.relic_berserk_rage, Some(1));
    combat.effects.relic_double_hit = false;
    let (second, _) = run_round(2, &mut combat, &mut SequenceEntropy::constant(0.0));
    assert_eq!(combat.boss_hp, 94); // 1 base + 1 rage.
    assert_eq!(second.effect_log.berserk_bonus, 1);
    let (third, _) = run_round(3, &mut combat, &mut SequenceEntropy::constant(0.0));
    assert_eq!(combat.boss_hp, 91); // rage caps at +2.
    assert_eq!(third.effect_log.berserk_bonus, 2);

    let mut saved = combat_for_effect_test(CombatEffects {
        bleed_rounds_remaining: 1,
        relic_deaths_door: true,
        ..CombatEffects::default()
    });
    saved.player_hp = 1;
    let (record, terminal) = run_round(
        1,
        &mut saved,
        &mut SequenceEntropy::new(vec![0.0, 0.99, 0.99], vec![], vec![]),
    );
    assert_eq!(terminal, None);
    assert_eq!(saved.player_hp, 1);
    assert!(record.effect_log.deaths_door);
    assert!(!saved.effects.relic_deaths_door);
}

#[test]
fn test_mechanic_defenses_have_python_precedence_and_are_one_shot() {
    let option = MechanicOption {
        label: "Take it".to_owned(),
        flavor: "Test".to_owned(),
        outcome_rolls: vec![OutcomeRoll {
            probability: 1.0,
            player_hp_delta: 0,
            boss_hp_delta: 0,
            skip_next_round_for: Some(Combatant::Player),
            status_effect: Some(MechanicStatusEffect::Burn),
            narrative: "Burned.".to_owned(),
        }],
    };
    let effects = CombatEffects {
        relic_paper_crane: true,
        gear_block_first_status: true,
        gear_block_first_skip: true,
        ..CombatEffects::default()
    };
    let first = apply_option_outcome(&option, 5, 5, &effects, &mut SequenceEntropy::constant(0.0));
    assert_eq!(
        first.effects.paper_crane_blocked,
        Some(MechanicStatusEffect::Burn)
    );
    assert!(first.effects.gear_block_first_status);
    assert!(first.effects.anchor_boots_blocked);
    assert_eq!(first.effects.burn_rounds_remaining, 0);
    assert_eq!(first.effects.skip_next_round_for, None);
    let second = apply_option_outcome(
        &option,
        5,
        5,
        &first.effects,
        &mut SequenceEntropy::constant(0.0),
    );
    assert_eq!(
        second.effects.nullweave_blocked,
        Some(MechanicStatusEffect::Burn)
    );
    assert_eq!(second.effects.burn_rounds_remaining, 0);
}

#[test]
fn test_unique_gear_round_effects_and_red_thread_consumption() {
    let mut combat = combat_for_effect_test(CombatEffects {
        trophy_start_hp: Some(5),
        gear_reflect_first_hit: true,
        gear_springheel_counter: true,
        gear_heal_first_crit: true,
        gear_reroll_first_miss: true,
        ..CombatEffects::default()
    });
    combat.player_hp = 4;
    combat.player_hit = 0.5;
    combat.boss_hit = 1.0;
    combat.critical_chance = 1.0;
    combat.critical_bonus = 1;
    let mut entropy = SequenceEntropy::new(vec![0.9, 0.2, 0.0, 0.0], vec![], vec![]);
    let (record, _) = run_round(1, &mut combat, &mut entropy);
    assert!(record.player_hit);
    assert!(record.effect_log.red_thread_reroll);
    assert!(record.effect_log.blood_locket_heal);
    assert!(record.effect_log.briarplate_reflect);
    assert_eq!(combat.player_hp, 4); // heal to 5, then the boss hits for 1.
    assert_eq!(combat.boss_hp, 7); // 2 crit damage plus 1 reflected.
    assert!(!combat.effects.gear_reroll_first_miss);
    assert!(!combat.effects.gear_heal_first_crit);
    assert!(!combat.effects.gear_reflect_first_hit);

    let mut counter = combat_for_effect_test(CombatEffects {
        gear_springheel_counter: true,
        ..CombatEffects::default()
    });
    let (record, _) = run_round(1, &mut counter, &mut SequenceEntropy::constant(0.99));
    assert_eq!(counter.boss_hp, 9);
    assert!(record.effect_log.springheel_counter);

    let mut silenced = combat_for_effect_test(CombatEffects {
        silenced_next_round: true,
        gear_reroll_first_miss: true,
        ..CombatEffects::default()
    });
    silenced.player_hit = 0.5;
    let (record, _) = run_round(1, &mut silenced, &mut SequenceEntropy::constant(0.9));
    assert!(!record.effect_log.red_thread_reroll);
    assert!(silenced.effects.gear_reroll_first_miss);
}

#[test]
fn test_halves_next_new_wager_and_consumes_curse() {
    let mut service = test_service(0.5);
    seed_at_first_boss(&mut service);
    let mut current = service
        .repository
        .load_tunnel(key())
        .expect("tunnel exists");
    current
        .stinger_curse
        .insert(CurseKind::HalveNextWager, "crystal_maiden");
    service.repository.insert_tunnel(key(), current);
    let outcome = service
        .start_boss_duel(key(), RiskTier::Cautious, 101)
        .expect("duel starts");
    let StartBossDuelOutcome::Paused(paused) = outcome else {
        panic!("expected a prompt");
    };
    assert_eq!(paused.wager, 51);
    let fresh = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    assert!(!fresh.stinger_curse.contains(CurseKind::HalveNextWager));
}

#[test]
fn test_halves_legacy_fight_wager_and_consumes_curse() {
    let mut service = test_service(0.999);
    seed_at_first_boss(&mut service);
    let mut current = service
        .repository
        .load_tunnel(key())
        .expect("tunnel exists");
    current
        .stinger_curse
        .insert(CurseKind::HalveNextWager, "crystal_maiden");
    service.repository.insert_tunnel(key(), current);
    let result = service
        .fight_boss(key(), RiskTier::Cautious, 101)
        .expect("fight resolves");
    assert!(!result.won);
    assert_eq!(result.jc_delta, -51);
    let fresh = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    assert_eq!(fresh.balance, 449);
    assert!(!fresh.stinger_curse.contains(CurseKind::HalveNextWager));
}

#[test]
fn test_no_scout_curse_blocks_once_without_consuming_lantern() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let mut current = service
        .repository
        .load_tunnel(key())
        .expect("tunnel exists");
    current.lanterns = 1;
    current
        .stinger_curse
        .insert(CurseKind::NoScoutNextDig, "spectre");
    service.repository.insert_tunnel(key(), current);
    assert_eq!(
        service.scout_boss(key()),
        Err(BossServiceError::ScoutCurseBlocked)
    );
    let after_block = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    assert_eq!(after_block.lanterns, 1);
    assert!(
        !after_block
            .stinger_curse
            .contains(CurseKind::NoScoutNextDig)
    );
    service.scout_boss(key()).expect("second scout succeeds");
    assert_eq!(
        service
            .repository
            .load_tunnel(key())
            .expect("tunnel remains")
            .lanterns,
        0
    );
}

#[test]
fn test_new_additive_knockback_flavor_describes_extra_distance() {
    for stinger_id in ["cairn_aftershock", "saltveil_pressgang"] {
        let flavor = stinger_by_id(stinger_id)
            .expect("stinger exists")
            .flavor_on_loss
            .to_lowercase();
        assert!(flavor.contains("farther"));
    }
}

#[test]
fn test_extra_knockback_widens_loss() {
    let mut service = test_service(0.99);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, "pudge"));
    set_progress(&mut service, progress, 24);
    seed_paused(
        &mut service,
        "pudge",
        "pudge_hook",
        combat(1, 4, 0.60, 0.30),
        4,
        GearSnapshot::default(),
    );
    let result = service.resume_boss_duel(key(), 2).expect("resume resolves");
    assert!(!result.won);
    assert_eq!(
        result.extra_knockback,
        stinger_by_id("pudge_drag")
            .expect("pudge stinger")
            .extra_knockback
    );
}

#[test]
fn test_cursed_status_written_on_loss() {
    let mut service = test_service(0.999);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, "ogre_magi"));
    set_progress(&mut service, progress, 24);
    let result = service
        .fight_boss_with_override(key(), RiskTier::Cautious, 10, Some(losing_override()))
        .expect("fight resolves");
    assert!(!result.won);
    assert_eq!(result.extra_cooldown_seconds, 900);
    let fresh = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    assert!(fresh.stinger_curse.active.is_empty());
}

#[test]
fn test_cursed_status_writes_when_configured() {
    let mut service = test_service(0.999);
    service.repository.insert_tunnel(key(), tunnel(49, 0, 500));
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Defeated, "grothak"));
    progress.insert("50".to_owned(), entry(BossStatus::Active, "crystal_maiden"));
    set_progress(&mut service, progress, 49);
    let result = service
        .fight_boss_with_override(key(), RiskTier::Cautious, 10, Some(losing_override()))
        .expect("fight resolves");
    assert!(!result.won);
    let fresh = service
        .repository
        .load_tunnel(key())
        .expect("tunnel remains");
    assert!(fresh.stinger_curse.contains(CurseKind::HalveNextWager));
}

#[test]
fn test_echo_written_under_specific_boss_id() {
    let mut service = test_service(0.0);
    seed_at_first_boss(&mut service);
    let mut progress = canonical_boss_progress();
    progress.insert("25".to_owned(), entry(BossStatus::Active, "pudge"));
    set_progress(&mut service, progress, 24);
    let result = service
        .fight_boss_with_override(key(), RiskTier::Cautious, 10, Some(winning_override()))
        .expect("fight resolves");
    assert!(result.won);
    assert!(
        service
            .repository
            .active_echo(key().guild_id, "pudge", NOW)
            .is_some()
    );
    assert!(
        service
            .repository
            .active_echo(key().guild_id, "grothak", NOW)
            .is_none()
    );
    assert!(
        service
            .repository
            .active_echo(key().guild_id, "ogre_magi", NOW)
            .is_none()
    );
}

#[test]
fn test_win_applies_economy_event_and_bankruptcy_debuff() {
    let mut repository = InMemoryBossRepository::default();
    repository.insert_tunnel(key(), tunnel(24, 0, 500));
    let mut service = BossMultiTierService::new(
        repository,
        SequenceEntropy::constant(0.01),
        FixedClock { unix_seconds: NOW },
        MultiplyingEconomyEvent {
            multiplier: 2,
            calls: Vec::new(),
        },
        HalvingBankruptcy::default(),
        InMemoryGearRepair::default(),
        InMemoryBossOutcomeSink::default(),
    );
    let result = service
        .fight_boss_with_override(key(), RiskTier::Cautious, 0, Some(winning_override()))
        .expect("fight resolves");
    assert!(result.won);
    let base = boss_base_reward(25, 0);
    assert_eq!(result.gross_payout, 2 * base);
    let full = scale_positive_dig_jc(2 * base);
    assert_eq!(result.jc_delta, full / 2);
    assert_eq!(result.bankruptcy_penalty, full - full / 2);
    assert_eq!(service.economy_event.calls, vec![(key().guild_id, base)]);
}

#[test]
fn test_free_fight_loss_repair_bill_floors_at_live_balance() {
    let mut service = test_service(0.5);
    service.repository.insert_tunnel(key(), tunnel(24, 0, 2));
    assert!(service.gear_repair.repair_bill() > 2);
    let result = service
        .fight_boss_with_override(key(), RiskTier::Cautious, 0, Some(losing_override()))
        .expect("fight resolves");
    assert!(!result.won);
    assert_eq!(result.jc_delta, -2);
    assert_eq!(
        service
            .repository
            .load_tunnel(key())
            .expect("tunnel remains")
            .balance,
        0
    );
}
