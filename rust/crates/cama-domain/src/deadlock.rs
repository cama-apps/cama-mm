//! Deadlock matchmaking policy. Independent mode ratings; no drafts or captains.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadlockFormat {
    #[default]
    StreetBrawl,
    Standard,
}

impl DeadlockFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StreetBrawl => "street_brawl",
            Self::Standard => "standard",
        }
    }
    pub const fn team_size(self) -> usize {
        match self {
            Self::StreetBrawl => 4,
            Self::Standard => 6,
        }
    }
    pub const fn player_count(self) -> usize {
        self.team_size() * 2
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::StreetBrawl => "Street Brawl 4v4",
            Self::Standard => "Standard 6v6",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeadlockPlayer {
    pub discord_id: i64,
    /// Steam32 account ID, matching the global player_steam_ids contract.
    pub steam_id: i64,
    pub mu: f64,
    pub sigma: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeadlockTeams {
    pub team1: Vec<DeadlockPlayer>,
    pub team2: Vec<DeadlockPlayer>,
}

/// Exhaustively evaluate unique partitions of an already selected FIFO roster.
/// At most 462 partitions for 6v6; deterministic ties preserve queue order.
pub fn balance(
    format: DeadlockFormat,
    players: &[DeadlockPlayer],
) -> Result<DeadlockTeams, &'static str> {
    balance_with_seed(format, players, 0)
}

/// Seeded reservoir sampling avoids a fixed first-player side or deterministic tied partition.
pub fn balance_with_seed(
    format: DeadlockFormat,
    players: &[DeadlockPlayer],
    seed: u64,
) -> Result<DeadlockTeams, &'static str> {
    let mut rng = fastrand::Rng::with_seed(seed);
    if players.len() != format.player_count() {
        return Err("incorrect roster size");
    }
    let mut ids = BTreeSet::new();
    let mut steam_ids = BTreeSet::new();
    for player in players {
        if player.discord_id <= 0
            || player.steam_id <= 0
            || !ids.insert(player.discord_id)
            || !steam_ids.insert(player.steam_id)
        {
            return Err("invalid or duplicate player identity");
        }
        if !player.mu.is_finite() || !player.sigma.is_finite() || player.sigma <= 0.0 {
            return Err("invalid player rating");
        }
    }
    let total: f64 = players.iter().map(|p| p.mu).sum();
    if !total.is_finite() {
        return Err("rating total exceeds supported range");
    }
    let mut best = (f64::INFINITY, 0_u32);
    let mut tied = 0_u64;
    for mask in 1_u32..(1_u32 << players.len()) {
        if mask & 1 == 0 || mask.count_ones() as usize != format.team_size() {
            continue;
        }
        let sum: f64 = players
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, p)| p.mu)
            .sum();
        let difference = (2.0 * sum - total).abs();
        if difference < best.0 {
            best = (difference, mask);
            tied = 1;
        } else if difference == best.0 {
            tied += 1;
            if rng.u64(..tied) == 0 {
                best.1 = mask;
            }
        }
    }
    if rng.bool() {
        best.1 ^= (1_u32 << players.len()) - 1;
    }
    let (team1, team2) = players
        .iter()
        .enumerate()
        .partition::<Vec<_>, _>(|(i, _)| best.1 & (1 << i) != 0);
    Ok(DeadlockTeams {
        team1: team1.into_iter().map(|(_, p)| p.clone()).collect(),
        team2: team2.into_iter().map(|(_, p)| p.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_formats_balance_exactly_and_reject_duplicate_accounts() {
        assert_eq!(DeadlockFormat::default().player_count(), 8);
        for format in [DeadlockFormat::StreetBrawl, DeadlockFormat::Standard] {
            let mut players: Vec<_> = (1..=format.player_count())
                .map(|id| DeadlockPlayer {
                    discord_id: id as i64,
                    steam_id: id as i64,
                    mu: id as f64,
                    sigma: 8.0,
                })
                .collect();
            let teams = balance(format, &players).unwrap();
            assert_eq!(teams.team1.len(), format.team_size());
            assert_eq!(
                teams.team1.iter().map(|p| p.mu).sum::<f64>(),
                teams.team2.iter().map(|p| p.mu).sum::<f64>()
            );
            players[1].steam_id = players[0].steam_id;
            assert!(balance(format, &players).is_err());
        }
    }
}
