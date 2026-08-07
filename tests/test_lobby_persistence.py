
import pytest

from repositories.lobby_repository import LobbyRepository
from repositories.player_repository import PlayerRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from tests.fakes.lobby_repo import FakeLobbyRepo


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


def test_lobby_kinds_persist_independently_in_same_guild(repo_db_path):
    lobby_repo = LobbyRepository(repo_db_path)
    manager1 = LobbyManager(lobby_repo)

    manager1.get_or_create_lobby(
        creator_id=10,
        guild_id=77,
        lobby_kind="open",
    )
    manager1.get_or_create_lobby(
        creator_id=20,
        guild_id=77,
        lobby_kind="lowskill",
    )
    assert manager1.join_lobby(101, guild_id=77, lobby_kind="open") == "ok"
    assert manager1.join_lobby(202, guild_id=77, lobby_kind="lowskill") == "ok"

    manager2 = LobbyManager(lobby_repo)

    open_lobby = manager2.get_lobby(guild_id=77, lobby_kind="open")
    lowskill_lobby = manager2.get_lobby(guild_id=77, lobby_kind="lowskill")
    assert open_lobby is not None
    assert lowskill_lobby is not None
    assert open_lobby.players == {101}
    assert lowskill_lobby.players == {202}
    assert open_lobby.lobby_id != lowskill_lobby.lobby_id


def test_lobby_service_join_persists_players(repo_db_path):
    """Test that players joined via LobbyService survive service restart."""
    # Create first service instance
    lobby_repo = LobbyRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    manager1 = LobbyManager(lobby_repo)
    service1 = LobbyService(manager1, player_repo)

    # Join players via service (this is what reaction handler does)
    service1.get_or_create_lobby(creator_id=100)
    success1, _, _ = service1.join_lobby(111)
    success2, _, _ = service1.join_lobby(222)
    success3, _, _ = service1.join_lobby(333)

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
    success4, _, _ = service2.join_lobby(444)
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
    service1.join_lobby(111, 0)
    service1.join_lobby(222, 0)
    service1.join_lobby(333, 0)

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


class LockCheckingLobbyRepo(FakeLobbyRepo):
    """Asserts persistence I/O never runs inside the manager's state lock."""

    manager: LobbyManager

    def save_lobby_state(self, *args, **kwargs):
        assert not self.manager._state_lock._is_owned()
        return super().save_lobby_state(*args, **kwargs)

    def clear_lobby_state(self, lobby_id, guild_id=None):
        assert not self.manager._state_lock._is_owned()
        return super().clear_lobby_state(lobby_id, guild_id=guild_id)


def test_lobby_persistence_runs_outside_state_lock():
    """DB writes must not run under the process-wide ``_state_lock`` the event
    loop also acquires synchronously (get_creation_lock)."""
    repo = LockCheckingLobbyRepo()
    manager = LobbyManager(repo)
    repo.manager = manager

    manager.get_or_create_lobby(creator_id=42, guild_id=7)
    assert manager.join_lobby(111, guild_id=7) == "ok"
    manager.set_lobby_message(message_id=1, channel_id=2, guild_id=7)
    assert manager.leave_lobby(111, guild_id=7) is True
    manager.reset_lobby(guild_id=7)

    assert manager.get_lobby(guild_id=7) is None
    assert repo.load_lobby_state(1, guild_id=7) is None


class FailingClearLobbyRepo(FakeLobbyRepo):
    def clear_lobby_state(self, lobby_id, guild_id=None):
        raise RuntimeError("database is locked")


def test_reset_lobby_failed_db_delete_keeps_in_memory_state():
    """The persisted row is deleted before memory is cleared: a failed delete
    must leave the in-memory lobby intact instead of letting the stale row
    resurrect a ghost lobby on the next restart."""
    repo = FailingClearLobbyRepo()
    manager = LobbyManager(repo)

    manager.get_or_create_lobby(creator_id=42, guild_id=7)
    assert manager.join_lobby(111, guild_id=7) == "ok"

    with pytest.raises(RuntimeError):
        manager.reset_lobby(guild_id=7)

    lobby = manager.get_lobby(guild_id=7)
    assert lobby is not None
    assert 111 in lobby.players
    # The row is still persisted, matching the surviving in-memory lobby.
    assert repo.load_lobby_state(lobby.lobby_id, guild_id=7) is not None
