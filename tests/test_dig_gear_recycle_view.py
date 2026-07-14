"""Discord view coverage for relic recycling from /dig gear."""

from unittest.mock import MagicMock

from commands.dig_helpers.gear_views import GearPanelView, RecycleRelicView


def test_gear_panel_exposes_recycle_button():
    view = GearPanelView(MagicMock(), user_id=12, guild_id=34)

    assert "Recycle Relics" in {item.label for item in view.children}


def test_recycle_view_requires_three_relics_and_labels_rarity():
    service = MagicMock()
    parent = GearPanelView(service, user_id=12, guild_id=34)
    relics = [
        {
            "db_id": index,
            "name": f"Relic {index}",
            "rarity": "Common",
            "equipped": 0,
            "recyclable": True,
        }
        for index in range(1, 4)
    ]

    view = RecycleRelicView(
        dig_service=service,
        user_id=12,
        guild_id=34,
        relics=relics,
        parent=parent,
    )

    assert view.select.min_values == 3
    assert view.select.max_values == 3
    assert all(option.label.startswith("[Common]") for option in view.select.options)
