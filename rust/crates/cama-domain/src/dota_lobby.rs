//! Dota lobby admission and launch policy, independent of Steam and persistence.

use std::collections::BTreeSet;

pub const STEAM_INDIVIDUAL_BASE: u64 = 76_561_197_960_265_728;

/// Valve's game-rules states are not ordered: map loading is 10 and team
/// showcase is 8, while playable pregame is 4. Only these explicit states
/// confirm that the hero draft and its preparation screens have finished.
pub fn gameplay_has_started(game_state: i32) -> bool {
    matches!(game_state, 4..=6)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Radiant,
    Dire,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpectedPlayer {
    pub discord_id: i64,
    pub account_id: u32,
    pub side: Side,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LobbySeat {
    pub account_id: u32,
    pub side: Option<Side>,
    /// Zero-based player slot (0..5), normalized by the transport adapter.
    pub slot: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Admission {
    pub missing: Vec<u32>,
    pub wrong_side: Vec<u32>,
    pub unexpected_players: Vec<u32>,
    pub ready: bool,
}

pub fn validate_roster(players: &[ExpectedPlayer], bot_account: u32) -> Result<(), &'static str> {
    if players.len() != 10 || players.iter().filter(|p| p.side == Side::Radiant).count() != 5 {
        return Err("hosting requires five Radiant and five Dire players");
    }
    if players.iter().any(|p| {
        p.discord_id <= 0
            || p.account_id == 0
            || p.account_id == u32::MAX
            || p.account_id == bot_account
    }) || players
        .iter()
        .map(|p| p.discord_id)
        .collect::<BTreeSet<_>>()
        .len()
        != 10
        || players
            .iter()
            .map(|p| p.account_id)
            .collect::<BTreeSet<_>>()
            .len()
            != 10
    {
        return Err("hosting requires ten distinct linked player accounts, excluding the host");
    }
    Ok(())
}

/// Human clients choose their own team; the host may return a wrong-side player
/// to the player pool. All ten must be observed in distinct playable slots.
pub fn assess_admission(players: &[ExpectedPlayer], seats: &[LobbySeat], bot: u32) -> Admission {
    let mut result = Admission::default();
    for expected in players {
        let matches = seats
            .iter()
            .filter(|s| s.account_id == expected.account_id)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [seat] if seat.side == Some(expected.side) && seat.slot < 5 => {}
            [] => result.missing.push(expected.account_id),
            _ => result.wrong_side.push(expected.account_id),
        }
    }
    for seat in seats.iter().filter(|s| s.side.is_some()) {
        if seat.account_id == bot || !players.iter().any(|p| p.account_id == seat.account_id) {
            result.unexpected_players.push(seat.account_id);
        }
    }
    let mut slots = BTreeSet::new();
    let distinct_slots = seats
        .iter()
        .filter(|s| s.side.is_some())
        .all(|s| slots.insert((s.side == Some(Side::Dire), s.slot)));
    result.ready = validate_roster(players, bot).is_ok()
        && result.missing.is_empty()
        && result.wrong_side.is_empty()
        && result.unexpected_players.is_empty()
        && distinct_slots;
    result
}

/// Stored account links use Dota's 32-bit account ID, never an unchecked cast.
pub fn account_id(value: i64) -> Option<u32> {
    let value = u64::try_from(value).ok()?;
    let account = if value >= STEAM_INDIVIDUAL_BASE {
        value - STEAM_INDIVIDUAL_BASE
    } else {
        value
    };
    u32::try_from(account)
        .ok()
        .filter(|id| *id > 0 && *id < u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gameplay_start_excludes_draft_strategy_and_unknown_states() {
        for state in [-1, 0, 1, 2, 3, 7, 8, 9, 10, 11, 12, 13, 99] {
            assert!(
                !gameplay_has_started(state),
                "state {state} is not playable"
            );
        }
        for state in [4, 5, 6] {
            assert!(gameplay_has_started(state));
        }
    }

    fn roster() -> Vec<ExpectedPlayer> {
        (1..=10)
            .map(|id| ExpectedPlayer {
                discord_id: i64::from(id),
                account_id: id,
                side: if id <= 5 { Side::Radiant } else { Side::Dire },
            })
            .collect()
    }

    #[test]
    fn teammates_can_choose_any_playable_slot() {
        let players = roster();
        // Rotate each side independently so every player occupies every slot.
        for radiant_offset in 0..5 {
            for dire_offset in 0..5 {
                let seats = players
                    .iter()
                    .enumerate()
                    .map(|(index, player)| {
                        let offset = match player.side {
                            Side::Radiant => radiant_offset,
                            Side::Dire => dire_offset,
                        };
                        LobbySeat {
                            account_id: player.account_id,
                            side: Some(player.side),
                            slot: ((index + offset) % 5) as u32,
                        }
                    })
                    .collect::<Vec<_>>();
                let admission = assess_admission(&players, &seats, 20);
                assert!(admission.ready);
                assert!(admission.wrong_side.is_empty());
            }
        }
    }

    #[test]
    fn launch_requires_exact_correct_roster_and_host_out_of_players() {
        let players = roster();
        let mut seats = players
            .iter()
            .enumerate()
            .map(|(index, p)| LobbySeat {
                account_id: p.account_id,
                side: Some(p.side),
                slot: (index % 5) as u32,
            })
            .collect::<Vec<_>>();
        assert!(assess_admission(&players, &seats, 20).ready);
        seats[0].side = Some(Side::Dire);
        assert_eq!(assess_admission(&players, &seats, 20).wrong_side, vec![1]);
        seats[0].side = Some(Side::Radiant);
        seats.push(LobbySeat {
            account_id: 20,
            side: Some(Side::Radiant),
            slot: 5,
        });
        assert!(!assess_admission(&players, &seats, 20).ready);
        seats.last_mut().unwrap().side = None;
        assert!(assess_admission(&players, &seats, 20).ready);
        seats.pop();
        seats[0].slot = 1;
        assert!(!assess_admission(&players, &seats, 20).ready);
    }

    #[test]
    fn spectators_and_unassigned_outsiders_do_not_block_ready() {
        let players = roster();
        let mut seats = players
            .iter()
            .enumerate()
            .map(|(index, player)| LobbySeat {
                account_id: player.account_id,
                side: Some(player.side),
                slot: (index % 5) as u32,
            })
            .collect::<Vec<_>>();
        // The lobby adapter represents both the spectator team and the player
        // pool as side=None. Neither is part of the ten-player admission.
        seats.push(LobbySeat {
            account_id: 20,
            side: None,
            slot: 10,
        });
        seats.push(LobbySeat {
            account_id: 21,
            side: None,
            slot: 11,
        });

        let admission = assess_admission(&players, &seats, 99);
        assert!(admission.ready);
        assert!(admission.missing.is_empty());
        assert!(admission.wrong_side.is_empty());
        assert!(admission.unexpected_players.is_empty());
    }

    #[test]
    fn only_outsiders_on_a_team_are_unexpected_and_wrong_side_players_are_recoverable() {
        let players = roster();
        let mut seats = players
            .iter()
            .enumerate()
            .map(|(index, player)| LobbySeat {
                account_id: player.account_id,
                side: Some(player.side),
                slot: (index % 5) as u32,
            })
            .collect::<Vec<_>>();
        seats[0].side = Some(Side::Dire);
        seats.push(LobbySeat {
            account_id: 20,
            side: Some(Side::Radiant),
            slot: 0,
        });

        let admission = assess_admission(&players, &seats, 99);
        assert_eq!(admission.wrong_side, vec![1]);
        assert_eq!(admission.unexpected_players, vec![20]);
        assert!(!admission.ready);
    }

    #[test]
    fn account_links_are_lossless_and_duplicate_accounts_rejected() {
        assert_eq!(account_id((STEAM_INDIVIDUAL_BASE + 123) as i64), Some(123));
        assert_eq!(account_id(-1), None);
        assert_eq!(account_id(i64::MAX), None);
        let mut players = roster();
        players[9].account_id = 1;
        assert!(validate_roster(&players, 20).is_err());
    }
}
