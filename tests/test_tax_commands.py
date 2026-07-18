from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

import commands.tax as tax_commands
from utils.embed_safety import EMBED_LIMITS, validate_embed


def _empty_prediction_exposure():
    return {
        "summary": {
            "cost_basis": 0,
            "expected_payout": 0,
            "ev": 0,
            "max_payout": 0,
        },
        "positions": [],
    }


def _ledger_row(
    idx: int,
    *,
    reason: str | None = None,
    source: str = "balance_update",
) -> dict:
    return {
        "ledger_id": idx,
        "guild_id": 123,
        "account_type": "player",
        "account_id": 10_000 + idx,
        "delta": idx + 1,
        "balance_before": idx * 10,
        "balance_after": idx * 10 + idx + 1,
        "source": source,
        "actor_id": 42,
        "related_type": "wheel_spin" if source == "gamba" else None,
        "related_id": "LIGHTNING_BOLT" if source == "gamba" else None,
        "reason": reason,
        "metadata": None,
        "created_at": 1_700_000_000 + idx,
    }


def _player_snapshot(recent_ledger: list[dict]) -> dict:
    return {
        "balance": 100,
        "visible_debt": 0,
        "loan_principal": 0,
        "loan_fee": 0,
        "loan_total": 0,
        "total_loans_taken": 0,
        "bankruptcy_count": 0,
        "penalty_games_remaining": 0,
        "dark_bargain_count": 0,
        "dark_bargain_due": 0,
        "effective_obligations": 0,
        "prediction_exposure": _empty_prediction_exposure(),
        "recent_ledger": recent_ledger,
    }


class _FakeResponse:
    def __init__(self):
        self.messages: list[dict] = []
        self._done = False
        self.deferred_ephemeral: bool | None = None

    async def send_message(self, content=None, ephemeral=None, embed=None, **kwargs):
        self._done = True
        self.messages.append(
            {"content": content, "ephemeral": ephemeral, "embed": embed, **kwargs}
        )

    async def defer(self, ephemeral=False):
        self._done = True
        self.deferred_ephemeral = ephemeral

    async def edit_message(self, **kwargs):
        self._done = True
        self.messages.append({"edit": True, **kwargs})

    def is_done(self):
        return self._done


class _FakeFollowup:
    def __init__(self):
        self.messages: list[dict] = []

    async def send(self, content=None, embed=None, ephemeral=None, **kwargs):
        self.messages.append(
            {"content": content, "embed": embed, "ephemeral": ephemeral, **kwargs}
        )


class _FakeInteraction:
    def __init__(self, *, guild_id: int = 123, user_id: int = 42):
        self.guild = SimpleNamespace(id=guild_id)
        self.user = SimpleNamespace(id=user_id)
        self.response = _FakeResponse()
        self.followup = _FakeFollowup()


def _embed_text(embed) -> str:
    parts = [embed.title or "", embed.description or ""]
    parts.extend(f"{field.name}\n{field.value}" for field in embed.fields)
    footer_text = getattr(embed.footer, "text", None)
    if footer_text:
        parts.append(footer_text)
    return "\n".join(parts)


def _global_silence_status() -> dict:
    effects = {
        "reward_multiplier": 0.64,
        "gamba_win_multiplier": 0.91,
        "bet_payout_multiplier": 0.97,
        "prediction_depth_multiplier": 0.16,
        "prediction_spread_ticks_delta": 6,
    }
    return {
        "policy": {
            "mode": "recovery",
            "target_annual_rate": -0.035,
            "inflation_ceiling": 0.02,
        },
        "balance_sheet": {
            "monetary_stock": 124_464,
            "player_wallets": 53_900,
            "average_wallet": 449.17,
            "reserve_available": 42_795,
            "reserve_locked": 0,
            "prediction_open_cash": 26_155,
            "wager_escrow": 1_614,
        },
        "latest_snapshot": {},
        "event": {
            "name": "Global Silence",
            "severity": 3,
            "direction": "deflationary",
            "announcement": (
                "Bonus rewards vanish and market makers fall quiet.\n"
                "Generated rewards (dig, trivia, and mana): **-36%**."
            ),
            "effects": effects,
            "forecast_flow_jc": 2_237,
            "target_effect_jc": -2_249,
            "expected_effect_jc": -2_203,
            "direct_effect_jc": 0,
            "ends_at": 1_752_943_600,
        },
        "effects": effects,
    }


def _assert_ledger_page_metadata(
    embed,
    *,
    page: int,
    total_entries: int,
    limit: int,
):
    text = _embed_text(embed)

    assert f"Page {page}" in text
    assert "page" in text.lower()
    if total_entries <= 0:
        assert "0 entries" in text
        return

    first_entry = (page - 1) * limit + 1
    last_entry = min(total_entries, page * limit)
    assert f"Entries {first_entry}-{last_entry} of {total_entries:,}" in text


def test_tax_group_contains_audit_and_enforcement_commands():
    names = {cmd.name for cmd in tax_commands.TaxCommands.tax.walk_commands()}

    assert names == {
        "audit",
        "event",
        "player",
        "ledger",
        "fine",
        "resetcooldown",
        "bankruptcy",
        "policy",
    }


def test_public_event_embed_is_high_level_theatrical_and_explains_indirect_effect():
    embed = tax_commands._build_public_event_embed(
        _global_silence_status(),
        icon_url="https://cdn.example/global_silence.png",
    )
    text = _embed_text(embed)

    assert embed.title == "🌑 Global Silence — Level III"
    assert embed.thumbnail.url == "https://cdn.example/global_silence.png"
    assert "36% lower" in text
    assert "9% lower" in text
    assert "3% lower" in text
    assert "84% thinner" in text
    assert "6 ticks wider" in text
    assert "No JC moved when this spell activated" in text
    assert "Forecast unmanaged flow" not in text
    assert "Target event effect" not in text
    assert "124,464" not in text
    assert validate_embed(embed) == []


def test_public_event_embed_names_immediate_reserve_and_wallet_actions():
    status = _global_silence_status()
    status["event"]["effects"] = {
        "reserve_burn_jc": 300,
        "wallet_burn_jc": 75,
    }

    text = _embed_text(tax_commands._build_public_event_embed(status))

    assert "300 JC" in text
    assert "burned from the Jopa Reserve" in text
    assert "75 JC" in text
    assert "burned from positive wallets" in text
    assert "No JC moved" not in text


def test_public_event_embed_handles_no_active_event():
    embed = tax_commands._build_public_event_embed({"event": None})

    assert embed.title == "🌤️ The Economy Is Between Spells"
    assert "10 AM Pacific" in embed.description
    assert validate_embed(embed) == []


def test_private_policy_embed_labels_zero_direct_effect_as_indirect():
    text = _embed_text(tax_commands._build_policy_embed(_global_silence_status()))

    assert "Projected daily impact" in text
    assert "Immediate supply change" in text
    assert "None" in text
    assert "works through adjusted outcomes" in text


@pytest.mark.asyncio
async def test_tax_event_is_public_and_does_not_require_tax_man(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        await interaction.response.defer(ephemeral=ephemeral)
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    def _unexpected_permission_check(_interaction):
        raise AssertionError("public event command must not check Tax Man permission")

    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)
    monkeypatch.setattr(
        tax_commands,
        "has_tax_man_permission",
        _unexpected_permission_check,
    )
    monkeypatch.setattr(
        tax_commands.trivia_data,
        "get_ability_icon_url_by_name",
        MagicMock(return_value="https://cdn.example/global_silence.png"),
    )
    economy_service = SimpleNamespace(
        get_policy_status=MagicMock(return_value=_global_silence_status())
    )
    cog = tax_commands.TaxCommands(
        bot=SimpleNamespace(economy_event_service=economy_service),
        tax_service=SimpleNamespace(),
    )
    interaction = _FakeInteraction(guild_id=123)

    await cog.event.callback(cog, interaction)

    economy_service.get_policy_status.assert_called_once_with(123)
    assert interaction.response.deferred_ephemeral is False
    message = interaction.followup.messages[-1]
    assert message["ephemeral"] is False
    assert message["embed"].title == "🌑 Global Silence — Level III"


@pytest.mark.asyncio
async def test_tax_event_survives_spell_icon_lookup_failure(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        await interaction.response.defer(ephemeral=ephemeral)
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)
    monkeypatch.setattr(
        tax_commands.trivia_data,
        "get_ability_icon_url_by_name",
        MagicMock(side_effect=RuntimeError("dotabase unavailable")),
    )
    economy_service = SimpleNamespace(
        get_policy_status=MagicMock(return_value=_global_silence_status())
    )
    cog = tax_commands.TaxCommands(
        bot=SimpleNamespace(economy_event_service=economy_service),
        tax_service=SimpleNamespace(),
    )
    interaction = _FakeInteraction(guild_id=123)

    await cog.event.callback(cog, interaction)

    embed = interaction.followup.messages[-1]["embed"]
    assert embed.title == "🌑 Global Silence — Level III"
    assert not embed.thumbnail


@pytest.mark.asyncio
async def test_tax_policy_remains_private_and_tax_man_only(monkeypatch):
    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: False)
    service = SimpleNamespace(get_policy_status=AsyncMock())
    cog = tax_commands.TaxCommands(
        bot=SimpleNamespace(economy_event_service=service),
        tax_service=SimpleNamespace(),
    )
    interaction = _FakeInteraction(guild_id=123)

    await cog.policy.callback(cog, interaction)

    service.get_policy_status.assert_not_awaited()
    message = interaction.response.messages[-1]
    assert message["ephemeral"] is True
    assert message["content"] == "Only Tax Men can use this command."


def test_tax_player_recent_ledger_splits_long_field():
    rows = [
        _ledger_row(
            idx,
            reason="gamba lightning bolt tax " + ("x" * 120),
            source="gamba",
        )
        for idx in range(12)
    ]
    user = SimpleNamespace(name="taxpayer", display_name="Taxpayer")

    embed = tax_commands._build_player_embed(
        user,
        _player_snapshot(rows),
    )

    assert validate_embed(embed) == []
    recent_fields = [
        field for field in embed.fields if field.name in {"Recent Ledger", "\u200b"}
    ]
    assert len(recent_fields) > 1
    assert all(len(field.value) <= EMBED_LIMITS["field_value"] for field in recent_fields)


def test_ledger_rows_prefer_descriptive_reason():
    text = tax_commands._format_ledger_rows(
        [
            _ledger_row(
                1,
                reason="gamba lightning bolt tax",
                source="gamba",
            )
        ]
    )

    assert "gamba lightning bolt tax" in text
    assert "via `balance_update`" not in text


def test_ledger_embed_truncates_description_to_discord_limit():
    rows = [
        _ledger_row(
            idx,
            reason="dig event credit " + ("y" * 240),
            source="dig",
        )
        for idx in range(25)
    ]

    embed = tax_commands._build_ledger_embed(rows, user=None)

    assert len(embed.description) <= EMBED_LIMITS["description"]
    assert validate_embed(embed) == []


def test_ledger_pagination_helpers_clamp_page_and_calculate_offset():
    assert tax_commands._ledger_total_pages(total_entries=0, limit=10) == 1
    assert tax_commands._ledger_total_pages(total_entries=26, limit=25) == 2
    assert tax_commands._clamp_ledger_page(page=99, limit=10, total_entries=21) == 3
    assert tax_commands._ledger_offset(page=3, limit=10) == 20


def test_ledger_embed_includes_page_footer():
    rows = [_ledger_row(idx, reason=f"entry {idx}") for idx in range(10)]

    embed = tax_commands._build_ledger_embed(
        rows,
        user=None,
        page=2,
        total_entries=27,
        limit=10,
    )

    assert embed.footer.text == "Tax Man ledger | Page 2/3 | Entries 11-20 of 27"
    assert validate_embed(embed) == []


def test_ledger_empty_embed_reports_zero_entry_page():
    embed = tax_commands._build_ledger_embed(
        [],
        user=None,
        page=9,
        total_entries=0,
        limit=10,
    )

    assert embed.description == "No ledger entries yet."
    assert embed.footer.text == "Tax Man ledger | Page 1/1 | 0 entries"


def test_ledger_embed_includes_page_and_limit_metadata():
    rows = [_ledger_row(idx) for idx in (30, 31)]

    embed = tax_commands._build_ledger_embed(
        rows,
        user=None,
        page=4,
        total_entries=100,
        limit=13,
    )

    _assert_ledger_page_metadata(embed, page=4, total_entries=100, limit=13)
    assert validate_embed(embed) == []


def test_ledger_embed_empty_high_page_clamps_to_available_page():
    embed = tax_commands._build_ledger_embed(
        [],
        user=None,
        page=99,
        total_entries=0,
        limit=10,
    )

    text = _embed_text(embed)
    assert "No ledger entries yet." in text
    _assert_ledger_page_metadata(embed, page=1, total_entries=0, limit=10)
    assert validate_embed(embed) == []


def test_ledger_offset_uses_one_indexed_pages():
    assert tax_commands._ledger_offset(1, 10) == 0
    assert tax_commands._ledger_offset(3, 7) == 14
    assert tax_commands._ledger_offset(0, 10) == 0


@pytest.mark.asyncio
async def test_tax_ledger_page_calculates_offset_for_service(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    tax_service = SimpleNamespace(
        count_ledger_entries=MagicMock(return_value=100),
        get_recent_ledger=MagicMock(return_value=[_ledger_row(14)])
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123)

    await cog.ledger.callback(
        cog,
        interaction,
        user=None,
        page=3,
        limit=7,
    )

    tax_service.count_ledger_entries.assert_called_once_with(123, user_id=None)
    tax_service.get_recent_ledger.assert_called_once_with(
        123,
        limit=7,
        offset=14,
        user_id=None,
    )
    assert interaction.followup.messages[-1]["ephemeral"] is True


@pytest.mark.asyncio
async def test_tax_player_uses_paginated_recent_ledger_slice(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    limit = tax_commands.PLAYER_LEDGER_DEFAULT_LIMIT
    tax_service = SimpleNamespace(
        get_player_snapshot=MagicMock(
            return_value=_player_snapshot([_ledger_row(idx) for idx in range(limit)])
        ),
        count_ledger_entries=MagicMock(return_value=17),
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await cog.player.callback(cog, interaction, user=target)

    tax_service.get_player_snapshot.assert_called_once_with(
        99,
        123,
        ledger_limit=limit,
        ledger_offset=0,
    )
    tax_service.count_ledger_entries.assert_called_once_with(123, user_id=99)

    message = interaction.followup.messages[-1]
    assert message["ephemeral"] is True
    assert isinstance(message["view"], tax_commands.TaxPlayerLedgerView)
    assert message["view"].previous_page.disabled is True
    assert message["view"].next_page.disabled is False
    _assert_ledger_page_metadata(
        message["embed"],
        page=1,
        total_entries=17,
        limit=limit,
    )
    assert validate_embed(message["embed"]) == []


@pytest.mark.asyncio
async def test_tax_player_ledger_view_loads_next_page_with_offset():
    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    limit = tax_commands.PLAYER_LEDGER_DEFAULT_LIMIT
    tax_service = SimpleNamespace(
        count_ledger_entries=MagicMock(return_value=21),
        get_player_snapshot=MagicMock(
            return_value=_player_snapshot([_ledger_row(idx) for idx in range(16, 21)])
        ),
    )
    view = tax_commands.TaxPlayerLedgerView(
        tax_service=tax_service,
        guild_id=123,
        requester_id=42,
        user=target,
        limit=limit,
        current_page=1,
        total_entries=21,
    )
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await view._show_page(interaction, 3)

    tax_service.count_ledger_entries.assert_called_once_with(123, user_id=99)
    tax_service.get_player_snapshot.assert_called_once_with(
        99,
        123,
        ledger_limit=limit,
        ledger_offset=16,
    )

    edited_message = interaction.response.messages[-1]
    assert edited_message["edit"] is True
    assert edited_message["view"] is view
    assert view.previous_page.disabled is False
    assert view.next_page.disabled is True
    _assert_ledger_page_metadata(
        edited_message["embed"],
        page=3,
        total_entries=21,
        limit=limit,
    )
    assert validate_embed(edited_message["embed"]) == []


@pytest.mark.asyncio
async def test_tax_fine_calls_service_and_reports_capped_amount(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    tax_service = SimpleNamespace(
        levy_fine=MagicMock(
            return_value={
                "status": "ok",
                "requested_amount": 10,
                "applied_amount": 7,
                "balance_before": 7,
                "balance_after": -3,
                "next_fine_at": 1_702_592_000,
            }
        )
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await cog.fine.callback(
        cog,
        interaction,
        user=target,
        amount=10,
        reason="failure to file",
    )

    tax_service.levy_fine.assert_called_once_with(
        99,
        123,
        amount=10,
        actor_id=42,
        reason="failure to file",
    )
    message = interaction.followup.messages[-1]
    assert message["ephemeral"] is True
    assert "Levied a 7" in message["content"]
    assert "Jopacoin Reserve" in message["content"]
    assert "capped to audited obligations" in message["content"]
    assert "7" in message["content"]
    assert "-3" in message["content"]
    assert "<t:1702592000:R>" in message["content"]


@pytest.mark.asyncio
async def test_tax_fine_reports_cooldown(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    tax_service = SimpleNamespace(
        levy_fine=MagicMock(
            return_value={
                "status": "cooldown",
                "next_fine_at": 1_702_592_000,
            }
        )
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await cog.fine.callback(
        cog,
        interaction,
        user=target,
        amount=10,
        reason=None,
    )

    assert "still on Tax Man fine cooldown" in interaction.followup.messages[-1]["content"]
    assert "<t:1702592000:f>" in interaction.followup.messages[-1]["content"]


@pytest.mark.asyncio
async def test_tax_resetcooldown_calls_service(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    tax_service = SimpleNamespace(
        reset_fine_cooldown=MagicMock(
            return_value={"status": "ok", "had_cooldown": True}
        )
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await cog.resetcooldown.callback(cog, interaction, user=target)

    tax_service.reset_fine_cooldown.assert_called_once_with(
        99,
        123,
        actor_id=42,
    )
    message = interaction.followup.messages[-1]
    assert message["ephemeral"] is True
    assert "Reset Tax Man fine cooldown for Taxpayer" in message["content"]


@pytest.mark.asyncio
async def test_tax_resetcooldown_reports_already_clear(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    tax_service = SimpleNamespace(
        reset_fine_cooldown=MagicMock(
            return_value={"status": "ok", "had_cooldown": False}
        )
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await cog.resetcooldown.callback(cog, interaction, user=target)

    assert "did not have an active Tax Man fine cooldown" in (
        interaction.followup.messages[-1]["content"]
    )


@pytest.mark.asyncio
async def test_tax_bankruptcy_add_calls_service(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    tax_service = SimpleNamespace(
        add_bankruptcy_modifier=MagicMock(
            return_value={
                "status": "ok",
                "action": "add",
                "games": 3,
                "previous_games": 0,
                "penalty_games_remaining": 3,
            }
        )
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await cog.bankruptcy.callback(
        cog,
        interaction,
        user=target,
        action=SimpleNamespace(value="add"),
        games=3,
        reason="manual review",
    )

    tax_service.add_bankruptcy_modifier.assert_called_once_with(
        99,
        123,
        games=3,
        actor_id=42,
        reason="manual review",
    )
    message = interaction.followup.messages[-1]
    assert message["ephemeral"] is True
    assert "Added 3" in message["content"]
    assert "0 -> 3" in message["content"]


@pytest.mark.asyncio
async def test_tax_bankruptcy_remove_calls_service(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kwargs):
        await interaction.followup.send(**kwargs)

    monkeypatch.setattr(tax_commands, "has_tax_man_permission", lambda _: True)
    monkeypatch.setattr(tax_commands, "safe_defer", _safe_defer)
    monkeypatch.setattr(tax_commands, "safe_followup", _safe_followup)

    target = SimpleNamespace(id=99, name="taxpayer", display_name="Taxpayer")
    tax_service = SimpleNamespace(
        remove_bankruptcy_modifier=MagicMock(
            return_value={
                "status": "ok",
                "action": "remove",
                "previous_games": 5,
                "penalty_games_remaining": 0,
            }
        )
    )
    cog = tax_commands.TaxCommands(bot=SimpleNamespace(), tax_service=tax_service)
    interaction = _FakeInteraction(guild_id=123, user_id=42)

    await cog.bankruptcy.callback(
        cog,
        interaction,
        user=target,
        action=SimpleNamespace(value="remove"),
        games=0,
        reason="appeal granted",
    )

    tax_service.remove_bankruptcy_modifier.assert_called_once_with(
        99,
        123,
        actor_id=42,
        reason="appeal granted",
    )
    message = interaction.followup.messages[-1]
    assert message["ephemeral"] is True
    assert "Removed bankruptcy modifier" in message["content"]
    assert "5 -> 0" in message["content"]
