"""
Tests for package deal scalar pricing: active-count-zero deal is cheap, subsequent deals paid.

These call the production pricing function (commands.shop._calculate_package_deal_cost)
directly, so the formula itself is under test rather than a copy of it.
"""

import pytest

from commands.shop import PACKAGE_DEAL_NO_ACTIVE_COST, _calculate_package_deal_cost
from config import SHOP_PACKAGE_DEAL_BASE_COST, SHOP_PACKAGE_DEAL_RATING_DIVISOR
from repositories.package_deal_repository import PackageDealRepository
from services.package_deal_service import PackageDealService


class TestPackageDealPricing:
    """Tests for the active-count-zero pricing logic."""

    @pytest.fixture
    def service(self, repo_db_path):
        repo = PackageDealRepository(repo_db_path)
        return PackageDealService(repo)

    def test_no_active_deals_costs_one_jopacoin(self, service):
        """With 0 active deals, the next deal should cost 1 jopacoin."""
        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        assert len(active_deals) == 0

        cost = _calculate_package_deal_cost(len(active_deals), False, 1500, 1500)
        assert cost == PACKAGE_DEAL_NO_ACTIVE_COST == 1

    def test_second_deal_costs_normal(self, service):
        """With 1 active deal, next deal should cost the normal formula price."""
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10, cost=1)

        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        assert len(active_deals) == 1

        is_extend = any(d.partner_discord_id == 300 for d in active_deals)
        assert not is_extend

        cost = _calculate_package_deal_cost(len(active_deals), is_extend, 1500, 1500)
        expected = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
        assert cost == expected

    def test_extending_existing_deal_costs_normal(self, service):
        """Extending an existing deal should always cost normal price, even if it's the only deal."""
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10, cost=1)

        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        is_extend = any(d.partner_discord_id == 200 for d in active_deals)
        assert is_extend

        cost = _calculate_package_deal_cost(len(active_deals), is_extend, 1500, 1500)
        expected = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
        assert cost == expected

    def test_all_deals_expired_resets_to_one_jopacoin(self, service):
        """When all deals expire (0 active), next deal should cost 1 jopacoin again."""
        # Create and expire a deal
        deal = service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=1, cost=0)
        service.decrement_deals(guild_id=123, deal_ids=[deal.id])

        # All deals expired — active count should be 0
        active_deals = service.get_user_deals(guild_id=123, discord_id=100)
        assert len(active_deals) == 0

        cost = _calculate_package_deal_cost(len(active_deals), False, 1500, 1500)
        assert cost == PACKAGE_DEAL_NO_ACTIVE_COST == 1

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
        cost = _calculate_package_deal_cost(len(active_deals), is_extend, 1500, 1500)
        expected = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
        assert cost == expected

    def test_rating_affects_cost(self, service):
        """Higher ratings should produce higher costs."""
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10, cost=0)
        active_deals = service.get_user_deals(guild_id=123, discord_id=100)

        cost_low = _calculate_package_deal_cost(len(active_deals), False, 1000, 1000)
        cost_high = _calculate_package_deal_cost(len(active_deals), False, 2000, 2000)
        assert cost_high > cost_low

    def test_missing_ratings_default_to_1500(self, service):
        """None ratings fall back to 1500 each in the paid formula."""
        service.create_or_extend_deal(guild_id=123, buyer_id=100, partner_id=200, games=10, cost=0)
        active_deals = service.get_user_deals(guild_id=123, discord_id=100)

        cost = _calculate_package_deal_cost(len(active_deals), False, None, None)
        expected = SHOP_PACKAGE_DEAL_BASE_COST + int(3000 / SHOP_PACKAGE_DEAL_RATING_DIVISOR)
        assert cost == expected
