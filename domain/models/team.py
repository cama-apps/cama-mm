"""
Team domain model.
"""

import itertools
from typing import List, Optional, Dict, Tuple
from domain.models.player import Player


class Team:
    """
    Represents a team of 5 players.
    
    This is a pure domain model with no infrastructure dependencies.
    """
    
    ROLES = ["1", "2", "3", "4", "5"]
    TEAM_SIZE = 5
    
    def __init__(self, players: List[Player], role_assignments: Optional[List[str]] = None):
        """
        Initialize a team.
        
        Args:
            players: List of 5 players
            role_assignments: Optional list of role assignments (1-5) for each player
        """
        if len(players) != self.TEAM_SIZE:
            raise ValueError(f"Team must have exactly {self.TEAM_SIZE} players")
        self.players = players
        self.role_assignments = role_assignments
    
    def get_team_value(self, use_glicko: bool = True, off_role_multiplier: float = 0.9) -> float:
        """
        Calculate total team value with off-role penalties.
        
        Args:
            use_glicko: Whether to use Glicko-2 ratings
            off_role_multiplier: Multiplier for rating when playing off-role
        
        Returns:
            Sum of all player values adjusted for role assignments
        """
        if not self.role_assignments:
            self.role_assignments = self._assign_roles_optimally()
        
        total_value = 0
        for player, assigned_role in zip(self.players, self.role_assignments):
            base_value = player.get_value(use_glicko)
            
            if player.preferred_roles and assigned_role in player.preferred_roles:
                total_value += base_value
            else:
                total_value += base_value * off_role_multiplier
        
        return total_value
    
    def get_off_role_count(self) -> int:
        """Count how many players are on off-role."""
        if not self.role_assignments:
            self.role_assignments = self._assign_roles_optimally()
        
        off_role_count = 0
        for player, assigned_role in zip(self.players, self.role_assignments):
            if not player.preferred_roles or assigned_role not in player.preferred_roles:
                off_role_count += 1
        
        return off_role_count

    def _count_off_roles(self, players: List[Player], role_assignments: List[str]) -> int:
        """Internal helper to count off-role players for a permutation."""
        off_role_count = 0
        for player, role in zip(players, role_assignments):
            if not player.preferred_roles or role not in player.preferred_roles:
                off_role_count += 1
        return off_role_count
    
    def get_all_optimal_role_assignments(self) -> List[List[str]]:
        """
        Return all permutations that minimize off-role penalties.
        """
        min_off_roles = float('inf')
        optimal_assignments: List[List[str]] = []
        
        for role_perm in itertools.permutations(self.ROLES):
            off_role_count = self._count_off_roles(self.players, list(role_perm))
            if off_role_count < min_off_roles:
                min_off_roles = off_role_count
                optimal_assignments = [list(role_perm)]
            elif off_role_count == min_off_roles:
                optimal_assignments.append(list(role_perm))
        
        return optimal_assignments or [list(self.ROLES)]

    def _assign_roles_optimally(self) -> List[str]:
        """
        Assign roles 1-5 to players to minimize off-role penalties.
        
        Returns:
            List of role assignments for each player
        """
        optimal_assignments = self.get_all_optimal_role_assignments()
        return optimal_assignments[0]
    
    def get_role_distribution(self) -> Dict[str, int]:
        """
        Get role distribution based on primary preferred roles.
        
        Returns:
            Dictionary with role counts
        """
        roles = {role: 0 for role in self.ROLES}
        roles["unknown"] = 0
        
        for player in self.players:
            if player.preferred_roles and len(player.preferred_roles) > 0:
                primary_role = player.preferred_roles[0]
                if primary_role in roles:
                    roles[primary_role] += 1
                else:
                    roles["unknown"] += 1
            else:
                roles["unknown"] += 1
        
        return roles
    
    def get_role_distribution_summary(self) -> Dict[str, int]:
        """
        Get simplified role distribution (cores vs supports).
        
        Returns:
            Dictionary with core/support/unknown counts
        """
        distribution = self.get_role_distribution()
        cores = distribution["1"] + distribution["2"] + distribution["3"]
        supports = distribution["4"] + distribution["5"]
        return {
            "cores": cores,
            "supports": supports,
            "unknown": distribution["unknown"]
        }
    
    def has_balanced_roles(self, target_cores: int = 3, target_supports: int = 2) -> bool:
        """Check if team has balanced role distribution."""
        summary = self.get_role_distribution_summary()
        return (summary["cores"] == target_cores and 
                summary["supports"] == target_supports)
    
    def get_role_balance_score(self) -> float:
        """
        Calculate a score for how well-balanced the roles are.
        Lower is better (0 = perfectly balanced).
        """
        distribution = self.get_role_distribution()
        target_per_role = 1
        penalty = 0
        for role in self.ROLES:
            count = distribution.get(role, 0)
            penalty += abs(count - target_per_role)
        return penalty
    
    def get_player_by_role(self, role: str, use_glicko: bool = True, off_role_multiplier: float = 0.9) -> Tuple[Player, float]:
        """
        Get the player assigned to a specific role and their effective value.
        
        Args:
            role: Role to get (1-5)
            use_glicko: Whether to use Glicko-2 ratings
            off_role_multiplier: Multiplier for rating when playing off-role
        
        Returns:
            Tuple of (player, effective_value)
        """
        if not self.role_assignments:
            self.role_assignments = self._assign_roles_optimally()
        
        # Find the player assigned to this role
        for player, assigned_role in zip(self.players, self.role_assignments):
            if assigned_role == role:
                base_value = player.get_value(use_glicko)
                if player.preferred_roles and role in player.preferred_roles:
                    effective_value = base_value
                else:
                    effective_value = base_value * off_role_multiplier
                return (player, effective_value)
        
        # Should never happen if team is valid, but return first player as fallback
        if self.players:
            player = self.players[0]
            base_value = player.get_value(use_glicko)
            return (player, base_value * off_role_multiplier)
        
        raise ValueError(f"No player found for role {role}")
    
    def __str__(self) -> str:
        player_names = ", ".join(p.name for p in self.players)
        return f"Team: {player_names}"

