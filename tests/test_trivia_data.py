"""Tests for services/trivia_data.py — data loading and CDN URL builders."""

import pytest

from services.trivia_data import (
    ability_icon_url,
    get_hero_by_id,
    hero_image_url,
    item_icon_url,
    load_abilities,
    load_facets,
    load_heroes,
    load_items,
    load_voicelines,
    redact_hero_name,
)


class TestCDNUrls:
    def test_hero_image_url(self):
        url = hero_image_url("npc_dota_hero_antimage")
        assert url is not None
        assert "antimage.png" in url
        assert "dota_react/heroes/" in url

    def test_hero_image_url_empty(self):
        assert hero_image_url("") is None

    def test_ability_icon_url(self):
        url = ability_icon_url("/panorama/images/spellicons/antimage_mana_break_png.png")
        assert url is not None
        assert "antimage_mana_break.png" in url
        assert "dota_react/abilities/" in url

    def test_ability_icon_url_none(self):
        assert ability_icon_url(None) is None

    def test_item_icon_url(self):
        url = item_icon_url("/panorama/images/items/blink_png.png")
        assert url is not None
        assert "blink.png" in url
        assert "dota_react/items/" in url

    def test_item_icon_url_none(self):
        assert item_icon_url(None) is None

    def test_item_icon_url_recipe(self):
        """Recipe icons end in .png (not _png.png) — must not produce double .png."""
        url = item_icon_url("/panorama/images/items/recipe.png")
        assert url is not None
        assert url.endswith("/items/recipe.png")
        assert ".png.png" not in url

    def test_item_icon_url_normal(self):
        """Standard items with _png.png suffix still work."""
        url = item_icon_url("/panorama/images/items/blink_png.png")
        assert url is not None
        assert url.endswith("/items/blink.png")
        assert ".png.png" not in url

    def test_ability_icon_url_trailing_png(self):
        """Ability icons ending in .png (not _png.png) must not produce double .png."""
        url = ability_icon_url("/panorama/images/spellicons/some_ability.png")
        assert url is not None
        assert url.endswith("/abilities/some_ability.png")
        assert ".png.png" not in url


class TestDataLoading:
    def test_load_heroes(self):
        heroes = load_heroes()
        assert len(heroes) > 100
        hero = heroes[0]
        assert hero.localized_name
        assert hero.id > 0

    def test_load_abilities(self):
        abilities = load_abilities()
        assert len(abilities) > 500
        # All should have localized names (filtered out otherwise)
        for a in abilities[:20]:
            assert a.localized_name

    def test_load_items(self):
        items = load_items()
        assert len(items) > 100
        # At least some should have cost
        with_cost = [i for i in items if i.cost]
        assert len(with_cost) > 50

    def test_load_items_excludes_internal_names(self):
        """Items with underscores in localized_name are internal and should be filtered."""
        items = load_items()
        bad = [i for i in items if "_" in i.localized_name]
        assert bad == [], f"Internal-named items leaked: {[i.localized_name for i in bad[:5]]}"

    def test_load_abilities_excludes_internal_names(self):
        """Abilities with underscores in localized_name are internal and should be filtered."""
        abilities = load_abilities()
        bad = [a for a in abilities if "_" in a.localized_name]
        assert bad == [], f"Internal-named abilities leaked: {[a.localized_name for a in bad[:5]]}"

    def test_load_voicelines(self):
        voicelines = load_voicelines()
        assert len(voicelines) > 100
        for vl in voicelines[:10]:
            assert vl.hero_id
            assert len(vl.text) >= 15

    def test_load_facets(self):
        facets = load_facets()
        assert len(facets) > 50
        for f in facets[:10]:
            assert f.localized_name
            assert f.hero_id

    def test_get_hero_by_id(self):
        heroes = load_heroes()
        hero = get_hero_by_id(heroes[0].id)
        assert hero is not None
        assert hero.id == heroes[0].id

    def test_get_hero_by_id_missing(self):
        assert get_hero_by_id(999999) is None


class TestRedactHeroName:
    def test_redacts_full_name(self):
        result = redact_hero_name("Anti-Mage is a powerful hero", "Anti-Mage")
        assert "Anti-Mage" not in result
        assert "???" in result

    def test_redacts_name_parts(self):
        result = redact_hero_name("The Phantom Assassin strikes", "Phantom Assassin")
        assert "Phantom" not in result
        assert "Assassin" not in result

    def test_does_not_redact_short_words(self):
        # Words <= 2 chars should not be redacted
        result = redact_hero_name("The io of the game", "Io")
        # "Io" is only 2 chars so not redacted by word split
        assert "game" in result

    def test_empty_text(self):
        assert redact_hero_name("", "Hero") == ""

    def test_none_text(self):
        assert redact_hero_name(None, "Hero") == ""
