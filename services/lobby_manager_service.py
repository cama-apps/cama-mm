"""
LobbyManagerService: manages lobby lifecycle with persistence.

Moved from domain/models/lobby.py to maintain clean architecture -
domain models should not depend on infrastructure (repositories).
"""

import asyncio
import logging
from datetime import datetime

from domain.models.lobby import Lobby
from repositories.interfaces import ILobbyRepository


class LobbyManagerService:
    """
    Manages a single global lobby with persistence.

    This service layer class handles:
    - Lobby lifecycle (create, join, leave, reset)
    - State persistence via ILobbyRepository
    - Message metadata tracking for Discord UI updates
    """

    DEFAULT_LOBBY_ID = 1

    def __init__(self, lobby_repo: ILobbyRepository):
        self.lobby_repo = lobby_repo
        self.lobby_message_id: int | None = None
        self.lobby_channel_id: int | None = None
        self.lobby_thread_id: int | None = None
        self.lobby_embed_message_id: int | None = None
        self.origin_channel_id: int | None = None  # Channel where /lobby was originally run
        self.lobby: Lobby | None = None
        self._creation_lock = asyncio.Lock()
        # Readycheck state (in-memory only, cleared on lobby reset)
        self.readycheck_message_id: int | None = None
        self.readycheck_channel_id: int | None = None
        self.readycheck_lobby_ids: set[int] = set()
        self.readycheck_reacted: dict[int, str] = {}  # {discord_id: "<@discord_id>"}
        self.readycheck_player_data: dict[int, dict] = {}  # {discord_id: {group, signals, name, ...}}
        self._load_state()

    @property
    def creation_lock(self) -> asyncio.Lock:
        """Lock for protecting the full lobby creation flow."""
        return self._creation_lock

    def get_or_create_lobby(self, creator_id: int | None = None) -> Lobby:
        if self.lobby is None or self.lobby.status != "open":
            self.lobby = Lobby(
                lobby_id=self.DEFAULT_LOBBY_ID,
                created_by=creator_id or 0,
                created_at=datetime.now(),
            )
            self._persist_lobby()
        return self.lobby

    def get_lobby(self) -> Lobby | None:
        return self.lobby if self.lobby and self.lobby.status == "open" else None

    def join_lobby(self, discord_id: int, max_players: int = 12) -> bool:
        lobby = self.get_or_create_lobby()
        # Check total count (regular + conditional) against max
        if lobby.get_total_count() >= max_players:
            return False
        success = lobby.add_player(discord_id)
        if success:
            self._persist_lobby()
        return success

    def join_lobby_conditional(self, discord_id: int, max_players: int = 12) -> bool:
        """Add player to conditional queue (frogling)."""
        lobby = self.get_or_create_lobby()
        # Check total count (regular + conditional) against max
        if lobby.get_total_count() >= max_players:
            return False
        success = lobby.add_conditional_player(discord_id)
        if success:
            self._persist_lobby()
        return success

    def leave_lobby(self, discord_id: int) -> bool:
        if not self.lobby:
            return False
        success = self.lobby.remove_player(discord_id)
        if success:
            self._persist_lobby()
        return success

    def leave_lobby_conditional(self, discord_id: int) -> bool:
        """Remove player from conditional queue."""
        if not self.lobby:
            return False
        success = self.lobby.remove_conditional_player(discord_id)
        if success:
            self._persist_lobby()
        return success

    def set_lobby_message(
        self,
        message_id: int | None,
        channel_id: int | None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        origin_channel_id: int | None = None,
    ) -> None:
        """Set the lobby message, channel, and thread IDs, persisting to database."""
        self.lobby_message_id = message_id
        self.lobby_channel_id = channel_id
        if thread_id is not None:
            self.lobby_thread_id = thread_id
        if embed_message_id is not None:
            self.lobby_embed_message_id = embed_message_id
        if origin_channel_id is not None:
            self.origin_channel_id = origin_channel_id
        if self.lobby:
            self._persist_lobby()

    def reset_lobby(self) -> None:
        logger = logging.getLogger("cama_bot.services.lobby_manager")
        logger.info(f"reset_lobby called. Current lobby: {self.lobby}")
        if self.lobby:
            self.lobby.status = "closed"
        self.lobby = None
        self.lobby_message_id = None
        self.lobby_channel_id = None
        self.lobby_thread_id = None
        self.lobby_embed_message_id = None
        self.origin_channel_id = None
        self.readycheck_message_id = None
        self.readycheck_channel_id = None
        self.readycheck_lobby_ids = set()
        self.readycheck_reacted = {}
        self.readycheck_player_data = {}
        self._clear_persistent_lobby()
        logger.info("reset_lobby completed - cleared persistent lobby")

    def _persist_lobby(self) -> None:
        if not self.lobby:
            return
        self.lobby_repo.save_lobby_state(
            lobby_id=self.DEFAULT_LOBBY_ID,
            players=list(self.lobby.players),
            conditional_players=list(self.lobby.conditional_players),
            status=self.lobby.status,
            created_by=self.lobby.created_by,
            created_at=self.lobby.created_at.isoformat(),
            message_id=self.lobby_message_id,
            channel_id=self.lobby_channel_id,
            thread_id=self.lobby_thread_id,
            embed_message_id=self.lobby_embed_message_id,
            origin_channel_id=self.origin_channel_id,
            player_join_times=self.lobby.player_join_times,
        )

    def _clear_persistent_lobby(self) -> None:
        self.lobby_repo.clear_lobby_state(self.DEFAULT_LOBBY_ID)

    def _load_state(self) -> None:
        data = self.lobby_repo.load_lobby_state(self.DEFAULT_LOBBY_ID)
        if not data:
            return
        self.lobby = Lobby.from_dict(data)
        self.lobby_message_id = data.get("message_id")
        self.lobby_channel_id = data.get("channel_id")
        self.lobby_thread_id = data.get("thread_id")
        self.lobby_embed_message_id = data.get("embed_message_id")
        self.origin_channel_id = data.get("origin_channel_id")


# Backward compatibility alias - allows gradual migration
LobbyManager = LobbyManagerService
