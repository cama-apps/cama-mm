"""
Tests for GuildConfigService.

This service manages per-guild configuration settings including:
- League ID (for match enrichment)
- Auto-enrich toggle
- AI features toggle
"""

import pytest

from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

# Uses guild_config_service fixture from conftest.py


class TestGuildConfigService:
    """Test GuildConfigService functionality."""

    def test_get_config_returns_none_for_new_guild(self, guild_config_service):
        """Test that get_config returns None for a guild with no configuration."""
        config = guild_config_service.get_config(TEST_GUILD_ID)
        assert config is None

    def test_set_and_get_league_id(self, guild_config_service):
        """Test setting and retrieving league ID."""
        league_id = 12345

        guild_config_service.set_league_id(TEST_GUILD_ID, league_id)
        result = guild_config_service.get_league_id(TEST_GUILD_ID)

        assert result == league_id

    def test_get_league_id_returns_none_when_not_set(self, guild_config_service):
        """Test that get_league_id returns None when no league is configured."""
        result = guild_config_service.get_league_id(TEST_GUILD_ID)
        assert result is None

    def test_league_id_per_guild_isolation(self, guild_config_service):
        """Test that league IDs are isolated per guild."""
        guild_config_service.set_league_id(TEST_GUILD_ID, 111)
        guild_config_service.set_league_id(TEST_GUILD_ID_SECONDARY, 222)

        assert guild_config_service.get_league_id(TEST_GUILD_ID) == 111
        assert guild_config_service.get_league_id(TEST_GUILD_ID_SECONDARY) == 222

    def test_auto_enrich_defaults_to_true(self, guild_config_service):
        """Test that auto-enrich defaults to True for new guilds."""
        result = guild_config_service.is_auto_enrich_enabled(TEST_GUILD_ID)
        assert result is True

    def test_set_auto_enrich_false(self, guild_config_service):
        """Test disabling auto-enrich."""
        guild_config_service.set_auto_enrich(TEST_GUILD_ID, False)
        assert guild_config_service.is_auto_enrich_enabled(TEST_GUILD_ID) is False

    def test_set_auto_enrich_true(self, guild_config_service):
        """Test enabling auto-enrich after disabling."""
        guild_config_service.set_auto_enrich(TEST_GUILD_ID, False)
        guild_config_service.set_auto_enrich(TEST_GUILD_ID, True)
        assert guild_config_service.is_auto_enrich_enabled(TEST_GUILD_ID) is True

    def test_ai_enabled_defaults_to_config_value(self, guild_config_service):
        """Test that AI enabled defaults to the global config value."""
        # The default depends on AI_FEATURES_ENABLED config
        # We just verify it returns a boolean
        result = guild_config_service.is_ai_enabled(TEST_GUILD_ID)
        assert isinstance(result, bool)

    def test_set_ai_enabled(self, guild_config_service):
        """Test enabling AI features."""
        guild_config_service.set_ai_enabled(TEST_GUILD_ID, True)
        assert guild_config_service.is_ai_enabled(TEST_GUILD_ID) is True

        guild_config_service.set_ai_enabled(TEST_GUILD_ID, False)
        assert guild_config_service.is_ai_enabled(TEST_GUILD_ID) is False

    def test_null_guild_id_normalized_to_zero(self, guild_config_service):
        """Test that None guild_id is normalized to 0."""
        guild_config_service.set_league_id(None, 99999)
        result = guild_config_service.get_league_id(None)
        assert result == 99999

        # Also verify that 0 returns the same value
        result_zero = guild_config_service.get_league_id(0)
        assert result_zero == 99999

    def test_get_config_after_setting_values(self, guild_config_service):
        """Test that get_config returns full config after values are set."""
        guild_config_service.set_league_id(TEST_GUILD_ID, 55555)

        config = guild_config_service.get_config(TEST_GUILD_ID)

        assert config is not None
        assert config["guild_id"] == TEST_GUILD_ID
        assert config["league_id"] == 55555


class TestGuildConfigServiceMultiGuild:
    """Test multi-guild isolation in GuildConfigService."""

    def test_settings_isolated_between_guilds(self, guild_config_service):
        """Test that all settings are isolated between guilds."""
        # Configure guild 1
        guild_config_service.set_league_id(TEST_GUILD_ID, 111)
        guild_config_service.set_auto_enrich(TEST_GUILD_ID, False)
        guild_config_service.set_ai_enabled(TEST_GUILD_ID, True)

        # Configure guild 2 differently
        guild_config_service.set_league_id(TEST_GUILD_ID_SECONDARY, 222)
        guild_config_service.set_auto_enrich(TEST_GUILD_ID_SECONDARY, True)
        guild_config_service.set_ai_enabled(TEST_GUILD_ID_SECONDARY, False)

        # Verify guild 1
        assert guild_config_service.get_league_id(TEST_GUILD_ID) == 111
        assert guild_config_service.is_auto_enrich_enabled(TEST_GUILD_ID) is False
        assert guild_config_service.is_ai_enabled(TEST_GUILD_ID) is True

        # Verify guild 2
        assert guild_config_service.get_league_id(TEST_GUILD_ID_SECONDARY) == 222
        assert guild_config_service.is_auto_enrich_enabled(TEST_GUILD_ID_SECONDARY) is True
        assert guild_config_service.is_ai_enabled(TEST_GUILD_ID_SECONDARY) is False


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
