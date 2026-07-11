"""
Role assignment domain service.

Handles optimal role assignment for teams.
"""


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


