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


@pytest.mark.asyncio
async def test_tip_successful_transfer():
    """Verify /tip successfully transfers jopacoin between players with fee to nonprofit."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()
    loan_service = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 123 else recipient if discord_id == 456 else None
    )
    player_service.get_balance.return_value = 100  # Sender has 100 coins

    # No outstanding loan
    loan_state = MagicMock()
    loan_state.has_outstanding_loan = False
    loan_service.get_state.return_value = loan_state

    # Mock the atomic tip method
    player_service.player_repo.tip_atomic = MagicMock(return_value={
        "amount": 50,
        "fee": 1,
        "from_new_balance": 49,
        "to_new_balance": 50,
    })

    # Create a proper guild mock for rate limiter
    guild = MagicMock()
    guild.id = 789

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 123
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 456
    recipient_member.mention = "<@456>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service,
        tip_repository=tip_repository, loan_service=loan_service
    )
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should call atomic tip method with correct args
    player_service.player_repo.tip_atomic.assert_called_once_with(
        from_discord_id=123,
        to_discord_id=456,
        amount=50,
        fee=1,
    )

    # Should add fee to nonprofit fund
    loan_service.add_to_nonprofit_fund.assert_called_once_with(789, 1)

    # Should log the transaction
    tip_repository.log_tip.assert_called_once_with(
        sender_id=123,
        recipient_id=456,
        amount=50,
        fee=1,
        guild_id=789,
    )

    # Should send success message with fee to nonprofit info
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "tipped" in message.lower()
    assert "50" in message
    assert "nonprofit" in message.lower()
    assert call_kwargs.get("ephemeral") is False  # Public message


@pytest.mark.asyncio
async def test_tip_insufficient_balance():
    """Verify /tip rejects when sender has insufficient balance (including fee)."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 123 else recipient if discord_id == 456 else None
    )
    player_service.get_balance.return_value = 20  # Sender only has 20 coins

    player_service.player_repo.add_balance_many = MagicMock()

    # Create a proper guild mock for rate limiter
    guild = MagicMock()
    guild.id = 789

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 123
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 456

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should NOT transfer
    player_service.player_repo.add_balance_many.assert_not_called()

    # Should send error message showing fee
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "insufficient" in message.lower()
    # Message should show total cost (50 + 1 fee = 51)
    assert "51" in message
    assert "fee" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_sender_not_registered():
    """Verify /tip rejects when sender is not registered."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Sender is NOT registered
    player_service.get_player.return_value = None

    # Create a proper guild mock for rate limiter
    guild = MagicMock()
    guild.id = 789

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 123
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 456

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should send registration error
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "register" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_recipient_not_registered():
    """Verify /tip rejects when recipient is not registered."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Sender is registered, recipient is NOT
    sender = MagicMock(name="Sender")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 123 else None
    )

    # Create a proper guild mock for rate limiter
    guild = MagicMock()
    guild.id = 789

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 123
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 456
    recipient_member.mention = "<@456>"

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should send error that recipient is not registered
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "not registered" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_self_not_allowed():
    """Verify /tip rejects when trying to tip yourself."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Create a proper guild mock for rate limiter
    # Use unique IDs to avoid rate limiting from previous tests
    guild = MagicMock()
    guild.id = 785

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 125
    interaction.user.mention = "<@125>"
    interaction.response.defer = AsyncMock()
    interaction.response.send_message = AsyncMock()
    interaction.followup.send = AsyncMock()

    # Trying to tip self
    recipient_member = MagicMock()
    recipient_member.id = 125  # Same as sender

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should defer first
    interaction.response.defer.assert_awaited_once_with(ephemeral=False)

    # Should send error
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "tip yourself" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_negative_amount():
    """Verify /tip rejects negative amounts."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Create a proper guild mock for rate limiter
    # Use unique IDs to avoid rate limiting from previous tests
    guild = MagicMock()
    guild.id = 786

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 126
    interaction.user.mention = "<@126>"
    interaction.response.defer = AsyncMock()
    interaction.response.send_message = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 457

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=-10)

    # Should defer first
    interaction.response.defer.assert_awaited_once_with(ephemeral=False)

    # Should send error
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "positive" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_with_outstanding_loan_blocked():
    """Verify /tip rejects when sender has outstanding loan."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 130 else recipient if discord_id == 460 else None
    )
    player_service.get_balance.return_value = 100  # Sender has 100 coins

    # Sender has outstanding loan
    loan_state = MagicMock()
    loan_state.has_outstanding_loan = True
    loan_state.outstanding_total = 60
    loan_service.get_state.return_value = loan_state

    player_service.player_repo.add_balance_many = MagicMock()

    guild = MagicMock()
    guild.id = 791

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 130
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 460
    recipient_member.mention = "<@460>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should NOT transfer
    player_service.player_repo.add_balance_many.assert_not_called()

    # Should send error about outstanding loan
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "outstanding loan" in message.lower()
    assert "60" in message  # Shows outstanding amount
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_fee_calculation_minimum_1_coin():
    """Verify /tip charges minimum 1 coin fee even for small tips."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 131 else recipient if discord_id == 461 else None
    )
    player_service.get_balance.return_value = 10  # Sender has 10 coins

    # Mock the atomic tip method
    player_service.player_repo.tip_atomic = MagicMock(return_value={
        "amount": 1,
        "fee": 1,
        "from_new_balance": 8,
        "to_new_balance": 1,
    })

    guild = MagicMock()
    guild.id = 792

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 131
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 461
    recipient_member.mention = "<@461>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, tip_repository=tip_repository
    )
    # Tip 1 coin - 1% = 0.01, but minimum is 1
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=1)

    # Should call atomic tip with minimum fee of 1
    player_service.player_repo.tip_atomic.assert_called_once_with(
        from_discord_id=131,
        to_discord_id=461,
        amount=1,
        fee=1,
    )

    # Should log with fee=1
    tip_repository.log_tip.assert_called_once()
    call_kwargs = tip_repository.log_tip.call_args.kwargs
    assert call_kwargs["fee"] == 1


@pytest.mark.asyncio
async def test_tip_fee_calculation_percentage():
    """Verify /tip calculates fee correctly for larger amounts."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 132 else recipient if discord_id == 462 else None
    )
    player_service.get_balance.return_value = 300  # Sender has 300 coins

    # Mock the atomic tip method
    player_service.player_repo.tip_atomic = MagicMock(return_value={
        "amount": 150,
        "fee": 2,
        "from_new_balance": 148,
        "to_new_balance": 150,
    })

    guild = MagicMock()
    guild.id = 793

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 132
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 462
    recipient_member.mention = "<@462>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, tip_repository=tip_repository
    )
    # Tip 150 coins - 1% = 1.5, ceil = 2
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=150)

    # Should call atomic tip with fee=2 (ceil(150 * 0.01))
    player_service.player_repo.tip_atomic.assert_called_once_with(
        from_discord_id=132,
        to_discord_id=462,
        amount=150,
        fee=2,
    )

    # Should log with fee=2
    tip_repository.log_tip.assert_called_once()
    call_kwargs = tip_repository.log_tip.call_args.kwargs
    assert call_kwargs["fee"] == 2


@pytest.mark.asyncio
async def test_tip_fee_goes_to_nonprofit():
    """Verify /tip sends fee to nonprofit fund."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()
    loan_service = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 133 else recipient if discord_id == 463 else None
    )
    player_service.get_balance.return_value = 200  # Sender has 200 coins

    # No outstanding loan
    loan_state = MagicMock()
    loan_state.has_outstanding_loan = False
    loan_service.get_state.return_value = loan_state

    # Mock the atomic tip method
    player_service.player_repo.tip_atomic = MagicMock(return_value={
        "amount": 100,
        "fee": 1,
        "from_new_balance": 99,
        "to_new_balance": 100,
    })

    guild = MagicMock()
    guild.id = 794

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 133
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 463
    recipient_member.mention = "<@463>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service,
        tip_repository=tip_repository, loan_service=loan_service
    )
    # Tip 100 coins - 1% = 1 fee
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=100)

    # Verify atomic transfer
    player_service.player_repo.tip_atomic.assert_called_once_with(
        from_discord_id=133,
        to_discord_id=463,
        amount=100,
        fee=1,
    )

    # Verify fee goes to nonprofit fund
    loan_service.add_to_nonprofit_fund.assert_called_once_with(794, 1)


@pytest.mark.asyncio
async def test_tip_transaction_logged():
    """Verify /tip logs the transaction to tip_repository."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 134 else recipient if discord_id == 464 else None
    )
    player_service.get_balance.return_value = 500  # Sender has 500 coins

    # Mock the atomic tip method
    player_service.player_repo.tip_atomic = MagicMock(return_value={
        "amount": 250,
        "fee": 3,
        "from_new_balance": 247,
        "to_new_balance": 250,
    })

    guild = MagicMock()
    guild.id = 795

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 134
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 464
    recipient_member.mention = "<@464>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, tip_repository=tip_repository
    )
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=250)

    # Should call atomic tip
    player_service.player_repo.tip_atomic.assert_called_once_with(
        from_discord_id=134,
        to_discord_id=464,
        amount=250,
        fee=3,
    )

    # Should log transaction with correct values
    # 250 * 0.01 = 2.5, ceil = 3
    tip_repository.log_tip.assert_called_once_with(
        sender_id=134,
        recipient_id=464,
        amount=250,
        fee=3,
        guild_id=795,
    )


@pytest.mark.asyncio
async def test_tip_unexpected_error_handling():
    """Verify /tip handles unexpected errors gracefully."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 137 else recipient if discord_id == 467 else None
    )
    player_service.get_balance.return_value = 100  # Sender has 100 coins

    # Mock tip_atomic to raise an unexpected exception (not ValueError)
    player_service.player_repo.tip_atomic = MagicMock(
        side_effect=RuntimeError("Database connection lost")
    )

    guild = MagicMock()
    guild.id = 798

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 137
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 467
    recipient_member.mention = "<@467>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, tip_repository=tip_repository
    )
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should NOT log the transaction (transfer failed)
    tip_repository.log_tip.assert_not_called()

    # Should send generic error message
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "failed" in message.lower()
    assert "try again" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_value_error_handling():
    """Verify /tip handles ValueError from tip_atomic (insufficient funds, not found)."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 138 else recipient if discord_id == 468 else None
    )
    player_service.get_balance.return_value = 100  # Sender has 100 coins

    # Mock tip_atomic to raise ValueError (insufficient funds detected atomically)
    player_service.player_repo.tip_atomic = MagicMock(
        side_effect=ValueError("Insufficient balance. You need 51 (tip: 50, fee: 1). You have 49.")
    )

    guild = MagicMock()
    guild.id = 799

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 138
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 468
    recipient_member.mention = "<@468>"

    commands = BettingCommands(
        bot, betting_service, match_service, player_service, tip_repository=tip_repository
    )
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should NOT log the transaction (transfer failed)
    tip_repository.log_tip.assert_not_called()

    # Should send the ValueError message directly
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "insufficient" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_tip_no_loan_service_still_works():
    """Verify /tip works when loan_service is not configured."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    tip_repository = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 135 else recipient if discord_id == 465 else None
    )
    player_service.get_balance.return_value = 100  # Sender has 100 coins

    # Mock the atomic tip method
    player_service.player_repo.tip_atomic = MagicMock(return_value={
        "amount": 50,
        "fee": 1,
        "from_new_balance": 49,
        "to_new_balance": 50,
    })

    guild = MagicMock()
    guild.id = 796

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 135
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 465
    recipient_member.mention = "<@465>"

    # No loan_service
    commands = BettingCommands(
        bot, betting_service, match_service, player_service,
        loan_service=None, tip_repository=tip_repository
    )
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should still work and call atomic tip
    player_service.player_repo.tip_atomic.assert_called_once_with(
        from_discord_id=135,
        to_discord_id=465,
        amount=50,
        fee=1,
    )
    interaction.followup.send.assert_awaited_once()


@pytest.mark.asyncio
async def test_tip_insufficient_balance_boundary():
    """Verify /tip rejects when balance is exactly equal to tip (but not fee)."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Both players are registered
    sender = MagicMock(name="Sender")
    recipient = MagicMock(name="Recipient")
    player_service.get_player.side_effect = lambda discord_id: (
        sender if discord_id == 136 else recipient if discord_id == 466 else None
    )
    # Sender has exactly 50 coins, but needs 51 (50 + 1 fee)
    player_service.get_balance.return_value = 50

    player_service.player_repo.add_balance_many = MagicMock()

    guild = MagicMock()
    guild.id = 797

    interaction = MagicMock()
    interaction.guild = guild
    interaction.user.id = 136
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock()

    recipient_member = MagicMock()
    recipient_member.id = 466

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.tip.callback(commands, interaction, player=recipient_member, amount=50)

    # Should NOT transfer (50 < 51)
    player_service.player_repo.add_balance_many.assert_not_called()

    # Should send error about insufficient balance
    interaction.followup.send.assert_awaited_once()
    call_kwargs = interaction.followup.send.call_args.kwargs
    message = call_kwargs.get("content", interaction.followup.send.call_args.args[0])
    assert "insufficient" in message.lower()
    assert call_kwargs.get("ephemeral") is True
