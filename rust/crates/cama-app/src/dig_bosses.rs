//! Typed, transport-independent policy for regular and pinnacle dig bosses.
//!
//! Python remains the live implementation. This additive module mirrors the
//! boss catalog and state transitions behind explicit in-memory ports, allowing
//! parity tests to exercise locking, HP carry-over, echoes, prestige gating,
//! mechanics, and relic settlement without opening Discord or the shared DB.

use std::collections::{BTreeMap, BTreeSet};

pub const BOSS_BOUNDARIES: [i32; 7] = [25, 50, 75, 100, 150, 200, 275];
pub const PINNACLE_DEPTH: i32 = 350;
pub const PINNACLE_REPROC_DEPTH: i32 = 450;
pub const PRESTIGE_HARD_CAP: i32 = 500;
pub const PINNACLE_SECRET_PHASE_CHANCE: f64 = 0.10;
pub const BOSS_ROUND_CAP: u8 = 20;
pub const BOSS_HP_REGEN_PER_24_HOURS: i32 = 1;
pub const WIN_CHANCE_CAP: f64 = 0.95;
pub const WIN_CHANCE_FLOOR: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Archetype {
    pub hp_multiplier: f64,
    pub hit_offset: f64,
    pub damage_offset: i32,
}

pub const fn archetype(name: &str) -> Archetype {
    match name.as_bytes() {
        b"tank" => Archetype {
            hp_multiplier: 1.5,
            hit_offset: -0.03,
            damage_offset: 0,
        },
        b"glass_cannon" => Archetype {
            hp_multiplier: 0.7,
            hit_offset: 0.05,
            damage_offset: 1,
        },
        b"slippery" => Archetype {
            hp_multiplier: 0.8,
            hit_offset: 0.10,
            damage_offset: 0,
        },
        _ => Archetype {
            hp_multiplier: 1.0,
            hit_offset: 0.0,
            damage_offset: 0,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatStats {
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: f64,
    pub player_damage: i32,
    pub boss_hit: f64,
    pub boss_damage: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DifficultyBonus {
    hp: i32,
    hit: f64,
    damage: i32,
}

const fn tier_bonus(depth: i32) -> DifficultyBonus {
    match depth {
        50 => DifficultyBonus {
            hp: 1,
            hit: 0.0,
            damage: 0,
        },
        75 => DifficultyBonus {
            hp: 2,
            hit: 0.01,
            damage: 0,
        },
        100 => DifficultyBonus {
            hp: 3,
            hit: 0.03,
            damage: 0,
        },
        150 => DifficultyBonus {
            hp: 6,
            hit: 0.05,
            damage: 0,
        },
        200 => DifficultyBonus {
            hp: 6,
            hit: 0.06,
            damage: 0,
        },
        275 => DifficultyBonus {
            hp: 7,
            hit: 0.07,
            damage: 0,
        },
        350 => DifficultyBonus {
            hp: 9,
            hit: 0.10,
            damage: 0,
        },
        _ => DifficultyBonus {
            hp: 0,
            hit: 0.0,
            damage: 0,
        },
    }
}

const fn normalized_tier(depth: i32) -> i32 {
    match depth {
        350.. => 350,
        275.. => 275,
        200.. => 200,
        150.. => 150,
        100.. => 100,
        75.. => 75,
        50.. => 50,
        _ => 25,
    }
}

const fn prestige_bonus(level: i32) -> DifficultyBonus {
    match level {
        i32::MIN..=0 => DifficultyBonus {
            hp: 0,
            hit: 0.0,
            damage: 0,
        },
        1 => DifficultyBonus {
            hp: 9,
            hit: 0.08,
            damage: 0,
        },
        2 => DifficultyBonus {
            hp: 9,
            hit: 0.09,
            damage: 0,
        },
        3 => DifficultyBonus {
            hp: 12,
            hit: 0.13,
            damage: 0,
        },
        4 => DifficultyBonus {
            hp: 14,
            hit: 0.15,
            damage: 0,
        },
        5 => DifficultyBonus {
            hp: 24,
            hit: 0.21,
            damage: 0,
        },
        6 => DifficultyBonus {
            hp: 26,
            hit: 0.24,
            damage: 0,
        },
        _ => DifficultyBonus {
            hp: 27,
            hit: 0.26,
            damage: 1,
        },
    }
}

#[must_use]
pub fn scale_boss_stats(
    stats: CombatStats,
    boss_id: &str,
    at_boss: i32,
    prestige_level: i32,
    echo_applied: bool,
) -> CombatStats {
    let boss = boss_by_id(boss_id);
    let archetype_name = boss.map_or("bruiser", |definition| definition.archetype);
    scale_boss_stats_for_archetype(stats, archetype_name, at_boss, prestige_level, echo_applied)
}

#[must_use]
pub fn scale_boss_stats_for_archetype(
    stats: CombatStats,
    archetype_name: &str,
    at_boss: i32,
    prestige_level: i32,
    echo_applied: bool,
) -> CombatStats {
    let kind = archetype(archetype_name);
    let tier = tier_bonus(normalized_tier(at_boss));
    let prestige = prestige_bonus(prestige_level);
    let mut hp =
        f64::from(stats.boss_hp).mul_add(kind.hp_multiplier, f64::from(tier.hp + prestige.hp));
    if prestige_level >= 4 {
        hp *= 1.0 + 0.03 * f64::from(prestige_level - 3);
    }
    let mut hp = hp.round().max(1.0) as i32;
    if echo_applied {
        hp = (f64::from(hp) * 0.75).round().max(1.0) as i32;
    }
    CombatStats {
        boss_hp: hp,
        boss_hit: (stats.boss_hit + kind.hit_offset + tier.hit + prestige.hit).clamp(0.05, 0.95),
        boss_damage: (stats.boss_damage + kind.damage_offset + tier.damage + prestige.damage)
            .max(1),
        ..stats
    }
}

#[must_use]
pub const fn luminosity_combat_penalty(luminosity: i32) -> (f64, i32) {
    match luminosity {
        76.. => (0.0, 0),
        26..=75 => (-0.03, 0),
        1..=25 => (-0.08, 0),
        _ => (-0.15, 1),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DuelOddsInput {
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: f64,
    pub player_damage: i32,
    pub boss_hit: f64,
    pub boss_damage: i32,
    pub critical_chance: f64,
    pub critical_bonus: i32,
}

/// Exact absorbing-chain equivalent of the Python Monte-Carlo scout helper.
#[must_use]
pub fn approximate_duel_win_probability(input: DuelOddsInput) -> f64 {
    duel_win_probability_with_damage_bonus(input, 0)
}

/// Exact absorbing-chain equivalent of Python's scout simulation, including
/// the per-hit fractional pet-damage roll. Keeping the fractional branch in
/// the probability graph avoids rounding a 12% assist into a permanent +0 or
/// +1 damage and lets live fight, scout, and payout policy share one number.
#[must_use]
pub fn duel_win_probability_with_damage_bonus(
    input: DuelOddsInput,
    player_damage_bonus_percent: i32,
) -> f64 {
    if input.player_hp <= 0
        || input.boss_hp <= 0
        || input.player_damage <= 0
        || input.boss_damage <= 0
    {
        return 0.0;
    }
    let player_hit = input.player_hit.clamp(0.0, 1.0);
    let boss_hit = input.boss_hit.clamp(0.0, 1.0);
    let critical = input.critical_chance.clamp(0.0, 1.0);
    let mut odds = vec![vec![0.0; (input.boss_hp + 1) as usize]; (input.player_hp + 1) as usize];
    let self_loop = (1.0 - player_hit) * (1.0 - boss_hit);
    let denominator = 1.0 - self_loop;

    for player_hp in 1..=input.player_hp {
        for boss_hp in 1..=input.boss_hp {
            let mut contribution = 0.0;
            let damage_outcomes = [
                (player_hit * (1.0 - critical), input.player_damage),
                (
                    player_hit * critical,
                    input.player_damage + input.critical_bonus,
                ),
            ];
            for (hit_probability, damage) in damage_outcomes {
                let base_damage = damage.max(0);
                let scaled = base_damage.saturating_mul(player_damage_bonus_percent.max(0));
                let guaranteed = scaled / 100;
                let fractional = f64::from(scaled % 100) / 100.0;
                for (bonus_probability, bonus) in
                    [(1.0 - fractional, guaranteed), (fractional, guaranteed + 1)]
                {
                    let probability = hit_probability * bonus_probability;
                    if probability == 0.0 {
                        continue;
                    }
                    let remaining_boss = boss_hp - base_damage - bonus;
                    if remaining_boss <= 0 {
                        contribution += probability;
                        continue;
                    }
                    let remaining_boss = remaining_boss as usize;
                    contribution +=
                        probability * (1.0 - boss_hit) * odds[player_hp as usize][remaining_boss];
                    if player_hp > input.boss_damage {
                        contribution += probability
                            * boss_hit
                            * odds[(player_hp - input.boss_damage) as usize][remaining_boss];
                    }
                }
            }
            if player_hp > input.boss_damage {
                contribution += (1.0 - player_hit)
                    * boss_hit
                    * odds[(player_hp - input.boss_damage) as usize][boss_hp as usize];
            }
            odds[player_hp as usize][boss_hp as usize] = if denominator > 0.0 {
                contribution / denominator
            } else {
                0.0
            };
        }
    }
    odds[input.player_hp as usize][input.boss_hp as usize].clamp(WIN_CHANCE_FLOOR, WIN_CHANCE_CAP)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BossDefinition {
    pub depth: i32,
    pub boss_id: &'static str,
    pub name: &'static str,
    pub prestige_required: i32,
    pub mechanic_pool: [&'static str; 3],
    pub stinger_id: &'static str,
    pub archetype: &'static str,
}

macro_rules! boss {
    ($depth:literal, $id:literal, $name:literal, $prestige:literal, $archetype:literal, [$m1:literal, $m2:literal, $m3:literal], $stinger:literal) => {
        BossDefinition {
            depth: $depth,
            boss_id: $id,
            name: $name,
            prestige_required: $prestige,
            mechanic_pool: [$m1, $m2, $m3],
            stinger_id: $stinger,
            archetype: $archetype,
        }
    };
}

pub const BOSSES: [BossDefinition; 33] = [
    boss!(
        25,
        "grothak",
        "Grothak the Unbreakable",
        0,
        "bruiser",
        [
            "grothak_earthquake",
            "grothak_crumble_wall",
            "grothak_bedrock_bellow"
        ],
        "grothak_crumble"
    ),
    boss!(
        25,
        "pudge",
        "The Butcher",
        0,
        "tank",
        ["pudge_hook", "pudge_rot", "pudge_dismember"],
        "pudge_drag"
    ),
    boss!(
        25,
        "ogre_magi",
        "The Twin-Skulled",
        0,
        "glass_cannon",
        ["ogre_multicast", "ogre_fireblast", "ogre_bloodlust"],
        "ogre_blast"
    ),
    boss!(
        50,
        "crystalia",
        "Crystalia the Refracted",
        0,
        "bruiser",
        [
            "crystalia_prism",
            "crystalia_shatter",
            "crystalia_mirror_maze"
        ],
        "crystalia_shard"
    ),
    boss!(
        50,
        "crystal_maiden",
        "The Frostbinder",
        0,
        "glass_cannon",
        ["cm_frostbite", "cm_freezing_field", "cm_crystal_nova"],
        "cm_freeze"
    ),
    boss!(
        50,
        "tusk",
        "The Ice Warlord",
        0,
        "tank",
        ["tusk_snowball", "tusk_walrus_punch", "tusk_ice_shards"],
        "tusk_kick"
    ),
    boss!(
        75,
        "magmus_rex",
        "Magmus Rex",
        0,
        "tank",
        ["magmus_eruption", "magmus_meteor", "magmus_lava_tide"],
        "magmus_burn"
    ),
    boss!(
        75,
        "lina",
        "The Scorchwitch",
        0,
        "glass_cannon",
        ["lina_laguna", "lina_dragon_slave", "lina_light_strike"],
        "lina_scorch"
    ),
    boss!(
        75,
        "doom",
        "The Deathbringer",
        0,
        "bruiser",
        ["doom_mark", "doom_scorched_earth", "doom_infernal_blade"],
        "doom_brand"
    ),
    boss!(
        100,
        "void_warden",
        "The Void Warden",
        0,
        "slippery",
        [
            "voidwarden_collapse",
            "voidwarden_silence",
            "voidwarden_gravity_well"
        ],
        "void_collapse"
    ),
    boss!(
        100,
        "spectre",
        "The Dread Shade",
        0,
        "slippery",
        ["spectre_haunt", "spectre_dagger", "spectre_dispersion"],
        "spectre_haunting"
    ),
    boss!(
        100,
        "void_spirit",
        "The Astral Echo",
        0,
        "slippery",
        [
            "void_spirit_step",
            "void_spirit_aether",
            "void_spirit_resonant_pulse"
        ],
        "void_spirit_exile"
    ),
    boss!(
        150,
        "sporeling_sovereign",
        "Sporeling Sovereign",
        0,
        "tank",
        ["sporeling_cloud", "sporeling_roots", "sporeling_bloom"],
        "sporeling_rot"
    ),
    boss!(
        150,
        "treant_protector",
        "The Elder Grove",
        0,
        "tank",
        [
            "treant_overgrowth",
            "treant_leech_seed",
            "treant_living_armor"
        ],
        "treant_entangle"
    ),
    boss!(
        150,
        "broodmother",
        "The Nestmother",
        0,
        "glass_cannon",
        [
            "broodmother_spawn",
            "broodmother_web",
            "broodmother_silken_snare"
        ],
        "broodmother_webbing"
    ),
    boss!(
        150,
        "aegis_warden",
        "The Aegis Warden",
        2,
        "tank",
        ["aegis_reclaim", "aegis_bulwark", "aegis_last_stand"],
        "aegis_reversal"
    ),
    boss!(
        150,
        "cairn_general",
        "The Cairn General",
        2,
        "bruiser",
        [
            "cairn_remnant",
            "cairn_rolling_fault",
            "cairn_magnetic_recall"
        ],
        "cairn_aftershock"
    ),
    boss!(
        150,
        "xalatath",
        "The Whispering Edge",
        3,
        "slippery",
        [
            "xalatath_void_pull",
            "xalatath_whisper_madness",
            "xalatath_blackout"
        ],
        "xalatath_unraveling"
    ),
    boss!(
        150,
        "blightcoil",
        "The Blightcoil",
        4,
        "bruiser",
        [
            "blightcoil_wards",
            "blightcoil_nova",
            "blightcoil_soul_lattice"
        ],
        "blightcoil_venom"
    ),
    boss!(
        200,
        "chronofrost",
        "Chronofrost",
        0,
        "slippery",
        [
            "chronofrost_still",
            "chronofrost_rewind",
            "chronofrost_time_shard"
        ],
        "chronofrost_stillness"
    ),
    boss!(
        200,
        "faceless_void",
        "The Timeless One",
        0,
        "slippery",
        [
            "faceless_void_chrono",
            "faceless_void_backtrack",
            "faceless_void_time_dilation"
        ],
        "void_chrono"
    ),
    boss!(
        200,
        "weaver",
        "The Skitterwing",
        0,
        "slippery",
        [
            "weaver_timelapse",
            "weaver_shukuchi",
            "weaver_geminate_strike"
        ],
        "weaver_unmake"
    ),
    boss!(
        200,
        "heartspire",
        "The Heartspire",
        2,
        "bruiser",
        [
            "heartspire_intent",
            "heartspire_blood_tithe",
            "heartspire_crimson_pulse"
        ],
        "heartspire_doubt"
    ),
    boss!(
        200,
        "brass_croupier",
        "The Brass Croupier",
        2,
        "glass_cannon",
        [
            "croupier_malice_cube",
            "croupier_clockwork_ballet",
            "croupier_overdraw"
        ],
        "croupier_collection"
    ),
    boss!(
        200,
        "lilith",
        "The Red Mother",
        3,
        "glass_cannon",
        [
            "lilith_blood_nova",
            "lilith_wing_descent",
            "lilith_blood_tether"
        ],
        "lilith_hemorrhage"
    ),
    boss!(
        200,
        "rimebound_king",
        "The Rimebound King",
        4,
        "tank",
        [
            "rimebound_harvest",
            "rimebound_raise",
            "rimebound_frozen_throne"
        ],
        "rimebound_soulchill"
    ),
    boss!(
        275,
        "nameless_depth",
        "The Nameless Depth",
        0,
        "tank",
        [
            "nameless_whisper",
            "nameless_silence",
            "nameless_false_floor"
        ],
        "nameless_erase"
    ),
    boss!(
        275,
        "oracle",
        "The Blindfolded Seer",
        0,
        "glass_cannon",
        [
            "oracle_fortune",
            "oracle_false_promise",
            "oracle_purifying_flames"
        ],
        "oracle_fate"
    ),
    boss!(
        275,
        "terrorblade",
        "The Sundered Prince",
        0,
        "glass_cannon",
        [
            "terrorblade_sunder",
            "terrorblade_metamorphosis",
            "terrorblade_reflection"
        ],
        "terrorblade_sundering"
    ),
    boss!(
        275,
        "emberwright",
        "The Emberwright",
        2,
        "bruiser",
        [
            "emberwright_overclock",
            "emberwright_molten_anvil",
            "emberwright_scrap_volley"
        ],
        "emberwright_slag"
    ),
    boss!(
        275,
        "saltveil_commodore",
        "The Saltveil Commodore",
        2,
        "tank",
        [
            "saltveil_spyglass",
            "saltveil_chainshot",
            "saltveil_boarding"
        ],
        "saltveil_pressgang"
    ),
    boss!(
        275,
        "underlord",
        "The Pit Lord",
        3,
        "tank",
        [
            "underlord_pit_pull",
            "underlord_firestorm",
            "underlord_dark_rift"
        ],
        "underlord_atrophy"
    ),
    boss!(
        275,
        "spineback",
        "The Spineback",
        4,
        "bruiser",
        [
            "spineback_regrowth",
            "spineback_divebomb",
            "spineback_quill_barrage"
        ],
        "spineback_rend"
    ),
];

#[must_use]
pub fn boss_by_id(boss_id: &str) -> Option<&'static BossDefinition> {
    BOSSES.iter().find(|boss| boss.boss_id == boss_id)
}

#[must_use]
pub fn boss_pool_for_tier(depth: i32, prestige_level: i32) -> Vec<&'static BossDefinition> {
    BOSSES
        .iter()
        .filter(|boss| boss.depth == depth && boss.prestige_required <= prestige_level)
        .collect()
}

#[must_use]
pub fn full_boss_pool_for_tier(depth: i32) -> Vec<&'static BossDefinition> {
    boss_pool_for_tier(depth, 99)
}

#[must_use]
pub fn stinger_registered(stinger_id: &str) -> bool {
    BOSSES.iter().any(|boss| boss.stinger_id == stinger_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusEffect {
    Bleed,
    Reveal,
    Silence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combatant {
    Player,
    Boss,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutcomeRoll {
    pub probability: f64,
    pub player_hp_delta: i32,
    pub boss_hp_delta: i32,
    pub status_effect: Option<StatusEffect>,
    pub skip_next_round_for: Option<Combatant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MechanicOption {
    pub label: &'static str,
    pub flavor: &'static str,
    pub outcome_rolls: Vec<OutcomeRoll>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BossMechanic {
    pub mechanic_id: &'static str,
    pub trigger_round: u8,
    pub safe_option_index: usize,
    pub options: Vec<MechanicOption>,
}

const fn roll(
    probability: f64,
    player_hp_delta: i32,
    boss_hp_delta: i32,
    status_effect: Option<StatusEffect>,
    skip_next_round_for: Option<Combatant>,
) -> OutcomeRoll {
    OutcomeRoll {
        probability,
        player_hp_delta,
        boss_hp_delta,
        status_effect,
        skip_next_round_for,
    }
}

fn option(
    label: &'static str,
    flavor: &'static str,
    outcome_rolls: &[OutcomeRoll],
) -> MechanicOption {
    MechanicOption {
        label,
        flavor,
        outcome_rolls: outcome_rolls.to_vec(),
    }
}

fn generic_mechanic(mechanic_id: &'static str) -> BossMechanic {
    let safe = [roll(0.75, 0, 0, None, None), roll(0.25, -1, -1, None, None)];
    let measured = [roll(0.60, 0, -2, None, None), roll(0.40, -2, 0, None, None)];
    let risky = [roll(0.40, 0, -3, None, None), roll(0.60, -3, 0, None, None)];
    BossMechanic {
        mechanic_id,
        trigger_round: 3,
        safe_option_index: 0,
        options: vec![
            option("Hold position", "You choose the measured line.", &safe),
            option(
                "Press the opening",
                "You trade space for damage.",
                &measured,
            ),
            option("Risk everything", "You commit through the hazard.", &risky),
        ],
    }
}

fn king_feast() -> BossMechanic {
    BossMechanic {
        mechanic_id: "king_feast",
        trigger_round: 3,
        safe_option_index: 2,
        options: vec![
            option(
                "Throw a loose stone",
                "You skip a shard of rubble across the maw.",
                &[
                    roll(0.65, 0, -2, None, None),
                    roll(0.35, -1, -1, None, None),
                ],
            ),
            option(
                "Strike the open throat",
                "You aim at the vulnerable second.",
                &[
                    roll(0.45, 0, -4, None, None),
                    roll(0.55, -3, 0, Some(StatusEffect::Bleed), None),
                ],
            ),
            option(
                "Hold the outer ring",
                "You keep a measured distance from the hunger.",
                &[roll(0.75, 0, 0, None, None), roll(0.25, -1, -1, None, None)],
            ),
        ],
    }
}

fn king_deathbed() -> BossMechanic {
    let mut mechanic = generic_mechanic("king_deathbed");
    mechanic.safe_option_index = 2;
    mechanic.options[0].outcome_rolls[0].boss_hp_delta = -1;
    mechanic
}

fn hollow_walls_close() -> BossMechanic {
    let mut mechanic = generic_mechanic("hollow_walls_close");
    mechanic.safe_option_index = 2;
    mechanic.options[1].label = "Risk a brace against the crush";
    mechanic
}

fn digger_phasing() -> BossMechanic {
    generic_mechanic("digger_phasing")
}

fn digger_high_risk(mechanic_id: &'static str, safe_option_index: usize) -> BossMechanic {
    let mut mechanic = generic_mechanic(mechanic_id);
    mechanic.safe_option_index = safe_option_index;
    mechanic.options[2].outcome_rolls = vec![
        roll(0.35, 0, -4, None, None),
        roll(
            0.65,
            -4,
            0,
            Some(StatusEffect::Bleed),
            Some(Combatant::Player),
        ),
    ];
    mechanic
}

fn digger_last_excavation() -> BossMechanic {
    let mut mechanic = generic_mechanic("digger_last_excavation");
    mechanic.trigger_round = 4;
    mechanic
}

fn low_risk_safe_mechanic(mechanic_id: &'static str, safe_option_index: usize) -> BossMechanic {
    let mut mechanic = generic_mechanic(mechanic_id);
    mechanic.safe_option_index = safe_option_index;
    mechanic.options[safe_option_index].outcome_rolls =
        vec![roll(0.75, 0, -1, None, None), roll(0.25, -1, 0, None, None)];
    for (index, option) in mechanic.options.iter_mut().enumerate() {
        if index != safe_option_index {
            option.outcome_rolls = vec![
                roll(0.50, 0, -3, None, None),
                roll(0.50, -3, 0, Some(StatusEffect::Silence), None),
            ];
        }
    }
    mechanic
}

fn king_crownfall() -> BossMechanic {
    let mut mechanic = generic_mechanic("king_crownfall");
    mechanic.trigger_round = 4;
    mechanic.options[2].label = "Catch the crown";
    mechanic.options[2].outcome_rolls = vec![
        roll(0.35, 0, -3, None, None),
        roll(0.65, -3, 0, Some(StatusEffect::Bleed), None),
    ];
    mechanic
}

#[must_use]
pub fn mechanic(mechanic_id: &str) -> Option<BossMechanic> {
    let registered = BOSSES
        .iter()
        .flat_map(|boss| boss.mechanic_pool)
        .chain(
            PINNACLE_BOSSES
                .iter()
                .flat_map(|boss| boss.phases.iter())
                .flat_map(|phase| phase.mechanic_pool),
        )
        .find(|candidate| *candidate == mechanic_id)?;
    Some(match registered {
        "king_feast" => king_feast(),
        "king_deathbed" => king_deathbed(),
        "hollow_walls_close" => hollow_walls_close(),
        "digger_phasing" => digger_phasing(),
        "digger_pickaxe_duel" => digger_high_risk("digger_pickaxe_duel", 0),
        "digger_tunnel_collapse" => digger_high_risk("digger_tunnel_collapse", 1),
        "digger_last_excavation" => digger_last_excavation(),
        "hollow_shape_shift" => low_risk_safe_mechanic("hollow_shape_shift", 1),
        "hollow_many_voices" => low_risk_safe_mechanic("hollow_many_voices", 2),
        "king_crownfall" => king_crownfall(),
        other => generic_mechanic(other),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnaclePhaseDefinition {
    pub title: &'static str,
    pub archetype: &'static str,
    pub mechanic_pool: [&'static str; 2],
    pub secret_title: Option<&'static str>,
    pub secret_dialogue: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnacleBossDefinition {
    pub boss_id: &'static str,
    pub name: &'static str,
    pub persona: &'static str,
    pub ascii_art: &'static str,
    pub phases: [PinnaclePhaseDefinition; 3],
}

const NO_SECRET: &[&str] = &[];
const SECRET_LINES: &[&str] = &["The hidden phase opens.", "The depth remembers."];

macro_rules! pinnacle_phase {
    ($title:literal, $archetype:literal, [$m1:literal, $m2:literal]) => {
        PinnaclePhaseDefinition {
            title: $title,
            archetype: $archetype,
            mechanic_pool: [$m1, $m2],
            secret_title: None,
            secret_dialogue: NO_SECRET,
        }
    };
    ($title:literal, $archetype:literal, [$m1:literal, $m2:literal], $secret:literal) => {
        PinnaclePhaseDefinition {
            title: $title,
            archetype: $archetype,
            mechanic_pool: [$m1, $m2],
            secret_title: Some($secret),
            secret_dialogue: SECRET_LINES,
        }
    };
}

pub const PINNACLE_BOSSES: [PinnacleBossDefinition; 6] = [
    PinnacleBossDefinition {
        boss_id: "forgotten_king",
        name: "The Forgotten King",
        persona: "ancient, dignified, hollowed by time",
        ascii_art: "<the forgotten king>",
        phases: [
            pinnacle_phase!(
                "The Forgotten King",
                "tank",
                ["king_decree", "king_crownfall"]
            ),
            pinnacle_phase!(
                "The Crowned Hunger",
                "glass_cannon",
                ["king_feast", "king_royal_hunt"]
            ),
            pinnacle_phase!(
                "The Last Breath of Kings",
                "slippery",
                ["king_deathbed", "king_final_judgment"],
                "The Crown Remembers"
            ),
        ],
    },
    PinnacleBossDefinition {
        boss_id: "hollowforged",
        name: "Hollowforged",
        persona: "the depth made flesh, plural, mineral",
        ascii_art: "<hollowforged>",
        phases: [
            pinnacle_phase!(
                "Hollowforged",
                "bruiser",
                ["hollow_walls_close", "hollow_empty_gaze"]
            ),
            pinnacle_phase!(
                "Hollowforged Reformed",
                "tank",
                ["hollow_shape_shift", "hollow_stolen_face"]
            ),
            pinnacle_phase!(
                "The Hollowforged Choir",
                "slippery",
                ["hollow_many_voices", "hollow_silence_between"],
                "The Choir Below the Choir"
            ),
        ],
    },
    PinnacleBossDefinition {
        boss_id: "first_digger",
        name: "The First Digger",
        persona: "gaunt, manic, the one who never came back up",
        ascii_art: "<the first digger>",
        phases: [
            pinnacle_phase!(
                "The First Digger",
                "glass_cannon",
                ["digger_pickaxe_duel", "digger_mirror_swing"]
            ),
            pinnacle_phase!(
                "The Digger Unbound",
                "slippery",
                ["digger_phasing", "digger_faultline_step"]
            ),
            pinnacle_phase!(
                "The Digger Eternal",
                "glass_cannon",
                ["digger_tunnel_collapse", "digger_last_excavation"],
                "The First Hole"
            ),
        ],
    },
    PinnacleBossDefinition {
        boss_id: "bastion_below",
        name: "The Bastion Below",
        persona: "a patient fortress commander guarding roads that should not meet",
        ascii_art: "<the bastion below>",
        phases: [
            pinnacle_phase!(
                "The Three Roads",
                "tank",
                ["bastion_three_roads", "bastion_hidden_route"]
            ),
            pinnacle_phase!(
                "The Lower Keep",
                "bruiser",
                ["bastion_fortify", "bastion_counter_siege"]
            ),
            pinnacle_phase!(
                "The Last Standard",
                "glass_cannon",
                ["bastion_throne_race", "bastion_last_reserve"],
                "The Road Behind the Wall"
            ),
        ],
    },
    PinnacleBossDefinition {
        boss_id: "pale_surveyor",
        name: "The Pale Surveyor",
        persona: "an arena architect who measures the living before redrawing the floor",
        ascii_art: "<the pale surveyor>",
        phases: [
            pinnacle_phase!(
                "The Measured Ground",
                "slippery",
                ["surveyor_marked_ground", "surveyor_false_landmark"]
            ),
            pinnacle_phase!(
                "The Folded Arena",
                "tank",
                ["surveyor_memory_echo", "surveyor_folded_arena"]
            ),
            pinnacle_phase!(
                "The Unwritten End",
                "glass_cannon",
                ["surveyor_corruption_ring", "surveyor_unwritten_end"],
                "The Map That Maps You"
            ),
        ],
    },
    PinnacleBossDefinition {
        boss_id: "lantern_engine",
        name: "The Lantern Engine",
        persona: "a magitech furnace that mistakes heat for a living will",
        ascii_art: "<the lantern engine>",
        phases: [
            pinnacle_phase!(
                "The Heat Chamber",
                "bruiser",
                ["lantern_heat_cycle", "lantern_venting_protocol"]
            ),
            pinnacle_phase!(
                "The Gearstorm",
                "slippery",
                ["lantern_overclock", "lantern_gearstorm"]
            ),
            pinnacle_phase!(
                "The Infinite Circuit",
                "glass_cannon",
                ["lantern_critical_mass", "lantern_infinite_feedback"],
                "The Unlit Lamp"
            ),
        ],
    },
];

pub const PINNACLE_POOL_IDS: [&str; 6] = [
    "forgotten_king",
    "hollowforged",
    "first_digger",
    "bastion_below",
    "pale_surveyor",
    "lantern_engine",
];

#[must_use]
pub fn pinnacle_by_id(boss_id: &str) -> Option<&'static PinnacleBossDefinition> {
    PINNACLE_BOSSES.iter().find(|boss| boss.boss_id == boss_id)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegularPhaseDefinition {
    pub boss_id: &'static str,
    pub phase: u8,
    pub depth: i32,
    pub name: &'static str,
    pub title: &'static str,
    pub dialogue: [&'static str; 3],
    pub win_odds_penalty: f64,
}

const PHASE_DIALOGUE: [&str; 3] = [
    "The chamber changes shape.",
    "The boss returns with a new answer.",
    "The next phase begins.",
];

macro_rules! regular_phase {
    ($id:literal, $phase:literal, $depth:literal, $name:literal, $title:literal, $penalty:literal) => {
        RegularPhaseDefinition {
            boss_id: $id,
            phase: $phase,
            depth: $depth,
            name: $name,
            title: $title,
            dialogue: PHASE_DIALOGUE,
            win_odds_penalty: $penalty,
        }
    };
}

pub const PHASE_TWO: [RegularPhaseDefinition; 33] = [
    regular_phase!(
        "grothak",
        2,
        25,
        "Grothak the Undying",
        "Skeletal Wrath",
        -0.10
    ),
    regular_phase!(
        "pudge",
        2,
        25,
        "The Butcher, Unstitched",
        "The Seams Come Apart",
        -0.10
    ),
    regular_phase!(
        "ogre_magi",
        2,
        25,
        "The Twin-Skulled, Of One Mind",
        "Both Heads Agree",
        -0.10
    ),
    regular_phase!(
        "crystalia",
        2,
        50,
        "Crystalia Shattered",
        "The Thousand Reflections",
        -0.10
    ),
    regular_phase!(
        "crystal_maiden",
        2,
        50,
        "The Frostbinder, Thawless",
        "The Mana Came Back",
        -0.10
    ),
    regular_phase!(
        "tusk",
        2,
        50,
        "The Ice Warlord, Off the Leash",
        "He Found More Snow",
        -0.10
    ),
    regular_phase!(
        "magmus_rex",
        2,
        75,
        "Magmus Unbound",
        "The Living Eruption",
        -0.10
    ),
    regular_phase!(
        "doom",
        2,
        75,
        "The Deathbringer, Unsealed",
        "The End Stops Waiting",
        -0.10
    ),
    regular_phase!(
        "lina",
        2,
        75,
        "The Scorchwitch, Overheated",
        "She Stopped Rationing",
        -0.10
    ),
    regular_phase!(
        "void_warden",
        2,
        100,
        "The Void Unraveled",
        "What Lies Beyond Nothing",
        -0.12
    ),
    regular_phase!(
        "spectre",
        2,
        100,
        "The Dread Shade, Manifest",
        "The Wound Opens",
        -0.12
    ),
    regular_phase!(
        "void_spirit",
        2,
        100,
        "The Astral Echo, Converged",
        "All Seven Tunnels At Once",
        -0.12
    ),
    regular_phase!(
        "sporeling_sovereign",
        2,
        150,
        "The Sporeling Collective",
        "We Are Legion",
        -0.12
    ),
    regular_phase!(
        "aegis_warden",
        2,
        150,
        "The Aegis Warden, Returned",
        "The Receipt Comes Due",
        -0.12
    ),
    regular_phase!(
        "broodmother",
        2,
        150,
        "The Nestmother, Hatching",
        "They Woke Up Hungry",
        -0.12
    ),
    regular_phase!(
        "treant_protector",
        2,
        150,
        "The Elder Grove, Deeper Rooted",
        "The Rings Close In",
        -0.12
    ),
    regular_phase!(
        "chronofrost",
        2,
        200,
        "Chronofrost Paradox",
        "The Time That Bites Back",
        -0.15
    ),
    regular_phase!(
        "faceless_void",
        2,
        200,
        "The Timeless One, Unbound",
        "The Cooldown Ends",
        -0.15
    ),
    regular_phase!(
        "heartspire",
        2,
        200,
        "The Heartspire, Quickening",
        "The Climb Beats Faster",
        -0.15
    ),
    regular_phase!(
        "weaver",
        2,
        200,
        "The Skitterwing, Unpicking",
        "One Thread Already Pulled",
        -0.15
    ),
    regular_phase!(
        "nameless_depth",
        2,
        275,
        "The Name Reclaimed",
        "[DATA EXPUNGED]",
        -0.15
    ),
    regular_phase!(
        "emberwright",
        2,
        275,
        "The Emberwright, White-Hot",
        "The Forge Takes a Deeper Breath",
        -0.15
    ),
    regular_phase!(
        "oracle",
        2,
        275,
        "The Blindfolded Seer, Unblinded",
        "The Coin Lands Wrong",
        -0.15
    ),
    regular_phase!(
        "terrorblade",
        2,
        275,
        "The Sundered Prince, Reflected",
        "The Mirror Steps Out",
        -0.15
    ),
    regular_phase!(
        "underlord",
        2,
        275,
        "The Pit Lord, Descended",
        "The Pit Comes With Him",
        -0.15
    ),
    regular_phase!(
        "cairn_general",
        2,
        150,
        "The Cairn General, Reformed",
        "The Fallen Close Ranks",
        -0.12
    ),
    regular_phase!(
        "brass_croupier",
        2,
        200,
        "The Brass Croupier, Re-Dealt",
        "The House Opens Another Table",
        -0.15
    ),
    regular_phase!(
        "saltveil_commodore",
        2,
        275,
        "The Saltveil Commodore, Broadside",
        "All Guns Below",
        -0.15
    ),
    regular_phase!(
        "blightcoil",
        2,
        150,
        "The Blightcoil, Unbudding",
        "It Was Only Half a Plant",
        -0.12
    ),
    regular_phase!(
        "rimebound_king",
        2,
        200,
        "The Rimebound King, Unthroned",
        "The Crown Comes Off Hard",
        -0.13
    ),
    regular_phase!(
        "spineback",
        2,
        275,
        "The Spineback, Shedding",
        "It Is Not Done Growing",
        -0.13
    ),
    regular_phase!(
        "xalatath",
        2,
        150,
        "The Whispering Edge, Unsheathed",
        "Now Read It Backwards",
        -0.13
    ),
    regular_phase!(
        "lilith",
        2,
        200,
        "The Red Mother, Unveiled",
        "Hatred Wears No Mask Here",
        -0.13
    ),
];

pub const PHASE_THREE: [RegularPhaseDefinition; 24] = [
    regular_phase!(
        "void_warden",
        3,
        100,
        "The Void Itself",
        "There Was Never Anything Here",
        -0.15
    ),
    regular_phase!(
        "spectre",
        3,
        100,
        "The Wound Itself",
        "It Was Never a Blade",
        -0.15
    ),
    regular_phase!(
        "void_spirit",
        3,
        100,
        "Every Dimension At Once",
        "There Is No Sideways Left",
        -0.15
    ),
    regular_phase!(
        "sporeling_sovereign",
        3,
        150,
        "The Hivemind Awoken",
        "Every Spore Speaks At Once",
        -0.18
    ),
    regular_phase!(
        "aegis_warden",
        3,
        150,
        "The Second Life",
        "The Shield Spends Itself",
        -0.18
    ),
    regular_phase!(
        "broodmother",
        3,
        150,
        "Nine Hundred At Once",
        "The Whole Brood Answers",
        -0.18
    ),
    regular_phase!(
        "treant_protector",
        3,
        150,
        "The Whole Forest",
        "Every Root Was Listening",
        -0.18
    ),
    regular_phase!(
        "chronofrost",
        3,
        200,
        "Chronofrost Rewound",
        "The Loop That Forgot Itself",
        -0.20
    ),
    regular_phase!(
        "faceless_void",
        3,
        200,
        "The Moment That Never Ends",
        "The Chronosphere Closes",
        -0.20
    ),
    regular_phase!(
        "heartspire",
        3,
        200,
        "The Whole Climb",
        "Every Floor Answers At Once",
        -0.20
    ),
    regular_phase!(
        "weaver",
        3,
        200,
        "The Thread Pulled Whole",
        "Nothing Was Ever Woven",
        -0.20
    ),
    regular_phase!(
        "nameless_depth",
        3,
        275,
        "The Final Erasure",
        "[REDACTED] [REDACTED] [REDACTED]",
        -0.20
    ),
    regular_phase!(
        "emberwright",
        3,
        275,
        "The Forge Itself",
        "Every Spark Grew Teeth",
        -0.20
    ),
    regular_phase!(
        "oracle",
        3,
        275,
        "The Outcome Already Chosen",
        "It Was Decided Before You Dug",
        -0.20
    ),
    regular_phase!(
        "terrorblade",
        3,
        275,
        "Both Halves At Once",
        "The Sundering Runs Both Ways",
        -0.20
    ),
    regular_phase!(
        "underlord",
        3,
        275,
        "The Pit Itself",
        "There Was Never a Floor",
        -0.20
    ),
    regular_phase!(
        "cairn_general",
        3,
        150,
        "The Buried Host",
        "Every Stone Takes the Order",
        -0.18
    ),
    regular_phase!(
        "brass_croupier",
        3,
        200,
        "The Hungry Engine",
        "The House Was Inside the Machine",
        -0.20
    ),
    regular_phase!(
        "saltveil_commodore",
        3,
        275,
        "The Drowned Fleet",
        "Every Bell Answers",
        -0.20
    ),
    regular_phase!(
        "blightcoil",
        3,
        150,
        "The Whole Garden",
        "Every Spore Was Listening",
        -0.18
    ),
    regular_phase!(
        "rimebound_king",
        3,
        200,
        "The Long Winter",
        "Nothing Was Ever Warm Here",
        -0.20
    ),
    regular_phase!(
        "spineback",
        3,
        275,
        "The Spineback, Endless",
        "It Grows Faster Than You Cut",
        -0.20
    ),
    regular_phase!(
        "xalatath",
        3,
        150,
        "The Last Page",
        "It Was Always About You",
        -0.20
    ),
    regular_phase!(
        "lilith",
        3,
        200,
        "The First Hatred",
        "She Was Here Before the Dark",
        -0.20
    ),
];

#[must_use]
pub fn regular_phase(boss_id: &str, phase: u8) -> Option<&'static RegularPhaseDefinition> {
    match phase {
        2 => PHASE_TWO
            .iter()
            .find(|definition| definition.boss_id == boss_id),
        3 => PHASE_THREE
            .iter()
            .find(|definition| definition.boss_id == boss_id),
        _ => None,
    }
}

#[must_use]
pub fn default_regular_phase(depth: i32, phase: u8) -> Option<&'static RegularPhaseDefinition> {
    match phase {
        2 => PHASE_TWO
            .iter()
            .find(|definition| definition.depth == depth),
        3 => PHASE_THREE
            .iter()
            .find(|definition| definition.depth == depth),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialogueSlots {
    pub first_meet: &'static [&'static str],
    pub after_defeat: &'static [&'static str],
    pub after_retreat: &'static [&'static str],
    pub after_close_win: &'static [&'static str],
    pub after_scout: &'static [&'static str],
}

const FIRST_MEET: &[&str] = &[
    "The boss blocks the shaft.",
    "The dark answers with a challenge.",
];
const AFTER_DEFEAT: &[&str] = &["You return. The boss remembers."];
const AFTER_RETREAT: &[&str] = &["The climb back did not end the fight."];
const AFTER_CLOSE_WIN: &[&str] = &["The margin was narrow. The path remains."];
const AFTER_SCOUT: &[&str] = &["You measured the danger. It measured you."];
const DIALOGUE_SLOTS: DialogueSlots = DialogueSlots {
    first_meet: FIRST_MEET,
    after_defeat: AFTER_DEFEAT,
    after_retreat: AFTER_RETREAT,
    after_close_win: AFTER_CLOSE_WIN,
    after_scout: AFTER_SCOUT,
};

#[must_use]
pub fn dialogue_slots(boss_id: &str) -> Option<DialogueSlots> {
    let is_prestige_four = matches!(boss_id, "blightcoil" | "rimebound_king" | "spineback");
    ((boss_by_id(boss_id).is_some() && !is_prestige_four) || pinnacle_by_id(boss_id).is_some())
        .then_some(DIALOGUE_SLOTS)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BarkContext<'a> {
    pub streak_days: i32,
    pub depth: i32,
    pub prestige_level: i32,
    pub killed_boss_name: Option<&'a str>,
}

#[must_use]
pub fn render_boss_bark(template: &str, context: BarkContext<'_>) -> String {
    template
        .replace("{streak}", &context.streak_days.max(1).to_string())
        .replace("{depth}", &context.depth.to_string())
        .replace("{prestige}", &context.prestige_level.to_string())
        .replace(
            "{killed_boss_name}",
            context.killed_boss_name.unwrap_or("the early dark"),
        )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BossStatus {
    #[default]
    Active,
    PhaseOneDefeated,
    PhaseTwoDefeated,
    Defeated,
}

impl BossStatus {
    #[must_use]
    pub const fn unfinished(self) -> bool {
        !matches!(self, Self::Defeated)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BossProgressEntry {
    pub status: BossStatus,
    pub boss_id: Option<String>,
    pub hp_remaining: Option<i32>,
    pub hp_max: Option<i32>,
    pub last_engaged_at: Option<i64>,
    pub last_outcome: Option<String>,
    pub first_meet_seen: bool,
    /// One-shot inter-phase event applied and cleared at the next fight.
    pub pending_phase_event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BossProgressValue {
    Legacy(BossStatus),
    Entry(BossProgressEntry),
}

impl BossProgressValue {
    #[must_use]
    pub const fn status(&self) -> BossStatus {
        match self {
            Self::Legacy(status) => *status,
            Self::Entry(entry) => entry.status,
        }
    }
}

pub type BossProgress = BTreeMap<String, BossProgressValue>;

#[must_use]
pub fn resolve_persisted_boss_hp(
    boss_progress: &BossProgress,
    key: &str,
    fresh_hp: i32,
    now: i64,
) -> (i32, i32) {
    let Some(BossProgressValue::Entry(entry)) = boss_progress.get(key) else {
        return (fresh_hp, fresh_hp);
    };
    let (Some(hp_remaining), Some(hp_max)) = (entry.hp_remaining, entry.hp_max) else {
        return (fresh_hp, fresh_hp);
    };
    let hp_max = hp_max.max(1);
    let mut hp_remaining = hp_remaining.clamp(1, hp_max);
    if let Some(last_engaged_at) = entry.last_engaged_at {
        let complete_days = (now - last_engaged_at).max(0) / 86_400;
        let regenerated = complete_days.saturating_mul(i64::from(BOSS_HP_REGEN_PER_24_HOURS));
        hp_remaining = i64::from(hp_remaining)
            .saturating_add(regenerated)
            .min(i64::from(hp_max)) as i32;
    }
    (hp_remaining.max(1), hp_max)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersistBossHp<'a> {
    pub key: &'a str,
    pub boss_id: &'a str,
    pub ending_hp: i32,
    pub hp_max: i32,
    pub won: bool,
    pub outcome: &'a str,
    pub now: i64,
}

pub fn persist_boss_hp_after_fight(boss_progress: &mut BossProgress, update: PersistBossHp<'_>) {
    let existing = boss_progress.remove(update.key);
    let mut entry = match existing {
        Some(BossProgressValue::Entry(entry)) => entry,
        Some(BossProgressValue::Legacy(status)) => BossProgressEntry {
            status,
            ..BossProgressEntry::default()
        },
        None => BossProgressEntry::default(),
    };
    entry.hp_remaining = Some(update.ending_hp.max(0));
    entry.hp_max = Some(update.hp_max);
    entry.last_engaged_at = Some(update.now);
    entry.last_outcome = Some(update.outcome.to_owned());
    entry.first_meet_seen = true;
    if entry.boss_id.is_none() && !update.boss_id.is_empty() {
        entry.boss_id = Some(update.boss_id.to_owned());
    }
    boss_progress.insert(update.key.to_owned(), BossProgressValue::Entry(entry));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BossEcho {
    pub boss_id: String,
    pub depth: i32,
    pub killer_discord_id: i64,
    pub weakened_until: i64,
}

pub trait BossEchoStore {
    fn record_boss_echo(
        &mut self,
        guild_id: Option<i64>,
        boss_id: &str,
        depth: i32,
        killer_discord_id: i64,
        window_seconds: i64,
        now: i64,
    );

    fn active_boss_echo(&self, guild_id: Option<i64>, boss_id: &str, now: i64)
    -> Option<&BossEcho>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InMemoryBossStore {
    echoes: BTreeMap<(i64, String), BossEcho>,
    inventory: BTreeMap<(i64, i64), BTreeSet<String>>,
    artifacts: BTreeMap<(i64, i64), Vec<ArtifactRecord>>,
    next_artifact_id: i64,
}

const fn normalized_guild_id(guild_id: Option<i64>) -> i64 {
    match guild_id {
        Some(guild_id) => guild_id,
        None => 0,
    }
}

impl BossEchoStore for InMemoryBossStore {
    fn record_boss_echo(
        &mut self,
        guild_id: Option<i64>,
        boss_id: &str,
        depth: i32,
        killer_discord_id: i64,
        window_seconds: i64,
        now: i64,
    ) {
        let echo = BossEcho {
            boss_id: boss_id.to_owned(),
            depth,
            killer_discord_id,
            weakened_until: now.saturating_add(window_seconds),
        };
        self.echoes
            .insert((normalized_guild_id(guild_id), boss_id.to_owned()), echo);
    }

    fn active_boss_echo(
        &self,
        guild_id: Option<i64>,
        boss_id: &str,
        now: i64,
    ) -> Option<&BossEcho> {
        self.echoes
            .get(&(normalized_guild_id(guild_id), boss_id.to_owned()))
            .filter(|echo| echo.weakened_until > now)
    }
}

impl InMemoryBossStore {
    pub fn add_inventory_item(&mut self, discord_id: i64, guild_id: i64, item_type: &str) {
        self.inventory
            .entry((discord_id, guild_id))
            .or_default()
            .insert(item_type.to_owned());
    }

    #[must_use]
    pub fn has_scout_lantern(&self, discord_id: i64, guild_id: i64) -> bool {
        self.inventory
            .get(&(discord_id, guild_id))
            .is_some_and(|items| items.contains("lantern") || items.contains("great_lantern"))
    }

    pub fn settle_relic(&mut self, discord_id: i64, guild_id: i64, relic: &PinnacleRelic) -> i64 {
        self.next_artifact_id += 1;
        let row = ArtifactRecord {
            id: self.next_artifact_id,
            artifact_id: relic.artifact_id.clone(),
        };
        self.artifacts
            .entry((discord_id, guild_id))
            .or_default()
            .push(row);
        self.next_artifact_id
    }

    #[must_use]
    pub fn artifacts(&self, discord_id: i64, guild_id: i64) -> &[ArtifactRecord] {
        self.artifacts
            .get(&(discord_id, guild_id))
            .map_or(&[], Vec::as_slice)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BossRunState {
    pub depth: i32,
    pub prestige_level: i32,
    pub boss_progress: BossProgress,
    pub pinnacle_boss_id: Option<String>,
    pub pinnacle_phase: u8,
    pub active_duel: Option<ActiveBossDuel>,
}

impl BossRunState {
    #[must_use]
    pub fn with_all_tiers_defeated(depth: i32) -> Self {
        let mut state = Self {
            depth,
            ..Self::default()
        };
        for boundary in BOSS_BOUNDARIES {
            state.boss_progress.insert(
                boundary.to_string(),
                BossProgressValue::Legacy(BossStatus::Defeated),
            );
        }
        state
    }
}

fn progress_status(progress: &BossProgress, boundary: i32) -> Option<BossStatus> {
    progress
        .get(&boundary.to_string())
        .map(BossProgressValue::status)
}

#[must_use]
pub fn all_regular_bosses_defeated(progress: &BossProgress) -> bool {
    BOSS_BOUNDARIES
        .iter()
        .all(|boundary| progress_status(progress, *boundary) == Some(BossStatus::Defeated))
}

#[must_use]
pub fn next_boss_boundary(progress: &BossProgress) -> Option<i32> {
    for boundary in BOSS_BOUNDARIES {
        if progress_status(progress, boundary).is_some_and(BossStatus::unfinished) {
            return Some(boundary);
        }
    }
    let pinnacle_status = progress_status(progress, PINNACLE_DEPTH);
    if all_regular_bosses_defeated(progress) && pinnacle_status.is_none_or(BossStatus::unfinished) {
        return Some(PINNACLE_DEPTH);
    }
    None
}

#[must_use]
pub fn at_boss_boundary(depth: i32, progress: &BossProgress) -> Option<i32> {
    for boundary in BOSS_BOUNDARIES {
        if depth >= boundary - 1
            && progress_status(progress, boundary).is_some_and(BossStatus::unfinished)
        {
            return Some(boundary);
        }
    }
    if (depth >= PINNACLE_DEPTH - 1 || depth >= PINNACLE_REPROC_DEPTH)
        && all_regular_bosses_defeated(progress)
        && progress_status(progress, PINNACLE_DEPTH).is_none_or(BossStatus::unfinished)
    {
        return Some(PINNACLE_DEPTH);
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BossLockError {
    EmptyPool(i32),
    PreferredBossUnavailable(String),
}

pub fn lock_regular_boss(
    state: &mut BossRunState,
    depth: i32,
    preferred_boss_id: Option<&str>,
) -> Result<&'static BossDefinition, BossLockError> {
    let key = depth.to_string();
    if let Some(BossProgressValue::Entry(entry)) = state.boss_progress.get(&key)
        && let Some(existing) = entry.boss_id.as_deref().and_then(boss_by_id)
    {
        return Ok(existing);
    }
    let pool = boss_pool_for_tier(depth, state.prestige_level);
    let selected = match preferred_boss_id {
        Some(preferred) => pool
            .iter()
            .copied()
            .find(|boss| boss.boss_id == preferred)
            .ok_or_else(|| BossLockError::PreferredBossUnavailable(preferred.to_owned()))?,
        None => *pool.first().ok_or(BossLockError::EmptyPool(depth))?,
    };
    let existing = state.boss_progress.remove(&key);
    let mut entry = match existing {
        Some(BossProgressValue::Entry(entry)) => entry,
        Some(BossProgressValue::Legacy(status)) => BossProgressEntry {
            status,
            ..BossProgressEntry::default()
        },
        None => BossProgressEntry::default(),
    };
    entry.boss_id = Some(selected.boss_id.to_owned());
    state
        .boss_progress
        .insert(key, BossProgressValue::Entry(entry));
    Ok(selected)
}

pub fn lock_pinnacle_boss(
    state: &mut BossRunState,
    preferred_boss_id: Option<&str>,
) -> Result<&'static str, BossLockError> {
    if let Some(existing) = state
        .pinnacle_boss_id
        .as_deref()
        .filter(|boss_id| pinnacle_by_id(boss_id).is_some())
    {
        return Ok(PINNACLE_POOL_IDS
            .iter()
            .copied()
            .find(|boss_id| *boss_id == existing)
            .expect("validated pinnacle id belongs to the static pool"));
    }
    let selected = match preferred_boss_id {
        Some(preferred) if pinnacle_by_id(preferred).is_some() => preferred,
        Some(preferred) => {
            return Err(BossLockError::PreferredBossUnavailable(
                preferred.to_owned(),
            ));
        }
        None => PINNACLE_POOL_IDS[0],
    };
    state.pinnacle_boss_id = Some(selected.to_owned());
    state.pinnacle_phase = 1;
    Ok(PINNACLE_POOL_IDS
        .iter()
        .copied()
        .find(|boss_id| *boss_id == selected)
        .expect("validated pinnacle id belongs to the static pool"))
}

pub const PINNACLE_FORESHADOW_LINES: [&str; 4] = [
    "Something deeper shifts when the last echo fades.",
    "The shaft below breathes once.",
    "Your lantern throws a shadow that does not belong to you.",
    "The stone beneath the final tier sounds hollow.",
];

#[must_use]
pub fn pinnacle_foreshadow_line(state: &BossRunState) -> Option<&'static str> {
    if !all_regular_bosses_defeated(&state.boss_progress)
        || progress_status(&state.boss_progress, PINNACLE_DEPTH) == Some(BossStatus::Defeated)
    {
        return None;
    }
    Some(PINNACLE_FORESHADOW_LINES[0])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrestigeCheck {
    pub can_prestige: bool,
    pub reason: Option<&'static str>,
}

#[must_use]
pub fn can_prestige(state: &BossRunState, max_prestige: i32) -> PrestigeCheck {
    if !all_regular_bosses_defeated(&state.boss_progress) {
        return PrestigeCheck {
            can_prestige: false,
            reason: Some("Bosses remain."),
        };
    }
    if progress_status(&state.boss_progress, PINNACLE_DEPTH) != Some(BossStatus::Defeated) {
        return PrestigeCheck {
            can_prestige: false,
            reason: Some("Something stirs deeper still — descend further."),
        };
    }
    if state.prestige_level >= max_prestige {
        return PrestigeCheck {
            can_prestige: false,
            reason: Some("Already at max prestige."),
        };
    }
    PrestigeCheck {
        can_prestige: true,
        reason: None,
    }
}

pub fn prestige_reset(state: &mut BossRunState, max_prestige: i32) -> Result<(), &'static str> {
    if !can_prestige(state, max_prestige).can_prestige {
        return Err("Cannot prestige.");
    }
    state.depth = 0;
    state.prestige_level += 1;
    state.boss_progress.clear();
    for boundary in BOSS_BOUNDARIES {
        state.boss_progress.insert(
            boundary.to_string(),
            BossProgressValue::Legacy(BossStatus::Active),
        );
    }
    state.pinnacle_boss_id = None;
    state.pinnacle_phase = 0;
    state.active_duel = None;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelicStat {
    pub id: &'static str,
    pub label: &'static str,
    pub title: &'static str,
}

pub const PINNACLE_RELIC_STATS: [RelicStat; 15] = [
    RelicStat {
        id: "hp_plus_1",
        label: "Tougher skin",
        title: "Scales",
    },
    RelicStat {
        id: "hit_plus_002",
        label: "Steadier hands",
        title: "Precision",
    },
    RelicStat {
        id: "dmg_plus_per_100",
        label: "Stronger with depth",
        title: "Depths",
    },
    RelicStat {
        id: "boss_hit_minus",
        label: "Bosses miss more often",
        title: "Feints",
    },
    RelicStat {
        id: "boss_hp_minus_10",
        label: "Bosses arrive weakened",
        title: "Withering",
    },
    RelicStat {
        id: "boss_payout_5",
        label: "Bosses pay better",
        title: "Tribute",
    },
    RelicStat {
        id: "jc_plus_5",
        label: "Richer veins",
        title: "Riches",
    },
    RelicStat {
        id: "cave_in_minus_5",
        label: "Steadier ceilings",
        title: "Bedrock",
    },
    RelicStat {
        id: "lum_refill_2",
        label: "Brighter mornings",
        title: "Dawn",
    },
    RelicStat {
        id: "durability_minus",
        label: "Gear lasts longer",
        title: "Patience",
    },
    RelicStat {
        id: "inventory_plus_1",
        label: "Roomier pack",
        title: "Satchels",
    },
    RelicStat {
        id: "streak_immunity",
        label: "Streak persists, once per delve",
        title: "Persistence",
    },
    RelicStat {
        id: "extra_relic_slot",
        label: "Another relic finds room",
        title: "Communion",
    },
    RelicStat {
        id: "scout_free",
        label: "Scouting comes cheap",
        title: "Scouting",
    },
    RelicStat {
        id: "cheer_buff",
        label: "Cheers ring louder",
        title: "Acclaim",
    },
];

pub const PINNACLE_RELIC_SUFFIX_POOL: [&str; 12] = [
    "Echoes",
    "Hunger",
    "Patience",
    "Ruin",
    "Bloom",
    "Silence",
    "Endings",
    "First Light",
    "Last Breath",
    "Hollow",
    "Persistence",
    "Forgotten Things",
];

pub const PHASE_TRANSITION_EVENT_IDS: [&str; 14] = [
    "cave_in",
    "fissure",
    "glowburst",
    "cold_snap",
    "spore_cloud",
    "void_pull",
    "echoing_drip",
    "iron_tang",
    "shifting_walls",
    "hollow_tone",
    "crystal_bloom",
    "tremor",
    "ash_fall",
    "quiet",
];

#[must_use]
pub const fn pinnacle_relic_base_name(pinnacle_id: &str) -> Option<&'static str> {
    match pinnacle_id.as_bytes() {
        b"forgotten_king" => Some("Crown"),
        b"hollowforged" => Some("Heart"),
        b"first_digger" => Some("Pickaxe"),
        b"bastion_below" => Some("Standard"),
        b"pale_surveyor" => Some("Compass"),
        b"lantern_engine" => Some("Core"),
        _ => None,
    }
}

#[must_use]
pub fn pinnacle_suffix_from_stats(stat_ids: &[&str]) -> String {
    let mut titles: Vec<&str> = stat_ids
        .iter()
        .filter_map(|stat_id| {
            PINNACLE_RELIC_STATS
                .iter()
                .find(|stat| stat.id == *stat_id)
                .map(|stat| stat.title)
        })
        .collect();
    titles.sort_by_key(|title| title.to_ascii_lowercase());
    titles.dedup();
    titles.join(" and ")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnacleRelic {
    pub name: String,
    pub stats: Vec<&'static str>,
    pub stat_ids: Vec<&'static str>,
    pub prestige_at_drop: i32,
    pub artifact_id: String,
}

#[must_use]
pub fn roll_pinnacle_relic(
    prestige_level: i32,
    pinnacle_id: &str,
    first_stat_index: usize,
    second_stat_index: usize,
) -> Option<PinnacleRelic> {
    let base = pinnacle_relic_base_name(pinnacle_id)?;
    let first = PINNACLE_RELIC_STATS[first_stat_index % PINNACLE_RELIC_STATS.len()];
    let mut second = PINNACLE_RELIC_STATS[second_stat_index % PINNACLE_RELIC_STATS.len()];
    if first.id == second.id {
        second = PINNACLE_RELIC_STATS[(second_stat_index + 1) % PINNACLE_RELIC_STATS.len()];
    }
    let stat_ids = vec![first.id, second.id];
    let suffix = pinnacle_suffix_from_stats(&stat_ids);
    Some(PinnacleRelic {
        name: format!("{base} of {suffix}"),
        stats: vec![first.label, second.label],
        stat_ids,
        prestige_at_drop: prestige_level,
        artifact_id: format!("pinnacle:{base}:{suffix}:{}:{}", first.id, second.id),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub id: i64,
    pub artifact_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnacleInfo {
    pub is_pinnacle: bool,
    pub phase: u8,
    pub phase_total: u8,
    pub name: &'static str,
}

#[must_use]
pub fn build_pinnacle_info(state: &BossRunState) -> Option<PinnacleInfo> {
    let boss = pinnacle_by_id(state.pinnacle_boss_id.as_deref()?)?;
    let phase = state.pinnacle_phase.clamp(1, 3);
    Some(PinnacleInfo {
        is_pinnacle: true,
        phase,
        phase_total: 3,
        name: boss.phases[usize::from(phase - 1)].title,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveBossDuel {
    pub boss_id: String,
    pub mechanic_id: String,
    pub phase: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinnacleFightDirective {
    WinWithoutMechanic,
    LoseWithoutMechanic {
        ending_boss_hp: i32,
        boss_hp_max: i32,
    },
    PauseAtMechanic {
        mechanic_index: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnacleFightResult {
    pub success: bool,
    pub won: Option<bool>,
    pub boss_id: String,
    pub boundary: i32,
    pub phase: u8,
    pub is_pinnacle: bool,
    pub phase_two_incoming: bool,
    pub pending_prompt: bool,
    pub mechanic_id: Option<String>,
    pub observed_bleed_rounds: Vec<u8>,
}

fn fight_result(boss_id: &str, phase: u8) -> PinnacleFightResult {
    PinnacleFightResult {
        success: true,
        won: None,
        boss_id: boss_id.to_owned(),
        boundary: PINNACLE_DEPTH,
        phase,
        is_pinnacle: true,
        phase_two_incoming: false,
        pending_prompt: false,
        mechanic_id: None,
        observed_bleed_rounds: Vec::new(),
    }
}

pub fn start_pinnacle_fight(
    state: &mut BossRunState,
    directive: PinnacleFightDirective,
) -> Result<PinnacleFightResult, &'static str> {
    let boss_id = state
        .pinnacle_boss_id
        .clone()
        .ok_or("No pinnacle boss is locked.")?;
    let boss = pinnacle_by_id(&boss_id).ok_or("Unknown pinnacle boss.")?;
    let phase = state.pinnacle_phase.clamp(1, 3);
    let mut result = fight_result(&boss_id, phase);
    match directive {
        PinnacleFightDirective::WinWithoutMechanic => {
            result.won = Some(true);
            if phase == 1 {
                state.pinnacle_phase = 2;
                result.phase_two_incoming = true;
                state.boss_progress.insert(
                    PINNACLE_DEPTH.to_string(),
                    BossProgressValue::Entry(BossProgressEntry {
                        status: BossStatus::PhaseOneDefeated,
                        boss_id: Some(boss_id),
                        ..BossProgressEntry::default()
                    }),
                );
            }
        }
        PinnacleFightDirective::LoseWithoutMechanic {
            ending_boss_hp,
            boss_hp_max,
        } => {
            result.won = Some(false);
            persist_boss_hp_after_fight(
                &mut state.boss_progress,
                PersistBossHp {
                    key: &format!("{PINNACLE_DEPTH}:{phase}"),
                    boss_id: &boss_id,
                    ending_hp: ending_boss_hp,
                    hp_max: boss_hp_max,
                    won: false,
                    outcome: "loss",
                    now: 1_000_000,
                },
            );
        }
        PinnacleFightDirective::PauseAtMechanic { mechanic_index } => {
            let mechanic_id = boss.phases[usize::from(phase - 1)].mechanic_pool[mechanic_index % 2];
            state.active_duel = Some(ActiveBossDuel {
                boss_id: boss_id.clone(),
                mechanic_id: mechanic_id.to_owned(),
                phase,
            });
            result.pending_prompt = true;
            result.mechanic_id = Some(mechanic_id.to_owned());
        }
    }
    Ok(result)
}

pub fn resume_pinnacle_fight(
    state: &mut BossRunState,
    option_index: usize,
    adverse_outcome: bool,
) -> Result<PinnacleFightResult, &'static str> {
    let active = state.active_duel.take().ok_or("No active boss duel.")?;
    let definition = mechanic(&active.mechanic_id).ok_or("Unknown boss mechanic.")?;
    let selected = definition
        .options
        .get(option_index)
        .ok_or("Invalid mechanic option.")?;
    let outcome = if adverse_outcome {
        selected.outcome_rolls.last()
    } else {
        selected.outcome_rolls.first()
    }
    .ok_or("Mechanic option has no outcome.")?;
    let mut result = fight_result(&active.boss_id, active.phase);
    result.won = Some(true);
    result.mechanic_id = Some(active.mechanic_id);
    if outcome.status_effect == Some(StatusEffect::Bleed) {
        let mut remaining = 3;
        for _ in 0..2 {
            result.observed_bleed_rounds.push(remaining);
            remaining -= 1;
        }
    }
    Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NestedValue {
    Text(String),
    TextList(Vec<String>),
    Object(DictObject),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DictObject(BTreeMap<String, NestedValue>);

impl DictObject {
    #[must_use]
    pub fn new(values: BTreeMap<String, NestedValue>) -> Self {
        Self(values)
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&NestedValue> {
        self.0.get(key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BossFightEmbed {
    pub fields: Vec<EmbedField>,
}

#[must_use]
pub fn build_pinnacle_victory_embed(result: &DictObject) -> BossFightEmbed {
    let mut embed = BossFightEmbed::default();
    let Some(NestedValue::Object(relic)) = result.get("pinnacle_relic") else {
        return embed;
    };
    let Some(NestedValue::Text(name)) = relic.get("name") else {
        return embed;
    };
    let stats = match relic.get("stats") {
        Some(NestedValue::TextList(values)) => values.join("\n"),
        _ => String::new(),
    };
    embed.fields.push(EmbedField {
        name: format!("Relic: {name}"),
        value: stats,
    });
    embed
}

#[cfg(test)]
#[path = "dig_bosses/tests.rs"]
mod tests;
