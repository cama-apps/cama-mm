//! Golden Wheel catalog calibration, eligibility, and repository delegation.
//!
//! This is additive migration policy. Python remains the live Discord and
//! schema authority. The shared Wheel catalog supplies authored wedges and
//! wheel kinds; this module adds server-state calibration and membership-aware
//! rank/wealth decisions while persistence stays behind typed ports.

use std::collections::BTreeSet;

use cama_domain::economy_scaling::scale_minigame_jc_delta;
use cama_domain::player::Player;

use crate::wheel::{
    WHEEL_GOLDEN_TOP_N, WheelEconomyPolicy, WheelKind, WheelMechanic, WheelValue, WheelWedge,
    get_wheel_wedges, select_wheel_kind, wheel_catalog,
};

pub const ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT: usize = i32::MAX as usize;
pub const HEIST_AVERAGE_RATE: f64 = 0.085;
pub const MARKET_CRASH_AVERAGE_RATE: f64 = 0.115;
pub const TRICKLE_DOWN_RATE_MIN: f64 = 0.02;
pub const TRICKLE_DOWN_RATE_MAX: f64 = 0.05;
pub const DIVIDEND_RATE: f64 = 0.005;
pub const RECESSION_TOP_RATE: f64 = 0.06;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveGoldenEconomy<'a> {
    pub spinner_balance: i64,
    pub other_top_balances: &'a [i64],
    pub rank_next_balance: Option<i64>,
    pub total_positive_balance: i64,
    pub bottom_player_balances: &'a [i64],
}

/// Estimate one special wedge using the same live-state formulas as Python.
#[must_use]
pub fn live_golden_mechanic_ev(mechanic: WheelMechanic, economy: LiveGoldenEconomy<'_>) -> f64 {
    match mechanic {
        WheelMechanic::RedShell | WheelMechanic::GreenShell => 0.0,
        WheelMechanic::BlueShell => -4.0,
        WheelMechanic::Heist => economy
            .bottom_player_balances
            .iter()
            .map(|balance| ((*balance as f64 * HEIST_AVERAGE_RATE) as i64).max(1))
            .sum::<i64>() as f64,
        WheelMechanic::MarketCrash => {
            if economy.other_top_balances.is_empty() {
                25.0
            } else {
                economy
                    .other_top_balances
                    .iter()
                    .map(|balance| ((*balance as f64 * MARKET_CRASH_AVERAGE_RATE) as i64).max(1))
                    .sum::<i64>() as f64
            }
        }
        WheelMechanic::CompoundInterest => 100.0,
        WheelMechanic::TrickleDown => {
            let others_positive = economy
                .total_positive_balance
                .saturating_sub(economy.spinner_balance.max(0))
                .max(0);
            (others_positive as f64 * ((TRICKLE_DOWN_RATE_MIN + TRICKLE_DOWN_RATE_MAX) / 2.0))
                as i64 as f64
        }
        WheelMechanic::Dividend => {
            ((economy.total_positive_balance as f64 * DIVIDEND_RATE) as i64).max(10) as f64
        }
        WheelMechanic::HostileTakeover => economy
            .rank_next_balance
            .filter(|value| *value != 0)
            .map_or(40.0, |balance| {
                ((balance as f64 * MARKET_CRASH_AVERAGE_RATE) as i64).max(1) as f64
            }),
        WheelMechanic::Recession => {
            -((economy.spinner_balance.max(0) as f64 * RECESSION_TOP_RATE) as i64) as f64
        }
        WheelMechanic::BananaPeel => -23.5,
        WheelMechanic::BombOmb => -52.5,
        _ => 0.0,
    }
}

/// Recalibrate the two OVEREXTENDED wedges from current guild wealth.
#[must_use]
pub fn compute_live_golden_wedges(
    economy: LiveGoldenEconomy<'_>,
    policy: WheelEconomyPolicy,
) -> Vec<WheelWedge> {
    let mut wedges = wheel_catalog(WheelKind::Golden, policy);
    let non_overextended_sum = wedges
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value >= 0 => Some(value),
            _ => None,
        })
        .sum::<i64>();
    let special_sum = wedges
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(mechanic) => Some(scale_minigame_jc_delta(
                live_golden_mechanic_ev(mechanic, economy),
                policy.minigame_scale,
            )),
            WheelValue::Numeric(_) => None,
        })
        .sum::<i64>();
    let overextended_count = wedges
        .iter()
        .filter(|wedge| matches!(wedge.value, WheelValue::Numeric(value) if value < 0))
        .count() as i64;
    let target_sum = scale_minigame_jc_delta(policy.golden_target_ev, policy.minigame_scale)
        .saturating_mul(i64::try_from(wedges.len()).expect("wheel length fits i64"));
    let overextended = if overextended_count == 0 {
        -1
    } else {
        (target_sum
            .saturating_sub(non_overextended_sum)
            .saturating_sub(special_sum)
            / overextended_count)
            .min(-1)
    };
    for wedge in &mut wedges {
        if matches!(wedge.value, WheelValue::Numeric(value) if value < 0) {
            wedge.label = overextended.to_string();
            wedge.value = WheelValue::Numeric(overextended);
        }
    }
    wedges
}

#[must_use]
pub fn live_golden_expected_value(
    wedges: &[WheelWedge],
    economy: LiveGoldenEconomy<'_>,
    policy: WheelEconomyPolicy,
) -> f64 {
    if wedges.is_empty() {
        return 0.0;
    }
    let sum = wedges
        .iter()
        .map(|wedge| match wedge.value {
            WheelValue::Numeric(value) => value,
            WheelValue::Mechanic(mechanic) => scale_minigame_jc_delta(
                live_golden_mechanic_ev(mechanic, economy),
                policy.minigame_scale,
            ),
        })
        .sum::<i64>();
    sum as f64 / wedges.len() as f64
}

#[must_use]
pub fn golden_wheel_wedges(policy: WheelEconomyPolicy) -> Vec<WheelWedge> {
    wheel_catalog(WheelKind::Golden, policy)
}

#[must_use]
pub fn wheel_wedges_for_flags(
    is_bankrupt: bool,
    is_golden: bool,
    policy: WheelEconomyPolicy,
) -> Vec<WheelWedge> {
    get_wheel_wedges(is_bankrupt, is_golden, policy)
}

#[must_use]
pub fn is_golden_eligible(spinner_id: i64, visible_leaderboard: &[Player], top_n: usize) -> bool {
    visible_leaderboard
        .iter()
        .take(top_n)
        .any(|player| player.discord_id == Some(spinner_id))
}

/// Live selection gives bankruptcy/bad-gamba status priority over wealth.
#[must_use]
pub fn select_live_wheel_kind(
    spinner_id: i64,
    balance: i64,
    bonus_spin: bool,
    is_bad_gamba: bool,
    visible_leaderboard: &[Player],
) -> WheelKind {
    if is_bad_gamba {
        return WheelKind::Bankrupt;
    }
    let visible_top_ids = visible_leaderboard
        .iter()
        .filter_map(|player| player.discord_id)
        .take(WHEEL_GOLDEN_TOP_N)
        .collect::<Vec<_>>();
    select_wheel_kind(spinner_id, balance, bonus_spin, &visible_top_ids)
}

#[must_use]
pub fn filter_visible_leaderboard(
    players: &[Player],
    guild_id: Option<i64>,
    member_ids: Option<&BTreeSet<i64>>,
    limit: usize,
) -> Vec<Player> {
    players
        .iter()
        .filter(|player| same_guild(player.guild_id, guild_id))
        .filter(|player| {
            member_ids.is_none_or(|members| {
                player
                    .discord_id
                    .is_some_and(|discord_id| members.contains(&discord_id))
            })
        })
        .take(limit)
        .cloned()
        .collect()
}

pub trait VisibleBalanceLeaderboardPort {
    type Error;

    fn balance_leaderboard(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Player>, Self::Error>;
}

pub fn load_visible_balance_leaderboard<P: VisibleBalanceLeaderboardPort>(
    port: &mut P,
    guild_id: Option<i64>,
    member_ids: Option<&BTreeSet<i64>>,
    limit: usize,
) -> Result<Vec<Player>, P::Error> {
    let candidate_limit = if member_ids.is_some() {
        ALL_GUILD_LEADERBOARD_ENTRIES_LIMIT
    } else {
        limit
    };
    let players = port.balance_leaderboard(guild_id, candidate_limit)?;
    Ok(filter_visible_leaderboard(
        &players, guild_id, member_ids, limit,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DividendResolution {
    pub total_positive_balance: i64,
    pub reward: i64,
}

#[must_use]
pub fn resolve_visible_dividend(
    players: &[Player],
    guild_id: Option<i64>,
    member_ids: Option<&BTreeSet<i64>>,
    minigame_scale: f64,
) -> DividendResolution {
    let total_positive_balance = players
        .iter()
        .filter(|player| same_guild(player.guild_id, guild_id))
        .filter(|player| {
            member_ids.is_none_or(|members| {
                player
                    .discord_id
                    .is_some_and(|discord_id| members.contains(&discord_id))
            })
        })
        .map(|player| player.jopacoin_balance.max(0))
        .fold(0_i64, i64::saturating_add);
    let base_reward = ((total_positive_balance as f64 * DIVIDEND_RATE) as i64).max(10);
    DividendResolution {
        total_positive_balance,
        reward: scale_minigame_jc_delta(base_reward as f64, minigame_scale),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GoldenSpinRequest {
    pub discord_id: i64,
    pub guild_id: Option<i64>,
    pub result: i64,
    pub spin_time: i64,
    pub is_golden: bool,
}

pub trait GoldenWheelRepositoryPort {
    type Error;

    fn get_leaderboard_bottom(
        &self,
        guild_id: Option<i64>,
        limit: usize,
        min_balance: i64,
    ) -> Result<Vec<Player>, Self::Error>;

    fn get_total_positive_balance(&self, guild_id: Option<i64>) -> Result<i64, Self::Error>;

    fn log_wheel_spin(&self, spin: GoldenSpinRequest) -> Result<i64, Self::Error>;
}

#[derive(Clone, Debug)]
pub struct GoldenWheelService<P> {
    repository: P,
}

impl<P> GoldenWheelService<P>
where
    P: GoldenWheelRepositoryPort,
{
    #[must_use]
    pub const fn new(repository: P) -> Self {
        Self { repository }
    }

    pub fn get_leaderboard_bottom(
        &self,
        guild_id: Option<i64>,
        limit: usize,
        min_balance: i64,
    ) -> Result<Vec<Player>, P::Error> {
        self.repository
            .get_leaderboard_bottom(guild_id, limit, min_balance)
    }

    pub fn get_total_positive_balance(&self, guild_id: Option<i64>) -> Result<i64, P::Error> {
        self.repository.get_total_positive_balance(guild_id)
    }

    pub fn log_wheel_spin(&self, spin: GoldenSpinRequest) -> Result<i64, P::Error> {
        self.repository.log_wheel_spin(spin)
    }

    #[must_use]
    pub const fn repository(&self) -> &P {
        &self.repository
    }
}

impl GoldenWheelRepositoryPort for cama_db::golden_wheel_repository::GoldenWheelRepository {
    type Error = cama_db::golden_wheel_repository::GoldenWheelRepositoryError;

    fn get_leaderboard_bottom(
        &self,
        guild_id: Option<i64>,
        limit: usize,
        min_balance: i64,
    ) -> Result<Vec<Player>, Self::Error> {
        cama_db::golden_wheel_repository::GoldenWheelRepository::get_leaderboard_bottom(
            self,
            guild_id,
            limit,
            min_balance,
        )
        .map(|rows| {
            rows.into_iter()
                .map(|row| Player {
                    discord_id: Some(row.discord_id),
                    guild_id: Some(row.guild_id),
                    wins: row.wins,
                    glicko_rating: row.glicko_rating,
                    jopacoin_balance: row.balance,
                    ..Player::new(row.username)
                })
                .collect()
        })
    }

    fn get_total_positive_balance(&self, guild_id: Option<i64>) -> Result<i64, Self::Error> {
        cama_db::golden_wheel_repository::GoldenWheelRepository::get_total_positive_balance(
            self, guild_id,
        )
    }

    fn log_wheel_spin(&self, spin: GoldenSpinRequest) -> Result<i64, Self::Error> {
        cama_db::golden_wheel_repository::GoldenWheelRepository::log_wheel_spin(
            self,
            cama_db::golden_wheel_repository::GoldenSpinLog {
                discord_id: spin.discord_id,
                guild_id: spin.guild_id,
                result: spin.result,
                spin_time: spin.spin_time,
                is_golden: spin.is_golden,
            },
        )
    }
}

const fn normalize_guild_id(guild_id: Option<i64>) -> i64 {
    match guild_id {
        Some(guild_id) => guild_id,
        None => 0,
    }
}

fn same_guild(row_guild_id: Option<i64>, requested_guild_id: Option<i64>) -> bool {
    normalize_guild_id(row_guild_id) == normalize_guild_id(requested_guild_id)
}

#[cfg(test)]
mod tests;
