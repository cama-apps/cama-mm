"""BossEncounterView.fight must swallow a double-click.

Without the ``_engaged`` guard a fast second click re-enters resolution before
the first click's await completes and stops the view — on the carried-wager
path that resolves the same phase (and settles the wager) twice.
"""

from __future__ import annotations

import asyncio
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
