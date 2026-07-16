"""
Draft domain model for Immortal Draft mode.
"""

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class DraftPhase(Enum):
    """Phases of the draft process."""

    COINFLIP = "coinflip"
    WINNER_CHOICE = "winner_choice"  # Winner chooses side OR hero pick
    WINNER_SIDE_CHOICE = "winner_side_choice"  # Winner chose to pick side
    WINNER_HERO_CHOICE = "winner_hero_choice"  # Winner chose to pick hero order
    LOSER_CHOICE = "loser_choice"  # Loser gets remaining choice
    DRAFTING = "drafting"  # Active player draft
    COMPLETE = "complete"


@dataclass
class DraftState:
    """
    Represents the state of an Immortal Draft session.

    Tracks:
    - Captain assignments and sides
    - Pre-draft choices (coinflip, side/hero pick)
    - Player draft progress
    - Player side preferences
    - UI state (message IDs)
    """

    guild_id: int

    # Pool of 10 players selected for this draft
    player_pool_ids: list[int] = field(default_factory=list)

    # Cached player data for the pool (avoids repeated DB queries)
    # Maps discord_id -> {name: str, rating: float, roles: list[str]}
    player_pool_data: dict[int, dict] = field(default_factory=dict)

    # Excluded players (if lobby had >10)
    excluded_player_ids: list[int] = field(default_factory=list)

    # Exclusion-factor bookkeeping carried into the pending match on completion.
    full_exclusion_increment_ids: list[int] = field(default_factory=list)
    half_exclusion_increment_ids: list[int] = field(default_factory=list)

    # Captains
    captain1_id: int | None = None  # First captain selected
    captain2_id: int | None = None  # Second captain selected
    captain1_rating: float = 0.0
    captain2_rating: float = 0.0

    # Side assignments (set after pre-draft choices)
    radiant_captain_id: int | None = None
    dire_captain_id: int | None = None

    # Coinflip
    coinflip_winner_id: int | None = None

    # Pre-draft choices
    # "side" or "hero_pick" - what the coinflip winner chose to pick
    winner_choice_type: str | None = None
    # The actual choice made by winner (e.g., "radiant" or "first")
    winner_choice_value: str | None = None
    # The choice made by loser
    loser_choice_value: str | None = None

    # Hero draft order (1 = first pick, 2 = second pick)
    radiant_hero_pick_order: int | None = None
    dire_hero_pick_order: int | None = None

    # Player draft
    current_round_first_captain_id: int | None = None
    current_pick_index: int = 0  # 0-7 for 8 picks

    # Teams being built during draft
    radiant_player_ids: list[int] = field(default_factory=list)
    dire_player_ids: list[int] = field(default_factory=list)

    # Player side preferences (live during draft)
    # Maps discord_id -> "radiant" | "dire"
    side_preferences: dict[int, str] = field(default_factory=dict)

    # Current phase
    phase: DraftPhase = DraftPhase.COINFLIP

    # UI state
    draft_message_id: int | None = None
    draft_channel_id: int | None = None
    captain_ping_message_id: int | None = None  # Ping message to delete after first choice

    @property
    def available_player_ids(self) -> list[int]:
        """Get list of players not yet picked (excluding captains who are auto-assigned)."""
        picked = set(self.radiant_player_ids) | set(self.dire_player_ids)
        # Explicitly exclude captains as defensive measure
        captain_ids = {self.radiant_captain_id, self.dire_captain_id} - {None}
        return [pid for pid in self.player_pool_ids if pid not in picked and pid not in captain_ids]

    @property
    def current_captain_id(self) -> int | None:
        """Get the ID of the captain whose turn it is to pick."""
        if self.phase != DraftPhase.DRAFTING:
            return None
        if self.current_pick_index >= 8:
            return None

        if self.current_pick_index % 2 == 0:
            return self.current_round_first_captain_id
        return self._other_captain_id(self.current_round_first_captain_id)

    @property
    def current_captain_team(self) -> str | None:
        """Get the team of the current picking captain."""
        captain_id = self.current_captain_id
        if captain_id is None:
            return None
        if captain_id == self.radiant_captain_id:
            return "radiant"
        return "dire"

    @property
    def picks_remaining_this_turn(self) -> int:
        """Each captain makes exactly one pick in the current round."""
        if self.phase != DraftPhase.DRAFTING:
            return 0
        if self.current_pick_index >= 8:
            return 0
        return 1

    def _player_rating(self, player_id: int) -> float:
        player_data = self.player_pool_data.get(player_id, {})
        rating = player_data.get("rating")
        if rating is not None:
            return float(rating)
        if player_id == self.captain1_id:
            return self.captain1_rating
        if player_id == self.captain2_id:
            return self.captain2_rating
        return 1500.0

    @property
    def radiant_rating_total(self) -> float:
        """Glicko total for the current Radiant roster."""
        return sum(self._player_rating(player_id) for player_id in self.radiant_player_ids)

    @property
    def dire_rating_total(self) -> float:
        """Glicko total for the current Dire roster."""
        return sum(self._player_rating(player_id) for player_id in self.dire_player_ids)

    def _other_captain_id(self, captain_id: int | None) -> int | None:
        if captain_id == self.radiant_captain_id:
            return self.dire_captain_id
        if captain_id == self.dire_captain_id:
            return self.radiant_captain_id
        return None

    def _choose_round_first_captain(self) -> int | None:
        if self.radiant_rating_total < self.dire_rating_total:
            return self.radiant_captain_id
        if self.dire_rating_total < self.radiant_rating_total:
            return self.dire_captain_id

        if self.current_pick_index == 0:
            return self._other_captain_id(self.coinflip_winner_id) or self.radiant_captain_id

        return (
            self._other_captain_id(self.current_round_first_captain_id)
            or self.radiant_captain_id
        )

    def start_player_draft(self) -> None:
        """Assign captains to teams and start the first two-pick round."""
        if self.radiant_captain_id not in self.radiant_player_ids:
            self.radiant_player_ids.append(self.radiant_captain_id)
        if self.dire_captain_id not in self.dire_player_ids:
            self.dire_player_ids.append(self.dire_captain_id)
        self.current_pick_index = 0
        self.phase = DraftPhase.DRAFTING
        self.current_round_first_captain_id = self._choose_round_first_captain()

    @property
    def is_draft_complete(self) -> bool:
        """Check if all 8 picks have been made."""
        return self.current_pick_index >= 8

    def pick_player(self, player_id: int, picker_id: int | None = None) -> bool:
        """
        Pick a player for the current captain's team.

        Args:
            player_id: Discord ID of player to pick
            picker_id: Discord ID of the captain attempting the pick. When
                provided, the pick is rejected unless it is that captain's turn.
                This guard runs synchronously (no awaits between the check and the
                state mutation), so concurrent button callbacks for the same
                captain can never both land — the first pick advances the round
                order and changes ``current_captain_id``, so the second's
                ``picker_id`` no longer matches.

        Returns:
            True if pick was successful, False otherwise
        """
        if self.phase != DraftPhase.DRAFTING:
            return False
        if picker_id is not None and picker_id != self.current_captain_id:
            return False
        if player_id not in self.available_player_ids:
            return False

        team = self.current_captain_team
        if team == "radiant":
            self.radiant_player_ids.append(player_id)
        elif team == "dire":
            self.dire_player_ids.append(player_id)
        else:
            return False

        # Clear side preference for picked player
        self.side_preferences.pop(player_id, None)

        # Advance to next pick
        self.current_pick_index += 1

        # Check if draft is complete
        if self.is_draft_complete:
            self.phase = DraftPhase.COMPLETE
        elif self.current_pick_index % 2 == 0:
            self.current_round_first_captain_id = self._choose_round_first_captain()

        return True

    def set_side_preference(self, player_id: int, side: str | None) -> bool:
        """
        Set a player's side preference.

        Args:
            player_id: Discord ID of player
            side: "radiant", "dire", or None to clear

        Returns:
            True if preference was set, False if player not available
        """
        if player_id not in self.available_player_ids:
            return False

        if side is None:
            self.side_preferences.pop(player_id, None)
        else:
            self.side_preferences[player_id] = side
        return True

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary format for serialization."""
        return {
            "guild_id": self.guild_id,
            "player_pool_ids": self.player_pool_ids,
            "player_pool_data": self.player_pool_data,
            "excluded_player_ids": self.excluded_player_ids,
            "full_exclusion_increment_ids": self.full_exclusion_increment_ids,
            "half_exclusion_increment_ids": self.half_exclusion_increment_ids,
            "captain1_id": self.captain1_id,
            "captain2_id": self.captain2_id,
            "captain1_rating": self.captain1_rating,
            "captain2_rating": self.captain2_rating,
            "radiant_captain_id": self.radiant_captain_id,
            "dire_captain_id": self.dire_captain_id,
            "coinflip_winner_id": self.coinflip_winner_id,
            "winner_choice_type": self.winner_choice_type,
            "winner_choice_value": self.winner_choice_value,
            "loser_choice_value": self.loser_choice_value,
            "radiant_hero_pick_order": self.radiant_hero_pick_order,
            "dire_hero_pick_order": self.dire_hero_pick_order,
            "current_round_first_captain_id": self.current_round_first_captain_id,
            "current_pick_index": self.current_pick_index,
            "radiant_player_ids": self.radiant_player_ids,
            "dire_player_ids": self.dire_player_ids,
            "side_preferences": self.side_preferences,
            "phase": self.phase.value,
            "draft_message_id": self.draft_message_id,
            "draft_channel_id": self.draft_channel_id,
            "captain_ping_message_id": self.captain_ping_message_id,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DraftState":
        """Create from dictionary format."""
        state = cls(guild_id=data["guild_id"])
        state.player_pool_ids = data.get("player_pool_ids", [])
        state.player_pool_data = data.get("player_pool_data", {})
        state.excluded_player_ids = data.get("excluded_player_ids", [])
        state.full_exclusion_increment_ids = data.get("full_exclusion_increment_ids", [])
        state.half_exclusion_increment_ids = data.get("half_exclusion_increment_ids", [])
        state.captain1_id = data.get("captain1_id")
        state.captain2_id = data.get("captain2_id")
        state.captain1_rating = data.get("captain1_rating", 0.0)
        state.captain2_rating = data.get("captain2_rating", 0.0)
        state.radiant_captain_id = data.get("radiant_captain_id")
        state.dire_captain_id = data.get("dire_captain_id")
        state.coinflip_winner_id = data.get("coinflip_winner_id")
        state.winner_choice_type = data.get("winner_choice_type")
        state.winner_choice_value = data.get("winner_choice_value")
        state.loser_choice_value = data.get("loser_choice_value")
        state.radiant_hero_pick_order = data.get("radiant_hero_pick_order")
        state.dire_hero_pick_order = data.get("dire_hero_pick_order")
        state.current_round_first_captain_id = data.get("current_round_first_captain_id")
        state.current_pick_index = data.get("current_pick_index", 0)
        state.radiant_player_ids = data.get("radiant_player_ids", [])
        state.dire_player_ids = data.get("dire_player_ids", [])
        state.side_preferences = data.get("side_preferences", {})
        state.phase = DraftPhase(data.get("phase", "coinflip"))
        state.draft_message_id = data.get("draft_message_id")
        state.draft_channel_id = data.get("draft_channel_id")
        state.captain_ping_message_id = data.get("captain_ping_message_id")
        return state
