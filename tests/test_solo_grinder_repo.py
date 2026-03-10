"""Tests for solo grinder repository operations."""

import pytest

from tests.conftest import TEST_GUILD_ID


class TestSoloGrinderRepository:
    """Test persisting and reading back solo grinder fields."""

    def test_default_values(self, player_repository):
        """New players should default to is_solo_grinder=False."""
        player_repository.add(
            discord_id=1001,
            discord_username="TestPlayer",
            guild_id=TEST_GUILD_ID,
        )
        player = player_repository.get_by_id(1001, TEST_GUILD_ID)
        assert player is not None
        assert player.is_solo_grinder is False
        assert player.solo_grinder_checked_at is None

    def test_update_solo_grinder_status(self, player_repository):
        """Should persist and read back grinder status."""
        player_repository.add(
            discord_id=1002,
            discord_username="GrinderPlayer",
            guild_id=TEST_GUILD_ID,
        )

        player_repository.update_solo_grinder_status(
            discord_id=1002,
            guild_id=TEST_GUILD_ID,
            is_grinder=True,
            checked_at="2026-03-09T12:00:00",
        )

        player = player_repository.get_by_id(1002, TEST_GUILD_ID)
        assert player is not None
        assert player.is_solo_grinder is True
        assert player.solo_grinder_checked_at == "2026-03-09T12:00:00"

    def test_update_grinder_to_casual(self, player_repository):
        """Should be able to flip grinder back to casual."""
        player_repository.add(
            discord_id=1003,
            discord_username="FlipPlayer",
            guild_id=TEST_GUILD_ID,
        )

        # Set as grinder
        player_repository.update_solo_grinder_status(1003, TEST_GUILD_ID, True, "2026-03-01T00:00:00")
        player = player_repository.get_by_id(1003, TEST_GUILD_ID)
        assert player.is_solo_grinder is True

        # Flip to casual
        player_repository.update_solo_grinder_status(1003, TEST_GUILD_ID, False, "2026-03-09T00:00:00")
        player = player_repository.get_by_id(1003, TEST_GUILD_ID)
        assert player.is_solo_grinder is False
        assert player.solo_grinder_checked_at == "2026-03-09T00:00:00"

    def test_grinder_status_guild_isolation(self, player_repository):
        """Grinder status is per-guild."""
        from tests.conftest import TEST_GUILD_ID_SECONDARY

        player_repository.add(discord_id=1004, discord_username="MultiGuild", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=1004, discord_username="MultiGuild", guild_id=TEST_GUILD_ID_SECONDARY)

        player_repository.update_solo_grinder_status(1004, TEST_GUILD_ID, True, "2026-03-09T00:00:00")

        player_g1 = player_repository.get_by_id(1004, TEST_GUILD_ID)
        player_g2 = player_repository.get_by_id(1004, TEST_GUILD_ID_SECONDARY)

        assert player_g1.is_solo_grinder is True
        assert player_g2.is_solo_grinder is False
