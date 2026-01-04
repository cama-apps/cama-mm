"""
Lobby orchestration and embed helpers.
"""

from domain.models.lobby import Lobby, LobbyManager
from repositories.interfaces import IPlayerRepository
from utils.embeds import create_lobby_embed


class LobbyService:
    """Wraps LobbyManager with DB lookups and embed generation."""

    def __init__(
        self,
        lobby_manager: LobbyManager,
        player_repo: IPlayerRepository,
        ready_threshold: int = 10,
        max_players: int = 12,
    ):
        self.player_repo = player_repo
        self.lobby_manager = lobby_manager
        self.ready_threshold = ready_threshold
        self.max_players = max_players

    def get_or_create_lobby(self, creator_id: int | None = None) -> Lobby:
        return self.lobby_manager.get_or_create_lobby(creator_id=creator_id)

    def get_lobby(self) -> Lobby | None:
        return self.lobby_manager.get_lobby()

    def join_lobby(self, discord_id: int) -> tuple[bool, str]:
        lobby = self.get_or_create_lobby()

        if lobby.get_player_count() >= self.max_players:
            return False, f"Lobby is full ({self.max_players}/{self.max_players})."

        # Use manager's join_lobby which persists to database
        if not self.lobby_manager.join_lobby(discord_id, self.max_players):
            return False, "Already in lobby or lobby is closed."

        return True, ""

    def leave_lobby(self, discord_id: int) -> bool:
        # Use manager's leave_lobby which persists to database
        return self.lobby_manager.leave_lobby(discord_id)

    def reset_lobby(self):
        self.lobby_manager.reset_lobby()

    def set_lobby_message_id(self, message_id: int | None, channel_id: int | None = None):
        """Set the lobby message ID and optionally channel ID, persisting to database."""
        self.lobby_manager.set_lobby_message(message_id, channel_id)

    def get_lobby_message_id(self) -> int | None:
        return self.lobby_manager.lobby_message_id

    def get_lobby_channel_id(self) -> int | None:
        return self.lobby_manager.lobby_channel_id

    def get_lobby_players(self, lobby: Lobby) -> tuple[list[int], list]:
        player_ids = list(lobby.players)
        players = self.player_repo.get_by_ids(player_ids)
        return player_ids, players

    def build_lobby_embed(self, lobby: Lobby) -> object | None:
        if not lobby:
            return None
        player_ids, players = self.get_lobby_players(lobby)
        return create_lobby_embed(lobby, players, player_ids, ready_threshold=self.ready_threshold)

    def is_ready(self, lobby: Lobby) -> bool:
        return lobby.get_player_count() >= self.ready_threshold
