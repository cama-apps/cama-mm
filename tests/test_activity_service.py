"""Tests for ActivityService — periodic text/voice channel sweeps.

Discord objects are mocked (per tests/test_streaming.py); the DB effect is
verified against a real PlayerRepository via is_active_for_lottery.
"""

from types import SimpleNamespace
from unittest.mock import MagicMock

import discord
import pytest

from repositories.player_repository import PlayerRepository
from services.activity_service import ActivityService
from tests.conftest import TEST_GUILD_ID

ACTIVITY_DAYS = 14


@pytest.fixture
def player_repo(repo_db_path):
    return PlayerRepository(repo_db_path)


@pytest.fixture
def service(player_repo):
    return ActivityService(player_repo)


def _register(repo: PlayerRepository, discord_id: int) -> None:
    repo.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=TEST_GUILD_ID,
        initial_mmr=3000,
    )


def _member(discord_id: int, *, bot: bool = False, deaf: bool = False, self_deaf: bool = False,
            mute: bool = False, self_mute: bool = False) -> MagicMock:
    m = MagicMock(spec=discord.Member)
    m.id = discord_id
    m.bot = bot
    m.voice = SimpleNamespace(deaf=deaf, self_deaf=self_deaf, mute=mute, self_mute=self_mute)
    return m


def _perms(*, view: bool = True, history: bool = True) -> SimpleNamespace:
    return SimpleNamespace(view_channel=view, read_message_history=history)


def _voice_channel(members, *, perms=None) -> MagicMock:
    ch = MagicMock(spec=discord.VoiceChannel)
    ch.members = members
    ch.permissions_for = lambda me: perms or _perms()
    return ch


async def _async_iter(items):
    for item in items:
        yield item


def _text_channel(messages, *, perms=None):
    ch = MagicMock(spec=discord.TextChannel)
    ch.permissions_for = lambda me: perms or _perms()
    ch.history = MagicMock(return_value=_async_iter(messages))
    return ch


def _message(author_id: int, *, bot: bool = False) -> SimpleNamespace:
    return SimpleNamespace(author=SimpleNamespace(id=author_id, bot=bot))


def _guild(*, voice_channels=None, text_channels=None, afk_channel=None) -> MagicMock:
    guild = MagicMock(spec=discord.Guild)
    guild.id = TEST_GUILD_ID
    guild.me = MagicMock()
    guild.afk_channel = afk_channel
    guild.voice_channels = voice_channels or []
    guild.text_channels = text_channels or []
    return guild


class TestVoiceSweep:
    @pytest.mark.asyncio
    async def test_bumps_only_registered_players(self, service, player_repo):
        """Registered member bumped; unregistered and bots ignored, no crash."""
        _register(player_repo, 1)
        channel = _voice_channel([_member(1), _member(2), _member(3, bot=True)])
        guild = _guild(voice_channels=[channel])

        await service.sweep_voice(guild)

        assert player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)
        assert not player_repo.is_active_for_lottery(2, TEST_GUILD_ID, ACTIVITY_DAYS)

    @pytest.mark.asyncio
    async def test_skips_permission_denied_channel(self, service, player_repo):
        """A voice channel the bot can't view is not read."""
        _register(player_repo, 1)
        channel = _voice_channel([_member(1)], perms=_perms(view=False))
        guild = _guild(voice_channels=[channel])

        await service.sweep_voice(guild)

        assert not player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)

    @pytest.mark.asyncio
    async def test_afk_channel_not_counted(self, service, player_repo):
        """Members sitting in the server's AFK channel are not active."""
        _register(player_repo, 1)
        afk = _voice_channel([_member(1)])
        guild = _guild(voice_channels=[afk], afk_channel=afk)

        await service.sweep_voice(guild)

        assert not player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)

    @pytest.mark.asyncio
    async def test_deafened_not_counted(self, service, player_repo):
        """Server-deaf and self-deaf members are not active."""
        _register(player_repo, 1)
        _register(player_repo, 2)
        channel = _voice_channel([
            _member(1, self_deaf=True),
            _member(2, deaf=True),
        ])
        guild = _guild(voice_channels=[channel])

        await service.sweep_voice(guild)

        assert not player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)
        assert not player_repo.is_active_for_lottery(2, TEST_GUILD_ID, ACTIVITY_DAYS)

    @pytest.mark.asyncio
    async def test_muted_still_counted(self, service, player_repo):
        """A muted/self-muted (but not deafened) member still counts as active."""
        _register(player_repo, 1)
        channel = _voice_channel([_member(1, mute=True, self_mute=True)])
        guild = _guild(voice_channels=[channel])

        await service.sweep_voice(guild)

        assert player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)


class TestTextSweep:
    @pytest.mark.asyncio
    async def test_bumps_recent_authors(self, service, player_repo):
        """A registered author of a recent message is bumped; bots ignored."""
        _register(player_repo, 1)
        channel = _text_channel([_message(1), _message(2, bot=True)])
        guild = _guild(text_channels=[channel])

        await service.sweep_text(guild, lookback_seconds=3600)

        assert player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)

    @pytest.mark.asyncio
    async def test_requires_read_message_history(self, service, player_repo):
        """Without Read Message History, the channel history is never fetched."""
        _register(player_repo, 1)
        channel = _text_channel([_message(1)], perms=_perms(history=False))
        guild = _guild(text_channels=[channel])

        await service.sweep_text(guild, lookback_seconds=3600)

        channel.history.assert_not_called()
        assert not player_repo.is_active_for_lottery(1, TEST_GUILD_ID, ACTIVITY_DAYS)

    @pytest.mark.asyncio
    async def test_watermark_advances(self, service, player_repo):
        """The second sweep passes an `after` at least as new as the first."""
        _register(player_repo, 1)
        channel = _text_channel([_message(1)])
        guild = _guild(text_channels=[channel])

        await service.sweep_text(guild, lookback_seconds=3600)
        first_after = channel.history.call_args.kwargs["after"]

        channel.history = MagicMock(return_value=_async_iter([]))
        await service.sweep_text(guild, lookback_seconds=3600)
        second_after = channel.history.call_args.kwargs["after"]

        assert second_after >= first_after

    @pytest.mark.asyncio
    async def test_forbidden_mid_sweep_is_swallowed(self, service, player_repo):
        """A Forbidden raised while reading history doesn't break the sweep."""
        _register(player_repo, 1)

        def _raise(*args, **kwargs):
            raise discord.Forbidden(MagicMock(status=403), "nope")

        channel = _text_channel([])
        channel.history = MagicMock(side_effect=_raise)
        guild = _guild(text_channels=[channel])

        # Should not raise.
        await service.sweep_text(guild, lookback_seconds=3600)
        # Watermark still advances so we don't re-scan the same window forever.
        assert (guild.id, channel.id) in service._text_watermarks
