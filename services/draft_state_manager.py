"""
Draft state management for Immortal Draft mode.

Handles in-memory state for active drafts, separated from business logic.
"""

import logging

from domain.models.draft import DraftPhase, DraftState

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

    def _normalize_guild_id(self, guild_id: int | None) -> int:
        """Normalize guild ID (0 for DMs/None)."""
        return guild_id if guild_id is not None else 0

    def get_state(self, guild_id: int | None = None) -> DraftState | None:
        """
        Get the current draft state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)

        Returns:
            DraftState or None if no active draft
        """
        return self._states.get(self._normalize_guild_id(guild_id))

    def create_draft(self, guild_id: int | None) -> DraftState:
        """
        Create a new draft state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)

        Returns:
            New DraftState

        Raises:
            ValueError: If a draft already exists for this guild
        """
        normalized = self._normalize_guild_id(guild_id)
        if normalized in self._states:
            raise ValueError("A draft is already in progress for this server.")

        state = DraftState(guild_id=normalized)
        self._states[normalized] = state
        logger.info(f"Created new draft state for guild {normalized}")
        return state

    def set_state(self, guild_id: int | None, state: DraftState) -> None:
        """
        Store draft state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)
            state: Draft state to store
        """
        self._states[self._normalize_guild_id(guild_id)] = state

    def clear_state(self, guild_id: int | None) -> DraftState | None:
        """
        Clear draft state for a guild.

        Args:
            guild_id: Guild ID (or None for DMs)

        Returns:
            The cleared state, or None if no state existed
        """
        normalized = self._normalize_guild_id(guild_id)
        state = self._states.pop(normalized, None)
        if state:
            logger.info(f"Cleared draft state for guild {normalized}")
        return state

    def has_active_draft(self, guild_id: int | None = None) -> bool:
        """Check if there's an active draft for the guild."""
        state = self.get_state(guild_id)
        if state is None:
            return False
        # Draft is active if not complete
        return state.phase != DraftPhase.COMPLETE

    def get_draft_phase(self, guild_id: int | None = None) -> DraftPhase | None:
        """Get the current phase of the draft."""
        state = self.get_state(guild_id)
        return state.phase if state else None

    def advance_phase(self, guild_id: int | None, new_phase: DraftPhase) -> bool:
        """
        Advance the draft to a new phase.

        Args:
            guild_id: Guild ID
            new_phase: The phase to advance to

        Returns:
            True if phase was updated, False if no draft exists
        """
        state = self.get_state(guild_id)
        if state is None:
            return False

        old_phase = state.phase
        state.phase = new_phase
        logger.info(f"Draft phase advanced: {old_phase.value} -> {new_phase.value} (guild {guild_id})")
        return True
