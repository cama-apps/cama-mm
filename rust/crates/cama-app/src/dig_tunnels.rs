//! Prestige-run and tunnel-lifecycle policy for the side-by-side Rust port.
//!
//! Python remains the live Discord and SQLite implementation. This module keeps
//! the transport-independent state machine behind explicit persistence and
//! entropy ports: guarded prestige/abandon transactions, display-name
//! projections, integer normalization, ascension/corruption/mutation policy,
//! scoring, and hall-of-fame ordering. The in-memory adapter is deliberately a
//! behavioral test double, not a second schema or migration authority.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use cama_domain::dig_economy::scale_positive_dig_jc;

use crate::dig_bosses::{BOSS_BOUNDARIES, BossStatus, PINNACLE_DEPTH};

pub type DiscordId = i64;
pub type GuildId = i64;

pub const MAX_PRESTIGE: i32 = 20;
pub const PRESTIGE_PERK_STACK_CAP: usize = 5;
pub const ABANDON_MINIMUM_DEPTH: i64 = 10;
pub const ABANDON_COOLDOWN_SECONDS: i64 = 86_400;
pub const PRESTIGE_GROSS_JC: i64 = 1_000;

pub const PRESTIGE_PERKS: [&str; 12] = [
    "advance_boost",
    "cave_in_resistance",
    "loot_multiplier",
    "mixed_bonus",
    "deep_sight",
    "veteran_miner",
    "tunnel_mastery",
    "dark_adaptation",
    "the_endless",
    "patient_step",
    "steady_hands",
    "reading_the_stone",
];

const ADVANCE_BOOST_PERK_EFFECTS: &[(&str, f64)] = &[("advance_min_bonus", 1.0)];
const CAVE_IN_RESISTANCE_PERK_EFFECTS: &[(&str, f64)] = &[("cave_in_reduction", 0.05)];
const LOOT_MULTIPLIER_PERK_EFFECTS: &[(&str, f64)] = &[("jc_bonus", 1.0)];
const MIXED_BONUS_PERK_EFFECTS: &[(&str, f64)] = &[
    ("advance_min_bonus", 0.5),
    ("cave_in_reduction", 0.02),
    ("jc_bonus", 0.5),
];
const DEEP_SIGHT_PERK_EFFECTS: &[(&str, f64)] = &[("luminosity_drain_reduction", 0.25)];
const VETERAN_MINER_PERK_EFFECTS: &[(&str, f64)] = &[("risky_success_bonus", 0.05)];
const TUNNEL_MASTERY_PERK_EFFECTS: &[(&str, f64)] = &[("expedition_reward_bonus", 0.50)];
const DARK_ADAPTATION_PERK_EFFECTS: &[(&str, f64)] = &[("dim_cave_in_immunity", 1.0)];
const THE_ENDLESS_PERK_EFFECTS: &[(&str, f64)] = &[("hollow_advance_bonus", 1.0)];
const PATIENT_STEP_PERK_EFFECTS: &[(&str, f64)] = &[("streak_bonus_multiplier", 0.5)];
const STEADY_HANDS_PERK_EFFECTS: &[(&str, f64)] = &[("cave_in_loss_reduction", 0.25)];
const READING_THE_STONE_PERK_EFFECTS: &[(&str, f64)] = &[("event_choice_reveal", 1.0)];

/// Canonical per-pick effects for a prestige perk.
///
/// Unknown persisted IDs deliberately contribute no effects, matching the
/// Python lookup through `PRESTIGE_PERK_VALUES.get(perk, {})`.
#[must_use]
pub fn prestige_perk_effects(perk: &str) -> &'static [(&'static str, f64)] {
    match perk {
        "advance_boost" => ADVANCE_BOOST_PERK_EFFECTS,
        "cave_in_resistance" => CAVE_IN_RESISTANCE_PERK_EFFECTS,
        "loot_multiplier" => LOOT_MULTIPLIER_PERK_EFFECTS,
        "mixed_bonus" => MIXED_BONUS_PERK_EFFECTS,
        "deep_sight" => DEEP_SIGHT_PERK_EFFECTS,
        "veteran_miner" => VETERAN_MINER_PERK_EFFECTS,
        "tunnel_mastery" => TUNNEL_MASTERY_PERK_EFFECTS,
        "dark_adaptation" => DARK_ADAPTATION_PERK_EFFECTS,
        "the_endless" => THE_ENDLESS_PERK_EFFECTS,
        "patient_step" => PATIENT_STEP_PERK_EFFECTS,
        "steady_hands" => STEADY_HANDS_PERK_EFFECTS,
        "reading_the_stone" => READING_THE_STONE_PERK_EFFECTS,
        _ => &[],
    }
}

/// Sum every picked prestige perk additively, including duplicate stacks.
#[must_use]
pub fn aggregate_prestige_perk_effects(perks: &[String]) -> BTreeMap<&'static str, f64> {
    let mut effects = BTreeMap::new();
    for perk in perks {
        for &(key, value) in prestige_perk_effects(perk) {
            *effects.entry(key).or_insert(0.0) += value;
        }
    }
    effects
}

/// Project a non-negative fractional flat perk bonus with Python's
/// `int(value + 0.5)` rule.
#[must_use]
pub fn round_prestige_perk_bonus_half_up(value: f64) -> i64 {
    (value.max(0.0) + 0.5) as i64
}

/// Return whether an optional tunnel owns at least one copy of a perk.
#[must_use]
pub fn owns_prestige_perk(tunnel: Option<&Tunnel>, perk_id: &str) -> bool {
    tunnel.is_some_and(|tunnel| tunnel.prestige_perks.iter().any(|owned| owned == perk_id))
}

pub const READING_STONE_SAFE_HINTS: [&str; 3] = [
    "The walls whisper of patience here.",
    "A familiar rhythm — caution holds today.",
    "Stillness gathers along the safer passage.",
];

pub const READING_STONE_RISKY_HINTS: [&str; 3] = [
    "The stones hum louder beside the bolder path.",
    "Something glints just past the edge of the dark.",
    "An unseen pull tugs you onward.",
];

pub const READING_STONE_DESPERATE_HINTS: [&str; 3] = [
    "Old bones remember reckless feet.",
    "The rock itself seems to dare you forward.",
    "A wild current beckons from the deepest dark.",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadingStoneDirection {
    Safe,
    Risky,
    Desperate,
}

impl ReadingStoneDirection {
    const fn hints(self) -> &'static [&'static str; 3] {
        match self {
            Self::Safe => &READING_STONE_SAFE_HINTS,
            Self::Risky => &READING_STONE_RISKY_HINTS,
            Self::Desperate => &READING_STONE_DESPERATE_HINTS,
        }
    }
}

/// Return whether an authored event contains an economically or mechanically
/// harmful outcome. Both canonical options and the legacy `outcomes` shape
/// are supported because Veteran Miner applies to both catalogs.
#[must_use]
pub fn event_has_risk(event: &serde_json::Value) -> bool {
    let Some(event) = event.as_object() else {
        return false;
    };
    for option_key in ["safe_option", "risky_option", "desperate_option"] {
        let Some(option) = event.get(option_key).and_then(serde_json::Value::as_object) else {
            continue;
        };
        if ["success", "failure"].into_iter().any(|outcome| {
            option
                .get(outcome)
                .and_then(serde_json::Value::as_object)
                .is_some_and(prestige_outcome_has_risk)
        }) {
            return true;
        }
    }
    event
        .get("outcomes")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|outcomes| {
            outcomes
                .values()
                .filter_map(serde_json::Value::as_object)
                .any(prestige_outcome_has_risk)
        })
}

fn prestige_outcome_has_risk(outcome: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["jc", "advance"]
        .into_iter()
        .any(|key| outcome.get(key).is_some_and(prestige_has_negative_number))
        || outcome.get("cave_in").is_some_and(prestige_python_truthy)
        || outcome
            .get("streak_loss")
            .and_then(prestige_json_integer)
            .is_some_and(|loss| loss > 0)
        || outcome.get("curse").is_some_and(prestige_python_truthy)
}

fn prestige_has_negative_number(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => number.as_f64().is_some_and(|number| number < 0.0),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| value.as_f64().is_some_and(|number| number < 0.0)),
        _ => false,
    }
}

fn prestige_json_integer(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn prestige_python_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
    }
}

/// Pick the authored option with the highest JC expected value. Ties retain
/// Python's stable safe/risky/desperate traversal order.
#[must_use]
pub fn reading_the_stone_direction(event: &serde_json::Value) -> Option<ReadingStoneDirection> {
    let event = event.as_object()?;
    let mut best: Option<(ReadingStoneDirection, f64)> = None;
    for (key, direction) in [
        ("safe_option", ReadingStoneDirection::Safe),
        ("risky_option", ReadingStoneDirection::Risky),
        ("desperate_option", ReadingStoneDirection::Desperate),
    ] {
        let Some(option) = event.get(key).and_then(serde_json::Value::as_object) else {
            continue;
        };
        let success_chance = option
            .get("success_chance")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);
        let success = option
            .get("success")
            .and_then(serde_json::Value::as_object)
            .and_then(|outcome| outcome.get("jc"))
            .map_or(0.0, average_prestige_jc);
        let failure = option
            .get("failure")
            .and_then(serde_json::Value::as_object)
            .and_then(|outcome| outcome.get("jc"))
            .map_or(0.0, average_prestige_jc);
        let expected_value = success_chance * success + (1.0 - success_chance) * failure;
        if best.is_none_or(|(_, best_value)| expected_value > best_value) {
            best = Some((direction, expected_value));
        }
    }
    best.map(|(direction, _)| direction)
}

/// Select atmospheric Reading-the-Stone copy after the caller supplies an
/// entropy-derived index. No expected-value number enters the returned copy.
#[must_use]
pub fn reading_the_stone_hint(
    event: &serde_json::Value,
    hint_choice: usize,
) -> Option<&'static str> {
    let hints = reading_the_stone_direction(event)?.hints();
    Some(hints[hint_choice % hints.len()])
}

fn average_prestige_jc(value: &serde_json::Value) -> f64 {
    match value {
        serde_json::Value::Number(value) => value.as_f64().unwrap_or(0.0),
        serde_json::Value::Array(values) if !values.is_empty() => {
            values
                .iter()
                .filter_map(serde_json::Value::as_f64)
                .sum::<f64>()
                / values.len() as f64
        }
        _ => 0.0,
    }
}

pub const PRESTIGE_CROWNS: [&str; 21] = [
    "", "⛏️", "💎", "👑", "💠", "⭐", "🔱", "♾️", "🔥", "🌌", "🖤", "🖤", "🖤", "🖤", "🖤", "🖤",
    "🖤", "🖤", "🖤", "🖤", "🖤",
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EffectValue {
    Number(f64),
    Bool(bool),
}

impl EffectValue {
    #[must_use]
    pub const fn number(self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(value),
            Self::Bool(_) => None,
        }
    }

    #[must_use]
    pub const fn boolean(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            Self::Number(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectSpec {
    pub key: &'static str,
    pub value: EffectValue,
}

const fn number(key: &'static str, value: f64) -> EffectSpec {
    EffectSpec {
        key,
        value: EffectValue::Number(value),
    }
}

const fn boolean(key: &'static str, value: bool) -> EffectSpec {
    EffectSpec {
        key,
        value: EffectValue::Bool(value),
    }
}

const ASCENSION_1: &[EffectSpec] = &[number("jc_multiplier", 0.18)];
const ASCENSION_2: &[EffectSpec] = &[
    number("cave_in_bonus", 0.03),
    number("event_chance_multiplier", 0.20),
    number("jc_layer_penalty", 0.03),
];
const ASCENSION_3: &[EffectSpec] = &[
    number("luminosity_drain_multiplier", 0.25),
    number("rare_event_multiplier", 0.50),
    number("jc_layer_penalty", 0.02),
];
const ASCENSION_4: &[EffectSpec] = &[
    number("boss_payout_multiplier", 0.50),
    number("jc_layer_penalty", 0.05),
];
const ASCENSION_5: &[EffectSpec] = &[
    number("decay_multiplier", 0.50),
    number("milestone_multiplier", 0.50),
    number("jc_layer_penalty", 0.07),
];
const ASCENSION_6: &[EffectSpec] = &[
    boolean("corruption", true),
    number("artifact_multiplier", 2.0),
    number("jc_layer_penalty", 0.05),
];
const ASCENSION_7: &[EffectSpec] = &[
    boolean("event_chain", true),
    number("chain_jc_multiplier", 1.5),
];
const ASCENSION_8: &[EffectSpec] = &[boolean("mutations", true)];
const ASCENSION_9: &[EffectSpec] = &[
    number("cruel_safe_fail", 0.10),
    number("legendary_event_multiplier", 3.0),
];
const ASCENSION_10: &[EffectSpec] = &[
    number("paid_dig_cost_multiplier", 0.50),
    number("score_multiplier", 2.0),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AscensionModifier {
    pub level: i32,
    pub name: &'static str,
    pub effects: &'static [EffectSpec],
}

pub const ASCENSION_MODIFIERS: [AscensionModifier; 10] = [
    AscensionModifier {
        level: 1,
        name: "Dense Stone",
        effects: ASCENSION_1,
    },
    AscensionModifier {
        level: 2,
        name: "Unstable Ground",
        effects: ASCENSION_2,
    },
    AscensionModifier {
        level: 3,
        name: "Hungry Darkness",
        effects: ASCENSION_3,
    },
    AscensionModifier {
        level: 4,
        name: "Boss Rage",
        effects: ASCENSION_4,
    },
    AscensionModifier {
        level: 5,
        name: "Erosion",
        effects: ASCENSION_5,
    },
    AscensionModifier {
        level: 6,
        name: "Corruption",
        effects: ASCENSION_6,
    },
    AscensionModifier {
        level: 7,
        name: "Event Chains",
        effects: ASCENSION_7,
    },
    AscensionModifier {
        level: 8,
        name: "Mutations",
        effects: ASCENSION_8,
    },
    AscensionModifier {
        level: 9,
        name: "Cruel Echoes",
        effects: ASCENSION_9,
    },
    AscensionModifier {
        level: 10,
        name: "The Endless",
        effects: ASCENSION_10,
    },
];

#[must_use]
pub fn ascension_effects(prestige_level: i32) -> BTreeMap<&'static str, EffectValue> {
    let mut combined = BTreeMap::new();
    for modifier in ASCENSION_MODIFIERS
        .iter()
        .filter(|modifier| modifier.level <= prestige_level)
    {
        combine_effects(&mut combined, modifier.effects);
    }
    combined
}

fn combine_effects(combined: &mut BTreeMap<&'static str, EffectValue>, effects: &[EffectSpec]) {
    for effect in effects {
        match effect.value {
            EffectValue::Bool(value) => {
                combined.insert(effect.key, EffectValue::Bool(value));
            }
            EffectValue::Number(value) => {
                let current = combined
                    .get(effect.key)
                    .and_then(|effect| effect.number())
                    .unwrap_or(0.0);
                combined.insert(effect.key, EffectValue::Number(current + value));
            }
        }
    }
}

const CAVE_IN_LOOT: &[EffectSpec] = &[
    number("cave_in_loot_chance", 0.30),
    number("cave_in_loot_min", 1.0),
    number("cave_in_loot_max", 3.0),
];
const DARK_SIGHT: &[EffectSpec] = &[boolean("ignore_luminosity_cave_in", true)];
const THICK_SKIN: &[EffectSpec] = &[boolean("daily_cave_in_shield", true)];
const TREASURE_SENSE: &[EffectSpec] = &[number("artifact_chance_bonus", 0.25)];
const EVENT_MAGNET: &[EffectSpec] = &[number("event_chance_bonus", 0.30)];
const SECOND_WIND: &[EffectSpec] = &[number("post_cave_in_advance", 3.0)];
const BRITTLE_WALLS: &[EffectSpec] = &[number("cave_in_loss_bonus", 2.0)];
const HEAVY_AIR: &[EffectSpec] = &[number("advance_max_penalty", 1.0)];
const JINXED: &[EffectSpec] = &[number("zero_jc_chance", 0.05)];
const PARANOIA: &[EffectSpec] = &[number("sabotage_damage_bonus", 0.25)];
const RESTLESS: &[EffectSpec] = &[number("cooldown_bonus_seconds", 3_600.0)];
const FRAGILE: &[EffectSpec] = &[number("injury_duration_bonus", 1.0)];

#[derive(Clone, Copy, Debug)]
pub struct MutationDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub positive: bool,
    pub effects: &'static [EffectSpec],
}

impl PartialEq for MutationDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for MutationDefinition {}

pub const MUTATIONS: [MutationDefinition; 12] = [
    MutationDefinition {
        id: "cave_in_loot",
        name: "Lucky Rubble",
        description: "Cave-ins have 30% chance to drop 1-3 JC",
        positive: true,
        effects: CAVE_IN_LOOT,
    },
    MutationDefinition {
        id: "dark_sight",
        name: "Dark Sight",
        description: "No luminosity penalty to cave-in chance",
        positive: true,
        effects: DARK_SIGHT,
    },
    MutationDefinition {
        id: "thick_skin",
        name: "Thick Skin",
        description: "First cave-in each day is prevented",
        positive: true,
        effects: THICK_SKIN,
    },
    MutationDefinition {
        id: "treasure_sense",
        name: "Treasure Sense",
        description: "+25% artifact find chance",
        positive: true,
        effects: TREASURE_SENSE,
    },
    MutationDefinition {
        id: "event_magnet",
        name: "Event Magnet",
        description: "+30% event encounter rate",
        positive: true,
        effects: EVENT_MAGNET,
    },
    MutationDefinition {
        id: "second_wind",
        name: "Second Wind",
        description: "After cave-in, next dig gets +3 advance",
        positive: true,
        effects: SECOND_WIND,
    },
    MutationDefinition {
        id: "brittle_walls",
        name: "Brittle Walls",
        description: "Cave-in block loss +2",
        positive: false,
        effects: BRITTLE_WALLS,
    },
    MutationDefinition {
        id: "heavy_air",
        name: "Heavy Air",
        description: "Advance max -1",
        positive: false,
        effects: HEAVY_AIR,
    },
    MutationDefinition {
        id: "jinxed",
        name: "Jinxed",
        description: "5% chance any dig yields 0 JC",
        positive: false,
        effects: JINXED,
    },
    MutationDefinition {
        id: "paranoia",
        name: "Paranoia",
        description: "Sabotage damage +25%",
        positive: false,
        effects: PARANOIA,
    },
    MutationDefinition {
        id: "restless",
        name: "Restless",
        description: "Free dig cooldown +1 hour",
        positive: false,
        effects: RESTLESS,
    },
    MutationDefinition {
        id: "fragile",
        name: "Fragile",
        description: "Injuries last 1 extra dig",
        positive: false,
        effects: FRAGILE,
    },
];

#[must_use]
pub fn mutation_by_id(id: &str) -> Option<MutationDefinition> {
    MUTATIONS.iter().find(|mutation| mutation.id == id).copied()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationRoll {
    pub forced: MutationDefinition,
    pub choices: Vec<MutationDefinition>,
}

#[must_use]
pub fn prestige_mutation_seed(
    discord_id: DiscordId,
    guild_id: GuildId,
    target_level: i32,
) -> String {
    format!("prestige-mutations:{guild_id}:{discord_id}:{target_level}")
}

pub trait EntropyPort {
    fn next_u64(&mut self) -> u64;

    fn unit_interval(&mut self) -> f64 {
        const SCALE: f64 = (1_u64 << 53) as f64;
        (self.next_u64() >> 11) as f64 / SCALE
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        assert!(upper_exclusive > 0, "entropy index bound must be positive");
        self.next_u64() as usize % upper_exclusive
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplitMixEntropy {
    state: u64,
}

impl SplitMixEntropy {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }
}

impl EntropyPort for SplitMixEntropy {
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

fn stable_seed(text: &str) -> u64 {
    text.as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn shuffle_mutations(pool: &mut [MutationDefinition], entropy: &mut impl EntropyPort) {
    for index in (1..pool.len()).rev() {
        let replacement = entropy.index(index + 1);
        pool.swap(index, replacement);
    }
}

#[must_use]
pub fn roll_mutations_for_prestige(
    seed: Option<&str>,
    entropy: &mut impl EntropyPort,
) -> MutationRoll {
    let mut pool = MUTATIONS;
    match seed {
        Some(seed) => {
            let mut seeded = SplitMixEntropy::new(stable_seed(seed));
            shuffle_mutations(&mut pool, &mut seeded);
        }
        None => shuffle_mutations(&mut pool, entropy),
    }
    MutationRoll {
        forced: pool[0],
        choices: pool[1..4].to_vec(),
    }
}

#[must_use]
pub fn mutation_effects(mutations: &[MutationDefinition]) -> BTreeMap<&'static str, EffectValue> {
    let mut combined = BTreeMap::new();
    for mutation in mutations {
        combine_effects(&mut combined, mutation.effects);
    }
    combined
}

/// Compatibility parser for the existing SQLite JSON column.
///
/// The live adapter will use a full JSON codec. This bounded helper extracts
/// catalog-backed `id` strings only, rejects non-array input, and never creates
/// unknown mutations.
#[must_use]
pub fn mutations_from_json(raw: Option<&str>) -> Vec<MutationDefinition> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Vec::new();
    };
    if !(raw.starts_with('[') && raw.ends_with(']')) {
        return Vec::new();
    }
    raw.split("\"id\"")
        .skip(1)
        .filter_map(|suffix| {
            let value = suffix.split_once(':')?.1.trim_start();
            let value = value.strip_prefix('"')?;
            let id = value.split_once('"')?.0;
            mutation_by_id(id)
        })
        .collect()
}

const CORRUPT_JC: &[EffectSpec] = &[number("jc_penalty", 1.0)];
const CORRUPT_CAVE_IN: &[EffectSpec] = &[number("cave_in_bonus", 0.03)];
const CORRUPT_ADVANCE: &[EffectSpec] = &[number("advance_penalty", 1.0)];
const CORRUPT_LUMINOSITY: &[EffectSpec] = &[number("luminosity_drain", 5.0)];
const CORRUPT_NO_ARTIFACT: &[EffectSpec] = &[boolean("skip_artifact", true)];
const CORRUPT_RISKY: &[EffectSpec] = &[number("risky_penalty", 0.05)];
const CORRUPT_DOUBLE_HALF: &[EffectSpec] = &[boolean("double_half_jc", true)];
const CORRUPT_OMINOUS: &[EffectSpec] = &[boolean("ominous_name", true)];
const CORRUPT_FIXED: &[EffectSpec] = &[number("fixed_jc", 1.0)];
const CORRUPT_ECHO: &[EffectSpec] = &[boolean("min_advance_roll", true)];

#[derive(Clone, Copy, Debug)]
pub struct CorruptionDefinition {
    pub id: &'static str,
    pub description: &'static str,
    pub weird: bool,
    pub effects: &'static [EffectSpec],
}

impl PartialEq for CorruptionDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for CorruptionDefinition {}

const CORRUPTION_BAD: [CorruptionDefinition; 6] = [
    CorruptionDefinition {
        id: "corrupt_jc",
        description: "-1 JC this dig",
        weird: false,
        effects: CORRUPT_JC,
    },
    CorruptionDefinition {
        id: "corrupt_cave_in",
        description: "+3% cave-in this dig",
        weird: false,
        effects: CORRUPT_CAVE_IN,
    },
    CorruptionDefinition {
        id: "corrupt_advance",
        description: "-1 advance this dig",
        weird: false,
        effects: CORRUPT_ADVANCE,
    },
    CorruptionDefinition {
        id: "corrupt_luminosity",
        description: "-5 extra luminosity drain",
        weird: false,
        effects: CORRUPT_LUMINOSITY,
    },
    CorruptionDefinition {
        id: "corrupt_no_artifact",
        description: "No artifact roll this dig",
        weird: false,
        effects: CORRUPT_NO_ARTIFACT,
    },
    CorruptionDefinition {
        id: "corrupt_risky",
        description: "Risky event success -5%",
        weird: false,
        effects: CORRUPT_RISKY,
    },
];

const CORRUPTION_WEIRD: [CorruptionDefinition; 4] = [
    CorruptionDefinition {
        id: "corrupt_double_half",
        description: "JC doubled then halved (net loss on odd)",
        weird: true,
        effects: CORRUPT_DOUBLE_HALF,
    },
    CorruptionDefinition {
        id: "corrupt_ominous_name",
        description: "Tunnel name temporarily changes to something ominous",
        weird: true,
        effects: CORRUPT_OMINOUS,
    },
    CorruptionDefinition {
        id: "corrupt_fixed_jc",
        description: "You find exactly 1 JC. Always 1. No more, no less.",
        weird: true,
        effects: CORRUPT_FIXED,
    },
    CorruptionDefinition {
        id: "corrupt_echo",
        description: "Your pickaxe swings echo twice. Advance is rolled twice, take the lower.",
        weird: true,
        effects: CORRUPT_ECHO,
    },
];

#[must_use]
pub fn roll_corruption(
    prestige_level: i32,
    entropy: &mut impl EntropyPort,
) -> Option<CorruptionDefinition> {
    if prestige_level < 6 {
        return None;
    }
    let pool: &[CorruptionDefinition] = if entropy.unit_interval() < 0.80 {
        &CORRUPTION_BAD
    } else {
        &CORRUPTION_WEIRD
    };
    Some(pool[entropy.index(pool.len())])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct TunnelKey {
    pub guild_id: GuildId,
    pub discord_id: DiscordId,
}

impl TunnelKey {
    #[must_use]
    pub const fn new(discord_id: DiscordId, guild_id: GuildId) -> Self {
        Self {
            guild_id,
            discord_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelicRarity {
    Rare,
    Legendary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelicGrant {
    pub id: &'static str,
    pub name: &'static str,
    pub rarity: RelicRarity,
}

/// Rare-or-better entries from Python's `RELICS` table, excluding its six
/// signature `TROPHY_RELIC_IDS`. The authored order is retained because it is
/// part of random-choice compatibility.
const PRESTIGE_RELICS: [RelicGrant; 24] = [
    RelicGrant {
        id: "echo_stone",
        name: "Echo Stone",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "spore_cloak",
        name: "Spore Cloak",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "frozen_clock",
        name: "Frozen Clock",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "hollow_eye",
        name: "Hollow Eye",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "hollow_fang",
        name: "Hollow Fang",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "echo_lantern",
        name: "Echo Lantern",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "patient_stone",
        name: "Patient Stone",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "prism_heart",
        name: "Prism Heart",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "bloodstone",
        name: "Bloodstone",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "gamblers_charm",
        name: "Gambler's Charm",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "vendetta_coin",
        name: "Vendetta Coin",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "stormcaller",
        name: "Stormcaller",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "slow_drip",
        name: "Slow Drip",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "deepveined_coal",
        name: "Deepveined Coal",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "diviners_knot",
        name: "Diviner's Knot",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "pathfinders_spur",
        name: "Pathfinder's Spur",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "aegis_fragment",
        name: "Aegis Fragment",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "bottled_quake",
        name: "Bottled Quake",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "black_wax_seal",
        name: "Black Wax Seal",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "burning_ledger",
        name: "Burning Ledger",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "shifting_idol",
        name: "Shifting Idol",
        rarity: RelicRarity::Legendary,
    },
    RelicGrant {
        id: "first_light",
        name: "First Light",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "berserkers_mark",
        name: "Berserker's Mark",
        rarity: RelicRarity::Rare,
    },
    RelicGrant {
        id: "gamblers_edge",
        name: "Gambler's Edge",
        rarity: RelicRarity::Rare,
    },
];

pub const TROPHY_RELIC_IDS: [&str; 6] = [
    "weeping_fang",
    "runebitten_shard",
    "aching_spine",
    "listening_shard",
    "hateborn_ember",
    "deaths_door",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tunnel {
    pub key: TunnelKey,
    pub tunnel_name: String,
    pub depth: i64,
    pub luminosity: i64,
    pub last_dig_at: Option<i64>,
    pub last_cheer_at: Option<i64>,
    pub boss_attempts: i64,
    pub pickaxe_tier: usize,
    pub prestige_level: i32,
    pub prestige_perks: Vec<String>,
    pub boss_progress: BTreeMap<i32, BossStatus>,
    pub balance: i64,
    pub artifacts: Vec<RelicGrant>,
    pub best_run_score: i64,
    pub total_prestige_score: i64,
    pub current_run_jc: i64,
    pub current_run_artifacts: i64,
    pub current_run_events: i64,
    pub mutations: Vec<MutationDefinition>,
    pub streak_days: i64,
    pub injury_active: bool,
    pub cheer_active: bool,
    pub pinnacle_boss_id: Option<String>,
    pub pinnacle_phase: u8,
    pub pinnacle_hp_remaining: Option<i32>,
    pub route_state: Option<String>,
}

impl Tunnel {
    #[must_use]
    pub fn new(key: TunnelKey, tunnel_name: impl Into<String>, balance: i64) -> Self {
        Self {
            key,
            tunnel_name: tunnel_name.into(),
            depth: 0,
            luminosity: 100,
            last_dig_at: None,
            last_cheer_at: None,
            boss_attempts: 0,
            pickaxe_tier: 0,
            prestige_level: 0,
            prestige_perks: Vec::new(),
            boss_progress: active_regular_bosses(),
            balance,
            artifacts: Vec::new(),
            best_run_score: 0,
            total_prestige_score: 0,
            current_run_jc: 0,
            current_run_artifacts: 0,
            current_run_events: 0,
            mutations: Vec::new(),
            streak_days: 0,
            injury_active: false,
            cheer_active: false,
            pinnacle_boss_id: None,
            pinnacle_phase: 0,
            pinnacle_hp_remaining: None,
            route_state: None,
        }
    }

    pub fn mark_prestige_ready(&mut self) {
        self.depth = i64::from(PINNACLE_DEPTH);
        for boundary in BOSS_BOUNDARIES {
            self.boss_progress.insert(boundary, BossStatus::Defeated);
        }
        self.boss_progress
            .insert(PINNACLE_DEPTH, BossStatus::Defeated);
    }
}

fn active_regular_bosses() -> BTreeMap<i32, BossStatus> {
    BOSS_BOUNDARIES
        .into_iter()
        .map(|boundary| (boundary, BossStatus::Active))
        .collect()
}

#[must_use]
pub fn calculate_run_score(tunnel: &Tunnel) -> i64 {
    let bosses_defeated = tunnel
        .boss_progress
        .values()
        .filter(|status| **status == BossStatus::Defeated)
        .count() as i64;
    let base = tunnel.depth
        + bosses_defeated * 50
        + (tunnel.current_run_jc as f64 * 0.5) as i64
        + tunnel.current_run_events * 10;
    let mut multiplier = 1.0 + f64::from(tunnel.prestige_level) * 0.1;
    if let Some(score_multiplier) = ascension_effects(tunnel.prestige_level)
        .get("score_multiplier")
        .and_then(|effect| effect.number())
    {
        multiplier += score_multiplier;
    }
    (base as f64 * multiplier) as i64
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegularBossWin {
    pub phase: u8,
    pub phase_two_incoming: bool,
    pub persisted_status: BossStatus,
}

#[must_use]
pub const fn regular_boss_win(prestige_level: i32) -> RegularBossWin {
    if prestige_level >= 2 {
        RegularBossWin {
            phase: 1,
            phase_two_incoming: true,
            persisted_status: BossStatus::PhaseOneDefeated,
        }
    } else {
        RegularBossWin {
            phase: 1,
            phase_two_incoming: false,
            persisted_status: BossStatus::Defeated,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RawTunnelValue {
    Integer(i64),
    Text(String),
    Null,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizationError {
    pub column: String,
    pub value: String,
}

impl Display for NormalizationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "tunnel column {} is not an integer: {}",
            self.column, self.value
        )
    }
}

impl Error for NormalizationError {}

fn is_integer_tunnel_column(column: &str) -> bool {
    matches!(
        column,
        "discord_id"
            | "guild_id"
            | "depth"
            | "max_depth"
            | "total_digs"
            | "total_jc_earned"
            | "last_dig_at"
            | "streak_days"
            | "pickaxe_tier"
            | "prestige_level"
            | "trap_active"
            | "trap_free_today"
            | "insured_until"
            | "reinforced_until"
            | "paid_digs_today"
            | "revenge_target"
            | "revenge_until"
            | "hard_hat_charges"
            | "void_bait_digs"
            | "luminosity"
            | "boss_attempts"
            | "grappling_hook_charges"
            | "sonar_skip_pending"
            | "best_run_score"
            | "current_run_jc"
            | "current_run_artifacts"
            | "current_run_events"
            | "total_prestige_score"
            | "stat_strength"
            | "stat_smarts"
            | "stat_stamina"
            | "stat_points"
            | "last_lum_update_at"
            | "pinnacle_phase"
            | "pinnacle_hp_remaining"
            | "pinnacle_last_engaged_at"
            | "cavein_free_streak"
            | "relic_trim_notice"
            | "auto_buy_torch"
            | "auto_buy_hard_hat"
            | "last_cheer_at"
    )
}

pub fn normalize_tunnel_row(
    row: &mut BTreeMap<String, RawTunnelValue>,
) -> Result<(), NormalizationError> {
    for (column, value) in row.iter_mut() {
        if !is_integer_tunnel_column(column) {
            continue;
        }
        if let RawTunnelValue::Text(raw) = value {
            let parsed = raw.parse::<i64>().map_err(|_| NormalizationError {
                column: column.clone(),
                value: raw.clone(),
            })?;
            *value = RawTunnelValue::Integer(parsed);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TunnelActionKind {
    Prestige,
    Abandon,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelAction {
    pub key: TunnelKey,
    pub kind: TunnelActionKind,
    pub jc_delta: i64,
    pub happened_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceError {
    MissingTunnel,
    StateConflict,
    InjectedFailure,
}

impl Display for PersistenceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTunnel => formatter.write_str("tunnel does not exist"),
            Self::StateConflict => formatter.write_str("tunnel state changed before commit"),
            Self::InjectedFailure => formatter.write_str("injected atomic persistence failure"),
        }
    }
}

impl Error for PersistenceError {}

pub trait TunnelPersistence {
    fn get_tunnel(&self, key: TunnelKey) -> Option<Tunnel>;

    fn atomic_replace(
        &mut self,
        expected_prestige_level: i32,
        replacement: Tunnel,
        action: TunnelAction,
    ) -> Result<(), PersistenceError>;

    fn has_recent_abandon(&self, key: TunnelKey, since: i64) -> bool;

    fn hall_of_fame(&self, guild_id: GuildId, limit: usize) -> Vec<HallOfFameEntry>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InMemoryTunnelRepository {
    tunnels: BTreeMap<TunnelKey, Tunnel>,
    actions: Vec<TunnelAction>,
    fail_next_atomic: bool,
}

impl InMemoryTunnelRepository {
    pub fn insert(&mut self, tunnel: Tunnel) {
        self.tunnels.insert(tunnel.key, tunnel);
    }

    #[must_use]
    pub fn tunnel(&self, key: TunnelKey) -> Option<&Tunnel> {
        self.tunnels.get(&key)
    }

    pub fn tunnel_mut(&mut self, key: TunnelKey) -> Option<&mut Tunnel> {
        self.tunnels.get_mut(&key)
    }

    pub fn update_last_cheer_at(
        &mut self,
        key: TunnelKey,
        last_cheer_at: i64,
    ) -> Result<(), PersistenceError> {
        let tunnel = self
            .tunnels
            .get_mut(&key)
            .ok_or(PersistenceError::MissingTunnel)?;
        tunnel.last_cheer_at = Some(last_cheer_at);
        Ok(())
    }

    pub fn fail_next_atomic(&mut self) {
        self.fail_next_atomic = true;
    }

    #[must_use]
    pub fn actions(&self) -> &[TunnelAction] {
        &self.actions
    }
}

impl TunnelPersistence for InMemoryTunnelRepository {
    fn get_tunnel(&self, key: TunnelKey) -> Option<Tunnel> {
        self.tunnels.get(&key).cloned()
    }

    fn atomic_replace(
        &mut self,
        expected_prestige_level: i32,
        replacement: Tunnel,
        action: TunnelAction,
    ) -> Result<(), PersistenceError> {
        if self.fail_next_atomic {
            self.fail_next_atomic = false;
            return Err(PersistenceError::InjectedFailure);
        }
        let current = self
            .tunnels
            .get(&replacement.key)
            .ok_or(PersistenceError::MissingTunnel)?;
        if current.prestige_level != expected_prestige_level {
            return Err(PersistenceError::StateConflict);
        }
        self.tunnels.insert(replacement.key, replacement);
        self.actions.push(action);
        Ok(())
    }

    fn has_recent_abandon(&self, key: TunnelKey, since: i64) -> bool {
        self.actions.iter().any(|action| {
            action.key == key
                && action.kind == TunnelActionKind::Abandon
                && action.happened_at >= since
        })
    }

    fn hall_of_fame(&self, guild_id: GuildId, limit: usize) -> Vec<HallOfFameEntry> {
        let mut entries: Vec<_> = self
            .tunnels
            .values()
            .filter(|tunnel| tunnel.key.guild_id == guild_id && tunnel.best_run_score > 0)
            .map(|tunnel| HallOfFameEntry {
                discord_id: tunnel.key.discord_id,
                tunnel_name: tunnel.tunnel_name.clone(),
                prestige_level: tunnel.prestige_level,
                best_run_score: tunnel.best_run_score,
            })
            .collect();
        entries.sort_by(|left, right| {
            right
                .best_run_score
                .cmp(&left.best_run_score)
                .then_with(|| left.discord_id.cmp(&right.discord_id))
        });
        entries.truncate(limit);
        entries
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HallOfFameEntry {
    pub discord_id: DiscordId,
    pub tunnel_name: String,
    pub prestige_level: i32,
    pub best_run_score: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrestigeGrant {
    pub jc: i64,
    pub relic: Option<RelicGrant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationPrestigeInfo {
    pub forced: MutationDefinition,
    pub chosen: Option<MutationDefinition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrestigeResult {
    pub prestige_level: i32,
    pub perk_chosen: String,
    pub run_score: i64,
    pub best_run_score: i64,
    pub total_prestige_score: i64,
    pub mutations: Option<MutationPrestigeInfo>,
    pub prestige_grant: PrestigeGrant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbandonResult {
    pub depth_lost: i64,
    pub refund: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunnelError {
    MissingTunnel,
    BossesRemaining,
    PinnacleRemaining,
    MaximumPrestige,
    InvalidPerk,
    PerkAtCap,
    TooShallowToAbandon,
    AbandonCooldown,
    EmptyTunnelName,
    Persistence(PersistenceError),
}

impl Display for TunnelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTunnel => formatter.write_str("You don't have a tunnel."),
            Self::BossesRemaining => formatter.write_str("Bosses remain."),
            Self::PinnacleRemaining => {
                formatter.write_str("Something stirs deeper still — descend further.")
            }
            Self::MaximumPrestige => formatter.write_str("Already at max prestige."),
            Self::InvalidPerk => formatter.write_str("Invalid prestige perk."),
            Self::PerkAtCap => formatter.write_str("You've maxed that perk."),
            Self::TooShallowToAbandon => {
                formatter.write_str("Tunnel must be at least 10 blocks deep to abandon.")
            }
            Self::AbandonCooldown => {
                formatter.write_str("You can only abandon once every 24 hours.")
            }
            Self::EmptyTunnelName => formatter.write_str("Tunnel has no display name."),
            Self::Persistence(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for TunnelError {}

impl From<PersistenceError> for TunnelError {
    fn from(error: PersistenceError) -> Self {
        Self::Persistence(error)
    }
}

pub struct TunnelService<R, E> {
    repository: R,
    entropy: E,
}

impl<R, E> TunnelService<R, E>
where
    R: TunnelPersistence,
    E: EntropyPort,
{
    #[must_use]
    pub const fn new(repository: R, entropy: E) -> Self {
        Self {
            repository,
            entropy,
        }
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    pub const fn repository_mut(&mut self) -> &mut R {
        &mut self.repository
    }

    #[must_use]
    pub fn into_parts(self) -> (R, E) {
        (self.repository, self.entropy)
    }

    pub fn prestige(
        &mut self,
        key: TunnelKey,
        perk_choice: &str,
        mutation_choice: Option<&str>,
        now: i64,
    ) -> Result<PrestigeResult, TunnelError> {
        let tunnel = self
            .repository
            .get_tunnel(key)
            .ok_or(TunnelError::MissingTunnel)?;
        ensure_prestige_ready(&tunnel)?;
        if !PRESTIGE_PERKS.contains(&perk_choice) {
            return Err(TunnelError::InvalidPerk);
        }
        if tunnel
            .prestige_perks
            .iter()
            .filter(|perk| perk.as_str() == perk_choice)
            .count()
            >= PRESTIGE_PERK_STACK_CAP
        {
            return Err(TunnelError::PerkAtCap);
        }

        let previous_level = tunnel.prestige_level;
        let prestige_level = previous_level + 1;
        let run_score = calculate_run_score(&tunnel);
        let best_run_score = tunnel.best_run_score.max(run_score);
        let total_prestige_score = tunnel.total_prestige_score + run_score;

        let (mutations, active_mutations) = if prestige_level >= 8 {
            let seed = prestige_mutation_seed(key.discord_id, key.guild_id, prestige_level);
            let rolled = roll_mutations_for_prestige(Some(&seed), &mut self.entropy);
            let chosen = mutation_choice
                .and_then(mutation_by_id)
                .or_else(|| rolled.choices.first().copied());
            let mut active = vec![rolled.forced];
            active.extend(chosen);
            (
                Some(MutationPrestigeInfo {
                    forced: rolled.forced,
                    chosen,
                }),
                active,
            )
        } else {
            (None, Vec::new())
        };

        let owned_ids: Vec<_> = tunnel
            .artifacts
            .iter()
            .map(|artifact| artifact.id)
            .collect();
        let eligible_relics: Vec<_> = PRESTIGE_RELICS
            .iter()
            .copied()
            .filter(|relic| !owned_ids.contains(&relic.id))
            .collect();
        let relic = (!eligible_relics.is_empty())
            .then(|| eligible_relics[self.entropy.index(eligible_relics.len())]);
        let jc_grant = scale_positive_dig_jc(PRESTIGE_GROSS_JC);

        let mut replacement = tunnel;
        replacement.depth = 0;
        replacement.boss_progress = active_regular_bosses();
        replacement.boss_attempts = 0;
        replacement.prestige_level = prestige_level;
        replacement.prestige_perks.push(perk_choice.to_owned());
        replacement.best_run_score = best_run_score;
        replacement.total_prestige_score = total_prestige_score;
        replacement.current_run_jc = 0;
        replacement.current_run_artifacts = 0;
        replacement.current_run_events = 0;
        replacement.mutations = active_mutations;
        replacement.injury_active = false;
        replacement.cheer_active = false;
        replacement.pinnacle_boss_id = None;
        replacement.pinnacle_phase = 0;
        replacement.pinnacle_hp_remaining = None;
        replacement.route_state = None;
        replacement.balance += jc_grant;
        replacement.artifacts.extend(relic);

        self.repository.atomic_replace(
            previous_level,
            replacement,
            TunnelAction {
                key,
                kind: TunnelActionKind::Prestige,
                jc_delta: jc_grant,
                happened_at: now,
            },
        )?;

        Ok(PrestigeResult {
            prestige_level,
            perk_chosen: perk_choice.to_owned(),
            run_score,
            best_run_score,
            total_prestige_score,
            mutations,
            prestige_grant: PrestigeGrant {
                jc: jc_grant,
                relic,
            },
        })
    }

    pub fn abandon(&mut self, key: TunnelKey, now: i64) -> Result<AbandonResult, TunnelError> {
        let tunnel = self
            .repository
            .get_tunnel(key)
            .ok_or(TunnelError::MissingTunnel)?;
        if tunnel.depth < ABANDON_MINIMUM_DEPTH {
            return Err(TunnelError::TooShallowToAbandon);
        }
        if self
            .repository
            .has_recent_abandon(key, now.saturating_sub(ABANDON_COOLDOWN_SECONDS))
        {
            return Err(TunnelError::AbandonCooldown);
        }
        let depth_lost = tunnel.depth;
        let gross_refund = (depth_lost as f64 * 0.10) as i64;
        let refund = scale_positive_dig_jc(gross_refund);
        let expected_prestige = tunnel.prestige_level;
        let mut replacement = tunnel;
        replacement.depth = 0;
        replacement.boss_progress = active_regular_bosses();
        replacement.boss_attempts = 0;
        replacement.injury_active = false;
        replacement.cheer_active = false;
        replacement.streak_days = 0;
        replacement.pinnacle_boss_id = None;
        replacement.pinnacle_phase = 0;
        replacement.pinnacle_hp_remaining = None;
        replacement.route_state = None;
        replacement.balance += refund;

        self.repository.atomic_replace(
            expected_prestige,
            replacement,
            TunnelAction {
                key,
                kind: TunnelActionKind::Abandon,
                jc_delta: refund,
                happened_at: now,
            },
        )?;
        Ok(AbandonResult { depth_lost, refund })
    }

    pub fn help_target(&self, target: TunnelKey) -> Result<TunnelNameProjection, TunnelError> {
        self.target_projection(target)
    }

    pub fn sabotage_target(&self, target: TunnelKey) -> Result<TunnelNameProjection, TunnelError> {
        self.target_projection(target)
    }

    pub fn flex_data(&self, key: TunnelKey) -> Result<TunnelNameProjection, TunnelError> {
        self.target_projection(key)
    }

    pub fn first_letter_clue(&self, key: TunnelKey) -> Result<String, TunnelError> {
        let name = self.target_projection(key)?.tunnel_name;
        let first = name.chars().next().ok_or(TunnelError::EmptyTunnelName)?;
        Ok(format!("Their tunnel name starts with **{first}**."))
    }

    #[must_use]
    pub fn hall_of_fame(&self, guild_id: GuildId) -> Vec<HallOfFameEntry> {
        self.repository.hall_of_fame(guild_id, 10)
    }

    fn target_projection(&self, key: TunnelKey) -> Result<TunnelNameProjection, TunnelError> {
        let tunnel = self
            .repository
            .get_tunnel(key)
            .ok_or(TunnelError::MissingTunnel)?;
        if tunnel.tunnel_name.is_empty() {
            return Err(TunnelError::EmptyTunnelName);
        }
        Ok(TunnelNameProjection {
            tunnel_name: tunnel.tunnel_name,
        })
    }
}

fn ensure_prestige_ready(tunnel: &Tunnel) -> Result<(), TunnelError> {
    if tunnel.prestige_level >= MAX_PRESTIGE {
        return Err(TunnelError::MaximumPrestige);
    }
    if !BOSS_BOUNDARIES
        .iter()
        .all(|boundary| tunnel.boss_progress.get(boundary).copied() == Some(BossStatus::Defeated))
    {
        return Err(TunnelError::BossesRemaining);
    }
    if tunnel.boss_progress.get(&PINNACLE_DEPTH).copied() != Some(BossStatus::Defeated) {
        return Err(TunnelError::PinnacleRemaining);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelNameProjection {
    pub tunnel_name: String,
}

#[cfg(test)]
#[path = "dig_tunnels/tests.rs"]
mod tests;
