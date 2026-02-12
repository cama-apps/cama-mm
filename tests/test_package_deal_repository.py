"""
Tests for PackageDealRepository.
"""

import pytest
import time
from repositories.package_deal_repository import PackageDealRepository, PackageDeal


class TestPackageDealRepository:
    """Tests for PackageDealRepository CRUD operations."""

    @pytest.fixture
    def repo(self, repo_db_path):
        """Create a repository with initialized schema."""
        return PackageDealRepository(repo_db_path)

    def test_create_deal(self, repo):
        """Test creating a new package deal."""
        deal = repo.create_or_extend_deal(
            guild_id=123,
            buyer_id=100,
            partner_id=200,
            games=10,
            cost=880,
        )

        assert deal.id is not None
        assert deal.guild_id == 123
        assert deal.buyer_discord_id == 100
        assert deal.partner_discord_id == 200
        assert deal.games_remaining == 10
        assert deal.cost_paid == 880
        assert deal.created_at > 0
        assert deal.updated_at > 0

    def test_extend_deal(self, repo):
        """Test extending an existing package deal."""
        # Create initial deal
        deal1 = repo.create_or_extend_deal(
            guild_id=123,
            buyer_id=100,
            partner_id=200,
            games=10,
            cost=500,
        )
        assert deal1.games_remaining == 10
        assert deal1.cost_paid == 500

        # Extend the deal
        deal2 = repo.create_or_extend_deal(
            guild_id=123,
            buyer_id=100,
            partner_id=200,
            games=10,
            cost=600,
        )

        # Should be same deal ID with extended games and accumulated cost
        assert deal2.id == deal1.id
        assert deal2.games_remaining == 20
        assert deal2.cost_paid == 1100  # 500 + 600

    def test_cannot_deal_with_self(self, repo):
        """Test that creating a deal with yourself raises ValueError."""
        with pytest.raises(ValueError, match="buyer and partner cannot be the same"):
            repo.create_or_extend_deal(
                guild_id=123,
                buyer_id=100,
                partner_id=100,
                games=10,
            )

    def test_get_active_deals_for_players(self, repo):
        """Test getting active deals for a set of players."""
        # Create deals
        repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10)
        repo.create_or_extend_deal(guild_id=123, buyer_id=300, partner_id=400, games=10)
        repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=500, games=10)

        # Get deals for players 100, 200, 300, 400, 500
        deals = repo.get_active_deals_for_players(123, [100, 200, 300, 400, 500])
        assert len(deals) == 3

        # Get deals for players 100, 200 only - should exclude 300->400
        deals = repo.get_active_deals_for_players(123, [100, 200])
        assert len(deals) == 1
        assert deals[0].buyer_discord_id == 100
        assert deals[0].partner_discord_id == 200

    def test_get_active_deals_excludes_expired(self, repo):
        """Test that expired deals (games_remaining=0) are excluded."""
        deal = repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=1)

        # Decrement to 0
        repo.decrement_deals(123, [deal.id])

        # Should not show up
        deals = repo.get_active_deals_for_players(123, [100, 200])
        assert len(deals) == 0

    def test_get_user_deals(self, repo):
        """Test getting all active deals for a user."""
        repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10)
        repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=300, games=10)
        repo.create_or_extend_deal(guild_id=123, buyer_id=200, partner_id=100, games=10)

        # User 100's deals (as buyer)
        deals = repo.get_user_deals(123, 100)
        assert len(deals) == 2
        assert all(d.buyer_discord_id == 100 for d in deals)

    def test_decrement_deals(self, repo):
        """Test decrementing deal games."""
        deal = repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=5)

        count = repo.decrement_deals(123, [deal.id])
        assert count == 1

        deals = repo.get_user_deals(123, 100)
        assert len(deals) == 1
        assert deals[0].games_remaining == 4

    def test_decrement_multiple_deals(self, repo):
        """Test decrementing multiple deals at once."""
        deal1 = repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=5)
        deal2 = repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=300, games=3)

        count = repo.decrement_deals(123, [deal1.id, deal2.id])
        assert count == 2

        deals = repo.get_user_deals(123, 100)
        assert len(deals) == 2
        games = {d.partner_discord_id: d.games_remaining for d in deals}
        assert games[200] == 4
        assert games[300] == 2

    def test_delete_expired_deals(self, repo):
        """Test deleting expired deals."""
        deal1 = repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=1)
        deal2 = repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=300, games=5)

        # Decrement deal1 to 0
        repo.decrement_deals(123, [deal1.id])

        # Delete expired
        deleted = repo.delete_expired_deals(123)
        assert deleted == 1

        # Only deal2 should remain
        deals = repo.get_user_deals(123, 100)
        assert len(deals) == 1
        assert deals[0].partner_discord_id == 300

    def test_guild_isolation(self, repo):
        """Test that deals are isolated by guild."""
        repo.create_or_extend_deal(guild_id=111, buyer_id=100, partner_id=200, games=10)
        repo.create_or_extend_deal(guild_id=222, buyer_id=100, partner_id=200, games=10)

        deals_111 = repo.get_user_deals(111, 100)
        deals_222 = repo.get_user_deals(222, 100)

        assert len(deals_111) == 1
        assert len(deals_222) == 1
        assert deals_111[0].id != deals_222[0].id

    def test_null_guild_normalized(self, repo):
        """Test that guild_id=None is normalized to 0."""
        deal = repo.create_or_extend_deal(
            guild_id=None,
            buyer_id=100,
            partner_id=200,
            games=10,
        )
        assert deal.guild_id == 0

        # Can retrieve with guild_id=0
        deals = repo.get_user_deals(0, 100)
        assert len(deals) == 1

        # Can also retrieve with guild_id=None
        deals = repo.get_user_deals(None, 100)
        assert len(deals) == 1
