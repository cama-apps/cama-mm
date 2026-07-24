"""Tests for LobbyService with pending match check."""

from datetime import datetime
from unittest.mock import MagicMock

import pytest

from domain.models.lobby import Lobby
from services.lobby_manager_service import LobbyManagerService
from services.lobby_service import LobbyService
from tests.fakes.lobby_repo import FakeLobbyRepo


class TestLobbyServicePendingMatchCheck:
    """Test that join_lobby blocks players in pending matches."""

    def test_join_lobby_blocks_player_in_pending_match(self):
        """Player in pending match cannot join lobby."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = {
            "pending_match_id": 42,
            "shuffle_message_jump_url": "https://discord.com/jump/123",
        }

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "in_pending_match"
        assert pending_info["pending_match_id"] == 42
        assert pending_info["shuffle_message_jump_url"] == "https://discord.com/jump/123"
        lobby_manager.join_lobby.assert_not_called()

    def test_join_lobby_allows_player_not_in_pending_match(self):
        """Player not in pending match can join lobby."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby.return_value = "ok"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is True
        assert reason == ""
        assert pending_info is None
        lobby_manager.join_lobby.assert_called_once()

    def test_join_lobby_without_match_state_service_works(self):
        """LobbyService works without match_state_service (backwards compat)."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby.return_value = "ok"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=None,
        )

        success, _, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is True
        assert pending_info is None

    def test_join_lobby_returns_lobby_full_reason(self):
        """Test that lobby full returns correct reason code.

        The capacity check lives inside the manager (under the state lock).
        LobbyService just maps the "full" reason string through.
        """
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None
        lobby_manager.join_lobby.return_value = "full"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
            max_players=12,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "lobby_full"
        assert pending_info is None

    def test_join_lobby_returns_already_joined_reason(self):
        """Test that already joined returns correct reason code."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None
        lobby_manager.join_lobby.return_value = "already_joined"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "already_joined"
        assert pending_info is None

    def test_join_lobby_conditional_returns_deprecated_reason(self):
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None
        lobby_manager.join_lobby_conditional.return_value = "deprecated"

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby_conditional(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "conditional_join_deprecated"
        assert pending_info is None


def test_readycheck_confirmation_snapshot_is_available_through_lobby_service():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    lobby_manager.set_readycheck_state(111, 222, {1}, {})
    lobby_manager.add_readycheck_reaction(1, "<@1>")
    service = LobbyService(lobby_manager=lobby_manager, player_repo=MagicMock())

    assert service.get_readycheck_confirmation_snapshot() == (111, {1})


def test_lobby_readycheck_snapshot_loads_players_from_atomic_manager_snapshot():
    lobby_manager = LobbyManagerService(FakeLobbyRepo())
    lobby = lobby_manager.get_or_create_lobby(creator_id=1)
    lobby.players.update({1, 2})
    lobby_manager.set_readycheck_state(111, 222, {1, 2}, {})
    lobby_manager.add_readycheck_reaction(1, "<@1>")
    player_repo = MagicMock()
    players = [MagicMock(discord_id=1), MagicMock(discord_id=2)]
    player_repo.get_by_ids.return_value = players
    service = LobbyService(lobby_manager=lobby_manager, player_repo=player_repo)

    player_ids, loaded_players, readycheck = (
        service.get_lobby_players_and_readycheck_snapshot(guild_id=0)
    )

    assert set(player_ids) == {1, 2}
    assert loaded_players == players
    assert readycheck == (111, {1})
    player_repo.get_by_ids.assert_called_once_with(player_ids, 0)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
