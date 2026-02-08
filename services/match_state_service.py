"""
Match state management service.

Handles pending match state: shuffle results, message metadata, and persistence.

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
    - In-memory cache of pending match state per guild
    - Persistence of state to database
    - Message metadata storage for Discord UI updates
    """

    def __init__(self, match_repo: IMatchRepository):
        """
        Initialize MatchStateService.

        Args:
            match_repo: Repository for match data persistence
        """
        self.match_repo = match_repo
        self._last_shuffle_by_guild: dict[int, dict] = {}
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

    def get_last_shuffle(self, guild_id: int | None = None) -> dict | None:
        """
        Get the pending shuffle state for a guild.

        First checks in-memory cache, then falls back to database.

        Args:
            guild_id: Guild ID to look up

        Returns:
            Pending match state dict or None if no pending shuffle
        """
        with self._shuffle_state_lock:
            normalized = self._normalize_guild_id(guild_id)
            state = self._last_shuffle_by_guild.get(normalized)
            if state:
                return state
            persisted = self.match_repo.get_pending_match(guild_id)
            if persisted:
                self._last_shuffle_by_guild[normalized] = persisted
                return persisted
            return None

    def set_last_shuffle(self, guild_id: int | None, payload: dict) -> None:
        """
        Set the pending shuffle state for a guild.

        Updates in-memory cache only. Use persist_state() to save to database.

        Args:
            guild_id: Guild ID
            payload: The pending match state dict
        """
        with self._shuffle_state_lock:
            self._last_shuffle_by_guild[self._normalize_guild_id(guild_id)] = payload

    def set_shuffle_message_url(self, guild_id: int | None, jump_url: str) -> None:
        """
        Store the message link for the current pending shuffle.

        Legacy helper retained for backward compatibility; prefers set_shuffle_message_info.

        Args:
            guild_id: Guild ID
            jump_url: Discord message jump URL
        """
        self.set_shuffle_message_info(guild_id, message_id=None, channel_id=None, jump_url=jump_url)

    def set_shuffle_message_info(
        self,
        guild_id: int | None,
        message_id: int | None,
        channel_id: int | None,
        jump_url: str | None = None,
        thread_message_id: int | None = None,
        thread_id: int | None = None,
        origin_channel_id: int | None = None,
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
        """
        with self._shuffle_state_lock:
            state = self.get_last_shuffle(guild_id)
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

    def get_shuffle_message_info(self, guild_id: int | None) -> dict[str, int | None]:
        """
        Return message metadata for the pending shuffle.

        Args:
            guild_id: Guild ID

        Returns:
            Dict with message_id, channel_id, jump_url, thread_message_id, thread_id, origin_channel_id
        """
        with self._shuffle_state_lock:
            state = self.get_last_shuffle(guild_id) or {}
            return {
                "message_id": state.get("shuffle_message_id"),
                "channel_id": state.get("shuffle_channel_id"),
                "jump_url": state.get("shuffle_message_jump_url"),
                "thread_message_id": state.get("thread_shuffle_message_id"),
                "thread_id": state.get("thread_shuffle_thread_id"),
                "origin_channel_id": state.get("origin_channel_id"),
            }

    def clear_last_shuffle(self, guild_id: int | None) -> None:
        """
        Clear the pending shuffle state for a guild.

        Removes from both in-memory cache and database.

        Args:
            guild_id: Guild ID
        """
        with self._shuffle_state_lock:
            self._last_shuffle_by_guild.pop(self._normalize_guild_id(guild_id), None)
            self.match_repo.clear_pending_match(guild_id)

    def ensure_pending_state(self, guild_id: int | None) -> dict:
        """
        Get the pending state, raising an error if none exists.

        Args:
            guild_id: Guild ID

        Returns:
            The pending match state dict

        Raises:
            ValueError: If no recent shuffle found
        """
        state = self.get_last_shuffle(guild_id)
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
        }

    def persist_state(self, guild_id: int | None, state: dict) -> None:
        """
        Persist the pending match state to database.

        Also updates the in-memory cache to keep it in sync.

        Args:
            guild_id: Guild ID
            state: The state dict to persist
        """
        payload = self.build_pending_match_payload(state)
        self.match_repo.save_pending_match(guild_id, payload)
        # Update in-memory cache to keep it in sync
        self.set_last_shuffle(guild_id, state)
