"""PrestigePerksView must swallow a double-click.

Without the ``_resolved`` guard a fast second click re-enters the perk
callback before the first click's awaits complete and ``stop()`` runs — so
the service call would execute twice.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import commands.dig_helpers.progression_views as pv


async def test_perk_callback_ignores_double_click(monkeypatch):
    deferred: list[int] = []

    async def fake_defer(interaction):
        deferred.append(1)

    monkeypatch.setattr(pv, "safe_defer", fake_defer)
    monkeypatch.setattr(pv, "get_neon_service", lambda client: None)

    dig_service = MagicMock()
    dig_service.prestige.return_value = {
        "success": True,
        "prestige_level": 1,
        "run_score": 10,
        "best_run_score": 10,
        "message": "done",
    }

    view = pv.PrestigePerksView(
        dig_service,
        user_id=42,
        guild_id=7,
        perks=[{"id": "p1", "name": "Perk"}],
    )
    monkeypatch.setattr(view, "_announce_ascension_publicly", AsyncMock())

    interaction = MagicMock()
    interaction.user.id = 42
    interaction.response = AsyncMock()
    interaction.followup = AsyncMock()

    callback = view.children[0].callback

    # First click resolves the prestige once.
    await callback(interaction)
    assert view._resolved is True
    assert dig_service.prestige.call_count == 1

    # Second click is swallowed by the guard — no second service call.
    await callback(interaction)
    dig_service.prestige.assert_called_once()
    interaction.response.send_message.assert_awaited_once()
    args, kwargs = interaction.response.send_message.await_args
    assert kwargs.get("ephemeral") is True
