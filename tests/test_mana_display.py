"""Tests for the emoji-only mana badge helper."""

from __future__ import annotations

import pytest

from domain.models.mana_effects import ManaEffects
from utils.mana_display import format_mana_badge


def test_format_mana_badge_returns_emoji_for_each_land():
    """Every supported land returns its associated emoji."""
    cases = [
        ("Forest", "🌲"),
        ("Mountain", "⛰️"),
        ("Island", "🏝️"),
        ("Plains", "🌾"),
        ("Swamp", "🌿"),
    ]
    for land, expected in cases:
        effects = ManaEffects.for_color({
            "Forest": "Green", "Mountain": "Red", "Island": "Blue",
            "Plains": "White", "Swamp": "Black",
        }[land], land)
        assert format_mana_badge(effects) == expected


def test_format_mana_badge_unassigned_returns_empty():
    """No land assignment yields an empty string (caller's choice what to do)."""
    assert format_mana_badge(None) == ""
    assert format_mana_badge(ManaEffects()) == ""


def test_format_mana_badge_unknown_land_returns_empty():
    """Unknown land label returns empty string rather than raising."""
    e = ManaEffects(color="???", land="Atlantis")
    assert format_mana_badge(e) == ""


@pytest.mark.asyncio
async def test_resolve_mana_badge_no_service_returns_empty():
    """Bot without mana_effects_service yields empty badge."""
    from utils.mana_display import resolve_mana_badge

    class FakeBot:
        pass

    badge = await resolve_mana_badge(FakeBot(), 1, 1)
    assert badge == ""


@pytest.mark.asyncio
async def test_resolve_mana_badge_returns_emoji():
    """A bot with a working mana service returns the player's emoji."""
    from utils.mana_display import resolve_mana_badge

    class FakeMana:
        def get_effects(self, discord_id, guild_id):
            return ManaEffects.for_color("Green", "Forest")

    class FakeBot:
        mana_effects_service = FakeMana()

    badge = await resolve_mana_badge(FakeBot(), 1, 1)
    assert badge == "🌲"


@pytest.mark.asyncio
async def test_resolve_mana_badge_swallows_exceptions():
    """A failing mana service yields '' rather than propagating an exception."""
    from utils.mana_display import resolve_mana_badge

    class FakeMana:
        def get_effects(self, discord_id, guild_id):
            raise RuntimeError("boom")

    class FakeBot:
        mana_effects_service = FakeMana()

    badge = await resolve_mana_badge(FakeBot(), 1, 1)
    assert badge == ""
