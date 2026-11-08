//! Discord-neutral betting payloads and automatic spectator policy.
//!
//! SQLite placement/settlement lives in
//! `cama_db::betting_service_repository`; nonprofit seed accounting remains in
//! [`crate::dota_bet_seed`]. This module models shuffle/pending payloads and
//! deterministic automatic spectator selection.

use std::collections::BTreeSet;

use cama_db::dota_bet_seed_repository::BettingTeam;

pub type DiscordId = i64;

pub const DEFAULT_WIN_REWARD: i64 = 10;
pub const DEFAULT_VANITY_TAX_BASIS_POINTS: i64 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Team {
    pub player_ids: Vec<DiscordId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShuffleResult {
    pub radiant_team: Team,
    pub dire_team: Team,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RatingSystem {
    OpenSkill,
    Glicko,
    Jopacoin,
}

impl RatingSystem {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenSkill => "openskill",
            Self::Glicko => "glicko",
            Self::Jopacoin => "jopacoin",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingMatchState {
    pub radiant_team_ids: Vec<DiscordId>,
    pub dire_team_ids: Vec<DiscordId>,
    pub shuffle_timestamp: i64,
    pub bet_lock_until: i64,
    pub is_draft: bool,
    pub is_bomb_pot: bool,
    pub is_openskill_shuffle: bool,
    pub balancing_rating_system: RatingSystem,
}

impl PendingMatchState {
    #[must_use]
    pub fn to_payload_json(&self) -> String {
        format!(
            "{{\"radiant_team_ids\":{},\"dire_team_ids\":{},\"shuffle_timestamp\":{},\"bet_lock_until\":{},\"is_draft\":{},\"is_bomb_pot\":{},\"is_openskill_shuffle\":{},\"balancing_rating_system\":\"{}\"}}",
            json_i64_array(&self.radiant_team_ids),
            json_i64_array(&self.dire_team_ids),
            self.shuffle_timestamp,
            self.bet_lock_until,
            self.is_draft,
            self.is_bomb_pot,
            self.is_openskill_shuffle,
            self.balancing_rating_system.as_str(),
        )
    }
}

#[must_use]
pub fn typed_shuffle(
    players: &[DiscordId],
    shuffle_timestamp: i64,
) -> (ShuffleResult, PendingMatchState) {
    let middle = players.len() / 2;
    let radiant = players[..middle].to_vec();
    let dire = players[middle..].to_vec();
    (
        ShuffleResult {
            radiant_team: Team {
                player_ids: radiant.clone(),
            },
            dire_team: Team {
                player_ids: dire.clone(),
            },
        },
        PendingMatchState {
            radiant_team_ids: radiant,
            dire_team_ids: dire,
            shuffle_timestamp,
            bet_lock_until: shuffle_timestamp.saturating_add(900),
            is_draft: false,
            is_bomb_pot: false,
            is_openskill_shuffle: false,
            balancing_rating_system: RatingSystem::Glicko,
        },
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WealthSnapshot {
    pub discord_id: DiscordId,
    pub balance: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomaticSpectatorConfig {
    pub enabled: bool,
    pub total_count: usize,
    pub top_count: usize,
    pub base_basis_points: i64,
    pub top_basis_points: i64,
}

impl Default for AutomaticSpectatorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            total_count: 10,
            top_count: 5,
            base_basis_points: 100,
            top_basis_points: 200,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutomaticSpectatorBet {
    pub discord_id: DiscordId,
    pub team: BettingTeam,
    pub amount: i64,
    pub networth: i64,
    pub basis_points: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomaticSpectatorPlan {
    pub bets: Vec<AutomaticSpectatorBet>,
    pub top_count: usize,
    pub total_count: usize,
    pub base_basis_points: i64,
    pub top_basis_points: i64,
    pub total_radiant: i64,
    pub total_dire: i64,
}

/// Select the richest nonparticipants and greedily balance their deterministic
/// automatic wagers. The output is an explicit plan for the DB batch port;
/// planning itself performs no persistence or RNG calls.
#[must_use]
pub fn plan_automatic_spectator_bets(
    candidates: &[WealthSnapshot],
    participant_ids: &BTreeSet<DiscordId>,
    config: AutomaticSpectatorConfig,
) -> AutomaticSpectatorPlan {
    let top_count = if config.top_basis_points > 0 {
        config.top_count.min(config.total_count)
    } else {
        0
    };
    let mut plan = AutomaticSpectatorPlan {
        bets: Vec::new(),
        top_count,
        total_count: config.total_count,
        base_basis_points: config.base_basis_points,
        top_basis_points: config.top_basis_points,
        total_radiant: 0,
        total_dire: 0,
    };
    if !config.enabled
        || config.total_count == 0
        || config.base_basis_points.max(config.top_basis_points) <= 0
    {
        return plan;
    }
    let mut spectators = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.balance > 0)
        .filter(|candidate| !participant_ids.contains(&candidate.discord_id))
        .collect::<Vec<_>>();
    spectators.sort_by(|left, right| {
        right
            .balance
            .cmp(&left.balance)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });

    for spectator in spectators {
        if plan.bets.len() >= config.total_count {
            break;
        }
        let basis_points = if plan.bets.len() < top_count {
            config.top_basis_points
        } else {
            config.base_basis_points
        };
        let amount = rounded_basis_points(spectator.balance, basis_points);
        if amount < 1 {
            continue;
        }
        let radiant_difference = plan
            .total_radiant
            .saturating_add(amount)
            .saturating_sub(plan.total_dire)
            .abs();
        let dire_difference = plan
            .total_dire
            .saturating_add(amount)
            .saturating_sub(plan.total_radiant)
            .abs();
        let team = if radiant_difference <= dire_difference {
            BettingTeam::Radiant
        } else {
            BettingTeam::Dire
        };
        match team {
            BettingTeam::Radiant => {
                plan.total_radiant = plan.total_radiant.saturating_add(amount);
            }
            BettingTeam::Dire => {
                plan.total_dire = plan.total_dire.saturating_add(amount);
            }
        }
        plan.bets.push(AutomaticSpectatorBet {
            discord_id: spectator.discord_id,
            team,
            amount,
            networth: spectator.balance,
            basis_points,
        });
    }
    plan
}

fn rounded_basis_points(value: i64, basis_points: i64) -> i64 {
    let numerator = value.saturating_mul(basis_points);
    let quotient = numerator / 10_000;
    let remainder = numerator % 10_000;
    let absolute_remainder = remainder.abs();
    let adjustment =
        if absolute_remainder > 5_000 || (absolute_remainder == 5_000 && quotient % 2 != 0) {
            numerator.signum()
        } else {
            0
        };
    quotient.saturating_add(adjustment)
}

fn json_i64_array(values: &[i64]) -> String {
    let body = values
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

#[cfg(test)]
#[path = "betting_service/tests.rs"]
mod tests;
