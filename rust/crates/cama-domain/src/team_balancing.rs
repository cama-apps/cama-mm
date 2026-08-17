//! Pure team-value and role-parity scoring policy.

use crate::team::{ROLES, Team, TeamError};

/// Sum the five critical lane matchups from role-ordered effective values.
///
/// Values are ordered by position 1 through 5. Carry is compared against the
/// opposing offlaner in both directions, mid against mid, and position 4
/// against the opposing position 5 in both directions.
#[must_use]
pub fn role_matchup_delta_from_values(team1_values: &[f64], team2_values: &[f64]) -> f64 {
    (team1_values[0] - team2_values[2]).abs()
        + (team2_values[0] - team1_values[2]).abs()
        + (team1_values[1] - team2_values[1]).abs()
        + (team1_values[3] - team2_values[4]).abs()
        + (team2_values[3] - team1_values[4]).abs()
}

/// Sum same-role effective-value differences for the two teams.
#[must_use]
pub fn role_parity_delta_from_values(team1_values: &[f64], team2_values: &[f64]) -> f64 {
    team1_values
        .iter()
        .zip(team2_values)
        .map(|(team1, team2)| (team1 - team2).abs())
        .sum()
}

/// Configuration and operations for scoring a two-team matchup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TeamBalancingService {
    pub use_glicko: bool,
    pub off_role_multiplier: f64,
    pub off_role_flat_value_penalty: f64,
    pub off_role_flat_penalty: f64,
    pub role_matchup_delta_weight: f64,
}

impl Default for TeamBalancingService {
    fn default() -> Self {
        Self {
            use_glicko: true,
            off_role_multiplier: 0.95,
            off_role_flat_value_penalty: 100.0,
            off_role_flat_penalty: 550.0,
            role_matchup_delta_weight: 0.18,
        }
    }
}

impl TeamBalancingService {
    /// Construct a service with explicit scoring policy.
    #[must_use]
    pub const fn new(
        use_glicko: bool,
        off_role_multiplier: f64,
        off_role_flat_penalty: f64,
        role_matchup_delta_weight: f64,
    ) -> Self {
        Self::new_with_off_role_value_penalty(
            use_glicko,
            off_role_multiplier,
            100.0,
            off_role_flat_penalty,
            role_matchup_delta_weight,
        )
    }

    /// Construct a service with both off-role reductions explicit.
    #[must_use]
    pub const fn new_with_off_role_value_penalty(
        use_glicko: bool,
        off_role_multiplier: f64,
        off_role_flat_value_penalty: f64,
        off_role_flat_penalty: f64,
        role_matchup_delta_weight: f64,
    ) -> Self {
        Self {
            use_glicko,
            off_role_multiplier,
            off_role_flat_value_penalty,
            off_role_flat_penalty,
            role_matchup_delta_weight,
        }
    }

    /// Calculate total team value using this service's off-role policy.
    pub fn calculate_team_value(
        &self,
        team: &Team,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> Result<f64, TeamError> {
        team.get_team_value_with_off_role_value_penalty(
            self.use_glicko,
            use_openskill,
            use_jopacoin,
            self.off_role_flat_value_penalty,
        )
    }

    /// Calculate the five cross-lane matchup differences.
    pub fn calculate_role_matchup_delta(
        &self,
        team1: &Team,
        team2: &Team,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> Result<f64, TeamError> {
        let team1_values = self.role_values(team1, use_openskill, use_jopacoin)?;
        let team2_values = self.role_values(team2, use_openskill, use_jopacoin)?;
        Ok(role_matchup_delta_from_values(&team1_values, &team2_values))
    }

    /// Calculate the five same-position differences.
    pub fn calculate_role_parity_delta(
        &self,
        team1: &Team,
        team2: &Team,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> Result<f64, TeamError> {
        let team1_values = self.role_values(team1, use_openskill, use_jopacoin)?;
        let team2_values = self.role_values(team2, use_openskill, use_jopacoin)?;
        Ok(role_parity_delta_from_values(&team1_values, &team2_values))
    }

    /// Score a matchup; lower values represent closer teams.
    ///
    /// Team-value difference, flat off-role penalties, lane matchup delta, and
    /// same-role parity all use the same selected rating mode.
    pub fn calculate_matchup_score(
        &self,
        team1: &Team,
        team2: &Team,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> Result<f64, TeamError> {
        let team1_value = self.calculate_team_value(team1, use_openskill, use_jopacoin)?;
        let team2_value = self.calculate_team_value(team2, use_openskill, use_jopacoin)?;
        let value_difference = (team1_value - team2_value).abs();

        let off_role_count = team1.get_off_role_count()? + team2.get_off_role_count()?;
        let off_role_penalty = off_role_count as f64 * self.off_role_flat_penalty;
        let role_matchup_delta =
            self.calculate_role_matchup_delta(team1, team2, use_openskill, use_jopacoin)?;
        let role_parity_delta =
            self.calculate_role_parity_delta(team1, team2, use_openskill, use_jopacoin)?;

        Ok(value_difference
            + off_role_penalty
            + (role_matchup_delta + role_parity_delta) * self.role_matchup_delta_weight)
    }

    fn role_values(
        &self,
        team: &Team,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> Result<[f64; 5], TeamError> {
        let role_value = |role| {
            team.get_player_by_role_with_off_role_value_penalty(
                role,
                self.use_glicko,
                use_openskill,
                use_jopacoin,
                self.off_role_flat_value_penalty,
            )
            .map(|(_, value)| value)
        };

        Ok([
            role_value(ROLES[0])?,
            role_value(ROLES[1])?,
            role_value(ROLES[2])?,
            role_value(ROLES[3])?,
            role_value(ROLES[4])?,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TeamBalancingService, role_matchup_delta_from_values, role_parity_delta_from_values,
    };
    use crate::player::Player;
    use crate::team::{ROLES, Team};

    /// Role-neutral fixtures: an even record in every role gives a factor of
    /// exactly 1.0, so these tests measure the balance formula rather than the
    /// role-performance multiplier.
    fn player(name: &str, mmr: i64, role: &str) -> Player {
        Player {
            mmr: Some(mmr),
            preferred_roles: Some(vec![role.to_owned()]),
            role_records: ROLES
                .iter()
                .map(|role| {
                    (
                        (*role).to_owned(),
                        crate::role_performance::RoleRecord::new(5, 5),
                    )
                })
                .collect(),
            ..Player::new(name)
        }
    }

    fn role_strings() -> Vec<String> {
        ROLES.iter().map(|role| (*role).to_owned()).collect()
    }

    fn fixture_teams() -> (Team, Team) {
        let team1 = Team::new(
            vec![
                player("Carry1", 2_000, "1"),
                player("Mid1", 1_500, "2"),
                player("Offlane1", 1_200, "3"),
                player("Sup1", 1_100, "4"),
                player("Sup2", 1_000, "5"),
            ],
            Some(role_strings()),
        )
        .expect("five players form a team");
        let team2 = Team::new(
            vec![
                player("Carry2", 1_000, "1"),
                player("Mid2", 1_500, "2"),
                player("Offlane2", 1_900, "3"),
                player("Sup3", 1_050, "4"),
                player("Sup4", 950, "5"),
            ],
            Some(role_strings()),
        )
        .expect("five players form a team");
        (team1, team2)
    }

    #[test]
    fn test_role_matchup_delta_calculation() {
        let (team1, team2) = fixture_teams();
        let service =
            TeamBalancingService::new_with_off_role_value_penalty(false, 1.0, 0.0, 50.0, 1.0);

        assert_eq!(
            service
                .calculate_role_matchup_delta(&team1, &team2, false, false)
                .expect("roles are assigned"),
            500.0
        );
        assert_eq!(
            service
                .calculate_role_parity_delta(&team1, &team2, false, false)
                .expect("roles are assigned"),
            1_800.0
        );
        assert_eq!(
            service
                .calculate_matchup_score(&team1, &team2, false, false)
                .expect("roles are assigned"),
            2_700.0
        );

        let weighted_service =
            TeamBalancingService::new_with_off_role_value_penalty(false, 1.0, 0.0, 50.0, 0.5);
        assert_eq!(
            weighted_service
                .calculate_role_matchup_delta(&team1, &team2, false, false)
                .expect("roles are assigned"),
            500.0
        );
        assert_eq!(
            weighted_service
                .calculate_matchup_score(&team1, &team2, false, false)
                .expect("roles are assigned"),
            1_550.0
        );

        let mut swapped_team1 = team1.clone();
        swapped_team1.role_assignments =
            Some(["2", "1", "3", "4", "5"].map(str::to_owned).to_vec());
        let flat_penalty_only =
            TeamBalancingService::new_with_off_role_value_penalty(false, 1.0, 0.0, 50.0, 0.0);
        assert_eq!(
            flat_penalty_only
                .calculate_matchup_score(&swapped_team1, &team1, false, false)
                .expect("roles are assigned"),
            100.0
        );
    }

    #[test]
    fn test_matchup_score_uses_requested_jopacoin_values() {
        let mut team1_players = Vec::new();
        let mut team2_players = Vec::new();
        for role in ROLES {
            let mut team1_player = player(&format!("Team1Role{role}"), 1_500, role);
            team1_player.jopacoin_balance = 100;
            team1_players.push(team1_player);

            let mut team2_player = player(&format!("Team2Role{role}"), 1_500, role);
            team2_player.jopacoin_balance = if role == "1" { 50 } else { 100 };
            team2_players.push(team2_player);
        }
        let team1 =
            Team::new(team1_players, Some(role_strings())).expect("five players form a team");
        let team2 =
            Team::new(team2_players, Some(role_strings())).expect("five players form a team");
        let service = TeamBalancingService::new(false, 1.0, 0.0, 0.5);

        assert_eq!(
            service
                .calculate_matchup_score(&team1, &team2, false, false)
                .expect("roles are assigned"),
            0.0
        );
        assert_eq!(
            service
                .calculate_matchup_score(&team1, &team2, false, true)
                .expect("roles are assigned"),
            100.0
        );
    }

    #[test]
    fn test_service_and_shuffler_metrics_helpers_agree() {
        let (team1, team2) = fixture_teams();
        let service =
            TeamBalancingService::new_with_off_role_value_penalty(false, 1.0, 0.0, 50.0, 1.0);
        let team1_values = [2_000.0, 1_500.0, 1_200.0, 1_100.0, 1_000.0];
        let team2_values = [1_000.0, 1_500.0, 1_900.0, 1_050.0, 950.0];

        let service_matchup = service
            .calculate_role_matchup_delta(&team1, &team2, false, false)
            .expect("roles are assigned");
        let service_parity = service
            .calculate_role_parity_delta(&team1, &team2, false, false)
            .expect("roles are assigned");
        assert_eq!(service_matchup, 500.0);
        assert_eq!(service_parity, 1_800.0);
        assert_eq!(
            role_matchup_delta_from_values(&team1_values, &team2_values),
            service_matchup
        );
        assert_eq!(
            role_parity_delta_from_values(&team1_values, &team2_values),
            service_parity
        );
    }
}
