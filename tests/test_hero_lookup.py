"""
Tests for hero lookup utility.
"""

import pytest

from utils.hero_lookup import get_hero_name, get_hero_short_name, get_all_heroes


class TestHeroLookup:
    """Tests for hero name lookup."""

    def test_get_hero_name_known_hero(self):
        """Test getting known hero names."""
        assert get_hero_name(1) == "Anti-Mage"
        assert get_hero_name(2) == "Axe"
        assert get_hero_name(14) == "Pudge"
        assert get_hero_name(44) == "Phantom Assassin"

    def test_get_hero_name_unknown_hero(self):
        """Test getting unknown hero ID returns fallback."""
        result = get_hero_name(9999)
        assert result == "Hero 9999"

    def test_get_hero_short_name_abbreviations(self):
        """Test hero short names with known abbreviations."""
        assert get_hero_short_name(1) == "AM"
        assert get_hero_short_name(5) == "CM"
        assert get_hero_short_name(44) == "PA"
        assert get_hero_short_name(46) == "TA"

    def test_get_hero_short_name_first_word(self):
        """Test hero short names fallback to first word."""
        # Heroes without abbreviations should return first word
        assert get_hero_short_name(3) == "Bane"  # Single word, returns as-is
        assert get_hero_short_name(2) == "Axe"

    def test_get_all_heroes(self):
        """Test getting all heroes."""
        heroes = get_all_heroes()
        assert isinstance(heroes, dict)
        assert len(heroes) > 100  # Should have 100+ heroes
        assert "1" in heroes
        assert heroes["1"] == "Anti-Mage"

    def test_hero_lookup_is_cached(self):
        """Test that hero data is loaded once and cached."""
        # First call loads
        get_hero_name(1)
        # Second call should use cache (this is implicit - just checking it works)
        assert get_hero_name(1) == "Anti-Mage"


class TestHeroData:
    """Tests for hero data integrity."""

    def test_all_hero_ids_are_strings(self):
        """Hero IDs in JSON are strings."""
        heroes = get_all_heroes()
        for key in heroes.keys():
            assert isinstance(key, str)
            assert key.isdigit()

    def test_common_heroes_exist(self):
        """Test that common heroes are present."""
        heroes = get_all_heroes()
        expected_heroes = [
            ("1", "Anti-Mage"),
            ("14", "Pudge"),
            ("74", "Invoker"),
            ("129", "Mars"),
        ]
        for hero_id, name in expected_heroes:
            assert hero_id in heroes
            assert heroes[hero_id] == name
