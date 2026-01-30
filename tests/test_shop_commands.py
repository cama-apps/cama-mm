"""
Tests for shop commands.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.shop import ShopCommands, SHOP_ANNOUNCE_COST, SHOP_ANNOUNCE_TARGET_COST, SHOP_MYSTERY_GIFT_COST


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
    item = SimpleNamespace(value="announce_target")

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, item, target=None)

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
    assert "need to `/register`" in message


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
    player_service.player_repo = MagicMock()

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    monkeypatch.setattr("commands.shop.random.choice", lambda _items: "Test message")
    monkeypatch.setattr("commands.shop.get_hero_color", lambda _hero_id: None)
    monkeypatch.setattr("commands.shop.get_hero_image_url", lambda _hero_id: None)

    await commands._handle_announce(interaction, target=target)

    player_service.player_repo.add_balance.assert_called_once_with(
        interaction.user.id, -SHOP_ANNOUNCE_TARGET_COST
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
    assert "need to `/register`" in message


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
    player_service.player_repo = MagicMock()

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_mystery_gift(interaction)

    # Verify balance was deducted
    player_service.player_repo.add_balance.assert_called_once_with(
        interaction.user.id, -SHOP_MYSTERY_GIFT_COST
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
    item = SimpleNamespace(value="mystery_gift")

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, item, target=None)

    # Should have called the handler which checks registration
    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "need to `/register`" in message
