"""Tests for utils/guild.py - guild ID normalization utilities."""

import pytest

from utils.guild import normalize_guild_id, is_dm_context


class TestNormalizeGuildId:
    """Tests for normalize_guild_id function."""

    def test_none_returns_zero(self):
        """None guild ID should normalize to 0."""
        assert normalize_guild_id(None) == 0

    def test_zero_returns_zero(self):
        """Zero guild ID should remain 0."""
        assert normalize_guild_id(0) == 0

    def test_positive_id_unchanged(self):
        """Positive guild IDs should be returned unchanged."""
        assert normalize_guild_id(123456789) == 123456789

    def test_large_discord_id(self):
        """Large Discord snowflake IDs should work correctly."""
        large_id = 1234567890123456789  # Typical Discord snowflake
        assert normalize_guild_id(large_id) == large_id


class TestIsDmContext:
    """Tests for is_dm_context function."""

    def test_none_is_dm(self):
        """None guild ID indicates DM context."""
        assert is_dm_context(None) is True

    def test_zero_is_dm(self):
        """Zero guild ID indicates DM context (normalized None)."""
        assert is_dm_context(0) is True

    def test_positive_id_not_dm(self):
        """Positive guild ID indicates guild context, not DM."""
        assert is_dm_context(123456789) is False

    def test_large_discord_id_not_dm(self):
        """Large Discord snowflake is not a DM context."""
        assert is_dm_context(1234567890123456789) is False
