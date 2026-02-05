"""
Lobby domain model.

Note: LobbyManager has been moved to services/lobby_manager_service.py
to maintain clean architecture (domain should not depend on infrastructure).
"""

from dataclasses import dataclass, field
from datetime import datetime


@dataclass
class Lobby:
    """Represents a matchmaking lobby."""

    lobby_id: int
    created_by: int  # Discord ID of creator
    created_at: datetime
    players: set[int] = field(default_factory=set)
    conditional_players: set[int] = field(default_factory=set)  # "Frogling" players
    status: str = "open"
    designated_player_id: int | None = None  # Temporary AFK removal permissions

    def add_player(self, discord_id: int) -> bool:
        """Add a player to the regular queue. Removes from conditional if present."""
        if self.status != "open":
            return False
        if discord_id in self.players:
            return False
        # Remove from conditional if switching
        self.conditional_players.discard(discord_id)
        self.players.add(discord_id)
        return True

    def remove_player(self, discord_id: int) -> bool:
        if discord_id in self.players:
            self.players.remove(discord_id)
            return True
        return False

    def add_conditional_player(self, discord_id: int) -> bool:
        """Add a player to the conditional queue. Removes from regular if present."""
        if self.status != "open":
            return False
        if discord_id in self.conditional_players:
            return False
        # Remove from regular if switching
        self.players.discard(discord_id)
        self.conditional_players.add(discord_id)
        return True

    def remove_conditional_player(self, discord_id: int) -> bool:
        if discord_id in self.conditional_players:
            self.conditional_players.remove(discord_id)
            return True
        return False

    def is_player_conditional(self, discord_id: int) -> bool:
        """Check if a player is in the conditional set."""
        return discord_id in self.conditional_players

    def get_player_count(self) -> int:
        """Return count of regular players only."""
        return len(self.players)

    def get_conditional_count(self) -> int:
        """Return count of conditional players."""
        return len(self.conditional_players)

    def get_total_count(self) -> int:
        """Return combined count of regular and conditional players."""
        return len(self.players) + len(self.conditional_players)

    def is_ready(self, min_players: int = 10) -> bool:
        """Ready if combined total meets threshold."""
        return self.get_total_count() >= min_players

    def can_create_teams(self, player_roles: dict[int, list[str]]) -> bool:
        role_counts = {"1": 0, "2": 0, "3": 0, "4": 0, "5": 0}

        # Include both regular and conditional players
        all_players = self.players | self.conditional_players
        for player_id in all_players:
            if player_id in player_roles and player_roles[player_id]:
                primary_role = player_roles[player_id][0]
                if primary_role in role_counts:
                    role_counts[primary_role] += 1

        return all(count >= 2 for count in role_counts.values())

    def to_dict(self) -> dict:
        return {
            "lobby_id": self.lobby_id,
            "created_by": self.created_by,
            "created_at": self.created_at.isoformat(),
            "players": list(self.players),
            "conditional_players": list(self.conditional_players),
            "status": self.status,
            "designated_player_id": self.designated_player_id,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "Lobby":
        created_at = data.get("created_at")
        created_at_dt = datetime.fromisoformat(created_at) if created_at else datetime.now()
        players = set(data.get("players", []))
        conditional_players = set(data.get("conditional_players", []))
        return cls(
            lobby_id=data.get("lobby_id", 1),
            created_by=data.get("created_by", 0),
            created_at=created_at_dt,
            players=players,
            conditional_players=conditional_players,
            status=data.get("status", "open"),
            designated_player_id=data.get("designated_player_id"),
        )


# LobbyManager has been moved to services/lobby_manager_service.py
# Import it from there for backward compatibility:
# from services.lobby_manager_service import LobbyManagerService as LobbyManager
