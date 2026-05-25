"""Region preference persistence, guild isolation, and the backfill query."""

from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY


class TestRegionPersistence:
    """preferred_region / inferred_region round-trip through PlayerRepository."""

    def test_update_and_read_preferred_region(self, player_repository):
        """An explicit region pick round-trips onto the Player model."""
        player_repository.add(discord_id=1, discord_username="P1", guild_id=TEST_GUILD_ID)
        player_repository.update_preferred_region(1, TEST_GUILD_ID, "USW")

        player = player_repository.get_by_id(1, TEST_GUILD_ID)
        assert player.preferred_region == "USW"
        assert player.inferred_region is None

    def test_inferred_region_and_sentinel_round_trip(self, player_repository):
        """A cached inferred region — including the NONE sentinel — round-trips."""
        player_repository.add(discord_id=2, discord_username="P2", guild_id=TEST_GUILD_ID)

        player_repository.update_inferred_region(2, TEST_GUILD_ID, "USE")
        assert player_repository.get_by_id(2, TEST_GUILD_ID).inferred_region == "USE"

        player_repository.update_inferred_region(2, TEST_GUILD_ID, "NONE")
        assert player_repository.get_by_id(2, TEST_GUILD_ID).inferred_region == "NONE"

    def test_region_is_guild_scoped(self, player_repository):
        """A user's region in one guild does not leak into another guild's row."""
        player_repository.add(discord_id=3, discord_username="P3", guild_id=TEST_GUILD_ID)
        player_repository.add(discord_id=3, discord_username="P3", guild_id=TEST_GUILD_ID_SECONDARY)

        player_repository.update_preferred_region(3, TEST_GUILD_ID, "USE")

        assert player_repository.get_by_id(3, TEST_GUILD_ID).preferred_region == "USE"
        assert player_repository.get_by_id(3, TEST_GUILD_ID_SECONDARY).preferred_region is None


class TestRegionBackfillQuery:
    """get_players_needing_region_backfill targets the right rows."""

    def test_returns_only_unchecked_players_with_steam_id(self, player_repository):
        """Only players with a Steam ID and no inferred_region yet are returned."""
        # Steam ID, region not yet checked -> included.
        player_repository.add(
            discord_id=10, discord_username="A", guild_id=TEST_GUILD_ID, steam_id=111
        )
        # Steam ID, already inferred -> excluded (so backfill converges).
        player_repository.add(
            discord_id=11, discord_username="B", guild_id=TEST_GUILD_ID, steam_id=222
        )
        player_repository.update_inferred_region(11, TEST_GUILD_ID, "USE")
        # No Steam ID -> excluded (nothing to query OpenDota with).
        player_repository.add(discord_id=12, discord_username="C", guild_id=TEST_GUILD_ID)
        # Fake user -> excluded.
        player_repository.add(
            discord_id=-5, discord_username="Fake", guild_id=TEST_GUILD_ID, steam_id=333
        )

        rows = {r["discord_id"]: r for r in player_repository.get_players_needing_region_backfill()}

        assert 10 in rows and rows[10]["steam_id"] == 111
        assert rows[10]["guild_id"] == TEST_GUILD_ID
        assert 11 not in rows
        assert 12 not in rows
        assert -5 not in rows
