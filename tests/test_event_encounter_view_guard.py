"""EventEncounterView must swallow a double-click.

Without the ``_resolved`` guard a fast second click (on any of the safe/risky/
desperate buttons) re-enters resolution before the first click's await completes
and stops the view. Because ``resolve_event`` applies the outcome — crediting JC
— with no consumed-state of its own, 172/188 events have a guaranteed-success
safe option, so repeated clicks within the 60s window bank the reward N times.
"""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock

import commands.dig_helpers.event_views as ev


def _make_view():
    return ev.EventEncounterView(
        MagicMock(),
        user_id=42,
        guild_id=7,
        event_data={
            "id": "underground_stream",
            "safe_option": {"label": "Play it safe"},
            "risky_option": {"label": "Take the risk"},
        },
    )


def test_event_encounter_ignores_double_click(monkeypatch):
    async def fake_defer(interaction):
        return True

    monkeypatch.setattr(ev, "safe_defer", fake_defer)

    async def scenario():
        view = _make_view()
        # Isolate the re-entry guard from the service call.
        resolve_calls: list[str] = []

        async def fake_resolve(choice):
            resolve_calls.append(choice)
            return MagicMock()

        view._resolve = fake_resolve
        view._send_result = AsyncMock()

        interaction = MagicMock()
        interaction.user.id = 42
        interaction.response = AsyncMock()

        # First click resolves once and marks the view resolved.
        await view.safe_btn.callback(interaction)
        assert view._resolved is True
        assert resolve_calls == ["safe"]

        # A second click — even on a different button — is swallowed: no second
        # resolution, so no second credit.
        await view.risky_btn.callback(interaction)
        assert resolve_calls == ["safe"]

    asyncio.run(scenario())


def test_event_encounter_rejects_non_owner(monkeypatch):
    async def fake_defer(interaction):
        return True

    monkeypatch.setattr(ev, "safe_defer", fake_defer)

    async def scenario():
        view = _make_view()
        resolve_calls: list[str] = []

        async def fake_resolve(choice):
            resolve_calls.append(choice)
            return MagicMock()

        view._resolve = fake_resolve
        view._send_result = AsyncMock()

        interaction = MagicMock()
        interaction.user.id = 999  # not the digger
        interaction.response = AsyncMock()

        await view.safe_btn.callback(interaction)
        # No resolution for a non-owner, and the view stays open for the owner.
        assert resolve_calls == []
        assert view._resolved is False

    asyncio.run(scenario())
