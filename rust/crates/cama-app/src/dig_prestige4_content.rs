//! Typed, Discord-neutral prestige-four Dig content policy.
//!
//! Existing boss, phase, mechanic, stinger, and artifact catalogs are reused
//! where they already match Python. This module adds signature-trophy metadata,
//! trophy combat effects, unique drop/equip orchestration, and the explicit
//! paused-duel → victory → optional-drop transaction sequence.

use std::collections::BTreeSet;

use cama_db::dig_prestige4_content::{
    ArtifactGrantOutcome, ArtifactRow, BossVictoryOutcome, BossVictoryRequest, ClaimedDuel,
    DigPrestige4Repository, DigPrestige4RepositoryError, EquipRelicOutcome, PrestigeContentKey,
    PrestigeTunnelSnapshot,
};

use crate::boss_multi_tier::{StingerDefinition, stinger_by_id};
use crate::dig_bosses::{
    BossDefinition, BossMechanic, RegularPhaseDefinition, boss_by_id, boss_pool_for_tier,
    default_regular_phase, mechanic, regular_phase,
};
use crate::dig_loot::{ArtifactDef, Rarity, artifact_catalog};
use crate::dig_service::layer_at;

pub const TROPHY_CARVE_RATE: f64 = 0.25;
pub const PRESTIGE_RELIC_DROP_RATE: f64 = 0.10;
pub const BASE_ARTIFACT_FIND_RATE: f64 = 0.005;
pub const RELIC_SLOT_BASE: i64 = 1;
pub const RELIC_SLOT_MAX: i64 = 7;

pub const PRESTIGE_FOUR_BOSS_IDS: [&str; 3] = ["blightcoil", "rimebound_king", "spineback"];
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
pub const PRESTIGE_FOUR_TROPHY_IDS: [&str; 5] = [
    "weeping_fang",
    "runebitten_shard",
    "aching_spine",
    "listening_shard",
    "hateborn_ember",
];
pub const PRESTIGE_FOUR_GENERAL_RELIC_IDS: [&str; 3] =
    ["deepveined_coal", "diviners_knot", "pathfinders_spur"];

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

pub trait Prestige4ContentPort {
    type Error;

    fn tunnel_snapshot(
        &self,
        key: PrestigeContentKey,
    ) -> Result<Option<PrestigeTunnelSnapshot>, Self::Error>;
    fn artifacts(&self, key: PrestigeContentKey) -> Result<Vec<ArtifactRow>, Self::Error>;
    fn grant_artifact_unique(
        &self,
        key: PrestigeContentKey,
        artifact_id: &str,
        is_relic: bool,
        found_at: i64,
    ) -> Result<ArtifactGrantOutcome, Self::Error>;
    fn equip_relic_unique(
        &self,
        key: PrestigeContentKey,
        database_id: i64,
        slot_base: i64,
        slot_max: i64,
    ) -> Result<EquipRelicOutcome, Self::Error>;
    fn claim_active_duel(
        &self,
        key: PrestigeContentKey,
    ) -> Result<Option<ClaimedDuel>, Self::Error>;
    fn complete_boss_victory(
        &self,
        request: BossVictoryRequest<'_>,
    ) -> Result<BossVictoryOutcome, Self::Error>;
}

impl Prestige4ContentPort for DigPrestige4Repository {
    type Error = DigPrestige4RepositoryError;

    fn tunnel_snapshot(
        &self,
        key: PrestigeContentKey,
    ) -> Result<Option<PrestigeTunnelSnapshot>, Self::Error> {
        DigPrestige4Repository::tunnel_snapshot(self, key)
    }

    fn artifacts(&self, key: PrestigeContentKey) -> Result<Vec<ArtifactRow>, Self::Error> {
        DigPrestige4Repository::artifacts(self, key)
    }

    fn grant_artifact_unique(
        &self,
        key: PrestigeContentKey,
        artifact_id: &str,
        is_relic: bool,
        found_at: i64,
    ) -> Result<ArtifactGrantOutcome, Self::Error> {
        DigPrestige4Repository::grant_artifact_unique(self, key, artifact_id, is_relic, found_at)
    }

    fn equip_relic_unique(
        &self,
        key: PrestigeContentKey,
        database_id: i64,
        slot_base: i64,
        slot_max: i64,
    ) -> Result<EquipRelicOutcome, Self::Error> {
        DigPrestige4Repository::equip_relic_unique(self, key, database_id, slot_base, slot_max)
    }

    fn claim_active_duel(
        &self,
        key: PrestigeContentKey,
    ) -> Result<Option<ClaimedDuel>, Self::Error> {
        DigPrestige4Repository::claim_active_duel(self, key)
    }

    fn complete_boss_victory(
        &self,
        request: BossVictoryRequest<'_>,
    ) -> Result<BossVictoryOutcome, Self::Error> {
        DigPrestige4Repository::complete_boss_victory(self, request)
    }
}

pub trait Prestige4Entropy {
    fn unit(&mut self) -> f64;
    fn choose_index(&mut self, upper_bound: usize) -> usize;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactDrop {
    pub id: &'static str,
    pub name: &'static str,
    pub rarity: Rarity,
}

#[derive(Debug)]
pub struct DigPrestige4Service<P, E> {
    port: P,
    entropy: E,
}

impl<P, E> DigPrestige4Service<P, E>
where
    P: Prestige4ContentPort,
    E: Prestige4Entropy,
{
    #[must_use]
    pub const fn new(port: P, entropy: E) -> Self {
        Self { port, entropy }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn maybe_carve_trophy(
        &mut self,
        key: PrestigeContentKey,
        boss_id: &str,
        now: i64,
    ) -> Result<Option<ArtifactDrop>, P::Error> {
        let Some(content) = prestige_boss_content(boss_id) else {
            return Ok(None);
        };
        if self
            .port
            .artifacts(key)?
            .iter()
            .any(|artifact| artifact.artifact_id == content.trophy_relic_id)
            || self.entropy.unit() >= TROPHY_CARVE_RATE
        {
            return Ok(None);
        }
        let Some(definition) = artifact_catalog()
            .into_iter()
            .find(|artifact| artifact.id == content.trophy_relic_id)
        else {
            return Ok(None);
        };
        match self
            .port
            .grant_artifact_unique(key, definition.id, definition.is_relic, now)?
        {
            ArtifactGrantOutcome::Granted { .. } => Ok(Some(ArtifactDrop {
                id: definition.id,
                name: definition.name,
                rarity: definition.rarity,
            })),
            ArtifactGrantOutcome::AlreadyOwned => Ok(None),
        }
    }

    pub fn maybe_drop_prestige_relic(
        &mut self,
        key: PrestigeContentKey,
        prestige_level: i64,
        now: i64,
    ) -> Result<Option<ArtifactDrop>, P::Error> {
        let owned = self
            .port
            .artifacts(key)?
            .into_iter()
            .map(|artifact| artifact.artifact_id)
            .collect::<BTreeSet<_>>();
        let eligible = artifact_catalog()
            .into_iter()
            .filter(|artifact| {
                artifact.is_relic
                    && artifact.min_prestige > 0
                    && artifact.min_prestige <= prestige_level
                    && !artifact.trophy
                    && !owned.contains(artifact.id)
            })
            .collect::<Vec<_>>();
        if eligible.is_empty() || self.entropy.unit() >= PRESTIGE_RELIC_DROP_RATE {
            return Ok(None);
        }
        let definition = eligible[self
            .entropy
            .choose_index(eligible.len())
            .min(eligible.len() - 1)];
        match self
            .port
            .grant_artifact_unique(key, definition.id, definition.is_relic, now)?
        {
            ArtifactGrantOutcome::Granted { .. } => Ok(Some(ArtifactDrop {
                id: definition.id,
                name: definition.name,
                rarity: definition.rarity,
            })),
            ArtifactGrantOutcome::AlreadyOwned => Ok(None),
        }
    }

    pub fn roll_artifact(
        &mut self,
        key: PrestigeContentKey,
        depth: i64,
        rate_modifier: f64,
        now: i64,
    ) -> Result<Option<ArtifactDrop>, P::Error> {
        if self.entropy.unit() >= BASE_ARTIFACT_FIND_RATE * rate_modifier.max(0.0) {
            return Ok(None);
        }
        let rarity = rarity_for_roll(self.entropy.unit());
        let owned = self
            .port
            .artifacts(key)?
            .into_iter()
            .map(|artifact| artifact.artifact_id)
            .collect::<BTreeSet<_>>();
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
        let eligible = if by_layer.is_empty() {
            all.into_iter()
                .filter(|artifact| {
                    is_ordinary_relic(*artifact)
                        && artifact.rarity == rarity
                        && !owned.contains(artifact.id)
                })
                .collect::<Vec<_>>()
        } else {
            by_layer
        };
        if eligible.is_empty() {
            return Ok(None);
        }
        let definition = eligible[self
            .entropy
            .choose_index(eligible.len())
            .min(eligible.len() - 1)];
        match self
            .port
            .grant_artifact_unique(key, definition.id, true, now)?
        {
            ArtifactGrantOutcome::Granted { .. } => Ok(Some(ArtifactDrop {
                id: definition.id,
                name: definition.name,
                rarity: definition.rarity,
            })),
            ArtifactGrantOutcome::AlreadyOwned => Ok(None),
        }
    }

    pub fn equip_relic(
        &self,
        key: PrestigeContentKey,
        database_id: i64,
    ) -> Result<EquipRelicOutcome, P::Error> {
        self.port
            .equip_relic_unique(key, database_id, RELIC_SLOT_BASE, RELIC_SLOT_MAX)
    }

    /// Claim first, commit victory second, roll trophy third. This ordering is
    /// intentionally visible and matches Python's non-atomic optional drop.
    pub fn resume_live_victory(
        &mut self,
        request: ResumeVictoryRequest<'_>,
    ) -> Result<ResumeVictoryOutcome, P::Error> {
        let Some(duel) = self.port.claim_active_duel(request.key)? else {
            return Ok(ResumeVictoryOutcome::MissingDuel);
        };
        if duel.boss_id != request.boss_id || duel.boss_hp > duel.player_damage {
            return Ok(ResumeVictoryOutcome::NotWon);
        }
        let Some(snapshot) = self.port.tunnel_snapshot(request.key)? else {
            return Ok(ResumeVictoryOutcome::MissingTunnel);
        };
        let victory = self.port.complete_boss_victory(BossVictoryRequest {
            expected: &snapshot,
            boss_id: request.boss_id,
            depth_after: duel.tier,
            max_depth_after: snapshot.max_depth.max(duel.tier),
            boss_progress_after: request.boss_progress_after,
            balance_delta: request.balance_delta,
            echo_window_seconds: 24 * 3_600,
            detail_json: request.detail_json,
            now: request.now,
        })?;
        let BossVictoryOutcome::Applied { .. } = victory else {
            return Ok(ResumeVictoryOutcome::Victory(victory));
        };
        let trophy = self.maybe_carve_trophy(request.key, request.boss_id, request.now)?;
        Ok(ResumeVictoryOutcome::Won { trophy })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResumeVictoryRequest<'a> {
    pub key: PrestigeContentKey,
    pub boss_id: &'a str,
    pub boss_progress_after: &'a str,
    pub balance_delta: i64,
    pub detail_json: &'a str,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeVictoryOutcome {
    Won { trophy: Option<ArtifactDrop> },
    Victory(BossVictoryOutcome),
    MissingDuel,
    MissingTunnel,
    NotWon,
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
