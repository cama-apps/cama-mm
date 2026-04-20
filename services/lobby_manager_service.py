"""
LobbyManagerService: manages lobby lifecycle with persistence.

Moved from domain/models/lobby.py to maintain clean architecture -
domain models should not depend on infrastructure (repositories).
"""

import asyncio
import logging
import threading
import time
from datetime import datetime

from domain.models.lobby import Lobby
from repositories.interfaces import ILobbyRepository


class LobbyManagerService:
    """
    Manages per-guild lobbies with persistence.

    Every piece of lobby state is keyed by normalized guild_id
    (``guild_id or 0``) so that two Discord guilds run independent lobbies.
    This service layer class handles:
    - Lobby lifecycle (create, join, leave, reset) per guild
    - State persistence via ILobbyRepository keyed by (lobby_id, guild_id)
    - Message metadata tracking for Discord UI updates per guild
    """

    DEFAULT_LOBBY_ID = 1
    SHUFFLE_LOCK_TIMEOUT = 60.0  # Auto-release shuffle lock after 60 seconds

    def __init__(self, lobby_repo: ILobbyRepository):
        self.lobby_repo = lobby_repo
        # Per-guild Discord message metadata
        self.lobby_message_ids: dict[int, int] = {}
        self.lobby_channel_ids: dict[int, int] = {}
        self.lobby_thread_ids: dict[int, int] = {}
        self.lobby_embed_message_ids: dict[int, int] = {}
        self.origin_channel_ids: dict[int, int] = {}
        # Per-guild lobbies
        self.lobbies: dict[int, Lobby] = {}
        # Per-guild creation locks — protects the full lobby creation flow so
        # concurrent /lobby calls in the *same* guild serialize, while distinct
        # guilds create their lobbies in parallel.
        self._creation_locks: dict[int, asyncio.Lock] = {}
        self._state_lock = threading.RLock()  # Protects all in-memory state mutations
        # Readycheck state (in-memory only, cleared on lobby reset) — per guild
        self.readycheck_message_ids: dict[int, int] = {}
        self.readycheck_channel_ids: dict[int, int] = {}
        self.readycheck_lobby_ids: dict[int, set[int]] = {}
        self.readycheck_reacted: dict[int, dict[int, str]] = {}
        self.readycheck_player_data: dict[int, dict[int, dict]] = {}
        # Per-guild shuffle/draft locks to prevent race conditions
        self._shuffle_locks: dict[int, asyncio.Lock] = {}
        self._shuffle_lock_times: dict[int, float] = {}  # Track when lock acquired
        self._load_state()

    @staticmethod
    def _normalize_guild_id(guild_id: int | None) -> int:
        """Normalize None to 0 (mirrors BaseRepository.normalize_guild_id)."""
        return guild_id if guild_id is not None else 0

    def get_creation_lock(self, guild_id: int | None = None) -> asyncio.Lock:
        """Get or create the per-guild lobby-creation lock.

        Mirrors ``get_shuffle_lock``: lazy-creates one ``asyncio.Lock`` per
        normalized guild_id so concurrent /lobby calls in the same guild
        serialize while distinct guilds proceed independently.
        """
        normalized = self._normalize_guild_id(guild_id)
        if normalized not in self._creation_locks:
            self._creation_locks[normalized] = asyncio.Lock()
        return self._creation_locks[normalized]

    def get_shuffle_lock(self, guild_id: int | None) -> asyncio.Lock:
        """Get or create the shuffle lock for a guild."""
        normalized = self._normalize_guild_id(guild_id)
        if normalized not in self._shuffle_locks:
            self._shuffle_locks[normalized] = asyncio.Lock()
        return self._shuffle_locks[normalized]

    def _check_stale_lock(self, guild_id: int | None) -> bool:
        """Check if lock is stale and reset if so. Returns True if was stale.

        Cannot call lock.release() from a different context than the one that
        acquired it (raises RuntimeError). Instead, replace with a fresh lock.
        """
        normalized = self._normalize_guild_id(guild_id)
        lock = self._shuffle_locks.get(normalized)
        if lock and lock.locked():
            acquired_at = self._shuffle_lock_times.get(normalized, 0)
            if time.time() - acquired_at > self.SHUFFLE_LOCK_TIMEOUT:
                self._shuffle_locks[normalized] = asyncio.Lock()
                self._shuffle_lock_times.pop(normalized, None)
                return True
        return False

    def record_lock_acquired(self, guild_id: int | None) -> None:
        """Record when lock was acquired for timeout tracking."""
        normalized = self._normalize_guild_id(guild_id)
        self._shuffle_lock_times[normalized] = time.time()

    def clear_lock_time(self, guild_id: int | None) -> None:
        """Clear lock acquisition time on release."""
        normalized = self._normalize_guild_id(guild_id)
        self._shuffle_lock_times.pop(normalized, None)

    def get_or_create_lobby(
        self, creator_id: int | None = None, guild_id: int | None = None
    ) -> Lobby:
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            current = self.lobbies.get(normalized)
            if current is None or current.status != "open":
                new_lobby = Lobby(
                    lobby_id=self.DEFAULT_LOBBY_ID,
                    guild_id=normalized,
                    created_by=creator_id or 0,
                    created_at=datetime.now(),
                )
                self.lobbies[normalized] = new_lobby
                self._persist_lobby(normalized)
                return new_lobby
            return current

    def get_lobby(self, guild_id: int | None = None) -> Lobby | None:
        normalized = self._normalize_guild_id(guild_id)
        lobby = self.lobbies.get(normalized)
        return lobby if lobby and lobby.status == "open" else None

    def join_lobby(
        self, discord_id: int, max_players: int = 12, guild_id: int | None = None
    ) -> bool:
        with self._state_lock:
            lobby = self.get_or_create_lobby(guild_id=guild_id)
            # Check total count (regular + conditional) against max
            if lobby.get_total_count() >= max_players:
                return False
            success = lobby.add_player(discord_id)
            if success:
                self._persist_lobby(lobby.guild_id)
            return success

    def join_lobby_conditional(
        self, discord_id: int, max_players: int = 12, guild_id: int | None = None
    ) -> bool:
        """Add player to conditional queue (frogling)."""
        with self._state_lock:
            lobby = self.get_or_create_lobby(guild_id=guild_id)
            # Check total count (regular + conditional) against max
            if lobby.get_total_count() >= max_players:
                return False
            success = lobby.add_conditional_player(discord_id)
            if success:
                self._persist_lobby(lobby.guild_id)
            return success

    def leave_lobby(self, discord_id: int, guild_id: int | None = None) -> bool:
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            lobby = self.lobbies.get(normalized)
            if not lobby:
                return False
            success = lobby.remove_player(discord_id)
            if success:
                self._persist_lobby(normalized)
            return success

    def leave_lobby_conditional(
        self, discord_id: int, guild_id: int | None = None
    ) -> bool:
        """Remove player from conditional queue."""
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            lobby = self.lobbies.get(normalized)
            if not lobby:
                return False
            success = lobby.remove_conditional_player(discord_id)
            if success:
                self._persist_lobby(normalized)
            return success

    def set_lobby_message(
        self,
        message_id: int | None,
        channel_id: int | None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        origin_channel_id: int | None = None,
        guild_id: int | None = None,
    ) -> None:
        """Set the lobby message, channel, and thread IDs, persisting to database."""
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            if message_id is None:
                self.lobby_message_ids.pop(normalized, None)
            else:
                self.lobby_message_ids[normalized] = message_id
            if channel_id is None:
                self.lobby_channel_ids.pop(normalized, None)
            else:
                self.lobby_channel_ids[normalized] = channel_id
            if thread_id is not None:
                self.lobby_thread_ids[normalized] = thread_id
            if embed_message_id is not None:
                self.lobby_embed_message_ids[normalized] = embed_message_id
            if origin_channel_id is not None:
                self.origin_channel_ids[normalized] = origin_channel_id
            if self.lobbies.get(normalized):
                self._persist_lobby(normalized)

    def get_lobby_message_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_message_ids.get(self._normalize_guild_id(guild_id))

    def get_lobby_channel_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_channel_ids.get(self._normalize_guild_id(guild_id))

    def get_lobby_thread_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_thread_ids.get(self._normalize_guild_id(guild_id))

    def get_lobby_embed_message_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_embed_message_ids.get(self._normalize_guild_id(guild_id))

    def get_origin_channel_id(self, guild_id: int | None = None) -> int | None:
        return self.origin_channel_ids.get(self._normalize_guild_id(guild_id))

    def reset_lobby(self, guild_id: int | None = None) -> None:
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            logger = logging.getLogger("cama_bot.services.lobby_manager")
            lobby = self.lobbies.get(normalized)
            logger.info(
                f"reset_lobby called for guild_id={normalized}. Current lobby: {lobby}"
            )
            if lobby:
                lobby.status = "closed"
            self.lobbies.pop(normalized, None)
            self.lobby_message_ids.pop(normalized, None)
            self.lobby_channel_ids.pop(normalized, None)
            self.lobby_thread_ids.pop(normalized, None)
            self.lobby_embed_message_ids.pop(normalized, None)
            self.origin_channel_ids.pop(normalized, None)
            self.readycheck_message_ids.pop(normalized, None)
            self.readycheck_channel_ids.pop(normalized, None)
            self.readycheck_lobby_ids.pop(normalized, None)
            self.readycheck_reacted.pop(normalized, None)
            self.readycheck_player_data.pop(normalized, None)
            self._clear_persistent_lobby(normalized)
            logger.info(
                f"reset_lobby completed for guild_id={normalized} - cleared persistent lobby"
            )

    # --- Readycheck state accessors/mutators (per guild) ---

    def get_readycheck_message_id(self, guild_id: int | None = None) -> int | None:
        return self.readycheck_message_ids.get(self._normalize_guild_id(guild_id))

    def get_readycheck_channel_id(self, guild_id: int | None = None) -> int | None:
        return self.readycheck_channel_ids.get(self._normalize_guild_id(guild_id))

    def get_readycheck_lobby_ids(self, guild_id: int | None = None) -> set[int]:
        return self.readycheck_lobby_ids.get(self._normalize_guild_id(guild_id), set())

    def get_readycheck_reacted(self, guild_id: int | None = None) -> dict[int, str]:
        return self.readycheck_reacted.get(self._normalize_guild_id(guild_id), {})

    def get_readycheck_player_data(self, guild_id: int | None = None) -> dict[int, dict]:
        return self.readycheck_player_data.get(self._normalize_guild_id(guild_id), {})

    def set_readycheck_state(
        self,
        message_id: int,
        channel_id: int,
        lobby_ids: set[int],
        player_data: dict[int, dict],
        guild_id: int | None = None,
    ) -> None:
        normalized = self._normalize_guild_id(guild_id)
        self.readycheck_message_ids[normalized] = message_id
        self.readycheck_channel_ids[normalized] = channel_id
        self.readycheck_lobby_ids[normalized] = lobby_ids
        self.readycheck_player_data[normalized] = player_data
        self.readycheck_reacted[normalized] = {}

    def update_readycheck_data(
        self,
        lobby_ids: set[int],
        player_data: dict[int, dict],
        guild_id: int | None = None,
    ) -> None:
        normalized = self._normalize_guild_id(guild_id)
        self.readycheck_lobby_ids[normalized] = lobby_ids
        self.readycheck_player_data[normalized] = player_data
        existing = self.readycheck_reacted.get(normalized, {})
        self.readycheck_reacted[normalized] = {
            k: v for k, v in existing.items() if k in lobby_ids
        }

    def add_readycheck_reaction(
        self, discord_id: int, tag: str, guild_id: int | None = None
    ) -> bool:
        """Record a reaction. Returns True if newly added."""
        normalized = self._normalize_guild_id(guild_id)
        reacted = self.readycheck_reacted.setdefault(normalized, {})
        lobby_ids = self.readycheck_lobby_ids.get(normalized, set())
        if discord_id in reacted:
            return False
        if discord_id not in lobby_ids:
            return False
        reacted[discord_id] = tag
        return True

    def remove_readycheck_reaction(
        self, discord_id: int, guild_id: int | None = None
    ) -> bool:
        """Remove a reaction. Returns True if was present."""
        normalized = self._normalize_guild_id(guild_id)
        reacted = self.readycheck_reacted.get(normalized)
        if not reacted or discord_id not in reacted:
            return False
        del reacted[discord_id]
        return True

    # --- Persistence helpers ---

    def _persist_lobby(self, guild_id: int | None = None) -> None:
        """Persist the lobby for a guild (default 0 for backward compatibility)."""
        normalized = self._normalize_guild_id(guild_id)
        lobby = self.lobbies.get(normalized)
        if not lobby:
            return
        self.lobby_repo.save_lobby_state(
            lobby_id=self.DEFAULT_LOBBY_ID,
            guild_id=normalized,
            players=list(lobby.players),
            conditional_players=list(lobby.conditional_players),
            status=lobby.status,
            created_by=lobby.created_by,
            created_at=lobby.created_at.isoformat(),
            message_id=self.lobby_message_ids.get(normalized),
            channel_id=self.lobby_channel_ids.get(normalized),
            thread_id=self.lobby_thread_ids.get(normalized),
            embed_message_id=self.lobby_embed_message_ids.get(normalized),
            origin_channel_id=self.origin_channel_ids.get(normalized),
            player_join_times=lobby.player_join_times,
        )

    def _clear_persistent_lobby(self, guild_id: int | None = None) -> None:
        normalized = self._normalize_guild_id(guild_id)
        self.lobby_repo.clear_lobby_state(self.DEFAULT_LOBBY_ID, guild_id=normalized)

    def _load_state(self) -> None:
        """Rehydrate per-guild lobby state from the repository on startup.

        Reads every persisted ``(lobby_id, guild_id)`` row via
        :meth:`ILobbyRepository.load_all_lobby_states` and populates the
        in-memory maps. A missing ``lobby_state`` table on a fresh install is
        tolerated (logged and treated as "no state"); any other failure
        propagates so we don't silently drop data.
        """
        import sqlite3

        logger = logging.getLogger("cama_bot.services.lobby_manager")
        try:
            rows = self.lobby_repo.load_all_lobby_states() or []
        except sqlite3.OperationalError as exc:
            # Fresh install before migrations applied: the table doesn't exist
            # yet. Safe to treat as empty state; the next persist will create
            # a row after the schema is in place.
            logger.info(
                "lobby_state table unavailable during _load_state; "
                "treating as empty state: %s",
                exc,
            )
            return

        for row in rows:
            guild_id = int(row.get("guild_id") or 0)
            data = self.lobby_repo.load_lobby_state(
                row.get("lobby_id", self.DEFAULT_LOBBY_ID),
                guild_id=guild_id,
            )
            if not data:
                continue
            data.setdefault("guild_id", guild_id)
            self.lobbies[guild_id] = Lobby.from_dict(data)
            if data.get("message_id") is not None:
                self.lobby_message_ids[guild_id] = data["message_id"]
            if data.get("channel_id") is not None:
                self.lobby_channel_ids[guild_id] = data["channel_id"]
            if data.get("thread_id") is not None:
                self.lobby_thread_ids[guild_id] = data["thread_id"]
            if data.get("embed_message_id") is not None:
                self.lobby_embed_message_ids[guild_id] = data["embed_message_id"]
            if data.get("origin_channel_id") is not None:
                self.origin_channel_ids[guild_id] = data["origin_channel_id"]


# Backward compatibility alias - allows gradual migration
LobbyManager = LobbyManagerService
