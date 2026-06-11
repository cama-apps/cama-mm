"""Tests for the Wheel of Fortune /gamba command."""
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from commands.betting import BettingCommands
from config import (
    WHEEL_BANANA_PEEL_EST_EV,
    WHEEL_BLUE_SHELL_EST_EV,
    WHEEL_BOMB_OMB_EST_EV,
    WHEEL_COOLDOWN_SECONDS,
    WHEEL_GREEN_SHELL_EST_EV,
    WHEEL_LIGHTNING_BOLT_EST_EV,
    WHEEL_RED_SHELL_EST_EV,
    WHEEL_TARGET_EV,
)
from domain.models.mana_effects import ManaEffects
from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES, WHEEL_WEDGES


def _assert_gamba_adjust_call(
    mock: MagicMock,
    discord_id: int,
    guild_id: int,
    amount: int,
) -> None:
    mock.assert_called_once()
    assert mock.call_args.args == (discord_id, guild_id, amount)
    kwargs = mock.call_args.kwargs
    assert kwargs["source"] == "gamba"
    assert kwargs["actor_id"] == discord_id
    assert kwargs["related_type"] == "wheel_spin"
    assert kwargs["reason"].startswith("gamba ")


def _assert_gamba_income_call(
    mock: MagicMock,
    discord_id: int,
    amount: int,
    guild_id: int,
) -> None:
    mock.assert_called_once()
    assert mock.call_args.args == (discord_id, amount, guild_id)
    kwargs = mock.call_args.kwargs
    assert kwargs["source"] == "gamba"
    assert kwargs["actor_id"] == discord_id
    assert kwargs["related_type"] == "wheel_spin"
    assert kwargs["reason"].startswith("gamba ")


def _assert_gamba_steal_call(
    mock: MagicMock,
    *,
    thief_discord_id: int,
    victim_discord_id: int,
    guild_id: int,
    amount: int,
) -> None:
    mock.assert_called_once()
    kwargs = mock.call_args.kwargs
    assert kwargs["thief_discord_id"] == thief_discord_id
    assert kwargs["victim_discord_id"] == victim_discord_id
    assert kwargs["guild_id"] == guild_id
    assert kwargs["amount"] == amount
    assert kwargs["source"] == "gamba"
    assert kwargs["actor_id"] == thief_discord_id
    assert kwargs["related_type"] == "wheel_spin"
    assert kwargs["reason"].startswith("gamba ")


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
    interaction.channel.name = "gamba"
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

    _FROZEN_NOW = 1_700_000_000  # fixed epoch; avoids sub-second boundary flakiness
    # Mock service methods - cooldown expired
    player_service.get_last_wheel_spin = MagicMock(return_value=_FROZEN_NOW - WHEEL_COOLDOWN_SECONDS - 1)
    player_service.adjust_balance = MagicMock()
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1001
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Pick a simple positive-int wedge so the spin path is straightforward.
    five_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 5)
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.time.time", return_value=_FROZEN_NOW):
        with patch("commands.betting.random.randint", return_value=five_idx):
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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1002
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # With balance=-100 (negative), the bankrupt wheel is used — find a positive numeric wedge.
    target_idx = next(i for i, w in enumerate(BANKRUPT_WHEEL_WEDGES) if isinstance(w[1], int) and w[1] > 0)
    expected_win = BANKRUPT_WHEEL_WEDGES[target_idx][1]

    # Mock _create_wheel_gif_file to avoid GIF generation calling random.randint
    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=target_idx):
            with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    # Should call garnishment service (user_id, amount, guild_id)
    _assert_gamba_income_call(garnishment_service.add_income, 1002, expected_win, 123)


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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1003
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Pick the index of a +5 wedge so the test survives wheel reordering.
    target_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 5)
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=target_idx):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Should add balance directly (user_id, guild_id, amount)
    _assert_gamba_adjust_call(player_service.adjust_balance, 1003, 123, 5)


@pytest.mark.asyncio
async def test_wheel_white_mana_animation_uses_capped_wedges():
    """Verify the wheel GIF uses the same capped wedges White mana rolls against."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    bot.mana_effects_service = MagicMock()
    bot.mana_effects_service.get_effects.return_value = ManaEffects(
        color="White",
        land="Plains",
        plains_max_wheel_win=50,
    )
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50
    player_service.get_leaderboard.return_value = []
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1010
    interaction.user.name = "Spinner"
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    target_idx = next(i for i, wedge in enumerate(WHEEL_WEDGES) if wedge[1] == 80)

    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()) as mock_gif:
        with patch("commands.betting.random.randint", return_value=target_idx):
            with patch("commands.betting.random.random", return_value=1.0):
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    _assert_gamba_adjust_call(player_service.adjust_balance, 1010, 123, 50)
    used_wedges = mock_gif.call_args.kwargs["wedges"]
    assert used_wedges[target_idx][0] == "50"
    assert used_wedges[target_idx][1] == 50


@pytest.mark.asyncio
async def test_wheel_blue_mana_embed_uses_reduced_numeric_payout():
    """Verify Blue mana shows the reduced payout, not the pre-tax wedge value."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    bot.mana_effects_service = MagicMock()
    bot.mana_effects_service.get_effects.return_value = ManaEffects(
        color="Blue",
        land="Island",
        blue_gamba_reduction=0.25,
    )
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = 50
    player_service.get_leaderboard.return_value = []
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1011
    interaction.user.name = "Spinner"
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)
    target_idx = next(i for i, wedge in enumerate(WHEEL_WEDGES) if wedge[1] == 100)

    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
        with patch("commands.betting.random.randint", return_value=target_idx):
            with patch("commands.betting.random.random", return_value=1.0):
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    _assert_gamba_adjust_call(player_service.adjust_balance, 1011, 123, 75)
    embed = message.edit.call_args.kwargs["embed"]
    assert embed.title == "🎉 Winner!"
    assert "won **75**" in embed.description
    assert any(field.name == "🏝️ Blue Mana Tax" for field in embed.fields)


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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1004
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock random to hit a BANKRUPT wedge and disable explosion
    bankrupt_idx_a = next(i for i, w in enumerate(WHEEL_WEDGES) if isinstance(w[1], int) and w[1] < 0)
    bankrupt_value = WHEEL_WEDGES[bankrupt_idx_a][1]
    assert bankrupt_value < 0, "Bankrupt should have negative value"
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=bankrupt_idx_a):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Should subtract the bankrupt value (negative)
    _assert_gamba_adjust_call(player_service.adjust_balance, 1004, 123, bankrupt_value)


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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1004
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    # Mock random to hit a BANKRUPT wedge and disable explosion
    bankrupt_idx_b = next(i for i, w in enumerate(WHEEL_WEDGES) if isinstance(w[1], int) and w[1] < 0)
    bankrupt_value = WHEEL_WEDGES[bankrupt_idx_b][1]
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=bankrupt_idx_b):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    # Should credit the nonprofit fund with the absolute loss value
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
    # With balance=-400 (negative), bankrupt wheel is used
    bankrupt_idx_b = next(
        i for i, w in enumerate(BANKRUPT_WHEEL_WEDGES) if isinstance(w[1], int) and w[1] < 0
    )
    bankrupt_value = BANKRUPT_WHEEL_WEDGES[bankrupt_idx_b][1]
    # Three get_balance calls: (1) for is_eligible_for_bad_gamba check, (2) before processing, (3) after adjust
    player_service.get_balance.side_effect = [-400, -400, -400 + bankrupt_value]

    # Mock service methods
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()
    # No COMEBACK pardon active (so BANKRUPT applies normally)
    player_service.get_wheel_pardon = MagicMock(return_value=False)
    player_service.set_wheel_pardon = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1005
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=bankrupt_idx_b):
        with patch("commands.betting.random.random", return_value=1.0):  # No explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Should subtract bankrupt value regardless of MAX_DEBT
    _assert_gamba_adjust_call(player_service.adjust_balance, 1005, 123, bankrupt_value)


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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1006
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Find the LOSE wedge (value == 0) dynamically and disable explosion
    lose_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 0)
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=lose_idx):
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
    interaction.channel.name = "gamba"
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
    _assert_gamba_adjust_call(player_service.adjust_balance, 1007, 123, 100)


def test_wheel_wedges_has_correct_count():
    """Verify WHEEL_WEDGES has exactly 24 wedges."""
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
    assert small_count == 3, f"Expected 3 small win wedges, got {small_count}"
    assert medium_count == 3, f"Expected 3 medium win wedges, got {medium_count}"
    assert good_count == 4, f"Expected 4 good win wedges, got {good_count}"
    assert great_count == 3, f"Expected 3 great win wedges, got {great_count}"
    assert jackpot_count == 2, f"Expected 2 Jackpot wedges, got {jackpot_count}"
    assert special_count == 6, f"Expected 6 special wedges, got {special_count}"


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
        "BANANA_PEEL": WHEEL_BANANA_PEEL_EST_EV,
        "GREEN_SHELL": WHEEL_GREEN_SHELL_EST_EV,
        "BOMB_OMB": WHEEL_BOMB_OMB_EST_EV,
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
    # Independent hardcoded sanity bound: the wheel is intentionally a coin sink
    # (WHEEL_TARGET_EV ≈ -25). Guard against runaway misconfiguration: must
    # never be a net positive (infinite money) or absurdly worse than designed.
    assert -50 <= expected_value <= 5, (
        f"Wheel EV {expected_value:.2f} outside sane [-50, 5] range — "
        "wedge table may be misconfigured"
    )


def test_wheel_bankrupt_always_negative():
    """Verify BANKRUPT wedges are always negative (capped at -1 minimum)."""
    bankrupt_wedges = [w for w in WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0]
    assert len(bankrupt_wedges) == 2, "Should have exactly 2 bankrupt wedges"
    for w in bankrupt_wedges:
        assert w[1] <= -1, f"Bankrupt value {w[1]} should be <= -1"


def test_wheel_special_wedges_have_string_values():
    """Verify special wedges have string values for special handling."""
    special_wedges = [w for w in WHEEL_WEDGES if isinstance(w[1], str)]
    assert len(special_wedges) == 6, "Should have exactly 6 special wedges"

    special_values = {w[1] for w in special_wedges}
    assert "RED_SHELL" in special_values, "Should have RED_SHELL wedge"
    assert "BLUE_SHELL" in special_values, "Should have BLUE_SHELL wedge"
    assert "LIGHTNING_BOLT" in special_values, "Should have LIGHTNING_BOLT wedge"
    assert "BANANA_PEEL" in special_values, "Should have BANANA_PEEL wedge"
    assert "GREEN_SHELL" in special_values, "Should have GREEN_SHELL wedge"
    assert "BOMB_OMB" in special_values, "Should have BOMB_OMB wedge"


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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1008
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Pick a simple positive-int wedge so the spin path is straightforward.
    target_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 5)
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.random.randint", return_value=target_idx):
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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1009
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    _FROZEN_NOW = 1_700_000_000  # fixed epoch; avoids sub-second boundary flakiness

    # Pick a simple positive-int wedge so the spin path is straightforward.
    target_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 5)
    # Mock GIF generation to avoid memory-intensive PIL operations in parallel tests
    with patch("commands.betting.time.time", return_value=_FROZEN_NOW):
        with patch("commands.betting.random.randint", return_value=target_idx):
            with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                        await commands.gamba.callback(commands, interaction)

    # Should have called set_last_wheel_spin with (user_id, guild_id, timestamp)
    player_service.set_last_wheel_spin.assert_called_once()
    call_args = player_service.set_last_wheel_spin.call_args[0]
    assert call_args[0] == 1009  # user_id
    assert call_args[1] == 123  # guild_id
    assert call_args[2] == _FROZEN_NOW  # exact frozen timestamp


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

    _FROZEN_NOW = 1_700_000_000  # fixed epoch; avoids sub-second boundary flakiness
    # Mock service methods - cooldown was just set (simulated as exactly now)
    player_service.get_last_wheel_spin = MagicMock(return_value=_FROZEN_NOW)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 789
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Pick a simple positive-int wedge so the spin path is straightforward.
    target_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 5)
    # Mock admin check to return True
    with patch("commands.betting.time.time", return_value=_FROZEN_NOW):
        with patch("commands.betting.has_admin_permission", return_value=True):
            with patch("commands.betting.random.randint", return_value=target_idx):
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
    interaction.channel.name = "gamba"
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
    _assert_gamba_steal_call(
        player_service.steal_atomic,
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
    interaction.channel.name = "gamba"
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
    interaction.channel.name = "gamba"
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

    # Should call get_leaderboard (once for golden eligibility check, once for blue shell target)
    player_service.get_leaderboard.assert_any_call(123, limit=1)

    # Should call steal_atomic: max(pct=5, flat=4) = 5 JC
    _assert_gamba_steal_call(
        player_service.steal_atomic,
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

    # Return [] for golden eligibility check (limit=3), real data for blue shell (limit=1)
    def leaderboard_side_effect(*args, **kwargs):
        if kwargs.get("limit") == 3:
            return []
        return [user_as_richest]
    player_service.get_leaderboard = MagicMock(side_effect=leaderboard_side_effect)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
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
    _assert_gamba_adjust_call(player_service.adjust_balance, 1013, 123, -10)

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

    # Return [] for golden eligibility check (limit=3), real data for lightning bolt
    def leaderboard_side_effect(*args, **kwargs):
        if kwargs.get("limit") == 3:
            return []
        return players
    player_service.get_leaderboard = MagicMock(side_effect=leaderboard_side_effect)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
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

    # Return [] for golden eligibility check (limit=3), real data for lightning bolt
    def leaderboard_side_effect(*args, **kwargs):
        if kwargs.get("limit") == 3:
            return []
        return players
    player_service.get_leaderboard = MagicMock(side_effect=leaderboard_side_effect)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
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

    # Return [] for golden eligibility check (limit=3), real data for lightning bolt
    def leaderboard_side_effect(*args, **kwargs):
        if kwargs.get("limit") == 3:
            return []
        return players
    player_service.get_leaderboard = MagicMock(side_effect=leaderboard_side_effect)

    message = MagicMock()
    message.edit = AsyncMock()

    interaction = MagicMock()
    interaction.channel.name = "gamba"
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
    """Bankrupt wheel keeps a meaningful pool of positive numeric wedges after the
    Mario Kart trio took 3 slots."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    numbered = sum(
        1 for w in BANKRUPT_WHEEL_WEDGES
        if isinstance(w[1], int) and w[1] > 0
    )
    assert numbered == 6, f"Expected 6 numbered wedges, got {numbered}"


def test_bankrupt_wheel_has_positive_numeric_wedges():
    """Bankrupt wheel should still have a few sizeable positive numeric wedges."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] > 0]
    assert len(values) >= 5, f"Expected at least 5 positive numerics, got {len(values)}"
    assert max(values) >= 75, "Bankrupt wheel should reach at least 75 JC to deliver positive EV"


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
    """Bankrupt wheel should have 24 wedges total (matches normal wheel count)."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    assert len(BANKRUPT_WHEEL_WEDGES) == 24, f"Expected 24 wedges, got {len(BANKRUPT_WHEEL_WEDGES)}"


def test_bankrupt_wheel_has_a_floor_wedge():
    """Bankrupt wheel should have at least one small-value wedge as a floor outcome."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] > 0]
    assert min(values) <= 50, f"Bankrupt wheel should have at least one wedge ≤50 JC, smallest is {min(values)}"


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
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES, WHEEL_WEDGES, get_wheel_wedges

    normal = get_wheel_wedges(is_bankrupt=False)
    bankrupt = get_wheel_wedges(is_bankrupt=True)

    assert normal is WHEEL_WEDGES, "Should return normal wheel when not bankrupt"
    assert bankrupt is BANKRUPT_WHEEL_WEDGES, "Should return bankrupt wheel when bankrupt"
    assert len(normal) == 24, "Normal wheel should have 24 wedges"
    assert len(bankrupt) == 24, "Bankrupt wheel should have 24 wedges"


def test_bankrupt_wheel_bankrupt_value_recalculated():
    """BANKRUPT wedges should have recalculated value on the bankrupt wheel."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES, WHEEL_WEDGES

    normal_bankrupt_values = [w[1] for w in WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0]
    bankrupt_bankrupt_values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0]

    # Both should have exactly 2 BANKRUPT wedges
    assert len(normal_bankrupt_values) == 2
    assert len(bankrupt_bankrupt_values) == 2

    # All BANKRUPT values should be negative
    for v in normal_bankrupt_values + bankrupt_bankrupt_values:
        assert v < 0, f"BANKRUPT value should be negative, got {v}"


def test_bankrupt_wheel_has_all_new_slices():
    """Bankrupt wheel should contain all new unique mechanic slices."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES

    special_values = {w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], str)}

    assert "JAILBREAK" in special_values, "JAILBREAK should be on bankrupt wheel"
    assert "CHAIN_REACTION" in special_values, "CHAIN_REACTION should be on bankrupt wheel"
    assert "TOWN_TRIAL" in special_values, "TOWN_TRIAL should be on bankrupt wheel"
    assert "DISCOVER" in special_values, "DISCOVER should be on bankrupt wheel"
    assert "EMERGENCY" in special_values, "EMERGENCY should be on bankrupt wheel"
    assert "COMMUNE" in special_values, "COMMUNE should be on bankrupt wheel"
    assert "COMEBACK" in special_values, "COMEBACK should be on bankrupt wheel"
    assert "REVEAL" not in special_values, "REVEAL should NOT be on bankrupt wheel"


def test_bankrupt_wheel_ev_maintained():
    """Bankrupt wheel expected value (using bankrupt-context overrides) should
    match WHEEL_BANKRUPT_TARGET_EV."""
    from config import WHEEL_BANKRUPT_TARGET_EV
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES, bankrupt_special_ev

    total_value = 0.0
    for _, v, _ in BANKRUPT_WHEEL_WEDGES:
        if isinstance(v, int):
            total_value += v
        elif isinstance(v, str):
            total_value += bankrupt_special_ev(v)

    expected_value = total_value / len(BANKRUPT_WHEEL_WEDGES)
    assert abs(expected_value - WHEEL_BANKRUPT_TARGET_EV) <= 1, (
        f"Bankrupt wheel EV ~{WHEEL_BANKRUPT_TARGET_EV}, got {expected_value:.2f}"
    )


# ============================================================================
# Mario Kart deflation wedges (BANANA_PEEL / GREEN_SHELL / BOMB_OMB)
# ============================================================================

def test_mario_kart_wedges_on_all_wheels():
    """Banana / Green / Bomb appear on the regular, bankrupt, and golden wheels."""
    from utils.wheel_drawing import (
        BANKRUPT_WHEEL_WEDGES,
        GOLDEN_WHEEL_WEDGES,
        WHEEL_WEDGES,
    )
    for wheel_name, wheel in (
        ("regular", WHEEL_WEDGES),
        ("bankrupt", BANKRUPT_WHEEL_WEDGES),
        ("golden", GOLDEN_WHEEL_WEDGES),
    ):
        values = {w[1] for w in wheel if isinstance(w[1], str)}
        for mechanic in ("BANANA_PEEL", "GREEN_SHELL", "BOMB_OMB"):
            assert mechanic in values, f"{mechanic} missing from {wheel_name} wheel"


def test_wheels_in_rainbow_order():
    """Each wheel's wedges form a rainbow: hue bands ascending, brightness ascending within a band."""
    from utils.wheel_drawing import (
        BANKRUPT_WHEEL_WEDGES,
        GOLDEN_WHEEL_WEDGES,
        WHEEL_WEDGES,
        _hue_sort_key,
    )
    for wheel_name, wheel in (
        ("regular", WHEEL_WEDGES),
        ("bankrupt", BANKRUPT_WHEEL_WEDGES),
        ("golden", GOLDEN_WHEEL_WEDGES),
    ):
        keys = [_hue_sort_key(w[2]) for w in wheel]
        assert keys == sorted(keys), (
            f"{wheel_name} wheel not in rainbow order:\n{keys}"
        )

    # Concrete rainbow semantics: red precedes green precedes blue on the regular wheel.
    colors = [w[2].lower() for w in WHEEL_WEDGES]
    assert colors.index("#e74c3c") < colors.index("#228b22") < colors.index("#3498db")


def test_bankrupt_wheel_jackpot_is_gold():
    """Bankrupt wheel's 100-JC jackpot wedges should share the regular wheel's gold (#f1c40f)."""
    from utils.wheel_drawing import BANKRUPT_WHEEL_WEDGES, WHEEL_WEDGES

    regular_jackpot_colors = {w[2].lower() for w in WHEEL_WEDGES if w[1] == 100}
    bankrupt_jackpot_colors = {w[2].lower() for w in BANKRUPT_WHEEL_WEDGES if w[1] == 100}

    assert bankrupt_jackpot_colors == regular_jackpot_colors, (
        f"Bankrupt jackpot colors {bankrupt_jackpot_colors} should match regular {regular_jackpot_colors}"
    )
    assert bankrupt_jackpot_colors == {"#f1c40f"}, (
        f"Jackpot color should be gold #f1c40f, got {bankrupt_jackpot_colors}"
    )


def _make_wheel_interaction(user_id: int):
    """Build a minimal interaction object for /gamba in the gamba channel."""
    message = MagicMock()
    message.edit = AsyncMock()
    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.guild.get_member = MagicMock(return_value=MagicMock(mention=f"@{user_id}"))
    interaction.user.id = user_id
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)
    return interaction


def _make_wheel_player_service(spinner_balance: int = 100):
    """Build a player_service mock with the standard wheel-spin scaffolding."""
    player_service = MagicMock()
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = spinner_balance
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()
    return player_service


@pytest.mark.asyncio
async def test_banana_peel_burns_player_below():
    """BANANA_PEEL burns from the player ranked directly below the spinner."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7001
    victim_below = MagicMock(name="Below", discord_id=8001, jopacoin_balance=100)
    player_service = _make_wheel_player_service()
    player_service.get_player_below = MagicMock(return_value=victim_below)

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    banana_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BANANA_PEEL")
    # side_effect: [wedge_idx, loss_roll] — pinning loss to 20 (within 15-25)
    with patch("commands.betting.random.randint", side_effect=[banana_idx, 20]):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    # adjust_balance must hit the victim_below's discord_id, not the spinner's
    player_service.get_player_below.assert_called_once_with(spinner_id, 123)
    assert player_service.adjust_balance.call_count == 1
    call = player_service.adjust_balance.call_args
    assert call[0][0] == 8001, "BANANA_PEEL must debit the player below, not the spinner"
    assert call[0][1] == 123
    assert call[0][2] < 0, "BANANA_PEEL must apply a negative delta to the victim"

    # Spinner never debited
    spinner_calls = [
        c for c in player_service.adjust_balance.call_args_list if c[0][0] == spinner_id
    ]
    assert spinner_calls == [], "BANANA_PEEL must NOT debit the spinner"


@pytest.mark.asyncio
async def test_banana_peel_misses_when_no_player_below():
    """BANANA_PEEL with no player below performs no balance changes."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7002
    player_service = _make_wheel_player_service()
    player_service.get_player_below = MagicMock(return_value=None)

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    banana_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BANANA_PEEL")
    with patch("commands.betting.random.randint", return_value=banana_idx):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    # No victim → no balance adjustments at all
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_banana_peel_clamps_to_victim_balance():
    """BANANA_PEEL clamps the loss to the victim's available balance."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7003
    # Victim only has 5 JC; loss roll would be 25 → clamped to 5
    victim_below = MagicMock(name="Below", discord_id=8002, jopacoin_balance=5)
    player_service = _make_wheel_player_service()
    player_service.get_player_below = MagicMock(return_value=victim_below)

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    banana_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BANANA_PEEL")
    # side_effect: [wedge_idx, 25] — 25 > 5, must clamp
    with patch("commands.betting.random.randint", side_effect=[banana_idx, 25]):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    assert player_service.adjust_balance.call_count == 1
    call = player_service.adjust_balance.call_args
    assert call[0][0] == 8002
    # Loss clamped: delta is -5 (victim ends at 0, not negative)
    assert call[0][2] == -5, f"Expected -5 clamped loss, got {call[0][2]}"


@pytest.mark.asyncio
async def test_green_shell_steals_from_random_other_via_steal_atomic():
    """GREEN_SHELL transfers from a random other positive-balance player via steal_atomic."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7004
    victim = MagicMock(name="Victim", discord_id=8003, jopacoin_balance=100)
    player_service = _make_wheel_player_service()
    # Exclude spinner from the leaderboard so the top-N golden-wheel check
    # rejects them; the BOMB/GREEN victim filter doesn't need the spinner present.
    player_service.get_leaderboard = MagicMock(return_value=[victim])
    player_service.steal_atomic = MagicMock(return_value={
        "amount": 20,
        "thief_new_balance": 70,
        "victim_new_balance": 80,
    })

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    green_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "GREEN_SHELL")
    # side_effect: [wedge_idx, steal_roll] — pinning steal to 20 (within 15-25)
    with patch("commands.betting.random.randint", side_effect=[green_idx, 20]):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    _assert_gamba_steal_call(
        player_service.steal_atomic,
        thief_discord_id=spinner_id,
        victim_discord_id=8003,
        guild_id=123,
        amount=20,
    )
    # steal_atomic handles both sides; adjust_balance must NOT be used here
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_green_shell_misses_when_no_eligible_victims():
    """GREEN_SHELL with no eligible victims performs no steal."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7005
    # Empty leaderboard → no eligible others AND spinner not in top-N (no golden).
    player_service = _make_wheel_player_service()
    player_service.get_leaderboard = MagicMock(return_value=[])
    player_service.steal_atomic = MagicMock()

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    green_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "GREEN_SHELL")
    with patch("commands.betting.random.randint", return_value=green_idx):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    player_service.steal_atomic.assert_not_called()


@pytest.mark.asyncio
async def test_bomb_omb_burns_three_random_others():
    """BOMB_OMB burns from exactly 3 different positive-balance victims; spinner unchanged."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7006
    others = [
        MagicMock(name=f"V{i}", discord_id=9000 + i, jopacoin_balance=100)
        for i in range(5)
    ]
    player_service = _make_wheel_player_service()
    # Exclude spinner — keeps them out of top-N so the regular wheel fires.
    player_service.get_leaderboard = MagicMock(return_value=others)

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    bomb_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BOMB_OMB")
    # side_effect: [wedge_idx, loss1, loss2, loss3] — 3 victim losses pinned at 15
    with patch("commands.betting.random.randint", side_effect=[bomb_idx, 15, 15, 15]):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    # Exactly 3 victim debits
    assert player_service.adjust_balance.call_count == 3, (
        f"Expected 3 BOMB_OMB victim debits, got {player_service.adjust_balance.call_count}"
    )
    victim_ids = [c[0][0] for c in player_service.adjust_balance.call_args_list]
    deltas = [c[0][2] for c in player_service.adjust_balance.call_args_list]
    # All targets are distinct, none is the spinner, all deltas negative
    assert len(set(victim_ids)) == 3, f"Victims must be distinct, got {victim_ids}"
    assert spinner_id not in victim_ids, "BOMB_OMB must NOT debit the spinner"
    assert all(d < 0 for d in deltas), f"All BOMB_OMB deltas must be negative, got {deltas}"


@pytest.mark.asyncio
async def test_bomb_omb_clamps_sample_to_eligible_pool():
    """BOMB_OMB samples at most len(pool) victims when pool < VICTIM_COUNT."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7007
    others = [
        MagicMock(name="V1", discord_id=9101, jopacoin_balance=100),
        MagicMock(name="V2", discord_id=9102, jopacoin_balance=100),
    ]
    player_service = _make_wheel_player_service()
    # Exclude spinner from leaderboard so the top-N golden check rejects them.
    player_service.get_leaderboard = MagicMock(return_value=others)

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    bomb_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BOMB_OMB")
    # side_effect: [wedge_idx, loss1, loss2] — only 2 victim losses (pool size 2)
    # If random.sample(pool, 3) were called instead of (pool, 2), ValueError would raise.
    with patch("commands.betting.random.randint", side_effect=[bomb_idx, 15, 15]):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    assert player_service.adjust_balance.call_count == 2, (
        f"Expected 2 BOMB_OMB victim debits (pool clamped), got {player_service.adjust_balance.call_count}"
    )


@pytest.mark.asyncio
async def test_bomb_omb_misses_when_no_eligible_victims():
    """BOMB_OMB with no eligible victims performs no balance changes."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7008
    # Empty leaderboard → spinner not in top-N, no eligible BOMB_OMB victims.
    player_service = _make_wheel_player_service()
    player_service.get_leaderboard = MagicMock(return_value=[])

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    bomb_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == "BOMB_OMB")
    with patch("commands.betting.random.randint", return_value=bomb_idx):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
@pytest.mark.parametrize("wedge_value", ["BANANA_PEEL", "GREEN_SHELL", "BOMB_OMB"])
async def test_deflation_wedges_do_not_credit_nonprofit(wedge_value):
    """None of the three Mario Kart deflation wedges credit the nonprofit fund.

    BANANA and BOMB are burns (coins destroyed); GREEN_SHELL is a zero-sum transfer.
    None of these routes should hit loan_service.add_to_nonprofit_fund.
    """
    bot = MagicMock()
    bot.bankruptcy_service = None
    bot.garnishment_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    loan_service = MagicMock()

    spinner_id = 7100
    victim = MagicMock(name="Victim", discord_id=8200, jopacoin_balance=100)

    player_service = _make_wheel_player_service()
    # Provide enough victim infrastructure for any of the three wedges to fire.
    # Spinner is excluded from the leaderboard so the top-N golden check rejects them.
    player_service.get_player_below = MagicMock(return_value=victim)
    player_service.get_leaderboard = MagicMock(return_value=[victim])
    player_service.steal_atomic = MagicMock(return_value={
        "amount": 20,
        "thief_new_balance": 70,
        "victim_new_balance": 80,
    })

    interaction = _make_wheel_interaction(spinner_id)
    cmds = BettingCommands(
        bot, betting_service, match_service, player_service, loan_service=loan_service
    )

    wedge_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == wedge_value)
    # Pad side_effect with extra loss rolls so BOMB_OMB's loop is safe.
    with patch("commands.betting.random.randint", side_effect=[wedge_idx, 15, 15, 15, 15]):
        with patch("commands.betting.random.random", return_value=1.0):
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()):
                    await cmds.gamba.callback(cmds, interaction)

    loan_service.add_to_nonprofit_fund.assert_not_called()


def test_bankrupt_wheel_bankrupt_value_is_real_penalty():
    """BANKRUPT wedges on the bankrupt wheel must impose a meaningful loss.

    Pins the current ~+12 EV target. Reverting target to ~+25 would clamp the
    computed BANKRUPT value to -1, which makes the wheel a free roll — this
    test will fail in that case to surface the regression.
    """
    # BANKRUPT wedge labels are rewritten to the computed value (e.g. "-24") at
    # build time, so identify them by negative int value, not by label.
    bankrupt_values = [w[1] for w in BANKRUPT_WHEEL_WEDGES if isinstance(w[1], int) and w[1] < 0]
    assert bankrupt_values, "Bankrupt wheel must contain at least one BANKRUPT wedge"
    assert any(v <= -5 for v in bankrupt_values), (
        f"At least one BANKRUPT wedge must have value <= -5 (got {bankrupt_values}); "
        "if all are clamped near -1, the bankrupt wheel target EV is too high."
    )


@pytest.mark.asyncio
async def test_wheel_positive_balance_with_penalty_skips_bankrupt_wheel():
    """A player with balance >= 0 must never see the bankrupt wheel, even if
    they still have penalty_games_remaining > 0 from a previous bankruptcy."""
    bot = MagicMock()
    bk_service = MagicMock()
    bk_state = MagicMock()
    bk_state.penalty_games_remaining = 4
    bk_service.get_state = MagicMock(return_value=bk_state)
    bot.bankruptcy_service = bk_service

    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # Recovered player: balance >= 0, still has penalty games pending
    player_service.get_player.return_value = MagicMock(name="Recovered")
    player_service.get_balance.return_value = 0
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.adjust_balance = MagicMock()
    player_service.get_leaderboard = MagicMock(return_value=[])
    bot.garnishment_service = None

    message = MagicMock()
    message.edit = AsyncMock()
    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 7002
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    cmds = BettingCommands(bot, betting_service, match_service, player_service)

    # Pick LOSE so the spin path is simple and doesn't matter which wheel
    lose_idx = next(i for i, w in enumerate(WHEEL_WEDGES) if w[1] == 0)
    with patch.object(cmds, "_create_wheel_gif_file", return_value=MagicMock()) as mock_gif:
        with patch("commands.betting.random.randint", return_value=lose_idx):
            with patch("commands.betting.random.random", return_value=1.0):
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await cmds.gamba.callback(cmds, interaction)

    # GIF args: (size, label, is_eligible_for_bad_gamba, is_golden, ...)
    mock_gif.assert_called_once()
    is_bankrupt_flag = mock_gif.call_args.args[2]
    assert is_bankrupt_flag is False, (
        f"Penalty-state player with balance >= 0 must NOT get bankrupt wheel "
        f"(is_eligible_for_bad_gamba={is_bankrupt_flag})"
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


@pytest.mark.asyncio
async def test_wheel_negative_balance_uses_bankrupt_wheel():
    """Verify a negative balance (no formal bankruptcy) triggers the bankrupt wheel GIF."""
    bot = MagicMock()
    bot.bankruptcy_service = None
    betting_service = MagicMock()
    match_service = MagicMock()
    player_service = MagicMock()

    # User is registered and in debt but has no formal bankruptcy state
    player_service.get_player.return_value = MagicMock(name="TestPlayer")
    player_service.get_balance.return_value = -50  # Negative balance

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
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = 123
    interaction.user.id = 1099
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    commands = BettingCommands(bot, betting_service, match_service, player_service)

    # Find first positive-value bankrupt wheel wedge to use as the spin target
    target_idx = next(
        i for i, w in enumerate(BANKRUPT_WHEEL_WEDGES) if isinstance(w[1], int) and w[1] > 0
    )

    with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()) as mock_gif:
        with patch("commands.betting.random.randint", return_value=target_idx):
            with patch("commands.betting.random.random", return_value=1.0):  # No explosion
                with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                    await commands.gamba.callback(commands, interaction)

    # GIF must have been generated with is_eligible_for_bad_gamba=True
    mock_gif.assert_called_once()
    assert mock_gif.call_args.args[2] is True, (
        f"Expected is_eligible_for_bad_gamba=True for negative-balance player, got {mock_gif.call_args.args[2]}"
    )
    # GIF must have been sent via followup
    interaction.followup.send.assert_awaited()


def test_commune_credits_spinner_debits_positive_balance_players(repo_db_path):
    """COMMUNE: each positive-balance player loses 1 JC; spinner gains total."""
    from repositories.player_repository import PlayerRepository

    player_repo = PlayerRepository(repo_db_path)

    # Spinner (in debt)
    player_repo.add(discord_id=8001, discord_username="Spinner", guild_id=0,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)
    player_repo.update_balance(8001, 0, -20)

    # Two positive-balance donors
    player_repo.add(discord_id=8002, discord_username="Donor1", guild_id=0,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)
    player_repo.update_balance(8002, 0, 50)

    player_repo.add(discord_id=8003, discord_username="Donor2", guild_id=0,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)
    player_repo.update_balance(8003, 0, 10)

    # One zero-balance player (should not donate)
    player_repo.add(discord_id=8004, discord_username="Broke", guild_id=0,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)
    player_repo.update_balance(8004, 0, 0)  # explicitly set to 0

    # Simulate COMMUNE: debit each positive-balance donor 1 JC, credit spinner
    commune_total = 0
    all_players = player_repo.get_leaderboard(0, limit=9999)
    for p in all_players:
        if p.discord_id != 8001 and p.jopacoin_balance > 0:
            player_repo.add_balance(p.discord_id, 0, -1)
            commune_total += 1
    player_repo.add_balance(8001, 0, commune_total)

    assert commune_total == 2, f"Expected 2 donors, got {commune_total}"
    assert player_repo.get_balance(8001, 0) == -20 + 2, "Spinner should have gained commune_total JC"
    assert player_repo.get_balance(8002, 0) == 49, "Donor1 should have lost 1 JC"
    assert player_repo.get_balance(8003, 0) == 9, "Donor2 should have lost 1 JC"
    assert player_repo.get_balance(8004, 0) == 0, "Zero-balance player should be unchanged"


def test_comeback_sets_and_consumes_pardon(repo_db_path):
    """COMEBACK sets pardon token; next BANKRUPT check consumes it and returns True."""
    from repositories.player_repository import PlayerRepository

    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=8010, discord_username="ComebackPlayer", guild_id=0,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)

    # Initially no pardon
    assert player_repo.get_wheel_pardon(8010, 0) is False, "Should have no pardon initially"

    # Simulate rolling COMEBACK: grant pardon
    player_repo.set_wheel_pardon(8010, 0, 1)
    assert player_repo.get_wheel_pardon(8010, 0) is True, "Pardon should be active after COMEBACK"

    # Simulate rolling BANKRUPT: consume pardon
    player_repo.set_wheel_pardon(8010, 0, 0)
    assert player_repo.get_wheel_pardon(8010, 0) is False, "Pardon should be consumed after BANKRUPT"


@pytest.mark.asyncio
async def test_wheel_penalized_winner_is_debuffed(repo_db_path):
    """A bankruptcy-penalized spinner keeps only the configured fraction of a
    wheel win. Guards the live gamba chokepoint and its bankruptcy_service-gated
    balance anchor — the path that caused the -n4 CI hang and which every other
    wheel test skips by setting bankruptcy_service=None.
    """
    from config import BANKRUPTCY_PENALTY_RATE
    from repositories.bankruptcy_repository import BankruptcyRepository
    from repositories.player_repository import PlayerRepository
    from services.bankruptcy_service import BankruptcyService

    guild_id, uid = 123, 7777
    player_repo = PlayerRepository(repo_db_path)
    bk_service = BankruptcyService(BankruptcyRepository(repo_db_path), player_repo)
    player_repo.add(discord_id=uid, discord_username="P", guild_id=guild_id,
                    glicko_rating=1500.0, glicko_rd=350.0, glicko_volatility=0.06)
    # Drive into debt, then declare bankruptcy -> penalized, balance reset >= 0
    # so the regular wheel is used (not the bankrupt wheel).
    player_repo.update_balance(uid, guild_id, -50)
    assert bk_service.execute_bankruptcy(uid, guild_id).success
    start_balance = player_repo.get_balance(uid, guild_id)
    assert start_balance >= 0

    bot = MagicMock()
    bot.bankruptcy_service = bk_service
    bot.buff_service = None
    bot.player_repo = player_repo
    bot.garnishment_service = None
    player_service = MagicMock()
    player_service.get_player.return_value = MagicMock(name="P")
    player_service.get_last_wheel_spin = MagicMock(return_value=None)
    player_service.set_last_wheel_spin = MagicMock()
    player_service.try_claim_wheel_spin = MagicMock(return_value=True)
    player_service.log_wheel_spin = MagicMock(return_value=1)
    player_service.get_leaderboard = MagicMock(return_value=[])
    # Callable side effects backed by the real repo — NOT an exhaustible list,
    # which is exactly the StopIteration trap the anchor gate fixed.
    player_service.get_balance.side_effect = lambda *a, **k: player_repo.get_balance(uid, guild_id)
    player_service.adjust_balance.side_effect = (
        lambda u, g, amt, **kwargs: player_repo.add_balance(u, g, amt, **kwargs)
    )

    message = MagicMock()
    message.edit = AsyncMock()
    interaction = MagicMock()
    interaction.channel.name = "gamba"
    interaction.guild = MagicMock()
    interaction.guild.id = guild_id
    interaction.user.id = uid
    interaction.response.defer = AsyncMock()
    interaction.followup.send = AsyncMock(return_value=message)

    # Largest positive-int wedge -> a clear, non-zero penalty.
    win_idx = max(
        (i for i, w in enumerate(WHEEL_WEDGES) if isinstance(w[1], int) and w[1] > 0),
        key=lambda i: WHEEL_WEDGES[i][1],
    )
    win_val = WHEEL_WEDGES[win_idx][1]
    expected_kept = int(win_val * BANKRUPTCY_PENALTY_RATE)
    assert win_val - expected_kept > 0  # sanity: the debuff actually bites

    commands = BettingCommands(bot, MagicMock(), MagicMock(), player_service)
    with patch("commands.betting.random.randint", return_value=win_idx):
        with patch("commands.betting.random.random", return_value=1.0):  # no explosion
            with patch("commands.betting.asyncio.sleep", new_callable=AsyncMock):
                with patch.object(commands, "_create_wheel_gif_file", return_value=MagicMock()):
                    await commands.gamba.callback(commands, interaction)

    # Won win_val, but kept only the configured fraction; the rest was sunk.
    net_gain = player_repo.get_balance(uid, guild_id) - start_balance
    assert net_gain == expected_kept
