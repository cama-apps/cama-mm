"""
Match state management.

Handles in-memory state for pending matches, separated from business logic.
Supports multiple concurrent pending matches per guild.
"""

from typing import Any

from domain.models.team import Team
from utils.guild import normalize_guild_id


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
        excluded_conditional_player_ids: list | None = None,
        pending_match_id: int | None = None,
    ):
        self.radiant_team_ids = radiant_team_ids
        self.dire_team_ids = dire_team_ids
        self.excluded_player_ids = excluded_player_ids
        self.excluded_conditional_player_ids = excluded_conditional_player_ids or []
        self.radiant_team = radiant_team
        self.dire_team = dire_team
        self.radiant_roles = radiant_roles
        self.dire_roles = dire_roles
        self.radiant_value = radiant_value
        self.dire_value = dire_value
        self.first_pick_team = first_pick_team
        self.pending_match_id = pending_match_id

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary format."""
        return {
            "radiant_team_ids": self.radiant_team_ids,
            "dire_team_ids": self.dire_team_ids,
            "excluded_player_ids": self.excluded_player_ids,
            "excluded_conditional_player_ids": self.excluded_conditional_player_ids,
            "radiant_team": self.radiant_team,
            "dire_team": self.dire_team,
            "radiant_roles": self.radiant_roles,
            "dire_roles": self.dire_roles,
            "radiant_value": self.radiant_value,
            "dire_value": self.dire_value,
            "first_pick_team": self.first_pick_team,
            "pending_match_id": self.pending_match_id,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "MatchState":
        """Create from dictionary format."""
        return cls(
            radiant_team_ids=data["radiant_team_ids"],
            dire_team_ids=data["dire_team_ids"],
            excluded_player_ids=data.get("excluded_player_ids", []),
            excluded_conditional_player_ids=data.get("excluded_conditional_player_ids", []),
            radiant_team=data["radiant_team"],
            dire_team=data["dire_team"],
            radiant_roles=data["radiant_roles"],
            dire_roles=data["dire_roles"],
            radiant_value=data["radiant_value"],
            dire_value=data["dire_value"],
            first_pick_team=data["first_pick_team"],
            pending_match_id=data.get("pending_match_id"),
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

    def contains_player(self, discord_id: int) -> bool:
        """Check if a player is in this match (either team)."""
        return discord_id in self.radiant_team_ids or discord_id in self.dire_team_ids


class MatchStateManager:
    """
    Manages in-memory state for pending matches.

    Responsibilities:
    - Store shuffle results per guild (supports multiple concurrent matches)
    - Retrieve match state for recording
    - Clear state after match completion

    This is separate from business logic to maintain single responsibility.

    Structure: dict[guild_id, dict[pending_match_id, MatchState]]
    """

    def __init__(self):
        # Nested dict: guild_id -> pending_match_id -> MatchState
        self._states: dict[int, dict[int, MatchState]] = {}

    def get_state(self, guild_id: int | None = None, pending_match_id: int | None = None) -> MatchState | None:
        """
        Get a match state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)
            pending_match_id: If provided, get specific match. If None, returns
                             the single match if only one exists.

        Returns:
            MatchState or None if no matching pending match
        """
        normalized = normalize_guild_id(guild_id)
        guild_states = self._states.get(normalized, {})

        if pending_match_id is not None:
            return guild_states.get(pending_match_id)

        # Backward compat: return single match if only one exists
        if len(guild_states) == 1:
            return next(iter(guild_states.values()))

        return None

    def get_all_states(self, guild_id: int | None = None) -> list[MatchState]:
        """
        Get all pending match states for a guild.

        Returns:
            List of MatchState objects, sorted by pending_match_id
        """
        normalized = normalize_guild_id(guild_id)
        guild_states = self._states.get(normalized, {})
        return sorted(guild_states.values(), key=lambda s: s.pending_match_id or 0)

    def get_state_for_player(self, guild_id: int | None, discord_id: int) -> MatchState | None:
        """
        Find the pending match that contains a specific player.

        Args:
            guild_id: Guild ID
            discord_id: Player's Discord ID

        Returns:
            MatchState if player is in a pending match, None otherwise
        """
        normalized = normalize_guild_id(guild_id)
        guild_states = self._states.get(normalized, {})
        for state in guild_states.values():
            if state.contains_player(discord_id):
                return state
        return None

    def get_all_pending_player_ids(self, guild_id: int | None = None) -> set[int]:
        """
        Get all player IDs currently in any pending match for a guild.

        Returns:
            Set of Discord IDs of all players in pending matches
        """
        normalized = normalize_guild_id(guild_id)
        guild_states = self._states.get(normalized, {})
        player_ids = set()
        for state in guild_states.values():
            player_ids.update(state.radiant_team_ids)
            player_ids.update(state.dire_team_ids)
        return player_ids

    def set_state(self, guild_id: int | None, state: MatchState) -> None:
        """
        Store match state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)
            state: Match state to store (must have pending_match_id set)
        """
        if state.pending_match_id is None:
            raise ValueError("MatchState must have pending_match_id set")

        normalized = normalize_guild_id(guild_id)
        if normalized not in self._states:
            self._states[normalized] = {}
        self._states[normalized][state.pending_match_id] = state

    def clear_state(self, guild_id: int | None, pending_match_id: int | None = None) -> None:
        """
        Clear match state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)
            pending_match_id: If provided, clear only this specific match.
                             If None, clear ALL matches for the guild.
        """
        normalized = normalize_guild_id(guild_id)

        if pending_match_id is not None:
            if normalized in self._states:
                self._states[normalized].pop(pending_match_id, None)
                # Clean up empty guild dict
                if not self._states[normalized]:
                    del self._states[normalized]
        else:
            # Clear all matches for guild
            self._states.pop(normalized, None)

    def has_pending_match(self, guild_id: int | None = None) -> bool:
        """Check if there's any pending match for the guild."""
        normalized = normalize_guild_id(guild_id)
        return bool(self._states.get(normalized))

    def get_pending_match_count(self, guild_id: int | None = None) -> int:
        """Get the number of pending matches for a guild."""
        normalized = normalize_guild_id(guild_id)
        return len(self._states.get(normalized, {}))

    # Legacy compatibility methods
    def get_last_shuffle(self, guild_id: int | None = None) -> dict | None:
        """
        Legacy method for backward compatibility.

        Returns the single pending match if exactly one exists, None otherwise.
        """
        state = self.get_state(guild_id)
        return state.to_dict() if state else None

    def set_last_shuffle(self, guild_id: int | None, payload: dict) -> None:
        """Legacy method for backward compatibility."""
        state = MatchState.from_dict(payload)
        if state.pending_match_id is None:
            raise ValueError("payload must include pending_match_id")
        self.set_state(guild_id, state)

    def clear_last_shuffle(self, guild_id: int | None, pending_match_id: int | None = None) -> None:
        """Legacy method for backward compatibility."""
        self.clear_state(guild_id, pending_match_id)
