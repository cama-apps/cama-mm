"""
Tests for PackageDealRepository.
"""


import threading
from concurrent.futures import ThreadPoolExecutor

import pytest

from repositories.package_deal_repository import PackageDealRepository


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

    def test_create_deal_caps_duration_at_ten_games(self, repo):
        deal = repo.create_or_extend_deal(
            guild_id=123,
            buyer_id=100,
            partner_id=200,
            games=25,
            cost=500,
        )

        assert deal.games_remaining == 10
        assert deal.games_added == 10

        purchases = repo.get_purchases_involving_player(123, 100, 0, 2**63 - 1)
        assert [purchase.games_committed for purchase in purchases] == [10]

    def test_extend_deal_tops_up_to_ten_games(self, repo):
        # Create initial deal
        deal1 = repo.create_or_extend_deal(
            guild_id=123,
            buyer_id=100,
            partner_id=200,
            games=7,
            cost=500,
        )
        assert deal1.games_remaining == 7
        assert deal1.cost_paid == 500

        # Extend the deal
        deal2 = repo.create_or_extend_deal(
            guild_id=123,
            buyer_id=100,
            partner_id=200,
            games=10,
            cost=600,
        )

        # The extension only commits the three games that fit under the cap.
        assert deal2.id == deal1.id
        assert deal2.games_remaining == 10
        assert deal2.games_added == 3
        assert deal2.cost_paid == 1100  # 500 + 600

        purchases = repo.get_purchases_involving_player(123, 100, 0, 2**63 - 1)
        assert sorted(purchase.games_committed for purchase in purchases) == [3, 7]

    def test_paid_purchase_at_cap_does_not_charge_or_record_purchase(
        self,
        repo,
        player_repository,
    ):
        guild_id = 123
        buyer_id = 100
        player_repository.add(buyer_id, "Buyer", guild_id)
        player_repository.update_balance(buyer_id, guild_id, 1000)
        repo.create_or_extend_deal(
            guild_id=guild_id,
            buyer_id=buyer_id,
            partner_id=200,
            games=10,
            cost=1,
        )

        result = repo.purchase_deal(
            guild_id=guild_id,
            buyer_id=buyer_id,
            partner_id=200,
            games=10,
            no_active_cost=1,
            active_cost=800,
        )

        assert result.success is False
        assert result.reason == "already_full"
        assert result.balance == 1000
        assert result.deal is not None
        assert result.deal.games_remaining == 10
        assert result.deal.games_added == 0
        assert player_repository.get_balance(buyer_id, guild_id) == 1000
        purchases = repo.get_purchases_involving_player(guild_id, buyer_id, 0, 2**63 - 1)
        assert [(purchase.jc_spent, purchase.games_committed) for purchase in purchases] == [
            (1, 10)
        ]

    def test_concurrent_paid_top_ups_charge_once_and_stop_at_ten(
        self,
        repo,
        player_repository,
    ):
        guild_id = 123
        buyer_id = 100
        player_repository.add(buyer_id, "Buyer", guild_id)
        player_repository.update_balance(buyer_id, guild_id, 1000)
        repo.create_or_extend_deal(
            guild_id=guild_id,
            buyer_id=buyer_id,
            partner_id=200,
            games=7,
            cost=0,
        )
        start = threading.Barrier(2)

        def purchase():
            start.wait()
            return repo.purchase_deal(
                guild_id=guild_id,
                buyer_id=buyer_id,
                partner_id=200,
                games=3,
                no_active_cost=1,
                active_cost=100,
            )

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _index: purchase(), range(2)))

        assert sorted(result.reason or "success" for result in results) == [
            "already_full",
            "success",
        ]
        assert player_repository.get_balance(buyer_id, guild_id) == 900
        deals = repo.get_user_deals(guild_id, buyer_id)
        assert len(deals) == 1
        assert deals[0].games_remaining == 10
        purchases = repo.get_purchases_involving_player(guild_id, buyer_id, 0, 2**63 - 1)
        assert sorted(
            (purchase.jc_spent, purchase.games_committed) for purchase in purchases
        ) == [(0, 7), (100, 3)]

    def test_concurrent_first_purchases_only_charge_introductory_price_once(
        self,
        repo,
        player_repository,
    ):
        guild_id = 123
        buyer_id = 100
        player_repository.add(buyer_id, "Buyer", guild_id)
        player_repository.update_balance(buyer_id, guild_id, 2000)
        start = threading.Barrier(2)

        def purchase(partner_id):
            start.wait()
            return repo.purchase_deal(
                guild_id=guild_id,
                buyer_id=buyer_id,
                partner_id=partner_id,
                games=10,
                no_active_cost=1,
                active_cost=800,
            )

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(purchase, [200, 300]))

        assert all(result.success for result in results)
        assert sorted(result.cost for result in results) == [1, 800]
        assert player_repository.get_balance(buyer_id, guild_id) == 1199
        deals = repo.get_user_deals(guild_id, buyer_id)
        assert len(deals) == 2
        assert {deal.partner_discord_id for deal in deals} == {200, 300}
        assert all(deal.games_remaining <= 10 for deal in deals)

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
        repo.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=300, games=5)

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

    def test_purchase_log_survives_consumption_and_deletion(self, repo):
        """The immutable purchase log records each purchase and survives the
        decrement-to-zero + delete_expired_deals cleanup that drops the active
        deal row (the year-in-review undercount bug)."""
        deal = repo.create_or_extend_deal(
            guild_id=123, buyer_id=100, partner_id=200, games=3, cost=90
        )

        # Consume fully, then run cleanup that DELETEs the active deal.
        for _ in range(3):
            repo.decrement_deals(123, [deal.id])
        repo.delete_expired_deals(123)
        assert repo.get_deals_involving_player(123, 100) == []

        # The purchase log still has the original purchase (initial games=3).
        purchases = repo.get_purchases_involving_player(123, 100, 0, 2**63 - 1)
        assert len(purchases) == 1
        assert purchases[0].buyer_discord_id == 100
        assert purchases[0].partner_discord_id == 200
        assert purchases[0].games_committed == 3
        assert purchases[0].jc_spent == 90

    def test_purchase_log_time_window_filter(self, repo):
        """get_purchases_involving_player respects the [start, end) window."""
        deal = repo.create_or_extend_deal(
            guild_id=123, buyer_id=100, partner_id=200, games=5, cost=40
        )
        created = deal.created_at

        # Inside the window (inclusive start, exclusive end).
        assert len(repo.get_purchases_involving_player(123, 100, created, created + 1)) == 1
        # Window entirely after the purchase -> excluded.
        assert repo.get_purchases_involving_player(123, 100, created + 1, created + 10) == []
        # Window entirely before the purchase -> excluded (end is exclusive).
        assert repo.get_purchases_involving_player(123, 100, created - 10, created) == []
