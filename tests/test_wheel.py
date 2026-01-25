"""Tests for the Wheel of Fortune /gamba command."""
import time
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from commands.betting import BettingCommands, WHEEL_WEDGES
from config import WHEEL_COOLDOWN_SECONDS


@pytest.mark.asyncio
async def test_wheel_requires_registration():
    """Verify /gamba rejects unregistered users."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is NOT registered
    player_service.get_player.return_value = None

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 456
    interaction.response.send_message = AsyncMock()

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    await commands.gamba.callback(commands, interaction)

    # Should reject with registration message
    interaction.response.send_message.assert_awaited_once()
    call_kwargs = interaction.response.send_message.call_args.kwargs
    message = call_kwargs.get("content", interaction.response.send_message.call_args.args[0])
    assert "register" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_wheel_cooldown_enforced():
    """Verify /gamba enforces 24-hour cooldown."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 789
    interaction.response.send_message = AsyncMock()

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Set cooldown to "just used"
    commands._gamba_cooldowns[789] = time.time()

    await commands.gamba.callback(commands, interaction)

    # Should reject with cooldown message
    interaction.response.send_message.assert_awaited_once()
    call_kwargs = interaction.response.send_message.call_args.kwargs
    message = call_kwargs.get("content", interaction.response.send_message.call_args.args[0])
    assert "already" in message.lower() or "spun" in message.lower()
    assert call_kwargs.get("ephemeral") is True


@pytest.mark.asyncio
async def test_wheel_cooldown_expired_allows_spin():
    """Verify /gamba allows spin when cooldown has expired."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1001
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Set cooldown to expired (more than 24 hours ago)
    commands._gamba_cooldowns[1001] = time.time() - WHEEL_COOLDOWN_SECONDS - 1

    # Mock random to get a predictable result (index 3 = "5 JC")
    with patch("commands.betting.random.randint", return_value=3):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    # Should respond (not reject)
    interaction.response.send_message.assert_awaited_once()
    call_kwargs = interaction.response.send_message.call_args.kwargs
    # Should have an embed, not ephemeral rejection
    assert "embed" in call_kwargs


@pytest.mark.asyncio
async def test_wheel_positive_applies_garnishment():
    """Verify positive wheel results go through garnishment service when in debt."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    garnishment_service = MagicMock()

    # User is registered and in debt
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = -100  # In debt

    # Set up garnishment service
    garnishment_service.add_income.return_value = {
        "garnished": 30,
        "new_balance": -70,
    }
    bot.garnishment_service = garnishment_service

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1002
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get a positive result (index 13 = "30 JC")
    # WHEEL_WEDGES[13] = ("30 JC", 30, "💎")
    with patch("commands.betting.random.randint", return_value=13):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    # Should call garnishment service
    garnishment_service.add_income.assert_called_once_with(1002, 30)


@pytest.mark.asyncio
async def test_wheel_positive_no_debt_adds_directly():
    """Verify positive wheel results add directly when not in debt."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and NOT in debt
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50  # Not in debt

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    # No garnishment service on bot
    bot.garnishment_service = None

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1003
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get a positive result (index 3 = "5 JC")
    with patch("commands.betting.random.randint", return_value=3):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    # Should add balance directly
    player_service.player_repo.add_balance.assert_called_once_with(1003, 5)


@pytest.mark.asyncio
async def test_wheel_bankrupt_subtracts_balance():
    """Verify Bankrupt wedge subtracts 100 JC from balance."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1004
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get Bankrupt (index 0)
    # WHEEL_WEDGES[0] = ("BANKRUPT", -100, "💀")
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    # Should subtract 100 (ignoring MAX_DEBT floor)
    player_service.player_repo.add_balance.assert_called_once_with(1004, -100)


@pytest.mark.asyncio
async def test_wheel_bankrupt_ignores_max_debt():
    """Verify Bankrupt can push balance below MAX_DEBT floor."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and already at -400 (near MAX_DEBT of 500)
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    # Balance will be -400, then -500 after bankrupt
    player_service.get_balance.side_effect = [-400, -500]

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1005
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get Bankrupt (index 0)
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    # Should subtract 100 regardless of MAX_DEBT
    player_service.player_repo.add_balance.assert_called_once_with(1005, -100)


@pytest.mark.asyncio
async def test_wheel_lose_turn_no_change():
    """Verify 'Lose a Turn' wedge doesn't change balance."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 75

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1006
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get "Lose a Turn" (index 2)
    # WHEEL_WEDGES[2] = ("LOSE A TURN", 0, "😐")
    with patch("commands.betting.random.randint", return_value=2):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    # Should NOT call add_balance at all
    player_service.player_repo.add_balance.assert_not_called()


@pytest.mark.asyncio
async def test_wheel_jackpot_result():
    """Verify Jackpot wedge awards 100 JC."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and NOT in debt
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1007
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get Jackpot (index 22)
    # WHEEL_WEDGES[22] = ("JACKPOT!", 100, "🌟")
    with patch("commands.betting.random.randint", return_value=22):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    # Should add 100
    player_service.player_repo.add_balance.assert_called_once_with(1007, 100)


def test_wheel_wedges_has_correct_count():
    """Verify WHEEL_WEDGES has exactly 24 wedges."""
    assert len(WHEEL_WEDGES) == 24


def test_wheel_wedges_distribution():
    """Verify the distribution of wheel wedges matches spec."""
    bankrupt_count = sum(1 for w in WHEEL_WEDGES if w[1] == -100)
    lose_turn_count = sum(1 for w in WHEEL_WEDGES if w[1] == 0)
    small_count = sum(1 for w in WHEEL_WEDGES if 5 <= w[1] <= 10)
    medium_count = sum(1 for w in WHEEL_WEDGES if 15 <= w[1] <= 25)
    good_count = sum(1 for w in WHEEL_WEDGES if 30 <= w[1] <= 50)
    great_count = sum(1 for w in WHEEL_WEDGES if 60 <= w[1] <= 80)
    jackpot_count = sum(1 for w in WHEEL_WEDGES if w[1] == 100)

    assert bankrupt_count == 2, f"Expected 2 Bankrupt wedges, got {bankrupt_count}"
    assert lose_turn_count == 1, f"Expected 1 Lose a Turn wedge, got {lose_turn_count}"
    assert small_count == 4, f"Expected 4 small win wedges, got {small_count}"
    assert medium_count == 6, f"Expected 6 medium win wedges, got {medium_count}"
    assert good_count == 6, f"Expected 6 good win wedges, got {good_count}"
    assert great_count == 3, f"Expected 3 great win wedges, got {great_count}"
    assert jackpot_count == 2, f"Expected 2 Jackpot wedges, got {jackpot_count}"


def test_wheel_expected_value_is_positive():
    """Verify the expected value of the wheel is positive (~25 JC)."""
    total_value = sum(w[1] for w in WHEEL_WEDGES)
    expected_value = total_value / len(WHEEL_WEDGES)

    # Expected value should be around 25 JC (positive)
    assert expected_value > 0, f"Expected positive EV, got {expected_value}"
    assert 20 <= expected_value <= 30, f"Expected EV around 25, got {expected_value}"


@pytest.mark.asyncio
async def test_wheel_animation_frame_count():
    """Verify the wheel animation has 5 frame edits."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1008
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    with patch("commands.betting.random.randint", return_value=5):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock) as mock_sleep:
            await commands.gamba.callback(commands, interaction)

    # Should have 5 animation frame sleeps + 1 final result sleep
    assert mock_sleep.await_count == 6

    # Should edit the message 5 times for animation + 1 for final result
    assert message.edit.await_count == 6


@pytest.mark.asyncio
async def test_wheel_updates_cooldown():
    """Verify the wheel updates cooldown on spin."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1009
    interaction.response.send_message = AsyncMock()
    interaction.original_response = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # No cooldown set initially
    assert 1009 not in commands._gamba_cooldowns

    before_time = time.time()

    with patch("commands.betting.random.randint", return_value=5):
        with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
            await commands.gamba.callback(commands, interaction)

    after_time = time.time()

    # Cooldown should now be set
    assert 1009 in commands._gamba_cooldowns
    assert before_time <= commands._gamba_cooldowns[1009] <= after_time
