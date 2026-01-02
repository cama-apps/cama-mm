"""
Tests for bot initialization and basic setup.
These tests verify that the bot can be imported and configured without connecting to Discord.
"""

import pytest
import sys
import os
import asyncio


def test_imports():
    """Test that all required modules can be imported."""
    # Test core modules
    from domain.models.player import Player
    from domain.models.team import Team
    from shuffler import BalancedShuffler
    from database import Database
    from domain.models.lobby import LobbyManager
    
    assert Player is not None
    assert Team is not None
    assert BalancedShuffler is not None
    assert Database is not None
    assert LobbyManager is not None


def test_bot_import():
    """Test that bot module can be imported (without running it)."""
    # This will import the bot but not run it
    # We need to mock the Discord token to prevent connection attempts
    import bot
    
    # Verify bot object exists
    assert hasattr(bot, 'bot')
    assert hasattr(bot, 'db')
    assert hasattr(bot, 'lobby_manager')


def test_bot_commands_registered():
    """Test that bot commands are registered in the command tree."""
    import bot
    
    # Ensure extensions are loaded so commands are registered
    asyncio.run(bot._load_extensions())
    
    # Get all registered commands
    commands = bot.bot.tree.get_commands()
    command_names = [cmd.name for cmd in commands]
    
    # Verify key commands exist
    expected_commands = [
        'register',
        'setroles',
        'lobby',
        'shuffle',
        'record',
        'stats',
        'leaderboard',
        'help',
        'resetuser'  # Note: 'reset' was renamed to 'resetuser' (admin only)
    ]
    
    for cmd_name in expected_commands:
        assert cmd_name in command_names, f"Command '{cmd_name}' not found in registered commands"


def test_role_configuration():
    """Test that role emojis and names are configured."""
    import bot
    
    assert hasattr(bot, 'ROLE_EMOJIS')
    assert hasattr(bot, 'ROLE_NAMES')
    assert len(bot.ROLE_EMOJIS) == 5
    assert len(bot.ROLE_NAMES) == 5
    
    # Verify all roles 1-5 are present
    for role in ['1', '2', '3', '4', '5']:
        assert role in bot.ROLE_EMOJIS
        assert role in bot.ROLE_NAMES


def test_format_role_display():
    """Test the format_role_display helper function."""
    import bot
    
    # Test formatting for each role
    # Note: format_role_display no longer includes the role number, just emoji and name
    for role in ['1', '2', '3', '4', '5']:
        formatted = bot.format_role_display(role)
        # Should contain the role name and emoji, but not the number
        assert bot.ROLE_NAMES[role] in formatted
        assert bot.ROLE_EMOJIS[role] in formatted


def test_admin_configuration():
    """Test that admin configuration exists."""
    import bot
    
    assert hasattr(bot, 'ADMIN_USER_IDS')
    assert isinstance(bot.ADMIN_USER_IDS, list)
    assert hasattr(bot, 'has_admin_permission')


if __name__ == "__main__":
    pytest.main([__file__, "-v"])

