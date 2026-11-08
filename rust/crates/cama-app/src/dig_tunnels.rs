//! Prestige-run policy catalogs.
//!
//! Transport- and storage-independent prestige perks, Reading the Stone
//! hints, ascension, mutation, and corruption policy, and the prestige relic
//! descriptor. The live prestige and abandon flows persist through the Dig
//! runtime services.

use std::collections::BTreeMap;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const MAX_PRESTIGE: i32 = 20;
pub const PRESTIGE_PERK_STACK_CAP: usize = 5;
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

/// Limit picks to stacks that have an additional mechanical effect.
#[must_use]
pub fn prestige_perk_stack_cap(perk: &str) -> usize {
    match perk {
        "dark_adaptation" | "reading_the_stone" => 1,
        "deep_sight" | "steady_hands" => 4,
        _ if PRESTIGE_PERKS.contains(&perk) => PRESTIGE_PERK_STACK_CAP,
        _ => 0,
    }
}

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

/// Sum perk stacks additively within their useful caps, including legacy saves.
#[must_use]
pub fn aggregate_prestige_perk_effects(perks: &[String]) -> BTreeMap<&'static str, f64> {
    let mut effects = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for perk in perks {
        let count = counts.entry(perk.as_str()).or_insert(0);
        if *count >= prestige_perk_stack_cap(perk) {
            continue;
        }
        *count += 1;
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

#[cfg(test)]
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

#[cfg(test)]
#[path = "dig_tunnels/tests.rs"]
mod tests;
