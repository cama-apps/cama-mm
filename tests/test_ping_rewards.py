from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from commands.ping_rewards import PingRewardsCog, PING_REWARD_JC

LUKE_ID = 100000000000000001
ASH_ID = 100000000000000002


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def _make_message(*, mentioned_ids: list[int], author_is_bot: bool = False, guild_id: int = 1):
    message = MagicMock()
    message.author.bot = author_is_bot
    message.guild = MagicMock()
    message.guild.id = guild_id
    message.channel.send = AsyncMock()
    message.mentions = [MagicMock(id=uid) for uid in mentioned_ids]
    message.author.id = 999
    return message


@pytest.fixture
def bot():
    b = MagicMock()
    b.player_service = MagicMock()
    b.player_service.adjust_balance = MagicMock()
    b.get_user.return_value = None
    b.fetch_user = AsyncMock(return_value=MagicMock(display_name="Tester", mention="<@999>"))
    return b


@pytest.fixture(autouse=True)
def patch_config_ids():
    with patch("commands.ping_rewards.config.LUKE_DISCORD_ID", LUKE_ID), \
         patch("commands.ping_rewards.config.ASH_DISCORD_ID", ASH_ID):
        yield


@pytest.fixture
def cog(bot):
    return PingRewardsCog(bot)


# ---------------------------------------------------------------------------
# Guard conditions
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_ignores_bot_messages(cog, bot):
    message = _make_message(mentioned_ids=[LUKE_ID], author_is_bot=True)
    await cog.on_message(message)
    bot.player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_ignores_dm_messages(cog, bot):
    message = _make_message(mentioned_ids=[LUKE_ID])
    message.guild = None
    await cog.on_message(message)
    bot.player_service.adjust_balance.assert_not_called()


# ---------------------------------------------------------------------------
# Luke ping
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_luke_ping_awards_jc_on_win(cog, bot):
    message = _make_message(mentioned_ids=[LUKE_ID])
    with patch("commands.ping_rewards.random.random", return_value=0.0):
        await cog.on_message(message)
    bot.player_service.adjust_balance.assert_called_once_with(
        message.author.id, message.guild.id, PING_REWARD_JC
    )
    message.channel.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_luke_ping_no_reward_on_loss(cog, bot):
    message = _make_message(mentioned_ids=[LUKE_ID])
    with patch("commands.ping_rewards.random.random", return_value=0.99):
        await cog.on_message(message)
    bot.player_service.adjust_balance.assert_not_called()
    message.channel.send.assert_not_awaited()


# ---------------------------------------------------------------------------
# Ash ping — JC roll
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_ash_ping_awards_jc_on_win(cog, bot):
    message = _make_message(mentioned_ids=[ASH_ID])
    # Both rolls win (< 0.01)
    with patch("commands.ping_rewards.random.random", return_value=0.0):
        await cog.on_message(message)
    bot.player_service.adjust_balance.assert_called_once_with(
        message.author.id, message.guild.id, PING_REWARD_JC
    )
    message.channel.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_ash_ping_no_jc_on_loss(cog, bot):
    message = _make_message(mentioned_ids=[ASH_ID])
    # Both rolls lose
    with patch("commands.ping_rewards.random.random", return_value=0.5):
        await cog.on_message(message)
    bot.player_service.adjust_balance.assert_not_called()


# ---------------------------------------------------------------------------
# Ash ping — Luke DM roll
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_ash_ping_dms_luke_on_win(cog, bot):
    message = _make_message(mentioned_ids=[ASH_ID])
    luke_user = MagicMock()
    luke_user.send = AsyncMock()
    pinger_user = MagicMock(display_name="Tester", mention="<@999>")

    async def fetch_user(uid):
        return luke_user if uid == LUKE_ID else pinger_user

    bot.fetch_user = fetch_user

    with patch("commands.ping_rewards.random.random", return_value=0.0):
        await cog.on_message(message)

    luke_user.send.assert_awaited_once()
    sent = luke_user.send.call_args[0][0]
    assert "Tester" in sent
    assert "goodmin" in sent


@pytest.mark.asyncio
async def test_ash_ping_no_dm_on_loss(cog, bot):
    message = _make_message(mentioned_ids=[ASH_ID])
    luke_user = MagicMock()
    luke_user.send = AsyncMock()
    bot.fetch_user = AsyncMock(return_value=luke_user)

    with patch("commands.ping_rewards.random.random", return_value=0.5):
        await cog.on_message(message)

    luke_user.send.assert_not_awaited()


# ---------------------------------------------------------------------------
# Both rolls independent on Ash ping
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_ash_ping_only_dm_fires(cog, bot):
    """JC roll loses, DM roll wins — only DM should fire."""
    message = _make_message(mentioned_ids=[ASH_ID])
    luke_user = MagicMock()
    luke_user.send = AsyncMock()
    pinger_user = MagicMock(display_name="Tester", mention="<@999>")

    async def fetch_user(uid):
        return luke_user if uid == LUKE_ID else pinger_user

    bot.fetch_user = fetch_user

    rolls = iter([0.5, 0.0])  # first roll (JC) loses, second roll (DM) wins
    with patch("commands.ping_rewards.random.random", side_effect=rolls):
        await cog.on_message(message)

    bot.player_service.adjust_balance.assert_not_called()
    luke_user.send.assert_awaited_once()
