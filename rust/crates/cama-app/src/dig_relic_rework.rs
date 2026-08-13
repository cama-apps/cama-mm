//! Discord-neutral policy for the Dig relic-cap and proc rework.
//!
//! Python remains the live runtime, catalog, and persistence authority during
//! migration. This module reuses Rust's additive artifact catalog, injects all
//! entropy, and isolates only the relic effects exercised by the canonical
//! tranche. It does not open SQLite or compose Discord commands.

use std::collections::{BTreeSet, VecDeque};

use cama_domain::dig_stats::{MinerStats, miner_stat_effects};
use cama_domain::game_date::game_date_for_timestamp;

use crate::dig_loot::{ArtifactDef, artifact_catalog};
use crate::dig_service::{LuminosityLevel, PINNACLE_DEPTH, luminosity_level};

pub const RELIC_SLOTS_BASE: i64 = 1;
pub const RELIC_SLOTS_MAX: i64 = 6;

pub const NEW_RELIC_IDS: [&str; 8] = [
    "prism_heart",
    "mana_conduit",
    "bloodstone",
    "gamblers_charm",
    "vendetta_coin",
    "mentors_lantern",
    "stormcaller",
    "slow_drip",
];

pub const DORMANT_RELIC_IDS: [&str; 7] = [
    "root_network",
    "frozen_clock",
    "hollow_eye",
    "mycelium_link",
    "hollow_fang",
    "echo_lantern",
    "patient_stone",
];

pub const BUFF_FUN_RELIC_IDS: [&str; 7] = [
    "midas_splinter",
    "lucky_seam",
    "prospectors_streak",
    "first_light",
    "berserkers_mark",
    "gamblers_edge",
    "deaths_door",
];

pub const TROPHY_RELIC_IDS: [&str; 6] = [
    "weeping_fang",
    "runebitten_shard",
    "aching_spine",
    "listening_shard",
    "hateborn_ember",
    "deaths_door",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelicText {
    pub lore: &'static str,
    pub effect: &'static str,
}

const RELIC_TEXTS: [(&str, RelicText); 15] = [
    (
        "prism_heart",
        RelicText {
            lore: "A geode that drinks colour from the air. What it gives back depends on what you brought.",
            effect: "Bonus shifts with today's mana color",
        },
    ),
    (
        "mana_conduit",
        RelicText {
            lore: "A filament of half-fused crystal. When you spend deeply, a thread of it returns to your pocket.",
            effect: "Refund 25% of any tap-mana shop cost",
        },
    ),
    (
        "bloodstone",
        RelicText {
            lore: "It does not pulse. It bargains. Half the time it doubles your haul; the other half it's hungry.",
            effect: "Coin-flip per dig: +50% or -25% JC",
        },
    ),
    (
        "gamblers_charm",
        RelicText {
            lore: "Won, lost, won again. A talisman that pays you for surviving the bad rolls.",
            effect: "Survive a cave-in: gain 50% of would-be lost depth as JC",
        },
    ),
    (
        "vendetta_coin",
        RelicText {
            lore: "Two faces. Both yours. Both angry. It remembers every shove down the shaft.",
            effect: "Sabotaged → reflect 50% damage and gain JC",
        },
    ),
    (
        "mentors_lantern",
        RelicText {
            lore: "Old miners say a lantern shared lights two paths. It pays the truth of that out, in coin.",
            effect: "Helping another digger: both gain +10 JC",
        },
    ),
    (
        "stormcaller",
        RelicText {
            lore: "Threaded with copper veins. It hums on storm days and goes silent in fair weather.",
            effect: "Storm: +50% yield, no hazard. Sunny: +10%",
        },
    ),
    (
        "slow_drip",
        RelicText {
            lore: "A clay jar that fills itself a coin at a time when no one is looking.",
            effect: "Idle income: 0.5 JC/min while away (cap 100/day)",
        },
    ),
    (
        "midas_splinter",
        RelicText {
            lore: "A fleck of something that was never quite ore. It warms when the digging is good.",
            effect: "Sometimes the rock just *gives* (chance to double a dig's coin)",
        },
    ),
    (
        "lucky_seam",
        RelicText {
            lore: "Old hands swear the mountain keeps one good secret for every thousand it buries.",
            effect: "Most veins are ore; one in a great many is a fortune (rare jackpot strike)",
        },
    ),
    (
        "prospectors_streak",
        RelicText {
            lore: "A tally-stone worn smooth by a thumb that counted every safe step — until it didn't.",
            effect: "Confidence compounds down here (more coin the longer you avoid a collapse; lose it all if one hits)",
        },
    ),
    (
        "first_light",
        RelicText {
            lore: "It holds a sliver of dawn that never fully goes out — brightest before anyone else has stirred.",
            effect: "The first swing of the day rings truest (your first dig each day pays more)",
        },
    ),
    (
        "berserkers_mark",
        RelicText {
            lore: "A brand that drinks pain and asks for more. It was cut into someone once, against their will.",
            effect: "It drinks your bruises and hits back (more damage to bosses the more you've been hit)",
        },
    ),
    (
        "gamblers_edge",
        RelicText {
            lore: "A coin filed to a blade on one edge. It has never once landed the way you feared.",
            effect: "The dice like you in the dark (your hits sometimes land double against a boss)",
        },
    ),
    (
        "deaths_door",
        RelicText {
            lore: "You have died here before. You got up. The Nameless Depth remembers, and resents it.",
            effect: "You got up last time (a chance to survive a killing blow)",
        },
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelicContent {
    pub definition: ArtifactDef,
    pub text: RelicText,
}

#[must_use]
pub fn relic_catalog() -> Vec<ArtifactDef> {
    artifact_catalog()
        .into_iter()
        .filter(|artifact| artifact.is_relic)
        .collect()
}

#[must_use]
pub fn relic_content(relic_id: &str) -> Option<RelicContent> {
    let definition = artifact_catalog()
        .into_iter()
        .find(|artifact| artifact.id == relic_id && artifact.is_relic)?;
    let text = RELIC_TEXTS
        .iter()
        .find_map(|(id, text)| (*id == relic_id).then_some(*text))?;
    Some(RelicContent { definition, text })
}

#[must_use]
pub fn trophy_relic_for_boss(boss_id: &str) -> Option<&'static str> {
    match boss_id {
        "blightcoil" => Some("weeping_fang"),
        "rimebound_king" => Some("runebitten_shard"),
        "spineback" => Some("aching_spine"),
        "xalatath" => Some("listening_shard"),
        "lilith" => Some("hateborn_ember"),
        "nameless_depth" => Some("deaths_door"),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RelicSet {
    ids: BTreeSet<String>,
}

impl RelicSet {
    #[must_use]
    pub fn new(ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            ids: ids.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, relic_id: &str) -> bool {
        self.ids.contains(relic_id)
    }
}

#[must_use]
pub fn post_pinnacle_decay_factor(depth: i64, relics: &RelicSet) -> f64 {
    if depth <= PINNACLE_DEPTH {
        return 1.0;
    }
    let steps_past = (depth - PINNACLE_DEPTH) / 25;
    let rate = if relics.contains("frozen_clock") {
        0.025
    } else if relics.contains("root_network") {
        0.0375
    } else {
        0.05
    };
    (1.0 - rate * steps_past as f64).max(0.0)
}

pub trait RelicEntropy {
    fn unit(&mut self) -> f64;
}

#[derive(Clone, Debug)]
pub struct ScriptedRelicEntropy {
    values: VecDeque<f64>,
    fallback: f64,
}

impl ScriptedRelicEntropy {
    #[must_use]
    pub fn new(values: impl IntoIterator<Item = f64>) -> Self {
        Self {
            values: values.into_iter().collect(),
            fallback: 1.0,
        }
    }

    #[must_use]
    pub fn constant(value: f64) -> Self {
        Self {
            values: VecDeque::new(),
            fallback: value,
        }
    }
}

impl RelicEntropy for ScriptedRelicEntropy {
    fn unit(&mut self) -> f64 {
        self.values.pop_front().unwrap_or(self.fallback)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct YieldContext<'a> {
    pub weather_code: Option<&'a str>,
    pub luminosity: Option<i64>,
    pub is_first_dig_today: bool,
    pub is_paid_dig: bool,
    pub include_random: bool,
}

#[must_use]
pub fn relic_jc_yield_multiplier(
    relics: &RelicSet,
    context: YieldContext<'_>,
    entropy: &mut impl RelicEntropy,
) -> f64 {
    let mut multiplier = 1.0;
    if relics.contains("echo_lantern") {
        multiplier *= 1.15;
    }
    if context.include_random && relics.contains("bloodstone") {
        multiplier *= if entropy.unit() < 0.5 { 1.5 } else { 0.75 };
    }
    if relics.contains("stormcaller") {
        match context
            .weather_code
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "storm" => multiplier *= 1.5,
            "sunny" => multiplier *= 1.10,
            _ => {}
        }
    }
    if context.luminosity.is_some_and(|luminosity| {
        matches!(
            luminosity_level(luminosity),
            LuminosityLevel::Dark | LuminosityLevel::PitchBlack
        )
    }) && relics.contains("deepveined_coal")
    {
        multiplier *= 1.20;
    }
    if context.include_random && relics.contains("midas_splinter") && entropy.unit() < 0.04 {
        multiplier *= 2.0;
    }
    if context.include_random && relics.contains("lucky_seam") && entropy.unit() < 0.005 {
        multiplier *= 10.0;
    }
    if context.is_first_dig_today && relics.contains("first_light") {
        multiplier *= 2.0;
    }
    if context.is_paid_dig && relics.contains("bone_abacus") {
        multiplier *= 0.90;
    }
    multiplier
}

#[must_use]
pub fn storm_negates_hazard(relics: &RelicSet, weather_code: Option<&str>) -> bool {
    weather_code.is_some_and(|weather| weather.eq_ignore_ascii_case("storm"))
        && relics.contains("stormcaller")
}

#[must_use]
pub fn relic_slot_cap(prestige: i64) -> i64 {
    prestige
        .saturating_add(RELIC_SLOTS_BASE)
        .min(RELIC_SLOTS_MAX)
}

const GAME_DAY_BOUNDARY_UTC_SECONDS: i64 = 12 * 3_600;
const SECONDS_PER_DAY: i64 = 86_400;

#[must_use]
pub fn game_day_index(timestamp: i64) -> i64 {
    timestamp
        .saturating_sub(GAME_DAY_BOUNDARY_UTC_SECONDS)
        .div_euclid(SECONDS_PER_DAY)
}

#[must_use]
pub fn is_first_dig_of_day(last_dig_at: Option<i64>, now: i64) -> bool {
    last_dig_at.is_none_or(|last| game_day_index(last) != game_day_index(now))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LanternStubRestoreInput<'a> {
    /// Luminosity after the ordinary dig drain/recovery policy has run.
    pub luminosity_after: i64,
    pub last_dig_at: Option<i64>,
    pub lantern_stub_date: Option<&'a str>,
    pub today: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanternStubRestore {
    pub luminosity_after: i64,
    pub restored: i64,
    /// Date the caller must persist with the luminosity update. `None` means
    /// the policy was a no-op and no idempotency stamp should be written.
    pub lantern_stub_date: Option<String>,
}

/// Apply Lantern Stub's first-dig-of-the-game-day restoration policy.
///
/// Persistence remains outside this pure helper. When `restored` is positive,
/// callers must commit `luminosity_after` and `lantern_stub_date` together;
/// the existing SQLite adapter's date claim then prevents a failed-dig retry
/// from restoring twice while `last_dig_at` is still stale.
#[must_use]
pub fn apply_lantern_stub_restore(
    relics: &RelicSet,
    input: LanternStubRestoreInput<'_>,
) -> LanternStubRestore {
    let unchanged = || LanternStubRestore {
        luminosity_after: input.luminosity_after,
        restored: 0,
        lantern_stub_date: None,
    };
    let first_dig_today = input.last_dig_at.is_none_or(|last_dig_at| {
        last_dig_at == 0
            || game_date_for_timestamp(last_dig_at as f64)
                .is_ok_and(|last_game_date| last_game_date != input.today)
    });
    if !relics.contains("lantern_stub")
        || !first_dig_today
        || input.lantern_stub_date == Some(input.today)
    {
        return unchanged();
    }
    let restored = 100_i64.saturating_sub(input.luminosity_after).clamp(0, 5);
    if restored == 0 {
        return unchanged();
    }
    LanternStubRestore {
        luminosity_after: input.luminosity_after.saturating_add(restored),
        restored,
        lantern_stub_date: Some(input.today.to_owned()),
    }
}

/// Compose the post-ascension paid-dig cost with Stamina and Bone Abacus.
///
/// Python first applies the ascension markup, then calculates the ordinary
/// Stamina discount. Bone Abacus strengthens that discount by 25%; it does
/// not multiply the already-discounted cost by a separate flat factor.
#[must_use]
pub fn relic_aware_paid_cost(post_ascension_cost: i64, stamina: i64, relics: &RelicSet) -> i64 {
    let effects = miner_stat_effects(
        MinerStats::new(0, 0, stamina.max(0)).expect("normalized stamina is non-negative"),
    );
    let mut multiplier = effects.paid_cost_multiplier;
    if relics.contains("bone_abacus") {
        let stamina_discount = 1.0 - multiplier;
        multiplier = 1.0 - stamina_discount * 1.25;
    }
    ((post_ascension_cost as f64 * multiplier) as i64).max(1)
}

pub const SHIFTING_IDOL_PLAYER_HIT_CEILING: f64 = 0.90;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShiftingIdolBonus {
    Hp,
    Hit,
    Crit,
}

impl ShiftingIdolBonus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hp => "hp",
            Self::Hit => "hit",
            Self::Crit => "crit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShiftingIdolStats {
    pub player_hp: i64,
    pub player_hit: f64,
    pub crit_chance: f64,
    pub bonus: Option<ShiftingIdolBonus>,
}

/// Roll and apply Shifting Idol's stat bonus for one fresh boss attempt.
///
/// The helper is deliberately stateless: every invocation with the relic
/// consumes one entropy value and returns a newly selected HP/hit/crit bonus.
#[must_use]
pub fn apply_shifting_idol_stats(
    relics: &RelicSet,
    player_hp: i64,
    player_hit: f64,
    crit_chance: f64,
    entropy: &mut impl RelicEntropy,
) -> ShiftingIdolStats {
    let mut result = ShiftingIdolStats {
        player_hp,
        player_hit,
        crit_chance,
        bonus: None,
    };
    if !relics.contains("shifting_idol") {
        return result;
    }
    let roll = entropy.unit();
    let choice = if !roll.is_finite() || roll <= 0.0 {
        0
    } else if roll >= 1.0 {
        2
    } else {
        (roll * 3.0) as usize
    };
    match choice {
        0 => {
            result.player_hp = result.player_hp.saturating_add(1);
            result.bonus = Some(ShiftingIdolBonus::Hp);
        }
        1 => {
            result.player_hit = (result.player_hit + 0.05).min(SHIFTING_IDOL_PLAYER_HIT_CEILING);
            result.bonus = Some(ShiftingIdolBonus::Hit);
        }
        _ => {
            result.crit_chance = (result.crit_chance + 0.05).min(1.0);
            result.bonus = Some(ShiftingIdolBonus::Crit);
        }
    }
    result
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RelicCombatStatus {
    pub berserk_rage: Option<i64>,
    pub double_hit_enabled: bool,
    pub deaths_door_available: bool,
    pub burn_rounds_remaining: i64,
    pub bleed_rounds_remaining: i64,
}

#[must_use]
pub fn relic_combat_status(relics: &RelicSet) -> RelicCombatStatus {
    RelicCombatStatus {
        berserk_rage: relics.contains("berserkers_mark").then_some(0),
        double_hit_enabled: relics.contains("gamblers_edge"),
        deaths_door_available: relics.contains("deaths_door"),
        ..RelicCombatStatus::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RelicRoundInput {
    pub round_num: i64,
    pub player_hp: i64,
    pub boss_hp: i64,
    pub player_hit: f64,
    pub player_damage: i64,
    pub boss_hit: f64,
    pub boss_damage: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelicRoundTerminal {
    PlayerWon,
    PlayerLost,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RelicRoundEvents {
    pub round_num: i64,
    pub player_hit: bool,
    pub boss_hit: bool,
    pub double_hit: bool,
    pub deaths_door: bool,
    pub player_hp: i64,
    pub boss_hp: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelicRoundResult {
    pub events: RelicRoundEvents,
    pub player_hp: i64,
    pub boss_hp: i64,
    pub terminal: Option<RelicRoundTerminal>,
}

#[must_use]
pub fn run_relic_round(
    input: RelicRoundInput,
    status: &mut RelicCombatStatus,
    entropy: &mut impl RelicEntropy,
) -> RelicRoundResult {
    let mut events = RelicRoundEvents {
        round_num: input.round_num,
        ..RelicRoundEvents::default()
    };
    let mut player_hp = input.player_hp;
    let mut boss_hp = input.boss_hp;
    let hp_at_round_start = player_hp;

    if status.burn_rounds_remaining > 0 {
        player_hp -= 1;
        status.burn_rounds_remaining -= 1;
    }
    if status.bleed_rounds_remaining > 0 {
        player_hp -= 1;
        status.bleed_rounds_remaining -= 1;
    }
    if player_hp <= 0 && !try_deaths_door(status, entropy, &mut events, &mut player_hp) {
        events.player_hp = 0;
        events.boss_hp = boss_hp.max(0);
        return RelicRoundResult {
            events,
            player_hp,
            boss_hp,
            terminal: Some(RelicRoundTerminal::PlayerLost),
        };
    }

    let player_landed = entropy.unit() < input.player_hit;
    events.player_hit = player_landed;
    if player_landed {
        let mut damage = input.player_damage;
        if let Some(rage) = status.berserk_rage.filter(|rage| *rage > 0) {
            damage = damage.saturating_add(rage.min(2));
        }
        if status.double_hit_enabled && entropy.unit() < 0.10 {
            damage = damage.saturating_mul(2);
            events.double_hit = true;
        }
        boss_hp = boss_hp.saturating_sub(damage);
    }
    events.boss_hp = boss_hp.max(0);
    if boss_hp <= 0 {
        events.player_hp = player_hp.max(0);
        return RelicRoundResult {
            events,
            player_hp,
            boss_hp,
            terminal: Some(RelicRoundTerminal::PlayerWon),
        };
    }

    let boss_landed = entropy.unit() < input.boss_hit;
    events.boss_hit = boss_landed;
    if boss_landed {
        player_hp = player_hp.saturating_sub(input.boss_damage);
    }
    if let Some(rage) = status.berserk_rage.as_mut()
        && player_hp < hp_at_round_start
    {
        *rage = rage.saturating_add(1).min(2);
    }
    if player_hp <= 0 && !try_deaths_door(status, entropy, &mut events, &mut player_hp) {
        events.player_hp = 0;
        events.boss_hp = boss_hp.max(0);
        return RelicRoundResult {
            events,
            player_hp,
            boss_hp,
            terminal: Some(RelicRoundTerminal::PlayerLost),
        };
    }
    events.player_hp = player_hp.max(0);
    events.boss_hp = boss_hp.max(0);
    RelicRoundResult {
        events,
        player_hp,
        boss_hp,
        terminal: None,
    }
}

fn try_deaths_door(
    status: &mut RelicCombatStatus,
    entropy: &mut impl RelicEntropy,
    events: &mut RelicRoundEvents,
    player_hp: &mut i64,
) -> bool {
    if status.deaths_door_available && entropy.unit() < 0.40 {
        status.deaths_door_available = false;
        events.deaths_door = true;
        *player_hp = 1;
        true
    } else {
        false
    }
}

#[cfg(test)]
#[path = "dig_relic_rework/tests.rs"]
mod tests;
