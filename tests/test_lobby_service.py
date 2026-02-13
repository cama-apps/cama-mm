"""Tests for LobbyService with pending match check."""

import pytest
from unittest.mock import MagicMock

from services.lobby_service import LobbyService
from services.lobby_manager_service import LobbyManagerService
from domain.models.lobby import Lobby
from datetime import datetime


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
        lobby_manager.join_lobby.return_value = True

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

    def test_join_lobby_conditional_blocks_player_in_pending_match(self):
        """Player in pending match cannot join conditional queue."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = {
            "pending_match_id": 99,
            "shuffle_message_jump_url": None,
        }

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby_conditional(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "in_pending_match"
        assert pending_info["pending_match_id"] == 99
        lobby_manager.join_lobby_conditional.assert_not_called()

    def test_join_lobby_without_match_state_service_works(self):
        """LobbyService works without match_state_service (backwards compat)."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby.return_value = True

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=None,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is True
        assert pending_info is None

    def test_join_lobby_returns_lobby_full_reason(self):
        """Test that lobby full returns correct reason code."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        # Add 12 players to make it full
        for i in range(12):
            lobby.players.add(1000 + i)

        lobby_manager.get_or_create_lobby.return_value = lobby

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

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby.return_value = False  # Already in lobby

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "already_joined"
        assert pending_info is None

    def test_join_lobby_conditional_returns_already_joined_reason(self):
        """Test that already joined conditional returns correct reason code."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby_conditional.return_value = False  # Already in lobby

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby_conditional(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "already_joined"
        assert pending_info is None

    def test_join_lobby_conditional_allows_player_not_in_pending_match(self):
        """Player not in pending match can join conditional queue."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        lobby_manager.get_or_create_lobby.return_value = lobby
        lobby_manager.join_lobby_conditional.return_value = True

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
        )

        success, reason, pending_info = service.join_lobby_conditional(discord_id=12345, guild_id=999)

        assert success is True
        assert reason == ""
        assert pending_info is None
        lobby_manager.join_lobby_conditional.assert_called_once()

    def test_join_lobby_conditional_returns_lobby_full_reason(self):
        """Test that lobby full returns correct reason code for conditional join."""
        lobby_manager = MagicMock(spec=LobbyManagerService)
        player_repo = MagicMock()
        match_state_service = MagicMock()

        match_state_service.get_pending_match_for_player.return_value = None

        lobby = Lobby(lobby_id=1, created_by=None, created_at=datetime.now())
        # Add 12 players to make it full
        for i in range(12):
            lobby.players.add(1000 + i)

        lobby_manager.get_or_create_lobby.return_value = lobby

        service = LobbyService(
            lobby_manager=lobby_manager,
            player_repo=player_repo,
            match_state_service=match_state_service,
            max_players=12,
        )

        success, reason, pending_info = service.join_lobby_conditional(discord_id=12345, guild_id=999)

        assert success is False
        assert reason == "lobby_full"
        assert pending_info is None


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
