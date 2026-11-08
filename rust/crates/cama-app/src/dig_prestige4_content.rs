//! Typed, Discord-neutral prestige-four Dig content policy.
//!
//! Existing boss, phase, mechanic, stinger, and artifact catalogs are reused
//! where they already match Python. This module adds signature-trophy metadata,
//! trophy combat effects, and the ordinary artifact-roll staging used by Dig.

use std::collections::BTreeSet;

use crate::boss_multi_tier::{StingerDefinition, stinger_by_id};
use crate::dig_bosses::{
    BossDefinition, BossMechanic, RegularPhaseDefinition, boss_by_id, boss_pool_for_tier,
    default_regular_phase, mechanic, regular_phase,
};
use crate::dig_loot::{ArtifactDef, Rarity, artifact_catalog};
use crate::dig_service::layer_at;

pub const BASE_ARTIFACT_FIND_RATE: f64 = 0.005;

pub const PRESTIGE_FOUR_BOSS_IDS: [&str; 3] = ["blightcoil", "rimebound_king", "spineback"];
#[cfg(test)]
pub const BESPOKE_PHASE_BOSS_IDS: [&str; 5] = [
    "blightcoil",
    "rimebound_king",
    "spineback",
    "xalatath",
    "lilith",
];
pub const PRESTIGE_FOUR_MECHANIC_IDS: [&str; 6] = [
    "blightcoil_wards",
    "blightcoil_nova",
    "rimebound_harvest",
    "rimebound_raise",
    "spineback_regrowth",
    "spineback_divebomb",
];
pub const PRESTIGE_FOUR_STINGER_IDS: [&str; 3] =
    ["blightcoil_venom", "rimebound_soulchill", "spineback_rend"];
pub const TROPHY_RELIC_IDS: [&str; 6] = [
    "weeping_fang",
    "runebitten_shard",
    "aching_spine",
    "listening_shard",
    "hateborn_ember",
    "deaths_door",
];
#[cfg(test)]
pub const PRESTIGE_FOUR_TROPHY_IDS: [&str; 5] = [
    "weeping_fang",
    "runebitten_shard",
    "aching_spine",
    "listening_shard",
    "hateborn_ember",
];
#[cfg(test)]
pub const PRESTIGE_FOUR_GENERAL_RELIC_IDS: [&str; 3] =
    ["deepveined_coal", "diviners_knot", "pathfinders_spur"];

/// Compose the independent multipliers that affect an ordinary artifact find.
///
/// `weather_factor`, `route_factor`, `ascension_factor`, and
/// `post_pinnacle_factor` are already multiplicative factors, so callers should
/// pass `1.0` when a source has no effect. `treasure_bonus` is the additive
/// bonus exposed by the Treasure Sense mutation (for example, `0.25` means
/// `+25%`). The base find rate is deliberately not included here.
#[must_use]
pub fn artifact_rate_modifier(
    echo_stone: bool,
    weather_factor: f64,
    route_factor: f64,
    ascension_factor: f64,
    treasure_bonus: f64,
    post_pinnacle_factor: f64,
) -> f64 {
    (if echo_stone { 1.1 } else { 1.0 })
        * weather_factor.max(0.0)
        * route_factor.max(0.0)
        * ascension_factor.max(0.0)
        * (1.0 + treasure_bonus).max(0.0)
        * post_pinnacle_factor.max(0.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrestigeBossContent {
    pub boss: &'static BossDefinition,
    pub trophy_relic_id: &'static str,
}

#[must_use]
pub fn prestige_boss_content(boss_id: &str) -> Option<PrestigeBossContent> {
    let trophy_relic_id = match boss_id {
        "blightcoil" => "weeping_fang",
        "rimebound_king" => "runebitten_shard",
        "spineback" => "aching_spine",
        "xalatath" => "listening_shard",
        "lilith" => "hateborn_ember",
        _ => return None,
    };
    Some(PrestigeBossContent {
        boss: boss_by_id(boss_id)?,
        trophy_relic_id,
    })
}

#[must_use]
pub fn phase_for(boss_id: &str, depth: i32, phase: u8) -> Option<&'static RegularPhaseDefinition> {
    regular_phase(boss_id, phase).or_else(|| default_regular_phase(depth, phase))
}

#[must_use]
pub fn prestige_four_mechanic(mechanic_id: &str) -> Option<BossMechanic> {
    PRESTIGE_FOUR_MECHANIC_IDS
        .contains(&mechanic_id)
        .then(|| mechanic(mechanic_id))
        .flatten()
}

#[must_use]
pub fn prestige_four_stinger(stinger_id: &str) -> Option<&'static StingerDefinition> {
    PRESTIGE_FOUR_STINGER_IDS
        .contains(&stinger_id)
        .then(|| stinger_by_id(stinger_id))
        .flatten()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactContent {
    pub definition: ArtifactDef,
    pub lore: &'static str,
    pub effect: &'static str,
}

const CONTENT_TEXT: [(&str, &str, &str); 8] = [
    (
        "weeping_fang",
        "A wet green fang carved from the Blightcoil still beads venom.",
        "Venom chips the boss for the first four rounds",
    ),
    (
        "runebitten_shard",
        "A broken rune remembers the warmth it stole from a king.",
        "Heal 1 HP on the first landed boss hit",
    ),
    (
        "aching_spine",
        "The shed spine aches toward the creature that grew it.",
        "Regrow 1 HP after an unharmed round, capped at starting HP",
    ),
    (
        "listening_shard",
        "It tells you what has not happened yet in a familiar voice.",
        "Forewarned: the boss cannot hit on round one",
    ),
    (
        "hateborn_ember",
        "It runs warmer as its bearer approaches the last breath.",
        "Last stand: +1 boss damage while at 1 HP",
    ),
    (
        "deepveined_coal",
        "It burns brightest where no light remains to spare.",
        "+20% JC while in the dark",
    ),
    (
        "diviners_knot",
        "A cold knot with one true end hidden in the tangle.",
        "+10% success on risky events",
    ),
    (
        "pathfinders_spur",
        "The deeper it descends, the harder it pulls toward the bottom.",
        "+1 advance per dig at depth 150+",
    ),
];

#[must_use]
pub fn artifact_content(artifact_id: &str) -> Option<ArtifactContent> {
    let definition = artifact_catalog()
        .into_iter()
        .find(|artifact| artifact.id == artifact_id)?;
    let (_, lore, effect) = CONTENT_TEXT
        .iter()
        .find(|(candidate, _, _)| *candidate == artifact_id)?;
    Some(ArtifactContent {
        definition,
        lore,
        effect,
    })
}

#[must_use]
pub fn is_ordinary_relic(artifact: ArtifactDef) -> bool {
    artifact.is_relic
        && artifact.min_prestige == 0
        && artifact.id != "aegis_fragment"
        && !artifact.trophy
}

/// Return the ordinary relics for a rolled rarity, preferring the current
/// layer and falling back to the full rarity pool when that layer is empty.
///
/// The returned vector retains catalog order. Selection is intentionally left
/// to the injected entropy source so callers can use a uniform index draw.
#[must_use]
pub fn ordinary_relic_candidates(
    depth: i64,
    rarity: Rarity,
    owned: &BTreeSet<String>,
) -> Vec<ArtifactDef> {
    let layer = layer_at(depth).name;
    let all = artifact_catalog();
    let by_layer = all
        .iter()
        .copied()
        .filter(|artifact| {
            is_ordinary_relic(*artifact)
                && artifact.rarity == rarity
                && artifact.layer.eq_ignore_ascii_case(layer)
                && !owned.contains(artifact.id)
        })
        .collect::<Vec<_>>();
    if by_layer.is_empty() {
        all.into_iter()
            .filter(|artifact| {
                is_ordinary_relic(*artifact)
                    && artifact.rarity == rarity
                    && !owned.contains(artifact.id)
            })
            .collect()
    } else {
        by_layer
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrophyStatus {
    pub venom_rounds: i64,
    pub forewarned: bool,
    pub lifesteal_available: bool,
    pub regrowth: bool,
    pub last_stand: bool,
    pub starting_hp: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrophyRoundInput {
    pub round: i64,
    pub player_hp: i64,
    pub boss_hp: i64,
    pub player_hit: bool,
    pub player_damage: i64,
    pub boss_hit: bool,
    pub boss_damage: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrophyRoundEvents {
    pub venom: bool,
    pub forewarned: bool,
    pub lifesteal: bool,
    pub regrowth: bool,
    pub last_stand: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrophyRoundResult {
    pub player_hp: i64,
    pub boss_hp: i64,
    pub terminal: Option<bool>,
    pub events: TrophyRoundEvents,
}

pub fn run_trophy_round(input: TrophyRoundInput, status: &mut TrophyStatus) -> TrophyRoundResult {
    let mut player_hp = input.player_hp;
    let mut boss_hp = input.boss_hp;
    let mut events = TrophyRoundEvents::default();
    if status.venom_rounds > 0 {
        boss_hp -= 1;
        status.venom_rounds -= 1;
        events.venom = true;
    }
    if boss_hp <= 0 {
        return TrophyRoundResult {
            player_hp,
            boss_hp,
            terminal: Some(true),
            events,
        };
    }
    if input.player_hit {
        let last_stand = player_hp == 1 && status.last_stand;
        boss_hp -= input.player_damage + i64::from(last_stand);
        events.last_stand = last_stand;
        if status.lifesteal_available {
            player_hp += 1;
            status.lifesteal_available = false;
            events.lifesteal = true;
        }
    }
    if boss_hp <= 0 {
        return TrophyRoundResult {
            player_hp,
            boss_hp,
            terminal: Some(true),
            events,
        };
    }
    let hp_before_boss = player_hp;
    if input.round == 1 && status.forewarned {
        events.forewarned = true;
    } else if input.boss_hit {
        player_hp -= input.boss_damage;
    }
    if player_hp <= 0 {
        return TrophyRoundResult {
            player_hp,
            boss_hp,
            terminal: Some(false),
            events,
        };
    }
    if status.regrowth && player_hp == hp_before_boss && player_hp < status.starting_hp {
        player_hp += 1;
        events.regrowth = true;
    }
    TrophyRoundResult {
        player_hp,
        boss_hp,
        terminal: None,
        events,
    }
}

pub trait Prestige4Entropy {
    fn unit(&mut self) -> f64;
    fn choose_index(&mut self, upper_bound: usize) -> usize;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArtifactRollPlan {
    pub depth: i64,
    pub rate_modifier: f64,
    pub skip_artifact: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactStagePlan {
    pub definition: ArtifactDef,
    pub current_run_artifacts_delta: i64,
}

#[must_use]
pub const fn artifact_stage_plan(definition: ArtifactDef) -> ArtifactStagePlan {
    ArtifactStagePlan {
        definition,
        current_run_artifacts_delta: 1,
    }
}

/// Apply the Python artifact-roll ordering without persistence side effects.
///
/// A skipped roll returns before touching entropy. Otherwise entropy is
/// consumed in exactly this order: find gate, rarity, then uniform candidate
/// selection (only when a candidate exists).
#[must_use]
pub fn roll_artifact_stage(
    plan: ArtifactRollPlan,
    owned: &BTreeSet<String>,
    entropy: &mut impl Prestige4Entropy,
) -> Option<ArtifactStagePlan> {
    if plan.skip_artifact {
        return None;
    }
    if entropy.unit() >= BASE_ARTIFACT_FIND_RATE * plan.rate_modifier.max(0.0) {
        return None;
    }
    let rarity = rarity_for_roll(entropy.unit());
    let candidates = ordinary_relic_candidates(plan.depth, rarity, owned);
    if candidates.is_empty() {
        return None;
    }
    let index = entropy
        .choose_index(candidates.len())
        .min(candidates.len() - 1);
    Some(artifact_stage_plan(candidates[index]))
}

#[must_use]
pub fn rarity_for_roll(roll: f64) -> Rarity {
    if roll < 0.60 {
        Rarity::Common
    } else if roll < 0.90 {
        Rarity::Uncommon
    } else if roll < 0.99 {
        Rarity::Rare
    } else {
        Rarity::Legendary
    }
}

#[must_use]
pub fn deepveined_yield_multiplier(has_relic: bool, luminosity: i64) -> f64 {
    if has_relic && luminosity < 20 {
        1.20
    } else {
        1.0
    }
}

#[must_use]
pub fn pathfinders_advance(base_advance: i64, has_relic: bool, depth: i64) -> i64 {
    base_advance + i64::from(has_relic && depth >= 150)
}

#[must_use]
pub fn diviners_risky_success_chance(base: f64, has_relic: bool, risky_choice: bool) -> f64 {
    (base + if has_relic && risky_choice { 0.10 } else { 0.0 }).min(1.0)
}

#[must_use]
pub fn prestige_four_bosses_at(depth: i32, prestige: i32) -> Vec<&'static BossDefinition> {
    boss_pool_for_tier(depth, prestige)
        .into_iter()
        .filter(|boss| PRESTIGE_FOUR_BOSS_IDS.contains(&boss.boss_id))
        .collect()
}

#[cfg(test)]
#[path = "dig_prestige4_content/tests.rs"]
mod tests;
