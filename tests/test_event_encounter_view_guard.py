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
        interaction.message.edit = AsyncMock()

        # First click resolves once and marks the view resolved.
        await view.safe_btn.callback(interaction)
        assert view._resolved is True
        assert resolve_calls == ["safe"]
        assert all(child.disabled for child in view.children)
        interaction.message.edit.assert_awaited_once_with(view=view)

        # A second click — even on a different button — is swallowed: no second
        # resolution, so no second credit.
        await view.risky_btn.callback(interaction)
        assert resolve_calls == ["safe"]

    asyncio.run(scenario())


def test_boon_selection_ignores_double_click_and_locks_controls(monkeypatch):
    async def fake_defer(interaction):
        return True

    monkeypatch.setattr(ev, "safe_defer", fake_defer)

    async def scenario():
        calls: list[str] = []

        class Service:
            def resolve_event(self, _user_id, _guild_id, event_id, choice):
                calls.append(f"{event_id}:{choice}")
                return {
                    "success": True,
                    "message": "The boon settles into place.",
                }

        view = ev.BoonSelectionView(
            Service(),
            user_id=42,
            guild_id=7,
            event_data={
                "id": "boon_probe",
                "name": "Boon Probe",
                "boon_options": [{"name": "Take the light"}],
            },
        )
        interaction = MagicMock()
        interaction.user.id = 42
        interaction.response = AsyncMock()
        interaction.followup = AsyncMock()
        interaction.message.edit = AsyncMock()

        button = view.children[0]
        await button.callback(interaction)
        await button.callback(interaction)

        assert view._resolved is True
        assert calls == ["boon_probe:boon_0"]
        assert all(child.disabled for child in view.children)
        interaction.message.edit.assert_awaited_once_with(view=view)

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
