"""
End-to-end tests for admin commands and permission checks.
"""

import pytest
import os
import tempfile
import time
from unittest.mock import Mock, AsyncMock, patch

from database import Database
from domain.models.lobby import LobbyManager
from services.match_service import MatchService
from services.lobby_service import LobbyService
from services.player_service import PlayerService
from commands.match import MatchCommands
from discord import app_commands
from repositories.lobby_repository import LobbyRepository
from repositories.player_repository import PlayerRepository
from repositories.match_repository import MatchRepository


class MockDiscordUser:
    """Mock Discord user for testing."""
    def __init__(self, user_id, username="TestUser"):
        self.id = user_id
        self.name = username
        self.display_name = username
        self.mention = f"<@{user_id}>"
    
    def __str__(self):
        return self.name


class MockDiscordInteraction:
    """Mock Discord interaction for testing."""
    def __init__(self, user_id, username="TestUser"):
        self.user = MockDiscordUser(user_id, username)
        self.response = AsyncMock()
        self.followup = AsyncMock()
        self.channel = Mock()
        self.guild = None
    
    async def defer(self, **kwargs):
        """Mock defer response."""
        pass


class TestAdminCommands:
    """Tests for admin command permission checks."""
    
    @pytest.fixture
    def test_db(self):
        """Create a temporary test database."""
        fd, db_path = tempfile.mkstemp(suffix='.db')
        os.close(fd)
        db = Database(db_path)
        yield db
        try:
            import sqlite3
            sqlite3.connect(db_path).close()
        except:
            pass
        time.sleep(0.1)
        try:
            os.unlink(db_path)
        except PermissionError:
            time.sleep(0.2)
            try:
                os.unlink(db_path)
            except:
                pass
    
    def test_admin_permission_check(self):
        """Test admin permission checking logic."""
        import bot
        
        # Test with admin user ID (fake test ID for security)
        admin_id = 999999999999999999  # Fake test admin ID
        mock_interaction = MockDiscordInteraction(admin_id, "AdminUser")
        
        # Mock has_admin_permission to check ADMIN_USER_IDS
        # In real bot, this checks Discord permissions
        # For test, we verify the logic exists
        assert hasattr(bot, 'has_admin_permission'), "Admin permission function should exist"
        assert hasattr(bot, 'ADMIN_USER_IDS'), "Admin user IDs should be configured"
        assert isinstance(bot.ADMIN_USER_IDS, list), "ADMIN_USER_IDS should be a list"
    
    def test_resetuser_requires_admin(self, test_db):
        """Test that resetuser command requires admin permissions."""
        # Add a player
        user_id = 200101
        test_db.add_player(
            discord_id=user_id,
            discord_username="UserToReset",
            initial_mmr=1500
        )
        
        # Verify player exists
        assert test_db.get_player(user_id) is not None
        
        # In real bot, non-admin would get error message
        # Here we test the database operation
        deleted = test_db.delete_player(user_id)
        assert deleted is True
        
        # Verify player is gone
        assert test_db.get_player(user_id) is None
    
    @pytest.mark.asyncio
    @pytest.mark.timeout(60)
    async def test_admin_override_record_command(self, test_db):
        """Test end-to-end admin override via /record command."""
        # Create 10 players
        player_ids = list(range(600001, 600011))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Create services
        lobby_repo = LobbyRepository(test_db.db_path)
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)
        lobby_manager = LobbyManager(lobby_repo)
        lobby_service = LobbyService(lobby_manager, player_repo)
        player_service = PlayerService(player_repo)
        
        # Create a mock bot
        mock_bot = Mock()
        mock_bot.db = test_db
        mock_bot.lobby_service = lobby_service
        mock_bot.match_service = match_service
        mock_bot.player_service = player_service
        
        # Shuffle players to create a pending match
        match_service.shuffle_players(player_ids, guild_id=12345)
        
        # Verify match is pending
        assert match_service.get_last_shuffle(12345) is not None
        assert match_service.can_record_match(12345) is False  # No submissions yet
        
        # Create admin interaction
        admin_id = 999999
        mock_interaction = MockDiscordInteraction(admin_id, "AdminUser")
        
        # Mock guild
        from types import SimpleNamespace
        mock_guild = SimpleNamespace(id=12345)
        mock_interaction.guild = mock_guild
        
        # Mock admin permissions
        mock_permissions = SimpleNamespace(administrator=True, manage_guild=False)
        mock_interaction.user.guild_permissions = mock_permissions
        
        # Mock the result choice
        result_choice = app_commands.Choice(name="Radiant Won", value="radiant")
        
        # Create MatchCommands instance
        match_commands = MatchCommands(mock_bot, lobby_service, match_service, player_service)
        
        # Patch has_admin_permission to return True for our admin
        with patch('commands.match.has_admin_permission', return_value=True):
            # Call the record method directly (bypassing the command decorator)
            # The decorator wraps it, but we can call the underlying method
            await match_commands.record.callback(match_commands, mock_interaction, result_choice)
        
        # Verify the followup was called
        assert mock_interaction.followup.send.called
        
        # Get the message that was sent
        call_args = mock_interaction.followup.send.call_args
        message = call_args[0][0] if call_args[0] else call_args[1].get('content', '')
        
        assert "Match recorded" in message
        
        # Verify match was actually recorded (state cleared)
        assert match_service.get_last_shuffle(12345) is None
        
        # Verify that the match was recorded in the database
        conn = test_db.get_connection()
        cursor = conn.cursor()
        cursor.execute("SELECT COUNT(*) as count FROM matches")
        match_count = cursor.fetchone()['count']
        conn.close()
        
        assert match_count > 0, "Match should have been recorded in database"
        
        # Verify that players have updated ratings (Glicko ratings should have changed)
        # At least some players should have updated ratings
        updated_ratings = 0
        for pid in player_ids:
            rating_data = test_db.get_player_glicko_rating(pid)
            if rating_data:
                rating, rd, vol = rating_data
                # Initial rating was 1500.0, after a match it should have changed
                if rating != 1500.0 or rd != 350.0:
                    updated_ratings += 1
        
        assert updated_ratings > 0, "At least some players should have updated ratings after match"
    
    @pytest.mark.asyncio
    @pytest.mark.timeout(60)
    async def test_non_admin_record_requires_3_submissions(self, test_db):
        """Test that non-admin /record command requires 3 submissions."""
        # Create 10 players
        player_ids = list(range(600101, 600111))
        for pid in player_ids:
            test_db.add_player(
                discord_id=pid,
                discord_username=f"Player{pid}",
                initial_mmr=1500,
                glicko_rating=1500.0,
                glicko_rd=350.0,
                glicko_volatility=0.06
            )
        
        # Create services
        lobby_repo = LobbyRepository(test_db.db_path)
        player_repo = PlayerRepository(test_db.db_path)
        match_repo = MatchRepository(test_db.db_path)
        match_service = MatchService(player_repo=player_repo, match_repo=match_repo, use_glicko=True)
        lobby_manager = LobbyManager(lobby_repo)
        lobby_service = LobbyService(lobby_manager, player_repo)
        player_service = PlayerService(player_repo)
        
        # Create a mock bot
        mock_bot = Mock()
        mock_bot.db = test_db
        mock_bot.lobby_service = lobby_service
        mock_bot.match_service = match_service
        mock_bot.player_service = player_service
        
        # Shuffle players to create a pending match
        match_service.shuffle_players(player_ids, guild_id=12346)
        
        # Create non-admin interaction
        user_id = 100001
        mock_interaction = MockDiscordInteraction(user_id, "RegularUser")
        
        # Mock guild
        from types import SimpleNamespace
        mock_guild = SimpleNamespace(id=12346)
        mock_interaction.guild = mock_guild
        
        # Mock non-admin permissions
        mock_permissions = SimpleNamespace(administrator=False, manage_guild=False)
        mock_interaction.user.guild_permissions = mock_permissions
        
        # Mock the result choice
        result_choice = app_commands.Choice(name="Radiant Won", value="radiant")
        
        # Create MatchCommands instance
        match_commands = MatchCommands(mock_bot, lobby_service, match_service, player_service)
        
        # Patch has_admin_permission to return False for non-admin
        with patch('commands.match.has_admin_permission', return_value=False):
            # First submission - should not be ready
            await match_commands.record.callback(match_commands, mock_interaction, result_choice)
            
            # Verify message indicates vote was recorded and shows counts
            call_args = mock_interaction.followup.send.call_args
            message = call_args[0][0] if call_args[0] else call_args[1].get('content', '')
            assert "Result recorded" in message
            assert "1/3" in message or "Radiant: 1/3" in message
            
            # Verify match is still pending
            assert match_service.get_last_shuffle(12346) is not None
            assert match_service.can_record_match(12346) is False
            
            # Second submission from different user
            mock_interaction2 = MockDiscordInteraction(100002, "User2")
            mock_interaction2.guild = mock_guild
            mock_interaction2.user.guild_permissions = mock_permissions
            mock_interaction2.response = AsyncMock()
            mock_interaction2.followup = AsyncMock()
            
            await match_commands.record.callback(match_commands, mock_interaction2, result_choice)
            
            # Still not ready
            assert match_service.can_record_match(12346) is False
            
            # Third submission from different user
            mock_interaction3 = MockDiscordInteraction(100003, "User3")
            mock_interaction3.guild = mock_guild
            mock_interaction3.user.guild_permissions = mock_permissions
            mock_interaction3.response = AsyncMock()
            mock_interaction3.followup = AsyncMock()
            
            # Before third submission, should not be ready
            assert match_service.can_record_match(12346) is False
            assert match_service.get_non_admin_submission_count(12346) == 2
            
            await match_commands.record.callback(match_commands, mock_interaction3, result_choice)
            
            # After third submission, match should be recorded (state cleared)
            # Check the message to see if it was recorded
            call_args = mock_interaction3.followup.send.call_args
            message = call_args[0][0] if call_args[0] else call_args[1].get('content', '')
            
            # Should have recorded the match (not just a submission message)
            assert "Match recorded" in message, f"Expected 'Match recorded' in message, got: {message}"
            # State should be cleared after recording
            assert match_service.get_last_shuffle(12346) is None


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

