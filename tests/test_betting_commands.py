import time
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest

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


@pytest.mark.asyncio
async def test_mybets_uses_discord_timestamp_format():
    """Verify /mybets displays bet time using Discord dynamic timestamp."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    bet_time = int(time.time()) - 300  # 5 minutes ago
    pending_state = {"bet_lock_until": int(time.time()) + 600, "betting_mode": "house"}
    match_service.get_last_shuffle.return_value = pending_state

    # Now uses get_pending_bets (plural) instead of get_pending_bet
    betting_service.get_pending_bets.return_value = [
        {
            "amount": 100,
            "team_bet_on": "radiant",
            "bet_time": bet_time,
            "leverage": 1,
        }
    ]

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 456
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.mybets.callback(commands, interaction)

    interaction.followup.send.assert_awaited_once()
    message = interaction.followup.send.call_args.kwargs.get(
        "content",
        interaction.followup.send.call_args.args[0]
        if interaction.followup.send.call_args.args
        else "",
    )

    # Verify Discord timestamp format <t:TIMESTAMP:t>
    assert f"<t:{bet_time}:t>" in message, f"Expected Discord timestamp in: {message}"


@pytest.mark.asyncio
async def test_mybets_leverage_uses_discord_timestamp_format():
    """Verify /mybets with leverage displays bet time using Discord dynamic timestamp."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    bet_time = int(time.time()) - 120  # 2 minutes ago
    pending_state = {"bet_lock_until": int(time.time()) + 600, "betting_mode": "house"}
    match_service.get_last_shuffle.return_value = pending_state

    # Now uses get_pending_bets (plural) instead of get_pending_bet
    betting_service.get_pending_bets.return_value = [
        {
            "amount": 50,
            "team_bet_on": "dire",
            "bet_time": bet_time,
            "leverage": 3,
        }
    ]

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 456
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.mybets.callback(commands, interaction)

    interaction.followup.send.assert_awaited_once()
    message = interaction.followup.send.call_args.kwargs.get(
        "content",
        interaction.followup.send.call_args.args[0]
        if interaction.followup.send.call_args.args
        else "",
    )

    # Verify Discord timestamp format and leverage info
    assert f"<t:{bet_time}:t>" in message, f"Expected Discord timestamp in: {message}"
    assert "3x" in message  # Leverage is shown in format "at 3x"


@pytest.mark.asyncio
async def test_loan_rejects_unregistered_user():
    """Verify /loan rejects users who haven't registered."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    # User is NOT registered
    player_service.get_player.return_value = None

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 456
    interaction.response.send_message = AsyncMock()

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )
    await commands.loan.callback(commands, interaction, amount=50)

    # Should reject with registration message
    interaction.response.send_message.assert_awaited_once()
    call_kwargs = interaction.response.send_message.call_args.kwargs
    assert "register" in call_kwargs.get("content", interaction.response.send_message.call_args.args[0]).lower()
    assert call_kwargs.get("ephemeral") is True

    # Loan service should NOT be called
    loan_service.can_take_loan.assert_not_called()
    loan_service.take_loan.assert_not_called()


@pytest.mark.asyncio
async def test_loan_checks_eligibility_for_registered_user():
    """Verify /loan checks eligibility for registered users."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    # User IS registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")

    # Loan is allowed
    loan_service.can_take_loan.return_value = {"allowed": True}
    loan_service.take_loan.return_value = {
        "success": True,
        "amount": 50,
        "fee": 10,
        "total_owed": 60,
        "new_balance": 40,
        "nonprofit_total": 10,
        "total_loans_taken": 1,
        "was_negative_loan": False,
    }

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 456
    interaction.response.send_message = AsyncMock()

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )
    await commands.loan.callback(commands, interaction, amount=50)

    # Should check eligibility
    loan_service.can_take_loan.assert_called_once_with(456, 50)
    # Should take the loan
    loan_service.take_loan.assert_called_once_with(456, 50, 123)


@pytest.mark.asyncio
async def test_disburse_status_no_active_proposal():
    """Verify /disburse status returns ephemeral message when no proposal exists."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    disburse_service = MagicMock()

    # No active proposal
    disburse_service.get_proposal.return_value = None

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.response.send_message = AsyncMock()

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, disburse_service=disburse_service
    )
    await commands._disburse_status(interaction, guild_id=123)

    interaction.response.send_message.assert_awaited_once()
    call_args = interaction.response.send_message.call_args
    # Message may be passed as positional or keyword arg
    message = call_args.args[0] if call_args.args else call_args.kwargs.get("content", "")
    assert "No active disbursement proposal" in message
    assert call_args.kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_disburse_status_deletes_old_message_and_creates_new():
    """Verify /disburse status deletes old message and creates new one with view."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    disburse_service = MagicMock()

    # Active proposal with stored message
    proposal = MagicMock()
    proposal.message_id = 111
    proposal.channel_id = 222
    proposal.fund_amount = 300
    proposal.quorum_required = 2
    proposal.total_votes = 1
    proposal.quorum_reached = False
    proposal.quorum_progress = 0.5
    proposal.votes = {"even": 1, "proportional": 0, "neediest": 0, "stimulus": 0}
    disburse_service.get_proposal.return_value = proposal

    # Mock old message deletion
    old_message = MagicMock()
    old_message.delete = AsyncMock()
    old_channel = MagicMock()
    old_channel.fetch_message = AsyncMock(return_value=old_message)
    bot.get_channel.return_value = old_channel

    # Mock new message creation
    new_message = MagicMock()
    new_message.id = 333

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.channel_id = 444
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=new_message)

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, disburse_service=disburse_service
    )
    await commands._disburse_status(interaction, guild_id=123)

    # Old message should be deleted
    old_channel.fetch_message.assert_awaited_once_with(111)
    old_message.delete.assert_awaited_once()

    # New message should be sent with embed and view
    interaction.response.send_message.assert_awaited_once()
    call_kwargs = interaction.response.send_message.call_args.kwargs
    assert "embed" in call_kwargs
    assert "view" in call_kwargs

    # Message reference should be updated
    disburse_service.set_proposal_message.assert_called_once_with(123, 333, 444)


@pytest.mark.asyncio
async def test_disburse_status_handles_missing_old_message():
    """Verify /disburse status handles case where old message was already deleted."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    disburse_service = MagicMock()

    # Active proposal with stored message
    proposal = MagicMock()
    proposal.message_id = 111
    proposal.channel_id = 222
    proposal.fund_amount = 300
    proposal.quorum_required = 2
    proposal.total_votes = 1
    proposal.quorum_reached = False
    proposal.quorum_progress = 0.5
    proposal.votes = {"even": 1, "proportional": 0, "neediest": 0, "stimulus": 0}
    disburse_service.get_proposal.return_value = proposal

    # Old message fetch raises NotFound
    old_channel = MagicMock()
    old_channel.fetch_message = AsyncMock(
        side_effect=discord.NotFound(MagicMock(), "not found")
    )
    bot.get_channel.return_value = old_channel

    # Mock new message creation
    new_message = MagicMock()
    new_message.id = 333

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.channel_id = 444
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=new_message)

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, disburse_service=disburse_service
    )

    # Should not raise, should continue to create new message
    await commands._disburse_status(interaction, guild_id=123)

    # New message should still be sent
    interaction.response.send_message.assert_awaited_once()
    call_kwargs = interaction.response.send_message.call_args.kwargs
    assert "embed" in call_kwargs
    assert "view" in call_kwargs

    # Message reference should be updated
    disburse_service.set_proposal_message.assert_called_once_with(123, 333, 444)


@pytest.mark.asyncio
async def test_disburse_status_no_stored_message():
    """Verify /disburse status works when proposal has no stored message yet."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    disburse_service = MagicMock()

    # Active proposal WITHOUT stored message (e.g., first time viewing)
    proposal = MagicMock()
    proposal.message_id = None
    proposal.channel_id = None
    proposal.fund_amount = 300
    proposal.quorum_required = 2
    proposal.total_votes = 0
    proposal.quorum_reached = False
    proposal.quorum_progress = 0.0
    proposal.votes = {"even": 0, "proportional": 0, "neediest": 0, "stimulus": 0}
    disburse_service.get_proposal.return_value = proposal

    # Mock new message creation
    new_message = MagicMock()
    new_message.id = 333

    interaction = MagicMock()
    interaction.guild.id = 123
    interaction.channel_id = 444
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=new_message)

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, disburse_service=disburse_service
    )
    await commands._disburse_status(interaction, guild_id=123)

    # Should NOT try to delete anything (no message to delete)
    bot.get_channel.assert_not_called()

    # New message should be sent with embed and view
    interaction.response.send_message.assert_awaited_once()
    call_kwargs = interaction.response.send_message.call_args.kwargs
    assert "embed" in call_kwargs
    assert "view" in call_kwargs

    # Message reference should be updated
    disburse_service.set_proposal_message.assert_called_once_with(123, 333, 444)
