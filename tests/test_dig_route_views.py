from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import discord
import pytest

from commands.dig import DigCommands
from commands.dig_helpers._shared import LAYER_COLORS
from commands.dig_helpers.route_views import (
    RouteChoiceView,
    add_route_choice_fields,
    build_route_choice_embed,
)


@pytest.fixture
def route_choice():
    return {
        "choice_required": True,
        "layer": "Stone",
        "start_depth": 25,
        "end_depth": 50,
        "offered_routes": [
            {
                "id": "shored_passage",
                "name": "Shored Passage",
                "description": "Safer, but slower.",
                "layer": None,
                "effects": {},
            },
            {
                "id": "old_supports",
                "name": "Old Supports",
                "description": "Old braces hold the stone back.",
                "layer": "Stone",
                "effects": {},
            },
            {
                "id": "fossil_seam",
                "name": "Fossil Seam",
                "description": "A rich but unstable seam.",
                "layer": "Stone",
                "effects": {},
            },
        ],
        "active_route": None,
    }


def test_route_choice_embed_and_boss_fields_show_the_persisted_offer(route_choice):
    embed = build_route_choice_embed(route_choice)

    assert embed.title == "Stone Junction"
    assert "25" in embed.description and "50" in embed.description
    assert [field.name for field in embed.fields] == [
        "Shored Passage",
        "Old Supports",
        "Fossil Seam",
    ]
    assert "next guardian falls" not in embed.footer.text

    boss_embed = discord.Embed(title="Boss Fight Result", color=0x00FF00)
    add_route_choice_fields(boss_embed, route_choice)
    assert boss_embed.title == "Stone Junction"
    assert boss_embed.color.value == LAYER_COLORS["Stone"]
    assert len(boss_embed.fields) == 4
    assert "next guardian falls" not in boss_embed.fields[0].value


@pytest.mark.asyncio
async def test_route_view_timeout_disables_buttons_without_auto_selecting(route_choice):
    service = MagicMock()
    view = RouteChoiceView(service, 10001, 12345, route_choice)
    view.message = AsyncMock()

    await view.on_timeout()

    assert len(view.children) == 3
    assert all(button.disabled for button in view.children)
    service.choose_route.assert_not_called()
    view.message.edit.assert_awaited_once_with(view=view)


@pytest.mark.asyncio
async def test_route_button_locks_selection_and_replaces_junction_embed(
    route_choice,
    monkeypatch,
):
    service = MagicMock()
    service.choose_route.return_value = {
        "success": True,
        "error": None,
        "route": route_choice["offered_routes"][1],
        "already_selected": False,
    }
    view = RouteChoiceView(service, 10001, 12345, route_choice)
    interaction = SimpleNamespace(
        user=SimpleNamespace(id=10001),
        response=SimpleNamespace(send_message=AsyncMock()),
        edit_original_response=AsyncMock(),
        followup=SimpleNamespace(send=AsyncMock()),
    )
    monkeypatch.setattr(
        "commands.dig_helpers.route_views.safe_defer",
        AsyncMock(),
    )

    await view.children[1].callback(interaction)

    service.choose_route.assert_called_once_with(10001, 12345, "old_supports")
    interaction.edit_original_response.assert_awaited_once()
    kwargs = interaction.edit_original_response.await_args.kwargs
    assert kwargs["embed"].title == "Route Locked: Old Supports"
    assert kwargs["view"] is None


@pytest.mark.asyncio
async def test_dispatch_routes_pending_choice_to_recovery_view():
    cog = MagicMock(spec=DigCommands)
    cog._handle_route_choice = AsyncMock()
    interaction = MagicMock()
    result = SimpleNamespace(route_choice_required=True)

    await DigCommands._dispatch_dig_result(cog, interaction, 12345, result)

    cog._handle_route_choice.assert_awaited_once_with(interaction, 12345, result)
