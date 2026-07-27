"""Functional UI coverage for large /dig gear collections."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from commands.dig_helpers.gear_views import GearPanelView, GearSelectView


def _gear_item(index: int) -> dict:
    return {
        "id": index,
        "slot": "ring" if index % 2 else "armor",
        "tier": 3,
        "item_id": f"unique_{index}",
        "name": f"Gear {index:02d}",
        "durability": 14,
        "max_durability": 14,
        "equipped": False,
        "effect": f"Effect {index}",
    }


def _relic(index: int) -> dict:
    return {
        "db_id": 1000 + index,
        "name": f"Relic {index:02d}",
        "equipped": False,
    }


@pytest.mark.asyncio
async def test_gear_picker_paginates_every_manageable_item():
    service = MagicMock()
    parent = GearPanelView(service, user_id=12, guild_id=34)
    gear_items = [_gear_item(index) for index in range(30)]
    relics = [_relic(index) for index in range(5)]

    first = GearSelectView(
        dig_service=service,
        user_id=12,
        guild_id=34,
        mode="equip",
        gear_items=gear_items,
        relics=relics,
        parent=parent,
        page=0,
    )
    second = GearSelectView(
        dig_service=service,
        user_id=12,
        guild_id=34,
        mode="equip",
        gear_items=gear_items,
        relics=relics,
        parent=parent,
        page=1,
    )

    first_values = {option.value for option in first.select.options}
    second_values = {option.value for option in second.select.options}

    assert first.page_count == 2
    assert len(first_values) == 25
    assert len(second_values) == 10
    assert first_values.isdisjoint(second_values)
    assert len(first_values | second_values) == 35
    assert first.previous_btn.disabled is True
    assert first.next_btn.disabled is False
    assert second.previous_btn.disabled is False
    assert second.next_btn.disabled is True
