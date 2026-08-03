"""
Draft state management for Immortal Draft mode.

Handles in-memory state for active drafts, separated from business logic.
"""

import logging
import threading

from domain.models.draft import DraftPhase, DraftState
from domain.models.lobby import LobbyKind
from utils.guild import normalize_guild_id

logger = logging.getLogger("cama_bot.services.draft_state_manager")


class DraftStateManager:
    """
    Manages in-memory state for active drafts.

    Responsibilities:
    - Store draft state per guild
    - Retrieve draft state for operations
    - Clear state after draft completion or cancellation

    This is separate from business logic to maintain single responsibility.
    """

    def __init__(self):
        self._states: dict[int, DraftState] = {}
        self._state_lock = threading.RLock()

    def get_state(self, guild_id: int | None = None) -> DraftState | None:
        """
        Get the current draft state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)

        Returns:
            DraftState or None if no active draft
        """
        with self._state_lock:
            return self._states.get(normalize_guild_id(guild_id))

    def create_draft(
        self,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> DraftState:
        """
        Create a new draft state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)

        Returns:
            New DraftState

        Raises:
            ValueError: If an active (non-COMPLETE) draft already exists for this guild
        """
        normalized = normalize_guild_id(guild_id)
        with self._state_lock:
            existing = self._states.get(normalized)
            if existing is not None:
                raise ValueError("A draft is already in progress for this server.")

            state = DraftState(
                guild_id=normalized,
                lobby_kind=LobbyKind.normalize(lobby_kind),
            )
            self._states[normalized] = state
        logger.info(f"Created new draft state for guild {normalized}")
        return state


    def clear_state(
        self,
        guild_id: int | None,
        expected_state: DraftState | None = None,
    ) -> DraftState | None:
        """
        Clear draft state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)

        Returns:
            The cleared state, or None if no matching state existed
        """
        normalized = normalize_guild_id(guild_id)
        with self._state_lock:
            state = self._states.get(normalized)
            if state is None or (
                expected_state is not None and state is not expected_state
            ):
                return None
            del self._states[normalized]
        if state:
            logger.info(f"Cleared draft state for guild {normalized}")
        return state

    def has_active_draft(self, guild_id: int | None = None) -> bool:
        """Check if there's an active draft for the guild."""
        with self._state_lock:
            state = self._states.get(normalize_guild_id(guild_id))
            return state is not None


    def advance_phase(self, guild_id: int | None, new_phase: DraftPhase) -> bool:
        """
        Advance the draft to a new phase.

        Args:
            guild_id: Guild ID
            new_phase: The phase to advance to

        Returns:
            True if phase was updated, False if no draft exists
        """
        with self._state_lock:
            state = self._states.get(normalize_guild_id(guild_id))
            if state is None:
                return False

            old_phase = state.phase
            state.phase = new_phase
        logger.info(f"Draft phase advanced: {old_phase.value} -> {new_phase.value} (guild {guild_id})")
        return True
