"""
Tests for shop commands.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from commands.shop import (
    SHOP_ANNOUNCE_COST,
    SHOP_ANNOUNCE_TARGET_COST,
    SHOP_JOPA_COIN_COST,
    SHOP_NEW_MYSTERY_GIFT_COST,
    SHOP_RECALIBRATE_COST,
    SHOP_WITCHS_CURSE_COST,
    ShopCommands,
)


def _make_interaction(user_id: int = 1001):
    interaction = MagicMock()
    interaction.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>", display_name="Buyer")
    interaction.response = AsyncMock()
    interaction.followup = AsyncMock()
    interaction.guild = None
    interaction.channel = None
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
    interaction.followup.send.assert_awaited_once()
    kwargs = interaction.followup.send.call_args.kwargs
    assert "embed" in kwargs


# --- Jopa Coin(TM) (renamed 10k flex item) ---


@pytest.mark.asyncio
async def test_handle_jopa_coin_requires_registration():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = None

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_jopa_coin(interaction)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "need to `/player register`" in message


@pytest.mark.asyncio
async def test_handle_jopa_coin_insufficient_balance():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_JOPA_COIN_COST - 1

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_jopa_coin(interaction)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "You need" in message
    assert "only have" in message


@pytest.mark.asyncio
async def test_handle_jopa_coin_success_deducts_balance():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_JOPA_COIN_COST + 100

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_jopa_coin(interaction)

    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id, None, -SHOP_JOPA_COIN_COST
    )
    interaction.response.defer.assert_awaited()
    interaction.followup.send.assert_awaited()
    embed = interaction.followup.send.call_args.kwargs["embed"]
    assert "Jopa Coin" in embed.title


# --- New 20k Mystery Gift ---


@pytest.mark.asyncio
async def test_handle_mystery_gift_requires_registration():
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
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_NEW_MYSTERY_GIFT_COST - 1

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_mystery_gift(interaction)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "You need" in message
    assert "only have" in message


@pytest.mark.asyncio
async def test_handle_mystery_gift_success_deducts_20k():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_NEW_MYSTERY_GIFT_COST + 100

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    await commands._handle_mystery_gift(interaction)

    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id, None, -SHOP_NEW_MYSTERY_GIFT_COST
    )
    interaction.response.defer.assert_awaited()
    interaction.followup.send.assert_awaited()
    embed = interaction.followup.send.call_args.kwargs["embed"]
    assert "Mystery Gift" in embed.title


@pytest.mark.asyncio
async def test_shop_mystery_gift_routes_to_handler(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = None

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, "mystery_gift", target=None)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "need to `/player register`" in message


@pytest.mark.asyncio
async def test_shop_jopa_coin_routes_to_handler(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = None

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, "jopa_coin", target=None)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "need to `/player register`" in message


# --- Witch's Curse ---


@pytest.mark.asyncio
async def test_handle_witchs_curse_requires_target(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, "witchs_curse", target=None)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "didn't specify a target" in message


@pytest.mark.asyncio
async def test_handle_witchs_curse_unavailable_when_service_missing():
    bot = MagicMock()
    player_service = MagicMock()
    commands = ShopCommands(bot, player_service, curse_service=None)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="Victim")

    await commands._handle_witchs_curse(interaction, target=target)

    interaction.response.send_message.assert_awaited_once()
    msg = interaction.response.send_message.call_args.args[0]
    assert "unavailable" in msg.lower() or "currently" in msg.lower()


@pytest.mark.asyncio
async def test_handle_witchs_curse_target_not_registered():
    bot = MagicMock()
    player_service = MagicMock()
    # First call (caster) succeeds; second call (target) returns None
    player_service.get_player.side_effect = [object(), None]
    curse_service = MagicMock()

    commands = ShopCommands(bot, player_service, curse_service=curse_service)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="Victim")

    await commands._handle_witchs_curse(interaction, target=target)

    interaction.response.send_message.assert_awaited_once()
    msg = interaction.response.send_message.call_args.args[0]
    assert "not registered" in msg


@pytest.mark.asyncio
async def test_handle_witchs_curse_insufficient_balance():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    player_service.get_balance.return_value = SHOP_WITCHS_CURSE_COST - 1
    curse_service = MagicMock()

    commands = ShopCommands(bot, player_service, curse_service=curse_service)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="Victim")

    await commands._handle_witchs_curse(interaction, target=target)

    interaction.response.send_message.assert_awaited_once()
    msg = interaction.response.send_message.call_args.args[0]
    assert "You need" in msg


@pytest.mark.asyncio
async def test_handle_witchs_curse_refunds_on_cast_failure():
    """If cast_curse raises after the balance is debited, the buyer must be refunded."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    player_service.get_balance.return_value = SHOP_WITCHS_CURSE_COST + 100
    curse_service = MagicMock()
    curse_service.cast_curse = AsyncMock(side_effect=RuntimeError("db locked"))

    commands = ShopCommands(bot, player_service, curse_service=curse_service)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="Victim")

    await commands._handle_witchs_curse(interaction, target=target)

    # Two adjust_balance calls: the debit, then the refund.
    assert player_service.adjust_balance.call_count == 2
    debit_args = player_service.adjust_balance.call_args_list[0].args
    refund_args = player_service.adjust_balance.call_args_list[1].args
    assert debit_args == (interaction.user.id, None, -SHOP_WITCHS_CURSE_COST)
    assert refund_args == (interaction.user.id, None, SHOP_WITCHS_CURSE_COST)
    # User gets an ephemeral failure message (not the success "hex sealed" line).
    interaction.followup.send.assert_awaited()
    failure_kwargs = interaction.followup.send.call_args.kwargs
    assert failure_kwargs.get("ephemeral") is True
    assert "refund" in failure_kwargs.get("content", "").lower()


@pytest.mark.asyncio
async def test_handle_witchs_curse_success_anonymous():
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    player_service.get_balance.return_value = SHOP_WITCHS_CURSE_COST + 100
    curse_service = MagicMock()
    curse_service.cast_curse = AsyncMock(return_value=1234567890)

    commands = ShopCommands(bot, player_service, curse_service=curse_service)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="Victim")

    await commands._handle_witchs_curse(interaction, target=target)

    # Cost deducted
    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id, None, -SHOP_WITCHS_CURSE_COST
    )
    # Curse cast through service
    curse_service.cast_curse.assert_awaited_once()
    # Ephemeral defer + ephemeral followup (anonymous - no public message)
    interaction.response.defer.assert_awaited()
    defer_kwargs = interaction.response.defer.call_args.kwargs
    assert defer_kwargs.get("ephemeral") is True
    interaction.followup.send.assert_awaited()
    followup_kwargs = interaction.followup.send.call_args.kwargs
    assert followup_kwargs.get("ephemeral") is True
    confirmation = followup_kwargs.get("content", "")
    assert "Victim" in confirmation
    assert "🧙" in confirmation


# --- Recalibrate tests ---


@pytest.mark.asyncio
async def test_handle_recalibrate_success(monkeypatch):
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

    player_service.adjust_balance.assert_called_once_with(1001, None, -SHOP_RECALIBRATE_COST)
    recal_service.recalibrate.assert_called_once_with(1001, None)


@pytest.mark.asyncio
async def test_handle_recalibrate_on_cooldown():
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
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_recalibrate_insufficient_balance():
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
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_recalibrate_not_registered():
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


@pytest.mark.asyncio
async def test_item_autocomplete_includes_curse_and_jopa_coin():
    """Witch's Curse and Jopa Coin(TM) appear in the autocomplete list."""
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
    values = {c.value for c in choices}
    assert "jopa_coin" in values
    assert "mystery_gift" in values
    assert "witchs_curse" in values


@pytest.mark.asyncio
async def test_item_autocomplete_excludes_dig_items():
    """Dig items have been deduped — they live only in /dig buy now."""
    bot = MagicMock()
    player_service = MagicMock()
    dig_service = MagicMock()  # Even WITH dig service, shop should not list dig items
    recal_service = MagicMock()
    recal_service.can_recalibrate.return_value = {
        "allowed": True,
        "current_rating": 1500.0,
        "current_rd": 63.0,
        "current_volatility": 0.06,
        "games_played": 20,
    }

    cmds = ShopCommands(
        bot, player_service, recalibration_service=recal_service, dig_service=dig_service
    )
    interaction = _make_interaction()

    choices = await cmds.item_autocomplete(interaction, "")
    values = {c.value for c in choices}
    assert not any(v.startswith("dig_") for v in values)
