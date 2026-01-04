"""
Match state management.

Handles in-memory state for pending matches, separated from business logic.
"""

from typing import Any

from domain.models.team import Team


class MatchState:
    """
    Represents the state of a pending match after shuffle.
    """

    def __init__(
        self,
        radiant_team_ids: list,
        dire_team_ids: list,
        excluded_player_ids: list,
        radiant_team: Team,
        dire_team: Team,
        radiant_roles: list,
        dire_roles: list,
        radiant_value: float,
        dire_value: float,
        first_pick_team: str,
    ):
        self.radiant_team_ids = radiant_team_ids
        self.dire_team_ids = dire_team_ids
        self.excluded_player_ids = excluded_player_ids
        self.radiant_team = radiant_team
        self.dire_team = dire_team
        self.radiant_roles = radiant_roles
        self.dire_roles = dire_roles
        self.radiant_value = radiant_value
        self.dire_value = dire_value
        self.first_pick_team = first_pick_team

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary format."""
        return {
            "radiant_team_ids": self.radiant_team_ids,
            "dire_team_ids": self.dire_team_ids,
            "excluded_player_ids": self.excluded_player_ids,
            "radiant_team": self.radiant_team,
            "dire_team": self.dire_team,
            "radiant_roles": self.radiant_roles,
            "dire_roles": self.dire_roles,
            "radiant_value": self.radiant_value,
            "dire_value": self.dire_value,
            "first_pick_team": self.first_pick_team,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "MatchState":
        """Create from dictionary format."""
        return cls(
            radiant_team_ids=data["radiant_team_ids"],
            dire_team_ids=data["dire_team_ids"],
            excluded_player_ids=data.get("excluded_player_ids", []),
            radiant_team=data["radiant_team"],
            dire_team=data["dire_team"],
            radiant_roles=data["radiant_roles"],
            dire_roles=data["dire_roles"],
            radiant_value=data["radiant_value"],
            dire_value=data["dire_value"],
            first_pick_team=data["first_pick_team"],
        )

    def get_winning_ids(self, winning_team: str) -> list:
        """Get player IDs for the winning team."""
        if winning_team == "radiant":
            return self.radiant_team_ids
        elif winning_team == "dire":
            return self.dire_team_ids
        raise ValueError(f"Invalid winning_team: {winning_team}")

    def get_losing_ids(self, winning_team: str) -> list:
        """Get player IDs for the losing team."""
        if winning_team == "radiant":
            return self.dire_team_ids
        elif winning_team == "dire":
            return self.radiant_team_ids
        raise ValueError(f"Invalid winning_team: {winning_team}")


class MatchStateManager:
    """
    Manages in-memory state for pending matches.

    Responsibilities:
    - Store shuffle results per guild
    - Retrieve match state for recording
    - Clear state after match completion

    This is separate from business logic to maintain single responsibility.
    """

    def __init__(self):
        self._states: dict[int, MatchState] = {}

    def _normalize_guild_id(self, guild_id: int | None) -> int:
        """Normalize guild ID (0 for DMs/None)."""
        return guild_id if guild_id is not None else 0

    def get_state(self, guild_id: int | None = None) -> MatchState | None:
        """
        Get the current match state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)

        Returns:
            MatchState or None if no pending match
        """
        return self._states.get(self._normalize_guild_id(guild_id))

    def set_state(self, guild_id: int | None, state: MatchState) -> None:
        """
        Store match state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)
            state: Match state to store
        """
        self._states[self._normalize_guild_id(guild_id)] = state

    def clear_state(self, guild_id: int | None) -> None:
        """
        Clear match state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)
        """
        self._states.pop(self._normalize_guild_id(guild_id), None)

    def has_pending_match(self, guild_id: int | None = None) -> bool:
        """Check if there's a pending match for the guild."""
        return self._normalize_guild_id(guild_id) in self._states

    # Legacy compatibility methods
    def get_last_shuffle(self, guild_id: int | None = None) -> dict | None:
        """Legacy method for backward compatibility."""
        state = self.get_state(guild_id)
        return state.to_dict() if state else None

    def set_last_shuffle(self, guild_id: int | None, payload: dict) -> None:
        """Legacy method for backward compatibility."""
        state = MatchState.from_dict(payload)
        self.set_state(guild_id, state)

    def clear_last_shuffle(self, guild_id: int | None) -> None:
        """Legacy method for backward compatibility."""
        self.clear_state(guild_id)
