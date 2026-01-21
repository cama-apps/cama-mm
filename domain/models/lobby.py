"""
Lobby domain model with persistence helpers.
"""

from dataclasses import dataclass, field
from datetime import datetime

from repositories.interfaces import ILobbyRepository


@dataclass
class Lobby:
    """Represents a matchmaking lobby."""

    lobby_id: int
    created_by: int  # Discord ID of creator
    created_at: datetime
    players: set[int] = field(default_factory=set)
    conditional_players: set[int] = field(default_factory=set)  # "Frogling" players
    status: str = "open"

    def add_player(self, discord_id: int) -> bool:
        """Add a player to the regular queue. Removes from conditional if present."""
        if self.status != "open":
            return False
        if discord_id in self.players:
            return False
        # Remove from conditional if switching
        self.conditional_players.discard(discord_id)
        self.players.add(discord_id)
        return True

    def remove_player(self, discord_id: int) -> bool:
        if discord_id in self.players:
            self.players.remove(discord_id)
            return True
        return False

    def add_conditional_player(self, discord_id: int) -> bool:
        """Add a player to the conditional queue. Removes from regular if present."""
        if self.status != "open":
            return False
        if discord_id in self.conditional_players:
            return False
        # Remove from regular if switching
        self.players.discard(discord_id)
        self.conditional_players.add(discord_id)
        return True

    def remove_conditional_player(self, discord_id: int) -> bool:
        if discord_id in self.conditional_players:
            self.conditional_players.remove(discord_id)
            return True
        return False

    def is_player_conditional(self, discord_id: int) -> bool:
        """Check if a player is in the conditional set."""
        return discord_id in self.conditional_players

    def get_player_count(self) -> int:
        """Return count of regular players only."""
        return len(self.players)

    def get_conditional_count(self) -> int:
        """Return count of conditional players."""
        return len(self.conditional_players)

    def get_total_count(self) -> int:
        """Return combined count of regular and conditional players."""
        return len(self.players) + len(self.conditional_players)

    def is_ready(self, min_players: int = 10) -> bool:
        """Ready if combined total meets threshold."""
        return self.get_total_count() >= min_players

    def can_create_teams(self, player_roles: dict[int, list[str]]) -> bool:
        role_counts = {"1": 0, "2": 0, "3": 0, "4": 0, "5": 0}

        # Include both regular and conditional players
        all_players = self.players | self.conditional_players
        for player_id in all_players:
            if player_id in player_roles and player_roles[player_id]:
                primary_role = player_roles[player_id][0]
                if primary_role in role_counts:
                    role_counts[primary_role] += 1

        return all(count >= 2 for count in role_counts.values())

    def to_dict(self) -> dict:
        return {
            "lobby_id": self.lobby_id,
            "created_by": self.created_by,
            "created_at": self.created_at.isoformat(),
            "players": list(self.players),
            "conditional_players": list(self.conditional_players),
            "status": self.status,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "Lobby":
        created_at = data.get("created_at")
        created_at_dt = datetime.fromisoformat(created_at) if created_at else datetime.now()
        players = set(data.get("players", []))
        conditional_players = set(data.get("conditional_players", []))
        return cls(
            lobby_id=data.get("lobby_id", 1),
            created_by=data.get("created_by", 0),
            created_at=created_at_dt,
            players=players,
            conditional_players=conditional_players,
            status=data.get("status", "open"),
        )


class LobbyManager:
    """Manages a single global lobby."""

    DEFAULT_LOBBY_ID = 1

    def __init__(self, lobby_repo: ILobbyRepository):
        self.lobby_repo = lobby_repo
        self.lobby_message_id: int | None = None
        self.lobby_channel_id: int | None = None
        self.lobby_thread_id: int | None = None
        self.lobby_embed_message_id: int | None = None
        self.lobby: Lobby | None = None
        self._load_state()

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
    ) -> None:
        """Set the lobby message, channel, and thread IDs, persisting to database."""
        self.lobby_message_id = message_id
        self.lobby_channel_id = channel_id
        if thread_id is not None:
            self.lobby_thread_id = thread_id
        if embed_message_id is not None:
            self.lobby_embed_message_id = embed_message_id
        if self.lobby:
            self._persist_lobby()

    def reset_lobby(self) -> None:
        import logging
        logger = logging.getLogger("cama_bot.domain.lobby")
        logger.info(f"reset_lobby called. Current lobby: {self.lobby}")
        if self.lobby:
            self.lobby.status = "closed"
        self.lobby = None
        self.lobby_message_id = None
        self.lobby_channel_id = None
        self.lobby_thread_id = None
        self.lobby_embed_message_id = None
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
