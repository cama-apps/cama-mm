"""
Tests for lobby commands to ensure guild_id is properly handled.

These tests verify that guild_id is defined before use in all command handlers,
catching UnboundLocalError issues that occur when guild_id is used before being
extracted from interaction.guild.id.
"""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from commands.lobby import LobbyCommands
from domain.models.player import Player
from services.lobby_manager_service import LobbyManagerService as LobbyManager
from services.lobby_service import LobbyService
from tests.conftest import TEST_GUILD_ID
from tests.fakes.lobby_repo import FakeLobbyRepo


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
        self.added_reactions = []
        self.removed_reactions = []
        self.jump_url = "https://discord.com/channels/123/456/789"
        self.id = 789

    async def edit(self, embed=None, allowed_mentions=None, content=None):
        self.edits.append({"embed": embed, "content": content})

    async def remove_reaction(self, emoji, user):
        self.removed_reactions.append((emoji, user))

    async def add_reaction(self, emoji):
        self.added_reactions.append(str(emoji))

    async def pin(self, reason=None):
        pass

    async def delete(self):
        pass

    async def create_thread(self, name=None, auto_archive_duration=None):
        return FakeThread()


class FakeThread:
    """Fake Discord thread."""

    def __init__(self):
        self.id = 999
        self.jump_url = "https://discord.com/channels/123/999"
        self.pinned_message = None
        self.message = FakeMessage()
        self.fetch_message_calls = []
        self.partial_message_calls = []

    async def send(self, content=None, embed=None):
        msg = FakeMessage()
        return msg

    async def fetch_message(self, message_id):
        self.fetch_message_calls.append(message_id)
        return self.message

    def get_partial_message(self, message_id):
        self.partial_message_calls.append(message_id)
        return self.message


class FakeChannel:
    """Fake Discord channel."""

    def __init__(self, message=None):
        self.message = message or FakeMessage()
        self.sent_messages = []
        self.id = 456
        self.fetch_message_calls = []
        self.partial_message_calls = []

    async def fetch_message(self, message_id):
        self.fetch_message_calls.append(message_id)
        return self.message

    def get_partial_message(self, message_id):
        self.partial_message_calls.append(message_id)
        return self.message

    async def create_thread(self, name=None, message=None, auto_archive_duration=None):
        return FakeThread()

    async def send(self, content=None, embed=None, view=None):
        msg = FakeMessage()
        self.sent_messages.append(msg)
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


class FakeStateService:
    """Fake state service for concurrent match support."""

    def get_pending_match_for_player(self, guild_id, discord_id):
        return None  # Player not in any pending match

    def get_all_pending_matches(self, guild_id):
        return []


class FakeMatchService:
    """Fake match service for pending match checks."""

    def __init__(self):
        self.state_service = FakeStateService()

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


class _EventBarrier:
    """Hold named async branches until every expected branch has entered."""

    def __init__(self, *expected: str):
        self.expected = set(expected)
        self.entered: set[str] = set()
        self.all_entered = asyncio.Event()
        self.release = asyncio.Event()

    async def enter(self, name: str, *, error: Exception | None = None) -> None:
        self.entered.add(name)
        if self.entered == self.expected:
            self.all_entered.set()
        await self.release.wait()
        if error is not None:
            raise error


def make_services(player_repo=None):
    """Create lobby manager, lobby service, and player service."""
    lobby_manager = LobbyManager(FakeLobbyRepo())
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

    # The command must create the lobby in THIS guild, credit the invoker as
    # creator, auto-join them, and confirm via followup.
    lobby = lobby_service.get_lobby(guild_id=TEST_GUILD_ID)
    assert lobby is not None
    assert lobby.created_by == 1
    assert 1 in lobby.players
    assert interaction.followup.messages
    assert "Lobby created and joined" in interaction.followup.messages[-1]["content"]
    reactions = interaction.channel.sent_messages[0].added_reactions
    assert "📋" in reactions
    assert all("frogling" not in reaction for reaction in reactions)


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
    joined, _ = await cog._auto_join_lobby(interaction, lobby)

    # Should have attempted to join (may succeed or fail based on implementation)
    assert isinstance(joined, bool)


@pytest.mark.asyncio
async def test_join_fans_out_confirmation_and_isolates_maintenance_failure(
    monkeypatch,
    monkeypatch_safe_defer,
):
    """Confirmation, display, and ordered thread work enter the same async wave."""
    import bot as bot_module

    _, lobby_service, player_service, player_repo = make_services()
    lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_player(1, TEST_GUILD_ID)
    lobby_service.set_lobby_message_id(
        message_id=789,
        channel_id=456,
        thread_id=999,
        embed_message_id=789,
        guild_id=TEST_GUILD_ID,
    )
    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service)
    barrier = _EventBarrier("followup", "display", "activity")
    confirmation = {}

    async def followup(_interaction, **kwargs):
        confirmation.update(kwargs)
        await barrier.enter("followup")

    async def sync_displays(_lobby, _guild_id):
        await barrier.enter("display", error=RuntimeError("display failed"))

    async def post_activity(_thread_id, _user):
        await barrier.enter("activity")

    rally = AsyncMock(return_value=True)
    monkeypatch.setattr("commands.lobby.safe_followup", followup)
    monkeypatch.setattr(cog, "_sync_lobby_displays", sync_displays)
    monkeypatch.setattr(cog, "_post_join_activity", post_activity)
    monkeypatch.setattr(bot_module, "notify_lobby_rally", rally)
    monkeypatch.setattr(bot_module, "notify_lobby_ready", AsyncMock())

    command = asyncio.create_task(cog.join.callback(cog, interaction))
    try:
        await asyncio.wait_for(barrier.all_entered.wait(), timeout=1)
        assert barrier.entered == barrier.expected
        assert confirmation["content"] == "✅ Joined the lobby!"
    finally:
        barrier.release.set()
    await command

    rally.assert_awaited_once()
    assert 1 in lobby_service.get_lobby(guild_id=TEST_GUILD_ID).players


@pytest.mark.asyncio
async def test_leave_fans_out_confirmation_and_isolates_maintenance_failure(
    monkeypatch,
    monkeypatch_safe_defer,
):
    """All leave maintenance starts with confirmation and failures stay local."""
    _, lobby_service, player_service, player_repo = make_services()
    lobby = lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_player(1, TEST_GUILD_ID)
    lobby.add_player(1)
    lobby_service.set_lobby_message_id(
        message_id=789,
        channel_id=456,
        thread_id=999,
        embed_message_id=789,
        guild_id=TEST_GUILD_ID,
    )
    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service)
    barrier = _EventBarrier("followup", "display", "reaction", "activity")
    confirmation = {}

    async def followup(_interaction, **kwargs):
        confirmation.update(kwargs)
        await barrier.enter("followup")

    async def sync_displays(_lobby, _guild_id):
        await barrier.enter("display")

    async def remove_reaction(_user, *, guild_id):
        assert guild_id == TEST_GUILD_ID
        await barrier.enter("reaction", error=RuntimeError("reaction failed"))

    async def post_activity(_thread_id, _user):
        await barrier.enter("activity")

    monkeypatch.setattr("commands.lobby.safe_followup", followup)
    monkeypatch.setattr(cog, "_sync_lobby_displays", sync_displays)
    monkeypatch.setattr(cog, "_remove_user_lobby_reactions", remove_reaction)
    monkeypatch.setattr(cog, "_post_leave_activity", post_activity)

    command = asyncio.create_task(cog.leave.callback(cog, interaction))
    try:
        await asyncio.wait_for(barrier.all_entered.wait(), timeout=1)
        assert barrier.entered == barrier.expected
        assert confirmation["content"] == "✅ Left the lobby."
    finally:
        barrier.release.set()
    await command

    assert 1 not in lobby_service.get_lobby(guild_id=TEST_GUILD_ID).players


@pytest.mark.asyncio
async def test_auto_join_runs_display_and_thread_publication_concurrently(
    monkeypatch,
):
    """A failed display refresh does not delay or suppress join publication."""
    import bot as bot_module

    _, lobby_service, player_service, player_repo = make_services()
    lobby = lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_player(1, TEST_GUILD_ID)
    lobby_service.set_lobby_message_id(
        message_id=789,
        channel_id=456,
        thread_id=999,
        embed_message_id=789,
        guild_id=TEST_GUILD_ID,
    )
    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service)
    barrier = _EventBarrier("display", "activity")

    async def sync_displays(_lobby, _guild_id):
        await barrier.enter("display", error=RuntimeError("display failed"))

    async def post_activity(_thread_id, _user):
        await barrier.enter("activity")

    rally = AsyncMock(return_value=True)
    monkeypatch.setattr(cog, "_sync_lobby_displays", sync_displays)
    monkeypatch.setattr(cog, "_post_join_activity", post_activity)
    monkeypatch.setattr(bot_module, "notify_lobby_rally", rally)
    monkeypatch.setattr(bot_module, "notify_lobby_ready", AsyncMock())

    auto_join = asyncio.create_task(cog._auto_join_lobby(interaction, lobby))
    try:
        await asyncio.wait_for(barrier.all_entered.wait(), timeout=1)
        assert barrier.entered == barrier.expected
    finally:
        barrier.release.set()
    joined, warning = await auto_join

    assert joined is True
    assert warning is None
    rally.assert_awaited_once()


@pytest.mark.asyncio
async def test_auto_join_activity_finishes_before_rally_thread_send(monkeypatch):
    """The ordered thread branch cannot publish rally before join activity."""
    import bot as bot_module

    _, lobby_service, player_service, player_repo = make_services()
    lobby = lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_player(1, TEST_GUILD_ID)
    lobby_service.set_lobby_message_id(
        message_id=789,
        channel_id=456,
        thread_id=999,
        embed_message_id=789,
        guild_id=TEST_GUILD_ID,
    )
    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    cog = LobbyCommands(FakeBot(), lobby_service, player_service)
    activity_entered = asyncio.Event()
    release_activity = asyncio.Event()
    rally_thread_sent = asyncio.Event()

    async def post_activity(_thread_id, _user):
        activity_entered.set()
        await release_activity.wait()

    async def rally(_channel, thread, _lobby, _guild_id):
        await thread.send("rally")
        return True

    async def thread_send(*_args, **_kwargs):
        rally_thread_sent.set()

    cog.bot._channel.send = thread_send
    monkeypatch.setattr(cog, "_sync_lobby_displays", AsyncMock())
    monkeypatch.setattr(cog, "_post_join_activity", post_activity)
    monkeypatch.setattr(bot_module, "notify_lobby_rally", rally)
    monkeypatch.setattr(bot_module, "notify_lobby_ready", AsyncMock())

    auto_join = asyncio.create_task(cog._auto_join_lobby(interaction, lobby))
    try:
        await asyncio.wait_for(activity_entered.wait(), timeout=1)
        assert not rally_thread_sent.is_set()
    finally:
        release_activity.set()
    joined, _ = await auto_join

    assert joined is True
    assert rally_thread_sent.is_set()


@pytest.mark.asyncio
async def test_sync_lobby_displays_uses_guild_id(monkeypatch_safe_defer):
    """Test _sync_lobby_displays helper properly uses guild_id."""
    _, lobby_service, player_service, _ = make_services()

    # Create lobby and set message IDs
    lobby = lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    lobby_service.set_lobby_message_id(
        message_id=12345,
        channel_id=100,
        guild_id=TEST_GUILD_ID,
    )

    fake_channel = FakeChannel()
    bot = FakeBot(channel=fake_channel)

    cog = LobbyCommands(bot, lobby_service, player_service)

    # This should not raise UnboundLocalError or TypeError
    await cog._sync_lobby_displays(lobby, guild_id=TEST_GUILD_ID)

    assert fake_channel.fetch_message_calls == []
    assert fake_channel.partial_message_calls == [12345]
    assert len(fake_channel.message.edits) == 1


@pytest.mark.asyncio
async def test_remove_lobby_reaction_uses_partial_message_without_fetch():
    """Removing a reaction should issue only the mutation request."""
    _, lobby_service, player_service, _ = make_services()
    lobby_service.get_or_create_lobby(creator_id=99, guild_id=TEST_GUILD_ID)
    lobby_service.set_lobby_message_id(
        message_id=12345,
        channel_id=100,
        guild_id=TEST_GUILD_ID,
    )
    fake_channel = FakeChannel()
    cog = LobbyCommands(FakeBot(channel=fake_channel), lobby_service, player_service)
    user = SimpleNamespace(id=42)

    await cog._remove_user_lobby_reactions(user, guild_id=TEST_GUILD_ID)

    assert fake_channel.fetch_message_calls == []
    assert fake_channel.partial_message_calls == [12345]
    assert fake_channel.message.removed_reactions == [("⚔️", user)]


@pytest.mark.asyncio
async def test_thread_embed_update_uses_partial_message_without_fetch():
    """A trusted thread message ID does not require a preceding Discord GET."""
    _, lobby_service, player_service, _ = make_services()
    lobby = lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    lobby_service.set_lobby_message_id(
        message_id=12345,
        channel_id=100,
        thread_id=999,
        embed_message_id=54321,
        guild_id=TEST_GUILD_ID,
    )
    fake_thread = FakeThread()
    cog = LobbyCommands(FakeBot(channel=fake_thread), lobby_service, player_service)

    await cog._update_thread_embed(lobby, guild_id=TEST_GUILD_ID)

    assert fake_thread.fetch_message_calls == []
    assert fake_thread.partial_message_calls == [54321]
    assert len(fake_thread.message.edits) == 1


@pytest.mark.asyncio
async def test_existing_lobby_auto_join_edits_starter_once(monkeypatch_safe_defer):
    """Successful auto-join must not refetch and re-edit the thread starter."""
    _, lobby_service, player_service, player_repo = make_services()
    lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_player(1, TEST_GUILD_ID)
    message = FakeMessage()
    channel = FakeChannel(message=message)
    lobby_service.set_lobby_message_id(
        message_id=message.id,
        channel_id=channel.id,
        thread_id=999,
        embed_message_id=message.id,
        guild_id=TEST_GUILD_ID,
    )
    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    interaction.channel = channel
    cog = LobbyCommands(FakeBot(channel=channel), lobby_service, player_service)

    await cog.lobby.callback(cog, interaction)

    # One GET remains intentionally: it validates the persisted lobby message.
    assert channel.fetch_message_calls == [message.id]
    # _auto_join_lobby performs the sole mutation through a partial handle.
    assert channel.partial_message_calls == [message.id]
    assert len(message.edits) == 1
    assert 1 in lobby_service.get_lobby(guild_id=TEST_GUILD_ID).players


@pytest.mark.asyncio
async def test_existing_member_keeps_single_repair_refresh(monkeypatch_safe_defer):
    """An existing member still gets the explicit stale-display repair edit."""
    _, lobby_service, player_service, player_repo = make_services()
    lobby = lobby_service.get_or_create_lobby(
        creator_id=99,
        guild_id=TEST_GUILD_ID,
    )
    player_repo.add_player(1, TEST_GUILD_ID)
    lobby.add_player(1)
    message = FakeMessage()
    channel = FakeChannel(message=message)
    lobby_service.set_lobby_message_id(
        message_id=message.id,
        channel_id=channel.id,
        thread_id=999,
        embed_message_id=message.id,
        guild_id=TEST_GUILD_ID,
    )
    interaction = FakeInteraction(user_id=1, guild_id=TEST_GUILD_ID)
    interaction.channel = channel
    cog = LobbyCommands(FakeBot(channel=channel), lobby_service, player_service)

    await cog.lobby.callback(cog, interaction)

    assert channel.fetch_message_calls == [message.id]
    assert channel.partial_message_calls == [message.id]
    assert len(message.edits) == 1


@pytest.mark.asyncio
async def test_update_thread_embed_uses_guild_id(monkeypatch_safe_defer):
    """Test _update_thread_embed helper properly uses guild_id."""
    _, lobby_service, player_service, _ = make_services()

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
