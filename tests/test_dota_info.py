"""
Tests for DotaInfoCommands cog.
"""

from commands.dota_info import (
    _format_ability_values,
    _format_stat,
    _get_ability_by_name,
    _get_all_abilities,
    _get_all_heroes,
    _get_hero_by_name,
)


class TestHeroLookup:
    """Tests for hero lookup functions."""

    def test_get_all_heroes_returns_list(self):
        """Test that get_all_heroes returns a list of tuples."""
        heroes = _get_all_heroes()
        assert isinstance(heroes, list)
        assert len(heroes) > 100  # Should have 100+ heroes
        # Each entry should be (name, id)
        assert all(isinstance(h, tuple) and len(h) == 2 for h in heroes)
        assert all(isinstance(h[0], str) and isinstance(h[1], int) for h in heroes)

    def test_get_hero_by_name_exact_match(self):
        """Test finding hero by exact name."""
        hero = _get_hero_by_name("Anti-Mage")
        assert hero is not None
        assert hero.localized_name == "Anti-Mage"
        assert hero.id == 1

    def test_get_hero_by_name_case_insensitive(self):
        """Test finding hero with different case."""
        hero = _get_hero_by_name("anti-mage")
        assert hero is not None
        assert hero.localized_name == "Anti-Mage"

    def test_get_hero_by_name_alias(self):
        """Test finding hero by alias."""
        hero = _get_hero_by_name("am")
        assert hero is not None
        assert hero.localized_name == "Anti-Mage"

    def test_get_hero_by_name_not_found(self):
        """Test that non-existent hero returns None."""
        hero = _get_hero_by_name("NotARealHero123")
        assert hero is None

    def test_get_hero_has_abilities(self):
        """Test that hero has abilities."""
        hero = _get_hero_by_name("Pudge")
        assert hero is not None
        assert hero.abilities is not None
        assert len(hero.abilities) > 0
        # Check for Meat Hook
        ability_names = [a.localized_name for a in hero.abilities]
        assert "Meat Hook" in ability_names

    def test_get_hero_has_talents(self):
        """Test that hero has talents."""
        hero = _get_hero_by_name("Crystal Maiden")
        assert hero is not None
        assert hero.talents is not None
        # Should have 8 talents (4 pairs)
        assert len(hero.talents) >= 4


class TestAbilityLookup:
    """Tests for ability lookup functions."""

    def test_get_all_abilities_returns_list(self):
        """Test that get_all_abilities returns a list of tuples."""
        abilities = _get_all_abilities()
        assert isinstance(abilities, list)
        assert len(abilities) > 200  # Should have many abilities
        # Each entry should be (name, id)
        assert all(isinstance(a, tuple) and len(a) == 2 for a in abilities)

    def test_get_ability_by_name_exact_match(self):
        """Test finding ability by exact name."""
        ability = _get_ability_by_name("Meat Hook")
        assert ability is not None
        assert ability.localized_name == "Meat Hook"

    def test_get_ability_by_name_case_insensitive(self):
        """Test finding ability with different case."""
        ability = _get_ability_by_name("meat hook")
        assert ability is not None
        assert ability.localized_name == "Meat Hook"

    def test_get_ability_by_name_not_found(self):
        """Test that non-existent ability returns None."""
        ability = _get_ability_by_name("NotARealAbility123")
        assert ability is None

    def test_ability_has_description(self):
        """Test that ability has description."""
        ability = _get_ability_by_name("Blink")
        assert ability is not None
        assert ability.description is not None
        assert len(ability.description) > 0


class TestFormatting:
    """Tests for formatting functions."""

    def test_format_stat_basic(self):
        """Test basic stat formatting."""
        result = _format_stat("Damage", 100)
        assert result == "**Damage:** 100"

    def test_format_stat_with_suffix(self):
        """Test stat formatting with suffix."""
        result = _format_stat("Magic Resist", 25, "%")
        assert result == "**Magic Resist:** 25%"

    def test_format_stat_none_value(self):
        """Test stat formatting with None value."""
        result = _format_stat("Damage", None)
        assert result == ""

    def test_format_ability_values_empty(self):
        """Test ability values formatting with no specials."""

        class MockAbility:
            ability_special = None

        result = _format_ability_values(MockAbility())
        assert result == ""

    def test_format_ability_values_with_data(self):
        """Test ability values formatting with special values."""

        class MockAbility:
            ability_special = [
                {"header": "DAMAGE:", "value": "90 180 270 360"},
                {"header": "RANGE:", "value": "1100 1200 1300 1400"},
            ]

        result = _format_ability_values(MockAbility())
        assert "Damage" in result
        assert "90 180 270 360" in result


class TestHeroAttributes:
    """Tests for hero attribute data."""

    def test_hero_has_primary_attribute(self):
        """Test that heroes have primary attributes."""
        # STR hero
        axe = _get_hero_by_name("Axe")
        assert axe is not None
        assert axe.attr_primary == "strength"

        # AGI hero
        pa = _get_hero_by_name("Phantom Assassin")
        assert pa is not None
        assert pa.attr_primary == "agility"

        # INT hero
        cm = _get_hero_by_name("Crystal Maiden")
        assert cm is not None
        assert cm.attr_primary == "intelligence"

    def test_hero_has_base_stats(self):
        """Test that heroes have base stats."""
        hero = _get_hero_by_name("Pudge")
        assert hero is not None
        assert hero.attr_strength_base is not None
        assert hero.attr_agility_base is not None
        assert hero.attr_intelligence_base is not None
        assert hero.base_movement is not None
        assert hero.base_armor is not None

    def test_hero_has_roles(self):
        """Test that heroes have roles."""
        hero = _get_hero_by_name("Lion")
        assert hero is not None
        assert hero.roles is not None
        assert "Support" in hero.roles or "Disabler" in hero.roles


class TestHeroFacets:
    """Tests for hero facets (new feature)."""

    def test_hero_has_facets(self):
        """Test that heroes have facets."""
        hero = _get_hero_by_name("Anti-Mage")
        assert hero is not None
        assert hero.facets is not None
        assert len(hero.facets) >= 2  # Should have at least 2 facets

    def test_facet_has_description(self):
        """Test that facets have descriptions."""
        hero = _get_hero_by_name("Anti-Mage")
        assert hero is not None
        for facet in hero.facets:
            assert facet.localized_name is not None
            assert facet.description is not None
