"""
Unit tests for lobby management.
"""

import pytest
from domain.models.lobby import Lobby, LobbyManager
from datetime import datetime
from database import Database


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


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

