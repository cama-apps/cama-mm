"""
Role assignment domain service.

Handles optimal role assignment for teams.
"""

import itertools

from domain.models.player import Player


class RoleAssignmentService:
    """
    Pure domain service for role assignment logic.

    Responsibilities:
    - Calculate optimal role assignments for a team
    - Determine off-role counts
    - Evaluate role compatibility
    """

    ROLES = ["1", "2", "3", "4", "5"]

    def assign_roles_optimally(self, players: list[Player]) -> list[str]:
        """
        Assign roles 1-5 to players to minimize off-role penalties.

        Args:
            players: List of 5 players

        Returns:
            List of role assignments for each player
        """
        if len(players) != 5:
            raise ValueError("Need exactly 5 players for role assignment")

        best_assignment = None
        best_off_roles = float("inf")

        for role_perm in itertools.permutations(self.ROLES):
            off_role_count = self._count_off_roles(players, list(role_perm))

            if off_role_count < best_off_roles:
                best_off_roles = off_role_count
                best_assignment = list(role_perm)

        return best_assignment if best_assignment else list(self.ROLES)

    def count_off_roles(self, players: list[Player], role_assignments: list[str]) -> int:
        """
        Count how many players are playing off-role.

        Args:
            players: List of players
            role_assignments: List of assigned roles

        Returns:
            Number of players playing off-role
        """
        return self._count_off_roles(players, role_assignments)

    def _count_off_roles(self, players: list[Player], role_assignments: list[str]) -> int:
        """Internal helper to count off-roles."""
        off_role_count = 0
        for player, role in zip(players, role_assignments):
            if not player.preferred_roles or role not in player.preferred_roles:
                off_role_count += 1
        return off_role_count

    def is_player_on_role(self, player: Player, assigned_role: str) -> bool:
        """Check if a player is on their preferred role."""
        if not player.preferred_roles:
            return False
        return assigned_role in player.preferred_roles

    def get_role_distribution(self, players: list[Player]) -> dict[str, int]:
        """
        Get role distribution based on primary preferred roles.

        Args:
            players: List of players

        Returns:
            Dictionary mapping roles to counts
        """
        roles = dict.fromkeys(self.ROLES, 0)
        roles["unknown"] = 0

        for player in players:
            if player.preferred_roles and len(player.preferred_roles) > 0:
                primary_role = player.preferred_roles[0]
                if primary_role in roles:
                    roles[primary_role] += 1
                else:
                    roles["unknown"] += 1
            else:
                roles["unknown"] += 1

        return roles

    def get_role_coverage(self, players: list[Player]) -> dict[str, list[Player]]:
        """
        Get which players can play each role.

        Args:
            players: List of players

        Returns:
            Dictionary mapping roles to list of players who can play that role
        """
        coverage = {role: [] for role in self.ROLES}

        for player in players:
            if player.preferred_roles:
                for role in player.preferred_roles:
                    if role in coverage:
                        coverage[role].append(player)

        return coverage

    def can_form_balanced_team(self, players: list[Player]) -> bool:
        """
        Check if players can form a team with all roles covered.

        Args:
            players: List of 5 players

        Returns:
            True if all roles can be covered with at least one player on-role
        """
        if len(players) != 5:
            return False

        best_off_roles = float("inf")
        for role_perm in itertools.permutations(self.ROLES):
            off_role_count = self._count_off_roles(players, list(role_perm))
            best_off_roles = min(best_off_roles, off_role_count)

        # Consider balanced if at most 2 players are off-role
        return best_off_roles <= 2
