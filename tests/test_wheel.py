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
    with patch("commands.betting.random.randint", return_value=3):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should add balance directly (user_id, guild_id, amount)
    player_service.adjust_balance.assert_called_once_with(1003, 123, 5)


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
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should subtract the bankrupt value (negative)
    bankrupt_value = WHEEL_WEDGES[0][1]
    assert bankrupt_value < 0, "Bankrupt should have negative value"
    player_service.adjust_balance.assert_called_once_with(1004, 123, bankrupt_value)


@pytest.mark.asyncio
async def test_wheel_bankrupt_credits_nonprofit_fund():
    """Verify Bankrupt wedge losses are credited to the nonprofit fund."""
    bot = MagicMock()
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
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await cmds.gamba.callback(cmds, interaction)

    # Should credit the nonprofit fund with the absolute loss value
    bankrupt_value = WHEEL_WEDGES[0][1]
    loan_service.add_to_nonprofit_fund.assert_called_once_with(123, abs(int(bankrupt_value)))


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
    with patch("commands.betting.random.randint", return_value=0):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should subtract bankrupt value regardless of MAX_DEBT
    player_service.adjust_balance.assert_called_once_with(1005, 123, bankrupt_value)


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
    with patch("commands.betting.random.randint", return_value=2):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                await commands.gamba.callback(commands, interaction)

    # Should NOT call adjust_balance at all
    player_service.adjust_balance.assert_not_called()


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

    with patch("commands.betting.random.randint", return_value=5):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
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
                    await commands.gamba.callback(commands, interaction)

    # Admin should be able to spin despite cooldown - file attachment means spin happened
    call_kwargs = interaction.followup.send.call_args.kwargs
    assert "file" in call_kwargs


@pytest.mark.asyncio
async def test_wheel_red_shell_steals_from_player_above():
    """Verify Red Shell steals from the player ranked above on leaderboard."""
    from domain.models.player import Player

    bot = MagicMock()
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
