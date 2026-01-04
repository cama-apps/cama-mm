"""
Unit tests for lobby management.
"""

import os
import pytest
import time
from domain.models.lobby import Lobby, LobbyManager
from datetime import datetime
from database import Database


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
        lobby = Lobby(
            lobby_id=1,
            created_by=12345,
            created_at=datetime.now()
        )
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
        for i, role in enumerate(['1', '2', '3', '4', '5', '1', '2', '3', '4', '5']):
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
            player_roles[player_id] = ['1']  # All carry
        
        assert lobby.can_create_teams(player_roles) is False


class TestLobbyManager:
    """Test LobbyManager class functionality."""
    
    def test_get_or_create_lobby(self):
        """Test getting or creating a lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
        lobby = manager.get_or_create_lobby(creator_id=12345)
        assert lobby is not None
        assert lobby.created_by == 12345
    
    def test_get_lobby_none(self):
        """Test getting lobby when none exists."""
        manager = LobbyManager(Database(db_path=":memory:"))
        lobby = manager.get_lobby()
        assert lobby is None
    
    def test_get_lobby_exists(self):
        """Test getting existing lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=12345)
        lobby = manager.get_lobby()
        assert lobby is not None
    
    def test_get_lobby_closed(self):
        """Test that closed lobbies aren't returned."""
        manager = LobbyManager(Database(db_path=":memory:"))
        lobby = manager.get_or_create_lobby()
        lobby.status = "closed"
        result = manager.get_lobby()
        assert result is None
    
    def test_join_lobby(self):
        """Test joining a lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
        result = manager.join_lobby(1001)
        assert result is True
        lobby = manager.get_lobby()
        assert 1001 in lobby.players
    
    def test_join_lobby_full(self):
        """Test joining a full lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
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
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.join_lobby(1001)
        result = manager.leave_lobby(1001)
        assert result is True
        lobby = manager.get_lobby()
        assert 1001 not in lobby.players
    
    def test_leave_lobby_not_in_lobby(self):
        """Test leaving when not in lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
        result = manager.leave_lobby(1001)
        assert result is False
    
    def test_reset_lobby(self):
        """Test resetting the lobby."""
        manager = LobbyManager(Database(db_path=":memory:"))
        lobby = manager.get_or_create_lobby()
        manager.join_lobby(1001)
        manager.lobby_message_id = 12345

        manager.reset_lobby()

        assert manager.lobby is None
        assert manager.lobby_message_id is None

    def test_reset_lobby_clears_channel_id(self):
        """Test that resetting the lobby also clears channel_id."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby()
        manager.set_lobby_message(message_id=12345, channel_id=67890)

        manager.reset_lobby()

        assert manager.lobby is None
        assert manager.lobby_message_id is None
        assert manager.lobby_channel_id is None


class TestLobbyPersistence:
    """Test lobby state persistence across bot restarts."""

    def test_message_and_channel_ids_persist_across_restart(self):
        """Test that message_id and channel_id are restored after restart."""
        # Use a file-based DB to persist across manager instances
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First "session" - create lobby and set message
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111222333, channel_id=444555666)

            # Verify IDs are set
            assert manager1.lobby_message_id == 111222333
            assert manager1.lobby_channel_id == 444555666

            # Simulate restart - create new manager with same DB
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # Verify IDs are restored
            assert manager2.lobby_message_id == 111222333
            assert manager2.lobby_channel_id == 444555666
        finally:
            _cleanup_db_file(db_path)

    def test_players_persist_across_restart(self):
        """Test that lobby players are restored after restart."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session - create lobby and add players
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.join_lobby(1002)
            manager1.join_lobby(1003)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

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
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

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
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.join_lobby(1002)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

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
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=99999)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            lobby = manager2.get_lobby()
            assert lobby is not None
            assert lobby.created_by == 99999
        finally:
            _cleanup_db_file(db_path)

    def test_lobby_status_persists_across_restart(self):
        """Test that lobby status is preserved after restart."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            lobby = manager2.get_lobby()
            assert lobby is not None
            assert lobby.status == "open"
        finally:
            _cleanup_db_file(db_path)

    def test_closed_lobby_not_restored_after_restart(self):
        """Test that a closed lobby doesn't restore message IDs."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)
            manager1.reset_lobby()  # Close the lobby

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # Lobby should not exist
            assert manager2.get_lobby() is None
            # Message IDs should be None since lobby was reset
            assert manager2.lobby_message_id is None
            assert manager2.lobby_channel_id is None
        finally:
            _cleanup_db_file(db_path)

    def test_message_id_without_channel_id(self):
        """Test handling when message_id is set but channel_id is None."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            # Set only message_id, not channel_id
            manager1.set_lobby_message(message_id=111, channel_id=None)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            assert manager2.lobby_message_id == 111
            assert manager2.lobby_channel_id is None
            # Lobby should still be usable
            lobby = manager2.get_lobby()
            assert lobby is not None
        finally:
            _cleanup_db_file(db_path)

    def test_set_lobby_message_persists_immediately(self):
        """Test that set_lobby_message triggers immediate persistence."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)

            # Set message IDs
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Immediately create new manager (no explicit save call needed)
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            assert manager2.lobby_message_id == 111
            assert manager2.lobby_channel_id == 222
        finally:
            _cleanup_db_file(db_path)

    def test_multiple_restarts_preserve_state(self):
        """Test that state is preserved across multiple restarts."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Second session - add more players
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)
            manager2.join_lobby(1002)

            # Third session - verify all state preserved
            db3 = Database(db_path=db_path)
            manager3 = LobbyManager(db3)

            assert manager3.lobby_message_id == 111
            assert manager3.lobby_channel_id == 222
            lobby = manager3.get_lobby()
            assert 1001 in lobby.players
            assert 1002 in lobby.players
        finally:
            _cleanup_db_file(db_path)

    def test_join_persists_message_id(self):
        """Test that joining lobby also persists message_id if already set."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Join lobby - this triggers _persist_lobby
            manager1.join_lobby(1001)

            # Verify message IDs still persisted
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            assert manager2.lobby_message_id == 111
            assert manager2.lobby_channel_id == 222
            assert 1001 in manager2.get_lobby().players
        finally:
            _cleanup_db_file(db_path)

    def test_leave_persists_message_id(self):
        """Test that leaving lobby preserves message_id."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.join_lobby(1002)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Player leaves - this triggers _persist_lobby
            manager1.leave_lobby(1001)

            # Verify message IDs still persisted
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            assert manager2.lobby_message_id == 111
            assert manager2.lobby_channel_id == 222
            assert 1001 not in manager2.get_lobby().players
            assert 1002 in manager2.get_lobby().players
        finally:
            _cleanup_db_file(db_path)

    def test_empty_lobby_still_has_message_id(self):
        """Test that an empty lobby (all players left) still has message_id."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.join_lobby(1001)
            manager1.set_lobby_message(message_id=111, channel_id=222)
            manager1.leave_lobby(1001)  # Lobby now empty

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # Message IDs should still be there
            assert manager2.lobby_message_id == 111
            assert manager2.lobby_channel_id == 222
            # Lobby should still exist but be empty
            lobby = manager2.get_lobby()
            assert lobby is not None
            assert lobby.get_player_count() == 0
        finally:
            _cleanup_db_file(db_path)

    def test_update_message_id_persists(self):
        """Test that updating message_id to a new value persists correctly."""
        import tempfile
        import os

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(message_id=111, channel_id=222)

            # Update to new message (e.g., lobby command run again)
            manager1.set_lobby_message(message_id=333, channel_id=444)

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # Should have the new values
            assert manager2.lobby_message_id == 333
            assert manager2.lobby_channel_id == 444
        finally:
            _cleanup_db_file(db_path)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

