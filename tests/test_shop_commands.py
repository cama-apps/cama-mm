"""
Tests for shop commands.
"""

import sqlite3
import threading
from concurrent.futures import ThreadPoolExecutor
from types import SimpleNamespace
from unittest.mock import ANY, AsyncMock, MagicMock

import discord
import pytest

from commands.shop import (
    JOPACOIN_EMOTE,
    PINGEDASH_COOLDOWN_SECONDS,
    PINGEDASH_COST,
    PINGEDASH_TENOR_URL,
    PINGEDKEVIN_COOLDOWN_SECONDS,
    PINGEDKEVIN_COST,
    PINGEDKEVIN_TENOR_URL,
    SHOP_ANNOUNCE_COST,
    SHOP_ANNOUNCE_TARGET_COST,
    SHOP_JOPA_COIN_COST,
    SHOP_NEW_MYSTERY_GIFT_COST,
    SHOP_PACKAGE_DEAL_BASE_COST,
    SHOP_PACKAGE_DEAL_RATING_DIVISOR,
    SHOP_RECALIBRATE_COST,
    ShopCommands,
    _calculate_soft_avoid_cost,
)
from services.protection_service import ProtectionService


def _make_interaction(user_id: int = 1001, guild_id: int | None = None):
    interaction = MagicMock()
    interaction.user = SimpleNamespace(id=user_id, mention=f"<@{user_id}>", display_name="Buyer")
    interaction.response = AsyncMock()
    interaction.followup = AsyncMock()
    interaction.guild = SimpleNamespace(id=guild_id) if guild_id is not None else None
    interaction.channel = None
    return interaction


def _allow_manashop_purchase(bot, balance_before: int) -> None:
    def purchase(_user_id, _guild_id, _item_id, _today, *, cost, tap_mana):
        return {
            "success": True,
            "reason": None,
            "balance": balance_before - cost,
            "purchase_id": "purchase-1",
        }

    bot.mana_repo.try_purchase_item_atomic.side_effect = purchase


def test_repository_contracts_require_atomic_shop_settlement():
    from repositories.interfaces import IManaRepository, IPlayerRepository

    assert "settle_double_or_nothing_atomic" in IPlayerRepository.__abstractmethods__
    assert {
        "try_purchase_item_atomic",
        "refund_item_purchase_atomic",
        "get_item_purchase",
        "mark_item_purchase_applying_atomic",
        "complete_item_purchase_atomic",
    } <= IManaRepository.__abstractmethods__


def test_pingedash_description_uses_configured_cost():
    assert ShopCommands.pingedash.description == (
        f"Spend {PINGEDASH_COST} jopacoin to send the Pingedash"
    )


def test_pingedkevin_description_and_url():
    assert ShopCommands.pingedkevin.description == (
        f"Spend {PINGEDKEVIN_COST} jopacoin to send the PingedKevin"
    )
    assert PINGEDKEVIN_TENOR_URL == (
        "https://tenor.com/view/in-trouble-mad-face-gif-10963449859613356314"
    )


@pytest.mark.asyncio
async def test_pingedash_is_not_a_shop_item():
    commands = ShopCommands(MagicMock(), MagicMock())
    choices = await commands.item_autocomplete(_make_interaction(guild_id=9000), "")

    assert "pingedash" not in {choice.value for choice in choices}
    assert "pingedkevin" not in {choice.value for choice in choices}


@pytest.mark.asyncio
async def test_protect_hero_is_not_a_shop_item():
    commands = ShopCommands(MagicMock(), MagicMock())
    choices = await commands.item_autocomplete(_make_interaction(guild_id=9000), "")

    assert "protect_hero" not in {choice.value for choice in choices}


def test_shop_buy_has_no_protect_hero_parameter():
    assert "hero" not in {parameter.name for parameter in ShopCommands.shop.parameters}


@pytest.mark.asyncio
async def test_shop_rejects_retired_protect_hero_value(monkeypatch):
    commands = ShopCommands(MagicMock(), MagicMock())
    interaction = _make_interaction(guild_id=9000)
    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, "protect_hero")

    interaction.response.send_message.assert_awaited_once_with(
        "That shop item is no longer available.",
        ephemeral=True,
    )


@pytest.mark.asyncio
async def test_shop_pricing_autocomplete_labels_show_soft_avoid_minimum():
    commands = ShopCommands(MagicMock(), MagicMock())
    choices = await commands.item_autocomplete(_make_interaction(guild_id=9000), "")
    labels = {choice.value: choice.name for choice in choices}

    assert "FREE" not in labels["package_deal"]
    assert "0 active" in labels["package_deal"]
    assert "dynamic" in labels["soft_avoid"]
    assert "minimum 300" in labels["soft_avoid"]


@pytest.mark.asyncio
async def test_package_deal_autocomplete_clamps_configured_duration_to_ten(monkeypatch):
    monkeypatch.setattr("commands.shop.PACKAGE_DEAL_GAMES_DURATION", 25)
    commands = ShopCommands(MagicMock(), MagicMock())

    choices = await commands.item_autocomplete(_make_interaction(guild_id=9000), "")
    package_deal_label = next(
        choice.name for choice in choices if choice.value == "package_deal"
    )

    assert "for 10 games" in package_deal_label
    assert "25 games" not in package_deal_label


def test_soft_avoid_floor_applies_to_configured_default(monkeypatch):
    monkeypatch.setattr("commands.shop.SHOP_SOFT_AVOID_COST", 100)

    assert _calculate_soft_avoid_cost(None) == 300
    assert _calculate_soft_avoid_cost({"games_together": 2, "wins_together": 0}) == 300


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

    # No defer happened, so the direct response is genuinely ephemeral — the
    # first followup after a public defer would ignore ephemeral=True.
    interaction.response.defer.assert_not_awaited()
    interaction.followup.send.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.call_args
    assert kwargs["ephemeral"] is True
    assert f"<t:{cooldown_ends_at}:R>" in args[0]
    assert PINGEDASH_TENOR_URL not in args[0]


@pytest.mark.asyncio
async def test_pingedash_failure_notice_tolerates_lapsed_interaction(monkeypatch):
    """The debit can outlast the 3s ack window under DB contention; a lapsed
    token must not crash the handler — the refused purchase needs no rollback."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.try_purchase_pingedash.return_value = {
        "success": False,
        "reason": "insufficient_balance",
        "balance": 5,
        "cooldown_ends_at": None,
    }
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    interaction.response.send_message.side_effect = discord.NotFound(
        SimpleNamespace(status=404, reason="Not Found"),
        {"message": "Unknown interaction"},
    )
    monkeypatch.setattr("commands.shop.PINGEDASH_TARGET_USER_ID", 123456789012345678)

    await commands._handle_pingedash(interaction)

    player_service.try_purchase_pingedash.assert_called_once()
    interaction.response.defer.assert_not_awaited()
    interaction.followup.send.assert_not_awaited()


@pytest.mark.asyncio
async def test_pingedkevin_requires_configured_target(monkeypatch):
    bot = MagicMock()
    player_service = MagicMock()
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    monkeypatch.setattr("commands.shop.PINGEDKEVIN_TARGET_USER_ID", None)

    await commands._handle_pingedkevin(interaction)

    interaction.response.send_message.assert_awaited_once()
    assert interaction.response.send_message.call_args.kwargs["ephemeral"] is True
    player_service.try_purchase_pingedkevin.assert_not_called()


@pytest.mark.asyncio
async def test_pingedkevin_success_charges_and_mentions_only_configured_target(
    monkeypatch,
):
    frozen_now = 1_700_000_000
    target_user_id = 234567890123456789
    bot = MagicMock()
    player_service = MagicMock()
    player_service.try_purchase_pingedkevin.return_value = {
        "success": True,
        "reason": None,
        "balance": 40,
        "cooldown_ends_at": frozen_now + PINGEDKEVIN_COOLDOWN_SECONDS,
    }
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    monkeypatch.setattr(
        "commands.shop.PINGEDKEVIN_TARGET_USER_ID",
        target_user_id,
    )
    monkeypatch.setattr("commands.shop.time.time", lambda: frozen_now)

    await commands._handle_pingedkevin(interaction)

    player_service.try_purchase_pingedkevin.assert_called_once_with(
        interaction.user.id,
        interaction.guild.id,
        cost=PINGEDKEVIN_COST,
        now=frozen_now,
        cooldown_seconds=PINGEDKEVIN_COOLDOWN_SECONDS,
    )
    interaction.followup.send.assert_awaited_once()
    kwargs = interaction.followup.send.call_args.kwargs
    assert kwargs["content"] == (
        f"<@{target_user_id}>\n{PINGEDKEVIN_TENOR_URL}"
    )
    assert [user.id for user in kwargs["allowed_mentions"].users] == [
        target_user_id
    ]
    assert kwargs["allowed_mentions"].everyone is False
    assert kwargs["allowed_mentions"].roles is False


@pytest.mark.asyncio
async def test_pingedkevin_cooldown_response_is_private(monkeypatch):
    cooldown_ends_at = 1_700_086_400
    bot = MagicMock()
    player_service = MagicMock()
    player_service.try_purchase_pingedkevin.return_value = {
        "success": False,
        "reason": "on_cooldown",
        "balance": 50,
        "cooldown_ends_at": cooldown_ends_at,
    }
    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    monkeypatch.setattr(
        "commands.shop.PINGEDKEVIN_TARGET_USER_ID",
        234567890123456789,
    )

    await commands._handle_pingedkevin(interaction)

    # No defer happened, so the direct response is genuinely ephemeral — the
    # first followup after a public defer would ignore ephemeral=True.
    interaction.response.defer.assert_not_awaited()
    interaction.followup.send.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.call_args
    assert kwargs["ephemeral"] is True
    assert f"<t:{cooldown_ends_at}:R>" in args[0]
    assert PINGEDKEVIN_TENOR_URL not in args[0]


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


def test_pingedkevin_has_an_independent_cooldown_and_ledger_source(
    player_repository,
):
    user_id = 1001
    guild_id = 9000
    now = 1_700_000_000
    player_repository.add(user_id, "Buyer", guild_id)
    player_repository.update_balance(user_id, guild_id, 30)

    ash = player_repository.try_purchase_pingedash(
        user_id,
        guild_id,
        cost=10,
        now=now,
        cooldown_seconds=86_400,
    )
    kevin = player_repository.try_purchase_pingedkevin(
        user_id,
        guild_id,
        cost=10,
        now=now,
        cooldown_seconds=86_400,
    )
    blocked_kevin = player_repository.try_purchase_pingedkevin(
        user_id,
        guild_id,
        cost=10,
        now=now + 1,
        cooldown_seconds=86_400,
    )

    assert ash["success"] is True
    assert kevin == {
        "success": True,
        "reason": None,
        "balance": 10,
        "cooldown_ends_at": now + 86_400,
    }
    assert blocked_kevin == {
        "success": False,
        "reason": "on_cooldown",
        "balance": 10,
        "cooldown_ends_at": now + 86_400,
    }

    with player_repository.connection() as conn:
        player_row = conn.execute(
            """
            SELECT last_pingedash, last_pingedkevin
            FROM players
            WHERE discord_id = ? AND guild_id = ?
            """,
            (user_id, guild_id),
        ).fetchone()
        ledger_rows = conn.execute(
            """
            SELECT delta, source, related_type, related_id
            FROM economy_ledger_entries
            WHERE guild_id = ? AND account_id = ?
              AND source IN ('pingedash', 'pingedkevin')
            ORDER BY ledger_id
            """,
            (guild_id, str(user_id)),
        ).fetchall()

    assert dict(player_row) == {
        "last_pingedash": now,
        "last_pingedkevin": now,
    }
    assert [dict(row) for row in ledger_rows] == [
        {
            "delta": -10,
            "source": "pingedash",
            "related_type": "command",
            "related_id": "pingedash",
        },
        {
            "delta": -10,
            "source": "pingedkevin",
            "related_type": "command",
            "related_id": "pingedkevin",
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


def test_double_or_nothing_settlement_rolls_back_and_retries_once(player_repository):
    """The cooldown, money movement, and stored flip are one transaction."""
    user_id = 1001
    guild_id = 9000
    now = 1_700_000_000
    player_repository.add(user_id, "Buyer", guild_id)
    player_repository.update_balance(user_id, guild_id, 200)

    with player_repository.connection() as conn:
        conn.execute(
            """
            CREATE TRIGGER fail_double_or_nothing_log
            BEFORE INSERT ON double_or_nothing_spins
            BEGIN
                SELECT RAISE(ABORT, 'forced log failure');
            END
            """
        )

    with pytest.raises(sqlite3.IntegrityError, match="forced log failure"):
        player_repository.settle_double_or_nothing_atomic(
            user_id,
            guild_id,
            cost=50,
            won=False,
            now=now,
            cooldown_seconds=86_400,
        )

    assert player_repository.get_balance(user_id, guild_id) == 200
    assert player_repository.get_last_double_or_nothing(user_id, guild_id) is None
    assert player_repository.get_double_or_nothing_history(user_id, guild_id) == []

    with player_repository.connection() as conn:
        conn.execute("DROP TRIGGER fail_double_or_nothing_log")

    settled = player_repository.settle_double_or_nothing_atomic(
        user_id,
        guild_id,
        cost=50,
        won=False,
        now=now,
        cooldown_seconds=86_400,
    )
    repeated = player_repository.settle_double_or_nothing_atomic(
        user_id,
        guild_id,
        cost=50,
        won=True,
        now=now + 1,
        cooldown_seconds=86_400,
    )

    assert settled == {
        "success": True,
        "reason": None,
        "balance_before": 200,
        "balance_at_risk": 150,
        "balance_after": 0,
        "won": False,
        "cooldown_ends_at": now + 86_400,
    }
    assert repeated["success"] is False
    assert repeated["reason"] == "on_cooldown"
    assert player_repository.get_balance(user_id, guild_id) == 0
    assert player_repository.get_double_or_nothing_history(user_id, guild_id) == [
        {
            "cost": 50,
            "balance_before": 150,
            "balance_after": 0,
            "won": False,
            "spin_time": now,
        }
    ]


def test_concurrent_double_or_nothing_settlements_store_one_outcome(player_repository):
    user_id = 1001
    guild_id = 9000
    now = 1_700_000_000
    player_repository.add(user_id, "Buyer", guild_id)
    player_repository.update_balance(user_id, guild_id, 200)
    start = threading.Barrier(2)

    def settle(won: bool):
        start.wait()
        return player_repository.settle_double_or_nothing_atomic(
            user_id,
            guild_id,
            cost=50,
            won=won,
            now=now,
            cooldown_seconds=86_400,
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        results = list(executor.map(settle, [True, False]))

    successful = [result for result in results if result["success"]]
    assert len(successful) == 1
    assert [result["reason"] for result in results if not result["success"]] == [
        "on_cooldown"
    ]
    expected_balance = 300 if successful[0]["won"] else 0
    assert player_repository.get_balance(user_id, guild_id) == expected_balance
    history = player_repository.get_double_or_nothing_history(user_id, guild_id)
    assert len(history) == 1
    assert history[0]["won"] is successful[0]["won"]
    assert history[0]["balance_after"] == expected_balance


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

    # Conditional debit, not adjust_balance: the read-then-adjust pair let two
    # purchases inside the rate-limit window both pass the check and overdraft.
    player_service.try_spend.assert_called_once()
    args, kwargs = player_service.try_spend.call_args
    assert args[:3] == (interaction.user.id, None, SHOP_ANNOUNCE_TARGET_COST)
    assert kwargs["source"] == "shop_announce"
    player_service.adjust_balance.assert_not_called()
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

    # Conditional debit, not adjust_balance: the read-then-adjust pair let two
    # purchases inside the rate-limit window both pass the check and overdraft.
    player_service.try_spend.assert_called_once()
    args, kwargs = player_service.try_spend.call_args
    assert args[:3] == (interaction.user.id, None, SHOP_JOPA_COIN_COST)
    assert kwargs["source"] == "shop_jopa_coin"
    player_service.adjust_balance.assert_not_called()
    interaction.response.defer.assert_awaited()
    interaction.followup.send.assert_awaited()
    embed = interaction.followup.send.call_args.kwargs["embed"]
    assert "Jopa Coin" in embed.title


@pytest.mark.asyncio
async def test_handle_jopa_coin_aborts_when_balance_vanishes_before_the_debit(monkeypatch):
    """Funds gone between the balance read and the debit: no mint, no overdraft.

    The shortfall notice must go out via interaction.response BEFORE the public
    defer — the first followup after a public defer ignores ephemeral=True, so
    a followup here would broadcast the user's balance shortfall publicly.
    """
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    # Check passes...
    player_service.get_balance.return_value = SHOP_JOPA_COIN_COST + 100
    # ...but the funds are gone by the time the debit runs.
    player_service.try_spend.return_value = False

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    safe_defer = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_defer", safe_defer)
    followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", followup)

    await commands._handle_jopa_coin(interaction)

    player_service.try_spend.assert_called_once()
    player_service.adjust_balance.assert_not_called()
    # No defer happened, so the direct response is genuinely ephemeral.
    safe_defer.assert_not_awaited()
    followup.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.call_args
    assert "no longer have" in args[0]
    assert kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_handle_jopa_coin_shortfall_tolerates_lapsed_interaction(monkeypatch):
    """The debit can outlast the 3s ack window under DB contention; a lapsed
    token must not crash the handler — the refused spend needs no rollback."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_JOPA_COIN_COST + 100
    player_service.try_spend.return_value = False

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()
    interaction.response.send_message.side_effect = discord.NotFound(
        SimpleNamespace(status=404, reason="Not Found"),
        {"message": "Unknown interaction"},
    )
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock())
    followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", followup)

    await commands._handle_jopa_coin(interaction)

    player_service.adjust_balance.assert_not_called()
    followup.assert_not_awaited()


# --- Soft Avoid / Package Deal ---


@pytest.mark.asyncio
async def test_handle_soft_avoid_prices_from_teammate_winrate(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.purchase_avoid.return_value = SimpleNamespace(
        success=True, reason=None, balance=571, avoid=SimpleNamespace(games_remaining=10)
    )
    bot.pairings_service.get_head_to_head.return_value = {
        "games_together": 7,
        "wins_together": 2,
    }
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())
    get_neon_service = MagicMock()
    monkeypatch.setattr("commands.shop.get_neon_service", get_neon_service)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    bot.pairings_service.get_head_to_head.assert_called_once_with(
        interaction.user.id,
        target.id,
        interaction.guild.id,
    )
    player_service.adjust_balance.assert_not_called()
    bot.soft_avoid_service.purchase_avoid.assert_called_once_with(
        guild_id=interaction.guild.id,
        avoider_id=interaction.user.id,
        avoided_id=target.id,
        cost=429,
        games=10,
    )
    get_neon_service.assert_not_called()


@pytest.mark.asyncio
async def test_handle_soft_avoid_uses_default_price_before_three_games(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.purchase_avoid.return_value = SimpleNamespace(
        success=True, reason=None, balance=0, avoid=SimpleNamespace(games_remaining=10)
    )
    bot.pairings_service.get_head_to_head.return_value = {
        "games_together": 2,
        "wins_together": 2,
    }
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    player_service.adjust_balance.assert_not_called()
    bot.soft_avoid_service.purchase_avoid.assert_called_once_with(
        guild_id=interaction.guild.id,
        avoider_id=interaction.user.id,
        avoided_id=target.id,
        cost=750,
        games=10,
    )


@pytest.mark.asyncio
async def test_handle_soft_avoid_uses_default_price_with_no_pairing_data(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.purchase_avoid.return_value = SimpleNamespace(
        success=True, reason=None, balance=0, avoid=SimpleNamespace(games_remaining=10)
    )
    bot.pairings_service.get_head_to_head.return_value = None
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())
    monkeypatch.setattr("commands.shop.get_neon_service", lambda _bot: None)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    player_service.adjust_balance.assert_not_called()
    bot.soft_avoid_service.purchase_avoid.assert_called_once_with(
        guild_id=interaction.guild.id,
        avoider_id=interaction.user.id,
        avoided_id=target.id,
        cost=750,
        games=10,
    )


@pytest.mark.asyncio
async def test_handle_soft_avoid_purchase_failure_does_not_charge(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.purchase_avoid.side_effect = RuntimeError("db locked")
    bot.pairings_service.get_head_to_head.return_value = None
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
    kwargs = safe_followup.call_args.kwargs
    assert kwargs["ephemeral"] is True
    assert "not charged" in kwargs["content"].lower()


@pytest.mark.asyncio
async def test_handle_soft_avoid_minimum_price_purchase_failure_does_not_charge(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.purchase_avoid.side_effect = RuntimeError("db locked")
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
    assert "not charged" in message


@pytest.mark.asyncio
async def test_handle_soft_avoid_charges_minimum_price_after_three_losses(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.purchase_avoid.return_value = SimpleNamespace(
        success=True, reason=None, balance=0, avoid=SimpleNamespace(games_remaining=10)
    )
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
    bot.soft_avoid_service.purchase_avoid.assert_called_once_with(
        guild_id=interaction.guild.id,
        avoider_id=interaction.user.id,
        avoided_id=target.id,
        cost=300,
        games=10,
    )
    safe_defer.assert_awaited_once_with(interaction, ephemeral=True)
    safe_followup.assert_awaited_once()


@pytest.mark.asyncio
async def test_handle_soft_avoid_rejects_active_duplicate_without_charge(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.get_user_avoids.return_value = [
        SimpleNamespace(avoided_discord_id=2002, games_remaining=6)
    ]
    bot.pairings_service.get_head_to_head.return_value = None
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "already active" in message.lower()
    assert "6 games remaining" in message.lower()
    bot.pairings_service.get_head_to_head.assert_not_called()
    player_service.get_balance.assert_not_called()
    player_service.adjust_balance.assert_not_called()
    bot.soft_avoid_service.purchase_avoid.assert_not_called()


@pytest.mark.asyncio
async def test_handle_soft_avoid_handles_concurrent_duplicate_without_charge(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.get_user_avoids.return_value = []
    bot.soft_avoid_service.purchase_avoid.return_value = SimpleNamespace(
        success=False,
        reason="already_active",
        balance=750,
        avoid=SimpleNamespace(games_remaining=6),
    )
    bot.pairings_service.get_head_to_head.return_value = None
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    message = safe_followup.call_args.kwargs["content"].lower()
    assert "already active" in message
    assert "6 games remaining" in message
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_soft_avoid_handles_atomic_insufficient_balance(monkeypatch):
    bot = MagicMock()
    bot.soft_avoid_service.get_user_avoids.return_value = []
    bot.soft_avoid_service.purchase_avoid.return_value = SimpleNamespace(
        success=False,
        reason="insufficient_balance",
        balance=200,
        avoid=None,
    )
    bot.pairings_service.get_head_to_head.return_value = {
        "games_together": 3,
        "wins_together": 0,
    }
    player_service = MagicMock()
    player_service.get_player.side_effect = [object(), object()]
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_soft_avoid(interaction, target=target)

    message = safe_followup.call_args.kwargs["content"].lower()
    assert "need 300" in message
    assert "only have 200" in message
    player_service.adjust_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_package_deal_costs_one_jopacoin_with_no_active_deals(monkeypatch):
    bot = MagicMock()
    bot.package_deal_service.purchase_deal.return_value = SimpleNamespace(
        success=True,
        reason=None,
        balance=0,
        deal=SimpleNamespace(games_remaining=10, games_added=10),
        cost=1,
        active_deal_count=0,
    )
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    player_service.get_balance.return_value = 1
    normal_cost = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    player_service.adjust_balance.assert_not_called()
    bot.package_deal_service.purchase_deal.assert_called_once_with(
        guild_id=interaction.guild.id,
        buyer_id=interaction.user.id,
        partner_id=target.id,
        games=10,
        no_active_cost=1,
        active_cost=normal_cost,
    )
    bot.package_deal_service.get_user_deals.assert_not_called()
    player_service.get_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_package_deal_does_not_charge_when_atomic_purchase_fails(monkeypatch):
    bot = MagicMock()
    bot.package_deal_service.purchase_deal.side_effect = RuntimeError("db locked")
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

    player_service.adjust_balance.assert_not_called()
    kwargs = safe_followup.call_args.kwargs
    assert kwargs["ephemeral"] is True
    assert "not charged" in kwargs["content"].lower()


@pytest.mark.asyncio
async def test_handle_package_deal_existing_active_deal_keeps_normal_price(monkeypatch):
    bot = MagicMock()
    normal_cost = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
    bot.package_deal_service.purchase_deal.return_value = SimpleNamespace(
        success=True,
        reason=None,
        balance=0,
        deal=SimpleNamespace(games_remaining=10, games_added=10),
        cost=normal_cost,
        active_deal_count=1,
    )
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    player_service.get_balance.return_value = normal_cost
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    player_service.adjust_balance.assert_not_called()
    assert bot.package_deal_service.purchase_deal.call_args.kwargs["active_cost"] == normal_cost
    embed = safe_followup.call_args.kwargs["embed"]
    assert f"**Cost:** {normal_cost} " in embed.description
    bot.package_deal_service.get_user_deals.assert_not_called()
    player_service.get_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_package_deal_partial_top_up_reports_games_added(monkeypatch):
    bot = MagicMock()
    normal_cost = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
    bot.package_deal_service.purchase_deal.return_value = SimpleNamespace(
        success=True,
        reason=None,
        balance=200,
        deal=SimpleNamespace(games_remaining=10, games_added=1),
        cost=normal_cost,
        active_deal_count=1,
    )
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    player_service.get_balance.return_value = 1000
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    embed = safe_followup.call_args.kwargs["embed"]
    assert "**Games added:** 1" in embed.description
    assert "**Games remaining:** 10" in embed.description
    assert f"**Cost:** {normal_cost} " in embed.description


@pytest.mark.asyncio
async def test_handle_package_deal_clamps_purchase_duration_to_ten(monkeypatch):
    monkeypatch.setattr("commands.shop.PACKAGE_DEAL_GAMES_DURATION", 25)
    bot = MagicMock()
    bot.package_deal_service.purchase_deal.return_value = SimpleNamespace(
        success=True,
        reason=None,
        balance=0,
        deal=SimpleNamespace(games_remaining=10, games_added=10),
        cost=1,
        active_deal_count=0,
    )
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

    assert bot.package_deal_service.purchase_deal.call_args.kwargs["games"] == 10


@pytest.mark.asyncio
async def test_handle_package_deal_insufficient_balance_uses_atomic_quote(monkeypatch):
    bot = MagicMock()
    normal_cost = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
    bot.package_deal_service.purchase_deal.return_value = SimpleNamespace(
        success=False,
        reason="insufficient_balance",
        balance=200,
        deal=None,
        cost=normal_cost,
        active_deal_count=1,
    )
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    player_service.get_balance.return_value = 1000
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    message = safe_followup.call_args.kwargs["content"].lower()
    assert f"need {normal_cost}" in message
    assert "only have 200" in message
    bot.package_deal_service.get_user_deals.assert_not_called()
    player_service.get_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_package_deal_already_full_does_not_charge(monkeypatch):
    bot = MagicMock()
    bot.package_deal_service.purchase_deal.return_value = SimpleNamespace(
        success=False,
        reason="already_full",
        balance=900,
        deal=SimpleNamespace(games_remaining=10, games_added=0),
        cost=800,
        active_deal_count=1,
    )
    player_service = MagicMock()
    player_service.get_player.side_effect = [
        SimpleNamespace(glicko_rating=1500),
        SimpleNamespace(glicko_rating=1500),
    ]
    player_service.get_balance.return_value = 1000
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    safe_followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", safe_followup)

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=9000)
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    await commands._handle_package_deal(interaction, target=target)

    player_service.adjust_balance.assert_not_called()
    bot.package_deal_service.purchase_deal.assert_called_once()
    message = safe_followup.call_args.kwargs["content"].lower()
    assert "already has 10 games remaining" in message
    assert "not charged" in message


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

    # Conditional debit, not adjust_balance: the read-then-adjust pair let two
    # purchases inside the rate-limit window both pass the check and overdraft.
    player_service.try_spend.assert_called_once()
    args, kwargs = player_service.try_spend.call_args
    assert args[:3] == (interaction.user.id, None, SHOP_NEW_MYSTERY_GIFT_COST)
    assert kwargs["source"] == "shop_mystery_gift"
    player_service.adjust_balance.assert_not_called()
    interaction.response.defer.assert_awaited()
    interaction.followup.send.assert_awaited()
    embed = interaction.followup.send.call_args.kwargs["embed"]
    assert "Mystery Gift" in embed.title


@pytest.mark.asyncio
async def test_handle_mystery_gift_aborts_when_balance_vanishes_before_the_debit(monkeypatch):
    """Funds gone between the balance read and the debit: no gift, no overdraft.

    The shortfall notice must go out via interaction.response BEFORE the public
    defer — the first followup after a public defer ignores ephemeral=True, so
    a followup here would broadcast the user's balance shortfall publicly.
    """
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    # Check passes...
    player_service.get_balance.return_value = SHOP_NEW_MYSTERY_GIFT_COST + 100
    # ...but the funds are gone by the time the debit runs.
    player_service.try_spend.return_value = False

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()

    safe_defer = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_defer", safe_defer)
    followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", followup)

    await commands._handle_mystery_gift(interaction)

    player_service.try_spend.assert_called_once()
    player_service.adjust_balance.assert_not_called()
    # No defer happened, so the direct response is genuinely ephemeral.
    safe_defer.assert_not_awaited()
    followup.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.call_args
    assert "no longer have" in args[0]
    assert kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_handle_mystery_gift_shortfall_tolerates_lapsed_interaction(monkeypatch):
    """The debit can outlast the 3s ack window under DB contention; a lapsed
    token must not crash the handler — the refused spend needs no rollback."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_balance.return_value = SHOP_NEW_MYSTERY_GIFT_COST + 100
    player_service.try_spend.return_value = False

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()
    interaction.response.send_message.side_effect = discord.NotFound(
        SimpleNamespace(status=404, reason="Not Found"),
        {"message": "Unknown interaction"},
    )
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock())
    followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", followup)

    await commands._handle_mystery_gift(interaction)

    player_service.adjust_balance.assert_not_called()
    followup.assert_not_awaited()


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


@pytest.mark.asyncio
async def test_shop_rejects_retired_witchs_curse_value(monkeypatch):
    player_service = MagicMock()
    commands = ShopCommands(MagicMock(), player_service)
    interaction = _make_interaction(guild_id=9000)

    monkeypatch.setattr(
        "commands.shop.GLOBAL_RATE_LIMITER.check",
        lambda **_kwargs: SimpleNamespace(allowed=True, retry_after_seconds=0),
    )

    await commands.shop.callback(commands, interaction, "witchs_curse", target=None)

    interaction.response.send_message.assert_awaited_once()
    message = interaction.response.send_message.call_args.args[0]
    assert "no longer available" in message
    player_service.adjust_balance.assert_not_called()


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

    # Conditional debit, not adjust_balance: the read-then-adjust pair let two
    # purchases inside the rate-limit window both pass the check and overdraft.
    player_service.try_spend.assert_called_once()
    args, kwargs = player_service.try_spend.call_args
    assert args[:3] == (1001, None, SHOP_RECALIBRATE_COST)
    assert kwargs["source"] == "shop_recalibrate"
    player_service.adjust_balance.assert_not_called()
    recal_service.recalibrate.assert_called_once_with(1001, None)


@pytest.mark.asyncio
async def test_handle_recalibrate_aborts_when_balance_vanishes_before_the_debit(monkeypatch):
    """Funds gone between the balance read and the debit: no overdraft, no reset.

    The shortfall notice must go out via interaction.response BEFORE the public
    defer — the first followup after a public defer ignores ephemeral=True, so
    a followup here would broadcast the user's balance shortfall publicly.
    """
    bot = MagicMock()
    player_service = MagicMock()
    # Check passes...
    player_service.get_balance.return_value = 2000
    # ...but the funds are gone by the time the debit runs.
    player_service.try_spend.return_value = False
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

    safe_defer = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_defer", safe_defer)
    followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", followup)

    await cmds._handle_recalibrate(interaction)

    player_service.try_spend.assert_called_once()
    player_service.adjust_balance.assert_not_called()
    recal_service.recalibrate.assert_not_called()
    # No defer happened, so the direct response is genuinely ephemeral.
    safe_defer.assert_not_awaited()
    followup.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.call_args
    assert "no longer have" in args[0]
    assert kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_handle_recalibrate_shortfall_tolerates_lapsed_interaction(monkeypatch):
    """The debit can outlast the 3s ack window under DB contention; a lapsed
    token must not crash the handler — the refused spend needs no rollback."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_balance.return_value = 2000
    player_service.try_spend.return_value = False
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
    interaction.response.send_message.side_effect = discord.NotFound(
        SimpleNamespace(status=404, reason="Not Found"),
        {"message": "Unknown interaction"},
    )
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock())

    await cmds._handle_recalibrate(interaction)

    player_service.adjust_balance.assert_not_called()
    recal_service.recalibrate.assert_not_called()


@pytest.mark.asyncio
async def test_handle_recalibrate_failure_refunds_with_ledger_context(monkeypatch):
    """A failed recalibration refunds through adjust_balance WITH a source."""
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
    recal_service.recalibrate.return_value = {"success": False}

    cmds = ShopCommands(bot, player_service, recalibration_service=recal_service)
    interaction = _make_interaction()

    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock())
    monkeypatch.setattr("commands.shop.safe_followup", AsyncMock())

    await cmds._handle_recalibrate(interaction)

    player_service.adjust_balance.assert_called_once()
    args, kwargs = player_service.adjust_balance.call_args
    assert args[:3] == (1001, None, SHOP_RECALIBRATE_COST)
    assert kwargs["source"] == "shop_recalibrate"


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
async def test_item_autocomplete_excludes_curse_and_includes_jopa_coin():
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
    assert "witchs_curse" not in values


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
    from utils.economy_scaling import scale_minigame_jc_delta
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
    _allow_manashop_purchase(bot, 1000)

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=user_id)
    player_service.get_balance.return_value = 1000

    shop = ShopCommands(bot, player_service, gambling_stats_service=gambling)
    interaction = _make_interaction(user_id=user_id, guild_id=TEST_GUILD_ID)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="regrowth"), target=None,
    )

    # 35% of the 1000 loss is capped at 120, then passes through the neutral
    # central scaling lever before the generated recovery is credited.
    expected_recovery = scale_minigame_jc_delta(120)
    assert expected_recovery == 120
    recovery_calls = [
        c for c in player_service.adjust_balance.call_args_list
        if c.args == (user_id, TEST_GUILD_ID, expected_recovery)
    ]
    assert recovery_calls, (
        "Regrowth should credit the centrally scaled form of its capped recovery; "
        f"adjust_balance calls were {player_service.adjust_balance.call_args_list}"
    )


def test_recent_loss_aggregate_combines_only_recent_losing_bets_and_wheel(
    repo_db_path,
):
    import sqlite3

    from repositories.bet_repository import BetRepository
    from repositories.player_repository import PlayerRepository
    from tests.conftest import TEST_GUILD_ID

    discord_id = 4242
    cutoff = 1_000_000
    with sqlite3.connect(repo_db_path) as conn:
        conn.executemany(
            """
            INSERT INTO matches (
                match_id, team1_players, team2_players, winning_team, guild_id
            ) VALUES (?, '[]', '[]', ?, ?)
            """,
            [
                (101, 2, TEST_GUILD_ID),
                (102, 1, TEST_GUILD_ID),
                (103, 2, TEST_GUILD_ID),
                (104, 2, TEST_GUILD_ID + 1),
            ],
        )
        conn.executemany(
            """
            INSERT INTO bets (
                guild_id, match_id, discord_id, team_bet_on,
                amount, leverage, bet_time
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            [
                (TEST_GUILD_ID, 101, discord_id, "radiant", 10, 5, cutoff + 1),
                (TEST_GUILD_ID, 102, discord_id, "radiant", 100, 5, cutoff + 2),
                (TEST_GUILD_ID, 103, discord_id, "radiant", 30, 1, cutoff + 3),
                (TEST_GUILD_ID, 103, discord_id, "radiant", 999, 1, cutoff - 1),
                (TEST_GUILD_ID + 1, 104, discord_id, "radiant", 999, 1, cutoff + 4),
            ],
        )

    player_repo = PlayerRepository(repo_db_path)
    player_repo.log_wheel_spin(discord_id, TEST_GUILD_ID, -70, cutoff + 5)
    player_repo.log_wheel_spin(discord_id, TEST_GUILD_ID, 500, cutoff + 6)
    player_repo.log_wheel_spin(discord_id, TEST_GUILD_ID, -1000, cutoff - 1)
    player_repo.log_wheel_spin(discord_id, TEST_GUILD_ID + 1, -900, cutoff + 7)

    assert BetRepository(repo_db_path).get_recent_loss_aggregates(
        discord_id, TEST_GUILD_ID, cutoff
    ) == {"largest": 70, "total": 150}


@pytest.mark.asyncio
async def test_manashop_failure_notices_precede_the_public_defer(monkeypatch):
    """Manashop rejections must respond ephemerally BEFORE any public defer -
    the first followup after a public defer ignores ephemeral=True and would
    broadcast the private notice."""
    from tests.conftest import TEST_GUILD_ID

    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Red")
    bot.mana_service.is_mana_consumed.return_value = False
    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=1001)

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(guild_id=TEST_GUILD_ID)
    safe_defer = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_defer", safe_defer)

    # Regrowth needs Green mana; the buyer holds Red.
    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="regrowth"), target=None,
    )

    safe_defer.assert_not_awaited()
    interaction.followup.send.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.call_args
    assert "requires" in args[0]
    assert kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_manashop_atomic_shortfall_rejects_before_effects(monkeypatch):
    """A balance lost after the friendly pre-check must not consume the item."""
    user_id = 1001
    guild_id = 9000
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Red")
    bot.mana_service.is_mana_consumed.return_value = False
    bot.mana_repo.try_purchase_item_atomic.return_value = {
        "success": False,
        "reason": "insufficient_balance",
        "balance": 10,
    }
    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=user_id)
    player_service.get_balance.return_value = 500
    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=user_id, guild_id=guild_id)
    safe_defer = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_defer", safe_defer)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value="pyroclasm"), target=None,
    )

    bot.mana_repo.try_purchase_item_atomic.assert_called_once_with(
        user_id,
        guild_id,
        "pyroclasm",
        ANY,
        cost=25,
        tap_mana=False,
    )
    safe_defer.assert_not_awaited()
    player_service.adjust_balance.assert_not_called()
    player_service.get_leaderboard.assert_not_called()
    interaction.response.send_message.assert_awaited_once_with(
        f"You need 25 {JOPACOIN_EMOTE} for Pyroclasm. You have 10.",
        ephemeral=True,
    )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("item", "color", "loss_method"),
    [
        ("mana_shield", "Blue", "_compute_largest_recent_loss"),
        ("regrowth", "Green", "_compute_cumulative_recent_losses"),
    ],
)
async def test_manashop_generated_recovery_scales_before_daily_event(
    monkeypatch, item, color, loss_method
):
    from tests.conftest import TEST_GUILD_ID
    from utils.economy_scaling import scale_minigame_jc_delta

    user_id = 5353
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color=color)
    bot.mana_service.is_mana_consumed.return_value = False
    _allow_manashop_purchase(bot, 1_000)
    economy_event_service = MagicMock()
    economy_event_service.adjust_reward.return_value = 41
    bot.economy_event_service = economy_event_service

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=user_id)
    player_service.get_balance.return_value = 1_000
    shop = ShopCommands(bot, player_service)
    monkeypatch.setattr(shop, loss_method, lambda *_args: 1_000)
    interaction = _make_interaction(user_id=user_id, guild_id=TEST_GUILD_ID)

    await shop.manashop.callback(
        shop, interaction, SimpleNamespace(value=item), target=None
    )

    scaled_cap = scale_minigame_jc_delta(120)
    assert scaled_cap == 120  # Neutral baseline; ordering remains observable.
    economy_event_service.adjust_reward.assert_called_once_with(
        TEST_GUILD_ID, scaled_cap
    )
    reward_calls = [
        call
        for call in player_service.adjust_balance.call_args_list
        if call.args == (user_id, TEST_GUILD_ID, 41)
    ]
    assert reward_calls
    assert reward_calls[0].kwargs["source"] == "mana_reward"
    assert reward_calls[0].kwargs["metadata"] == {
        "base_reward": 120,
        "adjusted_reward": 41,
    }


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
    _allow_manashop_purchase(bot, 100)
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

    player_service.adjust_balance.assert_not_called()
    bot.buff_service.grant_reprieve.assert_called_once_with(user_id, guild_id)
    bot.protection_service.reconcile_purchased_pool.assert_called_once_with(
        user_id, guild_id, 77, 24 * 3600,
    )
    bot.mana_repo.try_purchase_item_atomic.assert_called_once_with(
        user_id, guild_id, "reprieve", ANY, cost=15, tap_mana=False,
    )
    message = interaction.followup.send.call_args.kwargs["content"]
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
    _allow_manashop_purchase(bot, 500)
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
        # Neutral 1.0 central scale leaves only the intentional 10%
        # deflationary-pressure bump: 20 -> 22 attempted loss.
        assert call.args[:3] == (target.discord_id, guild_id, 22)
        assert call.kwargs["kind"] == "pyroclasm"
        assert call.kwargs["destination"] == "burn"
        assert call.kwargs["clamp_to_balance"] is True

    balance_calls = [call.args for call in player_service.adjust_balance.call_args_list]
    assert balance_calls == [
        (buyer_id, guild_id, 13),
    ]
    bot.mana_repo.try_purchase_item_atomic.assert_called_once_with(
        buyer_id, guild_id, "pyroclasm", ANY, cost=25, tap_mana=False,
    )
    message = interaction.followup.send.call_args.kwargs["content"]
    assert "**27" in message
    assert "You claim **13" in message
    assert "Shields absorbed **27" in message


@pytest.mark.asyncio
async def test_manashop_pyroclasm_batches_real_protection_gateway(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))
    monkeypatch.setattr("commands.shop.random.sample", lambda population, count: population[:count])
    monkeypatch.setattr("commands.shop.random.randint", lambda low, high: 20)

    buyer_id = 4242
    guild_id = 9001
    targets = [
        SimpleNamespace(
            discord_id=5100 + index,
            name=f"Target {index}",
            jopacoin_balance=100,
        )
        for index in range(3)
    ]
    outcomes = [
        SimpleNamespace(applied=22, absorbed=0),
        SimpleNamespace(applied=11, absorbed=11),
        SimpleNamespace(applied=0, absorbed=22),
    ]
    protection_service = ProtectionService(MagicMock())
    protection_service.apply_hostile_losses = MagicMock(return_value=outcomes)
    protection_service.apply_hostile_loss = MagicMock()

    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Red")
    bot.mana_service.is_mana_consumed.return_value = False
    _allow_manashop_purchase(bot, 500)
    bot.protection_service = protection_service

    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(discord_id=buyer_id)
    player_service.get_balance.return_value = 500
    player_service.get_leaderboard.return_value = targets

    shop = ShopCommands(bot, player_service)
    interaction = _make_interaction(user_id=buyer_id, guild_id=guild_id)

    await shop.manashop.callback(shop, interaction, SimpleNamespace(value="pyroclasm"), target=None)

    protection_service.apply_hostile_losses.assert_called_once()
    losses = protection_service.apply_hostile_losses.call_args.args[0]
    assert [loss["victim_id"] for loss in losses] == [5100, 5101, 5102]
    assert [loss["amount"] for loss in losses] == [22, 22, 22]
    assert all(loss["kind"] == "pyroclasm" for loss in losses)
    assert all(loss["destination"] == "burn" for loss in losses)
    assert len({loss["event_key"].rsplit(":", 1)[0] for loss in losses}) == 1
    protection_service.apply_hostile_loss.assert_not_called()


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
    _allow_manashop_purchase(bot, 500)
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

    player_service.adjust_balance.assert_not_called()
    bot.mana_repo.try_purchase_item_atomic.assert_called_once_with(
        buyer_id, guild_id, "soul_harvest", ANY, cost=25, tap_mana=False,
    )
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
    message = interaction.followup.send.call_args.kwargs["content"]
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
    _allow_manashop_purchase(bot, 500)

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
    assert calls[-1] == (buyer_id, guild_id, 6)
    assert not any(c[0] == low_player.discord_id for c in calls)
    assert not any(c[0] == zero_player.discord_id for c in calls)
    assert not any(c[0] == bankrupt_player.discord_id for c in calls)

    victim_debits = [c for c in calls if c[0] != buyer_id and c[2] < 0]
    assert len(victim_debits) == len(positive_players)
    assert sum(-delta for _, _, delta in victim_debits) == 6
    assert all(delta == -2 for _, _, delta in victim_debits)

    message = interaction.followup.send.call_args.kwargs["content"]
    assert "drains the living" in message.lower()
    assert "balance: 481" in message
    bot.mana_repo.try_purchase_item_atomic.assert_called_once_with(
        buyer_id, guild_id, "soul_harvest", ANY, cost=25, tap_mana=False,
    )


@pytest.mark.asyncio
async def test_manashop_soul_harvest_refunds_and_releases_daily_slot_without_targets(monkeypatch):
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock(return_value=True))

    buyer_id = 4242
    guild_id = 9001
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Black")
    bot.mana_service.is_mana_consumed.return_value = False
    _allow_manashop_purchase(bot, 500)

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

    player_service.adjust_balance.assert_not_called()
    message = interaction.followup.send.call_args.kwargs["content"]
    assert "No living souls" in message
    bot.mana_repo.refund_item_purchase_atomic.assert_called_once_with(
        "purchase-1",
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
    _allow_manashop_purchase(bot, 500)
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
    # Neutral 1.0 central scale plus the intentional deflationary bump: 10 -> 11.
    assert call.args[:3] == (victim.discord_id, guild_id, 11)
    assert call.kwargs["kind"] == "wildfire"
    assert call.kwargs["destination"] == "burn"
    # 45% of the 4 JC that landed floors to 1; the absorbed 5 pays nothing.
    assert [entry.args for entry in player_service.adjust_balance.call_args_list] == [
        (buyer_id, guild_id, 1),
    ]
    bot.mana_repo.try_purchase_item_atomic.assert_called_once_with(
        buyer_id, guild_id, "wildfire", ANY, cost=150, tap_mana=True,
    )
    message = interaction.followup.send.call_args.kwargs["content"]
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
    _allow_manashop_purchase(bot, 500)
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

    player_service.adjust_balance.assert_not_called()
    bot.mana_repo.try_purchase_item_atomic.assert_called_once_with(
        buyer_id, guild_id, "sanctuary", ANY, cost=90, tap_mana=True,
    )
    bot.buff_service.grant_sanctuary.assert_called_once_with(
        buyer_id, guild_id, ally_id,
    )
    message = interaction.followup.send.call_args.kwargs["content"]
    assert "150" in message
    assert "match" not in message.lower()


@pytest.mark.asyncio
async def test_dark_bargain_due_amount_matches_loan_principal():
    bot = MagicMock()
    bot.mana_effects_service.get_effects.return_value = SimpleNamespace(color="Black")
    bot.mana_service.is_mana_consumed.return_value = False
    _allow_manashop_purchase(bot, 1000)
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
    assert "700 due in 7 days" in interaction.followup.send.call_args.kwargs["content"]


@pytest.mark.asyncio
async def test_double_or_nothing_atomic_settlement_blocks_concurrent_second_spin():
    """The cooldown, money movement, and flip log settle in one repository call.

    Two rapid presses both pass the stale read-based check; only the first may
    settle. The losing claim must not move money or store a second outcome.
    """
    from unittest.mock import patch

    from commands.shop import SHOP_DOUBLE_OR_NOTHING_COST as COST

    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = object()
    player_service.get_last_double_or_nothing.return_value = None  # stale read: no cooldown
    player_service.get_balance.return_value = 200
    player_service.settle_double_or_nothing.side_effect = [
        {
            "success": True,
            "reason": None,
            "balance_before": 200,
            "balance_at_risk": 200 - COST,
            "balance_after": 2 * (200 - COST),
            "won": True,
            "cooldown_ends_at": 1_700_000_000 + 86_400,
        },
        {
            "success": False,
            "reason": "on_cooldown",
            "balance": 2 * (200 - COST),
            "cooldown_ends_at": 1_700_000_000 + 86_400,
        },
    ]

    commands = ShopCommands(bot, player_service)

    # First press: claim succeeds, WIN doubles the at-risk balance.
    first = _make_interaction(guild_id=9001)
    first.user.avatar = None
    with patch("commands.shop.random.random", return_value=0.25):
        await commands._handle_double_or_nothing(first)

    first_call = player_service.settle_double_or_nothing.call_args_list[0]
    assert first_call.args == (1001, 9001)
    assert first_call.kwargs["cost"] == COST
    assert first_call.kwargs["won"] is True
    assert first_call.kwargs["cooldown_seconds"] > 0
    assert first_call.kwargs["bypass_cooldown"] is False

    # Second press: atomic claim rejects; no money may move again.
    second = _make_interaction(guild_id=9001)
    with patch("commands.shop.random.random", return_value=0.25):
        await commands._handle_double_or_nothing(second)

    second.response.send_message.assert_awaited_once()
    assert second.response.send_message.call_args.kwargs.get("ephemeral") is True
    assert player_service.settle_double_or_nothing.call_count == 2
    player_service.adjust_balance.assert_not_called()
    player_service.log_double_or_nothing.assert_not_called()
    player_service.set_balance.assert_not_called()


@pytest.mark.asyncio
async def test_handle_announce_aborts_when_balance_vanishes_before_the_debit(monkeypatch):
    """A purchase whose funds disappear after the check must not deliver the item.

    The balance read is only a friendly fast path — the shop rate limit allows
    3 actions per 60s, so two purchases could both pass it and overdraft. The
    conditional debit is the authoritative gate.

    The shortfall notice must go out via interaction.response BEFORE the public
    defer — the first followup after a public defer ignores ephemeral=True, so
    a followup here would broadcast the user's balance shortfall publicly.
    """
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(
        wins=10, losses=5, jopacoin_balance=500, glicko_rating=1500.0
    )
    # Check passes...
    player_service.get_balance.return_value = SHOP_ANNOUNCE_TARGET_COST + 10
    # ...but the funds are gone by the time the debit runs.
    player_service.try_spend.return_value = False

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()
    target = SimpleNamespace(id=2002, mention="<@2002>", display_name="TargetPlayer")

    monkeypatch.setattr("commands.shop.random.choice", lambda _items: "Test message")
    monkeypatch.setattr("commands.shop.get_hero_color", lambda _hero_id: None)
    monkeypatch.setattr("commands.shop.get_hero_image_url", lambda _hero_id: None)
    safe_defer = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_defer", safe_defer)
    followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", followup)

    await commands._handle_announce(interaction, target=target)

    player_service.try_spend.assert_called_once()
    player_service.adjust_balance.assert_not_called()
    # No defer happened, so the direct response is genuinely ephemeral.
    safe_defer.assert_not_awaited()
    followup.assert_not_awaited()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.call_args
    assert "no longer have" in args[0]
    assert kwargs["ephemeral"] is True


@pytest.mark.asyncio
async def test_handle_announce_shortfall_tolerates_lapsed_interaction(monkeypatch):
    """The debit can outlast the 3s ack window under DB contention; a lapsed
    token must not crash the handler — the refused spend needs no rollback."""
    bot = MagicMock()
    player_service = MagicMock()
    player_service.get_player.return_value = SimpleNamespace(
        wins=10, losses=5, jopacoin_balance=500, glicko_rating=1500.0
    )
    player_service.get_balance.return_value = SHOP_ANNOUNCE_COST + 10
    player_service.try_spend.return_value = False

    commands = ShopCommands(bot, player_service)
    interaction = _make_interaction()
    interaction.response.send_message.side_effect = discord.NotFound(
        SimpleNamespace(status=404, reason="Not Found"),
        {"message": "Unknown interaction"},
    )
    monkeypatch.setattr("commands.shop.safe_defer", AsyncMock())
    followup = AsyncMock()
    monkeypatch.setattr("commands.shop.safe_followup", followup)

    await commands._handle_announce(interaction, target=None)

    player_service.adjust_balance.assert_not_called()
    followup.assert_not_awaited()
