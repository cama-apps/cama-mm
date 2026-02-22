"""Tests for the Wheel of Fortune /gamba command."""
import time
from unittest.mock import AsyncMock, MagicMock, patch

import discord
import pytest

from commands.betting import BettingCommands
from utils.wheel_drawing import WHEEL_WEDGES
from config import (
    WHEEL_BLUE_SHELL_EST_EV,
    WHEEL_COOLDOWN_SECONDS,
    WHEEL_LIGHTNING_BOLT_EST_EV,
    WHEEL_RED_SHELL_EST_EV,
    WHEEL_TARGET_EV,
)


@pytest.mark.asyncio
async def test_wheel_requires_registration():
    """Verify /gamba rejects unregistered users."""
    bot = MagicMock()
    bot.bankruptcy_service = None
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
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods - cooldown expired
    player_service.get_last_wheel_spin = MagicMock(return_value=int(time.time()) - WHEEL_COOLDOWN_SECONDS - 1)
    player_service.adjust_balance = MagicMock()
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)

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
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=3):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
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
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    garnishment_service = MagicMock()

    # User is registered and in debt
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = -100  # In debt

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)

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

    # Find a positive value wedge dynamically (30)
    target_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 30)

    # Mock _create_wheel_gif_file to avoid GIF generation calling random.randint
    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=target_idx):
            with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    # Should call garnishment service (user_id, amount, guild_id)
    garnishment_service.add_income.assert_called_once_with(1002, 30, 123)


@pytest.mark.asyncio
async def test_wheel_positive_no_debt_adds_directly():
    """Verify positive wheel results add directly when not in debt."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and NOT in debt
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50  # Not in debt

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

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
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=3):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Should add balance directly (user_id, guild_id, amount)
    player_service.adjust_balance.assert_called_once_with(1003, 123, 5)


@pytest.mark.asyncio
async def test_wheel_bankrupt_subtracts_balance():
    """Verify Bankrupt wedge subtracts from balance (value based on EV config)."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock repository
    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

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
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Should subtract the bankrupt value (negative)
    bankrupt_value = WHEEL_WEDGES[0][1]
    assert bankrupt_value < 0, "Bankrupt should have negative value"
    player_service.adjust_balance.assert_called_once_with(1004, 123, bankrupt_value)


@pytest.mark.asyncio
async def test_wheel_bankrupt_credits_nonprofit_fund():
    """Verify Bankrupt wedge losses are credited to the nonprofit fund."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1004
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    # Mock random to get Bankrupt (index 0) and disable explosion
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    # Should credit the nonprofit fund with the absolute loss value
    bankrupt_value = WHEEL_WEDGES[0][1]
    loan_service.add_to_nonprofit_fund.assert_called_once_with(123, abs(int(bankrupt_value)))


@pytest.mark.asyncio
async def test_wheel_bankrupt_ignores_max_debt():
    """Verify Bankrupt can push balance below MAX_DEBT floor."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and already at -400 (near MAX_DEBT of 500)
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    bankrupt_value = WHEEL_WEDGES[0][1]
    # Balance will be -400, then more negative after bankrupt
    player_service.get_balance.side_effect = [-400, -400 + bankrupt_value]

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

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
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Should subtract bankrupt value regardless of MAX_DEBT
    player_service.adjust_balance.assert_called_once_with(1005, 123, bankrupt_value)


@pytest.mark.asyncio
async def test_wheel_lose_turn_no_change():
    """Verify 'Lose a Turn' wedge doesn't change balance."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 75

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

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
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=2):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Should NOT call adjust_balance at all
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_wheel_jackpot_result():
    """Verify Jackpot wedge awards 100 JC."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and NOT in debt
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1007
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Find jackpot (100) wedge dynamically
    jackpot_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 100)

    # Mock _create_wheel_gif_file to avoid GIF generation calling random.randint
    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=jackpot_idx):
            with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    # Should add 100
    player_service.adjust_balance.assert_called_once_with(1007, 123, 100)


def test_wheel_wedges_has_correct_count():
    """Verify WHEEL_WEDGES has exactly 24 wedges (22 base + 2 shells)."""
    assert len(WHEEL_WEDGES) == 24


def test_wheel_wedges_distribution():
    """Verify the distribution of wheel wedges matches spec."""
    # Bankrupt wedges have negative integer values
    bankrupt_count = sum(1 for w in WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0)
    lose_turn_count = sum(1 for w in WHEEL_WEDGES if w[1] == 0)
    small_count = sum(1 for w in WHEEL_WEDGES if isinstance(w[1], int) and 5 <= w[1] <= 10)
    medium_count = sum(1 for w in WHEEL_WEDGES if isinstance(w[1], int) and 15 <= w[1] <= 25)
    good_count = sum(1 for w in WHEEL_WEDGES if isinstance(w[1], int) and 30 <= w[1] <= 50)
    great_count = sum(1 for w in WHEEL_WEDGES if isinstance(w[1], int) and 60 <= w[1] <= 80)
    jackpot_count = sum(1 for w in WHEEL_WEDGES if w[1] == 100)
    special_count = sum(1 for w in WHEEL_WEDGES if isinstance(w[1], str))

    assert bankrupt_count == 2, f"Expected 2 Bankrupt wedges, got {bankrupt_count}"
    assert lose_turn_count == 1, f"Expected 1 Lose a Turn wedge, got {lose_turn_count}"
    assert small_count == 4, f"Expected 4 small win wedges, got {small_count}"
    assert medium_count == 5, f"Expected 5 medium win wedges, got {medium_count}"
    assert good_count == 4, f"Expected 4 good win wedges, got {good_count}"
    assert great_count == 3, f"Expected 3 great win wedges, got {great_count}"
    assert jackpot_count == 2, f"Expected 2 Jackpot wedges, got {jackpot_count}"
    assert special_count == 3, f"Expected 3 special wedges, got {special_count}"


def test_wheel_expected_value_matches_config():
    """Verify the expected value of the wheel matches WHEEL_TARGET_EV config.

    Special wedges use configurable estimated EVs for total economic impact:
    - RED_SHELL/BLUE_SHELL: transfers (net ~0) with small nonprofit drain on self-hit
    - LIGHTNING_BOLT: server-wide tax, all to nonprofit sink (large negative)
    BANKRUPT is adjusted so the overall wheel EV hits the target.
    """
    est_evs = {
        "RED_SHELL": WHEEL_RED_SHELL_EST_EV,
        "BLUE_SHELL": WHEEL_BLUE_SHELL_EST_EV,
        "LIGHTNING_BOLT": WHEEL_LIGHTNING_BOLT_EST_EV,
    }
    # Sum integer wedges + estimated EVs for special wedges
    total_value = 0.0
    for _, v, _ in WHEEL_WEDGES:
        if isinstance(v, int):
            total_value += v
        elif isinstance(v, str):
            total_value += est_evs.get(v, 0.0)
    expected_value = total_value / len(WHEEL_WEDGES)

    # EV should be close to the configured target (within 1 due to integer rounding)
    assert abs(expected_value - WHEEL_TARGET_EV) <= 1, f"Expected EV ~{WHEEL_TARGET_EV}, got {expected_value}"


def test_wheel_bankrupt_always_negative():
    """Verify BANKRUPT wedges are always negative (capped at -1 minimum)."""
    bankrupt_wedges = [w for w in WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0]
    assert len(bankrupt_wedges) == 2, "Should have exactly 2 bankrupt wedges"
    for w in bankrupt_wedges:
        assert w[1] <= -1, f"Bankrupt value {w[1]} should be <= -1"


def test_wheel_special_wedges_have_string_values():
    """Verify special wedges have string values for special handling."""
    special_wedges = [w for w in WHEEL_WEDGES if isinstance(w[1], str)]
    assert len(special_wedges) == 3, "Should have exactly 3 special wedges"

    special_values = {w[1] for w in special_wedges}
    assert "RED_SHELL" in special_values, "Should have RED_SHELL wedge"
    assert "BLUE_SHELL" in special_values, "Should have BLUE_SHELL wedge"
    assert "LIGHTNING_BOLT" in special_values, "Should have LIGHTNING_BOLT wedge"


@pytest.mark.asyncio
async def test_wheel_animation_uses_gif():
    """Verify the wheel animation uses a single GIF upload."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1008
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=5):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock) as mock_sleep:
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # GIF animation: 1 sleep for animation + 1 sleep before result
    assert mock_sleep.await_count == 2

    # Should only edit once (for final result embed)
    assert message.edit.await_count == 1


@pytest.mark.asyncio
async def test_wheel_updates_cooldown_in_database():
    """Verify the wheel updates cooldown in database on spin."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods - no previous spin
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

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

    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=5):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    after_time = int(time.time())

    # Should have called set_last_wheel_spin with (user_id, guild_id, timestamp)
    player_service.set_last_wheel_spin.assert_called_once()
    call_args = player_service.set_last_wheel_spin.call_args[0]
    assert call_args[0] == 1009  # user_id
    assert call_args[1] == 123  # guild_id
    assert before_time <= call_args[2] <= after_time  # timestamp


@pytest.mark.asyncio
async def test_wheel_admin_bypasses_cooldown():
    """Verify admins can bypass wheel cooldown."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods - cooldown was just set
    player_service.get_last_wheel_spin = MagicMock(return_value=int(time.time()))
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

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
                    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                        await commands.gamba.callback(commands, interaction)

    # Admin should be able to spin despite cooldown - file attachment means spin happened
    call_kwargs = interaction.followup.send.call_args.kwargs
    assert "file" in call_kwargs


@pytest.mark.asyncio
async def test_wheel_red_shell_steals_from_player_above():
    """Verify Red Shell steals from the player ranked above on leaderboard."""
    from domain.models.player import Player

    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods - player above exists
    player_above = Player(
        name="RicherPlayer",
        discord_id=2001,
        mmr=None,
        initial_mmr=None,
        wins=0,
        losses=0,
        preferred_roles=None,
        main_role=None,
        glicko_rating=None,
        glicko_rd=None,
        glicko_volatility=None,
        jopacoin_balance=100,
    )
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.get_player_above = MagicMock(return_value=player_above)
    player_service.steal_atomic = MagicMock(return_value={
        "amount": 3,
        "thief_new_balance": 53,
        "victim_new_balance": 97,
    })

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.guild.get_member = MagicMock(return_value=MagicMock(mention="@RicherPlayer"))
    interaction.user.id = 1010
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Find RED_SHELL index dynamically
    red_shell_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "RED_SHELL")

    # Mock _create_wheel_gif_file to avoid GIF generation calling random.randint
    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint") as mock_randint:
            # First call: wedge selection, second call: flat amount (2)
            mock_randint.side_effect = [red_shell_idx, 2]
            with patch("commands.betting.random.uniform", return_value=0.03):  # 3% of 100 = 3 JC
                with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                    with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                        await commands.gamba.callback(commands, interaction)

    # Should call get_player_above
    player_service.get_player_above.assert_called_once_with(1010, 123)

    # Should call steal_atomic: max(pct=3, flat=2) = 3 JC
    player_service.steal_atomic.assert_called_once_with(
        thief_discord_id=1010,
        victim_discord_id=2001,
        guild_id=123,
        amount=3,
    )


@pytest.mark.asyncio
async def test_wheel_red_shell_misses_when_first_place():
    """Verify Red Shell misses when user is already in first place."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 1000  # Highest balance

    # Mock service methods - no player above (user is #1)
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.steal_atomic = MagicMock()
    player_service.get_player_above = MagicMock(return_value=None)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1011
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Find RED_SHELL index dynamically
    red_shell_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "RED_SHELL")

    # Mock _create_wheel_gif_file to avoid GIF generation calling random.randint
    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=red_shell_idx):
            with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    # Should NOT call steal_atomic (shell missed)
    player_service.steal_atomic.assert_not_called()


@pytest.mark.asyncio
async def test_wheel_blue_shell_steals_from_richest():
    """Verify Blue Shell steals from the richest player."""
    from domain.models.player import Player

    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered (not the richest)
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50

    # Mock service methods - richest player is someone else
    richest = Player(
        name="RichestPlayer",
        discord_id=3001,
        mmr=None,
        initial_mmr=None,
        wins=0,
        losses=0,
        preferred_roles=None,
        main_role=None,
        glicko_rating=None,
        glicko_rd=None,
        glicko_volatility=None,
        jopacoin_balance=500,
    )
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.get_leaderboard = MagicMock(return_value=[richest])
    player_service.steal_atomic = MagicMock(return_value={
        "amount": 5,
        "thief_new_balance": 55,
        "victim_new_balance": 495,
    })

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.guild.get_member = MagicMock(return_value=MagicMock(mention="@RichestPlayer"))
    interaction.user.id = 1012
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Find BLUE_SHELL index dynamically
    blue_shell_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BLUE_SHELL")

    # Mock _create_wheel_gif_file to avoid GIF generation calling random.randint
    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint") as mock_randint:
            # First call: wedge selection, second call: flat amount (4)
            mock_randint.side_effect = [blue_shell_idx, 4]
            with patch("commands.betting.random.uniform", return_value=0.01):  # 1% of 500 = 5 JC
                with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                    with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                        await commands.gamba.callback(commands, interaction)

    # Should call get_leaderboard
    player_service.get_leaderboard.assert_called_once_with(123, limit=1)

    # Should call steal_atomic: max(pct=5, flat=4) = 5 JC
    player_service.steal_atomic.assert_called_once_with(
        thief_discord_id=1012,
        victim_discord_id=3001,
        guild_id=123,
        amount=5,
    )


@pytest.mark.asyncio
async def test_wheel_blue_shell_self_hit_when_richest():
    """Verify Blue Shell self-hits when user is the richest player."""
    from domain.models.player import Player

    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    # User is registered (and is the richest)
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 500

    # Mock service methods - user is the richest
    user_as_richest = Player(
        name="TestPlayer",
        discord_id=1013,
        mmr=None,
        initial_mmr=None,
        wins=0,
        losses=0,
        preferred_roles=None,
        main_role=None,
        glicko_rating=None,
        glicko_rd=None,
        glicko_volatility=None,
        jopacoin_balance=500,
    )
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()
    player_service.get_leaderboard = MagicMock(return_value=[user_as_richest])

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1013  # Same as richest
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    # Find BLUE_SHELL index dynamically
    blue_shell_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BLUE_SHELL")

    # Mock _create_wheel_gif_file to avoid GIF generation calling random.randint
    with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint") as mock_randint:
            # First call: wedge selection, second call: flat amount (4)
            mock_randint.side_effect = [blue_shell_idx, 4]
            with patch("commands.betting.random.uniform", return_value=0.02):  # 2% of 500 = 10 JC
                with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                    with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                        await cmds.gamba.callback(cmds, interaction)

    # Self-hit uses adjust_balance (not steal_atomic since no victim)
    # max(pct=10, flat=4) = 10 JC loss
    player_service.adjust_balance.assert_called_once_with(1013, 123, -10)

    # Should credit nonprofit fund with the loss
    loan_service.add_to_nonprofit_fund.assert_called_once_with(123, 10)


@pytest.mark.asyncio
async def test_wheel_lightning_bolt_taxes_all_players():
    """Verify Lightning Bolt taxes all players with positive balance and sends to nonprofit."""
    from domain.models.player import Player

    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 200

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    # 3 players with positive balances
    players = [
        Player(name="Alice", discord_id=2001, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=1000),
        Player(name="Bob", discord_id=2002, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=500),
        Player(name="Carol", discord_id=2003, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=100),
    ]
    player_service.get_leaderboard = MagicMock(return_value=players)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 2001
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    # Find LIGHTNING_BOLT index dynamically
    bolt_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "LIGHTNING_BOLT")

    with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=bolt_idx):
            with patch("commands.betting.random.uniform", return_value=0.02):  # 2% tax
                with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                    with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                        await cmds.gamba.callback(cmds, interaction)

    # Should call adjust_balance for each positive-balance player
    # Alice: 2% of 1000 = 20, Bob: 2% of 500 = 10, Carol: 2% of 100 = 2
    adjust_calls = player_service.adjust_balance.call_args_list
    assert len(adjust_calls) == 3
    # Check each call is negative (tax)
    for call in adjust_calls:
        assert call[0][2] < 0, "Tax should be negative"

    # Should credit nonprofit fund with total tax (20 + 10 + 2 = 32)
    loan_service.add_to_nonprofit_fund.assert_called_once_with(123, 32)


@pytest.mark.asyncio
async def test_wheel_lightning_bolt_skips_zero_balance():
    """Verify Lightning Bolt skips players with zero or negative balance."""
    from domain.models.player import Player

    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    # User is registered
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 100

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    # Mix of positive, zero, and negative balance players
    players = [
        Player(name="Rich", discord_id=3001, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=500),
        Player(name="Broke", discord_id=3002, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=0),
        Player(name="InDebt", discord_id=3003, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=-100),
    ]
    player_service.get_leaderboard = MagicMock(return_value=players)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 3001
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    bolt_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "LIGHTNING_BOLT")

    with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=bolt_idx):
            with patch("commands.betting.random.uniform", return_value=0.02):  # 2% tax
                with patch("commands.betting.random.random", return_value=1.0):
                    with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                        await cmds.gamba.callback(cmds, interaction)

    # Only Rich (500 JC) should be taxed; Broke (0) and InDebt (-100) skipped
    adjust_calls = player_service.adjust_balance.call_args_list
    assert len(adjust_calls) == 1
    assert adjust_calls[0][0] == (3001, 123, -10)  # 2% of 500 = 10

    # Nonprofit receives only the one tax
    loan_service.add_to_nonprofit_fund.assert_called_once_with(123, 10)


@pytest.mark.asyncio
async def test_wheel_lightning_bolt_spinner_also_taxed():
    """Verify the spinner's discord_id appears in the taxed players."""
    from domain.models.player import Player

    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 4001

    # User is registered
    player_service.get_player.return_value = MagicMock(name="Spinner")
    player_service.get_balance.return_value = 300

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    # Include the spinner in the leaderboard
    players = [
        Player(name="Spinner", discord_id=spinner_id, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=300),
        Player(name="Other", discord_id=4002, mmr=None, initial_mmr=None,
               wins=0, losses=0, preferred_roles=None, main_role=None,
               glicko_rating=None, glicko_rd=None, glicko_volatility=None,
               jopacoin_balance=200),
    ]
    player_service.get_leaderboard = MagicMock(return_value=players)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = spinner_id
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    bolt_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "LIGHTNING_BOLT")

    with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=bolt_idx):
            with patch("commands.betting.random.uniform", return_value=0.01):  # 1% tax
                with patch("commands.betting.random.random", return_value=1.0):
                    with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                        await cmds.gamba.callback(cmds, interaction)

    # Both players should be taxed (including the spinner)
    adjust_calls = player_service.adjust_balance.call_args_list
    assert len(adjust_calls) == 2

    # Verify the spinner was taxed
    taxed_ids = {call[0][0] for call in adjust_calls}
    assert spinner_id in taxed_ids, "Spinner should be taxed too"


# ============================================================================
# Bankrupt Wheel Tests (for players in bankruptcy penalty)
# ============================================================================

def test_bankrupt_wheel_has_correct_numbered_count():
    """Bankrupt wheel has 10 numbered positive-value wedges (1,2,5,5,10,10,15,15,20,20)."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    numbered = sum(
        1 for w in BANKRUPT_WHEEL_WEDGES
        if isinstance(w[1], int) and w[1] > 0
    )
    assert numbered == 10, f"Expected 10 numbered wedges, got {numbered}"


def test_bankrupt_wheel_removes_high_values():
    """Highest value wedges (100, 80, 70, etc) should be removed."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] > 0]
    assert 100 not in values, "100 should be removed from bankrupt wheel"
    assert 80 not in values, "80 should be removed from bankrupt wheel"
    assert 70 not in values, "70 should be removed from bankrupt wheel"
    assert 60 not in values, "60 should be removed from bankrupt wheel"


def test_bankrupt_wheel_keeps_special_wedges():
    """Non-numbered wedges (shells, bolt, lose, bankrupt) remain."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    special = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], str)]
    assert "RED_SHELL" in special, "RED_SHELL should remain on bankrupt wheel"
    assert "BLUE_SHELL" in special, "BLUE_SHELL should remain on bankrupt wheel"
    assert "LIGHTNING_BOLT" in special, "LIGHTNING_BOLT should remain on bankrupt wheel"


def test_bankrupt_wheel_has_extension_slices():
    """Bankrupt wheel should have EXTEND_1 and EXTEND_2 slices."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    special = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], str)]
    assert "EXTEND_1" in special, "EXTEND_1 should be on bankrupt wheel"
    assert "EXTEND_2" in special, "EXTEND_2 should be on bankrupt wheel"


def test_bankrupt_wheel_total_wedge_count():
    """Bankrupt wheel should have 24 wedges total (matches normal wheel count).

    Composition:
    - 2 BANKRUPT, 1 LOSE, 3 special shells/bolt, 2 extension slices = 8 non-positive
    - 10 numbered (1,2,5,5,10,10,15,15,20,20)
    - 6 new unique mechanics (JAILBREAK, CHAIN_REACTION, TOWN_TRIAL, DISCOVER, EMERGENCY, REVEAL)
    Total = 8 + 10 + 6 = 24
    """
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    assert len(BANKRUPT_WHEEL_WEDGES) == 24, f"Expected 24 wedges, got {len(BANKRUPT_WHEEL_WEDGES)}"


def test_bankrupt_wheel_keeps_low_value_wedges():
    """Low value wedges (5, 10, 15, 20) should remain on bankrupt wheel."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] > 0]
    assert 5 in values, "5 should remain on bankrupt wheel"
    assert 10 in values, "10 should remain on bankrupt wheel"
    assert 15 in values, "15 should remain on bankrupt wheel"
    assert 20 in values, "20 should remain on bankrupt wheel"


def test_bankrupt_wheel_extension_slices_have_dark_red_colors():
    """Extension slices should have dark red colors."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    extend_wedges = [w for w in BANKRUPT_WHEEL_WEDGES if w[1] in ("EXTEND_1", "EXTEND_2")]
    assert len(extend_wedges) == 2, "Should have exactly 2 extension wedges"

    for label, value, color in extend_wedges:
        # Colors should be dark red variants (#8B0000, #660000)
        assert color.startswith("#"), f"Color should be hex format, got {color}"
        # Convert hex to RGB and check it's reddish
        r = int(color[1:3], 16)
        g = int(color[3:5], 16)
        b = int(color[5:7], 16)
        assert r > g and r > b, f"Extension slice color should be red-dominant, got {color}"


def test_get_wheel_wedges_returns_correct_wheel():
    """get_wheel_wedges() should return correct wheel based on is_bankrupt flag."""
    from utils.wheel_drawing import WHEEL_WEDGES, BANKRUPT_WHEEL_WEDGES, get_wheel_wedges

    normal = get_wheel_wedges(is_bankrupt=False)
    bankrupt = get_wheel_wedges(is_bankrupt=True)

    assert normal is WHEEL_WEDGES, "Should return normal wheel when not bankrupt"
    assert bankrupt is BANKRUPT_WHEEL_WEDGES, "Should return bankrupt wheel when bankrupt"
    assert len(normal) == 24, "Normal wheel should have 24 wedges"
    assert len(bankrupt) == 24, "Bankrupt wheel should have 24 wedges"


def test_get_wedge_at_index_for_player_returns_correct_wedge():
    """get_wedge_at_index_for_player() should return wedge from correct wheel."""
    from utils.wheel_drawing import (
        WHEEL_WEDGES, BANKRUPT_WHEEL_WEDGES,
        get_wedge_at_index_for_player
    )

    # Index 0 on normal wheel
    normal_wedge = get_wedge_at_index_for_player(0, is_bankrupt=False)
    assert normal_wedge == WHEEL_WEDGES[0]

    # Index 0 on bankrupt wheel
    bankrupt_wedge = get_wedge_at_index_for_player(0, is_bankrupt=True)
    assert bankrupt_wedge == BANKRUPT_WHEEL_WEDGES[0]

    # Index wrapping for normal wheel (24 wedges)
    wrapped_normal = get_wedge_at_index_for_player(24, is_bankrupt=False)
    assert wrapped_normal == WHEEL_WEDGES[0]

    # Index wrapping for bankrupt wheel (24 wedges)
    wrapped_bankrupt = get_wedge_at_index_for_player(24, is_bankrupt=True)
    assert wrapped_bankrupt == BANKRUPT_WHEEL_WEDGES[0]


def test_bankrupt_wheel_bankrupt_value_recalculated():
    """BANKRUPT wedges should have recalculated value on the bankrupt wheel."""
    from utils.wheel_drawing import WHEEL_WEDGES, BANKRUPT_WHEEL_WEDGES

    normal_bankrupt_values = [w[1] for w in WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0]
    bankrupt_bankrupt_values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0]

    # Both should have exactly 2 BANKRUPT wedges
    assert len(normal_bankrupt_values) == 2
    assert len(bankrupt_bankrupt_values) == 2

    # All BANKRUPT values should be negative
    for v in normal_bankrupt_values + bankrupt_bankrupt_values:
        assert v < 0, f"BANKRUPT value should be negative, got {v}"


def test_bankrupt_wheel_has_all_new_slices():
    """Bankrupt wheel should contain all 8 new unique mechanic slices."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    special_values = {w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], str)}

    assert "JAILBREAK" in special_values, "JAILBREAK should be on bankrupt wheel"
    assert "CHAIN_REACTION" in special_values, "CHAIN_REACTION should be on bankrupt wheel"
    assert "TOWN_TRIAL" in special_values, "TOWN_TRIAL should be on bankrupt wheel"
    assert "DISCOVER" in special_values, "DISCOVER should be on bankrupt wheel"
    assert "EMERGENCY" in special_values, "EMERGENCY should be on bankrupt wheel"
    assert "REVEAL" in special_values, "REVEAL should be on bankrupt wheel"


def test_bankrupt_wheel_has_micro_win_slices():
    """Bankrupt wheel should have 1 JC and 2 JC slices."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] > 0]
    assert 1 in values, "1 JC (BREADCRUMBS) should be on bankrupt wheel"
    assert 2 in values, "2 JC (PITY PRIZE) should be on bankrupt wheel"


def test_bankrupt_wheel_ev_maintained():
    """Bankrupt wheel expected value should match WHEEL_TARGET_EV."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES
    from utils.wheel_drawing import _SPECIAL_WEDGE_EST_EVS, _load_special_wedge_evs

    _load_special_wedge_evs()

    total_value = 0.0
    for _, v, _ in BANKRUPT_WHEEL_WEDGES:
        if isinstance(v, int):
            total_value += v
        elif isinstance(v, str):
            total_value += _SPECIAL_WEDGE_EST_EVS.get(v, 0.0)

    expected_value = total_value / len(BANKRUPT_WHEEL_WEDGES)
    assert abs(expected_value - WHEEL_TARGET_EV) <= 1, (
        f"Bankrupt wheel EV ~{WHEEL_TARGET_EV}, got {expected_value:.2f}"
    )


def test_jailbreak_clamps_at_zero(repo_db_path):
    """add_penalty_games(-1) when already at 0 should stay at 0."""
    from repositories.player_repository import PlayerRepository
    from services.bankruptcy_service import BankruptcyRepository, BankruptcyService

    player_repo = PlayerRepository(repo_db_path)
    bk_repo = BankruptcyRepository(repo_db_path)
    bk_service = BankruptcyService(
        bankruptcy_repo=bk_repo,
        player_repo=player_repo,
        cooldown_seconds=604800,
        penalty_games=5,
        penalty_rate=0.5,
    )

    player_repo.add(discord_id=9001, discord_username="TestJailbreak", guild_id=0,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)
    player_repo.update_balance(9001, 0, -100)
    bk_service.execute_bankruptcy(9001, 0)        # creates state with 5 penalty games
    bk_service.add_penalty_games(9001, 0, -5)    # reduce to 0

    result = bk_service.add_penalty_games(9001, 0, -1)
    assert result == 0, f"Expected 0 after JAILBREAK on 0 games, got {result}"


def test_jailbreak_decrements_games(repo_db_path):
    """add_penalty_games(-1) with 3 remaining should give 2."""
    from repositories.player_repository import PlayerRepository
    from services.bankruptcy_service import BankruptcyRepository, BankruptcyService

    player_repo = PlayerRepository(repo_db_path)
    bk_repo = BankruptcyRepository(repo_db_path)
    bk_service = BankruptcyService(
        bankruptcy_repo=bk_repo,
        player_repo=player_repo,
        cooldown_seconds=604800,
        penalty_games=5,
        penalty_rate=0.5,
    )

    player_repo.add(discord_id=9002, discord_username="TestJailbreak2", guild_id=0,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)
    player_repo.update_balance(9002, 0, -100)
    bk_service.execute_bankruptcy(9002, 0)        # creates state with 5 penalty games
    bk_service.add_penalty_games(9002, 0, -2)    # reduce to 3

    result = bk_service.add_penalty_games(9002, 0, -1)
    assert result == 2, f"Expected 2 after JAILBREAK on 3 games, got {result}"
