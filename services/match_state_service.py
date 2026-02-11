"""
Match state management service.

Handles pending match state: shuffle results, message metadata, and persistence.
Supports multiple concurrent pending matches per guild.

Thread Safety:
    All public methods that read or modify state are protected by _shuffle_state_lock.
    For operations requiring atomic read-modify-write cycles (like voting), use the
    state_lock() context manager to hold the lock for the entire operation.
"""

import threading
from contextlib import contextmanager
from typing import Any, Generator

from repositories.interfaces import IMatchRepository


class MatchStateService:
    """
    Manages pending match state for shuffle results and voting.

    This service handles:
    - In-memory cache of pending match state per guild (supports multiple concurrent matches)
    - Persistence of state to database
    - Message metadata storage for Discord UI updates

    Structure: dict[guild_id, dict[pending_match_id, state]]
    """

    def __init__(self, match_repo: IMatchRepository):
        """
        Initialize MatchStateService.

        Args:
            match_repo: Repository for match data persistence
        """
        self.match_repo = match_repo
        # Nested dict: guild_id -> pending_match_id -> state
        self._last_shuffle_by_guild: dict[int, dict[int, dict]] = {}
        self._shuffle_state_lock = threading.RLock()

    def _normalize_guild_id(self, guild_id: int | None) -> int:
        """Normalize guild_id to handle None case."""
        return guild_id if guild_id is not None else 0

    @contextmanager
    def state_lock(self) -> Generator[None, None, None]:
        """
        Context manager for acquiring the state lock.

        Use this for atomic read-modify-write operations that span multiple
        method calls. The lock is reentrant, so nested acquisitions are safe.

        Example:
            with state_service.state_lock():
                state = state_service.ensure_pending_state(guild_id)
                submissions = state_service.ensure_record_submissions(state)
                submissions[user_id] = {"result": result, "is_admin": is_admin}
                state_service.persist_state(guild_id, state)

        Yields:
            None - the lock is held while in the context
        """
        with self._shuffle_state_lock:
            yield

    def get_last_shuffle(self, guild_id: int | None = None, pending_match_id: int | None = None) -> dict | None:
        """
        Get the pending shuffle state for a guild.

        First checks in-memory cache, then falls back to database.

        Args:
            guild_id: Guild ID to look up
            pending_match_id: If provided, get specific match. If None, returns
                             the single match if only one exists.

        Returns:
            Pending match state dict or None if no pending shuffle
        """
        with self._shuffle_state_lock:
            normalized = self._normalize_guild_id(guild_id)
            guild_states = self._last_shuffle_by_guild.get(normalized, {})

            if pending_match_id is not None:
                state = guild_states.get(pending_match_id)
                if state:
                    return state
                # Try to load from DB
                persisted = self.match_repo.get_pending_match_by_id(pending_match_id)
                if persisted:
                    if normalized not in self._last_shuffle_by_guild:
                        self._last_shuffle_by_guild[normalized] = {}
                    self._last_shuffle_by_guild[normalized][pending_match_id] = persisted
                    return persisted
                return None

            # Backward compat: return single match if only one exists in memory
            if len(guild_states) == 1:
                return next(iter(guild_states.values()))

            # Check database for single match (legacy behavior)
            persisted = self.match_repo.get_pending_match(guild_id)
            if persisted:
                pmid = persisted.get("pending_match_id")
                if pmid:
                    if normalized not in self._last_shuffle_by_guild:
                        self._last_shuffle_by_guild[normalized] = {}
                    self._last_shuffle_by_guild[normalized][pmid] = persisted
                return persisted
            return None

    def get_all_pending_matches(self, guild_id: int | None = None) -> list[dict]:
        """
        Get all pending match states for a guild.

        Args:
            guild_id: Guild ID to look up

        Returns:
            List of pending match state dicts
        """
        with self._shuffle_state_lock:
            normalized = self._normalize_guild_id(guild_id)

            # Load from database
            persisted = self.match_repo.get_pending_matches(guild_id)

            # Update in-memory cache
            if normalized not in self._last_shuffle_by_guild:
                self._last_shuffle_by_guild[normalized] = {}

            for match in persisted:
                pmid = match.get("pending_match_id")
                if pmid:
                    self._last_shuffle_by_guild[normalized][pmid] = match

            return persisted

    def get_pending_match_for_player(self, guild_id: int | None, discord_id: int) -> dict | None:
        """
        Find the pending match that contains a specific player.

        Args:
            guild_id: Guild ID
            discord_id: Player's Discord ID

        Returns:
            Pending match state if player is a participant, None otherwise
        """
        with self._shuffle_state_lock:
            # Use repository method which checks payload
            return self.match_repo.get_pending_match_for_player(guild_id, discord_id)

    def get_all_pending_player_ids(self, guild_id: int | None = None) -> set[int]:
        """
        Get all player IDs currently in any pending match for a guild.

        Returns:
            Set of Discord IDs of all players in pending matches
        """
        with self._shuffle_state_lock:
            return self.match_repo.get_all_pending_match_player_ids(guild_id)

    def set_last_shuffle(self, guild_id: int | None, payload: dict) -> None:
        """
        Set the pending shuffle state for a guild.

        Updates in-memory cache only. Use persist_state() to save to database.

        Args:
            guild_id: Guild ID
            payload: The pending match state dict (must have pending_match_id)
        """
        with self._shuffle_state_lock:
            pending_match_id = payload.get("pending_match_id")
            if pending_match_id is None:
                # Legacy single-match mode - use 0 as placeholder
                pending_match_id = 0

            normalized = self._normalize_guild_id(guild_id)
            if normalized not in self._last_shuffle_by_guild:
                self._last_shuffle_by_guild[normalized] = {}
            self._last_shuffle_by_guild[normalized][pending_match_id] = payload

    def set_shuffle_message_url(self, guild_id: int | None, jump_url: str, pending_match_id: int | None = None) -> None:
        """
        Store the message link for the current pending shuffle.

        Legacy helper retained for backward compatibility; prefers set_shuffle_message_info.

        Args:
            guild_id: Guild ID
            jump_url: Discord message jump URL
            pending_match_id: Optional specific match ID
        """
        self.set_shuffle_message_info(
            guild_id, message_id=None, channel_id=None, jump_url=jump_url,
            pending_match_id=pending_match_id
        )

    def set_shuffle_message_info(
        self,
        guild_id: int | None,
        message_id: int | None,
        channel_id: int | None,
        jump_url: str | None = None,
        thread_message_id: int | None = None,
        thread_id: int | None = None,
        origin_channel_id: int | None = None,
        pending_match_id: int | None = None,
    ) -> None:
        """
        Store message metadata for the pending shuffle.

        Used for updating betting display in thread and sending reminders.

        Args:
            guild_id: Guild ID
            message_id: Discord message ID
            channel_id: Discord channel ID
            jump_url: Discord message jump URL
            thread_message_id: Thread message ID for updates
            thread_id: Thread ID
            origin_channel_id: Original channel for betting reminders
            pending_match_id: Optional specific match ID
        """
        with self._shuffle_state_lock:
            state = self.get_last_shuffle(guild_id, pending_match_id)
            if not state:
                return
            if message_id is not None:
                state["shuffle_message_id"] = message_id
            if channel_id is not None:
                state["shuffle_channel_id"] = channel_id
            if jump_url is not None:
                state["shuffle_message_jump_url"] = jump_url
            if thread_message_id is not None:
                state["thread_shuffle_message_id"] = thread_message_id
            if thread_id is not None:
                state["thread_shuffle_thread_id"] = thread_id
            if origin_channel_id is not None:
                state["origin_channel_id"] = origin_channel_id
            self.persist_state(guild_id, state)

    def get_shuffle_message_info(self, guild_id: int | None, pending_match_id: int | None = None) -> dict[str, int | None]:
        """
        Return message metadata for the pending shuffle.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID

        Returns:
            Dict with message_id, channel_id, jump_url, thread_message_id, thread_id, origin_channel_id
        """
        with self._shuffle_state_lock:
            state = self.get_last_shuffle(guild_id, pending_match_id) or {}
            return {
                "message_id": state.get("shuffle_message_id"),
                "channel_id": state.get("shuffle_channel_id"),
                "jump_url": state.get("shuffle_message_jump_url"),
                "thread_message_id": state.get("thread_shuffle_message_id"),
                "thread_id": state.get("thread_shuffle_thread_id"),
                "origin_channel_id": state.get("origin_channel_id"),
                "pending_match_id": state.get("pending_match_id"),
            }

    def clear_last_shuffle(self, guild_id: int | None, pending_match_id: int | None = None) -> None:
        """
        Clear the pending shuffle state for a guild.

        Removes from both in-memory cache and database.

        Args:
            guild_id: Guild ID
            pending_match_id: If provided, clear only this specific match.
                             If None, clear ALL matches for the guild.
        """
        with self._shuffle_state_lock:
            normalized = self._normalize_guild_id(guild_id)

            if pending_match_id is not None:
                # Clear specific match
                if normalized in self._last_shuffle_by_guild:
                    self._last_shuffle_by_guild[normalized].pop(pending_match_id, None)
                    if not self._last_shuffle_by_guild[normalized]:
                        del self._last_shuffle_by_guild[normalized]
                self.match_repo.clear_pending_match(guild_id, pending_match_id)
            else:
                # Clear all matches for guild
                self._last_shuffle_by_guild.pop(normalized, None)
                self.match_repo.clear_pending_match(guild_id)

    def ensure_pending_state(self, guild_id: int | None, pending_match_id: int | None = None) -> dict:
        """
        Get the pending state, raising an error if none exists.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID

        Returns:
            The pending match state dict

        Raises:
            ValueError: If no recent shuffle found
        """
        state = self.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            raise ValueError("No recent shuffle found.")
        return state

    def ensure_record_submissions(self, state: dict) -> dict[int, dict[str, Any]]:
        """
        Ensure record_submissions dict exists in state.

        Args:
            state: The pending match state dict

        Returns:
            The record_submissions dict
        """
        if "record_submissions" not in state:
            state["record_submissions"] = {}
        return state["record_submissions"]

    def build_pending_match_payload(self, state: dict) -> dict:
        """
        Build a clean payload for database persistence from state.

        Args:
            state: The full in-memory state dict

        Returns:
            A dict with only the fields needed for persistence
        """
        return {
            "radiant_team_ids": state["radiant_team_ids"],
            "dire_team_ids": state["dire_team_ids"],
            "radiant_roles": state["radiant_roles"],
            "dire_roles": state["dire_roles"],
            "radiant_value": state["radiant_value"],
            "dire_value": state["dire_value"],
            "value_diff": state["value_diff"],
            "first_pick_team": state["first_pick_team"],
            "excluded_player_ids": state.get("excluded_player_ids", []),
            "record_submissions": state.get("record_submissions", {}),
            "shuffle_timestamp": state.get("shuffle_timestamp"),
            "bet_lock_until": state.get("bet_lock_until"),
            "shuffle_message_jump_url": state.get("shuffle_message_jump_url"),
            "shuffle_message_id": state.get("shuffle_message_id"),
            "shuffle_channel_id": state.get("shuffle_channel_id"),
            "betting_mode": state.get("betting_mode", "pool"),
            "is_draft": state.get("is_draft", False),
            "effective_avoid_ids": state.get("effective_avoid_ids", []),
            "is_bomb_pot": state.get("is_bomb_pot", False),
            "pending_match_id": state.get("pending_match_id"),
        }

    def persist_state(self, guild_id: int | None, state: dict) -> int:
        """
        Persist the pending match state to database.

        Also updates the in-memory cache to keep it in sync.

        Args:
            guild_id: Guild ID
            state: The state dict to persist

        Returns:
            pending_match_id: The ID of the persisted match
        """
        payload = self.build_pending_match_payload(state)
        pending_match_id = state.get("pending_match_id")

        if pending_match_id is not None:
            # Update existing match
            self.match_repo.update_pending_match(pending_match_id, payload)
        else:
            # Create new match
            pending_match_id = self.match_repo.save_pending_match(guild_id, payload)
            state["pending_match_id"] = pending_match_id
            payload["pending_match_id"] = pending_match_id

        # Update in-memory cache to keep it in sync
        self.set_last_shuffle(guild_id, state)
        return pending_match_id

    def has_pending_match(self, guild_id: int | None = None) -> bool:
        """Check if there's any pending match for the guild."""
        with self._shuffle_state_lock:
            normalized = self._normalize_guild_id(guild_id)

            # Check in-memory first
            if self._last_shuffle_by_guild.get(normalized):
                return True

            # Check database
            pending = self.match_repo.get_pending_matches(guild_id)
            return len(pending) > 0

    def get_pending_match_count(self, guild_id: int | None = None) -> int:
        """Get the number of pending matches for a guild."""
        with self._shuffle_state_lock:
            pending = self.match_repo.get_pending_matches(guild_id)
            return len(pending)
