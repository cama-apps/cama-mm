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
from types import SimpleNamespace
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

    def test_every_pool_has_non_empty_strings(self):
        """All defined flavor pools should be non-empty and contain only strings."""
        for key, pool in FLAVOR_POOLS.items():
            assert len(pool) > 0, f"FLAVOR_POOLS['{key}'] is empty"
            for i, entry in enumerate(pool):
                assert isinstance(entry, str), f"FLAVOR_POOLS['{key}'][{i}] is not a string"


# ===========================================================================
# _compute_percentile() tests
# ===========================================================================


class TestComputePercentile:
    @pytest.mark.parametrize(
        "query,values,expected",
        [
            (10, [], 50.0),
            (5, [5], 50.0),
            (10, list(range(1, 11)), 95.0),
            (1, list(range(1, 11)), 5.0),
            (5, list(range(1, 11)), 45.0),
            (5, [5, 5, 5], 50.0),
            (3, [1, 3, 3, 5], 50.0),
        ],
    )
    def test_compute_percentile(self, query, values, expected):
        assert WrappedService._compute_percentile(query, values) == expected


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

    @pytest.mark.parametrize(
        "cls,assertions",
        [
            (PairwiseWrapped, lambda obj: (obj.best_teammates == [], obj.nemesis is None)),
            (
                PackageDealWrapped,
                lambda obj: (
                    obj.times_bought == 0,
                    obj.times_bought_on_you == 0,
                    obj.unique_buyers == 0,
                    obj.jc_spent == 0,
                    obj.jc_spent_on_you == 0,
                    obj.total_games_committed == 0,
                ),
            ),
            (HeroSpotlightWrapped, lambda obj: (obj.top_hero_name == "", obj.top_3_heroes == [])),
            (RoleBreakdownWrapped, lambda obj: (obj.lane_freq == {}, obj.total_games == 0)),
        ],
    )
    def test_no_arg_dataclass_defaults(self, cls, assertions):
        obj = cls()
        for check in assertions(obj):
            assert check


# ===========================================================================
# get_personal_summary_wrapped() tests
# ===========================================================================


class TestGetPersonalSummaryWrapped:
    def test_returns_none_when_player_not_found(self):
        svc, _, player_repo = _build_service()
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

        wrapped_repo.get_player_rating_change.return_value = 75

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
        start_ts, end_ts = svc._get_year_timestamps(2026)
        wrapped_repo.get_player_rating_change.assert_called_once_with(
            111, 0, start_ts, end_ts
        )
        wrapped_repo.get_month_rating_changes.assert_not_called()


# ===========================================================================
# Shared wrapped story snapshot
# ===========================================================================


def test_wrapped_story_snapshot_reuses_queries_and_decoded_enrichment():
    svc, wrapped_repo, player_repo = _build_service()
    steam_id = 76561198000000001
    player_repo.get_by_id.return_value = SimpleNamespace(name="TestPlayer")
    player_repo.get_steam_ids.return_value = [steam_id]
    wrapped_repo.get_player_rating_change.return_value = 0
    wrapped_repo.get_month_match_stats.return_value = [
        {
            "discord_id": 111,
            "games_played": 3,
            "wins": 2,
            "total_kills": 20,
            "total_deaths": 10,
            "total_assists": 30,
        }
    ]
    wrapped_repo.get_month_player_heroes.return_value = [
        {
            "discord_id": 111,
            "hero_id": 1,
            "picks": 3,
            "wins": 2,
            "total_kills": 20,
            "total_deaths": 10,
            "total_assists": 30,
        }
    ]
    rows = []
    for match_id, lane_role, won in (
        (1, 1, 1),
        (2, 2, 0),
        (3, 1, 1),
    ):
        rows.append(
            {
                "match_id": match_id,
                "match_date": f"2026-01-0{match_id} 20:00:00",
                "duration_seconds": 1800 + match_id,
                "won": won,
                "hero_id": 1,
                "kills": match_id,
                "deaths": 1,
                "assists": 2,
                "enrichment_data": json.dumps(
                    {
                        "players": [
                            {
                                "account_id": steam_id,
                                "lane_role": lane_role,
                                "actions_per_min": 100 + match_id,
                            }
                        ]
                    }
                ),
            }
        )
    wrapped_repo.get_player_year_matches.return_value = rows

    json_loads = MagicMock(wraps=json.loads)
    json_adapter = SimpleNamespace(
        loads=json_loads,
        JSONDecodeError=json.JSONDecodeError,
    )
    with patch("services.wrapped_service.json", json_adapter):
        snapshot = svc.build_wrapped_story_snapshot(111, 2026, guild_id=0)
        summary = svc.get_personal_summary_wrapped(
            111,
            2026,
            guild_id=0,
            snapshot=snapshot,
        )
        records = svc.get_player_records_wrapped(
            111,
            2026,
            guild_id=0,
            snapshot=snapshot,
        )
        spotlight = svc.get_hero_spotlight_wrapped(
            111,
            2026,
            guild_id=0,
            snapshot=snapshot,
        )
        roles = svc.get_role_breakdown_wrapped(
            111,
            2026,
            guild_id=0,
            snapshot=snapshot,
        )

    assert summary is not None
    assert records is not None
    assert spotlight is not None
    assert roles is not None
    assert roles.lane_freq == {1: 2, 2: 1}
    assert json_loads.call_count == len(rows)
    wrapped_repo.get_month_match_stats.assert_called_once()
    wrapped_repo.get_month_player_heroes.assert_called_once()
    wrapped_repo.get_player_year_matches.assert_called_once()
    wrapped_repo.get_month_player_match_details.assert_not_called()
    player_repo.get_steam_ids.assert_called_once_with(111)


# ===========================================================================
# get_player_wrapped() rating lookup regression
# ===========================================================================


def test_player_wrapped_uses_player_scoped_rating_change():
    svc, wrapped_repo, player_repo = _build_service()
    player_repo.get_by_id.return_value = SimpleNamespace(name="TestPlayer")
    wrapped_repo.get_month_player_match_details.return_value = {
        "games_played": 2,
        "wins": 1,
        "losses": 1,
    }
    wrapped_repo.get_player_rating_change.return_value = 42
    wrapped_repo.get_month_player_heroes.return_value = []
    wrapped_repo.get_month_betting_stats.return_value = []

    result = svc.get_player_wrapped(111, 2026, guild_id=0)

    assert result is not None
    assert result.rating_change == 42
    wrapped_repo.get_player_rating_change.assert_called_once()
    wrapped_repo.get_month_rating_changes.assert_not_called()


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

        names = {222: "Alice", 333: "Bob", 444: "Charlie", 555: "Diana", 666: "Eve"}
        player_repo.get_by_ids.return_value = [
            SimpleNamespace(discord_id=pid, name=name)
            for pid, name in names.items()
        ]

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
        player_repo.get_by_ids.assert_called_once_with(
            [222, 333, 444, 555, 666], 0
        )
        player_repo.get_by_id.assert_not_called()

    def test_pairwise_name_lookup_deduplicates_overlapping_players(self):
        pairings_repo = MagicMock()
        pairings_repo.get_best_teammates.return_value = [
            {
                "teammate_id": 222,
                "games_together": 10,
                "wins_together": 8,
                "win_rate": 0.8,
            }
        ]
        pairings_repo.get_most_played_with.return_value = [
            {
                "teammate_id": 222,
                "games_together": 12,
                "wins_together": 9,
                "win_rate": 0.75,
            }
        ]
        pairings_repo.get_worst_matchups.return_value = []
        pairings_repo.get_best_matchups.return_value = []
        pairings_repo.get_most_played_against.return_value = [
            {
                "opponent_id": 222,
                "games_against": 6,
                "wins_against": 3,
                "win_rate": 0.5,
            }
        ]
        svc, _, player_repo = _build_service(pairings_repo=pairings_repo)
        player_repo.get_by_ids.return_value = [
            SimpleNamespace(discord_id=222, name="Alice")
        ]

        result = svc.get_pairwise_wrapped(111, guild_id=0)

        assert result is not None
        assert result.best_teammates[0].username == "Alice"
        assert result.most_played_with[0].username == "Alice"
        assert result.most_played_against[0].username == "Alice"
        player_repo.get_by_ids.assert_called_once_with([222], 0)

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
        player_mock.discord_id = 222
        player_mock.name = "Teammate"
        player_repo.get_by_ids.return_value = [player_mock]

        svc.get_pairwise_wrapped(111, guild_id=None)

        # guild_id=None is passed through; repos handle normalization internally
        pairings_repo.get_best_teammates.assert_called_once_with(111, None, min_games=3, limit=3)
        player_repo.get_by_ids.assert_called_once_with([222], None)


# ===========================================================================
# get_package_deal_wrapped() tests
# ===========================================================================


class TestGetPackageDealWrapped:
    def test_returns_none_without_service(self):
        svc, _, _ = _build_service(package_deal_service=None)
        result = svc.get_package_deal_wrapped(111, 2026, guild_id=0)
        assert result is None

    def test_returns_none_when_no_deals(self):
        pkg_service = MagicMock()
        pkg_service.package_deal_repo.get_purchases_involving_player.return_value = []

        svc, _, _ = _build_service(package_deal_service=pkg_service)
        result = svc.get_package_deal_wrapped(111, 2026, guild_id=0)
        assert result is None

    def test_returns_deal_data(self):
        pkg_service = MagicMock()

        # Create mock PackageDealPurchase objects
        buy1 = MagicMock()
        buy1.buyer_discord_id = 111  # Player bought this one
        buy1.partner_discord_id = 222
        buy1.games_committed = 5
        buy1.jc_spent = 50

        buy2 = MagicMock()
        buy2.buyer_discord_id = 333  # Someone else bought this on player
        buy2.partner_discord_id = 111
        buy2.games_committed = 3
        buy2.jc_spent = 30

        pkg_service.package_deal_repo.get_purchases_involving_player.return_value = [buy1, buy2]

        svc, _, _ = _build_service(package_deal_service=pkg_service)

        result = svc.get_package_deal_wrapped(111, 2026, guild_id=0)

        assert result is not None
        assert result.times_bought == 1  # buy1 (player is buyer)
        assert result.times_bought_on_you == 1  # buy2 (player is partner)
        assert result.unique_buyers == 1  # Only player 333
        assert result.jc_spent == 50  # buy1 cost
        assert result.jc_spent_on_you == 30  # buy2 cost
        assert result.total_games_committed == 8  # 5 + 3


class TestGetPackageDealWrappedConsumedDeals:
    """[T11] Regression: deals bought during the year must still be counted
    in get_package_deal_wrapped even after they are fully consumed and the
    active package_deals rows are DELETEd.

    Before the immutable purchase-log fix, get_package_deal_wrapped read the
    live package_deals table (filtered to games_remaining > 0), so a player who
    bought deals that were all consumed/deleted showed ZERO -- silently
    undercounting the year. This test fails on that old behavior.
    """

    def _build_real_service(self, repo_db_path):
        from datetime import UTC, datetime

        from repositories.package_deal_repository import PackageDealRepository
        from services.package_deal_service import PackageDealService

        pkg_repo = PackageDealRepository(repo_db_path)
        pkg_service = PackageDealService(pkg_repo)
        svc = WrappedService(
            wrapped_repo=MagicMock(),
            player_repo=MagicMock(),
            match_repo=MagicMock(),
            bet_repo=MagicMock(),
            package_deal_service=pkg_service,
        )
        year = datetime.now(UTC).year
        return svc, pkg_repo, year

    def test_consumed_and_deleted_deals_still_counted(self, repo_db_path):
        svc, pkg_repo, year = self._build_real_service(repo_db_path)
        guild_id = 0
        buyer = 111

        # Player buys two distinct deals (10 games each) this year.
        deal_a = pkg_repo.create_or_extend_deal(
            guild_id=guild_id, buyer_id=buyer, partner_id=222, games=10, cost=50
        )
        deal_b = pkg_repo.create_or_extend_deal(
            guild_id=guild_id, buyer_id=buyer, partner_id=333, games=10, cost=70
        )

        # Fully consume both deals, then run the cleanup that DELETEs them.
        for _ in range(10):
            pkg_repo.decrement_deals(guild_id, [deal_a.id, deal_b.id])
        pkg_repo.delete_expired_deals(guild_id)

        # Sanity: the OLD source of truth (live active deals) is now empty,
        # which is exactly what made the old wrapped logic report zero.
        assert pkg_repo.get_deals_involving_player(guild_id, buyer) == []

        # The fix: the purchase log still has both deals, so wrapped counts them.
        result = svc.get_package_deal_wrapped(buyer, year, guild_id=guild_id)

        assert result is not None
        assert result.times_bought == 2
        assert result.jc_spent == 120  # 50 + 70
        assert result.total_games_committed == 20  # 10 + 10 at purchase time

    def test_partner_purchases_counted_after_deletion(self, repo_db_path):
        svc, pkg_repo, year = self._build_real_service(repo_db_path)
        guild_id = 0
        target = 111

        # Someone else buys a deal ON the target, which is then consumed/deleted.
        deal = pkg_repo.create_or_extend_deal(
            guild_id=guild_id, buyer_id=999, partner_id=target, games=10, cost=80
        )
        for _ in range(10):
            pkg_repo.decrement_deals(guild_id, [deal.id])
        pkg_repo.delete_expired_deals(guild_id)

        result = svc.get_package_deal_wrapped(target, year, guild_id=guild_id)

        assert result is not None
        assert result.times_bought_on_you == 1
        assert result.unique_buyers == 1
        assert result.jc_spent_on_you == 80

    def test_purchases_outside_year_excluded(self, repo_db_path):
        svc, pkg_repo, year = self._build_real_service(repo_db_path)
        guild_id = 0
        buyer = 111

        pkg_repo.create_or_extend_deal(
            guild_id=guild_id, buyer_id=buyer, partner_id=222, games=10, cost=50
        )

        # A different calendar year must not see this purchase.
        result = svc.get_package_deal_wrapped(buyer, year - 5, guild_id=guild_id)
        assert result is None


# ===========================================================================
# get_hero_spotlight_wrapped() tests
# ===========================================================================


class TestGetHeroSpotlightWrapped:
    def test_returns_none_when_no_heroes(self):
        svc, wrapped_repo, _ = _build_service()
        wrapped_repo.get_month_player_heroes.return_value = []
        result = svc.get_hero_spotlight_wrapped(111, 2026, guild_id=0)
        assert result is None

    def test_returns_none_when_no_heroes_for_player(self):
        svc, wrapped_repo, _ = _build_service()
        wrapped_repo.get_month_player_heroes.return_value = [
            {"discord_id": 999, "hero_id": 1, "picks": 5, "wins": 3},
        ]
        result = svc.get_hero_spotlight_wrapped(111, 2026, guild_id=0)
        assert result is None

    @patch("services.wrapped_service.get_hero_name")
    def test_returns_hero_spotlight(self, mock_hero_name):
        mock_hero_name.side_effect = lambda hid: {1: "Anti-Mage", 2: "Axe", 3: "Bane"}.get(hid)

        svc, wrapped_repo, _ = _build_service()
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


class TestDrawLaneBreakdownSlide:
    def test_with_lane_data(self):
        from utils.wrapped_drawing import draw_lane_breakdown_slide

        freq = {1: 5, 2: 8, 3: 3, 4: 1}
        buf = draw_lane_breakdown_slide("TestPlayer", "Cama Wrapped 2026", freq, total_games=17)
        img = Image.open(buf)
        assert img.size == (800, 600)


class TestWordWrap:
    def test_empty_string(self):
        from PIL import Image, ImageDraw, ImageFont

        from utils.wrapped_drawing import _word_wrap

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        assert _word_wrap("", font, 200, draw) == []

    def test_single_word(self):
        from PIL import Image, ImageDraw, ImageFont

        from utils.wrapped_drawing import _word_wrap

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        assert _word_wrap("hello", font, 200, draw) == ["hello"]

    def test_fits_one_line(self):
        from PIL import Image, ImageDraw, ImageFont

        from utils.wrapped_drawing import _word_wrap

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        result = _word_wrap("short text", font, 500, draw)
        assert result == ["short text"]

    def test_wraps_long_text(self):
        from PIL import Image, ImageDraw, ImageFont

        from utils.wrapped_drawing import _word_wrap

        img = Image.new("RGBA", (100, 100))
        draw = ImageDraw.Draw(img)
        font = ImageFont.load_default()
        result = _word_wrap("this is a longer piece of text that should wrap", font, 80, draw)
        assert len(result) > 1
        # All words should be preserved
        assert " ".join(result) == "this is a longer piece of text that should wrap"

    def test_truncates_oversized_word(self):
        from PIL import Image, ImageDraw, ImageFont

        from utils.wrapped_drawing import _word_wrap

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


class TestWrappedGamblingData:
    def test_reuses_degen_score_from_player_stats(self):
        from commands.wrapped import WrappedCog

        embedded_degen = SimpleNamespace(
            total=73,
            title="Degenerate",
            emoji="🎰",
        )
        player_stats = SimpleNamespace(
            total_bets=12,
            win_rate=0.5,
            net_pnl=-40,
            roi=-0.25,
            degen_score=embedded_degen,
        )
        pnl_series = [(1, -10), (2, -40)]
        wrapped_service = MagicMock()
        snapshot = object()
        wrapped_service.build_wrapped_story_snapshot.return_value = snapshot
        gambling_stats = MagicMock()
        gambling_stats.get_cumulative_pnl_series.return_value = pnl_series
        gambling_stats.get_player_stats.return_value = player_stats
        gambling_stats.calculate_degen_score.return_value = SimpleNamespace(
            total=99,
            title="Wrong duplicate result",
            emoji="❌",
        )
        bot = SimpleNamespace(
            wrapped_service=wrapped_service,
            gambling_stats_service=gambling_stats,
            match_repo=None,
        )

        result = WrappedCog(bot)._fetch_all_wrapped_data(42, 2026, 111)

        assert result[7] == (
            pnl_series,
            {
                "total_bets": 12,
                "win_rate": 0.5,
                "net_pnl": -40,
                "roi": -0.25,
                "degen_score": 73,
                "degen_title": "Degenerate",
                "degen_emoji": "🎰",
            },
        )
        gambling_stats.get_player_stats.assert_called_once_with(111, 42)
        gambling_stats.calculate_degen_score.assert_not_called()
        wrapped_service.build_wrapped_story_snapshot.assert_called_once_with(
            111,
            2026,
            42,
        )
        assert wrapped_service.get_server_wrapped.call_args.kwargs == {"snapshot": snapshot}
        assert wrapped_service.get_personal_summary_wrapped.call_args.kwargs == {
            "snapshot": snapshot
        }
        assert wrapped_service.get_player_records_wrapped.call_args.kwargs == {"snapshot": snapshot}
        assert wrapped_service.get_hero_spotlight_wrapped.call_args.kwargs == {"snapshot": snapshot}
        assert wrapped_service.get_role_breakdown_wrapped.call_args.kwargs == {"snapshot": snapshot}


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
