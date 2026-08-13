//! Five-player teams and deterministic role assignment.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::player::Player;

/// The five Dota positions in the canonical permutation order.
pub const ROLES: [&str; 5] = ["1", "2", "3", "4", "5"];

/// Errors raised while constructing or scoring a team.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TeamError {
    /// Teams must always contain exactly five players.
    IncorrectPlayerCount { actual: usize },
    /// A role-dependent operation was attempted before roles were assigned.
    MissingRoleAssignments { method_name: &'static str },
    /// Retained for parity with Python's defensive empty-team fallback.
    NoPlayerForRole { role: String },
}

impl Display for TeamError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncorrectPlayerCount { .. } => {
                formatter.write_str("Team must have exactly 5 players")
            }
            Self::MissingRoleAssignments { method_name } => write!(
                formatter,
                "Team.{method_name}() requires role_assignments to be set. Pass role_assignments \
                 to Team(...) or call team.ensure_role_assignments() explicitly before this call."
            ),
            Self::NoPlayerForRole { role } => write!(formatter, "No player found for role {role}"),
        }
    }
}

impl Error for TeamError {}

/// Compute every role permutation that minimizes the number of off-role players.
///
/// `player_roles_key` represents missing preferences as empty vectors. Results
/// use the same lexicographic permutation order as Python's
/// `itertools.permutations`, making the first optimal assignment deterministic.
#[must_use]
pub fn compute_optimal_role_assignments(player_roles_key: &[Vec<String>]) -> Vec<Vec<String>> {
    fn visit_permutations(
        depth: usize,
        used: &mut [bool; 5],
        current: &mut Vec<&'static str>,
        player_roles_key: &[Vec<String>],
        minimum_off_roles: &mut usize,
        optimal_assignments: &mut Vec<Vec<String>>,
    ) {
        if depth == ROLES.len() {
            let off_role_count = player_roles_key
                .iter()
                .zip(current.iter())
                .filter(|(preferred_roles, assigned_role)| {
                    preferred_roles.is_empty()
                        || !preferred_roles
                            .iter()
                            .any(|preferred_role| preferred_role == **assigned_role)
                })
                .count();
            let assignment: Vec<String> = current
                .iter()
                .map(|assigned_role| (*assigned_role).to_owned())
                .collect();

            match off_role_count.cmp(minimum_off_roles) {
                std::cmp::Ordering::Less => {
                    *minimum_off_roles = off_role_count;
                    optimal_assignments.clear();
                    optimal_assignments.push(assignment);
                }
                std::cmp::Ordering::Equal => optimal_assignments.push(assignment),
                std::cmp::Ordering::Greater => {}
            }
            return;
        }

        for (index, role) in ROLES.iter().enumerate() {
            if used[index] {
                continue;
            }
            used[index] = true;
            current.push(role);
            visit_permutations(
                depth + 1,
                used,
                current,
                player_roles_key,
                minimum_off_roles,
                optimal_assignments,
            );
            current.pop();
            used[index] = false;
        }
    }

    let mut optimal_assignments = Vec::new();
    let mut minimum_off_roles = usize::MAX;
    visit_permutations(
        0,
        &mut [false; 5],
        &mut Vec::with_capacity(5),
        player_roles_key,
        &mut minimum_off_roles,
        &mut optimal_assignments,
    );

    if optimal_assignments.is_empty() {
        vec![ROLES.iter().map(|role| (*role).to_owned()).collect()]
    } else {
        optimal_assignments
    }
}

/// A team of exactly five players and, optionally, explicit role assignments.
#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    pub players: Vec<Player>,
    pub role_assignments: Option<Vec<String>>,
}

impl Team {
    pub const ROLES: [&'static str; 5] = ROLES;
    pub const TEAM_SIZE: usize = 5;

    /// Build a team, defensively owning both input vectors.
    ///
    /// An empty assignment vector has the same meaning as Python's false-y
    /// empty list and is normalized to a missing assignment.
    pub fn new(
        players: Vec<Player>,
        role_assignments: Option<Vec<String>>,
    ) -> Result<Self, TeamError> {
        if players.len() != Self::TEAM_SIZE {
            return Err(TeamError::IncorrectPlayerCount {
                actual: players.len(),
            });
        }

        Ok(Self {
            players,
            role_assignments: role_assignments.filter(|roles| !roles.is_empty()),
        })
    }

    /// Populate missing role assignments with the deterministic first optimum.
    pub fn ensure_role_assignments(&mut self) -> &[String] {
        if self.role_assignments.as_ref().is_none_or(Vec::is_empty) {
            self.role_assignments = Some(self.assign_roles_optimally());
        }

        self.role_assignments
            .as_deref()
            .expect("role assignments were populated above")
    }

    /// Return every assignment tied for the minimum off-role count.
    #[must_use]
    pub fn get_all_optimal_role_assignments(&self) -> Vec<Vec<String>> {
        let player_roles_key: Vec<Vec<String>> = self
            .players
            .iter()
            .map(|player| player.preferred_roles.clone().unwrap_or_default())
            .collect();
        compute_optimal_role_assignments(&player_roles_key)
    }

    /// Calculate total team value after applying the multiplier to off-role players.
    pub fn get_team_value(
        &self,
        use_glicko: bool,
        off_role_multiplier: f64,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> Result<f64, TeamError> {
        let role_assignments = self.require_role_assignments("get_team_value")?;
        Ok(self
            .players
            .iter()
            .zip(role_assignments)
            .map(|(player, assigned_role)| {
                let base_value = player.get_value(use_glicko, use_openskill, use_jopacoin);
                if player
                    .preferred_roles
                    .as_ref()
                    .is_some_and(|roles| roles.contains(assigned_role))
                {
                    base_value
                } else {
                    base_value * off_role_multiplier
                }
            })
            .sum())
    }

    /// Count players whose assigned position is absent from their preferences.
    pub fn get_off_role_count(&self) -> Result<usize, TeamError> {
        let role_assignments = self.require_role_assignments("get_off_role_count")?;
        Ok(self
            .players
            .iter()
            .zip(role_assignments)
            .filter(|(player, assigned_role)| {
                !player
                    .preferred_roles
                    .as_ref()
                    .is_some_and(|roles| roles.contains(assigned_role))
            })
            .count())
    }

    /// Find the player assigned to `role` and return their off-role-adjusted value.
    ///
    /// Like Python, a malformed non-empty assignment without the requested
    /// role falls back to the first player with the off-role multiplier.
    pub fn get_player_by_role(
        &self,
        role: &str,
        use_glicko: bool,
        off_role_multiplier: f64,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> Result<(&Player, f64), TeamError> {
        let role_assignments = self.require_role_assignments("get_player_by_role")?;

        for (player, assigned_role) in self.players.iter().zip(role_assignments) {
            if assigned_role == role {
                let base_value = player.get_value(use_glicko, use_openskill, use_jopacoin);
                let effective_value = if player
                    .preferred_roles
                    .as_ref()
                    .is_some_and(|roles| roles.iter().any(|preferred| preferred == role))
                {
                    base_value
                } else {
                    base_value * off_role_multiplier
                };
                return Ok((player, effective_value));
            }
        }

        self.players.first().map_or_else(
            || {
                Err(TeamError::NoPlayerForRole {
                    role: role.to_owned(),
                })
            },
            |player| {
                Ok((
                    player,
                    player.get_value(use_glicko, use_openskill, use_jopacoin) * off_role_multiplier,
                ))
            },
        )
    }

    fn require_role_assignments(&self, method_name: &'static str) -> Result<&[String], TeamError> {
        self.role_assignments
            .as_deref()
            .filter(|roles| !roles.is_empty())
            .ok_or(TeamError::MissingRoleAssignments { method_name })
    }

    fn assign_roles_optimally(&self) -> Vec<String> {
        self.get_all_optimal_role_assignments()
            .into_iter()
            .next()
            .unwrap_or_else(|| ROLES.iter().map(|role| (*role).to_owned()).collect())
    }
}

impl Display for Team {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let player_names = self
            .players
            .iter()
            .map(|player| player.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        write!(formatter, "Team: {player_names}")
    }
}

#[cfg(test)]
mod tests {
    use super::{ROLES, Team, TeamError};
    use crate::player::Player;

    fn player(name: &str, mmr: i64, preferred_roles: &[&str]) -> Player {
        Player {
            mmr: Some(mmr),
            preferred_roles: Some(
                preferred_roles
                    .iter()
                    .map(|role| (*role).to_owned())
                    .collect(),
            ),
            ..Player::new(name)
        }
    }

    fn role_strings() -> Vec<String> {
        ROLES.iter().map(|role| (*role).to_owned()).collect()
    }

    #[test]
    fn test_team_creation() {
        let players = (0..5)
            .map(|index| player(&format!("Player{index}"), 1_500, &[]))
            .collect();
        let team = Team::new(players, None).expect("five players form a team");
        assert_eq!(team.players.len(), 5);
    }

    #[test]
    fn test_team_creation_wrong_size() {
        let players = (0..4)
            .map(|index| player(&format!("Player{index}"), 1_500, &[]))
            .collect();
        assert_eq!(
            Team::new(players, None),
            Err(TeamError::IncorrectPlayerCount { actual: 4 })
        );
    }

    #[test]
    fn test_get_team_value_requires_role_assignments() {
        let players = vec![
            player("P1", 2_000, &["1"]),
            player("P2", 1_800, &["2"]),
            player("P3", 1_600, &["3"]),
            player("P4", 1_400, &["4"]),
            player("P5", 1_200, &["5"]),
        ];
        let team = Team::new(players, None).expect("five players form a team");

        for error in [
            team.get_team_value(false, 0.9, false, false)
                .expect_err("team value requires roles"),
            team.get_off_role_count()
                .map(|count| count as f64)
                .expect_err("off-role count requires roles"),
            team.get_player_by_role("1", false, 0.9, false, false)
                .map(|(_, value)| value)
                .expect_err("role lookup requires roles"),
        ] {
            assert!(error.to_string().contains("role_assignments"));
        }
    }

    #[test]
    fn test_ensure_role_assignments_populates_and_is_idempotent() {
        let players = vec![
            player("P1", 2_000, &["1"]),
            player("P2", 1_800, &["2"]),
            player("P3", 1_600, &["3"]),
            player("P4", 1_400, &["4"]),
            player("P5", 1_200, &["5"]),
        ];
        let mut team = Team::new(players, None).expect("five players form a team");

        assert_eq!(team.ensure_role_assignments(), role_strings());
        team.role_assignments = Some(["5", "4", "3", "2", "1"].map(str::to_owned).to_vec());
        assert_eq!(team.ensure_role_assignments(), ["5", "4", "3", "2", "1"]);
    }

    #[test]
    fn test_team_value_and_off_role_count() {
        let players = vec![
            player("P1", 2_000, &["1"]),
            player("P2", 1_800, &["2"]),
            player("P3", 1_600, &["3"]),
            player("P4", 1_400, &["4"]),
            player("P5", 1_200, &["5"]),
        ];
        let on_role =
            Team::new(players.clone(), Some(role_strings())).expect("five players form a team");
        assert_eq!(
            on_role
                .get_team_value(false, 0.9, false, false)
                .expect("roles are assigned"),
            8_000.0
        );
        assert_eq!(on_role.get_off_role_count().expect("roles are assigned"), 0);

        let off_role = Team::new(
            players,
            Some(["2", "1", "3", "4", "5"].map(str::to_owned).to_vec()),
        )
        .expect("five players form a team");
        assert_eq!(
            off_role
                .get_team_value(false, 0.9, false, false)
                .expect("roles are assigned"),
            7_620.0
        );
        assert_eq!(
            off_role.get_off_role_count().expect("roles are assigned"),
            2
        );
    }

    #[test]
    fn test_role_assignment_optimal() {
        let players = vec![
            player("P1", 2_000, &["1", "2"]),
            player("P2", 1_800, &["2", "3"]),
            player("P3", 1_600, &["3"]),
            player("P4", 1_400, &["4", "5"]),
            player("P5", 1_200, &["5"]),
        ];
        let mut team = Team::new(players, None).expect("five players form a team");
        assert_eq!(team.ensure_role_assignments(), role_strings());
        assert!(
            team.get_off_role_count()
                .expect("roles were explicitly ensured")
                <= 2
        );

        let every_role = ROLES.as_slice();
        let ambiguous_players = (0..5)
            .map(|index| player(&format!("P{index}"), 1_500, every_role))
            .collect();
        let ambiguous_team = Team::new(ambiguous_players, None).expect("five players form a team");
        let all_optima = ambiguous_team.get_all_optimal_role_assignments();
        assert_eq!(all_optima.len(), 120);
        assert_eq!(all_optima.first(), Some(&role_strings()));
        assert_eq!(
            all_optima.last(),
            Some(&["5", "4", "3", "2", "1"].map(str::to_owned).to_vec())
        );
    }

    #[test]
    fn test_get_player_by_role_effective_value() {
        let players = vec![
            player("Carry", 2_000, &["1"]),
            player("Mid", 1_800, &["2"]),
            player("Offlane", 1_600, &["3"]),
            player("Support1", 1_400, &["4"]),
            player("Support2", 1_200, &["5"]),
        ];
        let mut team = Team::new(players, Some(role_strings())).expect("five players form a team");

        let (carry, value) = team
            .get_player_by_role("1", false, 0.5, false, false)
            .expect("roles are assigned");
        assert_eq!(carry.name, "Carry");
        assert_eq!(value, 2_000.0);

        team.role_assignments = Some(["3", "1", "2", "4", "5"].map(str::to_owned).to_vec());
        let (mid, value) = team
            .get_player_by_role("1", false, 0.5, false, false)
            .expect("roles are assigned");
        assert_eq!(mid.name, "Mid");
        assert_eq!(value, 900.0);
    }

    #[test]
    fn test_team_value_jopacoin() {
        let mut players = vec![
            player("P1", 2_000, &["1"]),
            player("P2", 1_800, &["2"]),
            player("P3", 1_600, &["3"]),
            player("P4", 1_400, &["4"]),
            player("P5", 1_200, &["5"]),
        ];
        for (player, balance) in players.iter_mut().zip([100, 200, 50, 75, 25]) {
            player.jopacoin_balance = balance;
        }

        let on_role =
            Team::new(players.clone(), Some(role_strings())).expect("five players form a team");
        assert_eq!(
            on_role
                .get_team_value(true, 0.9, false, true)
                .expect("roles are assigned"),
            450.0
        );

        let off_role = Team::new(
            players,
            Some(["2", "1", "3", "4", "5"].map(str::to_owned).to_vec()),
        )
        .expect("five players form a team");
        assert_eq!(
            off_role
                .get_team_value(true, 0.9, false, true)
                .expect("roles are assigned"),
            420.0
        );
    }

    #[test]
    fn test_get_player_by_role_jopacoin() {
        let mut players = vec![
            player("P1", 0, &["1"]),
            player("P2", 0, &["2"]),
            player("P3", 0, &["3"]),
            player("P4", 0, &["4"]),
            player("P5", 0, &["5"]),
        ];
        for (player, balance) in players.iter_mut().zip([100, 200, 50, 75, 25]) {
            player.jopacoin_balance = balance;
        }
        let team = Team::new(players, Some(role_strings())).expect("five players form a team");

        let (player, value) = team
            .get_player_by_role("2", true, 0.9, false, true)
            .expect("roles are assigned");
        assert_eq!(player.name, "P2");
        assert_eq!(value, 200.0);
    }
}
