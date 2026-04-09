"""
Tests for package deal scalar pricing: first deal free, subsequent deals paid.
"""

import pytest

from config import SHOP_PACKAGE_DEAL_BASE_COST, SHOP_PACKAGE_DEAL_RATING_DIVISOR
from repositories.package_deal_repository import PackageDealRepository
from services.package_deal_service import PackageDealService


class TestPackageDealPricing:
    """Tests for the free-first-deal pricing logic."""

    @pytest.fixture
    def service(self, repo_db_path):
        repo = PackageDealRepository(repo_db_path)
        return PackageDealService(repo)

    def _calculate_cost(self, active_deals, is_extend, buyer_rating=1500, partner_rating=1500):
        """Replicate the pricing logic from _handle_package_deal."""
        if len(active_deals) == 0 and not is_extend:
            return 0
        return SHOP_PACKAGE_DEAL_BASE_COST + int(
            (buyer_rating + partner_rating) / SHOP_PACKAGE_DEAL_RATING_DIVISOR
        )

    def test_first_deal_is_free(self, service):
        """With 0 active deals, first deal should be free."""
        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        assert len(active_deals) == 0

        cost = self._calculate_cost(active_deals, is_extend=False)
        assert cost == 0

    def test_second_deal_costs_normal(self, service):
        """With 1 active deal, next deal should cost the normal formula price."""
        # Create first deal
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10, cost=0)

        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        assert len(active_deals) == 1

        is_extend = any(d.partner_discord_id == 300 for d in active_deals)
        assert not is_extend

        cost = self._calculate_cost(active_deals, is_extend)
        expected = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
        assert cost == expected

    def test_extending_existing_deal_costs_normal(self, service):
        """Extending an existing deal should always cost normal price, even if it's the only deal."""
        # Create first deal (free)
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10, cost=0)

        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        is_extend = any(d.partner_discord_id == 200 for d in active_deals)
        assert is_extend

        cost = self._calculate_cost(active_deals, is_extend)
        expected = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
        assert cost == expected

    def test_all_deals_expired_resets_free(self, service):
        """When all deals expire (0 active), next deal should be free again."""
        # Create and expire a deal
        deal = service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=1, cost=0)
        service.decrement_deals(guild_id=123, deal_ids=[deal.id])

        # All deals expired — active count should be 0
        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        assert len(active_deals) == 0

        cost = self._calculate_cost(active_deals, is_extend=False)
        assert cost == 0

    def test_one_expired_one_active_still_paid(self, service):
        """If one deal expired but another is active, next deal costs normal."""
        # Create two deals
        deal1 = service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=1, cost=0)
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=300, games=10, cost=500)

        # Expire the first one
        service.decrement_deals(guild_id=123, deal_ids=[deal1.id])

        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        assert len(active_deals) == 1  # Only deal with partner 300

        is_extend = any(d.partner_discord_id == 400 for d in active_deals)
        cost = self._calculate_cost(active_deals, is_extend)
        expected = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
        assert cost == expected

    def test_rating_affects_cost(self, service):
        """Higher ratings should produce higher costs."""
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10, cost=0)
        active_deals = service.get_user_deals(guild_id=123, discord_id=100)

        cost_low = self._calculate_cost(active_deals, is_extend=False, buyer_rating=1000, partner_rating=1000)
        cost_high = self._calculate_cost(active_deals, is_extend=False, buyer_rating=2000, partner_rating=2000)
        assert cost_high > cost_low
