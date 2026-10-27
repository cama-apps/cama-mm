//! Rate one decided match under both Glicko-2 and OpenSkill.
//!
//! Recording a result and correcting a recorded result gather their pre-match
//! inputs differently (live player state versus stored history snapshots), but
//! both apply the same update to those inputs. This module owns that update.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crate::openskill::{
    CamaOpenSkillSystem, OpenSkillError, WeightedPlayer, WeightedRatingUpdate, WinningTeam,
};
use crate::rating::{
    CamaRatingSystem, MatchUpdateOptions, Player as GlickoPlayer, RatingError, TeamPlayer,
};

/// One participant's pre-match state and per-player update modifiers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchRatingParticipant {
    pub discord_id: i64,
    pub glicko: GlickoPlayer,
    /// Pre-match OpenSkill rating; `None` falls back to the system default.
    pub os_mu: Option<f64>,
    pub os_sigma: Option<f64>,
    /// Weights the OpenSkill update by contribution; `None` is equal weight.
    pub fantasy_points: Option<f64>,
    pub streak_multiplier: f64,
    pub gain_multiplier: f64,
}

/// A decided match to rate. Teams keep their roster order.
#[derive(Clone, Copy, Debug)]
pub struct MatchRatingRequest<'a> {
    pub radiant: &'a [MatchRatingParticipant],
    pub dire: &'a [MatchRatingParticipant],
    /// `1` for Radiant, `2` for Dire.
    pub winning_team: i32,
    /// The recorded multiplier for a replay; `None` uses the configured one.
    pub base_rating_delta_multiplier: Option<f64>,
    /// Whether to apply the OpenSkill update at all.
    pub rate_openskill: bool,
}

/// Glicko-2 state after the match.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlickoAfter {
    pub rating: f64,
    pub rd: f64,
    pub volatility: f64,
}

/// Every participant's post-match ratings, keyed by Discord ID.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatchRatingUpdates {
    pub glicko: BTreeMap<i64, GlickoAfter>,
    /// Empty when the request skipped OpenSkill.
    pub openskill: BTreeMap<i64, WeightedRatingUpdate>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MatchRatingError {
    Glicko(RatingError),
    OpenSkill(OpenSkillError),
}

impl fmt::Display for MatchRatingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Glicko(error) => error.fmt(formatter),
            Self::OpenSkill(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MatchRatingError {}

/// Apply the Glicko-2 team update and, when requested, the OpenSkill update.
pub fn rate_match(
    rating_system: &CamaRatingSystem,
    openskill_system: &CamaOpenSkillSystem,
    request: &MatchRatingRequest<'_>,
) -> Result<MatchRatingUpdates, MatchRatingError> {
    let participants = || request.radiant.iter().chain(request.dire);
    let glicko_team = |side: &[MatchRatingParticipant]| {
        side.iter()
            .map(|participant| TeamPlayer::new(participant.glicko, participant.discord_id))
            .collect::<Vec<_>>()
    };
    let glicko = rating_system
        .update_ratings_after_match_with_options(
            &glicko_team(request.radiant),
            &glicko_team(request.dire),
            request.winning_team,
            &MatchUpdateOptions {
                streak_multipliers: participants()
                    .map(|participant| (participant.discord_id, participant.streak_multiplier))
                    .collect::<HashMap<_, _>>(),
                base_rating_delta_multiplier: request.base_rating_delta_multiplier,
                gain_multipliers: participants()
                    .map(|participant| (participant.discord_id, participant.gain_multiplier))
                    .collect::<HashMap<_, _>>(),
            },
        )
        .map_err(MatchRatingError::Glicko)?;
    let glicko = glicko
        .team1
        .into_iter()
        .chain(glicko.team2)
        .map(|update| {
            (
                update.id,
                GlickoAfter {
                    rating: update.rating,
                    rd: update.rd,
                    volatility: update.volatility,
                },
            )
        })
        .collect();
    if !request.rate_openskill {
        return Ok(MatchRatingUpdates {
            glicko,
            openskill: BTreeMap::new(),
        });
    }

    // OpenSkill keys players by an unsigned ID. Roster positions stand in for
    // Discord IDs so signed fixture IDs rate the same as real ones.
    let discord_ids = participants()
        .map(|participant| participant.discord_id)
        .collect::<Vec<_>>();
    let mut position = 0_u64;
    let mut openskill_team = |side: &[MatchRatingParticipant]| {
        side.iter()
            .map(|participant| {
                position += 1;
                WeightedPlayer::new(
                    position,
                    participant.os_mu,
                    participant.os_sigma,
                    participant.fantasy_points,
                )
            })
            .collect::<Vec<_>>()
    };
    let radiant = openskill_team(request.radiant);
    let dire = openskill_team(request.dire);
    let by_position = |value: fn(&MatchRatingParticipant) -> f64| {
        participants()
            .zip(1_u64..)
            .map(|(participant, position)| (position, value(participant)))
            .collect::<BTreeMap<_, _>>()
    };
    let openskill = openskill_system
        .update_ratings_after_match(
            &radiant,
            &dire,
            if request.winning_team == 1 {
                WinningTeam::Team1
            } else {
                WinningTeam::Team2
            },
            &by_position(|participant| participant.streak_multiplier),
            &by_position(|participant| participant.gain_multiplier),
        )
        .map_err(MatchRatingError::OpenSkill)?
        .into_iter()
        .map(|(position, update)| {
            let index = usize::try_from(position - 1).expect("roster positions fit in usize");
            (discord_ids[index], update)
        })
        .collect();
    Ok(MatchRatingUpdates { glicko, openskill })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openskill::Player as OpenSkillPlayer;
    use crate::rating::TeamPlayer;

    fn participant(discord_id: i64, rating: f64, mu: f64) -> MatchRatingParticipant {
        MatchRatingParticipant {
            discord_id,
            glicko: CamaRatingSystem::default().create_player_from_rating(rating, 120.0, 0.06),
            os_mu: Some(mu),
            os_sigma: Some(5.0),
            fantasy_points: None,
            streak_multiplier: if discord_id == -3 { 1.2 } else { 1.0 },
            gain_multiplier: if discord_id == 7 { 1.1 } else { 1.0 },
        }
    }

    #[test]
    fn rate_match_applies_the_canonical_glicko_and_openskill_updates() {
        let rating = CamaRatingSystem::default();
        let openskill = CamaOpenSkillSystem::new();
        let radiant =
            [-1, -2, -3, 4, 5].map(|id| participant(id, 1_400.0 + id as f64, 30.0 + id as f64));
        let dire = [6, 7, 8, 9, 10].map(|id| participant(id, 1_350.0 + id as f64, 28.0));
        let updates = rate_match(
            &rating,
            &openskill,
            &MatchRatingRequest {
                radiant: &radiant,
                dire: &dire,
                winning_team: 2,
                base_rating_delta_multiplier: None,
                rate_openskill: true,
            },
        )
        .expect("match rates");

        let team = |side: &[MatchRatingParticipant]| {
            side.iter()
                .map(|player| TeamPlayer::new(player.glicko, player.discord_id))
                .collect::<Vec<_>>()
        };
        let all = radiant.iter().chain(&dire);
        let expected_glicko = rating
            .update_ratings_after_match_with_options(
                &team(&radiant),
                &team(&dire),
                2,
                &MatchUpdateOptions {
                    streak_multipliers: HashMap::from([(-3, 1.2)]),
                    base_rating_delta_multiplier: None,
                    gain_multipliers: HashMap::from([(7, 1.1)]),
                },
            )
            .expect("reference Glicko update");
        for update in expected_glicko.team1.iter().chain(&expected_glicko.team2) {
            assert_eq!(
                updates.glicko[&update.id],
                GlickoAfter {
                    rating: update.rating,
                    rd: update.rd,
                    volatility: update.volatility,
                }
            );
        }

        let ids = all
            .clone()
            .map(|player| player.discord_id)
            .collect::<Vec<_>>();
        let os_team = |side: &[MatchRatingParticipant], offset: u64| {
            side.iter()
                .zip(offset..)
                .map(|(player, id)| OpenSkillPlayer::new(id, player.os_mu, player.os_sigma))
                .collect::<Vec<_>>()
        };
        let expected_openskill = openskill
            .update_ratings_equal_weight(
                &os_team(&radiant, 1),
                &os_team(&dire, 6),
                WinningTeam::Team2,
                &BTreeMap::from([(3, 1.2)]),
                &BTreeMap::from([(7, 1.1)]),
            )
            .expect("reference OpenSkill update");
        assert_eq!(updates.openskill.len(), 10);
        for (position, rating) in expected_openskill {
            let discord_id = ids[usize::try_from(position - 1).unwrap()];
            assert_eq!(updates.openskill[&discord_id].rating, rating);
        }
    }

    #[test]
    fn rate_match_can_skip_openskill() {
        let radiant = [1, 2, 3, 4, 5].map(|id| participant(id, 1_500.0, 25.0));
        let dire = [6, 7, 8, 9, 10].map(|id| participant(id, 1_500.0, 25.0));
        let updates = rate_match(
            &CamaRatingSystem::default(),
            &CamaOpenSkillSystem::new(),
            &MatchRatingRequest {
                radiant: &radiant,
                dire: &dire,
                winning_team: 1,
                base_rating_delta_multiplier: Some(0.5),
                rate_openskill: false,
            },
        )
        .expect("match rates");
        assert_eq!(updates.glicko.len(), 10);
        assert!(updates.openskill.is_empty());
    }
}
