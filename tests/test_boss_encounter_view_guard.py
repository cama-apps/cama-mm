"""BossEncounterView.fight must swallow a double-click.

Without the ``_engaged`` guard a fast second click re-enters resolution before
the first click's await completes and stops the view — on the carried-wager
path that resolves the same phase (and settles the wager) twice.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import commands.dig_helpers.boss_views as bv


def test_fight_ignores_double_click(monkeypatch):
    deferred: list[int] = []

    async def fake_defer(interaction):
        deferred.append(1)

    monkeypatch.setattr(bv, "safe_defer", fake_defer)
    monkeypatch.setattr(bv, "BossWagerModal", lambda *a, **k: MagicMock())

    async def scenario():
        dig_service = MagicMock()
        dig_service.get_carried_wager.return_value = None  # no carry → modal path
        view = bv.BossEncounterView(
            dig_service, user_id=42, guild_id=7, boss_info=MagicMock(),
        )

        interaction = MagicMock()
        interaction.user.id = 42
        interaction.response = AsyncMock()

        # ``view.fight`` is a discord.py Button; invoke its bound callback.
        fight = view.fight.callback

        # First click opens the wager modal and marks the view engaged.
        await fight(interaction)
        assert view._engaged is True
        interaction.response.send_modal.assert_awaited_once()

        # Second click is swallowed by the guard — no second modal, just a defer.
        interaction.response.send_modal.reset_mock()
        await fight(interaction)
        interaction.response.send_modal.assert_not_called()
        assert deferred == [1]

    asyncio.run(scenario())


def test_fight_uses_risk_modal_when_wagers_are_disabled(monkeypatch):
    risk_modal = MagicMock()
    risk_modal_factory = MagicMock(return_value=risk_modal)
    wager_modal_factory = MagicMock()

    monkeypatch.setattr(bv, "BossRiskModal", risk_modal_factory)
    monkeypatch.setattr(bv, "BossWagerModal", wager_modal_factory)

    async def scenario():
        dig_service = MagicMock()
        dig_service.get_carried_wager.return_value = None
        boss_info = MagicMock()
        boss_info.wager_allowed = False
        view = bv.BossEncounterView(
            dig_service, user_id=42, guild_id=7, boss_info=boss_info,
        )

        interaction = MagicMock()
        interaction.user.id = 42
        interaction.response = AsyncMock()

        await view.fight.callback(interaction)

        interaction.response.send_modal.assert_awaited_once_with(risk_modal)
        assert view._engaged is True
        assert not dig_service.get_carried_wager.called
        wager_modal_factory.assert_not_called()

    asyncio.run(scenario())


def test_risk_modal_submit_reports_unexpected_resolution_failure(monkeypatch):
    deferred = []
    followups = []

    async def fake_defer(interaction, **kwargs):
        deferred.append(kwargs)
        return True

    async def fake_followup(interaction, **kwargs):
        followups.append(kwargs)

    async def fail_resolution(*args, **kwargs):
        raise RuntimeError("boom")

    monkeypatch.setattr(bv, "safe_defer", fake_defer)
    monkeypatch.setattr(bv, "safe_followup", fake_followup)
    monkeypatch.setattr(bv, "_resolve_phase_fight_without_modal", fail_resolution)

    async def scenario():
        modal = SimpleNamespace(
            risk_tier=SimpleNamespace(value="cautious"),
            dig_service=MagicMock(),
            user_id=42,
            guild_id=7,
            dig_flavor_service=None,
            stop=MagicMock(),
        )
        interaction = MagicMock()

        await bv.BossRiskModal.on_submit(modal, interaction)

        assert deferred == [{"thinking": True}]
        assert followups == [{
            "content": "Boss fight failed. Try again.",
            "ephemeral": True,
        }]
        modal.stop.assert_called_once()

    asyncio.run(scenario())
