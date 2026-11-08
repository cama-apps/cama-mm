use std::collections::{BTreeSet, VecDeque};

use super::*;

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

#[derive(Debug)]
struct UnitChoosingEntropy {
    units: VecDeque<f64>,
}

impl UnitChoosingEntropy {
    fn new(units: impl IntoIterator<Item = f64>) -> Self {
        Self {
            units: units.into_iter().collect(),
        }
    }
}

impl Prestige4Entropy for UnitChoosingEntropy {
    fn unit(&mut self) -> f64 {
        self.units.pop_front().unwrap_or(0.99)
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            return 0;
        }
        ((self.unit().clamp(0.0, 1.0 - f64::EPSILON) * upper_bound as f64) as usize)
            .min(upper_bound - 1)
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
fn test_artifact_skip_does_not_consume_entropy() {
    let mut entropy = ScriptedEntropy::new([0.123]);
    let result = roll_artifact_stage(
        ArtifactRollPlan {
            depth: 10,
            rate_modifier: 1.0,
            skip_artifact: true,
        },
        &BTreeSet::new(),
        &mut entropy,
    );
    assert_eq!(result, None);
    assert!((entropy.unit() - 0.123).abs() < f64::EPSILON);
}

#[test]
fn test_artifact_rate_modifier_is_multiplicative() {
    let modifier = artifact_rate_modifier(true, 2.0, 0.75, 2.0, 0.25, 0.5);
    assert!((modifier - 2.0625).abs() < f64::EPSILON);
    assert_eq!(artifact_rate_modifier(false, 1.0, 1.0, 1.0, 0.0, 1.0), 1.0);
}

#[test]
fn test_artifact_rarity_and_layer_fallback_match_python_roll() {
    assert_eq!(rarity_for_roll(0.59), Rarity::Common);
    assert_eq!(rarity_for_roll(0.60), Rarity::Uncommon);
    assert_eq!(rarity_for_roll(0.90), Rarity::Rare);
    assert_eq!(rarity_for_roll(0.99), Rarity::Legendary);

    let owned = BTreeSet::new();
    let dirt_common = ordinary_relic_candidates(10, Rarity::Common, &owned);
    assert!(!dirt_common.is_empty());
    assert!(dirt_common.iter().all(|artifact| artifact.layer == "Dirt"));
    assert!(
        dirt_common
            .iter()
            .any(|artifact| artifact.id == "midas_splinter")
    );

    let legendary_fallback = ordinary_relic_candidates(10, Rarity::Legendary, &owned);
    assert!(!legendary_fallback.is_empty());
    assert!(
        legendary_fallback
            .iter()
            .all(|artifact| is_ordinary_relic(*artifact) && artifact.rarity == Rarity::Legendary)
    );
    assert!(
        legendary_fallback
            .iter()
            .any(|artifact| artifact.layer != "Dirt")
    );

    let mut entropy = UnitChoosingEntropy::new([0.0, 0.0, 0.999_999]);
    let stage = roll_artifact_stage(
        ArtifactRollPlan {
            depth: 10,
            rate_modifier: 1.0,
            skip_artifact: false,
        },
        &owned,
        &mut entropy,
    )
    .expect("deterministic common artifact");
    assert_eq!(
        stage.definition,
        *dirt_common.last().expect("dirt candidate")
    );
    assert_eq!(stage.current_run_artifacts_delta, 1);

    let owned_dirt = dirt_common
        .iter()
        .map(|artifact| artifact.id.to_owned())
        .collect::<BTreeSet<_>>();
    let fallback_after_owned = ordinary_relic_candidates(10, Rarity::Common, &owned_dirt);
    assert!(
        fallback_after_owned
            .iter()
            .any(|artifact| artifact.layer != "Dirt")
    );
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
