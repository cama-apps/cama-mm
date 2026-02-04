"""Tests for the Wheel of Fortune /gamba command."""
import time
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from commands.betting import BettingCommands
from utils.wheel_drawing import WHEEL_WEDGES
from config import WHEEL_COOLDOWN_SECONDS, WHEEL_TARGET_EV


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
async def test_wheel_cooldown_expired_allows_spin():
    """Verify /gamba allows spin when cooldown has expired."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository - cooldown expired
    player_service.player_repo = MagicMock()
    player_service.player_repo.get_last_wheel_spin.return_value = int(time.time()) - WHEEL_COOLDOWN_SECONDS - 1
    player_service.player_repo.add_balance = MagicMock()
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1001
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get a predictable result (index 3 = "5") and disable explosion
    with patch("commands.betting.random.randint", return_value=3):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should defer then send via followup
    interaction.response.defer.assert_awaited_once()
    interaction.followup.send.assert_awaited_once()
    # Should have a file attachment (GIF)
    call_kwargs = interaction.followup.send.call_args.kwargs
    assert "file" in call_kwargs


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

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)

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
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get a positive result (index 13 = "30") and disable explosion
    with patch("commands.betting.random.randint", return_value=13):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should call garnishment service (user_id, amount, guild_id)
    garnishment_service.add_income.assert_called_once_with(1002, 30, 123)


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
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    # No garnishment service on bot
    bot.garnishment_service = None

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1003
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get a positive result (index 3 = "5") and disable explosion
    with patch("commands.betting.random.randint", return_value=3):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should add balance directly (user_id, guild_id, amount)
    player_service.player_repo.add_balance.assert_called_once_with(1003, 123, 5)


@pytest.mark.asyncio
async def test_wheel_bankrupt_subtracts_balance():
    """Verify Bankrupt wedge subtracts from balance (value based on EV config)."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1004
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get Bankrupt (index 0) and disable explosion
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should subtract the bankrupt value (negative)
    bankrupt_value = WHEEL_WEDGES[0][1]
    assert bankrupt_value < 0, "Bankrupt should have negative value"
    player_service.player_repo.add_balance.assert_called_once_with(1004, 123, bankrupt_value)


@pytest.mark.asyncio
async def test_wheel_bankrupt_ignores_max_debt():
    """Verify Bankrupt can push balance below MAX_DEBT floor."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and already at -400 (near MAX_DEBT of 500)
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    bankrupt_value = WHEEL_WEDGES[0][1]
    # Balance will be -400, then more negative after bankrupt
    player_service.get_balance.side_effect = [-400, -400 + bankrupt_value]

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1005
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get Bankrupt (index 0) and disable explosion
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should subtract bankrupt value regardless of MAX_DEBT
    player_service.player_repo.add_balance.assert_called_once_with(1005, 123, bankrupt_value)


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
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1006
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get "Lose a Turn" (index 2) and disable explosion
    with patch("commands.betting.random.randint", return_value=2):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
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
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1007
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to get Jackpot (index 22) and disable explosion
    with patch("commands.betting.random.randint", return_value=22):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should add 100
    player_service.player_repo.add_balance.assert_called_once_with(1007, 123, 100)


def test_wheel_wedges_has_correct_count():
    """Verify WHEEL_WEDGES has exactly 24 wedges."""
    assert len(WHEEL_WEDGES) == 24


def test_wheel_wedges_distribution():
    """Verify the distribution of wheel wedges matches spec."""
    # Bankrupt wedges have negative values
    bankrupt_count = sum(1 for w in WHEEL_WEDGES if w[1] < 0)
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


def test_wheel_expected_value_matches_config():
    """Verify the expected value of the wheel matches WHEEL_TARGET_EV config."""
    total_value = sum(w[1] for w in WHEEL_WEDGES)
    expected_value = total_value / len(WHEEL_WEDGES)

    # EV should be close to the configured target (within 1 due to integer rounding)
    assert abs(expected_value - WHEEL_TARGET_EV) <= 1, f"Expected EV ~{WHEEL_TARGET_EV}, got {expected_value}"


def test_wheel_bankrupt_always_negative():
    """Verify BANKRUPT wedges are always negative (capped at -1 minimum)."""
    bankrupt_wedges = [w for w in WHEEL_WEDGES if w[1] < 0]
    assert len(bankrupt_wedges) == 2, "Should have exactly 2 bankrupt wedges"
    for w in bankrupt_wedges:
        assert w[1] <= -1, f"Bankrupt value {w[1]} should be <= -1"


@pytest.mark.asyncio
async def test_wheel_animation_uses_gif():
    """Verify the wheel animation uses a single GIF upload."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    player_service.player_repo = MagicMock()
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1008
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    with patch("commands.betting.random.randint", return_value=5):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock) as mock_sleep:
                await commands.gamba.callback(commands, interaction)

    # GIF animation: 1 sleep for animation + 1 sleep before result
    assert mock_sleep.await_count == 2

    # Should only edit once (for final result embed)
    assert message.edit.await_count == 1


@pytest.mark.asyncio
async def test_wheel_updates_cooldown_in_database():
    """Verify the wheel updates cooldown in database on spin."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository - no previous spin
    player_service.player_repo = MagicMock()
    player_service.player_repo.get_last_wheel_spin.return_value = None
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1009
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    before_time = int(time.time())

    with patch("commands.betting.random.randint", return_value=5):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    after_time = int(time.time())

    # Should have called set_last_wheel_spin with (user_id, guild_id, timestamp)
    player_service.player_repo.set_last_wheel_spin.assert_called_once()
    call_args = player_service.player_repo.set_last_wheel_spin.call_args[0]
    assert call_args[0] == 1009  # user_id
    assert call_args[1] == 123  # guild_id
    assert before_time <= call_args[2] <= after_time  # timestamp


@pytest.mark.asyncio
async def test_wheel_admin_bypasses_cooldown():
    """Verify admins can bypass wheel cooldown."""
    bot = MagicMock()
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository - cooldown was just set
    player_service.player_repo = MagicMock()
    player_service.player_repo.get_last_wheel_spin.return_value = int(time.time())
    player_service.player_repo.set_last_wheel_spin = MagicMock()
    player_service.player_repo.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.player_repo.log_wheel_spin = MagicMock(return_value=1)
    player_service.player_repo.add_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 789
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock admin check to return True
    with patch("commands.betting.has_admin_permission", return_value=True):
        with patch("commands.betting.random.randint", return_value=5):
            with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    # Admin should be able to spin despite cooldown - file attachment means spin happened
    call_kwargs = interaction.followup.send.call_args.kwargs
    assert "file" in call_kwargs
