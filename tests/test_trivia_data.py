"""Tests for services/trivia_data.py — data loading and CDN URL builders."""

import pytest

from services.trivia_data import (
    ability_icon_url,
    get_ability_icon_url_by_name,
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


@pytest.mark.parametrize(
    "fn,arg,expected_substr",
    [
        (hero_image_url, "npc_dota_hero_antimage", "/dota_react/heroes/antimage.png"),
        (hero_image_url, "", None),
        (
            ability_icon_url,
            "/panorama/images/spellicons/antimage_mana_break_png.png",
            "/dota_react/abilities/antimage_mana_break.png",
        ),
        (
            ability_icon_url,
            "/panorama/images/spellicons/some_ability.png",
            "/dota_react/abilities/some_ability.png",
        ),
        (ability_icon_url, None, None),
        (item_icon_url, "/panorama/images/items/blink_png.png", "/dota_react/items/blink.png"),
        (item_icon_url, "/panorama/images/items/recipe.png", "/dota_react/items/recipe.png"),
        (item_icon_url, None, None),
    ],
)
def test_cdn_url(fn, arg, expected_substr):
    """One pass over hero/ability/item URL builders. Catches double-.png and missing-prefix bugs."""
    result = fn(arg)
    if expected_substr is None:
        assert result is None
    else:
        assert result is not None
        assert expected_substr in result
        assert ".png.png" not in result


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

    def test_load_items_excludes_unavailable(self):
        """Items marked ItemPurchasable=0 in game files (non-neutral) must be excluded."""
        items = load_items()
        names = [i.localized_name for i in items]
        assert "Trident" not in names, "Trident is a disabled item and must be excluded"
        assert "Iron Talon" not in names, "Iron Talon was removed from the game"
        assert "Crystal Raindrop" not in names, "Crystal Raindrop was removed"
        assert "Faded Broach" not in names, "Faded Broach is a retired neutral item"
        assert "Gossamer Cape" not in names, "Gossamer Cape is a retired neutral item"
        assert "Broom Handle" not in names, "Broom Handle is a retired neutral item"

    def test_load_items_keeps_recipe_scrolls(self):
        """Recipe scrolls are legitimately purchasable and must remain in the item pool."""
        items = load_items()
        names = [i.localized_name for i in items]
        assert "Magic Wand Recipe" in names

    def test_load_items_dagon_level_suffix(self):
        """All 5 Dagon levels share localized_name 'Dagon' — must get level suffix."""
        items = load_items()
        names = [i.localized_name for i in items]
        for level in range(1, 6):
            assert f"Dagon {level}" in names, f"Expected 'Dagon {level}' in item pool"
        assert "Dagon" not in names, "Bare 'Dagon' must be replaced by 'Dagon 1'–'Dagon 5'"

    def test_load_items_dagon_is_leveled(self):
        """The 5 Dagon levels are the only leveled items; each carries base_level 1–5.

        'Dagon Recipe' shares the prefix but is a distinct, non-leveled item.
        """
        items = load_items()
        leveled = sorted((i for i in items if i.is_leveled), key=lambda i: i.base_level)
        assert [i.localized_name for i in leveled] == [f"Dagon {n}" for n in range(1, 6)]
        assert [i.base_level for i in leveled] == [1, 2, 3, 4, 5]

    def test_hero_damage_at_level1(self):
        """damage_*_at_level1 = base weapon damage + primary-attribute bonus at level 1."""
        heroes = load_heroes()
        # Present and ordered for every hero that has base attack damage.
        for h in heroes:
            if h.attack_damage_min:
                assert h.damage_min_at_level1 is not None
                assert h.damage_max_at_level1 >= h.damage_min_at_level1
                # Level-1 damage includes the attribute bonus, so it exceeds raw weapon damage.
                assert h.damage_min_at_level1 >= h.attack_damage_min

    def test_hero_armor_at_level1(self):
        """armor_at_level1 = base_armor + agility_base / 6, present on all heroes."""
        from dotabase_integration import Hero, dotabase_session

        heroes = load_heroes()
        assert all(h.armor_at_level1 is not None for h in heroes)
        am = next(h for h in heroes if h.localized_name == "Anti-Mage")
        with dotabase_session() as session:
            raw_am = session.query(Hero).filter(Hero.id == am.id).one()
            expected = raw_am.base_armor + raw_am.attr_agility_base / 6
        assert am.armor_at_level1 == pytest.approx(expected, abs=0.0001)

    def test_loaders_release_database_connections(self):
        from dotabase_integration import _ENGINE

        loaders = [load_heroes, load_abilities, load_items, load_voicelines, load_facets]
        for loader in loaders:
            loader.cache_clear()
        checked_out_before = _ENGINE.pool.checkedout()

        for loader in loaders:
            loader()

        assert _ENGINE.pool.checkedout() == checked_out_before

    def test_load_abilities_excludes_internal_names(self):
        """Abilities with underscores in localized_name are internal and should be filtered."""
        abilities = load_abilities()
        bad = [a for a in abilities if "_" in a.localized_name]
        assert bad == [], f"Internal-named abilities leaked: {[a.localized_name for a in bad[:5]]}"

    def test_get_ability_icon_url_by_name_is_case_insensitive(self):
        icon_url = get_ability_icon_url_by_name("  global silence  ")

        assert icon_url is not None
        assert icon_url.endswith("/abilities/silencer_global_silence.png")

    def test_get_ability_icon_url_by_name_missing(self):
        assert get_ability_icon_url_by_name("Definitely Not An Ability") is None

    def test_load_voicelines(self):
        voicelines = load_voicelines()
        assert len(voicelines) > 100
        for vl in voicelines[:10]:
            assert vl.hero_id
            assert len(vl.text) >= 15

    def test_load_facets(self):
        facets = load_facets()
        assert len(facets) > 30
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
