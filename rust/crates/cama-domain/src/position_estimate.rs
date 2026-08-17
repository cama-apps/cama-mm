//! Heuristic estimation of which Dota position (1-5) a player actually
//! played in a finished match, derived from OpenDota match-enrichment
//! stats already captured per participant (`last_hits`, `net_worth`, `gpm`,
//! `lane_role`, `obs_placed`, `sen_placed`).
//!
//! This is independent of, and may disagree with, the shuffler's pre-game
//! role assignment in [`crate::team`] — players often swap positions by
//! in-lobby agreement after the shuffle runs, so the assignment is not
//! reliable evidence of what was actually played.

/// One team member's OpenDota-enrichment-derived stats for a single match,
/// as already persisted on `match_participants`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParticipantFarmStats {
    pub discord_id: i64,
    /// OpenDota's lane classification: 1=safe, 2=mid, 3=off, 4=jungle.
    pub lane_role: Option<i64>,
    pub last_hits: Option<i64>,
    pub net_worth: Option<i64>,
    pub gpm: Option<i64>,
    pub obs_placed: Option<i64>,
    pub sen_placed: Option<i64>,
}

/// Reasons a team's positions could not be estimated. Partial or missing
/// enrichment data yields an error rather than a guess, since the result is
/// presented to players as an estimate of fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionEstimateError {
    /// A team is always exactly five players.
    IncorrectTeamSize { actual: usize },
    /// A participant is missing `lane_role`, or the team as a whole doesn't
    /// have any single farm metric (`last_hits`, then `net_worth`, then
    /// `gpm`) populated for every member — a farm ranking needs one
    /// consistent metric across the team, not a per-member mix of them.
    MissingFarmData { discord_id: i64 },
    /// Exactly one team member must have `lane_role == 2` (mid) to anchor
    /// the rest of the estimate; zero or multiple mids make the rest of the
    /// ranking unreliable.
    AmbiguousMidLane { mid_count: usize },
}

/// Estimate each of a full team's five members' actually-played position
/// (`"1"`..`"5"`, matching [`crate::team::ROLES`]) from farm and
/// support-item signals:
///
/// 1. The sole `lane_role == 2` member is position 2 (mid).
/// 2. Of the remaining four, the two highest by farm are position 1 (carry)
///    and position 3 (off), in that order. Farm is ranked by `last_hits`
///    where available — unlike `net_worth`/`gpm`, creep score isn't
///    inflated by kill/bounty gold a support happened to pick up, so it's
///    a cleaner core-vs-support signal — falling back to `net_worth`, then
///    `gpm`, only when `last_hits` isn't populated for the whole team.
/// 3. Of the remaining two (lowest farm), the one with more wards placed
///    (`obs_placed + sen_placed`) is position 5 (hard support); ties break
///    toward the lower-farm player. The other is position 4 (soft support).
///
/// # Errors
///
/// Returns [`PositionEstimateError`] if `team` is not exactly five players,
/// any member is missing the data the heuristic depends on, or mid lane
/// cannot be uniquely identified.
pub fn estimate_team_positions(
    team: &[ParticipantFarmStats],
) -> Result<Vec<(i64, &'static str)>, PositionEstimateError> {
    if team.len() != 5 {
        return Err(PositionEstimateError::IncorrectTeamSize { actual: team.len() });
    }
    for member in team {
        if member.lane_role.is_none() {
            return Err(PositionEstimateError::MissingFarmData {
                discord_id: member.discord_id,
            });
        }
    }

    let Some(metric) = select_farm_metric(team) else {
        let culprit = team
            .iter()
            .find(|member| member.last_hits.is_none())
            .expect("select_farm_metric failed, so some member lacks last_hits");
        return Err(PositionEstimateError::MissingFarmData {
            discord_id: culprit.discord_id,
        });
    };
    let farm_value = |member: &ParticipantFarmStats| metric.value(member).unwrap_or(0) as f64;

    let mids: Vec<&ParticipantFarmStats> = team
        .iter()
        .filter(|member| member.lane_role == Some(2))
        .collect();
    let [mid] = mids.as_slice() else {
        return Err(PositionEstimateError::AmbiguousMidLane {
            mid_count: mids.len(),
        });
    };

    let mut rest: Vec<&ParticipantFarmStats> = team
        .iter()
        .filter(|member| member.discord_id != mid.discord_id)
        .collect();
    rest.sort_by(|a, b| farm_value(b).total_cmp(&farm_value(a)));
    let [pos1, pos3, support_a, support_b] = rest.as_slice() else {
        unreachable!("exactly four non-mid members remain after filtering a team of five")
    };

    let (pos5, pos4) = if wards(support_a) != wards(support_b) {
        if wards(support_a) > wards(support_b) {
            (support_a, support_b)
        } else {
            (support_b, support_a)
        }
    } else if farm_value(support_a) <= farm_value(support_b) {
        (support_a, support_b)
    } else {
        (support_b, support_a)
    };

    Ok(vec![
        (pos1.discord_id, "1"),
        (mid.discord_id, "2"),
        (pos3.discord_id, "3"),
        (pos4.discord_id, "4"),
        (pos5.discord_id, "5"),
    ])
}

/// A farm metric usable for ranking a whole team consistently. Variants are
/// declared in preference order; [`select_farm_metric`] picks the first one
/// populated for every member.
#[derive(Clone, Copy)]
enum FarmMetric {
    LastHits,
    NetWorth,
    Gpm,
}

impl FarmMetric {
    fn value(self, member: &ParticipantFarmStats) -> Option<i64> {
        match self {
            Self::LastHits => member.last_hits,
            Self::NetWorth => member.net_worth,
            Self::Gpm => member.gpm,
        }
    }
}

/// Picks the highest-preference metric that's populated for every member of
/// `team`. Mixing metrics per-member (e.g. `last_hits` for one player and
/// `gpm` for another) would compare incompatible units, so a metric is only
/// usable when the whole team has it.
fn select_farm_metric(team: &[ParticipantFarmStats]) -> Option<FarmMetric> {
    [FarmMetric::LastHits, FarmMetric::NetWorth, FarmMetric::Gpm]
        .into_iter()
        .find(|metric| team.iter().all(|member| metric.value(member).is_some()))
}

fn wards(member: &ParticipantFarmStats) -> i64 {
    member.obs_placed.unwrap_or(0) + member.sen_placed.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a member with only `last_hits` populated as a farm metric —
    /// the preferred one. Tests exercising the `net_worth`/`gpm` fallbacks
    /// override those fields explicitly.
    fn member(
        discord_id: i64,
        lane_role: i64,
        last_hits: i64,
        obs_placed: i64,
        sen_placed: i64,
    ) -> ParticipantFarmStats {
        ParticipantFarmStats {
            discord_id,
            lane_role: Some(lane_role),
            last_hits: Some(last_hits),
            net_worth: None,
            gpm: None,
            obs_placed: Some(obs_placed),
            sen_placed: Some(sen_placed),
        }
    }

    #[test]
    fn test_estimates_a_clean_team_by_last_hits_and_wards() {
        let team = vec![
            member(1, 1, 220, 0, 0), // highest last hits, safelane -> pos1
            member(2, 2, 180, 0, 0), // sole mid -> pos2
            member(3, 3, 140, 1, 0), // second-highest last hits, offlane -> pos3
            member(4, 3, 20, 2, 1),  // low last hits, more wards -> pos5
            member(5, 1, 25, 0, 1),  // low last hits, fewer wards -> pos4
        ];

        let result = estimate_team_positions(&team).expect("clean team estimates");

        assert_eq!(
            result,
            vec![(1, "1"), (2, "2"), (3, "3"), (5, "4"), (4, "5")]
        );
    }

    #[test]
    fn test_ward_tie_breaks_toward_lower_last_hits_as_hard_support() {
        let team = vec![
            member(1, 1, 220, 0, 0),
            member(2, 2, 180, 0, 0),
            member(3, 3, 140, 0, 0),
            member(4, 3, 15, 3, 3), // tied wards, lower last hits -> pos5
            member(5, 1, 25, 3, 3), // tied wards, higher last hits -> pos4
        ];

        let result = estimate_team_positions(&team).expect("tie-break resolves deterministically");

        assert_eq!(result[3], (5, "4"));
        assert_eq!(result[4], (4, "5"));
    }

    #[test]
    fn test_falls_back_to_net_worth_when_last_hits_is_missing() {
        let mut team = vec![
            member(1, 1, 220, 0, 0),
            member(2, 2, 180, 0, 0),
            member(3, 3, 140, 1, 0),
            member(4, 3, 20, 2, 1),
            member(5, 1, 25, 0, 1),
        ];
        let net_worths = [24000, 15000, 18000, 6000, 7000];
        for (participant, net_worth) in team.iter_mut().zip(net_worths) {
            participant.last_hits = None;
            participant.net_worth = Some(net_worth);
        }

        let result = estimate_team_positions(&team).expect("net_worth fallback still estimates");

        assert_eq!(
            result,
            vec![(1, "1"), (2, "2"), (3, "3"), (5, "4"), (4, "5")]
        );
    }

    #[test]
    fn test_falls_back_to_gpm_when_last_hits_and_net_worth_are_missing() {
        let mut team = vec![
            member(1, 1, 220, 0, 0),
            member(2, 2, 180, 0, 0),
            member(3, 3, 140, 1, 0),
            member(4, 3, 20, 2, 1),
            member(5, 1, 25, 0, 1),
        ];
        let gpms = [900, 550, 700, 250, 300];
        for (participant, gpm) in team.iter_mut().zip(gpms) {
            participant.last_hits = None;
            participant.gpm = Some(gpm);
        }

        let result = estimate_team_positions(&team).expect("gpm fallback still estimates");

        assert_eq!(
            result,
            vec![(1, "1"), (2, "2"), (3, "3"), (5, "4"), (4, "5")]
        );
    }

    #[test]
    fn test_rejects_when_no_single_metric_covers_the_whole_team() {
        let mut team = vec![
            member(1, 1, 220, 0, 0),
            member(2, 2, 180, 0, 0),
            member(3, 3, 140, 0, 0),
            member(4, 3, 20, 2, 1),
            member(5, 1, 25, 0, 1),
        ];
        team[4].last_hits = None;

        let error = estimate_team_positions(&team).expect_err("no metric is fully populated");

        assert_eq!(
            error,
            PositionEstimateError::MissingFarmData { discord_id: 5 }
        );
    }

    #[test]
    fn test_rejects_a_team_that_is_not_five_players() {
        let team = vec![member(1, 1, 220, 0, 0)];

        let error = estimate_team_positions(&team).expect_err("one player is not a team");

        assert_eq!(
            error,
            PositionEstimateError::IncorrectTeamSize { actual: 1 }
        );
    }

    #[test]
    fn test_rejects_missing_lane_role() {
        let mut team = vec![
            member(1, 1, 220, 0, 0),
            member(2, 2, 180, 0, 0),
            member(3, 3, 140, 0, 0),
            member(4, 3, 20, 2, 1),
            member(5, 1, 25, 0, 1),
        ];
        team[3].lane_role = None;

        let error = estimate_team_positions(&team).expect_err("missing lane_role is rejected");

        assert_eq!(
            error,
            PositionEstimateError::MissingFarmData { discord_id: 4 }
        );
    }

    #[test]
    fn test_rejects_zero_mids() {
        let team = vec![
            member(1, 1, 220, 0, 0),
            member(2, 1, 180, 0, 0),
            member(3, 3, 140, 0, 0),
            member(4, 3, 20, 2, 1),
            member(5, 1, 25, 0, 1),
        ];

        let error = estimate_team_positions(&team).expect_err("no mid lane is ambiguous");

        assert_eq!(
            error,
            PositionEstimateError::AmbiguousMidLane { mid_count: 0 }
        );
    }

    #[test]
    fn test_rejects_two_mids() {
        let team = vec![
            member(1, 2, 220, 0, 0),
            member(2, 2, 180, 0, 0),
            member(3, 3, 140, 0, 0),
            member(4, 3, 20, 2, 1),
            member(5, 1, 25, 0, 1),
        ];

        let error = estimate_team_positions(&team).expect_err("two mids is ambiguous");

        assert_eq!(
            error,
            PositionEstimateError::AmbiguousMidLane { mid_count: 2 }
        );
    }
}
