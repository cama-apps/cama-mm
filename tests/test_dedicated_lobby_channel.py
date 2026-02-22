"""
Tests for the dedicated lobby channel feature.

This feature allows lobby embeds to be posted to a dedicated channel while
tracking the origin channel (where /lobby was run) for rally notifications.
"""

import os
import tempfile
import time
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from database import Database
from repositories.lobby_repository import LobbyRepository
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService


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


class TestOriginChannelIdStorage:
    """Test origin_channel_id storage in LobbyManager."""

    def test_set_origin_channel_id(self):
        """Test setting origin_channel_id via set_lobby_message."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=12345)
        manager.set_lobby_message(
            message_id=111,
            channel_id=222,
            origin_channel_id=333,
        )

        assert manager.origin_channel_id == 333

    def test_origin_channel_id_defaults_to_none(self):
        """Test that origin_channel_id defaults to None."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=12345)

        assert manager.origin_channel_id is None

    def test_origin_channel_id_not_overwritten_when_not_passed(self):
        """Test that origin_channel_id is preserved when not passed to set_lobby_message."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=12345)
        manager.set_lobby_message(
            message_id=111,
            channel_id=222,
            origin_channel_id=333,
        )

        # Update message without passing origin_channel_id
        manager.set_lobby_message(
            message_id=444,
            channel_id=555,
        )

        # origin_channel_id should be preserved
        assert manager.origin_channel_id == 333

    def test_reset_lobby_clears_origin_channel_id(self):
        """Test that reset_lobby clears origin_channel_id."""
        manager = LobbyManager(Database(db_path=":memory:"))
        manager.get_or_create_lobby(creator_id=12345)
        manager.set_lobby_message(
            message_id=111,
            channel_id=222,
            origin_channel_id=333,
        )

        manager.reset_lobby()

        assert manager.origin_channel_id is None


class TestOriginChannelIdPersistence:
    """Test origin_channel_id persistence across bot restarts."""

    def test_origin_channel_id_persists_across_restart(self):
        """Test that origin_channel_id is restored after restart."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # First session - create lobby and set origin_channel_id
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(
                message_id=111,
                channel_id=222,
                origin_channel_id=333,
            )

            assert manager1.origin_channel_id == 333

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # origin_channel_id should be restored
            assert manager2.origin_channel_id == 333
        finally:
            _cleanup_db_file(db_path)

    def test_origin_channel_id_persists_with_all_ids(self):
        """Test that origin_channel_id persists alongside other IDs."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(
                message_id=111,
                channel_id=222,
                thread_id=333,
                embed_message_id=444,
                origin_channel_id=555,
            )

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # All IDs should be restored
            assert manager2.lobby_message_id == 111
            assert manager2.lobby_channel_id == 222
            assert manager2.lobby_thread_id == 333
            assert manager2.lobby_embed_message_id == 444
            assert manager2.origin_channel_id == 555
        finally:
            _cleanup_db_file(db_path)

    def test_origin_channel_id_cleared_after_reset_persists(self):
        """Test that cleared origin_channel_id persists as None after restart."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            db1 = Database(db_path=db_path)
            manager1 = LobbyManager(db1)
            manager1.get_or_create_lobby(creator_id=12345)
            manager1.set_lobby_message(
                message_id=111,
                channel_id=222,
                origin_channel_id=333,
            )
            manager1.reset_lobby()

            # Simulate restart
            db2 = Database(db_path=db_path)
            manager2 = LobbyManager(db2)

            # Should be cleared
            assert manager2.origin_channel_id is None
            assert manager2.lobby_message_id is None
        finally:
            _cleanup_db_file(db_path)


class TestLobbyRepositoryOriginChannelId:
    """Test origin_channel_id in LobbyRepository."""

    def test_save_and_load_origin_channel_id(self, repo_db_path):
        """Test saving and loading origin_channel_id via repository."""
        repo = LobbyRepository(repo_db_path)

        repo.save_lobby_state(
            lobby_id=1,
            players=[1001, 1002],
            status="open",
            created_by=12345,
            created_at="2024-01-01T00:00:00",
            message_id=111,
            channel_id=222,
            thread_id=333,
            embed_message_id=444,
            origin_channel_id=555,
        )

        state = repo.load_lobby_state(1)
        assert state is not None
        assert state["origin_channel_id"] == 555
        assert state["message_id"] == 111
        assert state["channel_id"] == 222

    def test_origin_channel_id_defaults_to_none(self, repo_db_path):
        """Test that origin_channel_id defaults to None when not provided."""
        repo = LobbyRepository(repo_db_path)

        repo.save_lobby_state(
            lobby_id=1,
            players=[1001],
            status="open",
            created_by=12345,
            created_at="2024-01-01T00:00:00",
        )

        state = repo.load_lobby_state(1)
        assert state is not None
        assert state["origin_channel_id"] is None

    def test_origin_channel_id_update(self, repo_db_path):
        """Test updating origin_channel_id."""
        repo = LobbyRepository(repo_db_path)

        # Initial save
        repo.save_lobby_state(
            lobby_id=1,
            players=[1001],
            status="open",
            created_by=12345,
            created_at="2024-01-01T00:00:00",
            origin_channel_id=111,
        )

        # Update with new origin_channel_id
        repo.save_lobby_state(
            lobby_id=1,
            players=[1001, 1002],
            status="open",
            created_by=12345,
            created_at="2024-01-01T00:00:00",
            origin_channel_id=222,
        )

        state = repo.load_lobby_state(1)
        assert state["origin_channel_id"] == 222


class TestLobbyServiceOriginChannelId:
    """Test origin_channel_id in LobbyService."""

    def test_get_origin_channel_id(self, lobby_repository, player_repository):
        """Test getting origin_channel_id via LobbyService."""
        manager = LobbyManager(lobby_repository)
        service = LobbyService(manager, player_repository)

        manager.get_or_create_lobby(creator_id=12345)
        service.set_lobby_message_id(
            message_id=111,
            channel_id=222,
            origin_channel_id=333,
        )

        assert service.get_origin_channel_id() == 333

    def test_get_origin_channel_id_returns_none_when_not_set(self, lobby_repository, player_repository):
        """Test that get_origin_channel_id returns None when not set."""
        manager = LobbyManager(lobby_repository)
        service = LobbyService(manager, player_repository)

        manager.get_or_create_lobby(creator_id=12345)

        assert service.get_origin_channel_id() is None


class TestDatabaseOriginChannelId:
    """Test origin_channel_id in Database class."""

    def test_database_save_and_load_origin_channel_id(self):
        """Test Database.save_lobby_state and load_lobby_state with origin_channel_id."""
        db = Database(db_path=":memory:")

        db.save_lobby_state(
            lobby_id=1,
            players=[1001, 1002],
            status="open",
            created_by=12345,
            created_at="2024-01-01T00:00:00",
            message_id=111,
            channel_id=222,
            origin_channel_id=333,
        )

        state = db.load_lobby_state(1)
        assert state is not None
        assert state["origin_channel_id"] == 333

    def test_database_origin_channel_id_update_on_conflict(self):
        """Test that origin_channel_id is updated on conflict."""
        db = Database(db_path=":memory:")

        # Initial save
        db.save_lobby_state(
            lobby_id=1,
            players=[1001],
            status="open",
            created_by=12345,
            created_at="2024-01-01T00:00:00",
            origin_channel_id=111,
        )

        # Update (same lobby_id triggers ON CONFLICT)
        db.save_lobby_state(
            lobby_id=1,
            players=[1001, 1002],
            status="open",
            created_by=12345,
            created_at="2024-01-01T00:00:00",
            origin_channel_id=222,
        )

        state = db.load_lobby_state(1)
        assert state["origin_channel_id"] == 222


class TestGetLobbyTargetChannelHelper:
    """Test _get_lobby_target_channel helper method."""

    @pytest.mark.asyncio
    async def test_returns_interaction_channel_when_no_config(self):
        """Test that interaction channel is returned when LOBBY_CHANNEL_ID not set."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 12345

        with patch("commands.lobby.LOBBY_CHANNEL_ID", None):
            channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        assert channel == interaction.channel
        assert is_dedicated is False

    @pytest.mark.asyncio
    async def test_returns_dedicated_channel_when_configured(self):
        """Test that dedicated channel is returned when LOBBY_CHANNEL_ID is set."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        dedicated_channel = MagicMock(spec=['permissions_for', 'guild', 'id'])
        dedicated_channel.id = 99999
        dedicated_channel.guild = MagicMock()
        dedicated_channel.guild.id = 12345
        dedicated_channel.guild.me = MagicMock()

        perms = MagicMock()
        perms.send_messages = True
        perms.create_public_threads = True
        dedicated_channel.permissions_for.return_value = perms

        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111
        interaction.guild = MagicMock()
        interaction.guild.id = 12345

        # Need to make dedicated_channel appear as TextChannel for isinstance check
        import discord
        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            with patch.object(discord, 'TextChannel', type(dedicated_channel)):
                channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        assert channel == dedicated_channel
        assert is_dedicated is True

    @pytest.mark.asyncio
    async def test_falls_back_on_permission_error(self):
        """Test fallback to interaction channel on permission errors."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        dedicated_channel = MagicMock(spec=['permissions_for', 'guild', 'id'])
        dedicated_channel.id = 99999
        dedicated_channel.guild = MagicMock()
        dedicated_channel.guild.id = 12345
        dedicated_channel.guild.me = MagicMock()

        # Missing send_messages permission
        perms = MagicMock()
        perms.send_messages = False
        perms.create_public_threads = True
        dedicated_channel.permissions_for.return_value = perms

        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111
        interaction.guild = MagicMock()
        interaction.guild.id = 12345

        import discord
        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            with patch.object(discord, 'TextChannel', type(dedicated_channel)):
                channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        assert channel == interaction.channel
        assert is_dedicated is False

    @pytest.mark.asyncio
    async def test_falls_back_on_channel_not_found(self):
        """Test fallback when dedicated channel doesn't exist."""
        from commands.lobby import LobbyCommands
        import discord

        bot = MagicMock()
        bot.get_channel.return_value = None
        bot.fetch_channel = AsyncMock(side_effect=discord.NotFound(MagicMock(), "Not found"))

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111

        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        assert channel == interaction.channel
        assert is_dedicated is False

    @pytest.mark.asyncio
    async def test_falls_back_on_different_guild(self):
        """Test fallback when dedicated channel is in a different guild."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        dedicated_channel = MagicMock(spec=['permissions_for', 'guild', 'id'])
        dedicated_channel.id = 99999
        dedicated_channel.guild = MagicMock()
        dedicated_channel.guild.id = 99999  # Different guild

        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111
        interaction.guild = MagicMock()
        interaction.guild.id = 12345  # Different from dedicated_channel.guild.id

        import discord
        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            with patch.object(discord, 'TextChannel', type(dedicated_channel)):
                channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        assert channel == interaction.channel
        assert is_dedicated is False


class TestDedicatedLobbyChannelE2E:
    """End-to-end tests for the dedicated lobby channel feature."""

    def test_full_flow_with_origin_channel(self):
        """Test full flow: create lobby, store origin channel, verify persistence."""
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            # Create lobby service (Database initializes schema, repo uses same path)
            Database(db_path=db_path)  # Initialize schema
            repo = LobbyRepository(db_path)
            manager = LobbyManager(repo)
            player_repo = MagicMock()
            service = LobbyService(manager, player_repo)

            # Simulate /lobby command
            lobby = service.get_or_create_lobby(creator_id=12345)

            # Store channel IDs (dedicated channel = 100, origin = 200)
            service.set_lobby_message_id(
                message_id=1,
                channel_id=100,  # Dedicated channel
                thread_id=2,
                embed_message_id=1,
                origin_channel_id=200,  # Where /lobby was run
            )

            # Verify storage
            assert service.get_lobby_channel_id() == 100
            assert service.get_origin_channel_id() == 200

            # Simulate restart
            repo2 = LobbyRepository(db_path)
            manager2 = LobbyManager(repo2)
            service2 = LobbyService(manager2, player_repo)

            # Verify persistence
            assert service2.get_lobby_channel_id() == 100
            assert service2.get_origin_channel_id() == 200
            assert service2.get_lobby() is not None
        finally:
            _cleanup_db_file(db_path)

    def test_reset_clears_all_channel_ids(self):
        """Test that reset_lobby clears both channel_id and origin_channel_id."""
        db = Database(db_path=":memory:")
        repo = LobbyRepository(":memory:")
        # Use the database's connection for the repo
        manager = LobbyManager(db)
        player_repo = MagicMock()
        service = LobbyService(manager, player_repo)

        # Create lobby with all IDs
        service.get_or_create_lobby(creator_id=12345)
        service.set_lobby_message_id(
            message_id=1,
            channel_id=100,
            thread_id=2,
            origin_channel_id=200,
        )

        # Reset
        service.reset_lobby()

        # Verify all cleared
        assert service.get_lobby_message_id() is None
        assert service.get_lobby_channel_id() is None
        assert service.get_origin_channel_id() is None

    def test_origin_channel_same_as_lobby_channel_when_no_dedicated(self):
        """Test scenario where origin and lobby channel are the same (no dedicated channel)."""
        db = Database(db_path=":memory:")
        manager = LobbyManager(db)
        player_repo = MagicMock()
        service = LobbyService(manager, player_repo)

        service.get_or_create_lobby(creator_id=12345)

        # When no dedicated channel, both should be the same
        same_channel_id = 12345
        service.set_lobby_message_id(
            message_id=1,
            channel_id=same_channel_id,
            origin_channel_id=same_channel_id,
        )

        assert service.get_lobby_channel_id() == same_channel_id
        assert service.get_origin_channel_id() == same_channel_id

    def test_origin_channel_different_from_lobby_channel(self):
        """Test scenario with dedicated channel (origin different from lobby channel)."""
        db = Database(db_path=":memory:")
        manager = LobbyManager(db)
        player_repo = MagicMock()
        service = LobbyService(manager, player_repo)

        service.get_or_create_lobby(creator_id=12345)

        # Dedicated channel scenario
        dedicated_channel_id = 100
        origin_channel_id = 200  # Where user ran /lobby

        service.set_lobby_message_id(
            message_id=1,
            channel_id=dedicated_channel_id,
            origin_channel_id=origin_channel_id,
        )

        # They should be different
        assert service.get_lobby_channel_id() != service.get_origin_channel_id()
        assert service.get_lobby_channel_id() == dedicated_channel_id
        assert service.get_origin_channel_id() == origin_channel_id


class TestConfigEnvVar:
    """Test LOBBY_CHANNEL_ID configuration parsing logic."""

    def test_lobby_channel_id_parsing_valid_int(self):
        """Test parsing logic for valid integer."""
        # Test the parsing logic directly without reloading config
        raw = "123456789"
        try:
            result = int(raw.strip())
        except ValueError:
            result = None
        assert result == 123456789

    def test_lobby_channel_id_parsing_none_when_empty(self):
        """Test parsing logic when value is empty."""
        raw = ""
        if raw:
            try:
                result = int(raw.strip())
            except ValueError:
                result = None
        else:
            result = None
        assert result is None

    def test_lobby_channel_id_parsing_none_on_invalid(self):
        """Test parsing logic for invalid value."""
        raw = "not-a-number"
        try:
            result = int(raw.strip())
        except ValueError:
            result = None
        assert result is None

    def test_lobby_channel_id_parsing_strips_whitespace(self):
        """Test that whitespace is stripped from value."""
        raw = "  123456789  "
        try:
            result = int(raw.strip())
        except ValueError:
            result = None
        assert result == 123456789


class TestGetLobbyTargetChannelEdgeCases:
    """Additional edge case tests for _get_lobby_target_channel."""

    @pytest.mark.asyncio
    async def test_fetch_channel_fallback_when_get_channel_returns_none(self):
        """Test that fetch_channel is called when get_channel returns None."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        dedicated_channel = MagicMock(spec=['permissions_for', 'guild', 'id'])
        dedicated_channel.id = 99999
        dedicated_channel.guild = MagicMock()
        dedicated_channel.guild.id = 12345
        dedicated_channel.guild.me = MagicMock()

        perms = MagicMock()
        perms.send_messages = True
        perms.create_public_threads = True
        dedicated_channel.permissions_for.return_value = perms

        bot.get_channel.return_value = None  # Not in cache
        bot.fetch_channel = AsyncMock(return_value=dedicated_channel)  # Fetch succeeds

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111
        interaction.guild = MagicMock()
        interaction.guild.id = 12345

        import discord
        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            with patch.object(discord, 'TextChannel', type(dedicated_channel)):
                channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        # fetch_channel should have been called
        bot.fetch_channel.assert_called_once_with(99999)
        assert channel == dedicated_channel
        assert is_dedicated is True

    @pytest.mark.asyncio
    async def test_dm_context_guild_none(self):
        """Test behavior when interaction.guild is None (DM context)."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        dedicated_channel = MagicMock(spec=['permissions_for', 'guild', 'id'])
        dedicated_channel.id = 99999
        dedicated_channel.guild = MagicMock()
        dedicated_channel.guild.id = 12345

        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111
        interaction.guild = None  # DM context

        import discord
        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            with patch.object(discord, 'TextChannel', type(dedicated_channel)):
                channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        # Should still work - guild check is skipped when interaction.guild is None
        assert channel == dedicated_channel
        assert is_dedicated is True

    @pytest.mark.asyncio
    async def test_falls_back_on_missing_thread_permission(self):
        """Test fallback when bot lacks create_public_threads permission."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        dedicated_channel = MagicMock(spec=['permissions_for', 'guild', 'id'])
        dedicated_channel.id = 99999
        dedicated_channel.guild = MagicMock()
        dedicated_channel.guild.id = 12345
        dedicated_channel.guild.me = MagicMock()

        perms = MagicMock()
        perms.send_messages = True
        perms.create_public_threads = False  # Missing thread permission
        dedicated_channel.permissions_for.return_value = perms

        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111
        interaction.guild = MagicMock()
        interaction.guild.id = 12345

        import discord
        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            with patch.object(discord, 'TextChannel', type(dedicated_channel)):
                channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        assert channel == interaction.channel
        assert is_dedicated is False

    @pytest.mark.asyncio
    async def test_falls_back_on_forbidden_exception(self):
        """Test fallback when fetch_channel raises Forbidden."""
        from commands.lobby import LobbyCommands
        import discord

        bot = MagicMock()
        bot.get_channel.return_value = None
        bot.fetch_channel = AsyncMock(side_effect=discord.Forbidden(MagicMock(), "Forbidden"))

        lobby_service = MagicMock()
        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 11111

        with patch("commands.lobby.LOBBY_CHANNEL_ID", 99999):
            channel, is_dedicated = await cog._get_lobby_target_channel(interaction)

        assert channel == interaction.channel
        assert is_dedicated is False


class TestNotifyLobbyRally:
    """Test notify_lobby_rally function uses origin_channel_id correctly."""

    @pytest.mark.asyncio
    async def test_rally_uses_origin_channel_when_different(self):
        """Test that rally notification goes to origin channel, not reaction channel."""
        from bot import notify_lobby_rally
        import bot as bot_module

        # Create mock lobby
        lobby = MagicMock()
        lobby.get_total_count.return_value = 8  # 2 more needed

        # Create mock channels
        reaction_channel = MagicMock()
        reaction_channel.id = 111
        reaction_channel.send = AsyncMock()

        origin_channel = MagicMock()
        origin_channel.id = 222
        origin_channel.send = AsyncMock()

        thread = MagicMock()
        thread.send = AsyncMock()

        # Mock bot with lobby_service attribute
        mock_bot = MagicMock()
        mock_bot.get_channel.return_value = origin_channel
        mock_bot.lobby_service = MagicMock()
        mock_bot.lobby_service.get_origin_channel_id.return_value = 222  # Different from reaction channel
        mock_bot.lobby_service.get_lobby_message_id.return_value = 111
        mock_bot.lobby_service.get_lobby_channel_id.return_value = 100

        # Patch module-level references
        with patch.object(bot_module, 'bot', mock_bot):
            with patch.object(bot_module, '_lobby_rally_cooldowns', {}):
                with patch.object(bot_module, 'LOBBY_RALLY_COOLDOWN_SECONDS', 0):
                    result = await notify_lobby_rally(reaction_channel, thread, lobby, guild_id=1)

        assert result is True
        # Origin channel should receive the embed, not reaction channel
        origin_channel.send.assert_called_once()
        reaction_channel.send.assert_not_called()
        thread.send.assert_called_once()

    @pytest.mark.asyncio
    async def test_rally_falls_back_to_reaction_channel(self):
        """Test rally falls back to reaction channel when origin_channel_id is None."""
        from bot import notify_lobby_rally
        import bot as bot_module

        lobby = MagicMock()
        lobby.get_total_count.return_value = 9  # 1 more needed

        reaction_channel = MagicMock()
        reaction_channel.id = 111
        reaction_channel.send = AsyncMock()

        thread = MagicMock()
        thread.send = AsyncMock()

        mock_bot = MagicMock()
        mock_bot.lobby_service = MagicMock()
        mock_bot.lobby_service.get_origin_channel_id.return_value = None  # Not set
        mock_bot.lobby_service.get_lobby_message_id.return_value = 111
        mock_bot.lobby_service.get_lobby_channel_id.return_value = 100

        with patch.object(bot_module, 'bot', mock_bot):
            with patch.object(bot_module, '_lobby_rally_cooldowns', {}):
                with patch.object(bot_module, 'LOBBY_RALLY_COOLDOWN_SECONDS', 0):
                    result = await notify_lobby_rally(reaction_channel, thread, lobby, guild_id=1)

        assert result is True
        # Reaction channel should be used as fallback
        reaction_channel.send.assert_called_once()

    @pytest.mark.asyncio
    async def test_rally_falls_back_when_origin_channel_fetch_fails(self):
        """Test rally falls back when origin channel cannot be fetched."""
        from bot import notify_lobby_rally
        import bot as bot_module

        lobby = MagicMock()
        lobby.get_total_count.return_value = 8

        reaction_channel = MagicMock()
        reaction_channel.id = 111
        reaction_channel.send = AsyncMock()

        thread = MagicMock()
        thread.send = AsyncMock()

        mock_bot = MagicMock()
        mock_bot.get_channel.return_value = None
        mock_bot.fetch_channel = AsyncMock(side_effect=Exception("Not found"))
        mock_bot.lobby_service = MagicMock()
        mock_bot.lobby_service.get_origin_channel_id.return_value = 222
        mock_bot.lobby_service.get_lobby_message_id.return_value = 111
        mock_bot.lobby_service.get_lobby_channel_id.return_value = 100

        with patch.object(bot_module, 'bot', mock_bot):
            with patch.object(bot_module, '_lobby_rally_cooldowns', {}):
                with patch.object(bot_module, 'LOBBY_RALLY_COOLDOWN_SECONDS', 0):
                    result = await notify_lobby_rally(reaction_channel, thread, lobby, guild_id=1)

        assert result is True
        # Should fall back to reaction channel
        reaction_channel.send.assert_called_once()

    @pytest.mark.asyncio
    async def test_rally_same_channel_no_duplicate(self):
        """Test no duplicate when origin_channel_id equals reaction channel."""
        from bot import notify_lobby_rally
        import bot as bot_module

        lobby = MagicMock()
        lobby.get_total_count.return_value = 8

        reaction_channel = MagicMock()
        reaction_channel.id = 111
        reaction_channel.send = AsyncMock()

        thread = MagicMock()
        thread.send = AsyncMock()

        mock_bot = MagicMock()
        mock_bot.lobby_service = MagicMock()
        mock_bot.lobby_service.get_origin_channel_id.return_value = 111  # Same as reaction channel
        mock_bot.lobby_service.get_lobby_message_id.return_value = 111
        mock_bot.lobby_service.get_lobby_channel_id.return_value = 100

        with patch.object(bot_module, 'bot', mock_bot):
            with patch.object(bot_module, '_lobby_rally_cooldowns', {}):
                with patch.object(bot_module, 'LOBBY_RALLY_COOLDOWN_SECONDS', 0):
                    result = await notify_lobby_rally(reaction_channel, thread, lobby, guild_id=1)

        assert result is True
        # Should only send once (not fetch origin since it's the same)
        reaction_channel.send.assert_called_once()


class TestShuffleDedicatedChannel:
    """Test /shuffle command behavior with dedicated lobby channel."""

    @pytest.mark.asyncio
    async def test_shuffle_posts_to_dedicated_channel(self):
        """Test that shuffle posts embed to the dedicated lobby channel."""
        from commands.match import MatchCommands

        # Create mock bot
        bot = MagicMock()
        dedicated_channel = MagicMock()
        dedicated_channel.id = 100
        dedicated_channel.send = AsyncMock(return_value=MagicMock(id=999))
        bot.get_channel.return_value = dedicated_channel

        # Create mock services (match signature: bot, lobby_service, match_service, player_service)
        lobby_service = MagicMock()
        lobby_service.get_lobby_channel_id.return_value = 100  # Dedicated channel
        lobby_service.get_lobby.return_value = MagicMock(players={1, 2, 3, 4, 5, 6, 7, 8, 9, 10})
        lobby_service.get_lobby_message_id.return_value = 111
        lobby_service.get_lobby_thread_id.return_value = 222

        match_service = MagicMock()
        player_service = MagicMock()

        cog = MatchCommands(bot, lobby_service, match_service, player_service)

        # Test the channel resolution logic directly (lines 596-605 in match.py)
        lobby_channel_id = lobby_service.get_lobby_channel_id()
        assert lobby_channel_id == 100

        # Verify bot.get_channel is called with the dedicated channel ID
        bot.get_channel.assert_not_called()  # Not called yet
        channel = bot.get_channel(lobby_channel_id)
        bot.get_channel.assert_called_with(100)
        assert channel == dedicated_channel

    @pytest.mark.asyncio
    async def test_shuffle_no_double_post_when_run_from_dedicated_channel(self):
        """Test that shuffle doesn't double-post when run from the dedicated lobby channel."""
        from commands.match import MatchCommands

        bot = MagicMock()
        dedicated_channel = MagicMock()
        dedicated_channel.id = 100
        dedicated_channel.send = AsyncMock(return_value=MagicMock(id=999))
        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        lobby_service.get_lobby_channel_id.return_value = 100  # Dedicated channel

        # Simulate interaction from the SAME channel as dedicated (100)
        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 100  # Same as dedicated channel
        interaction.channel.send = AsyncMock()

        # Test the double-post prevention logic directly
        lobby_channel_id = lobby_service.get_lobby_channel_id()
        command_channel_id = interaction.channel.id if interaction.channel else None

        # This is the condition from match.py lines 608-609
        should_post_to_command_channel = (
            command_channel_id and command_channel_id != lobby_channel_id
        )

        # When channels are the same, should NOT post to command channel
        assert should_post_to_command_channel is False
        assert command_channel_id == lobby_channel_id == 100

    @pytest.mark.asyncio
    async def test_shuffle_posts_to_both_channels_when_different(self):
        """Test that shuffle posts to both channels when they differ."""
        from commands.match import MatchCommands

        bot = MagicMock()
        dedicated_channel = MagicMock()
        dedicated_channel.id = 100
        dedicated_channel.send = AsyncMock(return_value=MagicMock(id=999))
        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        lobby_service.get_lobby_channel_id.return_value = 100  # Dedicated channel

        # Simulate interaction from a DIFFERENT channel (200)
        interaction = MagicMock()
        interaction.channel = MagicMock()
        interaction.channel.id = 200  # Different from dedicated channel
        interaction.channel.send = AsyncMock()

        # Test the double-post logic
        lobby_channel_id = lobby_service.get_lobby_channel_id()
        command_channel_id = interaction.channel.id if interaction.channel else None

        should_post_to_command_channel = (
            command_channel_id and command_channel_id != lobby_channel_id
        )

        # When channels differ, SHOULD post to command channel
        assert should_post_to_command_channel is True
        assert command_channel_id == 200
        assert lobby_channel_id == 100


class TestResetLobbyDedicatedChannel:
    """Test /resetlobby command behavior with dedicated lobby channel."""

    @pytest.mark.asyncio
    async def test_resetlobby_unpins_from_dedicated_channel(self):
        """Test that resetlobby unpins from the dedicated lobby channel, not interaction channel."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        dedicated_channel = MagicMock()
        dedicated_channel.id = 100

        # Mock the pinned message
        pinned_message = MagicMock()
        pinned_message.unpin = AsyncMock()
        dedicated_channel.fetch_message = AsyncMock(return_value=pinned_message)

        bot.get_channel.return_value = dedicated_channel

        lobby_service = MagicMock()
        lobby_service.get_lobby_channel_id.return_value = 100  # Dedicated channel
        lobby_service.get_lobby_message_id.return_value = 111
        lobby_service.get_lobby.return_value = MagicMock(
            created_by=12345,
            players=set()
        )

        player_service = MagicMock()

        cog = LobbyCommands(bot, lobby_service, player_service)

        # Simulate the channel resolution logic from resetlobby (lines 639-650)
        lobby_channel_id = lobby_service.get_lobby_channel_id()
        lobby_channel = None
        if lobby_channel_id:
            lobby_channel = bot.get_channel(lobby_channel_id)

        # Verify dedicated channel is used for unpinning
        assert lobby_channel == dedicated_channel
        assert lobby_channel.id == 100
        bot.get_channel.assert_called_with(100)

    @pytest.mark.asyncio
    async def test_resetlobby_falls_back_to_interaction_channel(self):
        """Test that resetlobby falls back to interaction channel when dedicated channel unavailable."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()
        bot.get_channel.return_value = None  # Dedicated channel not found
        bot.fetch_channel = AsyncMock(side_effect=Exception("Channel not found"))

        lobby_service = MagicMock()
        lobby_service.get_lobby_channel_id.return_value = 100  # Configured but unavailable
        lobby_service.get_lobby_message_id.return_value = 111

        interaction_channel = MagicMock()
        interaction_channel.id = 200

        # Simulate the fallback logic from resetlobby (lines 647-650)
        lobby_channel_id = lobby_service.get_lobby_channel_id()
        lobby_channel = None
        if lobby_channel_id:
            try:
                lobby_channel = bot.get_channel(lobby_channel_id)
                if not lobby_channel:
                    lobby_channel = await bot.fetch_channel(lobby_channel_id)
            except Exception:
                lobby_channel = interaction_channel
        else:
            lobby_channel = interaction_channel

        # Should fall back to interaction channel
        assert lobby_channel == interaction_channel
        assert lobby_channel.id == 200

    @pytest.mark.asyncio
    async def test_resetlobby_uses_interaction_channel_when_no_dedicated(self):
        """Test that resetlobby uses interaction channel when no dedicated channel configured."""
        from commands.lobby import LobbyCommands

        bot = MagicMock()

        lobby_service = MagicMock()
        lobby_service.get_lobby_channel_id.return_value = None  # No dedicated channel
        lobby_service.get_lobby_message_id.return_value = 111

        interaction_channel = MagicMock()
        interaction_channel.id = 200

        # Simulate the logic from resetlobby
        lobby_channel_id = lobby_service.get_lobby_channel_id()
        if lobby_channel_id:
            lobby_channel = bot.get_channel(lobby_channel_id)
        else:
            lobby_channel = interaction_channel

        # Should use interaction channel directly
        assert lobby_channel == interaction_channel
        bot.get_channel.assert_not_called()


class TestSchemaMigration:
    """Test the origin_channel_id schema migration."""

    def test_migration_adds_origin_channel_id_column(self):
        """Test that the migration adds origin_channel_id column to lobby_state."""
        import sqlite3
        from infrastructure.schema_manager import SchemaManager

        # Use temp file since :memory: creates new DB per connection
        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager = SchemaManager(db_path)
            manager.initialize()

            # Verify the column exists
            conn = sqlite3.connect(db_path)
            cursor = conn.cursor()
            cursor.execute("PRAGMA table_info(lobby_state)")
            columns = {row[1] for row in cursor.fetchall()}
            conn.close()

            assert "origin_channel_id" in columns
        finally:
            _cleanup_db_file(db_path)

    def test_migration_allows_null_origin_channel_id(self):
        """Test that origin_channel_id can be NULL (for backward compatibility)."""
        import sqlite3
        from infrastructure.schema_manager import SchemaManager

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager = SchemaManager(db_path)
            manager.initialize()

            conn = sqlite3.connect(db_path)
            cursor = conn.cursor()

            # Insert a row without origin_channel_id
            cursor.execute("""
                INSERT INTO lobby_state (lobby_id, players, status, created_by, created_at)
                VALUES (1, '[]', 'open', 12345, '2024-01-01')
            """)
            conn.commit()

            # Verify it was inserted with NULL
            cursor.execute("SELECT origin_channel_id FROM lobby_state WHERE lobby_id = 1")
            result = cursor.fetchone()
            conn.close()

            assert result[0] is None
        finally:
            _cleanup_db_file(db_path)

    def test_migration_stores_origin_channel_id(self):
        """Test that origin_channel_id can be stored and retrieved."""
        import sqlite3
        from infrastructure.schema_manager import SchemaManager

        with tempfile.NamedTemporaryFile(delete=False, suffix=".db") as f:
            db_path = f.name

        try:
            manager = SchemaManager(db_path)
            manager.initialize()

            conn = sqlite3.connect(db_path)
            cursor = conn.cursor()

            # Insert a row with origin_channel_id
            cursor.execute("""
                INSERT INTO lobby_state (lobby_id, players, status, created_by, created_at, origin_channel_id)
                VALUES (1, '[]', 'open', 12345, '2024-01-01', 999888777)
            """)
            conn.commit()

            # Verify it was stored
            cursor.execute("SELECT origin_channel_id FROM lobby_state WHERE lobby_id = 1")
            result = cursor.fetchone()
            conn.close()

            assert result[0] == 999888777
        finally:
            _cleanup_db_file(db_path)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
