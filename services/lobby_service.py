"""
Lobby orchestration and embed helpers.
"""

import asyncio

from domain.models.lobby import Lobby
from domain.models.pending_match_state import PendingMatchState
from repositories.interfaces import IPlayerRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from utils.embeds import create_lobby_embed


class LobbyService:
    """Wraps LobbyManager with DB lookups and embed generation.

    All lobby operations are scoped per-guild. Callers should pass
    ``guild_id`` from Discord (``interaction.guild.id`` or ``None`` in DMs);
    the manager normalizes ``None`` to ``0``.
    """

    def __init__(
        self,
        lobby_manager: LobbyManager,
        player_repo: IPlayerRepository,
        ready_threshold: int = 10,
        max_players: int = 12,
        bankruptcy_repo=None,
        match_state_service=None,
    ):
        self.player_repo = player_repo
        self.lobby_manager = lobby_manager
        self.ready_threshold = ready_threshold
        self.max_players = max_players
        self.bankruptcy_repo = bankruptcy_repo
        self.match_state_service = match_state_service

    def get_creation_lock(self, guild_id: int | None = None) -> asyncio.Lock:
        """Get the per-guild lobby-creation lock.

        Delegates to :meth:`LobbyManagerService.get_creation_lock` so each
        Discord guild serializes its own /lobby creations independently.
        """
        return self.lobby_manager.get_creation_lock(guild_id=guild_id)

    def get_or_create_lobby(
        self, creator_id: int | None = None, guild_id: int | None = None
    ) -> Lobby:
        return self.lobby_manager.get_or_create_lobby(
            creator_id=creator_id, guild_id=guild_id
        )

    def get_lobby(self, guild_id: int | None = None) -> Lobby | None:
        return self.lobby_manager.get_lobby(guild_id=guild_id)

    def join_lobby(
        self, discord_id: int, guild_id: int | None = 0
    ) -> tuple[bool, str, PendingMatchState | None]:
        """
        Join a player to the lobby.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID for pending match lookup

        Returns:
            Tuple of (success, reason, pending_info):
            - success: True if joined, False otherwise
            - reason: "" on success, or one of: "in_pending_match", "lobby_full", "already_joined"
            - pending_info: PendingMatchState if blocked by an existing match, None otherwise
        """
        # Check if player is in a pending match FIRST
        if self.match_state_service:
            pending_match = self.match_state_service.get_pending_match_for_player(
                guild_id, discord_id
            )
            if pending_match:
                return False, "in_pending_match", pending_match

        # Delegate entirely to manager — capacity check is inside the lock,
        # which is the only place it can be race-free.
        reason = self.lobby_manager.join_lobby(
            discord_id, self.max_players, guild_id=guild_id
        )
        if reason == "ok":
            return True, "", None
        if reason == "full":
            return False, "lobby_full", None
        return False, "already_joined", None

    def join_lobby_conditional(
        self, discord_id: int, guild_id: int | None = 0
    ) -> tuple[bool, str, PendingMatchState | None]:
        """Reject joins to the deprecated conditional queue."""
        return False, "conditional_join_deprecated", None

    def leave_lobby(self, discord_id: int, guild_id: int | None = None) -> bool:
        return self.lobby_manager.leave_lobby(discord_id, guild_id=guild_id)

    def leave_lobby_conditional(
        self, discord_id: int, guild_id: int | None = None
    ) -> bool:
        """Remove legacy conditional state during compatibility cleanup."""
        return self.lobby_manager.leave_lobby_conditional(
            discord_id, guild_id=guild_id
        )

    def reset_lobby(self, guild_id: int | None = None):
        self.lobby_manager.reset_lobby(guild_id=guild_id)

    def set_lobby_message_id(
        self,
        message_id: int | None,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        origin_channel_id: int | None = None,
        guild_id: int | None = None,
    ):
        """Set the lobby message ID and optionally channel/thread IDs, persisting to database."""
        self.lobby_manager.set_lobby_message(
            message_id,
            channel_id,
            thread_id,
            embed_message_id,
            origin_channel_id,
            guild_id=guild_id,
        )

    def get_lobby_message_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_manager.get_lobby_message_id(guild_id=guild_id)

    def get_lobby_channel_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_manager.get_lobby_channel_id(guild_id=guild_id)

    def get_lobby_thread_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_manager.get_lobby_thread_id(guild_id=guild_id)

    def get_lobby_embed_message_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_manager.get_lobby_embed_message_id(guild_id=guild_id)

    def get_origin_channel_id(self, guild_id: int | None = None) -> int | None:
        """Get the channel where /lobby was originally run (for rally notifications)."""
        return self.lobby_manager.get_origin_channel_id(guild_id=guild_id)

    # -- Readycheck state --

    def set_readycheck_state(
        self,
        message_id: int,
        channel_id: int,
        lobby_ids: set[int],
        player_data: dict[int, dict],
        guild_id: int | None = None,
        created_at: float | None = None,
    ) -> None:
        """Store readycheck message info and classification data. Resets reacted set."""
        self.lobby_manager.set_readycheck_state(
            message_id,
            channel_id,
            lobby_ids,
            player_data,
            guild_id=guild_id,
            created_at=created_at,
        )

    def update_readycheck_data(
        self,
        lobby_ids: set[int],
        player_data: dict[int, dict],
        guild_id: int | None = None,
    ) -> None:
        """Update classification data on refresh (preserves reacted)."""
        self.lobby_manager.update_readycheck_data(
            lobby_ids, player_data, guild_id=guild_id
        )

    def get_readycheck_message_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_manager.get_readycheck_message_id(guild_id=guild_id)

    def get_readycheck_created_at(self, guild_id: int | None = None) -> float | None:
        return self.lobby_manager.get_readycheck_created_at(guild_id=guild_id)

    def get_readycheck_channel_id(self, guild_id: int | None = None) -> int | None:
        return self.lobby_manager.get_readycheck_channel_id(guild_id=guild_id)

    def get_readycheck_lobby_ids(self, guild_id: int | None = None) -> set[int]:
        return self.lobby_manager.get_readycheck_lobby_ids(guild_id=guild_id)

    def get_readycheck_reacted(self, guild_id: int | None = None) -> dict[int, str]:
        return self.lobby_manager.get_readycheck_reacted(guild_id=guild_id)

    def get_readycheck_confirmation_snapshot(
        self, guild_id: int | None = None
    ) -> tuple[int, set[int]] | None:
        return self.lobby_manager.get_readycheck_confirmation_snapshot(
            guild_id=guild_id
        )

    def get_lobby_players_and_readycheck_snapshot(
        self, guild_id: int | None = None
    ) -> tuple[list[int], list, tuple[int, set[int]] | None] | None:
        snapshot = self.lobby_manager.get_lobby_readycheck_snapshot(guild_id=guild_id)
        if snapshot is None:
            return None

        player_ids, readycheck = snapshot
        players = self.player_repo.get_by_ids(player_ids, guild_id)
        return player_ids, players, readycheck

    def get_readycheck_player_data(
        self, guild_id: int | None = None
    ) -> dict[int, dict]:
        return self.lobby_manager.get_readycheck_player_data(guild_id=guild_id)

    def add_readycheck_reaction(
        self,
        discord_id: int,
        tag: str,
        guild_id: int | None = None,
        expected_message_id: int | None = None,
    ) -> bool:
        """Record a ✅ reaction. Returns True if newly added."""
        return self.lobby_manager.add_readycheck_reaction(
            discord_id,
            tag,
            guild_id=guild_id,
            expected_message_id=expected_message_id,
        )

    def get_readycheck_reaction_count_for_message(
        self,
        message_id: int,
        guild_id: int | None = None,
    ) -> int | None:
        """Return confirmations only if ``message_id`` is still current."""
        return self.lobby_manager.get_readycheck_reaction_count_for_message(
            message_id,
            guild_id=guild_id,
        )

    def remove_readycheck_reaction(
        self,
        discord_id: int,
        guild_id: int | None = None,
        expected_message_id: int | None = None,
    ) -> bool:
        """Remove a ✅ reaction. Returns True if was present."""
        return self.lobby_manager.remove_readycheck_reaction(
            discord_id,
            guild_id=guild_id,
            expected_message_id=expected_message_id,
        )

    def get_lobby_players(self, lobby: Lobby, guild_id: int | None = None) -> tuple[list[int], list]:
        """Get regular (non-conditional) player IDs and Player objects."""
        player_ids = list(lobby.players)
        players = self.player_repo.get_by_ids(player_ids, guild_id)
        return player_ids, players

    def get_conditional_players(self, lobby: Lobby, guild_id: int | None = None) -> tuple[list[int], list]:
        """Return an empty roster for the deprecated conditional queue."""
        return [], []

    def build_lobby_embed(self, lobby: Lobby, guild_id: int | None = None) -> object | None:
        if not lobby:
            return None
        player_ids, players = self.get_lobby_players(lobby, guild_id)

        return create_lobby_embed(
            lobby, players, player_ids,
            ready_threshold=self.ready_threshold,
            max_players=self.max_players,
            bankruptcy_repo=self.bankruptcy_repo,
            guild_id=guild_id,
        )

    def is_ready(self, lobby: Lobby) -> bool:
        """Ready if the regular-player count meets the threshold."""
        return lobby.get_player_count() >= self.ready_threshold
