"""Tests for DotaInfoCommands cog — hero/ability lookup and formatting helpers."""

import pytest

from commands.dota_info import (
    _format_ability_values,
    _format_stat,
    _get_ability_by_name,
    _get_all_abilities,
    _get_all_heroes,
    _get_hero_by_name,
)


class TestHeroLookup:
    def test_get_all_heroes_shape(self):
        heroes = _get_all_heroes()
        assert isinstance(heroes, list)
        assert len(heroes) > 100
        assert all(isinstance(h, tuple) and len(h) == 2 for h in heroes)
        assert all(isinstance(h[0], str) and isinstance(h[1], int) for h in heroes)

    @pytest.mark.parametrize(
        "query,expected_name,expected_id",
        [
            ("Anti-Mage", "Anti-Mage", 1),
            ("anti-mage", "Anti-Mage", 1),
            ("am", "Anti-Mage", 1),
        ],
    )
    def test_hero_by_name_finds(self, query, expected_name, expected_id):
        hero = _get_hero_by_name(query)
        assert hero is not None
        assert hero.localized_name == expected_name
        assert hero.id == expected_id

    def test_hero_by_name_missing_returns_none(self):
        assert _get_hero_by_name("NotARealHero123") is None

    def test_hero_abilities_and_talents(self):
        pudge = _get_hero_by_name("Pudge")
        assert pudge is not None
        ability_names = [a.localized_name for a in pudge.abilities]
        assert "Meat Hook" in ability_names

        cm = _get_hero_by_name("Crystal Maiden")
        assert cm is not None
        assert len(cm.talents) >= 4


class TestAbilityLookup:
    def test_get_all_abilities_shape(self):
        abilities = _get_all_abilities()
        assert isinstance(abilities, list)
        assert len(abilities) > 200
        assert all(isinstance(a, tuple) and len(a) == 2 for a in abilities)

    @pytest.mark.parametrize("query", ["Meat Hook", "meat hook"])
    def test_ability_by_name_finds(self, query):
        ability = _get_ability_by_name(query)
        assert ability is not None
        assert ability.localized_name == "Meat Hook"

    def test_ability_by_name_missing_returns_none(self):
        assert _get_ability_by_name("NotARealAbility123") is None

    def test_ability_has_description(self):
        blink = _get_ability_by_name("Blink")
        assert blink is not None
        assert blink.description and len(blink.description) > 0


class TestFormatting:
    @pytest.mark.parametrize(
        "label,value,suffix,expected",
        [
            ("Damage", 100, "", "**Damage:** 100"),
            ("Magic Resist", 25, "%", "**Magic Resist:** 25%"),
            ("Damage", None, "", ""),
        ],
    )
    def test_format_stat(self, label, value, suffix, expected):
        assert _format_stat(label, value, suffix) == expected

    def test_format_ability_values_empty(self):
        class MockAbility:
            ability_special = None

        assert _format_ability_values(MockAbility()) == ""

    def test_format_ability_values_with_data(self):
        class MockAbility:
            ability_special = [
                {"header": "DAMAGE:", "value": "90 180 270 360"},
                {"header": "RANGE:", "value": "1100 1200 1300 1400"},
            ]

        result = _format_ability_values(MockAbility())
        assert "Damage" in result
        assert "90 180 270 360" in result


class TestHeroAttributes:
    @pytest.mark.parametrize(
        "name,attr",
        [
            ("Axe", "strength"),
            ("Phantom Assassin", "agility"),
            ("Crystal Maiden", "intelligence"),
        ],
    )
    def test_hero_primary_attr(self, name, attr):
        hero = _get_hero_by_name(name)
        assert hero is not None
        assert hero.attr_primary == attr

    def test_hero_base_stats_present(self):
        pudge = _get_hero_by_name("Pudge")
        assert pudge is not None
        assert pudge.attr_strength_base is not None
        assert pudge.attr_agility_base is not None
        assert pudge.attr_intelligence_base is not None
        assert pudge.base_movement is not None
        assert pudge.base_armor is not None

    def test_hero_has_roles(self):
        lion = _get_hero_by_name("Lion")
        assert lion is not None
        assert "Support" in lion.roles or "Disabler" in lion.roles


class TestHeroFacets:
    def test_hero_has_facets_with_descriptions(self):
        wd = _get_hero_by_name("Witch Doctor")
        assert wd is not None
        assert len(wd.facets) >= 2
        for facet in wd.facets:
            assert facet.localized_name
            assert facet.description
