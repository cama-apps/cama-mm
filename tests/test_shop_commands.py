"""
Tests for shop commands.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.shop import ShopCommands, SHOP_ANNOUNCE_COST, SHOP_ANNOUNCE_TARGET_COST, SHOP_MYSTERY_GIFT_COST, SHOP_RECALIBRATE_COST


def _make_interaction(user_id: int = 1001):
    interaction = MagicMock()
    interaction.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
    interaction.response = AsyncMock()
    interaction.followup = AsyncMock()
    interaction.guild = None
    return interaction


@pytest.mark.asyncio
async def test_shop_requires_target_for_announce_target(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    commands = ShopCommands(bot, player_service)

    interaction = _make_interaction()

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, "announce_target", target=None)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "didn't specify a target" in message


@pytest.mark.asyncio
async def test_handle_announce_requires_registration():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = None

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_announce(interaction, target=None)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "need to `/player register`" in message


@pytest.mark.asyncio
async def test_handle_announce_insufficient_balance():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_ANNOUNCE_COST - 1

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_announce(interaction, target=None)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "You need" in message
    assert "only have" in message


@pytest.mark.asyncio
async def test_handle_announce_success_deducts_balance(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    # Mock player with required attributes for _get_flex_stats
    mock_player = SimpleNamespace(
        wins=10, losses=5, jopacoin_balance=500, glicko_rating=1500.0
    )
    player_service.get_player.return_value = mock_player
    player_service.get_balance.return_value = SHOP_ANNOUNCE_TARGET_COST + 10

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    monkeypatch.setattr("commands.shop.random.choice", lambda _items: "Test message")
    monkeypatch.setattr("commands.shop.get_hero_color", lambda _hero_id: None)
    monkeypatch.setattr("commands.shop.get_hero_image_url", lambda _hero_id: None)

    await commands._handle_announce(interaction, target=target)

    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id, None, -SHOP_ANNOUNCE_TARGET_COST
    )
    # shop uses safe_defer then safe_followup, so check followup.send
    interaction.followup.send.assert_awaited_once()
    kwargs = interaction.followup.send.call_args.kwargs
    assert "embed" in kwargs


@pytest.mark.asyncio
async def test_handle_mystery_gift_requires_registration():
    """Test that mystery gift requires player registration."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = None

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_mystery_gift(interaction)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "need to `/player register`" in message


@pytest.mark.asyncio
async def test_handle_mystery_gift_insufficient_balance():
    """Test that mystery gift requires sufficient balance (10k JC)."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_MYSTERY_GIFT_COST - 1

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_mystery_gift(interaction)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "You need" in message
    assert "only have" in message


@pytest.mark.asyncio
async def test_handle_mystery_gift_success_deducts_balance():
    """Test that mystery gift deducts 10k JC and sends announcement."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_MYSTERY_GIFT_COST + 100

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_mystery_gift(interaction)

    # Verify balance was deducted
    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id, None, -SHOP_MYSTERY_GIFT_COST
    )

    # Verify public announcement was sent
    interaction.response.send_message.assert_awaited_once()
    kwargs = interaction.response.send_message.call_args.kwargs
    assert "embed" in kwargs
    embed = kwargs["embed"]
    assert "Mystery Gift" in embed.title


@pytest.mark.asyncio
async def test_shop_mystery_gift_routes_to_handler(monkeypatch):
    """Test that selecting mystery_gift item routes to the handler."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = None  # Will fail registration check

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, "mystery_gift", target=None)

    # Should have called the handler which checks registration
    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "need to `/player register`" in message


# --- Recalibrate tests ---


@pytest.mark.asyncio
async def test_handle_recalibrate_success(monkeypatch):
    """Successful recalibrate purchase deducts balance and calls service."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_balance.return_value = 2000
    recal_service = MagicMock()
    recal_service.can_recalibrate.return_value = {
        "allowed": True,
        "current_rating": 1500.0,
        "current_rd": 63.0,
        "current_volatility": 0.06,
        "games_played": 20,
    }
    recal_service.recalibrate.return_value = {
        "success": True,
        "old_rating": 1500.0,
        "old_rd": 63.0,
        "old_volatility": 0.06,
        "new_rd": 350.0,
        "new_volatility": 0.06,
        "total_recalibrations": 1,
        "cooldown_ends_at": 9999999999,
    }

    cmds = ShopCommands(bot, player_service, recalibration_service=recal_service)
    interaction = _make_interaction()

    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock())
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())
    monkeypatch.setattr("commands.shop.get_hero_image_url", lambda _id: None)

    await cmds._handle_recalibrate(interaction)

    # Balance deducted
    player_service.adjust_balance.assert_called_once_with(1001, None, -SHOP_RECALIBRATE_COST)
    # Recalibration executed
    recal_service.recalibrate.assert_called_once_with(1001, None)


@pytest.mark.asyncio
async def test_handle_recalibrate_on_cooldown():
    """Recalibrate blocked when on cooldown — no balance deducted."""
    bot = MagicMock()
    player_service = MagicMock()
    recal_service = MagicMock()
    recal_service.can_recalibrate.return_value = {
        "allowed": False,
        "reason": "on_cooldown",
        "cooldown_ends_at": 9999999999,
    }

    cmds = ShopCommands(bot, player_service, recalibration_service=recal_service)
    interaction = _make_interaction()

    await cmds._handle_recalibrate(interaction)

    interaction.response.send_message.assert_awaited_once()
    msg = interaction.response.send_message.call_args.args[0]
    assert "cooldown" in msg.lower()
    # Balance NOT deducted
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_recalibrate_insufficient_balance():
    """Recalibrate blocked when balance is too low."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_balance.return_value = 100
    recal_service = MagicMock()
    recal_service.can_recalibrate.return_value = {
        "allowed": True,
        "current_rating": 1500.0,
        "current_rd": 63.0,
        "current_volatility": 0.06,
        "games_played": 20,
    }

    cmds = ShopCommands(bot, player_service, recalibration_service=recal_service)
    interaction = _make_interaction()

    await cmds._handle_recalibrate(interaction)

    interaction.response.send_message.assert_awaited_once()
    msg = interaction.response.send_message.call_args.args[0]
    assert "300" in msg
    assert "100" in msg
    # Balance NOT deducted
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_recalibrate_not_registered():
    """Recalibrate blocked when player is not registered."""
    bot = MagicMock()
    player_service = MagicMock()
    recal_service = MagicMock()
    recal_service.can_recalibrate.return_value = {
        "allowed": False,
        "reason": "not_registered",
    }

    cmds = ShopCommands(bot, player_service, recalibration_service=recal_service)
    interaction = _make_interaction()

    await cmds._handle_recalibrate(interaction)

    interaction.response.send_message.assert_awaited_once()
    msg = interaction.response.send_message.call_args.args[0]
    assert "register" in msg.lower()
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_item_autocomplete_shows_cooldown():
    """Autocomplete shows 'ON COOLDOWN' when user is on cooldown."""
    bot = MagicMock()
    player_service = MagicMock()
    recal_service = MagicMock()
    recal_service.can_recalibrate.return_value = {
        "allowed": False,
        "reason": "on_cooldown",
        "cooldown_ends_at": 9999999999,
    }

    cmds = ShopCommands(bot, player_service, recalibration_service=recal_service)
    interaction = _make_interaction()

    choices = await cmds.item_autocomplete(interaction, "")

    recal_choices = [c for c in choices if "recalibrate" in c.value.lower()]
    assert len(recal_choices) == 1
    assert recal_choices[0].value == "recalibrate_cooldown"
    assert "ON COOLDOWN" in recal_choices[0].name


@pytest.mark.asyncio
async def test_item_autocomplete_shows_price_when_available():
    """Autocomplete shows price when recalibration is available."""
    bot = MagicMock()
    player_service = MagicMock()
    recal_service = MagicMock()
    recal_service.can_recalibrate.return_value = {
        "allowed": True,
        "current_rating": 1500.0,
        "current_rd": 63.0,
        "current_volatility": 0.06,
        "games_played": 20,
    }

    cmds = ShopCommands(bot, player_service, recalibration_service=recal_service)
    interaction = _make_interaction()

    choices = await cmds.item_autocomplete(interaction, "")

    recal_choices = [c for c in choices if c.value == "recalibrate"]
    assert len(recal_choices) == 1
    assert "300" in recal_choices[0].name
