"""
Lobby orchestration and embed helpers.
"""

import asyncio

from domain.models.lobby import Lobby
from repositories.interfaces import IPlayerRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from utils.embeds import create_lobby_embed


class LobbyService:
    """Wraps LobbyManager with DB lookups and embed generation."""

    def __init__(
        self,
        lobby_manager: LobbyManager,
        player_repo: IPlayerRepository,
        ready_threshold: int = 10,
        max_players: int = 12,
        bankruptcy_repo=None,
    ):
        self.player_repo = player_repo
        self.lobby_manager = lobby_manager
        self.ready_threshold = ready_threshold
        self.max_players = max_players
        self.bankruptcy_repo = bankruptcy_repo

    @property
    def creation_lock(self) -> asyncio.Lock:
        """Lock for protecting the full lobby creation flow."""
        return self.lobby_manager.creation_lock

    def get_or_create_lobby(self, creator_id: int | None = None) -> Lobby:
        return self.lobby_manager.get_or_create_lobby(creator_id=creator_id)

    def get_lobby(self) -> Lobby | None:
        return self.lobby_manager.get_lobby()

    def join_lobby(self, discord_id: int) -> tuple[bool, str]:
        lobby = self.get_or_create_lobby()

        if lobby.get_total_count() >= self.max_players:
            return False, f"Lobby is full ({self.max_players}/{self.max_players})."

        # Use manager's join_lobby which persists to database
        if not self.lobby_manager.join_lobby(discord_id, self.max_players):
            return False, "Already in lobby or lobby is closed."

        return True, ""

    def join_lobby_conditional(self, discord_id: int) -> tuple[bool, str]:
        """Add a player to the conditional (frogling) queue."""
        lobby = self.get_or_create_lobby()

        if lobby.get_total_count() >= self.max_players:
            return False, f"Lobby is full ({self.max_players}/{self.max_players})."

        # Use manager's join_lobby_conditional which persists to database
        if not self.lobby_manager.join_lobby_conditional(discord_id, self.max_players):
            return False, "Already in lobby or lobby is closed."

        return True, ""

    def leave_lobby(self, discord_id: int) -> bool:
        # Use manager's leave_lobby which persists to database
        return self.lobby_manager.leave_lobby(discord_id)

    def leave_lobby_conditional(self, discord_id: int) -> bool:
        """Remove a player from the conditional (frogling) queue."""
        return self.lobby_manager.leave_lobby_conditional(discord_id)

    def reset_lobby(self):
        self.lobby_manager.reset_lobby()

    def set_lobby_message_id(
        self,
        message_id: int | None,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        origin_channel_id: int | None = None,
    ):
        """Set the lobby message ID and optionally channel/thread IDs, persisting to database."""
        self.lobby_manager.set_lobby_message(
            message_id, channel_id, thread_id, embed_message_id, origin_channel_id
        )

    def get_lobby_message_id(self) -> int | None:
        return self.lobby_manager.lobby_message_id

    def get_lobby_channel_id(self) -> int | None:
        return self.lobby_manager.lobby_channel_id

    def get_lobby_thread_id(self) -> int | None:
        return self.lobby_manager.lobby_thread_id

    def get_lobby_embed_message_id(self) -> int | None:
        return self.lobby_manager.lobby_embed_message_id

    def get_origin_channel_id(self) -> int | None:
        """Get the channel where /lobby was originally run (for rally notifications)."""
        return self.lobby_manager.origin_channel_id

    def get_lobby_players(self, lobby: Lobby, guild_id: int | None = None) -> tuple[list[int], list]:
        """Get regular (non-conditional) player IDs and Player objects."""
        player_ids = list(lobby.players)
        players = self.player_repo.get_by_ids(player_ids, guild_id)
        return player_ids, players

    def get_conditional_players(self, lobby: Lobby, guild_id: int | None = None) -> tuple[list[int], list]:
        """Get conditional (frogling) player IDs and Player objects."""
        player_ids = list(lobby.conditional_players)
        players = self.player_repo.get_by_ids(player_ids, guild_id)
        return player_ids, players

    def build_lobby_embed(self, lobby: Lobby, guild_id: int | None = None) -> object | None:
        if not lobby:
            return None
        player_ids, players = self.get_lobby_players(lobby, guild_id)
        conditional_ids, conditional_players = self.get_conditional_players(lobby, guild_id)

        # Fetch captain-eligible IDs from all lobby players
        all_ids = player_ids + conditional_ids
        captain_eligible_ids = set(self.player_repo.get_captain_eligible_players(all_ids, guild_id)) if all_ids else set()

        return create_lobby_embed(
            lobby, players, player_ids,
            conditional_players=conditional_players,
            conditional_ids=conditional_ids,
            ready_threshold=self.ready_threshold,
            max_players=self.max_players,
            bankruptcy_repo=self.bankruptcy_repo,
            captain_eligible_ids=captain_eligible_ids,
        )

    def is_ready(self, lobby: Lobby) -> bool:
        """Ready if combined total meets threshold."""
        return lobby.get_total_count() >= self.ready_threshold
