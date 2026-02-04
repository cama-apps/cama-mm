"""
Tests for lobby commands to ensure guild_id is properly handled.

These tests verify that guild_id is defined before use in all command handlers,
catching UnboundLocalError issues that occur when guild_id is used before being
extracted from interaction.guild.id.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.lobby import LobbyCommands
from database import Database
from domain.models.player import Player
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService

TEST_GUILD_ID = 12345


class FakeGuild:
    """Fake Discord guild with an id."""

    def __init__(self, guild_id=TEST_GUILD_ID):
        self.id = guild_id


class FakeFollowup:
    """Capture followup messages."""

    def __init__(self):
        self.messages = []

    async def send(self, content=None, ephemeral=None, embed=None, allowed_mentions=None):
        self.messages.append({
            "content": content,
            "ephemeral": ephemeral,
            "embed": embed,
        })


class FakeMessage:
    """Fake Discord message."""

    def __init__(self):
        self.edits = []
        self.jump_url = "https://discord.com/channels/123/456/789"
        self.id = 789

    async def edit(self, embed=None, allowed_mentions=None, content=None):
        self.edits.append({"embed": embed, "content": content})

    async def remove_reaction(self, emoji, user):
        pass

    async def pin(self):
        pass

    async def create_thread(self, name=None, auto_archive_duration=None):
        return FakeThread()


class FakeThread:
    """Fake Discord thread."""

    def __init__(self):
        self.id = 999
        self.jump_url = "https://discord.com/channels/123/999"
        self.pinned_message = None

    async def send(self, content=None, embed=None):
        msg = FakeMessage()
        return msg

    async def fetch_message(self, message_id):
        return FakeMessage()


class FakeChannel:
    """Fake Discord channel."""

    def __init__(self, message=None):
        self.message = message or FakeMessage()
        self.id = 456

    async def fetch_message(self, message_id):
        return self.message

    async def create_thread(self, name=None, message=None, auto_archive_duration=None):
        return FakeThread()

    async def send(self, content=None, embed=None, view=None):
        msg = FakeMessage()
        return msg


class FakeResponse:
    """Fake Discord interaction response."""

    def __init__(self):
        self.deferred = False

    async def defer(self, ephemeral=False):
        self.deferred = True

    async def send_message(self, content=None, embed=None, ephemeral=False):
        pass


class FakeInteraction:
    """Fake Discord interaction with guild support."""

    def __init__(self, user_id=1, guild_id=TEST_GUILD_ID):
        self.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = FakeGuild(guild_id) if guild_id else None
        self.channel = FakeChannel()
        self.followup = FakeFollowup()
        self.response = FakeResponse()


class FakePlayerRepo:
    """Fake player repository that returns test data."""

    def __init__(self):
        self.players = {}

    def add_player(self, discord_id, guild_id=TEST_GUILD_ID):
        player = Player(
            name=f"Player{discord_id}",
            mmr=3000,
            initial_mmr=3000,
            preferred_roles=["1", "2"],
            main_role="1",
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
            discord_id=discord_id,
        )
        self.players[(discord_id, guild_id)] = player
        return player

    def get_by_ids(self, ids, guild_id=None):
        return [self.players.get((id, guild_id)) for id in ids if (id, guild_id) in self.players]

    def get_captain_eligible_players(self, ids, guild_id=None):
        return []


class FakePlayerService:
    """Fake player service that returns test data."""

    def __init__(self, player_repo):
        self.player_repo = player_repo

    def get_player(self, discord_id, guild_id=None):
        return self.player_repo.players.get((discord_id, guild_id))


class FakeMatchService:
    """Fake match service for pending match checks."""

    def get_last_shuffle(self, guild_id):
        return None  # No pending match


class FakeBot:
    """Fake Discord bot."""

    def __init__(self, channel=None):
        self._channel = channel or FakeChannel()
        self.match_service = FakeMatchService()

    def get_channel(self, channel_id):
        return self._channel

    async def fetch_channel(self, channel_id):
        return self._channel


def make_services(player_repo=None):
    """Create lobby manager, lobby service, and player service."""
    db = Database(db_path=":memory:")
    lobby_manager = LobbyManager(db)
    player_repo = player_repo or FakePlayerRepo()
    lobby_service = LobbyService(lobby_manager, player_repo)
    player_service = FakePlayerService(player_repo)
    return lobby_manager, lobby_service, player_service, player_repo


@pytest.fixture
def monkeypatch_safe_defer(monkeypatch):
    """Mock safe_defer to return True."""
    monkeypatch.setattr("commands.lobby.safe_defer", AsyncMock(return_value=True))


@pytest.mark.asyncio
async def test_lobby_command_uses_guild_id(monkeypatch_safe_defer):
    """Test /lobby command properly extracts and uses guild_id."""
    _, lobby_service, player_service, player_repo = make_services()

    # Register a player
    player_repo.add_player(1, TEST_GUILD_ID)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    bot = FakeBot(channel=interaction.channel)

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError
    await cog.lobby.callback(cog, interaction)

    # Should have sent some response
    assert interaction.followup.messages or lobby_service.get_lobby() is not None


@pytest.mark.asyncio
async def test_join_command_uses_guild_id(monkeypatch_safe_defer):
    """Test /join command properly extracts and uses guild_id."""
    _, lobby_service, player_service, player_repo = make_services()

    # Create lobby and register player
    lobby_service.get_or_create_lobby(creator_id=99)
    player_repo.add_player(1, TEST_GUILD_ID)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    bot = FakeBot()

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError
    await cog.join.callback(cog, interaction)

    # Should have sent a response
    assert interaction.followup.messages


@pytest.mark.asyncio
async def test_leave_command_uses_guild_id(monkeypatch_safe_defer):
    """Test /leave command properly extracts and uses guild_id."""
    _, lobby_service, player_service, player_repo = make_services()

    # Create lobby, register player, and add to lobby
    lobby = lobby_service.get_or_create_lobby(creator_id=99)
    player_repo.add_player(1, TEST_GUILD_ID)
    lobby.add_player(1)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    bot = FakeBot()

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError
    await cog.leave.callback(cog, interaction)

    # Should have sent a response
    assert interaction.followup.messages


@pytest.mark.asyncio
async def test_kick_command_uses_guild_id(monkeypatch, monkeypatch_safe_defer):
    """Test /kick command properly extracts and uses guild_id."""
    monkeypatch.setattr("commands.lobby.has_admin_permission", lambda _: True)

    _, lobby_service, player_service, player_repo = make_services()

    # Create lobby, register players, add kicked player to lobby
    lobby = lobby_service.get_or_create_lobby(creator_id=1)
    player_repo.add_player(1, TEST_GUILD_ID)
    player_repo.add_player(42, TEST_GUILD_ID)
    lobby.add_player(42)
    lobby_service.set_lobby_message_id(message_id=12345, channel_id=100)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    kicked_player = SimpleNamespace(id=42, mention="<@42>")
    bot = FakeBot(channel=interaction.channel)

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError
    await cog.kick.callback(cog, interaction, kicked_player)

    # Should have sent a response
    assert interaction.followup.messages


@pytest.mark.asyncio
async def test_lobby_command_unregistered_player(monkeypatch_safe_defer):
    """Test /lobby command handles unregistered player with guild_id."""
    _, lobby_service, player_service, _ = make_services()

    # Don't register the player
    interaction = FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    bot = FakeBot()

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError, even for unregistered player
    await cog.lobby.callback(cog, interaction)

    # Should have sent an error about registration
    assert interaction.followup.messages
    assert "register" in interaction.followup.messages[0]["content"].lower()


@pytest.mark.asyncio
async def test_join_command_unregistered_player(monkeypatch_safe_defer):
    """Test /join command handles unregistered player with guild_id."""
    _, lobby_service, player_service, _ = make_services()

    # Create lobby but don't register the player
    lobby_service.get_or_create_lobby(creator_id=99)

    interaction = FakeInteraction(user_id=999, guild_id=TEST_GUILD_ID)
    bot = FakeBot()

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError
    await cog.join.callback(cog, interaction)

    # Should have sent an error about registration
    assert interaction.followup.messages
    assert "register" in interaction.followup.messages[0]["content"].lower()


@pytest.mark.asyncio
async def test_lobby_command_with_none_guild(monkeypatch_safe_defer):
    """Test /lobby command handles None guild (DM context)."""
    _, lobby_service, player_service, player_repo = make_services()

    # Register player with guild_id=None (normalized to 0 in real code)
    player_repo.add_player(1, None)

    interaction = FakeInteraction(user_id=1, guild_id=None)
    bot = FakeBot()

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError or AttributeError
    await cog.lobby.callback(cog, interaction)


@pytest.mark.asyncio
async def test_auto_join_lobby_uses_guild_id(monkeypatch_safe_defer):
    """Test _auto_join_lobby helper properly uses guild_id."""
    _, lobby_service, player_service, player_repo = make_services()

    # Create lobby and register player with roles
    lobby = lobby_service.get_or_create_lobby(creator_id=99)
    player_repo.add_player(1, TEST_GUILD_ID)

    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    bot = FakeBot()

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError
    joined, message = await cog._auto_join_lobby(interaction, lobby)

    # Should have attempted to join (may succeed or fail based on implementation)
    assert isinstance(joined, bool)


@pytest.mark.asyncio
async def test_sync_lobby_displays_uses_guild_id(monkeypatch_safe_defer):
    """Test _sync_lobby_displays helper properly uses guild_id."""
    _, lobby_service, player_service, player_repo = make_services()

    # Create lobby and set message IDs
    lobby = lobby_service.get_or_create_lobby(creator_id=99)
    lobby_service.set_lobby_message_id(message_id=12345, channel_id=100)

    fake_channel = FakeChannel()
    bot = FakeBot(channel=fake_channel)

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError or TypeError
    await cog._sync_lobby_displays(lobby, guild_id=TEST_GUILD_ID)


@pytest.mark.asyncio
async def test_update_thread_embed_uses_guild_id(monkeypatch_safe_defer):
    """Test _update_thread_embed helper properly uses guild_id."""
    _, lobby_service, player_service, player_repo = make_services()

    # Create lobby (no thread set, so this should be a no-op)
    lobby = lobby_service.get_or_create_lobby(creator_id=99)

    bot = FakeBot()
    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError or TypeError
    await cog._update_thread_embed(lobby, guild_id=TEST_GUILD_ID)


class TestGuildIdDefinitionOrder:
    """
    Tests that specifically verify guild_id is defined before use.

    These tests catch the pattern where guild_id is used before being
    extracted from interaction.guild.id.
    """

    @pytest.mark.asyncio
    async def test_lobby_command_guild_id_order(self, monkeypatch_safe_defer):
        """Verify guild_id is defined before any service calls in /lobby."""
        _, lobby_service, player_service, player_repo = make_services()
        player_repo.add_player(1, TEST_GUILD_ID)

        # Track the order of calls
        call_order = []
        original_get_player = player_service.get_player

        def tracking_get_player(discord_id, guild_id=None):
            call_order.append(("get_player", discord_id, guild_id))
            return original_get_player(discord_id, guild_id)

        player_service.get_player = tracking_get_player

        interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
        bot = FakeBot()
        cog = LobbyCommands(bot, lobby_service, player_service)

        await cog.lobby.callback(cog, interaction)

        # Verify get_player was called with the correct guild_id
        assert any(call[2] == TEST_GUILD_ID for call in call_order if call[0] == "get_player")

    @pytest.mark.asyncio
    async def test_join_command_guild_id_order(self, monkeypatch_safe_defer):
        """Verify guild_id is defined before any service calls in /join."""
        _, lobby_service, player_service, player_repo = make_services()
        lobby_service.get_or_create_lobby(creator_id=99)
        player_repo.add_player(1, TEST_GUILD_ID)

        call_order = []
        original_get_player = player_service.get_player

        def tracking_get_player(discord_id, guild_id=None):
            call_order.append(("get_player", discord_id, guild_id))
            return original_get_player(discord_id, guild_id)

        player_service.get_player = tracking_get_player

        interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
        bot = FakeBot()
        cog = LobbyCommands(bot, lobby_service, player_service)

        await cog.join.callback(cog, interaction)

        # Verify get_player was called with the correct guild_id
        assert any(call[2] == TEST_GUILD_ID for call in call_order if call[0] == "get_player")
