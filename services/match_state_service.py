"""
Match state management service.

Handles pending match state: shuffle results, message metadata, and persistence.
Supports multiple concurrent pending matches per guild.

The in-memory and on-the-wire form is :class:`PendingMatchState` (a typed
dataclass). Persistence still goes through the repo as JSON; this service
wraps the conversion so the rest of the codebase only sees typed objects.

Thread Safety:
    All public methods that read or modify state are protected by _shuffle_state_lock.
    For operations requiring atomic read-modify-write cycles (like voting), use the
    state_lock() context manager to hold the lock for the entire operation.
"""

import threading
from collections.abc import Generator
from contextlib import contextmanager
from typing import Any

from domain.models.pending_match_state import PendingMatchState
from repositories.interfaces import IMatchRepository
from utils.guild import normalize_guild_id


def _row_to_state(row: dict[str, Any] | None) -> PendingMatchState | None:
    """Convert a repo row (JSON-loaded payload + injected pending_match_id) to a typed state."""
    if not row:
        return None
    state = PendingMatchState.from_dict(row)
    # Repo injects pending_match_id from the row PK after json.loads — preserve it.
    pmid = row.get("pending_match_id")
    if pmid is not None:
        state.pending_match_id = pmid
    return state


class MatchStateService:
    """
    Manages pending match state for shuffle results and voting.

    This service handles:
    - In-memory cache of pending match state per guild (supports multiple concurrent matches)
    - Persistence of state to database
    - Message metadata storage for Discord UI updates

    Structure: dict[guild_id, dict[pending_match_id, PendingMatchState]]
    """

    def __init__(self, match_repo: IMatchRepository):
        """
        Initialize MatchStateService.

        Args:
            match_repo: Repository for match data persistence
        """
        self.match_repo = match_repo
        # Nested dict: guild_id -> pending_match_id -> PendingMatchState
        self._last_shuffle_by_guild: dict[int, dict[int, PendingMatchState]] = {}
        self._shuffle_state_lock = threading.RLock()

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

    def get_last_shuffle(self, guild_id: int | None = None, pending_match_id: int | None = None) -> PendingMatchState | None:
        """
        Get the pending shuffle state for a guild.

        First checks in-memory cache, then falls back to database.

        Args:
            guild_id: Guild ID to look up
            pending_match_id: If provided, get specific match. If None, returns
                             the single match if only one exists.

        Returns:
            Pending match state or None if no pending shuffle
        """
        with self._shuffle_state_lock:
            normalized = normalize_guild_id(guild_id)
            guild_states = self._last_shuffle_by_guild.get(normalized, {})

            if pending_match_id is not None:
                state = guild_states.get(pending_match_id)
                if state:
                    return state
                # Try to load from DB
                row = self.match_repo.get_pending_match_by_id(pending_match_id)
                state = _row_to_state(row)
                if state:
                    if normalized not in self._last_shuffle_by_guild:
                        self._last_shuffle_by_guild[normalized] = {}
                    self._last_shuffle_by_guild[normalized][pending_match_id] = state
                return state

            # Always check database for authoritative count (fixes stale cache issue)
            # get_pending_match returns single match only if exactly one exists in DB
            row = self.match_repo.get_pending_match(guild_id)
            state = _row_to_state(row)
            if state and state.pending_match_id is not None:
                if normalized not in self._last_shuffle_by_guild:
                    self._last_shuffle_by_guild[normalized] = {}
                self._last_shuffle_by_guild[normalized][state.pending_match_id] = state
            return state

    def get_all_pending_matches(self, guild_id: int | None = None) -> list[PendingMatchState]:
        """
        Get all pending match states for a guild.

        Args:
            guild_id: Guild ID to look up

        Returns:
            List of pending match states
        """
        with self._shuffle_state_lock:
            normalized = normalize_guild_id(guild_id)

            # Load from database
            rows = self.match_repo.get_pending_matches(guild_id)

            # Update in-memory cache
            if normalized not in self._last_shuffle_by_guild:
                self._last_shuffle_by_guild[normalized] = {}

            results: list[PendingMatchState] = []
            for row in rows:
                state = _row_to_state(row)
                if state and state.pending_match_id is not None:
                    self._last_shuffle_by_guild[normalized][state.pending_match_id] = state
                if state:
                    results.append(state)
            return results

    def get_pending_match_for_player(self, guild_id: int | None, discord_id: int) -> PendingMatchState | None:
        """
        Find the pending match that contains a specific player.

        Args:
            guild_id: Guild ID
            discord_id: Player's Discord ID

        Returns:
            Pending match state if player is a participant, None otherwise
        """
        with self._shuffle_state_lock:
            row = self.match_repo.get_pending_match_for_player(guild_id, discord_id)
            return _row_to_state(row)

    def get_all_pending_player_ids(self, guild_id: int | None = None) -> set[int]:
        """
        Get all player IDs currently in any pending match for a guild.

        Returns:
            Set of Discord IDs of all players in pending matches
        """
        with self._shuffle_state_lock:
            return self.match_repo.get_all_pending_match_player_ids(guild_id)

    def set_last_shuffle(self, guild_id: int | None, state: PendingMatchState) -> None:
        """
        Set the pending shuffle state for a guild.

        Updates in-memory cache only. Use persist_state() to save to database.

        Args:
            guild_id: Guild ID
            state: The pending match state (must have pending_match_id)
        """
        with self._shuffle_state_lock:
            pending_match_id = state.pending_match_id
            if pending_match_id is None:
                # Legacy single-match mode - use 0 as placeholder
                pending_match_id = 0

            normalized = normalize_guild_id(guild_id)
            if normalized not in self._last_shuffle_by_guild:
                self._last_shuffle_by_guild[normalized] = {}
            self._last_shuffle_by_guild[normalized][pending_match_id] = state

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
        cmd_message_id: int | None = None,
        cmd_channel_id: int | None = None,
    ) -> None:
        """
        Store message metadata for the pending shuffle.

        Used for updating betting display in thread and sending reminders.

        Args:
            guild_id: Guild ID
            message_id: Discord message ID (lobby channel)
            channel_id: Discord channel ID (lobby channel)
            jump_url: Discord message jump URL
            thread_message_id: Thread message ID for updates
            thread_id: Thread ID
            origin_channel_id: Original channel for betting reminders
            pending_match_id: Optional specific match ID
            cmd_message_id: Command channel message ID (if different from lobby)
            cmd_channel_id: Command channel ID (if different from lobby)
        """
        with self._shuffle_state_lock:
            state = self.get_last_shuffle(guild_id, pending_match_id)
            if not state:
                return
            if message_id is not None:
                state.shuffle_message_id = message_id
            if channel_id is not None:
                state.shuffle_channel_id = channel_id
            if jump_url is not None:
                state.shuffle_message_jump_url = jump_url
            if thread_message_id is not None:
                state.thread_shuffle_message_id = thread_message_id
            if thread_id is not None:
                state.thread_shuffle_thread_id = thread_id
            if origin_channel_id is not None:
                state.origin_channel_id = origin_channel_id
            if cmd_message_id is not None:
                state.cmd_shuffle_message_id = cmd_message_id
            if cmd_channel_id is not None:
                state.cmd_shuffle_channel_id = cmd_channel_id
            self.persist_state(guild_id, state)

    def get_shuffle_message_info(self, guild_id: int | None, pending_match_id: int | None = None) -> dict[str, int | None]:
        """
        Return message metadata for the pending shuffle.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID

        Returns:
            Dict with message_id, channel_id, jump_url, thread_message_id, thread_id,
            origin_channel_id, cmd_message_id, cmd_channel_id
        """
        with self._shuffle_state_lock:
            state = self.get_last_shuffle(guild_id, pending_match_id)
            if not state:
                return {
                    "message_id": None,
                    "channel_id": None,
                    "jump_url": None,
                    "thread_message_id": None,
                    "thread_id": None,
                    "origin_channel_id": None,
                    "pending_match_id": None,
                    "cmd_message_id": None,
                    "cmd_channel_id": None,
                }
            return {
                "message_id": state.shuffle_message_id,
                "channel_id": state.shuffle_channel_id,
                "jump_url": state.shuffle_message_jump_url,
                "thread_message_id": state.thread_shuffle_message_id,
                "thread_id": state.thread_shuffle_thread_id,
                "origin_channel_id": state.origin_channel_id,
                "pending_match_id": state.pending_match_id,
                "cmd_message_id": state.cmd_shuffle_message_id,
                "cmd_channel_id": state.cmd_shuffle_channel_id,
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
            normalized = normalize_guild_id(guild_id)

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

    def ensure_pending_state(self, guild_id: int | None, pending_match_id: int | None = None) -> PendingMatchState:
        """
        Get the pending state, raising an error if none exists.

        Args:
            guild_id: Guild ID
            pending_match_id: Optional specific match ID

        Returns:
            The pending match state

        Raises:
            ValueError: If no recent shuffle found
        """
        state = self.get_last_shuffle(guild_id, pending_match_id)
        if not state:
            raise ValueError("No recent shuffle found.")
        return state

    def ensure_record_submissions(self, state: PendingMatchState) -> dict[int, dict[str, Any]]:
        """
        Ensure record_submissions dict has integer keys.

        JSON serialization converts integer keys to strings, so we need to
        normalize the keys back to integers when loading from the database.
        ``from_dict`` does this on read; this method is kept for parity with
        callers that mutate the dict in place after acquiring the state lock.

        Args:
            state: The pending match state

        Returns:
            The record_submissions dict with integer keys
        """
        normalized: dict[int, dict[str, Any]] = {}
        for key, value in state.record_submissions.items():
            int_key = int(key) if isinstance(key, str) else key
            normalized[int_key] = value
        state.record_submissions = normalized
        return state.record_submissions

    def build_pending_match_payload(self, state: PendingMatchState) -> dict:
        """
        Build a clean payload for database persistence from state.

        Strict on write: only known fields. Excludes pending_match_id (row PK).

        Args:
            state: The full in-memory state

        Returns:
            A dict with only the fields needed for persistence
        """
        return state.to_dict()

    def persist_state(self, guild_id: int | None, state: PendingMatchState) -> int:
        """
        Persist the pending match state to database.

        Also updates the in-memory cache to keep it in sync.

        Args:
            guild_id: Guild ID
            state: The state to persist

        Returns:
            pending_match_id: The ID of the persisted match
        """
        payload = self.build_pending_match_payload(state)
        pending_match_id = state.pending_match_id

        if pending_match_id is not None:
            # Update existing match
            self.match_repo.update_pending_match(pending_match_id, payload)
        else:
            # Create new match
            pending_match_id = self.match_repo.save_pending_match(guild_id, payload)
            state.pending_match_id = pending_match_id

        # Update in-memory cache to keep it in sync
        self.set_last_shuffle(guild_id, state)
        return pending_match_id

    def has_pending_match(self, guild_id: int | None = None) -> bool:
        """Check if there's any pending match for the guild."""
        with self._shuffle_state_lock:
            normalized = normalize_guild_id(guild_id)

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
