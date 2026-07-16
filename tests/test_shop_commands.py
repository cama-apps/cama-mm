"""
Tests for shop commands.
"""

from types import SimpleNamespace
from unittest.mock import ANY, AsyncMock, MagicMock

import pytest

from commands.shop import (
    PINGEDASH_COOLDOWN_SECONDS,
    PINGEDASH_COST,
    PINGEDASH_TENOR_URL,
    SHOP_ANNOUNCE_COST,
    SHOP_ANNOUNCE_TARGET_COST,
    SHOP_JOPA_COIN_COST,
    SHOP_NEW_MYSTERY_GIFT_COST,
    SHOP_PACKAGE_DEAL_BASE_COST,
    SHOP_PACKAGE_DEAL_RATING_DIVISOR,
    SHOP_RECALIBRATE_COST,
    SHOP_WITCHS_CURSE_COST,
    ShopCommands,
)


def _make_interaction(user_id: int = 1001, guild_id: int | None = None):
    interaction = MagicMock()
    interaction.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>", display_name="Buyer")
    interaction.response = AsyncMock()
    interaction.followup = AsyncMock()
    interaction.guild = SimpleNamespace(id=guild_id) if guild_id is not None else None
    interaction.channel = None
    return interaction


def test_pingedash_description_uses_configured_cost():
    assert ShopCommands.pingedash.description == (
        f"Spend {PINGEDASH_COST} jopacoin to send the Pingedash"
    )


@pytest.mark.asyncio
async def test_pingedash_is_not_a_shop_item():
    commands = ShopCommands(MagicMock(), MagicMock())
    choices = await commands.item_autocomplete(_make_interaction(guild_id=9000), "")

    assert "pingedash" not in {choice.value for choice in choices}


@pytest.mark.asyncio
async def test_shop_pricing_autocomplete_labels_do_not_claim_free_or_minimum_soft_avoid():
    commands = ShopCommands(MagicMock(), MagicMock())
    choices = await commands.item_autocomplete(_make_interaction(guild_id=9000), "")
    labels = {choice.value: choice.name for choice in choices}

    assert "FREE" not in labels["package_deal"]
    assert "0 active" in labels["package_deal"]
    assert "from 750" not in labels["soft_avoid"]
    assert "dynamic" in labels["soft_avoid"]


@pytest.mark.asyncio
async def test_pingedash_requires_configured_target(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    monkeypatch.setattr("commands.shop.PINGEDASH_TARGET_USER_ID", None)

    await commands._handle_pingedash(interaction)

    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.call_args.kwargs["ephemeral"] is True
    player_service.try_purchase_pingedash.assert_not_called()


@pytest.mark.asyncio
async def test_pingedash_success_charges_and_mentions_only_configured_target(monkeypatch):
    frozen_now = 1_700_000_000
    target_user_id = 123456789012345678
    bot = MagicMock()
    player_service = MagicMock()
    player_service.try_purchase_pingedash.return_value = {
        "success": True,
        "reason": None,
        "balance": 40,
        "cooldown_ends_at": frozen_now + PINGEDASH_COOLDOWN_SECONDS,
    }
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    monkeypatch.setattr("commands.shop.PINGEDASH_TARGET_USER_ID", target_user_id)
    monkeypatch.setattr("commands.shop.time.time", lambda: frozen_now)

    await commands._handle_pingedash(interaction)

    player_service.try_purchase_pingedash.assert_called_once_with(
        interaction.user.id,
        interaction.guild.id,
        cost=PINGEDASH_COST,
        now=frozen_now,
        cooldown_seconds=PINGEDASH_COOLDOWN_SECONDS,
    )
    interaction.followup.send.assert_awaited_once()
    kwargs = interaction.followup.send.call_args.kwargs
    assert kwargs["content"] == f"<@{target_user_id}>\n{PINGEDASH_TENOR_URL}"
    assert [user.id for user in kwargs["allowed_mentions"].users] == [target_user_id]
    assert kwargs["allowed_mentions"].everyone is False
    assert kwargs["allowed_mentions"].roles is False


@pytest.mark.asyncio
async def test_pingedash_cooldown_response_is_private(monkeypatch):
    cooldown_ends_at = 1_700_086_400
    bot = MagicMock()
    player_service = MagicMock()
    player_service.try_purchase_pingedash.return_value = {
        "success": False,
        "reason": "on_cooldown",
        "balance": 50,
        "cooldown_ends_at": cooldown_ends_at,
    }
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    monkeypatch.setattr("commands.shop.PINGEDASH_TARGET_USER_ID", 123456789012345678)

    await commands._handle_pingedash(interaction)

    kwargs = interaction.followup.send.call_args.kwargs
    assert kwargs["ephemeral"] is True
    assert f"<t:{cooldown_ends_at}:R>" in kwargs["content"]
    assert PINGEDASH_TENOR_URL not in kwargs["content"]


def test_pingedash_purchase_debits_once_per_cooldown(player_repository):
    user_id = 1001
    guild_id = 9000
    now = 1_700_000_000
    player_repository.add(user_id, "Buyer", guild_id)
    player_repository.update_balance(user_id, guild_id, 30)

    first = player_repository.try_purchase_pingedash(
        user_id,
        guild_id,
        cost=10,
        now=now,
        cooldown_seconds=86_400,
    )
    blocked = player_repository.try_purchase_pingedash(
        user_id,
        guild_id,
        cost=10,
        now=now + 86_399,
        cooldown_seconds=86_400,
    )
    second = player_repository.try_purchase_pingedash(
        user_id,
        guild_id,
        cost=10,
        now=now + 86_400,
        cooldown_seconds=86_400,
    )

    assert first == {
        "success": True,
        "reason": None,
        "balance": 20,
        "cooldown_ends_at": now + 86_400,
    }
    assert blocked == {
        "success": False,
        "reason": "on_cooldown",
        "balance": 20,
        "cooldown_ends_at": now + 86_400,
    }
    assert second["success"] is True
    assert second["balance"] == 10
    assert player_repository.get_balance(user_id, guild_id) == 10

    with player_repository.connection() as conn:
        ledger_rows = conn.execute(
            """
            SELECT delta, source, related_type, related_id
            FROM economy_ledger_entries
            WHERE guild_id = ? AND account_id = ? AND source = 'pingedash'
            ORDER BY ledger_id
            """,
            (guild_id, str(user_id)),
        ).fetchall()
    assert [dict(row) for row in ledger_rows] == [
        {
            "delta": -10,
            "source": "pingedash",
            "related_type": "command",
            "related_id": "pingedash",
        },
        {
            "delta": -10,
            "source": "pingedash",
            "related_type": "command",
            "related_id": "pingedash",
        },
    ]


def test_pingedash_insufficient_balance_does_not_claim_cooldown(player_repository):
    user_id = 1001
    guild_id = 9000
    now = 1_700_000_000
    player_repository.add(user_id, "Buyer", guild_id)
    player_repository.update_balance(user_id, guild_id, 9)

    rejected = player_repository.try_purchase_pingedash(
        user_id,
        guild_id,
        cost=10,
        now=now,
        cooldown_seconds=86_400,
    )
    player_repository.update_balance(user_id, guild_id, 10)
    purchased = player_repository.try_purchase_pingedash(
        user_id,
        guild_id,
        cost=10,
        now=now,
        cooldown_seconds=86_400,
    )

    assert rejected["reason"] == "insufficient_balance"
    assert rejected["cooldown_ends_at"] is None
    assert purchased["success"] is True
    assert purchased["balance"] == 0


@pytest.mark.asyncio
async def test_shop_requires_target_for_announce_target(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    commands = ShopCommands(bot, player_service)

    interaction = _make_interaction(guild_id=9000)

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


# --- Soft Avoid / Package Deal ---


@pytest.mark.asyncio
async def test_handle_soft_avoid_prices_from_teammate_winrate(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.create_or_extend_avoid.return_value = SimpleNamespace(games_remaining=10)
    bot.pairings_service.get_head_to_head.return_value = {
        "games_together": 7,
        "wins_together": 2,
    }
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    player_service.get_balance.return_value = 429
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    bot.pairings_service.get_head_to_head.assert_called_once_with(
        interaction.user.id,
        target.id,
        interaction.guild.id,
    )
    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id,
        interaction.guild.id,
        -429,
    )
    bot.soft_avoid_service.create_or_extend_avoid.assert_called_once()
    assert bot.soft_avoid_service.create_or_extend_avoid.call_args.kwargs["avoided_id"] == target.id


@pytest.mark.asyncio
async def test_handle_soft_avoid_uses_default_price_before_three_games(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.create_or_extend_avoid.return_value = SimpleNamespace(games_remaining=10)
    bot.pairings_service.get_head_to_head.return_value = {
        "games_together": 2,
        "wins_together": 2,
    }
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    player_service.get_balance.return_value = 750
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id,
        interaction.guild.id,
        -750,
    )


@pytest.mark.asyncio
async def test_handle_soft_avoid_uses_default_price_with_no_pairing_data(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.create_or_extend_avoid.return_value = SimpleNamespace(games_remaining=10)
    bot.pairings_service.get_head_to_head.return_value = None
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    player_service.get_balance.return_value = 750
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id,
        interaction.guild.id,
        -750,
    )


@pytest.mark.asyncio
async def test_handle_soft_avoid_refunds_when_create_fails_after_debit(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.create_or_extend_avoid.side_effect = RuntimeError("db locked")
    bot.pairings_service.get_head_to_head.return_value = None
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    player_service.get_balance.return_value = 750
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    assert [call.args for call in player_service.adjust_balance.call_args_list] == [
        (interaction.user.id, interaction.guild.id, -750),
        (interaction.user.id, interaction.guild.id, 750),
    ]
    kwargs = safe_followup.call_args.kwargs
    assert kwargs["ephemeral"] is True
    assert "refund" in kwargs["content"].lower()


@pytest.mark.asyncio
async def test_handle_soft_avoid_zero_price_create_failure_does_not_claim_refund(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.create_or_extend_avoid.side_effect = RuntimeError("db locked")
    bot.pairings_service.get_head_to_head.return_value = {
        "games_together": 3,
        "wins_together": 0,
    }
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    player_service.adjust_balance.assert_not_called()
    message = safe_followup.call_args.kwargs["content"].lower()
    assert "refund" not in message
    assert "not charged" in message


@pytest.mark.asyncio
async def test_handle_soft_avoid_allows_zero_price_after_three_losses(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.create_or_extend_avoid.return_value = SimpleNamespace(games_remaining=10)
    bot.pairings_service.get_head_to_head.return_value = {
        "games_together": 3,
        "wins_together": 0,
    }
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    safe_defer = AsyncMock(return_value=True)
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_defer", safe_defer)
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    player_service.get_balance.assert_not_called()
    player_service.adjust_balance.assert_not_called()
    safe_defer.assert_awaited_once_with(interaction, ephemeral=True)
    safe_followup.assert_awaited_once()
    bot.soft_avoid_service.create_or_extend_avoid.assert_called_once()


@pytest.mark.asyncio
async def test_handle_package_deal_costs_one_jopacoin_with_no_active_deals(monkeypatch):
    bot = MagicMock()
    bot.package_deal_service.get_user_deals.return_value = []
    bot.package_deal_service.create_or_extend_deal.return_value = SimpleNamespace(games_remaining=10)
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    player_service.get_balance.return_value = 1
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id,
        interaction.guild.id,
        -1,
    )
    assert bot.package_deal_service.create_or_extend_deal.call_args.kwargs["cost"] == 1


@pytest.mark.asyncio
async def test_handle_package_deal_refunds_one_jopacoin_when_create_fails(monkeypatch):
    bot = MagicMock()
    bot.package_deal_service.get_user_deals.return_value = []
    bot.package_deal_service.create_or_extend_deal.side_effect = RuntimeError("db locked")
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    player_service.get_balance.return_value = 1
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    assert [call.args for call in player_service.adjust_balance.call_args_list] == [
        (interaction.user.id, interaction.guild.id, -1),
        (interaction.user.id, interaction.guild.id, 1),
    ]
    kwargs = safe_followup.call_args.kwargs
    assert kwargs["ephemeral"] is True
    assert "refund" in kwargs["content"].lower()


@pytest.mark.asyncio
async def test_handle_package_deal_existing_active_deal_keeps_normal_price(monkeypatch):
    bot = MagicMock()
    bot.package_deal_service.get_user_deals.return_value = [
        SimpleNamespace(partner_discord_id=3003),
    ]
    bot.package_deal_service.create_or_extend_deal.return_value = SimpleNamespace(games_remaining=10)
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    normal_cost = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
    player_service.get_balance.return_value = normal_cost
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    player_service.adjust_balance.assert_called_once_with(
        interaction.user.id,
        interaction.guild.id,
        -normal_cost,
    )
    assert bot.package_deal_service.create_or_extend_deal.call_args.kwargs["cost"] == normal_cost


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
    interaction = _make_interaction(guild_id=9000)

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
    interaction = _make_interaction(guild_id=9000)

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
    interaction = _make_interaction(guild_id=9000)

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
async def test_double_or_nothing_rejects_balance_equal_to_cost():
    """DoN must reject when balance == cost: nothing left to double, spin would pay nothing."""
    from commands.shop import SHOP_DOUBLE_OR_NOTHING_COST

    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_DOUBLE_OR_NOTHING_COST
    player_service.get_last_double_or_nothing.return_value = None  # no cooldown

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9001)

    await commands._handle_double_or_nothing(interaction)

    # Should send an ephemeral rejection; must NOT deduct balance or call adjust_balance
    interaction.response.send_message.assert_awaited_once()
    message_kwargs = interaction.response.send_message.call_args.kwargs
    assert message_kwargs.get("ephemeral") is True
    body = interaction.response.send_message.call_args.args[0]
    assert "double" in body.lower() or "nothing" in body.lower() or "earn" in body.lower()
    player_service.adjust_balance.assert_not_called()


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


@pytest.mark.asyncio
async def test_regrowth_recovers_losses_within_24h_even_before_4am_reset(monkeypatch, repo_db_path):
    """Regrowth must recover losses from the last 24h, not only losses since
    today's 4 AM PST reset.

    Bug ("recovers 0"): the loss window was a 4 AM-PST calendar bucket, so a
    player who lost JC last night and ran regrowth the next morning — after the
    4 AM rollover — had every loss dropped. The fix uses a rolling 24h window
    (matching the sibling Mana Shield item).

    Scenario: it is 4:30 AM PST and the player lost 1000 JC twelve hours earlier
    (≈4:30 PM PST yesterday) — within 24h but *before* today's 4 AM reset.
    Regrowth should still credit 35%, capped at 120.
    """
    import datetime as dt
    import sqlite3

    from repositories.bet_repository import BetRepository
    from repositories.player_repository import PlayerRepository
    from tests.conftest import TEST_GUILD_ID
    from utils.game_date import _PST

    user_id = 4242
    # Freeze "now" to 4:30 AM PST. The old 4 AM-PST bucket would exclude a loss
    # from 12h ago; a rolling 24h window includes it.
    frozen_now = int(dt.datetime(2026, 5, 20, 4, 30, tzinfo=_PST).timestamp())
    monkeypatch.setattr("commands.shop.time", SimpleNamespace(time=lambda: frozen_now))

    # A 200 JC bet at 5x on the losing side = -1000, placed 12h before "now"
    # (yesterday afternoon, before today's 4 AM PST).
    conn = sqlite3.connect(repo_db_path)
    conn.execute(
        "INSERT INTO matches (match_id, team1_players, team2_players, winning_team, guild_id)"
        " VALUES (1,'[]','[]',2,?)",
        (TEST_GUILD_ID,),
    )
    conn.execute(
        "INSERT INTO bets (guild_id, match_id, discord_id, team_bet_on, amount, bet_time, leverage)"
        " VALUES (?,1,?,'radiant',200,?,5)",
        (TEST_GUILD_ID, user_id, frozen_now - 12 * 3600),
    )
    conn.commit()
    conn.close()

    gambling = SimpleNamespace(
        bet_repo=BetRepository(repo_db_path),
        player_repo=PlayerRepository(repo_db_path),
    )

    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Green")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=user_id)
    player_service.get_balance.return_value = 1000

    shop = ShopCommands(bot, player_service, gambling_stats_service=gambling)
    interaction = _make_interaction(user_id=user_id, guild_id=TEST_GUILD_ID)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="regrowth"), target=None,
    )

    # 35% of the 1000 loss, capped at 120, credited back via adjust_balance.
    recovery_calls = [
        c for c in player_service.adjust_balance.call_args_list
        if c.args == (user_id, TEST_GUILD_ID, 120)
    ]
    assert recovery_calls, (
        "Regrowth should credit 120 (35% of a 1000 loss from 12h ago, capped); "
        f"adjust_balance calls were {player_service.adjust_balance.call_args_list}"
    )


@pytest.mark.asyncio
async def test_manashop_reprieve_grants_pool_and_reconciles_rolling_losses(
    monkeypatch,
):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))

    user_id = 4242
    guild_id = 9001
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="White")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True
    bot.buff_service = MagicMock()
    bot.buff_service.grant_reprieve.return_value = 77
    bot.protection_service = MagicMock()
    bot.protection_service.reconcile_purchased_pool.return_value = 10

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=user_id)
    player_service.get_balance.return_value = 100

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=user_id, guild_id=guild_id)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="reprieve"), target=None,
    )

    assert player_service.adjust_balance.call_args_list[0].args == (
        user_id,
        guild_id,
        -15,
    )
    bot.buff_service.grant_reprieve.assert_called_once_with(user_id, guild_id)
    bot.protection_service.reconcile_purchased_pool.assert_called_once_with(
        user_id, guild_id, 77, 24 * 3600,
    )
    bot.mana_repo.mark_item_used_atomic.assert_called_once_with(
        user_id, guild_id, "reprieve", ANY,
    )
    message = interaction.followup.send.call_args.args[0]
    assert "REPRIEVE" in message
    assert "Recovered **10" in message
    assert "balance: 95" in message


@pytest.mark.asyncio
async def test_manashop_pyroclasm_uses_applied_losses_for_bounty(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr(
        "commands.shop.random.sample", lambda population, count: population[:count]
    )

    def pyroclasm_roll(low, high):
        assert (low, high) == (12, 28)
        return 20

    monkeypatch.setattr("commands.shop.random.randint", pyroclasm_roll)

    buyer_id = 4242
    guild_id = 9001
    targets = [
        SimpleNamespace(
            discord_id=5000 + index,
            name=f"Target {index}",
            jopacoin_balance=100,
        )
        for index in range(3)
    ]
    outcomes = [
        SimpleNamespace(applied_loss=0, absorbed_amount=18),
        SimpleNamespace(applied_loss=9, absorbed_amount=9),
        SimpleNamespace(applied_loss=18, absorbed_amount=0),
    ]

    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Red")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True
    bot.protection_service = MagicMock()
    bot.protection_service.apply_hostile_loss.side_effect = outcomes

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=buyer_id)
    player_service.get_balance.return_value = 500
    player_service.get_leaderboard.return_value = targets

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=buyer_id, guild_id=guild_id)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="pyroclasm"), target=None,
    )

    protection_calls = bot.protection_service.apply_hostile_loss.call_args_list
    assert len(protection_calls) == 3
    prefixes = {
        call.kwargs["event_key"].rsplit(":", 1)[0]
        for call in protection_calls
    }
    assert len(prefixes) == 1
    for target, call in zip(targets, protection_calls, strict=True):
        assert call.args[:3] == (target.discord_id, guild_id, 18)
        assert call.kwargs["kind"] == "pyroclasm"
        assert call.kwargs["destination"] == "burn"
        assert call.kwargs["clamp_to_balance"] is True

    balance_calls = [call.args for call in player_service.adjust_balance.call_args_list]
    assert balance_calls == [
        (buyer_id, guild_id, -25),
        (buyer_id, guild_id, 13),
    ]
    message = interaction.followup.send.call_args.args[0]
    assert "**27" in message
    assert "You claim **13" in message
    assert "Shields absorbed **27" in message


@pytest.mark.asyncio
async def test_manashop_soul_harvest_gateway_moves_only_applied_loss(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    bonus_roll = MagicMock(side_effect=[0.19, 0.20])
    monkeypatch.setattr("commands.shop.random.random", bonus_roll)

    buyer_id = 4242
    guild_id = 9001
    targets = [
        SimpleNamespace(discord_id=5001, name="A", jopacoin_balance=100),
        SimpleNamespace(discord_id=5002, name="B", jopacoin_balance=100),
    ]
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Black")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True
    bot.protection_service = MagicMock()
    bot.protection_service.apply_hostile_loss.side_effect = [
        SimpleNamespace(applied_loss=1, absorbed_amount=2),
        SimpleNamespace(applied_loss=1, absorbed_amount=1),
    ]

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=buyer_id)
    player_service.get_balance.return_value = 500
    player_service.get_leaderboard.return_value = targets

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=buyer_id, guild_id=guild_id)
    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="soul_harvest"), target=None,
    )

    assert [call.args for call in player_service.adjust_balance.call_args_list] == [
        (buyer_id, guild_id, -25),
    ]
    calls = bot.protection_service.apply_hostile_loss.call_args_list
    assert [call.args[:3] for call in calls] == [
        (targets[0].discord_id, guild_id, 3),
        (targets[1].discord_id, guild_id, 2),
    ]
    assert bonus_roll.call_count == len(targets)
    for call in calls:
        assert call.kwargs["kind"] == "soul_harvest"
        assert call.kwargs["destination"] == "player"
        assert call.kwargs["recipient_id"] == buyer_id
    message = interaction.followup.send.call_args.args[0]
    assert "Gained **2" in message
    assert "Shields absorbed **3" in message


@pytest.mark.asyncio
async def test_manashop_soul_harvest_keeps_effect_but_claims_daily_slot(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.random.random", lambda: 1.0)

    buyer_id = 4242
    guild_id = 9001
    positive_players = [
        SimpleNamespace(discord_id=5000 + idx, name=f"Target {idx}", jopacoin_balance=100)
        for idx in range(3)
    ]
    low_player = SimpleNamespace(discord_id=6666, name="Low", jopacoin_balance=49)
    zero_player = SimpleNamespace(discord_id=7777, name="Flat", jopacoin_balance=0)
    bankrupt_player = SimpleNamespace(discord_id=8888, name="Bankrupt", jopacoin_balance=-10)

    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Black")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=buyer_id)
    player_service.get_balance.return_value = 500
    player_service.get_leaderboard.return_value = [
        *positive_players,
        low_player,
        zero_player,
        bankrupt_player,
    ]

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=buyer_id, guild_id=guild_id)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="soul_harvest"), target=None,
    )

    calls = [c.args for c in player_service.adjust_balance.call_args_list]
    assert calls[0] == (buyer_id, guild_id, -25)
    assert calls[-1] == (buyer_id, guild_id, 6)
    assert not any(c[0] == low_player.discord_id for c in calls)
    assert not any(c[0] == zero_player.discord_id for c in calls)
    assert not any(c[0] == bankrupt_player.discord_id for c in calls)

    victim_debits = [c for c in calls if c[0] != buyer_id and c[2] < 0]
    assert len(victim_debits) == len(positive_players)
    assert sum(-delta for _, _, delta in victim_debits) == 6
    assert all(delta == -2 for _, _, delta in victim_debits)

    message = interaction.followup.send.call_args.args[0]
    assert "drains the living" in message.lower()
    assert "balance: 481" in message
    bot.mana_repo.mark_item_used_atomic.assert_called_once_with(
        buyer_id, guild_id, "soul_harvest", ANY,
    )


@pytest.mark.asyncio
async def test_manashop_soul_harvest_refunds_and_releases_daily_slot_without_targets(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))

    buyer_id = 4242
    guild_id = 9001
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Black")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=buyer_id)
    player_service.get_balance.return_value = 500
    player_service.get_leaderboard.return_value = [
        SimpleNamespace(discord_id=7777, name="Flat A", jopacoin_balance=0),
        SimpleNamespace(discord_id=8888, name="Flat", jopacoin_balance=0),
        SimpleNamespace(discord_id=9999, name="Bankrupt", jopacoin_balance=-100),
    ]

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=buyer_id, guild_id=guild_id)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="soul_harvest"), target=None,
    )

    assert [c.args for c in player_service.adjust_balance.call_args_list] == [
        (buyer_id, guild_id, -25),
        (buyer_id, guild_id, 25),
    ]
    message = interaction.followup.send.call_args.args[0]
    assert "No living souls" in message
    bot.mana_repo.unmark_item_used.assert_called_once_with(
        buyer_id, guild_id, "soul_harvest", ANY,
    )


@pytest.mark.asyncio
async def test_manashop_wildfire_reward_uses_post_shield_loss(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))

    def wildfire_roll(low, high):
        assert (low, high) == (4, 14)
        return 10

    monkeypatch.setattr("commands.shop.random.randint", wildfire_roll)

    buyer_id = 4242
    guild_id = 9001
    victim = SimpleNamespace(discord_id=5001, name="Target", jopacoin_balance=100)
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Red")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True
    bot.mana_repo.mark_mana_consumed_atomic.return_value = True
    bot.dig_service = None
    bot.protection_service = MagicMock()
    bot.protection_service.apply_hostile_loss.return_value = SimpleNamespace(
        applied_loss=4,
        absorbed_amount=5,
    )

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=buyer_id)
    player_service.get_balance.return_value = 500
    player_service.get_leaderboard.return_value = [victim]

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=buyer_id, guild_id=guild_id)
    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="wildfire"), target=None,
    )

    call = bot.protection_service.apply_hostile_loss.call_args
    assert call.args[:3] == (victim.discord_id, guild_id, 9)
    assert call.kwargs["kind"] == "wildfire"
    assert call.kwargs["destination"] == "burn"
    # 45% of the 4 JC that landed floors to 1; the absorbed 5 pays nothing.
    assert [entry.args for entry in player_service.adjust_balance.call_args_list] == [
        (buyer_id, guild_id, -150),
        (buyer_id, guild_id, 1),
    ]
    message = interaction.followup.send.call_args.args[0]
    assert "Drained **4" in message
    assert "claim **1" in message
    assert "Shields absorbed **5" in message


@pytest.mark.asyncio
async def test_manashop_sanctuary_costs_90_without_match_bonus(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))

    buyer_id = 4242
    ally_id = 5001
    guild_id = 9001
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="White")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_item_used_atomic.return_value = True
    bot.mana_repo.mark_mana_consumed_atomic.return_value = True
    bot.dig_service = None
    bot.buff_service = MagicMock()

    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(discord_id=buyer_id),
        SimpleNamespace(discord_id=ally_id),
    ]
    player_service.get_balance.return_value = 500

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=buyer_id, guild_id=guild_id)
    target = SimpleNamespace(
        id=ally_id,
        mention=f"<@{ally_id}>",
        display_name="Ally",
    )
    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="sanctuary"), target=target,
    )

    assert player_service.adjust_balance.call_args_list[0].args == (
        buyer_id,
        guild_id,
        -90,
    )
    bot.buff_service.grant_sanctuary.assert_called_once_with(
        buyer_id, guild_id, ally_id,
    )
    message = interaction.followup.send.call_args.args[0]
    assert "150" in message
    assert "match" not in message.lower()


@pytest.mark.asyncio
async def test_dark_bargain_due_amount_matches_loan_principal():
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Black")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.mark_mana_consumed_atomic.return_value = True
    bot.buff_service = MagicMock()

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=1001)
    player_service.get_balance.return_value = 1000

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="dark_bargain"), target=None,
    )

    bot.buff_service.grant_dark_bargain_debt.assert_called_once_with(
        interaction.user.id, interaction.guild.id, amount_due=700, due_in_days=7,
    )
    assert "700 due in 7 days" in interaction.followup.send.call_args.args[0]
