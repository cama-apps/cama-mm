import pytest

from services.lobby_manager_service import LobbyManagerService as LobbyManager
from repositories.lobby_repository import LobbyRepository
from repositories.player_repository import PlayerRepository
from services.lobby_service import LobbyService


def test_lobby_manager_persists_and_recovers_state(repo_db_path):
    lobby_repo = LobbyRepository(repo_db_path)
    manager1 = LobbyManager(lobby_repo)

    lobby = manager1.get_or_create_lobby(creator_id=42)
    lobby.add_player(111)
    lobby.add_player(222)
    manager1._persist_lobby()

    # New instance should load persisted state
    manager2 = LobbyManager(lobby_repo)
    loaded = manager2.get_lobby()
    assert loaded is not None
    assert loaded.players == {111, 222}
    assert loaded.created_by == 42


def test_lobby_service_join_persists_players(repo_db_path):
    """Test that players joined via LobbyService survive service restart."""
    # Create first service instance
    lobby_repo = LobbyRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    manager1 = LobbyManager(lobby_repo)
    service1 = LobbyService(manager1, player_repo)

    # Join players via service (this is what reaction handler does)
    service1.get_or_create_lobby(creator_id=100)
    success1, _ = service1.join_lobby(111)
    success2, _ = service1.join_lobby(222)
    success3, _ = service1.join_lobby(333)

    assert success1, "Player 111 should join successfully"
    assert success2, "Player 222 should join successfully"
    assert success3, "Player 333 should join successfully"

    lobby1 = service1.get_lobby()
    assert lobby1.get_player_count() == 3

    # Simulate service restart - create new manager and service
    manager2 = LobbyManager(lobby_repo)
    service2 = LobbyService(manager2, player_repo)

    # Players should still be in lobby after restart
    lobby2 = service2.get_lobby()
    assert lobby2 is not None, "Lobby should exist after restart"
    assert lobby2.get_player_count() == 3, "All 3 players should survive restart"
    assert lobby2.players == {111, 222, 333}, "Player IDs should match"

    # Join another player after restart
    success4, _ = service2.join_lobby(444)
    assert success4, "Player 444 should join successfully after restart"
    assert lobby2.get_player_count() == 4

    # Simulate another restart
    manager3 = LobbyManager(lobby_repo)
    service3 = LobbyService(manager3, player_repo)
    lobby3 = service3.get_lobby()
    assert lobby3.get_player_count() == 4, "All 4 players should survive second restart"


def test_lobby_service_leave_persists(repo_db_path):
    """Test that player leave via LobbyService survives service restart."""
    lobby_repo = LobbyRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    manager1 = LobbyManager(lobby_repo)
    service1 = LobbyService(manager1, player_repo)

    # Add players
    service1.get_or_create_lobby(creator_id=100)
    service1.join_lobby(111)
    service1.join_lobby(222)
    service1.join_lobby(333)

    # One player leaves
    left = service1.leave_lobby(222)
    assert left, "Player 222 should leave successfully"

    lobby1 = service1.get_lobby()
    assert lobby1.get_player_count() == 2
    assert 222 not in lobby1.players

    # Restart
    manager2 = LobbyManager(lobby_repo)
    service2 = LobbyService(manager2, player_repo)
    lobby2 = service2.get_lobby()

    assert lobby2.get_player_count() == 2, "Only 2 players should remain after restart"
    assert lobby2.players == {111, 333}, "Correct players should remain"
    assert 222 not in lobby2.players, "Player 222 should still be gone"


def test_lobby_message_id_persists(repo_db_path):
    """Test that lobby message ID survives service restart."""
    lobby_repo = LobbyRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    manager1 = LobbyManager(lobby_repo)
    service1 = LobbyService(manager1, player_repo)

    service1.get_or_create_lobby(creator_id=100)
    service1.set_lobby_message_id(message_id=12345678, channel_id=87654321)

    assert service1.get_lobby_message_id() == 12345678
    assert service1.get_lobby_channel_id() == 87654321

    # Restart
    manager2 = LobbyManager(lobby_repo)
    service2 = LobbyService(manager2, player_repo)

    assert service2.get_lobby_message_id() == 12345678, "Message ID should survive restart"
    assert service2.get_lobby_channel_id() == 87654321, "Channel ID should survive restart"
