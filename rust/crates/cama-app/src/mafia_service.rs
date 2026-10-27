//! Mafia DTOs shared with flavor text, plus the role-count table and payout
//! split used by the runtime Mafia provider. Game state and persistence live
//! in `cama_db::mafia_repository`.

use std::collections::BTreeMap;

pub type GuildId = i64;
pub type PlayerId = i64;
pub type GameId = u64;

pub const MVP_BONUS: i64 = 20;
pub const MAX_WINNER_PAYOUT: i64 = 50;
pub const BOOKIE_PAYOUT: i64 = MAX_WINNER_PAYOUT;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Phase {
    Setup,
    Night,
    Day,
    Resolved,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Role {
    Mafia,
    Doctor,
    Detective,
    Vigilante,
    Townie,
    Jester,
    Bookie,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Twist {
    BloodMoon,
    TownHall,
    MemoryFog,
    Plague,
    Resurrection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Winner {
    Town,
    Mafia,
    Jester,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Player {
    pub discord_id: PlayerId,
    pub guild_id: GuildId,
    pub role: Role,
    pub is_godfather: bool,
    pub hero_name: String,
    pub is_alive: bool,
    pub eliminated_phase: Option<Phase>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    pub game_id: GameId,
    pub guild_id: GuildId,
    pub game_date: String,
    pub phase: Phase,
    pub roster_size: usize,
    pub twist: Option<Twist>,
    pub day_number: u8,
    pub winner: Option<Winner>,
    pub entry_fee: i64,
    pub status: GameStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameStatus {
    Active,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoleCounts {
    pub mafia: usize,
    pub doctor: usize,
    pub detective: usize,
    pub vigilante: usize,
}

#[must_use]
pub const fn role_counts(roster_size: usize) -> RoleCounts {
    match roster_size {
        5 | 6 => RoleCounts {
            mafia: 1,
            doctor: 1,
            detective: 1,
            vigilante: 0,
        },
        7..=9 => RoleCounts {
            mafia: 2,
            doctor: 1,
            detective: 1,
            vigilante: 0,
        },
        10..=12 => RoleCounts {
            mafia: 3,
            doctor: 1,
            detective: 1,
            vigilante: 1,
        },
        13..=15 => RoleCounts {
            mafia: 4,
            doctor: 1,
            detective: 1,
            vigilante: 1,
        },
        _ => RoleCounts {
            mafia: 0,
            doctor: 0,
            detective: 0,
            vigilante: 0,
        },
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutPlan {
    pub deltas: BTreeMap<PlayerId, i64>,
    pub payout_per_winner: i64,
    pub nonprofit_overflow: i64,
}

#[must_use]
pub fn compute_payouts(
    pot_total: i64,
    winning_ids: &[PlayerId],
    mvp_id: Option<PlayerId>,
    bookie_id: Option<PlayerId>,
) -> PayoutPlan {
    let mut deltas = BTreeMap::new();
    let mut faction_pot = pot_total;
    if let Some(bookie) = bookie_id {
        let take = BOOKIE_PAYOUT.min(pot_total);
        deltas.insert(bookie, take);
        faction_pot -= take;
    }

    let mut base = 0;
    if !winning_ids.is_empty() {
        let mvp_wins = mvp_id.is_some_and(|id| winning_ids.contains(&id));
        let mvp_share = if mvp_wins {
            MVP_BONUS.min(faction_pot)
        } else {
            0
        };
        let remainder = faction_pot - mvp_share;
        base = remainder / i64::try_from(winning_ids.len()).unwrap_or(1);
        let dust = remainder - base * i64::try_from(winning_ids.len()).unwrap_or(1);
        for &winner in winning_ids {
            deltas.insert(winner, base);
        }
        if let Some(mvp) = mvp_id.filter(|id| winning_ids.contains(id)) {
            *deltas.entry(mvp).or_default() += mvp_share + dust;
        } else if let Some(first) = winning_ids.first() {
            *deltas.entry(*first).or_default() += dust;
        }
    }

    for amount in deltas.values_mut() {
        *amount = (*amount).min(MAX_WINNER_PAYOUT);
    }
    let distributed: i64 = deltas.values().sum();
    PayoutPlan {
        deltas,
        payout_per_winner: base.min(MAX_WINNER_PAYOUT),
        nonprofit_overflow: pot_total - distributed,
    }
}

#[cfg(test)]
#[path = "mafia_service/tests.rs"]
mod tests;
