use std::collections::{BTreeSet, VecDeque};
use std::sync::Mutex;

use super::*;

const ACTOR: i64 = 111;

#[derive(Clone, Debug)]
struct FakeState {
    tunnel: Option<PrestigeTunnelSnapshot>,
    artifacts: Vec<ArtifactRow>,
    next_id: i64,
    duel: Option<ClaimedDuel>,
    victory_calls: usize,
}

#[derive(Debug)]
struct FakePort {
    state: Mutex<FakeState>,
}

impl FakePort {
    fn new() -> Self {
        Self {
            state: Mutex::new(FakeState {
                tunnel: Some(PrestigeTunnelSnapshot {
                    key: key(),
                    depth: 300,
                    max_depth: 300,
                    prestige_level: 5,
                    boss_progress: Some(
                        r#"{"150":{"boss_id":"blightcoil","status":"phase1_defeated"}}"#.to_owned(),
                    ),
                    balance: 5_000,
                }),
                artifacts: Vec::new(),
                next_id: 1,
                duel: None,
                victory_calls: 0,
            }),
        }
    }

    fn insert_unchecked(&self, artifact_id: &str) -> i64 {
        let mut state = self.state.lock().expect("fake state");
        let id = state.next_id;
        state.next_id += 1;
        state.artifacts.push(ArtifactRow {
            database_id: id,
            artifact_id: artifact_id.to_owned(),
            found_at: id,
            is_relic: true,
            equipped: false,
        });
        id
    }

    fn seed_duel(&self) {
        self.state.lock().expect("fake state").duel = Some(ClaimedDuel {
            key: key(),
            boss_id: "blightcoil".to_owned(),
            tier: 150,
            mechanic_id: "blightcoil_wards".to_owned(),
            risk_tier: "cautious".to_owned(),
            wager: 0,
            player_hp: 5,
            boss_hp: 1,
            round_num: 3,
            round_log_json: "[]".to_owned(),
            status_effects_json: r#"{"initial_win_chance":0.5}"#.to_owned(),
            player_hit: 0.9,
            player_damage: 1,
            boss_hit: 0.0,
            boss_damage: 1,
        });
    }

    fn artifact_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("fake state")
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect()
    }
}

impl Prestige4ContentPort for FakePort {
    type Error = &'static str;

    fn tunnel_snapshot(
        &self,
        requested: PrestigeContentKey,
    ) -> Result<Option<PrestigeTunnelSnapshot>, Self::Error> {
        Ok(self
            .state
            .lock()
            .map_err(|_| "poisoned")?
            .tunnel
            .as_ref()
            .filter(|tunnel| tunnel.key == requested)
            .cloned())
    }

    fn artifacts(&self, requested: PrestigeContentKey) -> Result<Vec<ArtifactRow>, Self::Error> {
        if requested != key() {
            return Ok(Vec::new());
        }
        Ok(self.state.lock().map_err(|_| "poisoned")?.artifacts.clone())
    }

    fn grant_artifact_unique(
        &self,
        requested: PrestigeContentKey,
        artifact_id: &str,
        is_relic: bool,
        found_at: i64,
    ) -> Result<ArtifactGrantOutcome, Self::Error> {
        if requested != key() {
            return Err("wrong scope");
        }
        let mut state = self.state.lock().map_err(|_| "poisoned")?;
        if state
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == artifact_id)
        {
            return Ok(ArtifactGrantOutcome::AlreadyOwned);
        }
        let id = state.next_id;
        state.next_id += 1;
        state.artifacts.push(ArtifactRow {
            database_id: id,
            artifact_id: artifact_id.to_owned(),
            found_at,
            is_relic,
            equipped: false,
        });
        Ok(ArtifactGrantOutcome::Granted { database_id: id })
    }

    fn equip_relic_unique(
        &self,
        requested: PrestigeContentKey,
        database_id: i64,
        slot_base: i64,
        slot_max: i64,
    ) -> Result<EquipRelicOutcome, Self::Error> {
        if requested != key() {
            return Ok(EquipRelicOutcome::Missing);
        }
        let mut state = self.state.lock().map_err(|_| "poisoned")?;
        let Some(index) = state
            .artifacts
            .iter()
            .position(|artifact| artifact.database_id == database_id)
        else {
            return Ok(EquipRelicOutcome::Missing);
        };
        if state.artifacts[index].equipped {
            return Ok(EquipRelicOutcome::AlreadyEquipped);
        }
        let artifact_id = state.artifacts[index].artifact_id.clone();
        if state
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == artifact_id && artifact.equipped)
        {
            return Ok(EquipRelicOutcome::DuplicateAlreadyEquipped);
        }
        let prestige = state
            .tunnel
            .as_ref()
            .map_or(0, |tunnel| tunnel.prestige_level);
        let cap = prestige.saturating_add(slot_base).min(slot_max);
        if state
            .artifacts
            .iter()
            .filter(|artifact| artifact.equipped)
            .count() as i64
            >= cap
        {
            return Ok(EquipRelicOutcome::AtCap { cap });
        }
        state.artifacts[index].equipped = true;
        Ok(EquipRelicOutcome::Equipped)
    }

    fn claim_active_duel(
        &self,
        requested: PrestigeContentKey,
    ) -> Result<Option<ClaimedDuel>, Self::Error> {
        if requested != key() {
            return Ok(None);
        }
        Ok(self.state.lock().map_err(|_| "poisoned")?.duel.take())
    }

    fn complete_boss_victory(
        &self,
        request: BossVictoryRequest<'_>,
    ) -> Result<BossVictoryOutcome, Self::Error> {
        let mut state = self.state.lock().map_err(|_| "poisoned")?;
        if state.tunnel.as_ref() != Some(request.expected) {
            return Ok(BossVictoryOutcome::Conflict);
        }
        let tunnel = state.tunnel.as_mut().expect("checked tunnel");
        tunnel.depth = request.depth_after;
        tunnel.max_depth = request.max_depth_after;
        tunnel.boss_progress = Some(request.boss_progress_after.to_owned());
        tunnel.balance += request.balance_delta;
        let balance_after = tunnel.balance;
        state.victory_calls += 1;
        Ok(BossVictoryOutcome::Applied { balance_after })
    }
}

#[derive(Debug, Default)]
struct ScriptedEntropy {
    units: VecDeque<f64>,
    selection: usize,
}

impl ScriptedEntropy {
    fn new(units: impl IntoIterator<Item = f64>) -> Self {
        Self {
            units: units.into_iter().collect(),
            selection: 0,
        }
    }
}

impl Prestige4Entropy for ScriptedEntropy {
    fn unit(&mut self) -> f64 {
        self.units.pop_front().unwrap_or(0.0)
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            return 0;
        }
        let index = self.selection % upper_bound;
        self.selection += 1;
        index
    }
}

fn key() -> PrestigeContentKey {
    PrestigeContentKey {
        discord_id: ACTOR,
        guild_id: None,
    }
}

#[test]
fn test_new_bosses_gated_to_prestige_4() {
    for (boss_id, depth) in [
        ("blightcoil", 150),
        ("rimebound_king", 200),
        ("spineback", 275),
    ] {
        assert!(
            prestige_four_bosses_at(depth, 4)
                .iter()
                .any(|boss| boss.boss_id == boss_id)
        );
        assert!(
            !prestige_four_bosses_at(depth, 3)
                .iter()
                .any(|boss| boss.boss_id == boss_id)
        );
    }
}

#[test]
fn test_new_bosses_fields() {
    let expected = [
        (
            "blightcoil",
            150,
            "The Blightcoil",
            "bruiser",
            "weeping_fang",
        ),
        (
            "rimebound_king",
            200,
            "The Rimebound King",
            "tank",
            "runebitten_shard",
        ),
        ("spineback", 275, "The Spineback", "bruiser", "aching_spine"),
    ];
    for (id, depth, name, archetype, trophy) in expected {
        let content = prestige_boss_content(id).expect("prestige boss");
        assert_eq!(content.boss.depth, depth);
        assert_eq!(content.boss.name, name);
        assert_eq!(content.boss.prestige_required, 4);
        assert_eq!(content.boss.archetype, archetype);
        assert_eq!(content.trophy_relic_id, trophy);
    }
}

#[test]
fn test_reskins_keep_ids_change_names_and_add_trophies() {
    let whisper = prestige_boss_content("xalatath").expect("whisper");
    let red = prestige_boss_content("lilith").expect("red mother");
    assert_eq!(whisper.boss.boss_id, "xalatath");
    assert_eq!(whisper.boss.name, "The Whispering Edge");
    assert_eq!(whisper.boss.prestige_required, 3);
    assert_eq!(whisper.trophy_relic_id, "listening_shard");
    assert_eq!(red.boss.boss_id, "lilith");
    assert_eq!(red.boss.name, "The Red Mother");
    assert_eq!(red.trophy_relic_id, "hateborn_ember");
}

#[test]
fn test_bespoke_phase_overrides_present() {
    for boss_id in BESPOKE_PHASE_BOSS_IDS {
        let boss = boss_by_id(boss_id).expect("boss");
        let phase_two = phase_for(boss_id, boss.depth, 2).expect("phase two");
        let phase_three = phase_for(boss_id, boss.depth, 3).expect("phase three");
        assert!(!phase_two.name.is_empty() && !phase_three.name.is_empty());
        assert!(
            ![
                "The Sporeling Collective",
                "Chronofrost Paradox",
                "The Name Reclaimed"
            ]
            .contains(&phase_two.name)
        );
    }
}

#[test]
fn test_phase_resolver_falls_back_to_depth_default() {
    assert_eq!(
        phase_for("unknown_spore_boss", 150, 2)
            .expect("default phase")
            .name,
        "The Sporeling Collective"
    );
    assert!(phase_for("grothak", 25, 3).is_none());
}

#[test]
fn test_new_mechanics_well_formed() {
    for mechanic_id in PRESTIGE_FOUR_MECHANIC_IDS {
        let definition = prestige_four_mechanic(mechanic_id).expect("mechanic");
        assert_eq!(definition.options.len(), 3);
        assert!(definition.safe_option_index < 3);
        for option in definition.options {
            let sum = option
                .outcome_rolls
                .iter()
                .map(|outcome| outcome.probability)
                .sum::<f64>();
            assert!((sum - 1.0).abs() < 1e-6);
        }
    }
}

#[test]
fn test_new_stingers_resolve() {
    for stinger_id in PRESTIGE_FOUR_STINGER_IDS {
        assert!(prestige_four_stinger(stinger_id).is_some());
    }
}

#[test]
fn test_new_relics_in_catalog_with_lore_and_effect() {
    for relic_id in PRESTIGE_FOUR_TROPHY_IDS
        .into_iter()
        .chain(PRESTIGE_FOUR_GENERAL_RELIC_IDS)
    {
        let content = artifact_content(relic_id).expect("artifact content");
        assert!(content.definition.is_relic);
        assert!(!content.lore.is_empty());
        assert!(!content.effect.is_empty());
    }
    assert_eq!(
        artifact_content("weeping_fang")
            .unwrap()
            .definition
            .min_prestige,
        4
    );
    assert_eq!(
        artifact_content("listening_shard")
            .unwrap()
            .definition
            .min_prestige,
        3
    );
    assert!(PRESTIGE_FOUR_GENERAL_RELIC_IDS.iter().all(|id| {
        artifact_content(id).is_some_and(|content| content.definition.min_prestige == 4)
    }));
}

#[test]
fn test_total_artifact_count() {
    let artifacts = artifact_catalog();
    assert_eq!(artifacts.len(), 47);
    assert_eq!(
        artifacts
            .iter()
            .filter(|artifact| artifact.is_relic)
            .count(),
        45
    );
    assert_eq!(
        artifacts
            .iter()
            .filter(|artifact| !artifact.is_relic)
            .map(|artifact| artifact.id)
            .collect::<BTreeSet<_>>(),
        ["map_of_the_first_descent", "miner_s_lullaby"]
            .into_iter()
            .collect()
    );
}

#[test]
fn test_trophy_weeping_fang_venom_chips_boss() {
    let mut status = TrophyStatus {
        venom_rounds: 4,
        ..TrophyStatus::default()
    };
    let result = run_trophy_round(round_input(), &mut status);
    assert_eq!(result.boss_hp, 9);
    assert!(result.events.venom);
}

#[test]
fn test_trophy_listening_shard_forewarned_only_round_1() {
    let mut status = TrophyStatus {
        forewarned: true,
        ..TrophyStatus::default()
    };
    let mut input = round_input();
    input.round = 1;
    input.boss_hit = true;
    let first = run_trophy_round(input, &mut status);
    assert_eq!(first.player_hp, 5);
    input.round = 2;
    let second = run_trophy_round(input, &mut TrophyStatus::default());
    assert_eq!(second.player_hp, 4);
}

#[test]
fn test_trophy_runebitten_lifesteal_first_hit_only() {
    let mut status = TrophyStatus {
        lifesteal_available: true,
        starting_hp: 3,
        ..TrophyStatus::default()
    };
    let mut input = round_input();
    input.round = 1;
    input.player_hp = 2;
    input.player_hit = true;
    let result = run_trophy_round(input, &mut status);
    assert_eq!(result.player_hp, 3);
    assert!(result.events.lifesteal);
    assert!(!status.lifesteal_available);
}

#[test]
fn test_trophy_aching_spine_regrowth_capped() {
    let mut input = round_input();
    input.player_hp = 3;
    let mut status = TrophyStatus {
        regrowth: true,
        starting_hp: 5,
        ..TrophyStatus::default()
    };
    let result = run_trophy_round(input, &mut status);
    assert_eq!(result.player_hp, 4);
    assert!(result.events.regrowth);
    status.starting_hp = 3;
    let capped = run_trophy_round(input, &mut status);
    assert_eq!(capped.player_hp, 3);
    assert!(!capped.events.regrowth);
}

#[test]
fn test_trophy_hateborn_last_stand_extra_damage_at_1hp() {
    let mut input = round_input();
    input.player_hp = 1;
    input.player_hit = true;
    let mut status = TrophyStatus {
        last_stand: true,
        ..TrophyStatus::default()
    };
    let result = run_trophy_round(input, &mut status);
    assert_eq!(result.boss_hp, 8);
    assert!(result.events.last_stand);
}

fn round_input() -> TrophyRoundInput {
    TrophyRoundInput {
        round: 2,
        player_hp: 5,
        boss_hp: 10,
        player_hit: false,
        player_damage: 1,
        boss_hit: false,
        boss_damage: 1,
    }
}

#[test]
fn test_carve_grants_then_dedups() {
    let mut service = DigPrestige4Service::new(FakePort::new(), ScriptedEntropy::default());
    let first = service
        .maybe_carve_trophy(key(), "blightcoil", 100)
        .expect("carve")
        .expect("drop");
    assert_eq!(first.id, "weeping_fang");
    assert_eq!(service.port().artifact_ids(), vec!["weeping_fang"]);
    assert!(
        service
            .maybe_carve_trophy(key(), "blightcoil", 101)
            .expect("dedup")
            .is_none()
    );
}

#[test]
fn test_carve_respects_rate() {
    let mut service = DigPrestige4Service::new(FakePort::new(), ScriptedEntropy::new([0.99]));
    assert!(
        service
            .maybe_carve_trophy(key(), "spineback", 100)
            .expect("rate result")
            .is_none()
    );
}

#[test]
fn test_carve_drops_through_live_victory() {
    let port = FakePort::new();
    port.seed_duel();
    let mut service = DigPrestige4Service::new(port, ScriptedEntropy::default());
    let result = service
        .resume_live_victory(ResumeVictoryRequest {
            key: key(),
            boss_id: "blightcoil",
            boss_progress_after: r#"{"150":{"boss_id":"blightcoil","status":"defeated"}}"#,
            balance_delta: 20,
            detail_json: r#"{"won":true}"#,
            now: 1_000,
        })
        .expect("resume victory");
    assert!(matches!(
        result,
        ResumeVictoryOutcome::Won {
            trophy: Some(ArtifactDrop {
                id: "weeping_fang",
                ..
            })
        }
    ));
    let state = service.port().state.lock().expect("fake state");
    assert_eq!(state.victory_calls, 1);
    assert!(state.duel.is_none());
}

#[test]
fn test_trophies_excluded_from_prestige_relic_pool() {
    let mut service = DigPrestige4Service::new(FakePort::new(), ScriptedEntropy::default());
    let mut dropped = BTreeSet::new();
    for now in 0..300 {
        if let Some(drop) = service
            .maybe_drop_prestige_relic(key(), 5, now)
            .expect("prestige drop")
        {
            dropped.insert(drop.id);
        }
    }
    assert!(dropped.is_disjoint(&TROPHY_RELIC_IDS.into_iter().collect()));
    assert!(!dropped.is_disjoint(&PRESTIGE_FOUR_GENERAL_RELIC_IDS.into_iter().collect()));
}

#[test]
fn test_roll_artifact_finds_only_ordinary_relics() {
    let mut service = DigPrestige4Service::new(FakePort::new(), ScriptedEntropy::default());
    let mut found = BTreeSet::new();
    for now in 0..80 {
        if let Some(drop) = service
            .roll_artifact(key(), 160, 1.0, now)
            .expect("artifact roll")
        {
            found.insert(drop.id);
        }
    }
    let ordinary = artifact_catalog()
        .into_iter()
        .filter(|artifact| is_ordinary_relic(*artifact))
        .map(|artifact| artifact.id)
        .collect::<BTreeSet<_>>();
    assert!(!found.is_empty());
    assert!(found.is_subset(&ordinary));
    for excluded in ["deepveined_coal", "aching_spine", "hollow_fang"] {
        assert!(!found.contains(excluded));
    }
}

#[test]
fn test_roll_artifact_relics_are_unique() {
    let mut service = DigPrestige4Service::new(FakePort::new(), ScriptedEntropy::default());
    let mut found = Vec::new();
    for now in 0..80 {
        if let Some(drop) = service
            .roll_artifact(key(), 160, 1.0, now)
            .expect("artifact roll")
        {
            found.push(drop.id);
        }
    }
    assert!(!found.is_empty());
    assert_eq!(
        found.len(),
        found.iter().copied().collect::<BTreeSet<_>>().len()
    );
}

#[test]
fn test_equip_rejects_duplicate_relic() {
    let port = FakePort::new();
    let first = port.insert_unchecked("mole_claws");
    let second = port.insert_unchecked("mole_claws");
    let service = DigPrestige4Service::new(port, ScriptedEntropy::default());
    assert_eq!(
        service.equip_relic(key(), first).expect("equip first"),
        EquipRelicOutcome::Equipped
    );
    assert_eq!(
        service.equip_relic(key(), second).expect("equip duplicate"),
        EquipRelicOutcome::DuplicateAlreadyEquipped
    );
}

#[test]
fn test_prestige_relic_drop_skips_owned() {
    let port = FakePort::new();
    port.insert_unchecked("deepveined_coal");
    let mut service = DigPrestige4Service::new(port, ScriptedEntropy::default());
    for now in 0..100 {
        if let Some(drop) = service
            .maybe_drop_prestige_relic(key(), 5, now)
            .expect("prestige drop")
        {
            assert_ne!(drop.id, "deepveined_coal");
        }
    }
}

#[test]
fn test_deepveined_coal_boosts_yield_only_in_the_dark() {
    assert_eq!(deepveined_yield_multiplier(true, 100), 1.0);
    assert!((deepveined_yield_multiplier(true, 5) - 1.20).abs() < f64::EPSILON);
}

#[test]
fn test_pathfinders_spur_adds_advance_in_deep_layers() {
    assert_eq!(pathfinders_advance(2, false, 160), 2);
    assert_eq!(pathfinders_advance(2, true, 160), 3);
    assert_eq!(pathfinders_advance(2, true, 149), 2);
}

#[test]
fn test_diviners_knot_boosts_risky_success() {
    let base = 0.5;
    let roll = 0.55;
    assert!(roll >= diviners_risky_success_chance(base, false, true));
    assert!(roll < diviners_risky_success_chance(base, true, true));
    assert_eq!(diviners_risky_success_chance(base, true, false), base);
}
