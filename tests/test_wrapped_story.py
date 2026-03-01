"""
Tests for new wrapped story slide features.

Covers:
- get_random_flavor() and FLAVOR_POOLS validation
- _compute_percentile() edge cases
- get_personal_summary_wrapped()
- get_pairwise_wrapped()
- get_package_deal_wrapped()
- get_hero_spotlight_wrapped()
- get_role_breakdown_wrapped()
- New dataclasses
- New drawing functions (smoke tests)
- get_deals_involving_player() repository method
"""

import io
import json
from unittest.mock import MagicMock, patch

import pytest
from PIL import Image

from services.wrapped_service import (
    FLAVOR_POOLS,
    HeroSpotlightWrapped,
    PackageDealWrapped,
    PairwiseEntry,
    PairwiseWrapped,
    PersonalSummaryWrapped,
    RoleBreakdownWrapped,
    WrappedService,
    get_random_flavor,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _build_service(
    pairings_repo=None,
    package_deal_service=None,
    gambling_stats_service=None,
):
    """Build a WrappedService with mocked repositories."""
    wrapped_repo = MagicMock()
    player_repo = MagicMock()
    match_repo = MagicMock()
    bet_repo = MagicMock()
    svc = WrappedService(
        wrapped_repo=wrapped_repo,
        player_repo=player_repo,
        match_repo=match_repo,
        bet_repo=bet_repo,
        gambling_stats_service=gambling_stats_service,
        pairings_repo=pairings_repo,
        package_deal_service=package_deal_service,
    )
    return svc, wrapped_repo, player_repo


# ===========================================================================
# get_random_flavor() tests
# ===========================================================================


class TestGetRandomFlavor:
    def test_returns_string_from_pool(self):
        result = get_random_flavor("games_played_high")
        assert isinstance(result, str)
        assert result in FLAVOR_POOLS["games_played_high"]

    def test_unknown_key_returns_empty(self):
        assert get_random_flavor("nonexistent_key_xyz") == ""

    def test_template_formatting(self):
        """If a pool had template strings, kwargs should format them."""
        # The current pools don't use templates, but test the mechanism
        with patch.dict(FLAVOR_POOLS, {"test_pool": ["Hello {name}, you scored {score}"]}):
            result = get_random_flavor("test_pool", name="Alice", score=42)
            assert result == "Hello Alice, you scored 42"

    def test_bad_template_returns_original(self):
        """Missing kwargs should not crash, returns unformatted string."""
        with patch.dict(FLAVOR_POOLS, {"test_pool": ["Hello {missing_var}"]}):
            result = get_random_flavor("test_pool")
            assert result == "Hello {missing_var}"

    def test_every_pool_is_non_empty(self):
        """All defined flavor pools should have at least one entry."""
        for key, pool in FLAVOR_POOLS.items():
            assert len(pool) > 0, f"FLAVOR_POOLS['{key}'] is empty"

    def test_every_pool_has_strings(self):
        """All entries in all pools should be strings."""
        for key, pool in FLAVOR_POOLS.items():
            for i, entry in enumerate(pool):
                assert isinstance(entry, str), f"FLAVOR_POOLS['{key}'][{i}] is not a string"


# ===========================================================================
# _compute_percentile() tests
# ===========================================================================


class TestComputePercentile:
    def test_empty_list_returns_50(self):
        assert WrappedService._compute_percentile(10, []) == 50.0

    def test_single_value_equal(self):
        # Only value equals query → below=0, equal=1 → (0 + 0.5) / 1 * 100 = 50
        assert WrappedService._compute_percentile(5, [5]) == 50.0

    def test_highest_value(self):
        # 10 in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        # below=9, equal=1 → (9 + 0.5) / 10 * 100 = 95.0
        result = WrappedService._compute_percentile(10, list(range(1, 11)))
        assert result == 95.0

    def test_lowest_value(self):
        # 1 in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        # below=0, equal=1 → (0 + 0.5) / 10 * 100 = 5.0
        result = WrappedService._compute_percentile(1, list(range(1, 11)))
        assert result == 5.0

    def test_middle_value(self):
        # 5 in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        # below=4, equal=1 → (4 + 0.5) / 10 * 100 = 45.0
        result = WrappedService._compute_percentile(5, list(range(1, 11)))
        assert result == 45.0

    def test_all_equal(self):
        # 5 in [5, 5, 5] → below=0, equal=3 → (0 + 1.5) / 3 * 100 = 50.0
        result = WrappedService._compute_percentile(5, [5, 5, 5])
        assert result == 50.0

    def test_ties(self):
        # 3 in [1, 3, 3, 5] → below=1, equal=2 → (1 + 1.0) / 4 * 100 = 50.0
        result = WrappedService._compute_percentile(3, [1, 3, 3, 5])
        assert result == 50.0


# ===========================================================================
# New dataclass tests
# ===========================================================================


class TestNewDataclasses:
    def test_personal_summary_wrapped(self):
        ps = PersonalSummaryWrapped(
            discord_id=123,
            discord_username="TestUser",
            games_played=20,
            wins=12,
            losses=8,
            win_rate=0.6,
            rating_change=50,
            total_kills=100,
            total_deaths=50,
            total_assists=80,
            avg_game_duration=2400,
            unique_heroes=10,
            games_played_percentile=75.0,
            win_rate_percentile=80.0,
            kda_percentile=65.0,
            unique_heroes_percentile=70.0,
            total_kda_percentile=60.0,
            flavor_text="Test flavor",
        )
        assert ps.games_played == 20
        assert ps.win_rate == 0.6
        assert ps.games_played_percentile == 75.0

    def test_pairwise_entry(self):
        pe = PairwiseEntry(discord_id=1, username="A", games=10, wins=7, win_rate=0.7)
        assert pe.win_rate == 0.7

    def test_pairwise_wrapped_defaults(self):
        pw = PairwiseWrapped()
        assert pw.best_teammates == []
        assert pw.nemesis is None

    def test_package_deal_wrapped_defaults(self):
        pdw = PackageDealWrapped()
        assert pdw.times_bought == 0
        assert pdw.times_bought_on_you == 0
        assert pdw.unique_buyers == 0
        assert pdw.jc_spent == 0
        assert pdw.jc_spent_on_you == 0
        assert pdw.total_games_committed == 0

    def test_hero_spotlight_wrapped_defaults(self):
        hsw = HeroSpotlightWrapped()
        assert hsw.top_hero_name == ""
        assert hsw.top_3_heroes == []

    def test_role_breakdown_wrapped_defaults(self):
        rbw = RoleBreakdownWrapped()
        assert rbw.lane_freq == {}
        assert rbw.total_games == 0


# ===========================================================================
# get_personal_summary_wrapped() tests
# ===========================================================================


class TestGetPersonalSummaryWrapped:
    def test_returns_none_when_player_not_found(self):
        svc, wrapped_repo, player_repo = _build_service()
        player_repo.get_by_id.return_value = None
        result = svc.get_personal_summary_wrapped(111, 2026, guild_id=0)
        assert result is None

    def test_returns_none_when_no_matches(self):
        svc, wrapped_repo, player_repo = _build_service()
        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        wrapped_repo.get_month_player_match_details.return_value = None
        result = svc.get_personal_summary_wrapped(111, 2026, guild_id=0)
        assert result is None

    def test_returns_summary_with_correct_stats(self):
        svc, wrapped_repo, player_repo = _build_service()

        player_mock = MagicMock()
        player_mock.name = "TestPlayer"
        player_repo.get_by_id.return_value = player_mock
        player_repo.get_steam_ids.return_value = []

        # Match details
        wrapped_repo.get_month_player_match_details.return_value = {
            "games_played": 15,
            "wins": 9,
            "losses": 6,
        }

        # Rating changes
        wrapped_repo.get_month_rating_changes.return_value = [
            {"discord_id": 111, "rating_change": 75},
        ]

        # Match stats for percentile + kills/deaths/assists
        wrapped_repo.get_month_match_stats.return_value = [
            {"discord_id": 111, "games_played": 15, "wins": 9,
             "total_kills": 120, "total_deaths": 60, "total_assists": 90},
            {"discord_id": 222, "games_played": 10, "wins": 5,
             "total_kills": 80, "total_deaths": 40, "total_assists": 50},
            {"discord_id": 333, "games_played": 20, "wins": 8,
             "total_kills": 150, "total_deaths": 80, "total_assists": 100},
        ]

        # Player heroes
        wrapped_repo.get_month_player_heroes.return_value = [
            {"discord_id": 111, "hero_id": 1, "picks": 5, "wins": 3},
            {"discord_id": 111, "hero_id": 2, "picks": 4, "wins": 2},
            {"discord_id": 111, "hero_id": 3, "picks": 3, "wins": 2},
            {"discord_id": 222, "hero_id": 1, "picks": 6, "wins": 3},
        ]

        # Year matches (for duration)
        wrapped_repo.get_player_year_matches.return_value = [
            {"match_date": "2026-01-05 20:00:00", "duration_seconds": 2400},
            {"match_date": "2026-01-10 20:00:00", "duration_seconds": 1800},
            {"match_date": "2026-01-15 20:00:00", "duration_seconds": 3000},
        ]

        result = svc.get_personal_summary_wrapped(111, 2026, guild_id=0)

        assert result is not None
        assert result.discord_username == "TestPlayer"
        assert result.games_played == 15
        assert result.wins == 9
        assert result.losses == 6
        assert result.win_rate == 9 / 15
        assert result.rating_change == 75
        assert result.total_kills == 120
        assert result.total_deaths == 60
        assert result.total_assists == 90
        assert result.unique_heroes == 3  # 3 heroes for discord_id=111
        assert isinstance(result.games_played_percentile, float)
        assert isinstance(result.win_rate_percentile, float)
        assert isinstance(result.kda_percentile, float)
        assert isinstance(result.unique_heroes_percentile, float)
        assert isinstance(result.total_kda_percentile, float)
        assert isinstance(result.flavor_text, str)
        assert len(result.flavor_text) > 0


# ===========================================================================
# get_pairwise_wrapped() tests
# ===========================================================================


class TestGetPairwiseWrapped:
    def test_returns_none_without_pairings_repo(self):
        svc, _, _ = _build_service(pairings_repo=None)
        result = svc.get_pairwise_wrapped(111, guild_id=0)
        assert result is None

    def test_returns_none_when_no_pairwise_data(self):
        pairings_repo = MagicMock()
        pairings_repo.get_best_teammates.return_value = []
        pairings_repo.get_most_played_with.return_value = []
        pairings_repo.get_worst_matchups.return_value = []
        pairings_repo.get_best_matchups.return_value = []
        pairings_repo.get_most_played_against.return_value = []

        svc, _, _ = _build_service(pairings_repo=pairings_repo)
        result = svc.get_pairwise_wrapped(111, guild_id=0)
        assert result is None

    def test_returns_pairwise_data_with_resolved_names(self):
        pairings_repo = MagicMock()
        pairings_repo.get_best_teammates.return_value = [
            {"teammate_id": 222, "games_together": 10, "wins_together": 8, "win_rate": 0.8},
        ]
        pairings_repo.get_most_played_with.return_value = [
            {"teammate_id": 333, "games_together": 15, "wins_together": 7, "win_rate": 0.47},
        ]
        pairings_repo.get_worst_matchups.return_value = [
            {"opponent_id": 444, "games_against": 8, "wins_against": 2, "win_rate": 0.25},
        ]
        pairings_repo.get_best_matchups.return_value = [
            {"opponent_id": 555, "games_against": 6, "wins_against": 5, "win_rate": 0.83},
        ]
        pairings_repo.get_most_played_against.return_value = [
            {"opponent_id": 666, "games_against": 12, "wins_against": 6, "win_rate": 0.5},
        ]

        svc, _, player_repo = _build_service(pairings_repo=pairings_repo)

        # Mock player lookups
        def mock_get_by_id(pid, gid):
            names = {222: "Alice", 333: "Bob", 444: "Charlie", 555: "Diana", 666: "Eve"}
            if pid in names:
                m = MagicMock()
                m.name = names[pid]
                return m
            return None

        player_repo.get_by_id.side_effect = mock_get_by_id

        result = svc.get_pairwise_wrapped(111, guild_id=0)

        assert result is not None
        assert len(result.best_teammates) == 1
        assert result.best_teammates[0].username == "Alice"
        assert result.best_teammates[0].games == 10
        assert result.best_teammates[0].wins == 8
        assert result.best_teammates[0].win_rate == 0.8

        assert len(result.most_played_with) == 1
        assert result.most_played_with[0].username == "Bob"

        assert result.nemesis is not None
        assert result.nemesis.username == "Charlie"
        assert result.nemesis.win_rate == 0.25

        assert result.punching_bag is not None
        assert result.punching_bag.username == "Diana"

        assert len(result.most_played_against) == 1
        assert result.most_played_against[0].username == "Eve"

    def test_guild_id_none_normalizes_to_zero(self):
        pairings_repo = MagicMock()
        pairings_repo.get_best_teammates.return_value = [
            {"teammate_id": 222, "games_together": 5, "wins_together": 3, "win_rate": 0.6},
        ]
        pairings_repo.get_most_played_with.return_value = []
        pairings_repo.get_worst_matchups.return_value = []
        pairings_repo.get_best_matchups.return_value = []
        pairings_repo.get_most_played_against.return_value = []

        svc, _, player_repo = _build_service(pairings_repo=pairings_repo)
        player_mock = MagicMock()
        player_mock.name = "Teammate"
        player_repo.get_by_id.return_value = player_mock

        result = svc.get_pairwise_wrapped(111, guild_id=None)

        # guild_id=None is passed through; repos handle normalization internally
        pairings_repo.get_best_teammates.assert_called_once_with(111, None, min_games=3, limit=3)


# ===========================================================================
# get_package_deal_wrapped() tests
# ===========================================================================


class TestGetPackageDealWrapped:
    def test_returns_none_without_service(self):
        svc, _, _ = _build_service(package_deal_service=None)
        result = svc.get_package_deal_wrapped(111, guild_id=0)
        assert result is None

    def test_returns_none_when_no_deals(self):
        pkg_service = MagicMock()
        pkg_service.package_deal_repo.get_deals_involving_player.return_value = []

        svc, _, _ = _build_service(package_deal_service=pkg_service)
        result = svc.get_package_deal_wrapped(111, guild_id=0)
        assert result is None

    def test_returns_deal_data(self):
        pkg_service = MagicMock()

        # Create mock PackageDeal objects
        deal1 = MagicMock()
        deal1.buyer_discord_id = 111  # Player bought this one
        deal1.partner_discord_id = 222
        deal1.games_remaining = 5
        deal1.cost_paid = 50

        deal2 = MagicMock()
        deal2.buyer_discord_id = 333  # Someone else bought this on player
        deal2.partner_discord_id = 111
        deal2.games_remaining = 3
        deal2.cost_paid = 30

        pkg_service.package_deal_repo.get_deals_involving_player.return_value = [deal1, deal2]

        svc, _, _ = _build_service(package_deal_service=pkg_service)

        result = svc.get_package_deal_wrapped(111, guild_id=0)

        assert result is not None
        assert result.times_bought == 1  # deal1 (player is buyer)
        assert result.times_bought_on_you == 1  # deal2 (player is partner)
        assert result.unique_buyers == 1  # Only player 333
        assert result.jc_spent == 50  # deal1 cost
        assert result.jc_spent_on_you == 30  # deal2 cost
        assert result.total_games_committed == 8  # 5 + 3


# ===========================================================================
# get_hero_spotlight_wrapped() tests
# ===========================================================================


class TestGetHeroSpotlightWrapped:
    def test_returns_none_when_no_heroes(self):
        svc, wrapped_repo, player_repo = _build_service()
        wrapped_repo.get_month_player_heroes.return_value = []
        result = svc.get_hero_spotlight_wrapped(111, 2026, guild_id=0)
        assert result is None

    def test_returns_none_when_no_heroes_for_player(self):
        svc, wrapped_repo, player_repo = _build_service()
        wrapped_repo.get_month_player_heroes.return_value = [
            {"discord_id": 999, "hero_id": 1, "picks": 5, "wins": 3},
        ]
        result = svc.get_hero_spotlight_wrapped(111, 2026, guild_id=0)
        assert result is None

    @patch("services.wrapped_service.get_hero_name")
    def test_returns_hero_spotlight(self, mock_hero_name):
        mock_hero_name.side_effect = lambda hid: {1: "Anti-Mage", 2: "Axe", 3: "Bane"}.get(hid)

        svc, wrapped_repo, player_repo = _build_service()
        wrapped_repo.get_month_player_heroes.return_value = [
            {"discord_id": 111, "hero_id": 1, "picks": 8, "wins": 5},
            {"discord_id": 111, "hero_id": 2, "picks": 5, "wins": 4},
            {"discord_id": 111, "hero_id": 3, "picks": 3, "wins": 1},
            {"discord_id": 222, "hero_id": 1, "picks": 10, "wins": 6},  # Other player
        ]

        result = svc.get_hero_spotlight_wrapped(111, 2026, guild_id=0)

        assert result is not None
        assert result.top_hero_name == "Anti-Mage"
        assert result.top_hero_picks == 8
        assert result.top_hero_wins == 5
        assert result.top_hero_win_rate == 5 / 8
        assert result.unique_heroes == 3
        assert len(result.top_3_heroes) == 3
        assert result.top_3_heroes[0]["name"] == "Anti-Mage"
        assert result.top_3_heroes[1]["name"] == "Axe"
        assert result.top_3_heroes[2]["name"] == "Bane"

    @patch("services.wrapped_service.get_hero_name")
    def test_single_hero(self, mock_hero_name):
        mock_hero_name.return_value = "Crystal Maiden"

        svc, wrapped_repo, _ = _build_service()
        wrapped_repo.get_month_player_heroes.return_value = [
            {"discord_id": 111, "hero_id": 5, "picks": 10, "wins": 7},
        ]

        result = svc.get_hero_spotlight_wrapped(111, 2026, guild_id=0)

        assert result is not None
        assert result.top_hero_name == "Crystal Maiden"
        assert result.unique_heroes == 1
        assert len(result.top_3_heroes) == 1


# ===========================================================================
# get_role_breakdown_wrapped() tests
# ===========================================================================


class TestGetRoleBreakdownWrapped:
    def test_returns_none_when_no_matches(self):
        svc, wrapped_repo, player_repo = _build_service()
        player_repo.get_steam_ids.return_value = []
        wrapped_repo.get_player_year_matches.return_value = []
        result = svc.get_role_breakdown_wrapped(111, 2026, guild_id=0)
        assert result is None

    def test_returns_result_with_no_enrichment_data(self):
        svc, wrapped_repo, player_repo = _build_service()
        player_repo.get_steam_ids.return_value = []
        # Matches exist but have no enrichment data
        wrapped_repo.get_player_year_matches.return_value = [
            {"match_date": "2026-02-05 20:00:00", "enrichment_data": None},
        ]
        result = svc.get_role_breakdown_wrapped(111, 2026, guild_id=0)
        # Year-scoped: matches exist so result is returned, but no role data
        assert result is not None
        assert result.lane_freq == {}
        # total_games counts only recognized lane games (none here)
        assert result.total_games == 0

    def test_returns_role_freq_from_enrichment(self):
        svc, wrapped_repo, player_repo = _build_service()
        steam_id = 76561198000000001
        player_repo.get_steam_ids.return_value = [steam_id]

        enrichment1 = json.dumps({"players": [{"account_id": steam_id, "lane_role": 1}]})
        enrichment2 = json.dumps({"players": [{"account_id": steam_id, "lane_role": 2}]})
        enrichment3 = json.dumps({"players": [{"account_id": steam_id, "lane_role": 1}]})

        wrapped_repo.get_player_year_matches.return_value = [
            {"match_date": "2026-01-05 20:00:00", "enrichment_data": enrichment1},
            {"match_date": "2026-01-10 20:00:00", "enrichment_data": enrichment2},
            {"match_date": "2026-01-15 20:00:00", "enrichment_data": enrichment3},
        ]

        result = svc.get_role_breakdown_wrapped(111, 2026, guild_id=0)

        assert result is not None
        assert result.total_games == 3
        assert result.lane_freq == {1: 2, 2: 1}

    def test_empty_freq_when_no_enrichment(self):
        svc, wrapped_repo, player_repo = _build_service()
        player_repo.get_steam_ids.return_value = []

        wrapped_repo.get_player_year_matches.return_value = [
            {"match_date": "2026-01-05 20:00:00", "enrichment_data": None},
            {"match_date": "2026-01-10 20:00:00", "enrichment_data": None},
        ]

        result = svc.get_role_breakdown_wrapped(111, 2026, guild_id=0)

        assert result is not None
        # total_games counts only recognized lane games (none here)
        assert result.total_games == 0
        assert result.lane_freq == {}

    def test_skips_lane_role_zero(self):
        svc, wrapped_repo, player_repo = _build_service()
        steam_id = 76561198000000001
        player_repo.get_steam_ids.return_value = [steam_id]

        enrichment = json.dumps({"players": [{"account_id": steam_id, "lane_role": 0}]})

        wrapped_repo.get_player_year_matches.return_value = [
            {"match_date": "2026-01-05 20:00:00", "enrichment_data": enrichment},
        ]

        result = svc.get_role_breakdown_wrapped(111, 2026, guild_id=0)
        assert result is not None
        assert result.lane_freq == {}

    def test_skips_jungle_lane_role_4(self):
        """Jungle (lane_role=4) is intentionally excluded from lane breakdown."""
        svc, wrapped_repo, player_repo = _build_service()
        steam_id = 76561198000000001
        player_repo.get_steam_ids.return_value = [steam_id]

        enrichment_jungle = json.dumps({"players": [{"account_id": steam_id, "lane_role": 4}]})
        enrichment_mid = json.dumps({"players": [{"account_id": steam_id, "lane_role": 2}]})

        wrapped_repo.get_player_year_matches.return_value = [
            {"match_date": "2026-01-05 20:00:00", "enrichment_data": enrichment_jungle},
            {"match_date": "2026-01-06 20:00:00", "enrichment_data": enrichment_mid},
        ]

        result = svc.get_role_breakdown_wrapped(111, 2026, guild_id=0)
        assert result is not None
        # Jungle game should be excluded
        assert 4 not in result.lane_freq
        assert result.lane_freq == {2: 1}
        # total_games counts only recognized lanes so percentages add to 100%
        assert result.total_games == 1


# ===========================================================================
# Repository: get_deals_involving_player() tests
# ===========================================================================


class TestGetDealsInvolvingPlayer:
    def test_returns_deals_as_buyer(self, repo_db_path):
        from repositories.package_deal_repository import PackageDealRepository

        repo = PackageDealRepository(repo_db_path)
        repo.create_or_extend_deal(guild_id=0, buyer_id=111, partner_id=222, games=5, cost=50)

        deals = repo.get_deals_involving_player(0, 111)
        assert len(deals) == 1
        assert deals[0].buyer_discord_id == 111
        assert deals[0].partner_discord_id == 222
        assert deals[0].games_remaining == 5

    def test_returns_deals_as_partner(self, repo_db_path):
        from repositories.package_deal_repository import PackageDealRepository

        repo = PackageDealRepository(repo_db_path)
        repo.create_or_extend_deal(guild_id=0, buyer_id=333, partner_id=111, games=3, cost=30)

        deals = repo.get_deals_involving_player(0, 111)
        assert len(deals) == 1
        assert deals[0].buyer_discord_id == 333

    def test_returns_both_buyer_and_partner_deals(self, repo_db_path):
        from repositories.package_deal_repository import PackageDealRepository

        repo = PackageDealRepository(repo_db_path)
        repo.create_or_extend_deal(guild_id=0, buyer_id=111, partner_id=222, games=5, cost=50)
        repo.create_or_extend_deal(guild_id=0, buyer_id=333, partner_id=111, games=3, cost=30)

        deals = repo.get_deals_involving_player(0, 111)
        assert len(deals) == 2

    def test_excludes_expired_deals(self, repo_db_path):
        from repositories.package_deal_repository import PackageDealRepository

        repo = PackageDealRepository(repo_db_path)
        repo.create_or_extend_deal(guild_id=0, buyer_id=111, partner_id=222, games=1, cost=10)
        # Decrement to 0
        deals = repo.get_deals_involving_player(0, 111)
        repo.decrement_deals(0, [deals[0].id])

        result = repo.get_deals_involving_player(0, 111)
        assert len(result) == 0

    def test_empty_when_no_deals(self, repo_db_path):
        from repositories.package_deal_repository import PackageDealRepository

        repo = PackageDealRepository(repo_db_path)
        deals = repo.get_deals_involving_player(0, 111)
        assert deals == []

    def test_guild_isolation(self, repo_db_path):
        from repositories.package_deal_repository import PackageDealRepository

        repo = PackageDealRepository(repo_db_path)
        repo.create_or_extend_deal(guild_id=100, buyer_id=111, partner_id=222, games=5, cost=50)
        repo.create_or_extend_deal(guild_id=200, buyer_id=111, partner_id=333, games=3, cost=30)

        deals_100 = repo.get_deals_involving_player(100, 111)
        deals_200 = repo.get_deals_involving_player(200, 111)
        deals_other = repo.get_deals_involving_player(999, 111)

        assert len(deals_100) == 1
        assert len(deals_200) == 1
        assert len(deals_other) == 0


# ===========================================================================
# Drawing smoke tests
# ===========================================================================


class TestDrawStorySlide:
    def test_basic_render(self):
        from utils.wrapped_drawing import draw_story_slide

        buf = draw_story_slide(
            headline="YOUR MONTH",
            stat_value="15",
            stat_label="GAMES PLAYED",
            flavor_text="You played a lot",
            accent_color=(88, 101, 242),
            username="TestPlayer",
            year_label="Cama Wrapped 2026",
        )
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_with_comparisons(self):
        from utils.wrapped_drawing import draw_story_slide

        buf = draw_story_slide(
            headline="YOUR MONTH",
            stat_value="15",
            stat_label="GAMES PLAYED",
            flavor_text="You played a lot",
            accent_color=(88, 101, 242),
            username="TestPlayer",
            year_label="Cama Wrapped 2026",
            comparisons=["More than 78% of players", "Top 22%"],
        )
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_empty_flavor_text(self):
        from utils.wrapped_drawing import draw_story_slide

        buf = draw_story_slide(
            headline="RATING",
            stat_value="+50",
            stat_label="RATING CHANGE",
            flavor_text="",
            accent_color=(241, 196, 15),
            username="TestPlayer",
            year_label="Cama Wrapped 2026",
        )
        img = Image.open(buf)
        assert img.size == (800, 600)


class TestDrawSummaryStatsSlide:
    def test_basic_render(self):
        from utils.wrapped_drawing import draw_summary_stats_slide

        stats = [
            ("60%", "WIN RATE", "Top 20%", (87, 242, 135)),
            ("+50", "RATING", "Rising star", (241, 196, 15)),
            ("120/60/90", "K/D/A", "Violence enjoyer", (237, 66, 69)),
            ("40 min", "AVG GAME", "Long games", (88, 101, 242)),
            ("10", "UNIQUE HEROES", "Diverse", (46, 204, 113)),
            ("3.5", "AVG KDA", "Clean", (155, 89, 182)),
        ]
        buf = draw_summary_stats_slide("TestPlayer", "Cama Wrapped 2026", stats)
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_partial_stats(self):
        from utils.wrapped_drawing import draw_summary_stats_slide

        stats = [
            ("60%", "WIN RATE", "", (87, 242, 135)),
            ("+50", "RATING", "", (241, 196, 15)),
        ]
        buf = draw_summary_stats_slide("TestPlayer", "Cama Wrapped 2026", stats)
        img = Image.open(buf)
        assert img.size == (800, 600)


class TestDrawPairwiseSlide:
    def test_teammates_slide(self):
        from utils.wrapped_drawing import draw_pairwise_slide

        entries = [
            {"discord_id": 222, "username": "Alice", "games": 10, "wins": 8,
             "win_rate": 0.8, "label": "Best Teammate", "flavor": "Unstoppable duo"},
            {"discord_id": 333, "username": "Bob", "games": 15, "wins": 7,
             "win_rate": 0.47, "label": "Most Played With", "flavor": None},
        ]
        buf = draw_pairwise_slide("TestPlayer", "Cama Wrapped 2026", entries, slide_type="teammates")
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_rivals_slide(self):
        from utils.wrapped_drawing import draw_pairwise_slide

        entries = [
            {"discord_id": 444, "username": "Charlie", "games": 8, "wins": 2,
             "win_rate": 0.25, "label": "Nemesis", "flavor": "Pain incarnate"},
        ]
        buf = draw_pairwise_slide("TestPlayer", "Cama Wrapped 2026", entries, slide_type="rivals")
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_with_avatar_fallback(self):
        from utils.wrapped_drawing import draw_pairwise_slide

        entries = [
            {"discord_id": 222, "username": "Alice", "games": 5, "wins": 3,
             "win_rate": 0.6, "label": None, "flavor": None},
        ]
        # No avatar images provided → should use initial circle fallback
        buf = draw_pairwise_slide("TestPlayer", "Cama Wrapped 2026", entries, avatar_images=None)
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_with_avatar_bytes(self):
        from utils.wrapped_drawing import draw_pairwise_slide

        # Create a tiny valid PNG for avatar
        avatar_img = Image.new("RGBA", (48, 48), (255, 0, 0, 255))
        avatar_buf = io.BytesIO()
        avatar_img.save(avatar_buf, format="PNG")
        avatar_bytes = avatar_buf.getvalue()

        entries = [
            {"discord_id": 222, "username": "Alice", "games": 5, "wins": 3,
             "win_rate": 0.6, "label": "Best", "flavor": None},
        ]
        buf = draw_pairwise_slide(
            "TestPlayer", "Cama Wrapped 2026", entries,
            avatar_images={222: avatar_bytes},
        )
        img = Image.open(buf)
        assert img.size == (800, 600)


class TestDrawHeroSpotlightSlide:
    def test_basic_render(self):
        from utils.wrapped_drawing import draw_hero_spotlight_slide

        top_hero = {"name": "Anti-Mage", "picks": 8, "wins": 5, "win_rate": 0.625}
        top_3 = [
            {"name": "Anti-Mage", "picks": 8, "wins": 5, "win_rate": 0.625},
            {"name": "Axe", "picks": 5, "wins": 4, "win_rate": 0.8},
            {"name": "Bane", "picks": 3, "wins": 1, "win_rate": 0.333},
        ]
        buf = draw_hero_spotlight_slide("TestPlayer", "Cama Wrapped 2026", top_hero, top_3, 10)
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_single_hero(self):
        from utils.wrapped_drawing import draw_hero_spotlight_slide

        top_hero = {"name": "Crystal Maiden", "picks": 15, "wins": 10, "win_rate": 0.667}
        buf = draw_hero_spotlight_slide("TestPlayer", "Cama Wrapped 2026", top_hero, [top_hero], 1)
        img = Image.open(buf)
        assert img.size == (800, 600)


class TestDrawLaneBreakdownSlide:
    def test_with_lane_data(self):
        from utils.wrapped_drawing import draw_lane_breakdown_slide

        freq = {1: 5, 2: 8, 3: 3, 4: 1}
        buf = draw_lane_breakdown_slide("TestPlayer", "Cama Wrapped 2026", freq, total_games=17)
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_empty_lane_data(self):
        from utils.wrapped_drawing import draw_lane_breakdown_slide

        buf = draw_lane_breakdown_slide("TestPlayer", "Cama Wrapped 2026", {}, total_games=10)
        img = Image.open(buf)
        assert img.size == (800, 600)

class TestWordWrap:
    def test_empty_string(self):
        from utils.wrapped_drawing import _word_wrap
        from PIL import Image, ImageDraw, ImageFont

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        assert _word_wrap("", font, 200, draw) == []

    def test_single_word(self):
        from utils.wrapped_drawing import _word_wrap
        from PIL import Image, ImageDraw, ImageFont

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        assert _word_wrap("hello", font, 200, draw) == ["hello"]

    def test_fits_one_line(self):
        from utils.wrapped_drawing import _word_wrap
        from PIL import Image, ImageDraw, ImageFont

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        result = _word_wrap("short text", font, 500, draw)
        assert result == ["short text"]

    def test_wraps_long_text(self):
        from utils.wrapped_drawing import _word_wrap
        from PIL import Image, ImageDraw, ImageFont

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        result = _word_wrap("this is a longer piece of text that should wrap", font, 80, draw)
        assert len(result) > 1
        # All words should be preserved
        assert " ".join(result) == "this is a longer piece of text that should wrap"

    def test_truncates_oversized_word(self):
        from utils.wrapped_drawing import _word_wrap
        from PIL import Image, ImageDraw, ImageFont

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        # Use a very narrow width so even a single word overflows
        result = _word_wrap("Supercalifragilisticexpialidocious", font, 30, draw)
        assert len(result) == 1
        assert result[0].endswith("..")


class TestDrawPackageDealSlide:
    def test_basic_render(self):
        from utils.wrapped_drawing import draw_package_deal_slide

        buf = draw_package_deal_slide(
            "TestPlayer", "Cama Wrapped 2026",
            times_bought=2, times_bought_on_you=3, unique_buyers=2,
            jc_spent=100, jc_spent_on_you=150, total_games=15,
        )
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_single_deal(self):
        from utils.wrapped_drawing import draw_package_deal_slide

        buf = draw_package_deal_slide(
            "TestPlayer", "Cama Wrapped 2026",
            times_bought=1, times_bought_on_you=0, unique_buyers=0,
            jc_spent=50, jc_spent_on_you=0, total_games=5,
        )
        img = Image.open(buf)
        assert img.size == (800, 600)


class TestWrapChartInSlide:
    def test_wraps_chart_image(self):
        from utils.wrapped_drawing import wrap_chart_in_slide

        # Create a test chart image
        chart_img = Image.new("RGBA", (700, 400), (100, 100, 100, 255))
        chart_buf = io.BytesIO()
        chart_img.save(chart_buf, format="PNG")
        chart_bytes = chart_buf.getvalue()

        buf = wrap_chart_in_slide(chart_bytes, "RATING HISTORY", "The climb was real")
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_handles_invalid_chart_bytes(self):
        from utils.wrapped_drawing import wrap_chart_in_slide

        buf = wrap_chart_in_slide(b"not_a_real_image", "CHART", "Oops")
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_large_chart_gets_scaled(self):
        from utils.wrapped_drawing import wrap_chart_in_slide

        # Create an oversized chart
        chart_img = Image.new("RGBA", (1200, 800), (100, 100, 100, 255))
        chart_buf = io.BytesIO()
        chart_img.save(chart_buf, format="PNG")
        chart_bytes = chart_buf.getvalue()

        buf = wrap_chart_in_slide(chart_bytes, "BIG CHART", "Scaled down")
        img = Image.open(buf)
        assert img.size == (800, 600)

    def test_empty_flavor_text(self):
        from utils.wrapped_drawing import wrap_chart_in_slide

        chart_img = Image.new("RGBA", (700, 400), (100, 100, 100, 255))
        chart_buf = io.BytesIO()
        chart_img.save(chart_buf, format="PNG")
        chart_bytes = chart_buf.getvalue()

        buf = wrap_chart_in_slide(chart_bytes, "CHART", "")
        img = Image.open(buf)
        assert img.size == (800, 600)


class TestSelectAwardsForViewer:
    """Tests for select_awards_for_viewer() award selection logic."""

    def _make_award(self, discord_id, title="Award"):
        from services.wrapped_service import Award

        return Award(
            category="test",
            title=title,
            stat_name="X",
            stat_value="100",
            discord_id=discord_id,
            discord_username=f"User{discord_id}",
            emoji="🏆",
            flavor_text="test",
        )

    def test_viewer_awards_always_included(self):
        from commands.wrapped import select_awards_for_viewer

        awards = [self._make_award(i, f"Award{i}") for i in range(1, 11)]
        result = select_awards_for_viewer(awards, viewer_id=10)
        viewer_in_result = [a for a in result if a.discord_id == 10]
        assert len(viewer_in_result) == 1

    def test_caps_at_max_awards(self):
        from commands.wrapped import select_awards_for_viewer

        awards = [self._make_award(i, f"Award{i}") for i in range(1, 21)]
        result = select_awards_for_viewer(awards, viewer_id=1)
        assert len(result) == 6

    def test_viewer_with_more_than_max_awards(self):
        from commands.wrapped import select_awards_for_viewer

        # Viewer has 8 awards but max is 6
        awards = [self._make_award(1, f"Award{i}") for i in range(8)]
        result = select_awards_for_viewer(awards, viewer_id=1)
        assert len(result) == 6
        assert all(a.discord_id == 1 for a in result)

    def test_fills_remaining_with_others(self):
        from commands.wrapped import select_awards_for_viewer

        awards = [self._make_award(1, "Viewer Award")] + [
            self._make_award(i, f"Other{i}") for i in range(2, 10)
        ]
        result = select_awards_for_viewer(awards, viewer_id=1)
        assert len(result) == 6
        assert result[0].discord_id == 1  # viewer first

    def test_empty_awards(self):
        from commands.wrapped import select_awards_for_viewer

        result = select_awards_for_viewer([], viewer_id=1)
        assert result == []

    def test_no_viewer_awards(self):
        from commands.wrapped import select_awards_for_viewer

        awards = [self._make_award(i, f"Award{i}") for i in range(2, 10)]
        result = select_awards_for_viewer(awards, viewer_id=1)
        assert len(result) == 6
        assert all(a.discord_id != 1 for a in result)


class TestDrawAwardsGrid:
    """Smoke tests for draw_awards_grid including viewer highlighting."""

    def _make_award(self, discord_id, title="Award"):
        from services.wrapped_service import Award

        return Award(
            category="test",
            title=title,
            stat_name="X",
            stat_value="100",
            discord_id=discord_id,
            discord_username=f"User{discord_id}",
            emoji="🏆",
            flavor_text="test",
        )

    def test_basic_render(self):
        from utils.wrapped_drawing import draw_awards_grid

        awards = [self._make_award(i, f"Award{i}") for i in range(1, 4)]
        buf = draw_awards_grid(awards)
        img = Image.open(buf)
        assert img.width > 0
        assert img.height > 0

    def test_viewer_highlight_render(self):
        from utils.wrapped_drawing import draw_awards_grid

        awards = [self._make_award(i, f"Award{i}") for i in range(1, 4)]
        buf = draw_awards_grid(awards, viewer_discord_id=1)
        img = Image.open(buf)
        assert img.width > 0
        assert img.height > 0

    def test_single_award(self):
        from utils.wrapped_drawing import draw_awards_grid

        awards = [self._make_award(1, "Solo Award")]
        buf = draw_awards_grid(awards, viewer_discord_id=1)
        img = Image.open(buf)
        assert img.width > 0

    def test_six_awards(self):
        from utils.wrapped_drawing import draw_awards_grid

        awards = [self._make_award(i, f"Award{i}") for i in range(1, 7)]
        buf = draw_awards_grid(awards, viewer_discord_id=3)
        img = Image.open(buf)
        assert img.width > 0
