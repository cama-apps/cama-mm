"""
Unit tests for lobby management.
"""

import os
import time
from datetime import datetime

import pytest

from domain.models.lobby import Lobby
from repositories.lobby_repository import LobbyRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from tests.fakes.lobby_repo import FakeLobbyRepo


def _cleanup_db_file(db_path: str) -> None:
    """Close sqlite handles and remove temp db with retries for Windows."""
    try:
        import sqlite3

        sqlite3.connect(db_path).close()
    except Exception:
        pass
    time.sleep(0.1)
    try:
        os.unlink(db_path)
    except PermissionError:
        time.sleep(0.2)
        try:
            os.unlink(db_path)
        except Exception:
            pass


class TestLobby:
    """Test Lobby class functionality."""

    def test_lobby_creation(self):
        """Test creating a lobby."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        assert lobby.lobby_id == 1
        assert lobby.created_by == 12345
        assert lobby.status == "open"
        assert len(lobby.players) == 0

    def test_add_player(self):
        """Test adding a player to the lobby."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        result = lobby.add_player(1001)
        assert result is True
        assert 1001 in lobby.players
        assert lobby.get_player_count() == 1

    def test_add_player_duplicate(self):
        """Test adding the same player twice."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        lobby.add_player(1001)
        result = lobby.add_player(1001)  # Try to add again
        assert result is False
        assert lobby.get_player_count() == 1

    def test_add_player_closed_lobby(self):
        """Test adding a player to a closed lobby."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        lobby.status = "closed"
        result = lobby.add_player(1001)
        assert result is False
        assert 1001 not in lobby.players

    def test_remove_player(self):
        """Test removing a player from the lobby."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        lobby.add_player(1001)
        result = lobby.remove_player(1001)
        assert result is True
        assert 1001 not in lobby.players
        assert lobby.get_player_count() == 0

    def test_remove_player_not_in_lobby(self):
        """Test removing a player who isn't in the lobby."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        result = lobby.remove_player(1001)
        assert result is False

    def test_is_ready(self):
        """Test checking if lobby is ready."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        # Add 10 players
        for i in range(10):
            lobby.add_player(1000 + i)
        assert lobby.is_ready() is True
        assert lobby.is_ready(min_players=12) is False

    def test_can_create_teams(self):
        """Test checking if lobby can create balanced teams."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        # Add 10 players with roles
        player_roles = {}
        for i, role in enumerate(["1", "2", "3", "4", "5", "1", "2", "3", "4", "5"]):
            player_id = 1000 + i
            lobby.add_player(player_id)
            player_roles[player_id] = [role]

        assert lobby.can_create_teams(player_roles) is True

    def test_can_create_teams_insufficient_roles(self):
        """Test that lobby can't create teams with insufficient role diversity."""
        lobby = Lobby(lobby_id=1, created_by=12345, created_at=datetime.now())
        # Add 10 players but all with same role
        player_roles = {}
        for i in range(10):
            player_id = 1000 + i
            lobby.add_player(player_id)
            player_roles[player_id] = ["1"]  # All carry

        assert lobby.can_create_teams(player_roles) is False


class TestLobbyManager:
    """Test LobbyManager class functionality."""

    def test_get_or_create_lobby(self):
        """Test getting or creating a lobby."""
        manager = LobbyManager(FakeLobbyRepo())
        lobby = manager.get_or_create_lobby(creator_id=12345)
        assert lobby is not None
        assert lobby.created_by == 12345

    def test_get_lobby_none(self):
        """Test getting lobby when none exists."""
        manager = LobbyManager(FakeLobbyRepo())
        lobby = manager.get_lobby()
        assert lobby is None

    def test_get_lobby_exists(self):
        """Test getting existing lobby."""
        manager = LobbyManager(FakeLobbyRepo())
        manager.get_or_create_lobby(creator_id=12345)
        lobby = manager.get_lobby()
        assert lobby is not None

    def test_get_lobby_closed(self):
        """Test that closed lobbies aren't returned."""
        manager = LobbyManager(FakeLobbyRepo())
        lobby = manager.get_or_create_lobby()
        lobby.status = "closed"
        result = manager.get_lobby()
        assert result is None

    def test_join_lobby(self):
        """Test joining a lobby."""
        manager = LobbyManager(FakeLobbyRepo())
        result = manager.join_lobby(1001)
        assert result is True
        lobby = manager.get_lobby()
        assert 1001 in lobby.players

    def test_join_lobby_full(self):
        """Test joining a full lobby."""
        manager = LobbyManager(FakeLobbyRepo())
        # Fill lobby to 12 players
        for i in range(12):
            manager.join_lobby(1000 + i)

        # Try to join when full
        result = manager.join_lobby(9999)
        assert result is False
        lobby = manager.get_lobby()
        assert 9999 not in lobby.players
        assert lobby.get_player_count() == 12

    def test_leave_lobby(self):
        """Test leaving a lobby."""
        manager = LobbyManager(FakeLobbyRepo())
        manager.join_lobby(1001)
        result = manager.leave_lobby(1001)
        assert result is True
        lobby = manager.get_lobby()
        assert 1001 not in lobby.players

    def test_leave_lobby_not_in_lobby(self):
        """Test leaving when not in lobby."""
        manager = LobbyManager(FakeLobbyRepo())
        result = manager.leave_lobby(1001)
        assert result is False

    def test_reset_lobby(self):
        """Test resetting the lobby."""
        manager = LobbyManager(FakeLobbyRepo())
        manager.get_or_create_lobby()
        manager.join_lobby(1001)
        manager.set_lobby_message(message_id=12345, channel_id=None)

        manager.reset_lobby()

        assert manager.get_lobby(guild_id=0) is None
        assert manager.get_lobby_message_id(guild_id=0) is None

    def test_reset_lobby_clears_channel_id(self):
        """Test that resetting the lobby also clears channel_id."""
        manager = LobbyManager(FakeLobbyRepo())
        manager.get_or_create_lobby()
        manager.set_lobby_message(message_id=12345, channel_id=67890)

        manager.reset_lobby()

        assert manager.get_lobby(guild_id=0) is None
        assert manager.get_lobby_message_id(guild_id=0) is None
        assert manager.get_lobby_channel_id(guild_id=0) is None


class TestLobbyPersistence:
    """Test lobby state persistence across bot restarts."""

    def test_message_and_channel_ids_persist_across_restart(self, repo_db_path):
        """Test that message_id and channel_id are restored after restart."""
        # First "session" - create lobby and set message
        manager1 = LobbyManager(LobbyRepository(repo_db_path))
        manager1.get_or_create_lobby(creator_id=12345)
        manager1.set_lobby_message(message_id=111222333, channel_id=444555666)

        # Verify IDs are set
        assert manager1.get_lobby_message_id(guild_id=0) == 111222333
        assert manager1.get_lobby_channel_id(guild_id=0) == 444555666

        # Simulate restart - create new manager with same DB
        manager2 = LobbyManager(LobbyRepository(repo_db_path))

        # Verify IDs are restored
        assert manager2.get_lobby_message_id(guild_id=0) == 111222333
        assert manager2.get_lobby_channel_id(guild_id=0) == 444555666

    def test_players_persist_across_restart(self):
        """Test that lobby players are restored after restart."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session - create lobby and add players
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.join_lobby(1002)
            manager1.join_lobby(1003)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            # Verify players are restored
            lobby = manager2.get_lobby()
            assert lobby is not None
            assert 1001 in lobby.players
            assert 1002 in lobby.players
            assert 1003 in lobby.players
            assert lobby.get_player_count() == 3
        finally:
            _cleanup_db_file(db_path)

    def test_can_join_lobby_after_restart(self):
        """Test that new players can join the lobby after restart."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            # New player joins after restart
            result = manager2.join_lobby(1002)
            assert result is True

            lobby = manager2.get_lobby()
            assert 1001 in lobby.players
            assert 1002 in lobby.players
            assert lobby.get_player_count() == 2
        finally:
            _cleanup_db_file(db_path)

    def test_can_leave_lobby_after_restart(self):
        """Test that players can leave the lobby after restart."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.join_lobby(1002)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            # Player leaves after restart
            result = manager2.leave_lobby(1001)
            assert result is True

            lobby = manager2.get_lobby()
            assert 1001 not in lobby.players
            assert 1002 in lobby.players
            assert lobby.get_player_count() == 1
        finally:
            _cleanup_db_file(db_path)

    def test_lobby_creator_persists_across_restart(self):
        """Test that lobby creator info is preserved after restart."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=99999)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            lobby = manager2.get_lobby()
            assert lobby is not None
            assert lobby.created_by == 99999
        finally:
            _cleanup_db_file(db_path)

    def test_lobby_status_persists_across_restart(self):
        """Test that lobby status is preserved after restart."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            lobby = manager2.get_lobby()
            assert lobby is not None
            assert lobby.status == "open"
        finally:
            _cleanup_db_file(db_path)

    def test_closed_lobby_not_restored_after_restart(self):
        """Test that a closed lobby doesn't restore message IDs."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)
            manager1.reset_lobby()  # Close the lobby

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            # Lobby should not exist
            assert manager2.get_lobby() is None
            # Message IDs should be None since lobby was reset
            assert manager2.get_lobby_message_id(guild_id=0) is None
            assert manager2.get_lobby_channel_id(guild_id=0) is None
        finally:
            _cleanup_db_file(db_path)

    def test_message_id_without_channel_id(self):
        """Test handling when message_id is set but channel_id is None."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            # Set only message_id, not channel_id
            manager1.set_lobby_message(message_id=111, channel_id=None)

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            assert manager2.get_lobby_message_id(guild_id=0) == 111
            assert manager2.get_lobby_channel_id(guild_id=0) is None
            # Lobby should still be usable
            lobby = manager2.get_lobby()
            assert lobby is not None
        finally:
            _cleanup_db_file(db_path)

    def test_set_lobby_message_persists_immediately(self):
        """Test that set_lobby_message triggers immediate persistence."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)

            # Set message IDs
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Immediately create new manager (no explicit save call needed)
            manager2 = LobbyManager(LobbyRepository(db_path))

            assert manager2.get_lobby_message_id(guild_id=0) == 111
            assert manager2.get_lobby_channel_id(guild_id=0) == 222
        finally:
            _cleanup_db_file(db_path)

    def test_multiple_restarts_preserve_state(self):
        """Test that state is preserved across multiple restarts."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Second session - add more players
            manager2 = LobbyManager(LobbyRepository(db_path))
            manager2.join_lobby(1002)

            # Third session - verify all state preserved
            manager3 = LobbyManager(LobbyRepository(db_path))

            assert manager3.get_lobby_message_id(guild_id=0) == 111
            assert manager3.get_lobby_channel_id(guild_id=0) == 222
            lobby = manager3.get_lobby()
            assert 1001 in lobby.players
            assert 1002 in lobby.players
        finally:
            _cleanup_db_file(db_path)

    def test_join_persists_message_id(self):
        """Test that joining lobby also persists message_id if already set."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Join lobby - this triggers _persist_lobby
            manager1.join_lobby(1001)

            # Verify message IDs still persisted
            manager2 = LobbyManager(LobbyRepository(db_path))

            assert manager2.get_lobby_message_id(guild_id=0) == 111
            assert manager2.get_lobby_channel_id(guild_id=0) == 222
            assert 1001 in manager2.get_lobby().players
        finally:
            _cleanup_db_file(db_path)

    def test_leave_persists_message_id(self):
        """Test that leaving lobby preserves message_id."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.join_lobby(1002)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Player leaves - this triggers _persist_lobby
            manager1.leave_lobby(1001)

            # Verify message IDs still persisted
            manager2 = LobbyManager(LobbyRepository(db_path))

            assert manager2.get_lobby_message_id(guild_id=0) == 111
            assert manager2.get_lobby_channel_id(guild_id=0) == 222
            assert 1001 not in manager2.get_lobby().players
            assert 1002 in manager2.get_lobby().players
        finally:
            _cleanup_db_file(db_path)

    def test_empty_lobby_still_has_message_id(self):
        """Test that an empty lobby (all players left) still has message_id."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.set_lobby_message(message_id=111, channel_id=222)
            manager1.leave_lobby(1001)  # Lobby now empty

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            # Message IDs should still be there
            assert manager2.get_lobby_message_id(guild_id=0) == 111
            assert manager2.get_lobby_channel_id(guild_id=0) == 222
            # Lobby should still exist but be empty
            lobby = manager2.get_lobby()
            assert lobby is not None
            assert lobby.get_player_count() == 0
        finally:
            _cleanup_db_file(db_path)

    def test_update_message_id_persists(self):
        """Test that updating message_id to a new value persists correctly."""
        import tempfile

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager1 = LobbyManager(LobbyRepository(db_path))
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Update to new message (e.g., lobby command run again)
            manager1.set_lobby_message(message_id=333, channel_id=444)

            # Simulate restart
            manager2 = LobbyManager(LobbyRepository(db_path))

            # Should have the new values
            assert manager2.get_lobby_message_id(guild_id=0) == 333
            assert manager2.get_lobby_channel_id(guild_id=0) == 444
        finally:
            _cleanup_db_file(db_path)


class TestLobbyMultiGuildIsolation:
    """Regression tests: lobbies must be isolated across Discord guilds."""

    def test_lobby_state_is_per_guild(self, repo_db_path):
        """Two guilds get two independent Lobby instances with distinct players."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        manager = LobbyManager(LobbyRepository(repo_db_path))

        lobby_a = manager.get_or_create_lobby(
            creator_id=1001, guild_id=TEST_GUILD_ID
        )
        lobby_b = manager.get_or_create_lobby(
            creator_id=2001, guild_id=TEST_GUILD_ID_SECONDARY
        )

        # Distinct objects, tagged with their own guild_id
        assert lobby_a is not lobby_b
        assert lobby_a.guild_id == TEST_GUILD_ID
        assert lobby_b.guild_id == TEST_GUILD_ID_SECONDARY

        assert manager.join_lobby(111, guild_id=TEST_GUILD_ID) is True
        assert manager.join_lobby(222, guild_id=TEST_GUILD_ID) is True
        assert manager.join_lobby(333, guild_id=TEST_GUILD_ID_SECONDARY) is True
        assert manager.join_lobby(444, guild_id=TEST_GUILD_ID_SECONDARY) is True

        a = manager.get_lobby(guild_id=TEST_GUILD_ID)
        b = manager.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert a is not None and b is not None

        # Guild A only sees 111/222; Guild B only sees 333/444
        assert a.players == {111, 222}
        assert b.players == {333, 444}
        # And no cross-leakage
        assert 333 not in a.players and 444 not in a.players
        assert 111 not in b.players and 222 not in b.players

    def test_reset_one_guild_leaves_other_alone(self, repo_db_path):
        """Resetting guild A's lobby must not touch guild B's state."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        manager = LobbyManager(LobbyRepository(repo_db_path))

        manager.get_or_create_lobby(creator_id=1, guild_id=TEST_GUILD_ID)
        manager.get_or_create_lobby(creator_id=2, guild_id=TEST_GUILD_ID_SECONDARY)
        manager.join_lobby(111, guild_id=TEST_GUILD_ID)
        manager.join_lobby(222, guild_id=TEST_GUILD_ID_SECONDARY)
        manager.set_lobby_message(
            message_id=9001, channel_id=9002, guild_id=TEST_GUILD_ID
        )
        manager.set_lobby_message(
            message_id=9101, channel_id=9102, guild_id=TEST_GUILD_ID_SECONDARY
        )

        manager.reset_lobby(guild_id=TEST_GUILD_ID)

        assert manager.get_lobby(guild_id=TEST_GUILD_ID) is None
        assert manager.get_lobby_message_id(guild_id=TEST_GUILD_ID) is None
        assert manager.get_lobby_channel_id(guild_id=TEST_GUILD_ID) is None

        b = manager.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert b is not None
        assert b.players == {222}
        assert manager.get_lobby_message_id(guild_id=TEST_GUILD_ID_SECONDARY) == 9101
        assert manager.get_lobby_channel_id(guild_id=TEST_GUILD_ID_SECONDARY) == 9102

    def test_multi_guild_lobbies_survive_restart(self, repo_db_path):
        """Both guilds' lobbies persist across a manager restart and stay distinct."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        repo = LobbyRepository(repo_db_path)
        manager1 = LobbyManager(repo)

        manager1.get_or_create_lobby(creator_id=1001, guild_id=TEST_GUILD_ID)
        manager1.get_or_create_lobby(creator_id=2001, guild_id=TEST_GUILD_ID_SECONDARY)
        manager1.join_lobby(111, guild_id=TEST_GUILD_ID)
        manager1.join_lobby(222, guild_id=TEST_GUILD_ID)
        manager1.join_lobby(333, guild_id=TEST_GUILD_ID_SECONDARY)

        # Fresh manager over the same persistent repo
        manager2 = LobbyManager(repo)

        a = manager2.get_lobby(guild_id=TEST_GUILD_ID)
        b = manager2.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert a is not None and b is not None
        assert a.players == {111, 222}
        assert b.players == {333}
        assert a.guild_id == TEST_GUILD_ID
        assert b.guild_id == TEST_GUILD_ID_SECONDARY

    def test_leave_is_scoped_to_guild(self, repo_db_path):
        """Leaving in guild A must not remove a player from guild B's lobby."""
        from repositories.lobby_repository import LobbyRepository
        from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

        manager = LobbyManager(LobbyRepository(repo_db_path))
        # Same discord id is in both guilds' lobbies
        manager.join_lobby(555, guild_id=TEST_GUILD_ID)
        manager.join_lobby(555, guild_id=TEST_GUILD_ID_SECONDARY)

        assert manager.leave_lobby(555, guild_id=TEST_GUILD_ID) is True
        a = manager.get_lobby(guild_id=TEST_GUILD_ID)
        b = manager.get_lobby(guild_id=TEST_GUILD_ID_SECONDARY)
        assert a is not None and 555 not in a.players
        assert b is not None and 555 in b.players


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
