"""
Tests for SoftAvoidRepository.
"""

import sqlite3
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest

from repositories.soft_avoid_repository import SoftAvoidRepository


@pytest.fixture
def soft_avoid_repo(repo_db_path):
    """Create a SoftAvoidRepository with temp database."""
    return SoftAvoidRepository(repo_db_path)


class TestSoftAvoidRepository:
    """Tests for SoftAvoidRepository methods."""

    def test_create_avoid(self, soft_avoid_repo):
        """Test creating a new soft avoid."""
        avoid = soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=123,
            avoider_id=100,
            avoided_id=200,
            games=10,
        )

        assert avoid is not None
        assert avoid.avoider_discord_id == 100
        assert avoid.avoided_discord_id == 200
        assert avoid.games_remaining == 10
        assert avoid.guild_id == 123

    def test_active_avoid_cannot_be_extended(self, soft_avoid_repo):
        """An active avoid cannot be repurchased to stack its duration."""
        avoid1 = soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=123,
            avoider_id=100,
            avoided_id=200,
            games=10,
        )
        assert avoid1.games_remaining == 10

        with pytest.raises(ValueError, match="already active"):
            soft_avoid_repo.create_or_reactivate_avoid(
                guild_id=123,
                avoider_id=100,
                avoided_id=200,
                games=10,
            )

        active = soft_avoid_repo.get_user_avoids(guild_id=123, discord_id=100)
        assert active[0].games_remaining == 10

    def test_expired_avoid_can_be_reactivated(self, soft_avoid_repo):
        avoid1 = soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=123,
            avoider_id=100,
            avoided_id=200,
            games=1,
        )
        soft_avoid_repo.decrement_avoids(guild_id=123, avoid_ids=[avoid1.id])

        avoid2 = soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=123,
            avoider_id=100,
            avoided_id=200,
            games=10,
        )

        assert avoid2.id == avoid1.id
        assert avoid2.games_remaining == 10

    @pytest.mark.parametrize("games", [-1, 0, 11])
    def test_duration_must_be_between_one_and_ten_games(self, soft_avoid_repo, games):
        with pytest.raises(ValueError, match="between 1 and 10 games"):
            soft_avoid_repo.create_or_reactivate_avoid(
                guild_id=123,
                avoider_id=100,
                avoided_id=200,
                games=games,
            )

    @pytest.mark.parametrize("games", [-1, 0, 11])
    def test_purchase_duration_must_be_between_one_and_ten_games(
        self,
        soft_avoid_repo,
        games,
    ):
        with pytest.raises(ValueError, match="between 1 and 10 games"):
            soft_avoid_repo.purchase_avoid(
                guild_id=123,
                avoider_id=100,
                avoided_id=200,
                cost=250,
                games=games,
            )

    def test_purchase_avoid_debits_once_and_writes_one_ledger_entry(
        self,
        soft_avoid_repo,
        player_repository,
    ):
        guild_id = 123
        avoider_id = 100
        player_repository.add(avoider_id, "Buyer", guild_id)
        player_repository.update_balance(avoider_id, guild_id, 1000)

        result = soft_avoid_repo.purchase_avoid(
            guild_id=guild_id,
            avoider_id=avoider_id,
            avoided_id=200,
            cost=250,
            games=10,
        )

        assert result.success is True
        assert result.reason is None
        assert result.balance == 750
        assert result.avoid is not None
        assert result.avoid.games_remaining == 10
        assert player_repository.get_balance(avoider_id, guild_id) == 750
        assert player_repository.get_lowest_balance(avoider_id, guild_id) == 750

        with soft_avoid_repo.connection() as conn:
            ledger_rows = conn.execute(
                """
                SELECT delta, source, related_type, related_id
                FROM economy_ledger_entries
                WHERE guild_id = ? AND account_id = ? AND source = 'soft_avoid'
                ORDER BY ledger_id
                """,
                (guild_id, str(avoider_id)),
            ).fetchall()

        assert [dict(row) for row in ledger_rows] == [
            {
                "delta": -250,
                "source": "soft_avoid",
                "related_type": "player",
                "related_id": "200",
            }
        ]

    def test_activation_failure_rolls_back_debit_and_ledger(
        self,
        soft_avoid_repo,
        player_repository,
    ):
        guild_id = 123
        avoider_id = 100
        player_repository.add(avoider_id, "Buyer", guild_id)
        player_repository.update_balance(avoider_id, guild_id, 1000)
        with soft_avoid_repo.connection() as conn:
            conn.execute(
                """
                CREATE TRIGGER fail_soft_avoid_activation
                BEFORE INSERT ON soft_avoids
                BEGIN
                    SELECT RAISE(ABORT, 'forced activation failure');
                END
                """
            )

        with pytest.raises(sqlite3.IntegrityError, match="forced activation failure"):
            soft_avoid_repo.purchase_avoid(
                guild_id=guild_id,
                avoider_id=avoider_id,
                avoided_id=200,
                cost=250,
                games=10,
            )

        assert player_repository.get_balance(avoider_id, guild_id) == 1000
        assert player_repository.get_lowest_balance(avoider_id, guild_id) == 1000
        assert soft_avoid_repo.get_user_avoids(guild_id, avoider_id) == []
        with soft_avoid_repo.connection() as conn:
            ledger_count = conn.execute(
                """
                SELECT COUNT(*)
                FROM economy_ledger_entries
                WHERE guild_id = ? AND account_id = ? AND source = 'soft_avoid'
                """,
                (guild_id, str(avoider_id)),
            ).fetchone()[0]
        assert ledger_count == 0

    def test_concurrent_duplicate_purchase_has_one_charge_and_one_activation(
        self,
        soft_avoid_repo,
        player_repository,
    ):
        guild_id = 123
        avoider_id = 100
        player_repository.add(avoider_id, "Buyer", guild_id)
        player_repository.update_balance(avoider_id, guild_id, 1000)
        start = threading.Barrier(2)

        def purchase():
            start.wait()
            return soft_avoid_repo.purchase_avoid(
                guild_id=guild_id,
                avoider_id=avoider_id,
                avoided_id=200,
                cost=250,
                games=10,
            )

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _index: purchase(), range(2)))

        assert sorted(result.reason or "success" for result in results) == [
            "already_active",
            "success",
        ]
        assert player_repository.get_balance(avoider_id, guild_id) == 750
        assert player_repository.get_lowest_balance(avoider_id, guild_id) == 750
        active = soft_avoid_repo.get_user_avoids(guild_id, avoider_id)
        assert len(active) == 1
        assert active[0].games_remaining == 10

        with soft_avoid_repo.connection() as conn:
            ledger_count = conn.execute(
                """
                SELECT COUNT(*)
                FROM economy_ledger_entries
                WHERE guild_id = ? AND account_id = ? AND source = 'soft_avoid'
                """,
                (guild_id, str(avoider_id)),
            ).fetchone()[0]

        assert ledger_count == 1

    def test_insufficient_balance_does_not_activate_or_touch_lowest_balance(
        self,
        soft_avoid_repo,
        player_repository,
    ):
        guild_id = 123
        avoider_id = 100
        player_repository.add(avoider_id, "Buyer", guild_id)
        player_repository.update_balance(avoider_id, guild_id, 249)

        result = soft_avoid_repo.purchase_avoid(
            guild_id=guild_id,
            avoider_id=avoider_id,
            avoided_id=200,
            cost=250,
            games=10,
        )

        assert result.success is False
        assert result.reason == "insufficient_balance"
        assert result.balance == 249
        assert result.avoid is None
        assert player_repository.get_balance(avoider_id, guild_id) == 249
        assert player_repository.get_lowest_balance(avoider_id, guild_id) == 249
        assert soft_avoid_repo.get_user_avoids(guild_id, avoider_id) == []

    def test_get_active_avoids_for_players(self, soft_avoid_repo):
        """Test getting active avoids where both players are in the list."""
        # Create avoids
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=10)
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=200, avoided_id=300, games=5)
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=400, avoided_id=500, games=10)

        # Get avoids for players 100, 200, 300
        player_ids = [100, 200, 300]
        avoids = soft_avoid_repo.get_active_avoids_for_players(guild_id=123, player_ids=player_ids)

        # Should get both avoids involving these players
        assert len(avoids) == 2
        avoider_ids = {a.avoider_discord_id for a in avoids}
        assert 100 in avoider_ids
        assert 200 in avoider_ids

    def test_get_active_avoids_excludes_zero_remaining(self, soft_avoid_repo):
        """Test that avoids with 0 games remaining are excluded."""
        avoid = soft_avoid_repo.create_or_reactivate_avoid(
            guild_id=123,
            avoider_id=100,
            avoided_id=200,
            games=1,
        )

        # Decrement to 0
        soft_avoid_repo.decrement_avoids(guild_id=123, avoid_ids=[avoid.id])

        # Should not appear in active avoids
        avoids = soft_avoid_repo.get_active_avoids_for_players(guild_id=123, player_ids=[100, 200])
        assert len(avoids) == 0

    def test_get_user_avoids(self, soft_avoid_repo):
        """Test getting a user's active avoids."""
        # Create avoids where user 100 is the avoider
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=10)
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=300, games=5)
        # Create an avoid where user 100 is the avoided (not the avoider)
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=400, avoided_id=100, games=10)

        avoids = soft_avoid_repo.get_user_avoids(guild_id=123, discord_id=100)

        # Should only get avoids where user 100 is the avoider
        assert len(avoids) == 2
        for avoid in avoids:
            assert avoid.avoider_discord_id == 100

    def test_decrement_avoids(self, soft_avoid_repo):
        """Test decrementing avoids."""
        avoid1 = soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=10)
        avoid2 = soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=300, avoided_id=400, games=5)

        count = soft_avoid_repo.decrement_avoids(guild_id=123, avoid_ids=[avoid1.id, avoid2.id])

        assert count == 2

        # Verify games decreased
        avoids = soft_avoid_repo.get_active_avoids_for_players(guild_id=123, player_ids=[100, 200, 300, 400])
        remaining = {a.id: a.games_remaining for a in avoids}
        assert remaining[avoid1.id] == 9
        assert remaining[avoid2.id] == 4

    def test_decrement_stops_at_zero(self, soft_avoid_repo):
        """Test that decrement stops at 0."""
        avoid = soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=1)

        # First decrement
        soft_avoid_repo.decrement_avoids(guild_id=123, avoid_ids=[avoid.id])

        # Second decrement should not go below 0
        count = soft_avoid_repo.decrement_avoids(guild_id=123, avoid_ids=[avoid.id])
        assert count == 0  # No rows updated (games_remaining must be > 0)

    def test_delete_expired_avoids(self, soft_avoid_repo):
        """Test deleting expired avoids."""
        avoid1 = soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=1)
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=300, avoided_id=400, games=10)

        # Decrement first avoid to 0
        soft_avoid_repo.decrement_avoids(guild_id=123, avoid_ids=[avoid1.id])

        # Delete expired
        deleted = soft_avoid_repo.delete_expired_avoids(guild_id=123)
        assert deleted == 1

        # Only one avoid should remain
        avoids = soft_avoid_repo.get_active_avoids_for_players(guild_id=123, player_ids=[100, 200, 300, 400])
        assert len(avoids) == 1
        assert avoids[0].avoider_discord_id == 300

    def test_guild_isolation(self, soft_avoid_repo):
        """Test that avoids are isolated by guild."""
        # Create avoid in guild 123
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=10)

        # Create avoid in guild 456
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=456, avoider_id=100, avoided_id=200, games=5)

        # Get avoids for guild 123
        avoids_123 = soft_avoid_repo.get_active_avoids_for_players(guild_id=123, player_ids=[100, 200])
        assert len(avoids_123) == 1
        assert avoids_123[0].games_remaining == 10

        # Get avoids for guild 456
        avoids_456 = soft_avoid_repo.get_active_avoids_for_players(guild_id=456, player_ids=[100, 200])
        assert len(avoids_456) == 1
        assert avoids_456[0].games_remaining == 5

    def test_empty_player_list_returns_empty(self, soft_avoid_repo):
        """Test that empty player list returns empty results."""
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=10)

        avoids = soft_avoid_repo.get_active_avoids_for_players(guild_id=123, player_ids=[])
        assert len(avoids) == 0

    def test_no_matching_avoids_returns_empty(self, soft_avoid_repo):
        """Test that no matching avoids returns empty results."""
        soft_avoid_repo.create_or_reactivate_avoid(guild_id=123, avoider_id=100, avoided_id=200, games=10)

        # Query with players not in the avoid
        avoids = soft_avoid_repo.get_active_avoids_for_players(guild_id=123, player_ids=[300, 400])
        assert len(avoids) == 0

    def test_self_avoid_raises_error(self, soft_avoid_repo):
        """Test that attempting to avoid oneself raises ValueError."""
        with pytest.raises(ValueError) as exc_info:
            soft_avoid_repo.create_or_reactivate_avoid(
                guild_id=123,
                avoider_id=100,
                avoided_id=100,  # Same as avoider
                games=10,
            )
        assert "same player" in str(exc_info.value).lower()
