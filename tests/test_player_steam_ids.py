"""
Tests for multi-Steam ID functionality.
"""

import pytest


class TestPlayerSteamIds:
    """Tests for multiple Steam IDs per player."""

    def test_add_steam_id(self, player_repository):
        """Test adding a Steam ID to a player."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)

        steam_ids = player_repository.get_steam_ids(12345)
        assert steam_ids == [100001]
        assert player_repository.get_primary_steam_id(12345) == 100001

    def test_add_multiple_steam_ids(self, player_repository):
        """Test adding multiple Steam IDs to a player."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)
        player_repository.add_steam_id(12345, 100002, is_primary=False)
        player_repository.add_steam_id(12345, 100003, is_primary=False)

        steam_ids = player_repository.get_steam_ids(12345)
        assert len(steam_ids) == 3
        assert steam_ids[0] == 100001  # Primary first
        assert 100002 in steam_ids
        assert 100003 in steam_ids

    def test_steam_id_uniqueness_across_players(self, player_repository):
        """Test that a Steam ID can only belong to one player."""
        player_repository.add(discord_id=12345, discord_username="Player1")
        player_repository.add(discord_id=67890, discord_username="Player2")

        player_repository.add_steam_id(12345, 100001, is_primary=True)

        with pytest.raises(ValueError, match="already linked"):
            player_repository.add_steam_id(67890, 100001, is_primary=True)

    def test_get_player_by_any_steam_id(self, player_repository):
        """Test finding a player by any of their Steam IDs."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)
        player_repository.add_steam_id(12345, 100002, is_primary=False)

        # Find by primary
        player1 = player_repository.get_player_by_any_steam_id(100001)
        assert player1 is not None
        assert player1.discord_id == 12345

        # Find by secondary
        player2 = player_repository.get_player_by_any_steam_id(100002)
        assert player2 is not None
        assert player2.discord_id == 12345

        # Not found
        player3 = player_repository.get_player_by_any_steam_id(999999)
        assert player3 is None

    def test_get_by_steam_id_uses_junction_table(self, player_repository):
        """Test that get_by_steam_id finds players via junction table."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        # Add via junction table only (not legacy column)
        player_repository.add_steam_id(12345, 100001, is_primary=True)

        player = player_repository.get_by_steam_id(100001)
        assert player is not None
        assert player.discord_id == 12345

    def test_set_primary_steam_id(self, player_repository):
        """Test changing the primary Steam ID."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)
        player_repository.add_steam_id(12345, 100002, is_primary=False)

        assert player_repository.get_primary_steam_id(12345) == 100001

        # Change primary
        result = player_repository.set_primary_steam_id(12345, 100002)
        assert result is True
        assert player_repository.get_primary_steam_id(12345) == 100002

        # Verify order in get_steam_ids (primary first)
        steam_ids = player_repository.get_steam_ids(12345)
        assert steam_ids[0] == 100002

    def test_set_primary_steam_id_not_linked(self, player_repository):
        """Test setting primary to an unlinked Steam ID fails."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)

        result = player_repository.set_primary_steam_id(12345, 999999)
        assert result is False

    def test_remove_steam_id(self, player_repository):
        """Test removing a Steam ID."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)
        player_repository.add_steam_id(12345, 100002, is_primary=False)

        removed = player_repository.remove_steam_id(12345, 100002)
        assert removed is True

        steam_ids = player_repository.get_steam_ids(12345)
        assert steam_ids == [100001]

    def test_remove_primary_promotes_next(self, player_repository):
        """Test that removing primary Steam ID promotes another."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)
        player_repository.add_steam_id(12345, 100002, is_primary=False)

        removed = player_repository.remove_steam_id(12345, 100001)
        assert removed is True

        # 100002 should now be primary
        assert player_repository.get_primary_steam_id(12345) == 100002
        steam_ids = player_repository.get_steam_ids(12345)
        assert steam_ids == [100002]

    def test_remove_last_steam_id_clears_legacy(self, player_repository):
        """Test that removing the last Steam ID clears legacy column."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)

        removed = player_repository.remove_steam_id(12345, 100001)
        assert removed is True

        assert player_repository.get_steam_ids(12345) == []
        assert player_repository.get_steam_id(12345) is None

    def test_remove_nonexistent_steam_id(self, player_repository):
        """Test removing a Steam ID that doesn't exist."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        removed = player_repository.remove_steam_id(12345, 999999)
        assert removed is False

    def test_get_steam_ids_bulk(self, player_repository):
        """Test bulk lookup of Steam IDs."""
        # Create players with various Steam ID configurations
        player_repository.add(discord_id=1, discord_username="Player1")
        player_repository.add(discord_id=2, discord_username="Player2")
        player_repository.add(discord_id=3, discord_username="Player3")

        # Player 1: Multiple Steam IDs
        player_repository.add_steam_id(1, 100001, is_primary=True)
        player_repository.add_steam_id(1, 100002, is_primary=False)

        # Player 2: Single Steam ID
        player_repository.add_steam_id(2, 200001, is_primary=True)

        # Player 3: No Steam IDs

        result = player_repository.get_steam_ids_bulk([1, 2, 3])

        assert len(result[1]) == 2
        assert result[1][0] == 100001  # Primary first
        assert 100002 in result[1]

        assert result[2] == [200001]
        assert result[3] == []

    def test_set_steam_id_updates_junction_table(self, player_repository):
        """Test that set_steam_id adds to junction table as primary."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        # Use set_steam_id (the old method)
        player_repository.set_steam_id(12345, 100001)

        # Should be in junction table as primary
        steam_ids = player_repository.get_steam_ids(12345)
        assert steam_ids == [100001]
        assert player_repository.get_primary_steam_id(12345) == 100001

    def test_set_steam_id_changes_primary(self, player_repository):
        """Test that set_steam_id changes the primary if called again."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        player_repository.add_steam_id(12345, 100001, is_primary=True)
        player_repository.set_steam_id(12345, 100002)  # Should become primary

        assert player_repository.get_primary_steam_id(12345) == 100002

        # Both should still be linked
        steam_ids = player_repository.get_steam_ids(12345)
        assert len(steam_ids) == 2
        assert steam_ids[0] == 100002  # Primary first

    def test_legacy_fallback_get_steam_id(self, player_repository):
        """Test that get_steam_id falls back to legacy column."""
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )

        # Directly set legacy column (simulating old data)
        import sqlite3
        with sqlite3.connect(player_repository.db_path) as conn:
            conn.execute(
                "UPDATE players SET steam_id = ? WHERE discord_id = ?",
                (100001, 12345),
            )

        # Should find it via legacy fallback
        steam_id = player_repository.get_steam_id(12345)
        assert steam_id == 100001

    def test_migration_existing_steam_ids(self, player_repository):
        """Test that existing steam_ids in players table work with junction table."""
        # Add player with steam_id via legacy set_steam_id
        player_repository.add(
            discord_id=12345,
            discord_username="TestPlayer",
        )
        player_repository.set_steam_id(12345, 100001)

        # Add a secondary account
        player_repository.add_steam_id(12345, 100002, is_primary=False)

        # Both should be accessible
        steam_ids = player_repository.get_steam_ids(12345)
        assert len(steam_ids) == 2
        assert 100001 in steam_ids
        assert 100002 in steam_ids

        # Player should be findable by either
        p1 = player_repository.get_by_steam_id(100001)
        p2 = player_repository.get_by_steam_id(100002)
        assert p1.discord_id == 12345
        assert p2.discord_id == 12345
