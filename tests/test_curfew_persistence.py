"""Curfew settings persistence and guild isolation (PlayerRepository.update_curfew)."""

from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


class TestCurfewPersistence:
    def test_defaults_are_disabled_and_unset(self, player_repository):
        player_repository.add(discord_id=1, discord_username="P1", guild_id=TEST_GUILD_ID)
        player = player_repository.get_by_id(1, TEST_GUILD_ID)

        assert player.curfew_enabled is False
        assert player.curfew_hour is None
        assert player.curfew_wake_hour is None
        assert player.curfew_timezone is None

    def test_update_and_read_curfew(self, player_repository):
        player_repository.add(discord_id=2, discord_username="P2", guild_id=TEST_GUILD_ID)
        player_repository.update_curfew(
            2,
            TEST_GUILD_ID,
            enabled=True,
            curfew_hour=22,
            curfew_minute=30,
            wake_hour=6,
            wake_minute=15,
            timezone="America/New_York",
        )

        player = player_repository.get_by_id(2, TEST_GUILD_ID)
        assert player.curfew_enabled is True
        assert player.curfew_hour == 22
        assert player.curfew_minute == 30
        assert player.curfew_wake_hour == 6
        assert player.curfew_wake_minute == 15
        assert player.curfew_timezone == "America/New_York"

    def test_disabling_preserves_configured_hours(self, player_repository):
        player_repository.add(discord_id=3, discord_username="P3", guild_id=TEST_GUILD_ID)
        player_repository.update_curfew(
            3,
            TEST_GUILD_ID,
            enabled=True,
            curfew_hour=22,
            curfew_minute=0,
            wake_hour=6,
            wake_minute=0,
            timezone="America/New_York",
        )
        player_repository.update_curfew(
            3,
            TEST_GUILD_ID,
            enabled=False,
            curfew_hour=22,
            curfew_minute=0,
            wake_hour=6,
            wake_minute=0,
            timezone="America/New_York",
        )

        player = player_repository.get_by_id(3, TEST_GUILD_ID)
        assert player.curfew_enabled is False
        assert player.curfew_hour == 22

    def test_curfew_is_guild_scoped(self, player_repository):
        player_repository.add(discord_id=4, discord_username="P4", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=4, discord_username="P4", guild_id=TEST_GUILD_ID_SECONDARY)

        player_repository.update_curfew(
            4,
            TEST_GUILD_ID,
            enabled=True,
            curfew_hour=22,
            curfew_minute=0,
            wake_hour=6,
            wake_minute=0,
            timezone="America/New_York",
        )

        assert player_repository.get_by_id(4, TEST_GUILD_ID).curfew_enabled is True
        assert player_repository.get_by_id(4, TEST_GUILD_ID_SECONDARY).curfew_enabled is False
