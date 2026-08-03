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

from domain.models.lobby import Lobby, LobbyKind
from repositories.interfaces import ILobbyRepository


class LobbyManagerService:
    """
    Manages per-guild lobbies with persistence.

    Every piece of lobby state is keyed by normalized guild_id and lobby kind
    so that each Discord guild can run both lobby kinds independently.
    This service layer class handles:
    - Lobby lifecycle (create, join, leave, reset) per guild
    - State persistence via ILobbyRepository keyed by (lobby_id, guild_id)
    - Message metadata tracking for Discord UI updates per guild
    """

    SHUFFLE_LOCK_TIMEOUT = 60.0  # Auto-release shuffle lock after 60 seconds

    def __init__(self, lobby_repo: ILobbyRepository):
        self.lobby_repo = lobby_repo
        # Per-guild Discord message metadata
        self.lobby_message_ids: dict[tuple[int, LobbyKind], int] = {}
        self.lobby_channel_ids: dict[tuple[int, LobbyKind], int] = {}
        self.lobby_thread_ids: dict[tuple[int, LobbyKind], int] = {}
        self.lobby_embed_message_ids: dict[tuple[int, LobbyKind], int] = {}
        self.origin_channel_ids: dict[tuple[int, LobbyKind], int] = {}
        # Per-guild lobbies
        self.lobbies: dict[tuple[int, LobbyKind], Lobby] = {}
        # Per-guild creation locks — protects the full lobby creation flow so
        # concurrent /lobby calls in the *same* guild serialize, while distinct
        # guilds create their lobbies in parallel.
        self._creation_locks: dict[tuple[int, LobbyKind], asyncio.Lock] = {}
        self._state_lock = threading.RLock()  # Protects all in-memory state mutations
        # Players selected by an in-progress shuffle or draft may not leave or
        # switch lobbies until that operation either creates its pending match
        # or is cancelled. Keyed independently from lobby membership so the
        # reservation check and membership mutation share one lock.
        self._in_flight_player_kinds: dict[
            tuple[int, int], tuple[LobbyKind, int]
        ] = {}
        # Readycheck state (in-memory only, cleared on lobby reset) — per guild
        self.readycheck_message_ids: dict[tuple[int, LobbyKind], int] = {}
        self.readycheck_channel_ids: dict[tuple[int, LobbyKind], int] = {}
        self.readycheck_lobby_ids: dict[tuple[int, LobbyKind], set[int]] = {}
        self.readycheck_reacted: dict[tuple[int, LobbyKind], dict[int, str]] = {}
        self.readycheck_player_data: dict[
            tuple[int, LobbyKind], dict[int, dict]
        ] = {}
        self.readycheck_created_ats: dict[tuple[int, LobbyKind], float] = {}
        # Per-guild shuffle/draft locks to prevent race conditions
        self._shuffle_locks: dict[tuple[int, LobbyKind], asyncio.Lock] = {}
        self._shuffle_lock_times: dict[tuple[int, LobbyKind], float] = {}
        self._load_state()

    @staticmethod
    def _normalize_guild_id(guild_id: int | None) -> int:
        """Normalize None to 0 (mirrors BaseRepository.normalize_guild_id)."""
        return guild_id if guild_id is not None else 0

    @classmethod
    def _lobby_key(
        cls,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> tuple[int, LobbyKind]:
        return cls._normalize_guild_id(guild_id), LobbyKind.normalize(lobby_kind)

    def get_creation_lock(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> asyncio.Lock:
        """Get or create the per-guild lobby-creation lock.

        Mirrors ``get_shuffle_lock``: lazy-creates one ``asyncio.Lock`` per
        normalized guild_id so concurrent /lobby calls in the same guild
        serialize while distinct guilds proceed independently.
        """
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if key not in self._creation_locks:
                self._creation_locks[key] = asyncio.Lock()
            return self._creation_locks[key]

    def get_shuffle_lock(
        self,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> asyncio.Lock:
        """Get or create the shuffle lock for one lobby."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if key not in self._shuffle_locks:
                self._shuffle_locks[key] = asyncio.Lock()
            return self._shuffle_locks[key]

    def _check_stale_lock(
        self,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Check if lock is stale and reset if so. Returns True if was stale.

        Cannot call lock.release() from a different context than the one that
        acquired it (raises RuntimeError). Instead, replace with a fresh lock.
        """
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            lock = self._shuffle_locks.get(key)
            if lock and lock.locked():
                acquired_at = self._shuffle_lock_times.get(key, 0)
                if time.time() - acquired_at > self.SHUFFLE_LOCK_TIMEOUT:
                    self._shuffle_locks[key] = asyncio.Lock()
                    self._shuffle_lock_times.pop(key, None)
                    return True
        return False

    def record_lock_acquired(
        self,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Record when lock was acquired for timeout tracking."""
        with self._state_lock:
            self._shuffle_lock_times[self._lobby_key(guild_id, lobby_kind)] = time.time()

    def clear_lock_time(
        self,
        guild_id: int | None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Clear lock acquisition time on release."""
        with self._state_lock:
            self._shuffle_lock_times.pop(self._lobby_key(guild_id, lobby_kind), None)

    def get_or_create_lobby(
        self,
        creator_id: int | None = None,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> Lobby:
        key = self._lobby_key(guild_id, lobby_kind)
        normalized, kind = key
        with self._state_lock:
            current = self.lobbies.get(key)
            if current is None or current.status != "open":
                new_lobby = Lobby(
                    lobby_id=kind.lobby_id,
                    guild_id=normalized,
                    kind=kind,
                    created_by=creator_id or 0,
                    created_at=datetime.now(),
                )
                self.lobbies[key] = new_lobby
                self._persist_lobby(normalized, kind)
                return new_lobby
            return current

    def get_lobby(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> Lobby | None:
        lobby = self.lobbies.get(self._lobby_key(guild_id, lobby_kind))
        return lobby if lobby and lobby.status == "open" else None

    def get_lobby_kind_for_player(
        self,
        discord_id: int,
        guild_id: int | None = None,
    ) -> LobbyKind | None:
        """Return the one open lobby kind containing a player."""
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            for kind in LobbyKind:
                lobby = self.lobbies.get((normalized, kind))
                if lobby and lobby.status == "open" and discord_id in lobby.players:
                    return kind
        return None

    def get_in_flight_lobby_kind_for_player(
        self,
        discord_id: int,
        guild_id: int | None = None,
    ) -> LobbyKind | None:
        """Return the lobby kind currently reserving a player."""
        key = (self._normalize_guild_id(guild_id), discord_id)
        with self._state_lock:
            reservation = self._in_flight_player_kinds.get(key)
            return reservation[0] if reservation else None

    def reserve_lobby_players(
        self,
        player_ids: list[int] | set[int],
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Atomically reserve selected players while a match is being built."""
        normalized, kind = self._lobby_key(guild_id, lobby_kind)
        selected_ids = set(player_ids)
        with self._state_lock:
            lobby = self.lobbies.get((normalized, kind))
            if (
                lobby is None
                or lobby.status != "open"
                or not selected_ids.issubset(
                    lobby.players | lobby.conditional_players
                )
            ):
                return False
            for player_id in selected_ids:
                reservation = self._in_flight_player_kinds.get((normalized, player_id))
                if reservation is not None:
                    return False
            for player_id in selected_ids:
                key = (normalized, player_id)
                self._in_flight_player_kinds[key] = (kind, 1)
            return True

    def has_in_flight_lobby_players(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Return whether a shuffle or draft currently owns this lobby."""
        normalized, kind = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            return any(
                player_key[0] == normalized and reservation[0] is kind
                for player_key, reservation in self._in_flight_player_kinds.items()
            )

    def release_lobby_players(
        self,
        player_ids: list[int] | set[int],
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Release matching player reservations without touching newer ones."""
        normalized, kind = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            for player_id in player_ids:
                key = (normalized, player_id)
                reservation = self._in_flight_player_kinds.get(key)
                if reservation and reservation[0] is kind:
                    if reservation[1] == 1:
                        self._in_flight_player_kinds.pop(key, None)
                    else:
                        self._in_flight_player_kinds[key] = (
                            kind,
                            reservation[1] - 1,
                        )

    def get_lobby_kind_for_message(
        self,
        message_id: int,
        guild_id: int | None = None,
    ) -> LobbyKind | None:
        """Return the lobby kind owning a lobby message in this guild."""
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            for kind in LobbyKind:
                if self.lobby_message_ids.get((normalized, kind)) == message_id:
                    return kind
        return None

    def get_lobby_kind_for_readycheck_message(
        self,
        message_id: int,
        guild_id: int | None = None,
    ) -> LobbyKind | None:
        """Return the lobby kind owning a ready-check message in this guild."""
        normalized = self._normalize_guild_id(guild_id)
        with self._state_lock:
            for kind in LobbyKind:
                if self.readycheck_message_ids.get((normalized, kind)) == message_id:
                    return kind
        return None

    def join_lobby(
        self,
        discord_id: int,
        max_players: int = 12,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> str:
        """Add a player to the lobby under the state lock.

        Returns one of: "ok", "full", "already_joined".
        """
        with self._state_lock:
            target_kind = LobbyKind.normalize(lobby_kind)
            reservation = self._in_flight_player_kinds.get(
                (self._normalize_guild_id(guild_id), discord_id)
            )
            reserved_kind = reservation[0] if reservation else None
            if reserved_kind is not None and reserved_kind is not target_kind:
                return "in_flight"
            current_kind = self.get_lobby_kind_for_player(discord_id, guild_id)
            if current_kind is not None and current_kind is not target_kind:
                return "in_other_lobby"
            lobby = self.get_or_create_lobby(
                guild_id=guild_id,
                lobby_kind=target_kind,
            )
            # Capacity check is the authoritative check — happens inside the lock
            # so two concurrent joins at the boundary cannot both slip through.
            if lobby.get_total_count() >= max_players:
                return "full"
            success = lobby.add_player(discord_id)
            if success:
                self._persist_lobby(lobby.guild_id, lobby.kind)
                return "ok"
            return "already_joined"

    def join_lobby_conditional(
        self, discord_id: int, max_players: int = 12, guild_id: int | None = None
    ) -> str:
        """Reject joins to the deprecated conditional queue."""
        return "deprecated"

    def leave_lobby(
        self,
        discord_id: int,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if self._in_flight_player_kinds.get((key[0], discord_id)) is not None:
                return False
            lobby = self.lobbies.get(key)
            if not lobby:
                return False
            success = lobby.remove_player(discord_id)
            if success:
                self._persist_lobby(lobby.guild_id, lobby.kind)
            return success

    def leave_lobby_conditional(
        self,
        discord_id: int,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Remove player from conditional queue."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if self._in_flight_player_kinds.get((key[0], discord_id)) is not None:
                return False
            lobby = self.lobbies.get(key)
            if not lobby:
                return False
            success = lobby.remove_conditional_player(discord_id)
            if success:
                self._persist_lobby(lobby.guild_id, lobby.kind)
            return success

    def set_lobby_message(
        self,
        message_id: int | None,
        channel_id: int | None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        origin_channel_id: int | None = None,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Set the lobby message, channel, and thread IDs, persisting to database."""
        key = self._lobby_key(guild_id, lobby_kind)
        normalized, kind = key
        with self._state_lock:
            if message_id is None:
                self.lobby_message_ids.pop(key, None)
            else:
                self.lobby_message_ids[key] = message_id
            if channel_id is None:
                self.lobby_channel_ids.pop(key, None)
            else:
                self.lobby_channel_ids[key] = channel_id
            if thread_id is not None:
                self.lobby_thread_ids[key] = thread_id
            if embed_message_id is not None:
                self.lobby_embed_message_ids[key] = embed_message_id
            if origin_channel_id is not None:
                self.origin_channel_ids[key] = origin_channel_id
            if self.lobbies.get(key):
                self._persist_lobby(normalized, kind)

    def get_lobby_message_id(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        return self.lobby_message_ids.get(self._lobby_key(guild_id, lobby_kind))

    def get_lobby_channel_id(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        return self.lobby_channel_ids.get(self._lobby_key(guild_id, lobby_kind))

    def get_lobby_thread_id(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        return self.lobby_thread_ids.get(self._lobby_key(guild_id, lobby_kind))

    def get_lobby_embed_message_id(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        return self.lobby_embed_message_ids.get(
            self._lobby_key(guild_id, lobby_kind)
        )

    def get_origin_channel_id(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        return self.origin_channel_ids.get(self._lobby_key(guild_id, lobby_kind))

    def reset_lobby(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        key = self._lobby_key(guild_id, lobby_kind)
        normalized, kind = key
        with self._state_lock:
            logger = logging.getLogger("cama_bot.services.lobby_manager")
            lobby = self.lobbies.get(key)
            logger.info(
                f"reset_lobby called for guild_id={normalized}. Current lobby: {lobby}"
            )
            if lobby:
                lobby.status = "closed"
            self.lobbies.pop(key, None)
            self.lobby_message_ids.pop(key, None)
            self.lobby_channel_ids.pop(key, None)
            self.lobby_thread_ids.pop(key, None)
            self.lobby_embed_message_ids.pop(key, None)
            self.origin_channel_ids.pop(key, None)
            self.readycheck_message_ids.pop(key, None)
            self.readycheck_channel_ids.pop(key, None)
            self.readycheck_lobby_ids.pop(key, None)
            self.readycheck_reacted.pop(key, None)
            self.readycheck_player_data.pop(key, None)
            self.readycheck_created_ats.pop(key, None)
            for player_key, reservation in list(
                self._in_flight_player_kinds.items()
            ):
                if player_key[0] == normalized and reservation[0] is kind:
                    self._in_flight_player_kinds.pop(player_key, None)
            self._clear_persistent_lobby(normalized, kind)
            logger.info(
                f"reset_lobby completed for guild_id={normalized} - cleared persistent lobby"
            )

    # --- Readycheck state accessors/mutators (per guild) ---

    def get_readycheck_message_id(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        return self.readycheck_message_ids.get(
            self._lobby_key(guild_id, lobby_kind)
        )

    def get_readycheck_channel_id(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        return self.readycheck_channel_ids.get(
            self._lobby_key(guild_id, lobby_kind)
        )

    def get_readycheck_lobby_ids(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> set[int]:
        return self.readycheck_lobby_ids.get(
            self._lobby_key(guild_id, lobby_kind), set()
        )

    def get_readycheck_reacted(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> dict[int, str]:
        return self.readycheck_reacted.get(
            self._lobby_key(guild_id, lobby_kind), {}
        )

    def get_readycheck_confirmation_snapshot(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> tuple[int, set[int]] | None:
        """Return the current readycheck generation and a copy of its confirmations."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            message_id = self.readycheck_message_ids.get(key)
            if message_id is None:
                return None
            return message_id, set(self.readycheck_reacted.get(key, {}))

    def get_lobby_readycheck_snapshot(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> tuple[list[int], dict[int, float], tuple[int, set[int]] | None] | None:
        """Copy the open lobby roster, join times, and confirmations atomically."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            lobby = self.lobbies.get(key)
            if lobby is None or lobby.status != "open":
                return None

            player_ids = list(lobby.players)
            player_join_times = {
                player_id: lobby.player_join_times[player_id]
                for player_id in player_ids
                if player_id in lobby.player_join_times
            }
            message_id = self.readycheck_message_ids.get(key)
            readycheck = (
                None
                if message_id is None
                else (
                    message_id,
                    set(self.readycheck_reacted.get(key, {})),
                )
            )
            return player_ids, player_join_times, readycheck

    def get_readycheck_player_data(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> dict[int, dict]:
        return self.readycheck_player_data.get(
            self._lobby_key(guild_id, lobby_kind), {}
        )

    def get_readycheck_display_snapshot(
        self,
        message_id: int,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> tuple[dict[int, dict], dict[int, str]] | None:
        """Copy display state only while ``message_id`` is the current generation."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if self.readycheck_message_ids.get(key) != message_id:
                return None
            player_data = {
                discord_id: dict(data)
                for discord_id, data in self.readycheck_player_data.get(
                    key, {}
                ).items()
            }
            reacted = dict(self.readycheck_reacted.get(key, {}))
            return player_data, reacted

    def get_readycheck_created_at(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> float | None:
        return self.readycheck_created_ats.get(
            self._lobby_key(guild_id, lobby_kind)
        )

    def set_readycheck_state(
        self,
        message_id: int,
        channel_id: int,
        lobby_ids: set[int],
        player_data: dict[int, dict],
        guild_id: int | None = None,
        created_at: float | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            self.readycheck_message_ids[key] = message_id
            self.readycheck_channel_ids[key] = channel_id
            self.readycheck_lobby_ids[key] = lobby_ids
            self.readycheck_player_data[key] = player_data
            self.readycheck_reacted[key] = {}
            self.readycheck_created_ats[key] = (
                created_at if created_at is not None else time.time()
            )

    def update_readycheck_data(
        self,
        lobby_ids: set[int],
        player_data: dict[int, dict],
        guild_id: int | None = None,
        expected_message_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> set[int] | None:
        """Update a readycheck roster and return confirmations that were removed.

        Returns ``None`` when ``expected_message_id`` is no longer the current
        readycheck generation.
        """
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if (
                expected_message_id is not None
                and self.readycheck_message_ids.get(key) != expected_message_id
            ):
                return None
            existing = self.readycheck_reacted.get(key, {})
            removed_confirmations = set(existing) - lobby_ids
            self.readycheck_lobby_ids[key] = lobby_ids
            self.readycheck_player_data[key] = player_data
            self.readycheck_reacted[key] = {
                k: v for k, v in existing.items() if k in lobby_ids
            }
            return removed_confirmations

    def add_readycheck_reaction(
        self,
        discord_id: int,
        tag: str,
        guild_id: int | None = None,
        expected_message_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Record a reaction. Returns True if newly added."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if (
                expected_message_id is not None
                and self.readycheck_message_ids.get(key) != expected_message_id
            ):
                return False
            reacted = self.readycheck_reacted.setdefault(key, {})
            lobby = self.lobbies.get(key)
            lobby_ids = (
                lobby.players
                if lobby is not None and lobby.status == "open"
                else self.readycheck_lobby_ids.get(key, set())
            )
            if discord_id in reacted:
                return False
            if discord_id not in lobby_ids:
                return False
            reacted[discord_id] = tag
            return True

    def get_readycheck_reaction_count_for_message(
        self,
        message_id: int,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> int | None:
        """Return the confirmation count only while ``message_id`` is current."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if self.readycheck_message_ids.get(key) != message_id:
                return None
            return len(self.readycheck_reacted.get(key, {}))

    def remove_readycheck_reaction(
        self,
        discord_id: int,
        guild_id: int | None = None,
        expected_message_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> bool:
        """Remove a reaction. Returns True if was present."""
        key = self._lobby_key(guild_id, lobby_kind)
        with self._state_lock:
            if (
                expected_message_id is not None
                and self.readycheck_message_ids.get(key) != expected_message_id
            ):
                return False
            reacted = self.readycheck_reacted.get(key)
            if not reacted or discord_id not in reacted:
                return False
            del reacted[discord_id]
            return True

    # --- Persistence helpers ---

    def _persist_lobby(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        """Persist one lobby kind for a guild."""
        key = self._lobby_key(guild_id, lobby_kind)
        normalized, _ = key
        lobby = self.lobbies.get(key)
        if not lobby:
            return
        self.lobby_repo.save_lobby_state(
            lobby_id=lobby.lobby_id,
            guild_id=normalized,
            players=list(lobby.players),
            conditional_players=[],
            status=lobby.status,
            created_by=lobby.created_by,
            created_at=lobby.created_at.isoformat(),
            message_id=self.lobby_message_ids.get(key),
            channel_id=self.lobby_channel_ids.get(key),
            thread_id=self.lobby_thread_ids.get(key),
            embed_message_id=self.lobby_embed_message_ids.get(key),
            origin_channel_id=self.origin_channel_ids.get(key),
            player_join_times=lobby.player_join_times,
        )

    def _clear_persistent_lobby(
        self,
        guild_id: int | None = None,
        lobby_kind: LobbyKind | str | None = None,
    ) -> None:
        normalized, kind = self._lobby_key(guild_id, lobby_kind)
        self.lobby_repo.clear_lobby_state(kind.lobby_id, guild_id=normalized)

    def _load_state(self) -> None:
        """Rehydrate per-guild lobby state from the repository on startup.

        Reads every persisted decoded lobby row via
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

        for data in rows:
            guild_id = int(data.get("guild_id") or 0)
            data.setdefault("guild_id", guild_id)
            lobby = Lobby.from_dict(data)
            key = self._lobby_key(guild_id, lobby.kind)
            self.lobbies[key] = lobby
            if data.get("message_id") is not None:
                self.lobby_message_ids[key] = data["message_id"]
            if data.get("channel_id") is not None:
                self.lobby_channel_ids[key] = data["channel_id"]
            if data.get("thread_id") is not None:
                self.lobby_thread_ids[key] = data["thread_id"]
            if data.get("embed_message_id") is not None:
                self.lobby_embed_message_ids[key] = data["embed_message_id"]
            if data.get("origin_channel_id") is not None:
                self.origin_channel_ids[key] = data["origin_channel_id"]

            persisted_join_times = data.get("player_join_times", {})
            if data.get("conditional_players") or len(persisted_join_times) != len(
                lobby.player_join_times
            ):
                self._persist_lobby(guild_id, lobby.kind)
