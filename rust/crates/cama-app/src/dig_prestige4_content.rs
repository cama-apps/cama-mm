//! Typed, Discord-neutral prestige-four Dig content policy.
//!
//! Existing boss, phase, mechanic, stinger, and artifact catalogs are reused
//! where they already match Python. This module adds the ordinary artifact-roll
//! staging used by Dig.

use std::collections::BTreeSet;

use crate::dig_loot::{ArtifactDef, Rarity, artifact_catalog};
use crate::dig_service::layer_at;

pub const BASE_ARTIFACT_FIND_RATE: f64 = 0.005;

#[cfg(test)]
pub const BESPOKE_PHASE_BOSS_IDS: [&str; 5] = [
    "blightcoil",
    "rimebound_king",
    "spineback",
    "xalatath",
    "lilith",
];
#[cfg(test)]
pub const PRESTIGE_FOUR_MECHANIC_IDS: [&str; 6] = [
    "blightcoil_wards",
    "blightcoil_nova",
    "rimebound_harvest",
    "rimebound_raise",
    "spineback_regrowth",
    "spineback_divebomb",
];
#[cfg(test)]
pub const PRESTIGE_FOUR_STINGER_IDS: [&str; 3] =
    ["blightcoil_venom", "rimebound_soulchill", "spineback_rend"];
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

#[cfg(test)]
#[path = "dig_prestige4_content/tests.rs"]
mod tests;
