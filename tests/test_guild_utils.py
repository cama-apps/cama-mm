"""Tests for guild utility functions."""

from utils.guild import normalize_guild_id, is_dm_context


class TestNormalizeGuildId:
    """Tests for normalize_guild_id."""

    def test_returns_guild_id_unchanged(self):
        """Valid guild IDs are returned unchanged."""
        assert normalize_guild_id(123456789) == 123456789
        assert normalize_guild_id(1) == 1

    def test_none_returns_zero(self):
        """None guild ID returns 0."""
        assert normalize_guild_id(None) == 0

    def test_zero_returns_zero(self):
        """Zero guild ID returns 0."""
        assert normalize_guild_id(0) == 0


class TestIsDmContext:
    """Tests for is_dm_context."""

    def test_none_is_dm(self):
        """None guild ID is a DM."""
        assert is_dm_context(None) is True

    def test_zero_is_dm(self):
        """Zero guild ID is a DM."""
        assert is_dm_context(0) is True

    def test_valid_guild_is_not_dm(self):
        """Valid guild ID is not a DM."""
        assert is_dm_context(123456789) is False
        assert is_dm_context(1) is False
