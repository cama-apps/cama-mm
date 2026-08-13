//! Typed policy and orchestration for six deflationary Dig encounters.
//!
//! Python remains the live event catalog, Discord renderer, and runtime
//! authority during migration. This module mirrors the authored fields pinned
//! by parity tests, injects success/jitter/selection entropy, and delegates the
//! existing-schema state transition to a typed atomic persistence port.

use std::collections::BTreeSet;

use cama_db::dig_tunnel_encounters_repository::{
    DigTunnelEncounterRepository, DigTunnelEncounterRepositoryError, EncounterCandidateQuery,
    EncounterOutcome, EncounterSettlement, EncounterSettlementRequest, EncounterVictimStrategy,
};

use crate::dig_loot::{LootEntropy, Rarity};
use crate::dig_new_events::{SplashMode, SplashTrigger};

pub const ACTIVE_DIGGER_LOOKBACK_SECONDS: i64 = 7 * 86_400;
pub const RANDOM_ACTIVE_LOOKBACK_SECONDS: i64 = 14 * 86_400;
pub const NEGATIVE_EVENT_JC_MULTIPLIER: f64 = 1.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncounterSplashSpec {
    pub strategy: EncounterVictimStrategy,
    pub victim_count: usize,
    pub penalty_jc: i64,
    pub trigger: SplashTrigger,
    pub mode: SplashMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TunnelEncounterSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub rarity: Rarity,
    pub min_depth: i64,
    pub max_depth: Option<i64>,
    pub social: bool,
    pub risky_success_chance: f64,
    pub risky_success_jc: i64,
    pub risky_failure_jc: i64,
    pub risky_failure_cave_in: bool,
    pub splash: EncounterSplashSpec,
}

impl TunnelEncounterSpec {
    #[must_use]
    pub fn eligible_at_depth(self, depth: i64) -> bool {
        depth >= self.min_depth && self.max_depth.is_none_or(|maximum| depth <= maximum)
    }

    /// The authored burn must cover the maximum (+50%) reward jitter before
    /// runtime scaling. This guarantees every fully funded event deflates.
    #[must_use]
    pub fn burn_dominates_maximum_payout(self) -> bool {
        let authored_burn = self.splash.penalty_jc as f64 * self.splash.victim_count as f64;
        authored_burn >= self.risky_success_jc as f64 * 1.5
    }
}

const fn splash(
    strategy: EncounterVictimStrategy,
    victim_count: usize,
    penalty_jc: i64,
) -> EncounterSplashSpec {
    EncounterSplashSpec {
        strategy,
        victim_count,
        penalty_jc,
        trigger: SplashTrigger::Success,
        mode: SplashMode::Burn,
    }
}

pub const TUNNEL_ENCOUNTERS: [TunnelEncounterSpec; 6] = [
    TunnelEncounterSpec {
        id: "hungering_dark",
        name: "The Hungering Dark",
        rarity: Rarity::Uncommon,
        min_depth: 10,
        max_depth: None,
        social: true,
        risky_success_chance: 0.60,
        risky_success_jc: 5,
        risky_failure_jc: -5,
        risky_failure_cave_in: false,
        splash: splash(EncounterVictimStrategy::RichestN, 2, 5),
    },
    TunnelEncounterSpec {
        id: "deny_the_seam",
        name: "Deny the Seam",
        rarity: Rarity::Uncommon,
        min_depth: 20,
        max_depth: None,
        social: true,
        risky_success_chance: 0.60,
        risky_success_jc: 6,
        risky_failure_jc: -6,
        risky_failure_cave_in: false,
        splash: splash(EncounterVictimStrategy::RichestN, 2, 6),
    },
    TunnelEncounterSpec {
        id: "turf_war",
        name: "Turf War",
        rarity: Rarity::Uncommon,
        min_depth: 35,
        max_depth: None,
        social: true,
        risky_success_chance: 0.58,
        risky_success_jc: 8,
        risky_failure_jc: 0,
        risky_failure_cave_in: true,
        splash: splash(EncounterVictimStrategy::ActiveDiggers, 3, 6),
    },
    TunnelEncounterSpec {
        id: "smoke_ambush",
        name: "Smoke Ambush",
        rarity: Rarity::Rare,
        min_depth: 60,
        max_depth: None,
        social: true,
        risky_success_chance: 0.55,
        risky_success_jc: 10,
        risky_failure_jc: -10,
        risky_failure_cave_in: false,
        splash: splash(EncounterVictimStrategy::ActiveDiggers, 3, 7),
    },
    TunnelEncounterSpec {
        id: "the_tear",
        name: "The Tear",
        rarity: Rarity::Rare,
        min_depth: 90,
        max_depth: None,
        social: true,
        risky_success_chance: 0.50,
        risky_success_jc: 10,
        risky_failure_jc: 0,
        risky_failure_cave_in: false,
        splash: splash(EncounterVictimStrategy::RandomActive, 3, 8),
    },
    TunnelEncounterSpec {
        id: "the_deep_hunter",
        name: "The Deep Hunter",
        rarity: Rarity::Rare,
        min_depth: 110,
        max_depth: None,
        social: true,
        risky_success_chance: 0.50,
        risky_success_jc: 12,
        risky_failure_jc: 0,
        risky_failure_cave_in: true,
        splash: splash(EncounterVictimStrategy::DeepestN, 2, 12),
    },
];

#[must_use]
pub fn encounter(event_id: &str) -> Option<&'static TunnelEncounterSpec> {
    TUNNEL_ENCOUNTERS.iter().find(|event| event.id == event_id)
}

pub trait TunnelEncounterPersistence {
    type Error;

    fn candidate_ids(&self, query: EncounterCandidateQuery) -> Result<Vec<i64>, Self::Error>;

    fn settle_encounter(
        &self,
        request: &EncounterSettlementRequest,
    ) -> Result<EncounterSettlement, Self::Error>;
}

impl TunnelEncounterPersistence for DigTunnelEncounterRepository {
    type Error = DigTunnelEncounterRepositoryError;

    fn candidate_ids(&self, query: EncounterCandidateQuery) -> Result<Vec<i64>, Self::Error> {
        self.candidate_ids(query)
    }

    fn settle_encounter(
        &self,
        request: &EncounterSettlementRequest,
    ) -> Result<EncounterSettlement, Self::Error> {
        self.settle_encounter_atomic(request)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveTunnelEncounterError<E> {
    UnknownEvent(String),
    IneligibleDepth {
        event_id: &'static str,
        depth: i64,
        minimum: i64,
    },
    Persistence(E),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelEncounterResolution {
    pub event_id: &'static str,
    pub event_name: &'static str,
    pub failure_cave_in: bool,
    pub settlement: EncounterSettlement,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolveTunnelEncounterRequest<'a> {
    pub digger_discord_id: i64,
    pub guild_id: Option<i64>,
    pub event_id: &'a str,
    pub event_key: &'a str,
    pub depth: i64,
    pub minigame_jc_delta_scale: f64,
    pub now: i64,
}

pub fn resolve_risky_tunnel_encounter<P: TunnelEncounterPersistence>(
    persistence: &P,
    request: ResolveTunnelEncounterRequest<'_>,
    entropy: &mut impl LootEntropy,
) -> Result<TunnelEncounterResolution, ResolveTunnelEncounterError<P::Error>> {
    let Some(spec) = encounter(request.event_id) else {
        return Err(ResolveTunnelEncounterError::UnknownEvent(
            request.event_id.to_owned(),
        ));
    };
    if !spec.eligible_at_depth(request.depth) {
        return Err(ResolveTunnelEncounterError::IneligibleDepth {
            event_id: spec.id,
            depth: request.depth,
            minimum: spec.min_depth,
        });
    }

    let succeeded = entropy.unit().clamp(0.0, 1.0) < spec.risky_success_chance;
    let (outcome, active_since) = if succeeded {
        let jittered_reward = jitter(spec.risky_success_jc, entropy);
        let active_since = request.now.saturating_sub(match spec.splash.strategy {
            EncounterVictimStrategy::ActiveDiggers => ACTIVE_DIGGER_LOOKBACK_SECONDS,
            EncounterVictimStrategy::RandomActive => RANDOM_ACTIVE_LOOKBACK_SECONDS,
            EncounterVictimStrategy::RichestN | EncounterVictimStrategy::DeepestN => 0,
        });
        let limit = match spec.splash.strategy {
            EncounterVictimStrategy::ActiveDiggers => Some(
                spec.splash
                    .victim_count
                    .saturating_mul(4)
                    .max(spec.splash.victim_count.saturating_add(8)),
            ),
            EncounterVictimStrategy::RandomActive => None,
            EncounterVictimStrategy::RichestN | EncounterVictimStrategy::DeepestN => {
                Some(spec.splash.victim_count)
            }
        };
        let mut candidates = persistence
            .candidate_ids(EncounterCandidateQuery {
                guild_id: request.guild_id,
                digger_discord_id: request.digger_discord_id,
                strategy: spec.splash.strategy,
                active_since,
                limit,
            })
            .map_err(ResolveTunnelEncounterError::Persistence)?;
        let mut seen = BTreeSet::new();
        candidates
            .retain(|candidate| *candidate != request.digger_discord_id && seen.insert(*candidate));
        let preferred_victim_ids = if spec.splash.strategy.uses_random_sample() {
            sample_without_replacement(candidates, spec.splash.victim_count, entropy)
        } else {
            Vec::new()
        };
        (
            EncounterOutcome::Success {
                strategy: spec.splash.strategy,
                victim_count: spec.splash.victim_count,
                authored_penalty_jc: spec.splash.penalty_jc,
                jittered_authored_reward_jc: jittered_reward,
                preferred_victim_ids,
            },
            active_since,
        )
    } else {
        let tuned_loss = if spec.risky_failure_jc == 0 {
            0
        } else {
            let multiplied =
                round_half_even(spec.risky_failure_jc as f64 * NEGATIVE_EVENT_JC_MULTIPLIER);
            jitter(multiplied, entropy)
        };
        (
            EncounterOutcome::Failure {
                tuned_authored_loss_jc: tuned_loss,
            },
            request.now,
        )
    };

    let settlement = persistence
        .settle_encounter(&EncounterSettlementRequest {
            digger_discord_id: request.digger_discord_id,
            guild_id: request.guild_id,
            event_id: spec.id.to_owned(),
            event_name: spec.name.to_owned(),
            event_key: request.event_key.to_owned(),
            outcome,
            minigame_jc_delta_scale: request.minigame_jc_delta_scale,
            active_since,
            now: request.now,
        })
        .map_err(ResolveTunnelEncounterError::Persistence)?;

    Ok(TunnelEncounterResolution {
        event_id: spec.id,
        event_name: spec.name,
        failure_cave_in: !settlement.succeeded && spec.risky_failure_cave_in,
        settlement,
    })
}

fn jitter(authored: i64, entropy: &mut impl LootEntropy) -> i64 {
    if authored == 0 {
        return 0;
    }
    let multiplier = 0.5 + entropy.unit().clamp(0.0, 1.0);
    round_half_even(authored as f64 * multiplier)
}

fn round_half_even(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if fraction < 0.5 {
        floor as i64
    } else if fraction > 0.5 {
        (floor + 1.0) as i64
    } else {
        let lower = floor as i64;
        if lower % 2 == 0 { lower } else { lower + 1 }
    }
}

fn sample_without_replacement(
    mut candidates: Vec<i64>,
    victim_count: usize,
    entropy: &mut impl LootEntropy,
) -> Vec<i64> {
    let mut selected = Vec::with_capacity(victim_count.min(candidates.len()));
    while !candidates.is_empty() && selected.len() < victim_count {
        let index = entropy.choose_index(candidates.len());
        selected.push(candidates.swap_remove(index));
    }
    selected
}

#[cfg(test)]
#[path = "dig_tunnel_encounters/tests.rs"]
mod tests;
