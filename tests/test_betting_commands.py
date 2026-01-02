import pytest
from unittest.mock import AsyncMock, MagicMock

import discord

from commands.betting import BettingCommands


@pytest.mark.asyncio
async def test_update_shuffle_message_wagers_updates_field():
    guild_id = 123

    # Mocks
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Pending state and message info (house mode is default)
    pending_state = {"bet_lock_until": 1700000000, "betting_mode": "house"}
    match_service.get_last_shuffle.return_value = pending_state
    match_service.get_shuffle_message_info.return_value = {
        "message_id": 42,
        "channel_id": 55,
        "jump_url": "https://example.com",
    }

    # Pot totals to display
    betting_service.get_pot_odds.return_value = {"radiant": 5, "dire": 7}

    # Fake embed/message/channel - use old field name to test update logic
    embed = discord.Embed(title="Balanced Team Shuffle")
    embed.add_field(name="💰 House Betting (1:1)", value="old", inline=False)

    message = MagicMock()
    message.embeds = [embed]
    message.edit = AsyncMock()

    channel = MagicMock()
    channel.fetch_message = AsyncMock(return_value=message)

    bot.get_channel.return_value = channel
    bot.fetch_channel = AsyncMock(return_value=channel)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Act
    await commands._update_shuffle_message_wagers(guild_id)

    # Assert embed updated with new totals
    message.edit.assert_awaited_once()
    edited_embed = message.edit.call_args.kwargs["embed"]
    fields = {f.name: f.value for f in edited_embed.fields}
    # Field name includes betting mode now
    assert "💰 House Betting (1:1)" in fields
    assert "Radiant: 5" in fields["💰 House Betting (1:1)"]
    assert "Dire: 7" in fields["💰 House Betting (1:1)"]
    assert "Closes" in fields["💰 House Betting (1:1)"]


@pytest.mark.asyncio
async def test_update_shuffle_message_wagers_handles_missing_message():
    guild_id = 123

    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    pending_state = {"bet_lock_until": 1700000000, "betting_mode": "house"}
    match_service.get_last_shuffle.return_value = pending_state
    match_service.get_shuffle_message_info.return_value = {
        "message_id": 42,
        "channel_id": 55,
        "jump_url": "https://example.com",
    }

    # Channel fetch raises to simulate deleted/missing message
    channel = MagicMock()
    channel.fetch_message = AsyncMock(side_effect=discord.NotFound(MagicMock(), "not found"))
    bot.get_channel.return_value = channel
    bot.fetch_channel = AsyncMock(return_value=channel)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Should not raise even if message fetch fails
    await commands._update_shuffle_message_wagers(guild_id)


@pytest.mark.asyncio
async def test_send_betting_reminder_warning_formats_totals():
    guild_id = 123
    lock_until = 1700000500

    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    pending_state = {"bet_lock_until": lock_until, "betting_mode": "house"}
    match_service.get_last_shuffle.return_value = pending_state
    match_service.get_shuffle_message_info.return_value = {
        "message_id": 42,
        "channel_id": 55,
        "jump_url": "https://example.com",
    }

    betting_service.get_pot_odds.return_value = {"radiant": 12, "dire": 8}

    message = MagicMock()
    message.reply = AsyncMock()
    channel = MagicMock()
    channel.fetch_message = AsyncMock(return_value=message)
    bot.get_channel.return_value = channel
    bot.fetch_channel = AsyncMock(return_value=channel)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    await commands._send_betting_reminder(
        guild_id,
        reminder_type="warning",
        lock_until=lock_until,
    )

    message.reply.assert_awaited_once()
    # reply content is the first positional arg
    reply_text = message.reply.call_args.args[0]
    assert "5 minutes" in reply_text
    assert f"<t:{lock_until}:R>" in reply_text
    assert "Radiant: 12" in reply_text
    assert "Dire: 8" in reply_text


@pytest.mark.asyncio
async def test_send_betting_reminder_closed_formats_totals():
    guild_id = 123

    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    pending_state = {"bet_lock_until": 1700000500, "betting_mode": "house"}
    match_service.get_last_shuffle.return_value = pending_state
    match_service.get_shuffle_message_info.return_value = {
        "message_id": 42,
        "channel_id": 55,
        "jump_url": "https://example.com",
    }

    betting_service.get_pot_odds.return_value = {"radiant": 3, "dire": 9}

    message = MagicMock()
    message.reply = AsyncMock()
    channel = MagicMock()
    channel.fetch_message = AsyncMock(return_value=message)
    bot.get_channel.return_value = channel
    bot.fetch_channel = AsyncMock(return_value=channel)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    await commands._send_betting_reminder(
        guild_id,
        reminder_type="closed",
        lock_until=pending_state["bet_lock_until"],
    )

    message.reply.assert_awaited_once()
    reply_text = message.reply.call_args.args[0]
    assert "Betting is now closed" in reply_text
    assert "Radiant: 3" in reply_text
    assert "Dire: 9" in reply_text